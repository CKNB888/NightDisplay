//! 更新检查：只读 CKNB888/NightDisplay 的 releases API，链接一律自己拼。
//!
//! 规矩：
//! * 不信任 API 返回的任何 URL，下载页始终用 `REPO_URL` 拼；
//! * 网络失败只是"检查失败"，绝不打断主流程；
//! * 版本比较用语义化版本，`v` 前缀可有可无。

use serde::Deserialize;

use crate::{NdResult, REPO_NAME, REPO_OWNER, REPO_URL, VERSION};

/// releases 接口地址。
#[must_use]
pub fn api_url() -> String {
    format!("https://api.github.com/repos/{REPO_OWNER}/{REPO_NAME}/releases/latest")
}

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    #[allow(dead_code)]
    name: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    published_at: String,
    #[serde(default)]
    prerelease: bool,
}

/// 检查结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    pub latest: String,
    pub current: String,
    pub url: String,
    pub notes: String,
    pub published_at: String,
    pub is_newer: bool,
}

/// 解析 `4.0.1` / `v4.0.1`，返回 (major, minor, patch)。
#[must_use]
pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let t = text.trim().trim_start_matches(['v', 'V']);
    // 忽略 -beta 之类的后缀，只比较数字部分。
    let core = t.split(['-', '+']).next().unwrap_or(t);
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

/// `a` 是否比 `b` 新。解析不了的版本号一律当作"不是更新"。
#[must_use]
pub fn is_newer(a: &str, b: &str) -> bool {
    match (parse_version(a), parse_version(b)) {
        (Some(x), Some(y)) => x > y,
        _ => false,
    }
}

/// 处理 API 返回的 JSON 文本。
pub fn parse_release(json: &str, current: &str) -> NdResult<UpdateInfo> {
    let release: Release = serde_json::from_str(json)
        .map_err(|e| crate::error::user(format!("更新信息格式不正确：{e}")))?;
    let tag = release.tag_name.trim().to_string();
    if tag.is_empty() {
        return Err(crate::error::user("更新信息里没有版本号"));
    }
    Ok(UpdateInfo {
        is_newer: !release.prerelease && is_newer(&tag, current),
        url: crate::release_url(&tag),
        latest: tag,
        current: current.to_string(),
        notes: truncate(&release.body, 1200),
        published_at: release.published_at,
    })
}

/// 更新说明太长时截断，界面不撑爆。
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    format!("{cut}…")
}

/// 当前版本的更新检查结果（不联网），供 UI 在检查失败时展示。
#[must_use]
pub fn current_info() -> UpdateInfo {
    UpdateInfo {
        latest: VERSION.to_string(),
        current: VERSION.to_string(),
        url: crate::latest_release_url(),
        notes: String::new(),
        published_at: String::new(),
        is_newer: false,
    }
}

/// 下载页。永远指向我们自己的仓库，不用 API 返回的 `html_url`。
#[must_use]
pub fn download_page() -> String {
    format!("{REPO_URL}/releases/latest")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions_with_and_without_v() {
        assert_eq!(parse_version("v4.0.1"), Some((4, 0, 1)));
        assert_eq!(parse_version("4.1"), Some((4, 1, 0)));
        assert_eq!(parse_version(" 3 "), Some((3, 0, 0)));
        assert_eq!(parse_version("4.0.0-beta.1"), Some((4, 0, 0)));
        assert_eq!(parse_version("abc"), None);
    }

    #[test]
    fn compares_versions() {
        assert!(is_newer("v4.1.0", "4.0.0"));
        assert!(is_newer("4.0.10", "4.0.9"));
        assert!(!is_newer("4.0.0", "4.0.0"));
        assert!(!is_newer("4.0.0", "v4.1.0"));
        assert!(!is_newer("abc", "4.0.0"));
    }

    #[test]
    fn builds_url_from_own_repo_only() {
        let json = r#"{
            "tag_name": "v9.9.9",
            "name": "NightDisplay 9.9.9",
            "body": "修了很多问题",
            "published_at": "2026-10-29T00:00:00Z",
            "prerelease": false,
            "html_url": "https://evil.example.com/malware.zip"
        }"#;
        let info = parse_release(json, "4.0.0").unwrap();
        assert!(info.is_newer);
        assert_eq!(
            info.url,
            "https://github.com/CKNB888/NightDisplay/releases/tag/v9.9.9"
        );
        assert!(!info.url.contains("evil.example.com"));
    }

    #[test]
    fn prerelease_is_not_offered() {
        let json = r#"{"tag_name":"v9.9.9","prerelease":true}"#;
        let info = parse_release(json, "4.0.0").unwrap();
        assert!(!info.is_newer);
    }

    #[test]
    fn same_or_older_version_is_not_newer() {
        let json = r#"{"tag_name":"v4.0.0"}"#;
        assert!(!parse_release(json, "4.0.0").unwrap().is_newer);
        let json = r#"{"tag_name":"v3.0.0"}"#;
        assert!(!parse_release(json, "4.0.0").unwrap().is_newer);
    }

    #[test]
    fn bad_json_is_a_clean_error() {
        let err = parse_release("{", "4.0.0").unwrap_err().to_string();
        assert!(err.contains("更新信息格式不正确"), "{err}");
        let err = parse_release(r#"{"tag_name":""}"#, "4.0.0")
            .unwrap_err()
            .to_string();
        assert!(err.contains("没有版本号"), "{err}");
    }

    #[test]
    fn notes_are_truncated() {
        let body = "很长的更新说明".repeat(500);
        let json = format!(r#"{{"tag_name":"v5.0.0","body":"{body}"}}"#);
        let info = parse_release(&json, "4.0.0").unwrap();
        assert!(info.notes.chars().count() <= 1201);
    }

    #[test]
    fn api_url_points_at_the_project() {
        assert_eq!(
            api_url(),
            "https://api.github.com/repos/CKNB888/NightDisplay/releases/latest"
        );
        assert!(download_page().starts_with(REPO_URL));
    }
}
