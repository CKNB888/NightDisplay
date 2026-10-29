//! `NightDisplay` 引擎核心：纯逻辑，不依赖任何平台 API。
//!
//! 这一层负责配置、时间计划、组合键、模式选择、IPC 协议和恢复决策，
//! 全部可以在 Linux CI 上跑单元测试。真正的 Windows 调用在 `nd-win`。
//!
//! # 虚拟屏夜间切换是什么
//!
//! 用户要的是"人走开时本机屏幕是黑的，远程软件照常能看到并操作桌面"。
//! 做法不是在本机屏幕上盖一层黑幕，而是把桌面**搬到一块虚拟显示器上**：
//! 夜间的所有窗口、任务栏都渲染在虚拟屏，实体屏的输出被摘掉，本机就是
//! 真的没画面（不是调暗、不是遮住），远程工具采集的则是那块虚拟屏。
//!
//! 三种模式，`Mode` 里有完整定义：
//!
//! * [`mode::Mode::VirtualScreen`] —— 默认。桌面交给虚拟屏，实体屏不再输出。
//! * [`mode::Mode::Overlay`] —— 兜底。虚拟屏在这台机器上切不过去时自动降级，
//!   本机盖全屏黑幕 + gamma 压黑 + DDC 背光归零，远程采集不受影响。
//! * [`mode::Mode::ScreenOff`] —— 用户显式选择，只关显示器电源，远程会跟着断。
//!
//! 降级顺序只由 [`mode::attempt_chain`] 决定：`auto` 是"先虚拟屏，不行才黑幕"。
//! 用户手动选过的模式不会被悄悄改掉 —— 选虚拟屏而虚拟屏不可用时宁可报错。
//!
//! # 为什么拓扑改动必须是一次调用
//!
//! 远程画面最怕的是"采集表面被重建"：`SetDisplayConfig` 只要分成两次提交，
//! 中间那一刻就是一个不完整的显示配置，远端会看到画面冻结、黑一帧、或者
//! 连接直接重连。所以：
//!
//! * 白天就把虚拟屏**预热**好（复制模式接进桌面），入夜只是把实体屏那条
//!   路径的 `ACTIVE` 位清掉 —— 一次 `SetDisplayConfig`，活动 target 数不变，
//!   虚拟屏的 target/source/模式索引一个字节都没动，采集表面无从重建；
//! * 提交前先 `SDC_VALIDATE`，校验不过就一个字都不改；
//! * 只提交**完整路径集**（`SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES`），
//!   从不用 `SDC_TOPOLOGY_*`：拓扑标志是让系统自己挑配置，挑出来的东西不可预测。
//!
//! 这套机制的平台实现见 `nd-win` 的 `topology` 模块，模块头有更细的约束说明。
//!
//! # 崩溃与断电
//!
//! 夜间状态必须能从异常退出里恢复：原始 gamma、背光、显示拓扑在**改动之前**
//! 就写进 `screen-state.json`，看门狗和下一次启动都从这份文件还原。
//! 状态机、恢复决策都在 [`engine`]，不在这里重复。
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
