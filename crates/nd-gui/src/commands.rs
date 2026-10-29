//! 前端能调用的全部命令。
//!
//! 每个命令都是"薄翻译层"：
//!
//! * 先把动作交给**已在运行的常驻进程**（本地命名管道 + 令牌）；
//! * 常驻进程不在、而这个动作必须有人在后台活着（夜间黑幕、关屏）时，
//!   先把常驻进程拉起来再重试一次 —— 界面自己永远不持有黑幕窗口；
//! * 只有纯配置类动作（改设置、登记计划任务）才就地用 `WinPlatform` 执行。
//!
//! 这个进程**从不以管理员身份运行**：装驱动、写计划任务这些走提升助手。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nd_core::config::Config;
use nd_core::ipc::CommandDetail;
use nd_core::onboarding;
use nd_core::status::Capabilities;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter as _, Manager as _};

use crate::settings::SettingsPatch;
use crate::state::{self, OnboardingView, Snapshot};

/// 常驻进程的命令超时。夜间切换要跑 DDC 和切换显示配置，给足 60 秒。
const DAEMON_TIMEOUT_MS: u32 = 60_000;
/// 拉起常驻进程后等它把管道建好的最长时间。
const DAEMON_WAIT: Duration = Duration::from_secs(8);
/// 能力快照的缓存时间。查显卡/显示器要起 PowerShell，不能每秒都查。
const CAPS_TTL: Duration = Duration::from_secs(30);

// ----------------------------------------------------------------------
// 能力缓存
// ----------------------------------------------------------------------

struct CachedCaps {
    at: Instant,
    value: Capabilities,
}

static CAPS: std::sync::Mutex<Option<CachedCaps>> = std::sync::Mutex::new(None);

fn caps_cached() -> Capabilities {
    let guard = CAPS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(cached) = guard.as_ref() {
        if cached.at.elapsed() < CAPS_TTL {
            return cached.value.clone();
        }
    }
    // 拿锁时不要跑 PowerShell：慢查询放到锁外，避免并发命令排队。
    drop(guard);
    let value = nd_win::current_capabilities();
    let mut guard = CAPS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = Some(CachedCaps {
        at: Instant::now(),
        value: value.clone(),
    });
    value
}

/// 丢掉缓存（改过设置、跑完向导之后调用，让界面立刻显示真实状态）。
fn caps_invalidate() {
    let mut guard = CAPS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = None;
}

// ----------------------------------------------------------------------
// 常驻进程
// ----------------------------------------------------------------------

/// 把动作发给常驻进程。
///
/// `Ok(Some(text))` 成功，`Ok(None)` 常驻进程没在跑，`Err` 出错或被拒绝。
pub fn daemon_call(action: &str, detail: Option<&CommandDetail>) -> anyhow::Result<Option<String>> {
    match nd_win::pipe::call_daemon(action, detail, DAEMON_TIMEOUT_MS) {
        Ok(Some(resp)) if resp.ok => Ok(Some(resp.result)),
        Ok(Some(resp)) => Err(nd_core::error::user(resp.result)),
        Ok(None) => Ok(None),
        Err(e) => Err(e),
    }
}

/// 确认常驻进程在跑；没跑就拉一个起来，并等它就绪。
///
/// 界面自己**不**执行夜间动作：黑幕窗口必须活在常驻进程里，否则用户一关界面
/// 屏幕就亮了。所以这里宁可多等几秒，也要把常驻进程等起来。
fn ensure_daemon() -> anyhow::Result<()> {
    if daemon_call("ping", None)?.is_some() {
        return Ok(());
    }
    let args = nd_win::sysutil::self_args(&nd_core::paths::home_dir(), &["--action", "serve"]);
    nd_win::sysutil::spawn_detached(&args)
        .map_err(|e| nd_core::error::user(format!("无法启动后台服务：{e}")))?;
    let deadline = Instant::now() + DAEMON_WAIT;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
        if daemon_call("ping", None).ok().flatten().is_some() {
            tracing::info!("后台服务已就绪");
            return Ok(());
        }
    }
    Err(nd_core::error::user(
        "后台服务没能启动（等了 8 秒）。请点“重新授权/修复”，或查看日志里的启动错误。",
    ))
}

/// 先交给常驻进程；没有常驻进程时启动它再试。返回给用户看的一句话。
fn daemon_or_start(action: &str, detail: Option<&CommandDetail>) -> anyhow::Result<String> {
    if let Some(text) = daemon_call(action, detail)? {
        return Ok(text);
    }
    tracing::info!(action, "常驻进程未运行，先启动再执行");
    ensure_daemon()?;
    match daemon_call(action, detail)? {
        Some(text) => Ok(text),
        None => Ok("已交给后台服务处理".to_string()),
    }
}

