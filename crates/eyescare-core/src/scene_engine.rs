//! SceneEngine（PR-A09 简版）：场景采样 → 规则决策（设计 §5）。
//!
//! 本模块是纯函数决策层：输入 `RuleEngine + SceneSnapshot`，输出动作意图
//! （`SceneDecision`）。执行动作（SafeModeController / DisplayService claims /
//! TimerService breaks policy）由壳层主循环负责，保证本模块可单测。
//!
//! 场景采样频率 750ms（设计 §技术分叉：轮询 MVP）；决策节流由壳层做。

use crate::rules::{BreaksAction, FilterAction, RuleAction, RuleEngine, RuleMatch};
use crate::scene::SceneSnapshot;

/// 单次决策输出。
#[derive(Debug, Clone, PartialEq)]
pub struct SceneDecision {
    /// 命中的规则（含规则本体）。
    pub matched: Option<RuleMatch>,
    /// 前台 app_key（场景身份，用于 SafeMode 防抖）。
    pub app_key: Option<String>,
    /// 全屏策略：休息行为（默认 Keep）。
    pub breaks_policy: BreaksPolicy,
    /// 全屏策略：滤镜是否暂停（= identity ramp 来源）。
    pub filter_paused: bool,
    /// 规则预设（rule_id, preset）——P3 claim 输入。
    pub rule_preset: Option<(String, String)>,
    /// 规则触发的旁路（rule_id）——SafeMode 规则联动输入。
    pub rule_safe_mode: Option<String>,
    /// 场景是否变化（app_key 或全屏状态变化），壳层据此决定是否 recompute。
    pub scene_changed: bool,
}

/// 全屏策略下的休息行为（与 timer::BreaksPolicy 同构，避免 core 内部循环依赖）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreaksPolicy {
    Keep,
    Pause,
    NotifyOnly,
}

impl From<BreaksAction> for BreaksPolicy {
    fn from(b: BreaksAction) -> Self {
        match b {
            BreaksAction::Keep => BreaksPolicy::Keep,
            BreaksAction::Pause => BreaksPolicy::Pause,
            BreaksAction::NotifyOnly => BreaksPolicy::NotifyOnly,
        }
    }
}

impl SceneDecision {
    fn empty(app_key: Option<String>, scene_changed: bool) -> Self {
        Self {
            matched: None,
            app_key,
            breaks_policy: BreaksPolicy::Keep,
            filter_paused: false,
            rule_preset: None,
            rule_safe_mode: None,
            scene_changed,
        }
    }
}

