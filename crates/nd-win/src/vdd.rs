//! 虚拟显示驱动（VDD）的安装、卸载与验证。
//!
//! **4.0 起这是必需组件**，不是可选项：
//! * guard 模式本身**不**切到虚拟屏（那正是旧版冻结远程的原因），
//!   虚拟屏只是待命 —— 装好、验证过、能被 `QueryDisplayConfig` 枚举到，
//!   但**不激活**，所以桌面布局和分辨率一点不变；
//! * 「仅虚拟屏」模式要能立刻用，靠的就是这块待命的屏幕；
//! * 一些老远程软件重连后需要一块"稳定的额外显示"才肯出画面。
//!
//! 安装走包内自带的 `install_vdd.ps1`（`pnputil` 静默安装 + 创建 `Root\\MttVDD` 设备），
//! 脚本会校验驱动文件的 SHA-256 和目录签名。装完必须用
//! [`verify_ready`] 确认虚拟屏真的出现在显示配置里，才算这一步成功。

#![cfg(windows)]

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::sysutil;

/// 硬件 ID，用来判断"自家 VDD"是否已安装（区别于 UU 等第三方虚拟屏）。
pub const HWID: &str = r"Root\MttVDD";

/// 脚本可能所在的几个位置。
fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(dir) = std::env::var("NIGHTDISPLAY_RESOURCES") {
        out.push(PathBuf::from(dir).join("install_vdd.ps1"));
    }
    // 安装包目录：<exe 目录>\resources\
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.join("resources").join("install_vdd.ps1"));
            out.push(dir.join("install_vdd.ps1"));
        }
    }
    out
}

/// 找到 `install_vdd.ps1`。
#[must_use]
pub fn script_path() -> Option<PathBuf> {
    candidates().into_iter().find(|p| p.is_file())
}

/// 随包的驱动目录（`resources` 里有 `MttVDD.inf` 和脚本）。
#[must_use]
pub fn source_dir() -> Option<PathBuf> {
    script_path().and_then(|p| p.parent().map(Path::to_path_buf))
}

/// 手动安装的说明（每一步都能照抄）。步骤失败时原样给用户看。
#[must_use]
pub fn manual_steps() -> String {
    let dir = source_dir().map_or_else(
        || "<安装目录>\\resources".to_string(),
        |d| d.display().to_string(),
    );
    format!(
        "手动安装（用管理员身份打开 PowerShell，逐行执行）：\n\
         1) pnputil /add-driver \"{dir}\\MttVDD.inf\" /install\n\
         2) 设备管理器 → 操作 → 添加过时硬件 → 显示适配器 → 从磁盘安装 → 选择 {dir}\\MttVDD.inf\n\
         3) 也可以在命令行运行：\n     \"{exe}\" --action install-vdd\n\
         驱动下载页（包内文件缺失时）：{}\n\
         装完回到向导点「重试」，或运行 \"{exe}\" --action repair",
        nd_core::REPO_URL,
        exe = crate::sysutil::exe_path().display()
    )
}

/// 驱动是否已安装且状态正常。
#[must_use]
pub fn installed() -> bool {
    match query() {
        Some(state) => state.installed,
        None => false,
    }
}

/// `install_vdd.ps1 -Status` 的结果。
#[derive(Debug, Clone, Default)]
pub struct VddState {
    pub installed: bool,
    /// `Get-PnpDevice` 的原始输出（中文提示里会引用）。
    pub detail: String,
}

/// 查询驱动状态。脚本不在或 PowerShell 出错时返回 None。
#[must_use]
pub fn query() -> Option<VddState> {
    let script = script_path()?;
    let out = sysutil::run_powershell_script(&script, &["-Status"]).ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let installed = out.status.success() && !text.contains("VDD not installed");
    Some(VddState {
        installed,
        detail: text,
    })
}

