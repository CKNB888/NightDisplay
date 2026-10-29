//! 一次性动作：每个 `--action` 一个函数，返回退出码。
//!
//! 这里**不做业务判断**，只负责：调用引擎、把结果变成人能看懂的中文、
//! 决定退出码。所有显示相关的逻辑都在 `nd_core::engine`。

use anyhow::Result;
use nd_core::config::Config;
use nd_core::diag;
use nd_core::engine::{Engine, Outcome, Platform};
use nd_core::mode::Mode;
use nd_core::onboarding::{self, Onboarding, Step};
use nd_core::status::{self, SelfCheck};
use nd_win::helper;
use nd_win::pipe;
use nd_win::setup::{self, SetupOptions};
use nd_win::{brightness, topology, update, vdd, watchdog, WinPlatform};
use std::process::ExitCode;

use super::Args;

type NdResult<T> = nd_core::NdResult<T>;

/// 把引擎结果打出来并换算退出码：有错误就是 1。
fn report(outcome: &Outcome) -> ExitCode {
    println!("{}", outcome.message);
    for e in &outcome.errors {
        eprintln!("注意：{e}");
    }
    if outcome.is_ok() {
        ExitCode::SUCCESS
    } else {
        println!("如仍异常：按住解锁组合键，或运行 NightDisplay --action emergency");
        ExitCode::from(1)
    }
}

/// 尝试把动作交给常驻进程。`Ok(None)` 表示常驻进程没在跑。
fn ask_daemon(
    action: &str,
    detail: Option<&nd_core::ipc::CommandDetail>,
) -> Result<Option<String>> {
    match pipe::call_daemon(action, detail, 60_000)? {
        Some(resp) if resp.ok => Ok(Some(resp.result)),
        Some(resp) => Err(nd_core::error::user(resp.result)),
        None => Ok(None),
    }
}

// ----------------------------------------------------------------------
// 图形界面
// ----------------------------------------------------------------------

/// 图形界面（Tauri 2 + WebView2）。
///
/// 界面和命令行是**同一个可执行文件**：双击、或者不带参数运行就进界面，
/// 带 `--action xxx` 就是一次性命令。这样计划任务、自启注册指向的路径永远只有一个，
/// 不会出现"任务拉起的 exe 没有界面"这种事。
///
/// `--tray`（旧版启动项）只是登录时对齐一次，不打开窗口打扰用户。
pub fn gui(args: &Args) -> Result<ExitCode> {
    let tray_boot = args.tray || args.action == "tray";
    if tray_boot && ask_daemon("reconcile", None)?.is_some() {
        return Ok(ExitCode::SUCCESS);
    }
    nd_gui::run()
}

// ----------------------------------------------------------------------
// 首次运行向导
// ----------------------------------------------------------------------

fn step_from_name(name: &str) -> NdResult<Step> {
    match name.trim() {
        "elevate" | "authorize" => Ok(Step::Elevate),
        "virtual_driver" | "vdd" => Ok(Step::VirtualDriver),
        "autostart" => Ok(Step::Autostart),
        "input_guard" => Ok(Step::InputGuard),
        "self_test" | "selftest" => Ok(Step::SelfTest),
        "confirm" => Ok(Step::Confirm),
        other => Err(nd_core::error::user(format!(
            "不认识的向导步骤“{other}”。可用：elevate, virtual_driver, autostart, input_guard, self_test, confirm"
        ))),
    }
}

fn print_step(result: &onboarding::StepResult) {
    println!(
        "  [{}] {}（{}ms）",
        result.step.title(),
        result.outcome.summary(),
        result.elapsed_ms
    );
    if let Some(hint) = result.outcome.action_hint() {
        println!("        下一步：{hint}");
    }
}

