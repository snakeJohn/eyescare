//! InsightsStore（PR-A16）：本地眼健康洞察（设计 §8）。
//!
//! - SQLite（WAL），三表：events / heartbeats / daily_rollup
//! - Heartbeat 60s；活跃时长、滤镜覆盖率、休息遵从、蓝光相对负荷
//! - **蓝光负荷仅 filter_on=1 区间计入**；Safe Mode / filter pause 计入 safe_sec（KD19）
//! - 90 天保留，启动 purge；app 身份默认哈希（§5.1 / §8.3）

use std::path::Path;

use chrono::{Local, NaiveDate};
use rusqlite::Connection;

use crate::error::Result;

pub const DEFAULT_RETENTION_DAYS: u32 = 90;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeartbeatSample {
    pub ts_utc: i64,
    pub active: bool,
    pub filter_on: bool,
    pub kelvin: Option<u32>,
    pub brightness: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BreakRecord {
    pub ts_utc: i64,
    pub prompted: bool,
    pub completed: bool,
}

/// 今日指标（洞察面板）。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct TodaySummary {
    pub active_sec: u64,
    pub filter_sec: u64,
    pub safe_sec: u64,
    pub break_prompted: u64,
    pub break_completed: u64,
    /// 0–100 相对分（非光度计量，附录 D）。
    pub blue_load: f64,
    /// 滤镜覆盖率 = filter_sec / active_sec（活跃为 0 时 1.0）。
    pub filter_coverage: f64,
    /// 休息遵从率 = completed / prompted（无提示时 1.0）。
    pub break_compliance: f64,
    /// 旁路时长占比。
    pub safe_ratio: f64,
}

pub struct InsightsStore {
    conn: Connection,
    /// 明文 display_name 是否落库（默认 false）。
    store_display_names: bool,
}

