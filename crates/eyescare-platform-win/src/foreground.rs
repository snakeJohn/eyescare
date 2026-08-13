//! Windows 前台应用采样（设计 §5.1 / 附录 C）。
//!
//! 常规 API：`GetForegroundWindow` → `GetWindowThreadProcessId` → `OpenProcess`
//! → `QueryFullProcessImageNameW`。MVP 不依赖 Accessibility。

use eyescare_platform::{
    ForegroundAppBackend, FullscreenConfidence, FullscreenKind, Result, SceneContext,
};
use windows::Win32::Foundation::{CloseHandle, HWND, MAX_PATH, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowPlacement, GetWindowRect, GetWindowThreadProcessId, IsIconic,
    SW_SHOWMAXIMIZED, SW_SHOWMINIMIZED, WINDOWPLACEMENT,
};

pub struct WindowsForegroundBackend;

impl ForegroundAppBackend for WindowsForegroundBackend {
    fn foreground(&self) -> Result<SceneContext> {
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.is_invalid() {
            // 无前台窗口（锁屏/桌面）
            return Ok(SceneContext {
                process_name: None,
                path_suffix: None,
                bundle_id: None,
                app_display_name: "Desktop".into(),
                window_title: None,
                is_fullscreen: false,
                fullscreen_confidence: FullscreenConfidence::Unknown,
                fullscreen_kind: FullscreenKind::None,
                display_id: None,
            });
        }

        let mut pid: u32 = 0;
        unsafe {
            let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
        }
        let (process_name, path_suffix) = process_name_of(pid);

        // 窗口标题 MVP 不参与匹配，跳过 GetWindowText 以免每秒分配。
        let window_title = None;

        // 全屏启发（§5.3）：最大化 Medium；rect≈monitor → borderless High
        let (is_fullscreen, confidence, kind) = fullscreen_heuristic(hwnd);

        let display_name = process_name
            .clone()
            .unwrap_or_else(|| "Unknown".into());

        Ok(SceneContext {
            process_name,
            path_suffix,
            bundle_id: None,
            app_display_name: display_name,
            window_title,
            is_fullscreen,
            fullscreen_confidence: confidence,
            fullscreen_kind: kind,
            display_id: None,
        })
    }
}

fn process_name_of(pid: u32) -> (Option<String>, Option<String>) {
    if pid == 0 {
        return (None, None);
    }
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) };
    let Ok(handle) = handle else {
        return (None, None);
    };
    let mut buf = [0u16; MAX_PATH as usize];
    let mut size = buf.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
    };
    unsafe { let _ = CloseHandle(handle); }
    if ok.is_err() {
        return (None, None);
    }
    let full = String::from_utf16_lossy(&buf[..size as usize]);
    let name = full.rsplit(['\\', '/']).next().map(|s| s.to_string());
    (name, Some(full))
}

/// 全屏启发（§5.3 Win 表）。
fn fullscreen_heuristic(hwnd: HWND) -> (bool, FullscreenConfidence, FullscreenKind) {
    // 最小化 → 不算
    if unsafe { IsIconic(hwnd) }.as_bool() {
        return (false, FullscreenConfidence::Unknown, FullscreenKind::None);
    }
    let mut placement = WINDOWPLACEMENT {
        length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    let ok = unsafe { GetWindowPlacement(hwnd, &mut placement) };
    if ok.is_err() {
        return (false, FullscreenConfidence::Unknown, FullscreenKind::None);
    }
    if placement.showCmd == SW_SHOWMINIMIZED.0 as u32 {
        return (false, FullscreenConfidence::Unknown, FullscreenKind::None);
    }
    if placement.showCmd == SW_SHOWMAXIMIZED.0 as u32 {
        // 最大化：Medium（§5.3）
        return (true, FullscreenConfidence::Medium, FullscreenKind::Maximized);
    }

    // 无边框：窗 rect ≈ monitor rect（无 thick frame 用 rect 覆盖粗判）
    let mut rect = RECT::default();
    unsafe { let _ = GetWindowRect(hwnd, &mut rect); }
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_invalid() {
        return (false, FullscreenConfidence::Unknown, FullscreenKind::None);
    }
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let ok = unsafe { GetMonitorInfoW(monitor, &mut mi) };
    if !ok.as_bool() {
        return (false, FullscreenConfidence::Unknown, FullscreenKind::None);
    }
    let m = mi.rcMonitor;
    let cover = rect.left <= m.left
        && rect.top <= m.top
        && rect.right >= m.right
        && rect.bottom >= m.bottom;
    if cover {
        // 盖住工作区 → borderless High（独占全屏无法从窗口 API 区分，UI 需提示）
        (true, FullscreenConfidence::High, FullscreenKind::Borderless)
    } else {
        (false, FullscreenConfidence::Unknown, FullscreenKind::None)
    }
}
