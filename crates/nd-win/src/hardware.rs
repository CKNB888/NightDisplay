//! 硬件报告：显卡/驱动版本、每块显示器的分辨率和 DDC 支持。
//!
//! 这些信息只出现在自检、诊断包和设置页里，**不参与任何显示决策**，
//! 所以全部按"尽力而为"处理：读不到就返回空列表，绝不因此报错。
//!
//! 显卡信息走 PowerShell 的 `Win32_VideoController` —— 走 WMI 的 COM 接口
//! 要写几百行 unsafe，而这一个查询在诊断时用一次，慢一点无所谓。

#![cfg(windows)]

use nd_core::status::{GpuInfo, MonitorInfo};

/// 一次 PowerShell 查询就能拿到的显卡信息。
const GPU_SCRIPT: &str = r"
$ErrorActionPreference = 'Stop'
Get-CimInstance -ClassName Win32_VideoController |
  Select-Object Name, DriverVersion, DriverDate |
  ConvertTo-Json -Compress
";

/// 读出显卡名字、驱动版本、驱动日期。读不到返回空列表。
#[must_use]
pub fn gpus() -> Vec<GpuInfo> {
    let Ok(output) = crate::sysutil::run_powershell(GPU_SCRIPT, &[]) else {
        return Vec::new();
    };
    if !output.status.success() {
        tracing::debug!("查询显卡信息失败，自检里留空");
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    // 只有一块显卡时 ConvertTo-Json 给对象，多块时给数组。
    let value: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(value) => value,
        Err(e) => {
            tracing::debug!(error = %e, "显卡信息不是合法 JSON，忽略");
            return Vec::new();
        }
    };
    let items: Vec<&serde_json::Value> = match &value {
        serde_json::Value::Array(items) => items.iter().collect(),
        other => vec![other],
    };
    items
        .into_iter()
        .filter_map(|item| {
            let name = item.get("Name")?.as_str()?.trim().to_string();
            if name.is_empty() {
                return None;
            }
            Some(GpuInfo {
                name,
                driver_version: item
                    .get("DriverVersion")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                driver_date: item
                    .get("DriverDate")
                    .and_then(serde_json::Value::as_str)
                    .map(|raw| {
                        // WMI 给的是 `/Date(1577836800000)/` 这种 JSON 日期。
                        raw.trim_start_matches("/Date(")
                            .trim_end_matches(")/")
                            .parse::<i64>()
                            .ok()
                            .and_then(chrono::DateTime::from_timestamp_millis)
                            .map(|at| {
                                at.with_timezone(&chrono::Local)
                                    .format("%Y-%m-%d")
                                    .to_string()
                            })
                            .unwrap_or_default()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect()
}

/// 每块显示器：名字、分辨率、刷新率、是否支持 DDC/CI、是不是虚拟屏。
///
/// 分辨率从 `DISPLAYCONFIG_TARGET_MODE` 里取（那是真实信号时序），
/// 不是从桌面坐标猜的 —— 缩放 150% 时坐标会骗人。
#[must_use]
pub fn monitors() -> Vec<MonitorInfo> {
    let Ok(entries) = crate::topology::monitor_modes() else {
        return Vec::new();
    };
    // DDC 支持与否只看面板描述能不能对上；对不上就当不支持（保守）。
    let ddc_names: Vec<String> = crate::brightness::Control::open()
        .map(|control| {
            control
                .snapshot()
                .into_iter()
                .map(|(name, _)| name)
                .collect()
        })
        .unwrap_or_default();
    entries
        .into_iter()
        .map(|entry| {
            let ddc = !entry.name.is_empty()
                && ddc_names
                    .iter()
                    .any(|name| name.contains(&entry.name) || entry.name.contains(name));
            MonitorInfo {
                name: entry.name,
                width: entry.width,
                height: entry.height,
                refresh_hz: entry.refresh_hz,
                ddc,
                virtual_output: entry.virtual_output,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_query_does_not_panic_without_powershell() {
        // 没有 PowerShell 的环境（例如交叉编译的测试机）应当返回空列表而不是崩溃。
        let list = gpus();
        for gpu in &list {
            assert!(!gpu.name.is_empty(), "名字为空的条目不该被保留");
        }
    }
}
