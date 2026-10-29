//! 防睡眠：夜间不让系统休眠，白天或者恢复时立刻取消。
//!
//! `SetThreadExecutionState` 带 `ES_CONTINUOUS` 是绑定线程的，所以必须由一个
//! 长驻线程持有。这里用一个专用线程持续"续订"，停止时发信号让线程自己退出 ——
//! 不需要按 PID 杀进程，也就没有旧版那种"PID 复用误杀别的进程"的风险。

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows::Win32::System::Power::{
    SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
    EXECUTION_STATE,
};

/// 当前是否需要保持唤醒。
static DESIRED: AtomicBool = AtomicBool::new(false);
/// 续订间隔（毫秒），日志和自检用。
static INTERVAL_MS: AtomicU64 = AtomicU64::new(30_000);

struct Worker {
    stop: Sender<()>,
    handle: Option<JoinHandle<()>>,
}

static WORKER: Mutex<Option<Worker>> = Mutex::new(None);

/// 开启/关闭防睡眠。开启时长只用于日志（线程会一直续订到显式关闭）。
pub fn keep_awake(on: bool, seconds: u64) -> anyhow::Result<()> {
    if !on {
        DESIRED.store(false, Ordering::SeqCst);
        // 立刻清掉本线程的标记，同时让续订线程退出。
        // SAFETY: 只清除当前线程的执行状态标记。
        unsafe {
            let _ = SetThreadExecutionState(ES_CONTINUOUS);
        }
        let mut guard = WORKER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(worker) = guard.take() {
            let _ = worker.stop.send(());
            if let Some(handle) = worker.handle {
                // 最多等 2 秒，线程本身每隔 interval 检查一次退出信号。
                let deadline = Instant::now() + Duration::from_secs(2);
                while !handle.is_finished() && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(20));
                }
            }
        }
        tracing::info!("已停止防睡眠");
        return Ok(());
    }

    DESIRED.store(true, Ordering::SeqCst);
    let mut guard = WORKER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.is_some() {
        tracing::debug!(seconds, "防睡眠已在运行，续订时长");
        return Ok(());
    }
    let (tx, rx) = mpsc::channel::<()>();
    let handle = thread::Builder::new()
        .name("nd-keepawake".into())
        .spawn(move || {
            // 先立即申请一次，避免刚进入夜间就睡下去。
            // SAFETY: 只影响本线程的电源请求，不修改系统设置。
            unsafe {
                let _ = SetThreadExecutionState(EXECUTION_STATE(
                    ES_CONTINUOUS.0 | ES_SYSTEM_REQUIRED.0 | ES_DISPLAY_REQUIRED.0,
                ));
            }
            loop {
                match rx.recv_timeout(Duration::from_millis(INTERVAL_MS.load(Ordering::SeqCst))) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if !DESIRED.load(Ordering::SeqCst) {
                            break;
                        }
                        // SAFETY: 续订本线程的电源请求。
                        unsafe {
                            let _ = SetThreadExecutionState(EXECUTION_STATE(
                                ES_CONTINUOUS.0 | ES_SYSTEM_REQUIRED.0 | ES_DISPLAY_REQUIRED.0,
                            ));
                        }
                    }
                }
            }
            // SAFETY: 线程退出前清掉标记，绝不把"永不睡眠"留给系统。
            unsafe {
                let _ = SetThreadExecutionState(ES_CONTINUOUS);
            }
            tracing::debug!("防睡眠线程退出");
        })
        .map_err(|e| anyhow::anyhow!("无法启动防睡眠线程：{e}"))?;
    *guard = Some(Worker {
        stop: tx,
        handle: Some(handle),
    });
    tracing::info!(seconds, "已开启防睡眠");
    Ok(())
}

/// 当前是否在防睡眠。
pub fn is_active() -> bool {
    DESIRED.load(Ordering::SeqCst)
}

/// 立即关屏（发显示器电源消息）。
pub fn screen_off() -> anyhow::Result<()> {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SYSCOMMAND,
    };
    const SC_MONITORPOWER: usize = 0xF170;
    const MONITOR_OFF: isize = 2;
    let mut result = 0usize;
    // SAFETY: 向所有顶层窗口广播电源消息，标准做法；超时 2 秒避免卡死。
    let ok = unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SYSCOMMAND,
            WPARAM(SC_MONITORPOWER),
            LPARAM(MONITOR_OFF),
            SMTO_ABORTIFHUNG,
            2000,
            Some(&raw mut result),
        )
    };
    let _ = HWND(std::ptr::null_mut());
    if ok.0 == 0 {
        return Err(nd_core::error::user("Windows 拒绝关闭显示器"));
    }
    Ok(())
}