// ----------------------------------------------------------------------
// 前端数据结构
// ----------------------------------------------------------------------

/// 一段日志的尾部。
#[derive(Debug, Clone, Serialize)]
pub struct LogTail {
    /// 组件名（`daemon` / `watchdog` / `helper` / `gui` / `installer` / `cli`）。
    pub component: String,
    /// 实际读到的文件（可能不是今天那份）。
    pub path: String,
    /// 文件大小（字节）。
    pub bytes: u64,
    /// 文件最后修改时间（`YYYY-MM-DD HH:MM:SS`）。
    pub updated: String,
    /// 最后若干行。
    pub text: String,
}

/// 日志目录里的一个文件。
#[derive(Debug, Clone, Serialize)]
pub struct LogFile {
    pub name: String,
    pub path: String,
    pub bytes: u64,
    /// `YYYY-MM-DD HH:MM`。
    pub updated: String,
    /// 看起来是 JSON Lines（诊断时更完整）。
    pub json: bool,
}

/// 日志页初始数据：目录 + 文件清单 + 可读的组件清单。
#[derive(Debug, Clone, Serialize)]
pub struct LogIndex {
    pub dir: String,
    pub files: Vec<LogFile>,
    /// 组件名 → 中文名。
    pub components: Vec<ComponentInfo>,
    /// 日志总量（字节），设置页要显示"日志占用"。
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentInfo {
    pub id: String,
    pub label: String,
    pub hint: String,
}

/// 一次向导步骤的进度事件（推给界面，让它按步骤亮灯）。
#[derive(Debug, Clone, Serialize)]
pub struct StepEvent {
    /// `elevate` / `virtual_driver` / `autostart` / `input_guard` / `self_test` / `confirm`。
    pub id: String,
    pub title: String,
    /// 这一步现在在干什么（"正在显示黑幕并校验采集排除…"）。
    pub text: String,
}

/// `read_logs` 的入参（两个来源都接受：组件名，或直接给文件名）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LogQuery {
    pub component: Option<String>,
    pub file: Option<String>,
    pub lines: Option<usize>,
}

/// 一次设置保存的结果。
#[derive(Debug, Clone, Serialize)]
pub struct SaveResult {
    pub message: String,
    pub snapshot: Snapshot,
}

// ----------------------------------------------------------------------
// 只读
// ----------------------------------------------------------------------

/// 界面需要的全部状态。
#[tauri::command]
pub async fn snapshot() -> Result<Snapshot, String> {
    blocking("读取状态", || {
        let cfg = Config::load().unwrap_or_default();
        let caps = caps_cached();
        let onboarding = onboarding::load_state();
        let mut snap = state::build(&cfg, &caps, &onboarding, chrono::Local::now());
        snap.daemon_running = daemon_call("ping", None).ok().flatten().is_some();
        Ok(snap)
    })
    .await
}

/// 向导状态（轻量，不带能力查询）。
#[tauri::command]
pub async fn onboarding_state() -> Result<OnboardingView, String> {
    blocking("读取向导状态", || Ok(state::onboarding_snapshot())).await
}

/// 日志目录、文件清单与组件列表。
#[tauri::command]
pub async fn log_index() -> Result<LogIndex, String> {
    blocking("列出日志", || Ok(log_index_sync())).await
}

/// 读一段日志尾部。
#[tauri::command]
pub async fn read_logs(query: LogQuery) -> Result<LogTail, String> {
    blocking("读取日志", move || {
        let lines = query.lines.unwrap_or(400).clamp(20, 5000);
        let path = match (query.file.as_deref(), query.component.as_deref()) {
            (Some(name), _) => safe_log_file(name)?,
            (None, Some(component)) => latest_for(component)?,
            (None, None) => latest_for("daemon")?,
        };
        Ok(tail_of(&path, lines))
    })
    .await
}

// ----------------------------------------------------------------------
// 动作
// ----------------------------------------------------------------------

/// 立即进入夜间（黑屏）。
#[tauri::command]
pub async fn night_now(mode: Option<String>) -> Result<String, String> {
    let detail = mode.map(|mode| CommandDetail {
        action: "night".into(),
        mode: Some(mode),
        seconds: None,
        test_id: None,
    });
    blocking("进入夜间", move || {
        daemon_or_start("night", detail.as_ref())
    })
    .await
}

