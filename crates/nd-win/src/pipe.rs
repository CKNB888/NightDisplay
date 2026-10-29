//! Windows 命名管道：`serve` 与 CLI/GUI 之间的传输层，以及提升助手的私有通道。
//!
//! 协议本身在 `nd_core::ipc`（纯逻辑、可测），这里只负责搬运：
//! * 管道名带当前用户的标识，不同用户各用各的；
//! * `PIPE_REJECT_REMOTE_CLIENTS`，远程机器连不上；
//! * 一问一答，超长报文由 `read_line_limited` 拒掉；
//! * 令牌校验在调用方做（协议层），传输层不判断权限。

#![cfg(windows)]

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use anyhow::Result;
use nd_core::ipc::{decode_response, encode_request, read_line_limited, Request, Response};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
    FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, WaitNamedPipeW, NAMED_PIPE_MODE,
    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
    PIPE_WAIT,
};

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// 当前用户的稳定短标识，避免两个用户抢同一条管道。
#[must_use]
pub fn user_tag() -> String {
    let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".into());
    let domain = std::env::var("USERDOMAIN").unwrap_or_default();
    // 用户名可能带非 ASCII，转成十六进制，避免管道名里出现奇怪字符。
    format!("{domain}\\{user}")
        .bytes()
        .fold(String::with_capacity(64), |mut acc, b| {
            use std::fmt::Write as _;
            // Writing into a String cannot fail.
            let _ = write!(acc, "{b:02x}");
            acc
        })
}

/// 主进程（serve）的管道全名。
#[must_use]
pub fn serve_pipe_name() -> String {
    format!(r"\\.\pipe\NightDisplay-serve-{}", user_tag())
}

/// 提升助手的管道全名。
#[must_use]
pub fn helper_pipe_name() -> String {
    format!(r"\\.\pipe\NightDisplay-helper-{}", user_tag())
}

/// 把 HANDLE 包一层，Drop 时关掉。
struct Handle(HANDLE);

// SAFETY: HANDLE 只是内核对象句柄，跨线程传递（不并发使用）是安全的。
unsafe impl Send for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: 句柄由本模块创建，只关一次。
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// 建一条管道实例。
fn create_instance(name: &str) -> Result<Handle> {
    let name_w = wide(name);
    let mode = NAMED_PIPE_MODE(
        PIPE_TYPE_BYTE.0 | PIPE_READMODE_BYTE.0 | PIPE_WAIT.0 | PIPE_REJECT_REMOTE_CLIENTS.0,
    );
    // SAFETY: name_w 以 NUL 结尾；其余参数是标准值。
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(name_w.as_ptr()),
            PIPE_ACCESS_DUPLEX,
            mode,
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            0,
            None,
        )
    };
    if handle.is_invalid() {
        return Err(nd_core::error::user(format!(
            "无法创建本地通信管道（{}）",
            std::io::Error::last_os_error()
        )));
    }
    Ok(Handle(handle))
}

/// 阻塞式服务循环：每条连接读一行请求、交给 `handler`、回一行。
///
/// `handler` 返回 `None` 表示要退出循环（例如收到 `quit`）。
pub fn serve<F>(name: &str, mut handler: F) -> Result<()>
where
    F: FnMut(Request) -> (Response, bool),
{
    tracing::info!(pipe = name, "本地通信管道已就绪");
    loop {
        let pipe = create_instance(name)?;
        // SAFETY: 同步连接，等待客户端。
        let connected = unsafe { ConnectNamedPipe(pipe.0, None) };
        if connected.is_err() {
            // 客户端在连接建立前就退出了，或者已经在等；继续下一轮。
            let err = windows::core::Error::from_thread();
            if err.code().0 as u32 != 535 {
                // ERROR_PIPE_CONNECTED = 535：已经连上，属于正常情况。
                tracing::debug!(error = %err, "管道连接未建立");
                continue;
            }
        }
        let outcome = handle_connection(pipe.0, &mut handler);
        // SAFETY: 断开并让 Handle 的 Drop 关闭实例。
        unsafe {
            let _ = DisconnectNamedPipe(pipe.0);
        }
        match outcome {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(e) => tracing::warn!(error = %e, "处理一条管道请求失败"),
        }
    }
}

/// 处理一条连接。返回 true 表示要求退出。
fn handle_connection(
    handle: HANDLE,
    handler: &mut impl FnMut(Request) -> (Response, bool),
) -> Result<bool> {
    let mut reader = std::io::BufReader::new(HandleReader(handle));
    let Some(line) = read_line_limited(&mut reader)? else {
        return Ok(false);
    };
    let request = match nd_core::ipc::decode_request(&line) {
        Ok(r) => r,
        Err(e) => {
            // 解不出请求也要回一句，客户端才不会卡在等回复。
            let resp = Response::error(e.to_string());
            write_all(handle, &nd_core::ipc::encode_response(&resp))?;
            return Ok(false);
        }
    };
    let (response, stop) = handler(request);
    write_all(handle, &nd_core::ipc::encode_response(&response))?;
    Ok(stop)
}

/// 用 `ReadFile` 包一个 `Read`，让协议层的 `read_line_limited` 能用。
struct HandleReader(HANDLE);

