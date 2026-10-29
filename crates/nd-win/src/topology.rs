//! 显示拓扑查询与切换 —— **只有用户手动选择「仅虚拟屏」时才会走到这里**。
//!
//! 默认的 `guard` 模式绝不调用本模块的任何函数：不改拓扑、不改分辨率、不改 DPI，
//! 这是这次重写的硬约束（旧版正是因为调 `SetDisplayConfig` 才让远程画面消失）。
//!
//! 这里保留的能力只有两条：
//! * `describe()` —— 只读，用来识别哪块屏是虚拟屏、以及状态文本里报显示器型号；
//! * `plan_virtual_only()` / `apply_virtual_only()` / `restore()` —— 手动模式的实现，
//!   带完整备份和分级回滚。

#![cfg(windows)]

use std::collections::HashMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig, SetDisplayConfig,
    DISPLAYCONFIG_ADAPTER_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_ADAPTER_NAME,
    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_TARGET_DEVICE_NAME,
    QDC_ALL_PATHS, QDC_ONLY_ACTIVE_PATHS, SDC_ALLOW_CHANGES, SDC_ALLOW_PATH_ORDER_CHANGES,
    SDC_APPLY, SDC_SAVE_TO_DATABASE, SDC_TOPOLOGY_CLONE, SDC_TOPOLOGY_EXTEND,
    SDC_TOPOLOGY_INTERNAL, SDC_TOPOLOGY_SUPPLIED, SDC_USE_SUPPLIED_DISPLAY_CONFIG, SDC_VALIDATE,
};

/// 路径 `flags` 里表示"这条路径当前是活动的"（`DISPLAYCONFIG_PATH_ACTIVE`）。
const PATH_FLAGS_ACTIVE: u32 = 0x0000_0001;

/// 一块显示器（按 target 去重）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayInfo {
    /// 友好名，例如 `DELL U2720Q`、`VDD by MTT`。
    pub name: String,
    /// 设备路径，能看出是不是虚拟屏。
    pub device_path: String,
    /// 适配器名。
    pub adapter_path: String,
    /// 输出技术代码（17 = 间接虚拟）。
    pub tech: i32,
    /// 这条路径现在是活动的。
    pub active: bool,
    /// target 的 id，排序用。
    pub target_id: u32,
    /// source 的 id。
    pub source_id: u32,
    /// target 所在适配器的 LUID 低位，用来和原始路径对上。
    #[serde(default)]
    pub adapter_low: u32,
    /// 适配器 LUID 高位。
    #[serde(default)]
    pub adapter_high: i32,
}

/// 内置 VDD 的匹配规则。必须足够具体：不能把 UU 之类的第三方虚拟屏当成自家驱动。
const BUNDLED_VDD_RE: &[&str] = &["mttvdd", "vdd by mtt", "root\\mttvdd", "display#mtt"];

/// 识别"虚拟屏"的通用规则（用户手动模式下会用到）。
const VIRTUAL_RE: &[&str] = &[
    "mttvdd",
    "virtual display",
    "vdd",
    "iddsample",
    "parsec",
    "uu",
];

fn has_any(haystack: &str, needles: &[&str]) -> bool {
    let lower = haystack.to_lowercase();
    needles.iter().any(|n| lower.contains(n))
}

/// 这块输出是不是包里自带的 VDD（而不是 UU 等第三方虚拟屏）。
pub fn is_bundled_vdd(d: &DisplayInfo) -> bool {
    has_any(
        &format!("{} {} {}", d.name, d.device_path, d.adapter_path),
        BUNDLED_VDD_RE,
    )
}

/// 是不是虚拟屏。
pub fn is_virtual(d: &DisplayInfo) -> bool {
    // 17 = DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INDIRECT_VIRTUAL。
    // 16（INDIRECT_WIRED）可能是 USB 实体屏，不能算。
    d.tech == 17
        || has_any(
            &format!("{} {} {}", d.name, d.device_path, d.adapter_path),
            VIRTUAL_RE,
        )
}

