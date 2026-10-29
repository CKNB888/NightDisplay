//! 配置读写：`%LOCALAPPDATA%\NightDisplay\config.json`。
//!
//! 兼容旧版 Python 的键名（`start`/`end`/`extra_ranges`/`unlock_chord`），
//! 升级后原来的计划、解锁键和自启偏好都保留；写回时同时镜像 `start`/`end`，
//! 万一用户退回旧版也能继续用。

use std::io::Write;
use std::path::PathBuf;

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

use crate::chord::{self, ChordKey};
use crate::error::{NdError, NdResult};
pub use crate::mode::Mode;
use crate::paths;
use crate::schedule::{NightRange, Schedule};

/// 当前配置格式版本。
pub const CONFIG_VERSION: u32 = 4;

/// 一个夜间区间，序列化成 `["20:00","10:30"]`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeDef(pub NightRange);

impl Serialize for RangeDef {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = s.serialize_seq(Some(2))?;
        seq.serialize_element(&self.0.start.to_hm())?;
        seq.serialize_element(&self.0.end.to_hm())?;
        seq.end()
    }
}

impl<'de> Deserialize<'de> for RangeDef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw: Vec<String> = Vec::deserialize(d)?;
        if raw.len() != 2 {
            return Err(serde::de::Error::custom("额外时间段应包含开始和结束时间"));
        }
        let start: crate::schedule::MinuteOfDay =
            raw[0].parse().map_err(serde::de::Error::custom)?;
        let end: crate::schedule::MinuteOfDay = raw[1].parse().map_err(serde::de::Error::custom)?;
        let range = NightRange::new(start, end).map_err(serde::de::Error::custom)?;
        Ok(RangeDef(range))
    }
}

/// 当前程序认为自己在白天还是夜间。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    #[default]
    Day,
    Night,
}

/// 界面主题。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

/// guard 模式的三层开关。黑幕是必需层，另外两层是尽力而为。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GuardSettings {
    /// 把 gamma 拉到全黑（不影响远程采集）。
    pub gamma: bool,
    /// DDC/CI 背光调到 0。
    pub brightness: bool,
    /// 背光重复校正间隔（秒）。
    pub brightness_reassert_secs: u64,
}

impl Default for GuardSettings {
    fn default() -> Self {
        Self {
            gamma: true,
            brightness: true,
            brightness_reassert_secs: 5,
        }
    }
}

/// 完整配置。所有新增字段都带默认值，旧配置能直接读进来。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    pub mode: Mode,
    /// 每日计划是否启用。
    pub enabled: bool,
    /// 主时间段 + 额外时间段，顺序即界面顺序。
    pub ranges: Vec<RangeDef>,
    pub unlock_chord: Vec<ChordKey>,
    pub unlock_hold_ms: u64,
    /// 黑屏期间锁定程序内的本地恢复操作（应急命令和组合键不受影响）。
    pub lock: bool,
    pub autostart: bool,
    pub autostart_preference: bool,
    pub keep_awake: bool,
    /// 严格本地防触控：吞掉真实硬件的键鼠事件，放行注入事件（远程）。
    pub strict_local_input: bool,
    /// 夜间抑制系统锁屏，避免登录界面盖住黑幕。
    pub suppress_lock: bool,
    pub guard: GuardSettings,
    /// 虚拟屏名称匹配（正则），仅 `virtual_only` 用。
    pub virtual_match: String,
    pub theme: Theme,

    // ---- 运行状态 ----
    pub state: RunState,
    pub night_mode: Option<Mode>,
    pub recovery_pending: bool,
    /// 组合键解锁后，本段剩余时间保持白天。
    pub override_until: Option<DateTime<Local>>,
    /// 白天手动进入夜间后保持到下一次计划结束。
    pub hold_night_until: Option<DateTime<Local>>,

    // ---- 测试与自检记录 ----
    pub last_test_mode: Option<Mode>,
    pub last_test_requested: Option<Mode>,
    pub last_test_at: Option<DateTime<Local>>,
    pub last_test_ok: bool,
    pub last_test_capability: String,
    pub last_recovery_at: Option<DateTime<Local>>,
    pub last_recovery_mode: Option<Mode>,
    pub test_id: String,
    pub last_night_latency_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        let primary = NightRange {
            start: "20:00".parse().expect("默认时间合法"),
            end: "10:30".parse().expect("默认时间合法"),
        };
        Self {
            version: CONFIG_VERSION,
            mode: Mode::Auto,
            enabled: false,
            ranges: vec![RangeDef(primary)],
            unlock_chord: chord::default_chord(),
            unlock_hold_ms: chord::DEFAULT_HOLD_MS,
            lock: true,
            autostart: true,
            autostart_preference: true,
            keep_awake: true,
            strict_local_input: true,
            suppress_lock: true,
            guard: GuardSettings::default(),
            virtual_match: String::new(),
            theme: Theme::Dark,
            state: RunState::Day,
            night_mode: None,
            recovery_pending: false,
            override_until: None,
            hold_night_until: None,
            last_test_mode: None,
            last_test_requested: None,
            last_test_at: None,
            last_test_ok: false,
            last_test_capability: String::new(),
            last_recovery_at: None,
            last_recovery_mode: None,
            test_id: String::new(),
            last_night_latency_ms: 0,
        }
    }
}

