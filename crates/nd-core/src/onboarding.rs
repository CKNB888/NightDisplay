//! 首次运行向导的步骤模型。
//!
//! 用户只被问一次、只弹一次管理员确认，然后永远不再打扰。这里定义**步骤与结果**，
//! 真正的动作由 `nd-win` 的 `setup` 模块执行（需要提权的那部分跑在独立的
//! 提升助手进程里，GUI 自己不提权）。
//!
//! 每个步骤都必须报告真实结果：成功 / 跳过 / 需要手动处理（带具体命令或链接），
//! 不允许只丢一句"失败了"。

use serde::{Deserialize, Serialize};

use crate::error::NdResult;

/// 向导步骤，顺序即执行顺序。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// 申请管理员权限，并留一个提升助手常驻，之后夜间操作不再弹 UAC。
    Elevate,
    /// 安装随包内置的虚拟显示驱动，并验证虚拟屏真的出现在显示配置里。
    ///
    /// 4.0 起这是**必需**步骤：guard 模式本身不切到虚拟屏，但虚拟屏必须
    /// 装好待命 —— 「仅虚拟屏」模式要能立刻用，远程软件也能枚举到一块稳定的
    /// 额外显示。安装后会把桌面布局还原成安装前的样子，不多出一块屏。
    VirtualDriver,
    /// 写入开机自启（Run 键 + 计划任务），并装好看门狗。
    Autostart,
    /// 打开本地防触控与夜间抑制锁屏。
    InputGuard,
    /// 15 秒自检：真实的黑屏演示。
    SelfTest,
    /// 确认远程能连，向导结束。
    Confirm,
}

impl Step {
    pub const ALL: [Step; 6] = [
        Step::Elevate,
        Step::VirtualDriver,
        Step::Autostart,
        Step::InputGuard,
        Step::SelfTest,
        Step::Confirm,
    ];

    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Step::Elevate => "授权并准备后台服务",
            Step::VirtualDriver => "虚拟显示驱动",
            Step::Autostart => "开机自启与看门狗",
            Step::InputGuard => "本地防触控与锁屏",
            Step::SelfTest => "15 秒自检",
            Step::Confirm => "确认远程能连",
        }
    }

    /// 步骤说明，界面上直接显示，讲清楚"这一步会发生什么"。
    #[must_use]
    pub fn detail(self) -> &'static str {
        match self {
            Step::Elevate => {
                "会弹一次管理员确认，之后夜间定时黑屏都用这个后台服务，不会再弹。\
                 管理员权限只用来装驱动、写计划任务和调背光。"
            }
            Step::VirtualDriver => {
                "安装包内的虚拟显示驱动，用 pnputil 静默安装并创建虚拟屏，装完立刻验证\
                 它出现在系统显示配置里。它平时只是待命，不会改动桌面布局；\
                 只有你主动选「仅虚拟屏」时才会把桌面切过去。"
            }
            Step::Autostart => {
                "把 NightDisplay 加到开机启动，并注册每天的时间点任务；\
                 同时启动看门狗，万一主程序被结束，几秒内也会自动把屏幕恢复。"
            }
            Step::InputGuard => {
                "夜间吞掉本机真实的键鼠事件，但完整放行远程软件注入的操作；\
                 并抑制系统锁屏，否则登录界面会盖住黑幕。"
            }
            Step::SelfTest => {
                "真的黑屏 15 秒再自动恢复，让你先看清晚上的样子。\
                 黑屏时请用 UU、ToDesk 连一下，确认能正常看到并操作画面。"
            }
            Step::Confirm => "勾选“已确认远程能连”，向导就不会再出现。",
        }
    }

    /// 这一步是否必需。
    ///
    /// 4.0 起虚拟显示驱动是必需的：guard 模式不切到它，但它必须装好待命，
    /// 「仅虚拟屏」才能随时作为后备方案用。
    /// 最后的“确认远程能连”本身就是那个勾选框（`Onboarding::remote_confirmed`），
    /// 不重复算作一个执行步骤。
    #[must_use]
    pub fn required(self) -> bool {
        matches!(
            self,
            Step::Elevate
                | Step::VirtualDriver
                | Step::Autostart
                | Step::InputGuard
                | Step::SelfTest
        )
    }
}

