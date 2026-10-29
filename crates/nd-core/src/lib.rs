//! NightDisplay 引擎核心：纯逻辑，不依赖任何平台 API。
//!
//! 这一层负责配置、时间计划、组合键、模式选择、IPC 协议和恢复决策，
//! 全部可以在 Linux CI 上跑单元测试。真正的 Windows 调用在 `nd-win`。
//!
//! 设计约束（来自实机反馈）：
//! * 默认模式 `guard` 绝不改显示拓扑，所以远程画面不会断流、显示器不会重新同步闪一下。
//! * `virtual_only` / `screen_off` 只能由用户显式选择，永远不进自动降级链。

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
pub fn release_url(tag: &str) -> String {
    format!("{REPO_URL}/releases/tag/{tag}")
}

/// 最新版本发布页（不跟随 API 返回的链接）。
pub fn latest_release_url() -> String {
    format!("{REPO_URL}/releases/latest")
}
