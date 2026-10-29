//! 状态文本与自检报告（纯逻辑，平台信息由调用方注入）。

use chrono::Local;
use serde::{Deserialize, Serialize};

use crate::config::{Config, Mode, RunState};
use crate::schedule::minute_of_day;
use crate::VERSION;

/// 平台层汇报的能力，供状态文本和自检使用。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Capabilities {
    pub os: String,
    /// Windows 版本号（build），非 Windows 为 0。
    pub build: u32,
    /// `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` 是否可用。
    pub capture_exclusion: bool,
    pub gamma_control: bool,
    /// 支持 DDC/CI 背光控制的显示器数量。
    pub brightness_displays: usize,
    /// 检测到的显示器总数。
    pub displays: usize,
    /// 是否有管理员权限（当前进程）。
    pub elevated: bool,
    /// 提升助手是否已安装并在运行。
    pub helper_running: bool,
    pub virtual_display: bool,
    pub autostart: bool,
}

/// 一行状态文本。
pub fn status_text(cfg: &Config, caps: &Capabilities, now: chrono::DateTime<Local>) -> String {
    let schedule = cfg.schedule();
    let ranges = cfg
        .ranges
        .iter()
        .map(|r| format!("{}→{}", r.0.start.to_hm(), r.0.end.to_hm()))
        .collect::<Vec<_>>()
        .join("，");
    let now_hm = minute_of_day(now).to_hm();
    let phase = match cfg.state {
        RunState::Night => "夜间（黑屏中）".to_string(),
        RunState::Day => {
            if schedule.is_night_at(now) {
                "白天（本应在夜间，等待修复）".to_string()
            } else {
                match schedule.next_switch(now) {
                    Some((true, secs)) => format!("白天，{} 后进入夜间", human_secs(secs)),
                    Some((false, secs)) => format!("白天，{} 后恢复", human_secs(secs)),
                    None => "白天（没有有效的切换点）".to_string(),
                }
            }
        }
    };
    let night_mode = cfg
        .night_mode
        .map(|m| m.label().to_string())
        .unwrap_or_else(|| "—".into());
    format!(
        "版本 {VERSION} ｜ {}（build {}）\n\
         模式：{} ｜ 自动计划：{} ｜ 夜间 {} ｜ 现在 {}，{}\n\
         上次实际模式：{} ｜ 首帧黑屏 {}ms\n\
         能力：采集排除={} ｜ gamma={} ｜ 背光 {} 屏 ｜ 显示器 {} 块\n\
         开机自启：配置={} 实际={} ｜ 提升助手：{}\n\
         数据目录：{}",
        caps.os,
        caps.build,
        cfg.mode.label(),
        if cfg.enabled {
            "已启用"
        } else {
            "未启用"
        },
        ranges,
        now_hm,
        phase,
        night_mode,
        cfg.last_night_latency_ms,
        if caps.capture_exclusion {
            "可用"
        } else {
            "不可用"
        },
        if caps.gamma_control {
            "可用"
        } else {
            "不可用"
        },
        caps.brightness_displays,
        caps.displays,
        if cfg.autostart { "开" } else { "关" },
        if caps.autostart {
            "已注册"
        } else {
            "未注册"
        },
        if caps.helper_running {
            "运行中"
        } else {
            "未运行"
        },
        crate::paths::home_dir().display()
    )
}

/// 自检报告。写入 `selfcheck.json`，也是支持排障的唯一入口。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelfCheck {
    pub version: String,
    pub os: String,
    pub build: u32,
    pub elevated: bool,
    pub helper_running: bool,
    pub capture_exclusion: bool,
    pub gamma_control: bool,
    pub brightness_displays: usize,
    pub displays: usize,
    pub virtual_display: bool,
    pub autostart_config: bool,
    pub autostart_actual: bool,
    pub mode: Mode,
    pub effective_mode: Mode,
    pub ranges: Vec<(String, String)>,
    pub unlock_chord: String,
    pub unlock_hold_ms: u64,
    pub strict_local_input: bool,
    pub suppress_lock: bool,
    pub state: RunState,
    pub night_mode: Option<Mode>,
    /// 每一条给用户看的结论，按"能/不能"写清楚。
    pub notes: Vec<String>,
}

