//! 时间计划：主时间段 + 任意多段额外时间段，每段都可以跨午夜。
//!
//! 与旧版 Python 实现保持同样的语义：
//! * `start == end` 的段无效（旧版按“永不进入夜间”处理）。
//! * 判断只看"时:分"，不涉及时区换算，用的是本机本地时间。
//! * 下一次切换点按分钟粒度向前找，最多找 24 小时。

use chrono::{DateTime, Duration, Local, NaiveTime, Timelike};

use crate::{error::user, NdResult};

/// 一天里的一个夜间区间，`start`/`end` 都为本地时间（时分）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NightRange {
    pub start: MinuteOfDay,
    pub end: MinuteOfDay,
}

/// 从零点起算的分钟数，0..=1439。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MinuteOfDay(u16);

impl MinuteOfDay {
    pub const MAX: u16 = 23 * 60 + 59;

    pub fn new(hour: u32, minute: u32) -> NdResult<Self> {
        if hour > 23 || minute > 59 {
            return Err(user("时间格式应为 HH:MM，例如 20:00"));
        }
        Ok(Self((hour * 60 + minute) as u16))
    }

    /// 从分钟数构造，越界返回中文错误而不是截断。
    pub fn from_minutes(minutes: i64) -> NdResult<Self> {
        if !(0..=Self::MAX as i64).contains(&minutes) {
            return Err(user("时间格式应为 HH:MM，例如 20:00"));
        }
        Ok(Self(minutes as u16))
    }

    pub fn minutes(self) -> u16 {
        self.0
    }

    pub fn hour(self) -> u32 {
        u32::from(self.0 / 60)
    }

    pub fn minute(self) -> u32 {
        u32::from(self.0 % 60)
    }

    /// `HH:MM` 形式，配置和日志都用这个写法。
    pub fn to_hm(self) -> String {
        format!("{:02}:{:02}", self.hour(), self.minute())
    }

    #[allow(dead_code)]
    fn offset_from(self, start: MinuteOfDay) -> i64 {
        let d = i64::from(self.0) - i64::from(start.0);
        if d < 0 {
            d + 24 * 60
        } else {
            d
        }
    }
}

impl std::fmt::Display for MinuteOfDay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hm())
    }
}

impl std::str::FromStr for MinuteOfDay {
    type Err = anyhow::Error;

    /// 接受 `9:05`、`09:05`、`9：05`（全角冒号），拒绝其它写法。
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let text = s.trim().replace('：', ":");
        let (h, m) = text
            .split_once(':')
            .ok_or_else(|| user("时间格式应为 HH:MM，例如 20:00"))?;
        if h.is_empty() || m.is_empty() || h.len() > 2 || m.len() != 2 {
            return Err(user("时间格式应为 HH:MM，例如 20:00"));
        }
        if !h.bytes().all(|b| b.is_ascii_digit()) || !m.bytes().all(|b| b.is_ascii_digit()) {
            return Err(user("时间格式应为 HH:MM，例如 20:00"));
        }
        let hour: u32 = h
            .parse()
            .map_err(|_| user("时间格式应为 HH:MM，例如 20:00"))?;
        let minute: u32 = m
            .parse()
            .map_err(|_| user("时间格式应为 HH:MM，例如 20:00"))?;
        Self::new(hour, minute)
    }
}

impl NightRange {
    pub fn new(start: MinuteOfDay, end: MinuteOfDay) -> NdResult<Self> {
        if start == end {
            return Err(user("时间段开始和结束不能相同"));
        }
        Ok(Self { start, end })
    }

    /// 这段是否跨午夜（例如 20:00→10:30）。
    pub fn crosses_midnight(&self) -> bool {
        self.start > self.end
    }

    /// 给定时刻是否落在这一段里。`now` 只看时分。
    pub fn contains(&self, now: MinuteOfDay) -> bool {
        if self.start == self.end {
            return false;
        }
        if self.start < self.end {
            now >= self.start && now < self.end
        } else {
            now >= self.start || now < self.end
        }
    }
}

/// 解析界面上的多段文本，例如 `12:00-13:30, 02:00-04:00`。
/// 分隔符接受中英文逗号、分号、空白，减号接受 `-` `~` `–`。
pub fn parse_ranges_text(text: &str) -> NdResult<Vec<NightRange>> {
    let mut out = Vec::new();
    for raw in text.split(['，', ',', '；', ';', ' ', '\t', '\n', '\r']) {
        let part = raw.trim();
        if part.is_empty() {
            continue;
        }
        let (a, b) = part
            .split_once(['-', '~', '–'])
            .ok_or_else(|| user(format!("时间段格式应为 HH:MM-HH:MM：{part}")))?;
        let start: MinuteOfDay = a
            .parse()
            .map_err(|_| user(format!("时间段格式应为 HH:MM-HH:MM：{part}")))?;
        let end: MinuteOfDay = b
            .parse()
            .map_err(|_| user(format!("时间段格式应为 HH:MM-HH:MM：{part}")))?;
        if start == end {
            return Err(user(format!("时间段开始和结束不能相同：{part}")));
        }
        out.push(NightRange { start, end });
    }
    Ok(out)
}

