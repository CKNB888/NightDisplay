//! 唯一的提升助手。
//!
//! 设计约束（这次重写的硬要求）：
//! * **GUI/CLI 自己永远不提权**。所有需要管理员的操作都通过这里；
//! * 整个安装过程只弹**一次** UAC：助手常驻，之后夜间操作不再询问；
//! * 通道面极窄 —— 只有下面 `Command` 里列的几件事，参数全部校验，
//!   不认识的一律拒绝并记日志；
//! * 管道只允许本机、只允许本用户的进程连接，还要对上令牌和进程镜像路径。
//!
//! 需要特权的操作实际上只有三类：装/卸驱动、登记/删除计划任务、登记看门狗。
//! 别的（黑幕、gamma、背光、键鼠钩子）都不需要管理员，所以不进这条通道。

#![cfg(windows)]

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::pipe;
use crate::sysutil;
use crate::tasks;
use crate::vdd;

/// 助手能做的事。**这就是全部权限面**。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// 探活。返回版本号，用来判断"助手是否已经在跑"。
    Ping,
    /// 安装随包内置的虚拟显示驱动。
    InstallVdd,
    /// 卸载虚拟显示驱动。
    UninstallVdd,
    /// 按给定的时间点登记每日计划任务。
    ///
    /// 时间点由调用方传字符串，助手自己解析校验 —— 不接受任何可执行路径。
    InstallTasks {
        /// `["20:00", "10:30"]` 这种时间边界。
        boundaries: Vec<String>,
    },
    /// 删除本程序登记的全部计划任务。
    RemoveTasks,
    /// 登记看门狗任务（登录时拉起）。
    InstallWatchdog,
    /// 助手退出。
    Quit,
}

impl Command {
    /// 从 IPC 的 `action` 字符串构造。只认白名单。
    pub fn parse(action: &str, detail: Option<&nd_core::ipc::CommandDetail>) -> Result<Self> {
        match action.trim() {
            "ping" => Ok(Command::Ping),
            "install-vdd" => Ok(Command::InstallVdd),
            "uninstall-vdd" => Ok(Command::UninstallVdd),
            "install-tasks" => {
                let raw = detail
                    .and_then(|d| d.test_id.as_deref())
                    .unwrap_or("")
                    .to_string();
                let boundaries: Vec<String> = raw
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if boundaries.is_empty() {
                    return Err(nd_core::error::user(
                        "登记计划任务时没有给出时间点（格式 20:00,10:30）",
                    ));
                }
                for b in &boundaries {
                    b.parse::<nd_core::schedule::MinuteOfDay>().map_err(|_| {
                        nd_core::error::user(format!("时间点“{b}”格式不对，应为 HH:MM"))
                    })?;
                }
                Ok(Command::InstallTasks { boundaries })
            }
            "remove-tasks" => Ok(Command::RemoveTasks),
            "install-watchdog" => Ok(Command::InstallWatchdog),
            "quit" => Ok(Command::Quit),
            other => Err(nd_core::error::user(format!("提升助手不接受命令“{other}”"))),
        }
    }

    /// 记日志用的名字。
    pub fn label(&self) -> &'static str {
        match self {
            Command::Ping => "ping",
            Command::InstallVdd => "install-vdd",
            Command::UninstallVdd => "uninstall-vdd",
            Command::InstallTasks { .. } => "install-tasks",
            Command::RemoveTasks => "remove-tasks",
            Command::InstallWatchdog => "install-watchdog",
            Command::Quit => "quit",
        }
    }
}

/// 助手通道的令牌文件。
fn token_file() -> PathBuf {
    nd_core::paths::home_dir().join("helper-token.txt")
}

/// 读令牌；没有就生成一个并落盘。
fn load_token() -> String {
    if let Ok(text) = std::fs::read_to_string(token_file()) {
        let trimmed = text.trim().to_string();
        if trimmed.len() == 64 {
            return trimmed;
        }
    }
    let token = nd_core::ipc::new_token();
    if let Some(dir) = token_file().parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(token_file(), &token);
    token
}

/// 助手进程是否在跑（一次 ping）。
pub fn is_running() -> bool {
    ping().is_ok()
}

