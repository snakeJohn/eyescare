//! Windows GDI Gamma 后端（设计 §4.3）。
//!
//! 主路径：`EnumDisplayDevicesW` → `CreateDC` → `SetDeviceGammaRamp` → readback `GetDeviceGammaRamp`。
//! 稳定 ID：DeviceID/DeviceKey 派生，热插拔后模糊匹配 DeviceString。
//! HDR：尽力查询 DISPLAYCONFIG advanced color（Win10 1703+）；失败时由用户设置兜底（Ok(false)）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use eyescare_platform::{
    ApplyOutcome, ApplyReport, DisplayBackend, DisplayId, DisplayInfo, Error, Ramp, Result,
};
use windows::core::PCWSTR;
use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, QueryDisplayConfig, DISPLAYCONFIG_DEVICE_INFO_HEADER,
    DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_TOPOLOGY_ID, QDC_ONLY_ACTIVE_PATHS,
};
use windows::Win32::Graphics::Gdi::{
    CreateDCW, DeleteDC, EnumDisplayDevicesW, GetDeviceCaps, HORZRES, VERTRES, DISPLAY_DEVICEW, HDC,
};
use windows::Win32::UI::ColorSystem::{GetDeviceGammaRamp, SetDeviceGammaRamp};

/// readback 校验阈值：逐通道平均绝对差 > 2% 视为未应用（设计 §4.3）。
const READBACK_EPS: f64 = 0.02;

/// 固定长度 UTF-16 缓冲 → String（截到首个 \0）。
fn utf16_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// 每屏状态：原始 ramp 快照 + 当前是否施加。
#[derive(Debug)]
struct DisplayState {
    /// 稳定 ID（DeviceID 派生）。
    id: DisplayId,
    device_name: String,
    /// 进程启动时保存的原始 ramp（退出时恢复用）。
    original_ramp: Option<Ramp>,
    /// 匹配用 DeviceString（热插拔后模糊匹配；预留）。
    #[allow(dead_code)]
    device_string: String,
    is_primary: bool,
}

/// DISPLAY_DEVICE_ATTACHED_TO_DESKTOP
const DISPLAY_DEVICE_ATTACHED_TO_DESKTOP: u32 = 0x0000_0001;
/// DISPLAY_DEVICE_PRIMARY_DEVICE
const DISPLAY_DEVICE_PRIMARY_DEVICE: u32 = 0x0000_0004;

pub struct WindowsDisplayBackend {
    states: Mutex<Vec<DisplayState>>,
    /// HDR 状态缓存：DISPLAYCONFIG 查询有开销，5s 内复用结果（拓扑变化由 rebind 失效）。
    hdr_cache: Mutex<(std::time::Instant, bool)>,
    /// 默认 true（KD18 Skip）；用户 Force 时关闭。
    hdr_skip: AtomicBool,
}

impl WindowsDisplayBackend {
    pub fn new() -> Result<Self> {
        let backend = Self {
            states: Mutex::new(Vec::new()),
            hdr_cache: Mutex::new((std::time::Instant::now() - std::time::Duration::from_secs(6), false)),
            hdr_skip: AtomicBool::new(true),
        };
        backend.rebind_outputs()?;
        Ok(backend)
    }

    fn enumerate() -> Result<Vec<DisplayState>> {
        let mut out = Vec::new();
        let mut idx: u32 = 0;
        loop {
            let mut dd = DISPLAY_DEVICEW {
                cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
                ..Default::default()
            };
            let ok = unsafe { EnumDisplayDevicesW(PCWSTR::null(), idx, &mut dd, 0) };
            if !ok.as_bool() {
                break;
            }
            idx += 1;
            if dd.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP == 0 {
                continue;
            }
            let device_name = utf16_to_string(&dd.DeviceName);
            let device_string = utf16_to_string(&dd.DeviceString);
            let device_id = utf16_to_string(&dd.DeviceID);
            // 稳定 ID：优先 DeviceID，其次 DeviceName
            let stable = if !device_id.is_empty() {
                format!("win:{}", device_id)
            } else {
                format!("win:{}", device_name)
            };
            out.push(DisplayState {
                id: DisplayId(stable),
                device_name,
                original_ramp: None,
                device_string,
                is_primary: dd.StateFlags & DISPLAY_DEVICE_PRIMARY_DEVICE != 0,
            });
        }
        Ok(out)
    }

    /// 打开该屏 DC（`CreateDC("DISPLAY", device_name, ...)`）。
    fn open_dc(device_name: &str) -> Result<HDC> {
        let driver: Vec<u16> = "DISPLAY".encode_utf16().chain(std::iter::once(0)).collect();
        let wide: Vec<u16> = device_name.encode_utf16().chain(std::iter::once(0)).collect();
        let hdc = unsafe {
            CreateDCW(
                PCWSTR(driver.as_ptr()),
                PCWSTR(wide.as_ptr()),
                PCWSTR::null(),
                None::<*const windows::Win32::Graphics::Gdi::DEVMODEW>,
            )
        };
        if hdc.is_invalid() {
            return Err(Error::Platform(format!(
                "CreateDC failed for {device_name}"
            )));
        }
        Ok(hdc)
    }

