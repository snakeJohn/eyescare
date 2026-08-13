//! DisplayTarget Resolver（PR-A06）：P0–P5 claim 优先级解析（设计 §4.6）。
//!
//! 每个 DisplayId 独立计算 `Vec<Claim>`，取最高优先级未跳过的 claim。
//! Sync 模式用主策略源广播；PerDisplay 支持每屏覆盖。
//!
//! 优先级表：
//! - P0 EmergencyRestore（崩溃/退出/用户「恢复显示」）
//! - P1 SafeModeBypass（滤镜旁路）
//! - P2 UserManualLock（用户拖动锁定）
//! - P3 RulePreset（规则预设，非 bypass）
//! - P4 DayNight（MVP）
//! - P5 DefaultPreset

use eyescare_platform::Ramp;

use crate::display::math::build_ramp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    P0Emergency,
    P1SafeBypass,
    P2UserLock,
    P3RulePreset,
    P4DayNight,
    P5Default,
}

/// 一次显示意图声明。
#[derive(Debug, Clone)]
pub struct Claim {
    pub priority: Priority,
    /// 语义来源（Insights source）。
    pub source: String,
    /// 参数来源标识（如 rule_id / "user" / "daynight"），诊断用。
    pub owner: String,
    /// 目标色温/亮度（None = 不覆盖该维度）。
    pub kelvin: Option<u32>,
    pub brightness: Option<f64>,
    /// 该屏是否跳过（HDR 屏由 backend 决策，此处保留字段供规则跳过）。
    pub skipped: bool,
}

impl Claim {
    pub fn emergency() -> Self {
        Self {
            priority: Priority::P0Emergency,
            source: "emergency_restore".into(),
            owner: "emergency".into(),
            kelvin: None,
            brightness: None,
            skipped: false,
        }
    }

    pub fn safe_bypass() -> Self {
        Self {
            priority: Priority::P1SafeBypass,
            source: "safe_bypass".into(),
            owner: "safe_mode".into(),
            kelvin: None,
            brightness: None,
            skipped: false,
        }
    }

    pub fn user_lock(kelvin: u32, brightness: f64) -> Self {
        Self {
            priority: Priority::P2UserLock,
            source: "user".into(),
            owner: "user_lock".into(),
            kelvin: Some(kelvin),
            brightness: Some(brightness),
            skipped: false,
        }
    }

    pub fn rule_preset(rule_id: &str, kelvin: u32, brightness: f64) -> Self {
        Self {
            priority: Priority::P3RulePreset,
            source: "rule".into(),
            owner: rule_id.into(),
            kelvin: Some(kelvin),
            brightness: Some(brightness),
            skipped: false,
        }
    }

    pub fn day_night(kelvin: u32) -> Self {
        Self {
            priority: Priority::P4DayNight,
            source: "daynight".into(),
            owner: "daynight".into(),
            kelvin: Some(kelvin),
            brightness: None,
            skipped: false,
        }
    }

    pub fn default_preset(kelvin: u32, brightness: f64) -> Self {
        Self {
            priority: Priority::P5Default,
            source: "default_preset".into(),
            owner: "default".into(),
            kelvin: Some(kelvin),
            brightness: Some(brightness),
            skipped: false,
        }
    }
}

/// 单屏解析结果。
#[derive(Debug, Clone)]
pub struct ResolvedTarget {
    pub display_id: String,
    pub claim: Option<Claim>,
    /// 是否为 identity/restore（P0/P1 时 true）。
    pub is_identity: bool,
}

impl ResolvedTarget {
    /// 生成目标 ramp。P0/P1 → identity（restore 由后端快照完成，此处语义为 identity）。
    pub fn ramp(&self) -> Ramp {
        match &self.claim {
            None => Ramp::identity(),
            Some(c) if c.kelvin.is_none() && c.brightness.is_none() => Ramp::identity(),
            Some(c) => {
                let k = c.kelvin.unwrap_or(4500);
                let b = c.brightness.unwrap_or(0.85);
                build_ramp(k, b, 0.35)
            }
        }
    }
}

