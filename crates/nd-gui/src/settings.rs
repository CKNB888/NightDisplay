//! 设置页的写入路径：把前端表单变成一次配置更新 + 一次系统登记。
//!
//! 所有校验都用引擎/核心里的那套函数（组合键、时间段、按住时长），
//! 界面自己**不**重新判断一遍 —— 否则两边的规则迟早会分叉。

use anyhow::Result;
use nd_core::chord::{self, ChordKey};
use nd_core::config::Config;
use nd_core::engine::Platform;
use nd_core::error::user;
use nd_core::schedule::{MinuteOfDay, NightRange};
use serde::Deserialize;

/// 前端提交的设置。字段为 `None` 表示"这次不改这一项"。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct SettingsPatch {
    /// `auto` / `virtual_screen` / `overlay` / `screen_off`。
    pub mode: Option<String>,
    /// 主时间段，`"20:00"`。
    pub start: Option<String>,
    /// 主时间段，`"10:30"`。
    pub end: Option<String>,
    /// 组合键，例如 `["Ctrl", "K"]`。
    pub chord: Option<Vec<String>>,
    /// 按住时长（毫秒）。
    pub hold_ms: Option<u64>,
    pub strict_local_input: Option<bool>,
    pub suppress_lock: Option<bool>,
    pub keep_awake: Option<bool>,
    pub lock: Option<bool>,
    pub autostart: Option<bool>,
    pub high_performance_at_night: Option<bool>,
}

impl SettingsPatch {
    /// 这个补丁有没有真的改动东西。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// 补丁涉及的三项需要重新登记系统计划：模式、时间、自启偏好。
    #[must_use]
    pub fn touches_schedule(&self) -> bool {
        self.mode.is_some()
            || self.start.is_some()
            || self.end.is_some()
            || self.autostart.is_some()
    }
}

/// 把补丁里的字符串解析成组合键数组（错误信息直接给用户看）。
pub fn parse_chord(names: &[String]) -> Result<Vec<ChordKey>> {
    let keys: Result<Vec<ChordKey>> = names.iter().map(|n| ChordKey::parse(n)).collect();
    chord::validate(&keys?)
}

/// 时间字符串必须是 `HH:MM`。
fn parse_hm(text: &str, label: &str) -> Result<MinuteOfDay> {
    text.trim()
        .parse::<MinuteOfDay>()
        .map_err(|_| user(format!("{label}格式不对，应为 HH:MM（现在是“{text}”）")))
}

