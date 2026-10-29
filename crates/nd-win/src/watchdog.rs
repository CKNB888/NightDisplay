//! 看门狗：独立进程，主程序被结束时几秒内把屏幕恢复。
//!
//! 为什么必须是独立进程：如果只是主进程里的一个线程，用户"结束任务"时
//! 它一起死，gamma 和背光就永远停在 0，屏幕看起来像坏了。
//!
//! 职责只有三件，全部不需要管理员权限：
//! 1. 监视父进程 PID；父进程消失 → 立刻从 `screen-state.json` 还原 gamma 和背光；
//! 2. 还原显示拓扑（如果夜间用过「仅虚拟屏」）；
//! 3. 把配置写成白天状态，必要时重启常驻进程，让"夜间的黑幕"真的消失。
//!
//! 看门狗自己也要能被关掉：`--parent` 对应的进程正常退出（`--action day`）时，
//! 主进程会先写 `watchdog.json` 标记"正常退出"，看门狗看到标记就自己退出。

#![cfg(windows)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;

/// 轮询间隔。
const POLL: Duration = Duration::from_millis(1000);
/// 父进程刚启动的一小段时间内容忍（避免误判）。
const GRACE: Duration = Duration::from_secs(3);

/// 看门狗的运行参数。
#[derive(Debug, Clone)]
pub struct WatchdogOptions {
    /// 要监视的进程 PID。0 表示"一直守着，直到被显式关闭"。
    pub parent: u32,
    /// 常驻进程死掉后是否要重新拉起。
    pub restart_daemon: bool,
}

impl Default for WatchdogOptions {
    fn default() -> Self {
        Self {
            parent: 0,
            restart_daemon: true,
        }
    }
}

/// 正常退出的标记文件。
fn marker_file() -> PathBuf {
    nd_core::paths::watchdog_file()
}

/// 主进程正常退出时写这个标记，看门狗据此自行退出。
pub fn write_clean_exit_marker() {
    #[derive(serde::Serialize)]
    struct Marker {
        pid: u32,
        at: chrono::DateTime<chrono::Local>,
    }
    let marker = Marker {
        pid: std::process::id(),
        at: chrono::Local::now(),
    };
    if let Ok(text) = serde_json::to_string(&marker) {
        if let Some(dir) = marker_file().parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(marker_file(), text);
    }
}

fn clean_exit_marker_exists() -> bool {
    marker_file().is_file()
}

/// 清掉标记（新的夜间开始前调用）。
pub fn clear_clean_exit_marker() {
    let _ = std::fs::remove_file(marker_file());
}

/// 进程是否还活着。
#[must_use]
pub fn process_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    if pid == 0 {
        return false;
    }
    // SAFETY: 只申请查询退出码的最小权限；句柄用完立刻关。
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &raw mut code).is_ok();
        let _ = CloseHandle(handle);
        ok && code == STILL_ACTIVE.0 as u32
    }
}

/// 看门狗主循环。`parent` 消失或出现正常退出标记时返回。
pub fn run(options: &WatchdogOptions) -> Result<()> {
    tracing::info!(
        parent = options.parent,
        restart_daemon = options.restart_daemon,
        "看门狗开始监视"
    );
    let started = Instant::now();

    loop {
        std::thread::sleep(POLL);
        if clean_exit_marker_exists() {
            tracing::info!("主程序正常退出，看门狗结束");
            return Ok(());
        }
        if options.parent != 0 && !process_alive(options.parent) {
            if started.elapsed() < GRACE {
                // 刚起来的父进程可能还在初始化，给它一点时间。
                continue;
            }
            tracing::warn!(parent = options.parent, "主程序已退出，开始恢复屏幕");
            // 先把屏幕还原：这一步无论如何都要做，不能因为"反正要重启"就跳过。
            restore_everything();
            if !options.restart_daemon {
                tracing::info!("看门狗完成恢复并退出（不重启）");
                return Ok(());
            }
            // 再尝试把它拉回来，最多两次；两次都起不来就保持还原状态退出。
            if revive_daemon(MAX_RESTARTS) {
                tracing::info!("常驻进程已重新拉起，看门狗退出");
            } else {
                tracing::error!(
                    attempts = MAX_RESTARTS,
                    "常驻进程连续重启失败，屏幕已保持还原状态，请手动启动 NightDisplay"
                );
            }
            return Ok(());
        }
        // 父进程为 0（登录任务拉起的那种）：只在出现"夜间状态 + 父进程已死"时动作，
        // 所以这里顺便检查配置有没有被留成夜间。
        if options.parent == 0 && stale_night_state() {
            tracing::warn!("发现残留的夜间状态，恢复屏幕");
            restore_everything();
            let _ = restart_daemon();
            return Ok(());
        }
    }
}

