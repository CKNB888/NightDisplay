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
use nd_core::mode::{LayerStatus, NightReport, OverlayOutcome, VirtualReport};
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

/// 夜间用的高性能电源计划 GUID（Windows 内置的「高性能」）。
const HIGH_PERFORMANCE_PLAN: &str = "{8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c}";

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
    #[must_use]
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
        let mut guard = self
            .ui
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        self.ui
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }

    /// 写入（并返回）夜间前的恢复点：原始 gamma、背光、电源计划、拓扑标志。
    ///
    /// **必须在改动任何显示设置之前调用。** 断电、蓝屏、任务管理器结束进程
    /// 都只会发生在两次写盘之间，只要恢复点先落地，下一次启动（或看门狗）
    /// 就有东西可还原。
    ///
    /// `topology_will_change` 传 `true` 表示马上要切拓扑：那就把标志也一起写进去，
    /// 这样"已经切了但还没来得及回写"这种窗口期同样被覆盖。
    fn write_restore_point(topology_will_change: bool) -> NdResult<ScreenState> {
        // gamma 只读不写：先把原始 ramp 拿到手，压黑在真正需要时再做。
        if matches!(gamma::capture(), LayerStatus::Failed { .. }) {
            tracing::warn!("部分显示器读不到原始 gamma，恢复时只能用线性 ramp 兜底");
        }
        let mut state = ScreenState {
            displays: Vec::new(),
            entered_at: Some(chrono::Local::now()),
            topology_changed: topology_will_change,
            power_plan: None,
        };
        for (device, ramp) in gamma::snapshot() {
            state.displays.push(DisplayState {
                device,
                gamma: ramp,
                brightness: None,
                description: String::new(),
            });
        }
        // 背光：现在读一次原始值（不改成 0，那一步由 enter_guard 负责）。
        if let Ok(control) = brightness::Control::open() {
            for (description, value) in control.snapshot() {
                match state
                    .displays
                    .iter_mut()
                    .find(|display| display.description == description)
                {
                    Some(entry) => entry.brightness = Some(value),
                    None => state.displays.push(DisplayState {
                        device: String::new(),
                        gamma: Vec::new(),
                        brightness: Some(value),
                        description,
                    }),
                }
            }
        }
        // 电源计划：只有用户真的要求夜间高性能时才记，白天不该乱切。
        if Config::load().is_ok_and(|config| config.high_performance_at_night) {
            state.power_plan = crate::power_plan::active();
        }
        crate::screen_state::save(&state)
            .map_err(|e| nd_core::error::user(format!("无法写入屏幕恢复点：{e}")))?;
        tracing::info!(
            displays = state.displays.len(),
            topology = topology_will_change,
            plan = state.power_plan.as_deref().unwrap_or("（不切换）"),
            "已写入屏幕恢复点"
        );
        Ok(state)
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
        let mut guard = self
            .ui
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
            virtual_display: topology::standby_present(),
            virtual_screen_holding: cfg!(windows) && topology::virtual_screen_holding(),
            autostart: crate::autostart::is_enabled(),
            registration: crate::tasks::registration(),
            gpu: crate::hardware::gpus(),
            monitors: crate::hardware::monitors(),
        }
    }

    fn enter_guard(&self, cfg: &Config) -> NdResult<NightReport> {
        let ui = self.ui()?;
        let requested = cfg.mode;
        let effective = nd_core::mode::resolve(requested);
        let guard_cfg = cfg.guard.clone();
        let started = std::time::Instant::now();

        // 0) 恢复点先落盘，再动任何显示设置（断电/被杀都只会落在两次写盘之间）。
        if let Err(e) = Self::write_restore_point(false) {
            // 写不了恢复点就别开始：宁可这一夜没黑屏，也不能留下改不回来的屏幕。
            return Err(nd_core::error::user(format!(
                "无法写入屏幕恢复点，已放弃本次黑屏：{}",
                nd_core::error::one_line(&e)
            )));
        }

        // 1) 黑幕：必须在界面线程上显示并校验采集排除。
        let outcomes: Vec<OverlayOutcome> = ui.call(|state| state.overlays.show())?;
        let latency = started.elapsed().as_millis() as u64;
        let overlay_ok =
            !outcomes.is_empty() && outcomes.iter().all(nd_core::mode::OverlayOutcome::ok);
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
                virtual_screen: None,
                fallback_reason: None,
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
                    let mut guard = self
                        .brightness
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        //    注意这一步已经晚了（gamma 和背光在 2/3 步就改了），所以真正的
        //    恢复点是在 enter_guard 一开始写的那一份；这里只是把它补全。
        let mut state = crate::screen_state::load().unwrap_or_default();
        state.entered_at = Some(chrono::Local::now());
        for entry in displays_state {
            match state
                .displays
                .iter_mut()
                .find(|display| display.description == entry.description)
            {
                Some(existing) => existing.brightness = entry.brightness,
                None => state.displays.push(entry),
            }
        }
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
            virtual_screen: None,
            fallback_reason: None,
        })
    }

    fn exit_guard(&self) -> NdResult<()> {
        let mut errors: Vec<String> = Vec::new();
        if let Some(handle) = self
            .ui
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
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
        let mut guard = self
            .brightness
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = None;
        // 电源计划必须在清掉恢复点之前切回去。
        if let Err(e) = crate::power_plan::restore_saved() {
            errors.push(format!(
                "恢复电源计划失败：{}",
                crate::error::user_text(&e, "未知原因")
            ));
        }
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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
        let guard = self
            .brightness
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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

    fn enter_virtual_screen(&self, match_pat: &str, dry_run: bool) -> NdResult<VirtualReport> {
        // 1) 白天预热：让虚拟屏接进桌面（复制），不改布局、不动采集表面。
        //    这一步在白天就做完了，入夜才能真正"一次调用"。
        let armed = topology::arm(match_pat, dry_run)?;
        if !armed.applied && !dry_run {
            // 预热失败说明这台机器的驱动不接受复制/接管；
            // 直接把原因交回引擎，由它决定是否降级到黑幕。
            return Ok(armed);
        }
        if dry_run {
            return Ok(armed);
        }
        tracing::info!(summary = %armed.summary(), "虚拟屏已预热");

        // 2) 先写恢复数据，再动拓扑。
        //    顺序不能反：断电/被杀只会发生在两次写盘之间，那时恢复数据已经在了。
        let restore_point = Self::write_restore_point(true)?;

        // 3) 入夜：一次 SetDisplayConfig 摘掉实体屏的桌面。
        let report = topology::detach(match_pat, false)?;
        if report.applied {
            tracing::info!(
                active = report.active_after,
                summary = %report.summary(),
                "已把桌面交给虚拟屏"
            );
            if let Some(plan) = restore_point.power_plan.as_deref() {
                // 拓扑已经切过去了，再切电源计划；失败只警告，不影响夜间成立。
                if let Err(e) = crate::power_plan::set(HIGH_PERFORMANCE_PLAN) {
                    tracing::warn!(error = %e, plan, "夜间切换电源计划失败");
                } else {
                    tracing::info!(plan, "夜间已切到高性能电源计划");
                }
            }
        } else if report.error.is_some() {
            // 入夜这一步失败：把预热撤掉，回到原样，不留半成品。
            if let Err(e) = topology::reattach() {
                tracing::warn!(error = %nd_core::error::one_line(&e), "回滚预热时出错");
            }
            // 拓扑没变，恢复点也该撤掉，免得白天误跑一次还原。
            let mut state = restore_point;
            state.topology_changed = false;
            state.power_plan = None;
            if let Err(e) = crate::screen_state::save(&state) {
                tracing::warn!(error = %e, "回滚后重写恢复点失败");
            }
        }
        Ok(report)
    }

    fn exit_virtual_screen(&self) -> NdResult<()> {
        let report = topology::reattach()?;
        if !report.applied {
            // 路径集方式失败，restore() 会退到系统拓扑兜底。
            let level = topology::restore()?;
            tracing::warn!(level, "路径集还原失败，已用兜底方式还原");
        }
        // 拓扑还原之后再切电源计划：顺序反了会让显卡重新协商一次模式。
        if let Err(e) = crate::power_plan::restore_saved() {
            tracing::warn!(error = %e, "恢复电源计划失败");
        }
        crate::screen_state::clear();
        Ok(())
    }

    fn virtual_screen_ready(&self) -> bool {
        topology::standby_present()
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
        if on {
            // 三条自启路径一起开：HKCU Run、登录计划任务、开机（SYSTEM）计划任务。
            // 任一条失败都不算致命，但要把真实结果说清楚。
            crate::autostart::set(true, &home)?;
            if let Err(e) = tasks::install_autostart(&home) {
                tracing::warn!(error = %e, "登记自启计划任务失败，仅 Run 键可用");
                return Err(e);
            }
            Ok(())
        } else {
            // 关掉时必须三条一起删：只删 Run 的话，登录任务还会把它拉起来。
            let mut first_error = None;
            if let Err(e) = crate::autostart::set(false, &home) {
                first_error = Some(e);
            }
            if let Err(e) = tasks::remove_autostart() {
                tracing::warn!(error = %e, "删除自启计划任务失败");
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
            match first_error {
                Some(e) => Err(e),
                None => Ok(()),
            }
        }
    }

    fn autostart_actual(&self) -> NdResult<bool> {
        Ok(crate::autostart::is_enabled())
    }

    fn install_schedule(&self, cfg: &Config) -> NdResult<()> {
        let home = nd_core::paths::home_dir();
        tasks::install(cfg, &home)?;
        // 自启是按用户偏好来的：关掉了就一条都不登记，下次启动也不会偷偷加回来。
        if cfg.autostart_preference {
            if let Err(e) = tasks::install_autostart(&home) {
                tracing::warn!(error = %e, "登记自启任务失败（每日计划仍然有效）");
            }
        } else if let Err(e) = tasks::remove_autostart() {
            tracing::warn!(error = %e, "用户已关闭自启，清理残留自启任务失败");
        }
        // 用户偏好是关的时候，Run 键也不该在。
        if !cfg.autostart_preference {
            if let Err(e) = crate::autostart::set(false, &home) {
                tracing::warn!(error = %e, "用户已关闭自启，删除 Run 键失败");
            }
        }
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
        let displays = topology::describe(true).map_or_else(
            |_| "unknown".into(),
            |list| {
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
            },
        );
        format!("win{build}_{displays}")
    }
}

/// 一次性取能力快照（不起界面线程，CLI 的 `status`/`selfcheck` 用）。
#[must_use]
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
        virtual_display: topology::standby_present(),
        virtual_screen_holding: topology::virtual_screen_holding(),
        autostart: crate::autostart::is_enabled(),
        registration: crate::tasks::registration(),
        gpu: crate::hardware::gpus(),
        monitors: crate::hardware::monitors(),
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
