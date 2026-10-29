//! 诊断包：一个 ZIP，装下排障要看的所有东西。
//!
//! 用户在「诊断」页点一下「导出诊断包」，拿到的就是这里生成的文件。
//! 里面**只有**本机的日志、配置和硬件信息 —— 不含远程工具账号、不含桌面内容。
//! 路径和令牌会先被 [`redact`] 换掉，因为诊断包经常会被贴到 issue 里。
//!
//! 包里固定这几项：
//!
//! ```text
//! manifest.txt            包里有什么、什么时候导出的
//! selfcheck.json          自检结果（显卡、驱动、每块屏的真实模式、DDC 支持）
//! config.json             配置（去掉本机路径和令牌）
//! sessions.jsonl          最近 5 次夜间会话，每一步的耗时和结果
//! logs/<组件>-<日期>.…    按组件分开的日志
//! ```text
//!
//! 为什么自己写 ZIP 而不用依赖：见 [`crate::zip`] 的模块说明。

use std::fmt::Write as _;
use std::path::Path;

use chrono::Local;
use serde::Serialize;

use crate::status::SelfCheck;
use crate::zip::ZipWriter;

/// 诊断包里最多放最近几次会话。再往前翻意义不大，包也会变大。
pub const SESSIONS_IN_BUNDLE: usize = 5;

/// 诊断包的名字，带导出时刻，同一秒导出两次也不会互相覆盖（第二次会加后缀）。
#[must_use]
pub fn bundle_name() -> String {
    format!(
        "NightDisplay-diag-{}.zip",
        Local::now().format("%Y%m%d-%H%M%S")
    )
}

/// 把文本里的本机标识抹掉：用户目录、账号名、令牌。
///
/// 规则很保守，只处理三种确定会泄漏身份的东西：
/// * 用户主目录的绝对路径 → `<home>`；
/// * 一串 64 位十六进制（IPC 令牌就是这个形状）→ `<token>`；
/// * 当前登录用户名出现在路径里的部分 → `<user>`。
#[must_use]
pub fn redact(text: &str, home: &Path) -> String {
    let home_text = home.to_string_lossy();
    let mut out = text.replace(home_text.as_ref(), "<home>");
    if let Some(name) = home.file_name().and_then(|n| n.to_str()) {
        // 只替换长度 >= 3 的用户名，避免把 "box" 这种词误伤。
        if name.len() >= 3 && name != "Users" && name != "home" {
            out = out.replace(name, "<user>");
        }
    }
    out = redact_tokens(&out);
    out
}

/// 把形如 64 位十六进制的串换成 `<token>`。
///
/// 按**字符**扫描而不是字节：日志里有大量中文，按字节重建会把 UTF-8 拆坏。
fn redact_tokens(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run_start: Option<usize> = None;
    let mut run_len = 0_usize;
    let flush = |out: &mut String, start: usize, len: usize, end: usize| {
        if len == TOKEN_HEX_LEN {
            out.push_str("<token>");
        } else {
            out.push_str(&text[start..end]);
        }
    };
    for (index, ch) in text.char_indices() {
        if ch.is_ascii_hexdigit() {
            if run_start.is_none() {
                run_start = Some(index);
            }
            run_len += 1;
        } else {
            if let Some(start) = run_start.take() {
                flush(&mut out, start, run_len, index);
            }
            run_len = 0;
            out.push(ch);
        }
    }
    if let Some(start) = run_start {
        flush(&mut out, start, run_len, text.len());
    }
    out
}

/// IPC 令牌的十六进制长度（32 字节）。
const TOKEN_HEX_LEN: usize = 64;

/// 一个已经攒好内容的诊断包。
pub struct Bundle {
    writer: ZipWriter,
    lines: Vec<String>,
    files: Vec<(String, usize)>,
}

impl Default for Bundle {
    fn default() -> Self {
        Self::new()
    }
}

impl Bundle {
    /// 新建一个空包。
    #[must_use]
    pub fn new() -> Self {
        Self {
            writer: ZipWriter::new(),
            lines: Vec::new(),
            files: Vec::new(),
        }
    }

    /// 加一个文本文件。
    pub fn add_text(&mut self, name: &str, text: &str, home: &Path) {
        let body = redact(text, home);
        self.files.push((name.to_string(), body.len()));
        self.writer.add(name, body);
    }

    /// 加一段 JSON（失败时记下原因，不让整个导出失败）。
    pub fn add_json<T: Serialize>(&mut self, name: &str, value: &T, home: &Path) {
        match serde_json::to_string_pretty(value) {
            Ok(text) => self.add_text(name, &text, home),
            Err(e) => self.lines.push(format!("跳过 {name}：序列化失败（{e}）")),
        }
    }

