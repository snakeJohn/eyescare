//! Windows 平台后端（PR-A04）：GDI Gamma、前台采样、系统事件。
//!
//! 实现设计 §4.3：per-monitor `CreateDC` + readback + HDR skip + rebind。
//! 仅供 `x86_64-pc-windows-msvc` 目标编译（本机交叉 check 验证）。

pub mod display;
pub mod foreground;
pub mod system;

pub use display::WindowsDisplayBackend;
pub use foreground::WindowsForegroundBackend;
pub use system::WindowsSystemBackend;
