//! `NightDisplay` 图形界面（Tauri 2 + WebView2）。
//!
//! # 这个 crate 的定位
//!
//! 界面**不含任何业务逻辑**：
//!
//! * 什么时候该黑、该用哪个模式、该不该降级 —— 全在 `nd-core` 的引擎里；
//! * 改屏幕、装驱动、登记计划任务 —— 全在 `nd-win`（需要提权的走提升助手）；
//! * 这里只做三件事：读状态给前端画、把用户的操作翻译成引擎调用、托盘与窗口。
//!
//! 因此 GUI 永远**不以管理员身份运行**，也永远不直接调 Win32。
//!
//! # 低内存
//!
//! 关窗即销毁 WebView2 与窗口（只留托盘图标），夜间引擎根本不在这个进程里 ——
//! 它在常驻进程（`NightDisplay.exe --action serve`）。关窗后 GUI 进程只剩托盘，
//! 实测常驻内存与一个托盘图标相当（见 `docs/architecture.md` 的实测记录）。
//!
//! # 为什么日志页要显示"每一步"
//!
//! 用户真正会问的是"昨晚到底哪一步没成"。所以界面读的是 `session-current.json`
//! 和 `sessions.jsonl`（`nd_core::session`），而不是让人去翻日志文本。
//!
//! # 与 CLI 的关系
//!
//! 两边共用同一个可执行文件：`--action gui`（或直接双击）进界面，
//! 其余参数走命令行。界面自己的动作**不**直接执行，而是优先交给已在运行的
//! 常驻进程（本地命名管道 + 令牌）；常驻进程不在时按同样的引擎逻辑就地执行。

#![cfg(windows)]
// Lint policy (see `docs/architecture.md` § "Lint policy").
//
// A GUI shell: the "too many lines" and "many bools" lints land on view models
// that really are flat sets of facts the webview renders.
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::module_name_repetitions,
    clippy::struct_excessive_bools,
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_lossless
)]
#![cfg_attr(test, allow(clippy::field_reassign_with_default))]

pub mod commands;
pub mod settings;
pub mod state;
pub mod tray;
pub mod window_state;

use std::process::ExitCode;

use anyhow::Result;

/// 界面进程的入口：初始化日志、建窗口、进事件循环。
///
/// 返回的退出码只反映"界面进程本身"有没有正常结束；夜间的成败在常驻进程，
/// 界面退出不会影响正在进行的黑屏。
pub fn run() -> Result<ExitCode> {
    // 界面自己的日志单独一份（`gui-YYYY-MM-DD.log`）。
    if let Err(e) = nd_core::logging::init(nd_core::logging::Component::Gui) {
        // 日志起不来不该挡住打开界面。
        eprintln!("提示：界面日志没能初始化（{e}），程序继续运行。");
    }
    tracing::info!(version = nd_core::VERSION, "界面启动");

    let app = tauri::Builder::default()
        // 单实例：第二次启动只把已有窗口叫到前台，不新建进程。
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            tray::show_main_window(app);
        }))
        .setup(|app| {
            tray::install(app.handle())?;
            tray::show_main_window(app.handle());
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // 关窗 = 收起界面，引擎不受影响。真正退出走托盘菜单。
                api.prevent_close();
                let _ = window.hide();
                tray::save_geometry(window);
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::snapshot,
            commands::onboarding_state,
            commands::log_index,
            commands::read_logs,
            commands::night_now,
            commands::day_now,
            commands::pause_schedule,
            commands::test_mode,
            commands::activate_schedule,
            commands::set_settings,
            commands::run_setup,
            commands::repair,
            commands::confirm_remote,
            commands::finish_onboarding,
            commands::export_diag,
            commands::reveal,
            commands::open_url,
            commands::quit_app,
            commands::open_window,
        ])
        .build(tauri::generate_context!())
        .map_err(|e| anyhow::anyhow!("界面初始化失败：{e}"))?;

    app.run(|_handle, event| {
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            // 关掉最后一个窗口不等于退出：托盘还在。
            api.prevent_exit();
        }
    });
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_module_is_reachable() {
        // 冒烟：保证 state 的公开接口在界面 crate 里可用。
        let snap = state::build(
            &nd_core::config::Config::default(),
            &nd_core::status::Capabilities::default(),
            &nd_core::onboarding::Onboarding::default(),
            chrono::Local::now(),
        );
        assert_eq!(snap.version, nd_core::VERSION);
        let _ = ExitCode::SUCCESS;
    }
}
