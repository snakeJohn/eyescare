//! DisplayService（PR-A06）：编排 Resolver + DayNight + 平台后端。
//!
//! 职责：
//! 1. 汇总各来源 claim（SafeMode P1 / UserLock P2 / Rule P3 / DayNight P4 / Default P5）
//! 2. recompute → Resolver → per-display apply（readback 校验）
//! 3. 过渡动画：lerp + 12Hz 限频；P1 进入立即 restore（无动画）
//! 4. 紧急恢复：restore_all + panic hook best-effort（设计 §4.6）
//!
//! 平台后端通过 `DisplayBackend` trait 注入，单测用 MockBackend。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use eyescare_platform::{ApplyOutcome, ApplyReport, DisplayBackend, DisplayId, Ramp};

use super::math::lerp_ramp;
use super::resolver::{Claim, Resolver};

#[derive(Debug, thiserror::Error)]
pub enum DisplayServiceError {
    #[error("backend error: {0}")]
    Backend(#[from] eyescare_platform::Error),
    #[error("no displays enumerated")]
    NoDisplays,
}

/// 单屏应用结果聚合。
#[derive(Debug, Clone, Default)]
pub struct ApplySummary {
    pub applied: Vec<ApplyReport>,
    pub rejected: Vec<ApplyReport>,
    pub hdr_skipped: Vec<ApplyReport>,
    pub gone: Vec<ApplyReport>,
}

impl ApplySummary {
    pub fn any_failure(&self) -> bool {
        !self.rejected.is_empty()
    }
}

/// 显示服务。线程安全（内部互斥），可被 Tauri 命令与后台线程共享。
pub struct DisplayService {
    backend: Arc<dyn DisplayBackend>,
    /// 各来源注册的 claims：source_key -> Vec<(display_id, Claim)>
    sources: Mutex<BTreeMap<String, Vec<(String, Claim)>>>,
    /// 显示器枚举缓存（首次/recompute 时惰性填充；rebind 时刷新）。
    /// 避免每次 recompute 都走 backend 枚举 + HDR 检测（性能，热插拔低频）。
    displays: Mutex<Option<Vec<eyescare_platform::DisplayInfo>>>,
    /// 当前实际 ramp（过渡动画起点）。
    current: Mutex<BTreeMap<String, Ramp>>,
    /// 上一次动画目标（判断是否变化）。
    target: Mutex<BTreeMap<String, Ramp>>,
    /// 动画进行中（需要多次 pump）。
    animating: Mutex<BTreeMap<String, (Ramp, Ramp, f64)>>, // (from, to, progress)
    /// 过渡时长 ms（P1 退出/常规微调）。
    transition_ms: u64,
}

impl DisplayService {
    pub fn new(backend: Arc<dyn DisplayBackend>) -> Self {
        Self {
            backend,
            sources: Mutex::new(BTreeMap::new()),
            displays: Mutex::new(None),
            current: Mutex::new(BTreeMap::new()),
            target: Mutex::new(BTreeMap::new()),
            animating: Mutex::new(BTreeMap::new()),
            transition_ms: 500,
        }
    }

    /// 强制刷新显示器枚举缓存（启动、WM_DISPLAYCHANGE、rebind 后调用）。
    pub fn refresh_displays(&self) -> Result<(), DisplayServiceError> {
        let displays = self.backend.list_displays()?;
        *self.displays.lock().unwrap() = Some(displays);
        Ok(())
    }

    /// 当前已知显示器 ID 列表（缓存；空则先枚举一次）。
    /// double-check：缓存命中直接返回；未命中先释放锁再枚举（不持锁做 backend IO）。
    pub fn known_displays(&self) -> Vec<DisplayId> {
        {
            let cache = self.displays.lock().unwrap();
            if let Some(d) = cache.as_ref() {
                return d.iter().map(|x| x.id.clone()).collect();
            }
        }
        if let Ok(d) = self.backend.list_displays() {
            let ids: Vec<DisplayId> = d.iter().map(|x| x.id.clone()).collect();
            *self.displays.lock().unwrap() = Some(d);
            return ids;
        }
        Vec::new()
    }

    /// 某来源注册/替换其全部 claims（如 SafeMode 只注册 P1 claim）。
    pub fn set_source(&self, source_key: &str, claims: Vec<(String, Claim)>) {
        self.sources.lock().unwrap().insert(source_key.into(), claims);
    }

    pub fn remove_source(&self, source_key: &str) {
        self.sources.lock().unwrap().remove(source_key);
    }

