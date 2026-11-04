//! 接続中モニターの列挙（Win32 → ドメイン型）

use crate::domain::monitor_layout::{sort_monitors, Monitor, Rect};
use crate::platform::layout::VirtualScreen;
use windows::Win32::Foundation::{BOOL, LPARAM, RECT, TRUE};
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, HDC, HMONITOR, MONITORINFO,
    MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForSystem, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, MONITORINFOF_PRIMARY, SM_CMONITORS, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplaySnapshot {
    pub monitor_count: i32,
    pub virtual_screen: VirtualScreen,
    pub monitors: Vec<Monitor>,
}

pub fn capture_display_snapshot() -> DisplaySnapshot {
    DisplaySnapshot {
        monitor_count: unsafe { GetSystemMetrics(SM_CMONITORS) },
        virtual_screen: unsafe { query_virtual_screen() },
        monitors: enumerate_monitors(),
    }
}

pub fn dpi_at_screen(x: i32, y: i32) -> u32 {
    unsafe {
        let monitor = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        if monitor.is_invalid() {
            return fallback_dpi();
        }
        let mut dpi_x = 0u32;
        let mut dpi_y = 0u32;
        match GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) {
            Ok(()) if dpi_x > 0 => dpi_x,
            _ => fallback_dpi(),
        }
    }
}

fn fallback_dpi() -> u32 {
    unsafe { GetDpiForSystem().max(96) }
}

pub fn enumerate_monitors() -> Vec<Monitor> {
    let mut monitors = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            HDC::default(),
            None,
            Some(enum_monitor),
            LPARAM(&mut monitors as *mut Vec<Monitor> as isize),
        );
    }
    sort_monitors(&mut monitors);
    monitors
}

unsafe extern "system" fn enum_monitor(
    monitor: HMONITOR,
    _dc: HDC,
    _clip: *mut RECT,
    data: LPARAM,
) -> BOOL {
    let list = &mut *(data.0 as *mut Vec<Monitor>);
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if !GetMonitorInfoW(
        monitor,
        &mut info as *mut MONITORINFOEXW as *mut MONITORINFO,
    )
    .as_bool()
    {
        return TRUE;
    }
    let rect = to_rect(info.monitorInfo.rcMonitor);
    if rect.is_valid() {
        list.push(Monitor {
            rect,
            work: to_rect(info.monitorInfo.rcWork),
            is_primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
        });
    }
    TRUE
}

fn to_rect(rect: RECT) -> Rect {
    Rect {
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
    }
}

unsafe fn query_virtual_screen() -> VirtualScreen {
    VirtualScreen {
        x: GetSystemMetrics(SM_XVIRTUALSCREEN),
        y: GetSystemMetrics(SM_YVIRTUALSCREEN),
        width: GetSystemMetrics(SM_CXVIRTUALSCREEN),
        height: GetSystemMetrics(SM_CYVIRTUALSCREEN),
    }
}
