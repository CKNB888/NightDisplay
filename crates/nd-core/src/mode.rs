//! 显示模式与"这一晚算不算成功"的判定。
//!
//! ## 主路径是虚拟屏，不是黑幕
//!
//! 产品的核心一直是"把实体屏从桌面上摘掉，让远程软件看虚拟屏"。旧版在这一点上
//! 做对了方向，但机制有问题，用户真实遇到的两个毛病：
//!
//! 1. **点黑屏瞬间远程就卡住，必须重连** —— 旧版把实体屏的 target 从活动路径集里
//!    整个删掉。远程软件抓着的那块采集表面跟着消失，画面就冻在那里了。
//! 2. **远程进来会亮一秒才黑** —— 旧版分两步做（先关实体屏、再开虚拟屏），
//!    显示器中间重新同步了一次。
//!
//! 4.0 的修法不是换掉虚拟屏，而是把它做对：
//!
//! * 虚拟屏（VDD）常驻，输出一直是活的，远程软件总能枚举到同一块稳定的显示；
//! * 入夜只做**一次** `SetDisplayConfig`，用
//!   `SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES` 提交完整的路径集
//!   （不是 `SDC_TOPOLOGY_*`）：实体屏的 target 保持 ACTIVE，只把它的 source
//!   摘掉（"接着但没桌面"），虚拟屏的 source 接管桌面；
//! * 活动 target 集合永远不为空，显示数量不变，操作系统不会拆掉采集表面；
//! * 应用前先 `SDC_VALIDATE`，校验不过就什么都不做。
//!
//! 只有在这台机器的显卡/驱动上确实做不到"无中断切换"时，才**自动降级**到黑幕
//! 模式并明确告诉用户 —— 绝不静默替换机制。
//!
//! ## 模式顺序
//!
//! `virtual_screen`（默认，主力）→ `overlay`（只在虚拟屏不可用时自动兜底）→ `screen_off`（手动）。

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

/// 显示模式。
///
/// * `auto` — 默认。先试 `virtual_screen`，不可用时自动降级到 `overlay`。
/// * `virtual_screen` — 实体屏摘掉桌面、虚拟屏接管。本机真黑，远程照常。
/// * `overlay` — 黑幕 + 采集排除。不需要改拓扑，作为兜底。
/// * `screen_off` — 直接关屏；动鼠标会亮，只能手动选。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Auto,
    /// 旧配置里叫 `virtual_only`，读得进来。
    #[serde(alias = "virtual_only")]
    VirtualScreen,
    /// 旧配置里叫 `guard`，读得进来。
    #[serde(alias = "guard")]
    Overlay,
    ScreenOff,
}

impl Mode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Auto => "auto",
            Mode::VirtualScreen => "virtual_screen",
            Mode::Overlay => "overlay",
            Mode::ScreenOff => "screen_off",
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Mode::Auto => "自动（虚拟屏，失败时用黑幕兜底）",
            Mode::VirtualScreen => "虚拟屏（本机真黑，远程正常，推荐）",
            Mode::Overlay => "黑幕兜底（不改显示配置）",
            Mode::ScreenOff => "基础关屏（动鼠标会亮）",
        }
    }

    /// 解析用户输入。旧名字都认：`virtual_only`、`guard`、`overlay`。
    pub fn parse(s: &str) -> crate::NdResult<Self> {
        match s.trim() {
            "auto" => Ok(Mode::Auto),
            "virtual_screen" | "virtual_only" | "virtual" => Ok(Mode::VirtualScreen),
            "overlay" | "guard" => Ok(Mode::Overlay),
            "screen_off" => Ok(Mode::ScreenOff),
            other => Err(crate::error::user(format!(
                "不支持的模式“{other}”。可用：auto、virtual_screen、overlay、screen_off"
            ))),
        }
    }

    /// 这个模式会不会打断远程画面。
    ///
    /// `virtual_screen` 设计上**不会**打断（采集表面一直活着）；
    /// `screen_off` 会（显示器断电，采集源跟着没）。
    #[must_use]
    pub fn interrupts_remote(self) -> bool {
        matches!(self, Mode::ScreenOff)
    }

    pub const ALL: [Mode; 4] = [
        Mode::Auto,
        Mode::VirtualScreen,
        Mode::Overlay,
        Mode::ScreenOff,
    ];
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
    #[must_use]
    pub fn applied(&self) -> bool {
        matches!(self, LayerStatus::Applied)
    }

    #[must_use]
    pub fn summary(&self, label: &str) -> String {
        match self {
            LayerStatus::Applied => format!("{label}已生效"),
            LayerStatus::Unsupported { reason } => format!("{label}不支持（{reason}），已跳过"),
            LayerStatus::Failed { reason } => format!("{label}失败（{reason}）"),
            LayerStatus::Disabled => format!("{label}已按设置关闭"),
        }
    }
}

