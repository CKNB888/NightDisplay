//! 结构化日志：按组件分文件、按天滚动、同时写 JSON Lines 和可读文本。
//!
//! 为什么要两份：
//! * 文本那份是给人看的 —— 用户双击打开就能读，排障时不用装工具；
//! * JSON Lines 那份是给诊断包和脚本看的 —— 一行一条记录，带 `level`、
//!   `target`、`fields` 和 `trace_id`，可以直接 `grep`/`jq`。
//!
//! 文件布局（全部在 `<数据目录>/logs/` 下）：
//!
//! ```text
//! daemon-2026-10-29.log      可读文本
//! daemon-2026-10-29.jsonl    同一天的机器可读版本
//! watchdog-2026-10-29.log
//! helper-2026-10-29.log
//! gui-2026-10-29.log
//! installer-2026-10-29.log
//! traceback.log              只在出错时追加完整错误链
//! ```
//!
//! 保留策略：按天删 14 天前的文件；总量超过 64 MiB 时从最旧的开始删。
//! 日志写入失败**绝不影响**显示动作 —— 写不进去就当写成功。

use std::fmt::Write as _;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use chrono::Local;
use tracing_subscriber::fmt::writer::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{reload, EnvFilter, Registry};

use crate::paths;

/// 日志保留天数。
pub const KEEP_DAYS: u64 = 14;
/// 日志目录总大小上限（字节）。超过就从最旧的开始删。
pub const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

/// 写日志的是哪个程序。每个组件一个独立文件，排障时不用在几万行里捞。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Component {
    /// 常驻进程（`--action serve`）。
    Daemon,
    /// 看门狗。
    Watchdog,
    /// 提升助手。
    Helper,
    /// 图形界面。
    Gui,
    /// 安装 / 卸载。
    Installer,
    /// 一次性命令行动作。
    Cli,
}

impl Component {
    /// 文件名前缀。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Daemon => "daemon",
            Self::Watchdog => "watchdog",
            Self::Helper => "helper",
            Self::Gui => "gui",
            Self::Installer => "installer",
            Self::Cli => "cli",
        }
    }

    /// 从动作名推断组件。
    #[must_use]
    pub fn for_action(action: &str) -> Self {
        match action {
            "serve" => Self::Daemon,
            "watchdog" => Self::Watchdog,
            "helper" => Self::Helper,
            "gui" => Self::Gui,
            "setup" | "repair" | "install-vdd" | "uninstall-vdd" => Self::Installer,
            _ => Self::Cli,
        }
    }
}

/// 一个把日志追加到 `<dir>/<component>-YYYY-MM-DD.<ext>` 的 writer。
#[derive(Clone)]
pub struct DailyWriter {
    dir: PathBuf,
    prefix: String,
    extension: &'static str,
    lock: Arc<Mutex<()>>,
}

impl DailyWriter {
    fn new(dir: PathBuf, component: Component, extension: &'static str) -> Self {
        Self {
            dir,
            prefix: component.as_str().to_string(),
            extension,
            lock: Arc::new(Mutex::new(())),
        }
    }

    /// 当前这一天的文件路径。
    #[must_use]
    pub fn today_path(&self) -> PathBuf {
        self.dir.join(format!(
            "{}-{}.{}",
            self.prefix,
            Local::now().format("%Y-%m-%d"),
            self.extension
        ))
    }
}

/// 一次写入的句柄。
pub struct DailyGuard {
    writer: DailyWriter,
}

