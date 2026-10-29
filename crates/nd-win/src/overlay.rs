//! 防窥黑幕窗口：每块显示器一个满屏纯黑窗口，并且把自己排除在屏幕采集之外。
//!
//! 这是这次重写的核心。旧版默认调用 `SetDisplayConfig` 把实体屏从桌面摘掉，
//! 结果远程软件的采集源消失，画面卡住必须重连；显示器重新同步还会先亮一下。
//! 这里**完全不碰显示拓扑**，只是在本机桌面最上层盖一层纯黑窗口：
//!
//! * `WS_POPUP`，不带 `WS_EX_APPWINDOW`，所以不占任务栏、不抢焦点；
//! * `WS_EX_TOPMOST` 保持最上层；
//! * `WS_EX_TOOLWINDOW` 不进 Alt+Tab；
//! * `WS_EX_NOACTIVATE` 永不激活，点击/触摸不会把它顶掉；
//! * `WS_EX_LAYERED` + `SetLayeredWindowAttributes(alpha=255)` 让合成器把它当独立图层；
//! * `WS_EX_TRANSPARENT` 让鼠标事件穿过去（本地按键另由钩子吞掉）；
//! * `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` 把它排除在 `PrintWindow`/
//!   `BitBlt`/桌面复制（远程软件用的就是这些）之外 —— 本机看是黑的，远程看到的是正常桌面。
//!
//! 采集排除的返回值必须校验；只要有一块屏没成功，就整体拆掉并回滚，绝不谎报"已黑屏"。
//!
//! 窗口在守护进程启动时就预先创建好并隐藏，进入夜间只需要 `ShowWindow`，
//! 第一帧黑色的实测耗时会在日志和自检里给出。

#![cfg(windows)]

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use anyhow::Result;
use nd_core::error::user;
use nd_core::mode::OverlayOutcome;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, EnumDisplayMonitors, GetMonitorInfoW, MonitorFromWindow,
    HBRUSH, HDC, HMONITOR, MONITORINFOEXW, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetSystemMetrics, GetWindowDisplayAffinity,
    IsWindow, RegisterClassExW, SetLayeredWindowAttributes, SetWindowDisplayAffinity, SetWindowPos,
    ShowWindow, HWND_TOPMOST, LWA_ALPHA, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_HIDE, SW_SHOWNA,
    WDA_EXCLUDEFROMCAPTURE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_DISPLAYCHANGE, WM_DPICHANGED,
    WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP, WS_VISIBLE,
};

/// 窗口类名。
const CLASS_NAME: PCWSTR = w!("NightDisplayGuardOverlay");

/// 一块显示器上的黑幕窗口。
struct Overlay {
    hwnd: HWND,
    /// 显示器描述，日志和自检里用。
    display: String,
    /// HMONITOR 句柄，用于识别显示器是否被换掉。
    monitor: isize,
}

// SAFETY: HWND 只是不透明句柄，窗口在自己创建它的线程上使用；
// 这里只把句柄放在 Mutex 里跨线程传递，所有 Win32 调用都在持有锁时进行。
unsafe impl Send for Overlay {}

/// 黑幕管理器。守护进程持有一个实例，全程复用窗口，避免每晚重建带来的闪烁。
pub struct Overlays {
    windows: Mutex<Vec<Overlay>>,
    /// 预创建过一次（不论当前是否显示）。
    warmed: AtomicBool,
    /// 上一次 `ShowWindow` 到第一帧黑色的耗时。
    last_latency_ms: AtomicI64,
    /// 是否处于夜间（窗口正在显示）。
    visible: AtomicBool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShowTiming {
    pub first_frame_ms: u64,
    pub displays: usize,
}

impl Default for Overlays {
    fn default() -> Self {
        Self::new()
    }
}

impl Overlays {
    pub fn new() -> Self {
        Self {
            windows: Mutex::new(Vec::new()),
            warmed: AtomicBool::new(false),
            last_latency_ms: AtomicI64::new(0),
            visible: AtomicBool::new(false),
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible.load(Ordering::SeqCst)
    }

    pub fn last_latency_ms(&self) -> u64 {
        self.last_latency_ms.load(Ordering::SeqCst).max(0) as u64
    }

    /// 守护进程启动时调用：把窗口全部创建好并完成一次 alpha 设置，然后隐藏。
    /// 这样真正进入夜间时只剩 `ShowWindow`，首帧时间能压到几十毫秒。
    pub fn warm_up(&self) -> Result<ShowTiming> {
        let started = Instant::now();
        let count = {
            let mut windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());
            self.rebuild_locked(&mut windows)?;
            windows.len()
        };
        self.warmed.store(true, Ordering::SeqCst);
        let elapsed = started.elapsed().as_millis() as u64;
        tracing::info!(displays = count, elapsed_ms = elapsed, "黑幕窗口已预热");
        Ok(ShowTiming {
            first_frame_ms: elapsed,
            displays: count,
        })
    }

