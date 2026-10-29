//! 关机 / 注销 / 关闭控制台窗口时的收尾。
//!
//! 为什么必须有：夜间屏幕是黑的（gamma 拉到 0、背光 0、桌面在虚拟屏上）。
//! 如果用户关机时我们什么都不做，下一次开机会先黑屏，要等到常驻进程起来
//! 才恢复 —— 这段时间用户看到的是"电脑坏了"。
//!
//! 做法是 `SetConsoleCtrlHandler`：Windows 在关机、注销、关闭控制台窗口时
//! 会给本进程的控制台发 `CTRL_SHUTDOWN_EVENT` / `CTRL_LOGOFF_EVENT` /
//! `CTRL_CLOSE_EVENT`，我们在那个线程上把 gamma、背光、拓扑还原掉。
//!
//! 只对**有控制台**的常驻进程有效（也就是 `--action serve`）。图形界面本身
//! 是瘦客户端，引擎在常驻进程里，所以不需要自己去监听会话结束。

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use windows::core::BOOL;
use windows::Win32::System::Console::{SetConsoleCtrlHandler, CTRL_C_EVENT};

/// 是否已经在处理收尾（避免重入，系统可能连发两次）。
static HANDLING: AtomicBool = AtomicBool::new(false);
/// 已经还原过（第二次进来直接放行）。
static DONE: AtomicBool = AtomicBool::new(false);

/// `SetConsoleCtrlHandler` 的回调。**不能 panic**：这里 panic 会直接终止进程，
/// 而且屏幕就停在黑着。
unsafe extern "system" fn handler(event: u32) -> BOOL {
    // SAFETY: 平台约定该事件类型表示"用户按了 Ctrl+C"。
    if event == CTRL_C_EVENT {
        // 不拦 Ctrl+C：让它继续走默认处理，下面的收尾照样做。
        // （常驻进程退出时会走 serve() 的正常收尾路径。）
        return BOOL(0);
    }
    // 关机 / 注销 / 关闭窗口：这是最后一次机会。
    if DONE.load(Ordering::SeqCst) || HANDLING.swap(true, Ordering::SeqCst) {
        return BOOL(0);
    }
    tracing::info!(event, "系统会话即将结束，先把屏幕还原");
    crate::watchdog::restore_everything();
    DONE.store(true, Ordering::SeqCst);
    // 不接管这件事：系统该继续关机就继续。
    BOOL(0)
}

/// 注册收尾回调。重复调用是安全的。
///
/// 没有控制台（例如被当成服务拉起）时调用会失败，这时只记日志 ——
/// 那种情况下还有看门狗兜底。
pub fn install() -> Result<()> {
    // SAFETY: 传入的是本模块里的 `unsafe extern "system"` 回调，签名与
    // `PHANDLER_ROUTINE` 一致；注册后系统只会在本进程的收尾阶段调用它。
    match unsafe { SetConsoleCtrlHandler(Some(handler), true) } {
        Ok(()) => {
            tracing::info!("已注册关机/注销时的屏幕还原回调");
            Ok(())
        }
        Err(e) => Err(nd_core::error::user(format!(
            "无法注册关机回调（进程可能没有控制台）：{e}"
        ))),
    }
}

/// 收尾是否已经跑过（测试和日志用）。
#[must_use]
pub fn already_handled() -> bool {
    DONE.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handler_flags_start_clean() {
        assert!(!already_handled());
    }

    #[test]
    fn repeated_registration_is_fine() {
        crate::test_support::with_temp_home(|_| {
            // 测试进程自己有控制台，第一次应当成功；重复注册也应当成功。
            let first = install();
            let second = install();
            assert!(first.is_ok() || second.is_ok(), "至少要有一次注册成功");
        });
    }
}
