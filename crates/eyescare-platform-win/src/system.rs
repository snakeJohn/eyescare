//! Windows 系统后端（§6.3）：GetLastInputInfo + 自启 + 显示拓扑轮询。

use std::thread;
use std::time::Duration;

use eyescare_platform::{
    Error, EventCallback, Result, Subscription, SystemBackend, SystemEvent,
};
use windows::core::PCWSTR;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_SET_VALUE, KEY_WRITE, REG_OPEN_CREATE_OPTIONS, REG_SAM_FLAGS, REG_SZ,
};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

/// 自启注册表路径（HKCU Run 键，MVP 不需要管理员）。
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "EyesCare";

pub struct WindowsSystemBackend;

impl WindowsSystemBackend {
    /// 显示器拓扑轮询：每 2s 检测显示器数量/设备名变化 → DisplayChanged。
    /// MVP 用轮询替代消息窗口（事件驱动在后续 PR 打磨，OQ I5）。
    fn topology_watcher(cb: EventCallback) {
        thread::Builder::new()
            .name("eyescare-topology-watch".into())
            .spawn(move || {
                let mut last = Self::topology_fingerprint();
                loop {
                    thread::sleep(Duration::from_secs(5));
                    let now = Self::topology_fingerprint();
                    if now != last {
                        last = now;
                        cb(SystemEvent::DisplayChanged);
                    }
                }
            })
            .ok();
    }

    fn topology_fingerprint() -> String {
        // 简化：显示器数量 + 设备名列表
        let mut out = Vec::new();
        let mut idx: u32 = 0;
        loop {
            let mut dd = windows::Win32::Graphics::Gdi::DISPLAY_DEVICEW {
                cb: std::mem::size_of::<windows::Win32::Graphics::Gdi::DISPLAY_DEVICEW>() as u32,
                ..Default::default()
            };
            let ok = unsafe {
                windows::Win32::Graphics::Gdi::EnumDisplayDevicesW(
                    PCWSTR::null(),
                    idx,
                    &mut dd,
                    0,
                )
            };
            if !ok.as_bool() {
                break;
            }
            idx += 1;
            if dd.StateFlags & 0x1 != 0 {
                // DISPLAY_DEVICE_ACTIVE
                let name = {
                    let end = dd
                        .DeviceName
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(dd.DeviceName.len());
                    String::from_utf16_lossy(&dd.DeviceName[..end])
                };
                out.push(name);
            }
        }
        out.join("|")
    }
}

impl SystemBackend for WindowsSystemBackend {
    fn seconds_since_input(&self) -> u64 {
        let mut info = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            ..Default::default()
        };
        let ok = unsafe { GetLastInputInfo(&mut info) };
        if !ok.as_bool() {
            return 0;
        }
        let now = unsafe { GetTickCount() } as u64;
        let last = info.dwTime as u64;
        now.saturating_sub(last) / 1000
    }

    fn set_auto_start(&self, on: bool) -> Result<()> {
        let wide: Vec<u16> = RUN_KEY.encode_utf16().chain(std::iter::once(0)).collect();
        let mut key: HKEY = HKEY::default();
        let err = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(wide.as_ptr()),
                0,
                PCWSTR::null(),
                REG_OPEN_CREATE_OPTIONS(0),
                REG_SAM_FLAGS(KEY_SET_VALUE.0 | KEY_WRITE.0),
                None,
                &mut key,
                None,
            )
        };
        if err.0 != 0 {
            return Err(Error::Platform(format!(
                "RegCreateKeyExW failed: {}" , err.0
            )));
        }
        let r = if on {
            // 自启命令：MVP 占位（OQ I4）。实际生效路径是 tauri-plugin-autostart
            // （前端设置页使用），它在安装时写入正确 exe 路径；此处为后备实现。
            let cmd: Vec<u16> = "EyesCare.exe --autostart"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let name: Vec<u16> = RUN_VALUE.encode_utf16().chain(std::iter::once(0)).collect();
            // REG_SZ 需要 UTF-16LE 完整字节（含 null terminator 2 字节）；
            // 不能用 `c as u8`（会丢高字节且长度错半）。
            let bytes: Vec<u8> = cmd.iter().flat_map(|&c| c.to_le_bytes()).collect();
            unsafe {
                RegSetValueExW(
                    key,
                    PCWSTR(name.as_ptr()),
                    0,
                    REG_SZ,
                    Some(&bytes),
                )
            }
        } else {
            let name: Vec<u16> = RUN_VALUE.encode_utf16().chain(std::iter::once(0)).collect();
            unsafe { RegDeleteValueW(key, PCWSTR(name.as_ptr())) }
        };
        unsafe { let _ = RegCloseKey(key); }
        if r.0 != 0 {
            return Err(Error::Platform(format!("registry write failed: {}" , r.0)));
        }
        Ok(())
    }

    fn on_power_and_display_events(&self, cb: EventCallback) -> Result<Subscription> {
        Self::topology_watcher(cb);
        Ok(Subscription::new())
    }
}
