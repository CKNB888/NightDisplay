//! 同步引擎：所有"什么时候黑、怎么黑、怎么恢复"的判断都在这里。
//!
//! 平台动作通过 [`Platform`] 注入，所以这套状态机可以在 Linux CI 上完整测试，
//! 不需要真的黑屏。GUI（第二阶段 Tauri）只需要调用这里的公开方法，
//! 不直接碰任何 Windows API。

use chrono::{DateTime, Local, TimeDelta};
use serde::{Deserialize, Serialize};

use crate::chord::ChordKey;
use crate::config::{Config, Mode, RunState};
use crate::error::{one_line, user, NdResult};
use crate::mode::{attempt_chain, LayerStatus, NightReport, VirtualReport};
use crate::schedule::Schedule;
use crate::session::{steps_from_report, SessionRecord, StepRecord};
use crate::status::Capabilities;

/// 历史会话保留条数。一天最多两次（入夜/恢复），50 条能回溯二十多天。
const SESSION_KEEP: usize = 50;

/// 平台层必须提供的能力。Windows 实现见 `nd-win`；测试用假实现。
pub trait Platform {
    /// 当前环境能力，用于状态文本、自检和测试指纹。
    fn capabilities(&self) -> Capabilities;

    /// 进入防窥黑幕。返回的报告里带有每块屏的结果和首帧耗时。
    fn enter_guard(&self, cfg: &Config) -> NdResult<NightReport>;
    /// 退出防窥黑幕：销毁窗口、恢复 gamma、恢复背光。
    fn exit_guard(&self) -> NdResult<()>;
    /// 黑幕是否还在（用于夜间修复）。
    fn guard_active(&self) -> bool;
    /// 夜间修复：重新断言背光和黑幕置顶，不重建窗口。
    fn repair_guard(&self) -> NdResult<()>;

    /// 备份当前显示配置（虚拟屏模式用得到）。
    fn backup_display(&self) -> NdResult<()>;
    /// 把桌面交给虚拟屏，实体屏保持枚举但不带桌面。
    ///
    /// **必须是一次原子的 `SetDisplayConfig`**：先 `SDC_VALIDATE` 校验，
    /// 校验不过就什么都不做。成功时活动 target 数不能是 0。
    /// `dry_run` 只校验不应用。
    fn enter_virtual_screen(&self, match_pat: &str, dry_run: bool) -> NdResult<VirtualReport>;
    /// 把桌面交回实体屏。同样是一次原子切换。
    fn exit_virtual_screen(&self) -> NdResult<()>;
    /// 虚拟屏是否已经装好并在待命（不改动桌面布局）。
    fn virtual_screen_ready(&self) -> bool;

    /// 直接关屏 / 唤醒屏幕。
    fn screen_off(&self) -> NdResult<()>;
    fn screen_on(&self) -> NdResult<()>;

    /// 防睡眠。
    fn keep_awake(&self, on: bool, seconds: u64) -> NdResult<()>;

    /// 本地键鼠防触控开关。
    fn set_input_guard(&self, on: bool, chord: &[ChordKey], hold_ms: u64) -> NdResult<()>;
    /// 夜间抑制系统锁屏。
    fn set_lock_suppression(&self, on: bool) -> NdResult<()>;

    /// 写/删开机自启，并回读校验。
    fn set_autostart(&self, on: bool) -> NdResult<()>;
    fn autostart_actual(&self) -> NdResult<bool>;
    /// 注册/删除每日计划任务。
    fn install_schedule(&self, cfg: &Config) -> NdResult<()>;
    fn remove_schedule(&self) -> NdResult<()>;

    /// 起一个几秒后自动恢复的独立进程（测试用，窗口被关也会恢复）。
    fn spawn_delayed_day(&self, seconds: u64, test_id: &str) -> NdResult<()>;
    /// 起看门狗：主进程意外退出时几秒内把屏幕恢复。
    fn spawn_watchdog(&self) -> NdResult<()>;

    /// 能力指纹：硬件/驱动变了就不能沿用上次测试结果。
    fn capability_fingerprint(&self) -> String;
}

// 方便在常驻进程里把平台实现装进 `Arc`，也方便把引用传给引擎。
impl<P: Platform + ?Sized> Platform for &P {
    fn capabilities(&self) -> Capabilities {
        (**self).capabilities()
    }
    fn enter_guard(&self, cfg: &Config) -> NdResult<NightReport> {
        (**self).enter_guard(cfg)
    }
    fn exit_guard(&self) -> NdResult<()> {
        (**self).exit_guard()
    }
    fn guard_active(&self) -> bool {
        (**self).guard_active()
    }
    fn repair_guard(&self) -> NdResult<()> {
        (**self).repair_guard()
    }
    fn backup_display(&self) -> NdResult<()> {
        (**self).backup_display()
    }
    fn enter_virtual_screen(&self, match_pat: &str, dry_run: bool) -> NdResult<VirtualReport> {
        (**self).enter_virtual_screen(match_pat, dry_run)
    }
    fn exit_virtual_screen(&self) -> NdResult<()> {
        (**self).exit_virtual_screen()
    }
    fn virtual_screen_ready(&self) -> bool {
        (**self).virtual_screen_ready()
    }
    fn screen_off(&self) -> NdResult<()> {
        (**self).screen_off()
    }
    fn screen_on(&self) -> NdResult<()> {
        (**self).screen_on()
    }
    fn keep_awake(&self, on: bool, seconds: u64) -> NdResult<()> {
        (**self).keep_awake(on, seconds)
    }
    fn set_input_guard(&self, on: bool, chord: &[ChordKey], hold_ms: u64) -> NdResult<()> {
        (**self).set_input_guard(on, chord, hold_ms)
    }
    fn set_lock_suppression(&self, on: bool) -> NdResult<()> {
        (**self).set_lock_suppression(on)
    }
    fn set_autostart(&self, on: bool) -> NdResult<()> {
        (**self).set_autostart(on)
    }
    fn autostart_actual(&self) -> NdResult<bool> {
        (**self).autostart_actual()
    }
    fn install_schedule(&self, cfg: &Config) -> NdResult<()> {
        (**self).install_schedule(cfg)
    }
    fn remove_schedule(&self) -> NdResult<()> {
        (**self).remove_schedule()
    }
    fn spawn_delayed_day(&self, seconds: u64, test_id: &str) -> NdResult<()> {
        (**self).spawn_delayed_day(seconds, test_id)
    }
    fn spawn_watchdog(&self) -> NdResult<()> {
        (**self).spawn_watchdog()
    }
    fn capability_fingerprint(&self) -> String {
        (**self).capability_fingerprint()
    }
}

impl<P: Platform + ?Sized> Platform for std::sync::Arc<P> {
    fn capabilities(&self) -> Capabilities {
        (**self).capabilities()
    }
    fn enter_guard(&self, cfg: &Config) -> NdResult<NightReport> {
        (**self).enter_guard(cfg)
    }
    fn exit_guard(&self) -> NdResult<()> {
        (**self).exit_guard()
    }
    fn guard_active(&self) -> bool {
        (**self).guard_active()
    }
    fn repair_guard(&self) -> NdResult<()> {
        (**self).repair_guard()
    }
    fn backup_display(&self) -> NdResult<()> {
        (**self).backup_display()
    }
    fn enter_virtual_screen(&self, match_pat: &str, dry_run: bool) -> NdResult<VirtualReport> {
        (**self).enter_virtual_screen(match_pat, dry_run)
    }
    fn exit_virtual_screen(&self) -> NdResult<()> {
        (**self).exit_virtual_screen()
    }
    fn virtual_screen_ready(&self) -> bool {
        (**self).virtual_screen_ready()
    }
    fn screen_off(&self) -> NdResult<()> {
        (**self).screen_off()
    }
    fn screen_on(&self) -> NdResult<()> {
        (**self).screen_on()
    }
    fn keep_awake(&self, on: bool, seconds: u64) -> NdResult<()> {
        (**self).keep_awake(on, seconds)
    }
    fn set_input_guard(&self, on: bool, chord: &[ChordKey], hold_ms: u64) -> NdResult<()> {
        (**self).set_input_guard(on, chord, hold_ms)
    }
    fn set_lock_suppression(&self, on: bool) -> NdResult<()> {
        (**self).set_lock_suppression(on)
    }
    fn set_autostart(&self, on: bool) -> NdResult<()> {
        (**self).set_autostart(on)
    }
    fn autostart_actual(&self) -> NdResult<bool> {
        (**self).autostart_actual()
    }
    fn install_schedule(&self, cfg: &Config) -> NdResult<()> {
        (**self).install_schedule(cfg)
    }
    fn remove_schedule(&self) -> NdResult<()> {
        (**self).remove_schedule()
    }
    fn spawn_delayed_day(&self, seconds: u64, test_id: &str) -> NdResult<()> {
        (**self).spawn_delayed_day(seconds, test_id)
    }
    fn spawn_watchdog(&self) -> NdResult<()> {
        (**self).spawn_watchdog()
    }
    fn capability_fingerprint(&self) -> String {
        (**self).capability_fingerprint()
    }
}

