//! 本地 IPC：GUI / CLI 与常驻进程之间的私有协议。
//!
//! 这一层只做**协议**，传输由平台层实现（Windows 命名管道）。这样协议本身
//! 能在 Linux CI 上跑测试，GUI（第二阶段 Tauri）接进来时也不需要改协议。
//!
//! 安全模型：
//! * 端点带随机令牌（32 字节 hex），写在数据目录里，只有本用户能读；
//! * 令牌用常量时间比较，避免时序侧信道；
//! * 报文长度上限 8 KiB，超长直接断开；
//! * 一条连接一条请求一条回复，行分隔 JSON。

use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};

use crate::error::user;
use crate::NdResult;

/// 单条报文最大字节数（含换行）。
pub const MAX_PACKET: usize = 8 * 1024;

/// 客户端发来的命令。字符串形式与旧版 CLI 保持一致，另外支持带参数的请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Command {
    /// `"night"`、`"day"`、`"status"` 这类无参数命令。
    Bare(String),
    /// `{"action": "test", "mode": "guard", "seconds": 15}`。
    Detailed(CommandDetail),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandDetail {
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_id: Option<String>,
}

impl Command {
    #[must_use]
    pub fn action(&self) -> &str {
        match self {
            Command::Bare(s) => s.as_str(),
            Command::Detailed(d) => d.action.as_str(),
        }
    }
}

/// 一条请求：令牌 + 命令。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub token: String,
    pub command: Command,
}

/// 一条回复：成功与否 + 文本结果。
///
/// 结果字符串沿用旧版的约定（`"ok"` 表示成功），GUI 只要看 `ok` 就够了。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    pub result: String,
}

impl Response {
    #[must_use]
    pub fn ok() -> Self {
        Self {
            ok: true,
            result: "ok".into(),
        }
    }

    pub fn value(text: impl Into<String>) -> Self {
        Self {
            ok: true,
            result: text.into(),
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            ok: false,
            result: text.into(),
        }
    }
}

/// 32 字节随机令牌的十六进制表示。
#[must_use]
pub fn new_token() -> String {
    use rand::RngCore;
    use std::fmt::Write as _;

    let mut buf = [0u8; 32];
    rand::rng().fill_bytes(&mut buf);
    buf.iter().fold(String::with_capacity(64), |mut acc, b| {
        // Writing into a String cannot fail.
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

/// 令牌比较，常量时间。
#[must_use]
pub fn token_matches(expected: &str, given: &str) -> bool {
    let a = expected.as_bytes();
    let b = given.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// 编码一条请求，末尾带换行。
pub fn encode_request(req: &Request) -> NdResult<String> {
    let mut text =
        serde_json::to_string(req).map_err(|e| user(format!("IPC 请求无法编码：{e}")))?;
    text.push('\n');
    if text.len() > MAX_PACKET {
        return Err(user("IPC 请求过长"));
    }
    Ok(text)
}

/// 编码一条回复，末尾带换行。
#[must_use]
pub fn encode_response(resp: &Response) -> String {
    let mut text = serde_json::to_string(resp)
        .unwrap_or_else(|_| r#"{"ok":false,"result":"IPC 回复无法编码"}"#.to_string());
    text.push('\n');
    text
}

/// 从一行文本解出请求。超长、缺字段、类型不对都返回中文错误。
pub fn decode_request(line: &str) -> NdResult<Request> {
    if line.len() > MAX_PACKET {
        return Err(user("IPC 请求过长"));
    }
    serde_json::from_str::<Request>(line.trim_end_matches(['\r', '\n']))
        .map_err(|e| user(format!("IPC 请求格式不正确：{e}")))
}

/// 从一行文本解出回复。
pub fn decode_response(line: &str) -> NdResult<Response> {
    if line.len() > MAX_PACKET {
        return Err(user("IPC 回复过长"));
    }
    serde_json::from_str::<Response>(line.trim_end_matches(['\r', '\n']))
        .map_err(|e| user(format!("常驻进程回复格式不正确：{e}")))
}

/// 读一行，超过上限直接报错（防止对端不断发数据把内存吃满）。
pub fn read_line_limited<R: BufRead>(reader: &mut R) -> std::io::Result<Option<String>> {
    let mut buf = Vec::with_capacity(256);
    let mut overflow = false;
    loop {
        let mut byte = [0u8; 1];
        if reader.read(&mut byte)? == 0 {
            if buf.is_empty() && !overflow {
                return Ok(None);
            }
            break;
        }
        if byte[0] == b'\n' {
            break;
        }
        if buf.len() >= MAX_PACKET {
            overflow = true;
            continue; // 继续读到换行，保持连接同步
        }
        buf.push(byte[0]);
    }
    if overflow {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "IPC 报文超过长度上限",
        ));
    }
    String::from_utf8(buf)
        .map(Some)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "IPC 报文不是 UTF-8"))
}

/// 写一帧并刷新。
pub fn write_frame<W: Write>(writer: &mut W, text: &str) -> std::io::Result<()> {
    writer.write_all(text.as_bytes())?;
    writer.flush()
}

/// 端点信息：端口/管道名 + 令牌，写到 `ipc.json`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub token: String,
    /// 命名管道的完整名字（Windows）。
    pub pipe: String,
}