/// 枚举显示器。`active_only` 为真时只看当前活动的输出。
pub fn describe(active_only: bool) -> Result<Vec<DisplayInfo>> {
    let flags = if active_only {
        QDC_ONLY_ACTIVE_PATHS
    } else {
        QDC_ALL_PATHS
    };
    let (paths, _modes) = query(flags)?;
    let mut names: HashMap<(u32, i32, u32), (String, String)> = HashMap::new();
    let mut out: Vec<DisplayInfo> = Vec::new();
    let mut index: HashMap<(u32, i32, u32), usize> = HashMap::new();

    for path in paths.iter() {
        let key = (
            path.targetInfo.adapterId.LowPart,
            path.targetInfo.adapterId.HighPart,
            path.targetInfo.id,
        );
        let (name, device_path) = names
            .entry(key)
            .or_insert_with(|| target_name(&path.targetInfo.adapterId, path.targetInfo.id))
            .clone();
        let adapter_path = adapter_name(&path.targetInfo.adapterId);
        let active = (path.flags & PATH_FLAGS_ACTIVE) != 0;
        // SAFETY: 两个 union 分支都是 u32 / bitfield 布局，读 modeInfoIdx 总是合法的。
        let source_id = path.sourceInfo.id;
        match index.get(&key) {
            Some(pos) => {
                if active {
                    out[*pos].active = true;
                }
            }
            None => {
                index.insert(key, out.len());
                out.push(DisplayInfo {
                    name,
                    device_path,
                    adapter_path,
                    tech: path.targetInfo.outputTechnology.0,
                    active,
                    target_id: path.targetInfo.id,
                    source_id,
                    adapter_low: key.0,
                    adapter_high: key.1,
                });
            }
        }
    }
    Ok(out)
}

/// 查一次显示配置。返回 (paths, modes)。
fn query(
    flags: windows::Win32::Devices::Display::QUERY_DISPLAY_CONFIG_FLAGS,
) -> Result<(Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>)> {
    for _ in 0..5 {
        let mut num_paths = 0u32;
        let mut num_modes = 0u32;
        // SAFETY: 只写两个 u32 输出参数。
        let status = unsafe { GetDisplayConfigBufferSizes(flags, &mut num_paths, &mut num_modes) };
        if status.0 != 0 {
            return Err(nd_core::error::user(format!(
                "读取显示配置失败（{}），可以先试 Win+P 手动切换",
                describe_error(status.0 as i32)
            )));
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); num_paths as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); num_modes as usize];
        // SAFETY: 两个缓冲区的长度与传入的元素个数一致。
        let status = unsafe {
            QueryDisplayConfig(
                flags,
                &mut num_paths,
                paths.as_mut_ptr(),
                &mut num_modes,
                modes.as_mut_ptr(),
                None,
            )
        };
        if status.0 == 122 {
            // ERROR_INSUFFICIENT_BUFFER：期间显示器变了，重来。
            continue;
        }
        if status.0 != 0 {
            return Err(nd_core::error::user(format!(
                "读取显示配置失败（{}）",
                describe_error(status.0 as i32)
            )));
        }
        paths.truncate(num_paths as usize);
        modes.truncate(num_modes as usize);
        return Ok((paths, modes));
    }
    Err(nd_core::error::user(
        "显示配置反复变化，读取不稳定，请稍后再试",
    ))
}

/// target 的友好名和设备路径。
fn target_name(adapter: &windows::Win32::Foundation::LUID, id: u32) -> (String, String) {
    let mut info = DISPLAYCONFIG_TARGET_DEVICE_NAME {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
            size: std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
            adapterId: *adapter,
            id,
        },
        ..Default::default()
    };
    // SAFETY: info 的类型与 header.type 一致，size 已填好。
    let status = unsafe { DisplayConfigGetDeviceInfo(&mut info.header) };
    if status != 0 {
        return (String::new(), String::new());
    }
    (
        utf16z(&info.monitorFriendlyDeviceName),
        utf16z(&info.monitorDevicePath),
    )
}

