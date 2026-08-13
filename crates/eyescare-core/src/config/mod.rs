//! 配置层（PR-A02）：schema v1、原子持久化、迁移、导入导出。
//!
//! 路径：`%APPDATA%\EyesCare\config.json` + `rules.json`（设计 §9.1）。
//! 原子写：同目录 `*.tmp` → fsync → rename；失败保留 `.bak`。

mod store;

pub use store::{ConfigStore, ExportBundle, ImportOutcome};

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

// ---------- 显示 ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MultiMonitorMode {
    #[default]
    Sync,
    PerDisplay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineKind {
    /// GDI Gamma / CG Transfer（截图友好，默认）。
    #[default]
    Gamma,
    /// 兼容模式（Magnification 矩阵，截图发黄；仅 opt-in）。
    Compat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HdrPolicy {
    /// HDR 屏跳过 gamma（默认，KD18）。
    #[default]
    Skip,
    /// 用户强制仍施加（风险提示）。
    Force,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DayNightMode {
    #[default]
    CustomTimes,
    /// v0.2：粗略日落表。MVP 仅 custom_times + 手动。
    SunsetTable,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DayNightConfig {
    pub enabled: bool,
    pub transition_minutes: u32,
    #[serde(default = "default_day_night_mode")]
    pub mode: DayNightMode,
    /// "HH:MM" 24h。
    pub day_start: String,
    pub night_start: String,
}

fn default_day_night_mode() -> DayNightMode {
    DayNightMode::CustomTimes
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplayConfig {
    /// 目标色温 K（clamp [1000, 10000]）。
    #[serde(default = "default_kelvin")]
    pub kelvin: u32,
    /// 软件亮度 0..=1（1% 步进）。
    #[serde(default = "default_brightness")]
    pub brightness: f64,
    /// 当前预设 id（设计附录 A）。
    #[serde(default = "default_preset")]
    pub preset: String,
    /// 用户手动锁定（P2）。
    #[serde(default)]
    pub manual_lock: bool,
    #[serde(default = "default_multi_monitor")]
    pub multi_monitor: MultiMonitorMode,
    #[serde(default = "default_pipeline")]
    pub pipeline: PipelineKind,
    #[serde(default = "default_hdr_policy")]
    pub hdr_policy: HdrPolicy,
    #[serde(default)]
    pub day_night: DayNightConfig,
    /// 每屏覆盖（PerDisplay 模式）：display_id -> (kelvin, brightness)。
    #[serde(default)]
    pub per_display: std::collections::BTreeMap<String, PerDisplayOverride>,
}

fn default_kelvin() -> u32 {
    4500
}
fn default_brightness() -> f64 {
    0.85
}
fn default_preset() -> String {
    "health".into()
}
fn default_multi_monitor() -> MultiMonitorMode {
    MultiMonitorMode::Sync
}
fn default_pipeline() -> PipelineKind {
    PipelineKind::Gamma
}
fn default_hdr_policy() -> HdrPolicy {
    HdrPolicy::Skip
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            kelvin: default_kelvin(),
            brightness: default_brightness(),
            preset: default_preset(),
            manual_lock: false,
            multi_monitor: default_multi_monitor(),
            pipeline: default_pipeline(),
            hdr_policy: default_hdr_policy(),
            day_night: DayNightConfig {
                enabled: true,
                transition_minutes: 60,
                mode: DayNightMode::CustomTimes,
                day_start: "07:00".into(),
                night_start: "19:30".into(),
            },
            per_display: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct PerDisplayOverride {
    pub kelvin: u32,
    pub brightness: f64,
}

// ---------- 安全模式（旁路）持久化状态 ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafeModeSource {
    User,
    Rule,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafeModeHold {
    Duration,
    WhileMatch,
}

/// 旁路状态机持久化视图。运行时权威状态在 SafeModeController，此字段仅启动恢复用。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SafeModeConfig {
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub source: Option<SafeModeSource>,
    #[serde(default)]
    pub hold: Option<SafeModeHold>,
    #[serde(default = "default_safe_minutes")]
    pub default_minutes: u32,
    /// duration 到期后抑制规则重入（直到前台 app 变化）。
    #[serde(default = "default_suppress_sec")]
    pub suppress_rule_reenter_sec: u32,
    /// 用户手动退出后抑制规则重入秒数（设计硬性规则 2，默认 30s 开）。
    #[serde(default = "default_suppress_sec")]
    pub suppress_after_user_exit_sec: u32,
}

fn default_safe_minutes() -> u32 {
    15
}
fn default_suppress_sec() -> u32 {
    30
}

impl Default for SafeModeConfig {
    fn default() -> Self {
        Self {
            active: false,
            source: None,
            hold: None,
            default_minutes: default_safe_minutes(),
            suppress_rule_reenter_sec: default_suppress_sec(),
            suppress_after_user_exit_sec: default_suppress_sec(),
        }
    }
}

// ---------- 计时 / 引导休息 ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimerProfile {
    /// 普通：工作 N 分钟休息 M 分钟。
    Normal,
    /// 20-20-20：每 20min 远眺 20s（默认）。
    #[default]
    TwentyTwentyTwenty,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimerConfig {
    #[serde(default)]
    pub profile: TimerProfile,
    #[serde(default = "default_work_sec")]
    pub work_sec: u32,
    #[serde(default = "default_break_sec")]
    pub break_sec: u32,
    /// 空闲暂停阈值（设计 §6.1，默认 3–5 min）。
    #[serde(default = "default_idle_sec")]
    pub idle_pause_sec: u32,
    /// 休息时显示引导 Overlay。
    #[serde(default = "default_true")]
    pub guided: bool,
    /// 静默休息（不弹 Overlay，仅通知）。
    #[serde(default)]
    pub silent_break: bool,
}

fn default_work_sec() -> u32 {
    1200
}
fn default_break_sec() -> u32 {
    20
}
fn default_idle_sec() -> u32 {
    240
}
fn default_true() -> bool {
    true
}

impl Default for TimerConfig {
    fn default() -> Self {
        Self {
            profile: TimerProfile::TwentyTwentyTwenty,
            work_sec: default_work_sec(),
            break_sec: default_break_sec(),
            idle_pause_sec: default_idle_sec(),
            guided: true,
            silent_break: false,
        }
    }
}

// ---------- 洞察 / 隐私 / flags ----------

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct InsightsConfig {
    /// 明文 display_name 是否落历史表（默认 false，设计 §5.1）。
    #[serde(default)]
    pub store_app_display_names: bool,
    #[serde(default = "default_retention")]
    pub retention_days: u32,
}

fn default_retention() -> u32 {
    90
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PrivacyConfig {
    #[serde(default)]
    pub telemetry: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FlagsConfig {
    #[serde(default)]
    pub deep_link: bool,
    #[serde(default)]
    pub chronotype: bool,
}

// ---------- 根配置 ----------

/// config.json（schema v1）。新增字段必须带 `#[serde(default)]`，保证向前兼容。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppConfig {
    pub schema_version: u32,
    #[serde(default)]
    pub display: DisplayConfig,
    #[serde(default)]
    pub safe_mode: SafeModeConfig,
    #[serde(default)]
    pub timer: TimerConfig,
    #[serde(default)]
    pub insights: InsightsConfig,
    #[serde(default)]
    pub privacy: PrivacyConfig,
    #[serde(default)]
    pub flags: FlagsConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            display: DisplayConfig::default(),
            safe_mode: SafeModeConfig::default(),
            timer: TimerConfig::default(),
            insights: InsightsConfig::default(),
            privacy: PrivacyConfig::default(),
            flags: FlagsConfig::default(),
        }
    }
}

impl AppConfig {
    /// 色温 clamp + 校验（设计 §4.1）。
    pub fn validated(mut self) -> Result<Self, crate::Error> {
        self.display.kelvin = self.display.kelvin.clamp(1000, 10000);
        if !(0.0..=1.0).contains(&self.display.brightness) {
            return Err(crate::Error::InvalidConfig(format!(
                "brightness out of range: {}",
                self.display.brightness
            )));
        }
        if self.display.brightness < 0.01 {
            self.display.brightness = 0.01;
        }
        for o in self.display.per_display.values_mut() {
            o.kelvin = o.kelvin.clamp(1000, 10000);
            o.brightness = o.brightness.clamp(0.01, 1.0);
        }
        if self.display.day_night.transition_minutes == 0 {
            return Err(crate::Error::InvalidConfig(
                "day_night.transition_minutes must be > 0".into(),
            ));
        }
        // 时间格式校验：非法 HH:MM 必须显式报错，不能静默回落默认值
        if !is_hhmm(&self.display.day_night.day_start) {
            return Err(crate::Error::InvalidConfig(format!(
                "day_night.day_start must be HH:MM, got {:?}",
                self.display.day_night.day_start
            )));
        }
        if !is_hhmm(&self.display.day_night.night_start) {
            return Err(crate::Error::InvalidConfig(format!(
                "day_night.night_start must be HH:MM, got {:?}",
                self.display.day_night.night_start
            )));
        }
        // 计时边界：work_sec/break_sec/idle_pause_sec 合理性（防 0 秒立即休息/永久暂停）
        if self.timer.work_sec < 30 {
            return Err(crate::Error::InvalidConfig(format!(
                "timer.work_sec must be >= 30, got {}",
                self.timer.work_sec
            )));
        }
        if self.timer.break_sec < 5 {
            return Err(crate::Error::InvalidConfig(format!(
                "timer.break_sec must be >= 5, got {}",
                self.timer.break_sec
            )));
        }
        if self.timer.idle_pause_sec < 10 {
            return Err(crate::Error::InvalidConfig(format!(
                "timer.idle_pause_sec must be >= 10, got {}",
                self.timer.idle_pause_sec
            )));
        }
        Ok(self)
    }
}

/// "HH:MM" 24h 格式校验。
fn is_hhmm(s: &str) -> bool {
    let mut it = s.split(':');
    let (Some(h), Some(m), None) = (it.next(), it.next(), it.next()) else {
        return false;
    };
    let (Ok(h), Ok(m)) = (h.parse::<u32>(), m.parse::<u32>()) else {
        return false;
    };
    h < 24 && m < 60
}

// ---------- 规则配置 ----------

use crate::rules::Rule;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchPolicy {
    #[default]
    FirstMatchWins,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RulesConfig {
    pub schema_version: u32,
    #[serde(default)]
    pub match_policy: MatchPolicy,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

impl RulesConfig {
    /// 整包校验：每条规则合法 + id 唯一（first_match_wins 依赖数组序，重复 id 会乱）。
    pub fn validate_all(&self) -> crate::Result<()> {
        let mut seen = std::collections::HashSet::new();
        for r in &self.rules {
            r.validate()?;
            if !seen.insert(r.id.clone()) {
                return Err(crate::Error::RuleValidation(format!(
                    "duplicate rule id: {}",
                    r.id
                )));
            }
        }
        Ok(())
    }
}

impl Default for RulesConfig {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            match_policy: MatchPolicy::FirstMatchWins,
            rules: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_roundtrip() {
        let cfg = AppConfig::default();
        let json = serde_json::to_string_pretty(&cfg).unwrap();
        let back: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
        assert_eq!(back.schema_version, SCHEMA_VERSION);
        assert_eq!(back.display.kelvin, 4500);
        assert_eq!(back.timer.profile, TimerProfile::TwentyTwentyTwenty);
    }

    #[test]
    fn missing_fields_use_defaults() {
        // 老配置缺字段时必须能加载（向前兼容）
        let json = r#"{"schema_version":1}"#;
        let cfg: AppConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.display.kelvin, 4500);
        assert!(!cfg.display.day_night.enabled || cfg.display.day_night.transition_minutes >= 1);
        assert_eq!(cfg.safe_mode.default_minutes, 15);
    }

    #[test]
    fn kelvin_clamp() {
        let mut cfg = AppConfig::default();
        cfg.display.kelvin = 500;
        let v = cfg.validated().unwrap();
        assert_eq!(v.display.kelvin, 1000);
    }

    #[test]
    fn brightness_range_validated() {
        let mut cfg = AppConfig::default();
        cfg.display.brightness = 1.5;
        assert!(cfg.clone().validated().is_err());
        cfg.display.brightness = -0.1;
        assert!(cfg.validated().is_err());
    }

    #[test]
    fn bad_time_format_rejected() {
        let mut cfg = AppConfig::default();
        cfg.display.day_night.day_start = "25:00".into();
        assert!(cfg.clone().validated().is_err());
        cfg.display.day_night.day_start = "07:60".into();
        assert!(cfg.clone().validated().is_err());
        // 单数字小时（"7:30"）是可解析的合法时间（NaiveTime 接受）
        cfg.display.day_night.day_start = "7:30".into();
        assert!(cfg.clone().validated().is_ok());
        cfg.display.day_night.day_start = "ab:cd".into();
        assert!(cfg.clone().validated().is_err());
        // 合法格式
        cfg.display.day_night.day_start = "07:00".into();
        cfg.display.day_night.night_start = "19:30".into();
        assert!(cfg.validated().is_ok());
    }

    #[test]
    fn timer_bounds_validated() {
        let mut cfg = AppConfig::default();
        cfg.timer.work_sec = 5;
        assert!(cfg.clone().validated().is_err());
        cfg.timer.work_sec = 1200;
        cfg.timer.break_sec = 2;
        assert!(cfg.clone().validated().is_err());
        cfg.timer.break_sec = 20;
        cfg.timer.idle_pause_sec = 3;
        assert!(cfg.validated().is_err());
    }

    #[test]
    fn rules_duplicate_id_rejected() {
        let mut rules = RulesConfig::default();
        let rule = crate::rules::builtin_templates().remove(0);
        rules.rules = vec![rule.clone(), rule];
        assert!(rules.validate_all().is_err());
    }
}
