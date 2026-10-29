//! 数据目录与文件位置。
//!
//! 与旧版 Python 版保持同一位置，升级后计划、日志和备份都还在：
//! * Windows：`%LOCALAPPDATA%\NightDisplay`
//! * 环境变量 `NIGHTDISPLAY_HOME` 优先（测试和便携模式用）

use std::path::{Path, PathBuf};

#[cfg_attr(not(windows), allow(unused_imports))]
use crate::APP;

/// 数据根目录。不创建目录，纯计算。
pub fn home_dir() -> PathBuf {
    if let Some(custom) = std::env::var_os("NIGHTDISPLAY_HOME") {
        let p = PathBuf::from(custom);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    #[cfg(windows)]
    {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("."));
        base.join(APP)
    }
    #[cfg(not(windows))]
    {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("."));
        base.join(".local").join("state").join("nightdisplay")
    }
}

pub fn config_file() -> PathBuf {
    home_dir().join("config.json")
}

pub fn logs_dir() -> PathBuf {
    home_dir().join("logs")
}

pub fn backups_dir() -> PathBuf {
    home_dir().join("backups")
}

pub fn selfcheck_file() -> PathBuf {
    home_dir().join("selfcheck.json")
}

pub fn status_file() -> PathBuf {
    home_dir().join("status.txt")
}

/// IPC 端点信息（端口 + 随机令牌）。
pub fn ipc_file() -> PathBuf {
    home_dir().join("ipc.json")
}

/// 全局动作锁，保证同一时间只有一个进程在改显示状态。
pub fn action_lock() -> PathBuf {
    home_dir().join("actions.lock")
}

/// 常驻进程的单实例锁。
pub fn daemon_lock() -> PathBuf {
    home_dir().join("daemon.lock")
}

/// 组合键解锁后的"本段剩余时间不再黑屏"标记。
pub fn unlock_file() -> PathBuf {
    home_dir().join("unlock.json")
}

/// 夜间期间保存的原始屏幕状态（gamma、背光、拓扑备份）。
pub fn screen_state_file() -> PathBuf {
    home_dir().join("screen-state.json")
}

/// 看门狗记录父进程状态的握手文件。
pub fn watchdog_file() -> PathBuf {
    home_dir().join("watchdog.json")
}

/// 确保数据目录存在，返回根目录。
pub fn ensure_home() -> std::io::Result<PathBuf> {
    let home = home_dir();
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(logs_dir())?;
    std::fs::create_dir_all(backups_dir())?;
    Ok(home)
}

/// 把用户给的 `--home` 解析成绝对路径。
pub fn absolute(p: &str) -> PathBuf {
    let path = Path::new(p);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_lives_under_home() {
        let home = home_dir();
        assert!(config_file().starts_with(&home));
        assert!(logs_dir().ends_with("logs"));
        assert!(ipc_file().ends_with("ipc.json"));
    }
}
