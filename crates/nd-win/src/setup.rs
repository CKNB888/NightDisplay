//! 首次运行向导的执行器：把 `nd_core::onboarding` 定义的每一步真正做出来。
//!
//! 六个步骤，每个都报告**真实结果**（成功 / 跳过 / 需要手动 / 失败），
//! 并且失败时必须给出可以照做的一条命令或一个链接。
//!
//! ```text
//! 1 授权并准备后台服务   → 一次 UAC，之后夜间不再弹（提升助手常驻）
//! 2 虚拟显示驱动（可选） → pnputil 静默安装，默认跳过
//! 3 开机自启与看门狗     → Run 键 + 计划任务 + 看门狗
//! 4 本地防触控与锁屏     → 写配置并立即生效
//! 5 15 秒自检           → 真的黑屏 15 秒再恢复，附远程连接提示
//! 6 确认远程能连         → 勾选框，写入完成状态
//! ```
//!
//! 关键约束：**GUI/CLI 自己从不提权**。只有第 1 步会拉起提升助手，
//! 第 2/3 步需要特权的动作都通过助手执行。

#![cfg(windows)]

use std::time::Instant;

use anyhow::Result;
use nd_core::config::Config;
use nd_core::onboarding::{Onboarding, Step, StepOutcome, StepResult};

use crate::brightness;
use crate::gamma;
use crate::helper;
use crate::overlay;
use crate::power;
use crate::tasks;
use crate::vdd;

/// 进度回调的类型别名（GUI 用它把每一步的状态推到界面上）。
pub type ProgressFn = Box<dyn Fn(Step, &str) + Send>;

/// 向导选项。
pub struct SetupOptions {
    /// 用户勾选了"兼容老远程软件"，需要安装虚拟显示驱动。
    pub install_vdd: bool,
    /// 自检秒数（默认 15）。
    pub self_test_seconds: u64,
    /// 只跑这些步骤（修复模式）；为空表示按 `Step::ALL` 全跑。
    pub only: Vec<Step>,
    /// 进度回调：`(step, 状态文字)`。GUI 用它做实时状态。
    pub progress: Option<ProgressFn>,
}

impl std::fmt::Debug for SetupOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SetupOptions")
            .field("install_vdd", &self.install_vdd)
            .field("self_test_seconds", &self.self_test_seconds)
            .field("only", &self.only)
            .field("progress", &self.progress.is_some())
            .finish()
    }
}

impl Default for SetupOptions {
    fn default() -> Self {
        Self {
            install_vdd: false,
            self_test_seconds: 15,
            only: Vec::new(),
            progress: None,
        }
    }
}

impl SetupOptions {
    fn wants(&self, step: Step) -> bool {
        self.only.is_empty() || self.only.contains(&step)
    }

    fn notify(&self, step: Step, text: &str) {
        if let Some(cb) = self.progress.as_ref() {
            cb(step, text);
        }
    }
}

/// 跑整个向导（或只跑指定步骤），每一步都记录真实结果。
///
/// 返回更新后的向导状态；调用方负责 `save_state`。
pub fn run(options: &SetupOptions, onboarding: &mut Onboarding) -> Result<Vec<StepResult>> {
    let mut results = Vec::new();
    for step in Step::ALL {
        if !options.wants(step) {
            continue;
        }
        // 第 2 步只有用户明确要装才跑，否则直接记"跳过"。
        let started = Instant::now();
        options.notify(step, "正在执行…");
        let outcome = match step {
            Step::Elevate => step_elevate(onboarding),
            Step::VirtualDriver => step_virtual_driver(options.install_vdd),
            Step::Autostart => step_autostart(),
            Step::InputGuard => step_input_guard(),
            Step::SelfTest => step_self_test(options.self_test_seconds, options),
            Step::Confirm => step_confirm(onboarding),
        };
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let result = StepResult {
            step,
            outcome,
            elapsed_ms,
        };
        tracing::info!(
            step = step.title(),
            outcome = %result.outcome.summary(),
            elapsed_ms,
            "向导步骤结束"
        );
        options.notify(step, &result.outcome.summary());
        onboarding.record(result.clone());
        results.push(result);
    }
    Ok(results)
}