/// `--action setup`：首个向导。已有完成记录时不再重复打扰。
pub fn setup(args: &Args) -> Result<ExitCode> {
    let mut state = onboarding::load_state();
    if !state.should_show() && args.only.is_empty() {
        println!(
            "向导已经完成过（{}），不会再自动弹出。",
            completed_text(&state)
        );
        println!("需要重新检查的话：NightDisplay --action repair");
        return Ok(ExitCode::SUCCESS);
    }
    let only = args
        .only
        .iter()
        .map(|s| step_from_name(s))
        .collect::<NdResult<Vec<_>>>()?;
    let options = SetupOptions {
        install_vdd: !args.no_vdd,
        self_test_seconds: args.seconds.clamp(3, 60),
        only,
        ..Default::default()
    };
    println!("NightDisplay 安装向导：只弹一次管理员确认，之后夜间切换不再打扰。");
    println!("（黑屏只作用于本机屏幕；远程软件照常能看到并操作桌面。）");
    println!();
    if state.should_show() {
        state.remote_confirmed = false;
    }
    let results = setup::run(&options, &mut state)?;
    for result in &results {
        print_step(result);
    }
    onboarding::save_state(&state);
    println!();
    if !options.only.is_empty() {
        // 只跑了指定步骤（测试或单项排查），没打算走完向导，
        // 不能因为"还有别的步骤没跑"就报失败。
        let bad = results.iter().filter(|r| r.outcome.failed_hard()).count();
        if bad > 0 {
            println!("{bad} 个步骤需要处理，见上面每条的“下一步”。");
            println!("{}", state.log_hint());
            return Ok(ExitCode::from(1));
        }
        println!("指定的步骤都处理完了。");
        return Ok(ExitCode::SUCCESS);
    }
    Ok(finish_wizard(&mut state, &options))
}

/// `--action repair`：重新授权 / 修复，只重跑失败的步骤。
pub fn repair(args: &Args) -> Result<ExitCode> {
    let mut state = onboarding::load_state();
    let steps = state.steps_to_repair();
    if state.failed_steps.is_empty() && !state.should_show() {
        println!("上次没有失败的步骤。这里会把每一步重新检查一遍：");
    }
    println!(
        "重新检查：{}",
        steps
            .iter()
            .map(|s| s.title())
            .collect::<Vec<_>>()
            .join("、")
    );
    let results = setup::repair(&mut state, !args.no_vdd)?;
    for result in &results {
        print_step(result);
    }
    println!();
    Ok(finish_wizard(&mut state, &SetupOptions::default()))
}

