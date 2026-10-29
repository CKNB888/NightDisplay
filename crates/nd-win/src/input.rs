//! 本地键鼠防触控 + 解锁组合键。
//!
//! 用 `WH_KEYBOARD_LL` / `WH_MOUSE_LL` 低级钩子，判定规则只有一条：
//!
//! * 事件带 `LLKHF_INJECTED` / `LLMHF_INJECTED` → **原样放行**。
//!   远程软件（UU、ToDesk 等）注入的操作属于这一类，必须完全不受影响。
//! * 事件是真实硬件 → **吞掉**，本机桌面收不到任何输入，所以碰屏幕既不会唤醒、
//!   也不会关掉黑幕或点开别的东西。
//!
//! 组合键在钩子里判定，按住够久就触发解锁；解锁本身也要放行（否则按下去的那两个键
//! 会一直被吞到钩子卸载）。
//!
//! 严格模式可以关掉（设置项 `strict_local_input`）：有些远程软件用的是内核级注入，
//! 事件不带 INJECTED 标记，那种情况下吞键会把远程操作也一起吞掉，用户需要能退回。

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::Mutex;

use nd_core::chord::ChordKey;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VIRTUAL_KEY, VK_0, VK_A, VK_CONTROL, VK_ESCAPE, VK_F1, VK_LWIN, VK_MENU,
    VK_RWIN, VK_SHIFT, VK_SPACE, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::HHOOK;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, SetWindowsHookExW, UnhookWindowsHookEx, KBDLLHOOKSTRUCT, LLKHF_INJECTED,
    LLMHF_INJECTED, MSLLHOOKSTRUCT, WH_KEYBOARD_LL, WH_MOUSE_LL,
};

/// 钩子是否生效。
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// 严格模式：吞掉真实硬件事件。
static STRICT: AtomicBool = AtomicBool::new(true);
/// 组合键是否按着（供钩子做放行判断）。
static CHORD_DOWN: AtomicBool = AtomicBool::new(false);
/// 组合键配置的全局副本，钩子回调不能加锁（会被系统在任意线程调用）。
static CHORD_VK: Mutex<Vec<(i32, bool)>> = Mutex::new(Vec::new());
/// 按住时长（毫秒）。
static HOLD_MS: AtomicU64 = AtomicU64::new(300);
/// 组合键按下的起始时刻（GetTickCount，毫秒）。
static CHORD_SINCE: AtomicU64 = AtomicU64::new(0);
/// 已经触发过，必须全部松手才能再次触发。
static CHORD_FIRED: AtomicBool = AtomicBool::new(false);
/// 最近一次触发的时间戳，供主循环取走。
static UNLOCK_PENDING: AtomicBool = AtomicBool::new(false);
/// 被吞掉的事件计数，用于日志和自检。
static SWALLOWED: AtomicU64 = AtomicU64::new(0);
/// 放行的注入事件计数。
static PASSED_INJECTED: AtomicU64 = AtomicU64::new(0);

static KEYBOARD_HOOK: Mutex<Option<isize>> = Mutex::new(None);
static MOUSE_HOOK: Mutex<Option<isize>> = Mutex::new(None);
/// 钩子所在的线程 id，卸载时要用它 `PostThreadMessage` 唤醒消息循环。
static HOOK_THREAD: AtomicI32 = AtomicI32::new(0);

/// 把 `ChordKey` 映射成 Windows 虚拟键码。
#[must_use]
pub fn vk_for(key: ChordKey) -> Option<u32> {
    let vk = match key {
        ChordKey::Ctrl => VK_CONTROL,
        ChordKey::Alt => VK_MENU,
        ChordKey::Shift => VK_SHIFT,
        ChordKey::Win => VK_LWIN,
        ChordKey::Char(c) if c.is_ascii_alphabetic() => {
            VIRTUAL_KEY(VK_A.0 + (c.to_ascii_uppercase() as u16 - 'A' as u16))
        }
        ChordKey::Char(c) if c.is_ascii_digit() => VIRTUAL_KEY(VK_0.0 + (c as u16 - '0' as u16)),
        ChordKey::Function(n) if (1..=12).contains(&n) => VIRTUAL_KEY(VK_F1.0 + (u16::from(n) - 1)),
        ChordKey::Space => VK_SPACE,
        ChordKey::Tab => VK_TAB,
        ChordKey::Esc => VK_ESCAPE,
        _ => return None,
    };
    Some(u32::from(vk.0))
}

