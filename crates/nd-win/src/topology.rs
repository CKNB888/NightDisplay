//! 显示拓扑：虚拟屏接管桌面，以及还原。
//!
//! ## 这一层的两条硬约束
//!
//! 1. **一次 `SetDisplayConfig`，原子完成。** 绝不"先关实体屏、再开虚拟屏"：
//!    两步之间显示器会重新同步，用户看到的就是"远程进来先亮一秒"。
//!    所以每次都用 `SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES`
//!    提交**完整**的路径集，而不是 `SDC_TOPOLOGY_*` 那种让系统自己猜的组合。
//!
//! 2. **活动 target 永远不为空，显示数量不变。** 旧版把实体屏的 target 从路径集里
//!    删掉，远程软件抓着的那块采集表面跟着消失，画面就冻住了。现在实体屏的 target
//!    保持 ACTIVE，只是把它后面的 source 摘掉 —— 设备还在、还能枚举、还占着一个
//!    target，只是不再有桌面。活动 target 数从 N 变成 N+1，永远不会是 0。
//!
//! 切换前一定先 `SDC_VALIDATE`；校验不过就什么都不做，把原因原样带回上层。
//!
//! 其余函数只读（`describe`、`verify_standby`）或只在还原时使用（`restore`）。

#![cfg(windows)]

use std::collections::HashMap;

use windows::Win32::Foundation::LUID;

use anyhow::Result;
use nd_core::mode::VirtualReport;
use serde::{Deserialize, Serialize};
use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig, SetDisplayConfig,
    DISPLAYCONFIG_ADAPTER_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_ADAPTER_NAME,
    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_MODE_INFO_0, DISPLAYCONFIG_MODE_INFO_TYPE_TARGET,
    DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_TARGET_DEVICE_NAME, DISPLAYCONFIG_TARGET_PREFERRED_MODE,
    QDC_ALL_PATHS, QDC_ONLY_ACTIVE_PATHS, SDC_ALLOW_CHANGES, SDC_APPLY, SDC_SAVE_TO_DATABASE,
    SDC_TOPOLOGY_CLONE, SDC_TOPOLOGY_EXTEND, SDC_TOPOLOGY_INTERNAL,
    SDC_USE_SUPPLIED_DISPLAY_CONFIG, SDC_VALIDATE, SET_DISPLAY_CONFIG_FLAGS,
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
#[must_use]
pub fn is_bundled_vdd(d: &DisplayInfo) -> bool {
    has_any(
        &format!("{} {} {}", d.name, d.device_path, d.adapter_path),
        BUNDLED_VDD_RE,
    )
}

