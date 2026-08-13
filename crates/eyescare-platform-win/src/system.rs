//! Windows 系统后端（§6.3）：GetLastInputInfo + 自启 + 显示拓扑轮询 + 电源/会话消息窗口。

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use eyescare_platform::{Error, EventCallback, Result, Subscription, SystemBackend, SystemEvent};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_SET_VALUE, KEY_WRITE, REG_OPEN_CREATE_OPTIONS, REG_SAM_FLAGS, REG_SZ,
};
use windows::Win32::System::RemoteDesktop::{
    WTSRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassW,
    TranslateMessage, HMENU, MSG, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, WINDOW_EX_STYLE,
    WM_POWERBROADCAST, WM_WTSSESSION_CHANGE, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_POPUP, WTS_SESSION_UNLOCK,
};

/// 自启注册表路径（HKCU Run 键，MVP 不需要管理员）。
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "EyesCare";
const POWER_SESSION_CLASS: &str = "EyesCarePowerSession";

type SharedEventCb = Arc<dyn Fn(SystemEvent) + Send + Sync>;

thread_local! {
    static POWER_CB: std::cell::RefCell<Option<SharedEventCb>> = const { std::cell::RefCell::new(None) };
}

/// GetTickCount 环绕安全空闲秒数。
fn idle_secs(now_ms: u32, last_ms: u32) -> u64 {
    now_ms.wrapping_sub(last_ms) as u64 / 1000
}

fn system_event_from_win_msg(msg: u32, wparam: usize) -> Option<SystemEvent> {
    match msg {
        WM_POWERBROADCAST => match wparam as u32 {
            PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => Some(SystemEvent::PowerResumed),
            _ => None,
        },
        WM_WTSSESSION_CHANGE if wparam as u32 == WTS_SESSION_UNLOCK => {
            Some(SystemEvent::SessionUnlocked)
        }
        _ => None,
    }
}

unsafe extern "system" fn power_session_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if let Some(ev) = system_event_from_win_msg(msg, wparam.0) {
        POWER_CB.with(|slot| {
            if let Some(cb) = slot.borrow().as_ref() {
                cb(ev);
            }
        });
        if msg == WM_POWERBROADCAST {
            return LRESULT(1);
        }
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

pub struct WindowsSystemBackend;

impl WindowsSystemBackend {
    /// 显示器拓扑轮询：每 2s 检测显示器数量/设备名变化 → DisplayChanged。
    /// 订阅 Drop 不终止本线程。
    fn topology_watcher(cb: SharedEventCb) {
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

    /// 隐藏消息窗口：电源恢复 / 会话解锁。
    /// 订阅 Drop 不终止本线程（与 topology_watcher 相同）。
    fn power_session_watcher(cb: SharedEventCb) {
        thread::Builder::new()
            .name("eyescare-power-session".into())
            .spawn(move || run_power_session_loop(cb))
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
                windows::Win32::Graphics::Gdi::EnumDisplayDevicesW(PCWSTR::null(), idx, &mut dd, 0)
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
        idle_secs(unsafe { GetTickCount() }, info.dwTime)
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
                "RegCreateKeyExW failed: {}",
                err.0
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
            unsafe { RegSetValueExW(key, PCWSTR(name.as_ptr()), 0, REG_SZ, Some(&bytes)) }
        } else {
            let name: Vec<u16> = RUN_VALUE.encode_utf16().chain(std::iter::once(0)).collect();
            unsafe { RegDeleteValueW(key, PCWSTR(name.as_ptr())) }
        };
        unsafe {
            let _ = RegCloseKey(key);
        }
        if r.0 != 0 {
            return Err(Error::Platform(format!("registry write failed: {}", r.0)));
        }
        Ok(())
    }

    fn on_power_and_display_events(&self, cb: EventCallback) -> Result<Subscription> {
        // 订阅 Drop 不终止 watcher 线程（既有 topology 轮询同样如此）。
        let cb: SharedEventCb = Arc::from(cb);
        Self::topology_watcher(Arc::clone(&cb));
        Self::power_session_watcher(cb);
        Ok(Subscription::new())
    }
}

fn run_power_session_loop(cb: SharedEventCb) {
    POWER_CB.with(|slot| *slot.borrow_mut() = Some(cb));

    let class_name: Vec<u16> = POWER_SESSION_CLASS
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let wc = WNDCLASSW {
        lpfnWndProc: Some(power_session_wnd_proc),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    unsafe {
        let _ = RegisterClassW(&wc);
    }

    // 顶层隐藏窗：WM_POWERBROADCAST 不会投递给 HWND_MESSAGE。
    let hwnd = match unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0),
            PCWSTR(class_name.as_ptr()),
            PCWSTR::null(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            HWND::default(),
            HMENU::default(),
            HINSTANCE::default(),
            None,
        )
    } {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("CreateWindowExW(power/session) failed: {e}");
            return;
        }
    };

    if let Err(e) = unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) } {
        tracing::warn!("WTSRegisterSessionNotification failed: {e}");
    }

    loop {
        let mut msg = MSG::default();
        let ret = unsafe { GetMessageW(&mut msg, HWND::default(), 0, 0) };
        if ret.0 == 0 || ret.0 == -1 {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::idle_secs;

    #[test]
    fn idle_secs_normal_delta() {
        assert_eq!(idle_secs(2000, 1000), 1);
    }

    #[test]
    fn idle_secs_tick_count_wrap() {
        let last = u32::MAX - 900;
        let got = idle_secs(100, last);
        assert_ne!(got, 0, "wrap must not collapse to 0");
        assert!(got < 5, "wrap idle should be ~1s, got {got}");
    }

    #[test]
    fn power_resume_and_unlock_map_from_win_msg() {
        use super::system_event_from_win_msg;
        use eyescare_platform::SystemEvent;
        use windows::Win32::UI::WindowsAndMessaging::{
            PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, WM_POWERBROADCAST, WM_WTSSESSION_CHANGE,
            WTS_SESSION_UNLOCK,
        };
        assert_eq!(
            system_event_from_win_msg(WM_POWERBROADCAST, PBT_APMRESUMEAUTOMATIC as usize),
            Some(SystemEvent::PowerResumed)
        );
        assert_eq!(
            system_event_from_win_msg(WM_POWERBROADCAST, PBT_APMRESUMESUSPEND as usize),
            Some(SystemEvent::PowerResumed)
        );
        assert_eq!(
            system_event_from_win_msg(WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK as usize),
            Some(SystemEvent::SessionUnlocked)
        );
        assert_eq!(system_event_from_win_msg(WM_POWERBROADCAST, 0), None);
    }
}
