//! NightDisplay 命令行入口。
//!
//! 这一层只做三件事：解析参数、初始化日志、把动作交给 `app` 模块。
//! 所有业务判断在 `nd-core`，所有 Windows 调用在 `nd-win`，
//! 所以第二阶段加 Tauri 图形界面时这个文件基本不用动。
//!
//! 退出码约定（与旧版一致）：
//! * `0` 成功；
//! * `1` 出错（屏幕状态已尽量保持安全）；
//! * `2` 黑屏锁定中，拒绝了本地恢复动作。

#![cfg_attr(not(windows), allow(dead_code))]

use std::process::ExitCode;

#[cfg(windows)]
mod app;

fn main() -> ExitCode {
    #[cfg(not(windows))]
    {
        eprintln!("错误：NightDisplay 4.x 目前只支持 Windows 10 2004 及以上版本。");
        return ExitCode::from(1);
    }

    #[cfg(windows)]
    {
        app::main_entry()
    }
}