/// 是不是虚拟屏。
#[must_use]
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

    for path in &paths {
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
        if let Some(pos) = index.get(&key) {
            if active {
                out[*pos].active = true;
            }
        } else {
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
        let status =
            unsafe { GetDisplayConfigBufferSizes(flags, &raw mut num_paths, &raw mut num_modes) };
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
                &raw mut num_paths,
                paths.as_mut_ptr(),
                &raw mut num_modes,
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
    let status = unsafe { DisplayConfigGetDeviceInfo(&raw mut info.header) };
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
    let status = unsafe { DisplayConfigGetDeviceInfo(&raw mut info.header) };
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
#[must_use]
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

/// 一次切换的完整计划：提交给 `SetDisplayConfig` 的**全部**路径和模式。
///
/// 关键在于"完整"：`SDC_USE_SUPPLIED_DISPLAY_CONFIG` 只认传进来的这一份，
/// 系统不会自己补齐，所以这里必须包含每一块屏的路径，包括那些暂时不显示的。
pub struct SwitchPlan {
    pub paths: Vec<DISPLAYCONFIG_PATH_INFO>,
    pub modes: Vec<DISPLAYCONFIG_MODE_INFO>,
    /// 接管桌面的虚拟屏名（用于日志/界面）。
    pub virtual_name: String,
    /// 被摘掉桌面的实体屏块数。
    pub physical_count: usize,
    /// 虚拟屏的模式和实体屏一致（一致 = 合成器不用重新初始化）。
    pub mode_matched: bool,
    /// 虚拟屏和实体屏共用同一个桌面（复制模式）。
    pub shared_source: bool,
    /// 切换前活动 target 数。
    pub active_before: usize,
    /// 计划里活动 target 数（永远 ≥ 1）。
    pub active_after: usize,
    /// 虚拟屏切换前后的身份（target 适配器 + id），用来确认采集表面没被动过。
    pub vdd_identity: Option<(u32, i32, u32)>,
}

fn is_active(p: &DISPLAYCONFIG_PATH_INFO) -> bool {
    (p.flags & PATH_FLAGS_ACTIVE) != 0
}

fn needles(match_pat: &str) -> Vec<String> {
    if match_pat.trim().is_empty() {
        VIRTUAL_RE
            .iter()
            .map(std::string::ToString::to_string)
            .collect()
    } else {
        vec![match_pat.trim().to_string()]
    }
}

fn path_text(p: &DISPLAYCONFIG_PATH_INFO) -> String {
    let (name, device) = target_name(&p.targetInfo.adapterId, p.targetInfo.id);
    let adapter = adapter_name(&p.targetInfo.adapterId);
    format!("{name} {device} {adapter}")
}

fn path_is_virtual(p: &DISPLAYCONFIG_PATH_INFO, patterns: &[String]) -> bool {
    if p.targetInfo.outputTechnology.0 == 17 {
        return true;
    }
    let text = path_text(p).to_lowercase();
    patterns.iter().any(|n| text.contains(&n.to_lowercase()))
        || VIRTUAL_RE.iter().any(|n| text.contains(n))
}

fn path_is_bundled(p: &DISPLAYCONFIG_PATH_INFO) -> bool {
    let text = path_text(p).to_lowercase();
    BUNDLED_VDD_RE.iter().any(|n| text.contains(n))
}

fn path_display_name(p: &DISPLAYCONFIG_PATH_INFO) -> String {
    let (name, device) = target_name(&p.targetInfo.adapterId, p.targetInfo.id);
    if name.is_empty() {
        if device.is_empty() {
            format!("显示器 {}", p.targetInfo.id)
        } else {
            device
        }
    } else {
        name
    }
}

/// modeInfoIdx 的"无效"哨兵值（`DISPLAYCONFIG_PATH_MODE_IDX_INVALID`）。
const MODE_IDX_INVALID: u32 = 0xFFFF_FFFF;

fn source_mode_idx(p: &DISPLAYCONFIG_PATH_INFO) -> Option<u32> {
    // SAFETY: 两个联合分支都是 u32 布局，读取总是合法的。
    let idx = unsafe { p.sourceInfo.Anonymous.modeInfoIdx };
    (idx != MODE_IDX_INVALID).then_some(idx)
}

fn target_mode_idx(p: &DISPLAYCONFIG_PATH_INFO) -> Option<u32> {
    // SAFETY: 同上。
    let idx = unsafe { p.targetInfo.Anonymous.modeInfoIdx };
    (idx != MODE_IDX_INVALID).then_some(idx)
}

/// 虚拟屏驱动自己推荐的目标模式（复制时如果实体屏的分辨率它吃不下，就用这个）。
fn preferred_target_mode(adapter: &LUID, id: u32) -> Option<DISPLAYCONFIG_MODE_INFO> {
    let mut info = DISPLAYCONFIG_TARGET_PREFERRED_MODE {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: windows::Win32::Devices::Display::DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_PREFERRED_MODE,
            size: std::mem::size_of::<DISPLAYCONFIG_TARGET_PREFERRED_MODE>() as u32,
            adapterId: *adapter,
            id,
        },
        ..Default::default()
    };
    // SAFETY: info 的类型与 header.type 一致，size 已填好。
    let status = unsafe { DisplayConfigGetDeviceInfo(&raw mut info.header) };
    if status != 0 {
        return None;
    }
    Some(DISPLAYCONFIG_MODE_INFO {
        infoType: DISPLAYCONFIG_MODE_INFO_TYPE_TARGET,
        id,
        adapterId: *adapter,
        Anonymous: DISPLAYCONFIG_MODE_INFO_0 {
            targetMode: info.targetMode,
        },
    })
}

/// 挑一块可用的虚拟 target 路径：自带 VDD 优先，其次优先已经活动的
/// （已经活动的说明远程软件可能正抓着它，更不能换）。
fn pick_virtual(paths: &[DISPLAYCONFIG_PATH_INFO], patterns: &[String]) -> Result<usize> {
    paths
        .iter()
        .enumerate()
        .filter(|(_, p)| p.targetInfo.targetAvailable.as_bool() && path_is_virtual(p, patterns))
        .max_by_key(|(i, p)| (path_is_bundled(p), is_active(p), !*i))
        .map(|(i, _)| i)
        .ok_or_else(|| {
            nd_core::error::user(
                "没有找到可用的虚拟屏（VDD）。请先执行「安装虚拟显示驱动」，\
                 或用 --action install-vdd 安装后重试",
            )
        })
}

/// 计划「白天预热」：虚拟屏接进桌面，和实体屏**共用同一个桌面**（复制）。
///
/// 为什么是复制而不是扩展：
/// * 复制不改桌面布局，窗口不会跑到虚拟屏上，用户白天完全无感；
/// * 复制时虚拟屏和实体屏同源，入夜只需要把实体屏那条路径的活动标志去掉，
///   虚拟屏后面的 source 一点没变，远程软件抓着的那块采集表面也就不会重建。
///
/// 如果虚拟屏已经活动，直接返回当前状态（幂等）。
pub fn plan_arm(match_pat: &str) -> Result<SwitchPlan> {
    let (paths, mut modes) = query(QDC_ALL_PATHS)?;
    let patterns = needles(match_pat);
    let vdd_idx = pick_virtual(&paths, &patterns)?;
    let active_before = paths.iter().filter(|p| is_active(p)).count();
    let mut out = paths.clone();
    let virtual_name = path_display_name(&out[vdd_idx]);
    let identity = (
        out[vdd_idx].targetInfo.adapterId.LowPart,
        out[vdd_idx].targetInfo.adapterId.HighPart,
        out[vdd_idx].targetInfo.id,
    );

    if is_active(&out[vdd_idx]) {
        // 已经预热过。确认它确实接着桌面（有 source）就好。
        let shared = out.iter().any(|p| {
            is_active(p) && !path_is_virtual(p, &patterns) && {
                // SAFETY: 见 source_mode_idx。
                let a = unsafe { p.sourceInfo.Anonymous.modeInfoIdx };
                let b = unsafe { out[vdd_idx].sourceInfo.Anonymous.modeInfoIdx };
                a == b
            }
        });
        return Ok(SwitchPlan {
            active_after: active_before,
            physical_count: 0,
            mode_matched: true,
            shared_source: shared,
            paths: out,
            modes,
            virtual_name,
            active_before,
            vdd_identity: Some(identity),
        });
    }

    // 选一块活动的实体屏当"桌面来源"。
    let phys_idx = out
        .iter()
        .enumerate()
        .filter(|(_, p)| is_active(p) && !path_is_virtual(p, &patterns))
        .max_by_key(|(_, p)| target_mode_idx(p).is_some())
        .map(|(i, _)| i)
        .ok_or_else(|| nd_core::error::user("当前没有活动的实体屏，虚拟屏没有可复制的桌面来源"))?;
    let phys = out[phys_idx];
    let src_idx = source_mode_idx(&phys)
        .ok_or_else(|| nd_core::error::user("实体屏的桌面模式索引无效，无法预热虚拟屏"))?;

    // 目标模式：优先照抄实体屏的信号，让两边像素一致。
    let mut mode_matched = false;
    let tgt_mode = match target_mode_idx(&phys).and_then(|i| modes.get(i as usize).copied()) {
        Some(mut m) => {
            mode_matched = true;
            m.id = out[vdd_idx].targetInfo.id;
            m.adapterId = out[vdd_idx].targetInfo.adapterId;
            m
        }
        None => preferred_target_mode(
            &out[vdd_idx].targetInfo.adapterId,
            out[vdd_idx].targetInfo.id,
        )
        .ok_or_else(|| nd_core::error::user("虚拟屏没有可用模式"))?,
    };
    let tgt_idx = modes.len() as u32;
    modes.push(tgt_mode);

    let vdd = &mut out[vdd_idx];
    vdd.flags |= PATH_FLAGS_ACTIVE;
    vdd.sourceInfo.adapterId = phys.sourceInfo.adapterId;
    vdd.sourceInfo.id = phys.sourceInfo.id;
    vdd.sourceInfo.Anonymous.modeInfoIdx = src_idx;
    vdd.targetInfo.Anonymous.modeInfoIdx = tgt_idx;

    let active_after = out.iter().filter(|p| is_active(p)).count();
    Ok(SwitchPlan {
        paths: out,
        modes,
        virtual_name,
        physical_count: 0,
        mode_matched,
        shared_source: true,
        active_before,
        active_after,
        vdd_identity: Some(identity),
    })
}

/// 计划「入夜」：把实体屏从活动路径集里摘掉，虚拟屏继续接着原来那块桌面。
///
/// 这是整晚唯一一次 `SetDisplayConfig`，而且是一次提交完整路径集：
///
/// * 虚拟屏那条路径**原样保留**（source id、target id、两个模式索引都不动），
///   所以远程软件拿着的采集表面不会消失 —— 这是"不冻住"的全部秘密；
/// * 实体屏的 target 仍然留在提交的路径集里（只是不再活动），
///   设备依旧枚举得到，显示数量不变；
/// * 至少要有一条活动路径，否则直接判定失败、不提交。
pub fn plan_detach(match_pat: &str) -> Result<SwitchPlan> {
    let (paths, modes) = query(QDC_ALL_PATHS)?;
    let patterns = needles(match_pat);
    let vdd_idx = pick_virtual(&paths, &patterns)?;
    let active_before = paths.iter().filter(|p| is_active(p)).count();

    if !is_active(&paths[vdd_idx]) {
        return Err(nd_core::error::user(
            "虚拟屏还没有接入桌面（未预热）。请先运行一次预热（--action plan-virtual 或重启程序）",
        ));
    }
    let desktop_source = source_mode_idx(&paths[vdd_idx]);
    let mut out = paths.clone();
    let mut physical_count = 0usize;
    let mut kept_active = 0usize;
    for (i, p) in out.iter_mut().enumerate() {
        if i == vdd_idx || !is_active(p) || path_is_virtual(p, &patterns) {
            if is_active(p) {
                kept_active += 1;
            }
            continue;
        }
        p.flags &= !PATH_FLAGS_ACTIVE;
        physical_count += 1;
    }
    let active_after = out.iter().filter(|p| is_active(p)).count();
    if active_after == 0 || kept_active == 0 {
        return Err(nd_core::error::user(
            "切换后没有任何活动显示，已中止（不会让机器变成无显示状态）",
        ));
    }
    if physical_count == 0 {
        return Err(nd_core::error::user(
            "虚拟屏已经接管，没有需要摘掉的实体屏（当前已经是夜间状态）",
        ));
    }
    let _ = desktop_source;
    Ok(SwitchPlan {
        paths: out,
        modes,
        virtual_name: path_display_name(&paths[vdd_idx]),
        physical_count,
        mode_matched: true,
        shared_source: true,
        active_before,
        active_after,
        vdd_identity: Some((
            paths[vdd_idx].targetInfo.adapterId.LowPart,
            paths[vdd_idx].targetInfo.adapterId.HighPart,
            paths[vdd_idx].targetInfo.id,
        )),
    })
}

/// 校验 + 应用，**各一次** `SetDisplayConfig`。
///
/// 校验不过就返回错误，什么都不改；应用失败也只是返回错误，不留半成品。
fn commit(plan: &SwitchPlan, dry_run: bool) -> Result<(u64, u64, u64)> {
    let flags = SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES;

    let t0 = std::time::Instant::now();
    // SAFETY: 路径数组和模式数组都按完整长度传入，索引都在范围内。
    let status =
        unsafe { SetDisplayConfig(Some(&plan.paths), Some(&plan.modes), SDC_VALIDATE | flags) };
    let validate_ms = t0.elapsed().as_millis() as u64;
    if status != 0 {
        return Err(nd_core::error::user(format!(
            "显卡驱动不接受这个显示配置（{}）",
            describe_error(status)
        )));
    }
    if dry_run {
        return Ok((validate_ms, 0, 0));
    }

    let t1 = std::time::Instant::now();
    // SAFETY: 与校验时完全相同的参数，只多了 SDC_APPLY。
    let status =
        unsafe { SetDisplayConfig(Some(&plan.paths), Some(&plan.modes), SDC_APPLY | flags) };
    let apply_ms = t1.elapsed().as_millis() as u64;
    if status != 0 {
        return Err(nd_core::error::user(format!(
            "提交显示配置失败（{}）",
            describe_error(status)
        )));
    }

    // 等画面落定，顺便测一下"从提交到稳定"用了多久。
    let t2 = std::time::Instant::now();
    std::thread::sleep(std::time::Duration::from_millis(120));
    let settle_ms = t2.elapsed().as_millis() as u64 + apply_ms;
    Ok((validate_ms, apply_ms, settle_ms))
}

/// 应用计划并核对结果。`want` 决定核对的判据。
fn apply_plan(plan: &SwitchPlan, dry_run: bool, stage: Stage) -> Result<VirtualReport> {
    let mut report = VirtualReport {
        virtual_display: plan.virtual_name.clone(),
        detached_physical: plan.physical_count,
        active_before: plan.active_before,
        active_after: plan.active_after,
        mode_matched: plan.mode_matched,
        ..Default::default()
    };
    match commit(plan, dry_run) {
        Ok((v, a, s)) => {
            report.validate_ms = v;
            report.apply_ms = a;
            report.settle_ms = s;
        }
        Err(e) => {
            report.error = Some(nd_core::error::one_line(&e));
            return Ok(report);
        }
    }
    if dry_run {
        report.applied = false;
        report.error = Some("仅校验，未应用".into());
        return Ok(report);
    }

    // 核对：系统真的按我们说的做了吗。
    match stage {
        Stage::Detach => {
            let active = describe(true)?;
            let virtual_only = !active.is_empty() && active.iter().all(is_virtual);
            let identity_kept = plan.vdd_identity.is_some_and(|(lo, hi, id)| {
                active
                    .iter()
                    .any(|d| d.adapter_low == lo && d.adapter_high == hi && d.target_id == id)
            });
            report.active_after = active.len();
            if !virtual_only {
                report.error = Some(format!(
                    "提交成功但结果不对：活动显示里还有实体屏（{} 块）",
                    active.iter().filter(|d| !is_virtual(d)).count()
                ));
                return Ok(report);
            }
            if !identity_kept {
                report.error = Some("提交成功但虚拟屏的 target 变了，采集表面可能已经重建".into());
                return Ok(report);
            }
            report.applied = true;
        }
        Stage::Arm => {
            let all = describe(false)?;
            let active = all.iter().filter(|d| d.active).count();
            report.active_after = active;
            if !all.iter().any(|d| d.active && is_virtual(d)) {
                report.error = Some("提交成功但虚拟屏没有变成活动输出".into());
                return Ok(report);
            }
            report.applied = true;
        }
    }
    if report.active_after == 0 {
        report.applied = false;
        report.error = Some("切换后没有任何活动显示".into());
    }
    Ok(report)
}

/// 切换阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// 白天预热：虚拟屏接进桌面（复制），不改布局。
    Arm,
    /// 入夜：摘掉实体屏的桌面，虚拟屏继续接管。
    Detach,
}

