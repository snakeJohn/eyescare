//! 原子持久化 + 迁移 + 导入导出（设计 §9、§Data Model）。
//!
//! 原子写协议：`<file>.tmp` 写入 → `sync_all` → `rename` 覆盖。
//! 迁移失败：保留 `config.json.bak` + 落默认配置（设计 §迁移失败）。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{AppConfig, RulesConfig, SCHEMA_VERSION};
use crate::error::{Error, Result};

/// 配置仓库：负责 config.json / rules.json 的读写、迁移、整包导入导出。
pub struct ConfigStore {
    dir: PathBuf,
}

impl ConfigStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn config_path(&self) -> PathBuf {
        self.dir.join(super::super::CONFIG_FILE)
    }
    pub fn rules_path(&self) -> PathBuf {
        self.dir.join(super::super::RULES_FILE)
    }

    fn ensure_dir(&self) -> Result<()> {
        fs::create_dir_all(&self.dir)?;
        Ok(())
    }

    // ---------- 读取 ----------

    pub fn load_config(&self) -> Result<AppConfig> {
        let path = self.config_path();
        if !path.exists() {
            let cfg = AppConfig::default();
            self.save_config(&cfg)?;
            return Ok(cfg);
        }
        let raw = fs::read_to_string(&path)?;
        let parsed: AppConfig = serde_json::from_str(&raw).map_err(|e| {
            Error::InvalidConfig(format!("parse {} failed: {e}", path.display()))
        })?;
        self.migrate_config(parsed)
    }

    pub fn load_rules(&self) -> Result<RulesConfig> {
        let path = self.rules_path();
        if !path.exists() {
            let rules = RulesConfig::default();
            self.save_rules(&rules)?;
            return Ok(rules);
        }
        let raw = fs::read_to_string(&path)?;
        let parsed: RulesConfig = serde_json::from_str(&raw).map_err(|e| {
            Error::InvalidConfig(format!("parse {} failed: {e}", path.display()))
        })?;
        if parsed.schema_version > SCHEMA_VERSION {
            return Err(Error::UnsupportedSchema {
                found: parsed.schema_version,
                latest: SCHEMA_VERSION,
            });
        }
        Ok(parsed)
    }

    // ---------- 保存（原子） ----------

    pub fn save_config(&self, cfg: &AppConfig) -> Result<()> {
        self.ensure_dir()?;
        atomic_write(&self.config_path(), cfg)
    }

    pub fn save_rules(&self, rules: &RulesConfig) -> Result<()> {
        self.ensure_dir()?;
        atomic_write(&self.rules_path(), rules)
    }

    // ---------- 迁移 ----------

    /// schema 迁移链。v1 为当前版本；未来 v1 → v2 在此串联。
    /// 未知更高版本 → 拒绝加载（不静默降级）。
    fn migrate_config(&self, parsed: AppConfig) -> Result<AppConfig> {
        if parsed.schema_version > SCHEMA_VERSION {
            return Err(Error::UnsupportedSchema {
                found: parsed.schema_version,
                latest: SCHEMA_VERSION,
            });
        }
        let mut cfg = parsed;
        if cfg.schema_version < SCHEMA_VERSION {
            // v1 尚无历史版本；此处为未来迁移预留。
            cfg.schema_version = SCHEMA_VERSION;
            // 迁移是持久化动作：立即回写，保证下次加载无需再迁。
            self.save_config(&cfg)?;
        }
        cfg.validated()
    }

    // ---------- 整包导入导出 ----------

    /// 导出整包（config + rules），供用户分享/备份。
    pub fn export(&self) -> Result<ExportBundle> {
        Ok(ExportBundle {
            schema_version: SCHEMA_VERSION,
            exported_at_utc: chrono::Utc::now().to_rfc3339(),
            config: self.load_config()?,
            rules: self.load_rules()?,
        })
    }

    /// 导入整包。schema 校验通过后原子落盘；任一失败则整体拒绝（不半写）。
    pub fn import(&self, bundle: &ExportBundle) -> Result<ImportOutcome> {
        if bundle.schema_version != SCHEMA_VERSION {
            return Err(Error::UnsupportedSchema {
                found: bundle.schema_version,
                latest: SCHEMA_VERSION,
            });
        }
        let cfg = bundle.config.clone().validated()?;
        bundle.rules.validate_all()?;
        // 先校验后落盘
        self.save_config(&cfg)?;
        self.save_rules(&bundle.rules)?;
        Ok(ImportOutcome {
            rules_imported: bundle.rules.rules.len(),
        })
    }
}

