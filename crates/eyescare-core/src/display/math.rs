//! 显示数学（PR-A03）：色温→RGB、ramp 构建、插值、12Hz 限频。
//!
//! `kelvin_to_rgb` 采用 Tanner Helland 算法（设计 §4.2 / References）。
//! ramp 生成后再做可读性启发式与 identity 偏离上限（设计 §4.3 极端暖色段）。

use eyescare_platform::{Ramp, RampChannel};

/// 有效色温范围（设计 §4.1）。
pub const KELVIN_MIN: u32 = 1000;
pub const KELVIN_MAX: u32 = 10000;

/// Tanner Helland 色温 → 线性 RGB（0..=1）。
pub fn kelvin_to_rgb(kelvin: u32) -> (f64, f64, f64) {
    let k = (kelvin.clamp(KELVIN_MIN, KELVIN_MAX) as f64) / 100.0;

    let red = if k <= 66.0 {
        255.0
    } else {
        329.698727446 * (k - 60.0).powf(-0.1332047592)
    };

    let green = if k <= 66.0 {
        99.4708025861 * k.ln() - 161.1195681661
    } else {
        288.1221695283 * (k - 60.0).powf(-0.0755148492)
    };

    let blue = if k >= 66.0 {
        255.0
    } else if k <= 19.0 {
        0.0
    } else {
        138.5177312231 * (k - 10.0).ln() - 305.0447927307
    };

    let clamp01 = |v: f64| v.clamp(0.0, 255.0) / 255.0;
    (clamp01(red), clamp01(green), clamp01(blue))
}

/// 由 (kelvin, brightness) 构建 gamma ramp。
/// 每通道：`out[i] = identity[i] * channel_factor`，channel_factor = rgb * brightness。
/// 可读性启发式：最小通道不低于 identity 的 `min_floor`（默认 0.35），
/// 且与 identity 的最大偏离 ≤ 0.6（防 OS 静默拒斥，设计 §4.3）。
pub fn build_ramp(kelvin: u32, brightness: f64, min_floor: f64) -> Ramp {
    let (r, g, b) = kelvin_to_rgb(kelvin);
    let bfactor = brightness.clamp(0.01, 1.0);
    let floor = min_floor.clamp(0.0, 1.0);

    let identity = RampChannel::identity();
    let make = |factor: f64| {
        let mut c = [0u16; 256];
        for (slot, v) in c.iter_mut().zip(identity.0.iter()) {
            let target = (*v as f64 / 65535.0) * factor;
            *slot = (target * 65535.0).round() as u16;
        }
        RampChannel(c)
    };

    // 限制与 identity 的最大偏离（暖色时可读性），避免整条曲线过于极端
    let max_dev = 0.6;
    let soften = |factor: f64| -> f64 {
        // factor ∈ [floor, 1]；偏离 = 1 - factor 若 <1。
        // 偏离超过 max_dev 时向 identity 回拉。
        let dev = (1.0 - factor).abs();
        if dev > max_dev {
            if factor < 1.0 {
                factor + (dev - max_dev)
            } else {
                factor - (dev - max_dev)
            }
        } else {
            factor
        }
    };

    // floor 约束通道因子，再按 identity 缩放（不抬升暗部 LUT）
    let channel_factor = |rgb: f64| soften(rgb * bfactor).clamp(floor, 1.0);

    Ramp {
        red: make(channel_factor(r)),
        green: make(channel_factor(g)),
        blue: make(channel_factor(b)),
    }
}

/// 两 ramp 间线性插值（过渡动画用）。t ∈ [0,1]。
pub fn lerp_ramp(a: &Ramp, b: &Ramp, t: f64) -> Ramp {
    let t = t.clamp(0.0, 1.0);
    let lerp_ch = |x: &RampChannel, y: &RampChannel| {
        let mut c = [0u16; 256];
        for (ci, (xv, yv)) in c.iter_mut().zip(x.0.iter().zip(y.0.iter())) {
            let v = *xv as f64 + (*yv as f64 - *xv as f64) * t;
            *ci = v.round() as u16;
        }
        RampChannel(c)
    };
    Ramp {
        red: lerp_ch(&a.red, &b.red),
        green: lerp_ch(&a.green, &b.green),
        blue: lerp_ch(&a.blue, &b.blue),
    }
}

/// 12Hz 限频器（设计 §4.2 过渡合并 ≤ 12Hz）。
/// `should_emit(now_ms)` 返回 true 时表示可以 apply；两次 emit 间隔 ≥ 83ms。
#[derive(Debug, Clone)]
pub struct RateLimiter {
    period_ms: u64,
    last_emit_ms: Option<u64>,
}

impl RateLimiter {
    pub fn new(period_ms: u64) -> Self {
        Self {
            period_ms,
            last_emit_ms: None,
        }
    }

