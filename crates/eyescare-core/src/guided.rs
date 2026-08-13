//! GuidedBreakPlayer（PR-A14）：引导休息步骤时间轴（设计 §6.2）。
//!
//! MVP：短休引导步骤 + 可静默；遵循 OS reduced motion 时步骤静默化。
//! 结束：ESC / 按钮「结束休息」（非强制，v0.2 才强制）。

use std::time::{Duration, Instant};

/// 引导步骤。
#[derive(Debug, Clone)]
pub struct BreakStep {
    /// 步骤标题（如「远眺 6 米外」）。
    pub title: &'static str,
    /// 引导文案。
    pub body: &'static str,
    pub duration_sec: u64,
    /// 是否播放提示音（声景的一部分）。
    pub chime: bool,
}

/// 20-20-20 时间轴（§6.2 MVP UX 表）。
pub fn twenty_twenty_timeline(break_sec: u64) -> Vec<BreakStep> {
    vec![BreakStep {
        title: "远眺窗外",
        body: "看向 6 米以外，放松眼部调节肌肉。",
        duration_sec: break_sec.max(20),
        chime: true,
    }]
}

/// 普通休息时间轴（引导性更强）。
pub fn normal_timeline(break_sec: u64) -> Vec<BreakStep> {
    let s = break_sec.max(30);
    vec![
        BreakStep {
            title: "闭眼放松",
            body: "轻闭双眼 10 秒，让泪膜均匀覆盖。",
            duration_sec: 10,
            chime: true,
        },
        BreakStep {
            title: "远眺",
            body: "看向远处，活动颈椎，深呼吸三次。",
            duration_sec: s.saturating_sub(10).max(10),
            chime: false,
        },
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuidedState {
    Idle,
    /// 步骤过渡（短暂提示下一步）。
    Playing,
    Done,
}

pub struct GuidedBreakPlayer {
    steps: Vec<BreakStep>,
    state: GuidedState,
    step_idx: usize,
    step_remaining: Duration,
    started_at: Option<Instant>,
    /// OS reduced motion：步骤静默化（不播 chime、无动画）。
    reduced_motion: bool,
    /// 音频静音（用户关闭声景）。
    sound_enabled: bool,
}

impl GuidedBreakPlayer {
    pub fn new(steps: Vec<BreakStep>) -> Self {
        Self {
            steps,
            state: GuidedState::Idle,
            step_idx: 0,
            step_remaining: Duration::ZERO,
            started_at: None,
            reduced_motion: false,
            sound_enabled: true,
        }
    }

    pub fn start(&mut self) {
        if self.steps.is_empty() {
            self.state = GuidedState::Done;
            return;
        }
        self.state = GuidedState::Playing;
        self.step_idx = 0;
        self.step_remaining = Duration::from_secs(self.steps[0].duration_sec);
        self.started_at = Some(Instant::now());
    }

    /// 当前步骤（None = 未开始/结束）。
    pub fn current_step(&self) -> Option<&BreakStep> {
        if self.state == GuidedState::Playing {
            self.steps.get(self.step_idx)
        } else {
            None
        }
    }

    pub fn step_remaining_sec(&self) -> Option<u64> {
        if self.state == GuidedState::Playing {
            Some(self.step_remaining.as_secs())
        } else {
            None
        }
    }

    pub fn state(&self) -> GuidedState {
        self.state
    }

    /// tick（1s 粒度）。返回 true 表示步骤变化（UI 刷新）。
    pub fn tick(&mut self) -> bool {
        if self.state != GuidedState::Playing {
            return false;
        }
        self.step_remaining = self.step_remaining.saturating_sub(Duration::from_secs(1));
        if self.step_remaining.is_zero() {
            self.step_idx += 1;
            if self.step_idx >= self.steps.len() {
                self.state = GuidedState::Done;
                return true;
            }
            self.step_remaining = Duration::from_secs(self.steps[self.step_idx].duration_sec);
            return true;
        }
        false
    }

    /// 提前结束（ESC / 按钮）。
    pub fn finish_early(&mut self) {
        if self.state == GuidedState::Playing {
            self.state = GuidedState::Done;
        }
    }

    /// 本步骤是否需要 chime（尊重 reduced_motion / sound_enabled）。
    pub fn should_chime(&self) -> bool {
        self.sound_enabled
            && !self.reduced_motion
            && self
                .current_step()
                .map(|s| s.chime)
                .unwrap_or(false)
    }

    pub fn set_reduced_motion(&mut self, on: bool) {
        self.reduced_motion = on;
    }
    pub fn set_sound_enabled(&mut self, on: bool) {
        self.sound_enabled = on;
    }

    pub fn elapsed_sec(&self) -> u64 {
        self.started_at
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_advances_and_done() {
        let mut p = GuidedBreakPlayer::new(normal_timeline(30));
        assert_eq!(p.state(), GuidedState::Idle);
        p.start();
        assert_eq!(p.state(), GuidedState::Playing);
        assert_eq!(p.current_step().unwrap().title, "闭眼放松");
        // 10s 后进入第二步
        let mut changed = false;
        for _ in 0..10 {
            changed = p.tick();
        }
        assert!(changed);
        assert_eq!(p.current_step().unwrap().title, "远眺");
        // 走完剩余
        for _ in 0..20 {
            p.tick();
        }
        assert_eq!(p.state(), GuidedState::Done);
        assert!(p.current_step().is_none());
    }

    #[test]
    fn finish_early() {
        let mut p = GuidedBreakPlayer::new(twenty_twenty_timeline(20));
        p.start();
        p.tick();
        p.finish_early();
        assert_eq!(p.state(), GuidedState::Done);
    }

    #[test]
    fn chime_respects_settings() {
        let mut p = GuidedBreakPlayer::new(twenty_twenty_timeline(20));
        p.start();
        assert!(p.should_chime());
        p.set_sound_enabled(false);
        assert!(!p.should_chime());
        p.set_sound_enabled(true);
        p.set_reduced_motion(true);
        assert!(!p.should_chime());
    }

    #[test]
    fn empty_timeline_immediately_done() {
        let mut p = GuidedBreakPlayer::new(vec![]);
        p.start();
        assert_eq!(p.state(), GuidedState::Done);
    }
}