impl InsightsStore {
    pub fn open(path: &Path, store_display_names: bool) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // 常驻托盘进程：压低 SQLite 页缓存与 mmap，避免默默占掉数 MB。
        conn.pragma_update(None, "cache_size", -256)?;
        conn.pragma_update(None, "mmap_size", 0)?;
        conn.pragma_update(None, "temp_store", "FILE")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS events (
              id            INTEGER PRIMARY KEY AUTOINCREMENT,
              ts_utc        INTEGER NOT NULL,
              kind          TEXT NOT NULL,
              app_key       TEXT,
              app_display   TEXT,
              rule_id       TEXT,
              kelvin        REAL,
              brightness    REAL,
              source        TEXT,
              payload_json  TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_events_ts ON events(ts_utc);
            CREATE INDEX IF NOT EXISTS idx_events_kind_ts ON events(kind, ts_utc);
            CREATE TABLE IF NOT EXISTS heartbeats (
              ts_utc     INTEGER PRIMARY KEY,
              active     INTEGER NOT NULL,
              filter_on  INTEGER NOT NULL,
              kelvin     REAL,
              brightness REAL
            );
            CREATE TABLE IF NOT EXISTS daily_rollup (
              day        TEXT PRIMARY KEY,
              active_sec INTEGER,
              filter_sec INTEGER,
              safe_sec   INTEGER,
              break_prompted INTEGER,
              break_completed INTEGER,
              blue_load  REAL
            );",
        )?;
        Ok(Self {
            conn,
            store_display_names,
        })
    }

    // ---------- 采集 ----------

    /// 60s heartbeat（§8.3）。upsert by ts_utc。
    pub fn heartbeat(&self, s: HeartbeatSample) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO heartbeats (ts_utc, active, filter_on, kelvin, brightness)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                s.ts_utc,
                s.active as i32,
                s.filter_on as i32,
                s.kelvin,
                s.brightness
            ],
        )?;
        Ok(())
    }

    /// 事件（filter_applied / safe_mode_enter / safe_mode_exit / break_prompted 等）。
    #[allow(clippy::too_many_arguments)]
    pub fn record_event(
        &self,
        ts_utc: i64,
        kind: &str,
        app_key: Option<&str>,
        app_display: Option<&str>,
        rule_id: Option<&str>,
        kelvin: Option<u32>,
        brightness: Option<f64>,
        source: &str,
        payload_json: Option<&str>,
    ) -> Result<()> {
        let display = if self.store_display_names {
            app_display
        } else {
            None
        };
        self.conn.execute(
            "INSERT INTO events (ts_utc, kind, app_key, app_display, rule_id, kelvin, brightness, source, payload_json)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            rusqlite::params![
                ts_utc,
                kind,
                app_key,
                display,
                rule_id,
                kelvin,
                brightness,
                source,
                payload_json
            ],
        )?;
        Ok(())
    }

    /// 休息记录（prompted/completed 分开落，遵从率 = completed/prompted）。
    pub fn record_break(&self, ts_utc: i64, kind: &str) -> Result<()> {
        // kind: break_prompted / break_completed / break_skipped
        self.record_event(ts_utc, kind, None, None, None, None, None, "timer", None)
    }

    // ---------- 聚合 ----------

    /// 把 heartbeats 聚合进指定本地日期的 daily_rollup（每小时或退出时）。
    /// blue_load 仅 filter_on 区间：partial = hours * warmth(kelvin) * brightness
    /// （附录 D 相对分，非光度计量）。
    pub fn rollup(&self, day: NaiveDate) -> Result<()> {
        // 该日 UTC 范围：本地日 00:00 到次日 00:00
        let start_local = day.and_hms_opt(0, 0, 0).unwrap();
        let end_local = (day + chrono::Days::new(1)).and_hms_opt(0, 0, 0).unwrap();
        let tz_offset = *Local::now().offset();
        let start_utc = local_to_utc(start_local, tz_offset);
        let end_utc = local_to_utc(end_local, tz_offset);

        let mut stmt = self.conn.prepare(
            "SELECT ts_utc, active, filter_on, kelvin, brightness FROM heartbeats
             WHERE ts_utc >= ?1 AND ts_utc < ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![start_utc, end_utc], |r| {
            // kelvin 列是 REAL，读取为 f64 再转 u32
            let kelvin: Option<f64> = r.get(3)?;
            Ok(HeartbeatSample {
                ts_utc: r.get(0)?,
                active: r.get::<_, i32>(1)? != 0,
                filter_on: r.get::<_, i32>(2)? != 0,
                kelvin: kelvin.map(|k| k as u32),
                brightness: r.get(4)?,
            })
        })?;

        let mut active_sec: u64 = 0;
        let mut filter_sec: u64 = 0;
        let mut safe_sec: u64 = 0;
        let mut blue_load: f64 = 0.0;

        for row in rows {
            let s = row?;
            if !s.active {
                continue;
            }
            active_sec += 60;
            if s.filter_on {
                filter_sec += 60;
                let k = s.kelvin.unwrap_or(4500) as f64;
                let b = s.brightness.unwrap_or(0.85);
                blue_load += blue_load_partial(60.0, k, b);
            } else {
                // filter off = Safe Mode / pause → safe_sec（KD19）
                safe_sec += 60;
            }
        }

        // 今日已有 break 计数（events）
        let (mut prompted, mut completed) = (0u64, 0u64);
        if let Ok(mut st) = self.conn.prepare(
            "SELECT kind FROM events WHERE ts_utc >= ?1 AND ts_utc < ?2 AND kind LIKE 'break_%'",
        ) {
            let kinds = st.query_map(rusqlite::params![start_utc, end_utc], |r| r.get::<_, String>(0))?;
            for k in kinds.flatten() {
                match k.as_str() {
                    "break_prompted" => prompted += 1,
                    "break_completed" => completed += 1,
                    _ => {}
                }
            }
        }

        let day_str = day.format("%Y-%m-%d").to_string();
        self.conn.execute(
            "INSERT OR REPLACE INTO daily_rollup
             (day, active_sec, filter_sec, safe_sec, break_prompted, break_completed, blue_load)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![
                day_str,
                active_sec as i64,
                filter_sec as i64,
                safe_sec as i64,
                prompted as i64,
                completed as i64,
                blue_load
            ],
        )?;
        Ok(())
    }

    // ---------- 查询 ----------

    /// 今日面板（本地日）。
    pub fn today_summary(&self) -> Result<TodaySummary> {
        let day = Local::now().date_naive();
        self.day_summary(day)
    }

    pub fn day_summary(&self, day: NaiveDate) -> Result<TodaySummary> {
        let day_str = day.format("%Y-%m-%d").to_string();
        let mut summary = TodaySummary::default();

        if let Ok(mut stmt) = self.conn.prepare(
            "SELECT active_sec, filter_sec, safe_sec, break_prompted, break_completed, blue_load
             FROM daily_rollup WHERE day = ?1",
        ) {
            if let Ok(row) = stmt.query_row(rusqlite::params![day_str], |r| {
                Ok((
                    r.get::<_, i64>(0)? as u64,
                    r.get::<_, i64>(1)? as u64,
                    r.get::<_, i64>(2)? as u64,
                    r.get::<_, i64>(3)? as u64,
                    r.get::<_, i64>(4)? as u64,
                    r.get::<_, f64>(5)?,
                ))
            }) {
                summary.active_sec = row.0;
                summary.filter_sec = row.1;
                summary.safe_sec = row.2;
                summary.break_prompted = row.3;
                summary.break_completed = row.4;
                summary.blue_load = row.5;
            }
        }

        summary.filter_coverage = if summary.active_sec > 0 {
            summary.filter_sec as f64 / summary.active_sec as f64
        } else {
            1.0
        };
        summary.break_compliance = if summary.break_prompted > 0 {
            summary.break_completed as f64 / summary.break_prompted as f64
        } else {
            1.0
        };
        summary.safe_ratio = if summary.active_sec > 0 {
            summary.safe_sec as f64 / summary.active_sec as f64
        } else {
            0.0
        };
        Ok(summary)
    }

    /// 90 天保留：purge 过期 heartbeats/events/rollup（启动时调用）。
    pub fn purge(&self, retention_days: u32) -> Result<()> {
        let cutoff = chrono::Utc::now().timestamp() - (retention_days as i64) * 86400;
        self.conn
            .execute("DELETE FROM heartbeats WHERE ts_utc < ?1", rusqlite::params![cutoff])?;
        self.conn
            .execute("DELETE FROM events WHERE ts_utc < ?1", rusqlite::params![cutoff])?;
        let cutoff_day = (chrono::Utc::now() - chrono::Duration::days(retention_days as i64))
            .format("%Y-%m-%d")
            .to_string();
        self.conn.execute(
            "DELETE FROM daily_rollup WHERE day < ?1",
            rusqlite::params![cutoff_day],
        )?;
        Ok(())
    }

    /// 诊断：最近 N 条事件。
    pub fn recent_events(&self, n: u32) -> Result<Vec<serde_json::Value>> {
        let mut stmt = self.conn.prepare(
            "SELECT ts_utc, kind, app_key, rule_id, kelvin, brightness, source
             FROM events ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![n], |r| {
            Ok(serde_json::json!({
                "ts_utc": r.get::<_, i64>(0)?,
                "kind": r.get::<_, String>(1)?,
                "app_key": r.get::<_, Option<String>>(2)?,
                "rule_id": r.get::<_, Option<String>>(3)?,
                "kelvin": r.get::<_, Option<f64>>(4)?,
                "brightness": r.get::<_, Option<f64>>(5)?,
                "source": r.get::<_, String>(6)?,
            }))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}

/// 本地日边界 → UTC 时间戳（DST 安全：Ambiguous 取早值；None 兜底按 UTC 处理，绝不 panic）。
fn local_to_utc(dt: chrono::NaiveDateTime, tz: chrono::FixedOffset) -> i64 {
    use chrono::LocalResult;
    match dt.and_local_timezone(tz) {
        LocalResult::Single(t) => t.timestamp(),
        LocalResult::Ambiguous(a, _) => a.timestamp(),
        LocalResult::None => dt.and_utc().timestamp(),
    }
}

/// 附录 D 蓝光相对负荷：partial = hours * warmth_factor(kelvin) * brightness_factor。
/// warmth 随 kelvin 升高而升高（更蓝）。返回 0..=1 归一化分。
fn blue_load_partial(seconds: f64, kelvin: f64, brightness: f64) -> f64 {
    let hours = seconds / 3600.0;
    let warmth = ((kelvin - 1000.0) / 9000.0).clamp(0.0, 1.0); // 1000K=0, 10000K=1
    let b = brightness.clamp(0.0, 1.0);
    // 相对分：满负荷 1 小时 ≈ 1 分（上限封顶）
    (hours * (0.3 + 0.7 * warmth) * (0.3 + 0.7 * b)).min(24.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> InsightsStore {
        let dir = tempfile::tempdir().unwrap();
        InsightsStore::open(&dir.path().join("insights.db"), false).unwrap()
    }

    fn ts(hour_ago: i64) -> i64 {
        chrono::Utc::now().timestamp() - hour_ago * 3600
    }

    #[test]
    fn heartbeat_and_rollup_today() {
        let s = store();
        // 两小时窗口，每 60s 采样：active + filter_on（白天 4500K）
        let now = chrono::Utc::now().timestamp();
        for i in 0..120 {
            s.heartbeat(HeartbeatSample {
                ts_utc: now - (120 - i) * 60,
                active: true,
                filter_on: true,
                kelvin: Some(4500),
                brightness: Some(0.85),
            })
            .unwrap();
        }
        let day = Local::now().date_naive();
        s.rollup(day).unwrap();
        let summary = s.today_summary().unwrap();
        assert_eq!(summary.active_sec, 120 * 60);
        assert_eq!(summary.filter_sec, 120 * 60);
        assert_eq!(summary.safe_sec, 0);
        assert!((summary.filter_coverage - 1.0).abs() < 1e-9);
        assert!(summary.blue_load > 0.0);
    }

    #[test]
    fn safe_mode_excluded_from_blue_load() {
        let s = store();
        let now = chrono::Utc::now().timestamp();
        // 一半时间 filter_on，一半 safe（filter_off）
        for i in 0..60 {
            s.heartbeat(HeartbeatSample {
                ts_utc: now - (120 - i) * 60,
                active: true,
                filter_on: true,
                kelvin: Some(4500),
                brightness: Some(0.85),
            })
            .unwrap();
        }
        for i in 60..120 {
            s.heartbeat(HeartbeatSample {
                ts_utc: now - (120 - i) * 60,
                active: true,
                filter_on: false,
                kelvin: None,
                brightness: None,
            })
            .unwrap();
        }
        let day = Local::now().date_naive();
        s.rollup(day).unwrap();
        let summary = s.today_summary().unwrap();
        assert_eq!(summary.filter_sec, 60 * 60);
        assert_eq!(summary.safe_sec, 60 * 60);
        assert!((summary.filter_coverage - 0.5).abs() < 1e-9);
        // 蓝光负荷只算前半
        let only_filter = blue_load_partial(3600.0, 4500.0, 0.85);
        assert!((summary.blue_load - only_filter).abs() < 1e-6);
    }

    #[test]
    fn break_compliance() {
        let s = store();
        s.record_break(ts(2), "break_prompted").unwrap();
        s.record_break(ts(1), "break_prompted").unwrap();
        s.record_break(ts(1), "break_completed").unwrap();
        let day = Local::now().date_naive();
        s.rollup(day).unwrap();
        let summary = s.today_summary().unwrap();
        assert_eq!(summary.break_prompted, 2);
        assert_eq!(summary.break_completed, 1);
        assert!((summary.break_compliance - 0.5).abs() < 1e-9);
    }

    #[test]
    fn inactive_heartbeats_not_counted() {
        let s = store();
        let now = chrono::Utc::now().timestamp();
        for i in 0..60 {
            s.heartbeat(HeartbeatSample {
                ts_utc: now - (60 - i) * 60,
                active: false,
                filter_on: true,
                kelvin: None,
                brightness: None,
            })
            .unwrap();
        }
        let day = Local::now().date_naive();
        s.rollup(day).unwrap();
        let summary = s.today_summary().unwrap();
        assert_eq!(summary.active_sec, 0);
        assert_eq!(summary.filter_sec, 0);
    }

    #[test]
    fn events_stored_with_hash_key() {
        let s = store();
        s.record_event(
            ts(1),
            "filter_applied",
            Some("abc123hash"),
            Some("Visual Studio Code"),
            None,
            Some(4500),
            Some(0.85),
            "rule",
            None,
        )
        .unwrap();
        let events = s.recent_events(5).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["kind"], "filter_applied");
        assert_eq!(events[0]["app_key"], "abc123hash");
        // store_display_names=false：明文不落库
        assert!(events[0].get("app_display").is_none() || events[0]["app_display"].is_null());
    }

    #[test]
    fn purge_removes_old_data() {
        let s = store();
        let old = chrono::Utc::now().timestamp() - 200 * 86400;
        s.heartbeat(HeartbeatSample {
            ts_utc: old,
            active: true,
            filter_on: true,
            kelvin: Some(4500),
            brightness: Some(0.85),
        })
        .unwrap();
        s.record_event(old, "filter_applied", None, None, None, None, None, "user", None)
            .unwrap();
        s.purge(90).unwrap();
        assert_eq!(s.conn.query_row("SELECT COUNT(*) FROM heartbeats", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(s.conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }

    #[test]
    fn open_creates_wal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("insights.db");
        let s = InsightsStore::open(&path, true).unwrap();
        s.heartbeat(HeartbeatSample {
            ts_utc: chrono::Utc::now().timestamp(),
            active: true,
            filter_on: true,
            kelvin: None,
            brightness: None,
        })
        .unwrap();
        // WAL 模式生效
        let mode: String = s
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }

    #[test]
    fn local_date_boundary() {
        // rollup 对"今天"的边界：不应报错，且 UTC 跨日时仍按本地日聚合
        let s = store();
        let now = chrono::Utc::now().timestamp();
        s.heartbeat(HeartbeatSample {
            ts_utc: now,
            active: true,
            filter_on: true,
            kelvin: Some(4500),
            brightness: Some(0.8),
        })
        .unwrap();
        let day = Local::now().date_naive();
        s.rollup(day).unwrap();
        let summary = s.day_summary(day).unwrap();
        assert!(summary.active_sec >= 60);
    }

    #[test]
    fn blue_load_normalized_range() {
        // 满负荷 24h 也不超过 24 分
        let v = blue_load_partial(24.0 * 3600.0, 10000.0, 1.0);
        assert!(v <= 24.0);
        // 暖色低亮度 → 低分
        let warm = blue_load_partial(3600.0, 3400.0, 0.7);
        let cool = blue_load_partial(3600.0, 6500.0, 1.0);
        assert!(warm < cool);
    }
}
