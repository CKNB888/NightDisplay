//! 开机自启：HKCU Run 一项 + Windows「启动」里的启用标记。
//!
//! 只写 HKCU，不写 HKLM，也不碰 Run 里别的程序。写完立刻回读，
//! 因为 360、电脑管家之类的"开机加速"会把这一项标成禁用 —— 那种情况
//! 必须让用户看见，而不是以为已经设好了。

#![cfg(windows)]

use std::path::Path;

use anyhow::Result;
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_BINARY, REG_OPTION_NON_VOLATILE,
    REG_SZ, REG_VALUE_TYPE,
};

use crate::sysutil;

/// Run 键下的值名。老版本 Python 版也是这个名字，升级后能直接接上。
pub const VALUE_NAME: &str = "NightDisplay";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// 「启动」页的开关状态。这里写 02 + 11 个 0 表示"用户允许"。
const APPROVED_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const APPROVED_ON: [u8; 12] = [0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
const APPROVED_OFF_MARK: u8 = 0x03;

fn wide(s: &str) -> HSTRING {
    HSTRING::from(s)
}

fn check(status: WIN32_ERROR, what: &str) -> Result<()> {
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(nd_core::error::user(format!(
            "{}失败（Windows 错误码 {}）",
            what, status.0
        )))
    }
}

/// 打开（不存在则创建）一个 HKCU 子键。
fn open_for_write(path: &str) -> Result<HKEY> {
    let mut key = HKEY::default();
    let sub = wide(path);
    // SAFETY: sub 的生命周期覆盖调用；key 是有效的输出指针。
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &sub,
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_QUERY_VALUE | KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
    };
    check(status, "打开注册表")?;
    Ok(key)
}

fn open_for_read(path: &str) -> Option<HKEY> {
    let mut key = HKEY::default();
    let sub = wide(path);
    // SAFETY: 同上；读不到就返回 None。
    let status = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &sub, None, KEY_QUERY_VALUE, &mut key) };
    (status == ERROR_SUCCESS).then_some(key)
}

struct KeyGuard(HKEY);