    /// 12Hz 默认。
    pub fn twelve_hz() -> Self {
        Self::new(83)
    }

    pub fn should_emit(&mut self, now_ms: u64) -> bool {
        match self.last_emit_ms {
            None => {
                self.last_emit_ms = Some(now_ms);
                true
            }
            Some(last) if now_ms.saturating_sub(last) >= self.period_ms => {
                self.last_emit_ms = Some(now_ms);
                true
            }
            _ => false,
        }
    }

    /// 强制放行（P1 旁路进入等紧急场景，设计 §4.6）。
    /// 语义：下一次 `should_emit` 立即通过。
    pub fn force(&mut self, now_ms: u64) {
        self.last_emit_ms = Some(now_ms.saturating_sub(self.period_ms));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kelvin_known_values() {
        // 6500K 接近白（RGB 都接近 1）
        let (r, g, b) = kelvin_to_rgb(6500);
        assert!((r - 1.0).abs() < 0.05);
        assert!((g - 1.0).abs() < 0.05);
        assert!((b - 1.0).abs() < 0.05);
        // 3400K（夜间预设）暖色：R > G > B
        let (r2, g2, b2) = kelvin_to_rgb(3400);
        assert!(r2 > g2 && g2 > b2);
        // 低色温蓝通道接近 0
        let (_, _, b3) = kelvin_to_rgb(1000);
        assert!(b3 < 0.05);
        // clamp 上限 10000K：Tanner 算法在 10000K 时 red≈0.79（非 1.0）
        let (r4, _, _) = kelvin_to_rgb(20000);
        assert!((r4 - 0.791).abs() < 0.01, "r4={r4}");
        // 6500K 是全通道接近 255 的平衡点（Tanner blue≈250/255）
        let (r5, g5, b5) = kelvin_to_rgb(6500);
        assert!((r5 - 1.0).abs() < 0.01 && (g5 - 1.0).abs() < 0.01 && b5 > 0.97);
        // 色温单调性：越高越蓝（blue 占比上升）
        let (_, _, b_low) = kelvin_to_rgb(3400);
        let (_, _, b_high) = kelvin_to_rgb(6500);
        assert!(b_high > b_low);
    }

    #[test]
    fn ramp_respects_brightness_and_floor() {
        let r = build_ramp(4500, 0.5, 0.35);
        // 中点值应低于 identity（压暗）
        let id = RampChannel::identity();
        assert!(r.red.0[128] < id.0[128]);
        // floor 作用于通道因子（中高灰度），不抬升 sample 0
        for ch in [&r.red, &r.green, &r.blue] {
            let min_ratio = (128..256)
                .map(|i| ch.0[i] as f64 / id.0[i] as f64)
                .fold(1.0, f64::min);
            assert!(min_ratio >= 0.35 - 1e-3, "floor violated: {min_ratio}");
        }
    }

    #[test]
    fn full_brightness_near_identity() {
        // 6500K + brightness 1.0 + floor 0.35 → 接近 identity（暗部不被抬升）
        let r = build_ramp(6500, 1.0, 0.35);
        let diff = r.mean_abs_diff(&Ramp::identity());
        assert!(diff < 0.03, "unexpected deviation {diff}");
        // index 0 保持接近 0，而不是被 floor 抬到 ~0.35
        for ch in [&r.red, &r.green, &r.blue] {
            let v0 = ch.0[0] as f64 / 65535.0;
            assert!(v0 < 0.01, "index 0 should stay near 0, got {v0}");
        }
    }

    #[test]
    fn lerp_midpoint() {
        let a = Ramp::identity();
        let mut b = Ramp::identity();
        b.red.0[0] = 0;
        let mid = lerp_ramp(&a, &b, 0.5);
        assert!((mid.red.0[0] as f64 - (a.red.0[0] as f64 + b.red.0[0] as f64) / 2.0).abs() < 1.0);
        // t=0 / t=1 边界
        let t0 = lerp_ramp(&a, &b, 0.0);
        assert_eq!(t0, a);
        let t1 = lerp_ramp(&a, &b, 1.0);
        assert_eq!(t1, b);
    }

    #[test]
    fn rate_limiter_12hz() {
        let mut rl = RateLimiter::twelve_hz();
        assert!(rl.should_emit(0));
        assert!(!rl.should_emit(50));
        assert!(!rl.should_emit(82));
        assert!(rl.should_emit(83));
        assert!(!rl.should_emit(100));
        assert!(rl.should_emit(200));
    }

    #[test]
    fn rate_limiter_force() {
        let mut rl = RateLimiter::twelve_hz();
        assert!(rl.should_emit(0));
        rl.force(100);
        assert!(rl.should_emit(101));
    }
}