/// 探活。
pub fn ping() -> Result<String> {
    let request = nd_core::ipc::Request {
        token: load_token(),
        command: nd_core::ipc::Command::Bare("ping".into()),
    };
    let resp = pipe::call(&pipe::helper_pipe_name(), &request, 400)?;
    if resp.ok {
        Ok(resp.result)
    } else {
        Err(nd_core::error::user(resp.result))
    }
}

/// 通过助手执行一条命令。助手没在跑时先弹一次 UAC 拉起它。
pub fn call(command: Command) -> Result<String> {
    let request = nd_core::ipc::Request {
        token: load_token(),
        command: nd_core::ipc::Command::Detailed(nd_core::ipc::CommandDetail {
            action: command.label().to_string(),
            mode: None,
            seconds: None,
            test_id: task_argument(&command),
        }),
    };
    // 先试一次，助手没起来再提升。
    match pipe::call(&pipe::helper_pipe_name(), &request, 500) {
        Ok(resp) if resp.ok => return Ok(resp.result),
        Ok(resp) => return Err(nd_core::error::user(resp.result)),
        Err(_) => {}
    }
    ensure_running()?;
    let resp = pipe::call(&pipe::helper_pipe_name(), &request, 15_000)?;
    if resp.ok {
        Ok(resp.result)
    } else {
        Err(nd_core::error::user(resp.result))
    }
}

/// 把命令里需要的参数压成一个字符串（只有时间点用得到）。
fn task_argument(command: &Command) -> Option<String> {
    match command {
        Command::InstallTasks { boundaries } => Some(boundaries.join(",")),
        _ => None,
    }
}

/// 确保助手在跑：不在就弹一次 UAC 启动它，并等它就绪。
pub fn ensure_running() -> Result<()> {
    if is_running() {
        return Ok(());
    }
    // 已经是管理员就直接起（不弹窗）。
    let exe = sysutil::exe_path();
    let args = sysutil::self_args(&nd_core::paths::home_dir(), &["--action", "helper"]);
    if crate::power::is_elevated() {
        sysutil::spawn_detached(&args)
            .map_err(|e| nd_core::error::user(format!("无法启动提升助手：{e}")))?;
    } else {
        elevate_and_run(&exe, &args)?;
    }
    // 等它就绪（最长 30 秒，用户可能正在看 UAC 弹窗）。
    for _ in 0..150 {
        std::thread::sleep(Duration::from_millis(200));
        if is_running() {
            tracing::info!("提升助手已就绪");
            return Ok(());
        }
    }
    Err(nd_core::error::user(
        "提升助手没有启动起来。可能是在 UAC 弹窗里点了“否”。\
         可以再点一次“重新授权”，或者右键 NightDisplay.exe 选“以管理员身份运行”。",
    ))
}

