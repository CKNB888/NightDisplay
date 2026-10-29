//! 平台层的错误转换：把 `anyhow::Error` 变成给人看的中文一行话。
//!
//! 规则和旧版一致：界面上只显示一句人话，完整错误链只写进日志。
//! 这样既不会把 `Os { code: 5, ... }` 这种垃圾抛给用户，也不会丢掉排查线索。

/// 取错误链里最底层那句话（通常是 Win32 的说明），用于日志。
pub fn chain_text(err: &anyhow::Error) -> String {
    let mut parts: Vec<String> = err.chain().map(|e| e.to_string()).collect();
    parts.dedup();
    parts.join(" ← ")
}

/// 给用户看的一句话。已经是用户可读错误时原样返回，否则补一个通用前缀。
pub fn user_text(err: &anyhow::Error, fallback: &str) -> String {
    let text = err.to_string();
    if text.trim().is_empty() {
        return fallback.to_string();
    }
    // Windows 原始错误（"存取被拒。 (os error 5)"）不适合直接给用户看。
    if text.contains("os error") {
        return format!("{fallback}：{}", friendly_os_error(&text));
    }
    text
}

/// 把 `os error N` 翻成人话。
pub fn friendly_os_error(text: &str) -> String {
    for (code, message) in [
        (5, "权限不足，需要以管理员身份运行一次安装向导"),
        (2, "找不到文件或程序"),
        (3, "找不到路径"),
        (32, "文件正被另一个程序占用"),
        (112, "磁盘空间不足"),
        (1155, "安装程序缺失，请重新下载完整安装包"),
        (183, "同名文件已存在"),
        (1314, "缺少必要的权限（需要提升助手在运行）"),
    ] {
        if text.contains(&format!("os error {code}")) {
            return message.to_string();
        }
    }
    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friendly_maps_access_denied() {
        assert!(friendly_os_error("存取被拒。 (os error 5)").contains("管理员"));
    }

    #[test]
    fn friendly_passes_unknown_through() {
        assert_eq!(
            friendly_os_error("某段说明 (os error 9999)"),
            "某段说明 (os error 9999)"
        );
    }

    #[test]
    fn user_text_keeps_user_messages() {
        let err = anyhow::anyhow!("没有检测到任何显示器，无法创建黑幕");
        assert_eq!(
            user_text(&err, "失败"),
            "没有检测到任何显示器，无法创建黑幕"
        );
    }

    #[test]
    fn chain_text_joins_causes() {
        let inner = anyhow::anyhow!("底层原因");
        let err = inner.context("外层说明");
        let text = chain_text(&err);
        assert!(text.contains("外层说明"));
        assert!(text.contains("底层原因"));
    }
}
