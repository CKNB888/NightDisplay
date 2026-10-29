//! 一个专用的 UI 线程，所有窗口和低级钩子都归它管。
//!
//! 为什么必须有这个线程：
//! * 黑幕窗口的窗口过程要在有消息循环的线程上跑，否则 `WM_PAINT` 永远不来，
//!   屏幕上就是不刷新的白块；
//! * 低级键鼠钩子（`WH_KEYBOARD_LL`）只在**安装它的那个线程**派发消息时回调，
//!   没有消息循环的线程装了也收不到。
//!
//! 调用方（`serve` 守护进程、CLI 一次性命令）通过 [`UiHandle::call`] 把活丢进来，
//! 同步等结果。这样"显示黑幕"这件事的所有 Win32 调用都发生在同一个线程上，
//! 不会出现跨线程操作窗口的隐患。

#![cfg(windows)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use nd_core::chord::ChordKey;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, PostThreadMessageW, SetTimer, TranslateMessage, MSG, WM_TIMER,
};

use crate::input;
use crate::overlay::Overlays;

/// 私有消息：有新任务。
const WM_JOB: u32 = 0x8000 + 1;
/// 定时器间隔（毫秒）。用于轮询解锁组合键。
const TICK_MS: u32 = 200;
/// 定时器 id。
const TIMER_ID: usize = 0x4E44;

/// 一次任务：在 UI 线程上执行，结果回传给调用方。
type Job = Box<dyn FnOnce(&mut UiState) + Send>;

/// UI 线程持有的状态。
pub struct UiState {
    pub overlays: Overlays,
    /// 钩子当前是否按我们记录的配置装着。
    input_on: bool,
    /// 定时轮询到的解锁次数。
    unlocks: u32,
}

impl UiState {
    pub fn input_active(&self) -> bool {
        self.input_on && input::is_active()
    }
}

/// 解锁通知回调（由平台层注入，用来触发"组合键解锁"）。
type UnlockHook = Arc<dyn Fn() + Send + Sync>;

struct Shared {
    rx: Mutex<mpsc::Receiver<Job>>,
    thread_id: AtomicU32,
    unlocks: Arc<AtomicU32>,
    on_unlock: Mutex<Option<UnlockHook>>,
}

/// 对 UI 线程的句柄。可以克隆，跨线程使用。
#[derive(Clone)]
pub struct UiHandle {
    shared: Arc<Shared>,
    tx: Sender<Job>,
}

/// 一次 `call` 的结果。任务里返回 `Result<T>` 时用它。
type Reply<T> = mpsc::Sender<T>;

impl UiHandle {
    /// 起 UI 线程并预热黑幕窗口。返回时窗口已经建好，只等显示。
    pub fn start() -> Result<Self> {
        let (tx, rx) = mpsc::channel::<Job>();
        let shared = Arc::new(Shared {
            rx: Mutex::new(rx),
            thread_id: AtomicU32::new(0),
            unlocks: Arc::new(AtomicU32::new(0)),
            on_unlock: Mutex::new(None),
        });
        let thread_shared = Arc::clone(&shared);
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();

        std::thread::Builder::new()
            .name("nd-ui".into())
            .spawn(move || ui_thread(&thread_shared, &ready_tx))
            .map_err(|e| nd_core::error::user(format!("无法启动界面线程：{e}")))?;

        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(Self { shared, tx }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(nd_core::error::user("界面线程启动超时")),
        }
    }

