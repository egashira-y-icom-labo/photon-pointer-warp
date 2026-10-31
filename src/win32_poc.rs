//! Phase 0 + PoC1: Raw Input 常駐 CLI（Windows のみ）

use std::io::{self, Write};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use photon_pointer_warp::domain::circle::{CircleDetector, DetectorConfig, MotionSample};
use windows::core::w;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, WPARAM, FALSE, TRUE};
use windows::Win32::System::Console::{SetConsoleCtrlHandler, CTRL_C_EVENT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
    RAWMOUSE, RIDEV_INPUTSINK, RID_INPUT, RIM_TYPEMOUSE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetCursorPos, GetMessageW, PostQuitMessage,
    RegisterClassW, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, MSG, SW_HIDE,
    WM_DESTROY, WM_INPUT, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP, HWND_MESSAGE,
};

const CLASS_NAME: windows::core::PCWSTR = w!("PhotonPointerWarpRawInput");

const RI_MOUSE_LEFT_BUTTON_DOWN: u16 = 0x0001;
const RI_MOUSE_LEFT_BUTTON_UP: u16 = 0x0002;
const RI_MOUSE_RIGHT_BUTTON_DOWN: u16 = 0x0004;
const RI_MOUSE_RIGHT_BUTTON_UP: u16 = 0x0008;
const RI_MOUSE_MIDDLE_BUTTON_DOWN: u16 = 0x0010;
const RI_MOUSE_MIDDLE_BUTTON_UP: u16 = 0x0020;

struct AppState {
    detector: CircleDetector,
    last_input_at: Option<Instant>,
    buttons_down: u32,
}

static APP: OnceLock<Mutex<AppState>> = OnceLock::new();

pub fn run() -> io::Result<()> {
    APP.get_or_init(|| {
        Mutex::new(AppState {
            detector: CircleDetector::new(DetectorConfig::default()),
            last_input_at: None,
            buttons_down: 0,
        })
    });

    unsafe {
        SetConsoleCtrlHandler(Some(console_ctrl_handler), true)
            .map_err(|e| io::Error::other(format!("SetConsoleCtrlHandler: {e}")))?;
    }

    println!("photon-pointer-warp PoC: Raw Input circle detector (background). Ctrl+C to exit.");
    io::stdout().flush()?;

    unsafe {
        let module = GetModuleHandleW(None)
            .map_err(|e| io::Error::other(format!("GetModuleHandleW: {e}")))?;
        let instance = windows::Win32::Foundation::HINSTANCE(module.0);

        let wc = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: instance,
            lpszClassName: CLASS_NAME,
            style: CS_HREDRAW | CS_VREDRAW,
            ..Default::default()
        };

        RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            CLASS_NAME,
            w!("photon-pointer-warp"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            None,
            instance,
            None,
        )
        .map_err(|e| io::Error::other(format!("CreateWindowExW: {e}")))?;

        register_raw_input(hwnd)?;

        let _ = ShowWindow(hwnd, SW_HIDE);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    Ok(())
}

unsafe fn register_raw_input(hwnd: HWND) -> io::Result<()> {
    let device = RAWINPUTDEVICE {
        usUsagePage: 0x01,
        usUsage: 0x02,
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: hwnd,
    };

    RegisterRawInputDevices(&[device], std::mem::size_of::<RAWINPUTDEVICE>() as u32)
        .map_err(|e| io::Error::other(format!("RegisterRawInputDevices: {e}")))?;
    Ok(())
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_INPUT => {
            if let Err(e) = handle_wm_input(lparam) {
                eprintln!("WM_INPUT error: {e}");
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn handle_wm_input(lparam: LPARAM) -> io::Result<()> {
    let hraw = HRAWINPUT(lparam.0 as _);
    let header_size = std::mem::size_of::<RAWINPUTHEADER>() as u32;

    let mut size = 0u32;
    let _ = GetRawInputData(
        hraw,
        RID_INPUT,
        None,
        &mut size,
        header_size,
    );

    if size == 0 {
        return Ok(());
    }

    let mut buffer = vec![0u8; size as usize];
    let written = GetRawInputData(
        hraw,
        RID_INPUT,
        Some(buffer.as_mut_ptr() as *mut _),
        &mut size,
        header_size,
    ) as usize;

    if written < std::mem::size_of::<RAWINPUTHEADER>() {
        return Ok(());
    }

    let raw = &*(buffer.as_ptr() as *const RAWINPUT);
    if raw.header.dwType != RIM_TYPEMOUSE.0 {
        return Ok(());
    }

    let mouse = raw.data.mouse;

    let mut guard = APP
        .get()
        .expect("APP initialized")
        .lock()
        .expect("APP mutex poisoned");

    apply_button_flags(&mut guard, &mouse);

    let dx = mouse.lLastX as f32;
    let dy = mouse.lLastY as f32;
    if dx == 0.0 && dy == 0.0 {
        return Ok(());
    }

    let now = Instant::now();
    let dt_millis = guard
        .last_input_at
        .map(|t| now.saturating_duration_since(t).as_millis() as u64)
        .unwrap_or(0);
    guard.last_input_at = Some(now);

    let sample = MotionSample {
        dx,
        dy,
        dt_millis: dt_millis.max(1),
    };

    if guard.detector.push(sample) {
        drop(guard);
        let mut point = POINT::default();
        GetCursorPos(&mut point)
            .map_err(|e| io::Error::other(format!("GetCursorPos: {e}")))?;
        println!(
            "★ Circle gesture detected! Origin: ({}, {})",
            point.x, point.y
        );
        let _ = io::stdout().flush();
    }

    Ok(())
}

fn apply_button_flags(state: &mut AppState, mouse: &RAWMOUSE) {
    let flags = unsafe { mouse.Anonymous.Anonymous.usButtonFlags };

    if flags & RI_MOUSE_LEFT_BUTTON_DOWN != 0 {
        state.buttons_down += 1;
    }
    if flags & RI_MOUSE_LEFT_BUTTON_UP != 0 {
        state.buttons_down = state.buttons_down.saturating_sub(1);
    }
    if flags & RI_MOUSE_RIGHT_BUTTON_DOWN != 0 {
        state.buttons_down += 1;
    }
    if flags & RI_MOUSE_RIGHT_BUTTON_UP != 0 {
        state.buttons_down = state.buttons_down.saturating_sub(1);
    }
    if flags & RI_MOUSE_MIDDLE_BUTTON_DOWN != 0 {
        state.buttons_down += 1;
    }
    if flags & RI_MOUSE_MIDDLE_BUTTON_UP != 0 {
        state.buttons_down = state.buttons_down.saturating_sub(1);
    }

    state
        .detector
        .set_drag_state(state.buttons_down > 0);
}

unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> BOOL {
    if ctrl_type == CTRL_C_EVENT {
        PostQuitMessage(0);
        return TRUE;
    }
    FALSE
}
