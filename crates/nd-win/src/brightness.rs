//! DDC/CI 背光控制：把每块面板的亮度（VCP 0x10）调到 0。
//!
//! 一体机如果不支持 VCP 0x10（很常见），这里**跳过并记日志**，
//! 不算模式失败 —— 黑幕本身已经把画面变成纯黑了，只是背光还亮着。
//!
//! 原始亮度值落盘，看门狗可以在主进程被结束后恢复。

#![cfg(windows)]

use anyhow::Result;
use nd_core::mode::LayerStatus;
use windows::Win32::Devices::Display::{
    DestroyPhysicalMonitors, GetPhysicalMonitorsFromHMONITOR, GetVCPFeatureAndVCPFeatureReply,
    SetVCPFeature, PHYSICAL_MONITOR,
};
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR};

/// 亮度控制用的 VCP 代码。
const VCP_BRIGHTNESS: u8 = 0x10;

/// 受控的一块面板。
struct Panel {
    handle: windows::Win32::Foundation::HANDLE,
    description: String,
    original: u32,
}

/// 一次夜间会话里打开的物理显示器句柄集合。
pub struct Control {
    panels: Vec<Panel>,
    unsupported: Vec<String>,
}

// SAFETY: HANDLE 是不透明值，只在持有 &mut self 时调用 DDC 接口；
// 这里把 Control 放进引擎结构里跨线程持有，不做并发访问。
unsafe impl Send for Panel {}

impl Control {
    /// 打开所有面板并读取原始亮度。读不到的面板记进 `unsupported`。
    pub fn open() -> Result<Self> {
        let mut panels = Vec::new();
        let mut unsupported = Vec::new();
        let monitors = enumerate()?;
        for (index, hmonitor) in monitors.into_iter().enumerate() {
            let mut array = [PHYSICAL_MONITOR::default(); 4];
            // SAFETY: array 长度与传入的切片长度一致，API 只会写入不超过该长度。
            let ok = unsafe { GetPhysicalMonitorsFromHMONITOR(hmonitor, &mut array) };
            if ok.is_err() {
                unsupported.push(format!("DISPLAY{}（无法打开物理显示器）", index + 1));
                continue;
            }
            for monitor in array.iter() {
                // PHYSICAL_MONITOR 是 packed 结构，按值取字段（不能取引用）。
                let handle = monitor.hPhysicalMonitor;
                let raw_name = monitor.szPhysicalMonitorDescription;
                if handle.is_invalid() {
                    continue;
                }
                let description = String::from_utf16_lossy(
                    &raw_name[..raw_name
                        .iter()
                        .position(|c| *c == 0)
                        .unwrap_or(raw_name.len())],
                );
                let mut current = 0u32;
                let mut maximum = 0u32;
                // SAFETY: 句柄来自刚打开的物理显示器数组。
                let read = unsafe {
                    GetVCPFeatureAndVCPFeatureReply(
                        handle,
                        VCP_BRIGHTNESS,
                        None,
                        &mut current,
                        Some(&mut maximum),
                    )
                };
                if read == 0 && maximum > 0 {
                    panels.push(Panel {
                        handle,
                        description,
                        original: current,
                    });
                } else {
                    unsupported.push(if description.is_empty() {
                        format!("DISPLAY{}（不支持 DDC/CI 亮度）", index + 1)
                    } else {
                        format!("{description}（不支持 DDC/CI 亮度）")
                    });
                    // 不支持的面板也要销毁，否则句柄泄漏。
                    let one = PHYSICAL_MONITOR {
                        hPhysicalMonitor: handle,
                        szPhysicalMonitorDescription: [0; 128],
                    };
                    // SAFETY: 单个元素的切片对应同一个句柄。
                    let _ = unsafe { DestroyPhysicalMonitors(std::slice::from_ref(&one)) };
                }
            }
        }
        Ok(Self {
            panels,
            unsupported,
        })
    }

    /// 把所有面板背光调到 0。返回结果和"不支持"的显示器名单。
    pub fn to_zero(&self) -> (LayerStatus, Vec<String>) {
        if self.panels.is_empty() {
            let reason = if self.unsupported.is_empty() {
                "没有找到可调背光的面板".to_string()
            } else {
                self.unsupported.join("、")
            };
            return (
                LayerStatus::Unsupported { reason },
                self.unsupported.clone(),
            );
        }
        let mut applied = 0usize;
        let mut failures = Vec::new();
        for panel in &self.panels {
            // SAFETY: 句柄在 Control 存活期间有效。
            let ok = unsafe { SetVCPFeature(panel.handle, VCP_BRIGHTNESS, 0) };
            if ok != 0 {
                applied += 1;
            } else {
                failures.push(panel.description.clone());
            }
        }
        let status = if applied == 0 {
            LayerStatus::Failed {
                reason: format!("面板拒绝把背光设为 0：{}", failures.join("、")),
            }
        } else {
            if !failures.is_empty() {
                tracing::warn!(panels = %failures.join("、"), "部分面板背光未能设为 0");
            }
            LayerStatus::Applied
        };
        if !self.unsupported.is_empty() {
            tracing::info!(
                panels = %self.unsupported.join("、"),
                "这些显示器不支持 DDC/CI 调光，背光保持原样（画面仍是纯黑）"
            );
        }
        (status, self.unsupported.clone())
    }

