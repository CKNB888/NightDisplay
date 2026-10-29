//! 状态文本与自检报告（纯逻辑，平台信息由调用方注入）。

use std::fmt::Write as _;

use chrono::Local;
use serde::{Deserialize, Serialize};

use crate::config::{Config, Mode, RunState};
use crate::schedule::minute_of_day;
use crate::VERSION;

/// 开机自启 / 计划任务的**真实**注册状态（不是配置里的开关）。
///
/// 用户能在「设置」里看到的是这个，而不是"我希望它开着"。三者任何一条
/// 都能把程序拉起来，所以要分开报告：Run 键、登录任务、开机任务。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registration {
    /// HKCU Run 里有这一项，且没在「启动」里被禁用。
    pub run_key: bool,
    /// 登录时触发的计划任务存在（普通用户权限即可）。
    pub task_logon: bool,
    /// 开机时触发的计划任务存在（SYSTEM、最高权限）。
    pub task_boot: bool,
    /// 看门狗任务存在。
    pub watchdog_task: bool,
    /// 每日边界任务的数量。
    pub schedule_tasks: usize,
    /// 诊断文字：命令是旧路径、被安全软件禁用之类的具体原因。
    #[serde(default)]
    pub run_key_note: String,
}

impl Registration {
    /// 至少有一条自启路径能用。
    #[must_use]
    pub fn any(&self) -> bool {
        self.run_key || self.task_logon || self.task_boot
    }

    /// 三条路径都在。
    #[must_use]
    pub fn all(&self) -> bool {
        self.run_key && self.task_logon && self.task_boot
    }

    /// 逐条写出真实状态，设置页和 `--action env` 共用。
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mark = |on: bool| if on { "已注册" } else { "未注册" };
        let run_key = if self.run_key_note.is_empty() {
            mark(self.run_key).to_string()
        } else {
            format!("{}（{}）", mark(self.run_key), self.run_key_note)
        };
        vec![
            format!("开机自启 · HKCU Run：{run_key}"),
            format!("开机自启 · 登录任务：{}", mark(self.task_logon)),
            format!("开机自启 · 开机任务（SYSTEM）：{}", mark(self.task_boot)),
            format!("看门狗任务：{}", mark(self.watchdog_task)),
            format!("每日切换任务：{} 个", self.schedule_tasks),
        ]
    }

    /// 一句话总结，状态文本里用。
    #[must_use]
    pub fn summary(&self) -> String {
        let count = [self.run_key, self.task_logon, self.task_boot]
            .iter()
            .filter(|on| **on)
            .count();
        match count {
            3 => "三条自启路径都已注册".into(),
            0 => "没有注册任何自启路径".into(),
            n => format!("{n}/3 条自启路径已注册"),
        }
    }
}

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
    /// 虚拟屏当前正接着桌面（`virtual_screen` 模式生效中）。
    #[serde(default)]
    pub virtual_screen_holding: bool,
    /// 至少有一条自启路径可用（旧字段，等价于 `registration.any()`）。
    pub autostart: bool,
    /// 每条自启路径的真实状态。
    #[serde(default)]
    pub registration: Registration,
}

/// 一行状态文本。
#[must_use]
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
        .map_or_else(|| "—".into(), |m| m.label().to_string());
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
    /// 虚拟屏此刻正接着桌面（夜间状态）。
    #[serde(default)]
    pub virtual_screen_holding: bool,
    /// 三条自启路径 + 看门狗 + 每日任务的真实状态。
    #[serde(default)]
    pub registration: Registration,
    /// 显卡与驱动版本（平台层填，非 Windows 为空）。
    #[serde(default)]
    pub gpu: Vec<GpuInfo>,
    /// 每一块显示器：名字、分辨率、是否支持 DDC/CI。
    #[serde(default)]
    pub monitors: Vec<MonitorInfo>,
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

/// 显卡与驱动版本。诊断包里靠它判断"这个组合能不能无闪切换"。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuInfo {
    pub name: String,
    pub driver_version: String,
    #[serde(default)]
    pub driver_date: String,
}

impl GpuInfo {
    #[must_use]
    pub fn label(&self) -> String {
        if self.driver_version.is_empty() {
            self.name.clone()
        } else {
            format!("{}（驱动 {}）", self.name, self.driver_version)
        }
    }
}