/// 把一组 `ChordKey` 展开成"要监视的虚拟键码"表。
///
/// `modifier` 标记只是记录用途，判定时统一按虚拟键码比较。纯函数，
/// 所以能在并行测试里直接断言，不用去读进程级全局量。
fn chord_table(keys: &[ChordKey]) -> Vec<(i32, bool)> {
    let mut list: Vec<(i32, bool)> = keys
        .iter()
        .filter_map(|k| vk_for(*k).map(|vk| (vk as i32, k.is_modifier())))
        .collect();
    // 左/右 Ctrl、Shift、Alt、Win 都要认。
    let mut extended = Vec::new();
    for (vk, is_mod) in list.iter().copied() {
        if is_mod {
            extended.extend(alt_vk(vk).into_iter().map(|v| (v, true)));
        }
    }
    list.extend(extended);
    list
}

/// 配置组合键。
pub fn configure_chord(keys: &[ChordKey], hold_ms: u64) {
    let list = chord_table(keys);
    if let Ok(mut guard) = CHORD_VK.lock() {
        *guard = list;
    }
    HOLD_MS.store(clamp_hold(hold_ms), Ordering::SeqCst);
}

/// 按住时长的合法区间由 `nd_core::chord::HOLD_MS_RANGE` 定义，这里只做夹取。
///
/// 单独拿出来是为了能被单独测 —— `HOLD_MS` 是进程级全局量，
/// 并行跑的其它测试会同时 `configure_chord`，直接断言全局值必然随机失败。
fn clamp_hold(ms: u64) -> u64 {
    let (lo, hi) = nd_core::chord::HOLD_MS_RANGE;
    ms.clamp(lo, hi)
}

/// 同一个修饰键的另一侧虚拟键码。
fn alt_vk(vk: i32) -> Vec<i32> {
    match VIRTUAL_KEY(vk as u16) {
        VK_CONTROL => vec![0xA2, 0xA3], // VK_LCONTROL / VK_RCONTROL
        // VK_LMENU / VK_RMENU 与 VK_LSHIFT / VK_RSHIFT 都是 0xA0 / 0xA1。
        VK_MENU | VK_SHIFT => vec![0xA0, 0xA1],
        VK_LWIN => vec![i32::from(VK_RWIN.0)],
        _ => vec![],
    }
}

/// 是否处于严格模式。
pub fn strict() -> bool {
    STRICT.load(Ordering::SeqCst)
}

/// 已吞掉的事件数。
pub fn swallowed_count() -> u64 {
    SWALLOWED.load(Ordering::SeqCst)
}

/// 已放行的注入事件数（远程操作）。
pub fn injected_passed_count() -> u64 {
    PASSED_INJECTED.load(Ordering::SeqCst)
}

/// 是否有待处理的解锁。
pub fn take_unlock() -> bool {
    UNLOCK_PENDING.swap(false, Ordering::SeqCst)
}

/// 跑消息循环，直到收到 `WM_QUIT`。
///
/// 低级钩子必须安装在会派发消息的线程上，否则系统收不到回调。
/// 这个函数就在那个线程上循环，退出时自动卸钩。
pub fn run_message_loop() {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, TranslateMessage, MSG,
    };
    let mut msg = MSG::default();
    loop {
        // SAFETY: 标准消息循环；msg 是本栈上的有效结构。
        let got = unsafe { GetMessageW(&raw mut msg, None, 0, 0) };
        if got.0 <= 0 {
            break;
        }
        // SAFETY: 消息来自 GetMessageW。
        unsafe {
            let _ = TranslateMessage(&raw const msg);
            DispatchMessageW(&raw const msg);
        }
    }
    stop();
}

/// 让钩子线程的消息循环退出。
pub fn request_stop() {
    use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_QUIT};
    let tid = HOOK_THREAD.load(Ordering::SeqCst);
    if tid != 0 {
        // SAFETY: 只向本进程自己的钩子线程投递 WM_QUIT。
        unsafe {
            let _ = PostThreadMessageW(tid as u32, WM_QUIT, WPARAM(0), LPARAM(0));
        }
    }
}

/// 钩子是否已安装。
pub fn is_active() -> bool {
    ACTIVE.load(Ordering::SeqCst)
}