/// 1. 申请管理员权限并保持一个提升助手常驻。
fn step_elevate(onboarding: &mut Onboarding) -> StepOutcome {
    if power::is_elevated() && helper::is_running() {
        onboarding.helper_ready = helper::ping().is_ok();
        return StepOutcome::ok("提升助手已经在运行，以后的定时黑屏不会再弹管理员确认");
    }
    onboarding.helper_ready = false;
    match helper::ensure_running() {
        Ok(()) => match helper::ping() {
            Ok(version) => {
                onboarding.helper_ready = true;
                StepOutcome::ok(format!("{version} 已就绪：夜间操作不再需要管理员确认"))
            }
            Err(e) => StepOutcome::manual(
                format!("授权窗口点了“是”，但助手没有应答（{e}）"),
                Some(helper_log_hint()),
            ),
        },
        Err(e) => {
            let text = crate::error::user_text(&e, "授权失败");
            // 用户点了"否"是最常见的情况，给出的下一步必须是可执行的。
            StepOutcome::manual(text, Some(helper_log_hint()))
        }
    }
}

fn helper_log_hint() -> String {
    nd_core::paths::logs_dir().display().to_string()
}

/// 2. 可选：安装虚拟显示驱动。
fn step_virtual_driver(wanted: bool) -> StepOutcome {
    if vdd::installed() {
        return StepOutcome::ok("虚拟显示驱动已安装（“仅虚拟屏”模式可用）");
    }
    if !wanted {
        return StepOutcome::skipped(
            "没有勾选“兼容老远程软件”。默认的防窥黑幕不需要虚拟屏，随时可以在设置里再装",
        );
    }
    match helper::call(helper::Command::InstallVdd) {
        Ok(text) => StepOutcome::ok(text),
        Err(e) => {
            let text = crate::error::user_text(&e, "安装虚拟显示驱动失败");
            StepOutcome::manual(text, Some(nd_core::REPO_URL.to_string()))
        }
    }
}

/// 3. 开机自启 + 计划任务 + 看门狗。
fn step_autostart() -> StepOutcome {
    let home = nd_core::paths::home_dir();
    let mut notes: Vec<String> = Vec::new();
    let mut problems: Vec<String> = Vec::new();

    // 3a. HKCU Run（不需要管理员）。
    match crate::autostart::set(true, &home) {
        Ok(()) => notes.push("开机自启已注册".into()),
        Err(e) => problems.push(crate::error::user_text(&e, "开机自启注册失败")),
    }

    // 3b. 计划任务（需要管理员 → 走助手）。
    let cfg = Config::load().unwrap_or_default();
    let boundaries: Vec<String> = cfg
        .schedule()
        .boundaries()
        .into_iter()
        .map(|m| m.to_hm())
        .collect();
    match install_tasks(&boundaries) {
        Ok(text) => notes.push(text),
        Err(e) => problems.push(crate::error::user_text(&e, "计划任务登记失败")),
    }

    // 3c. 看门狗任务（登录时拉起）。
    match install_watchdog() {
        Ok(text) => notes.push(text),
        Err(e) => problems.push(crate::error::user_text(&e, "看门狗登记失败")),
    }

    if problems.is_empty() {
        StepOutcome::ok(notes.join("；"))
    } else {
        let instructions = format!(
            "{}\n手动登记计划任务：以管理员身份运行\n  \"{}\" --action serve\n然后在设置里点“重新授权/修复”。",
            problems.join("；"),
            crate::sysutil::exe_path().display()
        );
        StepOutcome::manual(instructions, Some(helper_log_hint()))
    }
}

fn install_tasks(boundaries: &[String]) -> Result<String> {
    // 已经是管理员就直接做，否则走助手。
    if power::is_elevated() {
        let names = tasks::install_boundaries(boundaries, &nd_core::paths::home_dir())?;
        return Ok(format!(
            "已登记 {} 个每日任务：{}",
            names.len(),
            names.join("、")
        ));
    }
    helper::call(helper::Command::InstallTasks {
        boundaries: boundaries.to_vec(),
    })
}

