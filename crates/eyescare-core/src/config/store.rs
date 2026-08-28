//! 原子持久化 + 迁移 + 导入导出（设计 §9、§Data Model）。
//!
//! 原子写协议：`<file>.tmp` 写入 → `sync_all` → 旧文件 rename 到 `.bak` → tmp rename 到位。
//! 主文件缺失/损坏时回退 `*.json.bak`；导入半失败则从 `.bak` 回滚。

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
        if path.exists() {
            match self.load_config_from(&path) {
                Ok(cfg) => return Ok(cfg),
                Err(e @ Error::UnsupportedSchema { .. }) => return Err(e),
                Err(primary_err) => {
                    if let Ok(cfg) = self.try_restore_config_from_bak() {
                        return Ok(cfg);
                    }
                    return Err(primary_err);
                }
            }
        }
        if let Ok(cfg) = self.try_restore_config_from_bak() {
            return Ok(cfg);
        }
        let cfg = AppConfig::default();
        self.save_config(&cfg)?;
        Ok(cfg)
    }

    pub fn load_rules(&self) -> Result<RulesConfig> {
        let path = self.rules_path();
        if path.exists() {
            match self.load_rules_from(&path) {
                Ok(rules) => {
                    if rules.schema_version != read_schema_version(&path)? {
                        self.save_rules(&rules)?;
                    }
                    return Ok(rules);
                }
                Err(e @ Error::UnsupportedSchema { .. }) => return Err(e),
                Err(primary_err) => {
                    if let Ok(rules) = self.try_restore_rules_from_bak() {
                        return Ok(rules);
                    }
                    return Err(primary_err);
                }
            }
        }
        if let Ok(rules) = self.try_restore_rules_from_bak() {
            return Ok(rules);
        }
        let rules = RulesConfig::default();
        self.save_rules(&rules)?;
        Ok(rules)
    }

    fn load_config_from(&self, path: &Path) -> Result<AppConfig> {
        let parsed = parse_json_file::<AppConfig>(path)?;
        self.migrate_config(parsed)
    }

    fn try_restore_config_from_bak(&self) -> Result<AppConfig> {
        let path = self.config_path();
        let bak = bak_path(&path);
        if !bak.exists() {
            return Err(Error::InvalidConfig("config.json.bak missing".into()));
        }
        let parsed = parse_json_file::<AppConfig>(&bak)?;
        if parsed.schema_version > SCHEMA_VERSION {
            return Err(Error::UnsupportedSchema {
                found: parsed.schema_version,
                latest: SCHEMA_VERSION,
            });
        }
        // 先校验再覆盖主文件，避免把坏 bak 写回
        parsed.clone().validated()?;
        fs::copy(&bak, &path)?;
        self.migrate_config(parsed)
    }

    fn try_restore_rules_from_bak(&self) -> Result<RulesConfig> {
        let path = self.rules_path();
        let bak = bak_path(&path);
        if !bak.exists() {
            return Err(Error::InvalidConfig("rules.json.bak missing".into()));
        }
        let original_schema = read_schema_version(&bak)?;
        let rules = self.load_rules_from(&bak)?;
        fs::copy(&bak, &path)?;
        if rules.schema_version != original_schema {
            self.save_rules(&rules)?;
        }
        Ok(rules)
    }

    fn load_rules_from(&self, path: &Path) -> Result<RulesConfig> {
        let parsed = parse_json_file::<RulesConfig>(path)?;
        if parsed.schema_version > SCHEMA_VERSION {
            return Err(Error::UnsupportedSchema {
                found: parsed.schema_version,
                latest: SCHEMA_VERSION,
            });
        }
        let mut rules = parsed;
        if rules.schema_version < SCHEMA_VERSION {
            rules.schema_version = SCHEMA_VERSION;
        }
        rules.validate_all()?;
        Ok(rules)
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
            let validated = cfg.validated()?;
            // 迁移是持久化动作：立即回写，保证下次加载无需再迁。
            self.save_config(&validated)?;
            return Ok(validated);
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

    /// 导入整包。信封与内层 schema 均须 == SCHEMA_VERSION；任一失败则整体拒绝（不半写）。
    pub fn import(&self, bundle: &ExportBundle) -> Result<ImportOutcome> {
        reject_unless_current(bundle.schema_version)?;
        reject_unless_current(bundle.config.schema_version)?;
        reject_unless_current(bundle.rules.schema_version)?;
        let cfg = bundle.config.clone().validated()?;
        bundle.rules.validate_all()?;
        self.save_config(&cfg)?;
        if let Err(e) = self.save_rules(&bundle.rules) {
            // rules 落盘失败：从 atomic_write 留下的 config.json.bak 回滚
            restore_primary_from_bak(&self.config_path())?;
            return Err(e);
        }
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

fn bak_path(path: &Path) -> PathBuf {
    path.with_extension("json.bak")
}

fn parse_json_file<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let raw = fs::read_to_string(path)?;
    serde_json::from_str(&raw)
        .map_err(|e| Error::InvalidConfig(format!("parse {} failed: {e}", path.display())))
}

fn read_schema_version(path: &Path) -> Result<u32> {
    let value = parse_json_file::<serde_json::Value>(path)?;
    value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| Error::InvalidConfig(format!("missing/invalid schema_version in {}", path.display())))
}

fn reject_unless_current(found: u32) -> Result<()> {
    if found != SCHEMA_VERSION {
        return Err(Error::UnsupportedSchema {
            found,
            latest: SCHEMA_VERSION,
        });
    }
    Ok(())
}

