//! TimerService（PR-A13）：休息计时状态机 + 空闲暂停（设计 §6.1）。
//!
//! 状态：Working ⇄ Paused（空闲暂停）→ PreBreak → GuidedBreak → Working。
//! Fullscreen `notify_only`：到点只通知，不进 Overlay（§6.1 / R6）。
//! MVP：休息可推迟/跳过一次（普通模式；强制 v0.2）。

use std::time::{Duration, Instant};

use crate::config::TimerConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TimerState {
    Working,
    /// 空闲暂停（无输入 ≥ 阈值；不累计工作时间）。
    Paused,
    /// 休息前 30s 预通知窗口。
    PreBreak,
    /// 引导休息进行中。
    Break,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakKind {
    /// 普通休息。
    Normal,
    /// 20-20-20。
    TwentyTwenty,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TimerStatus {
    pub state: TimerState,
    pub work_elapsed_sec: u64,
    pub break_remaining_sec: Option<u64>,
    pub prebreak_remaining_sec: Option<u64>,
    pub kind: Option<BreakKind>,
    /// 本次休息是否已用过一次推迟。
    pub snoozed_once: bool,
    /// 本次休息是否已跳过。
    pub skipped: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerEvent {
    /// 进入 PreBreak（触发 30s 预通知）。
    PreBreakStarted,
    /// 休息到点（Fullscreen notify_only 时只发此事件，不进 Break）。
    BreakDue,
    /// 休息开始（Overlay 展示）。
    BreakStarted,
    /// 休息完成（用户提前结束或到时）。
    BreakFinished { completed: bool },
    /// 空闲暂停开始/恢复。
    PausedChanged { paused: bool },
}

/// 全屏策略下的休息行为。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreaksPolicy {
    Keep,
    Pause,
    /// 到点仅通知，不进 Overlay。
    NotifyOnly,
}

pub struct TimerService {
    cfg: TimerConfig,
    state: TimerState,
    work_elapsed: Duration,
    break_remaining: Option<Duration>,
    prebreak_remaining: Option<Duration>,
    kind: Option<BreakKind>,
    snoozed_once: bool,
    skipped: bool,
    last_input: Option<Instant>,
    /// 空闲暂停阈值。
    idle_pause_sec: u64,
    /// 预通知窗口。
    prebreak_sec: u64,
    events: Vec<TimerEvent>,
}

impl TimerService {
    pub fn new(cfg: TimerConfig) -> Self {
        Self {
            state: TimerState::Working,
            work_elapsed: Duration::ZERO,
            break_remaining: None,
            prebreak_remaining: None,
            kind: None,
            snoozed_once: false,
            skipped: false,
            last_input: Some(Instant::now()),
            idle_pause_sec: cfg.idle_pause_sec.max(10) as u64,
            prebreak_sec: 30,
            events: Vec::new(),
            cfg,
        }
    }

    pub fn status(&self) -> TimerStatus {
        TimerStatus {
            state: self.state,
            work_elapsed_sec: self.work_elapsed.as_secs(),
            break_remaining_sec: self.break_remaining.map(|d| d.as_secs()),
            prebreak_remaining_sec: self.prebreak_remaining.map(|d| d.as_secs()),
            kind: self.kind,
            snoozed_once: self.snoozed_once,
            skipped: self.skipped,
        }
    }

    pub fn drain_events(&mut self) -> Vec<TimerEvent> {
        std::mem::take(&mut self.events)
    }

    /// 用户输入活动（Idle 检测调用）。
    pub fn note_activity(&mut self) {
        self.note_activity_at(Instant::now());
    }

    /// 带时钟的输入活动（测试/注入用）。
    pub fn note_activity_at(&mut self, now: Instant) {
        self.last_input = Some(now);
    }

    /// 周期性 tick（建议 1s）。`breaks_policy` 为当前全屏策略。
    pub fn tick(&mut self, now: Instant, breaks_policy: BreaksPolicy) -> Vec<TimerEvent> {
        // 空闲检测：Working / PreBreak 均可暂停，避免预通知窗口空闲仍倒计时到 BreakDue
        let idle_sec = self
            .last_input
            .map(|t| now.saturating_duration_since(t).as_secs())
            .unwrap_or(0);
        if matches!(self.state, TimerState::Working | TimerState::PreBreak)
            && idle_sec >= self.idle_pause_sec
        {
            self.state = TimerState::Paused;
            self.events.push(TimerEvent::PausedChanged { paused: true });
        } else if self.state == TimerState::Paused && idle_sec < self.idle_pause_sec {
            // 若在 PreBreak 中暂停，恢复预通知窗口而不是丢掉倒计时
            self.state = if self.prebreak_remaining.is_some() {
                TimerState::PreBreak
            } else {
                TimerState::Working
            };
            self.events.push(TimerEvent::PausedChanged { paused: false });
        }

        match self.state {
            TimerState::Working => {
                self.work_elapsed += Duration::from_secs(1);
                // 进入 PreBreak 前 30s（含端点：work_sec ≤ prebreak_sec 时窗口退化为单点）
                if self.work_elapsed.as_secs() >= self.cfg.work_sec.saturating_sub(self.prebreak_sec as u32) as u64
                    && self.work_elapsed.as_secs() <= self.cfg.work_sec as u64
                    && self.prebreak_remaining.is_none()
                {
                    self.state = TimerState::PreBreak;
                    self.prebreak_remaining = Some(Duration::from_secs(
                        (self.cfg.work_sec as u64).saturating_sub(self.work_elapsed.as_secs()),
                    ));
                    self.events.push(TimerEvent::PreBreakStarted);
                }
            }
            TimerState::PreBreak => {
                let rem = self
                    .prebreak_remaining
                    .get_or_insert(Duration::from_secs(0))
                    .saturating_sub(Duration::from_secs(1));
                self.prebreak_remaining = Some(rem);
                if rem.is_zero() {
                    self.work_elapsed = Duration::ZERO;
                    self.prebreak_remaining = None;
                    match breaks_policy {
                        BreaksPolicy::Keep => {
                            self.events.push(TimerEvent::BreakDue);
                            self.start_break();
                        }
                        BreaksPolicy::NotifyOnly => {
                            // 只通知，不进 Overlay；直接回 Working（§6.1 / R6）
                            self.events.push(TimerEvent::BreakDue);
                            self.state = TimerState::Working;
                        }
                        BreaksPolicy::Pause => {
                            // 全屏：休息暂停，无提示（§5.4 breaks: pause）
                            self.state = TimerState::Working;
                        }
                    }
                }
            }
            TimerState::Break => {
                let rem = self
                    .break_remaining
                    .get_or_insert(Duration::from_secs(0))
                    .saturating_sub(Duration::from_secs(1));
                self.break_remaining = Some(rem);
                if rem.is_zero() {
                    self.finish_break(true);
                }
            }
            TimerState::Paused => {
                // 不累计工作时间
            }
        }
        std::mem::take(&mut self.events)
    }

    fn start_break(&mut self) {
        self.state = TimerState::Break;
        self.kind = Some(match self.cfg.profile {
            crate::config::TimerProfile::Normal => BreakKind::Normal,
            crate::config::TimerProfile::TwentyTwentyTwenty => BreakKind::TwentyTwenty,
        });
        self.break_remaining = Some(Duration::from_secs(self.cfg.break_sec as u64));
        self.snoozed_once = false;
        self.skipped = false;
        self.events.push(TimerEvent::BreakStarted);
    }

    fn finish_break(&mut self, completed: bool) {
        self.state = TimerState::Working;
        self.break_remaining = None;
        self.prebreak_remaining = None;
        self.kind = None;
        self.work_elapsed = Duration::ZERO;
        self.events.push(TimerEvent::BreakFinished { completed });
    }

    // ---------- 用户动作 ----------

    /// 提前结束休息（ESC / 按钮）。
    pub fn finish_break_early(&mut self) {
        if self.state == TimerState::Break {
            self.finish_break(false);
        }
    }

    /// 立即开始一次休息，供用户显式触发的全局快捷键使用。
    pub fn start_break_now(&mut self) {
        if !matches!(self.state, TimerState::Break) {
            self.work_elapsed = Duration::ZERO;
            self.prebreak_remaining = None;
            self.start_break();
        }
    }

    /// 推迟休息一次（snooze = work_sec 的 20%，MVP 固定 5min）。PreBreak 同样可推迟。
    pub fn snooze(&mut self) {
        if matches!(self.state, TimerState::Break | TimerState::PreBreak) && !self.snoozed_once {
            self.snoozed_once = true;
            self.state = TimerState::Working;
            self.break_remaining = None;
            self.prebreak_remaining = None;
            self.kind = None;
            self.work_elapsed = Duration::from_secs((self.cfg.work_sec as u64) * 4 / 5);
        }
    }

    /// 跳过本次休息（Break 或 PreBreak 预通知）。
    pub fn skip(&mut self) {
        if matches!(self.state, TimerState::Break | TimerState::PreBreak) {
            self.skipped = true;
            self.finish_break(false);
        }
    }

    pub fn state(&self) -> TimerState {
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(work: u32, brk: u32) -> TimerConfig {
        TimerConfig {
            profile: crate::config::TimerProfile::Normal,
            work_sec: work,
            break_sec: brk,
            idle_pause_sec: 240,
            guided: true,
            silent_break: false,
        }
    }

    #[test]
    fn status_serializes_snake_case() {
        let t = TimerService::new(cfg(60, 10));
        let json = serde_json::to_value(t.status()).unwrap();
        assert_eq!(json["state"], "working");
    }

    #[test]
    fn working_counts_to_break() {
        let mut t = TimerService::new(cfg(60, 10));
        let t0 = Instant::now();
        for i in 0..60 {
            let evs = t.tick(t0 + Duration::from_secs(i), BreaksPolicy::Keep);
            if i == 29 {
                // elapsed=30 时进入 PreBreak（work_sec - prebreak_sec = 30）
                assert!(evs.contains(&TimerEvent::PreBreakStarted));
                assert_eq!(t.state(), TimerState::PreBreak);
            }
            if i == 59 {
                assert!(evs.contains(&TimerEvent::BreakDue));
            }
        }
        assert_eq!(t.state(), TimerState::Break);
        assert_eq!(t.status().break_remaining_sec, Some(10));
    }

    #[test]
    fn break_finishes_and_resets() {
        let mut t = TimerService::new(cfg(5, 5));
        let t0 = Instant::now();
        // 直接驱动到 Break
        for i in 0..5 {
            t.tick(t0 + Duration::from_secs(i), BreaksPolicy::Keep);
        }
        assert_eq!(t.state(), TimerState::Break);
        for i in 5..10 {
            t.tick(t0 + Duration::from_secs(i), BreaksPolicy::Keep);
        }
        assert_eq!(t.state(), TimerState::Working);
        assert_eq!(t.status().work_elapsed_sec, 0);
    }

    #[test]
    fn idle_pauses_accumulation() {
        let mut t = TimerService::new(TimerConfig {
            idle_pause_sec: 12,
            ..cfg(1000, 10)
        });
        let t0 = Instant::now();
        // 工作 2s
        t.tick(t0, BreaksPolicy::Keep);
        t.tick(t0 + Duration::from_secs(1), BreaksPolicy::Keep);
        // 空闲 12s → 暂停
        let evs = t.tick(t0 + Duration::from_secs(13), BreaksPolicy::Keep);
        assert!(evs.contains(&TimerEvent::PausedChanged { paused: true }));
        assert_eq!(t.state(), TimerState::Paused);
        let paused_at = t.status().work_elapsed_sec;
        // 继续空闲 10s：不累计
        for i in 14..24 {
            t.tick(t0 + Duration::from_secs(i), BreaksPolicy::Keep);
        }
        assert_eq!(t.status().work_elapsed_sec, paused_at);
        // 恢复输入（带注入时钟）
        t.note_activity_at(t0 + Duration::from_secs(24));
        let evs = t.tick(t0 + Duration::from_secs(24), BreaksPolicy::Keep);
        assert!(evs.contains(&TimerEvent::PausedChanged { paused: false }));
        assert_eq!(t.state(), TimerState::Working);
    }

    #[test]
    fn notify_only_no_overlay() {
        let mut t = TimerService::new(cfg(5, 10));
        let t0 = Instant::now();
        let mut all = Vec::new();
        for i in 0..5 {
            all.extend(t.tick(t0 + Duration::from_secs(i), BreaksPolicy::NotifyOnly));
        }
        // 到点只通知，直接回 Working，不进 Break
        assert_eq!(t.state(), TimerState::Working);
        assert!(all.contains(&TimerEvent::BreakDue));
        assert!(!all.contains(&TimerEvent::BreakStarted));
    }

    #[test]
    fn early_finish_and_skip() {
        let mut t = TimerService::new(cfg(5, 10));
        let t0 = Instant::now();
        for i in 0..5 {
            t.tick(t0 + Duration::from_secs(i), BreaksPolicy::Keep);
        }
        assert_eq!(t.state(), TimerState::Break);
        t.finish_break_early();
        assert_eq!(t.state(), TimerState::Working);
        let evs = t.drain_events();
        assert!(evs.contains(&TimerEvent::BreakFinished { completed: false }));
    }

    #[test]
    fn start_break_now_enters_break_and_emits_start() {
        let mut t = TimerService::new(cfg(1200, 20));
        t.start_break_now();
        assert_eq!(t.state(), TimerState::Break);
        assert_eq!(t.status().break_remaining_sec, Some(20));
        assert!(t.drain_events().contains(&TimerEvent::BreakStarted));
    }

    #[test]
    fn snooze_once_then_break() {
        let mut t = TimerService::new(cfg(10, 5));
        let t0 = Instant::now();
        // 推进到第一次 Break（状态驱动，不数 tick）
        for i in 0..30 {
            if t.state() == TimerState::Break {
                break;
            }
            t.tick(t0 + Duration::from_secs(i), BreaksPolicy::Keep);
        }
        assert_eq!(t.state(), TimerState::Break);
        t.snooze();
        assert_eq!(t.state(), TimerState::Working);
        // 第二次到点后 snooze 不可用（once）
        for i in 30..60 {
            if t.state() == TimerState::Break {
                break;
            }
            t.tick(t0 + Duration::from_secs(i), BreaksPolicy::Keep);
        }
        assert_eq!(t.state(), TimerState::Break);
        // 每个休息周期可 snooze 一次（设计验收第 8 条：可推迟/跳过一次）
        t.snooze();
        assert_eq!(t.state(), TimerState::Working);
        // 新周期再次到点后仍可 snooze（每周期一次的语义由 start_break 重置保证）
        for i in 60..90 {
            if t.state() == TimerState::Break {
                break;
            }
            t.tick(t0 + Duration::from_secs(i), BreaksPolicy::Keep);
        }
        assert_eq!(t.state(), TimerState::Break);
        t.snooze();
        assert_eq!(t.state(), TimerState::Working);
    }

    #[test]
    fn idle_during_prebreak_pauses_without_break_due() {
        let mut t = TimerService::new(TimerConfig {
            idle_pause_sec: 12,
            ..cfg(40, 10)
        });
        let t0 = Instant::now();
        t.note_activity_at(t0);
        // work_sec=40、prebreak=30 → elapsed=10 进入 PreBreak
        for i in 0..10 {
            t.tick(t0 + Duration::from_secs(i), BreaksPolicy::Keep);
        }
        assert_eq!(t.state(), TimerState::PreBreak);
        // 空闲达阈值：暂停，不得倒计时到 BreakDue
        let evs = t.tick(t0 + Duration::from_secs(12), BreaksPolicy::Keep);
        assert!(evs.contains(&TimerEvent::PausedChanged { paused: true }));
        assert_eq!(t.state(), TimerState::Paused);
        assert!(!evs.contains(&TimerEvent::BreakDue));
        let evs = t.tick(t0 + Duration::from_secs(80), BreaksPolicy::Keep);
        assert_eq!(t.state(), TimerState::Paused);
        assert!(!evs.contains(&TimerEvent::BreakDue));
    }

    #[test]
    fn skip_during_prebreak_cancels_and_resets() {
        let mut t = TimerService::new(cfg(40, 10));
        let t0 = Instant::now();
        t.note_activity_at(t0);
        for i in 0..10 {
            t.tick(t0 + Duration::from_secs(i), BreaksPolicy::Keep);
        }
        assert_eq!(t.state(), TimerState::PreBreak);
        t.skip();
        assert_eq!(t.state(), TimerState::Working);
        assert_eq!(t.status().work_elapsed_sec, 0);
        assert!(t.status().skipped);
        assert!(t.status().prebreak_remaining_sec.is_none());
        let evs = t.drain_events();
        assert!(evs.contains(&TimerEvent::BreakFinished { completed: false }));
    }
}
