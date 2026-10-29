//! 夜间会话记录：每次进入夜间到恢复白天，记一条结构化摘要。
//!
//! 为什么单独存一份：日志是按天滚动的文本，翻起来要人肉拼；而"昨晚那一次
//! 到底哪一步慢、哪一层没生效"是排障时最常问的问题。这里每次会话一行 JSON，
//! 追加写进 `sessions.jsonl`，界面和诊断包直接读最近几条。
//!
//! 一次会话的生命周期：
//! * 进入夜间时 [`SessionRecord::start`]，拿到一个 `trace_id`（同时写进日志）；
//! * 每一步（预热、切换、gamma、背光、钩子、看门狗）都记耗时和结果；
//! * 恢复白天时补上结束时间和结果，落盘。

use std::io::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

use crate::mode::{LayerStatus, Mode, NightReport};

/// 一步的记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepRecord {
    /// 步骤名，英文小写（`prewarm`、`topology`、`gamma`…）。
    pub step: String,
    /// 是否成功。
    pub ok: bool,
    /// 耗时（毫秒）。无法测量时为 0。
    pub ms: u64,
    /// 失败原因或补充说明（中文，给用户看）。
    #[serde(default)]
    pub detail: String,
}

impl StepRecord {
    /// 成功的一步。
    #[must_use]
    pub fn ok(step: &str, ms: u64) -> Self {
        Self {
            step: step.to_string(),
            ok: true,
            ms,
            detail: String::new(),
        }
    }

    /// 失败的一步。
    #[must_use]
    pub fn failed(step: &str, ms: u64, detail: impl Into<String>) -> Self {
        Self {
            step: step.to_string(),
            ok: false,
            ms,
            detail: detail.into(),
        }
    }

    /// 跳过的一步（例如用户关掉了这一层）。
    #[must_use]
    pub fn skipped(step: &str, reason: impl Into<String>) -> Self {
        Self {
            step: step.to_string(),
            ok: true,
            ms: 0,
            detail: format!("跳过：{}", reason.into()),
        }
    }
}

/// 一次完整的夜间会话。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    /// 贯穿本次会话的追踪 id，日志里也带它。
    pub trace_id: String,
    /// 用户请求的模式。
    pub requested: Mode,
    /// 实际生效的模式（可能降级）。
    pub effective: Mode,
    /// 进入夜间的时刻。
    pub started_at: DateTime<Local>,
    /// 恢复白天的时刻；`None` 表示还在夜间或者异常结束。
    #[serde(default)]
    pub ended_at: Option<DateTime<Local>>,
    /// 是否正常恢复。
    #[serde(default)]
    pub ended_cleanly: bool,
    /// 结束时的说明（例如"已恢复白天显示"）。
    #[serde(default)]
    pub outcome: String,
    /// 每一步的明细。
    #[serde(default)]
    pub steps: Vec<StepRecord>,
}

impl SessionRecord {
    /// 开一次新会话。
    #[must_use]
    pub fn start(requested: Mode, effective: Mode, trace_id: &str) -> Self {
        Self {
            trace_id: trace_id.to_string(),
            requested,
            effective,
            started_at: Local::now(),
            ended_at: None,
            ended_cleanly: false,
            outcome: String::new(),
            steps: Vec::new(),
        }
    }

    /// 记一步。
    pub fn push(&mut self, step: StepRecord) {
        self.steps.push(step);
    }

    /// 总耗时（毫秒）。会话还没结束时按"到现在"算。
    #[must_use]
    pub fn duration_ms(&self) -> u64 {
        let end = self.ended_at.unwrap_or_else(Local::now);
        u64::try_from((end - self.started_at).num_milliseconds()).unwrap_or(0)
    }

    /// 失败的步骤数。
    #[must_use]
    pub fn failures(&self) -> usize {
        self.steps.iter().filter(|step| !step.ok).count()
    }

    /// 一行给人看的摘要。
    #[must_use]
    pub fn summary(&self) -> String {
        let when = self.started_at.format("%m-%d %H:%M");
        let state = if self.ended_cleanly {
            "已恢复"
        } else if self.ended_at.is_some() {
            "恢复不完整"
        } else {
            "夜间中"
        };
        format!(
            "{when} {} · {state} · {} 步 / {} 失败 · {} 秒",
            self.effective.label(),
            self.steps.len(),
            self.failures(),
            self.duration_ms() / 1000
        )
    }
}