/// 一块显示器。`ddc` 为真表示支持 DDC/CI 背光。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorInfo {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
    pub ddc: bool,
    pub virtual_output: bool,
}

impl MonitorInfo {
    #[must_use]
    pub fn label(&self) -> String {
        let mut out = if self.name.is_empty() {
            "未命名显示器".to_string()
        } else {
            self.name.clone()
        };
        if self.width > 0 && self.height > 0 {
            // Writing into a String cannot fail.
            let _ = write!(out, " {}×{}", self.width, self.height);
            if self.refresh_hz > 0 {
                let _ = write!(out, "@{}Hz", self.refresh_hz);
            }
        }
        if self.virtual_output {
            out.push_str("（虚拟屏）");
        }
        if self.ddc {
            out.push_str(" · DDC/CI");
        }
        out
    }
}

impl SelfCheck {
    #[must_use]
    pub fn build(cfg: &Config, caps: &Capabilities) -> Self {
        let effective = crate::mode::resolve(cfg.mode);
        let mut notes = Vec::new();

        // 第一句永远是"这台机器会怎么黑"。
        match effective {
            Mode::VirtualScreen => notes.push(
                "默认模式是虚拟屏：夜里把桌面交给虚拟屏，实体屏不再显示任何东西，\
                 远程工具仍然能正常看到并操作桌面。"
                    .to_string(),
            ),
            Mode::Overlay => {
                notes
                    .push("当前使用防窥黑幕兜底：屏幕全黑，远程照常能看到桌面和操作。".to_string());
            }
            Mode::ScreenOff => notes.push(
                "当前使用基础关屏：只关显示器，动一下鼠标就会亮，远程画面会中断。".to_string(),
            ),
            Mode::Auto => notes.push("当前是自动模式。".to_string()),
        }

        if !caps.virtual_display {
            notes.push(
                "没有检测到虚拟显示器（VDD）。虚拟屏是默认模式的核心，\
                 请先运行安装向导的第 2 步安装虚拟显示驱动。"
                    .to_string(),
            );
        } else if caps.virtual_screen_holding {
            notes.push("虚拟屏当前正接着桌面（夜间状态）。".to_string());
        } else {
            notes.push("虚拟屏已就绪、正在待命，不影响白天的桌面布局。".to_string());
        }

        if effective == Mode::VirtualScreen && !caps.capture_exclusion {
            notes.push(
                "系统版本较旧（需要 Windows 10 2004 及以上）。虚拟屏模式本身不依赖采集排除，\
                 但如果被迫降级到黑幕，远程可能跟着变黑。"
                    .to_string(),
            );
        }

        if caps.brightness_displays == 0 {
            notes.push(
                "没有检测到支持 DDC/CI 调光的面板。虚拟屏模式下本机不显示画面，\
                 所以这不影响使用；只有降级到黑幕时背光才会亮着。"
                    .to_string(),
            );
        } else {
            notes.push(format!(
                "{} 块面板支持背光调节，降级到黑幕时会把背光调到 0。",
                caps.brightness_displays
            ));
        }

        if effective == Mode::Overlay {
            if !cfg.strict_local_input {
                notes.push(
                    "已关闭“严格本地防触控”：本机真实键鼠仍会送到桌面，只是看不到画面。"
                        .to_string(),
                );
            }
            if cfg.suppress_lock {
                notes.push(
                    "夜间会抑制系统锁屏，否则登录界面会盖住黑幕。请自行评估安全影响。".to_string(),
                );
            }
        }

        let registration = caps.registration.clone();
        if !registration.all() {
            notes.push(registration.summary());
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
            virtual_screen_holding: caps.virtual_screen_holding,
            registration,
            gpu: Vec::new(),
            monitors: Vec::new(),
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
#[must_use]
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
        // 默认模式是虚拟屏，不是黑幕。
        assert_eq!(report.effective_mode, Mode::VirtualScreen);
        assert!(
            report.notes.iter().any(|n| n.contains("虚拟显示器")),
            "没装 VDD 必须明说：{:?}",
            report.notes
        );
        assert!(
            report.notes.iter().any(|n| n.contains("虚拟屏")),
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