/// 配置里是不是还写着"夜间"，但已经没有进程在守着。
fn stale_night_state() -> bool {
    let Ok(cfg) = nd_core::config::Config::load() else {
        return false;
    };
    if cfg.state != nd_core::config::RunState::Night {
        return false;
    }
    // 常驻进程的锁被占用说明还有人在跑，别抢。
    !daemon_running()
}

/// 常驻进程是否在跑（靠锁文件判断，不靠 PID 杀进程）。
///
/// 能立刻拿到独占锁 = 没有别的进程持有 = 常驻进程不在。
fn daemon_running() -> bool {
    let path = nd_core::paths::daemon_lock();
    // 非阻塞探测：拿到锁说明没人持有。
    nd_core::state_lock::write_lock_timeout(&path, Duration::ZERO).is_err()
}

/// 把 gamma、背光、拓扑全部还原，并把配置写成白天。
pub fn restore_everything() {
    if let Err(e) = crate::gamma::restore() {
        tracing::warn!(error = %e, "看门狗恢复 gamma 失败");
    }
    // 背光：从磁盘快照读原始值。
    if let Some(state) = crate::screen_state::load() {
        if let Err(e) = crate::brightness::restore_from_snapshot(&state) {
            tracing::warn!(error = %e, "看门狗恢复背光失败");
        }
        if state.topology_changed {
            if let Err(e) = crate::topology::restore() {
                tracing::warn!(error = %e, "看门狗恢复显示拓扑失败");
            }
        }
        if state.power_plan.is_some() {
            if let Err(e) = crate::power_plan::restore_saved() {
                tracing::warn!(error = %e, "看门狗恢复电源计划失败");
            }
        }
    }
    // 配置写成白天，避免下次启动又以为自己在夜间。
    let _ = nd_core::config::Config::update(|c| {
        c.state = nd_core::config::RunState::Day;
        c.night_mode = None;
        c.recovery_pending = false;
        c.hold_night_until = None;
        Ok(())
    });
    crate::screen_state::clear();
    tracing::info!("看门狗已把屏幕恢复到白天状态");
}

/// 最多重启几次常驻进程。
const MAX_RESTARTS: u32 = 2;

/// 重启常驻进程并等它真的拿到单实例锁。
///
/// 返回 `true` 表示它就绪了；两次都失败说明有更深的问题（例如程序被删了、
/// 权限被改），这时屏幕已经还原，交给用户处理比无限重启更安全。
fn revive_daemon(attempts: u32) -> bool {
    for attempt in 1..=attempts {
        if let Err(e) = restart_daemon() {
            tracing::warn!(attempt, error = %e, "重启常驻进程失败");
            continue;
        }
        // 等它起来并拿到锁：每 250ms 看一眼，最多 10 秒。
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(250));
            if daemon_running() {
                tracing::info!(attempt, "常驻进程重新就绪");
                return true;
            }
        }
        tracing::warn!(attempt, "常驻进程没有在 10 秒内就绪");
    }
    false
}

/// 重新拉起常驻进程（夜间状态还在的话它会自己把黑幕搭回去）。
fn restart_daemon() -> Result<()> {
    let cfg = nd_core::config::Config::load().unwrap_or_default();
    if cfg.state != nd_core::config::RunState::Night {
        tracing::info!("当前不是夜间，不重启常驻进程");
        return Ok(());
    }
    let args = crate::sysutil::self_args(&nd_core::paths::home_dir(), &["--action", "serve"]);
    let child = crate::sysutil::spawn_detached(&args)
        .map_err(|e| nd_core::error::user(format!("无法重启常驻进程：{e}")))?;
    tracing::info!(pid = child.id(), "已重新拉起常驻进程");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_is_alive() {
        assert!(process_alive(std::process::id()));
    }

    #[test]
    fn zero_pid_is_not_alive() {
        assert!(!process_alive(0));
    }

    #[test]
    fn clean_exit_marker_round_trip() {
        crate::test_support::with_temp_home(|_| {
            clear_clean_exit_marker();
            assert!(!clean_exit_marker_exists());
            write_clean_exit_marker();
            assert!(clean_exit_marker_exists());
            clear_clean_exit_marker();
            assert!(!clean_exit_marker_exists());
        });
    }
}
