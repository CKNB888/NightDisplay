//! 解锁组合键：解析、校验、按键状态机。
//!
//! 规则与旧版一致，只是从 Python 移到 Rust：
//! * 2 个或 3 个不同的键。
//! * 不能全是修饰键（Ctrl/Alt/Shift/Win），必须带一个字母、数字、功能键或特殊键。
//! * 拒绝系统自己占用的组合（Ctrl+Alt+Esc、Alt+Tab、Alt+F4）。
//! * 默认 C+K，按住 300ms 触发，时长可配置。

use std::time::{Duration, Instant};

use crate::{error::user, NdResult};

/// 一个可以参与组合键的按键。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ChordKey {
    Ctrl,
    Alt,
    Shift,
    Win,
    /// A-Z、0-9，统一存大写。
    Char(char),
    /// F1-F12。
    Function(u8),
    Space,
    Tab,
    Esc,
}

impl ChordKey {
    pub fn is_modifier(self) -> bool {
        matches!(
            self,
            ChordKey::Ctrl | ChordKey::Alt | ChordKey::Shift | ChordKey::Win
        )
    }

    /// 展示名，配置里保存的也是这个写法。
    pub fn name(self) -> String {
        match self {
            ChordKey::Ctrl => "Ctrl".into(),
            ChordKey::Alt => "Alt".into(),
            ChordKey::Shift => "Shift".into(),
            ChordKey::Win => "Win".into(),
            ChordKey::Char(c) => c.to_string(),
            ChordKey::Function(n) => format!("F{n}"),
            ChordKey::Space => "Space".into(),
            ChordKey::Tab => "Tab".into(),
            ChordKey::Esc => "Esc".into(),
        }
    }

    /// 解析用户写的键名。大小写、`Ctrl_L` 这类后缀、`Option`/`Cmd` 别名都接受。
    pub fn parse(name: &str) -> NdResult<Self> {
        let text = name.trim();
        if text.is_empty() {
            return Err(user(
                "这个键用不了。请用字母、数字，或者 Ctrl、Alt、Shift。",
            ));
        }
        let upper = text
            .to_uppercase()
            .replace("_L", "")
            .replace("_R", "")
            .replace("-L", "")
            .replace("-R", "");
        let key = match upper.as_str() {
            "CTRL" | "CONTROL" => ChordKey::Ctrl,
            "ALT" | "OPTION" => ChordKey::Alt,
            "SHIFT" => ChordKey::Shift,
            "WIN" | "CMD" | "SUPER" | "META" | "COMMAND" => ChordKey::Win,
            "SPACE" => ChordKey::Space,
            "TAB" => ChordKey::Tab,
            "ESC" | "ESCAPE" => ChordKey::Esc,
            other => {
                if other.len() == 1 {
                    let c = other.chars().next().unwrap_or(' ');
                    if c.is_ascii_alphanumeric() {
                        ChordKey::Char(c.to_ascii_uppercase())
                    } else {
                        return Err(user(
                            "这个键用不了。请用字母、数字，或者 Ctrl、Alt、Shift。",
                        ));
                    }
                } else if let Some(rest) = other.strip_prefix('F') {
                    let n: u8 = rest.parse().map_err(|_| {
                        user("这个键用不了。请用字母、数字，或者 Ctrl、Alt、Shift。")
                    })?;
                    if (1..=12).contains(&n) {
                        ChordKey::Function(n)
                    } else {
                        return Err(user("功能键只能用 F1 到 F12。"));
                    }
                } else {
                    return Err(user(
                        "这个键用不了。请用字母、数字，或者 Ctrl、Alt、Shift。",
                    ));
                }
            }
        };
        Ok(key)
    }
}

impl serde::Serialize for ChordKey {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.name())
    }
}

impl<'de> serde::Deserialize<'de> for ChordKey {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = <String as serde::Deserialize>::deserialize(d)?;
        ChordKey::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// 默认组合键：C + K。
pub fn default_chord() -> Vec<ChordKey> {
    vec![ChordKey::Char('C'), ChordKey::Char('K')]
}

/// 默认按住时长（毫秒）。
pub const DEFAULT_HOLD_MS: u64 = 300;
/// 按住时长的允许范围。
pub const HOLD_MS_RANGE: (u64, u64) = (100, 3000);

/// 校验并规范化一组键：去重、保持用户顺序。
pub fn validate(keys: &[ChordKey]) -> NdResult<Vec<ChordKey>> {
    let mut out: Vec<ChordKey> = Vec::new();
    for k in keys {
        if !out.contains(k) {
            out.push(*k);
        }
    }
    if !(2..=3).contains(&out.len()) {
        return Err(user("请同时按住 2 个或 3 个不同的键。只按一个不够。"));
    }
    if out.iter().all(|k| k.is_modifier()) {
        return Err(user(
            "不能只按 Ctrl、Alt、Shift 或 Win。要带上一个字母或数字，比如 C 和 K。",
        ));
    }
    let mut sorted = out.clone();
    sorted.sort();
    let reserved: [&[ChordKey]; 3] = [
        &[ChordKey::Ctrl, ChordKey::Alt, ChordKey::Esc],
        &[ChordKey::Alt, ChordKey::Tab],
        &[ChordKey::Alt, ChordKey::Function(4)],
    ];
    for combo in reserved {
        let mut c = combo.to_vec();
        c.sort();
        if c == sorted {
            return Err(user("这个组合系统自己在用。请换两个键，比如 C 和 K。"));
        }
    }
    Ok(out)
}

/// 校验按住时长，越界时返回中文提示。
pub fn validate_hold_ms(ms: u64) -> NdResult<u64> {
    let (lo, hi) = HOLD_MS_RANGE;
    if !(lo..=hi).contains(&ms) {
        return Err(user(format!("按住时长应在 {lo} 到 {hi} 毫秒之间")));
    }
    Ok(ms)
}

/// `C + K` 这样的展示文本。
pub fn format_chord(keys: &[ChordKey]) -> String {
    keys.iter()
        .map(|k| k.name())
        .collect::<Vec<_>>()
        .join(" + ")
}

/// 组合键状态机：喂进"当前哪些键按着"，到时间就触发一次。
///
/// 触发后必须松开所有键才能再次触发，避免长按反复解锁。
#[derive(Debug)]
pub struct ChordWatcher {
    keys: Vec<ChordKey>,
    hold: Duration,
    since: Option<Instant>,
    fired: bool,
}

impl ChordWatcher {
    pub fn new(keys: Vec<ChordKey>, hold_ms: u64) -> Self {
        Self {
            keys,
            hold: Duration::from_millis(hold_ms.clamp(HOLD_MS_RANGE.0, HOLD_MS_RANGE.1)),
            since: None,
            fired: false,
        }
    }

