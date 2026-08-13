//! 显示层：算法（A03）+ Resolver/Service/DayNight（A06）。

pub mod day_night;
pub mod math;
pub mod resolver;
pub mod service;

pub use math::{build_ramp, kelvin_to_rgb, lerp_ramp, RateLimiter};
pub use resolver::{Claim, Priority, Resolver, ResolvedTarget};
pub use service::{DisplayService, DisplayServiceError};
