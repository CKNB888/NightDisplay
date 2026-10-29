//! `nd_core::engine::Platform` 的 Windows 实现。
//!
//! 这里只做"把各个子系统拼起来"，不含业务判断（该不该黑、黑到什么时候由引擎决定）。
//!
//! 线程约束：黑幕窗口和键鼠钩子都归 [`crate::runtime::UiHandle`] 的界面线程，
//! 本模块只在需要时把闭包丢进去执行。

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use nd_core::chord::ChordKey;
use nd_core::config::Config;
use nd_core::engine::Platform;
use nd_core::mode::{LayerStatus, NightReport, OverlayOutcome};
use nd_core::status::Capabilities;
use nd_core::NdResult;

use crate::brightness;
use crate::gamma;
use crate::input;
use crate::overlay;
use crate::power;
use crate::runtime::{self, UiHandle};
use crate::screen_state::{DisplayState, ScreenState};
use crate::sysutil;
use crate::tasks;
use crate::topology;

/// Windows 平台实现。
pub struct WinPlatform {
    /// 界面线程句柄。第一次需要时创建（CLI 的 `status` 不该为此起线程）。
    ui: std::sync::Mutex<Option<UiHandle>>,
    /// 背光控制器，夜间期间一直持有，退出时 Drop 自动还原。
    brightness: std::sync::Mutex<Option<brightness::Control>>,
    /// 白天/夜间的快速判断，避免每次都问界面线程。
    guard_visible: Arc<AtomicBool>,
    /// 组合键是否按下过（由界面线程置位）。
    unlock_pending: Arc<AtomicBool>,
}

impl Default for WinPlatform {
    fn default() -> Self {
        Self::new()
    }
}

impl WinPlatform {
    pub fn new() -> Self {
        let guard_visible = Arc::new(AtomicBool::new(false));
        let unlock_pending = Arc::new(AtomicBool::new(false));
        Self {
            ui: std::sync::Mutex::new(None),
            brightness: std::sync::Mutex::new(None),
            guard_visible,
            unlock_pending,
        }
    }

    /// 拿到（必要时创建）界面线程。
    fn ui(&self) -> NdResult<UiHandle> {
        let mut guard = self.ui.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(handle) = guard.as_ref() {
            return Ok(handle.clone());
        }
        let handle = UiHandle::start()?;
        let pending = Arc::clone(&self.unlock_pending);
        handle.set_unlock_hook(move || {
            pending.store(true, Ordering::SeqCst);
        });
        *guard = Some(handle.clone());
        Ok(handle)
    }

    /// 组合键有没有被按下（取走即清零）。
    pub fn take_unlock(&self) -> bool {
        self.unlock_pending.swap(false, Ordering::SeqCst)
    }

    /// 界面线程是否已经起来（看门狗判断用）。
    pub fn ui_running(&self) -> bool {
        self.ui.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    /// 提前把界面线程和黑幕窗口建好（常驻进程启动时调用）。
    ///
    /// 这样夜间开始时只剩一次 `ShowWindow`，第一帧黑屏的延迟能压到毫秒级，
    /// 也不会出现"远程画面先亮一下再黑"的观感。
    pub fn warm(&self) -> NdResult<()> {
        self.ui().map(|_| ())
    }

    /// 关掉界面线程。
    pub fn shutdown(&self) {
        let mut guard = self.ui.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(handle) = guard.take() {
            handle.shutdown();
        }
    }
}

impl Platform for WinPlatform {
    fn capabilities(&self) -> Capabilities {
        let build = power::windows_build();
        // 采集排除需要 Windows 10 2004（build 19041）及以上。
        let capture_exclusion = build == 0 || build >= 19041;
        let displays = overlay::display_count();
        Capabilities {
            os: power::os_description(),
            build,
            capture_exclusion,
            gamma_control: gamma::supported(),
            brightness_displays: brightness::supported_display_count(),
            displays,
            elevated: power::is_elevated(),
            helper_running: crate::helper::is_running(),
            virtual_display: topology::describe(true)
                .map(|list| list.iter().any(topology::is_virtual))
                .unwrap_or(false),
            autostart: crate::autostart::is_enabled(),
        }
    }

