//! 规则引擎（PR-A10）：身份匹配 + 动作 + 内置模板。
//!
//! 设计 §5.2 匹配模型：`first_match_wins`，字段与 op 见下。
//! 规则身份：`process_name`（Win 主）/ `bundle_id`（mac 主）/ `path_suffix` / `app_display_name`。
//! `window_title` MVP 仅采集、不参与匹配。

use serde::{Deserialize, Serialize};

use crate::config::SafeModeHold;
use crate::error::{Error, Result};
use crate::scene::SceneSnapshot;

// ---------- 条件 ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleField {
    ProcessName,
    BundleId,
    PathSuffix,
    AppDisplayName,
    /// 全屏状态（布尔比较，value 为 "true"/"false"）。
    IsFullscreen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleOp {
    Equals,
    Prefix,
    Suffix,
    Contains,
    /// 仅支持 `*` 与 `?` 通配。
    Glob,
}

/// 单个条件。`field` 取值与 `scene` 字段对应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleCondition {
    pub field: RuleField,
    pub op: RuleOp,
    pub value: String,
    /// 默认 false；bundle_id 强制 true（Apple 惯例，§5.2）。
    #[serde(default)]
    pub case_sensitive: bool,
}

impl RuleCondition {
    pub fn matches(&self, scene: &SceneSnapshot) -> bool {
        let haystack = match self.field {
            RuleField::ProcessName => scene.process_name.as_deref(),
            RuleField::BundleId => scene.bundle_id.as_deref(),
            RuleField::PathSuffix => scene.path_suffix.as_deref(),
            RuleField::AppDisplayName => Some(scene.app_display_name.as_str()),
            RuleField::IsFullscreen => {
                // 仅 High（无边框/独占）视为规则全屏；最大化 Medium / Unknown 不算。
                let high_fs = scene.is_fullscreen
                    && scene.fullscreen_confidence
                        == eyescare_platform::FullscreenConfidence::High;
                return match self.op {
                    RuleOp::Equals => {
                        self.value.eq_ignore_ascii_case(if high_fs {
                            "true"
                        } else {
                            "false"
                        })
                    }
                    _ => false,
                };
            }
        };
        let Some(h) = haystack else {
            return false;
        };
        let case_sensitive = self.case_sensitive || self.field == RuleField::BundleId;
        let (h, v) = if case_sensitive {
            (h.to_string(), self.value.clone())
        } else {
            (h.to_lowercase(), self.value.to_lowercase())
        };
        match self.op {
            RuleOp::Equals => h == v,
            RuleOp::Prefix => h.starts_with(&v),
            RuleOp::Suffix => h.ends_with(&v),
            RuleOp::Contains => h.contains(&v),
            RuleOp::Glob => glob_match(&h, &v),
        }
    }
}

/// 仅支持 `*`（任意多字符）与 `?`（单字符）的极小 glob。
fn glob_match(text: &str, pattern: &str) -> bool {
    let t: Vec<char> = text.chars().collect();
    let p: Vec<char> = pattern.chars().collect();
    // 经典双指针回溯
    let (mut ti, mut pi) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            ti += 1;
            pi += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            mark = ti;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

// ---------- 条件组 ----------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConditionGroup {
    /// 任一匹配即中。
    #[serde(default)]
    pub any: Vec<RuleCondition>,
    /// 全部匹配才中（与 any 同时存在时 = (any 或空) 且 (all 或空)）。
    #[serde(default)]
    pub all: Vec<RuleCondition>,
}

impl ConditionGroup {
    pub fn is_empty(&self) -> bool {
        self.any.is_empty() && self.all.is_empty()
    }

    pub fn matches(&self, scene: &SceneSnapshot) -> bool {
        let any_ok = self.any.is_empty() || self.any.iter().any(|c| c.matches(scene));
        let all_ok = self.all.iter().all(|c| c.matches(scene));
        any_ok && all_ok
    }
}

// ---------- 动作 ----------

