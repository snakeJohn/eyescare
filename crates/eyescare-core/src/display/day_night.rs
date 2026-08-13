//! DayNight 调度（MVP，设计 §7）。
//!
//! MVP：自定义时刻（day_start / night_start）或手动。过渡插值 60min 默认。
//! v0.2 才抽 ChronotypeScheduler。

use chrono::{Local, NaiveTime, Timelike};

use crate::config::DayNightConfig;

/// 给定本地时间，返回目标色温（K）。
/// 过渡段内做线性插值：day_start 前后 ±transition/2 分钟。
pub fn day_night_kelvin(cfg: &DayNightConfig, now: NaiveTime) -> u32 {
    if !cfg.enabled {
        return day_kelvin();
    }
    let day = parse_hhmm(&cfg.day_start).unwrap_or(NaiveTime::from_hms_opt(7, 0, 0).unwrap());
    let night = parse_hhmm(&cfg.night_start).unwrap_or(NaiveTime::from_hms_opt(19, 30, 0).unwrap());
    let transition = cfg.transition_minutes.max(1) as i64;

    let secs = |t: NaiveTime| t.num_seconds_from_midnight() as i64;
    let now_s = secs(now);
    let day_s = secs(day);
    let night_s = secs(night);

    // 线性时差（可为负；跨午夜用周期语义：now-目标 < -transition 视为更早的"另一侧"）。
    // 过渡窗口：目标时刻前 transition 秒内完成插值；目标时刻后即稳定。
    let transition_sec = (transition * 60) as f64;
    let d_day = now_s - day_s;
    let d_night = now_s - night_s;

    let day_k = day_kelvin() as f64;
    let night_k = night_kelvin() as f64;

    // 过渡窗口在目标时刻之前：progress 0 = 窗口起点（旧色温），1 = 目标时刻（新色温）。
    // 旧公式用「距目标的剩余比例」当 progress，方向反了，且在目标时刻会跳变。
    let day_target = if d_day >= 0 {
        day_k
    } else if (-d_day as f64) <= transition_sec {
        let progress = 1.0 - (-d_day as f64) / transition_sec;
        night_k + (day_k - night_k) * progress
    } else {
        night_k
    };

    let night_target = if d_night >= 0 {
        night_k
    } else if (-d_night as f64) <= transition_sec {
        let progress = 1.0 - (-d_night as f64) / transition_sec;
        day_k + (night_k - day_k) * progress
    } else {
        day_k
    };

    // 取两者中色温更低（更暖）者——夜晚需求优先。
    let k = day_target.min(night_target).clamp(1000.0, 10000.0);
    k.round() as u32
}

/// 白天色温（health 预设 4500K 为日间基线；DayNight 白天用 5500 办公色温更自然）。
fn day_kelvin() -> u32 {
    5500
}

fn night_kelvin() -> u32 {
    3400
}

fn parse_hhmm(s: &str) -> Option<NaiveTime> {
    let mut it = s.split(':');
    let h: u32 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    NaiveTime::from_hms_opt(h.min(23), m.min(59), 0)
}

/// 当前本地 NaiveTime（测试外使用）。
pub fn now_local() -> NaiveTime {
    Local::now().time()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(day: &str, night: &str, transition: u32) -> DayNightConfig {
        DayNightConfig {
            enabled: true,
            transition_minutes: transition,
            mode: crate::config::DayNightMode::CustomTimes,
            day_start: day.into(),
            night_start: night.into(),
        }
    }

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    #[test]
    fn night_is_warm_day_is_cool() {
        let c = cfg("07:00", "19:30", 60);
        // 正午：接近日间色温
        let noon = day_night_kelvin(&c, t(12, 0));
        assert!(noon >= 5000, "noon={noon}");
        // 深夜 23:30：接近夜间色温
        let late = day_night_kelvin(&c, t(23, 30));
        assert!(late <= 3600, "late={late}");
        // 凌晨 3:00 也在夜间
        let early = day_night_kelvin(&c, t(3, 0));
        assert!(early <= 3600, "early={early}");
        // 夜间比正午暖
        assert!(late < noon);
    }

    #[test]
    fn transition_is_gradual() {
        let c = cfg("07:00", "19:30", 60);
        // 19:00（night_start 前 30min = 过渡中点）应介于日夜之间
        let mid = day_night_kelvin(&c, t(19, 0));
        let noon = day_night_kelvin(&c, t(12, 0));
        let late = day_night_kelvin(&c, t(23, 30));
        assert!(mid < noon && mid > late, "mid={mid} noon={noon} late={late}");
    }

    #[test]
    fn disabled_returns_day() {
        let mut c = cfg("07:00", "19:30", 60);
        c.enabled = false;
        let k = day_night_kelvin(&c, t(23, 30));
        assert!(k >= 5000);
    }

    #[test]
    fn short_transition_quick_switch() {
        let c = cfg("07:00", "19:30", 10);
        // 19:31 应已基本进入夜间
        let k = day_night_kelvin(&c, t(19, 31));
        assert!(k <= 3700, "k={k}");
        // 19:25 还在过渡中（偏日间）
        let k2 = day_night_kelvin(&c, t(19, 25));
        assert!(k2 > 3700, "k2={k2}");
    }

    #[test]
    fn transition_direction_morning_warms_toward_day() {
        let c = cfg("07:00", "19:30", 60);
        let start = day_night_kelvin(&c, t(6, 5));
        let end = day_night_kelvin(&c, t(6, 55));
        assert!(
            start < end,
            "morning transition must rise toward day: {start} -> {end}"
        );
        assert!(start <= 3800, "start={start}");
        assert!(end >= 5000, "end={end}");
    }

    #[test]
    fn transition_direction_evening_cools_toward_night() {
        let c = cfg("07:00", "19:30", 60);
        let start = day_night_kelvin(&c, t(18, 35));
        let end = day_night_kelvin(&c, t(19, 25));
        assert!(
            end < start,
            "evening transition must fall toward night: {start} -> {end}"
        );
        assert!(start >= 5000, "start={start}");
        assert!(end <= 3800, "end={end}");
    }

    #[test]
    fn cross_midnight_day_start() {
        // 极端配置：day_start 01:00, night_start 13:00
        let c = cfg("01:00", "13:00", 60);
        // 11:00 尚未进入 13:00 前的过渡（-2h < -1h）→ 日间
        let noon = day_night_kelvin(&c, t(11, 0));
        let night = day_night_kelvin(&c, t(20, 0));
        assert!(noon > night);
        // 12:00 已进入过渡尾部 → 接近夜间
        let tail = day_night_kelvin(&c, t(12, 0));
        assert!(tail < noon);
    }
}
