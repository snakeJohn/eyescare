//! SafeModeController（PR-A08）：滤镜旁路状态机（设计 §4.5）。
//!
//! 硬性规则：
//! 1. 禁止 `restore → 同帧 rule 再 enter` 振荡：`while_match` 电平保持；
//!    `duration` 到期后进入 `cooldown_until_app_change`（防抖键 `last_rule_safe_key`）。
//! 2. 用户手动进入抢占规则旁路（UserTriggered 覆盖 RuleTriggered）；
//!    用户退出后若 while_match 仍成立 → 边沿重新武装可再 RuleOn
//!    （默认抑制 30s：`suppress_after_user_exit_sec`）。
//! 3. 旁路期间 Scene/Rule 不得 apply_preset；可更新「本将命中」调试状态。
//! 4. 退出旁路：由 DisplayService.recompute() 统一处理（P2–P5 重新解析）。
//! 5. P1 旁路声明由 SafeModeController 持有；规则只是触发器。

use std::time::{Duration, Instant};

use crate::config::SafeModeConfig;
use crate::display::Claim;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SafeState {
    Off,
    UserOn,
    RuleOn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SafeSource {
    User,
    Rule,
}

/// hold 类型复用 config 定义（serde + 状态机共用，避免双枚举漂移）。
pub use crate::config::SafeModeHold as SafeHold;

/// 状态机状态（可持久化视图）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SafeModeStatus {
    pub state: SafeState,
    pub source: Option<SafeSource>,
    pub hold: Option<SafeHold>,
    pub rule_id: Option<String>,
    /// duration 模式剩余秒数（UserOn/RuleOn duration 时）。
    pub remaining_sec: Option<u64>,
    /// 旁路期间「本将命中」的规则（调试状态，不触发动作）。
    pub would_match_rule: Option<String>,
}

/// SafeModeController。纯逻辑 + 时钟注入，线程安全。
pub struct SafeModeController {
    config: SafeModeConfig,
    state: SafeState,
    source: Option<SafeSource>,
    hold: Option<SafeHold>,
    rule_id: Option<String>,
    entered_at: Option<Instant>,
    duration_until: Option<Instant>,
    /// 防抖键：duration 到期后，直到前台 app_id 变化才允许该规则再次触发。
    last_rule_safe_key: Option<String>,
    /// 用户退出后抑制规则重入的截止时间。
    suppress_rule_until: Option<Instant>,
    /// 旁路期间调试状态。
    would_match_rule: Option<String>,
}

impl SafeModeController {
    pub fn new(config: SafeModeConfig) -> Self {
        Self {
            config,
            state: SafeState::Off,
            source: None,
            hold: None,
            rule_id: None,
            entered_at: None,
            duration_until: None,
            last_rule_safe_key: None,
            suppress_rule_until: None,
            would_match_rule: None,
        }
    }

    pub fn status(&self) -> SafeModeStatus {
        let remaining_sec = self.duration_until.map(|d| {
            d.saturating_duration_since(Instant::now())
                .as_secs()
        });
        SafeModeStatus {
            state: self.state,
            source: self.source,
            hold: self.hold,
            rule_id: self.rule_id.clone(),
            remaining_sec,
            would_match_rule: self.would_match_rule.clone(),
        }
    }

    pub fn is_active(&self) -> bool {
        self.state != SafeState::Off
    }

    /// P1 claim（对所有活动屏广播）。
    pub fn p1_claims(&self, display_ids: &[String]) -> Vec<(String, Claim)> {
        if !self.is_active() {
            return vec![];
        }
        display_ids
            .iter()
            .map(|id| (id.clone(), Claim::safe_bypass()))
            .collect()
    }

    // ---------- 用户路径 ----------

    /// 用户手动进入旁路（托盘/快捷键）。
    pub fn user_enter(&mut self) {
        let minutes = self.config.default_minutes.max(1) as u64;
        self.enter(SafeSource::User, SafeHold::Duration, None, minutes);
    }

