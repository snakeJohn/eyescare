//! Windows GDI Gamma 后端（设计 §4.3）。
//!
//! 主路径：`EnumDisplayDevicesW` → `CreateDC` → `SetDeviceGammaRamp` → readback `GetDeviceGammaRamp`。
//! 稳定 ID：监视器 DeviceKey / DeviceID 派生，热插拔后模糊匹配 DeviceString。
//! HDR：GetDisplayConfigBufferSizes + QueryDisplayConfig，按 target 查 advanced color；失败时 Ok(false)。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use eyescare_platform::{
    ApplyOutcome, ApplyReport, DisplayBackend, DisplayId, DisplayInfo, Error, Ramp, Result,
};
use windows::core::PCWSTR;
use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
    DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
    QDC_ONLY_ACTIVE_PATHS,
};
use windows::Win32::Graphics::Gdi::{
    CreateDCW, DeleteDC, EnumDisplayDevicesW, GetDeviceCaps, DISPLAY_DEVICEW, HDC, HORZRES, VERTRES,
};
use windows::Win32::UI::ColorSystem::{GetDeviceGammaRamp, SetDeviceGammaRamp};

/// readback 校验阈值：逐通道平均绝对差 > 2% 视为未应用（设计 §4.3）。
const READBACK_EPS: f64 = 0.02;

/// 固定长度 UTF-16 缓冲 → String（截到首个 \0）。
fn utf16_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// 稳定 ID：优先监视器 DeviceKey，其次监视器 DeviceID，再次 DeviceName。
/// `adapter_device_id` 仅为最后兜底（同一 GPU 上多屏会碰撞，不能优先）。
fn stable_id(
    adapter_device_id: &str,
    monitor_device_key: &str,
    monitor_device_id: &str,
    device_name: &str,
) -> String {
    let token = if !monitor_device_key.is_empty() {
        monitor_device_key
    } else if !monitor_device_id.is_empty() {
        monitor_device_id
    } else if !device_name.is_empty() {
        device_name
    } else {
        adapter_device_id
    };
    format!("win:{token}")
}

/// 每屏状态：原始 ramp 快照 + 当前是否施加。
#[derive(Debug)]
struct DisplayState {
    /// 稳定 ID（监视器 DeviceKey / DeviceID 派生）。
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
    /// HDR 状态缓存：按 DisplayId 分屏，5s 内复用（拓扑变化由 rebind 失效）。
    hdr_cache: Mutex<(std::time::Instant, HashMap<String, bool>)>,
    /// 默认 true（KD18 Skip）；用户 Force 时关闭。
    hdr_skip: AtomicBool,
}

impl WindowsDisplayBackend {
    pub fn new() -> Result<Self> {
        let backend = Self {
            states: Mutex::new(Vec::new()),
            hdr_cache: Mutex::new((
                std::time::Instant::now() - std::time::Duration::from_secs(6),
                HashMap::new(),
            )),
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
            let adapter_device_id = utf16_to_string(&dd.DeviceID);
            // 监视器级身份：再 Enum 一次 adapter DeviceName + 子设备 0
            let mut mon = DISPLAY_DEVICEW {
                cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
                ..Default::default()
            };
            let adapter_wide: Vec<u16> = device_name
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let mon_ok =
                unsafe { EnumDisplayDevicesW(PCWSTR(adapter_wide.as_ptr()), 0, &mut mon, 0) };
            let (monitor_key, monitor_id) = if mon_ok.as_bool() {
                (
                    utf16_to_string(&mon.DeviceKey),
                    utf16_to_string(&mon.DeviceID),
                )
            } else {
                (String::new(), String::new())
            };
            let stable = stable_id(&adapter_device_id, &monitor_key, &monitor_id, &device_name);
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
        let wide: Vec<u16> = device_name
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
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

    /// HDR 检测：GetDisplayConfigBufferSizes + QueryDisplayConfig（尽力，Win10 1703+）。
    /// advanced color 查 **target**（非 source）。键为 GDI 设备名（`\\.\DISPLAYn`）。
    /// 探测失败 → 空表，调用方对该屏视为 false（用户 Force 仍可通过 hdr_skip=false）。
    pub fn detect_hdr_via_displayconfig() -> Result<HashMap<String, bool>> {
        let mut num_paths: u32 = 0;
        let mut num_modes: u32 = 0;
        let size_err = unsafe {
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut num_paths, &mut num_modes)
        };
        if size_err.0 != 0 || num_paths == 0 || num_paths > 64 {
            return Ok(HashMap::new());
        }
        let mut paths: Vec<DISPLAYCONFIG_PATH_INFO> = vec![Default::default(); num_paths as usize];
        let mut modes: Vec<DISPLAYCONFIG_MODE_INFO> = vec![Default::default(); num_modes as usize];
        // QDC_ONLY_ACTIVE_PATHS 时 currentTopologyId 必须为 NULL
        let err = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut num_paths,
                paths.as_mut_ptr(),
                &mut num_modes,
                modes.as_mut_ptr(),
                None,
            )
        };
        if err.0 != 0 {
            return Ok(HashMap::new());
        }
        paths.truncate(num_paths as usize);