/// 适配器名。
fn adapter_name(adapter: &windows::Win32::Foundation::LUID) -> String {
    let mut info = DISPLAYCONFIG_ADAPTER_NAME {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_ADAPTER_NAME,
            size: std::mem::size_of::<DISPLAYCONFIG_ADAPTER_NAME>() as u32,
            adapterId: *adapter,
            id: 0,
        },
        ..Default::default()
    };
    // SAFETY: 同上。
    let status = unsafe { DisplayConfigGetDeviceInfo(&mut info.header) };
    if status != 0 {
        return String::new();
    }
    utf16z(&info.adapterDevicePath)
}

fn utf16z(buf: &[u16]) -> String {
    let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len]).trim().to_string()
}

/// Windows 错误码的中文说明（只列常见的几个）。
pub fn describe_error(code: i32) -> String {
    match code {
        0 => "成功".into(),
        5 => "拒绝访问（需要管理员权限）".into(),
        31 => "设备未工作（显示器或驱动已断开）".into(),
        50 => "系统不支持该配置".into(),
        87 => "参数无效".into(),
        122 => "缓冲区不足".into(),
        1610 => "配置无效（显卡驱动不接受这个显示组合）".into(),
        other => format!("错误码 {other}"),
    }
}

/// 备份文件的结构。路径和模式按原始结构序列化，还原时原样交回。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DisplayBackup {
    pub paths: Vec<PathRecord>,
    pub modes: Vec<ModeRecord>,
    pub displays: Vec<DisplayInfo>,
}

/// 一条路径的可序列化形式（只保留还原需要的字段）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathRecord {
    pub source_id: u32,
    pub target_id: u32,
    pub flags: u32,
    pub tech: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModeRecord {
    pub id: u32,
    pub info_type: i32,
}

/// 备份当前活动的显示配置。
pub fn backup() -> Result<DisplayBackup> {
    let (paths, modes) = query(QDC_ONLY_ACTIVE_PATHS)?;
    Ok(DisplayBackup {
        paths: paths
            .iter()
            .map(|p| PathRecord {
                source_id: p.sourceInfo.id,
                target_id: p.targetInfo.id,
                flags: p.flags,
                tech: p.targetInfo.outputTechnology.0,
            })
            .collect(),
        modes: modes
            .iter()
            .map(|m| ModeRecord {
                id: m.id,
                info_type: m.infoType.0,
            })
            .collect(),
        displays: describe(true)?,
    })
}

/// 「仅虚拟屏」的执行计划：挑中的虚拟输出 + 那条原始路径。
pub struct VirtualPlan {
    pub target: DisplayInfo,
    /// 原始路径（来自 `QDC_ALL_PATHS`），应用时改成"唯一活动的路径"。
    pub path: DISPLAYCONFIG_PATH_INFO,
    /// 被让出的实体屏数量，写进日志。
    pub physical_count: usize,
}