/// 立即恢复显示。
#[tauri::command]
pub async fn day_now() -> Result<String, String> {
    blocking("恢复显示", || daemon_or_start("day", None)).await
}

/// 暂停今天的自动计划。
#[tauri::command]
pub async fn pause_schedule() -> Result<String, String> {
    blocking("暂停计划", || daemon_or_start("pause", None)).await
}

/// 限时测试（真的黑屏 N 秒，然后自动恢复）。
#[tauri::command]
pub async fn test_mode(seconds: u64, mode: Option<String>) -> Result<String, String> {
    let seconds = seconds.clamp(5, 3600);
    let detail = CommandDetail {
        action: "test".into(),
        mode,
        seconds: Some(seconds),
        test_id: None,
    };
    blocking("测试黑屏", move || {
        daemon_or_start("test", Some(&detail))
    })
    .await
}

/// 按配置里的时间段启用每日计划。
#[tauri::command]
pub async fn activate_schedule() -> Result<String, String> {
    blocking("启用每日计划", || daemon_or_start("activate", None)).await
}

/// 保存设置。
#[tauri::command]
pub async fn set_settings(patch: SettingsPatch) -> Result<SaveResult, String> {
    blocking("保存设置", move || {
        let platform = nd_win::WinPlatform::new();
        let message = crate::settings::apply(&platform, &patch);
        platform.shutdown();
        let message = message?;
        caps_invalidate();
        // 计划/模式改了，让常驻进程立刻按新设置对齐一次（它不在就算了）。
        if patch.touches_schedule() {
            if let Err(e) = daemon_call("reconcile", None) {
                tracing::warn!(error = %e, "通知后台服务重新对齐失败");
            }
        }
        let cfg = Config::load().unwrap_or_default();
        let snapshot = state::build(
            &cfg,
            &caps_cached(),
            &onboarding::load_state(),
            chrono::Local::now(),
        );
        Ok(SaveResult { message, snapshot })
    })
    .await
}

/// 跑整个安装向导。每步的状态通过 `setup-step` 事件实时推给界面。
#[tauri::command]
pub async fn run_setup(
    app: AppHandle,
    install_vdd: bool,
    self_test_seconds: u64,
) -> Result<OnboardingView, String> {
    blocking("运行向导", move || {
        let mut onboarding = onboarding::load_state();
        onboarding.remote_confirmed = false;
        let options = nd_win::setup::SetupOptions {
            install_vdd,
            self_test_seconds: self_test_seconds.clamp(3, 60),
            only: Vec::new(),
            progress: Some(progress_bridge(app)),
        };
        let results = nd_win::setup::run(&options, &mut onboarding)?;
        onboarding::save_state(&onboarding);
        let failed = results.iter().filter(|r| r.outcome.failed_hard()).count();
        tracing::info!(steps = results.len(), failed, "向导执行结束");
        caps_invalidate();
        Ok(state::onboarding_snapshot())
    })
    .await
}

/// 重新授权 / 修复：只重跑失败的步骤。
#[tauri::command]
pub async fn repair(app: AppHandle) -> Result<OnboardingView, String> {
    blocking("修复向导步骤", move || {
        let mut onboarding = onboarding::load_state();
        let only = onboarding.steps_to_repair();
        let options = nd_win::setup::SetupOptions {
            install_vdd: true,
            self_test_seconds: 15,
            only,
            progress: Some(progress_bridge(app)),
        };
        nd_win::setup::run(&options, &mut onboarding)?;
        onboarding::save_state(&onboarding);
        caps_invalidate();
        Ok(state::onboarding_snapshot())
    })
    .await
}

/// 勾选 / 取消"已确认远程能连"。
#[tauri::command]
pub async fn confirm_remote(confirmed: bool) -> Result<OnboardingView, String> {
    blocking("记录远程确认", move || {
        let mut onboarding = onboarding::load_state();
        onboarding.remote_confirmed = confirmed;
        onboarding::save_state(&onboarding);
        Ok(state::onboarding_snapshot())
    })
    .await
}

/// 结束向导：必需步骤都处理完、并勾选过确认后才会真正完成。
#[tauri::command]
pub async fn finish_onboarding() -> Result<String, String> {
    blocking("结束向导", move || {
        let mut onboarding = onboarding::load_state();
        let message = nd_win::setup::finish(&mut onboarding)?;
        caps_invalidate();
        Ok(message)
    })
    .await
}

