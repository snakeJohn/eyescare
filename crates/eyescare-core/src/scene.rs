//! 场景快照（PR-A09）：core 侧规范化视图，规则匹配与洞察采集的输入。
//!
//! 数据来源：`ForegroundAppBackend::foreground()`（平台线程 750ms 轮询）。
//! 洞察落库默认只存 `app_key`（sha256），不存明文（设计 §5.1）。

use sha2::{Digest, Sha256};

/// 规范化场景（与平台 SceneContext 同构，但属于 core 契约，避免平台类型泄漏）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneSnapshot {
    pub process_name: Option<String>,
    pub path_suffix: Option<String>,
    pub bundle_id: Option<String>,
    pub app_display_name: String,
    pub window_title: Option<String>,
    pub is_fullscreen: bool,
    /// 全屏置信度（High/Medium/Low/Unknown）——gaming 模板只认 High。
    pub fullscreen_confidence: eyescare_platform::FullscreenConfidence,
    pub display_id: Option<String>,
}

impl SceneSnapshot {
    /// 从平台 SceneContext 转换。
    pub fn from_platform(c: &eyescare_platform::SceneContext) -> Self {
        SceneSnapshot {
            process_name: c.process_name.clone(),
            path_suffix: c.path_suffix.clone(),
            bundle_id: c.bundle_id.clone(),
            app_display_name: c.app_display_name.clone(),
            // 标题不参与匹配，每秒采集会反复分配；洞察也不落明文。
            window_title: None,
            is_fullscreen: c.is_fullscreen,
            fullscreen_confidence: c.fullscreen_confidence,
            display_id: c.display_id.clone(),
        }
    }

    /// 身份键：`sha256(process_name|bundle_id)`（小写十六进制）。
    /// 采集不到身份时（桌面等）返回 None。
    pub fn app_key(&self) -> Option<String> {
        let identity = self
            .bundle_id
            .clone()
            .or_else(|| self.process_name.clone())?;
        let mut hasher = Sha256::new();
        hasher.update(identity.as_bytes());
        Some(format!("{:x}", hasher.finalize()))
    }

    /// 展示用身份名（UI/规则调试），不落历史表。
    pub fn display_identity(&self) -> String {
        self.app_display_name.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SceneSnapshot {
        SceneSnapshot {
            process_name: Some("Code.exe".into()),
            path_suffix: None,
            bundle_id: Some("com.microsoft.VSCode".into()),
            app_display_name: "Visual Studio Code".into(),
            window_title: None,
            is_fullscreen: false,
            fullscreen_confidence: eyescare_platform::FullscreenConfidence::Unknown,
            display_id: None,
        }
    }

    #[test]
    fn app_key_is_stable_hash() {
        let a = sample();
        let key = a.app_key().unwrap();
        assert_eq!(key.len(), 64);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
        // 同身份同 key
        let b = sample();
        assert_eq!(a.app_key(), b.app_key());
        // 明文不落 key
        assert!(!key.contains("Code"));
    }

    #[test]
    fn no_identity_no_key() {
        let s = SceneSnapshot {
            process_name: None,
            bundle_id: None,
            path_suffix: None,
            app_display_name: "Desktop".into(),
            window_title: None,
            is_fullscreen: false,
            fullscreen_confidence: eyescare_platform::FullscreenConfidence::Unknown,
            display_id: None,
        };
        assert_eq!(s.app_key(), None);
    }
}
