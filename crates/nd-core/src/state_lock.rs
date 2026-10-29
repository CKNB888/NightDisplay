//! 跨进程文件锁。多个进程（常驻、CLI、看门狗）写同一个配置时用它串行。
//!
//! 用 `fs4`：Windows 上是 `LockFileEx`，锁跟着文件句柄走，句柄关掉锁自动释放，
//! 进程被杀也不会留下死锁。这里没有 unsafe。

use std::fs::{File, OpenOptions};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::error::NdError;

/// 持锁期间必须让文件句柄活着。
pub struct FileLock {
    file: File,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = fs4::FileExt::unlock(&self.file);
    }
}

fn open(path: &Path) -> anyhow::Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| NdError::Io {
            path: parent.display().to_string(),
            source: e,
        })?;
    }
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| {
            NdError::Io {
                path: path.display().to_string(),
                source: e,
            }
            .into()
        })
}

/// 共享锁（只读配置）。
pub fn read_lock(path: impl AsRef<Path>) -> anyhow::Result<FileLock> {
    let file = open(path.as_ref())?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match file.try_lock_shared() {
            Ok(()) => return Ok(FileLock { file }),
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(15));
            }
            Err(_) => return Err(NdError::Busy.into()),
        }
    }
}

/// 独占锁（改配置、切换显示）。默认最多等 60 秒。
pub fn write_lock(path: impl AsRef<Path>) -> anyhow::Result<FileLock> {
    write_lock_timeout(path, Duration::from_secs(60))
}

/// 独占锁，等待时间可指定；传 `Duration::ZERO` 表示非阻塞探测。
pub fn write_lock_timeout(path: impl AsRef<Path>, timeout: Duration) -> anyhow::Result<FileLock> {
    let file = open(path.as_ref())?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(()) = file.try_lock() {
            return Ok(FileLock { file });
        }
        if Instant::now() >= deadline {
            return Err(NdError::Busy.into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_exclusive_lock_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.lock");
        let _first = write_lock_timeout(&p, Duration::ZERO).unwrap();
        let second = write_lock_timeout(&p, Duration::from_millis(40));
        assert!(second.is_err(), "同一文件不能同时拿到两把独占锁");
    }

    #[test]
    fn lock_releases_after_drop() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("y.lock");
        {
            let _held = write_lock_timeout(&p, Duration::ZERO).unwrap();
        }
        assert!(write_lock_timeout(&p, Duration::from_millis(50)).is_ok());
    }
}