/// 导出诊断包，返回生成的 zip 路径。
#[tauri::command]
pub async fn export_diag() -> Result<String, String> {
    blocking("导出诊断包", || {
        let path = nd_win::diag::export(None)?;
        tracing::info!(path = %path.display(), "诊断包已导出");
        Ok(path.display().to_string())
    })
    .await
}

/// 在资源管理器里定位文件/文件夹。只允许数据目录与日志目录里的路径。
#[tauri::command]
pub async fn reveal(path: String) -> Result<String, String> {
    blocking("打开位置", move || {
        let target = safe_reveal_target(&path)?;
        nd_win::sysutil::open_in_explorer(&target)
            .map_err(|e| nd_core::error::user(format!("无法打开 {}：{e}", target.display())))?;
        Ok(target.display().to_string())
    })
    .await
}

/// 用默认浏览器打开一个链接（只允许 http/https）。
#[tauri::command]
pub async fn open_url(url: String) -> Result<(), String> {
    blocking("打开链接", move || {
        nd_win::sysutil::open_url(&url)
            .map_err(|e| nd_core::error::user(format!("无法打开链接：{e}")))
    })
    .await
}

/// 界面退出（夜间黑屏在常驻进程里，不受影响）。
///
/// `AppHandle` 必须按值接收：Tauri 的命令宏按类型注入参数，`&AppHandle`
/// 不被支持（会编译失败），所以这里保留 by-value 并显式豁免。
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn quit_app(app: AppHandle) {
    if let Some(view) = app.get_webview_window(crate::tray::MAIN_WINDOW) {
        crate::tray::save_geometry(&view);
    }
    app.exit(0);
}

/// 把主窗口叫出来（托盘菜单里"打开设置"也会走这里）。
///
/// 同 `quit_app`：Tauri 命令注入要求 `AppHandle` 按值。
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn open_window(app: AppHandle) {
    crate::tray::show_main_window(&app);
}

// ----------------------------------------------------------------------
// 内部工具
// ----------------------------------------------------------------------

/// 把阻塞工作丢到线程池，命令本身保持 `async`，不堵 Tauri 的运行时。
async fn blocking<T, F>(what: &'static str, f: F) -> Result<T, String>
where
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
    T: Send + 'static,
{
    match tauri::async_runtime::spawn_blocking(f).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => {
            nd_core::logging::record_error(what, &e);
            Err(nd_core::error::one_line(&e))
        }
        Err(e) => {
            tracing::error!(error = %e, what, "后台任务异常结束");
            Err(format!("{what}时内部线程出错：{e}"))
        }
    }
}

/// 把 `setup` 的进度回调接到 Tauri 事件上。
fn progress_bridge(app: AppHandle) -> nd_win::setup::ProgressFn {
    Box::new(move |step, text| {
        let event = StepEvent {
            id: state::step_id(step).to_string(),
            title: step.title().to_string(),
            text: text.to_string(),
        };
        if let Err(e) = app.emit("setup-step", &event) {
            tracing::warn!(error = %e, "推送向导进度失败");
        }
    })
}

/// 日志组件清单（顺序即界面上的顺序）。
fn component_list() -> Vec<ComponentInfo> {
    vec![
        ComponentInfo {
            id: "daemon".into(),
            label: "后台服务".into(),
            hint: "定时切换、黑幕、背光都在这".into(),
        },
        ComponentInfo {
            id: "helper".into(),
            label: "授权助手".into(),
            hint: "装驱动、写计划任务".into(),
        },
        ComponentInfo {
            id: "watchdog".into(),
            label: "看门狗".into(),
            hint: "主进程没了它负责恢复屏幕".into(),
        },
        ComponentInfo {
            id: "gui".into(),
            label: "界面".into(),
            hint: "当前窗口自己的日志".into(),
        },
        ComponentInfo {
            id: "installer".into(),
            label: "安装与向导".into(),
            hint: "安装驱动、首次配置".into(),
        },
        ComponentInfo {
            id: "cli".into(),
            label: "命令行".into(),
            hint: "手动执行的命令".into(),
        },
    ]
}

fn log_index_sync() -> LogIndex {
    let dir = nd_core::paths::logs_dir();
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            files.push(LogFile {
                name: name.to_string(),
                path: path.display().to_string(),
                bytes: meta.len(),
                updated: meta
                    .modified()
                    .ok()
                    .map(|t| {
                        let at: chrono::DateTime<chrono::Local> = t.into();
                        at.format("%Y-%m-%d %H:%M").to_string()
                    })
                    .unwrap_or_default(),
                json: Path::new(name)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl")),
            });
        }
    }
    // 新的排前面：出问题时要看的是最近那份。
    files.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.name.cmp(&b.name)));
    LogIndex {
        dir: dir.display().to_string(),
        files,
        components: component_list(),
        total_bytes: nd_core::logging::total_size(),
    }
}