fn install_watchdog() -> Result<String> {
    if power::is_elevated() {
        tasks::install_watchdog(&nd_core::paths::home_dir())?;
        return Ok("看门狗任务已登记".into());
    }
    helper::call(helper::Command::InstallWatchdog)
}

/// 4. 本地防触控 + 抑制锁屏：写配置并立刻生效。
fn step_input_guard() -> StepOutcome {
    let mut notes = Vec::new();
    let mut problems = Vec::new();

    // 配置是这件事的"事实来源"，先写。
    match Config::update(|c| {
        c.strict_local_input = true;
        c.suppress_lock = true;
        Ok(())
    }) {
        Ok(_) => notes.push(format!(
            "已开启严格本地防触控（解锁组合键 {}）",
            Config::load().map(|c| c.chord_text()).unwrap_or_default()
        )),
        Err(e) => problems.push(format!("写入设置失败：{e}")),
    }

    // 立即尝试抑制锁屏，确认这条链路能用。
    match power::set_lock_suppression(true) {
        Ok(()) => {
            notes.push("已开启夜间抑制锁屏".into());
            let _ = power::set_lock_suppression(false);
        }
        Err(e) => problems.push(crate::error::user_text(&e, "抑制锁屏失败")),
    }

    if problems.is_empty() {
        StepOutcome::ok(notes.join("；"))
    } else {
        StepOutcome::failed(
            problems.join("；"),
            "可以在设置里手动打开“严格本地防触控”和“夜间抑制锁屏”，或查看日志",
        )
    }
}

/// 5. 15 秒自检：真的黑屏，再自动恢复。
fn step_self_test(seconds: u64, options: &SetupOptions) -> StepOutcome {
    options.notify(Step::SelfTest, "正在准备黑幕…");

    // 先看关键能力在不在，缺了就直接说清楚缺什么，不浪费用户 15 秒。
    let build = power::windows_build();
    if build != 0 && build < 19041 {
        return StepOutcome::failed(
            format!(
                "当前 Windows build {build} 不支持“把黑幕排除在屏幕采集之外”（需要 Windows 10 2004 及以上）"
            ),
            "升级 Windows 后再试；或者在设置里手动选择“仅虚拟屏”（会中断远程画面一次）",
        );
    }

    options.notify(Step::SelfTest, "正在显示黑幕并校验采集排除…");
    // 自检用的黑幕由本进程自己创建，结束后必须自己收掉。
    let overlays = overlay::Overlays::new();
    let shown = match overlays.show() {
        Ok(outcomes) => outcomes,
        Err(e) => {
            overlays.destroy_all();
            return StepOutcome::failed(
                crate::error::user_text(&e, "黑幕没有成功显示"),
                "查看日志里的详细原因；如果是显卡驱动问题，重启后再试",
            );
        }
    };
    let failed: Vec<&nd_core::mode::OverlayOutcome> = shown.iter().filter(|o| !o.ok()).collect();
    if !failed.is_empty() {
        let detail = failed
            .iter()
            .map(|o| {
                format!(
                    "{}：{}",
                    o.display,
                    o.error.clone().unwrap_or_else(|| "未知原因".into())
                )
            })
            .collect::<Vec<_>>()
            .join("；");
        let _ = overlays.hide();
        overlays.destroy_all();
        return StepOutcome::failed(detail, "查看日志；必要时更新显卡驱动后重试");
    }

    // gamma + 背光：能生效就生效，让用户看到的和晚上完全一样。
    let gamma_status = gamma::apply();
    let (brightness_note, brightness_control) = match brightness::Control::open() {
        Ok(control) => {
            let (status, unsupported) = control.to_zero();
            let mut note = status.summary("背光");
            if !unsupported.is_empty() {
                note.push_str(&format!(
                    "（{} 不支持 DDC/CI，这是正常的）",
                    unsupported.join("、")
                ));
            }
            (note, Some(control))
        }
        Err(e) => (crate::error::user_text(&e, "背光控制不可用"), None),
    };

    options.notify(
        Step::SelfTest,
        &format!("正在黑屏 {seconds} 秒 —— 现在请用 UU / ToDesk 连一下，确认能看到并能操作画面"),
    );
    std::thread::sleep(std::time::Duration::from_secs(seconds.clamp(3, 60)));

    // 恢复：先收黑幕，再按真实夜间退出的顺序还原 gamma 和背光。
    let mut restore_problems = Vec::new();
    if let Err(e) = overlays.hide() {
        restore_problems.push(format!("收起黑幕：{e}"));
    }
    overlays.destroy_all();
    if let Some(control) = brightness_control {
        if let Err(e) = control.restore() {
            restore_problems.push(format!("背光：{e}"));
        }
    }
    if let Err(e) = gamma::restore() {
        restore_problems.push(format!("gamma：{e}"));
    }

    let seconds_shown = seconds.clamp(3, 60);
    let summary = format!(
        "{} 块屏黑幕已确认排除采集，首帧 {}ms；{}；{}；{} 秒后已恢复",
        shown.len(),
        shown.iter().map(|o| o.first_frame_ms).max().unwrap_or(0),
        gamma_status.summary("gamma"),
        brightness_note,
        seconds_shown
    );

    if restore_problems.is_empty() {
        StepOutcome::ok(format!(
            "{summary}。\n远程连接检查：黑屏期间如果你能用远程软件看到并操作画面，就说明配置正确。"
        ))
    } else {
        StepOutcome::failed(
            format!("自检后恢复不完整：{}", restore_problems.join("；")),
            "点“重新授权/修复”重跑自检；如果屏幕还是黑的，运行 NightDisplay.exe --action emergency",
        )
    }
}