/// 唤醒屏幕。
pub fn screen_on() -> anyhow::Result<()> {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SYSCOMMAND,
    };
    const SC_MONITORPOWER: usize = 0xF170;
    const MONITOR_ON: isize = -1;
    let mut result = 0usize;
    // SAFETY: 标准广播唤醒。
    unsafe {
        let _ = SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SYSCOMMAND,
            WPARAM(SC_MONITORPOWER),
            LPARAM(MONITOR_ON),
            SMTO_ABORTIFHUNG,
            2000,
            Some(&raw mut result),
        );
    }
    // SAFETY: 发送零位移鼠标移动，确保显示器被唤醒；这是注入事件，钩子会放行。
    unsafe {
        windows::Win32::UI::Input::KeyboardAndMouse::mouse_event(
            windows::Win32::UI::Input::KeyboardAndMouse::MOUSEEVENTF_MOVE,
            0,
            0,
            0,
            usize::MAX as _,
        );
    }
    Ok(())
}

/// 抑制锁屏：每 60 秒注入一次无害的按键（F15 无作用键），把空闲计时器清零。
///
/// 不做这件事的话，Windows 到了空闲时间会锁屏，登录界面会盖住黑幕 ——
/// 用户看到的是登录框而不是"关机"。安全权衡写在 README 里。
pub fn set_lock_suppression(on: bool) -> anyhow::Result<()> {
    static SUPPRESSION: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
    static STOP: Mutex<bool> = Mutex::new(false);
    static WAKE_SIGNAL: std::sync::Condvar = std::sync::Condvar::new();

    if !on {
        // 用 condvar 唤醒而不是等 60 秒的 sleep 走完 —— 否则"关闭抑制"
        // 会把 UI 卡住最多一分钟（首启向导第 4 步就会踩到）。
        {
            let mut stopped = STOP
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *stopped = true;
        }
        WAKE_SIGNAL.notify_all();
        let mut guard = SUPPRESSION
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(handle) = guard.take() {
            let _ = handle.join();
        }
        tracing::info!("已恢复系统锁屏");
        return Ok(());
    }

    let mut guard = SUPPRESSION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.is_some() {
        return Ok(());
    }
    {
        let mut stopped = STOP
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *stopped = false;
    }
    let handle = thread::Builder::new()
        .name("nd-locksuppress".into())
        .spawn(|| loop {
            poke_idle_timer();
            let stopped = STOP
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // 先看谓词再等，避免通知比 wait 先到就丢掉（会白等 60 秒）。
            if *stopped {
                break;
            }
            let (stopped, _timeout) = WAKE_SIGNAL
                .wait_timeout(stopped, Duration::from_secs(60))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *stopped {
                break;
            }
        })
        .map_err(|e| anyhow::anyhow!("无法启动锁屏抑制线程：{e}"))?;
    *guard = Some(handle);
    poke_idle_timer();
    tracing::info!("夜间已抑制系统锁屏（登录界面不会盖住黑幕）");
    Ok(())
}

/// 注入一个无副作用的按键，把系统空闲计时清零。
fn poke_idle_timer() {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY,
    };
    // F15 在普通键盘上不存在，绝大多数程序也不会响应，纯用来重置空闲计时。
    const VK_F15: VIRTUAL_KEY = VIRTUAL_KEY(0x7E);
    let inputs = [
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VK_F15,
                    wScan: 0,
                    dwFlags:
                        windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS::default(),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VK_F15,
                    wScan: 0,
                    dwFlags: KEYEVENTF_KEYUP,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
    ];
    // SAFETY: inputs 长度与传入的 cbSize 一致，结构体布局符合 API。
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

/// 当前进程是否有管理员权限。
#[must_use]
pub fn is_elevated() -> bool {
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut token = windows::Win32::Foundation::HANDLE::default();
    // SAFETY: 只查询当前进程的令牌。
    unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut size = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some((&raw mut elevation).cast()),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &raw mut size,
        )
        .is_ok();
        let _ = windows::Win32::Foundation::CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

/// 当前 Windows 版本号（build），用于判断 `WDA_EXCLUDEFROMCAPTURE` 是否可用。
#[must_use]
pub fn windows_build() -> u32 {
    // SAFETY: 纯查询，不分配。
    unsafe {
        let mut info = windows::Win32::System::SystemInformation::OSVERSIONINFOW {
            dwOSVersionInfoSize: std::mem::size_of::<
                windows::Win32::System::SystemInformation::OSVERSIONINFOW,
            >() as u32,
            ..Default::default()
        };
        if windows::Wdk::System::SystemServices::RtlGetVersion(&raw mut info).is_ok() {
            info.dwBuildNumber
        } else {
            0
        }
    }
}

/// 操作系统描述，写进状态文本。
#[must_use]
pub fn os_description() -> String {
    format!("Windows (build {})", windows_build())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_number_looks_sane() {
        // CI 上可能是 server 版，但 build 一定大于 10000。
        let build = windows_build();
        assert!(build == 0 || build > 10000, "build = {build}");
    }

    #[test]
    fn elevated_is_queryable() {
        // 只要求能查询，不要求一定是管理员。
        let _ = is_elevated();
    }
}