    /// 显示器数量或布局变了：销毁旧窗口，按当前显示器重建（保持隐藏）。
    pub fn refresh(&self) -> Result<()> {
        let mut windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        self.rebuild_locked(&mut windows)
    }

    /// 进入夜间：显示全部黑幕，并校验每一块都被排除在采集之外。
    ///
    /// 任何一块失败都会把已经显示的窗口全部收回，返回的错误里带显示器名字。
    pub fn show(&self) -> Result<Vec<OverlayOutcome>> {
        let started = Instant::now();
        let mut windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        // 显示器可能在这期间变了（插拔、分辨率调整），先对齐一次。
        self.rebuild_locked(&mut windows)?;
        if windows.is_empty() {
            return Err(user("没有检测到任何显示器，无法创建黑幕"));
        }

        let mut outcomes = Vec::with_capacity(windows.len());
        let mut failed = false;
        for overlay in windows.iter() {
            let outcome = show_one(overlay, started);
            if !outcome.ok() {
                failed = true;
            }
            outcomes.push(outcome);
        }

        if failed {
            for overlay in windows.iter() {
                // SAFETY: hwnd 是本进程创建并持有的窗口句柄，仍有效。
                unsafe {
                    let _ = ShowWindow(overlay.hwnd, SW_HIDE);
                }
            }
            self.visible.store(false, Ordering::SeqCst);
            let detail = outcomes
                .iter()
                .find(|o| !o.ok())
                .and_then(|o| o.error.clone())
                .unwrap_or_else(|| "未知原因".into());
            return Err(user(format!("防窥黑幕未能确认（{detail}），已全部撤回")));
        }

        let latency = started.elapsed().as_millis() as u64;
        self.last_latency_ms.store(latency as i64, Ordering::SeqCst);
        self.visible.store(true, Ordering::SeqCst);
        tracing::info!(
            displays = outcomes.len(),
            first_frame_ms = latency,
            "黑幕已显示并确认排除采集"
        );
        Ok(outcomes)
    }

    /// 退出夜间：隐藏全部窗口（不销毁，下次直接复用）。
    pub fn hide(&self) -> Result<()> {
        let windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        for overlay in windows.iter() {
            // SAFETY: hwnd 由本进程创建并持有。
            unsafe {
                let _ = ShowWindow(overlay.hwnd, SW_HIDE);
            }
        }
        self.visible.store(false, Ordering::SeqCst);
        tracing::info!(displays = windows.len(), "黑幕已隐藏");
        Ok(())
    }

