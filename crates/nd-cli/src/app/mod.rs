//! 命令行实现的入口与分发。
//!
//! 分层约定（第二阶段接 Tauri 时保持不变）：
//! * `mod.rs` —— 参数解析、日志、退出码、把动作分派到下面两个模块；
//! * `actions.rs` —— 一次性动作（status / night / day / test / setup …）；
//! * `serve.rs` —— 常驻进程：本地 IPC + 定时对齐 + 组合键解锁。
//!
//! 这个 crate 永远**不以管理员身份**执行特权动作：装驱动、登记计划任务
//! 全部转给 `nd-win::helper` 的提升助手。

use std::process::ExitCode;

use anyhow::Result;
use nd_core::error::one_line;
use nd_core::{logging, paths};

pub mod actions;
pub mod serve;

/// 解析后的命令行参数。旧版的参数名全部保留，新增的排在后面。
#[derive(Debug, Clone)]
pub struct Args {
    pub action: String,
    /// 开机自启：只显示托盘（这一阶段没有界面，等价于静默对齐一次）。
    pub tray: bool,
    pub home: Option<String>,
    pub mode: Option<String>,
    pub start: Option<String>,
    pub end: Option<String>,
    /// 延迟多少秒再执行（测试的自动恢复靠它）。
    pub delay: i64,
    /// 测试时长 / 防睡眠时长。
    pub seconds: u64,
    /// 这是测试的恢复阶段，允许在"黑屏锁定"时执行。
    pub test_recovery: bool,
    pub test_id: String,
    /// 出错时在终端打印完整错误链。
    pub debug: bool,
    /// 看门狗要监视的父进程 PID。
    pub parent: u32,
    /// 向导：跳过虚拟显示驱动的安装（**只给自动化测试用**）。
    ///
    /// 4.0 起 VDD 是必需组件，正常使用时不提供跳过选项。
    pub no_vdd: bool,
    /// 向导：只跑这些步骤（修复模式）。
    pub only: Vec<String>,
    /// 界面自检：N 秒后自动退出（这一阶段没有界面，接受但忽略）。
    pub selftest: u64,
    /// 看门狗由登录任务拉起（不跟随父进程）。
    pub autostart: bool,
    /// 诊断包的导出目录（默认 `<数据目录>/diag`）。
    pub out: Option<String>,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            action: "gui".into(),
            tray: false,
            home: None,
            mode: None,
            start: None,
            end: None,
            delay: 0,
            seconds: 120,
            test_recovery: false,
            test_id: String::new(),
            debug: false,
            parent: 0,
            no_vdd: false,
            only: Vec::new(),
            selftest: 0,
            autostart: false,
            out: None,
        }
    }
}

const ACTIONS: &[&str] = &[
    "gui",
    "tray",
    "night",
    "day",
    "unlock",
    "reconcile",
    "test",
    "activate",
    "pause",
    "emergency",
    "uninstall",
    "status",
    "awake",
    "backup",
    "restore",
    "displays",
    "plan-virtual",
    "install-vdd",
    "uninstall-vdd",
    "selfcheck",
    "probe",
    "probe-reset",
    "export-diag",
    "diag",
    "env",
    "setup",
    "repair",
    "onboarding",
    "update",
    "serve",
    "watchdog",
    "helper",
    "quit",
];

const HELP: &str = r"NightDisplay —— 夜间本地黑屏、远程正常

用法：
  NightDisplay --action <动作> [选项]

动作（--action）：
  setup          首次运行向导：一次授权，之后夜间不再弹管理员确认
  repair         重新授权 / 修复：只重跑上次失败的步骤
  onboarding     查看向导状态（每一步的真实结果）
  gui, tray      打开图形界面（4.1 起提供；现在请用 setup）
  serve          常驻进程（托盘自启、计划任务都调它）
  night          立刻进入夜间（按计划里的模式）
  day            立刻恢复白天（黑屏锁定期间会被拒绝，请用 emergency 或组合键）
  unlock         走一遍组合键解锁的完整逻辑（恢复白天并保持本段白天）
  test           测试 N 秒后自动恢复（先暂停每日计划）
  activate       启用每日计划（必须先成功测试过一次）
  pause          暂停每日计划（不改变当前屏幕）
  emergency      应急恢复：立刻亮屏并暂停计划，可远程执行
  uninstall      卸载：恢复 + 删除计划任务和自启项
  status         打印当前状态
  selfcheck      打印自检结果（人读摘要 + 完整 JSON），并写 selfcheck.json
  export-diag    导出诊断包（日志 / 脱敏配置 / 自检 / 显示能力 / 最近会话）
  env            打印本机能力（排障用）
  displays       列出显示器
  plan-virtual   只校验“仅虚拟屏”，不改动
  backup         备份当前显示配置
  restore        还原显示配置
  install-vdd    安装内置虚拟显示驱动并验证虚拟屏（会弹一次管理员确认）
  uninstall-vdd  卸载虚拟显示驱动
  awake          保持唤醒 N 秒
  update         检查新版本
  watchdog       看门狗（内部使用）
  helper         提升助手（内部使用，必须由向导授权启动）
  quit           结束常驻进程