impl Config {
    /// 主时间段的起止（界面第一行）。
    #[must_use]
    pub fn primary_range(&self) -> NightRange {
        self.ranges.first().map_or_else(
            || NightRange {
                start: "20:00".parse().expect("常量合法"),
                end: "10:30".parse().expect("常量合法"),
            },
            |r| r.0,
        )
    }

    /// 转成用于判定的时间计划。
    #[must_use]
    pub fn schedule(&self) -> Schedule {
        Schedule::new(self.ranges.iter().map(|r| r.0).collect())
    }

    /// 所有区间的 `HH:MM` 字符串对，给界面和计划任务用。
    #[must_use]
    pub fn range_strings(&self) -> Vec<(String, String)> {
        self.ranges
            .iter()
            .map(|r| (r.0.start.to_hm(), r.0.end.to_hm()))
            .collect()
    }

    /// 组合键展示文本，例如 `C + K`。
    #[must_use]
    pub fn chord_text(&self) -> String {
        chord::format_chord(&self.unlock_chord)
    }

    /// 完整性校验。任何一项不对都返回可直接展示的中文错误。
    pub fn validate(&mut self) -> NdResult<()> {
        if self.ranges.is_empty() {
            return Err(crate::error::user("至少要保留一个时间段"));
        }
        for r in &self.ranges {
            if r.0.start == r.0.end {
                return Err(crate::error::user("时间段开始和结束不能相同"));
            }
        }
        self.unlock_chord = chord::validate(&self.unlock_chord)?;
        self.unlock_hold_ms = chord::validate_hold_ms(self.unlock_hold_ms)?;
        if self.virtual_match.len() > 120 {
            return Err(crate::error::user("虚拟屏匹配规则过长（最多 120 字符）"));
        }
        if !self.virtual_match.is_empty() {
            regex::Regex::new(&self.virtual_match).map_err(|e| {
                crate::error::user(format!("虚拟屏匹配规则不是有效正则表达式：{e}"))
            })?;
        }
        let gap = self.guard.brightness_reassert_secs.clamp(2, 60);
        self.guard.brightness_reassert_secs = gap;
        if self.version > CONFIG_VERSION {
            // 配置文件来自更新的版本时保留原值，只在日志里提示，不阻止运行。
            tracing::warn!(
                config_version = self.version,
                supported = CONFIG_VERSION,
                "配置文件来自更新的版本"
            );
            self.version = CONFIG_VERSION;
        } else {
            self.version = CONFIG_VERSION;
        }
        Ok(())
    }

    /// 读磁盘配置（带读锁）。
    pub fn load() -> NdResult<Config> {
        load()
    }

    /// 改配置并保存（带写锁）。改动会先校验，不合法则不落盘。
    pub fn update<R>(f: impl FnOnce(&mut Config) -> NdResult<R>) -> NdResult<R> {
        mutate(f)
    }

    /// 夜间是否处于"计划驱动"的锁定状态。
    #[must_use]
    pub fn is_locked(&self) -> bool {
        self.state == RunState::Night && self.lock
    }
}

/// 从磁盘读配置。文件不存在时返回默认配置。
pub fn load_from(path: &PathBuf) -> NdResult<Config> {
    let raw = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut cfg = Config::default();
            cfg.validate()?;
            return Ok(cfg);
        }
        Err(e) => {
            return Err(NdError::Io {
                path: path.display().to_string(),
                source: e,
            }
            .into())
        }
    };
    let mut cfg = parse(&raw)?;
    cfg.validate()?;
    tracing::debug!(path = %path.display(), "已读取配置");
    Ok(cfg)
}