/// Resolver：输入 claims 集合，输出每屏目标。
#[derive(Debug, Clone, Default)]
pub struct Resolver {
    /// display_id → claims（已排序由 resolve 完成）。
    claims: std::collections::BTreeMap<String, Vec<Claim>>,
}

impl Resolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// 全量替换 claims（每次 recompute 时由 DisplayService 汇总各来源）。
    pub fn set_claims(&mut self, claims: Vec<(String, Claim)>) {
        let mut map: std::collections::BTreeMap<String, Vec<Claim>> = Default::default();
        for (id, claim) in claims {
            map.entry(id).or_default().push(claim);
        }
        self.claims = map;
    }

    /// 为指定屏解析最高优先级 claim。数值越小优先级越高（P0 > P1 > ... > P5）。
    pub fn resolve(&self, display_id: &str) -> ResolvedTarget {
        let mut active: Vec<&Claim> = Vec::new();
        if let Some(list) = self.claims.get(display_id) {
            for c in list {
                if !c.skipped {
                    active.push(c);
                }
            }
        }
        let mut best: Option<Claim> = None;
        for c in &active {
            match &best {
                None => best = Some((*c).clone()),
                Some(b) if c.priority < b.priority => best = Some((*c).clone()),
                _ => {}
            }
        }
        let is_identity = match &best {
            None => true,
            Some(c) => c.priority <= Priority::P1SafeBypass,
        };
        // P2–P5：未设置的 kelvin/brightness 从更低优先级未跳过 claim 继承。P0/P1 保持 identity。
        if let Some(ref mut winning) = best {
            if winning.priority > Priority::P1SafeBypass {
                let mut lower: Vec<&Claim> = active
                    .iter()
                    .copied()
                    .filter(|c| c.priority > winning.priority)
                    .collect();
                lower.sort_by_key(|c| c.priority);
                for c in lower {
                    if winning.kelvin.is_none() {
                        winning.kelvin = c.kelvin;
                    }
                    if winning.brightness.is_none() {
                        winning.brightness = c.brightness;
                    }
                    if winning.kelvin.is_some() && winning.brightness.is_some() {
                        break;
                    }
                }
            }
        }
        ResolvedTarget {
            display_id: display_id.to_string(),
            claim: best,
            is_identity,
        }
    }

    /// 全屏解析。
    pub fn resolve_all(&self, display_ids: &[String]) -> Vec<ResolvedTarget> {
        display_ids.iter().map(|id| self.resolve(id)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims_for(id: &str, claims: Vec<Claim>) -> Vec<(String, Claim)> {
        claims.into_iter().map(|c| (id.to_string(), c)).collect()
    }

    #[test]
    fn p1_bypass_wins_over_p4_daynight() {
        let mut r = Resolver::new();
        r.set_claims(claims_for(
            "D1",
            vec![Claim::day_night(3400), Claim::safe_bypass()],
        ));
        let t = r.resolve("D1");
        assert!(t.is_identity);
        assert_eq!(t.claim.unwrap().priority, Priority::P1SafeBypass);
    }

    #[test]
    fn p0_emergency_highest() {
        let mut r = Resolver::new();
        r.set_claims(claims_for(
            "D1",
            vec![
                Claim::user_lock(4000, 0.9),
                Claim::safe_bypass(),
                Claim::emergency(),
            ],
        ));
        let t = r.resolve("D1");
        assert_eq!(t.claim.unwrap().priority, Priority::P0Emergency);
    }

    #[test]
    fn user_lock_overrides_rule_and_daynight() {
        let mut r = Resolver::new();
        r.set_claims(claims_for(
            "D1",
            vec![
                Claim::day_night(3400),
                Claim::rule_preset("coding", 6500, 0.95),
                Claim::user_lock(4000, 0.8),
            ],
        ));
        let t = r.resolve("D1");
        let c = t.claim.unwrap();
        assert_eq!(c.priority, Priority::P2UserLock);
        assert_eq!(c.kelvin, Some(4000));
    }

    #[test]
    fn daynight_wins_over_default() {
        let mut r = Resolver::new();
        r.set_claims(claims_for(
            "D1",
            vec![Claim::default_preset(4500, 0.85), Claim::day_night(3400)],
        ));
        let t = r.resolve("D1");
        let c = t.claim.clone().unwrap();
        assert_eq!(c.priority, Priority::P4DayNight);
        assert_eq!(c.kelvin, Some(3400));
    }

    #[test]
    fn daynight_ramp_inherits_default_brightness() {
        // P4 只给色温；亮度从下一档未跳过 claim（P5 0.85）继承，而不是 unwrap_or(1.0)
        let mut r = Resolver::new();
        r.set_claims(claims_for(
            "D1",
            vec![Claim::default_preset(4500, 0.85), Claim::day_night(3400)],
        ));
        let t = r.resolve("D1");
        assert_eq!(t.ramp(), build_ramp(3400, 0.85, 0.35));
        assert_ne!(t.ramp(), build_ramp(3400, 1.0, 0.35));
    }

    #[test]
    fn daynight_inherits_nondefault_brightness_from_lower_claim() {
        let mut r = Resolver::new();
        r.set_claims(claims_for(
            "D1",
            vec![Claim::default_preset(4500, 0.7), Claim::day_night(3400)],
        ));
        let t = r.resolve("D1");
        assert_eq!(t.ramp(), build_ramp(3400, 0.7, 0.35));
    }

    #[test]
    fn unset_brightness_falls_back_to_085() {
        let mut r = Resolver::new();
        r.set_claims(claims_for("D1", vec![Claim::day_night(3400)]));
        let t = r.resolve("D1");
        assert_eq!(t.ramp(), build_ramp(3400, 0.85, 0.35));
    }

    #[test]
    fn p1_does_not_merge_lower_claim_into_ramp() {
        let mut r = Resolver::new();
        r.set_claims(claims_for(
            "D1",
            vec![Claim::safe_bypass(), Claim::default_preset(4500, 0.85)],
        ));
        let t = r.resolve("D1");
        assert!(t.is_identity);
        assert_eq!(t.ramp(), Ramp::identity());
    }

    #[test]
    fn skipped_claim_ignored() {
        let mut c = Claim::user_lock(4000, 0.9);
        c.skipped = true;
        let mut r = Resolver::new();
        r.set_claims(claims_for("D1", vec![c, Claim::day_night(3400)]));
        let t = r.resolve("D1");
        assert_eq!(t.claim.unwrap().priority, Priority::P4DayNight);
    }

    #[test]
    fn no_claims_means_identity() {
        let r = Resolver::new();
        let t = r.resolve("D1");
        assert!(t.is_identity);
        assert!(t.claim.is_none());
    }

    #[test]
    fn per_display_independent() {
        let mut r = Resolver::new();
        r.set_claims(vec![
            ("D1".to_string(), Claim::day_night(3400)),
            ("D2".to_string(), Claim::safe_bypass()),
        ]);
        assert!(!r.resolve("D1").is_identity);
        assert!(r.resolve("D2").is_identity);
        // 未提及的屏
        assert!(r.resolve("D3").is_identity);
    }

    #[test]
    fn resolved_ramp_matches_claim() {
        let mut r = Resolver::new();
        r.set_claims(claims_for("D1", vec![Claim::user_lock(6500, 1.0)]));
        let t = r.resolve("D1");
        let ramp = t.ramp();
        assert!(!ramp.is_identity());
        let id_ramp = Ramp::identity();
        // 6500K / 1.0 接近 identity；blue 通道 Tanner ≈ 0.981 → 总偏差 < 8%
        assert!(ramp.mean_abs_diff(&id_ramp) < 0.08);
    }
}
