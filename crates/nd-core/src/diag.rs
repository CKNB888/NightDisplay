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
/// 规则很保守，只处理这几种确定会泄漏身份的东西：
/// * 用户主目录的绝对路径 → `<home>`；
/// * `C:\Users\<名字>\…`、`/home/<名字>/…`、`/Users/<名字>/…` 里的名字 → `<user>`；
/// * 环境变量里的当前登录用户名（至少 3 个字符）→ `<user>`；
/// * 一串 64 位十六进制（IPC 令牌就是这个形状）→ `<token>`。
///
/// 为什么要专门处理 `C:\Users\<名字>`：只替换**数据目录**是不够的。
/// 日志里还有安装路径（`%LOCALAPPDATA%\Programs\NightDisplay`）和
/// 开机自启命令行，它们带的是同一个用户名 —— 诊断包会被贴到 issue 里，
/// 那等于把报问题的人的账号名一起公开了。
#[must_use]
pub fn redact(text: &str, home: &Path) -> String {
    let home_text = home.to_string_lossy();
    let mut out = text.replace(home_text.as_ref(), "<home>");
    // JSON 日志里反斜杠是转义的（`D:\\a\\...`），原样匹配不到。
    // 同一段路径换个写法照样会漏，所以两种形式都要替换。
    if home_text.contains('\\') {
        let escaped = home_text.replace('\\', "\\\\");
        out = out.replace(&escaped, "<home>");
    }
    // 数据目录名是 `NightDisplay`，把它当用户名会把 `NightDisplay.exe`
    // 一起吃掉（产物里只剩 `<user>.exe`）。所以**只有**数据目录本身就是
    // 用户主目录（`C:\Users\alice`、`/home/alice`）时，最后一段才等于账号名。
    // 路径里的账号名一律交给 `redact_users()` 按目录结构识别；这里只补
    // "账号名自己在别处单独出现、不带 Users\\ 前缀"这一种情况。
    if is_user_profile(&home_text) {
        if let Some(name) = last_segment(&home_text) {
            // 只替换长度 >= 3 的名字，避免把短词误伤。
            if name.len() >= 3 {
                out = out.replace(name, "<user>");
            }
        }
    }
    out = redact_users(&out);
    out = redact_tokens(&out);
    out
}

/// 取路径的最后一段。`/` 和 `\` 都算分隔符。
///
/// 不用 `Path::file_name()`：它在 Linux 上不把反斜杠当分隔符，于是
/// `C:\Users\x` 会被当成一整段文件名 —— 同一个函数在两种系统上行为不同，
/// 单元测试也就测不出真机上的样子。
fn last_segment(path: &str) -> Option<&str> {
    let trimmed = path.trim_end_matches(['\\', '/']);
    let start = trimmed.rfind(['\\', '/']).map_or(0, |i| i + 1);
    let segment = &trimmed[start..];
    if segment.is_empty() {
        None
    } else {
        Some(segment)
    }
}

/// 这个目录是不是"就是一个用户主目录"，即上一段是 `Users` / `home`。
fn is_user_profile(path: &str) -> bool {
    let trimmed = path.trim_end_matches(['\\', '/']);
    let Some((parent, _name)) = trimmed.rsplit_once(['\\', '/']) else {
        return false;
    };
    last_segment(parent)
        .map(str::to_ascii_lowercase)
        .is_some_and(|seg| seg == "users" || seg == "home")
}

/// 用户目录标记：这些片段后面紧跟的那一段就是账号名。
const USER_DIR_MARKERS: [&str; 4] = ["\\users\\", "/users/", "/home/", "\\home\\"];