/// 用 `.bak` 覆盖主文件。无 bak 时删掉半写入的主文件，保持导入全有或全无。
fn restore_primary_from_bak(path: &Path) -> Result<()> {
    let bak = bak_path(path);
    if bak.exists() {
        fs::copy(&bak, path)?;
        return Ok(());
    }
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// 原子写：tmp → sync_all → 旧文件 rename 到 `.bak` → tmp rename 到位。
/// 第二步 rename 失败则尝试把 bak 改回主路径。Windows 不能 rename 覆盖已存在目标。
pub fn atomic_write<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    let bak = bak_path(path);
    let json = serde_json::to_string_pretty(value)?;
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(json.as_bytes())?;
        f.sync_all()?;
    }
    if path.exists() {
        if bak.exists() {
            fs::remove_file(&bak)?;
        }
        fs::rename(path, &bak)?;
    }
    if let Err(e) = fs::rename(&tmp, path) {
        if bak.exists() {
            let _ = fs::rename(&bak, path);
        }
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
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
        let bak = store.config_path().with_extension("json.bak");
        assert!(bak.exists());
        let bak_cfg: AppConfig = serde_json::from_str(&fs::read_to_string(&bak).unwrap()).unwrap();
        assert_eq!(bak_cfg.display.kelvin, 3800);
        assert!(!store.config_path().with_extension("json.tmp").exists());
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
        let mut cfg = AppConfig::default();
        cfg.display.kelvin = 3400;
        store.save_config(&cfg).unwrap();
        let path = store.config_path();
        let bak = path.with_extension("json.bak");
        fs::copy(&path, &bak).unwrap();
        fs::write(&path, "{not json").unwrap();
        let loaded = store.load_config().unwrap();
        assert_eq!(loaded.display.kelvin, 3400);
        let restored: AppConfig =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(restored.display.kelvin, 3400);
    }

    #[test]
    fn load_config_restores_bak_when_primary_missing() {
        let store = ConfigStore::new(tmpdir());
        let mut cfg = AppConfig::default();
        cfg.display.kelvin = 3800;
        store.save_config(&cfg).unwrap();
        let path = store.config_path();
        let bak = path.with_extension("json.bak");
        fs::rename(&path, &bak).unwrap();
        let loaded = store.load_config().unwrap();
        assert_eq!(loaded.display.kelvin, 3800);
        assert!(path.exists());
    }

    #[test]
    fn load_rules_restores_bak_when_primary_missing() {
        let store = ConfigStore::new(tmpdir());
        let mut rules = RulesConfig::default();
        rules.rules = crate::rules::builtin_templates();
        rules.rules.truncate(1);
        store.save_rules(&rules).unwrap();
        let path = store.rules_path();
        let bak = path.with_extension("json.bak");
        fs::rename(&path, &bak).unwrap();
        let loaded = store.load_rules().unwrap();
        assert_eq!(loaded.rules.len(), 1);
        assert!(path.exists());
    }

    #[test]
    fn old_rules_schema_is_migrated_and_persisted() {
        let store = ConfigStore::new(tmpdir());
        fs::write(
            store.rules_path(),
            r#"{"schema_version":0,"match_policy":"first_match_wins","rules":[]}"#,
        )
        .unwrap();
        let loaded = store.load_rules().unwrap();
        assert_eq!(loaded.schema_version, SCHEMA_VERSION);
        let persisted: RulesConfig =
            serde_json::from_str(&fs::read_to_string(store.rules_path()).unwrap()).unwrap();
        assert_eq!(persisted.schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn corrupt_config_without_bak_still_errors() {
        let store = ConfigStore::new(tmpdir());
        store.save_config(&AppConfig::default()).unwrap();
        fs::write(store.config_path(), "{not json").unwrap();
        // 无 bak 时解析失败不得静默覆盖成出厂默认
        assert!(store.load_config().is_err());
    }

    #[test]
    fn load_config_partial_display_uses_legal_day_night() {
        let store = ConfigStore::new(tmpdir());
        fs::write(
            store.config_path(),
            r#"{"schema_version":1,"display":{"kelvin":3800}}"#,
        )
        .unwrap();
        let cfg = store.load_config().unwrap();
        assert_eq!(cfg.display.kelvin, 3800);
        assert!(cfg.display.day_night.enabled);
        assert_eq!(cfg.display.day_night.transition_minutes, 60);
        assert_eq!(cfg.display.day_night.day_start, "07:00");
        assert_eq!(cfg.display.day_night.night_start, "19:30");
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

    #[test]
    fn import_rejects_nested_schema_version() {
        let store = ConfigStore::new(tmpdir());
        let mut bundle = store.export().unwrap();
        bundle.config.schema_version = 99;
        let err = store.import(&bundle).unwrap_err();
        assert!(matches!(err, Error::UnsupportedSchema { found: 99, .. }));

        let mut bundle = store.export().unwrap();
        bundle.rules.schema_version = 0;
        let err = store.import(&bundle).unwrap_err();
        assert!(matches!(err, Error::UnsupportedSchema { found: 0, .. }));
    }

    #[test]
    fn import_rolls_back_config_if_rules_save_fails() {
        let store = ConfigStore::new(tmpdir());
        let mut cfg = AppConfig::default();
        cfg.display.kelvin = 3800;
        store.save_config(&cfg).unwrap();

        let mut bundle = store.export().unwrap();
        bundle.config.display.kelvin = 4200;
        bundle.rules.rules = crate::rules::builtin_templates();
        bundle.rules.rules.truncate(1);

        // 占住 rules.json.tmp，使第二条原子写失败
        fs::create_dir(store.rules_path().with_extension("json.tmp")).unwrap();
        assert!(store.import(&bundle).is_err());
        assert_eq!(store.load_config().unwrap().display.kelvin, 3800);
        assert!(store.load_rules().unwrap().rules.is_empty());
    }
}