/// 解析配置文本，兼容旧版键名。
pub fn parse(raw: &str) -> NdResult<Config> {
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| NdError::ConfigCorrupt {
            path: paths::config_file().display().to_string(),
            source: e,
        })?;
    let obj = value
        .as_object()
        .ok_or_else(|| crate::error::user("配置内容必须是 JSON 对象"))?;

    // 先看旧版字段，再叠加新字段，保证 ranges 缺失时能自动迁移。
    let mut cfg: Config =
        serde_json::from_value(value.clone()).map_err(|e| NdError::ConfigCorrupt {
            path: paths::config_file().display().to_string(),
            source: e,
        })?;

    if !obj.contains_key("ranges") {
        if let (Some(start), Some(end)) = (
            obj.get("start").and_then(|v| v.as_str()),
            obj.get("end").and_then(|v| v.as_str()),
        ) {
            let s = start
                .parse()
                .map_err(|_| crate::error::user("时间格式应为 HH:MM，例如 20:00"))?;
            let e = end
                .parse::<crate::schedule::MinuteOfDay>()
                .map_err(|_| crate::error::user("时间格式应为 HH:MM，例如 20:00"))?;
            let mut ranges = vec![RangeDef(NightRange::new(s, e)?)];
            if let Some(extra) = obj.get("extra_ranges").and_then(|v| v.as_array()) {
                for pair in extra {
                    let Ok(rd) = serde_json::from_value::<RangeDef>(pair.clone()) else {
                        continue;
                    };
                    if rd.0.start != rd.0.end {
                        ranges.push(rd);
                    }
                }
            }
            cfg.ranges = ranges;
        }
    }
    // 旧版的 `enabled` 与 `pause` 语义一致，无需迁移；`state` 直接沿用。
    Ok(cfg)
}

/// 原子写入配置：先写临时文件，再改名覆盖。Windows 上被占用时重试。
pub fn write_to(path: &PathBuf, cfg: &Config) -> NdResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| NdError::Io {
            path: parent.display().to_string(),
            source: e,
        })?;
    }
    let mut doc = serde_json::to_value(cfg)
        .map_err(|e| crate::error::user(format!("配置无法序列化：{e}")))?;
    if let Some(obj) = doc.as_object_mut() {
        // 镜像旧版字段，方便退回旧版本继续使用。
        let primary = cfg.primary_range();
        obj.insert("start".into(), serde_json::json!(primary.start.to_hm()));
        obj.insert("end".into(), serde_json::json!(primary.end.to_hm()));
        obj.insert(
            "extra_ranges".into(),
            serde_json::json!(cfg
                .ranges
                .iter()
                .skip(1)
                .map(|r| vec![r.0.start.to_hm(), r.0.end.to_hm()])
                .collect::<Vec<_>>()),
        );
        obj.insert(
            "unlock_chord".into(),
            serde_json::json!(cfg
                .unlock_chord
                .iter()
                .map(|k| k.name())
                .collect::<Vec<_>>()),
        );
    }
    let text = serde_json::to_string_pretty(&doc)
        .map_err(|e| crate::error::user(format!("配置无法序列化：{e}")))?;

    let tmp = path.with_extension("json.tmp");
    {
        let mut f = std::fs::File::create(&tmp).map_err(|e| NdError::Io {
            path: tmp.display().to_string(),
            source: e,
        })?;
        f.write_all(text.as_bytes()).map_err(|e| NdError::Io {
            path: tmp.display().to_string(),
            source: e,
        })?;
        f.write_all(b"\n").ok();
        f.sync_all().ok();
    }
    let mut last_err = None;
    for attempt in 0..30 {
        match std::fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(20 * (attempt + 1) as u64));
            }
        }
    }
    // 改名一直失败时退化成直接覆盖，至少别让状态停在夜间。
    if let Err(e) = std::fs::copy(&tmp, path) {
        return Err(NdError::Io {
            path: path.display().to_string(),
            source: last_err.unwrap_or(e),
        }
        .into());
    }
    let _ = std::fs::remove_file(&tmp);
    Ok(())
}

/// 读取配置（带跨进程读锁）。
pub fn load() -> NdResult<Config> {
    paths::ensure_home().ok();
    let _lock = crate::state_lock::read_lock(paths::home_dir().join("config.lock"))?;
    load_from(&paths::config_file())
}