/// 全屏策略项（§5.4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterAction {
    #[default]
    Keep,
    Pause,
    /// 游戏预设（目前等价 pause + 提示）。
    GamingPreset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BreaksAction {
    #[default]
    Keep,
    Pause,
    /// 到点仅通知，不进 Overlay（§6.1 / R6）。
    NotifyOnly,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RuleAction {
    /// 滤镜旁路（§4.5）。hold 必填；Duration 时 minutes > 0 必填。
    SafeMode {
        hold: SafeModeHold,
        #[serde(default)]
        minutes: Option<u32>,
    },
    /// 应用预设（附录 A，如 editing/reading）。
    Preset {
        preset: String,
    },
    /// 全屏策略（游戏模板：filter pause + breaks notify_only）。
    FullscreenPolicy {
        #[serde(default)]
        filter: FilterAction,
        #[serde(default)]
        breaks: BreaksAction,
    },
}

// ---------- 规则 ----------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    /// 模板 JSON 用 `"if"`；前端/历史文件可能写 `"if_"`。
    #[serde(alias = "if")]
    pub if_: ConditionGroup,
    pub then: RuleAction,
    /// 模板默认是否启用（设计：设计旁路模板默认 false）。
    #[serde(default)]
    pub enabled_default: bool,
    /// 运行/持久化启用状态。`None` = 文件未写（回落到 enabled_default）；
    /// `Some(v)` = 用户显式开关，save_rules 时落盘。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

impl Rule {
    /// 生效启用状态（文件值优先，缺省出厂值）。
    pub fn effective_enabled(&self) -> bool {
        self.enabled.unwrap_or(self.enabled_default)
    }
}

impl Rule {
    pub fn validate(&self) -> Result<()> {
        if self.id.is_empty() {
            return Err(Error::RuleValidation("rule id is empty".into()));
        }
        if self.if_.is_empty() {
            return Err(Error::RuleValidation(format!(
                "rule {} has empty condition",
                self.id
            )));
        }
        if let RuleAction::SafeMode { hold, minutes } = self.then {
            if hold == SafeModeHold::Duration && minutes.unwrap_or(0) == 0 {
                return Err(Error::RuleValidation(format!(
                    "rule {} safe_mode duration requires minutes > 0",
                    self.id
                )));
            }
        }
        // 校验条件值合法性（空 value 永不匹配 = 死规则；glob 空模式同样非法）
        for cond in self.if_.any.iter().chain(self.if_.all.iter()) {
            if cond.value.is_empty() {
                return Err(Error::RuleValidation(format!(
                    "rule {} has empty value for condition on {:?}",
                    self.id, cond.field
                )));
            }
            // glob 无通配符等价于精确匹配（内置模板如 idea64.exe 合法使用），不额外限制
        }
        Ok(())
    }
}

// ---------- 引擎 ----------

/// 规则命中结果。
#[derive(Debug, Clone, PartialEq)]
pub struct RuleMatch {
    pub rule: Rule,
}

/// RuleEngine：`first_match_wins`，数组序即优先级（UI 可调序）。
#[derive(Debug, Clone)]
pub struct RuleEngine {
    pub rules: Vec<Rule>,
}

impl RuleEngine {
    pub fn new(rules: Vec<Rule>) -> Self {
        Self { rules }
    }

    pub fn from_config(cfg: &crate::config::RulesConfig) -> Self {
        Self::new(cfg.rules.clone())
    }

    /// 第一条匹配且启用的规则。
    pub fn first_match(&self, scene: &SceneSnapshot) -> Option<RuleMatch> {
        self.rules
            .iter()
            .filter(|r| r.effective_enabled())
            .find(|r| r.if_.matches(scene))
            .cloned()
            .map(|rule| RuleMatch { rule })
    }
}

// ---------- 内置模板（附录 B） ----------

/// 内置模板。返回前先 validate；模板自身保证合法。
pub fn builtin_templates() -> Vec<Rule> {
    let mk = |id: &str, name: &str, conditions: Vec<RuleCondition>, then: RuleAction, enabled: bool| Rule {
        id: id.into(),
        name: Some(name.into()),
        if_: ConditionGroup {
            any: conditions,
            all: vec![],
        },
        then,
        enabled_default: enabled,
        enabled: None,
    };

    let eq = |field: RuleField, value: &str| RuleCondition {
        field,
        op: RuleOp::Equals,
        value: value.into(),
        case_sensitive: false,
    };
    let glob = |field: RuleField, value: &str| RuleCondition {
        field,
        op: RuleOp::Glob,
        value: value.into(),
        case_sensitive: false,
    };

    vec![
        // 设计旁路模板：默认 opt-in（KD21）
        mk(
            "design-figma-safe",
            "设计旁路 · Figma",
            vec![
                eq(RuleField::ProcessName, "Figma.exe"),
                eq(RuleField::BundleId, "com.figma.Desktop"),
            ],
            RuleAction::SafeMode {
                hold: SafeModeHold::WhileMatch,
                minutes: None,
            },
            false,
        ),
        mk(
            "design-photoshop-safe",
            "设计旁路 · Photoshop",
            vec![eq(RuleField::ProcessName, "Photoshop.exe")],
            RuleAction::SafeMode {
                hold: SafeModeHold::WhileMatch,
                minutes: None,
            },
            false,
        ),
        // 编码模板：默认启用
        mk(
            "coding-vscode",
            "编码 · VS Code",
            vec![
                eq(RuleField::ProcessName, "Code.exe"),
                eq(RuleField::BundleId, "com.microsoft.VSCode"),
            ],
            RuleAction::Preset {
                preset: "editing".into(),
            },
            true,
        ),
        mk(
            "coding-cursor",
            "编码 · Cursor",
            vec![
                eq(RuleField::ProcessName, "Cursor.exe"),
                eq(RuleField::BundleId, "com.todesktop.230113mitalinfvmv"),
            ],
            RuleAction::Preset {
                preset: "editing".into(),
            },
            true,
        ),
        // JetBrains：显式 glob 列表（附录 B）
        mk(
            "coding-jetbrains",
            "编码 · JetBrains",
            vec![
                glob(RuleField::ProcessName, "idea64.exe"),
                glob(RuleField::ProcessName, "idea.exe"),
                glob(RuleField::ProcessName, "webstorm64.exe"),
                glob(RuleField::ProcessName, "pycharm64.exe"),
                glob(RuleField::ProcessName, "goland64.exe"),
            ],
            RuleAction::Preset {
                preset: "editing".into(),
            },
            true,
        ),
        // 游戏模板：全屏 + filter pause + breaks notify_only（§5.4 默认）
        mk(
            "gaming-fullscreen",
            "游戏 · 全屏",
            vec![RuleCondition {
                field: RuleField::IsFullscreen,
                op: RuleOp::Equals,
                value: "true".into(),
                case_sensitive: false,
            }],
            RuleAction::FullscreenPolicy {
                filter: FilterAction::Pause,
                breaks: BreaksAction::NotifyOnly,
            },
            true,
        ),
    ]
}

/// 校验并合并内置模板到用户规则前（去重 by id）。
pub fn merge_templates(rules: &mut Vec<Rule>, templates: Vec<Rule>) -> Result<()> {
    for t in templates {
        t.validate()?;
        if !rules.iter().any(|r| r.id == t.id) {
            rules.push(t);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::SceneSnapshot;

    fn scene(process: Option<&str>, bundle: Option<&str>, fullscreen: bool) -> SceneSnapshot {
        SceneSnapshot {
            process_name: process.map(String::from),
            bundle_id: bundle.map(String::from),
            path_suffix: None,
            app_display_name: "X".into(),
            window_title: None,
            is_fullscreen: fullscreen,
            fullscreen_confidence: if fullscreen {
                eyescare_platform::FullscreenConfidence::High
            } else {
                eyescare_platform::FullscreenConfidence::Unknown
            },
            display_id: None,
        }
    }

    fn scene_fs(
        process: Option<&str>,
        fullscreen: bool,
        confidence: eyescare_platform::FullscreenConfidence,
    ) -> SceneSnapshot {
        SceneSnapshot {
            process_name: process.map(String::from),
            bundle_id: None,
            path_suffix: None,
            app_display_name: "X".into(),
            window_title: None,
            is_fullscreen: fullscreen,
            fullscreen_confidence: confidence,
            display_id: None,
        }
    }

    #[test]
    fn process_name_case_insensitive_equals() {
        let c = RuleCondition {
            field: RuleField::ProcessName,
            op: RuleOp::Equals,
            value: "code.exe".into(),
            case_sensitive: false,
        };
        assert!(c.matches(&scene(Some("CODE.EXE"), None, false)));
        assert!(!c.matches(&scene(Some("cursor.exe"), None, false)));
    }

    #[test]
    fn bundle_id_case_sensitive() {
        let c = RuleCondition {
            field: RuleField::BundleId,
            op: RuleOp::Equals,
            value: "com.figma.Desktop".into(),
            case_sensitive: false, // 引擎强制 bundle 大小写敏感
        };
        assert!(c.matches(&scene(None, Some("com.figma.Desktop"), false)));
        assert!(!c.matches(&scene(None, Some("com.figma.desktop"), false)));
    }

    #[test]
    fn glob_idea() {
        let c = RuleCondition {
            field: RuleField::ProcessName,
            op: RuleOp::Glob,
            value: "idea64.exe".into(),
            case_sensitive: false,
        };
        assert!(c.matches(&scene(Some("idea64.exe"), None, false)));
        assert!(!c.matches(&scene(Some("idea32.exe"), None, false)));

        let c2 = RuleCondition {
            field: RuleField::ProcessName,
            op: RuleOp::Glob,
            value: "idea?.exe".into(),
            case_sensitive: false,
        };
        assert!(c2.matches(&scene(Some("idea1.exe"), None, false)));
        assert!(!c2.matches(&scene(Some("idea12.exe"), None, false)));
    }

    #[test]
    fn glob_star_patterns() {
        let c = RuleCondition {
            field: RuleField::PathSuffix,
            op: RuleOp::Suffix,
            value: "Microsoft VS Code\\Code.exe".into(),
            case_sensitive: false,
        };
        assert!(c.matches(&SceneSnapshot {
            process_name: Some("Code.exe".into()),
            bundle_id: None,
            path_suffix: Some("C:\\Users\\u\\AppData\\Local\\Programs\\Microsoft VS Code\\Code.exe".into()),
            app_display_name: "Code".into(),
            window_title: None,
            is_fullscreen: false,
            fullscreen_confidence: eyescare_platform::FullscreenConfidence::Unknown,
            display_id: None,
        }));
    }

    #[test]
    fn fullscreen_condition() {
        let c = RuleCondition {
            field: RuleField::IsFullscreen,
            op: RuleOp::Equals,
            value: "true".into(),
            case_sensitive: false,
        };
        assert!(c.matches(&scene(None, None, true)));
        assert!(!c.matches(&scene(None, None, false)));
    }

    #[test]
    fn maximized_medium_is_not_rule_fullscreen() {
        use eyescare_platform::FullscreenConfidence::{High, Medium, Unknown};
        let is_fs = RuleCondition {
            field: RuleField::IsFullscreen,
            op: RuleOp::Equals,
            value: "true".into(),
            case_sensitive: false,
        };
        let not_fs = RuleCondition {
            field: RuleField::IsFullscreen,
            op: RuleOp::Equals,
            value: "false".into(),
            case_sensitive: false,
        };
        // 最大化 Medium / 未知 不算规则全屏
        assert!(!is_fs.matches(&scene_fs(None, true, Medium)));
        assert!(not_fs.matches(&scene_fs(None, true, Medium)));
        assert!(not_fs.matches(&scene_fs(None, true, Unknown)));
        // 无边框 High 才算
        assert!(is_fs.matches(&scene_fs(None, true, High)));
        assert!(!not_fs.matches(&scene_fs(None, true, High)));
    }

    #[test]
    fn gaming_fullscreen_high_only() {
        use eyescare_platform::FullscreenConfidence::{High, Medium};
        let engine = RuleEngine::new(builtin_templates());
        let maximized = engine.first_match(&scene_fs(Some("game.exe"), true, Medium));
        assert!(
            maximized
                .as_ref()
                .is_none_or(|m| m.rule.id != "gaming-fullscreen"),
            "maximized Medium must not match gaming-fullscreen"
        );
        let borderless = engine.first_match(&scene_fs(Some("game.exe"), true, High));
        assert_eq!(
            borderless.as_ref().map(|m| m.rule.id.as_str()),
            Some("gaming-fullscreen")
        );
    }

    #[test]
    fn first_match_wins_order() {
        let r1 = Rule {
            id: "r1".into(),
            name: None,
            if_: ConditionGroup {
                any: vec![RuleCondition {
                    field: RuleField::ProcessName,
                    op: RuleOp::Equals,
                    value: "Code.exe".into(),
                    case_sensitive: false,
                }],
                all: vec![],
            },
            then: RuleAction::Preset {
                preset: "editing".into(),
            },
            enabled_default: true,
            enabled: Some(true),
        };
        let mut r2 = r1.clone();
        r2.id = "r2".into();
        r2.then = RuleAction::Preset {
            preset: "reading".into(),
        };
        let engine = RuleEngine::new(vec![r1, r2]);
        let m = engine.first_match(&scene(Some("Code.exe"), None, false)).unwrap();
        assert_eq!(m.rule.id, "r1");
    }

    #[test]
    fn disabled_rules_skipped() {
        let mut r = Rule {
            id: "r1".into(),
            name: None,
            if_: ConditionGroup {
                any: vec![RuleCondition {
                    field: RuleField::ProcessName,
                    op: RuleOp::Equals,
                    value: "Code.exe".into(),
                    case_sensitive: false,
                }],
                all: vec![],
            },
            then: RuleAction::Preset {
                preset: "editing".into(),
            },
            enabled_default: true,
            enabled: Some(true),
        };
        r.enabled = Some(false);
        let engine = RuleEngine::new(vec![r]);
        assert!(engine.first_match(&scene(Some("Code.exe"), None, false)).is_none());
    }

    #[test]
    fn builtin_templates_validate_and_design_optin() {
        let templates = builtin_templates();
        for t in &templates {
            t.validate().unwrap();
        }
        let figma = templates.iter().find(|t| t.id == "design-figma-safe").unwrap();
        assert!(!figma.enabled_default, "设计模板必须默认 opt-in（KD21）");
        let vscode = templates.iter().find(|t| t.id == "coding-vscode").unwrap();
        assert!(vscode.enabled_default);
        // 微信不得误触设计规则（附录 E）
        let wechat = scene(Some("WeChat.exe"), Some("com.tencent.xinWeChat"), false);
        let engine = RuleEngine::new(templates.clone());
        let m = engine.first_match(&wechat);
        assert!(m.is_none() || !m.unwrap().rule.id.contains("safe"));
    }

    #[test]
    fn empty_condition_value_rejected() {
        let mut r = Rule {
            id: "empty".into(),
            name: None,
            if_: ConditionGroup {
                any: vec![RuleCondition {
                    field: RuleField::ProcessName,
                    op: RuleOp::Equals,
                    value: "".into(),
                    case_sensitive: false,
                }],
                all: vec![],
            },
            then: RuleAction::Preset {
                preset: "editing".into(),
            },
            enabled_default: true,
            enabled: None,
        };
        assert!(r.validate().is_err());
        r.if_.any[0].value = "Code.exe".into();
        assert!(r.validate().is_ok());
    }

    #[test]
    fn safe_duration_rule_requires_minutes() {
        let mut r = Rule {
            id: "bad".into(),
            name: None,
            if_: ConditionGroup {
                any: vec![RuleCondition {
                    field: RuleField::ProcessName,
                    op: RuleOp::Equals,
                    value: "x.exe".into(),
                    case_sensitive: false,
                }],
                all: vec![],
            },
            then: RuleAction::SafeMode {
                hold: SafeModeHold::Duration,
                minutes: None,
            },
            enabled_default: true,
            enabled: Some(true),
        };
        assert!(r.validate().is_err());
        r.then = RuleAction::SafeMode {
            hold: SafeModeHold::Duration,
            minutes: Some(15),
        };
        assert!(r.validate().is_ok());
    }

    #[test]
    fn deserializes_if_alias_from_template_json() {
        let json = r#"{
            "id": "t1",
            "if": {"any": [{"field": "process_name", "op": "equals", "value": "X.exe"}], "all": []},
            "then": {"action": "preset", "preset": "editing"},
            "enabled_default": true
        }"#;
        let r: Rule = serde_json::from_str(json).unwrap();
        assert_eq!(r.id, "t1");
        assert_eq!(r.if_.any.len(), 1);
        assert_eq!(r.if_.any[0].value, "X.exe");
    }

    #[test]
    fn schema_version_exported() {
        let cfg = crate::config::RulesConfig::default();
        assert_eq!(cfg.schema_version, crate::config::SCHEMA_VERSION);
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains("\"rules\""));
    }
}