impl SelfCheck {
    pub fn build(cfg: &Config, caps: &Capabilities) -> Self {
        let effective = crate::mode::resolve(cfg.mode);
        let mut notes = Vec::new();
        if caps.capture_exclusion {
            notes.push(
                "本机默认使用防窥黑幕：屏幕全黑，远程照常能看到桌面和操作，不会断开连接。"
                    .to_string(),
            );
        } else {
            notes.push(
                "系统不支持把窗口排除在屏幕采集之外（需要 Windows 10 2004 及以上），\
                 防窥黑幕无法保证远程看得见画面。请升级系统。"
                    .to_string(),
            );
        }
        if caps.brightness_displays == 0 {
            notes.push(
                "没有检测到支持 DDC/CI 调光的面板：屏幕会变成纯黑，但背光仍然亮着。\
                 多数一体机属于这种情况，属于正常限制。"
                    .to_string(),
            );
        } else {
            notes.push(format!(
                "{} 块面板支持背光调节，夜间会把背光调到 0。",
                caps.brightness_displays
            ));
        }
        if !cfg.strict_local_input {
            notes.push(
                "已关闭“严格本地防触控”：本机真实键鼠仍会送到桌面，只是看不到画面。".to_string(),
            );
        }
        if cfg.suppress_lock {
            notes.push(
                "夜间会抑制系统锁屏，否则登录界面会盖住黑幕。请自行评估安全影响。".to_string(),
            );
        }
        SelfCheck {
            version: VERSION.to_string(),
            os: caps.os.clone(),
            build: caps.build,
            elevated: caps.elevated,
            helper_running: caps.helper_running,
            capture_exclusion: caps.capture_exclusion,
            gamma_control: caps.gamma_control,
            brightness_displays: caps.brightness_displays,
            displays: caps.displays,
            virtual_display: caps.virtual_display,
            autostart_config: cfg.autostart,
            autostart_actual: caps.autostart,
            mode: cfg.mode,
            effective_mode: effective,
            ranges: cfg.range_strings(),
            unlock_chord: cfg.chord_text(),
            unlock_hold_ms: cfg.unlock_hold_ms,
            strict_local_input: cfg.strict_local_input,
            suppress_lock: cfg.suppress_lock,
            state: cfg.state,
            night_mode: cfg.night_mode,
            notes,
        }
    }
}

/// `1 小时 5 分` 这样的中文时长。
pub fn human_secs(secs: i64) -> String {
    let s = secs.max(0);
    let hours = s / 3600;
    let minutes = (s % 3600) / 60;
    match (hours, minutes) {
        (0, 0) => format!("{s} 秒"),
        (0, m) => format!("{m} 分"),
        (h, 0) => format!("{h} 小时"),
        (h, m) => format!("{h} 小时 {m} 分"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn cfg_at(h: u32, m: u32) -> (Config, chrono::DateTime<Local>) {
        let date = Local
            .with_ymd_and_hms(2026, 10, 29, 0, 0, 0)
            .unwrap()
            .date_naive();
        let now = Local
            .from_local_datetime(&date.and_hms_opt(h, m, 0).unwrap())
            .single()
            .unwrap();
        (Config::default(), now)
    }

    #[test]
    fn status_mentions_ranges_and_next_switch() {
        let (cfg, now) = cfg_at(18, 0);
        let text = status_text(&cfg, &Capabilities::default(), now);
        assert!(text.contains("20:00→10:30"), "{text}");
        assert!(text.contains("2 小时"), "{text}");
        assert!(text.contains(VERSION), "{text}");
    }

    #[test]
    fn status_flags_missing_night_while_scheduled() {
        let (cfg, now) = cfg_at(23, 0);
        let text = status_text(&cfg, &Capabilities::default(), now);
        assert!(text.contains("本应在夜间"), "{text}");
    }

    #[test]
    fn human_secs_reads_naturally() {
        assert_eq!(human_secs(45), "45 秒");
        assert_eq!(human_secs(600), "10 分");
        assert_eq!(human_secs(3600), "1 小时");
        assert_eq!(human_secs(3900), "1 小时 5 分");
        assert_eq!(human_secs(-5), "0 秒");
    }

    #[test]
    fn selfcheck_notes_explain_limits_in_chinese() {
        let cfg = Config::default();
        let caps = Capabilities {
            capture_exclusion: true,
            brightness_displays: 0,
            displays: 1,
            ..Default::default()
        };
        let report = SelfCheck::build(&cfg, &caps);
        assert_eq!(report.effective_mode, Mode::Guard);
        assert!(
            report.notes.iter().any(|n| n.contains("背光仍然亮着")),
            "{:?}",
            report.notes
        );
        assert!(
            report.notes.iter().any(|n| n.contains("防窥黑幕")),
            "{:?}",
            report.notes
        );
    }

    #[test]
    fn selfcheck_warns_when_capture_exclusion_missing() {
        let cfg = Config::default();
        let caps = Capabilities {
            capture_exclusion: false,
            ..Default::default()
        };
        let report = SelfCheck::build(&cfg, &caps);
        assert!(
            report.notes.iter().any(|n| n.contains("2004")),
            "{:?}",
            report.notes
        );
    }
}
