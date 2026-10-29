//! Tauri 的构建脚本：只在 Windows 目标上生成资源（图标、版本信息）。
//!
//! 非 Windows 目标直接跳过 —— 这个 crate 只在 Windows 上运行，Linux CI 只是
//! 为了能 `cargo clippy --target x86_64-pc-windows-msvc`，不需要真的打包。

fn main() {
    #[cfg(windows)]
    tauri_build::build();

    #[cfg(not(windows))]
    println!("cargo:rerun-if-changed=build.rs");
}