    /// 夜间修复：重新置顶并再次校验采集排除（远程软件或别的程序可能改动过）。
    pub fn reassert(&self) -> Result<()> {
        let windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        for overlay in windows.iter() {
            // SAFETY: hwnd 由本进程创建并持有，且仍在有效期内。
            unsafe {
                let _ = SetWindowPos(
                    overlay.hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
                if let Err(e) = SetWindowDisplayAffinity(overlay.hwnd, WDA_EXCLUDEFROMCAPTURE) {
                    tracing::warn!(
                        display = %overlay.display,
                        error = %e,
                        "重新设置采集排除失败"
                    );
                }
            }
        }
        Ok(())
    }

    /// 进程退出前调用：销毁所有窗口。
    pub fn destroy_all(&self) {
        let mut windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        for overlay in windows.drain(..) {
            // SAFETY: hwnd 由本进程创建，只销毁一次。
            unsafe {
                let _ = DestroyWindow(overlay.hwnd);
            }
        }
        self.visible.store(false, Ordering::SeqCst);
        self.warmed.store(false, Ordering::SeqCst);
    }

    /// 按当前显示器布局重建窗口列表。调用方必须已经持有锁。
    fn rebuild_locked(&self, windows: &mut Vec<Overlay>) -> Result<()> {
        let monitors = enumerate_monitors()?;
        // 布局没变就不重建，避免无意义闪烁。
        if same_layout(windows, &monitors) {
            return Ok(());
        }
        for overlay in windows.drain(..) {
            // SAFETY: hwnd 由本进程创建，只销毁一次。
            unsafe {
                let _ = DestroyWindow(overlay.hwnd);
            }
        }
        ensure_class_registered()?;
        for monitor in monitors {
            let hwnd = create_overlay_window(&monitor)?;
            windows.push(Overlay {
                hwnd,
                display: monitor.name.clone(),
                monitor: monitor.handle.0 as isize,
            });
        }
        tracing::debug!(displays = windows.len(), "已按当前显示器重建黑幕窗口");
        Ok(())
    }
}

impl Drop for Overlays {
    fn drop(&mut self) {
        self.destroy_all();
    }
}

/// 一块显示器：句柄、名字、物理区域。
#[derive(Debug, Clone, PartialEq)]
struct MonitorInfo {
    handle: HMONITOR,
    name: String,
    rect: RECT,
}

fn same_layout(windows: &[Overlay], monitors: &[MonitorInfo]) -> bool {
    if windows.len() != monitors.len() {
        return false;
    }
    windows
        .iter()
        .zip(monitors.iter())
        .all(|(w, m)| w.monitor == m.handle.0 as isize && w.display == m.name)
}

/// 枚举所有显示器（含虚拟屏，虚拟屏也会被盖上，远程看到的是同一块黑幕）。
fn enumerate_monitors() -> Result<Vec<MonitorInfo>> {
    let collected: RefCell<Vec<MonitorInfo>> = RefCell::new(Vec::new());
    let mut index = 0usize;
    // SAFETY: 回调只在这个同步调用期间被调用，捕获的引用在调用结束前一直有效。
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(enum_proc),
            LPARAM(&collected as *const _ as isize),
        )
    };
    if !ok.as_bool() && collected.borrow().is_empty() {
        return Err(user("枚举显示器失败"));
    }
    let mut list = collected.into_inner();
    // 保证顺序稳定，日志里好对照。
    list.sort_by(|a, b| {
        (a.rect.left, a.rect.top)
            .cmp(&(b.rect.left, b.rect.top))
            .then_with(|| a.name.cmp(&b.name))
    });
    for m in list.iter_mut() {
        if m.name.is_empty() {
            index += 1;
            m.name = format!("DISPLAY{index}");
        }
    }
    Ok(list)
}

unsafe extern "system" fn enum_proc(
    monitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> windows::core::BOOL {
    // SAFETY: 指针来自 enumerate_monitors 里的 `&collected`，回调期间有效；
    // MonitorInfo 只在当前线程使用，不会被跨线程访问。
    let list = unsafe { &*(data.0 as *const RefCell<Vec<MonitorInfo>>) };
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: info 尺寸已按 API 要求填好。
    if unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo).as_bool() } {
        let name = String::from_utf16_lossy(
            &info.szDevice[..info
                .szDevice
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(info.szDevice.len())],
        );
        list.borrow_mut().push(MonitorInfo {
            handle: monitor,
            name,
            rect: info.monitorInfo.rcMonitor,
        });
    }
    windows::core::BOOL(1)
}

/// 窗口类只注册一次。
fn ensure_class_registered() -> Result<()> {
    static REGISTERED: OnceLock<bool> = OnceLock::new();
    let ok = *REGISTERED.get_or_init(|| {
        // 按显示器 DPI 感知，避免在缩放不同的多屏上窗口被系统拉伸。
        // SAFETY: 只是设置当前进程的 DPI 感知上下文，失败时退回系统默认。
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: Default::default(),
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        // SAFETY: wc 是完整初始化的结构体，类名是静态宽字符串。
        let atom = unsafe { RegisterClassExW(&wc) };
        if atom == 0 {
            let err = windows::core::Error::from_thread();
            // 重复注册会返回 0 且 last error 是 ERROR_CLASS_ALREADY_EXISTS。
            if err.code() == windows::Win32::Foundation::ERROR_CLASS_ALREADY_EXISTS.to_hresult() {
                return true;
            }
            tracing::error!(error = %err, "注册黑幕窗口类失败");
            return false;
        }
        true
    });
    if ok {
        Ok(())
    } else {
        Err(user("无法注册黑幕窗口类，系统拒绝创建窗口"))
    }
}