/// 单个步骤的执行结果。必须能表达"跳过"和"需要手动处理"。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum StepOutcome {
    /// 成功，附带一句给用户看的中文说明。
    Ok { detail: String },
    /// 按设计跳过（例如用户不需要虚拟驱动），不是错误。
    Skipped { reason: String },
    /// 需要用户手动做一步，附上准确命令或链接。
    Manual {
        instruction: String,
        link: Option<String>,
    },
    /// 失败，附上原因和可以照做的下一步。
    Failed { reason: String, next: String },
}

impl StepOutcome {
    pub fn ok(detail: impl Into<String>) -> Self {
        StepOutcome::Ok {
            detail: detail.into(),
        }
    }

    pub fn skipped(reason: impl Into<String>) -> Self {
        StepOutcome::Skipped {
            reason: reason.into(),
        }
    }

    pub fn manual(instruction: impl Into<String>, link: Option<String>) -> Self {
        StepOutcome::Manual {
            instruction: instruction.into(),
            link,
        }
    }

    pub fn failed(reason: impl Into<String>, next: impl Into<String>) -> Self {
        StepOutcome::Failed {
            reason: reason.into(),
            next: next.into(),
        }
    }

    /// 算不算"这一步已经处理完了"。跳过算完，手动和失败不算。
    #[must_use]
    pub fn done(&self) -> bool {
        matches!(self, StepOutcome::Ok { .. } | StepOutcome::Skipped { .. })
    }

    #[must_use]
    pub fn failed_hard(&self) -> bool {
        matches!(
            self,
            StepOutcome::Failed { .. } | StepOutcome::Manual { .. }
        )
    }

    /// 一行中文说明，日志和界面共用。
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            StepOutcome::Ok { detail } => detail.clone(),
            StepOutcome::Skipped { reason } => format!("已跳过：{reason}"),
            StepOutcome::Manual { instruction, .. } => format!("需要手动处理：{instruction}"),
            StepOutcome::Failed { reason, next } => format!("失败：{reason}；{next}"),
        }
    }

    /// 这一步给用户的下一步动作（日志链接、命令等）。
    #[must_use]
    pub fn action_hint(&self) -> Option<String> {
        match self {
            StepOutcome::Manual { link, .. } => link.clone(),
            StepOutcome::Failed { next, .. } => Some(next.clone()),
            _ => None,
        }
    }
}

/// 一步的执行记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepResult {
    pub step: Step,
    pub outcome: StepOutcome,
    /// 这一步实际耗时（毫秒）。
    pub elapsed_ms: u64,
}

/// 整个向导的状态，持久化到 `onboarding.json`，完成后永不再弹。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Onboarding {
    pub completed: bool,
    pub completed_at: Option<chrono::DateTime<chrono::Local>>,
    /// 用户已勾选"已确认远程能连"。
    pub remote_confirmed: bool,
    /// 需要修复的步骤（重跑"修复"时只跑这些）。
    pub failed_steps: Vec<Step>,
    pub results: Vec<StepResult>,
    /// 授权助手是否已经就绪（以后夜间操作不再需要 UAC）。
    pub helper_ready: bool,
}

impl Onboarding {
    /// 向导是否应当自动弹出：没完成过就弹。
    #[must_use]
    pub fn should_show(&self) -> bool {
        !self.completed
    }

    /// 记录一步结果，并同步失败列表。
    pub fn record(&mut self, result: StepResult) {
        let step = result.step;
        let failed = result.outcome.failed_hard();
        if let Some(existing) = self.results.iter_mut().find(|r| r.step == step) {
            *existing = result;
        } else {
            self.results.push(result);
        }
        self.failed_steps.retain(|s| *s != step);
        if failed {
            self.failed_steps.push(step);
            self.failed_steps.sort();
            self.failed_steps.dedup();
        }
    }