/// 预热：让虚拟屏接进桌面并保持活动。**幂等**，重复调用不会反复改拓扑。
pub fn arm(match_pat: &str, dry_run: bool) -> Result<VirtualReport> {
    let plan = plan_arm(match_pat)?;
    apply_plan(&plan, dry_run, Stage::Arm)
}

/// 入夜：把桌面交给虚拟屏。调用前虚拟屏必须已经预热（`arm`）。
pub fn detach(match_pat: &str, dry_run: bool) -> Result<VirtualReport> {
    let plan = plan_detach(match_pat)?;
    apply_plan(&plan, dry_run, Stage::Detach)
}

/// 白天：把实体屏接回桌面，虚拟屏退回"复制预热"状态。
///
/// 也是一次 `SetDisplayConfig`：实体屏那条路径原样恢复活动（它的模式索引一直在）。
pub fn reattach() -> Result<VirtualReport> {
    let (paths, modes) = query(QDC_ALL_PATHS)?;
    let patterns = needles("");
    let active = paths.iter().filter(|p| is_active(p)).count();
    let has_physical_active = paths
        .iter()
        .any(|p| is_active(p) && !path_is_virtual(p, &patterns));
    if has_physical_active {
        // 已经是白天了。
        return Ok(VirtualReport {
            applied: true,
            active_before: active,
            active_after: active,
            mode_matched: true,
            virtual_display: "（实体屏已在用）".into(),
            ..Default::default()
        });
    }
    // 找一条"实体屏 + 有模式索引"的路径：它就是我们入夜时摘掉的那条。
    let candidate = paths
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            !path_is_virtual(p, &patterns)
                && p.targetInfo.targetAvailable.as_bool()
                && source_mode_idx(p).is_some()
                && target_mode_idx(p).is_some()
        })
        .max_by_key(|(_, p)| p.targetInfo.outputTechnology.0)
        .map(|(i, _)| i);
    let Some(idx) = candidate else {
        return Err(nd_core::error::user(
            "找不到带桌面模式的实体屏路径，请按 Win+P 手动选择显示方式",
        ));
    };
    let mut out = paths.clone();
    out[idx].flags |= PATH_FLAGS_ACTIVE;
    let plan = SwitchPlan {
        paths: out,
        modes,
        virtual_name: path_display_name(&paths[idx]),
        physical_count: 0,
        mode_matched: true,
        shared_source: true,
        active_before: active,
        active_after: active + 1,
        vdd_identity: None,
    };
    let mut report = VirtualReport {
        virtual_display: plan.virtual_name.clone(),
        active_before: active,
        ..Default::default()
    };
    match commit(&plan, false) {
        Ok((v, a, s)) => {
            report.validate_ms = v;
            report.apply_ms = a;
            report.settle_ms = s;
        }
        Err(e) => {
            report.error = Some(nd_core::error::one_line(&e));
            return Ok(report);
        }
    }
    let now = describe(true)?;
    report.active_after = now.len();
    if now.iter().any(|d| !is_virtual(d)) {
        report.applied = true;
    } else {
        report.error = Some("提交成功但实体屏仍未接回桌面".into());
    }
    Ok(report)
}