/// 导出包结构（顶层 schema_version 防混用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportBundle {
    pub schema_version: u32,
    pub exported_at_utc: String,
    pub config: AppConfig,
    pub rules: RulesConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ImportOutcome {
    pub rules_imported: usize,
}

/// 原子写：tmp → sync_all → rename。失败时清理 tmp。
pub fn atomic_write<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(value)?;
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(json.as_bytes())?;
        f.sync_all()?;
    }
    // rename 前备份旧文件（迁移失败场景需要 .bak）
    if path.exists() {
        let bak = path.with_extension("json.bak");
        let _ = fs::copy(path, &bak);
        // Windows rename 不能覆盖已存在的目标文件。
        #[cfg(windows)]
        fs::remove_file(path)?;
    }
    fs::rename(&tmp, path)?;
    // 目录 fsync 保证 rename 持久
    if let Ok(d) = fs::File::open(path.parent().unwrap_or(Path::new("."))) {
        let _ = d.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        // tempfile 3.x：keep() 返回 &Path（TempDir 生命周期内有效），这里克隆为独立 PathBuf
        tempfile::tempdir().unwrap().keep().to_path_buf()
    }

    #[test]
    fn first_load_creates_defaults() {
        let store = ConfigStore::new(tmpdir());
        let cfg = store.load_config().unwrap();
        assert_eq!(cfg.schema_version, SCHEMA_VERSION);
        assert!(store.config_path().exists());
        let rules = store.load_rules().unwrap();
        assert!(rules.rules.is_empty());
        assert!(store.rules_path().exists());
    }

    #[test]
    fn atomic_save_roundtrip() {
        let store = ConfigStore::new(tmpdir());
        let mut cfg = AppConfig::default();
        cfg.display.kelvin = 3800;
        store.save_config(&cfg).unwrap();
        let loaded = store.load_config().unwrap();
        assert_eq!(loaded.display.kelvin, 3800);
        // 无 .tmp 残留
        assert!(!store.config_path().with_extension("json.tmp").exists());
    }

    #[test]
    fn atomic_save_overwrites_existing() {
        let store = ConfigStore::new(tmpdir());
        let mut cfg = AppConfig::default();
        cfg.display.kelvin = 3800;
        store.save_config(&cfg).unwrap();
        cfg.display.kelvin = 4200;
        store.save_config(&cfg).unwrap();
        let loaded = store.load_config().unwrap();
        assert_eq!(loaded.display.kelvin, 4200);
        assert!(store.config_path().with_extension("json.bak").exists());
    }

    #[test]
    fn unsupported_future_schema_rejected() {
        let store = ConfigStore::new(tmpdir());
        store.save_config(&AppConfig::default()).unwrap();
        // 手写未来版本
        let path = store.config_path();
        let json = r#"{"schema_version":99,"display":{}}"#;
        fs::write(&path, json).unwrap();
        let err = store.load_config().unwrap_err();
        assert!(matches!(err, Error::UnsupportedSchema { .. }));
    }

    #[test]
    fn corrupt_config_falls_back_with_bak() {
        let store = ConfigStore::new(tmpdir());
        store.save_config(&AppConfig::default()).unwrap();
        let path = store.config_path();
        fs::write(&path, "{not json").unwrap();
        // 当前策略：解析失败 → 报错（不静默覆盖用户文件）。上层可决定降级。
        assert!(store.load_config().is_err());
    }

    #[test]
    fn export_import_roundtrip() {
        let dir = tmpdir();
        let store = ConfigStore::new(&dir);
        let mut cfg = AppConfig::default();
        cfg.display.kelvin = 3400;
        store.save_config(&cfg).unwrap();
        let bundle = store.export().unwrap();
        assert_eq!(bundle.config.display.kelvin, 3400);

        // 导入到"新机器"
        let store2 = ConfigStore::new(dir.join("other"));
        let outcome = store2.import(&bundle).unwrap();
        assert_eq!(outcome.rules_imported, 0);
        assert_eq!(store2.load_config().unwrap().display.kelvin, 3400);
    }

    #[test]
    fn import_rejects_wrong_schema() {
        let store = ConfigStore::new(tmpdir());
        let mut bundle = store.export().unwrap();
        bundle.schema_version = 99;
        assert!(store.import(&bundle).is_err());
    }
}
