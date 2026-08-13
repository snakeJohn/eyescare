//! 前台应用采样契约（设计 §5.1 SceneContext）。

use serde::{Deserialize, Serialize};

use crate::Result;

/// 全屏置信度（§5.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FullscreenConfidence {
    High,
    Medium,
    Low,
    Unknown,
}

/// 全屏种类（§5.3 启发）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FullscreenKind {
    /// 无边框全屏（窗 rect ≈ monitor rect 且无 thick frame）→ High 可测 Overlay。
    Borderless,
    /// 独占全屏（DXGI fullscreen 等；Overlay 不可依赖）。
    Exclusive,
    /// 最大化窗口。
    Maximized,
    None,
}

/// 规范身份（§5.1）。`app_key` 由采集方计算：`sha256(process_name|bundle_id)`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneContext {
    /// Win 主键："Code.exe"；mac 可为空。
    pub process_name: Option<String>,
    /// 可选路径后缀：`\Microsoft VS Code\Code.exe`。
    pub path_suffix: Option<String>,
    /// macOS 主键。
    pub bundle_id: Option<String>,
    /// 仅 UI 展示，不落历史表（默认）。
    pub app_display_name: String,
    /// MVP 可采但默认不参与规则匹配。
    pub window_title: Option<String>,
    pub is_fullscreen: bool,
    pub fullscreen_confidence: FullscreenConfidence,
    pub fullscreen_kind: FullscreenKind,
    pub display_id: Option<String>,
}

impl SceneContext {
    /// 归一化身份键。两侧平台统一：优先 bundle_id，其次 process_name。
    /// 返回 None 表示采集不到任何身份（桌面/锁屏等）。
    pub fn identity_key(&self) -> Option<String> {
        self.bundle_id
            .clone()
            .or_else(|| self.process_name.clone())
            .map(|s| s.to_lowercase())
    }
}

/// 前台采样后端。
pub trait ForegroundAppBackend: Send + Sync {
    fn foreground(&self) -> Result<SceneContext>;
}
