//! 常驻进程：`--action serve`。
//!
//! 职责：
//! 1. 拿着 `daemon.lock` 保证单实例；
//! 2. 起来就把黑幕窗口预热好（夜里第一帧是 `ShowWindow`，毫秒级）；
//! 3. 每 2 秒做一次 `reconcile`，按计划进入/退出夜间，夜间期间补背光和黑幕置顶；
//! 4. 起一个看门狗，进程被杀也能把屏幕恢复；
//! 5. 在命名管道上接受 CLI / GUI 的本地请求（令牌校验）。
//!
//! 线程分工：界面线程归 `WinPlatform`（黑幕 + 键鼠钩子），
//! 本模块的循环跑在主线程上，只做判断和调用，绝不自己碰窗口。

use std::time::{Duration, Instant};

use anyhow::Result;
use nd_core::config::Config;
use nd_core::engine::{Engine, Platform};
use nd_core::ipc::{self, Command, CommandDetail, Request, Response};
use nd_core::mode::Mode;
use nd_win::{pipe, watchdog, WinPlatform};

use std::process::ExitCode;

/// 主循环的节拍。夜间要 5 秒补一次背光，白天可以慢一点。
const TICK_NIGHT: Duration = Duration::from_secs(2);
const TICK_DAY: Duration = Duration::from_secs(15);
/// 背光重复校正间隔（配置里可调，这里是兜底的默认值）。
const BRIGHTNESS_REASSERT: Duration = Duration::from_secs(5);

/// 常驻进程入口。
pub fn serve() -> Result<ExitCode> {
    // 单实例：拿不到锁说明已经有一个在跑，这时只回一句就退出，别抢。
    let Ok(lock) =
        nd_core::state_lock::write_lock_timeout(nd_core::paths::daemon_lock(), Duration::ZERO)
    else {
        println!("NightDisplay 常驻进程已经在运行。");
        return Ok(ExitCode::SUCCESS);
    };

    nd_core::paths::ensure_home()
        .map_err(|e| nd_core::error::user(format!("无法创建数据目录：{e}")))?;

    let platform = WinPlatform::new();
    // 关机/注销时先还原屏幕：用户下次开机不该看到黑着的桌面。
    if let Err(e) = nd_win::shutdown::install() {
        tracing::warn!(error = %e, "注册关机收尾失败，仍有看门狗兜底");
    }
    // 预热：把黑幕窗口先建好藏起来，晚上直接显示。
    if let Err(e) = platform.warm() {
        // 预热失败不算致命：真到夜间会再试一次，失败会如实报告。
        tracing::warn!(error = %e, "黑幕预热失败，夜间会重新尝试");
    }
    // 起看门狗：本进程被结束时它负责把 gamma / 背光 / 拓扑还原。
    watchdog::clear_clean_exit_marker();
    let engine = Engine::new(&platform);
    // 上次是不是没走完就没了？是的话先把屏幕救回来。
    recover_after_crash(&engine);
    if let Err(e) = engine.platform.spawn_watchdog() {
        tracing::warn!(error = %e, "看门狗启动失败，主功能仍然可用");
    }

    // 端点：令牌只在数据目录里，CLI/GUI 读它来连。
    let endpoint = ipc::Endpoint::new(pipe::serve_pipe_name());
    pipe::save_endpoint(&endpoint.token)?;
    tracing::info!(pipe = %endpoint.pipe, "常驻进程已就绪");
    println!("NightDisplay 常驻进程已启动。按 Ctrl+C 结束（会先恢复屏幕）。");

    // 主循环与管道服务分两个线程：管道是阻塞的，不能和定时检查抢。
    let token = endpoint.token.clone();
    let loop_platform = std::sync::Arc::new(platform);
    let worker = {
        let platform = std::sync::Arc::clone(&loop_platform);
        std::thread::spawn(move || run_loop(&platform))
    };

    let handler_token = token;
    let handler_platform = std::sync::Arc::clone(&loop_platform);
    let result = pipe::serve(&endpoint.pipe, move |request: Request| {
        if !ipc::token_matches(&handler_token, &request.token) {
            tracing::warn!("收到令牌不匹配的本地请求，已拒绝");
            return (Response::error("令牌不匹配，拒绝执行"), false);
        }
        dispatch(&handler_platform, &request.command)
    });

    // 管道循环结束（收到 quit 或出错）：让工作线程收尾。
    nd_win::input::request_stop();
    WORKER_STOP.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = worker.join();
    shutdown(&loop_platform);
    pipe::clear_endpoint();
    drop(lock);
    result.map(|()| ExitCode::SUCCESS)
}

