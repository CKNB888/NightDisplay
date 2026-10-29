//! 界面要显示的全部内容，一次算好交给前端。
//!
//! 前端只做渲染，不做任何判断 —— 这样"界面显示的和实际执行的是同一套逻辑"，
//! 不会出现界面说"已启用"而引擎其实没启用这种偏差。
//!
//! 这里只依赖 `nd-core`，所以能在任何平台上跑单元测试。

use chrono::{DateTime, Local};
use nd_core::config::{Config, Mode, RunState};
use nd_core::onboarding::{Onboarding, Step};
use nd_core::schedule::minute_of_day;
use nd_core::session::{SessionRecord, StepRecord};
use nd_core::status::{Capabilities, Registration};
use serde::Serialize;

/// 一个时间段，给界面上的两个时间输入框用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RangeView {
    /// `HH:MM`。
    pub start: String,
    /// `HH:MM`。
    pub end: String,
    /// 是否跨过午夜（20:00 → 10:30）。
    pub overnight: bool,
    /// `8 小时 30 分`。
    pub length: String,
}

/// 向导里的一步。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StepView {
    /// 内部标识（`virtual_driver` 这种），前端用来配对图标。
    pub id: String,
    pub title: String,
    pub detail: String,
    /// 是否必需。
    pub required: bool,
    /// `pending` / `ok` / `skipped` / `manual` / `failed`。
    pub status: String,
    /// 结果说明（成功原因、失败原因或手动步骤）。
    pub message: String,
    /// 需要用户点开的链接或要执行的命令。
    pub action: Option<String>,
    /// 这一步的耗时（毫秒）；没跑过是 0。
    pub elapsed_ms: u64,
}

/// 向导整体状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct OnboardingView {
    pub completed: bool,
    pub completed_at: Option<String>,
    pub remote_confirmed: bool,
    pub helper_ready: bool,
    /// 需要修复的步骤数。
    pub failed: usize,
    pub steps: Vec<StepView>,
    /// 日志目录，失败时界面上给个"打开日志"。
    pub log_hint: String,
}

/// 一次夜间会话，给「诊断」页用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionView {
    pub trace_id: String,
    pub mode: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub ended_cleanly: bool,
    pub outcome: String,
    pub duration_secs: u64,
    pub failures: usize,
    pub steps: Vec<StepRecord>,
    /// 一行摘要。
    pub summary: String,
}

impl SessionView {
    #[must_use]
    pub fn from_record(record: &SessionRecord) -> Self {
        Self {
            trace_id: record.trace_id.clone(),
            mode: record.effective.label().to_string(),
            started_at: record.started_at.format("%m-%d %H:%M").to_string(),
            ended_at: record.ended_at.map(|t| t.format("%m-%d %H:%M").to_string()),
            ended_cleanly: record.ended_cleanly,
            outcome: record.outcome.clone(),
            duration_secs: record.duration_ms() / 1000,
            failures: record.failures(),
            steps: record.steps.clone(),
            summary: record.summary(),
        }
    }
}

/// 首页那个倒计时环需要的东西。
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct Countdown {
    /// 距离下一次切换还有多少秒。
    pub seconds: i64,
    /// 下一次是进入夜间（`true`）还是恢复白天（`false`）。
    pub to_night: bool,
    /// 当前这一段已经走掉的百分比（0.0–1.0），环的进度用它。
    pub progress: f64,
    /// 这一段总长（秒）。
    pub total_secs: i64,
    /// 现在处于夜间还是白天。
    pub night: bool,
    /// 人类可读的"还有 2 小时 15 分"。
    pub text: String,
}