    /// 全量重算并应用。返回 apply 摘要（含 readback 失败）。
    pub fn recompute(&self) -> Result<ApplySummary, DisplayServiceError> {
        // 枚举走缓存（首次惰性填充），避免每场景变化都查 backend + HDR
        if self.displays.lock().unwrap().is_none() {
            self.refresh_displays()?;
        }
        let displays = self.displays.lock().unwrap().clone().unwrap_or_default();
        if displays.is_empty() {
            return Err(DisplayServiceError::NoDisplays);
        }
        let ids: Vec<String> = displays.iter().map(|d| d.id.0.clone()).collect();

        let mut all: Vec<(String, Claim)> = Vec::new();
        for claims in self.sources.lock().unwrap().values() {
            all.extend(claims.iter().cloned());
        }
        let mut resolver = Resolver::new();
        resolver.set_claims(all);
        let targets = resolver.resolve_all(&ids);

        let mut summary = ApplySummary::default();
        let mut next_current = self.current.lock().unwrap().clone();

        for t in &targets {
            let target_ramp = t.ramp();
            let prev_target = self.target.lock().unwrap().get(&t.display_id).cloned();

            // 目标未变化且未在动画中 → 跳过（省 apply）
            let unchanged = prev_target.as_ref().map(|p| *p == target_ramp).unwrap_or(false);
            let in_anim = self.animating.lock().unwrap().contains_key(&t.display_id);

            if unchanged && !in_anim {
                continue;
            }

            let from = self
                .current
                .lock()
                .unwrap()
                .get(&t.display_id)
                .cloned()
                .unwrap_or_else(Ramp::identity);

            // P1 进入 / 紧急恢复（identity 目标）：立即应用，无动画（设计 §4.6）
            // 首次出现：直接应用
            // 其余（旁路退出、同优先级微调、优先级切换）：从当前实际插向新目标
            let apply_now = if t.is_identity || prev_target.is_none() {
                Some(target_ramp.clone())
            } else {
                self.animating.lock().unwrap().insert(
                    t.display_id.clone(),
                    (from, target_ramp.clone(), 0.0),
                );
                None
            };

            if let Some(ramp) = apply_now {
                let report = self.apply_one(&t.display_id, &ramp)?;
                self.target.lock().unwrap().insert(t.display_id.clone(), ramp.clone());
                next_current.insert(t.display_id.clone(), ramp.clone());
                self.animating.lock().unwrap().remove(&t.display_id);
                self.classify(&mut summary, report);
            }
        }

        *self.current.lock().unwrap() = next_current;
        Ok(summary)
    }

    /// 是否有未完成的过渡动画（壳层据此跳过空 pump，少一次锁+克隆）。
    pub fn is_animating(&self) -> bool {
        !self.animating.lock().unwrap().is_empty()
    }

    /// 热插拔后重绑输出并刷新枚举缓存。
    pub fn rebind_and_refresh(&self) -> Result<(), DisplayServiceError> {
        self.backend.rebind_outputs()?;
        self.refresh_displays()
    }

    /// 是否跳过 HDR 屏的 gamma（默认跳过；Force 策略关闭此开关）。
    pub fn set_hdr_skip(&self, skip: bool) {
        self.backend.set_hdr_skip(skip);
    }

    /// 动画泵：壳层 1s tick 调用一次。`dt_ms` 按 1000ms 计，500ms 过渡在一拍内完成。
    /// （旧实现按 12Hz 步进，但主循环是 1Hz，500ms 过渡会被拉成约 6 秒。）
    pub fn pump(&self) -> Result<ApplySummary, DisplayServiceError> {
        self.pump_dt(1000.0)
    }

    pub fn pump_dt(&self, dt_ms: f64) -> Result<ApplySummary, DisplayServiceError> {
        let mut summary = ApplySummary::default();
        let mut finished: Vec<String> = Vec::new();

        let anims = self.animating.lock().unwrap().clone();
        for (id, (from, to, progress)) in anims {
            let step = dt_ms.max(1.0);
            let new_progress = progress + step / self.transition_ms.max(1) as f64;
            let ramp = if new_progress >= 1.0 {
                to.clone()
            } else {
                lerp_ramp(&from, &to, new_progress)
            };
            let report = self.apply_one(&id, &ramp)?;
            self.target.lock().unwrap().insert(id.clone(), ramp.clone());
            self.current.lock().unwrap().insert(id.clone(), ramp.clone());
            if new_progress >= 1.0 {
                finished.push(id.clone());
            } else {
                self.animating.lock().unwrap().insert(id.clone(), (from, to, new_progress));
            }
            self.classify(&mut summary, report);
        }
        for id in finished {
            self.animating.lock().unwrap().remove(&id);
        }
        Ok(summary)
    }

