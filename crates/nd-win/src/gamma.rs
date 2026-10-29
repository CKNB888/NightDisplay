//! Gamma 拉到全黑。
//!
//! 原理：`SetDeviceGammaRamp` 改的是显示输出链路上的查找表，
//! **不在**屏幕采集缓冲里，所以本机全黑、远程抓到的画面完全正常。
//! 这层不影响任何显示拓扑，可以随时还原。
//!
//! 保存原始 ramp，退出夜间时原样写回。某些驱动/笔记本内屏会拒绝设置，
//! 这时只记日志、标记为"不支持"，不影响黑幕模式成功。

#![cfg(windows)]

use std::sync::Mutex;

use anyhow::Result;
use nd_core::mode::LayerStatus;
use windows::core::w;
use windows::Win32::Graphics::Gdi::{CreateDCW, DeleteDC, HDC};
use windows::Win32::UI::ColorSystem::{GetDeviceGammaRamp, SetDeviceGammaRamp};

use crate::overlay;

/// 一条显示输出上的 gamma 备份。
#[derive(Debug, Clone)]
pub struct SavedGamma {
    pub device: String,
    /// 768 个 u16（256 红 + 256 绿 + 256 蓝），原样保存。
    pub ramp: Vec<u16>,
}

static SAVED: Mutex<Vec<SavedGamma>> = Mutex::new(Vec::new());

/// 全黑的 gamma ramp：所有输出都为 0。
fn black_ramp() -> Vec<u16> {
    vec![0u16; 768]
}

/// 线性 ramp，用来在没有原始数据时兜底恢复。
fn linear_ramp() -> Vec<u16> {
    let mut ramp = Vec::with_capacity(768);
    for _ in 0..3 {
        for i in 0..256u32 {
            ramp.push((i * 257) as u16);
        }
    }
    ramp
}

/// 打开某个显示设备的 DC。
fn device_dc(device: &str) -> Option<HDC> {
    let wide: Vec<u16> = device.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: CreateDCW 只读取传入的设备名；返回的 DC 由调用方 DeleteDC 释放。
    let hdc = unsafe {
        CreateDCW(
            w!("DISPLAY"),
            windows::core::PCWSTR(wide.as_ptr()),
            None,
            None,
        )
    };
    if hdc.is_invalid() {
        None
    } else {
        Some(hdc)
    }
}

/// 读取并保存所有显示输出的原始 gamma ramp。**不改任何显示设置**。
///
/// 单独拆出来是因为"先把恢复数据落盘、再动屏幕"这条硬要求：
/// 原始 ramp 必须在压黑之前就拿到手里。
pub fn capture() -> LayerStatus {
    let displays = match overlay::display_devices() {
        Ok(list) if !list.is_empty() => list,
        Ok(_) => {
            return LayerStatus::Unsupported {
                reason: "没有枚举到显示输出".into(),
            }
        }
        Err(e) => {
            return LayerStatus::Unsupported {
                reason: format!("枚举显示输出失败：{e}"),
            }
        }
    };

    let mut saved = SAVED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    saved.clear();
    let mut applied = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for device in displays {
        let Some(hdc) = device_dc(&device) else {
            failures.push(format!("{device}: 无法打开显示设备"));
            continue;
        };
        let mut original = vec![0u16; 768];
        // SAFETY: original 长度固定为 768 个 u16，符合 API 要求。
        let got = unsafe { GetDeviceGammaRamp(hdc, original.as_mut_ptr().cast()) };
        // SAFETY: hdc 由本函数创建。
        unsafe {
            let _ = DeleteDC(hdc);
        }
        if got.as_bool() {
            applied += 1;
            saved.push(SavedGamma {
                device,
                ramp: original,
            });
        } else {
            failures.push(format!("{device}: 读取原始 gamma 失败，跳过而不冒险"));
        }
    }

    if applied == 0 {
        return LayerStatus::Failed {
            reason: if failures.is_empty() {
                "没有显示器接受 gamma 设置".into()
            } else {
                failures.join("；")
            },
        };
    }
    if !failures.is_empty() {
        tracing::warn!(detail = %failures.join("；"), "部分显示器 gamma 读取失败");
    }
    tracing::info!(applied, "已读取原始 gamma");
    LayerStatus::Applied
}

