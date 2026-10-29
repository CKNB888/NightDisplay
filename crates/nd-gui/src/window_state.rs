//! 窗口大小与位置：关窗前存一份，下次打开原样恢复。
//!
//! 两点讲究：
//! 1. 只在**普通窗口**状态下记录。最大化时记下的是最大化标志，而不是
//!    最大化后的像素尺寸 —— 否则取消最大化后窗口会变成一个占满屏幕的怪东西。
//! 2. 恢复前先检查这块矩形还在不在某块显示器上。拔掉外接屏或改过缩放后，
//!    上次的位置可能已经在屏幕外面，那时就放弃位置、只恢复尺寸并居中。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{PhysicalPosition, PhysicalSize, WebviewWindow, Window};

/// 存盘的窗口几何。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Geometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
}

/// 窗口比这还窄/矮就不认，避免存进去一个 0×0 导致下次打不开。
const MIN_WIDTH: u32 = 640;
const MIN_HEIGHT: u32 = 480;

impl Geometry {
    /// 这份几何数据是不是明显坏掉了。
    #[must_use]
    pub fn is_sane(&self) -> bool {
        self.width >= MIN_WIDTH && self.height >= MIN_HEIGHT
    }

    /// 窗口左上角是否落在某块显示器的工作区里（允许只有一角露出来）。
    #[must_use]
    pub fn is_visible_on(&self, screens: &[(i32, i32, u32, u32)]) -> bool {
        screens.iter().any(|(sx, sy, sw, sh)| {
            let right = *sx as i64 + i64::from(*sw);
            let bottom = *sy as i64 + i64::from(*sh);
            let x = i64::from(self.x);
            let y = i64::from(self.y);
            // 标题栏必须能被点到：左上角往里缩一点仍在屏幕内。
            x + 80 > i64::from(*sx) && x < right && y + 40 > i64::from(*sy) && y < bottom
        })
    }
}

/// 存盘路径：`<home>/gui-window.json`。
#[must_use]
pub fn file() -> PathBuf {
    nd_core::paths::home_dir().join("gui-window.json")
}

/// 读回上次的几何。读不到就用默认值（`is_sane()` 为假，调用方会忽略）。
#[must_use]
pub fn load() -> Geometry {
    std::fs::read_to_string(file())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// 写回几何。失败只记日志：记不住窗口位置不该影响使用。
pub fn save(geometry: Geometry) {
    if !geometry.is_sane() {
        return;
    }
    if let Err(e) = nd_core::paths::ensure_home() {
        tracing::warn!(error = %e, "无法创建数据目录，窗口位置未保存");
        return;
    }
    match serde_json::to_string_pretty(&geometry) {
        Ok(text) => {
            let path = file();
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &path).is_err() {
                tracing::warn!(path = %path.display(), "窗口位置保存失败");
            }
        }
        Err(e) => tracing::warn!(error = %e, "窗口位置无法序列化"),
    }
}

/// 窗口与 `WebviewWindow` 共有的几何接口。
///
/// 关窗事件（`on_window_event`）拿到的是 `Window`，而命令层拿到的是
/// `WebviewWindow`；两边都要记位置，所以用一个小 trait 把差异挡住。
pub trait WindowLike {
    fn is_visible(&self) -> tauri::Result<bool>;
    fn is_maximized(&self) -> tauri::Result<bool>;
    fn outer_size(&self) -> tauri::Result<PhysicalSize<u32>>;
    fn outer_position(&self) -> tauri::Result<PhysicalPosition<i32>>;
    fn set_size(&self, size: PhysicalSize<u32>) -> tauri::Result<()>;
    fn set_position(&self, position: PhysicalPosition<i32>) -> tauri::Result<()>;
    fn center(&self) -> tauri::Result<()>;
    fn maximize(&self) -> tauri::Result<()>;
    fn available_monitors(&self) -> tauri::Result<Vec<(i32, i32, u32, u32)>>;
}

impl WindowLike for Window {
    fn is_visible(&self) -> tauri::Result<bool> {
        Window::is_visible(self)
    }
    fn is_maximized(&self) -> tauri::Result<bool> {
        Window::is_maximized(self)
    }
    fn outer_size(&self) -> tauri::Result<PhysicalSize<u32>> {
        Window::outer_size(self)
    }
    fn outer_position(&self) -> tauri::Result<PhysicalPosition<i32>> {
        Window::outer_position(self)
    }
    fn set_size(&self, size: PhysicalSize<u32>) -> tauri::Result<()> {
        Window::set_size(self, size)
    }
    fn set_position(&self, position: PhysicalPosition<i32>) -> tauri::Result<()> {
        Window::set_position(self, position)
    }
    fn center(&self) -> tauri::Result<()> {
        Window::center(self)
    }
    fn maximize(&self) -> tauri::Result<()> {
        Window::maximize(self)
    }
    fn available_monitors(&self) -> tauri::Result<Vec<(i32, i32, u32, u32)>> {
        Ok(Window::available_monitors(self)?
            .iter()
            .map(|m| {
                let pos = m.position();
                let size = m.size();
                (pos.x, pos.y, size.width, size.height)
            })
            .collect())
    }
}