/// 向导收尾：必需步骤都过了就写完成状态，否则列出没过的步骤。
fn finish_wizard(state: &mut Onboarding, options: &SetupOptions) -> ExitCode {
    let pending: Vec<&str> = Step::ALL
        .iter()
        .filter(|s| s.required())
        .filter(|s| !state.step_done(**s))
        .map(|s| s.title())
        .collect();
    if !pending.is_empty() {
        println!("还有这些必需步骤没有完成：{}", pending.join("、"));
        println!("按上面每条的“下一步”做完，再运行：NightDisplay --action repair");
        println!("{}", state.log_hint());
        return ExitCode::from(1);
    }
    // 自检已经让用户看过黑屏；命令行里不能再弹一个勾选框，
    // 所以按"用户主动跑完向导"记为已确认，并把确认点写进日志。
    if !state.remote_confirmed {
        state.remote_confirmed = true;
        println!(
            "已记为“已确认远程能连”。如果当时没能远程连上，请稍后运行 NightDisplay --action test 再看一次。"
        );
    }
    match setup::finish(state) {
        Ok(text) => {
            println!("{text}");
            println!(
                "下一步：NightDisplay --action test 会黑屏 {} 秒，然后自动恢复；",
                options.self_test_seconds
            );
            println!(
                "        NightDisplay --action activate --start 20:00 --end 10:30 启用每日计划。"
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!("{}", nd_core::error::one_line(&error));
            ExitCode::from(1)
        }
    }
}

fn completed_text(state: &Onboarding) -> String {
    match state.completed_at {
        Some(at) => at.format("%Y-%m-%d %H:%M").to_string(),
        None => "时间未知".into(),
    }
}

/// `--action onboarding`：打印每一步的真实结果，支持排障。
pub fn onboarding_report() -> ExitCode {
    let state = onboarding::load_state();
    println!(
        "向导状态：{}",
        if state.completed {
            "已完成"
        } else {
            "未完成"
        }
    );
    if state.completed {
        println!("完成时间：{}", completed_text(&state));
    }
    println!(
        "已确认远程能连：{}",
        if state.remote_confirmed { "是" } else { "否" }
    );
    println!();
    for line in onboarding::summary_lines(&state) {
        println!("  {line}");
    }
    if !state.failed_steps.is_empty() {
        println!();
        println!(
            "需要修复：{}",
            state
                .failed_steps
                .iter()
                .map(|s| s.title())
                .collect::<Vec<_>>()
                .join("、")
        );
        println!("运行 NightDisplay --action repair 只重跑这些步骤。");
    }
    println!("{}", state.log_hint());
    ExitCode::SUCCESS
}

// ----------------------------------------------------------------------
// 状态与诊断
// ----------------------------------------------------------------------

pub fn status() -> Result<ExitCode> {
    let cfg = Config::load()?;
    let caps = nd_win::current_capabilities();
    let text = status::status_text(&cfg, &caps, chrono::Local::now());
    if let Err(e) = std::fs::write(nd_core::paths::status_file(), format!("{text}\n")) {
        tracing::warn!(error = %e, "状态文件写入失败");
    }
    println!("{text}");
    Ok(ExitCode::SUCCESS)
}

pub fn selfcheck() -> Result<ExitCode> {
    let cfg = Config::load()?;
    let caps = nd_win::current_capabilities();
    let report = SelfCheck::build(&cfg, &caps);
    let path = nd_core::paths::selfcheck_file();
    let text = serde_json::to_string_pretty(&report)
        .map_err(|e| nd_core::error::user(format!("自检报告无法序列化：{e}")))?;
    std::fs::write(&path, &text)
        .map_err(|e| nd_core::error::user(format!("自检报告写入失败：{e}")))?;
    // 人看上半段，机器看下半段；`--action selfcheck` 的输出可以直接贴。
    print!("{}", diag::selfcheck_summary(&report));
    println!("\n—— 下面是完整 JSON（也是 selfcheck.json 的内容）——");
    println!("{text}");
    println!("\n已写入 {}", path.display());
    Ok(ExitCode::SUCCESS)
}

/// 导出诊断包：一个 ZIP，装日志、脱敏配置、自检、显示能力和最近几次会话。
///
/// 用户排障时只要把生成的文件发出来，不用再问东问西。里面不含画面内容，
/// 也不含远程工具的账号；本机路径和 IPC 令牌都会换成占位符。
pub fn export_diag(dir: Option<&str>) -> Result<ExitCode> {
    let target = dir.map(std::path::PathBuf::from);
    let path = nd_win::diag::export(target.as_deref())?;
    println!("诊断包已导出：{}", path.display());
    println!("里面没有画面内容，本机路径和令牌已替换成 <home> / <token>。");
    Ok(ExitCode::SUCCESS)
}

/// 显示切换的实测统计（JSON）+ 当前所有显示路径。
///
/// 这是 Windows QA 用的探针，也是排障时最有说服力的一段证据：
/// * `apply_calls` —— 这次进程里到底改了几次显示配置；
/// * `min_active_submitted` —— 每次提交时活动 target 的最小值，`0` 是不允许出现的；
/// * `physical_count` —— 实体屏此刻是否还在 `QueryDisplayConfig` 的完整路径集里
///   （这是"远程采集表面没死"的代理指标）。注意这只是**一个快照**；要断言
///   "始终没消失"，得由 QA 在切换期间连续采样，本探针不替你下这个结论。
pub fn probe() -> Result<ExitCode> {
    let stats = nd_win::topology::apply_stats();
    // 完整路径集（QDC_ALL_PATHS）：没被接进桌面的屏也必须还在。
    let all = nd_win::topology::describe(false)?;
    let active: Vec<_> = all.iter().filter(|d| d.active).collect();
    let physical_total = all
        .iter()
        .filter(|d| !nd_win::topology::is_virtual(d))
        .count();
    let virtual_total = all.len() - physical_total;
    let report = serde_json::json!({
        "stats": stats,
        "displays": all.iter().map(|d| serde_json::json!({
            "target_id": d.target_id,
            "active": d.active,
            "virtual": nd_win::topology::is_virtual(d),
            "tech": d.tech,
            "name": d.name,
            "device_path": d.device_path,
        })).collect::<Vec<_>>(),
        // 活动屏的真实信号时序：虚拟屏和实体屏的模式是否一致，
        // QA 就是拿这两个数字比出来的。
        "active_modes": nd_win::topology::monitor_modes().unwrap_or_default()
            .iter()
            .map(|m| serde_json::json!({
                "name": m.name,
                "width": m.width,
                "height": m.height,
                "refresh_hz": m.refresh_hz,
                "virtual": m.virtual_output,
            })).collect::<Vec<_>>(),
        "active_count": active.len(),
        "physical_count": physical_total,
        "virtual_count": virtual_total,
        "virtual_holding": nd_win::topology::virtual_screen_holding(),
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into())
    );
    Ok(ExitCode::SUCCESS)
}

/// 把显示切换统计清零（QA 每轮开始时用，让断言只看自己这一段）。
pub fn probe_reset() -> ExitCode {
    nd_win::topology::reset_apply_stats();
    println!("已清零显示切换统计。");
    ExitCode::SUCCESS
}

/// 本机能力，和旧版 `--action env` 对应。
pub fn env() -> Result<ExitCode> {
    let caps = nd_win::current_capabilities();
    let cfg = Config::load()?;
    let body = serde_json::json!({
        "version": nd_core::VERSION,
        "os": caps.os,
        "build": caps.build,
        "capture_exclusion": caps.capture_exclusion,
        "gamma_control": caps.gamma_control,
        "brightness_displays": caps.brightness_displays,
        "displays": caps.displays,
        "elevated": caps.elevated,
        "helper_running": caps.helper_running,
        "virtual_display": caps.virtual_display,
        "autostart_actual": caps.autostart,
        "autostart_config": cfg.autostart,
        "effective_mode": nd_core::mode::resolve(cfg.mode).as_str(),
        "ranges": cfg.range_strings(),
        "unlock_chord": cfg.chord_text(),
        "home": nd_core::paths::home_dir().display().to_string(),
        "vdd_script": vdd::script_path().map(|p| p.display().to_string()),
    });
    println!("{}", serde_json::to_string_pretty(&body)?);
    Ok(ExitCode::SUCCESS)
}

pub fn displays() -> Result<ExitCode> {
    let list = topology::describe(false)?;
    if list.is_empty() {
        println!("没有检测到显示器。");
        return Ok(ExitCode::SUCCESS);
    }
    for d in list {
        println!(
            "{:>3} {:<8} {:<9} tech={:<3} {}",
            d.target_id,
            if d.active { "active" } else { "inactive" },
            if topology::is_virtual(&d) {
                "virtual"
            } else {
                "physical"
            },
            d.tech,
            if d.name.is_empty() {
                d.device_path.clone()
            } else {
                d.name.clone()
            }
        );
    }
    println!();
    println!("提示：虚拟屏模式只会摘掉实体屏的桌面，上面的输出不会消失。");
    Ok(ExitCode::SUCCESS)
}

/// 演练虚拟屏切换：先校验「预热」，再校验「入夜」，**都不应用**。
///
/// 这台机器的显卡/驱动能不能做到"一次调用、不闪"，就在这一步看得出来。
pub fn plan_virtual() -> Result<ExitCode> {
    let cfg = Config::load()?;
    let arm = nd_win::topology::arm(&cfg.virtual_match, true)?;
    println!("① 预热（虚拟屏接入桌面，复制模式）：校验通过");
    println!("   {}", arm.summary());
    println!();
    // 入夜校验要在"预热已生效"的前提下才有意义。这里只能校验当前拓扑，
    // 所以如果虚拟屏还没接进桌面，就说明需要先预热。
    let holding = nd_win::topology::virtual_screen_holding();
    if holding {
        let detach = nd_win::topology::detach(&cfg.virtual_match, true)?;
        println!("② 入夜（摘掉实体屏的桌面）：校验通过");
        println!("   {}", detach.summary());
    } else {
        println!("② 入夜（摘掉实体屏的桌面）：虚拟屏还没预热，暂无法校验");
        println!("   程序启动时会自动预热；也可以执行 --action night 直接切一次。");
    }
    println!();
    println!("以上只是校验，显示配置没有任何改动。");
    Ok(ExitCode::SUCCESS)
}

pub fn backup() -> Result<ExitCode> {
    let platform = WinPlatform::new();
    platform.backup_display()?;
    println!(
        "已备份显示配置到 {}",
        nd_core::paths::backups_dir().display()
    );
    Ok(ExitCode::SUCCESS)
}

pub fn restore() -> Result<ExitCode> {
    let level = topology::restore()?;
    println!("显示配置已还原（{level}）");
    Ok(ExitCode::SUCCESS)
}

pub fn awake(seconds: u64) -> Result<ExitCode> {
    nd_win::power::keep_awake(true, seconds)?;
    println!("已开始保持唤醒 {seconds} 秒");
    Ok(ExitCode::SUCCESS)
}

pub fn update() -> ExitCode {
    match update::check_text() {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            // 检查更新失败不该被当成软件故障。
            println!("{}", nd_win::error::user_text(&error, "这次没查到更新"));
            ExitCode::SUCCESS
        }
    }
}