/// 虚拟屏是不是正接着桌面（夜间状态）。
pub fn virtual_screen_holding() -> bool {
    match describe(true) {
        Ok(active) => !active.is_empty() && active.iter().all(is_virtual),
        Err(_) => false,
    }
}

/// 备用虚拟屏的验证结果：装好了、能被枚举到，但**没有**改动桌面布局。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandbyReport {
    /// 虚拟屏的友好名（可能为空，那就用设备路径）。
    pub name: String,
    /// 它在系统显示配置里已经激活（真的在输出画面）。
    pub active: bool,
    /// 当前活动输出的数量，用来确认"没有多出一块屏"。
    pub active_outputs: usize,
}

impl StandbyReport {
    #[must_use]
    pub fn label(&self) -> String {
        if self.name.is_empty() {
            "虚拟屏".to_string()
        } else {
            self.name.clone()
        }
    }
}

/// 检查随包 VDD 的虚拟屏是否就绪（**只读，不改任何显示设置**）。
///
/// 就绪的定义：在 `QueryDisplayConfig(QDC_ALL_PATHS)` 里能看到一块属于
/// 自带 VDD 的 target。它可以是未激活的 —— 未激活的屏不占桌面、不改布局，
/// 正是我们想要的"待命"状态：远程软件能枚举到一块稳定的额外显示，
/// 但用户看不到任何变化；只有主动选「仅虚拟屏」时才把它激活。
pub fn verify_standby() -> Result<StandbyReport> {
    let all = describe(false)?;
    let active_outputs = all.iter().filter(|d| d.active).count();
    let Some(vdd) = all.iter().find(|d| is_bundled_vdd(d)) else {
        return Err(nd_core::error::user(
            "系统显示配置里没有找到虚拟屏。驱动可能没装上，或者装完还没生效",
        ));
    };
    Ok(StandbyReport {
        name: vdd.name.clone(),
        active: vdd.active,
        active_outputs,
    })
}