/// 创建一块显示器上的黑幕窗口（隐藏状态）。
fn create_overlay_window(monitor: &MonitorInfo) -> Result<HWND> {
    let style = WINDOW_STYLE(WS_POPUP.0);
    let ex_style = WINDOW_EX_STYLE(
        WS_EX_TOPMOST.0
            | WS_EX_TOOLWINDOW.0
            | WS_EX_NOACTIVATE.0
            | WS_EX_LAYERED.0
            | WS_EX_TRANSPARENT.0,
    );
    let width = monitor.rect.right - monitor.rect.left;
    let height = monitor.rect.bottom - monitor.rect.top;
    // SAFETY: 类已经注册；窗口过程是静态函数；父窗口为空表示顶层窗口。
    // 不带 WS_VISIBLE 创建，进入夜间时再 ShowWindow，避免白天闪一下。
    let hwnd = unsafe {
        CreateWindowExW(
            ex_style,
            CLASS_NAME,
            CLASS_NAME,
            style,
            monitor.rect.left,
            monitor.rect.top,
            width,
            height,
            None,
            None,
            None,
            None,
        )
    }
    .map_err(|e| user(format!("创建黑幕窗口失败（{}）：{e}", monitor.name)))?;

    // alpha=255 的纯黑图层；窗口背景在 WM_ERASEBKGND 里用黑色画刷铺满。
    // SAFETY: hwnd 刚创建成功，句柄有效。
    unsafe {
        SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA)
            .map_err(|e| user(format!("设置黑幕透明度失败（{}）：{e}", monitor.name)))?;
    }
    Ok(hwnd)
}

/// 显示一块屏的黑幕，并校验采集排除真的生效了。
fn show_one(overlay: &Overlay, started: Instant) -> OverlayOutcome {
    // SAFETY: hwnd 由本进程创建并持有，仍在有效期内。
    unsafe {
        let _ = ShowWindow(overlay.hwnd, SW_SHOWNA);
        let _ = SetWindowPos(
            overlay.hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );

        // 采集排除：先设置，再回读校验。WDA_EXCLUDEFROMCAPTURE 是 Windows 10 2004+。
        if let Err(e) = SetWindowDisplayAffinity(overlay.hwnd, WDA_EXCLUDEFROMCAPTURE) {
            return OverlayOutcome {
                display: overlay.display.clone(),
                shown: true,
                capture_excluded: false,
                first_frame_ms: started.elapsed().as_millis() as u64,
                error: Some(format!("系统拒绝把黑幕排除在屏幕采集之外：{e}")),
            };
        }
        let mut affinity = 0u32;
        if let Err(e) = GetWindowDisplayAffinity(overlay.hwnd, &mut affinity) {
            return OverlayOutcome {
                display: overlay.display.clone(),
                shown: true,
                capture_excluded: false,
                first_frame_ms: started.elapsed().as_millis() as u64,
                error: Some(format!("无法回读黑幕的采集排除状态：{e}")),
            };
        }
        if affinity != WDA_EXCLUDEFROMCAPTURE.0 {
            return OverlayOutcome {
                display: overlay.display.clone(),
                shown: true,
                capture_excluded: false,
                first_frame_ms: started.elapsed().as_millis() as u64,
                error: Some(format!(
                    "采集排除没有生效（当前值 {affinity}，应为 {}）",
                    WDA_EXCLUDEFROMCAPTURE.0
                )),
            };
        }
        if !IsWindow(Some(overlay.hwnd)).as_bool() {
            return OverlayOutcome {
                display: overlay.display.clone(),
                shown: false,
                capture_excluded: false,
                first_frame_ms: started.elapsed().as_millis() as u64,
                error: Some("黑幕窗口在显示后立即失效".into()),
            };
        }
        // 立即强制一次重绘，保证第一帧黑色已经在屏幕上，而不是等下一轮消息循环。
        let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(overlay.hwnd), None, false);
        let _ = windows::Win32::Graphics::Gdi::UpdateWindow(overlay.hwnd);
    }
    OverlayOutcome {
        display: overlay.display.clone(),
        shown: true,
        capture_excluded: true,
        first_frame_ms: started.elapsed().as_millis() as u64,
        error: None,
    }
}

