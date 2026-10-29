//! 夜间期间需要跨进程保存的原始屏幕状态。
//!
//! 为什么落盘：主进程可能被任务管理器结束，这时看门狗（另一个进程）
//! 必须知道原来的 gamma 和背光值才能恢复。文件写在数据目录里，
//! 只有当前用户可读。

#![cfg(windows)]

use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// 一块显示器的原始状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayState {
    /// 显示器设备名，例如 `\\.\DISPLAY1`。
    pub device: String,
    /// 原始 gamma ramp（768 个 u16）。
    #[serde(default)]
    pub gamma: Vec<u16>,
    /// 原始背光值（VCP 0x10），不支持时为 None。
    #[serde(default)]
    pub brightness: Option<u32>,
    /// 显示器描述，日志用。
    #[serde(default)]
    pub description: String,
}

/// 整个屏幕状态快照。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ScreenState {
    pub displays: Vec<DisplayState>,
    /// 进入夜间的时刻，用于判断快照是否过期。
    pub entered_at: Option<chrono::DateTime<chrono::Local>>,
    /// 是否曾切到虚拟屏（需要还原显示拓扑）。
    ///
    /// 这个标志在**提交拓扑切换之前**就写成 `true`：如果进程在
    /// `SetDisplayConfig` 之后、写文件之前被杀掉，下次启动看到的是
    /// "可能改过拓扑"，会走一遍还原（幂等，白天还原是无操作）。
    pub topology_changed: bool,
    /// 夜间开始前的电源计划 GUID（`{8c5e7fda-...}` 这种格式）。
    ///
    /// 只在用户开了"夜间高性能"时才有值；白天必须切回这一个。
    pub power_plan: Option<String>,
}

#[must_use]
pub fn state_file() -> PathBuf {
    nd_core::paths::screen_state_file()
}

pub fn save(state: &ScreenState) -> Result<()> {
    nd_core::paths::ensure_home()?;
    let path = state_file();
    let text = serde_json::to_string_pretty(state)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn load() -> Option<ScreenState> {
    let text = std::fs::read_to_string(state_file()).ok()?;
    match serde_json::from_str::<ScreenState>(&text) {
        Ok(state) => Some(state),
        Err(e) => {
            tracing::warn!(error = %e, "屏幕状态快照不可读，忽略");
            None
        }
    }
}

pub fn clear() {
    let _ = std::fs::remove_file(state_file());
}

/// 从磁盘快照里取 gamma 备份。
#[must_use]
pub fn load_gamma() -> Option<Vec<crate::gamma::SavedGamma>> {
    let state = load()?;
    let entries: Vec<crate::gamma::SavedGamma> = state
        .displays
        .into_iter()
        .filter(|d| d.gamma.len() == 768)
        .map(|d| crate::gamma::SavedGamma {
            device: d.device,
            ramp: d.gamma,
        })
        .collect();
    if entries.is_empty() {
        None
    } else {
        Some(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_state() {
        crate::test_support::with_temp_home(|_| {
            let state = ScreenState {
                displays: vec![DisplayState {
                    device: r"\\.\DISPLAY1".into(),
                    gamma: vec![7u16; 768],
                    brightness: Some(42),
                    description: "测试屏".into(),
                }],
                entered_at: None,
                topology_changed: false,
                power_plan: None,
            };
            save(&state).unwrap();
            let back = load().unwrap();
            assert_eq!(back.displays.len(), 1);
            assert_eq!(back.displays[0].brightness, Some(42));
            assert_eq!(back.displays[0].gamma.len(), 768);
            clear();
        });
    }
}