/// 一个屏幕的黑幕层结果（`overlay` 模式用）。
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
    #[must_use]
    pub fn ok(&self) -> bool {
        self.shown && self.capture_excluded && self.error.is_none()
    }
}

/// 虚拟屏切换的实测结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct VirtualReport {
    /// 切换是否成功（应用 + 校验都过）。
    pub applied: bool,
    /// 接管桌面的虚拟屏名。
    pub virtual_display: String,
    /// 保持"接着但没桌面"的实体屏块数。
    pub detached_physical: usize,
    /// 切换前活动 target 数（必须 > 0，切换后也不能是 0）。
    pub active_before: usize,
    /// 切换后活动 target 数。
    pub active_after: usize,
    /// 虚拟屏的模式是不是和原实体屏一致（不一致会让合成器重新初始化）。
    pub mode_matched: bool,
    /// `SDC_VALIDATE` 耗时（毫秒）。
    pub validate_ms: u64,
    /// 单次 `SDC_APPLY` 耗时（毫秒）。
    pub apply_ms: u64,
    /// 从提交到校验通过的耗时（毫秒），用于和"亮一秒"对比。
    pub settle_ms: u64,
    /// 失败原因（`applied == false` 时一定有）。
    pub error: Option<String>,
}

impl VirtualReport {
    pub fn failure(reason: impl Into<String>) -> Self {
        Self {
            error: Some(reason.into()),
            ..Default::default()
        }
    }

    #[must_use]
    pub fn summary(&self) -> String {
        if !self.applied {
            return format!(
                "虚拟屏切换未生效：{}",
                self.error.clone().unwrap_or_else(|| "未知原因".into())
            );
        }
        format!(
            "虚拟屏 {} 接管桌面，{} 块实体屏保持枚举，活动 target {}→{}，{}；\
             校验 {}ms + 应用 {}ms + 稳定 {}ms",
            if self.virtual_display.is_empty() {
                "（未命名）"
            } else {
                &self.virtual_display
            },
            self.detached_physical,
            self.active_before,
            self.active_after,
            if self.mode_matched {
                "模式与实体屏一致"
            } else {
                "模式未能对齐（可能有一次重新初始化）"
            },
            self.validate_ms,
            self.apply_ms,
            self.settle_ms
        )
    }
}

/// 整次进入夜间的报告。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NightReport {
    /// 用户设置的模式。
    pub requested: Mode,
    /// 实际生效的模式（`auto` 可能降级成 `overlay`）。
    pub effective: Mode,
    /// 虚拟屏切换结果（只有 `virtual_screen` 有）。
    pub virtual_screen: Option<VirtualReport>,
    /// 降级到黑幕时给出的原因，会写日志并告诉用户。
    pub fallback_reason: Option<String>,
    pub overlays: Vec<OverlayOutcome>,
    pub gamma: LayerStatus,
    pub brightness: LayerStatus,
    /// 本地键鼠吞掉是否生效（严格防触控）。
    pub local_input_guard: LayerStatus,
    /// 从开始进入到确认完成的总耗时。
    pub start_latency_ms: u64,
    /// 背光控制不支持的屏幕名单，写进日志和 selfcheck。
    pub brightness_unsupported_displays: Vec<String>,
}