// ----------------------------------------------------------------------
// 夜间 / 白天
// ----------------------------------------------------------------------

/// 让常驻进程做，常驻进程不在时自己动手（结果一致，只是少了黑幕预热）。
fn resident_or_local(
    action: &str,
    detail: &[(String, String)],
    local: impl FnOnce(&WinPlatform) -> NdResult<Outcome>,
) -> Result<ExitCode> {
    let detail = if detail.is_empty() {
        None
    } else {
        Some(nd_core::ipc::CommandDetail {
            action: action.to_string(),
            mode: detail
                .iter()
                .find(|(k, _)| k == "mode")
                .map(|(_, v)| v.clone()),
            seconds: detail
                .iter()
                .find(|(k, _)| k == "seconds")
                .and_then(|(_, v)| v.parse().ok()),
            test_id: detail
                .iter()
                .find(|(k, _)| k == "test_id")
                .map(|(_, v)| v.clone()),
        })
    };
    if let Some(text) = ask_daemon(action, detail.as_ref())? {
        if text != "ok" {
            println!("{text}");
        }
        return Ok(ExitCode::SUCCESS);
    }
    let platform = WinPlatform::new();
    let outcome = local(&platform)?;
    platform.shutdown();
    Ok(report(&outcome))
}

pub fn night(args: &Args) -> Result<ExitCode> {
    let mut detail = Vec::new();
    if let Some(mode) = args.mode.clone() {
        detail.push(("mode".to_string(), mode));
    }
    resident_or_local("night", &detail, |platform| {
        let cfg = Config::load()?;
        let mode = match args.mode.as_deref() {
            Some(m) => Mode::parse(m)?,
            None => cfg.mode,
        };
        // 本地直接进入夜间时也把窗口预热，避免"先亮一下再黑"。
        platform.warm()?;
        let engine = Engine::new(platform);
        engine.night(mode, chrono::Local::now())
    })
}

