//! 显示后端契约（设计文档 §4 + §API）。
//!
//! 主路径是 GDI Gamma Ramp（Win）/ CG Transfer（mac）。所有实现必须：
//! - `apply_ramp` 内部做 readback 校验（`ApplyReport` 携带结果，不吞错）
//! - HDR 活动的屏返回 `HdrSkipped`，不施加 gamma
//! - 热插拔后由 `rebind_outputs` 触发重枚举

use serde::{Deserialize, Serialize};

use crate::Result;

/// 单通道 256 项 gamma ramp（0..=65535）。
/// serde：数组长度超 serde 内建上限（32），手写为 Vec<u16> 序列化。
#[derive(Debug, Clone, PartialEq)]
pub struct RampChannel(pub [u16; 256]);

impl serde::Serialize for RampChannel {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.iter())
    }
}

impl<'de> serde::Deserialize<'de> for RampChannel {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let v: Vec<u16> = serde::Deserialize::deserialize(deserializer)?;
        if v.len() != 256 {
            return Err(serde::de::Error::custom(format!(
                "ramp channel must have 256 entries, got {}",
                v.len()
            )));
        }
        let mut c = [0u16; 256];
        c.copy_from_slice(&v);
        Ok(RampChannel(c))
    }
}

impl RampChannel {
    pub fn identity() -> Self {
        let mut c = [0u16; 256];
        for (i, v) in c.iter_mut().enumerate() {
            *v = ((i as u32 * 65535) / 255) as u16;
        }
        Self(c)
    }

    /// 逐项平均绝对差，归一化到 0..=1（相对 65535）。
    pub fn mean_abs_diff(&self, other: &RampChannel) -> f64 {
        let mut acc: u64 = 0;
        for (a, b) in self.0.iter().zip(other.0.iter()) {
            acc += (*a as i64 - *b as i64).unsigned_abs();
        }
        (acc as f64 / 256.0) / 65535.0
    }

    pub fn is_identity(&self) -> bool {
        self.mean_abs_diff(&RampChannel::identity()) < 1e-9
    }
}

/// 三通道 ramp（RGB）。与 Windows `SetDeviceGammaRamp` 布局一致：
/// 连续 256×3 u16，先 R 后 G 后 B。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ramp {
    pub red: RampChannel,
    pub green: RampChannel,
    pub blue: RampChannel,
}

impl Ramp {
    pub fn identity() -> Self {
        Self {
            red: RampChannel::identity(),
            green: RampChannel::identity(),
            blue: RampChannel::identity(),
        }
    }

    /// 平铺为 Windows 布局的 768 项数组。
    pub fn as_flat_u16(&self) -> [u16; 768] {
        let mut out = [0u16; 768];
        out[..256].copy_from_slice(&self.red.0);
        out[256..512].copy_from_slice(&self.green.0);
        out[512..768].copy_from_slice(&self.blue.0);
        out
    }

    pub fn from_flat_u16(flat: &[u16]) -> Self {
        debug_assert!(flat.len() >= 768);
        let mut r = [0u16; 256];
        let mut g = [0u16; 256];
        let mut b = [0u16; 256];
        r.copy_from_slice(&flat[..256]);
        g.copy_from_slice(&flat[256..512]);
        b.copy_from_slice(&flat[512..768]);
        Self {
            red: RampChannel(r),
            green: RampChannel(g),
            blue: RampChannel(b),
        }
    }

    /// 与另一 ramp 的逐通道平均绝对差（0..=1）。
    pub fn mean_abs_diff(&self, other: &Ramp) -> f64 {
        (self.red.mean_abs_diff(&other.red)
            + self.green.mean_abs_diff(&other.green)
            + self.blue.mean_abs_diff(&other.blue))
            / 3.0
    }

    pub fn is_identity(&self) -> bool {
        self.red.is_identity() && self.green.is_identity() && self.blue.is_identity()
    }
}

/// 显示器稳定 ID。Win：DeviceKey/DeviceID 派生；mac：CGDisplay UUID 派生。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DisplayId(pub String);