impl NightReport {
    /// 模式是否成功。
    ///
    /// * `virtual_screen`：切换应用成功 **且** 校验通过（活动 target 不为空）；
    /// * `overlay`：黑幕全部显示 **且** 采集排除全部校验通过；
    /// * `screen_off`：由调用方按指令是否被接受判定。
    #[must_use]
    pub fn success(&self) -> bool {
        match self.effective {
            Mode::VirtualScreen => self
                .virtual_screen
                .as_ref()
                .is_some_and(|v| v.applied && v.active_after > 0),
            Mode::Overlay => {
                !self.overlays.is_empty() && self.overlays.iter().all(OverlayOutcome::ok)
            }
            Mode::ScreenOff => true,
            Mode::Auto => false,
        }
    }

    /// 失败原因，用于中文提示。
    #[must_use]
    pub fn failure_reason(&self) -> Option<String> {
        if self.effective == Mode::VirtualScreen {
            let v = self.virtual_screen.as_ref()?;
            if v.applied && v.active_after > 0 {
                return None;
            }
            Some(
                v.error
                    .clone()
                    .unwrap_or_else(|| "虚拟屏切换后没有留下任何活动显示".to_string()),
            )
        } else {
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
    }

    /// 给日志和界面用的一行中文摘要。
    #[must_use]
    pub fn summary(&self) -> String {
        let main = match self.effective {
            Mode::VirtualScreen => self
                .virtual_screen
                .as_ref()
                .map_or_else(|| "虚拟屏未执行".into(), VirtualReport::summary),
            Mode::Overlay => {
                if self.success() {
                    format!("黑幕 {} 块屏全部排除采集", self.overlays.len())
                } else {
                    self.failure_reason().unwrap_or_else(|| "黑幕未确认".into())
                }
            }
            Mode::ScreenOff => "已发送关屏指令".into(),
            Mode::Auto => "未执行".into(),
        };
        let mut text = format!(
            "{main}；{}；{}",
            self.gamma.summary("gamma"),
            self.brightness.summary("背光")
        );
        if let Some(reason) = &self.fallback_reason {
            // Writing into a String cannot fail.
            let _ = write!(
                text,
                "；虚拟屏模式在这台机器上没能生效，已自动降级到黑幕兜底：{reason}"
            );
        }
        let _ = write!(text, "；总耗时 {}ms", self.start_latency_ms);
        text
    }
}

/// `auto` 解析成具体模式：默认就是虚拟屏。
#[must_use]
pub fn resolve(mode: Mode) -> Mode {
    match mode {
        Mode::Auto => Mode::VirtualScreen,
        other => other,
    }
}

/// 进入夜间时按顺序尝试的模式列表。
///
/// * `auto` → 先虚拟屏，不行再黑幕（黑幕是唯一允许的自动降级）；
/// * 手动选 `overlay` / `screen_off` → 只用它，不偷偷换；
/// * 手动选 `virtual_screen` → 就试虚拟屏，失败如实报告（用户明确要这个）。
///
/// `screen_off` 永远不会出现在自动链路里。
#[must_use]
pub fn attempt_chain(mode: Mode) -> Vec<Mode> {
    match mode {
        Mode::Auto => vec![Mode::VirtualScreen, Mode::Overlay],
        other => vec![other],
    }
}

/// 黑幕失败后的建议，用于中文错误提示。
pub enum FailureAdvice {
    /// 明确告诉用户下一步该选什么，不要自己偷偷切模式。
    TellUser(String),
}

/// 进入夜间彻底失败时给用户的建议。
#[must_use]
pub fn advice_on_failure(mode: Mode) -> FailureAdvice {
    let effective = resolve(mode);
    let text = match effective {
        Mode::VirtualScreen | Mode::Overlay => {
            "两种模式都没能确认成功，已放弃本次黑屏，屏幕保持正常。请查看日志；\
             如果是虚拟屏切换被显卡驱动拒绝，向导会给出可以照抄的手动命令。"
                .to_string()
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

    fn virtual_ok() -> VirtualReport {
        VirtualReport {
            applied: true,
            virtual_display: "VDD by MTT".into(),
            detached_physical: 1,
            active_before: 1,
            active_after: 2,
            mode_matched: true,
            validate_ms: 3,
            apply_ms: 42,
            settle_ms: 60,
            error: None,
        }
    }

    fn report(effective: Mode, overlays: Vec<OverlayOutcome>) -> NightReport {
        NightReport {
            requested: Mode::Auto,
            effective,
            virtual_screen: None,
            fallback_reason: None,
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
    fn auto_prefers_virtual_screen_and_falls_back_to_overlay() {
        assert_eq!(resolve(Mode::Auto), Mode::VirtualScreen);
        assert_eq!(
            attempt_chain(Mode::Auto),
            vec![Mode::VirtualScreen, Mode::Overlay]
        );
        // 屏幕关断永远不进自动链路。
        assert!(!attempt_chain(Mode::Auto).contains(&Mode::ScreenOff));
    }

    #[test]
    fn explicit_modes_are_not_silently_replaced() {
        assert_eq!(attempt_chain(Mode::Overlay), vec![Mode::Overlay]);
        assert_eq!(
            attempt_chain(Mode::VirtualScreen),
            vec![Mode::VirtualScreen],
            "用户明确选了虚拟屏，失败就如实报告，不要偷偷换成黑幕"
        );
    }

    #[test]
    fn virtual_success_needs_active_targets_left() {
        let mut r = report(Mode::VirtualScreen, vec![]);
        r.virtual_screen = Some(virtual_ok());
        assert!(r.success());
        assert!(r.summary().contains("虚拟屏"));

        let mut broken = virtual_ok();
        broken.active_after = 0;
        r.virtual_screen = Some(broken);
        assert!(!r.success(), "活动 target 归零就是失败");
        assert!(r.failure_reason().is_some());
    }

    #[test]
    fn overlay_success_requires_capture_exclusion_on_every_display() {
        let mut r = report(
            Mode::Overlay,
            vec![overlay(true, true), overlay(true, true)],
        );
        assert!(r.success());
        r.overlays = vec![overlay(true, true), overlay(true, false)];
        assert!(!r.success());
        r.overlays = vec![overlay(false, true)];
        assert!(!r.success());
        r.overlays = vec![];
        assert!(!r.success());
    }

    #[test]
    fn unsupported_brightness_still_counts_as_success() {
        let r = report(Mode::Overlay, vec![overlay(true, true)]);
        assert_eq!(r.gamma, LayerStatus::Applied);
        assert!(matches!(r.brightness, LayerStatus::Unsupported { .. }));
        assert!(r.success(), "背光不支持只是加分项缺失，模式仍算成功");
    }

    #[test]
    fn failure_reason_mentions_capture_exclusion() {
        let r = report(Mode::Overlay, vec![overlay(true, false)]);
        let reason = r.failure_reason().unwrap();
        assert!(reason.contains("采集"), "{reason}");
    }

    #[test]
    fn summary_mentions_latency_and_fallback() {
        let mut r = report(Mode::Overlay, vec![overlay(true, true)]);
        r.fallback_reason = Some("显卡驱动拒绝无中断切换".into());
        let text = r.summary();
        assert!(text.contains("黑幕"), "{text}");
        assert!(text.contains("ms"), "{text}");
        assert!(text.contains("降级"), "{text}");
    }

    #[test]
    fn virtual_report_summary_is_honest_about_mode_mismatch() {
        let mut v = virtual_ok();
        v.mode_matched = false;
        let text = v.summary();
        assert!(text.contains("未能对齐"), "{text}");
    }

    #[test]
    fn advice_is_never_silent_fallback() {
        match advice_on_failure(Mode::Auto) {
            FailureAdvice::TellUser(t) => {
                assert!(t.contains("日志"), "{t}");
            }
        }
    }

    #[test]
    fn old_mode_names_still_parse() {
        assert_eq!(Mode::parse("virtual_only").unwrap(), Mode::VirtualScreen);
        assert_eq!(Mode::parse("guard").unwrap(), Mode::Overlay);
        assert_eq!(Mode::parse("overlay").unwrap(), Mode::Overlay);
        // 旧配置值也要能反序列化。
        let m: Mode = serde_json::from_str("\"virtual_only\"").unwrap();
        assert_eq!(m, Mode::VirtualScreen);
        let g: Mode = serde_json::from_str("\"guard\"").unwrap();
        assert_eq!(g, Mode::Overlay);
    }

    #[test]
    fn only_screen_off_interrupts_remote() {
        assert!(!Mode::VirtualScreen.interrupts_remote());
        assert!(!Mode::Overlay.interrupts_remote());
        assert!(Mode::ScreenOff.interrupts_remote());
    }
}