    /// 加一行清单说明（例如"日志 7 个、共 1.2 MiB"）。
    pub fn note(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }

    /// 已加入的文件数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// 是不是空包。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// 写盘。`manifest.txt` 在所有文件加入之后才生成，所以在这里补。
    pub fn write(mut self, path: &Path) -> std::io::Result<()> {
        let mut manifest = String::new();
        let _ = writeln!(
            manifest,
            "NightDisplay {} 诊断包\n导出时间：{}",
            crate::VERSION,
            Local::now().format("%Y-%m-%d %H:%M:%S")
        );
        for line in &self.lines {
            let _ = writeln!(manifest, "{line}");
        }
        let _ = writeln!(manifest, "\n文件：");
        let files = std::mem::take(&mut self.files);
        for (name, size) in &files {
            let _ = writeln!(manifest, "  {name}  {size} 字节");
        }
        self.writer.add("manifest.txt", manifest);
        self.writer.write_to(path)
    }
}

/// 自检结果的人读摘要。
///
/// `--action selfcheck` 先打这份摘要再打 JSON：人看上半段，机器看下半段。
#[must_use]
pub fn selfcheck_summary(report: &SelfCheck) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "NightDisplay {} · {}（build {}）",
        report.version, report.os, report.build
    );
    let _ = writeln!(
        out,
        "模式：{}（实际 {}）· 每日计划 {}",
        report.mode.label(),
        report.effective_mode.label(),
        report
            .ranges
            .iter()
            .map(|(start, end)| format!("{start}→{end}"))
            .collect::<Vec<_>>()
            .join("，")
    );
    let _ = writeln!(
        out,
        "虚拟显示驱动：{} · 采集排除：{} · gamma：{} · DDC/CI 面板：{} 块",
        if report.virtual_display {
            "已安装"
        } else {
            "未安装"
        },
        if report.capture_exclusion {
            "可用"
        } else {
            "不可用"
        },
        if report.gamma_control {
            "可用"
        } else {
            "不可用"
        },
        report.brightness_displays
    );
    if report.gpu.is_empty() {
        let _ = writeln!(out, "显卡：读不到");
    } else {
        let _ = writeln!(
            out,
            "显卡：{}",
            report
                .gpu
                .iter()
                .map(crate::status::GpuInfo::label)
                .collect::<Vec<_>>()
                .join("，")
        );
    }
    for monitor in &report.monitors {
        let _ = writeln!(out, "显示器：{}", monitor.label());
    }
    let _ = writeln!(out, "自启：{}", report.registration.summary());
    if !report.notes.is_empty() {
        let _ = writeln!(out, "\n结论：");
        for note in &report.notes {
            let _ = writeln!(out, "  · {note}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn redact_hides_home_and_tokens() {
        let home = PathBuf::from("/home/alice/.local/state/nightdisplay");
        let token = "a".repeat(64);
        let text = format!("path={} token={token}", home.display());
        let out = redact(&text, &home);
        assert!(out.contains("<home>"), "{out}");
        assert!(out.contains("<token>"), "{out}");
        assert!(!out.contains(&token), "{out}");
        assert!(!out.contains("alice"), "{out}");
    }

    #[test]
    fn redact_leaves_normal_text_alone() {
        let home = PathBuf::from("/home/alice");
        let text = "虚拟屏切换耗时 18ms，活动 target 2→2";
        assert_eq!(redact(text, &home), text);
    }

    #[test]
    fn redact_keeps_short_hex_runs() {
        let home = PathBuf::from("/home/alice");
        // build 号、GUID 片段都不该被当成令牌。
        let text = "build 22631 guid {8c5e7fda-e8bf-4a96}";
        assert_eq!(redact(text, &home), text);
    }

    #[test]
    fn bundle_lists_its_files() {
        let home = PathBuf::from("/home/alice");
        let mut bundle = Bundle::new();
        bundle.add_text("a.txt", "hello", &home);
        bundle.note("测试");
        assert_eq!(bundle.len(), 1);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.zip");
        bundle.write(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        // 本地文件头签名 + 中央目录结尾签名，证明是个能解开的 zip。
        assert_eq!(&bytes[..4], b"PK\x03\x04");
        assert!(bytes.windows(4).any(|w| w == b"PK\x05\x06"));
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("manifest.txt"));
        assert!(text.contains("a.txt"));
    }
}