/// 安装驱动。**需要管理员权限**，所以只能由提升助手调用。
///
/// 返回人话结论：安装成功要说明"已装好"；失败要把脚本打印的手动步骤带出来。
pub fn install() -> Result<String> {
    let Some(script) = script_path() else {
        return Err(nd_core::error::user(format!(
            "安装包里没有找到 install_vdd.ps1，无法自动安装虚拟显示驱动。\n{}",
            manual_steps()
        )));
    };
    if installed() {
        // 驱动在，但设备可能被禁用/移除：仍然验证一遍虚拟屏能不能枚举到。
        return match verify_ready() {
            Ok(report) => Ok(format!(
                "虚拟显示驱动已安装，虚拟屏 {} 已在待命（不改动桌面布局）",
                report.label()
            )),
            Err(e) => Err(nd_core::error::user(format!(
                "驱动已安装，但虚拟屏没有出现在显示配置里：{}\n{}",
                crate::error::user_text(&e, "查询显示配置失败"),
                manual_steps()
            ))),
        };
    }
    let log_dir = nd_core::paths::logs_dir();
    std::fs::create_dir_all(&log_dir).ok();
    let log_arg = log_dir.display().to_string();
    let mut args: Vec<&str> = vec!["-LogDirectory", &log_arg];
    // 用包内自带的驱动，绝不联网下载（"自带驱动、离线静默安装"是明确要求）。
    let source = source_dir().map(|d| d.display().to_string());
    if let Some(dir) = source.as_deref() {
        args.push("-Source");
        args.push(dir);
    }
    let out = sysutil::run_powershell_script(&script, &args)?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let log_file = log_dir.join("install_vdd.log");
    let tail = stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .to_string();

    if out.status.success() && installed() {
        // 装完必须真的能在显示配置里看到虚拟屏，否则等于没装好。
        match verify_ready() {
            Ok(report) => {
                tracing::info!(display = %report.label(), "虚拟显示驱动安装成功");
                return Ok(format!(
                    "虚拟显示驱动已安装，虚拟屏 {} 已在待命（不改动桌面布局）",
                    report.label()
                ));
            }
            Err(e) => {
                tracing::warn!(error = %e, "驱动脚本成功但虚拟屏没有出现在显示配置里");
                return Err(nd_core::error::user(format!(
                    "驱动装上了，但系统显示配置里没有出现虚拟屏（{}）。\
                     常见原因是设备被禁用，或需要重启一次。\n{}",
                    crate::error::user_text(&e, "查询显示配置失败"),
                    manual_steps()
                )));
            }
        }
    }
    tracing::warn!(
        code = out.status.code(),
        stdout = %stdout,
        stderr = %stderr,
        "虚拟显示驱动安装失败"
    );
    Err(nd_core::error::user(format!(
        "虚拟显示驱动没有装上：{}\n详情见 {} 或 {}\n{}",
        if tail.is_empty() {
            "脚本返回失败".to_string()
        } else {
            tail
        },
        log_file.display(),
        script.display(),
        manual_steps()
    )))
}

/// 卸载驱动（恢复"没装过"的状态）。需要管理员权限。
pub fn uninstall() -> Result<String> {
    let Some(script) = script_path() else {
        return Err(nd_core::error::user(
            "安装包里没有找到 install_vdd.ps1，无法自动卸载虚拟显示驱动",
        ));
    };
    let out = sysutil::run_powershell_script(&script, &["-Uninstall"])?;
    if !out.status.success() {
        return Err(nd_core::error::user(format!(
            "卸载虚拟显示驱动失败：{}",
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .last()
                .unwrap_or("脚本返回失败")
        )));
    }
    Ok("虚拟显示驱动已卸载".into())
}

/// 验证虚拟屏真的可以待命：能枚举到，且**没有**改变桌面布局。
///
/// 这一步是向导的验收标准：只有它通过，第 2 步才算 ok。
pub fn verify_ready() -> Result<crate::topology::StandbyReport> {
    crate::topology::verify_standby()
}

/// 给状态文本用的一句话。
#[must_use]
pub fn describe() -> &'static str {
    if script_path().is_none() {
        "安装包缺少驱动脚本"
    } else if !installed() {
        "未安装（必须安装：向导第 2 步）"
    } else if crate::topology::standby_present() {
        "已安装，虚拟屏待命中"
    } else {
        "驱动已装，但虚拟屏还没有出现在显示配置里"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_steps_include_pnputil_and_repo() {
        let text = manual_steps();
        assert!(text.contains("pnputil"));
        assert!(text.contains("设备管理器"));
        assert!(text.contains(nd_core::REPO_OWNER));
    }

    #[test]
    fn hwid_matches_bundled_driver() {
        assert_eq!(HWID, r"Root\MttVDD");
    }

    #[test]
    fn candidates_are_absolute_paths() {
        for path in candidates() {
            assert!(path.is_absolute(), "{}", path.display());
        }
    }
}
