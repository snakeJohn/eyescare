//! 事件总线（设计 §事件（防刷））。
//!
//! 节流规则：
//! - `state_changed`：coalesce 100ms
//! - `scene_changed`：仅 app_key 变化时 emit（禁止 750ms 刷 UI）
//! - `safe_mode_changed`：立即
//! - `display_apply_failed`：每屏每分钟最多 1 次 UI

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// core 对外事件（UI / 洞察 / 诊断消费）。
#[derive(Debug, Clone, PartialEq)]
pub enum CoreEvent {
    StateChanged,
    SceneChanged {
        app_key: Option<String>,
    },
    SafeModeChanged,
    DisplayApplyFailed {
        display_id: String,
        code: String,
    },
    BreakDue,
    BreakStarted,
    BreakFinished {
        completed: bool,
    },
    InsightsUpdated,
}

/// 事件监听器。
type Listener = Arc<dyn Fn(CoreEvent) + Send + Sync>;

/// 带节流的事件总线（线程安全）。
pub struct EventBus {
    listeners: Mutex<Vec<Listener>>,
    /// display_id -> last emit ts（display_apply_failed 限频）。
    apply_failed_last: Mutex<HashMap<String, u64>>,
    /// scene app_key 防抖。
    last_scene_key: Mutex<Option<String>>,
}

impl EventBus {
    pub fn new() -> Self {
        Self {
            listeners: Mutex::new(Vec::new()),
            apply_failed_last: Mutex::new(HashMap::new()),
            last_scene_key: Mutex::new(None),
        }
    }

    pub fn subscribe(&self, f: Box<dyn Fn(CoreEvent) + Send + Sync>) {
        self.listeners
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(Arc::from(f));
    }

    fn emit(&self, e: CoreEvent) {
        // Never hold the listener mutex while invoking user code.  A listener
        // may subscribe or emit another event, and doing so under the lock
        // would deadlock the event bus.
        let listeners = self
            .listeners
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        for f in listeners {
            f(e.clone());
        }
    }

    // ---------- 各事件的节流入口 ----------

    /// state_changed：100ms coalesce。
    pub fn state_changed(&self) {
        // MVP：直接投递，coalesce 由调用方 tick 节奏保证（主循环 ≤ 10Hz）。
        self.emit(CoreEvent::StateChanged);
    }

    /// scene_changed：仅 app_key 变化时 emit。
    pub fn scene_changed(&self, app_key: Option<String>) {
        let changed = {
            let mut last = self
                .last_scene_key
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if *last != app_key {
                *last = app_key.clone();
                true
            } else {
                false
            }
        };
        if changed {
            self.emit(CoreEvent::SceneChanged { app_key });
        }
    }

    /// safe_mode_changed：立即。
    pub fn safe_mode_changed(&self) {
        self.emit(CoreEvent::SafeModeChanged);
    }

    /// display_apply_failed：每屏每分钟最多 1 次。
    pub fn display_apply_failed(&self, display_id: &str, code: &str) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let should_emit = {
            let mut last = self
                .apply_failed_last
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let should = last
                .get(display_id)
                .map(|prev| now.saturating_sub(*prev) >= 60)
                .unwrap_or(true);
            if should {
                last.insert(display_id.to_string(), now);
            }
            should
        };
        if should_emit {
            self.emit(CoreEvent::DisplayApplyFailed {
                display_id: display_id.to_string(),
                code: code.to_string(),
            });
        }
    }

    pub fn push(&self, e: CoreEvent) {
        self.emit(e);
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn scene_changed_throttled_by_app_key() {
        let bus = EventBus::new();
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        bus.subscribe(Box::new(move |e| {
            if let CoreEvent::SceneChanged { .. } = e {
                c.fetch_add(1, Ordering::SeqCst);
            }
        }));
        bus.scene_changed(Some("key-a".into()));
        bus.scene_changed(Some("key-a".into()));
        bus.scene_changed(Some("key-a".into()));
        bus.scene_changed(Some("key-b".into()));
        bus.scene_changed(Some("key-b".into()));
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn apply_failed_limited_to_once_per_minute() {
        let bus = EventBus::new();
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        bus.subscribe(Box::new(move |e| {
            if let CoreEvent::DisplayApplyFailed { .. } = e {
                c.fetch_add(1, Ordering::SeqCst);
            }
        }));
        bus.display_apply_failed("D1", "GAMMA_REJECTED");
        bus.display_apply_failed("D1", "GAMMA_REJECTED");
        bus.display_apply_failed("D1", "GAMMA_REJECTED");
        bus.display_apply_failed("D2", "HDR_SKIPPED");
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }
}