        let mut out = HashMap::new();
        for path in paths.iter() {
            let target = path.targetInfo;
            let mut info = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    size: std::mem::size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>() as u32,
                    adapterId: target.adapterId,
                    id: target.id,
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
                },
                ..Default::default()
            };
            let ret = unsafe {
                DisplayConfigGetDeviceInfo(
                    &mut info.header as *mut DISPLAYCONFIG_DEVICE_INFO_HEADER,
                )
            };
            let hdr = if ret == 0 {
                // 位域（Win SDK）：bit0 = advancedColorSupported, bit1 = advancedColorEnabled
                let bits = unsafe { info.Anonymous.Anonymous._bitfield };
                bits & 0b10 != 0
            } else {
                false
            };

            let source = path.sourceInfo;
            let mut src_name = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    size: std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                    adapterId: source.adapterId,
                    id: source.id,
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                },
                ..Default::default()
            };
            let name_ret = unsafe {
                DisplayConfigGetDeviceInfo(
                    &mut src_name.header as *mut DISPLAYCONFIG_DEVICE_INFO_HEADER,
                )
            };
            if name_ret != 0 {
                continue;
            }
            let gdi_name = utf16_to_string(&src_name.viewGdiDeviceName);
            if !gdi_name.is_empty() {
                out.insert(gdi_name, hdr);
            }
        }
        Ok(out)
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
    /// 刷新分屏 HDR 缓存（不持有 states 锁时调用，避免与 rebind 死锁）。
    fn refresh_hdr_by_id(&self) -> HashMap<String, bool> {
        let by_gdi = Self::detect_hdr_via_displayconfig().unwrap_or_default();
        let states = self.states.lock().unwrap();
        states
            .iter()
            .map(|s| {
                (
                    s.id.0.clone(),
                    by_gdi.get(&s.device_name).copied().unwrap_or(false),
                )
            })
            .collect()
    }

    /// 带 5s 缓存的分屏 HDR 检测。未知 id / 探测失败 → false。
    fn hdr_active_for(&self, id: &DisplayId) -> bool {
        {
            let cache = self.hdr_cache.lock().unwrap();
            if cache.0.elapsed() < std::time::Duration::from_secs(5) {
                return cache.1.get(&id.0).copied().unwrap_or(false);
            }
        }
        let mapped = self.refresh_hdr_by_id();
        let result = mapped.get(&id.0).copied().unwrap_or(false);
        if let Ok(mut cache) = self.hdr_cache.lock() {
            cache.0 = std::time::Instant::now();
            cache.1 = mapped;
        }
        result
    }

    fn invalidate_hdr_cache(&self) {
        if let Ok(mut cache) = self.hdr_cache.lock() {
            cache.0 = std::time::Instant::now() - std::time::Duration::from_secs(6);
            cache.1.clear();
        }
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
        let mut out = Vec::with_capacity(snapshot.len());
        for (id, device_name, is_primary) in snapshot {
            let hdc = Self::open_dc(&device_name)?;
            let (w, h) = unsafe { (GetDeviceCaps(hdc, HORZRES), GetDeviceCaps(hdc, VERTRES)) };
            unsafe {
                let _ = DeleteDC(hdc);
            }
            let hdr = self.hdr_active_for(&id);
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
        if self.hdr_skip.load(Ordering::Relaxed) && self.hdr_active_for(id) {
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
                    unsafe {
                        let _ = DeleteDC(hdc);
                    }
                }
            }
        }
        *states = fresh;
        drop(states);
        self.invalidate_hdr_cache();
        Ok(())
    }

    fn detect_hdr_active(&self, id: &DisplayId) -> Result<bool> {
        Ok(self.hdr_active_for(id))
    }

    fn set_hdr_skip(&self, skip: bool) {
        self.hdr_skip.store(skip, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::stable_id;

    const ADAPTER: &str = r"PCI\VEN_10DE&DEV_2684&SUBSYS_51011462&REV_A1";

    #[test]
    fn stable_id_same_adapter_different_device_key() {
        let a = stable_id(
            ADAPTER,
            r"\Registry\Machine\System\CurrentControlSet\Control\Video\{11111111-1111-1111-1111-111111111111}\0000",
            r"MONITOR\DEL40A6\{4d36e96e-e325-11ce-bfc1-08002be10318}\0000",
            r"\\.\DISPLAY1",
        );
        let b = stable_id(
            ADAPTER,
            r"\Registry\Machine\System\CurrentControlSet\Control\Video\{11111111-1111-1111-1111-111111111111}\0001",
            r"MONITOR\ACR0521\{4d36e96e-e325-11ce-bfc1-08002be10318}\0001",
            r"\\.\DISPLAY2",
        );
        assert_ne!(a, b, "同卡双屏必须得到不同 DisplayId");
    }

    #[test]
    fn stable_id_prefers_device_key() {
        let a = stable_id(ADAPTER, r"KEY\0000", "MON_A", r"\\.\DISPLAY1");
        let b = stable_id(ADAPTER, r"KEY\0000", "MON_B", r"\\.\DISPLAY2");
        assert_eq!(a, b);
        assert_eq!(a, r"win:KEY\0000");
    }

    #[test]
    fn stable_id_falls_back_to_monitor_device_id() {
        let a = stable_id(ADAPTER, "", "MON_A", r"\\.\DISPLAY1");
        let b = stable_id(ADAPTER, "", "MON_B", r"\\.\DISPLAY2");
        assert_ne!(a, b);
        assert_eq!(a, "win:MON_A");
    }

    #[test]
    fn stable_id_falls_back_to_device_name() {
        let a = stable_id(ADAPTER, "", "", r"\\.\DISPLAY1");
        let b = stable_id(ADAPTER, "", "", r"\\.\DISPLAY2");
        assert_ne!(a, b);
        assert_eq!(a, r"win:\\.\DISPLAY1");
    }
}