/// 安装钩子。必须在会跑消息循环的线程上调用；返回时钩子已生效。
pub fn start(keys: &[ChordKey], hold_ms: u64, strict_mode: bool) -> anyhow::Result<()> {
    configure_chord(keys, hold_ms);
    STRICT.store(strict_mode, Ordering::SeqCst);
    if ACTIVE.load(Ordering::SeqCst) {
        return Ok(());
    }
    HOOK_THREAD.store(
        // SAFETY: 纯查询当前线程 id。
        unsafe { windows::Win32::System::Threading::GetCurrentThreadId() } as i32,
        Ordering::SeqCst,
    );
    // SAFETY: 两个钩子都是全局低级钩子，回调函数是静态的；dwthreadid=0 表示全局。
    unsafe {
        let kb = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), None, 0)
            .map_err(|e| anyhow::anyhow!("安装键盘钩子失败：{e}"))?;
        *KEYBOARD_HOOK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(kb.0 as isize);
        match SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), None, 0) {
            Ok(mouse) => {
                *MOUSE_HOOK
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(mouse.0 as isize);
            }
            Err(e) => {
                // 鼠标钩子失败就把键盘钩子也撤掉，不留半套。
                let _ = UnhookWindowsHookEx(HHOOK(kb.0));
                *KEYBOARD_HOOK
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                return Err(anyhow::anyhow!("安装鼠标钩子失败：{e}"));
            }
        }
    }
    ACTIVE.store(true, Ordering::SeqCst);
    tracing::info!(
        strict = strict_mode,
        chord = %nd_core::chord::format_chord(keys),
        hold_ms,
        "本地键鼠防触控已启用"
    );
    Ok(())
}

/// 卸下钩子。
pub fn stop() {
    if !ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }
    // SAFETY: 只卸载本进程安装的钩子，句柄取自静态存储。
    unsafe {
        if let Some(hook) = KEYBOARD_HOOK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = UnhookWindowsHookEx(HHOOK(hook as *mut core::ffi::c_void));
        }
        if let Some(hook) = MOUSE_HOOK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = UnhookWindowsHookEx(HHOOK(hook as *mut core::ffi::c_void));
        }
    }
    CHORD_DOWN.store(false, Ordering::SeqCst);
    CHORD_SINCE.store(0, Ordering::SeqCst);
    CHORD_FIRED.store(false, Ordering::SeqCst);
    tracing::info!(
        swallowed = swallowed_count(),
        injected_passed = injected_passed_count(),
        "本地键鼠防触控已停止"
    );
}

/// 组合键当前是否都按着（用 `GetAsyncKeyState` 查全局状态）。
fn chord_down_now() -> bool {
    let list = match CHORD_VK.lock() {
        Ok(l) => l.clone(),
        Err(e) => e.into_inner().clone(),
    };
    if list.is_empty() {
        return false;
    }
    // 按虚拟键码分组：每个"用户按键"在 list 里可能对应左右两个码，任意一个按着即算按下。
    let mut required: Vec<Vec<i32>> = Vec::new();
    for (vk, is_mod) in list {
        if is_mod {
            if let Some(pos) = required.iter().position(|g| g.contains(&vk)) {
                let _ = pos;
                continue;
            }
            // 修饰键合并到同一组
            if let Some(group) = required
                .iter_mut()
                .find(|g| g.iter().any(|v| alt_vk(*v).contains(&vk)))
            {
                if !group.contains(&vk) {
                    group.push(vk);
                }
                continue;
            }
            required.push(vec![vk]);
        } else {
            required.push(vec![vk]);
        }
    }
    required.iter().all(|group| {
        group.iter().any(|vk| {
            // SAFETY: 纯查询按键状态，无副作用。
            unsafe { (GetAsyncKeyState(*vk) & 0x8000u16 as i16) != 0 }
        })
    })
}

/// 用 `GetTickCount` 取毫秒时间戳。
fn now_ms() -> u64 {
    // SAFETY: 纯查询。
    unsafe { windows::Win32::System::SystemInformation::GetTickCount64() }
}