    fn apply_one(&self, id: &str, ramp: &Ramp) -> Result<ApplyReport, DisplayServiceError> {
        // HDR 屏由 backend 返回 HdrSkipped；后端负责策略
        let report = self
            .backend
            .apply_ramp(&DisplayId(id.to_string()), ramp)?;
        Ok(report)
    }

    fn classify(&self, summary: &mut ApplySummary, report: ApplyReport) {
        match report.outcome {
            ApplyOutcome::Applied => summary.applied.push(report),
            ApplyOutcome::Rejected => summary.rejected.push(report),
            ApplyOutcome::HdrSkipped => summary.hdr_skipped.push(report),
            ApplyOutcome::DisplayGone => summary.gone.push(report),
        }
    }

    // ---------- 紧急恢复（设计 §4.6） ----------

    /// 恢复全部屏到启动快照（托盘「恢复显示」/退出钩子）。
    pub fn restore_all(&self) -> Result<(), DisplayServiceError> {
        self.backend.restore_all()?;
        self.sources.lock().unwrap().clear();
        self.animating.lock().unwrap().clear();
        self.target.lock().unwrap().clear();
        self.current.lock().unwrap().clear();
        Ok(())
    }

    /// 注册 panic hook：best-effort restore（强杀无法保证，文档说明）。
    /// 使用 Weak 引用：service 被共享时也能执行 restore，不阻塞 panic 流程。
    pub fn install_panic_restore(service: &Arc<Self>) {
        let weak = Arc::downgrade(service);
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if let Some(svc) = weak.upgrade() {
                let _ = svc.restore_all();
            }
            prev(info);
        }));
    }

    /// 当前应用摘要（诊断用）。
    pub fn current_targets(&self) -> BTreeMap<String, Ramp> {
        self.target.lock().unwrap().clone()
    }

    pub fn transition_ms(&self) -> u64 {
        self.transition_ms
    }
    pub fn set_transition_ms(&mut self, ms: u64) {
        self.transition_ms = ms;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::math::build_ramp;

    /// Mock backend：记录 apply 调用，可注入 readback 失败。
    #[derive(Default)]
    struct MockBackend {
        applied: std::sync::Mutex<Vec<(String, Ramp)>>,
        fail_next: std::sync::Mutex<bool>,
        enum_count: std::sync::atomic::AtomicU64,
    }

    impl MockBackend {
        fn applied_count(&self) -> usize {
            self.applied.lock().unwrap().len()
        }
        fn last_ramp(&self) -> Option<Ramp> {
            self.applied.lock().unwrap().last().map(|(_, r)| r.clone())
        }
        fn set_fail(&self, on: bool) {
            *self.fail_next.lock().unwrap() = on;
        }
    }

    impl DisplayBackend for MockBackend {
        fn list_displays(&self) -> eyescare_platform::Result<Vec<eyescare_platform::DisplayInfo>> {
            self.enum_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(vec![
                eyescare_platform::DisplayInfo {
                    id: DisplayId("D1".into()),
                    device_name: "\\\\.\\DISPLAY1".into(),
                    is_primary: true,
                    width: 1920,
                    height: 1080,
                    hdr_active: false,
                    refresh_hz: Some(60),
                },
                eyescare_platform::DisplayInfo {
                    id: DisplayId("D2".into()),
                    device_name: "\\\\.\\DISPLAY2".into(),
                    is_primary: false,
                    width: 2560,
                    height: 1440,
                    hdr_active: false,
                    refresh_hz: Some(144),
                },
            ])
        }
        fn apply_ramp(
            &self,
            id: &DisplayId,
            ramp: &Ramp,
        ) -> eyescare_platform::Result<ApplyReport> {
            if *self.fail_next.lock().unwrap() {
                *self.fail_next.lock().unwrap() = false;
                return Ok(ApplyReport {
                    display_id: id.clone(),
                    outcome: ApplyOutcome::Rejected,
                    readback_diff: 0.9,
                    readback_vs_original_diff: 0.0,
                });
            }
            self.applied.lock().unwrap().push((id.0.clone(), ramp.clone()));
            Ok(ApplyReport {
                display_id: id.clone(),
                outcome: ApplyOutcome::Applied,
                readback_diff: 0.001,
                readback_vs_original_diff: 0.5,
            })
        }
        fn restore(&self, _id: &DisplayId) -> eyescare_platform::Result<()> {
            Ok(())
        }
        fn restore_all(&self) -> eyescare_platform::Result<()> {
            Ok(())
        }
        fn rebind_outputs(&self) -> eyescare_platform::Result<()> {
            Ok(())
        }
        fn detect_hdr_active(&self, _id: &DisplayId) -> eyescare_platform::Result<bool> {
            Ok(false)
        }
    }

    fn svc() -> (Arc<DisplayService>, Arc<MockBackend>) {
        let backend = Arc::new(MockBackend::default());
        let svc = Arc::new(DisplayService::new(backend.clone()));
        (svc, backend)
    }

    #[test]
    fn recompute_applies_both_displays() {
        let (svc, backend) = svc();
        svc.set_source(
            "daynight",
            vec![
                ("D1".into(), Claim::day_night(3400)),
                ("D2".into(), Claim::day_night(3400)),
            ],
        );
        let summary = svc.recompute().unwrap();
        assert_eq!(summary.applied.len(), 2);
        assert!(backend.applied_count() >= 2);
    }

    #[test]
    fn unchanged_target_skips_apply() {
        let (svc, backend) = svc();
        svc.set_source("daynight", vec![("D1".into(), Claim::day_night(3400))]);
        svc.recompute().unwrap();
        let n1 = backend.applied_count();
        // 同目标再 recompute：不应重复 apply
        svc.recompute().unwrap();
        let n2 = backend.applied_count();
        assert_eq!(n1, n2, "unchanged target must not re-apply");
    }

    #[test]
    fn safe_bypass_restores_identity_immediately() {
        let (svc, backend) = svc();
        svc.set_source("daynight", vec![("D1".into(), Claim::day_night(3400))]);
        svc.recompute().unwrap();
        // 进入旁路
        svc.set_source("safe", vec![("D1".into(), Claim::safe_bypass())]);
        let summary = svc.recompute().unwrap();
        let last = backend.last_ramp().unwrap();
        assert!(last.is_identity(), "P1 must restore identity immediately");
        assert!(!summary.applied.is_empty());
    }

    #[test]
    fn rejected_apply_reported() {
        let (svc, backend) = svc();
        svc.set_source("daynight", vec![("D1".into(), Claim::day_night(3400))]);
        backend.set_fail(true);
        let summary = svc.recompute().unwrap();
        assert_eq!(summary.rejected.len(), 1);
        assert!(summary.any_failure());
    }

    #[test]
    fn remove_source_falls_back_to_default() {
        let (svc, backend) = svc();
        svc.set_source("daynight", vec![("D1".into(), Claim::day_night(3400))]);
        svc.recompute().unwrap();
        svc.remove_source("daynight");
        let summary = svc.recompute().unwrap();
        // 无 claims → identity（is_identity 路径）
        let last = backend.last_ramp().unwrap();
        assert!(last.is_identity());
        assert_eq!(summary.applied.len(), 1);
    }

    #[test]
    fn restore_all_clears_state() {
        let (svc, backend) = svc();
        svc.set_source("daynight", vec![("D1".into(), Claim::day_night(3400))]);
        svc.recompute().unwrap();
        svc.restore_all().unwrap();
        assert!(svc.current_targets().is_empty());
        // 恢复后再 recompute：identity（无 claims）
        let summary = svc.recompute().unwrap();
        assert!(backend.last_ramp().unwrap().is_identity());
        assert_eq!(summary.applied.len(), 2);
    }

    #[test]
    fn display_enumeration_is_cached() {
        let (svc, backend) = svc();
        // known_displays 首次触发枚举，之后走缓存
        let ids1 = svc.known_displays();
        assert_eq!(ids1.len(), 2);
        let ids2 = svc.known_displays();
        assert_eq!(ids1, ids2);
        let n = backend.enum_count.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(n, 1, "known_displays must cache enumeration");
        // recompute 不重新枚举（缓存）
        svc.set_source("daynight", vec![("D1".into(), Claim::day_night(3400))]);
        let _ = svc.recompute().unwrap();
        assert_eq!(
            backend.enum_count.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "recompute must use cached enumeration"
        );
        // refresh_displays 强制刷新
        svc.refresh_displays().unwrap();
        assert_eq!(backend.enum_count.load(std::sync::atomic::Ordering::Relaxed), 2);
    }

    #[test]
    fn pump_animates_toward_target() {
        let (svc, backend) = svc();
        svc.set_source("daynight", vec![("D1".into(), Claim::day_night(3400))]);
        // 首次 recompute：直接应用目标（无动画）
        svc.recompute().unwrap();
        let first = backend.last_ramp().unwrap();
        // 改变目标触发动画
        svc.set_source("daynight", vec![("D1".into(), Claim::day_night(5500))]);
        svc.recompute().unwrap();
        // 壳层 1s 一拍：一次 pump 覆盖 500ms 过渡
        svc.pump().unwrap();
        let last = backend.last_ramp().unwrap();
        let target = build_ramp(5500, 1.0, 0.35);
        assert!(last.mean_abs_diff(&target) < 0.01, "should converge to target");
        assert!(last.mean_abs_diff(&first) > 0.01, "should have moved");
    }
}
