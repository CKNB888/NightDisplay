//! 显示模式与"这一晚算不算成功"的判定。
//!
//! 这是整个重写的核心决定：**默认模式绝不改动显示拓扑**。
//!
//! 旧版默认走 `SetDisplayConfig` 把实体屏从桌面摘掉，结果：
//! 1. 远程软件的采集源消失，画面卡住，必须重连；
//! 2. 显示器重新同步，本地会先亮一下再黑。
//!
//! 新版 `guard` 只叠三层东西，全部即时可逆：
//! * 每块屏一个满屏纯黑窗口，并用 `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`
//!   把自己排除在屏幕采集之外 —— 本机看是黑的，远程看是正常桌面；
//! * gamma 拉到全黑（在显示输出链路上，不进采集缓冲）；
//! * DDC/CI 背光调到 0（一体机如果不支持就跳过，只记日志）。
//!
//! 只要黑幕创建成功**且**采集排除校验通过，这一晚就算成功；gamma 和背光是加分项。

use serde::{Deserialize, Serialize};

/// 显示模式。
///
/// * `auto` — 默认。只走 `guard`，绝不改显示拓扑。
/// * `guard` — 黑幕 + gamma + 背光，全程可远程。
/// * `virtual_only` — 仅保留虚拟屏；**会中断远程连接**，只能手动选。
/// * `screen_off` — 直接关屏；动鼠标会亮，只能手动选。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Auto,
    Guard,
    VirtualOnly,
    ScreenOff,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Auto => "auto",
            Mode::Guard => "guard",
            Mode::VirtualOnly => "virtual_only",
            Mode::ScreenOff => "screen_off",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::Auto => "自动（防窥黑幕，推荐）",
            Mode::Guard => "防窥黑幕（本地黑，远程正常）",
            Mode::VirtualOnly => "仅虚拟屏（会中断远程连接）",
            Mode::ScreenOff => "基础关屏（动鼠标会亮）",
        }
    }

    /// 解析用户输入。旧版的 `overlay` 等同新版 `guard`。
    pub fn parse(s: &str) -> crate::NdResult<Self> {
        match s.trim() {
            "auto" => Ok(Mode::Auto),
            "guard" | "overlay" => Ok(Mode::Guard),
            "virtual_only" => Ok(Mode::VirtualOnly),
            "screen_off" => Ok(Mode::ScreenOff),
            other => Err(crate::error::user(format!(
                "不支持的模式“{other}”。可用：auto、guard、virtual_only、screen_off"
            ))),
        }
    }

    /// 这个模式会不会打断远程画面。UI 和文档必须如实标注。
    pub fn interrupts_remote(self) -> bool {
        matches!(self, Mode::VirtualOnly | Mode::ScreenOff)
    }

    pub const ALL: [Mode; 4] = [Mode::Auto, Mode::Guard, Mode::VirtualOnly, Mode::ScreenOff];
}

/// 单个附加层（gamma / 背光）的执行结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LayerStatus {
    /// 已生效。
    Applied,
    /// 硬件/驱动不支持，属于正常情况，不影响模式成功。
    Unsupported { reason: String },
    /// 尝试了但失败，记日志，不影响模式成功。
    Failed { reason: String },
    /// 用户在设置里关掉了这一层。
    Disabled,
}

impl LayerStatus {
    pub fn applied(&self) -> bool {
        matches!(self, LayerStatus::Applied)
    }

    pub fn summary(&self, label: &str) -> String {
        match self {
            LayerStatus::Applied => format!("{label}已生效"),
            LayerStatus::Unsupported { reason } => format!("{label}不支持（{reason}），已跳过"),
            LayerStatus::Failed { reason } => format!("{label}失败（{reason}），黑幕仍然有效"),
            LayerStatus::Disabled => format!("{label}已按设置关闭"),
        }
    }
}

/// 一个屏幕的黑幕层结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverlayOutcome {
    /// 显示器标识（设备名 + HMONITOR 序号）。
    pub display: String,
    /// 窗口创建并显示成功。
    pub shown: bool,
    /// `SetWindowDisplayAffinity` 返回成功，并且回读校验通过。
    pub capture_excluded: bool,
    /// `ShowWindow` 到第一帧黑色的实测耗时（毫秒）。
    pub first_frame_ms: u64,
    /// 失败原因。
    pub error: Option<String>,
}

impl OverlayOutcome {
    pub fn ok(&self) -> bool {
        self.shown && self.capture_excluded && self.error.is_none()
    }
}

/// 整次进入夜间的报告。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NightReport {
    /// 用户设置的模式。
    pub requested: Mode,
    /// 实际执行的模式（`auto` 会解析成具体模式）。
    pub effective: Mode,
    pub overlays: Vec<OverlayOutcome>,
    pub gamma: LayerStatus,
    pub brightness: LayerStatus,
    /// 本地键鼠吞掉是否生效（严格防触控）。
    pub local_input_guard: LayerStatus,
    /// 从开始进入到全部黑幕出现的最长耗时。
    pub start_latency_ms: u64,
    /// 背光控制不支持的屏幕名单，写进日志和 selfcheck。
    pub brightness_unsupported_displays: Vec<String>,
}

impl NightReport {
    /// 模式是否成功：黑幕全部显示 **且** 采集排除全部校验通过。
    pub fn success(&self) -> bool {
        !self.overlays.is_empty() && self.overlays.iter().all(|o| o.ok())
    }

