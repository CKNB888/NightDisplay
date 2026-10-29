//! `NightDisplay` Windows 平台层。
//!
//! 只在 Windows 上编译；非 Windows 下 `platform` / `helper` 等对外接口都返回
//! "仅支持 Windows"错误，这样纯逻辑的 `nd-core` 和它的测试可以在 Linux CI 上跑。
//!
//! 分层约定：
//! * **GUI/CLI 永远不以管理员身份运行**（除非用户为了别的目的自己提升）；
//! * 需要特权的动作（装驱动、登记计划任务、写 HKCU Run 之外的系统设置）
//!   全部经由一个常驻的**提升助手**（[`helper`]），IPC 面很小且只接受白名单命令；
//! * 黑幕、gamma、背光、键鼠钩子这些**不需要管理员**的动作在主进程里直接做。

// Lint policy (see `docs/architecture.md` § "Lint policy").
//
// This crate is a thin, typed wrapper over Win32: the casts and the boolean
// bundles come from the API shapes, not from our abstractions.
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::module_name_repetitions,
    clippy::struct_excessive_bools,
    clippy::too_many_lines,
    // `DISPLAYCONFIG_*` fields are `u32`/`i32` by definition, LUID halves are
    // `u32`/`i32`, and `SetWindowLongPtr` takes an `isize`. Every cast below is
    // either a Win32 ABI conversion or a value we just bounds-checked.
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_lossless,
    clippy::doc_markdown
)]
// 测试里构造配置时逐字段赋值更贴近"用户改一项"的场景，不必强求结构体字面量。
#![cfg_attr(test, allow(clippy::field_reassign_with_default))]
#![cfg_attr(not(windows), allow(unused))]

pub mod error;

#[cfg(windows)]
pub mod autostart;
#[cfg(windows)]
pub mod brightness;
#[cfg(windows)]
pub mod diag;
#[cfg(windows)]
pub mod gamma;
#[cfg(windows)]
pub mod hardware;
pub mod helper;
#[cfg(windows)]
pub mod input;
#[cfg(windows)]
pub mod overlay;
#[cfg(windows)]
pub mod pipe;
#[cfg(windows)]
pub mod platform;
#[cfg(windows)]
pub mod power;
pub mod power_plan;
#[cfg(windows)]
pub mod runtime;
pub mod screen_state;
#[cfg(windows)]
pub mod setup;
pub mod shutdown;
#[cfg(windows)]
pub mod sysutil;
#[cfg(windows)]
pub mod tasks;
#[cfg(windows)]
pub mod topology;
#[cfg(windows)]
pub mod update;
#[cfg(windows)]
pub mod vdd;
#[cfg(windows)]
pub mod watchdog;

#[cfg(all(test, windows))]
mod test_support;

#[cfg(windows)]
pub use platform::{current_capabilities, WinPlatform};

/// 非 Windows 上的统一错误。
#[must_use]
pub fn unsupported() -> anyhow::Error {
    nd_core::error::user("NightDisplay 4.x 目前只支持 Windows 10 2004 及以上版本。")
}
