//! 平台抽象层：trait 定义 + 跨 crate 共享类型。
//!
//! 设计文档 §API / Interface Changes。实现方：
//! - `eyescare-platform-win`（GDI Gamma / 前台采样 / 系统事件）
//! - `eyescare-platform-mac`（MVP-B 对齐，当前 stub）
//!
//! 本 crate 必须保持平台无关、无 IO 副作用，方便在 CI 全平台编译测试。

pub mod display;
pub mod foreground;
pub mod system;

pub use display::{
    ApplyOutcome, ApplyReport, DisplayBackend, DisplayId, DisplayInfo, Ramp, RampChannel,
};
pub use foreground::{ForegroundAppBackend, FullscreenConfidence, FullscreenKind, SceneContext};
pub use system::{EventCallback, Subscription, SystemBackend, SystemEvent};

/// 统一的错误模型（设计 §错误模型）。
/// 上层把 [`Error`] 序列化为 `{ "code": "...", "display_id": "...", "message": "..." }`。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("gamma rejected by OS: {0}")]
    GammaRejected(String),
    #[error("display not found or gone: {0}")]
    DisplayGone(String),
    #[error("hdr active, gamma skipped: {0}")]
    HdrSkipped(String),
    #[error("platform io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("platform error: {0}")]
    Platform(String),
    #[error("not implemented on this platform yet")]
    Unsupported,
}

pub type Result<T> = std::result::Result<T, Error>;