/// 计划"只保留虚拟屏"：挑一块虚拟输出（**不应用**）。
pub fn plan_virtual_only(match_pattern: &str) -> Result<VirtualPlan> {
    let (paths, _modes) = query(QDC_ALL_PATHS)?;
    let all = describe(false)?;
    let needles: Vec<&str> = if match_pattern.is_empty() {
        VIRTUAL_RE.to_vec()
    } else {
        vec![match_pattern]
    };

    // 每条路径带上它的显示器信息，方便按 target 去重后挑。
    let mut candidates: Vec<(DISPLAYCONFIG_PATH_INFO, DisplayInfo)> = Vec::new();
    for path in paths.iter() {
        let key = (
            path.targetInfo.adapterId.LowPart,
            path.targetInfo.adapterId.HighPart,
            path.targetInfo.id,
        );
        let Some(info) = all
            .iter()
            .find(|d| d.target_id == key.2 && d.adapter_low == key.0 && d.adapter_high == key.1)
        else {
            continue;
        };
        if !path.targetInfo.targetAvailable.as_bool() {
            continue;
        }
        let virtual_output = info.tech == 17
            || has_any(
                &format!("{} {} {}", info.name, info.device_path, info.adapter_path),
                &needles,
            );
        if virtual_output {
            candidates.push((*path, info.clone()));
        }
    }
    if candidates.is_empty() {
        return Err(nd_core::error::user(
            "没有找到可用的虚拟显示器（VDD）。请先在设置里安装虚拟显示驱动。",
        ));
    }
    // 内置 VDD 优先；同类里优先本来就活动的（避免抢走远程软件正在用的那块）。
    candidates.sort_by_key(|(_, d)| (!is_bundled_vdd(d), !d.active, d.source_id));
    let (path, target) = candidates.remove(0);
    let physical_count = all.iter().filter(|d| !is_virtual(d)).count();
    Ok(VirtualPlan {
        target,
        path,
        physical_count,
    })
}

/// 把一条路径改成"活动、模式由系统自选"。
fn activate_only_path(mut path: DISPLAYCONFIG_PATH_INFO) -> DISPLAYCONFIG_PATH_INFO {
    path.flags |= PATH_FLAGS_ACTIVE;
    // 模式索引设成 0xFFFFFFFF，让系统在 SDC_TOPOLOGY_SUPPLIED 下自己挑分辨率。
    // 这两个 union 的首字段都是 u32，写 modeInfoIdx 是合法用法（union 字段访问本身
    // 不需要 unsafe 的是被生成的访问器，这里写的是普通字段）。
    path.sourceInfo.Anonymous.modeInfoIdx = u32::MAX;
    path.targetInfo.Anonymous.modeInfoIdx = u32::MAX;
    path
}

/// 应用「仅虚拟屏」。`dry_run` 只做 `SDC_VALIDATE`。
///
/// **绝不写数据库**：这是临时状态，写进去会导致重启后实体屏直接不亮。
pub fn apply_virtual_only(plan: &VirtualPlan, dry_run: bool) -> Result<()> {
    let paths = [activate_only_path(plan.path)];
    let flags = [
        SDC_TOPOLOGY_SUPPLIED | SDC_ALLOW_PATH_ORDER_CHANGES,
        SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES,
    ];
    let target_label = if plan.target.name.is_empty() {
        plan.target.device_path.clone()
    } else {
        plan.target.name.clone()
    };

    let mut chosen = None;
    let mut failures = Vec::new();
    for flag in flags {
        // SAFETY: 传一个长度正确的路径数组，模式数组为空；这是文档允许的组合。
        let status = unsafe { SetDisplayConfig(Some(&paths), None, SDC_VALIDATE | flag) };
        if status == 0 {
            chosen = Some(flag);
            break;
        }
        failures.push(describe_error(status));
    }
    let Some(flag) = chosen else {
        return Err(nd_core::error::user(format!(
            "系统不接受「仅虚拟屏」配置：{}（目标：{target_label}，需要放弃 {} 块实体屏）",
            failures.join(" / "),
            plan.physical_count
        )));
    };
    if dry_run {
        tracing::info!(target = %target_label, "「仅虚拟屏」配置校验通过（未应用）");
        return Ok(());
    }
    // SAFETY: 同上，这次是应用。
    let status = unsafe { SetDisplayConfig(Some(&paths), None, SDC_APPLY | flag) };
    if status != 0 {
        return Err(nd_core::error::user(format!(
            "应用「仅虚拟屏」失败：{}",
            describe_error(status)
        )));
    }
    tracing::warn!(
        target = %target_label,
        physical = plan.physical_count,
        "已切换到「仅虚拟屏」，远程画面会中断一次"
    );
    Ok(())
}