/// 引擎执行结果，给 CLI 决定退出码、给 GUI 决定提示。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    /// 人类可读的一行结果。
    pub message: String,
    /// 实际使用的模式（发生切换时）。
    pub mode: Option<Mode>,
    /// 失败/部分失败的原因集合。
    pub errors: Vec<String>,
    /// 是否需要提示用户"恢复部分失败"。
    pub recovery_pending: bool,
}

impl Outcome {
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            mode: None,
            errors: Vec::new(),
            recovery_pending: false,
        }
    }

    #[must_use]
    pub fn with_mode(mut self, mode: Mode) -> Self {
        self.mode = Some(mode);
        self
    }

    pub fn failed(message: impl Into<String>, errors: Vec<String>) -> Self {
        Self {
            message: message.into(),
            mode: None,
            errors,
            recovery_pending: true,
        }
    }

    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// 引擎：配置 + 平台层。
pub struct Engine<'a, P: Platform> {
    pub platform: &'a P,
}

impl<'a, P: Platform> Engine<'a, P> {
    pub fn new(platform: &'a P) -> Self {
        Self { platform }
    }

    // ------------------------------------------------------------------
    // 模式解析与能力
    // ------------------------------------------------------------------

    #[must_use]
    pub fn capabilities(&self) -> Capabilities {
        self.platform.capabilities()
    }

    /// 校验用户选的模式在这台机器上能不能用。
    pub fn validate_mode(&self, mode: Mode) -> NdResult<Mode> {
        let caps = self.platform.capabilities();
        match mode {
            Mode::Auto | Mode::VirtualScreen => {
                if !caps.virtual_display {
                    return Err(user(
                        "没有检测到虚拟显示器。虚拟屏是默认模式的核心，\
                         请先在向导里安装虚拟显示驱动（第 2 步是必需项）。",
                    ));
                }
                Ok(Mode::VirtualScreen)
            }
            Mode::Overlay => {
                if !caps.capture_exclusion {
                    return Err(user(
                        "这台系统不支持把窗口排除在屏幕采集之外（需要 Windows 10 2004 及以上）。\
                         黑幕模式下远程会跟着变黑，所以这个模式不可用。",
                    ));
                }
                Ok(Mode::Overlay)
            }
            Mode::ScreenOff => Ok(Mode::ScreenOff),
        }
    }

    // ------------------------------------------------------------------
    // 夜间 / 白天
    // ------------------------------------------------------------------

    /// 进入夜间。成功才把状态写成夜间；失败保持原状态并返回中文原因。
    ///
    /// 模式顺序由 [`attempt_chain`] 决定：`auto` 先虚拟屏，不行才降级到黑幕，
    /// 并且**只有**这种情况下才会降级 —— 用户手动选过模式的不会被偷偷改掉。
    pub fn night(&self, mode: Mode, now: DateTime<Local>) -> NdResult<Outcome> {
        let cfg = Config::load()?;
        if cfg.state == RunState::Night {
            let current = cfg.night_mode.unwrap_or(Mode::Auto);
            if attempt_chain(mode).contains(&current) {
                return Ok(Outcome::ok("已经是夜间状态").with_mode(current));
            }
            return Err(user("请先恢复显示，再切换夜间模式"));
        }

        let chain = attempt_chain(mode);
        let trace_id = crate::session::new_trace_id();
        tracing::info!(
            trace_id = %trace_id,
            requested = mode.as_str(),
            chain = ?chain.iter().map(|m| m.as_str()).collect::<Vec<_>>(),
            "进入夜间"
        );

        let mut attempts: Vec<String> = Vec::new();
        let mut entered: Option<NightReport> = None;
        let mut effective = chain[0];
        let mut fallback_reason: Option<String> = None;

        for (index, candidate) in chain.iter().copied().enumerate() {
            // 能力不满足的模式直接跳过（例如没装 VDD 就选不了虚拟屏）。
            if let Err(e) = self.validate_mode(candidate) {
                attempts.push(format!("{} 不可用：{}", candidate.label(), one_line(&e)));
                if index == chain.len() - 1 {
                    return Err(user(attempts.join("；")));
                }
                fallback_reason = Some(one_line(&e));
                continue;
            }
            let started = std::time::Instant::now();
            let result = match candidate {
                Mode::VirtualScreen => self.enter_virtual_screen(&cfg),
                Mode::Overlay => self.enter_guard(&cfg),
                Mode::ScreenOff => self.platform.screen_off().map(|()| None),
                Mode::Auto => unreachable!("attempt_chain 不含 auto"),
            };
            match result {
                Ok(report) => {
                    let ok = report
                        .as_ref()
                        .is_none_or(super::mode::NightReport::success);
                    if ok {
                        effective = candidate;
                        entered = report;
                        break;
                    }
                    let reason = report
                        .as_ref()
                        .and_then(super::mode::NightReport::failure_reason)
                        .unwrap_or_else(|| "未知原因".into());
                    tracing::warn!(
                        mode = candidate.as_str(),
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        reason = %reason,
                        "这个模式没能确认成功"
                    );
                    // 黑幕没确认成功时必须拆掉，不能留一个盖住屏幕、
                    // 却没能排除采集的半成品。
                    if candidate == Mode::Overlay {
                        if let Err(revert) = self.platform.exit_guard() {
                            tracing::warn!(
                                error = %revert,
                                "拆掉未确认的黑幕时出错，请手动按 Win+P 或重启"
                            );
                        }
                    }
                    attempts.push(format!("{} 失败：{reason}", candidate.label()));
                    if index == chain.len() - 1 {
                        return Err(user(format!("{}。屏幕保持正常。", attempts.join("；"))));
                    }
                    fallback_reason = Some(reason);
                }
                Err(e) => {
                    tracing::warn!(
                        mode = candidate.as_str(),
                        error = %e,
                        "这个模式执行失败"
                    );
                    attempts.push(format!("{} 失败：{}", candidate.label(), one_line(&e)));
                    if index == chain.len() - 1 {
                        return Err(user(format!("{}。屏幕保持正常。", attempts.join("；"))));
                    }
                    fallback_reason = Some(one_line(&e));
                }
            }
        }

        let mut report = entered.unwrap_or_else(|| {
            // screen_off 路径没有报告，构造一个占位以便统一处理。
            NightReport {
                requested: mode,
                effective,
                virtual_screen: None,
                fallback_reason: fallback_reason.clone(),
                overlays: Vec::new(),
                gamma: LayerStatus::Disabled,
                brightness: LayerStatus::Disabled,
                local_input_guard: LayerStatus::Disabled,
                start_latency_ms: 0,
                brightness_unsupported_displays: Vec::new(),
            }
        });
        report.fallback_reason.clone_from(&fallback_reason);
        let latency = report.start_latency_ms;
        let summary = report.summary();

        // 夜间附加动作：保持唤醒、本地防触控、抑制锁屏。
        // 每一步的耗时和结果顺带记进会话记录，界面的「诊断」页直接读这份。
        let mut warnings = Vec::new();
        let mut steps: Vec<StepRecord> = steps_from_report(&report);
        let keep_secs = Self::keep_awake_seconds(&cfg, now);
        if cfg.keep_awake {
            let started = std::time::Instant::now();
            match self.platform.keep_awake(true, keep_secs) {
                Ok(()) => steps.push(StepRecord::ok("keep_awake", elapsed_ms(started))),
                Err(e) => {
                    warnings.push(format!("防睡眠没有生效：{}", one_line(&e)));
                    steps.push(StepRecord::failed(
                        "keep_awake",
                        elapsed_ms(started),
                        one_line(&e),
                    ));
                }
            }
        } else {
            steps.push(StepRecord::skipped("keep_awake", "设置里关闭了防睡眠"));
        }
        // 本地防触控和抑制锁屏只在黑幕模式有意义：
        // 虚拟屏接管后本机根本没有输出，物理键鼠也碰不到那块屏。
        if effective == Mode::Overlay {
            if cfg.strict_local_input {
                let started = std::time::Instant::now();
                match self
                    .platform
                    .set_input_guard(true, &cfg.unlock_chord, cfg.unlock_hold_ms)
                {
                    Ok(()) => steps.push(StepRecord::ok("input_hook", elapsed_ms(started))),
                    Err(e) => {
                        warnings.push(format!("严格本地防触控没有生效：{}", one_line(&e)));
                        steps.push(StepRecord::failed(
                            "input_hook",
                            elapsed_ms(started),
                            one_line(&e),
                        ));
                    }
                }
            } else {
                steps.push(StepRecord::skipped("input_hook", "设置里关闭了严格防触控"));
            }
            if cfg.suppress_lock {
                let started = std::time::Instant::now();
                if let Err(e) = self.platform.set_lock_suppression(true) {
                    warnings.push(format!("抑制锁屏没有生效：{}", one_line(&e)));
                    steps.push(StepRecord::failed(
                        "lock_suppression",
                        elapsed_ms(started),
                        one_line(&e),
                    ));
                } else {
                    steps.push(StepRecord::ok("lock_suppression", elapsed_ms(started)));
                }
            }
        } else {
            steps.push(StepRecord::skipped(
                "input_hook",
                "虚拟屏接管后本机没有输出，不需要按键拦截",
            ));
        }

        Config::update(|c| {
            c.state = RunState::Night;
            c.night_mode = Some(effective);
            c.recovery_pending = false;
            c.last_night_latency_ms = latency;
            Ok(())
        })?;
        // 白天手动进入夜间时记住保持到什么时候。
        self.note_manual_night(now)?;

        // 会话记录：先存"正在进行中"，这样进程被杀也留得下这一夜的证据；
        // 正常恢复时才追加进 sessions.jsonl 并清掉这份。
        let mut session = SessionRecord::start(mode, effective, &trace_id);
        session.steps = steps;
        crate::session::save_current(&session);
        tracing::info!(
            trace_id = %trace_id,
            steps = session.steps.len(),
            failures = session.failures(),
            "夜间会话已开始"
        );

        let mut msg = summary;
        if !warnings.is_empty() {
            msg.push('；');
            msg.push_str(&warnings.join("；"));
        }
        let mut outcome = Outcome::ok(msg).with_mode(effective);
        outcome.errors = warnings;
        Ok(outcome)
    }