impl std::fmt::Display for DisplayId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// 单屏信息（枚举结果）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayInfo {
    pub id: DisplayId,
    /// 平台设备名（Win: `\\.\DISPLAY1` 等；mac: 显示名）。用于 CreateDC 等调用。
    pub device_name: String,
    pub is_primary: bool,
    pub width: u32,
    pub height: u32,
    pub hdr_active: bool,
    /// 分辨率刷新率（hz），仅诊断用。
    pub refresh_hz: Option<u32>,
}

/// apply 的单屏结果（含 readback 校验结论）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyReport {
    pub display_id: DisplayId,
    pub outcome: ApplyOutcome,
    /// readback 与期望 ramp 的平均绝对差（0..=1），成功时为 ≤ ε。
    pub readback_diff: f64,
    /// readback 与原始 ramp 的距离（用于"静默拒绝"检测：没变也当失败）。
    pub readback_vs_original_diff: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApplyOutcome {
    /// 已施加且 readback 校验通过。
    Applied,
    /// OS 返回失败或 readback 偏差超 ε（设计 §4.3 Apply 后校验）。
    Rejected,
    /// HDR 活动，跳过 gamma（设计 KD18）。
    HdrSkipped,
    /// 显示器已不存在（拔线等），不报错但标记。
    DisplayGone,
}

/// 显示后端 trait。所有方法必须可重入；实现应内部加锁保证并发安全。
pub trait DisplayBackend: Send + Sync {
    /// 枚举当前活动显示器。失败返回 `Error`（如 GDI 初始化失败）。
    fn list_displays(&self) -> Result<Vec<DisplayInfo>>;

    /// 施加 ramp 并 readback 校验（设计 §4.3）。
    fn apply_ramp(&self, id: &DisplayId, ramp: &Ramp) -> Result<ApplyReport>;

    /// 恢复该屏到进程启动时快照（无快照则 identity）。
    fn restore(&self, id: &DisplayId) -> Result<()>;

    /// 恢复全部屏。
    fn restore_all(&self) -> Result<()>;

    /// 热插拔/显示变化后重新绑定输出并重枚举（WM_DISPLAYCHANGE / mac reconfig）。
    fn rebind_outputs(&self) -> Result<()>;

    /// 尽力检测 HDR 活动；无法检测时返回 Ok(false)（由用户设置兜底）。
    fn detect_hdr_active(&self, id: &DisplayId) -> Result<bool>;

    /// `true`（默认）时 HDR 屏跳过 gamma；`false` 为用户 Force 策略。
    fn set_hdr_skip(&self, _skip: bool) {}
}

/// 触发 apply 的语义来源（Insights 事件 source 字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplySource {
    User,
    Rule,
    DayNight,
    SafeBypass,
    EmergencyRestore,
    DefaultPreset,
}

impl std::fmt::Display for ApplySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ApplySource::User => "user",
            ApplySource::Rule => "rule",
            ApplySource::DayNight => "daynight",
            ApplySource::SafeBypass => "safe_bypass",
            ApplySource::EmergencyRestore => "emergency_restore",
            ApplySource::DefaultPreset => "default_preset",
        };
        f.write_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_ramp_is_linear() {
        let id = RampChannel::identity();
        assert_eq!(id.0[0], 0);
        assert_eq!(id.0[255], 65535);
        // 中点 127 → ~32639（127/255*65535 = 32639.47）
        assert!((id.0[127] as i32 - 32639).abs() <= 1);
    }

    #[test]
    fn flat_roundtrip() {
        let mut r = Ramp::identity();
        r.red.0[10] = 1234;
        r.green.0[20] = 5678;
        let flat = r.as_flat_u16();
        let back = Ramp::from_flat_u16(&flat);
        assert_eq!(back, r);
        assert_eq!(flat[10], 1234);
        assert_eq!(flat[256 + 20], 5678);
    }

    #[test]
    fn mean_abs_diff_scales() {
        let a = RampChannel::identity();
        let mut b = RampChannel::identity();
        b.0[0] = 65535;
        // 单点全差：65535/256/65535 = 1/256
        let d = a.mean_abs_diff(&b);
        assert!((d - 1.0 / 256.0).abs() < 1e-9);
        assert!(RampChannel::identity().mean_abs_diff(&RampChannel::identity()) < 1e-12);
    }

    #[test]
    fn identity_detection() {
        assert!(Ramp::identity().is_identity());
        let mut r = Ramp::identity();
        r.blue.0[100] += 1;
        assert!(!r.is_identity());
    }
}