/// 决策入口：对一次场景采样输出动作意图。
/// `prev_app_key` 用于 scene_changed 判断（壳层维护最近一次 key）。
pub fn decide(
    engine: &RuleEngine,
    scene: Option<&SceneSnapshot>,
    prev_app_key: &Option<String>,
) -> SceneDecision {
    let app_key = scene.and_then(|s| s.app_key());
    let scene_changed = *prev_app_key != app_key;
    let mut d = SceneDecision::empty(app_key.clone(), scene_changed);

    let Some(scene) = scene else {
        // 无前台身份（桌面/锁屏）：无规则可匹配
        return d;
    };

    let Some(m) = engine.first_match(scene) else {
        return d;
    };
    d.matched = Some(m.clone());

    match &m.rule.then {
        RuleAction::SafeMode { .. } => {
            d.rule_safe_mode = Some(m.rule.id.clone());
        }
        RuleAction::Preset { preset } => {
            d.rule_preset = Some((m.rule.id.clone(), preset.clone()));
        }
        RuleAction::FullscreenPolicy { filter, breaks } => {
            match filter {
                FilterAction::Keep => {}
                FilterAction::Pause | FilterAction::GamingPreset => {
                    d.filter_paused = true;
                }
            }
            d.breaks_policy = (*breaks).into();
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{builtin_templates, ConditionGroup, RuleCondition, RuleField, RuleOp};
    use eyescare_platform::FullscreenConfidence;

    fn scene(process: Option<&str>, fullscreen: bool) -> SceneSnapshot {
        SceneSnapshot {
            process_name: process.map(String::from),
            path_suffix: None,
            bundle_id: None,
            app_display_name: process.unwrap_or("X").to_string(),
            window_title: None,
            is_fullscreen: fullscreen,
            fullscreen_confidence: if fullscreen {
                FullscreenConfidence::High
            } else {
                FullscreenConfidence::Unknown
            },
            display_id: None,
        }
    }

    fn engine_with(rules: Vec<crate::rules::Rule>) -> RuleEngine {
        RuleEngine::new(rules)
    }

    fn safe_rule(id: &str, process: &str) -> crate::rules::Rule {
        crate::rules::Rule {
            id: id.into(),
            name: None,
            if_: ConditionGroup {
                any: vec![RuleCondition {
                    field: RuleField::ProcessName,
                    op: RuleOp::Equals,
                    value: process.into(),
                    case_sensitive: false,
                }],
                all: vec![],
            },
            then: RuleAction::SafeMode {
                hold: crate::config::SafeModeHold::WhileMatch,
                minutes: None,
            },
            enabled_default: false,
            enabled: Some(true),
        }
    }

    #[test]
    fn no_identity_no_rule() {
        let engine = engine_with(vec![safe_rule("r1", "Figma.exe")]);
        let d = decide(&engine, None, &None);
        assert!(d.matched.is_none());
        assert!(d.rule_safe_mode.is_none());
        assert_eq!(d.breaks_policy, BreaksPolicy::Keep);
    }

    #[test]
    fn safe_mode_rule_fires() {
        let engine = engine_with(vec![safe_rule("figma-safe", "Figma.exe")]);
        let d = decide(&engine, Some(&scene(Some("Figma.exe"), false)), &None);
        assert_eq!(d.rule_safe_mode.as_deref(), Some("figma-safe"));
        assert!(d.scene_changed);
        // 同 app 再采样：scene_changed=false
        let d2 = decide(&engine, Some(&scene(Some("Figma.exe"), false)), &d.app_key);
        assert!(!d2.scene_changed);
        // 规则仍命中：rule_safe_mode 保持（电平保持，由 SafeMode 防抖处理）
        assert_eq!(d2.rule_safe_mode.as_deref(), Some("figma-safe"));
    }

    #[test]
    fn gaming_fullscreen_policy() {
        let engine = engine_with(builtin_templates());
        // 全屏 → gaming 模板（filter pause + breaks notify_only）
        let d = decide(&engine, Some(&scene(Some("game.exe"), true)), &None);
        assert!(d.filter_paused);
        assert_eq!(d.breaks_policy, BreaksPolicy::NotifyOnly);
        // 非全屏 → 无策略
        let d2 = decide(&engine, Some(&scene(Some("game.exe"), false)), &None);
        assert!(!d2.filter_paused);
        assert_eq!(d2.breaks_policy, BreaksPolicy::Keep);
    }

    #[test]
    fn preset_rule_produces_p3() {
        let engine = engine_with(builtin_templates());
        // VS Code → editing 预设
        let d = decide(&engine, Some(&scene(Some("Code.exe"), false)), &None);
        assert_eq!(d.rule_preset.as_ref().map(|(_, p)| p.as_str()), Some("editing"));
        assert!(!d.filter_paused);
    }

    #[test]
    fn first_match_wins_in_scene() {
        // 设计旁路规则排在编码规则前 → Figma 命中旁路
        let mut figma = safe_rule("design-figma-safe", "Figma.exe");
        figma.enabled = Some(true);
        let mut coding = safe_rule("coding-vscode", "Code.exe");
        coding.then = RuleAction::Preset {
            preset: "editing".into(),
        };
        coding.enabled = Some(true);
        let engine = engine_with(vec![figma, coding]);
        let d = decide(&engine, Some(&scene(Some("Figma.exe"), false)), &None);
        assert_eq!(d.rule_safe_mode.as_deref(), Some("design-figma-safe"));
    }
}