/// 生成一个短的追踪 id（16 位十六进制）。写进日志和会话记录，方便把
/// "这一晚的每一步"从几个不同组件的日志里挑出来。
#[must_use]
pub fn new_trace_id() -> String {
    use rand::RngCore as _;
    let mut bytes = [0_u8; 8];
    rand::rng().fill_bytes(&mut bytes);
    let mut text = String::with_capacity(16);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// 把引擎拿到的 [`NightReport`] 摊平成逐步记录。
///
/// 平台层只能汇报"这一层最后成不成"，耗时分段在报告里；这里统一成
/// 界面和诊断包要的形状，顺序就是真实的执行顺序。
#[must_use]
pub fn steps_from_report(report: &NightReport) -> Vec<StepRecord> {
    let mut steps = Vec::new();
    if let Some(virtual_screen) = &report.virtual_screen {
        if virtual_screen.validate_ms > 0 || virtual_screen.applied {
            steps.push(if virtual_screen.applied {
                StepRecord::ok("prewarm", virtual_screen.validate_ms)
            } else {
                StepRecord::failed(
                    "prewarm",
                    virtual_screen.validate_ms,
                    virtual_screen
                        .error
                        .clone()
                        .unwrap_or_else(|| "虚拟屏预热失败".into()),
                )
            });
            steps.push(StepRecord::ok("topology", virtual_screen.apply_ms));
            steps.push(if virtual_screen.mode_matched {
                StepRecord::ok("mode_match", 0)
            } else {
                StepRecord::failed(
                    "mode_match",
                    0,
                    "虚拟屏模式与原实体屏不一致，可能有一次重新初始化",
                )
            });
            steps.push(StepRecord::ok("settle", virtual_screen.settle_ms));
            steps.push(StepRecord::ok("active_paths", 0));
            if let Some(last) = steps.last_mut() {
                last.detail = format!(
                    "活动 target {}→{}",
                    virtual_screen.active_before, virtual_screen.active_after
                );
            }
        }
    }
    for overlay in &report.overlays {
        steps.push(if overlay.ok() {
            StepRecord::ok("overlay", overlay.first_frame_ms)
        } else {
            StepRecord::failed(
                "overlay",
                overlay.first_frame_ms,
                overlay
                    .error
                    .clone()
                    .unwrap_or_else(|| format!("{} 黑幕未确认", overlay.display)),
            )
        });
    }
    steps.push(layer_step("gamma", &report.gamma));
    steps.push(layer_step("brightness", &report.brightness));
    steps.push(layer_step("input_hook", &report.local_input_guard));
    steps
}

fn layer_step(name: &str, status: &LayerStatus) -> StepRecord {
    match status {
        LayerStatus::Applied => StepRecord::ok(name, 0),
        LayerStatus::Disabled => StepRecord::skipped(name, "设置里关闭了这一层"),
        LayerStatus::Unsupported { reason } | LayerStatus::Failed { reason } => {
            StepRecord::failed(name, 0, reason.clone())
        }
    }
}

/// 会话文件（一次会话一行，只追加）。
#[must_use]
pub fn session_file() -> PathBuf {
    crate::paths::home_dir().join("sessions.jsonl")
}

/// 正在进行中的那一夜。单独一个文件，方便被中断时留下证据。
#[must_use]
pub fn current_file() -> PathBuf {
    crate::paths::home_dir().join("session-current.json")
}

/// 记下"当前正在夜间"。**不**追加到历史，真正结束时才追加。
pub fn save_current(record: &SessionRecord) {
    save_current_to(&current_file(), record);
}

fn save_current_to(path: &Path, record: &SessionRecord) {
    let Ok(text) = serde_json::to_string_pretty(record) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let temporary = path.with_extension("json.tmp");
    if std::fs::write(&temporary, text).is_ok() {
        let _ = std::fs::rename(&temporary, path);
    }
}

/// 取出正在进行中的会话（不删除）。
#[must_use]
pub fn load_current() -> Option<SessionRecord> {
    load_current_from(&current_file())
}

fn load_current_from(path: &Path) -> Option<SessionRecord> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// 删掉进行中的会话文件（正常结束或已归档后调用）。
pub fn clear_current() {
    clear_current_at(&current_file());
}

fn clear_current_at(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// 结束并归档：追加到历史，删掉进行中的那份。
pub fn finish(record: &SessionRecord) {
    append(record);
    clear_current();
}

/// 追加一条会话。失败只记日志（会话记录不该影响显示动作）。
pub fn append(record: &SessionRecord) {
    append_to(&session_file(), record);
}

fn append_to(path: &Path, record: &SessionRecord) {
    let Ok(line) = serde_json::to_string(record) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    let _ = writeln!(file, "{line}");
}

/// 读最近 `count` 条会话，最新的在前。坏行直接跳过。
#[must_use]
pub fn recent(count: usize) -> Vec<SessionRecord> {
    recent_from(&session_file(), count)
}

fn recent_from(path: &Path, count: usize) -> Vec<SessionRecord> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out: Vec<SessionRecord> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    out.reverse();
    out.truncate(count);
    out
}

/// 只保留最近 `keep` 条，避免文件无限增长。
pub fn trim(keep: usize) {
    trim_at(&session_file(), keep);
}

fn trim_at(path: &Path, keep: usize) {
    let mut all = recent_from(path, usize::MAX);
    if all.len() <= keep {
        return;
    }
    all.truncate(keep);
    all.reverse();
    let mut text = String::new();
    for record in &all {
        if let Ok(line) = serde_json::to_string(record) {
            text.push_str(&line);
            text.push('\n');
        }
    }
    let _ = std::fs::write(path, text);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(at: u32) -> SessionRecord {
        SessionRecord {
            trace_id: format!("t{at}"),
            requested: Mode::Auto,
            effective: Mode::VirtualScreen,
            started_at: Local::now(),
            ended_at: Some(Local::now()),
            ended_cleanly: true,
            outcome: "已恢复白天显示".into(),
            steps: vec![StepRecord::ok("topology", 12)],
        }
    }

    #[test]
    fn steps_are_classified() {
        let ok = StepRecord::ok("gamma", 5);
        assert!(ok.ok && ok.ms == 5);
        let failed = StepRecord::failed("brightness", 7, "面板拒绝");
        assert!(!failed.ok);
        assert!(failed.detail.contains("面板拒绝"));
        let skipped = StepRecord::skipped("gamma", "用户关闭了这一层");
        assert!(skipped.ok);
        assert!(skipped.detail.starts_with("跳过："));
    }

    #[test]
    fn failures_count_only_failed_steps() {
        let mut session = SessionRecord::start(Mode::Auto, Mode::VirtualScreen, "t");
        session.push(StepRecord::ok("prewarm", 3));
        session.push(StepRecord::failed("topology", 9, "驱动拒绝"));
        session.push(StepRecord::skipped("gamma", "关掉了"));
        assert_eq!(session.failures(), 1);
        assert_eq!(session.steps.len(), 3);
    }

    #[test]
    fn summary_mentions_mode_and_state() {
        let mut session = SessionRecord::start(Mode::Auto, Mode::VirtualScreen, "t");
        session.push(StepRecord::ok("prewarm", 3));
        let text = session.summary();
        assert!(text.contains("虚拟屏"), "{text}");
        assert!(text.contains("夜间中"), "{text}");
        session.ended_at = Some(Local::now());
        session.ended_cleanly = true;
        assert!(session.summary().contains("已恢复"));
    }

    #[test]
    fn append_then_read_returns_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.jsonl");
        for index in 0..3 {
            append_to(&path, &record(index));
        }
        let recent = recent_from(&path, 2);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].trace_id, "t2", "最新的在前");
        assert_eq!(recent[1].trace_id, "t1");
    }

    #[test]
    fn missing_file_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(recent_from(&dir.path().join("nope.jsonl"), 5).len(), 0);
    }

    #[test]
    fn corrupt_lines_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.jsonl");
        append_to(&path, &record(1));
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, "{{not json").unwrap();
        append_to(&path, &record(2));
        let recent = recent_from(&path, 10);
        assert_eq!(recent.len(), 2, "坏行被跳过，好行都保留");
    }

    #[test]
    fn trim_keeps_the_newest_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.jsonl");
        for index in 0..10 {
            append_to(&path, &record(index));
        }
        trim_at(&path, 3);
        let kept = recent_from(&path, 10);
        assert_eq!(kept.len(), 3);
        assert_eq!(kept[0].trace_id, "t9");
        assert_eq!(kept[2].trace_id, "t7");
    }
}