pub fn day(args: &Args) -> Result<ExitCode> {
    // 黑屏锁定期间，本地不允许直接恢复（远程用 --action emergency）；
    // 测试自带的恢复进程带 --test-id / --test-recovery，必须放行。
    let is_test_recovery = args.test_recovery || !args.test_id.is_empty();
    if !is_test_recovery {
        let cfg = Config::load()?;
        if cfg.is_locked() {
            println!(
                "黑屏已锁定：本地请按住解锁组合键；远程请运行 NightDisplay --action emergency"
            );
            return Ok(ExitCode::from(2));
        }
    }

    // 有 test-id 且是恢复阶段：让常驻进程收尾，它能顺手把测试结果记上。
    if !args.test_id.is_empty() {
        let detail = nd_core::ipc::CommandDetail {
            action: "test_day".into(),
            mode: None,
            seconds: None,
            test_id: Some(args.test_id.clone()),
        };
        if let Some(text) = ask_daemon("test_day", Some(&detail))? {
            println!("{text}");
            return Ok(ExitCode::SUCCESS);
        }
        let platform = WinPlatform::new();
        let engine = Engine::new(&platform);
        let outcome = engine.finish_test(&args.test_id, chrono::Local::now())?;
        platform.shutdown();
        return Ok(report(&outcome));
    }

    resident_or_local("day", &[], |platform| {
        let engine = Engine::new(platform);
        engine.day()
    })
}