    /// 用户延长（UserOn 时）。
    pub fn user_extend(&mut self, minutes: u64) {
        if self.state == SafeState::UserOn {
            let minutes = minutes.max(1);
            self.duration_until = Some(Instant::now() + Duration::from_secs(minutes * 60));
        }
    }

    /// 用户手动退出。若 while_match 规则仍匹配，边沿重新武装（带抑制窗口）。
    pub fn user_exit(&mut self) {
        if self.state == SafeState::Off {
            return;
        }
        self.leave();
        let suppress = self.config.suppress_after_user_exit_sec.max(1) as u64;
        self.suppress_rule_until = Some(Instant::now() + Duration::from_secs(suppress));
    }

    // ---------- 规则路径 ----------

    /// 规则评估（RuleEngine first_match 结果）。`hold` 来自规则动作；
    /// `scene_app_key` 为当前前台身份；`minutes` 仅 Duration 生效。
    /// 返回 true 表示状态变化。
    pub fn rule_eval(
        &mut self,
        rule_id: Option<&str>,
        hold: SafeHold,
        scene_app_key: Option<&str>,
        minutes: u64,
    ) -> bool {
        // 调试状态：旁路期间记录"本将命中"
        self.would_match_rule = rule_id.map(String::from);
        if self.is_active() {
            return false; // 旁路期间不触发动作（硬性规则 3）
        }

        let Some(rule_id) = rule_id else {
            return false;
        };
        if self.state != SafeState::Off {
            return false;
        }

        // duration 防抖只拦 duration 规则（硬性规则 1：到期后 cooldown_until_app_change）。
        // while_match 电平保持，无振荡问题，不受防抖键约束。
        if hold == SafeHold::Duration {
            if let Some(last_key) = &self.last_rule_safe_key {
                if scene_app_key.is_some() && scene_app_key == Some(last_key.as_str()) {
                    return false;
                }
            }
        }
        // 用户退出后抑制窗口（硬性规则 2 默认开）
        if let Some(until) = self.suppress_rule_until {
            if Instant::now() < until {
                return false;
            }
            self.suppress_rule_until = None;
        }

        let mins = if hold == SafeHold::Duration {
            minutes.max(1)
        } else {
            0
        };
        self.enter(SafeSource::Rule, hold, Some(rule_id.to_string()), mins);
        true
    }

    /// 场景变化：while_match 规则旁路在匹配条件不再成立时退出。
    /// 返回 true 表示状态变化。
    pub fn scene_changed(&mut self, rule_still_matches: bool, app_key_changed: bool) -> bool {
        // 更新防抖键的 app 变化基准
        if app_key_changed {
            // app 变化后，duration 防抖解除
        }
        if self.state == SafeState::RuleOn
            && self.hold == Some(SafeHold::WhileMatch)
            && !rule_still_matches
        {
            self.leave();
            return true;
        }
        false
    }

    /// 周期性 tick：duration 到期退出；检查抑制窗口过期。
    pub fn tick(&mut self) -> bool {
        let now = Instant::now();
        if let Some(until) = self.suppress_rule_until {
            if now >= until {
                self.suppress_rule_until = None;
            }
        }
        if self.state == SafeState::Off {
            return false;
        }
        if self.hold == Some(SafeHold::Duration) {
            if let Some(until) = self.duration_until {
                if now >= until {
                    self.leave();
                    return true;
                }
            }
        }
        false
    }

    // ---------- 内部 ----------

    fn enter(&mut self, source: SafeSource, hold: SafeHold, rule_id: Option<String>, minutes: u64) {
        self.state = match source {
            SafeSource::User => SafeState::UserOn,
            SafeSource::Rule => SafeState::RuleOn,
        };
        self.source = Some(source);
        self.hold = Some(hold);
        self.rule_id = rule_id;
        self.entered_at = Some(Instant::now());
        self.duration_until = if hold == SafeHold::Duration && minutes > 0 {
            Some(Instant::now() + Duration::from_secs(minutes * 60))
        } else {
            None
        };
        // duration 防抖键：进入时绝不删除（设计硬性规则 1：到期后 cooldown_until_app_change）。
        // while_match 模式电平保持、无振荡风险，进入时清除残留的 duration 防抖键，
        // 避免被旧的 duration 键误拦（窗口切换后规则重新匹配应放行）。
        if hold == SafeHold::WhileMatch {
            self.last_rule_safe_key = None;
        }
    }