/// 在钩子线程里更新组合键状态机。返回 true 表示刚触发解锁。
fn poll_chord() -> bool {
    let down = chord_down_now();
    if !down {
        CHORD_DOWN.store(false, Ordering::SeqCst);
        CHORD_SINCE.store(0, Ordering::SeqCst);
        CHORD_FIRED.store(false, Ordering::SeqCst);
        return false;
    }
    CHORD_DOWN.store(true, Ordering::SeqCst);
    if CHORD_FIRED.load(Ordering::SeqCst) {
        return false;
    }
    let now = now_ms();
    let since = CHORD_SINCE.load(Ordering::SeqCst);
    if since == 0 {
        CHORD_SINCE.store(now, Ordering::SeqCst);
        return false;
    }
    if now.saturating_sub(since) >= HOLD_MS.load(Ordering::SeqCst) {
        CHORD_FIRED.store(true, Ordering::SeqCst);
        UNLOCK_PENDING.store(true, Ordering::SeqCst);
        tracing::info!("检测到解锁组合键，请求恢复");
        return true;
    }
    false
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 || !ACTIVE.load(Ordering::Relaxed) {
        // SAFETY: 未处理的消息必须交给下一个钩子。
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    // SAFETY: 低级键盘钩子的 lparam 指向 KBDLLHOOKSTRUCT。
    let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
    let injected = (info.flags.0 & LLKHF_INJECTED.0) != 0;

    // 组合键状态：只在真实硬件事件上推进，避免注入事件伪造解锁。
    if !injected {
        poll_chord();
    }

    if injected {
        PASSED_INJECTED.fetch_add(1, Ordering::Relaxed);
        // SAFETY: 注入事件必须原样放行，远程操作才能生效。
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    if !STRICT.load(Ordering::Relaxed) {
        // SAFETY: 非严格模式不吞键，只做组合键检测。
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    // 组合键按键本身要放行，否则用户按下的 C/K 会一直压在系统队列里。
    let is_release = (wparam.0 & 0x8000) != 0;
    if CHORD_DOWN.load(Ordering::Relaxed) || chord_key_vk(info.vkCode) {
        let _ = is_release;
        // SAFETY: 放行组合键事件。
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    SWALLOWED.fetch_add(1, Ordering::Relaxed);
    LRESULT(1)
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 || !ACTIVE.load(Ordering::Relaxed) {
        // SAFETY: 未处理的消息交给下一个钩子。
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    // SAFETY: 低级鼠标钩子的 lparam 指向 MSLLHOOKSTRUCT。
    let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
    let injected = (info.flags & LLMHF_INJECTED) != 0;
    if injected || !STRICT.load(Ordering::Relaxed) {
        if injected {
            PASSED_INJECTED.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: 注入事件放行。
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    SWALLOWED.fetch_add(1, Ordering::Relaxed);
    LRESULT(1)
}

/// 这个虚拟键码是不是组合键的一部分。
fn chord_key_vk(vk: u32) -> bool {
    let list = match CHORD_VK.lock() {
        Ok(l) => l.clone(),
        Err(e) => e.into_inner().clone(),
    };
    list.iter()
        .any(|(v, is_mod)| *v as u32 == vk && (!*is_mod || alt_vk(*v).contains(&(vk as i32))))
}

/// 组合键按下多少毫秒后触发（供 UI 显示）。
pub fn hold_ms() -> u64 {
    HOLD_MS.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_letters_digits_and_function_keys() {
        assert_eq!(vk_for(ChordKey::Char('C')), Some(0x43));
        assert_eq!(vk_for(ChordKey::Char('k')), Some(0x4B));
        assert_eq!(vk_for(ChordKey::Char('1')), Some(0x31));
        assert_eq!(vk_for(ChordKey::Function(1)), Some(0x70));
        assert_eq!(vk_for(ChordKey::Function(12)), Some(0x7B));
        assert_eq!(vk_for(ChordKey::Ctrl), Some(0x11));
        assert_eq!(vk_for(ChordKey::Win), Some(0x5B));
    }

    #[test]
    fn configure_chord_accepts_default() {
        // C + K = 两个键，且都不带左右分身。
        let list = chord_table(&nd_core::chord::default_chord());
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|(vk, _)| *vk == 0x43));
        assert!(list.iter().any(|(vk, _)| *vk == 0x4B));
        // Ctrl+Alt+N 里的 Ctrl 和 Alt 要各展开成左右两个。
        let with_mods: Vec<ChordKey> = ["Ctrl", "Alt", "N"]
            .into_iter()
            .map(|n| ChordKey::parse(n).unwrap())
            .collect();
        let list = chord_table(&with_mods);
        assert_eq!(list.len(), 1 + 2 + 2, "N + 左右 Ctrl + 左右 Alt");
    }

    #[test]
    fn modifier_gets_both_sides() {
        let alts = alt_vk(i32::from(VK_CONTROL.0));
        assert_eq!(alts, vec![0xA2, 0xA3]);
        let alts = alt_vk(i32::from(VK_LWIN.0));
        assert_eq!(alts, vec![i32::from(VK_RWIN.0)]);
    }

    #[test]
    fn hold_ms_is_clamped() {
        let (lo, hi) = nd_core::chord::HOLD_MS_RANGE;
        assert_eq!(clamp_hold(0), lo);
        assert_eq!(clamp_hold(5), lo);
        assert_eq!(clamp_hold(lo), lo);
        assert_eq!(clamp_hold(1200), 1200);
        assert_eq!(clamp_hold(hi), hi);
        assert_eq!(clamp_hold(99_999), hi);
        // 界面上限和核心区间必须是同一个数，不能各写一份。
        assert_eq!((lo, hi), (100, 3000));
    }
}