/// 把保存的原始 ramp 对应的输出全部压成黑。必须先 [`capture`]。
pub fn blacken() -> LayerStatus {
    let saved = SAVED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if saved.is_empty() {
        return LayerStatus::Unsupported {
            reason: "没有可用的 gamma 备份（读取失败）".into(),
        };
    }
    let black = black_ramp();
    let mut applied = 0usize;
    let mut failures: Vec<String> = Vec::new();
    for entry in saved.iter() {
        let Some(hdc) = device_dc(&entry.device) else {
            failures.push(format!("{}: 无法打开显示设备", entry.device));
            continue;
        };
        // SAFETY: black 是 768 个 u16，符合 API 要求。
        let ok = unsafe { SetDeviceGammaRamp(hdc, black.as_ptr().cast()) }.as_bool();
        // SAFETY: hdc 由本函数创建。
        unsafe {
            let _ = DeleteDC(hdc);
        }
        if ok {
            applied += 1;
        } else {
            failures.push(format!("{}: 驱动拒绝把 gamma 设为 0", entry.device));
        }
    }
    if applied == 0 {
        return LayerStatus::Failed {
            reason: failures.join("；"),
        };
    }
    if !failures.is_empty() {
        tracing::warn!(detail = %failures.join("；"), "部分显示器 gamma 未能设为 0");
    }
    tracing::info!(applied, "gamma 已拉到全黑");
    LayerStatus::Applied
}

/// 读取原始值并立刻压黑。等价于 [`capture`] + [`blacken`]。
pub fn apply() -> LayerStatus {
    let status = capture();
    if !matches!(status, LayerStatus::Applied) {
        return status;
    }
    blacken()
}

/// 还原所有保存过的 gamma ramp。进程崩溃后由看门狗调用同一函数。
pub fn restore() -> Result<()> {
    let mut saved = SAVED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if saved.is_empty() {
        // 没有内存里的备份（例如是另一个进程进来恢复），从磁盘读。
        if let Some(disk) = crate::screen_state::load_gamma() {
            for entry in disk {
                write_ramp(&entry.device, &entry.ramp);
            }
        }
        return Ok(());
    }
    for entry in saved.drain(..) {
        write_ramp(&entry.device, &entry.ramp);
    }
    Ok(())
}

/// 把指定 ramp 写回一块显示器。
fn write_ramp(device: &str, ramp: &[u16]) {
    if ramp.len() != 768 {
        tracing::warn!(
            device,
            len = ramp.len(),
            "备份的 gamma 长度不对，用线性 ramp 兜底"
        );
        write_ramp(device, &linear_ramp());
        return;
    }
    let Some(hdc) = device_dc(device) else {
        tracing::warn!(device, "恢复 gamma 时无法打开显示设备");
        return;
    };
    // SAFETY: ramp 长度已检查为 768 个 u16。
    let ok = unsafe { SetDeviceGammaRamp(hdc, ramp.as_ptr().cast()) }.as_bool();
    // SAFETY: hdc 由本函数创建。
    unsafe {
        let _ = DeleteDC(hdc);
    }
    if ok {
        tracing::info!(device, "gamma 已恢复");
    } else {
        tracing::warn!(device, "gamma 恢复被驱动拒绝");
    }
}

/// 当前保存的原始 ramp，供写入磁盘备份。
pub fn snapshot() -> Vec<(String, Vec<u16>)> {
    let saved = SAVED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    saved
        .iter()
        .map(|s| (s.device.clone(), s.ramp.clone()))
        .collect()
}

/// 判断当前系统是否允许设置 gamma（有些远程/虚拟驱动会拒绝）。
#[must_use]
pub fn supported() -> bool {
    let Ok(list) = overlay::display_devices() else {
        return false;
    };
    for device in list {
        if let Some(hdc) = device_dc(&device) {
            let mut ramp = vec![0u16; 768];
            // SAFETY: ramp 长度正确。
            let ok = unsafe { GetDeviceGammaRamp(hdc, ramp.as_mut_ptr().cast()) }.as_bool();
            // SAFETY: hdc 由本函数创建。
            unsafe {
                let _ = DeleteDC(hdc);
            }
            if ok {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_ramp_is_all_zero_and_correct_length() {
        let ramp = black_ramp();
        assert_eq!(ramp.len(), 768);
        assert!(ramp.iter().all(|v| *v == 0));
    }

    #[test]
    fn linear_ramp_spans_full_range() {
        let ramp = linear_ramp();
        assert_eq!(ramp.len(), 768);
        assert_eq!(ramp[0], 0);
        assert_eq!(ramp[255], 65535);
        assert_eq!(ramp[256], 0, "每 256 项重新从 0 开始");
    }
}