    pub fn keys(&self) -> &[ChordKey] {
        &self.keys
    }

    /// 每次采样调用一次。所有键都按着并持续够久时返回 true（每次按住只返回一次）。
    pub fn poll(&mut self, down: impl Fn(ChordKey) -> bool, now: Instant) -> bool {
        let all_down = self.keys.iter().all(|k| down(*k));
        if !all_down {
            self.since = None;
            self.fired = false;
            return false;
        }
        let started = *self.since.get_or_insert(now);
        if self.fired {
            return false;
        }
        if now.saturating_duration_since(started) >= self.hold {
            self.fired = true;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(list: &[&str]) -> Vec<ChordKey> {
        list.iter().map(|s| ChordKey::parse(s).unwrap()).collect()
    }

    #[test]
    fn parses_names_and_aliases() {
        assert_eq!(ChordKey::parse("ctrl_l").unwrap(), ChordKey::Ctrl);
        assert_eq!(ChordKey::parse("Option").unwrap(), ChordKey::Alt);
        assert_eq!(ChordKey::parse("cmd").unwrap(), ChordKey::Win);
        assert_eq!(ChordKey::parse("k").unwrap(), ChordKey::Char('K'));
        assert_eq!(ChordKey::parse("7").unwrap(), ChordKey::Char('7'));
        assert_eq!(ChordKey::parse("f12").unwrap(), ChordKey::Function(12));
        assert!(ChordKey::parse("f13").is_err());
        assert!(ChordKey::parse("hello").is_err());
        assert!(ChordKey::parse("Ctrl+Alt").is_err());
        assert!(ChordKey::parse("").is_err());
    }

    #[test]
    fn default_chord_is_valid() {
        let v = validate(&default_chord()).unwrap();
        assert_eq!(format_chord(&v), "C + K");
    }

    #[test]
    fn rejects_single_and_four_keys() {
        assert!(validate(&keys(&["C"])).is_err());
        assert!(validate(&keys(&["C", "K", "L", "M"])).is_err());
        // 重复的键去重后只剩一个，同样不合法
        assert!(validate(&keys(&["C", "C"])).is_err());
    }

    #[test]
    fn rejects_modifier_only_and_reserved() {
        assert!(validate(&keys(&["Ctrl", "Alt", "Shift"])).is_err());
        assert!(validate(&keys(&["Ctrl", "Alt", "Esc"])).is_err());
        assert!(validate(&keys(&["Alt", "Tab"])).is_err());
        assert!(validate(&keys(&["Alt", "F4"])).is_err());
        assert!(validate(&keys(&["Ctrl", "K"])).is_ok());
        assert!(validate(&keys(&["Ctrl", "Alt", "K"])).is_ok());
    }

    #[test]
    fn dedups_preserving_order() {
        let v = validate(&keys(&["K", "C", "K"])).unwrap();
        assert_eq!(format_chord(&v), "K + C");
    }

    #[test]
    fn hold_ms_range_enforced() {
        assert!(validate_hold_ms(50).is_err());
        assert!(validate_hold_ms(300).is_ok());
        assert!(validate_hold_ms(5000).is_err());
    }

    #[test]
    fn watcher_fires_once_after_hold() {
        let mut w = ChordWatcher::new(default_chord(), 300);
        let t0 = Instant::now();
        let down = |k: ChordKey| matches!(k, ChordKey::Char('C') | ChordKey::Char('K'));
        assert!(!w.poll(down, t0));
        assert!(!w.poll(down, t0 + Duration::from_millis(200)));
        assert!(w.poll(down, t0 + Duration::from_millis(300)));
        // 继续按着不重复触发
        assert!(!w.poll(down, t0 + Duration::from_millis(2000)));
        // 松开再按住才会再次触发
        assert!(!w.poll(|_| false, t0 + Duration::from_millis(2100)));
        let t1 = t0 + Duration::from_millis(2200);
        assert!(!w.poll(down, t1));
        assert!(w.poll(down, t1 + Duration::from_millis(300)));
    }

    #[test]
    fn watcher_requires_all_keys() {
        let mut w = ChordWatcher::new(default_chord(), 100);
        let t0 = Instant::now();
        let only_c = |k: ChordKey| k == ChordKey::Char('C');
        assert!(!w.poll(only_c, t0));
        assert!(!w.poll(only_c, t0 + Duration::from_secs(1)));
    }
}
