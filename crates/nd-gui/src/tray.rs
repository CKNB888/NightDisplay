//! 托盘图标与托盘菜单。
//!
//! 关窗只是收起界面，真正退出走托盘菜单 —— 这是"夜间黑屏"类工具该有的形态：
//! 用户不会接受"不小心点掉窗口，晚上就没人管屏幕了"。
//!
//! 菜单五项刻意保持最少：立即黑屏 / 恢复 / 暂停 / 打开设置 / 退出。
//! 每一项都走 `commands` 里同一条路径（优先交给常驻进程），
//! 所以托盘和界面永远不会做出不一致的事。

use tauri::menu::MenuBuilder;
use tauri::tray::{TrayIconBuilder, TrayIconEvent};
use tauri::webview::WebviewWindow;
use tauri::{AppHandle, Emitter as _, Manager as _};

use crate::window_state;

/// 托盘菜单项 id。
pub const MENU_NIGHT: &str = "tray-night";
pub const MENU_DAY: &str = "tray-day";
pub const MENU_PAUSE: &str = "tray-pause";
pub const MENU_SETTINGS: &str = "tray-settings";
pub const MENU_QUIT: &str = "tray-quit";

/// 主窗口的 label（与 `tauri.conf.json` 一致）。
pub const MAIN_WINDOW: &str = "main";

/// 装托盘图标与菜单。已经装过就什么都不做（单实例重复调用是安全的）。
pub fn install(app: &AppHandle) -> tauri::Result<()> {
    if app.tray_by_id("main").is_some() {
        return Ok(());
    }
    let menu = MenuBuilder::new(app)
        .text(MENU_NIGHT, "立即黑屏")
        .text(MENU_DAY, "恢复显示")
        .text(MENU_PAUSE, "暂停今天的计划")
        .separator()
        .text(MENU_SETTINGS, "打开设置")
        .separator()
        .text(MENU_QUIT, "退出 NightDisplay")
        .build()?;

    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip("NightDisplay —— 夜间本地黑屏，远程照常")
        .menu(&menu)
        // 左键点图标直接开界面，右键才是菜单：和 Windows 上的习惯一致。
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| handle_menu(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::DoubleClick { .. }
            | TrayIconEvent::Click {
                button: tauri::tray::MouseButton::Left,
                button_state: tauri::tray::MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// 菜单点击分发。
fn handle_menu(app: &AppHandle, id: &str) {
    match id {
        MENU_NIGHT => run_quietly("night", "立即黑屏", app),
        MENU_DAY => run_quietly("day", "恢复显示", app),
        MENU_PAUSE => run_quietly("pause", "暂停今天的计划", app),
        MENU_SETTINGS => {
            show_main_window(app);
            // 界面若已经在跑，直接跳到设置页；没在跑时前端会在启动后自己读 hash。
            let _ = app.emit("navigate", "settings");
        }
        MENU_QUIT => quit(app),
        other => tracing::debug!(id = other, "托盘菜单项没有对应动作"),
    }
}

/// 托盘动作失败时只提示一句，不弹窗口也不写文件。
fn run_quietly(action: &str, label: &str, app: &AppHandle) {
    match crate::commands::daemon_call(action, None) {
        Ok(text) => {
            let text = text.unwrap_or_else(|| "已处理".to_string());
            tracing::info!(action, %text, "托盘动作完成");
            let _ = app.emit("toast", format!("{label}：{text}"));
        }
        Err(e) => {
            let text = nd_core::error::one_line(&e);
            tracing::warn!(action, error = %text, "托盘动作失败");
            let _ = app.emit("toast", format!("{label}失败：{text}"));
        }
    }
}

/// 退出整个界面进程（夜间的黑屏在常驻进程里，不受影响）。
fn quit(app: &AppHandle) {
    if let Some(view) = main_view(app) {
        save_geometry(&view);
    }
    tracing::info!("用户从托盘退出界面");
    app.exit(0);
}

/// 把主窗口叫到前台（首次启动、单实例第二次启动、双击托盘都用它）。
pub fn show_main_window(app: &AppHandle) {
    let Some(window) = main_view(app) else {
        tracing::warn!("主窗口不存在，无法显示");
        return;
    };
    // 首次显示时先摆回上次的大小与位置，再显示，避免用户看到窗口跳一下。
    if !window.is_visible().unwrap_or(false) {
        let geometry = window_state::load();
        if geometry.is_sane() {
            window_state::restore(&window, geometry);
        }
    }
    window.show().ok();
    window.unminimize().ok();
    window.set_focus().ok();
}

/// 关窗前记下大小与位置。窗口已经不在了就跳过。
pub fn save_geometry<W: window_state::WindowLike + ?Sized>(window: &W) {
    if let Some(geometry) = window_state::capture(window) {
        window_state::save(geometry);
    }
}

/// 主窗口（带 webview 的那个），拿不到就是还没建好。
fn main_view(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(MAIN_WINDOW)
}

/// 窗口几何的读取入口，给命令层用（`snapshot` 里带上，前端可显示"上次位置"）。
#[must_use]
pub fn last_geometry() -> window_state::Geometry {
    window_state::load()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_ids_are_stable_and_distinct() {
        // 菜单 id 一旦改了，托盘点击就会全部失效；这里钉住它们。
        let ids = [MENU_NIGHT, MENU_DAY, MENU_PAUSE, MENU_SETTINGS, MENU_QUIT];
        for (i, a) in ids.iter().enumerate() {
            for b in ids.iter().skip(i + 1) {
                assert_ne!(a, b, "菜单 id 不能重复");
            }
        }
        assert_eq!(MAIN_WINDOW, "main");
    }

    #[test]
    fn geometry_roundtrip() {
        crate::test_home::with_temp_home(|_| {
            let g = window_state::Geometry {
                x: 40,
                y: 60,
                width: 1080,
                height: 720,
                maximized: true,
            };
            window_state::save(g);
            assert_eq!(last_geometry(), g);
        });
    }
}