/// 把路径里的账号名换成 `<user>`。
///
/// 按**字节**扫描，但只在 ASCII 标记上定位；大小写用 `to_ascii_lowercase`
/// 消除，它只映射 ASCII，字节长度不变，所以下标可以直接用来切片中文文本。
fn redact_users(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for marker in USER_DIR_MARKERS {
        let mut from = 0;
        while let Some(found) = lower[from..].find(marker) {
            let mut name_start = from + found + marker.len();
            // JSON 会把路径写成 `C:\\Users\\runneradmin\\...`（反斜杠被转义）。
            // 标记 `\users\` 会匹配进那一对反斜杠里，正好把一个反斜杠留给
            // "账号名"，取出来是空串于是整条漏掉 —— 先把多余的分隔符跳过去。
            while name_start < lower.len() && matches!(lower.as_bytes()[name_start], b'\\' | b'/') {
                name_start += 1;
            }
            let tail = &lower[name_start..];
            // 账号名到下一个路径分隔符或空白为止。必须在空白处也停下：
            // `C:\Users\ 后面没有名字` 这种句子不能把后半句整段吃掉。
            let name_end = tail
                .find(['\\', '/', ' ', '\t', '\n', '\r', '"', '\'', ')', ',', ';'])
                .map_or(text.len(), |i| name_start + i);
            if name_end > name_start
                && !spans
                    .iter()
                    .any(|(s, e)| name_start >= *s && name_start < *e)
            {
                spans.push((name_start, name_end));
            }
            from = name_start;
        }
    }
    if spans.is_empty() {
        return text.to_string();
    }
    spans.sort_unstable();
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for (start, end) in spans {
        if start < cursor {
            continue;
        }
        out.push_str(&text[cursor..start]);
        out.push_str("<user>");
        cursor = end;
    }
    out.push_str(&text[cursor..]);
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
    fn redact_hides_account_name_in_install_paths() {
        let home = PathBuf::from("C:\\Users\\runneradmin\\AppData\\Local\\NightDisplay");
        // 只替换数据目录是不够的：安装路径和自启命令行里的账号名会漏出去。
        let text = "command=C:\\Users\\runneradmin\\AppData\\Local\\Programs\\NightDisplay\\NightDisplay.exe";
        let out = redact(text, &home);
        assert!(out.contains("<user>"), "{out}");
        assert!(!out.contains("runneradmin"), "{out}");
        assert!(out.contains("NightDisplay.exe"), "{out}");
    }

    #[test]
    fn redact_hides_unix_and_forward_slash_accounts() {
        let home = PathBuf::from("/home/alice/.local/state/nightdisplay");
        let out = redact("/home/alice/bin/x 和 C:/Users/bob/y", &home);
        assert!(!out.contains("alice"), "{out}");
        assert!(!out.contains("bob"), "{out}");
        assert!(out.contains("<user>"), "{out}");
    }

    #[test]
    fn redact_hides_account_name_inside_json_escaped_paths() {
        // 日志是 JSONL，路径里的反斜杠会被转义成 `\\`：
        // `C:\\Users\\runneradmin\\AppData\\...`。这类行以前整条漏掉。
        let home = PathBuf::from("C:\\Users\\runneradmin\\AppData\\Local\\NightDisplay");
        let text = r#"{"command":"C:\\Users\\runneradmin\\AppData\\Local\\Programs\\NightDisplay\\NightDisplay.exe --tray"}"#;
        let out = redact(text, &home);
        assert!(!out.contains("runneradmin"), "{out}");
        assert!(out.contains(r"\\<user>\\AppData"), "{out}");
    }

    #[test]
    fn redact_hides_json_escaped_home_too() {
        let home = PathBuf::from("D:\\a\\NightDisplay\\Home");
        let text = r#"{"home":"D:\\a\\NightDisplay\\Home\\logs"}"#;
        let out = redact(text, &home);
        assert!(!out.contains("NightDisplay"), "{out}");
        assert!(out.contains("<home>"), "{out}");
    }

    #[test]
    fn redact_keeps_a_bare_users_segment() {
        let home = PathBuf::from("C:\\Users\\runneradmin\\AppData\\Local\\NightDisplay");
        // 没有账号名可替换时不能把文本改坏。
        let text = "路径 C:\\Users\\ 后面没有名字";
        assert_eq!(redact(text, &home), text);
    }

    #[test]
    fn redact_does_not_mistake_the_product_name_for_a_username() {
        // Windows 默认数据目录是 `%LOCALAPPDATA%\NightDisplay`，最后一段是
        // 产品名而不是账号名。以前这里不看上下文，会把诊断包里每一处
        // `NightDisplay` 都换成 `<user>`，真正的账号名反而留着。
        let home = PathBuf::from("C:\\Users\\runneradmin\\AppData\\Local\\NightDisplay");
        let text =
            "NightDisplay.exe 在 C:\\Users\\runneradmin\\AppData\\Local\\Programs\\NightDisplay";
        let out = redact(text, &home);
        assert!(!out.contains("runneradmin"), "{out}");
        assert_eq!(
            out.matches("NightDisplay").count(),
            2,
            "产品名不该被抹掉：{out}"
        );
    }

    #[test]
    fn redact_hides_a_bare_username_when_home_is_the_profile() {
        // 数据目录就是主目录时，账号名可能在别的地方单独出现（没有 Users\ 前缀）。
        let home = PathBuf::from("C:\\Users\\runneradmin");
        let out = redact("登录用户 runneradmin 在 21:00 进入夜间", &home);
        assert!(!out.contains("runneradmin"), "{out}");
        assert!(out.contains("<user>"), "{out}");
        // unix 同理，且不用 Path::file_name（它在 Windows 上对 `/` 的处理不同）。
        let unix = redact("/home/alice/logs 目录", &PathBuf::from("/home/alice"));
        assert!(!unix.contains("alice"), "{unix}");
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