    fn enter_guard(&self, cfg: &Config) -> NdResult<Option<NightReport>> {
        let report = self.platform.enter_guard(cfg)?;
        tracing::info!(success = report.success(), summary = %report.summary(), "防窥黑幕");
        Ok(Some(report))
    }

    /// 虚拟屏接管桌面。
    ///
    /// 先备份一份当前拓扑（真出事还能手工还原），再交给平台层做**一次**
    /// `SetDisplayConfig`。失败时不留下任何半完成的中间状态。
    fn enter_virtual_screen(&self, cfg: &Config) -> NdResult<Option<NightReport>> {
        self.platform.backup_display()?;
        let report = self
            .platform
            .enter_virtual_screen(&cfg.virtual_match, false)?;
        tracing::info!(
            success = report.applied,
            active_before = report.active_before,
            active_after = report.active_after,
            summary = %report.summary(),
            "虚拟屏接管桌面"
        );
        Ok(Some(NightReport {
            requested: Mode::Auto,
            effective: Mode::VirtualScreen,
            virtual_screen: Some(report),
            fallback_reason: None,
            overlays: Vec::new(),
            gamma: LayerStatus::Disabled,
            brightness: LayerStatus::Disabled,
            local_input_guard: LayerStatus::Disabled,
            start_latency_ms: 0,
            brightness_unsupported_displays: Vec::new(),
        }))
    }

    /// 恢复白天。返回是否完全成功；部分失败时把 `recovery_pending` 置上。
    pub fn day(&self) -> NdResult<Outcome> {
        let cfg = Config::load()?;
        let last = cfg.night_mode;
        tracing::info!(night_mode = ?last, "恢复白天显示");
        let mut errors: Vec<String> = Vec::new();

        if cfg.state == RunState::Night || last.is_some() {
            match last {
                Some(Mode::Overlay) => {
                    if let Err(e) = self.platform.exit_guard() {
                        errors.push(format!("关闭黑幕失败：{}", one_line(&e)));
                    }
                }
                Some(Mode::VirtualScreen) => {
                    if let Err(e) = self.platform.exit_virtual_screen() {
                        errors.push(format!("还原显示配置失败：{}", one_line(&e)));
                    }
                }
                _ => {}
            }
        }
        if let Err(e) = self.platform.set_input_guard(false, &[], 0) {
            errors.push(format!("解除本地防触控失败：{}", one_line(&e)));
        }
        if let Err(e) = self.platform.set_lock_suppression(false) {
            errors.push(format!("恢复锁屏设置失败：{}", one_line(&e)));
        }
        if let Err(e) = self.platform.screen_on() {
            errors.push(format!("唤醒屏幕失败：{}", one_line(&e)));
        }
        if let Err(e) = self.platform.keep_awake(false, 0) {
            errors.push(format!("停止防睡眠失败：{}", one_line(&e)));
        }

        archive_session(&errors);

        if errors.is_empty() {
            Config::update(|c| {
                c.state = RunState::Day;
                c.night_mode = None;
                c.recovery_pending = false;
                c.hold_night_until = None;
                Ok(())
            })?;
            Ok(Outcome::ok("已恢复白天显示"))
        } else {
            Config::update(|c| {
                c.recovery_pending = true;
                Ok(())
            })?;
            let text = errors.join(" | ");
            tracing::warn!(%text, "恢复警告（部分失败）");
            Ok(Outcome::failed("恢复部分失败", errors))
        }
    }

    /// 夜间修复：进程重启、显示器变化、远程软件顶回实体屏时调用。
    pub fn ensure_night(&self, now: DateTime<Local>) -> NdResult<Outcome> {
        let cfg = Config::load()?;
        if cfg.state != RunState::Night || cfg.recovery_pending {
            return Ok(Outcome::ok("当前不在夜间"));
        }
        match cfg.night_mode {
            Some(Mode::Overlay) => {
                if self.platform.guard_active() {
                    self.platform.repair_guard()?;
                } else {
                    tracing::warn!("黑幕不在了，重新创建");
                    let report = self.platform.enter_guard(&cfg)?;
                    if !report.success() {
                        let _ = self.platform.exit_guard();
                        return Err(user(format!(
                            "重建黑幕失败：{}",
                            report.failure_reason().unwrap_or_default()
                        )));
                    }
                }
            }
            Some(Mode::VirtualScreen) => {
                // 虚拟屏接管后桌面本来就不在实体屏上；
                // 只有在实体屏又拿回桌面时（远程软件、Win+P、驱动重置）才需要重新切一次。
                if virtual_screen_lost(&self.platform) {
                    tracing::warn!("实体屏重新拿到了桌面，重新把桌面交给虚拟屏");
                    let report = self
                        .platform
                        .enter_virtual_screen(&cfg.virtual_match, false)?;
                    if !report.applied {
                        return Err(user(format!(
                            "重新切换虚拟屏失败：{}",
                            report.error.unwrap_or_else(|| "未知原因".into())
                        )));
                    }
                }
            }
            Some(Mode::ScreenOff) => {
                // 关屏只发一次的话，键鼠点亮后会一直亮着，所以每次检查都补一次。
                if let Err(e) = self.platform.screen_off() {
                    tracing::warn!(error = %e, "再次关屏未成功，保持夜间");
                }
            }
            // 虚拟屏和黑幕都在上面各自处理；其余情况（包括 auto）
            // 运行时不会作为 night_mode 落盘。
            None | Some(Mode::Auto) => {}
        }
        if cfg.keep_awake {
            let secs = Self::keep_awake_seconds(&cfg, now);
            if let Err(e) = self.platform.keep_awake(true, secs) {
                tracing::warn!(error = %e, "续订防睡眠失败");
            }
        }
        Ok(Outcome::ok("夜间状态正常"))
    }