/// 前端拿到的全部状态。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Snapshot {
    pub version: String,
    /// 状态页那几行纯文本，和 `--action status` 完全一致。
    pub status_text: String,
    pub state: RunState,
    /// 夜间实际用的模式（没在夜间时是 `None`）。
    pub night_mode: Option<Mode>,
    /// 配置里选的模式。
    pub mode: Mode,
    /// 解析后的模式（`auto` → `virtual_screen`）。
    pub effective_mode: Mode,
    pub enabled: bool,
    pub ranges: Vec<RangeView>,
    pub unlock_chord: String,
    pub unlock_hold_ms: u64,
    pub strict_local_input: bool,
    pub suppress_lock: bool,
    pub keep_awake: bool,
    pub lock: bool,
    pub high_performance_at_night: bool,
    pub autostart_preference: bool,
    pub autostart_config: bool,
    pub registration: Registration,
    pub helper_running: bool,
    pub virtual_display: bool,
    pub virtual_screen_holding: bool,
    pub capture_exclusion: bool,
    pub gamma_control: bool,
    pub brightness_displays: usize,
    pub displays: usize,
    pub build: u32,
    pub os: String,
    pub gpu: Vec<String>,
    pub monitors: Vec<String>,
    pub countdown: Countdown,
    pub onboarding: OnboardingView,
    pub sessions: Vec<SessionView>,
    pub current_session: Option<SessionView>,
    /// 上次进入夜间的耗时（毫秒）。
    pub last_night_latency_ms: u64,
    pub logs_dir: String,
    pub home: String,
    /// 有没有一次成功的测试（启用每日计划的前提）。
    pub test_ready: bool,
    /// 上次测试说明。
    pub test_note: String,
    /// 常驻进程在不在（界面上决定"立即黑屏"是否可靠）。
    pub daemon_running: bool,
}

/// 把时长写成 `2 小时 15 分`。
#[must_use]
pub fn human_secs(seconds: i64) -> String {
    let total = seconds.max(0);
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    if hours > 0 {
        format!("{hours} 小时 {minutes} 分")
    } else if minutes > 0 {
        format!("{minutes} 分")
    } else {
        format!("{total} 秒")
    }
}

fn range_view(range: nd_core::schedule::NightRange) -> RangeView {
    RangeView {
        start: range.start.to_hm(),
        end: range.end.to_hm(),
        overnight: range.crosses_midnight(),
        length: human_secs(span_secs(range.start.minutes(), range.end.minutes())),
    }
}

/// 一个时间段的长度（秒），跨午夜按 24 小时回绕。
fn span_secs(start: u16, end: u16) -> i64 {
    let start = i64::from(start);
    let end = i64::from(end);
    let minutes = if end > start {
        end - start
    } else {
        24 * 60 - start + end
    };
    minutes * 60
}

