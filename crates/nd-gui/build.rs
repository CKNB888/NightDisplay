//! Tauri 的构建脚本：生成 ACL、能力清单和 Windows 资源。
//!
//! 只在 Windows 目标上真正执行 —— 这个 crate 只在 Windows 上运行；Linux CI
//! 只是为了能 `cargo clippy --target x86_64-pc-windows-msvc` 先把类型查干净。

fn main() {
    println!("cargo:rerun-if-changed=tauri.conf.json");
    println!("cargo:rerun-if-changed=capabilities");
    println!("cargo:rerun-if-changed=permissions");
    println!("cargo:rerun-if-changed=icons");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // 界面自己实现的命令要在 ACL 里有对应的 allow-* 权限。
    let attributes = tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(&APP_COMMANDS));
    tauri_build::try_build(attributes).expect("tauri-build 失败");
}

/// 与 `src/commands.rs` 里 `#[tauri::command]` 的函数名一一对应。
const APP_COMMANDS: [&str; 19] = [
    "snapshot",
    "onboarding_state",
    "log_index",
    "read_logs",
    "night_now",
    "day_now",
    "pause_schedule",
    "test_mode",
    "activate_schedule",
    "set_settings",
    "run_setup",
    "repair",
    "confirm_remote",
    "finish_onboarding",
    "export_diag",
    "reveal",
    "open_url",
    "quit_app",
    "open_window",
];