    fn keep_awake_seconds(cfg: &Config, now: DateTime<Local>) -> u64 {
        let schedule = cfg.schedule();
        let secs = if cfg.enabled {
            schedule.seconds_left_in_night(now).max(0) + 600
        } else {
            6 * 3600
        };
        secs.clamp(60, 12 * 3600) as u64
    }

    // ------------------------------------------------------------------
    // 解锁与手动保持
    // ------------------------------------------------------------------

    /// 组合键解锁：恢复白天，并且本段剩余时间不再自动黑屏。
    pub fn chord_unlock(&self, now: DateTime<Local>) -> NdResult<Outcome> {
        let cfg = Config::load()?;
        Self::remember_explicit_day(&cfg, now)?;
        let mut outcome = self.day()?;
        Config::update(|c| {
            c.test_id.clear();
            c.last_test_ok = false;
            c.hold_night_until = None;
            Ok(())
        })?;
        if outcome.is_ok() {
            outcome.message = format!("已通过组合键 {} 解除黑屏", cfg.chord_text());
        }
        tracing::info!(chord = %cfg.chord_text(), "已通过组合键解除黑屏");
        Ok(outcome)
    }

    /// 用户明确要求回白天时，记录"本段剩余时间保持白天"。
    fn remember_explicit_day(cfg: &Config, now: DateTime<Local>) -> NdResult<()> {
        let mut override_until = None;
        if cfg.enabled && cfg.schedule().is_night_at(now) {
            let left = cfg.schedule().seconds_left_in_night(now).max(1);
            override_until = Some(now + TimeDelta::seconds(left));
        }
        Config::update(|c| {
            c.hold_night_until = None;
            c.override_until = override_until;
            Ok(())
        })?;
        Ok(())
    }