    fn leave(&mut self) {
        self.state = SafeState::Off;
        self.source = None;
        self.hold = None;
        self.rule_id = None;
        self.entered_at = None;
        self.duration_until = None;
    }

    /// duration 到期退出时设置防抖键（需当前前台 app key）。
    pub fn set_cooldown_key(&mut self, scene_app_key: Option<String>) {
        if self.state == SafeState::Off && self.hold.is_none() {
            self.last_rule_safe_key = scene_app_key;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> SafeModeConfig {
        SafeModeConfig {
            active: false,
            source: None,
            hold: None,
            default_minutes: 15,
            suppress_rule_reenter_sec: 30,
            suppress_after_user_exit_sec: 30,
        }
    }

    #[test]
    fn user_enter_exit_cycle() {
        let mut sm = SafeModeController::new(cfg());
        assert!(!sm.is_active());
        sm.user_enter();
        assert_eq!(sm.status().state, SafeState::UserOn);
        assert_eq!(sm.status().source, Some(SafeSource::User));
        assert!(sm.status().remaining_sec.unwrap() > 0);
        sm.user_exit();
        assert_eq!(sm.status().state, SafeState::Off);
    }

    #[test]
    fn user_extend() {
        let mut sm = SafeModeController::new(cfg());
        sm.user_enter();
        let before = sm.status().remaining_sec.unwrap();
        sm.user_extend(60);
        let after = sm.status().remaining_sec.unwrap();
        assert!(after > before);
    }

    #[test]
    fn rule_while_match_enters_and_exits() {
        let mut sm = SafeModeController::new(cfg());
        // 规则进入
        assert!(sm.rule_eval(Some("figma-safe"), SafeHold::WhileMatch, Some("key-a"), 0));
        assert_eq!(sm.status().state, SafeState::RuleOn);
        assert_eq!(sm.status().rule_id.as_deref(), Some("figma-safe"));
        // 电平保持：同一规则仍匹配 → 不抖动（无变化）
        assert!(!sm.rule_eval(Some("figma-safe"), SafeHold::WhileMatch, Some("key-a"), 0));
        // 条件消失 → 退出
        assert!(sm.scene_changed(false, false));
        assert_eq!(sm.status().state, SafeState::Off);
    }

    #[test]
    fn no_rule_no_action() {
        let mut sm = SafeModeController::new(cfg());
        assert!(!sm.rule_eval(None, SafeHold::WhileMatch, Some("key-a"), 0));
        assert_eq!(sm.status().state, SafeState::Off);
    }

    #[test]
    fn duration_rule_expires_and_cooldown_blocks_reentry() {
        let mut sm = SafeModeController::new(cfg());
        // 用 while_match 进入，然后强制模拟 duration 到期
        assert!(sm.rule_eval(Some("r1"), SafeHold::Duration, Some("key-a"), 15));
        // 手动把它转成 duration 到期场景
        sm.leave();
        sm.set_cooldown_key(Some("key-a".into()));
        // 同一 app 下立刻再触发 → 被防抖拒绝
        assert!(!sm.rule_eval(Some("r1"), SafeHold::Duration, Some("key-a"), 15));
        // app 变化后可再次触发
        assert!(sm.rule_eval(Some("r1"), SafeHold::Duration, Some("key-b"), 15));
    }

    #[test]
    fn duration_cooldown_survives_user_enter() {
        let mut sm = SafeModeController::new(cfg());
        // duration 到期：设置防抖键
        sm.leave();
        sm.set_cooldown_key(Some("key-a".into()));
        // 用户手动进入不破坏防抖键
        sm.user_enter();
        sm.user_exit();
        // 同 app 规则仍被拦
        assert!(!sm.rule_eval(Some("r1"), SafeHold::Duration, Some("key-a"), 15));
    }

    #[test]
    fn while_match_clears_stale_duration_key() {
        let mut sm = SafeModeController::new(cfg());
        // 残留 duration 防抖键（appX 到期）
        sm.set_cooldown_key(Some("appX".into()));
        // while_match 规则在 appX 重新匹配 → 应放行（电平保持语义）
        assert!(sm.rule_eval(Some("figma-safe"), SafeHold::WhileMatch, Some("appX"), 0));
        assert_eq!(sm.status().state, SafeState::RuleOn);
    }

    #[test]
    fn user_exit_suppresses_rule_reentry() {
        let mut sm = SafeModeController::new(cfg());
        sm.user_enter();
        sm.user_exit();
        // 抑制窗口内规则不得进入
        assert!(!sm.rule_eval(Some("figma-safe"), SafeHold::WhileMatch, Some("key-a"), 0));
        // 抑制窗口过期后可以（用 tick 推进：无法快进 Instant，改为直接验证窗口逻辑）
        sm.suppress_rule_until = None;
        assert!(sm.rule_eval(Some("figma-safe"), SafeHold::WhileMatch, Some("key-a"), 0));
    }

    #[test]
    fn user_overrides_rule() {
        let mut sm = SafeModeController::new(cfg());
        assert!(sm.rule_eval(Some("r1"), SafeHold::Duration, Some("key-a"), 15));
        assert_eq!(sm.status().source, Some(SafeSource::Rule));
        // 用户手动进入 → 抢占为 UserOn
        sm.user_enter();
        assert_eq!(sm.status().state, SafeState::UserOn);
        assert_eq!(sm.status().source, Some(SafeSource::User));
    }

    #[test]
    fn p1_claims_only_when_active() {
        let mut sm = SafeModeController::new(cfg());
        let ids = vec!["D1".to_string(), "D2".to_string()];
        assert!(sm.p1_claims(&ids).is_empty());
        sm.user_enter();
        let claims = sm.p1_claims(&ids);
        assert_eq!(claims.len(), 2);
        assert!(claims.iter().all(|(_, c)| c.priority == crate::display::Priority::P1SafeBypass));
    }

    #[test]
    fn would_match_debug_state() {
        let mut sm = SafeModeController::new(cfg());
        sm.user_enter();
        // 旁路期间规则命中只记录调试状态，不触发
        assert!(!sm.rule_eval(Some("figma-safe"), SafeHold::WhileMatch, Some("key-a"), 0));
        assert_eq!(sm.status().would_match_rule.as_deref(), Some("figma-safe"));
        assert_eq!(sm.status().state, SafeState::UserOn);
    }

    #[test]
    fn duration_rule_uses_minutes() {
        let mut sm = SafeModeController::new(cfg());
        assert!(sm.rule_eval(Some("r1"), SafeHold::Duration, Some("key-a"), 20));
        let remaining = sm.status().remaining_sec.unwrap();
        assert!(remaining > 19 * 60 && remaining <= 20 * 60, "remaining={remaining}");
    }

    #[test]
    fn status_serializes_snake_case() {
        let mut sm = SafeModeController::new(cfg());
        sm.user_enter();
        let json = serde_json::to_value(sm.status()).unwrap();
        assert_eq!(json["state"], "user_on");
        assert_eq!(json["source"], "user");
        assert_eq!(json["hold"], "duration");
    }

    #[test]
    fn duration_tick_exits() {
        let mut sm = SafeModeController::new(cfg());
        sm.user_enter();
        sm.duration_until = Some(Instant::now() - Duration::from_secs(1));
        assert!(sm.tick());
        assert_eq!(sm.status().state, SafeState::Off);
    }
}