    fn enter_guard(&self, cfg: &Config) -> NdResult<NightReport> {
        let ui = self.ui()?;
        let requested = cfg.mode;
        let effective = nd_core::mode::resolve(requested);
        let guard_cfg = cfg.guard.clone();
        let started = std::time::Instant::now();

        // 1) 黑幕：必须在界面线程上显示并校验采集排除。
        let outcomes: Vec<OverlayOutcome> = ui.call(|state| state.overlays.show())?;
        let latency = started.elapsed().as_millis() as u64;
        let overlay_ok = !outcomes.is_empty() && outcomes.iter().all(|o| o.ok());
        if !overlay_ok {
            // 失败时界面线程已经把所有窗口收回去了，这里不需要再 hide。
            self.guard_visible.store(false, Ordering::SeqCst);
            return Ok(NightReport {
                requested,
                effective,
                overlays: outcomes.clone(),
                gamma: LayerStatus::Disabled,
                brightness: LayerStatus::Disabled,
                local_input_guard: LayerStatus::Disabled,
                start_latency_ms: latency,
                brightness_unsupported_displays: Vec::new(),
            });
        }
        self.guard_visible.store(true, Ordering::SeqCst);

        // 2) gamma（可选层，失败不影响成功）。
        let gamma_status = if guard_cfg.gamma {
            gamma::apply()
        } else {
            LayerStatus::Disabled
        };

        // 3) 背光（可选层）。拿到控制权后一直持有到 exit_guard。
        let mut displays_state: Vec<DisplayState> = Vec::new();
        let brightness_status = if guard_cfg.brightness {
            match brightness::Control::open() {
                Ok(control) => {
                    let (status, unsupported) = control.to_zero();
                    for (desc, value) in control.snapshot() {
                        displays_state.push(DisplayState {
                            device: desc.clone(),
                            gamma: Vec::new(),
                            brightness: Some(value),
                            description: desc,
                        });
                    }
                    let mut guard = self.brightness.lock().unwrap_or_else(|e| e.into_inner());
                    *guard = Some(control);
                    if !unsupported.is_empty() {
                        tracing::info!(
                            panels = %unsupported.join("、"),
                            "这些面板不支持 DDC/CI 背光，只靠黑幕和 gamma"
                        );
                    }
                    status
                }
                Err(e) => LayerStatus::Unsupported {
                    reason: crate::error::user_text(&e, "无法读取背光"),
                },
            }
        } else {
            LayerStatus::Disabled
        };

        // 4) 落盘原始状态：主进程被杀时看门狗要靠它恢复。
        let mut state = ScreenState {
            displays: displays_state,
            entered_at: Some(chrono::Local::now()),
            topology_changed: false,
        };
        for (device, ramp) in gamma::snapshot() {
            match state.displays.iter_mut().find(|d| d.device == device) {
                Some(entry) => entry.gamma = ramp,
                None => state.displays.push(DisplayState {
                    device,
                    gamma: ramp,
                    brightness: None,
                    description: String::new(),
                }),
            }
        }
        if let Err(e) = crate::screen_state::save(&state) {
            tracing::warn!(error = %e, "无法保存屏幕状态快照，看门狗可能无法恢复 gamma");
        }

        let local_input_guard = if cfg.strict_local_input {
            // 真正装钩子由引擎随后调用 set_input_guard，这里只报告"打算开"。
            LayerStatus::Applied
        } else {
            LayerStatus::Disabled
        };

        Ok(NightReport {
            requested,
            effective,
            overlays: outcomes,
            gamma: gamma_status,
            brightness: brightness_status,
            local_input_guard,
            start_latency_ms: latency,
            brightness_unsupported_displays: Vec::new(),
        })
    }

