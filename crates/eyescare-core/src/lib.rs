//! EyesCare 核心逻辑（平台无关）。
//!
//! 模块划分对应设计文档 PR-A02…A16：
//! - `config`：schema v1 + 原子持久化 + 迁移 + 导入导出（A02）
//! - `display`：算法（A03）+ Resolver/DisplayService/DayNight（A06）
//! - `safe_mode`：滤镜旁路状态机（A08）
//! - `scene` / `rules`：场景与规则引擎（A09/A10）
//! - `timer` / `guided`：计时 + 引导休息（A13/A14）
//! - `insights`：本地洞察 SQLite（A16）
//! - `events`：防刷事件总线
//!
//! 本 crate 不直接触碰平台 API；所有平台能力通过 `eyescare-platform` traits 注入。

pub mod config;
pub mod display;
pub mod error;
pub mod events;
pub mod guided;
pub mod insights;
pub mod rules;
pub mod safe_mode;
pub mod scene;
pub mod scene_engine;
pub mod timer;

pub use error::{Error, Result};

/// 默认配置文件名（AppData/EyesCare 下）。
pub const CONFIG_FILE: &str = "config.json";
/// 规则文件名。
pub const RULES_FILE: &str = "rules.json";
/// 洞察数据库文件名。
pub const INSIGHTS_DB: &str = "insights.db";
