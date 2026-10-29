//! 进程相关的小工具：当前 exe 路径、隐藏窗口跑命令、脱离父进程起子进程。
//!
//! 这些函数在 GUI 阶段也会用到，所以放在平台层而不是 CLI 里。

#![cfg(windows)]

use std::ffi::OsStr;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use windows::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, DETACHED_PROCESS,
};

/// `CreateProcess` 里表示"不要给我分配控制台窗口"。
const NO_WINDOW: u32 = CREATE_NO_WINDOW.0;

/// 当前可执行文件路径。
#[must_use]
pub fn current_exe() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("NightDisplay.exe"))
}

/// 本程序自己的 exe 路径（GUI 里叫 NightDisplay.exe，CLI 也叫 NightDisplay.exe）。
#[must_use]
pub fn exe_path() -> PathBuf {
    current_exe()
}

/// 组装 `程序 --action xxx --home <dir>` 这种命令行。
#[must_use]
pub fn self_args(home: &std::path::Path, extra: &[&str]) -> Vec<String> {
    let mut args = vec!["--home".to_string(), home.display().to_string()];
    args.extend(extra.iter().map(std::string::ToString::to_string));
    args
}

/// 用 Windows 的 `CommandLineToArgvW` 规则做一次引号化。
#[must_use]
pub fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            c => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

/// 把 exe + 参数拼成一条命令行字符串（写注册表 Run、计划任务时用）。
#[must_use]
pub fn command_line(exe: &std::path::Path, args: &[String]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(quote(&exe.display().to_string()));
    parts.extend(args.iter().map(|a| quote(a)));
    parts.join(" ")
}

/// 在资源管理器里定位一个文件（选中它）或打开一个文件夹。
///
/// `explorer.exe` 即使成功也会返回 1，所以不看它的退出码，只看能不能起得来。
pub fn open_in_explorer(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        tracing::warn!(path = %path.display(), "要打开的位置不存在");
        return Ok(());
    }
    let arg = if path.is_dir() {
        path.display().to_string()
    } else {
        format!("/select,{}", path.display())
    };
    run_hidden(OsStr::new("explorer"), &[arg.as_str()], "资源管理器").map(|_| ())
}

/// 用系统默认浏览器打开一个 http(s) 链接。
///
/// 只接受 `http://` / `https://`：这里的 URL 会原样交给 shell，
/// 别的协议（`file:`、`javascript:`、自定义协议）一律拒绝。
pub fn open_url(url: &str) -> std::io::Result<()> {
    let trimmed = url.trim();
    if !(trimmed.starts_with("https://") || trimmed.starts_with("http://")) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "只支持 http/https 链接",
        ));
    }
    run_hidden(
        OsStr::new("rundll32"),
        &["url.dll,FileProtocolHandler", trimmed],
        "浏览器",
    )
    .map(|_| ())
}

/// 跑一个命令并等它结束，不弹控制台窗口。stdout/stderr 一并收回来。
pub fn run_hidden(program: &OsStr, args: &[&str], timeout_hint: &str) -> std::io::Result<Output> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .creation_flags(NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd
        .spawn()
        .map_err(|e| std::io::Error::new(e.kind(), format!("无法启动 {timeout_hint}：{e}")))?;
    child.wait_with_output()
}

/// 跑 PowerShell 脚本（脚本走 `-Command`，不长驻）。
pub fn run_powershell(script: &str, env: &[(&str, &str)]) -> std::io::Result<Output> {
    let mut cmd = Command::new("powershell");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        script,
    ])
    .creation_flags(NO_WINDOW)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.spawn()
        .and_then(std::process::Child::wait_with_output)
        .map_err(|e| std::io::Error::new(e.kind(), format!("无法启动 PowerShell：{e}")))
}

/// 起一个脱离父进程的副本：父进程退出后它继续活着。
///
/// 看门狗、延时恢复（测试）都用这个。返回子进程 PID。
pub fn spawn_detached(args: &[String]) -> std::io::Result<Child> {
    let exe = exe_path();
    Command::new(&exe)
        .args(args)
        .creation_flags(DETACHED_PROCESS.0 | CREATE_NEW_PROCESS_GROUP.0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| std::io::Error::new(e.kind(), format!("无法启动 {}：{e}", exe.display())))
}

/// 用 `-File` 跑一个 ps1 脚本，附带额外参数。
pub fn run_powershell_script(script: &std::path::Path, args: &[&str]) -> std::io::Result<Output> {
    let mut cmd = Command::new("powershell");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
    ])
    .arg(script)
    .args(args)
    .creation_flags(NO_WINDOW)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    cmd.spawn()
        .and_then(std::process::Child::wait_with_output)
        .map_err(|e| std::io::Error::new(e.kind(), format!("无法启动 PowerShell：{e}")))
}

/// 静默参数（不需要控制台窗口）。
#[must_use]
pub fn no_window_flag() -> u32 {
    NO_WINDOW
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_only_when_needed() {
        assert_eq!(quote("abc"), "abc");
        assert_eq!(quote("a b"), "\"a b\"");
        assert_eq!(quote(""), "\"\"");
    }

    #[test]
    fn quotes_backslashes_before_quote() {
        // 结尾的反斜杠要成对，否则会把后面的引号转义掉。
        assert_eq!(quote("a b\\"), "\"a b\\\\\"");
        assert_eq!(quote("say \"hi\""), "\"say \\\"hi\\\"\"");
    }

    #[test]
    fn command_line_includes_exe_and_args() {
        let line = command_line(
            std::path::Path::new(r"C:\Program Files\NightDisplay\NightDisplay.exe"),
            &["--tray".to_string()],
        );
        assert!(line.starts_with(r#""C:\Program Files\NightDisplay\NightDisplay.exe""#));
        assert!(line.ends_with("--tray"));
    }

    #[test]
    fn self_args_starts_with_home() {
        let args = self_args(
            std::path::Path::new(r"C:\Users\a\AppData\Local\NightDisplay"),
            &["--tray"],
        );
        assert_eq!(args[0], "--home");
        assert_eq!(args[2], "--tray");
    }
}
