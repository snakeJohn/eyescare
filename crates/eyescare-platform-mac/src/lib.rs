//! macOS 平台后端（占位 stub，PR-A05 / MVP-B 对齐）。
//!
//! 设计 §4.4：`CGSetDisplayTransferByFormula/ByTable`、`CGGetActiveDisplayList`、
//! `CGDisplayCreateUUIDFromDisplayID`、reconfig/wake re-apply。
//! 当前实现返回 Unsupported；Win-first 不阻塞 MVP-A 发布（KD16）。

use eyescare_platform::{
    ApplyOutcome, ApplyReport, DisplayBackend, DisplayId, DisplayInfo, Error, ForegroundAppBackend,
    FullscreenConfidence, FullscreenKind, Ramp, Result, SceneContext, Subscription, SystemBackend,
    SystemEvent, EventCallback,
};

pub struct MacDisplayBackend;

impl DisplayBackend for MacDisplayBackend {
    fn list_displays(&self) -> Result<Vec<DisplayInfo>> {
        Err(Error::Unsupported)
    }
    fn apply_ramp(&self, _id: &DisplayId, _ramp: &Ramp) -> Result<ApplyReport> {
        Err(Error::Unsupported)
    }
    fn restore(&self, _id: &DisplayId) -> Result<()> {
        Err(Error::Unsupported)
    }
    fn restore_all(&self) -> Result<()> {
        Err(Error::Unsupported)
    }
    fn rebind_outputs(&self) -> Result<()> {
        Err(Error::Unsupported)
    }
    fn detect_hdr_active(&self, _id: &DisplayId) -> Result<bool> {
        Err(Error::Unsupported)
    }
}

pub struct MacForegroundBackend;

impl ForegroundAppBackend for MacForegroundBackend {
    fn foreground(&self) -> Result<SceneContext> {
        Err(Error::Unsupported)
    }
}

pub struct MacSystemBackend;

impl SystemBackend for MacSystemBackend {
    fn seconds_since_input(&self) -> u64 {
        0
    }
    fn set_auto_start(&self, _on: bool) -> Result<()> {
        Err(Error::Unsupported)
    }
    fn on_power_and_display_events(&self, _cb: EventCallback) -> Result<Subscription> {
        Err(Error::Unsupported)
    }
}

// 占位：让 stub 类型不至于全空（便于未来 PR 直接填充）。
#[allow(dead_code)]
fn _placeholder() {
    let _ = FullscreenConfidence::Unknown;
    let _ = FullscreenKind::None;
    let _ = ApplyOutcome::Applied;
    let _ = SystemEvent::DisplayChanged;
}
