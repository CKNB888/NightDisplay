//! 当前电源计划（power scheme）的读取与切换。
//!
//! 夜间会把电源计划切到"高性能"，免得系统在没人操作时把机器睡下去；
//! 白天必须原样切回去，所以计划 GUID 在**改动之前**就写进 `screen-state.json`。
//!
//! 只用 `powrprof.dll` 的两个函数，不需要管理员权限，也不需要解析
//! `powercfg /getactivescheme` 的输出文本。

#![cfg(windows)]

use anyhow::Result;
use windows::core::GUID;
use windows::Win32::System::Power::{PowerGetActiveScheme, PowerSetActiveScheme};

/// 把 GUID 写成 `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}`。
#[must_use]
pub fn format_guid(guid: &GUID) -> String {
    format!(
        "{{{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}}}",
        guid.data1,
        guid.data2,
        guid.data3,
        guid.data4[0],
        guid.data4[1],
        guid.data4[2],
        guid.data4[3],
        guid.data4[4],
        guid.data4[5],
        guid.data4[6],
        guid.data4[7]
    )
}

/// 解析上面的格式。大小写都接受，花括号可有可无。
pub fn parse_guid(text: &str) -> Option<GUID> {
    let trimmed = text.trim().trim_matches('{').trim_matches('}');
    let parts: Vec<&str> = trimmed.split('-').collect();
    if parts.len() != 5 || parts[0].len() != 8 || parts[3].len() != 4 || parts[4].len() != 12 {
        return None;
    }
    let data1 = u32::from_str_radix(parts[0], 16).ok()?;
    let data2 = u16::from_str_radix(parts[1], 16).ok()?;
    let data3 = u16::from_str_radix(parts[2], 16).ok()?;
    let tail = format!("{}{}", parts[3], parts[4]);
    let mut data4 = [0u8; 8];
    for (index, slot) in data4.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&tail[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(GUID {
        data1,
        data2,
        data3,
        data4,
    })
}

/// 读取当前电源计划的 GUID 字符串。读不到返回 `None`（不算错误）。
#[must_use]
pub fn active() -> Option<String> {
    let mut pointer: *mut GUID = std::ptr::null_mut();
    // SAFETY: 传的是本地指针的地址；成功时系统分配一块 GUID，由 LocalFree 释放。
    let status = unsafe { PowerGetActiveScheme(None, &raw mut pointer) };
    if status.0 != 0 || pointer.is_null() {
        return None;
    }
    // SAFETY: 指针非空且由上面的调用分配。
    let guid = unsafe { *pointer };
    // SAFETY: PowerGetActiveScheme 用 LocalAlloc 分配，必须配 LocalFree。
    unsafe {
        let _ = windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(
            pointer.cast(),
        )));
    }
    Some(format_guid(&guid))
}

/// 切成指定电源计划。GUID 不合法或系统拒绝时返回错误。
pub fn set(scheme: &str) -> Result<()> {
    let guid = parse_guid(scheme)
        .ok_or_else(|| nd_core::error::user(format!("电源计划标识“{scheme}”格式不对")))?;
    // SAFETY: 传的是本地 GUID 的引用，调用期间一直有效。
    let status = unsafe { PowerSetActiveScheme(None, Some(&raw const guid)) };
    if status.0 != 0 {
        return Err(nd_core::error::user(format!(
            "切换电源计划失败（错误码 {}）",
            status.0
        )));
    }
    Ok(())
}

/// 如果夜间切过电源计划，按恢复点切回原来的那个。
///
/// 没有恢复点、或恢复点里没记计划，就什么都不做（这是常态）。
pub fn restore_saved() -> Result<()> {
    let Some(state) = crate::screen_state::load() else {
        return Ok(());
    };
    let Some(saved) = state.power_plan.as_deref() else {
        return Ok(());
    };
    set(saved)?;
    tracing::info!(plan = saved, "已切回原来的电源计划");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guid_round_trip() {
        let text = "{8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c}";
        let guid = parse_guid(text).expect("合法 GUID 必须能解析");
        assert_eq!(format_guid(&guid), text);
    }

    #[test]
    fn guid_without_braces_and_uppercase() {
        let guid = parse_guid("8C5E7FDA-E8BF-4A96-9A85-A6E23A8C635C").expect("无花括号也要接受");
        assert_eq!(format_guid(&guid), "{8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c}");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_guid("高性能").is_none());
        assert!(parse_guid("").is_none());
        assert!(parse_guid("{8c5e7fda-e8bf-4a96-9a85}").is_none());
    }
}