/// 某个组件最近一份日志（先找今天的，找不到就找日期最大的）。
fn latest_for(component: &str) -> anyhow::Result<PathBuf> {
    let prefix = component.trim();
    if !prefix
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(nd_core::error::user(format!("组件名“{prefix}”不合法")));
    }
    let dir = nd_core::paths::logs_dir();
    let today = format!("{prefix}-{}.log", chrono::Local::now().format("%Y-%m-%d"));
    let today_path = dir.join(&today);
    if today_path.is_file() {
        return Ok(today_path);
    }
    let mut best: Option<(String, PathBuf)> = None;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            // 按 `<component>-<date>.log` 精确匹配，避免 "gui" 命中 "gui-old"。
            let Some(rest) = name.strip_prefix(prefix) else {
                continue;
            };
            let Some(rest) = rest.strip_prefix('-') else {
                continue;
            };
            // 扩展名判断走 Path::extension，避免大小写/前缀误判。
            let is_log = Path::new(rest).extension().is_some_and(|ext| {
                ext.eq_ignore_ascii_case("log") || ext.eq_ignore_ascii_case("jsonl")
            });
            if !is_log {
                continue;
            }
            if best
                .as_ref()
                .is_none_or(|(best_name, _)| name > best_name.as_str())
            {
                best = Some((name.to_string(), path));
            }
        }
    }
    match best {
        Some((_, path)) => Ok(path),
        None => Err(nd_core::error::user(format!(
            "还没有 {prefix} 的日志。第一次运行之后才会生成。"
        ))),
    }
}

/// 校验前端传来的日志文件名，只允许日志目录里的直接子文件。
fn safe_log_file(name: &str) -> anyhow::Result<PathBuf> {
    if name.is_empty()
        || name.contains(['/', '\\', ':'])
        || name.contains("..")
        || name.starts_with('.')
    {
        return Err(nd_core::error::user(format!("文件名“{name}”不合法")));
    }
    let path = nd_core::paths::logs_dir().join(name);
    if !path.is_file() {
        return Err(nd_core::error::user(format!("找不到日志文件：{name}")));
    }
    Ok(path)
}

/// 只允许定位数据目录里的东西：前端传来的路径可能被篡改，不能当"打开任意文件"用。
fn safe_reveal_target(text: &str) -> anyhow::Result<PathBuf> {
    let raw = Path::new(text.trim());
    let home = nd_core::paths::home_dir();
    let logs = nd_core::paths::logs_dir();
    // 先看原始路径（诊断包可能被存到自定义目录），再退回数据目录内的相对名。
    let candidate = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        home.join(raw)
    };
    let inside_home = candidate.starts_with(&home);
    let inside_logs = candidate.starts_with(&logs);
    if !inside_home && !inside_logs {
        return Err(nd_core::error::user(format!(
            "只能打开 NightDisplay 自己的数据目录里的位置（{text}）"
        )));
    }
    Ok(candidate)
}

/// 读文件最后若干行。文件很大时只读尾部，不做全文扫描。
fn tail_of(path: &Path, lines: usize) -> LogTail {
    const TAIL_BYTES: u64 = 512 * 1024;
    let meta = std::fs::metadata(path).ok();
    let bytes = meta.as_ref().map_or(0, std::fs::Metadata::len);
    let text = read_tail(path, bytes, TAIL_BYTES);
    let mut rows: Vec<&str> = text.lines().collect();
    if rows.len() > lines {
        rows.drain(..rows.len() - lines);
    }
    LogTail {
        component: path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .split('-')
            .next()
            .unwrap_or_default()
            .to_string(),
        path: path.display().to_string(),
        bytes,
        updated: meta
            .and_then(|m| m.modified().ok())
            .map(|t| {
                let at: chrono::DateTime<chrono::Local> = t.into();
                at.format("%Y-%m-%d %H:%M:%S").to_string()
            })
            .unwrap_or_default(),
        text: rows.join("\n"),
    }
}

fn read_tail(path: &Path, size: u64, limit: u64) -> String {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    if size > limit && file.seek(SeekFrom::Start(size - limit)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    // 只读尾部时第一行可能是半行，丢掉它。
    let text = String::from_utf8_lossy(&buf).to_string();
    if size > limit {
        match text.find('\n') {
            Some(index) => text[index + 1..].to_string(),
            None => text,
        }
    } else {
        text
    }
}
