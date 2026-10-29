//! `NightDisplay` 引擎核心：纯逻辑，不依赖任何平台 API。
//!
//! 这一层负责配置、时间计划、组合键、模式选择、IPC 协议和恢复决策，
//! 全部可以在 Linux CI 上跑单元测试。真正的 Windows 调用在 `nd-win`。
//!
//! 设计约束（来自实机反馈）：
//! * 默认模式 `guard` 绝不改显示拓扑，所以远程画面不会断流、显示器不会重新同步闪一下。
//! * `virtual_only` / `screen_off` 只能由用户显式选择，永远不进自动降级链。

// Lint policy (see `docs/architecture.md` § "Lint policy" for the reasoning).
//
// `-W clippy::pedantic` is on in CI, so the handful of lints we do not want are
// listed here once, per crate, instead of being silenced at 200 call sites.
#![allow(
    // Chinese-facing library: every fallible function is called from our own
    // binary, and a `# Errors` section per function would restate the type.
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    // Predicates on small `Copy` enums; adding `#[must_use]` to all 60 of them
    // is noise, and the compiler cannot warn about the ones that matter.
    clippy::must_use_candidate,
    // Module names intentionally repeat the domain (`mode::Mode`); renaming them
    // to `ModeKind`/`ModeChoice` would read worse at every call site.
    clippy::module_name_repetitions,
    // `Status`, `Capabilities` and `CheckReport` really are sets of independent
    // boolean facts gathered from Win32; splitting them would add indirection.
    clippy::struct_excessive_bools,
    // The night/restore state machines are linear and heavily commented; they
    // are easier to audit as one function than split into six-argument helpers.
    clippy::too_many_lines,
    // Time arithmetic on `u32`/`i64` minutes and seconds; the ranges are checked
    // by `MinuteOfDay::from_str` and `validate` before anything is cast.
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_lossless,
    // Documentation is Chinese prose; wrapping every English word in backticks
    // makes it harder to read, not easier.
    clippy::doc_markdown
)]
// 测试里构造配置时逐字段赋值更贴近"用户改一项"的场景，不必强求结构体字面量。
#![cfg_attr(test, allow(clippy::field_reassign_with_default))]

pub mod chord;
pub mod config;
pub mod engine;
pub mod error;
pub mod ipc;
pub mod logging;
pub mod mode;
pub mod onboarding;
pub mod paths;
pub mod schedule;
pub mod state_lock;
pub mod status;
pub mod updates;

pub use error::{NdError, NdResult};

/// 程序名，同时用于数据目录、注册表值和日志文件。
pub const APP: &str = "NightDisplay";
/// 版本号与 Cargo.toml 保持一致。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// 作者署名，界面与 README 必须一致。
pub const AUTHOR: &str = "传康Kk";
/// 赞助及联系方式。
pub const CONTACT: &str = "1837620622";
/// 发布仓库，更新检查和下载链接都从这里拼，不信任 API 返回的地址。
pub const REPO_OWNER: &str = "CKNB888";
pub const REPO_NAME: &str = "NightDisplay";
pub const REPO_URL: &str = "https://github.com/CKNB888/NightDisplay";

/// 拼出某个标签对应的发布页地址。
#[must_use]
pub fn release_url(tag: &str) -> String {
    format!("{REPO_URL}/releases/tag/{tag}")
}

/// 最新版本发布页（不跟随 API 返回的链接）。
#[must_use]
pub fn latest_release_url() -> String {
    format!("{REPO_URL}/releases/latest")
}