impl Write for DailyGuard {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // 日志写不进去就当作写成功：磁盘满了不该让黑屏动作失败。
        let _guard = self.writer.lock.lock();
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.writer.today_path())
        {
            let _ = file.write_all(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for DailyWriter {
    type Writer = DailyGuard;

    fn make_writer(&'a self) -> DailyGuard {
        DailyGuard {
            writer: self.clone(),
        }
    }
}

/// 运行期调档开关（`--debug` 用它把级别调高，不用重启）。
static RELOAD: OnceLock<reload::Handle<EnvFilter, Registry>> = OnceLock::new();
/// 是否已经初始化过（重复调用直接返回）。
static INITIALISED: AtomicBool = AtomicBool::new(false);

/// 初始化日志。默认 `info`，`NIGHTDISPLAY_LOG=debug` 或 [`set_debug`] 可调。
///
/// 重复调用是安全的（例如库被 GUI 嵌入时）。
pub fn init(component: Component) -> anyhow::Result<()> {
    if INITIALISED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    let dir = paths::logs_dir();
    let _ = std::fs::create_dir_all(&dir);
    prune(&dir);

    let level = std::env::var("NIGHTDISPLAY_LOG").unwrap_or_else(|_| "info".into());
    let filter = EnvFilter::try_new(format!("{level},nd_core=info,nd_win=info"))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let (filter, handle) = reload::Layer::new(filter);
    let _ = RELOAD.set(handle);

    let text = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(true)
        .with_writer(DailyWriter::new(dir.clone(), component, "log"));
    let json = tracing_subscriber::fmt::layer()
        .json()
        .flatten_event(true)
        .with_ansi(false)
        .with_target(true)
        .with_current_span(true)
        .with_writer(DailyWriter::new(dir, component, "jsonl"));
    let console = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(false)
        .with_writer(std::io::stderr);

    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(console)
        .with(text)
        .with(json)
        .try_init();
    Ok(())
}

/// 运行期把级别调到 `debug`。`--debug` 用它。
pub fn set_debug() {
    if let Some(handle) = RELOAD.get() {
        let filter = EnvFilter::new("debug,nd_core=debug,nd_win=debug");
        if let Err(e) = handle.reload(filter) {
            tracing::warn!(error = %e, "无法调高日志级别");
        }
    }
}

/// 把完整错误链写进 `logs/traceback.log`，只在出错时调用。
pub fn record_error(context: &str, err: &anyhow::Error) {
    let mut text = format!(
        "{} {}\n{:#}\n\n",
        Local::now().format("%Y-%m-%d %H:%M:%S"),
        context,
        err
    );
    for (index, cause) in err.chain().enumerate().skip(1) {
        let _ = writeln!(text, "  cause[{index}]: {cause}");
    }
    let dir = paths::logs_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("traceback.log"))
    {
        let _ = file.write_all(text.as_bytes());
    }
}

/// 按保留策略清理日志目录。启动时跑一次就够了。
///
/// 两步：先删超过 [`KEEP_DAYS`] 天的，再按总大小从最旧的开始删。
pub fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let cutoff = Local::now() - chrono::Duration::days(i64::try_from(KEEP_DAYS).unwrap_or(14));
    let date_format = "%Y-%m-%d";
    let mut kept: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // traceback.log 不带日期，单独按修改时间处理。
        let dated = name.rsplit_once('.').and_then(|(stem, _)| {
            stem.rsplit_once('-').and_then(|_| {
                let tail = stem.get(stem.len().saturating_sub(10)..)?;
                chrono::NaiveDate::parse_from_str(tail, date_format).ok()
            })
        });
        if let Some(date) = dated {
            let midnight = date
                .and_hms_opt(0, 0, 0)
                .and_then(|naive| naive.and_local_timezone(Local).single());
            if midnight.is_some_and(|at| at < cutoff) {
                let _ = std::fs::remove_file(&path);
                continue;
            }
        }
        let meta = entry.metadata().ok();
        let size = meta.as_ref().map_or(0, std::fs::Metadata::len);
        let modified = meta
            .and_then(|m| m.modified().ok())
            .unwrap_or(std::time::UNIX_EPOCH);
        kept.push((modified, size, path));
    }

    let total: u64 = kept.iter().map(|(_, size, _)| *size).sum();
    if total <= MAX_TOTAL_BYTES {
        return;
    }
    // 从最旧的开始删，直到降到上限以内。
    kept.sort_by_key(|(modified, _, _)| *modified);
    let mut running = total;
    for (_, size, path) in kept {
        if running <= MAX_TOTAL_BYTES {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            running = running.saturating_sub(size);
        }
    }
}

/// 当前日志目录里所有文件的大小之和（诊断包和设置页要用）。
#[must_use]
pub fn total_size() -> u64 {
    let Ok(entries) = std::fs::read_dir(paths::logs_dir()) else {
        return 0;
    };
    entries
        .flatten()
        .filter_map(|entry| entry.metadata().ok())
        .filter(std::fs::Metadata::is_file)
        .map(|meta| meta.len())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_names_are_stable() {
        assert_eq!(Component::Daemon.as_str(), "daemon");
        assert_eq!(Component::Watchdog.as_str(), "watchdog");
        assert_eq!(Component::Helper.as_str(), "helper");
        assert_eq!(Component::Gui.as_str(), "gui");
        assert_eq!(Component::Installer.as_str(), "installer");
        assert_eq!(Component::Cli.as_str(), "cli");
    }

    #[test]
    fn component_is_inferred_from_the_action() {
        assert_eq!(Component::for_action("serve"), Component::Daemon);
        assert_eq!(Component::for_action("watchdog"), Component::Watchdog);
        assert_eq!(Component::for_action("helper"), Component::Helper);
        assert_eq!(Component::for_action("gui"), Component::Gui);
        assert_eq!(Component::for_action("setup"), Component::Installer);
        assert_eq!(Component::for_action("status"), Component::Cli);
        assert_eq!(Component::for_action(""), Component::Cli);
    }

    #[test]
    fn daily_path_has_component_and_date() {
        let writer = DailyWriter::new(PathBuf::from("/tmp/logs"), Component::Daemon, "jsonl");
        let path = writer.today_path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("daemon-"), "{name}");
        assert!(path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl")));
        assert_eq!(name.len(), "daemon-2026-10-29.jsonl".len());
    }

    #[test]
    fn prune_removes_only_old_dated_files() {
        let dir = tempfile::tempdir().unwrap();
        let stale = dir.path().join("daemon-2000-01-01.log");
        let fresh = dir.path().join("daemon-2999-01-01.log");
        let other = dir.path().join("traceback.log");
        for path in [&stale, &fresh, &other] {
            std::fs::write(path, b"x").unwrap();
        }
        prune(dir.path());
        assert!(!stale.exists(), "过期日志应被删掉");
        assert!(fresh.exists(), "未来日期不该被误删");
        assert!(other.exists(), "不带日期的文件按修改时间保留");
    }

    #[test]
    fn prune_keeps_totals_under_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon-2999-01-01.log");
        std::fs::write(&path, b"x").unwrap();
        prune(dir.path());
        assert!(path.exists());
    }
}