/// 当前这一段的进度与剩余时间。
fn countdown_for(cfg: &Config, now: DateTime<Local>) -> Countdown {
    let schedule = cfg.schedule();
    let night = schedule.is_night_at(now);
    if !cfg.enabled || schedule.is_empty() {
        return Countdown {
            seconds: 0,
            to_night: true,
            progress: 0.0,
            total_secs: 0,
            night,
            text: "每日计划未启用".to_string(),
        };
    }
    let Some((to_night, seconds)) = schedule.next_switch(now) else {
        return Countdown {
            seconds: 0,
            to_night: !night,
            progress: 0.0,
            total_secs: 0,
            night,
            text: "没有有效的切换点".to_string(),
        };
    };
    // 环的进度要的是"这一段已走 / 这一段总长"，两段的长度都得从计划里算，
    // 不能拿"剩余"当分母 —— 那样任何时刻的进度都是 50%。
    let (_, elapsed, total) = schedule
        .current_segment_secs(now)
        .unwrap_or((night, 0, seconds));
    let progress = if total > 0 {
        (elapsed as f64 / total as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let text = format!(
        "{}后{}",
        human_secs(seconds),
        if to_night {
            "进入夜间"
        } else {
            "恢复显示"
        }
    );
    Countdown {
        seconds,
        to_night,
        progress,
        total_secs: total,
        night,
        text,
    }
}

fn step_view(step: Step, onboarding: &Onboarding) -> StepView {
    let result = onboarding.result_for(step);
    let (status, message, action, elapsed_ms) = match result {
        Some(r) => {
            use nd_core::onboarding::StepOutcome;
            let (status, action) = match &r.outcome {
                StepOutcome::Ok { .. } => ("ok", None),
                StepOutcome::Skipped { .. } => ("skipped", None),
                StepOutcome::Manual { link, .. } => ("manual", link.clone()),
                StepOutcome::Failed { next, .. } => ("failed", Some(next.clone())),
            };
            (status, r.outcome.summary(), action, r.elapsed_ms)
        }
        None => ("pending", String::new(), None, 0),
    };
    StepView {
        id: step_id(step).to_string(),
        title: step.title().to_string(),
        detail: step.detail().to_string(),
        required: step.required(),
        status: status.to_string(),
        message,
        action,
        elapsed_ms,
    }
}

/// 步骤的稳定标识（前端用它对图标；公开是为了让命令层推进度事件时复用）。
#[must_use]
pub fn step_id(step: Step) -> &'static str {
    match step {
        Step::Elevate => "elevate",
        Step::VirtualDriver => "virtual_driver",
        Step::Autostart => "autostart",
        Step::InputGuard => "input_guard",
        Step::SelfTest => "self_test",
        Step::Confirm => "confirm",
    }
}

fn onboarding_view(onboarding: &Onboarding) -> OnboardingView {
    OnboardingView {
        completed: onboarding.completed,
        completed_at: onboarding
            .completed_at
            .map(|t| t.format("%Y-%m-%d %H:%M").to_string()),
        remote_confirmed: onboarding.remote_confirmed,
        helper_ready: onboarding.helper_ready,
        failed: onboarding.failed_steps.len(),
        steps: Step::ALL
            .iter()
            .map(|s| step_view(*s, onboarding))
            .collect(),
        log_hint: onboarding.log_hint(),
    }
}

/// 算出一整份快照。
#[must_use]
pub fn build(
    cfg: &Config,
    caps: &Capabilities,
    onboarding: &Onboarding,
    now: DateTime<Local>,
) -> Snapshot {
    let countdown = countdown_for(cfg, now);
    let test_ready = cfg.last_test_ok && cfg.last_test_at.is_some();
    let test_note = match (cfg.last_test_at, cfg.last_test_ok) {
        (Some(at), true) => format!("{} 的测试已通过", at.format("%m-%d %H:%M")),
        (Some(at), false) => format!("{} 的测试没有通过", at.format("%m-%d %H:%M")),
        (None, _) => "还没有测试过".to_string(),
    };
    Snapshot {
        version: nd_core::VERSION.to_string(),
        status_text: nd_core::status::status_text(cfg, caps, now),
        state: cfg.state,
        night_mode: cfg.night_mode,
        mode: cfg.mode,
        effective_mode: nd_core::mode::resolve(cfg.mode),
        enabled: cfg.enabled,
        ranges: cfg.ranges.iter().map(|r| range_view(r.0)).collect(),
        unlock_chord: cfg.chord_text(),
        unlock_hold_ms: cfg.unlock_hold_ms,
        strict_local_input: cfg.strict_local_input,
        suppress_lock: cfg.suppress_lock,
        keep_awake: cfg.keep_awake,
        lock: cfg.lock,
        high_performance_at_night: cfg.high_performance_at_night,
        autostart_preference: cfg.autostart_preference,
        autostart_config: cfg.autostart,
        registration: caps.registration.clone(),
        helper_running: caps.helper_running,
        virtual_display: caps.virtual_display,
        virtual_screen_holding: caps.virtual_screen_holding,
        capture_exclusion: caps.capture_exclusion,
        gamma_control: caps.gamma_control,
        brightness_displays: caps.brightness_displays,
        displays: caps.displays,
        build: caps.build,
        os: caps.os.clone(),
        gpu: caps
            .gpu
            .iter()
            .map(nd_core::status::GpuInfo::label)
            .collect(),
        monitors: caps
            .monitors
            .iter()
            .map(nd_core::status::MonitorInfo::label)
            .collect(),
        countdown,
        onboarding: onboarding_view(onboarding),
        sessions: nd_core::session::recent(5)
            .iter()
            .map(SessionView::from_record)
            .collect(),
        current_session: nd_core::session::load_current()
            .as_ref()
            .map(SessionView::from_record),
        last_night_latency_ms: cfg.last_night_latency_ms,
        logs_dir: nd_core::paths::logs_dir().display().to_string(),
        home: nd_core::paths::home_dir().display().to_string(),
        test_ready,
        test_note,
        daemon_running: false,
    }
}

/// 只读向导状态：不枚举显示能力，界面轮询它很便宜。
#[must_use]
pub fn onboarding_snapshot() -> OnboardingView {
    onboarding_view(&nd_core::onboarding::load_state())
}

/// 现在这个时刻（单独抽出来，测试可注入）。
#[must_use]
pub fn now() -> DateTime<Local> {
    Local::now()
}

/// 当前是夜间还是白天（给前端做皮肤用）。
#[must_use]
pub fn is_night(cfg: &Config, now: DateTime<Local>) -> bool {
    cfg.state == RunState::Night && cfg.schedule().is_night_at(now)
}

/// 一天里的第几分钟，界面上显示"现在 21:04"用。
#[must_use]
pub fn clock(now: DateTime<Local>) -> String {
    let minute = minute_of_day(now);
    format!("{} · {}", now.format("%H:%M"), minute.to_hm())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;

    fn at(h: u32, m: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 10, 29, h, m, 0)
            .single()
            .unwrap()
    }

    fn enabled_cfg() -> Config {
        let mut cfg = Config::default();
        cfg.enabled = true;
        cfg
    }

    #[test]
    fn countdown_before_night_points_at_night() {
        let cfg = enabled_cfg();
        let c = countdown_for(&cfg, at(18, 0));
        assert!(c.to_night);
        assert!(!c.night);
        assert_eq!(c.seconds, 2 * 3600);
        assert!(c.text.contains("进入夜间"), "{}", c.text);
        // 白天段是 10:30 → 20:00，共 9.5 小时；18:00 时已走 7.5 小时。
        assert_eq!(c.total_secs, 9 * 3600 + 30 * 60, "段长要算整段而不是剩余");
        assert!((c.progress - 7.5 / 9.5).abs() < 0.01, "{}", c.progress);
    }

    #[test]
    fn countdown_progress_moves_through_the_night() {
        let cfg = enabled_cfg();
        // 夜间段 20:00 → 10:30 共 14.5 小时：刚入夜是 0，走到一半是 0.5。
        let start = countdown_for(&cfg, at(20, 0));
        let middle = countdown_for(&cfg, at(3, 15));
        let late = countdown_for(&cfg, at(10, 29));
        assert_eq!(start.total_secs, 14 * 3600 + 30 * 60);
        assert!(start.progress < 0.01, "{}", start.progress);
        assert!((middle.progress - 0.5).abs() < 0.01, "{}", middle.progress);
        assert!(late.progress > 0.99, "{}", late.progress);
        // 进度必须单调递增，环才不会来回跳。
        assert!(start.progress <= middle.progress && middle.progress <= late.progress);
    }

    #[test]
    fn countdown_in_night_points_at_restore() {
        let cfg = enabled_cfg();
        let c = countdown_for(&cfg, at(23, 30));
        assert!(!c.to_night);
        assert!(c.night);
        assert_eq!(c.seconds, 11 * 3600, "10:30 恢复");
        assert!(c.text.contains("恢复显示"), "{}", c.text);
    }

    #[test]
    fn countdown_is_empty_when_schedule_off() {
        let cfg = Config::default();
        let c = countdown_for(&cfg, at(23, 30));
        assert_eq!(c.seconds, 0);
        assert_ne!(c.text, "");
        assert_eq!(c.progress, 0.0);
    }

    #[test]
    fn range_view_marks_overnight_ranges() {
        let cfg = Config::default();
        let view = range_view(cfg.ranges[0].0);
        assert_eq!(view.start, "20:00");
        assert_eq!(view.end, "10:30");
        assert!(view.overnight);
        assert_eq!(view.length, "14 小时 30 分");
    }

    #[test]
    fn human_secs_reads_naturally() {
        assert_eq!(human_secs(0), "0 秒");
        assert_eq!(human_secs(59), "59 秒");
        assert_eq!(human_secs(60), "1 分");
        assert_eq!(human_secs(3 * 3600 + 5 * 60), "3 小时 5 分");
        assert_eq!(human_secs(-10), "0 秒");
    }

    #[test]
    fn snapshot_reports_pending_steps_before_onboarding() {
        let onboarding = Onboarding::default();
        let snap = build(
            &Config::default(),
            &Capabilities::default(),
            &onboarding,
            at(12, 0),
        );
        assert!(!snap.onboarding.completed);
        assert_eq!(snap.onboarding.steps.len(), Step::ALL.len());
        assert!(snap.onboarding.steps.iter().all(|s| s.status == "pending"));
        assert_eq!(snap.onboarding.failed, 0);
        assert!(!snap.test_ready);
    }

    #[test]
    fn snapshot_shows_step_results() {
        use nd_core::onboarding::StepResult;
        let mut onboarding = Onboarding::default();
        onboarding.record(StepResult {
            step: Step::VirtualDriver,
            outcome: nd_core::onboarding::StepOutcome::failed(
                "驱动装不上",
                "运行 pnputil /add-driver",
            ),
            elapsed_ms: 42,
        });
        let snap = build(
            &Config::default(),
            &Capabilities::default(),
            &onboarding,
            at(12, 0),
        );
        let step = snap
            .onboarding
            .steps
            .iter()
            .find(|s| s.id == "virtual_driver")
            .unwrap();
        assert_eq!(step.status, "failed");
        assert_eq!(step.elapsed_ms, 42);
        assert!(step
            .action
            .as_deref()
            .unwrap_or_default()
            .contains("pnputil"));
        assert_eq!(snap.onboarding.failed, 1);
    }
}