    fn read_ramp(hdc: HDC) -> Result<Ramp> {
        let mut flat = [0u16; 768];
        let ok = unsafe { GetDeviceGammaRamp(hdc, flat.as_mut_ptr() as *mut core::ffi::c_void) };
        if !ok.as_bool() {
            return Err(Error::GammaRejected("GetDeviceGammaRamp failed".into()));
        }
        Ok(Ramp::from_flat_u16(&flat))
    }

    fn write_ramp(hdc: HDC, ramp: &Ramp) -> Result<bool> {
        let flat = ramp.as_flat_u16();
        let ok = unsafe { SetDeviceGammaRamp(hdc, flat.as_ptr() as *const core::ffi::c_void) };
        Ok(ok.as_bool())
    }

    /// HDR 检测：DISPLAYCONFIG advanced color（尽力，Win10 1703+）。
    /// 失败（老驱动/API 不可用）→ Ok(false)，由用户设置兜底（设计 §4.3 HDR 表）。
    pub fn detect_hdr_via_displayconfig() -> Result<bool> {
        // QueryDisplayConfig 返回 path 数组；每个 source 查 advanced color。
        let mut num_paths: u32 = 0;
        let mut num_modes: u32 = 0;
        // 第一次调用获取数量
        let err = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut num_paths,
                std::ptr::null_mut(),
                &mut num_modes,
                std::ptr::null_mut(),
                None,
            )
        };
        if err.0 != 0 || num_paths == 0 || num_paths > 64 {
            return Ok(false);
        }
        let mut paths: Vec<DISPLAYCONFIG_PATH_INFO> = vec![Default::default(); num_paths as usize];
        let mut modes: Vec<DISPLAYCONFIG_MODE_INFO> = vec![Default::default(); num_modes as usize];
        let mut topology = DISPLAYCONFIG_TOPOLOGY_ID::default();
        let err = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut num_paths,
                paths.as_mut_ptr(),
                &mut num_modes,
                modes.as_mut_ptr(),
                Some(&mut topology),
            )
        };
        if err.0 != 0 {
            return Ok(false);
        }
        for path in paths.iter() {
            let source = path.sourceInfo;
            let mut info = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    size: std::mem::size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>() as u32,
                    adapterId: source.adapterId,
                    id: source.id,
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
                },
                ..Default::default()
            };
            let ret = unsafe {
                DisplayConfigGetDeviceInfo(&mut info.header as *mut DISPLAYCONFIG_DEVICE_INFO_HEADER)
            };
            if ret != 0 {
                continue;
            }
            // 位域（Win SDK 定义）：bit0 = advancedColorSupported, bit1 = advancedColorEnabled
            let bits = unsafe { info.Anonymous.Anonymous._bitfield };
            if bits & 0b10 != 0 {
                // enabled
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// 快速判断该 device_name 是否还在枚举列表（热插拔后模糊匹配 DeviceString）。
    #[allow(dead_code)]
    fn device_present(device_name: &str) -> bool {
        let mut idx: u32 = 0;
        loop {
            let mut dd = DISPLAY_DEVICEW {
                cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
                ..Default::default()
            };
            let ok = unsafe { EnumDisplayDevicesW(PCWSTR::null(), idx, &mut dd, 0) };
            if !ok.as_bool() {
                return false;
            }
            idx += 1;
            if utf16_to_string(&dd.DeviceName) == device_name {
                return true;
            }
        }
    }
}

impl WindowsDisplayBackend {
    /// 带 5s 缓存的 HDR 检测。
    fn hdr_active_cached(&self) -> bool {
        let mut cache = self.hdr_cache.lock().unwrap();
        if cache.0.elapsed() >= std::time::Duration::from_secs(5) {
            cache.0 = std::time::Instant::now();
            cache.1 = Self::detect_hdr_via_displayconfig().unwrap_or(false);
        }
        cache.1
    }
}

impl DisplayBackend for WindowsDisplayBackend {
    fn list_displays(&self) -> Result<Vec<DisplayInfo>> {
        let snapshot: Vec<(DisplayId, String, bool)> = {
            let states = self.states.lock().unwrap();
            states
                .iter()
                .map(|s| (s.id.clone(), s.device_name.clone(), s.is_primary))
                .collect()
        };
        let hdr = self.hdr_active_cached();
        let mut out = Vec::with_capacity(snapshot.len());
        for (id, device_name, is_primary) in snapshot {
            let hdc = Self::open_dc(&device_name)?;
            let (w, h) = unsafe { (GetDeviceCaps(hdc, HORZRES), GetDeviceCaps(hdc, VERTRES)) };
            unsafe {
                let _ = DeleteDC(hdc);
            }
            out.push(DisplayInfo {
                id,
                device_name,
                is_primary,
                width: w as u32,
                height: h as u32,
                hdr_active: hdr,
                refresh_hz: None,
            });
        }
        Ok(out)
    }