    /// 在 UI 线程上跑一段代码，等它返回。
    pub fn call<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut UiState) -> Result<T> + Send + 'static,
    {
        let (reply_tx, reply_rx) = mpsc::channel::<Result<T>>();
        let job: Job = Box::new(move |state| {
            let _ = reply_tx.send(f(state));
        });
        self.tx
            .send(job)
            .map_err(|_| nd_core::error::user("界面线程已经退出"))?;
        let tid = self.shared.thread_id.load(Ordering::SeqCst);
        if tid != 0 {
            // SAFETY: 只向本进程自己的界面线程投递唤醒消息。
            unsafe {
                let _ = PostThreadMessageW(tid, WM_JOB, WPARAM(0), LPARAM(0));
            }
        }
        reply_rx
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| nd_core::error::user("界面线程没有在 30 秒内完成操作"))?
    }

    /// 设置解锁回调。
    pub fn set_unlock_hook<F>(&self, f: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        *self
            .shared
            .on_unlock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(f));
    }

    /// 到目前为止检测到的解锁次数。
    #[must_use]
    pub fn unlock_count(&self) -> u32 {
        self.shared.unlocks.load(Ordering::SeqCst)
    }

    /// 让界面线程退出（并卸下钩子、销毁窗口）。
    pub fn shutdown(&self) {
        let tid = self.shared.thread_id.load(Ordering::SeqCst);
        if tid != 0 {
            // SAFETY: 只向本进程自己的界面线程投递退出消息。
            unsafe {
                let _ = PostThreadMessageW(
                    tid,
                    windows::Win32::UI::WindowsAndMessaging::WM_QUIT,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
    }
}

/// UI 线程主体。
fn ui_thread(shared: &Shared, ready: &Reply<Result<()>>) {
    // 记下线程 id，供 PostThreadMessageW 唤醒。
    // SAFETY: 纯查询当前线程 id。
    let tid = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
    shared.thread_id.store(tid, Ordering::SeqCst);
    let unlocks = Arc::clone(&shared.unlocks);

    let mut state = UiState {
        overlays: Overlays::new(),
        input_on: false,
        unlocks: 0,
    };

    // 预热：把每块屏的黑幕窗口先建好隐藏起来，进入夜间时只剩 ShowWindow。
    match state.overlays.warm_up() {
        Ok(timing) => {
            tracing::info!(
                displays = timing.displays,
                elapsed_ms = timing.first_frame_ms,
                "黑幕窗口预热完成"
            );
            let _ = ready.send(Ok(()));
        }
        Err(e) => {
            let message = crate::error::user_text(&e, "黑幕窗口预热失败");
            tracing::error!(error = %crate::error::chain_text(&e), "黑幕窗口预热失败");
            let _ = ready.send(Err(nd_core::error::user(message)));
            return;
        }
    }

    // 200ms 一次的定时器：轮询组合键解锁，同时作为消息循环的心跳。
    // SAFETY: 定时器属于本线程，没有窗口句柄（hwnd 为空）时消息投递到线程队列。
    let timer = unsafe { SetTimer(None, TIMER_ID, TICK_MS, None) };
    if timer == 0 {
        tracing::warn!("无法创建界面线程定时器，组合键解锁将依赖钩子自己的判定");
    }

    let mut last_tick = Instant::now();
    let mut msg = MSG::default();
    loop {
        // SAFETY: 标准消息循环。
        let got = unsafe { GetMessageW(&raw mut msg, None, 0, 0) };
        if got.0 <= 0 {
            break;
        }
        if msg.message == WM_JOB {
            // 有新任务：把队列里的全部执行掉。
            loop {
                let job = {
                    let rx = shared.rx.lock().unwrap_or_else(|e| e.into_inner());
                    rx.try_recv()
                };
                match job {
                    Ok(job) => job(&mut state),
                    Err(_) => break,
                }
            }
            continue;
        }
        if msg.message == WM_TIMER {
            if msg.wParam.0 == TIMER_ID {
                tick(&mut state, shared, &unlocks, &mut last_tick);
            }
            continue;
        }
        // SAFETY: 消息来自 GetMessageW，转发给对应的窗口过程。
        unsafe {
            let _ = TranslateMessage(&raw const msg);
            DispatchMessageW(&raw const msg);
        }
    }

    // 退出前把一切还原干净。
    if state.input_on {
        input::stop();
    }
    let _ = state.overlays.hide();
    state.overlays.destroy_all();
    tracing::info!("界面线程已退出");
}

/// 定时心跳：检查解锁组合键。
fn tick(state: &mut UiState, shared: &Shared, unlocks: &Arc<AtomicU32>, last_tick: &mut Instant) {
    let now = Instant::now();
    if now.duration_since(*last_tick) < Duration::from_millis(u64::from(TICK_MS) / 2) {
        return;
    }
    *last_tick = now;

    if !state.input_on {
        // 钩子没开时也清一下，避免残留标志在下一段夜间误触发。
        let _ = input::take_unlock();
        return;
    }
    if input::take_unlock() {
        state.unlocks = state.unlocks.wrapping_add(1);
        unlocks.store(state.unlocks, Ordering::SeqCst);
        tracing::info!("组合键解锁已触发");
        if let Some(hook) = shared
            .on_unlock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            hook();
        }
    }
}

/// 在 UI 线程上安装/卸下键鼠钩子。由 `UiHandle::call` 调用。
pub fn apply_input(
    state: &mut UiState,
    on: bool,
    chord: &[ChordKey],
    hold_ms: u64,
    strict: bool,
) -> Result<()> {
    if on {
        state.input_on = true;
        input::start(chord, hold_ms, strict)?;
    } else {
        state.input_on = false;
        input::stop();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_constant_does_not_collide_with_window_messages() {
        // WM_APP 区间是 0x8000..0xBFFF，不会和系统消息冲突。
        assert!((0x8000..0xC000).contains(&WM_JOB));
        assert_ne!(WM_JOB, WM_TIMER);
    }
}
