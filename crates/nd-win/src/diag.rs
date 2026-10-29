//! 诊断包的**采集**部分：把只有 Windows 才知道的东西（显示路径、DDC 能力）
//! 交给 `nd_core::diag::Bundle` 打包。
//!
//! GUI 的「导出诊断包」按钮和 `--action export-diag` 走的是同一条路径，
//! 所以用户报问题时拿到的包一定是可复现的。

#![cfg(windows)]

use std::path::{Path, PathBuf};

use anyhow::Result;
use nd_core::config::Config;
use nd_core::diag::{self, Bundle};
use nd_core::status::SelfCheck;

use crate::{brightness, topology};

/// 导出诊断包，返回写出的文件路径。
///
/// `dir` 为 `None` 时写到 `<数据目录>/diag`。同名文件不覆盖。
pub fn export(dir: Option<&Path>) -> Result<PathBuf> {
    let home = nd_core::paths::home_dir();
    let cfg = Config::load()?;
    let caps = crate::current_capabilities();
    let report = SelfCheck::build(&cfg, &caps);

    let mut bundle = Bundle::new();
    bundle.note(format!(
        "数据目录：{}（诊断包里会替换成 <home>）",
        home.display()
    ));
    bundle.note(format!("配置里的模式：{}", cfg.mode.as_str()));
    bundle.add_json("selfcheck.json", &report, &home);
    bundle.add_json("config.json", &cfg, &home);

    // 显示路径与 DDC 能力：这张表决定"这台机器能不能无闪切换"。
    match topology::describe(false) {
        Ok(list) => bundle.add_json("displays.json", &list, &home),
        Err(e) => bundle.note(format!("读不到显示路径：{}", nd_core::error::one_line(&e))),
    }
    match brightness::Control::open() {
        Ok(control) => {
            let panels: Vec<serde_json::Value> = control
                .snapshot()
                .into_iter()
                .map(|(name, level)| serde_json::json!({ "panel": name, "recorded_level": level }))
                .collect();
            let value = serde_json::json!({
                "supported": control.supported_count(),
                "panels": panels,
            });
            bundle.add_json("ddc.json", &value, &home);
        }
        Err(e) => bundle.note(format!(
            "DDC/CI 背光接口不可用：{}",
            nd_core::error::one_line(&e)
        )),
    }

    // 最近几次夜间会话：每一步的耗时和结果。
    let sessions = nd_core::session::recent(diag::SESSIONS_IN_BUNDLE);
    bundle.add_json("sessions.json", &sessions, &home);
    if let Some(current) = nd_core::session::load_current() {
        // 正在夜里时，这一夜还没进历史，单独放一份。
        bundle.add_json("session-current.json", &current, &home);
    }

    // 日志：按组件分开，原样带上（内部已按天滚动，只保留 14 天）。
    let mut log_count = 0_usize;
    if let Ok(entries) = std::fs::read_dir(nd_core::paths::logs_dir()) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    bundle.add_text(&format!("logs/{name}"), &text, &home);
                    log_count += 1;
                }
                Err(e) => bundle.note(format!("跳过日志 {name}：{e}")),
            }
        }
    }
    bundle.note(format!("日志文件：{log_count} 个"));

    let target_dir = dir.map_or_else(|| home.join("diag"), Path::to_path_buf);
    std::fs::create_dir_all(&target_dir)
        .map_err(|e| nd_core::error::user(format!("无法创建导出目录：{e}")))?;
    let path = unique_path(&target_dir, &diag::bundle_name());
    bundle
        .write(&path)
        .map_err(|e| nd_core::error::user(format!("诊断包写入失败：{e}")))?;
    tracing::info!(path = %path.display(), "诊断包已导出");
    Ok(path)
}

/// 同名的包不覆盖：`a.zip` 已存在就用 `a-2.zip`。
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let stem = name.strip_suffix(".zip").unwrap_or(name);
    for index in 2..1000 {
        let candidate = dir.join(format!("{stem}-{index}.zip"));
        if !candidate.exists() {
            return candidate;
        }
    }
    first
}
