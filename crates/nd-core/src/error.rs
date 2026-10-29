//! 错误类型。给用户看的文案一律中文，技术细节只进日志。

use std::fmt;

pub type NdResult<T> = Result<T, anyhow::Error>;

/// 可以直接展示给用户、不需要再包一层的错误。
#[derive(Debug, Clone)]
pub struct UserMessage(pub String);

impl fmt::Display for UserMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UserMessage {}

/// 构造一条用户可读错误。
pub fn user(msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(UserMessage(msg.into()))
}

/// 构造并返回 `Err(user(msg))`。
#[macro_export]
macro_rules! bail_user {
    ($($arg:tt)*) => {
        return Err($crate::error::user(format!($($arg)*)))
    };
}

/// 只在日志里出现的技术错误，最终展示时统一包上中文说明。
#[derive(Debug, thiserror::Error)]
pub enum NdError {
    #[error("配置文件损坏，请检查 {path}：{source}")]
    ConfigCorrupt {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("写入 {path} 失败：{source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("另一个 NightDisplay 操作仍在进行，请稍后再试")]
    Busy,
    #[error("{0}")]
    Other(String),
}

/// 把错误压成一行，供 IPC 和状态文本使用。
pub fn one_line(err: &anyhow::Error) -> String {
    let mut text = err.to_string();
    text = text.replace(['\n', '\r'], " ");
    if text.len() > 600 {
        let mut cut = 600;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push('…');
    }
    text
}