impl Drop for KeyGuard {
    fn drop(&mut self) {
        // SAFETY: 句柄由 open_for_write/open_for_read 返回，只关一次。
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

/// 写一个 REG_SZ。
fn write_string(key: HKEY, name: &str, value: &str) -> Result<()> {
    let name_w = wide(name);
    // UTF-16 字节（不含结尾 NUL）直接交给 API。
    let mut bytes: Vec<u8> = value.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    if bytes.is_empty() {
        bytes.extend([0, 0]);
    }
    // SAFETY: bytes 长度与切片一致，类型是 REG_SZ。
    let status = unsafe { RegSetValueExW(key, &name_w, None, REG_SZ, Some(&bytes)) };
    check(status, "写入注册表")?;
    Ok(())
}

fn write_bytes(key: HKEY, name: &str, value: &[u8]) -> Result<()> {
    let name_w = wide(name);
    // SAFETY: 同上，类型 REG_BINARY。
    let status = unsafe { RegSetValueExW(key, &name_w, None, REG_BINARY, Some(value)) };
    check(status, "写入注册表")?;
    Ok(())
}

fn delete_value(key: HKEY, name: &str) {
    let name_w = wide(name);
    // SAFETY: 删除一个值，失败也不重要（可能本来就没有）。
    unsafe {
        let _ = RegDeleteValueW(key, &name_w);
    }
}

/// 读出 Run 里的值，并附带类型。
fn read_value(key: HKEY, name: &str) -> Option<(Vec<u8>, REG_VALUE_TYPE)> {
    let name_w = wide(name);
    let mut kind = REG_VALUE_TYPE::default();
    let mut len = 0u32;
    // SAFETY: 第一次探测长度，buf 传 None。
    let status =
        unsafe { RegQueryValueExW(key, &name_w, None, Some(&mut kind), None, Some(&mut len)) };
    if status != ERROR_SUCCESS {
        return None;
    }
    let mut buf = vec![0u8; len as usize];
    let mut kind2 = REG_VALUE_TYPE::default();
    // SAFETY: buf 长度与 len 一致。
    let status = unsafe {
        RegQueryValueExW(
            key,
            &name_w,
            None,
            Some(&mut kind2),
            Some(buf.as_mut_ptr()),
            Some(&mut len),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    buf.truncate(len as usize);
    Some((buf, kind2))
}

/// 当前程序应在 Run 里写的命令。
pub fn desired_command(home: &Path) -> String {
    let args = sysutil::self_args(home, &["--tray"]);
    sysutil::command_line(&sysutil::exe_path(), &args)
}

/// 读取 Run 里现有的命令。没有这一项、类型不对或读失败都返回空串。
pub fn current_command() -> String {
    let Some(key) = open_for_read(RUN_KEY) else {
        return String::new();
    };
    let _guard = KeyGuard(key);
    let Some((bytes, kind)) = read_value(key, VALUE_NAME) else {
        return String::new();
    };
    if kind != REG_SZ && kind != windows::Win32::System::Registry::REG_EXPAND_SZ {
        return String::new();
    }
    #[allow(clippy::chunks_exact_to_as_chunks)]
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|u| *u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// 「启动」里这一项有没有被禁用。
pub fn startup_approved() -> bool {
    let Some(key) = open_for_read(APPROVED_KEY) else {
        // 没有这个键就说明用户没动过，按允许算。
        return true;
    };
    let _guard = KeyGuard(key);
    match read_value(key, VALUE_NAME) {
        Some((bytes, _)) => bytes.first().copied() != Some(APPROVED_OFF_MARK),
        None => true,
    }
}

/// 写入或删除开机自启，写成功后回读校验。
pub fn set(enabled: bool, home: &Path) -> Result<()> {
    {
        let key = open_for_write(RUN_KEY)?;
        let _guard = KeyGuard(key);
        if enabled {
            let cmd = desired_command(home);
            write_string(key, VALUE_NAME, &cmd)?;
            tracing::info!(command = %cmd, "已写入开机自启（HKCU Run）");
        } else {
            delete_value(key, VALUE_NAME);
            tracing::info!("已删除开机自启");
        }
    }

    // 「启动」页的开关：设成开时顺手标成启用，设成关时一并清掉。
    if let Ok(approved) = open_for_write(APPROVED_KEY) {
        let _guard = KeyGuard(approved);
        if enabled {
            write_bytes(approved, VALUE_NAME, &APPROVED_ON)?;
        } else {
            delete_value(approved, VALUE_NAME);
        }
    }

    // 回读校验：Run 里必须真的是我们刚写的命令。
    if enabled {
        let actual = current_command();
        let want = desired_command(home);
        if actual != want {
            // 命令不一致通常是被别的程序改回去了。
            if actual.is_empty() {
                return Err(nd_core::error::user(
                    "开机自启写入后没有生效，可能被安全软件拦下了",
                ));
            }
        }
        if !startup_approved() {
            return Err(nd_core::error::user(
                "开机自启已写入，但在「任务管理器 → 启动」里被禁用了，需要手动启用",
            ));
        }
    }
    Ok(())
}

/// 实际是否注册了（命令非空且没被禁用）。
pub fn is_enabled() -> bool {
    !current_command().is_empty() && startup_approved()
}

/// 开机自启的诊断信息，给设置页和自检用。
pub fn diagnose(home: &Path) -> String {
    let actual = current_command();
    if actual.is_empty() {
        return "未注册".into();
    }
    let want = desired_command(home);
    if actual != want {
        return format!("已注册，但命令是旧路径：{actual}");
    }
    if !startup_approved() {
        return "已注册，但在「启动」里被禁用".into();
    }
    "已注册".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desired_command_mentions_tray_and_home() {
        let cmd = desired_command(Path::new(r"C:\data\nd"));
        assert!(cmd.contains("--tray"));
        assert!(cmd.contains(r"--home"));
        assert!(cmd.contains("nd"));
    }

    #[test]
    fn approved_marker_is_off_for_03() {
        let mut bytes = APPROVED_ON;
        bytes[0] = APPROVED_OFF_MARK;
        assert_eq!(bytes.first().copied(), Some(3));
    }

    #[test]
    fn reading_absent_value_is_empty() {
        // 用一个肯定不存在的值名，确认读不到时不会 panic。
        let Some(key) = open_for_read(RUN_KEY) else {
            return;
        };
        let _guard = KeyGuard(key);
        assert!(read_value(key, "NightDisplay__test_absent_value__").is_none());
    }

    #[test]
    fn success_is_zero() {
        assert_eq!(ERROR_SUCCESS.0, 0);
    }

    #[test]
    fn wide_roundtrip() {
        let h = wide("NightDisplay");
        assert_eq!(h.to_string(), "NightDisplay");
    }
}