impl WindowLike for WebviewWindow {
    fn is_visible(&self) -> tauri::Result<bool> {
        WebviewWindow::is_visible(self)
    }
    fn is_maximized(&self) -> tauri::Result<bool> {
        WebviewWindow::is_maximized(self)
    }
    fn outer_size(&self) -> tauri::Result<PhysicalSize<u32>> {
        WebviewWindow::outer_size(self)
    }
    fn outer_position(&self) -> tauri::Result<PhysicalPosition<i32>> {
        WebviewWindow::outer_position(self)
    }
    fn set_size(&self, size: PhysicalSize<u32>) -> tauri::Result<()> {
        WebviewWindow::set_size(self, size)
    }
    fn set_position(&self, position: PhysicalPosition<i32>) -> tauri::Result<()> {
        WebviewWindow::set_position(self, position)
    }
    fn center(&self) -> tauri::Result<()> {
        WebviewWindow::center(self)
    }
    fn maximize(&self) -> tauri::Result<()> {
        WebviewWindow::maximize(self)
    }
    fn available_monitors(&self) -> tauri::Result<Vec<(i32, i32, u32, u32)>> {
        Ok(WebviewWindow::available_monitors(self)?
            .iter()
            .map(|m| {
                let pos = m.position();
                let size = m.size();
                (pos.x, pos.y, size.width, size.height)
            })
            .collect())
    }
}

/// 从窗口读出当前几何。
///
/// 返回 `None` 表示这次不值得记录（例如窗口刚建好还不可见、正在最小化）。
#[must_use]
pub fn capture<W: WindowLike + ?Sized>(window: &W) -> Option<Geometry> {
    if !window.is_visible().unwrap_or(false) {
        return None;
    }
    let maximized = window.is_maximized().unwrap_or(false);
    let size = window.outer_size().ok()?;
    let position = window.outer_position().ok()?;
    let geometry = Geometry {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
        maximized,
    };
    geometry.is_sane().then_some(geometry)
}

/// 把窗口摆回上次的位置与大小。
///
/// 位置不在任何显示器上时只恢复尺寸、让窗口居中。
pub fn restore<W: WindowLike + ?Sized>(window: &W, geometry: Geometry) {
    if !geometry.is_sane() {
        return;
    }
    window
        .set_size(PhysicalSize::new(
            geometry.width.max(MIN_WIDTH),
            geometry.height.max(MIN_HEIGHT),
        ))
        .ok();
    let screens = window.available_monitors().unwrap_or_default();
    if geometry.is_visible_on(&screens) {
        window
            .set_position(PhysicalPosition::new(geometry.x, geometry.y))
            .ok();
    } else {
        window.center().ok();
    }
    if geometry.maximized {
        window.maximize().ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> Vec<(i32, i32, u32, u32)> {
        vec![(0, 0, 1920, 1040)]
    }

    #[test]
    fn rejects_tiny_geometry() {
        let g = Geometry {
            width: 100,
            height: 100,
            ..Default::default()
        };
        assert!(!g.is_sane());
    }

    #[test]
    fn accepted_geometry_inside_primary_screen() {
        let g = Geometry {
            x: 120,
            y: 90,
            width: 1080,
            height: 720,
            maximized: false,
        };
        assert!(g.is_visible_on(&screen()));
    }

    #[test]
    fn geometry_off_screen_is_detected() {
        let g = Geometry {
            x: 4000,
            y: 300,
            width: 1080,
            height: 720,
            maximized: false,
        };
        assert!(!g.is_visible_on(&screen()), "屏幕外的位置不该被采用");
    }

    #[test]
    fn geometry_on_secondary_screen_is_kept() {
        let mut screens = screen();
        screens.push((1920, 0, 2560, 1400));
        let g = Geometry {
            x: 2400,
            y: 200,
            width: 1080,
            height: 720,
            maximized: true,
        };
        assert!(g.is_visible_on(&screens));
    }

    #[test]
    fn negative_position_on_left_monitor_is_fine() {
        let screens = vec![(-1920, 0, 1920, 1040), (0, 0, 1920, 1040)];
        let g = Geometry {
            x: -1800,
            y: 40,
            width: 1080,
            height: 720,
            maximized: false,
        };
        assert!(g.is_visible_on(&screens));
    }

    #[test]
    fn save_ignores_insane_geometry() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::env::set_var("NIGHTDISPLAY_HOME", dir.path());
        save(Geometry::default());
        assert!(!file().exists(), "坏数据不该落盘");
        std::env::remove_var("NIGHTDISPLAY_HOME");
    }
}