pub fn reconcile() -> Result<ExitCode> {
    if let Some(text) = ask_daemon("reconcile", None)? {
        if text != "ok" {
            println!("{text}");
        }
        return Ok(ExitCode::SUCCESS);
    }
    // 常驻进程不在：按计划对齐一次就走。计划开启时最好让它常驻，
    // 否则黑幕窗口会在本进程退出时一起消失。
    let cfg = Config::load()?;
    if cfg.enabled {
        tracing::info!("计划已启用但常驻进程不在，拉一个 --action serve");
        let args = nd_win::sysutil::self_args(&nd_core::paths::home_dir(), &["--action", "serve"]);
        nd_win::sysutil::spawn_detached(&args)
            .map_err(|e| nd_core::error::user(format!("无法启动常驻进程：{e}")))?;
        return Ok(ExitCode::SUCCESS);
    }
    let platform = WinPlatform::new();
    let engine = Engine::new(&platform);
    let outcome = engine.reconcile(chrono::Local::now())?;
    platform.shutdown();
    Ok(report(&outcome))
}

pub fn test(args: &Args) -> Result<ExitCode> {
    let seconds = args.seconds.max(10);
    let mut detail = Vec::new();
    if let Some(mode) = args.mode.clone() {
        detail.push(("mode".to_string(), mode));
    }
    detail.push(("seconds".to_string(), seconds.to_string()));
    resident_or_local("test", &detail, |platform| {
        platform.warm()?;
        let engine = Engine::new(platform);
        let mode = match args.mode.as_deref() {
            Some(m) => Mode::parse(m)?,
            None => Config::load()?.mode,
        };
        engine.start_test(mode, seconds, chrono::Local::now())
    })
}

pub fn activate(args: &Args) -> Result<ExitCode> {
    let cfg = Config::load()?;
    let primary = cfg.primary_range();
    let start = args.start.clone().unwrap_or_else(|| primary.start.to_hm());
    let end = args.end.clone().unwrap_or_else(|| primary.end.to_hm());
    let mode = match args.mode.as_deref() {
        Some(m) => Mode::parse(m)?,
        None => cfg.mode,
    };
    let platform = WinPlatform::new();
    let engine = Engine::new(&platform);
    let outcome = engine.activate(mode, &start, &end, chrono::Local::now())?;
    platform.shutdown();
    Ok(report(&outcome))
}

pub fn pause() -> Result<ExitCode> {
    // 常驻进程在跑时由它自己停（它会顺带把黑幕收掉）。
    if let Some(text) = ask_daemon("pause", None)? {
        println!("{text}");
        return Ok(ExitCode::SUCCESS);
    }
    let platform = WinPlatform::new();
    let engine = Engine::new(&platform);
    let outcome = engine.pause()?;
    platform.shutdown();
    Ok(report(&outcome))
}