    /// 还原原始亮度（以及背光到 0 之前用户设置的值）。
    pub fn restore(&self) -> Result<()> {
        for panel in &self.panels {
            // SAFETY: 句柄在 Control 存活期间有效。
            let ok = unsafe { SetVCPFeature(panel.handle, VCP_BRIGHTNESS, panel.original) };
            if ok != 0 {
                tracing::info!(panel = %panel.description, value = panel.original, "背光已恢复");
            } else {
                tracing::warn!(panel = %panel.description, "背光恢复被拒绝，可能已断电");
            }
        }
        Ok(())
    }

    /// 原始亮度，供落盘备份。
    pub fn snapshot(&self) -> Vec<(String, u32)> {
        self.panels
            .iter()
            .map(|p| (p.description.clone(), p.original))
            .collect()
    }

    pub fn supported_count(&self) -> usize {
        self.panels.len()
    }

    pub fn unsupported(&self) -> &[String] {
        &self.unsupported
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        // 先还原，再销毁句柄；两步都不能抛错。
        let _ = self.restore();
        for panel in &self.panels {
            let one = PHYSICAL_MONITOR {
                hPhysicalMonitor: panel.handle,
                szPhysicalMonitorDescription: [0; 128],
            };
            // SAFETY: 每个句柄只销毁一次，切片长度为 1 对应该句柄。
            let _ = unsafe { DestroyPhysicalMonitors(std::slice::from_ref(&one)) };
        }
    }
}

/// 枚举所有 HMONITOR。
fn enumerate() -> Result<Vec<HMONITOR>> {
    use std::cell::RefCell;

    let list: RefCell<Vec<HMONITOR>> = RefCell::new(Vec::new());
    // SAFETY: 回调只在本次同步调用期间执行，捕获的引用一直有效。
    let _ = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect),
            LPARAM(&list as *const _ as isize),
        )
    };
    Ok(list.into_inner())
}

unsafe extern "system" fn collect(
    monitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> windows::core::BOOL {
    // SAFETY: 指针来自 enumerate() 的同步调用栈上的引用。
    let list = unsafe { &*(data.0 as *const std::cell::RefCell<Vec<HMONITOR>>) };
    list.borrow_mut().push(monitor);
    windows::core::BOOL(1)
}

/// 从磁盘快照恢复背光（看门狗用：它拿不到原来的 `Control`）。
///
/// 按显示器描述匹配；匹配不到就按顺序对位，宁可改错也先别把屏幕留在全黑。
pub fn restore_from_snapshot(state: &crate::screen_state::ScreenState) -> Result<()> {
    let wanted: Vec<&crate::screen_state::DisplayState> = state
        .displays
        .iter()
        .filter(|d| d.brightness.is_some())
        .collect();
    if wanted.is_empty() {
        return Ok(());
    }
    let control = Control::open()?;
    let handles = &control.panels;
    for (index, want) in wanted.iter().enumerate() {
        let value = want.brightness.unwrap_or(0);
        let target = handles
            .iter()
            .find(|p| !want.description.is_empty() && p.description == want.description)
            .or_else(|| handles.get(index));
        let Some(panel) = target else { continue };
        // SAFETY: 句柄在 control 存活期间有效。
        let ok = unsafe { SetVCPFeature(panel.handle, VCP_BRIGHTNESS, value) };
        if ok == 0 {
            tracing::warn!(panel = %panel.description, value, "看门狗恢复背光被拒绝");
        } else {
            tracing::info!(panel = %panel.description, value, "看门狗已恢复背光");
        }
    }
    // 不要让 Drop 再把背光改回"打开前的值"（那正是我们刚恢复的值，无害但多余）。
    std::mem::forget(control);
    Ok(())
}

/// 当前系统有几块面板支持 DDC/CI 亮度。
pub fn supported_display_count() -> usize {
    match Control::open() {
        Ok(c) => c.supported_count(),
        Err(_) => 0,
    }
}