/// 应用补丁。返回一句给用户看的中文说明。
///
/// 顺序很重要：
/// 1. 先把新值算好并整体校验（不合法就一个字都不改）；
/// 2. 需要的话先登记系统计划（失败就不落盘，屏幕行为保持不变）；
/// 3. 最后写配置并让新设置立刻生效。
pub fn apply<P: Platform + ?Sized>(platform: &P, patch: &SettingsPatch) -> Result<String> {
    if patch.is_empty() {
        return Ok("没有需要保存的改动".to_string());
    }

    let before = Config::load()?;
    let mut after = before.clone();

    if let Some(mode) = patch.mode.as_deref() {
        after.mode = nd_core::mode::Mode::parse(mode)?;
    }
    if patch.start.is_some() || patch.end.is_some() {
        let start = match patch.start.as_deref() {
            Some(text) => parse_hm(text, "开始时间")?,
            None => after.ranges[0].0.start,
        };
        let end = match patch.end.as_deref() {
            Some(text) => parse_hm(text, "结束时间")?,
            None => after.ranges[0].0.end,
        };
        after.ranges[0].0 = NightRange::new(start, end)?;
    }
    if let Some(names) = patch.chord.as_deref() {
        after.unlock_chord = parse_chord(names)?;
    }
    if let Some(ms) = patch.hold_ms {
        after.unlock_hold_ms = chord::validate_hold_ms(ms)?;
    }
    if let Some(value) = patch.strict_local_input {
        after.strict_local_input = value;
    }
    if let Some(value) = patch.suppress_lock {
        after.suppress_lock = value;
    }
    if let Some(value) = patch.keep_awake {
        after.keep_awake = value;
    }
    if let Some(value) = patch.lock {
        after.lock = value;
    }
    if let Some(value) = patch.high_performance_at_night {
        after.high_performance_at_night = value;
    }
    if let Some(value) = patch.autostart {
        after.autostart_preference = value;
        after.autostart = value;
    }
    // 整体校验：时间段、组合键、按住时长一起过一遍。
    after.validate()?;

    // 2) 需要登记系统计划时先做，失败就整个放弃（配置保持原样）。
    if patch.touches_schedule() {
        if let Err(e) = platform.install_schedule(&after) {
            // 回滚：把刚登记的按旧配置复原，别留下半套任务。
            let _ = platform.install_schedule(&before);
            return Err(user(format!(
                "写入系统计划任务失败：{}",
                nd_core::error::one_line(&e)
            )));
        }
    }

    // 3) 落盘。
    let chord_text = after.chord_text();
    Config::update(|c| {
        *c = after.clone();
        Ok(())
    })?;

    // 自启开关单独再确认一次真实状态（界面上要显示"实际"而不是"希望"）。
    let mut notes = Vec::new();
    if let Some(on) = patch.autostart {
        if let Err(e) = platform.set_autostart(on) {
            notes.push(format!(
                "配置已保存，但自启{}：{}",
                if on { "注册" } else { "注销" },
                nd_core::error::one_line(&e)
            ));
        }
    }
    // 严格防触控关掉时，立即把钩子收掉，不必等下一次夜间。
    if patch.strict_local_input == Some(false) {
        if let Err(e) = platform.set_input_guard(false, &[], 0) {
            notes.push(format!(
                "解除本地防触控失败：{}",
                nd_core::error::one_line(&e)
            ));
        }
    }

    let mut message = format!("设置已保存（解锁组合键 {chord_text}）");
    if !notes.is_empty() {
        message.push('；');
        message.push_str(&notes.join("；"));
    }
    tracing::info!(message = %message, "设置已更新");
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nd_core::engine::testing::FakePlatform;

    /// 每个测试用自己的数据目录，互不干扰。
    fn with_home<T>(f: impl FnOnce() -> T) -> T {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("NIGHTDISPLAY_HOME", dir.path());
        let out = f();
        std::env::remove_var("NIGHTDISPLAY_HOME");
        out
    }

    fn seed() {
        nd_core::paths::ensure_home().unwrap();
        nd_core::config::write_to(&nd_core::paths::config_file(), &Config::default()).unwrap();
    }

    #[test]
    fn empty_patch_changes_nothing() {
        with_home(|| {
            seed();
            let platform = FakePlatform::default();
            let message = apply(&platform, &SettingsPatch::default()).unwrap();
            assert!(message.contains("没有需要保存"));
            assert!(!platform.called("install_schedule"));
        });
    }

    #[test]
    fn updates_mode_and_range() {
        with_home(|| {
            seed();
            let platform = FakePlatform::default();
            let patch = SettingsPatch {
                mode: Some("overlay".into()),
                start: Some("21:15".into()),
                end: Some("08:45".into()),
                ..Default::default()
            };
            apply(&platform, &patch).unwrap();
            let cfg = Config::load().unwrap();
            assert_eq!(cfg.mode, nd_core::mode::Mode::Overlay);
            assert_eq!(cfg.primary_range().start.to_hm(), "21:15");
            assert_eq!(cfg.primary_range().end.to_hm(), "08:45");
            assert!(platform.called("install_schedule"));
        });
    }

    #[test]
    fn rejects_bad_chord_without_saving() {
        with_home(|| {
            seed();
            let platform = FakePlatform::default();
            let patch = SettingsPatch {
                chord: Some(vec!["Ctrl".into()]),
                mode: Some("overlay".into()),
                ..Default::default()
            };
            assert!(apply(&platform, &patch).is_err());
            let cfg = Config::load().unwrap();
            assert_eq!(cfg.mode, nd_core::mode::Mode::Auto, "校验失败不能落盘");
        });
    }

    #[test]
    fn rejects_identical_start_and_end() {
        with_home(|| {
            seed();
            let platform = FakePlatform::default();
            let patch = SettingsPatch {
                start: Some("20:00".into()),
                end: Some("20:00".into()),
                ..Default::default()
            };
            assert!(apply(&platform, &patch).is_err());
        });
    }

    #[test]
    fn schedule_failure_keeps_old_config() {
        with_home(|| {
            seed();
            let platform = FakePlatform::default();
            *platform.schedule_fails.borrow_mut() = true;
            let patch = SettingsPatch {
                mode: Some("screen_off".into()),
                ..Default::default()
            };
            assert!(apply(&platform, &patch).is_err());
            let cfg = Config::load().unwrap();
            assert_eq!(cfg.mode, nd_core::mode::Mode::Auto);
        });
    }

    #[test]
    fn chord_patch_is_normalized() {
        with_home(|| {
            seed();
            let platform = FakePlatform::default();
            let patch = SettingsPatch {
                chord: Some(vec!["k".into(), "ctrl".into(), "K".into()]),
                hold_ms: Some(500),
                ..Default::default()
            };
            apply(&platform, &patch).unwrap();
            let cfg = Config::load().unwrap();
            assert_eq!(cfg.chord_text(), "K + Ctrl");
            assert_eq!(cfg.unlock_hold_ms, 500);
        });
    }
}
