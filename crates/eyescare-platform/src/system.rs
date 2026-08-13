//! 系统后端契约：空闲检测、自启、电源/显示事件（§6.3 + §API）。

use crate::Result;

/// 事件回调。由平台线程触发，必须快速返回（内部转发到 core 事件总线）。
pub type EventCallback = Box<dyn Fn(SystemEvent) + Send + Sync>;

/// 平台系统事件（节流由 core 侧完成，平台只负责投递）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemEvent {
    /// 显示器拓扑变化（热插拔/分辨率变更；Win: WM_DISPLAYCHANGE）。
    DisplayChanged,
    /// 会话解锁（Win: WTS_SESSION_UNLOCK）。
    SessionUnlocked,
    /// 电源恢复（Win: PBT_APMRESUMEAUTOMATIC / mac: didWakeNotification）。
    PowerResumed,
    /// 系统睡眠（用于记录 claims，MVP 仅日志）。
    SystemSleep,
}

/// 事件订阅句柄；Drop 时注销。
#[derive(Default)]
pub struct Subscription {
    _private: (),
}

impl Subscription {
    pub fn new() -> Self {
        Self::default()
    }
}

/// 系统后端。
pub trait SystemBackend: Send + Sync {
    /// 距最后一次用户输入经过的秒数（Win: GetLastInputInfo）。
    fn seconds_since_input(&self) -> u64;

    /// 开机自启（Win: 注册表 Run 键；mac: SMAppService，MVP-B）。
    fn set_auto_start(&self, on: bool) -> Result<()>;

    /// 注册电源/显示事件。返回订阅句柄，Drop 即注销。
    fn on_power_and_display_events(&self, cb: EventCallback) -> Result<Subscription>;
}