impl Endpoint {
    #[must_use]
    pub fn new(pipe: String) -> Self {
        Self {
            token: new_token(),
            pipe,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn bare_command_round_trip() {
        let req = Request {
            token: "t".into(),
            command: Command::Bare("night".into()),
        };
        let text = encode_request(&req).unwrap();
        assert!(text.ends_with('\n'));
        let back = decode_request(&text).unwrap();
        assert_eq!(back, req);
        assert_eq!(back.command.action(), "night");
    }

    #[test]
    fn detailed_command_round_trip() {
        let req = Request {
            token: "abc".into(),
            command: Command::Detailed(CommandDetail {
                action: "test".into(),
                mode: Some("guard".into()),
                seconds: Some(15),
                test_id: None,
            }),
        };
        let back = decode_request(&encode_request(&req).unwrap()).unwrap();
        assert_eq!(back.command.action(), "test");
        match back.command {
            Command::Detailed(d) => {
                assert_eq!(d.mode.as_deref(), Some("guard"));
                assert_eq!(d.seconds, Some(15));
            }
            Command::Bare(other) => panic!("应当解析成带参数命令，实际是 {other}"),
        }
    }

    #[test]
    fn token_comparison_is_exact() {
        assert!(token_matches("abc123", "abc123"));
        assert!(!token_matches("abc123", "abc124"));
        assert!(!token_matches("abc123", "abc12"));
        assert!(!token_matches("", "x"));
    }

    #[test]
    fn response_helpers() {
        assert!(Response::ok().ok);
        assert_eq!(Response::ok().result, "ok");
        assert!(!Response::error("坏了").ok);
        let back = decode_response(&encode_response(&Response::value("夜间"))).unwrap();
        assert_eq!(back.result, "夜间");
    }

    #[test]
    fn rejects_garbage_and_oversized_packets() {
        assert!(decode_request("not json").is_err());
        assert!(decode_request(&"x".repeat(MAX_PACKET + 10)).is_err());
        assert!(decode_response("{").is_err());
    }

    #[test]
    fn reads_lines_and_detects_overflow() {
        let mut ok = Cursor::new(b"{\"token\":\"t\",\"command\":\"ping\"}\n".to_vec());
        let line = read_line_limited(&mut ok).unwrap().unwrap();
        assert!(line.starts_with('{'));

        let mut empty = Cursor::new(Vec::new());
        assert!(read_line_limited(&mut empty).unwrap().is_none());

        let mut huge = Cursor::new(vec![b'a'; MAX_PACKET + 64]);
        huge.set_position(0);
        let mut padded = Vec::from(huge.get_ref().as_slice());
        padded.push(b'\n');
        let mut reader = Cursor::new(padded);
        assert!(read_line_limited(&mut reader).is_err());
    }

    #[test]
    fn token_looks_random() {
        let a = new_token();
        let b = new_token();
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
    }
}