/// 校验现在是不是"只剩虚拟屏"。
pub fn verify_virtual_only() -> bool {
    match describe(true) {
        Ok(active) => !active.is_empty() && active.iter().all(is_virtual),
        Err(_) => false,
    }
}

/// 还原显示拓扑：先内部屏，再扩展、复制。任一成功即停止。
///
/// 返回实际使用的级别，写进日志。
pub fn restore() -> Result<&'static str> {
    let attempts: [(
        &'static str,
        windows::Win32::Devices::Display::SET_DISPLAY_CONFIG_FLAGS,
    ); 3] = [
        ("仅电脑屏幕", SDC_TOPOLOGY_INTERNAL),
        ("扩展", SDC_TOPOLOGY_EXTEND),
        ("复制", SDC_TOPOLOGY_CLONE),
    ];
    let mut tried = Vec::new();
    for (label, flag) in attempts {
        // SAFETY: 只用拓扑标志，标准恢复方式。
        let status = unsafe { SetDisplayConfig(None, None, SDC_APPLY | flag) };
        if status != 0 {
            tried.push(format!("{label}：{}", describe_error(status)));
            continue;
        }
        let active = describe(true).unwrap_or_default();
        if active.is_empty() {
            tried.push(format!("{label}：恢复后没有活动显示器"));
            continue;
        }
        if active.iter().all(is_virtual) {
            tried.push(format!("{label}：恢复后仍然只有虚拟屏"));
            continue;
        }
        // 确认有实体屏了才写数据库，避免把黑屏拓扑存成开机默认。
        // SAFETY: 与上面同一次调用的持久化版本。
        let persisted =
            unsafe { SetDisplayConfig(None, None, SDC_APPLY | flag | SDC_SAVE_TO_DATABASE) };
        if persisted != 0 {
            tracing::warn!(
                error = %describe_error(persisted),
                "显示已恢复，但没能写入系统显示数据库，重启后可能回到旧布局"
            );
        }
        tracing::info!(level = label, "显示拓扑已恢复");
        return Ok(label);
    }
    Err(nd_core::error::user(format!(
        "所有恢复方式都失败了，请按 Win+P 手动选择显示方式（已尝试：{}）",
        tried.join("；")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(name: &str, tech: i32) -> DisplayInfo {
        DisplayInfo {
            name: name.into(),
            device_path: String::new(),
            adapter_path: String::new(),
            tech,
            active: true,
            target_id: 1,
            source_id: 1,
            adapter_low: 0,
            adapter_high: 0,
        }
    }

    #[test]
    fn bundled_vdd_matches_own_driver_only() {
        assert!(is_bundled_vdd(&display("MttVDD", 0)));
        assert!(is_bundled_vdd(&display("VDD by MTT", 0)));
        assert!(!is_bundled_vdd(&display("UU Virtual Display", 0)));
    }

    #[test]
    fn indirect_virtual_counts_as_virtual() {
        assert!(is_virtual(&display("Generic PnP Monitor", 17)));
        // 16 = INDIRECT_WIRED，可能是 USB 实体屏，不算虚拟。
        assert!(!is_virtual(&display("USB Monitor", 16)));
    }

    #[test]
    fn uu_and_parsec_are_virtual_by_name() {
        assert!(is_virtual(&display("UU Virtual Display", 0)));
        assert!(is_virtual(&display("Parsec Virtual Display", 0)));
    }

    #[test]
    fn physical_dell_is_not_virtual() {
        assert!(!is_virtual(&display("DELL U2720Q", 0)));
    }

    #[test]
    fn error_codes_have_chinese_text() {
        assert_eq!(describe_error(5), "拒绝访问（需要管理员权限）");
        assert!(describe_error(9999).contains("9999"));
    }

    #[test]
    fn path_active_bit_is_one() {
        // 与 Windows SDK 的 DISPLAYCONFIG_PATH_ACTIVE 保持一致。
        assert_eq!(PATH_FLAGS_ACTIVE, 1);
    }
}