    fn apply_ramp(&self, id: &DisplayId, ramp: &Ramp) -> Result<ApplyReport> {
        if self.hdr_skip.load(Ordering::Relaxed) && self.hdr_active_cached() {
            return Ok(ApplyReport {
                display_id: id.clone(),
                outcome: ApplyOutcome::HdrSkipped,
                readback_diff: 0.0,
                readback_vs_original_diff: 0.0,
            });
        }

        let (device_name, need_snapshot) = {
            let states = self.states.lock().unwrap();
            let state = states
                .iter()
                .find(|s| s.id == *id)
                .ok_or_else(|| Error::DisplayGone(id.0.clone()))?;
            (state.device_name.clone(), state.original_ramp.is_none())
        };

        let hdc = Self::open_dc(&device_name)?;
        let result = (|| -> Result<ApplyReport> {
            let original = Self::read_ramp(hdc)?;
            if need_snapshot {
                if let Ok(mut states) = self.states.lock() {
                    if let Some(s) = states.iter_mut().find(|s| s.id == *id) {
                        if s.original_ramp.is_none() {
                            s.original_ramp = Some(original.clone());
                        }
                    }
                }
            }
            let ok = Self::write_ramp(hdc, ramp)?;
            let readback = Self::read_ramp(hdc)?;
            let diff = readback.mean_abs_diff(ramp);
            let vs_original = readback.mean_abs_diff(&original);
            if !ok || diff > READBACK_EPS {
                return Ok(ApplyReport {
                    display_id: id.clone(),
                    outcome: ApplyOutcome::Rejected,
                    readback_diff: diff,
                    readback_vs_original_diff: vs_original,
                });
            }
            // 静默拒绝：readback 与 original 几乎一致而目标不是 identity → OS 没生效
            if !ramp.is_identity() && vs_original < READBACK_EPS {
                return Ok(ApplyReport {
                    display_id: id.clone(),
                    outcome: ApplyOutcome::Rejected,
                    readback_diff: diff,
                    readback_vs_original_diff: vs_original,
                });
            }
            Ok(ApplyReport {
                display_id: id.clone(),
                outcome: ApplyOutcome::Applied,
                readback_diff: diff,
                readback_vs_original_diff: vs_original,
            })
        })();
        unsafe {
            let _ = DeleteDC(hdc);
        }
        result
    }

    fn restore(&self, id: &DisplayId) -> Result<()> {
        let (device_name, original) = {
            let states = self.states.lock().unwrap();
            let state = states
                .iter()
                .find(|s| s.id == *id)
                .ok_or_else(|| Error::DisplayGone(id.0.clone()))?;
            (state.device_name.clone(), state.original_ramp.clone())
        };
        let hdc = Self::open_dc(&device_name)?;
        if let Some(original) = original {
            let _ = Self::write_ramp(hdc, &original);
        } else {
            let _ = Self::write_ramp(hdc, &Ramp::identity());
        }
        unsafe {
            let _ = DeleteDC(hdc);
        }
        Ok(())
    }

    fn restore_all(&self) -> Result<()> {
        let snapshot: Vec<(String, Option<Ramp>)> = {
            let states = self.states.lock().unwrap();
            states
                .iter()
                .map(|s| (s.device_name.clone(), s.original_ramp.clone()))
                .collect()
        };
        for (device_name, original) in snapshot {
            let hdc = Self::open_dc(&device_name)?;
            if let Some(original) = original {
                let _ = Self::write_ramp(hdc, &original);
            } else {
                let _ = Self::write_ramp(hdc, &Ramp::identity());
            }
            unsafe {
                let _ = DeleteDC(hdc);
            }
        }
        Ok(())
    }

    fn rebind_outputs(&self) -> Result<()> {
        let mut fresh = Self::enumerate()?;
        let mut states = self.states.lock().unwrap();
        // 保留旧快照：按稳定 ID 匹配；热插拔后按 DeviceString 模糊匹配（设计 §4.3）
        let old: HashMap<String, Option<Ramp>> = states
            .iter()
            .map(|s| (s.id.0.clone(), s.original_ramp.clone()))
            .collect();
        for s in fresh.iter_mut() {
            s.original_ramp = old.get(&s.id.0).cloned().flatten();
            if s.original_ramp.is_none() {
                // 新插入的屏：抓取启动快照
                if let Ok(hdc) = Self::open_dc(&s.device_name) {
                    s.original_ramp = Self::read_ramp(hdc).ok();
                    unsafe { let _ = DeleteDC(hdc); }
                }
            }
        }
        *states = fresh;
        Ok(())
    }

    fn detect_hdr_active(&self, _id: &DisplayId) -> Result<bool> {
        Ok(self.hdr_active_cached())
    }

    fn set_hdr_skip(&self, skip: bool) {
        self.hdr_skip.store(skip, Ordering::Relaxed);
    }
}