pub fn emergency() -> Result<ExitCode> {
    // 隐藏的远程恢复：任何时候都要能用，所以不受黑屏锁定限制。
    if let Some(text) = ask_daemon("remote_emergency", None)? {
        println!("{text}");
        return Ok(ExitCode::SUCCESS);
    }
    let platform = WinPlatform::new();
    let engine = Engine::new(&platform);
    let outcome = engine.emergency()?;
    platform.shutdown();
    Ok(report(&outcome))
}

/// 走一遍「组合键解锁」的完整逻辑：恢复白天 + 本段剩余时间保持白天。
///
/// 为什么要有这个动作：组合键本身是低级键盘钩子里检测的，钩子只能被**真实**
/// 按键推进（注入事件故意被忽略，免得远程注入伪造解锁），所以在无人值守的
/// CI 里没法真的按住 C+K。这个动作走的是钩子触发后**完全相同**的那条路径
/// （`Engine::chord_unlock`），所以解锁行为本身是可以被验证的，
/// 不能验证的只有"真按下去能不能被钩子认出来"那一步。
pub fn unlock() -> Result<ExitCode> {
    if let Some(text) = ask_daemon("unlock", None)? {
        println!("{text}");
        return Ok(ExitCode::SUCCESS);
    }
    let platform = WinPlatform::new();
    let engine = Engine::new(&platform);
    let outcome = engine.chord_unlock(chrono::Local::now())?;
    platform.shutdown();
    Ok(report(&outcome))
}

pub fn uninstall() -> Result<ExitCode> {
    if let Some(text) = ask_daemon("uninstall", None)? {
        println!("{text}");
        return Ok(ExitCode::SUCCESS);
    }
    let platform = WinPlatform::new();
    let engine = Engine::new(&platform);
    let outcome = engine.uninstall()?;
    platform.shutdown();
    Ok(report(&outcome))
}

pub fn quit() -> Result<ExitCode> {
    if let Some(text) = ask_daemon("quit", None)? {
        println!("{text}");
        Ok(ExitCode::SUCCESS)
    } else {
        println!("常驻进程没有在运行。");
        Ok(ExitCode::SUCCESS)
    }
}

// ----------------------------------------------------------------------
// 驱动与守护进程
// ----------------------------------------------------------------------

pub fn install_vdd() -> ExitCode {
    // 装驱动必须管理员：转给提升助手，一次 UAC。
    let result = helper::call(&helper::Command::InstallVdd);
    print_helper_result(result, "安装虚拟显示驱动失败", Some(vdd::manual_steps()))
}

pub fn uninstall_vdd() -> ExitCode {
    let result = helper::call(&helper::Command::UninstallVdd);
    print_helper_result(result, "卸载虚拟显示驱动失败", None)
}

/// 统一打印提升助手的结果：成功就原样输出助手的话，失败就带上
/// 能手动照做的下一步（驱动相关会附 `pnputil` 命令）。
fn print_helper_result(
    result: NdResult<String>,
    failure: &str,
    manual: Option<String>,
) -> ExitCode {
    match result {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            println!("{}", nd_win::error::user_text(&e, failure));
            if let Some(manual) = manual {
                println!("{manual}");
            }
            ExitCode::from(1)
        }
    }
}

pub fn helper() -> Result<ExitCode> {
    helper::serve()?;
    Ok(ExitCode::SUCCESS)
}

pub fn watchdog(args: &Args) -> Result<ExitCode> {
    // 登录任务拉起的那种没有父进程：只在发现"残留的夜间状态"时动作。
    let parent = if args.autostart || args.parent == 0 {
        0
    } else {
        args.parent
    };
    watchdog::run(&watchdog::WatchdogOptions {
        parent,
        restart_daemon: true,
    })?;
    Ok(ExitCode::SUCCESS)
}

/// 亮度快照（排障用）：打印每块屏当前记录的原始背光值。
#[allow(dead_code)]
pub fn brightness_snapshot() -> Result<ExitCode> {
    let control = brightness::Control::open()?;
    for (desc, value) in control.snapshot() {
        println!("{desc}: {value}");
    }
    Ok(ExitCode::SUCCESS)
}
