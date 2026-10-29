//! 联网检查更新。
//!
//! 为了不把 TLS 栈和一堆依赖塞进安装包，HTTP 走 PowerShell 的 `Invoke-RestMethod`
//! （Windows 自带，走系统代理和证书库）。协议解析、版本比较、URL 拼装都在
//! `nd_core::updates` 里，可以单测；这里只负责取文本和给出中文结论。
//!
//! 原则：**检查失败绝不打断主流程**。用户点一下才查，查不到就说"这次没查到"。

#![cfg(windows)]

use anyhow::Result;
use nd_core::updates::{self, UpdateInfo};

use crate::sysutil;

/// 拉 releases API 的原始 JSON。
fn fetch_latest_json() -> Result<String> {
    let url = updates::api_url();
    // PowerShell 里用单引号包字符串，避免 URL 里的字符被解释。
    let script = format!(
        r#"$ProgressPreference='SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$r = Invoke-RestMethod -Uri '{url}' -Headers @{{ 'User-Agent' = 'NightDisplay/{ver}'; 'Accept' = 'application/vnd.github+json' }} -TimeoutSec 15
$r | ConvertTo-Json -Depth 4 -Compress"#,
        url = url,
        ver = nd_core::VERSION
    );
    let out = sysutil::run_powershell(&script, &[])?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let last = err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        return Err(nd_core::error::user(format!(
            "连不上 GitHub 检查更新{}{}",
            if last.is_empty() { "" } else { "：" },
            last.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// 检查更新。返回更新信息，`is_newer` 表示有新版本。
pub fn check() -> Result<UpdateInfo> {
    let json = fetch_latest_json()?;
    if json.is_empty() {
        return Err(nd_core::error::user(
            "GitHub 返回了空内容，可能是网络被拦截了",
        ));
    }
    updates::parse_release(&json, nd_core::VERSION)
}

/// 检查更新并翻译成一句人话（CLI `--action update` 用）。
pub fn check_text() -> Result<String> {
    let info = check()?;
    if info.is_newer {
        Ok(format!(
            "发现新版本 {latest}（当前 {current}）。\n下载页：{url}\n\n{notes}",
            latest = info.latest,
            current = info.current,
            url = info.url,
            notes = info.notes.trim()
        ))
    } else {
        Ok(format!(
            "当前已是最新版本（{}）。\n发布页：{}",
            info.current,
            updates::download_page()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_only_uses_our_api_url() {
        // 检查用的脚本必须指向自己的仓库接口，不能出现外部下载地址。
        let url = updates::api_url();
        assert!(url.starts_with("https://api.github.com/repos/CKNB888/NightDisplay/"));
    }

    #[test]
    fn current_info_is_never_newer() {
        let info = updates::current_info();
        assert!(!info.is_newer);
        assert_eq!(info.current, nd_core::VERSION);
    }
}