选项：
  --mode <auto|guard|virtual_only|screen_off>
  --start HH:MM --end HH:MM     启用时的时间段
  --delay N                     延迟 N 秒执行
  --seconds N                   测试/防睡眠时长
  --test-id <id>                测试标识（内部使用）
  --test-recovery               测试的恢复阶段（内部使用）
  --parent <pid>                看门狗监视的进程
  --no-vdd                      仅测试用：跳过虚拟显示驱动安装（正常安装必须装）
  --only a,b                    setup/repair 只跑指定步骤
  --home <目录>                 指定数据目录
  --out <目录>                  诊断包导出到哪里（默认 <数据目录>/diag）
  --debug                       出错时打印完整错误链
  -h, --help                    显示这段说明
  -V, --version                 显示版本
";

/// 进程入口。所有错误在这里收敛成一句中文 + 退出码。
pub fn main_entry() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1).collect()) {
        Ok(Some(a)) => a,
        // 打印了帮助或版本，正常退出。
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("错误：{e}");
            return ExitCode::from(1);
        }
    };

    if let Some(home) = args.home.as_deref() {
        // 计划任务、提升助手、看门狗都靠这个环境变量找同一个数据目录。
        std::env::set_var("NIGHTDISPLAY_HOME", paths::absolute(home));
    }
    let _ = paths::ensure_home();
    let component = logging::Component::for_action(&args.action);
    if let Err(e) = logging::init(component) {
        eprintln!("提示：日志没能初始化（{e}），程序继续运行。");
    }
    if args.debug {
        // --debug 要把级别调高，而且不能要求重启：reload 句柄在 init 里已存好。
        logging::set_debug();
    }

    match run(&args) {
        Ok(code) => code,
        Err(e) => {
            let action = if args.action.is_empty() {
                "run"
            } else {
                args.action.as_str()
            };
            logging::record_error(action, &e);
            tracing::error!(action, error = %one_line(&e), "命令失败");
            if args.debug {
                eprintln!("{e:#}");
            }
            eprintln!(
                "错误：{}（详细信息见日志；加 --debug 显示完整错误链）",
                one_line(&e)
            );
            ExitCode::from(1)
        }
    }
}

fn run(args: &Args) -> Result<ExitCode> {
    if args.delay < 0 || args.seconds == 0 {
        return Err(nd_core::error::user("时长必须为正数，延迟不能为负数"));
    }
    // 模式先校验一次，参数写错时不要等到改显示才发现。
    if let Some(mode) = args.mode.as_deref() {
        nd_core::mode::Mode::parse(mode)?;
    }

    match args.action.as_str() {
        // 图形界面属于第二阶段；先如实告诉用户现在该怎么用。
        "gui" | "tray" => return actions::gui(args),
        _ => {}
    }

    if args.delay > 0 {
        tracing::info!(delay = args.delay, action = %args.action, "延迟执行");
        std::thread::sleep(std::time::Duration::from_secs(args.delay.cast_unsigned()));
    }

    match args.action.as_str() {
        "serve" => serve::serve(),
        "watchdog" => actions::watchdog(args),
        "helper" => actions::helper(),
        "status" => actions::status(),
        "selfcheck" => actions::selfcheck(),
        "probe" => actions::probe(),
        "probe-reset" => Ok(actions::probe_reset()),
        "export-diag" | "diag" => actions::export_diag(args.out.as_deref()),
        "env" => actions::env(),
        "displays" => actions::displays(),
        "plan-virtual" => actions::plan_virtual(),
        "backup" => actions::backup(),
        "restore" => actions::restore(),
        "awake" => actions::awake(args.seconds),
        "update" => Ok(actions::update()),
        "setup" => actions::setup(args),
        "repair" => actions::repair(args),
        "onboarding" => Ok(actions::onboarding_report()),
        "install-vdd" => Ok(actions::install_vdd()),
        "uninstall-vdd" => Ok(actions::uninstall_vdd()),
        "night" => actions::night(args),
        "day" => actions::day(args),
        "unlock" => actions::unlock(),
        "reconcile" => actions::reconcile(),
        "test" => actions::test(args),
        "activate" => actions::activate(args),
        "pause" => actions::pause(),
        "emergency" => actions::emergency(),
        "uninstall" => actions::uninstall(),
        "quit" => actions::quit(),
        other => Err(nd_core::error::user(format!(
            "不认识的 --action “{other}”。可用动作见 --help。"
        ))),
    }
}