    /// 完成向导：只有必需的步骤都处理完、且用户确认远程能连，才算真正完成。
    pub fn finish(&mut self, now: chrono::DateTime<chrono::Local>) -> NdResult<()> {
        if !self.remote_confirmed {
            return Err(crate::error::user("请先勾选“已确认远程能连”，再结束向导。"));
        }
        let missing: Vec<&str> = Step::ALL
            .iter()
            .filter(|s| s.required())
            .filter(|s| !self.step_done(**s))
            .map(|s| s.title())
            .collect();
        if !missing.is_empty() {
            return Err(crate::error::user(format!(
                "这些步骤还没有完成：{}。点“重新检查”再试一次。",
                missing.join("、")
            )));
        }
        self.completed = true;
        self.completed_at = Some(now);
        self.failed_steps.clear();
        Ok(())
    }

    #[must_use]
    pub fn step_done(&self, step: Step) -> bool {
        self.results
            .iter()
            .find(|r| r.step == step)
            .is_some_and(|r| r.outcome.done())
    }

    #[must_use]
    pub fn result_for(&self, step: Step) -> Option<&StepResult> {
        self.results.iter().find(|r| r.step == step)
    }

    /// "重新授权 / 修复"要跑的步骤：只跑失败的，失败列表为空时全部重跑一遍检查。
    #[must_use]
    pub fn steps_to_repair(&self) -> Vec<Step> {
        if self.failed_steps.is_empty() {
            Step::ALL.to_vec()
        } else {
            let mut v = self.failed_steps.clone();
            v.sort();
            v
        }
    }

    /// 需要用户点开日志时给出路径提示。
    #[must_use]
    pub fn log_hint(&self) -> String {
        format!(
            "如果某一步失败，日志在 {}",
            crate::paths::logs_dir().display()
        )
    }
}

/// 向导状态文件。
#[must_use]
pub fn state_file() -> std::path::PathBuf {
    crate::paths::home_dir().join("onboarding.json")
}

/// 读向导状态。文件不存在或损坏时当作"没做过"，向导会再弹一次。
#[must_use]
pub fn load_state() -> Onboarding {
    let path = state_file();
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!(error = %e, path = %path.display(), "向导状态不可读，按首次运行处理");
            Onboarding::default()
        }),
        Err(_) => Onboarding::default(),
    }
}