/// 一组夜间区间。判定“现在是不是夜间”就是其中任一段命中。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Schedule {
    ranges: Vec<NightRange>,
}

impl Schedule {
    pub fn new(ranges: Vec<NightRange>) -> Self {
        Self { ranges }
    }

    pub fn ranges(&self) -> &[NightRange] {
        &self.ranges
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    pub fn contains_minute(&self, now: MinuteOfDay) -> bool {
        self.ranges.iter().any(|r| r.contains(now))
    }

    /// 某个具体时刻是否在夜间。
    pub fn is_night_at(&self, now: DateTime<Local>) -> bool {
        self.contains_minute(minute_of_day(now))
    }

    /// 所有边界时间点（去重后升序）。用来注册系统计划任务。
    pub fn boundaries(&self) -> Vec<MinuteOfDay> {
        let mut v: Vec<MinuteOfDay> = self.ranges.iter().flat_map(|r| [r.start, r.end]).collect();
        v.sort();
        v.dedup();
        v
    }

    /// 下一次状态翻转：返回 (进入夜间= true / 恢复白天= false, 再过多少秒)。
    ///
    /// 以分钟为粒度向前找，最多 24 小时。全天被覆盖或完全没有有效段时返回 `None`，
    /// 表示不存在切换点（旧版在这里返回 (None, 86400)）。
    pub fn next_switch(&self, now: DateTime<Local>) -> Option<(bool, i64)> {
        let current = self.is_night_at(now);
        let base = now.with_second(0)?.with_nanosecond(0)?;
        for step in 1..=1440i64 {
            let t = base + Duration::minutes(step);
            if self.is_night_at(t) != current {
                let secs = (t - now).num_seconds();
                return Some((!current, secs.max(1)));
            }
        }
        None
    }

    /// 下一次进入夜间还要多久（秒）。已经在夜间时返回 0。
    pub fn seconds_until_night(&self, now: DateTime<Local>) -> i64 {
        if self.is_night_at(now) {
            return 0;
        }
        match self.next_switch(now) {
            Some((true, secs)) => secs,
            _ => 0,
        }
    }

    /// 当前这一段夜间还剩多少秒。不在夜间时返回 0。
    pub fn seconds_left_in_night(&self, now: DateTime<Local>) -> i64 {
        if !self.is_night_at(now) {
            return 0;
        }
        match self.next_switch(now) {
            Some((false, secs)) => secs,
            _ => 0,
        }
    }
}

/// 取某个时刻的“时:分”。
pub fn minute_of_day(t: DateTime<Local>) -> MinuteOfDay {
    MinuteOfDay((t.hour() as u16) * 60 + t.minute() as u16)
}

/// 把 `HH:MM` 转成今天（或明天）的 `NaiveTime`，供状态文本展示用。
pub fn naive_time(m: MinuteOfDay) -> NaiveTime {
    NaiveTime::from_hms_opt(m.hour(), m.minute(), 0).unwrap_or(NaiveTime::MIN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(h: u32, m: u32, s: u32) -> DateTime<Local> {
        let date = Local
            .with_ymd_and_hms(2026, 10, 29, 0, 0, 0)
            .unwrap()
            .date_naive();
        Local
            .from_local_datetime(&date.and_hms_opt(h, m, s).unwrap())
            .single()
            .expect("本地时间应当唯一")
    }

    fn default_schedule() -> Schedule {
        Schedule::new(vec![NightRange {
            start: "20:00".parse().unwrap(),
            end: "10:30".parse().unwrap(),
        }])
    }

    #[test]
    fn parses_flexible_hour_forms() {
        assert_eq!("9:05".parse::<MinuteOfDay>().unwrap().to_hm(), "09:05");
        assert_eq!("09：05".parse::<MinuteOfDay>().unwrap().to_hm(), "09:05");
        assert_eq!("23:59".parse::<MinuteOfDay>().unwrap().minutes(), 1439);
        assert!("24:00".parse::<MinuteOfDay>().is_err());
        assert!("9:5".parse::<MinuteOfDay>().is_err());
        assert!("--".parse::<MinuteOfDay>().is_err());
        assert!("".parse::<MinuteOfDay>().is_err());
    }

    #[test]
    fn rejects_equal_start_and_end() {
        let a: MinuteOfDay = "20:00".parse().unwrap();
        assert!(NightRange::new(a, a).is_err());
        assert!(parse_ranges_text("20:00-20:00").is_err());
    }

    #[test]
    fn cross_midnight_range_wraps() {
        let s = default_schedule();
        assert!(s.is_night_at(at(20, 0, 0)), "20:00 应算夜间（含左端）");
        assert!(s.is_night_at(at(23, 59, 0)));
        assert!(s.is_night_at(at(0, 30, 0)));
        assert!(s.is_night_at(at(10, 29, 0)));
        assert!(!s.is_night_at(at(10, 30, 0)), "10:30 应算白天（不含右端）");
        assert!(!s.is_night_at(at(19, 59, 0)));
        assert!(!s.is_night_at(at(12, 0, 0)));
    }

    #[test]
    fn same_day_range_works() {
        let s = Schedule::new(vec![NightRange {
            start: "12:00".parse().unwrap(),
            end: "13:30".parse().unwrap(),
        }]);
        assert!(s.is_night_at(at(12, 0, 0)));
        assert!(s.is_night_at(at(13, 29, 0)));
        assert!(!s.is_night_at(at(13, 30, 0)));
        assert!(!s.is_night_at(at(11, 59, 0)));
    }

    #[test]
    fn overlapping_ranges_are_merged_for_switching() {
        let s = Schedule::new(vec![
            NightRange {
                start: "20:00".parse().unwrap(),
                end: "10:30".parse().unwrap(),
            },
            NightRange {
                start: "23:00".parse().unwrap(),
                end: "23:30".parse().unwrap(),
            },
        ]);
        // 23:00-23:30 完全包在 20:00-10:30 里，不应产生假的切换点。
        let (go_night, secs) = s.next_switch(at(18, 0, 0)).unwrap();
        assert!(go_night);
        assert_eq!(secs, 2 * 3600);
        let (go_night, secs) = s.next_switch(at(9, 0, 0)).unwrap();
        assert!(!go_night);
        assert_eq!(secs, 90 * 60);
    }

    #[test]
    fn next_switch_between_adjacent_ranges_skips_gap() {
        let s = Schedule::new(vec![
            NightRange {
                start: "01:00".parse().unwrap(),
                end: "02:00".parse().unwrap(),
            },
            NightRange {
                start: "03:00".parse().unwrap(),
                end: "04:00".parse().unwrap(),
            },
        ]);
        let (go_night, secs) = s.next_switch(at(1, 30, 0)).unwrap();
        assert!(!go_night);
        assert_eq!(secs, 30 * 60);
        assert_eq!(s.seconds_until_night(at(2, 30, 0)), 30 * 60);
        assert_eq!(s.seconds_left_in_night(at(3, 15, 0)), 45 * 60);
    }

    #[test]
    fn boundaries_are_sorted_and_unique() {
        let s = Schedule::new(vec![
            NightRange {
                start: "20:00".parse().unwrap(),
                end: "10:30".parse().unwrap(),
            },
            NightRange {
                start: "10:30".parse().unwrap(),
                end: "12:00".parse().unwrap(),
            },
        ]);
        let b: Vec<String> = s.boundaries().iter().map(|m| m.to_hm()).collect();
        assert_eq!(b, vec!["10:30", "12:00", "20:00"]);
    }

    #[test]
    fn full_day_coverage_has_no_switch_point() {
        let s = Schedule::new(vec![
            NightRange {
                start: "00:00".parse().unwrap(),
                end: "12:00".parse().unwrap(),
            },
            NightRange {
                start: "12:00".parse().unwrap(),
                end: "23:59".parse().unwrap(),
            },
            NightRange {
                start: "23:59".parse().unwrap(),
                end: "00:00".parse().unwrap(),
            },
        ]);
        assert!(s.next_switch(at(8, 0, 0)).is_none());
    }

    #[test]
    fn empty_schedule_is_never_night() {
        let s = Schedule::default();
        assert!(!s.is_night_at(at(23, 0, 0)));
        assert!(s.next_switch(at(23, 0, 0)).is_none());
    }

    #[test]
    fn parses_range_text_separators() {
        let r = parse_ranges_text("12:00-13:30，02:00~04:00; 05:00–06:00").unwrap();
        assert_eq!(r.len(), 3);
        assert_eq!(r[1].start.to_hm(), "02:00");
        assert_eq!(r[2].end.to_hm(), "06:00");
        assert_eq!(parse_ranges_text("   ").unwrap().len(), 0);
        assert!(parse_ranges_text("12:00").is_err());
    }

    #[test]
    fn crosses_midnight_flag() {
        assert!(NightRange {
            start: "20:00".parse().unwrap(),
            end: "10:30".parse().unwrap()
        }
        .crosses_midnight());
        assert!(!NightRange {
            start: "01:00".parse().unwrap(),
            end: "02:00".parse().unwrap()
        }
        .crosses_midnight());
    }
}
