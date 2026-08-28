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
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// restore_all / 关滤镜后禁止 apply，避免 tick 回写。
    applies_enabled: AtomicBool,
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
            applies_enabled: AtomicBool::new(true),
        }
    }

    pub fn set_applies_enabled(&self, on: bool) {
        self.applies_enabled.store(on, Ordering::SeqCst);
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
        if !self.applies_enabled.load(Ordering::SeqCst) {
            return Ok(ApplySummary::default());
        }
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

            // 目标未变化：已落地则跳过；动画中且终点未变则不重置 progress
            let unchanged = prev_target.as_ref().map(|p| *p == target_ramp).unwrap_or(false);
            if unchanged {
                continue;
            }

            // P0/P1 identity：restore 启动快照，不写 identity ramp（设计 §4.6）
            if t.is_identity {
                if !self.applies_enabled.load(Ordering::SeqCst) {
                    continue;
                }
                match self.backend.restore(&DisplayId(t.display_id.clone())) {
                    Ok(()) => {}
                    Err(eyescare_platform::Error::DisplayGone(_)) => {
                        self.target.lock().unwrap().remove(&t.display_id);
                        next_current.remove(&t.display_id);
                        self.animating.lock().unwrap().remove(&t.display_id);
                        summary.gone.push(ApplyReport {
                            display_id: DisplayId(t.display_id.clone()),
                            outcome: ApplyOutcome::DisplayGone,
                            readback_diff: 0.0,
                            readback_vs_original_diff: 0.0,
                        });
                        continue;
                    }
                    Err(error) => return Err(DisplayServiceError::Backend(error)),
                }
                self.target
                    .lock()
                    .unwrap()
                    .insert(t.display_id.clone(), Ramp::identity());
                next_current.insert(t.display_id.clone(), Ramp::identity());
                self.animating.lock().unwrap().remove(&t.display_id);
                summary.applied.push(ApplyReport {
                    display_id: DisplayId(t.display_id.clone()),
                    outcome: ApplyOutcome::Applied,
                    readback_diff: 0.0,
                    readback_vs_original_diff: 0.0,
                });
                continue;
            }

            let from = next_current
                .get(&t.display_id)
                .cloned()
                .unwrap_or_else(Ramp::identity);

            if prev_target.is_none() {
                // 首次出现：直接 apply；仅 Applied 写入 current/target
                let report = self.apply_one(&t.display_id, &target_ramp)?;
                match report.outcome {
                    ApplyOutcome::Applied => {
                        self.target
                            .lock()
                            .unwrap()
                            .insert(t.display_id.clone(), target_ramp.clone());
                        next_current.insert(t.display_id.clone(), target_ramp.clone());
                    }
                    ApplyOutcome::HdrSkipped => {
                        // HDR may have become active while a previous ramp
                        // was already on the panel. Restore the startup ramp
                        // before dropping our bookkeeping; merely skipping
                        // the new write would leave the old tint in place.
                        if next_current.contains_key(&t.display_id) {
                            let _ = self
                                .backend
                                .restore(&DisplayId(t.display_id.clone()));
                        }
                        self.target.lock().unwrap().remove(&t.display_id);
                        next_current.remove(&t.display_id);
                    }
                    ApplyOutcome::DisplayGone => {
                        self.target.lock().unwrap().remove(&t.display_id);
                        next_current.remove(&t.display_id);
                    }
                    ApplyOutcome::Rejected => {}
                }
                self.animating.lock().unwrap().remove(&t.display_id);
                self.classify(&mut summary, report);
            } else {
                // 启动动画：立即记下终点（非插值）；pump 只推进 current
                self.target
                    .lock()
                    .unwrap()
                    .insert(t.display_id.clone(), target_ramp.clone());
                self.animating.lock().unwrap().insert(
                    t.display_id.clone(),
                    (from, target_ramp.clone(), 0.0),
                );
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
        // A reconnect can reuse the same stable ID while the hardware has
        // reset its gamma ramp. Never treat the old target/current as already
        // applied after a topology or power event. Clear these before the
        // enumeration refresh too, so a refresh failure cannot leave stale
        // state that makes the next recompute skip a necessary write.
        self.target.lock().unwrap().clear();
        self.current.lock().unwrap().clear();
        self.animating.lock().unwrap().clear();
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
        if !self.applies_enabled.load(Ordering::SeqCst) {
            return Ok(ApplySummary::default());
        }
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
            // pump 只更新 current；target 保持动画终点
            match report.outcome {
                ApplyOutcome::Applied => {
                    self.current.lock().unwrap().insert(id.clone(), ramp.clone());
                    if new_progress >= 1.0 {
                        finished.push(id.clone());
                    } else {
                        self.animating
                            .lock()
                            .unwrap()
                            .insert(id.clone(), (from, to, new_progress));
                    }
                }
                ApplyOutcome::HdrSkipped => {
                    // HDR skip is intentional, not a transient failure. Do
                    // not leave a permanent animation retry loop. The target
                    // is removed so a later HDR/topology change can retry.
                    if self.current.lock().unwrap().contains_key(&id) {
                        let _ = self.backend.restore(&DisplayId(id.clone()));
                    }
                    self.target.lock().unwrap().remove(&id);
                    self.current.lock().unwrap().remove(&id);
                    finished.push(id.clone());
                }
                ApplyOutcome::DisplayGone => {
                    // A disconnected display cannot make progress. Drop its
                    // state and let the next rebind enumerate it again.
                    self.target.lock().unwrap().remove(&id);
                    self.current.lock().unwrap().remove(&id);
                    finished.push(id.clone());
                }
                ApplyOutcome::Rejected => {
                    // Rejected: keep the animation and retry on the next pump
                    // (the OS may have temporarily refused the ramp).
                    self.animating.lock().unwrap().insert(
                        id.clone(),
                        (from, to, progress.min(0.999)),
                    );
                }
            }
            self.classify(&mut summary, report);
        }
        for id in finished {
            self.animating.lock().unwrap().remove(&id);
        }
        Ok(summary)
    }

    fn apply_one(&self, id: &str, ramp: &Ramp) -> Result<ApplyReport, DisplayServiceError> {
        if !self.applies_enabled.load(Ordering::SeqCst) {
            return Ok(ApplyReport {
                display_id: DisplayId(id.to_string()),
                outcome: ApplyOutcome::Rejected,
                readback_diff: 0.0,
                readback_vs_original_diff: 0.0,
            });
        }
        // HDR 屏由 backend 返回 HdrSkipped；后端负责策略
        match self
            .backend
            .apply_ramp(&DisplayId(id.to_string()), ramp)
        {
            Ok(report) => Ok(report),
            // A stale display cache is expected during hot-unplug. Normalize
            // the platform error to the trait's terminal outcome so an
            // animation is dropped instead of retrying forever until the
            // topology watcher runs.
            Err(eyescare_platform::Error::DisplayGone(_)) => Ok(ApplyReport {
                display_id: DisplayId(id.to_string()),
                outcome: ApplyOutcome::DisplayGone,
                readback_diff: 0.0,
                readback_vs_original_diff: 0.0,
            }),
            Err(error) => Err(DisplayServiceError::Backend(error)),
        }
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
        self.applies_enabled.store(false, Ordering::SeqCst);
        // Clear in-memory state even when the backend fails. Keeping stale
        // targets after a failed restore can make the next enable path skip
        // re-application, leaving the actual gamma state unknown.
        let result = self.backend.restore_all();
        self.sources.lock().unwrap().clear();
        self.animating.lock().unwrap().clear();
        self.target.lock().unwrap().clear();
        self.current.lock().unwrap().clear();
        result.map_err(DisplayServiceError::from)
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

impl Drop for DisplayService {
    fn drop(&mut self) {
        let _ = self.restore_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::math::build_ramp;

    /// Mock backend：记录 apply / restore 调用，可注入 readback 失败。
    #[derive(Default)]
    struct MockBackend {
        applied: std::sync::Mutex<Vec<(String, Ramp)>>,
        fail_next: std::sync::Mutex<bool>,
        enum_count: std::sync::atomic::AtomicU64,
        restored: std::sync::Mutex<Vec<String>>,
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
        fn restore_count(&self) -> usize {
            self.restored.lock().unwrap().len()
        }
        fn last_ramp_for(&self, id: &str) -> Option<Ramp> {
            self.applied
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|(d, _)| d == id)
                .map(|(_, r)| r.clone())
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
        fn restore(&self, id: &DisplayId) -> eyescare_platform::Result<()> {
            self.restored.lock().unwrap().push(id.0.clone());
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
        let after_filter = backend.last_ramp_for("D1").unwrap();
        assert!(!after_filter.is_identity());
        let apply_n = backend.applied_count();
        let restore_n = backend.restore_count();
        // 进入旁路：P1 必须 restore 快照，不得再 apply_ramp(identity)
        svc.set_source("safe", vec![("D1".into(), Claim::safe_bypass())]);
        let summary = svc.recompute().unwrap();
        assert!(
            backend.restore_count() > restore_n,
            "P1 must call backend.restore"
        );
        assert_eq!(
            backend.applied_count(),
            apply_n,
            "P1 must not apply_ramp identity"
        );
        let last = backend.last_ramp_for("D1").unwrap();
        assert!(
            !last.is_identity(),
            "last applied ramp must not be a forced identity write"
        );
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
    fn rejected_apply_is_retried() {
        let (svc, backend) = svc();
        svc.set_source("daynight", vec![("D1".into(), Claim::day_night(3400))]);
        backend.set_fail(true);
        let s1 = svc.recompute().unwrap();
        assert_eq!(s1.rejected.len(), 1);
        assert!(svc.current_targets().get("D1").is_none());
        let n1 = backend.applied_count();
        // fail_next 已消耗：同 claim 再 recompute 必须重试 apply
        let s2 = svc.recompute().unwrap();
        assert_eq!(s2.rejected.len(), 0);
        assert_eq!(s2.applied.len(), 1);
        assert!(
            backend.applied_count() > n1,
            "rejected apply must retry next recompute"
        );
        assert!(svc.current_targets().get("D1").is_some());
    }

    #[test]
    fn rejected_pump_keeps_anim_and_retries() {
        let (svc, backend) = svc();
        svc.set_source("daynight", vec![("D1".into(), Claim::day_night(3400))]);
        svc.recompute().unwrap();
        svc.set_source("daynight", vec![("D1".into(), Claim::day_night(5500))]);
        svc.recompute().unwrap();
        backend.set_fail(true);
        let s1 = svc.pump().unwrap();
        assert_eq!(s1.rejected.len(), 1);
        assert!(svc.is_animating(), "rejected pump must not settle animation");
        let n1 = backend.applied_count();
        let s2 = svc.pump().unwrap();
        assert_eq!(s2.rejected.len(), 0);
        assert!(backend.applied_count() > n1);
        assert!(!svc.is_animating());
    }

    #[test]
    fn restore_all_blocks_later_apply() {
        let (svc, backend) = svc();
        svc.set_source("daynight", vec![("D1".into(), Claim::day_night(3400))]);
        svc.recompute().unwrap();
        let n = backend.applied_count();
        svc.restore_all().unwrap();
        svc.set_source("daynight", vec![("D1".into(), Claim::day_night(5500))]);
        let _ = svc.recompute().unwrap();
        assert_eq!(
            backend.applied_count(),
            n,
            "apply after restore_all must no-op until re-enabled"
        );
    }

    #[test]
    fn remove_source_falls_back_to_default() {
        let (svc, backend) = svc();
        svc.set_source("daynight", vec![("D1".into(), Claim::day_night(3400))]);
        svc.recompute().unwrap();
        let restore_n = backend.restore_count();
        svc.remove_source("daynight");
        let summary = svc.recompute().unwrap();
        // 无 claims → identity：restore 快照，不写 identity ramp
        assert!(backend.restore_count() > restore_n);
        assert!(!backend.last_ramp_for("D1").unwrap().is_identity());
        assert_eq!(summary.applied.len(), 1);
    }

    #[test]
    fn restore_all_clears_state() {
        let (svc, backend) = svc();
        svc.set_source("daynight", vec![("D1".into(), Claim::day_night(3400))]);
        svc.recompute().unwrap();
        svc.restore_all().unwrap();
        assert!(svc.current_targets().is_empty());
        let restore_n = backend.restore_count();
        // restore_all 会禁止后续 apply；重新打开滤镜时再允许
        svc.set_applies_enabled(true);
        let summary = svc.recompute().unwrap();
        assert!(backend.restore_count() >= restore_n + 2);
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
        let target = build_ramp(5500, 0.85, 0.35);
        assert!(last.mean_abs_diff(&target) < 0.01, "should converge to target");
        assert!(last.mean_abs_diff(&first) > 0.01, "should have moved");
    }

    #[test]
    fn animation_target_is_destination_immediately() {
        let (svc, _backend) = svc();
        svc.set_source("daynight", vec![("D1".into(), Claim::day_night(3400))]);
        svc.recompute().unwrap();
        svc.set_source("daynight", vec![("D1".into(), Claim::day_night(5500))]);
        svc.recompute().unwrap();
        let dest = build_ramp(5500, 0.85, 0.35);
        let stored = svc.current_targets().get("D1").cloned().unwrap();
        assert!(
            stored.mean_abs_diff(&dest) < 0.01,
            "target must be destination immediately"
        );
        assert!(svc.is_animating());
        // 未完成的 pump 只推进 current，不得把插值写入 target
        svc.pump_dt(100.0).unwrap();
        let stored_after = svc.current_targets().get("D1").cloned().unwrap();
        assert!(
            stored_after.mean_abs_diff(&dest) < 0.01,
            "pump must not write lerp into target"
        );
        assert!(svc.is_animating());
    }

    #[test]
    fn recompute_same_destination_does_not_reset_anim() {
        let (svc, backend) = svc();
        svc.set_source("daynight", vec![("D1".into(), Claim::day_night(3400))]);
        svc.recompute().unwrap();
        svc.set_source("daynight", vec![("D1".into(), Claim::day_night(5500))]);
        svc.recompute().unwrap();
        svc.pump_dt(100.0).unwrap(); // progress 0.2
        svc.recompute().unwrap(); // 终点未变：不得把 progress 重置为 0
        svc.pump_dt(400.0).unwrap(); // 0.2 + 0.8 = 1.0
        assert!(
            !svc.is_animating(),
            "same destination must not reset animation progress"
        );
        let dest = build_ramp(5500, 0.85, 0.35);
        assert!(backend.last_ramp().unwrap().mean_abs_diff(&dest) < 0.01);
    }
}