/// 解析参数。返回 `Ok(None)` 表示已经把帮助或版本打印出去了。
fn parse_args(argv: Vec<String>) -> Result<Option<Args>> {
    let mut parsed = Args::default();
    let mut it = argv.into_iter();
    while let Some(raw) = it.next() {
        let (flag, inline) = match raw.split_once('=') {
            Some((key, value)) => (key.to_string(), Some(value.to_string())),
            None => (raw.clone(), None),
        };
        let mut value = |name: &str| -> Result<String> {
            match inline.clone() {
                Some(v) => Ok(v),
                None => it
                    .next()
                    .ok_or_else(|| nd_core::error::user(format!("{name} 后面缺少参数值"))),
            }
        };
        match flag.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("NightDisplay {}", nd_core::VERSION);
                return Ok(None);
            }
            "--action" => parsed.action = value("--action")?,
            "--home" => parsed.home = Some(value("--home")?),
            "--out" => parsed.out = Some(value("--out")?),
            "--mode" => parsed.mode = Some(value("--mode")?),
            "--start" => parsed.start = Some(value("--start")?),
            "--end" => parsed.end = Some(value("--end")?),
            "--test-id" => parsed.test_id = value("--test-id")?,
            "--only" => {
                parsed.only = value("--only")?
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            "--delay" => parsed.delay = parse_num(&value("--delay")?, "--delay")?,
            "--seconds" => parsed.seconds = parse_num(&value("--seconds")?, "--seconds")?,
            "--selftest" => parsed.selftest = parse_num(&value("--selftest")?, "--selftest")?,
            "--parent" => parsed.parent = parse_num(&value("--parent")?, "--parent")?,
            "--tray" => parsed.tray = true,
            "--debug" => parsed.debug = true,
            "--no-vdd" => parsed.no_vdd = true,
            "--test-recovery" => parsed.test_recovery = true,
            "--autostart" => parsed.autostart = true,
            other => {
                return Err(nd_core::error::user(format!(
                    "不认识的参数 “{other}”。运行 NightDisplay --help 看用法。"
                )))
            }
        }
    }
    if parsed.action.trim().is_empty() {
        parsed.action = "gui".into();
    }
    if !ACTIONS.contains(&parsed.action.as_str()) {
        return Err(nd_core::error::user(format!(
            "不认识的 --action “{}”。可用动作见 --help。",
            parsed.action
        )));
    }
    Ok(Some(parsed))
}

fn parse_num<T: std::str::FromStr>(text: &str, name: &str) -> Result<T> {
    text.trim()
        .parse::<T>()
        .map_err(|_| nd_core::error::user(format!("{name} 需要一个整数，收到“{text}”")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_action_and_options() {
        let args = parse_args(
            ["--action", "test", "--seconds", "15", "--mode", "guard"]
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(args.action, "test");
        assert_eq!(args.seconds, 15);
        assert_eq!(args.mode.as_deref(), Some("guard"));
    }

    #[test]
    fn accepts_equals_form() {
        let args = parse_args(vec!["--action=status".to_string()])
            .unwrap()
            .unwrap();
        assert_eq!(args.action, "status");
    }

    #[test]
    fn default_action_is_gui() {
        let args = parse_args(vec![]).unwrap().unwrap();
        assert_eq!(args.action, "gui");
    }

    #[test]
    fn rejects_unknown_action_and_flag() {
        assert!(parse_args(vec!["--action".into(), "frobnicate".into()]).is_err());
        assert!(parse_args(vec!["--nope".into()]).is_err());
    }

    #[test]
    fn help_returns_none() {
        assert!(parse_args(vec!["--help".into()]).unwrap().is_none());
    }

    #[test]
    fn only_splits_on_comma() {
        let args = parse_args(vec!["--action=repair".into(), "--only=a,b".into()])
            .unwrap()
            .unwrap();
        assert_eq!(args.only, vec!["a", "b"]);
    }
}