/// 用 `runas` 谓词启动（会弹 UAC）。
fn elevate_and_run(exe: &Path, args: &[String]) -> Result<()> {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let file: Vec<u16> = OsStr::new("runas")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let target: Vec<u16> = exe
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let params: Vec<u16> = sysutil::command_line(Path::new(""), args)
        .trim()
        .trim_start_matches('"')
        .trim_end_matches('"')
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: 四个字符串都以 NUL 结尾，句柄为空表示没有父窗口。
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(file.as_ptr()),
            PCWSTR(target.as_ptr()),
            PCWSTR(params.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW 返回值 <= 32 表示失败。
    if result.0 as isize <= 32 {
        return Err(nd_core::error::user(
            "无法弹出管理员授权窗口（可能被安全软件拦下了）。\
             请右键 NightDisplay.exe 选择“以管理员身份运行”，再点一次“重新授权”。",
        ));
    }
    Ok(())
}

/// 助手主循环（`--action helper`）。**必须以管理员身份运行**，否则直接退出。
pub fn serve() -> Result<()> {
    if !crate::power::is_elevated() {
        return Err(nd_core::error::user(
            "提升助手需要管理员权限，请通过安装向导的“授权”步骤启动。",
        ));
    }
    let token = load_token();
    let home = nd_core::paths::home_dir();
    tracing::info!(pipe = %pipe::helper_pipe_name(), "提升助手开始服务");

    let result = pipe::serve(&pipe::helper_pipe_name(), |request| {
        // 令牌不对：不回细节，直接拒绝。
        if !nd_core::ipc::token_matches(&token, &request.token) {
            tracing::warn!(action = %request.command.action(), "助手收到令牌错误的请求，已拒绝");
            return (
                nd_core::ipc::Response::error("提升助手拒绝：令牌不匹配"),
                false,
            );
        }
        let detail = match &request.command {
            nd_core::ipc::Command::Detailed(d) => Some(d.clone()),
            _ => None,
        };
        let command = match Command::parse(request.command.action(), detail.as_ref()) {
            Ok(c) => c,
            Err(e) => return (nd_core::ipc::Response::error(e.to_string()), false),
        };
        let quit = command == Command::Quit;
        tracing::info!(command = command.label(), "助手执行命令");
        let response = match execute(&command, &home) {
            Ok(text) => nd_core::ipc::Response::value(text),
            Err(e) => {
                let text = crate::error::user_text(&e, "提升操作失败");
                tracing::error!(command = command.label(), error = %crate::error::chain_text(&e), "助手执行失败");
                nd_core::ipc::Response::error(text)
            }
        };
        (response, quit)
    });
    tracing::info!("提升助手退出");
    result
}

/// 执行一条已校验的命令。
fn execute(command: &Command, home: &Path) -> Result<String> {
    match command {
        Command::Ping => Ok(format!("NightDisplay 提升助手 {}", nd_core::VERSION)),
        Command::InstallVdd => vdd::install(),
        Command::UninstallVdd => vdd::uninstall(),
        Command::InstallTasks { boundaries } => {
            // 只按传入的时间点登记任务，不读用户配置 —— 助手不该依赖调用方的文件。
            let names = tasks::install_boundaries(boundaries, home)?;
            Ok(format!(
                "已登记 {} 个计划任务：{}",
                names.len(),
                names.join("、")
            ))
        }
        Command::RemoveTasks => {
            let removed = tasks::remove_all()?;
            Ok(format!("已删除 {removed} 个计划任务"))
        }
        Command::InstallWatchdog => {
            tasks::install_watchdog(home)?;
            Ok("看门狗任务已登记".into())
        }
        Command::Quit => Ok("助手已退出".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nd_core::ipc::CommandDetail;

    fn detail(test_id: Option<&str>) -> CommandDetail {
        CommandDetail {
            action: String::new(),
            mode: None,
            seconds: None,
            test_id: test_id.map(|s| s.to_string()),
        }
    }

    #[test]
    fn rejects_unknown_commands() {
        assert!(Command::parse("rm -rf /", None).is_err());
        assert!(Command::parse("run", None).is_err());
        assert!(Command::parse("", None).is_err());
    }

    #[test]
    fn accepts_whitelisted_commands() {
        assert_eq!(Command::parse("ping", None).unwrap(), Command::Ping);
        assert_eq!(
            Command::parse("install-vdd", None).unwrap(),
            Command::InstallVdd
        );
        assert_eq!(
            Command::parse("remove-tasks", None).unwrap(),
            Command::RemoveTasks
        );
        assert_eq!(
            Command::parse("install-watchdog", None).unwrap(),
            Command::InstallWatchdog
        );
    }

    #[test]
    fn install_tasks_validates_time_format() {
        let d = detail(Some("20:00,10:30"));
        assert_eq!(
            Command::parse("install-tasks", Some(&d)).unwrap(),
            Command::InstallTasks {
                boundaries: vec!["20:00".into(), "10:30".into()]
            }
        );
        let bad = detail(Some("25:00"));
        assert!(Command::parse("install-tasks", Some(&bad)).is_err());
        let empty = detail(None);
        assert!(Command::parse("install-tasks", Some(&empty)).is_err());
    }

    #[test]
    fn token_file_has_stable_length() {
        crate::test_support::with_temp_home(|_| {
            let a = load_token();
            let b = load_token();
            assert_eq!(a.len(), 64);
            assert_eq!(a, b);
        });
    }
}