    /// 失败原因，用于中文提示。
    pub fn failure_reason(&self) -> Option<String> {
        if self.overlays.is_empty() {
            return Some("没有创建任何黑幕窗口".into());
        }
        for o in &self.overlays {
            if !o.capture_excluded {
                return Some(format!(
                    "{} 的画面无法从屏幕采集中排除，远程会看到黑屏",
                    o.display
                ));
            }
            if !o.shown {
                return Some(format!(
                    "{} 的黑幕窗口未能显示：{}",
                    o.display,
                    o.error.clone().unwrap_or_else(|| "未知原因".into())
                ));
            }
        }
        None
    }

    /// 给日志和界面用的一行中文摘要。
    pub fn summary(&self) -> String {
        let overlay_part = if self.success() {
            format!("黑幕 {} 块屏全部排除采集", self.overlays.len())
        } else {
            self.failure_reason().unwrap_or_else(|| "黑幕未确认".into())
        };
        format!(
            "{}；{}；{}；首帧 {}ms",
            overlay_part,
            self.gamma.summary("gamma"),
            self.brightness.summary("背光"),
            self.start_latency_ms
        )
    }
}

/// `auto` 解析成具体模式。现在**只有** `guard`，不再降级到会断远程的模式。
pub fn resolve(mode: Mode) -> Mode {
    match mode {
        Mode::Auto => Mode::Guard,
        other => other,
    }
}

/// 进入夜间时按顺序尝试的模式列表。`auto` 只有一步。
///
/// 这里刻意不含 `virtual_only` / `screen_off`：那两种会切断或干扰远程采集，
/// 只能由用户在设置里明确选择，绝不在自动链路里悄悄发生。
pub fn attempt_chain(mode: Mode) -> Vec<Mode> {
    vec![resolve(mode)]
}

/// 黑幕失败后的建议，用于中文错误提示。
pub enum FailureAdvice {
    /// 明确告诉用户下一步该选什么，不要自己偷偷切模式。
    TellUser(String),
}

/// 进入夜间彻底失败时给用户的建议。
pub fn advice_on_failure(mode: Mode) -> FailureAdvice {
    let effective = resolve(mode);
    let text = match effective {
        Mode::Guard => {
            "防窥黑幕未能确认（窗口未显示或无法从采集排除），已放弃本次黑屏，屏幕保持正常。\
            请查看日志；如果你的远程软件只在重连后正常，可以在设置里手动选择“仅虚拟屏”，\
            但这种模式会中断远程画面一次。"
                .to_string()
        }
        Mode::VirtualOnly => {
            "仅虚拟屏切换失败，已回滚到原来的显示配置。请确认已安装虚拟显示驱动。".to_string()
        }
        Mode::ScreenOff => "关屏指令被系统拒绝，屏幕保持正常。".to_string(),
        Mode::Auto => unreachable!("resolve 不会返回 auto"),
    };
    FailureAdvice::TellUser(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlay(shown: bool, excluded: bool) -> OverlayOutcome {
        OverlayOutcome {
            display: "DISPLAY1".into(),
            shown,
            capture_excluded: excluded,
            first_frame_ms: 120,
            error: None,
        }
    }

    fn report(overlays: Vec<OverlayOutcome>) -> NightReport {
        NightReport {
            requested: Mode::Auto,
            effective: Mode::Guard,
            overlays,
            gamma: LayerStatus::Applied,
            brightness: LayerStatus::Unsupported {
                reason: "VCP 0x10 不支持".into(),
            },
            local_input_guard: LayerStatus::Applied,
            start_latency_ms: 130,
            brightness_unsupported_displays: vec!["DISPLAY1".into()],
        }
    }

    #[test]
    fn auto_never_falls_back_to_topology_change() {
        assert_eq!(attempt_chain(Mode::Auto), vec![Mode::Guard]);
        assert_eq!(attempt_chain(Mode::Guard), vec![Mode::Guard]);
        assert!(!attempt_chain(Mode::Auto).contains(&Mode::VirtualOnly));
        assert!(!attempt_chain(Mode::Auto).contains(&Mode::ScreenOff));
    }

    #[test]
    fn opt_in_modes_stay_as_chosen() {
        assert_eq!(attempt_chain(Mode::VirtualOnly), vec![Mode::VirtualOnly]);
        assert_eq!(attempt_chain(Mode::ScreenOff), vec![Mode::ScreenOff]);
    }

    #[test]
    fn success_requires_capture_exclusion_on_every_display() {
        assert!(report(vec![overlay(true, true), overlay(true, true)]).success());
        assert!(!report(vec![overlay(true, true), overlay(true, false)]).success());
        assert!(!report(vec![overlay(false, true)]).success());
        assert!(!report(vec![]).success());
    }

    #[test]
    fn unsupported_brightness_still_counts_as_success() {
        let r = report(vec![overlay(true, true)]);
        assert_eq!(r.gamma, LayerStatus::Applied);
        assert!(matches!(r.brightness, LayerStatus::Unsupported { .. }));
        assert!(r.success(), "背光不支持只是加分项缺失，模式仍算成功");
    }

    #[test]
    fn failure_reason_mentions_capture_exclusion() {
        let r = report(vec![overlay(true, false)]);
        let reason = r.failure_reason().unwrap();
        assert!(reason.contains("采集"), "{reason}");
    }

    #[test]
    fn summary_is_chinese_and_mentions_latency() {
        let text = report(vec![overlay(true, true)]).summary();
        assert!(text.contains("黑幕"), "{text}");
        assert!(text.contains("ms"), "{text}");
    }

    #[test]
    fn advice_is_never_silent_fallback() {
        match advice_on_failure(Mode::Auto) {
            FailureAdvice::TellUser(t) => {
                assert!(t.contains("仅虚拟屏"));
                assert!(t.contains("中断"));
            }
        }
    }
}