/// 修改并保存配置。`f` 里抛错则不写盘。
pub fn mutate<R>(f: impl FnOnce(&mut Config) -> NdResult<R>) -> NdResult<R> {
    paths::ensure_home().map_err(|e| crate::error::user(format!("无法创建数据目录：{e}")))?;
    let path = paths::config_file();
    let _lock = crate::state_lock::write_lock(paths::home_dir().join("config.lock"))?;
    let mut cfg = load_from(&path)?;
    let out = f(&mut cfg)?;
    cfg.validate()?;
    write_to(&path, &cfg)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_product_spec() {
        let cfg = Config::default();
        assert_eq!(cfg.primary_range().start.to_hm(), "20:00");
        assert_eq!(cfg.primary_range().end.to_hm(), "10:30");
        assert_eq!(cfg.chord_text(), "C + K");
        assert_eq!(cfg.unlock_hold_ms, 300);
        assert!(cfg.strict_local_input);
        assert!(cfg.suppress_lock);
        assert_eq!(cfg.mode, Mode::Auto);
        assert!(!cfg.enabled);
    }

    #[test]
    fn round_trips_through_json() {
        let cfg = Config::default();
        let text = serde_json::to_string(&cfg).unwrap();
        let back: Config = serde_json::from_str(&text).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn migrates_legacy_start_end_and_extras() {
        let legacy = r#"{
            "version": 3,
            "mode": "auto",
            "enabled": true,
            "start": "21:15",
            "end": "07:45",
            "extra_ranges": [["12:00", "13:30"]],
            "unlock_chord": ["ctrl", "k"],
            "state": "day",
            "theme": "light"
        }"#;
        let cfg = parse(legacy).unwrap();
        assert_eq!(cfg.ranges.len(), 2);
        assert_eq!(cfg.ranges[0].0.start.to_hm(), "21:15");
        assert_eq!(cfg.ranges[0].0.end.to_hm(), "07:45");
        assert_eq!(cfg.ranges[1].0.start.to_hm(), "12:00");
        // 旧配置里的 ["ctrl","k"] 原样保留，不被默认值覆盖
        assert_eq!(cfg.chord_text(), "Ctrl + K");
        assert!(cfg.enabled);
    }

    #[test]
    fn unknown_mode_is_rejected_with_chinese_text() {
        let err = Mode::parse("nonsense").unwrap_err().to_string();
        assert!(err.contains("不支持的模式"), "{err}");
    }

    #[test]
    fn legacy_mode_names_still_parse() {
        // 旧配置里叫 guard / virtual_only，升级后必须读得进来。
        assert_eq!(Mode::parse("overlay").unwrap(), Mode::Overlay);
        assert_eq!(Mode::parse("guard").unwrap(), Mode::Overlay);
        assert_eq!(Mode::parse("virtual_only").unwrap(), Mode::VirtualScreen);
        assert_eq!(Mode::parse("virtual_screen").unwrap(), Mode::VirtualScreen);
        assert_eq!(Mode::parse("auto").unwrap(), Mode::Auto);
    }

    #[test]
    fn validate_rejects_bad_chord_and_hold() {
        let mut cfg = Config::default();
        cfg.unlock_chord = vec![ChordKey::Ctrl];
        assert!(cfg.validate().is_err());

        let mut cfg = Config::default();
        cfg.unlock_hold_ms = 10;
        assert!(cfg.validate().is_err());

        let mut cfg = Config::default();
        cfg.virtual_match = "(".into();
        assert!(cfg.validate().is_err());

        let mut cfg = Config::default();
        cfg.ranges.clear();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn atomic_write_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut cfg = Config::default();
        cfg.enabled = true;
        cfg.ranges.push(RangeDef(NightRange {
            start: "12:00".parse().unwrap(),
            end: "13:30".parse().unwrap(),
        }));
        write_to(&path, &cfg).unwrap();
        let back = load_from(&path).unwrap();
        assert!(back.enabled);
        assert_eq!(back.ranges.len(), 2);
        // 旧版镜像字段同时写出
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(raw["start"], "20:00");
        assert_eq!(raw["end"], "10:30");
        assert_eq!(raw["extra_ranges"][0][0], "12:00");
        assert_eq!(raw["unlock_chord"][0], "C");
    }

    #[test]
    fn corrupt_file_reports_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, "{not json").unwrap();
        let err = load_from(&path).unwrap_err().to_string();
        assert!(err.contains("配置文件损坏"), "{err}");
    }

    #[test]
    fn mode_flags_mark_remote_interruption() {
        // 虚拟屏设计上不打断远程；只有真的关屏会。
        assert!(!Mode::Overlay.interrupts_remote());
        assert!(!Mode::Auto.interrupts_remote());
        assert!(!Mode::VirtualScreen.interrupts_remote());
        assert!(Mode::ScreenOff.interrupts_remote());
    }

    #[test]
    fn missing_file_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = load_from(&dir.path().join("nope.json")).unwrap();
        assert_eq!(cfg.primary_range().start.to_hm(), "20:00");
    }
}