/// 保存向导状态。写入失败只记日志，绝不挡住向导继续。
pub fn save_state(ob: &Onboarding) {
    let path = state_file();
    if let Err(e) = crate::paths::ensure_home() {
        tracing::warn!(error = %e, "无法创建数据目录，向导进度未保存");
        return;
    }
    match serde_json::to_string_pretty(ob) {
        Ok(text) => {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, text).is_ok() {
                if let Err(e) = std::fs::rename(&tmp, &path) {
                    tracing::warn!(error = %e, "向导进度保存失败");
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "向导进度无法序列化"),
    }
}

/// 向导结束后的摘要，写到日志里方便排障。
#[must_use]
pub fn summary_lines(ob: &Onboarding) -> Vec<String> {
    ob.results
        .iter()
        .map(|r| format!("{:<16} {}", r.step.title(), r.outcome.summary()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Local, TimeZone};

    fn now() -> chrono::DateTime<Local> {
        Local.with_ymd_and_hms(2026, 10, 29, 12, 0, 0).unwrap()
    }

    fn result(step: Step, outcome: StepOutcome) -> StepResult {
        StepResult {
            step,
            outcome,
            elapsed_ms: 10,
        }
    }

    #[test]
    fn unknown_or_partial_json_uses_defaults() {
        let ob: Onboarding = serde_json::from_str("{}").unwrap();
        assert!(!ob.completed);
        assert!(ob.should_show());
        assert_eq!(ob.results.len(), 0, "默认状态不该有步骤结果");
    }

    #[test]
    fn failing_step_lands_in_repair_list_and_leaves_it_on_success() {
        let mut ob = Onboarding::default();
        ob.record(result(
            Step::Autostart,
            StepOutcome::failed(
                "计划任务被组策略拦住",
                "手动运行一次 NightDisplay.exe --action serve",
            ),
        ));
        assert_eq!(ob.failed_steps, vec![Step::Autostart]);
        assert!(!ob.step_done(Step::Autostart));

        ob.record(result(Step::Autostart, StepOutcome::ok("已注册 2 个任务")));
        assert_eq!(ob.failed_steps.len(), 0, "成功后必须从待修复列表里移除");
        assert!(ob.step_done(Step::Autostart));
    }

    #[test]
    fn skipped_step_counts_as_done() {
        let mut ob = Onboarding::default();
        ob.record(result(
            Step::Autostart,
            StepOutcome::skipped("测试里不碰真实计划任务"),
        ));
        assert!(ob.step_done(Step::Autostart));
        assert_eq!(ob.failed_steps.len(), 0, "跳过的步骤不算失败");
    }

    #[test]
    fn virtual_driver_is_required_and_blocks_completion() {
        // 4.0 起虚拟显示驱动是必需的：只做手动提示不能让向导通过。
        let mut ob = Onboarding::default();
        for step in [
            Step::Elevate,
            Step::Autostart,
            Step::InputGuard,
            Step::SelfTest,
        ] {
            ob.record(result(step, StepOutcome::ok("ok")));
        }
        ob.remote_confirmed = true;
        let err = ob.finish(now()).unwrap_err().to_string();
        assert!(err.contains("虚拟显示驱动"), "{err}");

        ob.record(result(
            Step::VirtualDriver,
            StepOutcome::manual("在设置里点“安装驱动”，或运行 install-vdd", None),
        ));
        assert!(ob.finish(now()).is_err(), "手动状态同样不该放行");

        ob.record(result(
            Step::VirtualDriver,
            StepOutcome::ok("已装好并通过验证"),
        ));
        ob.finish(now()).unwrap();
        assert!(ob.completed);
    }

    #[test]
    fn finish_requires_confirmation_and_required_steps() {
        let mut ob = Onboarding::default();
        let err = ob.finish(now()).unwrap_err().to_string();
        assert!(err.contains("已确认远程能连"), "{err}");

        ob.remote_confirmed = true;
        let err = ob.finish(now()).unwrap_err().to_string();
        assert!(err.contains("授权并准备后台服务"), "{err}");

        for step in Step::ALL.iter().copied().filter(|s| s.required()) {
            ob.record(result(step, StepOutcome::ok("ok")));
        }
        ob.finish(now()).unwrap();
        assert!(ob.completed);
        assert!(!ob.should_show());
    }

    #[test]
    fn required_steps_include_the_virtual_driver() {
        assert!(Step::VirtualDriver.required());
        // Confirm 是勾选框本身，不算执行步骤。
        assert!(!Step::Confirm.required());
    }

    #[test]
    fn repair_runs_only_failed_steps() {
        let mut ob = Onboarding::default();
        ob.record(result(
            Step::Elevate,
            StepOutcome::failed("用户取消了管理员确认", "重新点“重新授权”"),
        ));
        ob.record(result(Step::Autostart, StepOutcome::ok("ok")));
        assert_eq!(ob.steps_to_repair(), vec![Step::Elevate]);

        let fresh = Onboarding::default();
        assert_eq!(fresh.steps_to_repair().len(), Step::ALL.len());
    }

    #[test]
    fn outcomes_serialize_with_status_tag() {
        let json = serde_json::to_string(&StepOutcome::ok("完成")).unwrap();
        assert!(json.contains("\"status\":\"ok\""), "{json}");
        let back: StepOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(back, StepOutcome::ok("完成"));
        let manual = StepOutcome::manual("看这里", Some("https://example.com".into()));
        assert!(manual.failed_hard());
        assert_eq!(manual.action_hint().as_deref(), Some("https://example.com"));
    }

    #[test]
    fn log_hint_points_at_logs_dir() {
        let ob = Onboarding::default();
        assert!(ob.log_hint().contains("logs"));
    }
}