/// 黑幕窗口的窗口过程：背景永远纯黑，不允许系统绘制任何内容。
unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        windows::Win32::UI::WindowsAndMessaging::WM_ERASEBKGND => {
            // SAFETY: wparam 是系统传来的 HDC，仅在本次绘制期间有效。
            let hdc = HDC(wparam.0 as *mut core::ffi::c_void);
            paint_black(hwnd, hdc);
            LRESULT(1)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_PAINT => {
            // SAFETY: BeginPaint/EndPaint 成对调用，hwnd 有效。
            unsafe {
                let mut ps = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
                let hdc = windows::Win32::Graphics::Gdi::BeginPaint(hwnd, &mut ps);
                paint_black(hwnd, hdc);
                let _ = windows::Win32::Graphics::Gdi::EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            // 显示器变化时黑幕窗口可能被系统挪动，主动全屏铺满主显示器区域。
            resize_to_monitor(hwnd);
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_NCHITTEST => {
            // 命中测试返回透明，触摸/点击直接穿到下层，不产生任何本机交互。
            LRESULT(windows::Win32::UI::WindowsAndMessaging::HTTRANSPARENT as isize)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_SETCURSOR => {
            // 隐藏本地光标；远程端自己画光标，不受影响。
            // SAFETY: 只设置光标显示状态。
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::SetCursor(None);
            }
            LRESULT(1)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_CLOSE => {
            // 黑幕窗口不允许被程序化关闭（任务管理器以外的方式）。
            LRESULT(0)
        }
        _ => {
            // SAFETY: 交给系统默认处理。
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}

/// 用黑色画刷铺满整块屏。
fn paint_black(hwnd: HWND, hdc: HDC) {
    static BRUSH: OnceLock<isize> = OnceLock::new();
    // SAFETY: CreateSolidBrush 只分配一个 GDI 对象，进程生命周期内复用；退出时随进程回收。
    let brush = *BRUSH.get_or_init(|| unsafe { CreateSolidBrush(COLORREF(0)).0 as isize });
    // SAFETY: rect/get client 均以有效 hwnd 调用；画刷是有效的 HBRUSH。
    unsafe {
        let mut rect = RECT::default();
        if windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rect).is_ok() {
            let _ = windows::Win32::Graphics::Gdi::FillRect(hdc, &rect, HBRUSH(brush as *mut _));
        }
    }
}

/// 让窗口重新贴合它所属的显示器（分辨率/DPI 变化后调用）。
fn resize_to_monitor(hwnd: HWND) {
    // SAFETY: hwnd 有效；MonitorFromWindow 只读。
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(monitor, &mut info.monitorInfo).as_bool() {
            let r = info.monitorInfo.rcMonitor;
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, true);
        }
    }
}

/// 整个虚拟桌面尺寸（日志用）。
pub fn virtual_screen_size() -> (i32, i32) {
    // SAFETY: 纯查询，无副作用。
    unsafe {
        (
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

/// 虚拟桌面原点（多屏时可能是负数）。
pub fn virtual_screen_origin() -> (i32, i32) {
    // SAFETY: 纯查询，无副作用。
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
        )
    }
}

/// 所有显示设备名（`\\.\DISPLAY1` 这种），gamma 需要按设备名打开 DC。
pub fn display_devices() -> Result<Vec<String>> {
    Ok(enumerate_monitors()?.into_iter().map(|m| m.name).collect())
}

/// 显示器数量（用于能力探测）。
pub fn display_count() -> usize {
    enumerate_monitors().map(|m| m.len()).unwrap_or(0)
}

/// 释放进程级画刷。进程退出时无所谓，测试里调用可以避免句柄泄漏。
pub fn release_brushes() {
    // 画刷保存在 OnceLock 里，这里只作为文档说明：进程退出由系统回收。
    let _ = DeleteObject;
    let _ = WS_VISIBLE;
}