/// 6. 确认远程能连。这一步本身不做事，只记录用户勾选。
fn step_confirm(onboarding: &mut Onboarding) -> StepOutcome {
    if onboarding.remote_confirmed {
        StepOutcome::ok("已确认远程能连，下次不再显示向导")
    } else {
        StepOutcome::manual(
            "请勾选“已确认远程能连”再结束向导（刚才自检黑屏时远程能看到画面即可）",
            None,
        )
    }
}

/// 向导结束：写完成状态。
pub fn finish(onboarding: &mut Onboarding) -> Result<String> {
    onboarding.finish(chrono::Local::now())?;
    nd_core::onboarding::save_state(onboarding);
    let lines = nd_core::onboarding::summary_lines(onboarding);
    Ok(format!(
        "向导已完成，不会再自动弹出。\n{}",
        lines.join("\n")
    ))
}

/// 修复模式：只重跑失败的步骤（没有失败记录时全部重查）。
pub fn repair(onboarding: &mut Onboarding, install_vdd: bool) -> Result<Vec<StepResult>> {
    let only = onboarding.steps_to_repair();
    tracing::info!(steps = ?only, "开始修复失败的步骤");
    let options = SetupOptions {
        install_vdd,
        only,
        ..Default::default()
    };
    let results = run(&options, onboarding)?;
    nd_core::onboarding::save_state(onboarding);
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_default_checks_everything() {
        let options = SetupOptions::default();
        for step in Step::ALL {
            assert!(options.wants(step));
        }
        assert!(!options.install_vdd);
        assert_eq!(options.self_test_seconds, 15);
    }

    #[test]
    fn only_list_limits_steps() {
        let options = SetupOptions {
            only: vec![Step::Autostart],
            ..Default::default()
        };
        assert!(options.wants(Step::Autostart));
        assert!(!options.wants(Step::SelfTest));
    }

    #[test]
    fn confirm_requires_checkbox() {
        let mut ob = Onboarding::default();
        let outcome = step_confirm(&mut ob);
        assert!(matches!(outcome, StepOutcome::Manual { .. }));
        ob.remote_confirmed = true;
        assert!(matches!(step_confirm(&mut ob), StepOutcome::Ok { .. }));
    }

    #[test]
    fn virtual_driver_is_skipped_by_default() {
        let outcome = step_virtual_driver(false);
        assert!(matches!(outcome, StepOutcome::Skipped { .. }));
        // 没勾选时绝不应该去碰助手或驱动。
        if let StepOutcome::Skipped { reason } = outcome {
            assert!(reason.contains("防窥黑幕不需要"));
        }
    }
}