    fn exit_guard(&self) -> NdResult<()> {
        let mut errors: Vec<String> = Vec::new();
        if let Some(handle) = self.ui.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            if let Err(e) = handle.call(|state| state.overlays.hide()) {
                errors.push(format!(
                    "关闭黑幕失败：{}",
                    crate::error::user_text(&e, "未知原因")
                ));
            }
        }
        self.guard_visible.store(false, Ordering::SeqCst);
        if let Err(e) = gamma::restore() {
            errors.push(format!(
                "恢复 gamma 失败：{}",
                crate::error::user_text(&e, "未知原因")
            ));
        }
        // Drop 里会还原背光。
        let mut guard = self.brightness.lock().unwrap_or_else(|e| e.into_inner());
        *guard = None;
        crate::screen_state::clear();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(nd_core::error::user(errors.join("；")))
        }
    }

    fn guard_active(&self) -> bool {
        if !self.guard_visible.load(Ordering::SeqCst) {
            return false;
        }
        let Ok(handle) = self
            .ui
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| nd_core::error::user("界面线程没起来"))
        else {
            return false;
        };
        handle
            .call(|state| Ok(state.overlays.is_visible()))
            .unwrap_or(false)
    }

    fn repair_guard(&self) -> NdResult<()> {
        let handle = self.ui()?;
        handle.call(|state| state.overlays.reassert())?;
        // 背光会被显示器自己拉回去（省电策略、菜单按钮），这里重新压一次。
        let guard = self.brightness.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(control) = guard.as_ref() {
            let (_status, _) = control.to_zero();
        }
        Ok(())
    }

    fn backup_display(&self) -> NdResult<()> {
        let backup = topology::backup()?;
        let dir = nd_core::paths::backups_dir();
        std::fs::create_dir_all(&dir)
            .map_err(|e| nd_core::error::user(format!("无法创建备份目录：{e}")))?;
        let path = dir.join(format!(
            "display-{}.json",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ));
        let text = serde_json::to_string_pretty(&backup)
            .map_err(|e| nd_core::error::user(format!("显示配置备份无法保存：{e}")))?;
        std::fs::write(&path, text)
            .map_err(|e| nd_core::error::user(format!("显示配置备份写入失败：{e}")))?;
        tracing::info!(path = %path.display(), "已备份显示配置");
        Ok(())
    }

    fn enter_virtual_only(&self, match_pat: &str, dry_run: bool) -> NdResult<String> {
        let plan = topology::plan_virtual_only(match_pat)?;
        let label = if plan.target.name.is_empty() {
            plan.target.device_path.clone()
        } else {
            plan.target.name.clone()
        };
        topology::apply_virtual_only(&plan, dry_run)?;
        if dry_run {
            return Ok(format!(
                "校验通过：将只保留虚拟屏 {label}，放弃 {} 块实体屏（未应用）",
                plan.physical_count
            ));
        }
        // 记录"拓扑被改过"，看门狗能据此还原。
        let mut state = crate::screen_state::load().unwrap_or_default();
        state.topology_changed = true;
        state.entered_at = Some(chrono::Local::now());
        let _ = crate::screen_state::save(&state);
        Ok(format!("已切换到「仅虚拟屏」{label}，远程画面会中断一次"))
    }

    fn exit_virtual_only(&self) -> NdResult<()> {
        let _level = topology::restore()?;
        if let Some(mut state) = crate::screen_state::load() {
            state.topology_changed = false;
            let _ = crate::screen_state::save(&state);
        }
        Ok(())
    }

    fn screen_off(&self) -> NdResult<()> {
        power::screen_off()
    }

    fn screen_on(&self) -> NdResult<()> {
        power::screen_on()
    }

    fn keep_awake(&self, on: bool, seconds: u64) -> NdResult<()> {
        power::keep_awake(on, seconds)
    }

    fn set_input_guard(&self, on: bool, chord: &[ChordKey], hold_ms: u64) -> NdResult<()> {
        if !on {
            // 关闭时即使界面线程没起来也要成功（退出路径不能因为没起线程而报错）。
            if self.ui_running() {
                let handle = self.ui()?;
                handle.call(|state| runtime::apply_input(state, false, &[], 0, false))?;
            } else {
                input::stop();
            }
            return Ok(());
        }
        let handle = self.ui()?;
        let chord = chord.to_vec();
        let strict = true;
        handle.call(move |state| runtime::apply_input(state, true, &chord, hold_ms, strict))?;
        Ok(())
    }

    fn set_lock_suppression(&self, on: bool) -> NdResult<()> {
        power::set_lock_suppression(on)
    }

    fn set_autostart(&self, on: bool) -> NdResult<()> {
        let home = nd_core::paths::home_dir();
        crate::autostart::set(on, &home)
    }

    fn autostart_actual(&self) -> NdResult<bool> {
        Ok(crate::autostart::is_enabled())
    }

    fn install_schedule(&self, cfg: &Config) -> NdResult<()> {
        let home = nd_core::paths::home_dir();
        tasks::install(cfg, &home)?;
        // 看门狗一并登记；失败不算致命（主功能仍然可用）。
        if let Err(e) = tasks::install_watchdog(&home) {
            tracing::warn!(error = %e, "登记看门狗任务失败，主进程仍可用");
        }
        Ok(())
    }

    fn remove_schedule(&self) -> NdResult<()> {
        let removed = tasks::remove_all()?;
        tracing::info!(removed, "已删除计划任务");
        Ok(())
    }

    fn spawn_delayed_day(&self, seconds: u64, test_id: &str) -> NdResult<()> {
        let home = nd_core::paths::home_dir();
        let args = sysutil::self_args(
            &home,
            &[
                "--action",
                "day",
                "--delay",
                &seconds.to_string(),
                "--test-id",
                test_id,
            ],
        );
        let child = sysutil::spawn_detached(&args)
            .map_err(|e| nd_core::error::user(format!("无法启动定时恢复进程：{e}")))?;
        tracing::info!(pid = child.id(), seconds, test_id, "已启动定时恢复进程");
        Ok(())
    }

    fn spawn_watchdog(&self) -> NdResult<()> {
        let home = nd_core::paths::home_dir();
        let pid = std::process::id();
        let args = sysutil::self_args(
            &home,
            &["--action", "watchdog", "--parent", &pid.to_string()],
        );
        let child = sysutil::spawn_detached(&args)
            .map_err(|e| nd_core::error::user(format!("无法启动看门狗：{e}")))?;
        tracing::info!(pid = child.id(), parent = pid, "看门狗已启动");
        Ok(())
    }

    fn capability_fingerprint(&self) -> String {
        // 显卡/驱动/显示器型号变了，就得重新测一次。这里只用可读的稳定信息，
        // 不用句柄（每次启动都不一样）。
        let build = power::windows_build();
        let displays = topology::describe(true)
            .map(|list| {
                list.iter()
                    .map(|d| {
                        format!(
                            "{}|{}|{}",
                            d.name.to_lowercase(),
                            d.tech,
                            if topology::is_virtual(d) { "v" } else { "p" }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_else(|_| "unknown".into());
        format!("win{}_{}", build, displays)
    }
}

/// 一次性取能力快照（不起界面线程，CLI 的 `status`/`selfcheck` 用）。
pub fn current_capabilities() -> Capabilities {
    let build = power::windows_build();
    Capabilities {
        os: power::os_description(),
        build,
        capture_exclusion: build == 0 || build >= 19041,
        gamma_control: gamma::supported(),
        brightness_displays: brightness::supported_display_count(),
        displays: overlay::display_count(),
        elevated: power::is_elevated(),
        helper_running: crate::helper::is_running(),
        virtual_display: topology::describe(true)
            .map(|list| list.iter().any(topology::is_virtual))
            .unwrap_or(false),
        autostart: crate::autostart::is_enabled(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable_and_descriptive() {
        let p = WinPlatform::new();
        let a = p.capability_fingerprint();
        let b = p.capability_fingerprint();
        assert_eq!(a, b, "同一环境下指纹必须稳定");
        assert!(a.starts_with("win"));
    }

    #[test]
    fn new_platform_is_not_guarding() {
        let p = WinPlatform::new();
        assert!(!p.guard_active());
        assert!(!p.ui_running());
    }
}