    /// 白天手动进入夜间后，保持到下一次计划夜间结束。
    pub fn note_manual_night(&self, now: DateTime<Local>) -> NdResult<()> {
        let cfg = Config::load()?;
        if !(cfg.enabled && cfg.state == RunState::Night) || cfg.schedule().is_night_at(now) {
            if cfg.hold_night_until.is_some() {
                Config::update(|c| {
                    c.hold_night_until = None;
                    Ok(())
                })?;
            }
            return Ok(());
        }
        let hold = manual_night_until(&cfg.schedule(), cfg.enabled, now);
        Config::update(|c| {
            c.hold_night_until = hold;
            Ok(())
        })?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // 定时检查（常驻进程的核心循环）
    // ------------------------------------------------------------------

    /// 按当前时间把状态对齐到计划。返回动作说明，便于日志和 GUI 显示。
    pub fn reconcile(&self, now: DateTime<Local>) -> NdResult<Outcome> {
        let mut cfg = Config::load()?;

        if cfg.recovery_pending {
            // 恢复没做完时，设定时段内不能改成亮屏；时段外才把屏幕拉回来。
            let in_night = cfg.enabled && cfg.schedule().is_night_at(now);
            if in_night && !unlock_active(&cfg, now) {
                Config::update(|c| {
                    c.recovery_pending = false;
                    Ok(())
                })?;
                cfg.recovery_pending = false;
            } else {
                let _ = self.day()?;
                cfg = Config::load()?;
            }
        }

        if !cfg.enabled {
            return Ok(Outcome::ok("每日计划未启用"));
        }

        let mut want_night = cfg.schedule().is_night_at(now);
        let mut hold_cleared = false;

        if let Some(until) = cfg.hold_night_until {
            if now < until {
                want_night = true;
            } else {
                hold_cleared = true;
            }
        }
        let mut override_cleared = false;
        if let Some(until) = cfg.override_until {
            if now < until {
                want_night = false;
            } else {
                override_cleared = true;
            }
        }
        if hold_cleared || override_cleared {
            Config::update(|c| {
                if hold_cleared {
                    c.hold_night_until = None;
                }
                if override_cleared {
                    c.override_until = None;
                }
                Ok(())
            })?;
        }

        if want_night {
            if cfg.state == RunState::Night {
                return self.ensure_night(now);
            }
            return self.night(cfg.mode, now);
        }

        if cfg.state == RunState::Night {
            return self.day();
        }
        Ok(Outcome::ok("白天，无需操作"))
    }

    // ------------------------------------------------------------------
    // 测试 / 启用 / 暂停 / 应急 / 卸载
    // ------------------------------------------------------------------

    /// 开始一次限时测试：先起独立恢复进程，再进入夜间。
    pub fn start_test(&self, mode: Mode, seconds: u64, now: DateTime<Local>) -> NdResult<Outcome> {
        let cfg = Config::load()?;
        if cfg.enabled {
            return Err(user("请先暂停每天自动黑屏，再测试。"));
        }
        if cfg.state == RunState::Night || !cfg.test_id.is_empty() {
            return Err(user(
                "屏幕还黑着，或者上一次测试还没结束。等它自己亮回来再试。",
            ));
        }
        if !(10..=3600).contains(&seconds) {
            return Err(user("测试时长应为 10 到 3600 秒"));
        }
        let effective = self.validate_mode(mode)?;
        let test_id = crate::ipc::new_token();
        let fingerprint = self.platform.capability_fingerprint();
        Config::update(|c| {
            c.test_id.clone_from(&test_id);
            c.last_test_requested = Some(mode);
            c.last_test_mode = Some(effective);
            c.last_test_capability = fingerprint;
            c.last_test_at = Some(now);
            c.last_test_ok = false;
            c.last_recovery_at = None;
            c.last_recovery_mode = None;
            Ok(())
        })?;

        // 先起独立恢复进程，即使 GUI 关掉、进程崩掉也会自己亮回来。
        if let Err(e) = self.platform.spawn_delayed_day(seconds, &test_id) {
            Config::update(|c| {
                c.test_id.clear();
                c.last_test_ok = false;
                Ok(())
            })?;
            return Err(user(format!("无法启动自动恢复进程：{}", one_line(&e))));
        }
        let result = self.night(mode, now);
        match result {
            Ok(outcome) => {
                Config::update(|c| {
                    c.last_test_mode = outcome.mode.or(Some(effective));
                    Ok(())
                })?;
                tracing::info!(seconds, mode = effective.as_str(), "测试开始");
                Ok(Outcome::ok(format!(
                    "测试开始（{}），{seconds} 秒后自动恢复",
                    effective.label()
                ))
                .with_mode(effective))
            }
            Err(e) => {
                Config::update(|c| {
                    c.test_id.clear();
                    c.last_test_ok = false;
                    Ok(())
                })?;
                Err(e)
            }
        }
    }

    /// 测试的恢复阶段：只有确认显示真的回来了才算测试通过。
    pub fn finish_test(&self, test_id: &str, now: DateTime<Local>) -> NdResult<Outcome> {
        let cfg = Config::load()?;
        if !test_id.is_empty() && cfg.test_id != test_id {
            return Ok(Outcome::ok("上一次测试已被替换，忽略"));
        }
        if cfg.test_id.is_empty() {
            return Ok(Outcome::ok("没有进行中的测试"));
        }
        let mode_tested = cfg.last_test_mode;
        let mut outcome = self.day()?;
        let recovered = outcome.is_ok();
        Config::update(|c| {
            c.test_id.clear();
            c.last_test_ok = recovered && c.state == RunState::Day;
            if recovered {
                c.last_recovery_at = Some(now);
                c.last_recovery_mode = mode_tested;
            } else {
                c.last_recovery_at = None;
                c.last_recovery_mode = None;
            }
            Ok(())
        })?;
        if recovered {
            outcome.message =
                "测试结束，屏幕已恢复。请确认远程画面正常后点「启用每天自动黑屏」。".into();
        } else {
            outcome.message =
                "测试结束，但屏幕没有完全恢复，已标记为恢复失败；请点「应急恢复」。".into();
        }
        Ok(outcome)
    }

    /// 启用每日计划。必须有一次 24 小时内、同一硬件上成功的测试。
    pub fn activate(
        &self,
        mode: Mode,
        start: &str,
        end: &str,
        now: DateTime<Local>,
    ) -> NdResult<Outcome> {
        use crate::schedule::MinuteOfDay;
        let start_m: MinuteOfDay = start.parse()?;
        let end_m: MinuteOfDay = end.parse()?;
        if start_m == end_m {
            return Err(user("开始和结束时间不能相同"));
        }
        let mut cfg = Config::load()?;
        let effective = self.validate_mode(mode)?;
        let fingerprint = self.platform.capability_fingerprint();

        let tested = cfg.last_test_mode;
        let same_hardware =
            !cfg.last_test_capability.is_empty() && cfg.last_test_capability == fingerprint;
        if !(cfg.last_test_ok
            && tested == Some(effective)
            && same_hardware
            && cfg.last_recovery_mode == Some(effective)
            && cfg.last_recovery_at.is_some())
        {
            return Err(user(
                "请先点「测试 15 秒」完成一次测试，等屏幕自己亮回来，再启用每天自动黑屏。",
            ));
        }
        let age = cfg.last_test_at.map_or(-1, |t| (now - t).num_seconds());
        if !(0..=86_400).contains(&age) {
            return Err(user(
                "上次测试已经超过一天。请再点一次「测试 15 秒」，等屏幕自己亮回来，然后再启用。",
            ));
        }

        // 先按新配置写系统计划，成功了才落盘，失败就保持原样。
        cfg.ranges[0].0.start = start_m;
        cfg.ranges[0].0.end = end_m;
        cfg.mode = effective;
        cfg.autostart = cfg.autostart_preference;
        let candidate = cfg.clone();
        if let Err(e) = self.platform.install_schedule(&candidate) {
            let _ = self.platform.remove_schedule();
            return Err(user(format!("写入系统计划任务失败：{}", one_line(&e))));
        }
        let (s, e2) = (start_m.to_hm(), end_m.to_hm());
        Config::update(|c| {
            c.mode = effective;
            c.ranges[0].0.start = start_m;
            c.ranges[0].0.end = end_m;
            c.enabled = true;
            c.autostart = c.autostart_preference;
            Ok(())
        })?;
        tracing::info!(%s, end = %e2, mode = effective.as_str(), "已启用每日计划");
        Ok(Outcome::ok(format!(
            "已启用每日计划：{s} → {e2}，模式 {}",
            effective.label()
        ))
        .with_mode(effective))
    }

    /// 暂停每日计划：不动当前显示，也不删登录自启。
    pub fn pause(&self) -> NdResult<Outcome> {
        Config::update(|c| {
            c.enabled = false;
            c.hold_night_until = None;
            Ok(())
        })?;
        self.platform.remove_schedule()?;
        tracing::info!("已暂停每日计划（不改变当前显示，也不删除登录自启）");
        Ok(Outcome::ok(
            "已暂停每日计划（不改变当前显示，也不删除登录自启）",
        ))
    }

    /// 应急恢复：立刻结束黑屏并暂停计划。可以远程执行。
    pub fn emergency(&self) -> NdResult<Outcome> {
        let mut errors = Vec::new();
        if let Err(e) = self.pause() {
            errors.push(format!("暂停计划失败：{}", one_line(&e)));
        }
        // 应急只结束这一次黑屏和每日计划，登录自启按用户偏好保留。
        let pref = Config::load()?.autostart_preference;
        if let Err(e) = self.platform.set_autostart(pref) {
            errors.push(format!("恢复开机自启设置失败：{}", one_line(&e)));
        }
        Config::update(|c| {
            c.test_id.clear();
            c.last_test_ok = false;
            Ok(())
        })?;
        match self.day() {
            Ok(outcome) => {
                if !outcome.is_ok() {
                    errors.extend(outcome.errors.clone());
                }
            }
            Err(e) => errors.push(one_line(&e)),
        }
        if errors.is_empty() {
            tracing::info!(
                "应急恢复完成。如仍黑屏：按 Win+P 选“仅电脑屏幕”，或用另一台电脑远程执行 --action emergency"
            );
            Ok(Outcome::ok(
                "应急恢复完成。如仍黑屏：按 Win+P 选“仅电脑屏幕”，或在远程再执行一次 --action emergency",
            ))
        } else {
            Ok(Outcome::failed("应急恢复部分失败", errors))
        }
    }

    /// 卸载：应急恢复 + 删掉所有自启/计划项 + 关掉自启偏好。
    pub fn uninstall(&self) -> NdResult<Outcome> {
        let mut outcome = self.emergency()?;
        Config::update(|c| {
            c.autostart_preference = false;
            c.autostart = false;
            Ok(())
        })?;
        if let Err(e) = self.platform.set_autostart(false) {
            outcome
                .errors
                .push(format!("关闭开机自启失败：{}", one_line(&e)));
        }
        if let Err(e) = self.platform.remove_schedule() {
            outcome
                .errors
                .push(format!("删除计划任务失败：{}", one_line(&e)));
        }
        outcome.message = format!(
            "已卸载计划任务和自启项；日志与备份保留在 {}",
            crate::paths::home_dir().display()
        );
        Ok(outcome)
    }
}

/// 虚拟屏模式是否已经掉了：实体屏上又有桌面了。
///
/// 只看"当前活动输出里还有没有实体屏"，不主动改任何东西。
/// 把进行中的会话收尾。
///
/// 没有进行中的会话（例如白天只是重复点了一次恢复）就什么都不做。
/// 会话记录永远不该阻止屏幕恢复，所以这里的失败只写日志。
fn archive_session(errors: &[String]) {
    let Some(mut session) = crate::session::load_current() else {
        return;
    };
    session.ended_at = Some(Local::now());
    session.ended_cleanly = errors.is_empty();
    session.outcome = if errors.is_empty() {
        "已恢复白天显示".to_string()
    } else {
        errors.join(" | ")
    };
    session.push(if errors.is_empty() {
        StepRecord::ok("restore", 0)
    } else {
        StepRecord::failed("restore", 0, session.outcome.clone())
    });
    tracing::info!(
        trace_id = %session.trace_id,
        clean = session.ended_cleanly,
        duration_ms = session.duration_ms(),
        "夜间会话结束"
    );
    crate::session::finish(&session);
    crate::session::trim(SESSION_KEEP);
}

/// 一段动作的耗时（毫秒）。取整到 u64，超长也不可能溢出。
fn elapsed_ms(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn virtual_screen_lost(platform: &impl Platform) -> bool {
    if !platform.virtual_screen_ready() {
        return false;
    }
    !platform.capabilities().virtual_screen_holding
}

/// 组合键解锁是否仍然有效（本段剩余时间保持白天）。
#[must_use]
pub fn unlock_active(cfg: &Config, now: DateTime<Local>) -> bool {
    match cfg.override_until {
        Some(until) => now < until,
        None => false,
    }
}

/// 白天手动进入夜间时，应该保持到什么时候。
///
/// 当前已在计划夜间内、或没有下一段夜间时返回 `None`。
#[must_use]
pub fn manual_night_until(
    schedule: &Schedule,
    enabled: bool,
    now: DateTime<Local>,
) -> Option<DateTime<Local>> {
    if !enabled || schedule.is_night_at(now) {
        return None;
    }
    let Some((true, secs)) = schedule.next_switch(now) else {
        return None;
    };
    // 只跨过边界 1 秒，避免跳过 20:00–20:01 这种合法短时段。
    let entered = now + TimeDelta::seconds(secs + 1);
    let Some((false, secs2)) = schedule.next_switch(entered) else {
        return None;
    };
    Some(entered + TimeDelta::seconds(secs2))
}

#[cfg(test)]
pub mod testing {
    //! 假平台层，供引擎测试使用。

    use super::*;
    use crate::mode::{LayerStatus, OverlayOutcome};
    use std::cell::RefCell;

    #[derive(Default)]
    pub struct FakePlatform {
        pub caps: Capabilities,
        pub guard_result: RefCell<Option<anyhow::Error>>,
        pub capture_excluded: RefCell<bool>,
        pub calls: RefCell<Vec<String>>,
        pub guard_active: RefCell<bool>,
        pub schedule_fails: RefCell<bool>,
        pub day_fails: RefCell<bool>,
        pub fingerprint: RefCell<String>,
        /// 虚拟屏切换是否失败（模拟这台机器不支持）。
        pub virtual_fails: RefCell<Option<String>>,
        /// 虚拟屏是否已经被切走（`capabilities().virtual_screen_holding`）。
        pub virtual_holding: RefCell<bool>,
        /// 虚拟屏驱动是否装好了。
        pub vdd_present: RefCell<bool>,
    }

    impl FakePlatform {
        pub fn new() -> Self {
            let mut caps = Capabilities {
                os: "test".into(),
                build: 19045,
                capture_exclusion: true,
                gamma_control: true,
                brightness_displays: 1,
                displays: 1,
                elevated: false,
                helper_running: false,
                virtual_display: false,
                virtual_screen_holding: false,
                autostart: true,
                registration: crate::status::Registration::default(),
                gpu: Vec::new(),
                monitors: Vec::new(),
            };
            caps.build = 19045;
            Self {
                caps,
                capture_excluded: RefCell::new(true),
                fingerprint: RefCell::new("fp-1".into()),
                ..Default::default()
            }
        }

        pub fn record(&self, what: &str) {
            self.calls.borrow_mut().push(what.to_string());
        }

        pub fn called(&self, what: &str) -> bool {
            self.calls.borrow().iter().any(|c| c == what)
        }

        pub fn call_count(&self, what: &str) -> usize {
            self.calls.borrow().iter().filter(|c| *c == what).count()
        }
    }

    impl Platform for FakePlatform {
        fn capabilities(&self) -> Capabilities {
            let mut caps = self.caps.clone();
            caps.virtual_screen_holding = *self.virtual_holding.borrow();
            caps
        }

        fn enter_guard(&self, _cfg: &Config) -> NdResult<NightReport> {
            self.record("enter_guard");
            if let Some(e) = self.guard_result.borrow().as_ref() {
                return Err(anyhow::anyhow!("{e}"));
            }
            *self.guard_active.borrow_mut() = true;
            Ok(NightReport {
                requested: Mode::Overlay,
                effective: Mode::Overlay,
                overlays: vec![OverlayOutcome {
                    display: "DISPLAY1".into(),
                    shown: true,
                    capture_excluded: *self.capture_excluded.borrow(),
                    first_frame_ms: 120,
                    error: None,
                }],
                gamma: LayerStatus::Applied,
                brightness: LayerStatus::Applied,
                local_input_guard: LayerStatus::Applied,
                start_latency_ms: 130,
                brightness_unsupported_displays: vec![],
                virtual_screen: None,
                fallback_reason: None,
            })
        }

        fn exit_guard(&self) -> NdResult<()> {
            self.record("exit_guard");
            *self.guard_active.borrow_mut() = false;
            if *self.day_fails.borrow() {
                return Err(user("假平台的关闭黑幕失败"));
            }
            Ok(())
        }

        fn guard_active(&self) -> bool {
            *self.guard_active.borrow()
        }

        fn repair_guard(&self) -> NdResult<()> {
            self.record("repair_guard");
            Ok(())
        }

        fn backup_display(&self) -> NdResult<()> {
            self.record("backup_display");
            Ok(())
        }

        fn enter_virtual_screen(&self, _p: &str, dry_run: bool) -> NdResult<VirtualReport> {
            self.record(if dry_run {
                "plan_virtual"
            } else {
                "virtual_screen"
            });
            if let Some(reason) = self.virtual_fails.borrow().clone() {
                return Ok(VirtualReport::failure(reason));
            }
            if let Some(reason) = self.guard_result.borrow().as_ref() {
                return Err(anyhow::anyhow!("{reason}"));
            }
            *self.virtual_holding.borrow_mut() = true;
            Ok(VirtualReport {
                applied: !dry_run,
                virtual_display: "VIRTUAL1".into(),
                detached_physical: usize::from(!dry_run),
                active_before: 2,
                active_after: 1,
                mode_matched: true,
                validate_ms: 3,
                apply_ms: 40,
                settle_ms: 150,
                error: None,
            })
        }

        fn exit_virtual_screen(&self) -> NdResult<()> {
            self.record("restore_display");
            *self.virtual_holding.borrow_mut() = false;
            if *self.day_fails.borrow() {
                return Err(user("假平台的还原显示失败"));
            }
            Ok(())
        }

        fn virtual_screen_ready(&self) -> bool {
            *self.vdd_present.borrow()
        }

        fn screen_off(&self) -> NdResult<()> {
            self.record("screen_off");
            Ok(())
        }

        fn screen_on(&self) -> NdResult<()> {
            self.record("screen_on");
            Ok(())
        }

        fn keep_awake(&self, on: bool, _s: u64) -> NdResult<()> {
            self.record(if on {
                "keep_awake_on"
            } else {
                "keep_awake_off"
            });
            Ok(())
        }

        fn set_input_guard(&self, on: bool, _c: &[ChordKey], _h: u64) -> NdResult<()> {
            self.record(if on {
                "input_guard_on"
            } else {
                "input_guard_off"
            });
            Ok(())
        }

        fn set_lock_suppression(&self, on: bool) -> NdResult<()> {
            self.record(if on {
                "lock_suppress_on"
            } else {
                "lock_suppress_off"
            });
            Ok(())
        }

        fn set_autostart(&self, on: bool) -> NdResult<()> {
            self.record(if on { "autostart_on" } else { "autostart_off" });
            Ok(())
        }

        fn autostart_actual(&self) -> NdResult<bool> {
            Ok(true)
        }

        fn install_schedule(&self, _cfg: &Config) -> NdResult<()> {
            self.record("install_schedule");
            if *self.schedule_fails.borrow() {
                return Err(user("假平台的计划任务写入失败"));
            }
            Ok(())
        }

        fn remove_schedule(&self) -> NdResult<()> {
            self.record("remove_schedule");
            Ok(())
        }

        fn spawn_delayed_day(&self, _s: u64, _id: &str) -> NdResult<()> {
            self.record("spawn_delayed_day");
            Ok(())
        }

        fn spawn_watchdog(&self) -> NdResult<()> {
            self.record("spawn_watchdog");
            Ok(())
        }

        fn capability_fingerprint(&self) -> String {
            self.fingerprint.borrow().clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::FakePlatform;
    use super::*;
    use chrono::TimeZone;

    /// 2026-10-29 当天的某个时刻。
    fn at(h: u32, m: u32, s: u32) -> DateTime<Local> {
        at_day(29, h, m, s)
    }

    /// 2026-10-{day} 的某个时刻。
    fn at_day(day: u32, h: u32, m: u32, s: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 10, day, h, m, s)
            .single()
            .unwrap()
    }

    /// 所有测试共享同一个 NIGHTDISPLAY_HOME，所以统一在一个互斥锁里跑。
    fn with_home<T>(f: impl FnOnce() -> T) -> T {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("NIGHTDISPLAY_HOME", dir.path());
        let out = f();
        std::env::remove_var("NIGHTDISPLAY_HOME");
        out
    }

    fn seed(cfg: &Config) {
        crate::paths::ensure_home().unwrap();
        crate::config::write_to(&crate::paths::config_file(), cfg).unwrap();
    }

    #[test]
    fn night_then_day_round_trip() {
        with_home(|| {
            seed(&Config::default());
            let mut p = FakePlatform::new();
            *p.vdd_present.borrow_mut() = true;
            p.caps.virtual_display = true;
            let e = Engine::new(&p);
            let out = e.night(Mode::Auto, at(21, 0, 0)).unwrap();
            assert_eq!(out.mode, Some(Mode::VirtualScreen));
            assert!(p.called("virtual_screen"));
            assert!(p.called("backup_display"));
            assert!(!p.called("enter_guard"), "默认模式不该先用黑幕");
            assert!(p.called("keep_awake_on"));
            // 虚拟屏接管后本机没有输出，本地防触控和抑制锁屏都不需要。
            assert!(!p.called("input_guard_on"));
            assert!(!p.called("lock_suppress_on"));
            let cfg = Config::load().unwrap();
            assert_eq!(cfg.state, RunState::Night);
            assert_eq!(cfg.night_mode, Some(Mode::VirtualScreen));

            let out = e.day().unwrap();
            assert!(out.is_ok());
            assert!(p.called("restore_display"));
            assert_eq!(Config::load().unwrap().state, RunState::Day);
        });
    }

    #[test]
    fn night_writes_a_session_record_and_day_archives_it() {
        with_home(|| {
            seed(&Config::default());
            let mut p = FakePlatform::new();
            *p.vdd_present.borrow_mut() = true;
            p.caps.virtual_display = true;
            let e = Engine::new(&p);

            e.night(Mode::Auto, at(21, 0, 0)).unwrap();
            // 夜里进程随时可能被杀，所以"进行中"必须先落盘。
            let current = crate::session::load_current().expect("夜间没有写进行中的会话");
            assert_eq!(current.effective, Mode::VirtualScreen);
            assert_eq!(current.trace_id.len(), 16);
            assert!(current.ended_at.is_none());
            assert!(current.steps.iter().any(|s| s.step == "topology"));

            e.day().unwrap();
            assert!(
                crate::session::load_current().is_none(),
                "正常恢复后不该留下进行中的会话"
            );
            let history = crate::session::recent(5);
            assert_eq!(history.len(), 1);
            let session = &history[0];
            assert!(session.ended_cleanly);
            assert_eq!(session.trace_id, current.trace_id);
            assert!(session.steps.iter().any(|s| s.step == "restore" && s.ok));
        });
    }

    #[test]
    fn auto_falls_back_to_overlay_when_virtual_fails_and_says_so() {
        with_home(|| {
            seed(&Config::default());
            let mut p = FakePlatform::new();
            *p.vdd_present.borrow_mut() = true;
            p.caps.virtual_display = true;
            // 驱动拒绝这次切换（校验没过）。
            *p.virtual_fails.borrow_mut() = Some("显卡驱动不接受这个显示配置（配置无效）".into());
            let e = Engine::new(&p);
            let out = e.night(Mode::Auto, at(21, 0, 0)).unwrap();
            assert_eq!(out.mode, Some(Mode::Overlay), "自动模式必须兜底到黑幕");
            assert!(p.called("enter_guard"));
            // 必须明确告诉用户为什么降级了，不能静默替换机制。
            assert!(out.message.contains("降级"), "{}", out.message);
            assert!(out.message.contains("虚拟屏"), "{}", out.message);
            let cfg = Config::load().unwrap();
            assert_eq!(cfg.night_mode, Some(Mode::Overlay));
        });
    }

    #[test]
    fn explicit_virtual_screen_is_never_silently_replaced() {
        with_home(|| {
            seed(&Config::default());
            let mut p = FakePlatform::new();
            *p.vdd_present.borrow_mut() = true;
            p.caps.virtual_display = true;
            *p.virtual_fails.borrow_mut() = Some("显卡驱动不接受这个显示配置（配置无效）".into());
            let e = Engine::new(&p);
            let err = e
                .night(Mode::VirtualScreen, at(21, 0, 0))
                .unwrap_err()
                .to_string();
            assert!(err.contains("虚拟屏"), "{err}");
            assert!(
                !p.called("enter_guard"),
                "用户明确选了虚拟屏就不能偷偷换成黑幕"
            );
            assert_eq!(Config::load().unwrap().state, RunState::Day);
        });
    }

    #[test]
    fn night_fails_when_capture_exclusion_missing_and_does_not_claim_success() {
        with_home(|| {
            seed(&Config::default());
            let p = FakePlatform::new();
            *p.capture_excluded.borrow_mut() = false;
            let e = Engine::new(&p);
            let err = e
                .night(Mode::Overlay, at(21, 0, 0))
                .unwrap_err()
                .to_string();
            assert!(err.contains("无法从屏幕采集中排除"), "{err}");
            assert!(p.called("exit_guard"), "失败时必须把黑幕拆掉");
            assert_eq!(Config::load().unwrap().state, RunState::Day);
        });
    }

    #[test]
    fn default_mode_switches_the_virtual_screen_not_an_overlay() {
        with_home(|| {
            seed(&Config::default());
            let mut p = FakePlatform::new();
            *p.vdd_present.borrow_mut() = true;
            p.caps.virtual_display = true;
            let e = Engine::new(&p);
            e.night(Mode::Auto, at(21, 0, 0)).unwrap();
            e.day().unwrap();
            assert!(p.called("virtual_screen"), "默认就是虚拟屏");
            assert!(p.called("backup_display"), "切换前必须备份拓扑");
            assert!(p.called("restore_display"));
            assert!(!p.called("enter_guard"));
        });
    }

    #[test]
    fn overlay_mode_never_touches_the_topology() {
        with_home(|| {
            seed(&Config::default());
            let p = FakePlatform::new();
            let e = Engine::new(&p);
            e.night(Mode::Overlay, at(21, 0, 0)).unwrap();
            e.day().unwrap();
            assert!(!p.called("virtual_screen"));
            assert!(!p.called("backup_display"));
            assert!(!p.called("restore_display"));
            assert!(p.called("input_guard_on"));
        });
    }

    #[test]
    fn reconcile_enters_night_inside_window_and_back_outside() {
        with_home(|| {
            let mut cfg = Config::default();
            cfg.enabled = true;
            seed(&cfg);
            let p = FakePlatform::new();
            let e = Engine::new(&p);

            let out = e.reconcile(at(19, 0, 0)).unwrap();
            assert!(out.message.contains("白天"), "{}", out.message);
            assert!(!p.called("enter_guard"));

            e.reconcile(at(21, 0, 0)).unwrap();
            assert_eq!(Config::load().unwrap().state, RunState::Night);

            e.reconcile(at(23, 30, 0)).unwrap();
            assert!(p.called("repair_guard"), "夜间内再检查应当修复而不是重建");

            e.reconcile(at(11, 0, 0)).unwrap();
            assert_eq!(Config::load().unwrap().state, RunState::Day);
        });
    }

    #[test]
    fn reconcile_does_nothing_when_disabled() {
        with_home(|| {
            seed(&Config::default());
            let p = FakePlatform::new();
            let e = Engine::new(&p);
            let out = e.reconcile(at(22, 0, 0)).unwrap();
            assert!(out.message.contains("未启用"));
            assert!(!p.called("enter_guard"));
        });
    }

    #[test]
    fn unlock_override_keeps_day_for_rest_of_window() {
        with_home(|| {
            let mut cfg = Config::default();
            cfg.enabled = true;
            seed(&cfg);
            let p = FakePlatform::new();
            let e = Engine::new(&p);
            e.reconcile(at(21, 0, 0)).unwrap();
            assert_eq!(Config::load().unwrap().state, RunState::Night);

            e.chord_unlock(at(21, 5, 0)).unwrap();
            assert_eq!(Config::load().unwrap().state, RunState::Day);

            // 同一个夜间时段内再检查，不应又黑回去
            e.reconcile(at(22, 0, 0)).unwrap();
            assert_eq!(Config::load().unwrap().state, RunState::Day);

            // 过了这个夜间时段，第二天晚上要照常黑
            e.reconcile(at(20, 30, 0)).unwrap();
            let cfg = Config::load().unwrap();
            assert_eq!(
                cfg.override_until.map(|t| chrono::Datelike::day(&t)),
                Some(30),
                "override 应当在本段结束时清掉"
            );
        });
    }

    #[test]
    fn manual_night_holds_until_next_window_end() {
        with_home(|| {
            let mut cfg = Config::default();
            cfg.enabled = true;
            seed(&cfg);
            let p = FakePlatform::new();
            let e = Engine::new(&p);
            e.night(Mode::Auto, at(15, 0, 0)).unwrap();
            let cfg = Config::load().unwrap();
            let hold = cfg
                .hold_night_until
                .expect("白天手动进入夜间要记下保持时间");
            assert_eq!(chrono::Timelike::hour(&hold), 10);
            assert_eq!(chrono::Timelike::minute(&hold), 30);
            assert_eq!(chrono::Datelike::day(&hold), 30);
            // 保持期内检查不应切回白天
            e.reconcile(at(16, 0, 0)).unwrap();
            assert_eq!(Config::load().unwrap().state, RunState::Night);
        });
    }

    #[test]
    fn test_flow_requires_recovery_before_activate() {
        with_home(|| {
            seed(&Config::default());
            let p = FakePlatform::new();
            let e = Engine::new(&p);
            let out = e.start_test(Mode::Overlay, 15, at(15, 0, 0)).unwrap();
            assert!(out.message.contains("15 秒后自动恢复"), "{}", out.message);
            assert!(p.called("spawn_delayed_day"));
            let cfg = Config::load().unwrap();
            assert!(!cfg.last_test_ok, "还没恢复时不能算测试通过");
            let id = cfg.test_id.clone();

            // 未恢复就启用必须被拒绝
            let err = e
                .activate(Mode::Overlay, "20:00", "10:30", at(15, 1, 0))
                .unwrap_err()
                .to_string();
            assert!(err.contains("请先点「测试 15 秒」"), "{err}");

            e.finish_test(&id, at(15, 0, 15)).unwrap();
            let cfg = Config::load().unwrap();
            assert!(cfg.last_test_ok);
            assert_eq!(cfg.state, RunState::Day);

            let out = e
                .activate(Mode::Overlay, "20:00", "10:30", at(15, 1, 0))
                .unwrap();
            assert!(out.message.contains("已启用每日计划"), "{}", out.message);
            assert!(p.called("install_schedule"));
            assert!(Config::load().unwrap().enabled);
        });
    }

    #[test]
    fn activate_rejects_stale_or_other_hardware_tests() {
        with_home(|| {
            seed(&Config::default());
            let p = FakePlatform::new();
            let e = Engine::new(&p);
            let out = e.start_test(Mode::Overlay, 15, at(15, 0, 0)).unwrap();
            assert!(out.is_ok());
            let id = Config::load().unwrap().test_id.clone();
            e.finish_test(&id, at(15, 0, 15)).unwrap();

            // 换了硬件（指纹变了）
            *p.fingerprint.borrow_mut() = "fp-2".into();
            let err = e
                .activate(Mode::Overlay, "20:00", "10:30", at(15, 1, 0))
                .unwrap_err()
                .to_string();
            assert!(err.contains("请先点"), "{err}");

            // 指纹恢复但测试超过一天
            *p.fingerprint.borrow_mut() = "fp-1".into();
            let err = e
                .activate(Mode::Overlay, "20:00", "10:30", at_day(30, 20, 0, 0))
                .unwrap_err()
                .to_string();
            assert!(err.contains("超过一天"), "{err}");
        });
    }

    #[test]
    fn failed_schedule_write_leaves_plan_disabled() {
        with_home(|| {
            seed(&Config::default());
            let p = FakePlatform::new();
            let e = Engine::new(&p);
            let out = e.start_test(Mode::Overlay, 15, at(15, 0, 0)).unwrap();
            assert!(out.is_ok());
            let id = Config::load().unwrap().test_id.clone();
            e.finish_test(&id, at(15, 0, 15)).unwrap();

            *p.schedule_fails.borrow_mut() = true;
            let err = e
                .activate(Mode::Overlay, "20:00", "10:30", at(15, 1, 0))
                .unwrap_err()
                .to_string();
            assert!(err.contains("写入系统计划任务失败"), "{err}");
            assert!(!Config::load().unwrap().enabled);
        });
    }

    #[test]
    fn test_start_refuses_while_plan_enabled_or_night() {
        with_home(|| {
            let mut cfg = Config::default();
            cfg.enabled = true;
            seed(&cfg);
            let p = FakePlatform::new();
            let e = Engine::new(&p);
            let err = e
                .start_test(Mode::Overlay, 15, at(15, 0, 0))
                .unwrap_err()
                .to_string();
            assert!(err.contains("请先暂停"), "{err}");

            let mut cfg = Config::default();
            cfg.state = RunState::Night;
            cfg.night_mode = Some(Mode::Overlay);
            seed(&cfg);
            let err = e
                .start_test(Mode::Overlay, 15, at(15, 0, 0))
                .unwrap_err()
                .to_string();
            assert!(err.contains("还黑着"), "{err}");
        });
    }

    #[test]
    fn test_duration_bounds() {
        with_home(|| {
            seed(&Config::default());
            let p = FakePlatform::new();
            let e = Engine::new(&p);
            assert!(e.start_test(Mode::Auto, 5, at(15, 0, 0)).is_err());
            assert!(e.start_test(Mode::Auto, 4000, at(15, 0, 0)).is_err());
        });
    }

    #[test]
    fn emergency_restores_and_pauses() {
        with_home(|| {
            let mut cfg = Config::default();
            cfg.enabled = true;
            seed(&cfg);
            let p = FakePlatform::new();
            let e = Engine::new(&p);
            e.reconcile(at(21, 0, 0)).unwrap();
            assert_eq!(Config::load().unwrap().state, RunState::Night);

            let out = e.emergency().unwrap();
            assert!(out.is_ok(), "{:?}", out.errors);
            let cfg = Config::load().unwrap();
            assert_eq!(cfg.state, RunState::Day);
            assert!(!cfg.enabled);
            assert!(p.called("autostart_on"), "应急后登录自启按偏好保留");
        });
    }

    #[test]
    fn partial_recovery_reports_nonzero_and_flags_pending() {
        with_home(|| {
            seed(&Config::default());
            let p = FakePlatform::new();
            let e = Engine::new(&p);
            e.night(Mode::Overlay, at(21, 0, 0)).unwrap();
            *p.day_fails.borrow_mut() = true;
            let out = e.day().unwrap();
            assert!(!out.is_ok(), "恢复失败不能被当成成功");
            assert!(out.recovery_pending);
            assert!(Config::load().unwrap().recovery_pending);
        });
    }

    #[test]
    fn uninstall_clears_autostart_preference() {
        with_home(|| {
            seed(&Config::default());
            let p = FakePlatform::new();
            let e = Engine::new(&p);
            let out = e.uninstall().unwrap();
            assert!(out.message.contains("已卸载"), "{}", out.message);
            let cfg = Config::load().unwrap();
            assert!(!cfg.autostart_preference);
            assert!(!cfg.autostart);
            assert!(!cfg.enabled);
            assert!(p.called("remove_schedule"));
        });
    }

    #[test]
    fn virtual_screen_needs_the_driver_installed() {
        with_home(|| {
            seed(&Config::default());
            let p = FakePlatform::new();
            let e = Engine::new(&p);
            // VDD 是必需项：没装就明确报错，不能假装成功。
            let err = e
                .night(Mode::VirtualScreen, at(21, 0, 0))
                .unwrap_err()
                .to_string();
            assert!(err.contains("虚拟显示"), "{err}");
            assert!(!p.called("virtual_screen"));
        });
    }

    #[test]
    fn virtual_screen_switches_when_the_driver_is_there() {
        with_home(|| {
            seed(&Config::default());
            let mut p = FakePlatform::new();
            p.caps.virtual_display = true;
            *p.vdd_present.borrow_mut() = true;
            let e = Engine::new(&p);
            let out = e.night(Mode::VirtualScreen, at(21, 0, 0)).unwrap();
            assert_eq!(out.mode, Some(Mode::VirtualScreen));
            assert!(p.called("backup_display"));
            assert!(p.called("virtual_screen"));
            assert!(!p.called("enter_guard"));
        });
    }

    #[test]
    fn auto_reports_both_paths_when_neither_works() {
        with_home(|| {
            seed(&Config::default());
            let mut p = FakePlatform::new();
            p.caps.capture_exclusion = false;
            let e = Engine::new(&p);
            let err = e.night(Mode::Auto, at(21, 0, 0)).unwrap_err().to_string();
            // 两条路都要说清楚为什么不行，不能只说“失败”。
            assert!(err.contains("虚拟"), "{err}");
            assert!(err.contains("2004"), "{err}");
            assert!(!p.called("virtual_screen"));
        });
    }

    #[test]
    fn ensure_night_rebuilds_missing_overlay() {
        with_home(|| {
            seed(&Config::default());
            let p = FakePlatform::new();
            let e = Engine::new(&p);
            e.night(Mode::Overlay, at(21, 0, 0)).unwrap();
            *p.guard_active.borrow_mut() = false;
            e.ensure_night(at(21, 1, 0)).unwrap();
            assert_eq!(p.call_count("enter_guard"), 2);
            assert!(!p.called("repair_guard"));
        });
    }

    #[test]
    fn ensure_night_takes_the_desktop_back_when_the_virtual_screen_loses_it() {
        with_home(|| {
            seed(&Config::default());
            let mut p = FakePlatform::new();
            p.caps.virtual_display = true;
            *p.vdd_present.borrow_mut() = true;
            let e = Engine::new(&p);
            e.night(Mode::VirtualScreen, at(21, 0, 0)).unwrap();
            assert_eq!(p.call_count("virtual_screen"), 1);

            // 好好的时候不折腾拓扑。
            e.ensure_night(at(21, 1, 0)).unwrap();
            assert_eq!(p.call_count("virtual_screen"), 1);

            // 实体屏又拿回了桌面（例如用户按了 Win+P），必须重新切回去。
            *p.virtual_holding.borrow_mut() = false;
            e.ensure_night(at(21, 2, 0)).unwrap();
            assert_eq!(p.call_count("virtual_screen"), 2);
        });
    }

    #[test]
    fn manual_night_until_none_when_already_in_window() {
        let cfg = Config::default();
        let s = cfg.schedule();
        assert!(manual_night_until(&s, true, at(22, 0, 0)).is_none());
        let until = manual_night_until(&s, true, at(12, 0, 0)).unwrap();
        assert_eq!(chrono::Datelike::day(&until), 30);
        assert_eq!(chrono::Timelike::hour(&until), 10);
    }
}