static WORKER_STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 定时对齐循环。
fn run_loop(platform: &std::sync::Arc<WinPlatform>) {
    let engine = Engine::new(&platform);
    let mut last_brightness = Instant::now();
    let mut was_night = false;
    while !WORKER_STOP.load(std::sync::atomic::Ordering::SeqCst) {
        let now = chrono::Local::now();
        // 组合键解锁：界面线程会置位，这里负责恢复白天。
        if platform.take_unlock() {
            tracing::info!("检测到解锁组合键");
            match engine.chord_unlock(now) {
                Ok(outcome) => tracing::info!(message = %outcome.message, "组合键解锁完成"),
                Err(e) => tracing::error!(error = %e, "组合键解锁失败"),
            }
        }

        match engine.reconcile(now) {
            Ok(outcome) => {
                if !outcome.is_ok() {
                    tracing::warn!(message = %outcome.message, "定时对齐有告警");
                }
            }
            Err(e) => tracing::error!(error = %e, "定时对齐失败"),
        }

        let cfg = Config::load().unwrap_or_default();
        let night = cfg.state == nd_core::config::RunState::Night;
        if night != was_night {
            was_night = night;
            tracing::info!(night, "常驻进程状态切换");
        }
        // 夜间每几秒补一次背光：显示器自己会把亮度拉回去。
        if night && last_brightness.elapsed() >= BRIGHTNESS_REASSERT {
            last_brightness = Instant::now();
            if let Err(e) = platform.repair_guard() {
                tracing::debug!(error = %e, "夜间补正失败");
            }
        }
        std::thread::sleep(if night { TICK_NIGHT } else { TICK_DAY });
    }
    tracing::info!("常驻进程循环结束");
}

/// 启动时的对账：上次异常退出留下的残局要先收拾干净。
///
/// 三种情况：
/// * 有残留的恢复点（`screen-state.json` 还在）—— 说明上次没走到 `day()`，
///   日志里明确写一行「上次异常退出」，然后按恢复点还原屏幕；
/// * 配置里还写着"夜间"—— 说明进程被杀在写状态之后，按当前时间决定
///   继续夜间（时段内）还是恢复白天（时段外）；
/// * 什么都没有 —— 正常启动，什么都不做。
fn recover_after_crash(engine: &Engine<WinPlatform>) {
    let leftover = nd_core::paths::screen_state_file().is_file();
    let cfg = Config::load();
    let was_night = cfg
        .as_ref()
        .is_ok_and(|c| c.state == nd_core::config::RunState::Night);
    if !leftover && !was_night {
        return;
    }
    tracing::warn!(
        leftover_restore_point = leftover,
        config_says_night = was_night,
        "上次异常退出（没有走完正常恢复），开始对账"
    );
    println!("检测到上次没有正常退出，正在恢复屏幕……");
    if leftover {
        watchdog::restore_everything();
    }
    match engine.reconcile(chrono::Local::now()) {
        Ok(outcome) => tracing::info!(message = %outcome.message, "启动对账完成"),
        Err(e) => tracing::error!(error = %e, "启动对账失败"),
    }
}