impl std::io::Read for HandleReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut read = 0u32;
        // SAFETY: buf 长度与传入的缓冲区一致。
        let ok = unsafe { ReadFile(self.0, Some(buf), Some(&raw mut read), None) };
        match ok {
            Ok(()) => Ok(read as usize),
            Err(e) => Err(std::io::Error::other(e.to_string())),
        }
    }
}

fn write_all(handle: HANDLE, text: &str) -> Result<()> {
    let bytes = text.as_bytes();
    let mut written = 0u32;
    // SAFETY: bytes 长度与缓冲区一致；管道是小报文，一次写完。
    let ok = unsafe { WriteFile(handle, Some(bytes), Some(&raw mut written), None) };
    ok.map_err(|e| nd_core::error::user(format!("写入本地通信管道失败：{e}")))
}

/// 打开管道并发送一条请求，等回复。
///
/// `timeout_ms` 是等待管道就绪的时间；常驻进程没起来时直接给出中文提示。
pub fn call(name: &str, request: &Request, timeout_ms: u32) -> Result<Response> {
    let payload = encode_request(request)?;
    let name_w = wide(name);
    // 先等实例可用，避免连到"管道不存在"。
    // SAFETY: name_w 以 NUL 结尾。
    let available = unsafe { WaitNamedPipeW(PCWSTR(name_w.as_ptr()), timeout_ms) };
    if !available.as_bool() {
        return Err(nd_core::error::user(
            "NightDisplay 常驻进程没有运行（本地通信管道不可用），请先运行一次 NightDisplay --action serve",
        ));
    }
    // SAFETY: 打开已存在的管道实例。
    let handle = unsafe {
        CreateFileW(
            PCWSTR(name_w.as_ptr()),
            (FILE_GENERIC_READ | FILE_GENERIC_WRITE).0,
            FILE_SHARE_NONE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(|e| nd_core::error::user(format!("无法连接本地通信管道：{e}")))?;
    let handle = Handle(handle);
    write_all(handle.0, &payload)?;
    let mut reader = std::io::BufReader::new(HandleReader(handle.0));
    let Some(line) = read_line_limited(&mut reader)? else {
        return Err(nd_core::error::user("常驻进程没有回复就断开了连接"));
    };
    decode_response(&line)
}

// ----------------------------------------------------------------------
// 端点持久化：常驻进程把令牌写进 `ipc.json`，CLI/GUI 读它来连。
// ----------------------------------------------------------------------

/// 把当前端点的令牌落盘（只有本用户可读的数据目录）。
pub fn save_endpoint(token: &str) -> Result<()> {
    let path = nd_core::paths::ipc_file();
    let body = serde_json::json!({
        "pipe": serve_pipe_name(),
        "token": token,
        "pid": std::process::id(),
    });
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(&body)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// 读端点。没有或者内容不对（多半是上次异常退出留下的）返回 `None`。
pub fn load_endpoint() -> Option<nd_core::ipc::Endpoint> {
    let text = std::fs::read_to_string(nd_core::paths::ipc_file()).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let token = value.get("token")?.as_str()?.to_string();
    if token.len() != 64 {
        return None;
    }
    let pipe = value
        .get("pipe")
        .and_then(|v| v.as_str())
        .map_or_else(serve_pipe_name, std::string::ToString::to_string);
    Some(nd_core::ipc::Endpoint { token, pipe })
}

/// 退出时删掉端点文件，免得下次 CLI 连到不存在的进程。
pub fn clear_endpoint() {
    let _ = std::fs::remove_file(nd_core::paths::ipc_file());
}

/// 给常驻进程发一条命令。
///
/// * `Ok(None)`：常驻进程没在跑（管道不存在），调用方自行降级处理；
/// * `Ok(Some(resp))`：拿到了回复；
/// * `Err(_)`：连上了但通讯出错。
pub fn call_daemon(
    action: &str,
    detail: Option<&nd_core::ipc::CommandDetail>,
    timeout_ms: u32,
) -> Result<Option<Response>> {
    let Some(endpoint) = load_endpoint() else {
        return Ok(None);
    };
    let command = match detail {
        Some(d) => nd_core::ipc::Command::Detailed(d.clone()),
        None => nd_core::ipc::Command::Bare(action.to_string()),
    };
    let request = Request {
        token: endpoint.token,
        command,
    };
    match call(&endpoint.pipe, &request, timeout_ms) {
        Ok(resp) => Ok(Some(resp)),
        Err(e) => {
            let text = e.to_string();
            // 管道不存在 = 常驻进程没起来，属于"没有"而不是"出错"。
            if text.contains("没有运行") {
                Ok(None)
            } else {
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_names_are_distinct_and_local() {
        let serve = serve_pipe_name();
        let helper = helper_pipe_name();
        assert!(serve.starts_with(r"\\.\pipe\NightDisplay-serve-"));
        assert!(helper.starts_with(r"\\.\pipe\NightDisplay-helper-"));
        assert_ne!(serve, helper);
    }

    #[test]
    fn user_tag_is_hex() {
        let tag = user_tag();
        assert_ne!(tag, "");
        assert!(tag.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn wide_is_nul_terminated() {
        let w = wide("abc");
        assert_eq!(w.last().copied(), Some(0));
        assert_eq!(w.len(), 4);
    }
}
