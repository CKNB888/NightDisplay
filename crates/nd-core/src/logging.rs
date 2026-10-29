//! 基于 tracing 的日志：控制台 + 按日期切分的文件。
//!
//! 规矩和旧版一致：日志失败绝不能影响显示动作；traceback / 完整错误只进文件。

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::Local;
use tracing_subscriber::fmt::writer::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use crate::paths;

/// 一个把日志追加到 `<数据目录>/logs/YYYY-MM-DD.log` 的 writer。
#[derive(Clone)]
pub struct FileWriter {
    dir: PathBuf,
    lock: std::sync::Arc<Mutex<()>>,
}

impl FileWriter {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            lock: std::sync::Arc::new(Mutex::new(())),
        }
    }

    fn open_today(&self) -> std::io::Result<std::fs::File> {
        let _ = std::fs::create_dir_all(&self.dir);
        let name = format!("{}.log", Local::now().format("%Y-%m-%d"));
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(name))
    }
}

pub struct FileGuard {
    writer: FileWriter,
}

impl Write for FileGuard {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // 日志写不进去就当作写成功，避免因为磁盘问题让显示动作失败。
        let _guard = self.writer.lock.lock();
        if let Ok(mut f) = self.writer.open_today() {
            let _ = f.write_all(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for FileWriter {
    type Writer = FileGuard;

    fn make_writer(&'a self) -> FileGuard {
        FileGuard {
            writer: self.clone(),
        }
    }
}

/// 初始化日志。默认 info 级别，`NIGHTDISPLAY_LOG=debug` 可调。
/// 重复调用是安全的（第二次直接返回）。
pub fn init() -> anyhow::Result<()> {
    let level = std::env::var("NIGHTDISPLAY_LOG").unwrap_or_else(|_| "info".into());
    let filter = EnvFilter::try_new(format!("{level},nd_core=info,nd_win=info,nd_cli=info"))
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(true)
        .with_writer(FileWriter::new(paths::logs_dir()));

    let console_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(false)
        .with_writer(std::io::stderr);

    // 已经初始化过（例如库被嵌入 GUI）时忽略错误。
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(console_layer)
        .with(file_layer)
        .try_init();
    Ok(())
}

/// 把完整错误链写进 `logs/traceback.log`，只在出错时调用。
pub fn record_error(context: &str, err: &anyhow::Error) {
    let mut text = format!(
        "{} {}\n{:#}\n\n",
        Local::now().format("%Y-%m-%d %H:%M:%S"),
        context,
        err
    );
    for (i, cause) in err.chain().enumerate().skip(1) {
        text.push_str(&format!("  cause[{i}]: {cause}\n"));
    }
    let dir = paths::logs_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("traceback.log"))
    {
        let _ = f.write_all(text.as_bytes());
    }
}

/// 清掉 30 天以前的日志，避免长期运行把磁盘写满。
pub fn prune_old_logs(keep_days: i64) {
    let dir = paths::logs_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let cutoff = Local::now().date_naive() - chrono::Duration::days(keep_days.max(1));
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".log") else {
            continue;
        };
        if let Ok(date) = chrono::NaiveDate::parse_from_str(stem, "%Y-%m-%d") {
            if date < cutoff {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}