/// 处理一条本地请求。返回 `(回复, 是否要求退出)`。
fn dispatch(platform: &WinPlatform, command: &Command) -> (Response, bool) {
    let engine = Engine::new(platform);
    let now = || chrono::Local::now();
    let action = command.action().to_string();
    tracing::info!(action = %action, "收到本地请求");
    let detail: Option<&CommandDetail> = match command {
        Command::Detailed(d) => Some(d),
        Command::Bare(_) => None,
    };

    // 每个分支只调用引擎，绝不在这里直接改显示。
    let outcome = match action.as_str() {
        "ping" | "show" => return (Response::ok(), false),
        "quit" => {
            watchdog::write_clean_exit_marker();
            return (Response::value("常驻进程已退出"), true);
        }
        "status" => {
            let cfg = Config::load().unwrap_or_default();
            let caps = platform.capabilities();
            return (
                Response::value(nd_core::status::status_text(&cfg, &caps, now())),
                false,
            );
        }
        "night" => {
            let mode = match detail.and_then(|d| d.mode.clone()) {
                Some(m) => match Mode::parse(&m) {
                    Ok(m) => m,
                    Err(e) => return (Response::error(e.to_string()), false),
                },
                None => match Config::load() {
                    Ok(cfg) => cfg.mode,
                    Err(e) => return (Response::error(e.to_string()), false),
                },
            };
            if let Err(e) = platform.warm() {
                tracing::warn!(error = %e, "夜间前预热失败");
            }
            engine.night(mode, now())
        }
        "day" => engine.day(),
        "reconcile" => engine.reconcile(now()),
        "test" => {
            let seconds = detail.and_then(|d| d.seconds).unwrap_or(120);
            let mode = match detail.and_then(|d| d.mode.clone()) {
                Some(m) => match Mode::parse(&m) {
                    Ok(m) => m,
                    Err(e) => return (Response::error(e.to_string()), false),
                },
                None => match Config::load() {
                    Ok(cfg) => cfg.mode,
                    Err(e) => return (Response::error(e.to_string()), false),
                },
            };
            if let Err(e) = platform.warm() {
                tracing::warn!(error = %e, "测试前预热失败");
            }
            engine.start_test(mode, seconds, now())
        }
        // 测试的恢复阶段：只有确认显示真回来了才算测试通过。
        "test_day" => {
            let test_id = detail.and_then(|d| d.test_id.clone()).unwrap_or_default();
            engine.finish_test(&test_id, now())
        }
        "pause" => engine.pause(),
        "remote_emergency" | "emergency" => engine.emergency(),
        "uninstall" => engine.uninstall(),
        "unlock" => engine.chord_unlock(now()),
        "repair_guard" => match platform.repair_guard() {
            Ok(()) => Ok(nd_core::engine::Outcome::ok("已重新压黑并校正背光")),
            Err(e) => return (Response::error(e.to_string()), false),
        },
        // 界面（第二阶段）用这条：时间段沿用配置里已经保存的那一段。
        "activate" => {
            let cfg = Config::load().unwrap_or_default();
            let primary = cfg.primary_range();
            let mode = match detail.and_then(|d| d.mode.clone()) {
                Some(m) => match Mode::parse(&m) {
                    Ok(m) => m,
                    Err(e) => return (Response::error(e.to_string()), false),
                },
                None => cfg.mode,
            };
            engine.activate(mode, &primary.start.to_hm(), &primary.end.to_hm(), now())
        }
        other => {
            return (
                Response::error(format!("常驻进程不接受命令“{other}”")),
                false,
            )
        }
    };

    match outcome {
        Ok(outcome) => {
            if outcome.is_ok() {
                (Response::value(outcome.message), false)
            } else {
                let mut text = outcome.message.clone();
                if !outcome.errors.is_empty() {
                    text.push('；');
                    text.push_str(&outcome.errors.join("；"));
                }
                (Response::error(text), false)
            }
        }
        Err(e) => (Response::error(e.to_string()), false),
    }
}

/// 退出前把屏幕恢复到白天，别把用户留在黑屏里。
fn shutdown(platform: &WinPlatform) {
    let engine = Engine::new(platform);
    let cfg = Config::load().unwrap_or_default();
    // 计划夜间时段内正常退出（例如用户重启）：保持夜间，交给下次启动继续。
    let in_window = cfg.enabled && cfg.schedule().is_night_at(chrono::Local::now());
    if !in_window && cfg.state == nd_core::config::RunState::Night {
        if let Err(e) = engine.day() {
            tracing::warn!(error = %e, "退出时恢复白天失败");
        }
    }
    platform.shutdown();
    watchdog::write_clean_exit_marker();
    tracing::info!("常驻进程已退出");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quit_asks_loop_to_stop() {
        let platform = WinPlatform::new();
        let (resp, stop) = dispatch(&platform, &Command::Bare("quit".into()));
        assert!(stop, "quit 必须让管道循环退出");
        assert!(resp.ok);
        // 清掉标记，免得影响别的测试。
        std::fs::remove_file(nd_core::paths::watchdog_file()).ok();
    }

    #[test]
    fn unknown_command_is_rejected() {
        let platform = WinPlatform::new();
        let (resp, stop) = dispatch(&platform, &Command::Bare("format-c".into()));
        assert!(!stop);
        assert!(!resp.ok);
        assert!(resp.result.contains("不接受"));
    }

    #[test]
    fn ping_is_silent_and_ok() {
        let platform = WinPlatform::new();
        let (resp, stop) = dispatch(&platform, &Command::Bare("ping".into()));
        assert!(resp.ok);
        assert!(!stop);
    }
}