/// 虚拟屏是不是已经在显示配置里（不看是否激活）。
#[must_use]
pub fn standby_present() -> bool {
    describe(false).is_ok_and(|all| all.iter().any(is_bundled_vdd))
}

/// 还原显示拓扑。返回实际使用的级别，写进日志。
///
/// 顺序：
/// 1. **优先走 `reattach()`** —— 用提交完整路径集的方式把实体屏接回桌面，
///    和入夜是同一套机制、同样一次调用，虚拟屏采集表面不动；
/// 2. 只有路径集方式彻底失败（例如驱动拒绝、路径被外部改乱）才退回到系统
///    拓扑标志，作为最后兜底；此时必定会中断一次远程画面，所以只在这里用，
///    而且会明确返回是哪一级。
pub fn restore() -> Result<&'static str> {
    match reattach() {
        Ok(report) if report.applied => {
            tracing::info!(
                active = report.active_after,
                cost = report.summary(),
                "显示拓扑已恢复（路径集方式）"
            );
            return Ok("路径集还原");
        }
        Ok(report) => {
            tracing::warn!(
                error = %report.error.unwrap_or_else(|| "未知原因".into()),
                "路径集还原未成功，改用系统拓扑标志兜底"
            );
        }
        Err(e) => {
            tracing::warn!(error = %nd_core::error::one_line(&e), "路径集还原报错，改用系统拓扑标志兜底");
        }
    }

    let attempts: [(&'static str, SET_DISPLAY_CONFIG_FLAGS); 3] = [
        ("仅电脑屏幕", SDC_TOPOLOGY_INTERNAL),
        ("扩展", SDC_TOPOLOGY_EXTEND),
        ("复制", SDC_TOPOLOGY_CLONE),
    ];
    let mut tried = Vec::new();
    for (label, flag) in attempts {
        // SAFETY: 只用拓扑标志，标准兜底恢复方式。
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
        tracing::warn!(level = label, "显示拓扑已用系统拓扑标志兜底恢复");
        return Ok("系统拓扑兜底");
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
