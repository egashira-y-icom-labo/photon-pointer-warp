//! `--calibrate` 用の純 Win32 / GDI キャリブレーションウィンドウ

use std::io;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use crate::domain::calibration::{
    analyze_relative_stroke, center_path_on_bounds, is_usable_calibration_stroke,
    recommend_detector_config, CalibrateGate, CalibrateGateAction, CalibratePhase, StrokeMetrics,
};
use crate::domain::circle::DetectorConfig;
use crate::domain::config::AppConfig;
use windows::core::w;
use windows::Win32::Foundation::{
    CloseHandle, COLORREF, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreatePen,
    CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect, GetMonitorInfoW,
    InvalidateRect, LineTo, MonitorFromPoint, MoveToEx, ScreenToClient, SelectObject, SetBkMode,
    SetTextColor, DEFAULT_CHARSET, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE,
    DT_WORDBREAK, FW_NORMAL, HDC, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT, PS_SOLID,
    SRCCOPY, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
    RID_INPUT, RIM_TYPEMOUSE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRect, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetClientRect, GetCursorPos, GetMessageW, GetSystemMetrics, KillTimer, LoadCursorW,
    PostQuitMessage, RegisterClassW, SetTimer, ShowWindow, TranslateMessage, CS_HREDRAW, CS_OWNDC,
    CS_VREDRAW, HMENU, IDC_ARROW, MSG, SM_CXSCREEN, SM_CYSCREEN, SW_SHOW, WM_CLOSE, WM_COMMAND,
    WM_DESTROY, WM_ERASEBKGND, WM_INPUT, WM_MOUSEMOVE, WM_PAINT, WM_TIMER, WNDCLASSW, WS_CAPTION,
    WS_CHILD, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};

pub const CALIBRATE_MUTEX_NAME: windows::core::PCWSTR = w!("Local\\PhotonPointerWarp.Calibrating");
const CLASS_NAME: windows::core::PCWSTR = w!("PhotonPointerWarpCalibrate");
const CLIENT_W: i32 = 460;
const CLIENT_H: i32 = 600;
const CANVAS_SIZE: i32 = 260;
const CANVAS_TOP: i32 = 78;
const TIMER_ID: usize = 1;
const TIMER_MS: u32 = 50;
const IDC_RESET: i32 = 101;
const IDC_SAVE: i32 = 102;
const IDC_CANCEL: i32 = 103;
const RECENT_LIST: usize = 5;

const COLOR_BG: (u8, u8, u8) = (30, 30, 30);
const COLOR_CANVAS: (u8, u8, u8) = (24, 24, 24);
const COLOR_TEXT: (u8, u8, u8) = (220, 220, 220);
const COLOR_DIM: (u8, u8, u8) = (140, 140, 140);
const COLOR_LINE: (u8, u8, u8) = (80, 210, 230);
const COLOR_FADE: (u8, u8, u8) = (50, 90, 100);
const COLOR_FRAME_IDLE: (u8, u8, u8) = (51, 51, 51);
const COLOR_FRAME_READY: (u8, u8, u8) = (0, 255, 255);

struct CalibrateSessionMutex {
    handle: HANDLE,
}

impl CalibrateSessionMutex {
    fn acquire() -> io::Result<Self> {
        let handle = unsafe { CreateMutexW(None, true, CALIBRATE_MUTEX_NAME) }
            .map_err(|e| io::Error::other(format!("CreateMutexW: {e}")))?;
        Ok(Self { handle })
    }
}

impl Drop for CalibrateSessionMutex {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

static STATE: OnceLock<Mutex<CalibrateState>> = OnceLock::new();

// HWND はメッセージスレッドのみで使う
unsafe impl Send for CalibrateState {}

struct CalibrateState {
    started: Instant,
    current: Vec<(f32, f32, u64)>,
    last_input_at: Option<Instant>,
    accepted: Vec<StrokeMetrics>,
    last_path: Vec<(f32, f32)>,
    record_origin: Option<(i32, i32)>,
    recommended: Option<DetectorConfig>,
    recommend_error: Option<&'static str>,
    save_hwnd: HWND,
    gate: CalibrateGate,
}

impl CalibrateState {
    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn reset(&mut self) {
        self.current.clear();
        self.last_input_at = None;
        self.accepted.clear();
        self.last_path.clear();
        self.record_origin = None;
        self.recommended = None;
        self.recommend_error = None;
        self.gate.reset();
    }

    fn refresh_recommendation(&mut self) {
        if self.accepted.len() < 3 {
            self.recommended = None;
            self.recommend_error = None;
            return;
        }
        match recommend_detector_config(&self.accepted) {
            Ok(cfg) => {
                self.recommended = Some(cfg);
                self.recommend_error = None;
            }
            Err(reason) => {
                self.recommended = None;
                self.recommend_error = Some(reason);
            }
        }
    }
}

pub fn run() -> io::Result<()> {
    let _session = CalibrateSessionMutex::acquire()?;
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let module = GetModuleHandleW(None)
            .map_err(|e| io::Error::other(format!("GetModuleHandleW: {e}")))?;
        let instance = HINSTANCE(module.0);
        let arrow = LoadCursorW(None, IDC_ARROW).unwrap_or_default();

        let wc = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: instance,
            lpszClassName: CLASS_NAME,
            style: CS_HREDRAW | CS_VREDRAW | CS_OWNDC,
            hCursor: arrow,
            ..Default::default()
        };
        RegisterClassW(&wc);

        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut wr = RECT {
            left: 0,
            top: 0,
            right: CLIENT_W,
            bottom: CLIENT_H,
        };
        let _ = AdjustWindowRect(&mut wr, style, false);
        let win_w = wr.right - wr.left;
        let win_h = wr.bottom - wr.top;
        let (x, y) = calibration_window_origin(win_w, win_h);

        let hwnd = CreateWindowExW(
            Default::default(),
            CLASS_NAME,
            w!("photon-pointer-warp - Calibrate"),
            style,
            x,
            y,
            win_w,
            win_h,
            None,
            None,
            instance,
            None,
        )
        .map_err(|e| io::Error::other(format!("CreateWindowExW: {e}")))?;

        let btn_y = CLIENT_H - 52;
        let btn_h = 32;
        let gap = 10;
        let btn_w = 140;
        let total_w = btn_w * 3 + gap * 2;
        let btn_x0 = (CLIENT_W - total_w) / 2;

        create_button(
            instance,
            hwnd,
            IDC_RESET,
            "やり直す (Reset)",
            btn_x0,
            btn_y,
            btn_w,
            btn_h,
        )?;
        let save_hwnd = create_button(
            instance,
            hwnd,
            IDC_SAVE,
            "適用して保存 (Save)",
            btn_x0 + btn_w + gap,
            btn_y,
            btn_w,
            btn_h,
        )?;
        create_button(
            instance,
            hwnd,
            IDC_CANCEL,
            "閉じる (Close)",
            btn_x0 + (btn_w + gap) * 2,
            btn_y,
            btn_w,
            btn_h,
        )?;
        let _ = EnableWindow(save_hwnd, false);

        STATE.get_or_init(|| {
            Mutex::new(CalibrateState {
                started: Instant::now(),
                current: Vec::new(),
                last_input_at: None,
                accepted: Vec::new(),
                last_path: Vec::new(),
                record_origin: None,
                recommended: None,
                recommend_error: None,
                save_hwnd,
                gate: CalibrateGate::new(),
            })
        });

        register_raw_input(hwnd)?;
        let _ = SetTimer(hwnd, TIMER_ID, TIMER_MS, None);
        let _ = ShowWindow(hwnd, SW_SHOW);

        println!("=== photon-pointer-warp calibrate ===");
        println!("Wait for cyan Ready, then draw circles. Save after 3 or more.");
        let _ = io::Write::flush(&mut io::stdout());

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

fn calibration_window_origin(win_w: i32, win_h: i32) -> (i32, i32) {
    unsafe { origin_on_cursor_work_area(win_w, win_h) }.unwrap_or_else(|| {
        let x = ((unsafe { GetSystemMetrics(SM_CXSCREEN) } - win_w) / 2).max(0);
        let y = ((unsafe { GetSystemMetrics(SM_CYSCREEN) } - win_h) / 2).max(0);
        (x, y)
    })
}

unsafe fn origin_on_cursor_work_area(win_w: i32, win_h: i32) -> Option<(i32, i32)> {
    let mut pt = POINT::default();
    GetCursorPos(&mut pt).ok()?;
    let monitor = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
    if monitor.0.is_null() {
        return None;
    }
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !GetMonitorInfoW(monitor, &mut info).as_bool() {
        return None;
    }
    let work = info.rcWork;
    let work_w = work.right - work.left;
    let work_h = work.bottom - work.top;
    if work_w <= 0 || work_h <= 0 {
        return None;
    }
    let mut x = work.left + (work_w - win_w) / 2;
    let mut y = work.top + (work_h - win_h) / 2;
    x = x.clamp(work.left, (work.right - win_w).max(work.left));
    y = y.clamp(work.top, (work.bottom - win_h).max(work.top));
    Some((x, y))
}

fn create_button(
    instance: HINSTANCE,
    parent: HWND,
    id: i32,
    label: &str,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> io::Result<HWND> {
    let wide: Vec<u16> = label.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        CreateWindowExW(
            Default::default(),
            w!("BUTTON"),
            windows::core::PCWSTR(wide.as_ptr()),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            x,
            y,
            w,
            h,
            parent,
            HMENU(id as *mut core::ffi::c_void),
            instance,
            None,
        )
        .map_err(|e| io::Error::other(format!("CreateWindowExW BUTTON: {e}")))
    }
}

unsafe fn register_raw_input(hwnd: HWND) -> io::Result<()> {
    let device = RAWINPUTDEVICE {
        usUsagePage: 0x01,
        usUsage: 0x02,
        dwFlags: Default::default(),
        hwndTarget: hwnd,
    };
    RegisterRawInputDevices(&[device], std::mem::size_of::<RAWINPUTDEVICE>() as u32)
        .map_err(|e| io::Error::other(format!("RegisterRawInputDevices: {e}")))
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_INPUT => {
            if let Err(e) = handle_wm_input(hwnd, lparam) {
                eprintln!("WM_INPUT error: {e}");
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            apply_gate(hwnd, None);
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == TIMER_ID {
                apply_gate(hwnd, None);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wparam.0 as u32) & 0xffff;
            match id as i32 {
                IDC_RESET => {
                    if let Some(mut g) = STATE.get().and_then(|s| s.lock().ok()) {
                        g.reset();
                        let _ = EnableWindow(g.save_hwnd, false);
                    }
                    let _ = InvalidateRect(hwnd, None, false);
                }
                IDC_SAVE => {
                    try_save_and_close(hwnd);
                }
                IDC_CANCEL => {
                    let _ = DestroyWindow(hwnd);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = KillTimer(hwnd, TIMER_ID);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn handle_wm_input(hwnd: HWND, lparam: LPARAM) -> io::Result<()> {
    let hraw = HRAWINPUT(lparam.0 as _);
    let header_size = std::mem::size_of::<RAWINPUTHEADER>() as u32;
    let mut size = 0u32;
    let _ = GetRawInputData(hraw, RID_INPUT, None, &mut size, header_size);
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
    let dx = raw.data.mouse.lLastX as f32;
    let dy = raw.data.mouse.lLastY as f32;
    if dx == 0.0 && dy == 0.0 {
        apply_gate(hwnd, None);
        return Ok(());
    }
    apply_gate(hwnd, Some((dx, dy)));
    Ok(())
}

fn canvas_rect_for_width(client_w: i32) -> RECT {
    let left = (client_w - CANVAS_SIZE) / 2;
    RECT {
        left,
        top: CANVAS_TOP,
        right: left + CANVAS_SIZE,
        bottom: CANVAS_TOP + CANVAS_SIZE,
    }
}

unsafe fn cursor_in_canvas(hwnd: HWND) -> bool {
    let mut client = RECT::default();
    if GetClientRect(hwnd, &mut client).is_err() {
        return false;
    }
    let canvas = canvas_rect_for_width(client.right);
    let mut pt = POINT::default();
    if GetCursorPos(&mut pt).is_err() {
        return false;
    }
    if !ScreenToClient(hwnd, &mut pt).as_bool() {
        return false;
    }
    pt.x >= canvas.left && pt.x < canvas.right && pt.y >= canvas.top && pt.y < canvas.bottom
}

unsafe fn cursor_client_pos(hwnd: HWND) -> Option<(i32, i32)> {
    let mut pt = POINT::default();
    GetCursorPos(&mut pt).ok()?;
    if !ScreenToClient(hwnd, &mut pt).as_bool() {
        return None;
    }
    Some((pt.x, pt.y))
}

fn apply_gate(hwnd: HWND, motion: Option<(f32, f32)>) {
    let inside = unsafe { cursor_in_canvas(hwnd) };
    let mut guard = match STATE.get().and_then(|s| s.lock().ok()) {
        Some(g) => g,
        None => return,
    };
    let now = Instant::now();
    let mut dirty = false;
    if !inside {
        if guard.gate.phase != CalibratePhase::Outside {
            guard.gate.note_outside();
            guard.current.clear();
            guard.record_origin = None;
            dirty = true;
        }
        drop(guard);
        if dirty {
            unsafe {
                let _ = InvalidateRect(hwnd, None, false);
            }
        }
        return;
    }

    let prev = guard.gate.phase;
    let action = guard.gate.note_inside(now, motion);
    if prev != guard.gate.phase {
        dirty = true;
    }
    match action {
        CalibrateGateAction::Ignore => {}
        CalibrateGateAction::StartRecording => {
            guard.current.clear();
            guard.record_origin = unsafe { cursor_client_pos(hwnd) };
            if let Some((dx, dy)) = motion {
                let t = guard.now_ms();
                guard.current.push((dx, dy, t));
                guard.last_input_at = Some(now);
            }
            dirty = true;
        }
        CalibrateGateAction::Record => {
            if let Some((dx, dy)) = motion {
                let t = guard.now_ms();
                guard.current.push((dx, dy, t));
                guard.last_input_at = Some(now);
            }
            dirty = true;
        }
        CalibrateGateAction::Finalize => {
            let stroke = std::mem::take(&mut guard.current);
            guard.last_input_at = None;
            guard.record_origin = None;
            if let Some(metrics) = analyze_relative_stroke(&stroke) {
                if is_usable_calibration_stroke(&metrics) {
                    guard.last_path = center_path_on_bounds(&integrate_path(&stroke));
                    guard.accepted.push(metrics);
                    guard.refresh_recommendation();
                } else {
                    guard.last_path.clear();
                }
            } else {
                guard.last_path.clear();
            }
            let can_save = guard.recommended.is_some();
            let save_hwnd = guard.save_hwnd;
            drop(guard);
            unsafe {
                let _ = EnableWindow(save_hwnd, can_save);
                let _ = InvalidateRect(hwnd, None, false);
            }
            return;
        }
    }
    drop(guard);
    if dirty {
        unsafe {
            let _ = InvalidateRect(hwnd, None, false);
        }
    }
}

fn try_save_and_close(hwnd: HWND) {
    let guard = match STATE.get().and_then(|s| s.lock().ok()) {
        Some(g) => g,
        None => return,
    };
    let Some(detector) = guard.recommended.clone() else {
        return;
    };
    drop(guard);
    let mut cfg = AppConfig::load_or_default();
    cfg.detector = detector;
    match cfg.save() {
        Ok(()) => {
            println!("[config] saved to {}", AppConfig::default_path().display());
            let _ = io::Write::flush(&mut io::stdout());
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        }
        Err(e) => {
            eprintln!("[config] save failed: {e}");
        }
    }
}

fn integrate_path(samples: &[(f32, f32, u64)]) -> Vec<(f32, f32)> {
    let mut pts = Vec::with_capacity(samples.len() + 1);
    let mut x = 0.0_f32;
    let mut y = 0.0_f32;
    pts.push((x, y));
    for (dx, dy, _) in samples {
        x += *dx;
        y += *dy;
        pts.push((x, y));
    }
    pts
}

fn rgb(c: (u8, u8, u8)) -> COLORREF {
    COLORREF(u32::from(c.0) | (u32::from(c.1) << 8) | (u32::from(c.2) << 16))
}

unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    if hdc.0.is_null() {
        return;
    }

    let mut client = RECT::default();
    let _ = GetClientRect(hwnd, &mut client);
    let width = client.right.max(1);
    let height = client.bottom.max(1);

    let mem = CreateCompatibleDC(hdc);
    let bmp = CreateCompatibleBitmap(hdc, width, height);
    let old_bmp = SelectObject(mem, bmp);

    let bg = CreateSolidBrush(rgb(COLOR_BG));
    FillRect(mem, &client, bg);

    let font = CreateFontW(
        18,
        0,
        0,
        0,
        FW_NORMAL.0 as i32,
        0,
        0,
        0,
        DEFAULT_CHARSET.0 as u32,
        0,
        0,
        0,
        0,
        w!("Segoe UI"),
    );
    let old_font = SelectObject(mem, font);
    SetBkMode(mem, TRANSPARENT);

    if let Some(g) = STATE.get().and_then(|s| s.lock().ok()) {
        paint_contents(mem, &g, width);
    }

    let _ = BitBlt(hdc, 0, 0, width, height, mem, 0, 0, SRCCOPY);

    SelectObject(mem, old_font);
    SelectObject(mem, old_bmp);
    let _ = DeleteObject(font);
    let _ = DeleteObject(bmp);
    let _ = DeleteObject(bg);
    let _ = DeleteDC(mem);
    let _ = EndPaint(hwnd, &ps);
}

unsafe fn paint_contents(hdc: HDC, state: &CalibrateState, client_w: i32) {
    SetTextColor(hdc, rgb(COLOR_TEXT));
    draw_text(
        hdc,
        20,
        14,
        client_w - 40,
        52,
        "キャンバス内で止まって枠がシアンになったら、普段の感覚で回してください",
        true,
    );

    let canvas = canvas_rect_for_width(client_w);
    let canvas_brush = CreateSolidBrush(rgb(COLOR_CANVAS));
    FillRect(hdc, &canvas, canvas_brush);
    let _ = DeleteObject(canvas_brush);

    let ready = matches!(
        state.gate.phase,
        CalibratePhase::Ready | CalibratePhase::Recording
    );
    let frame_color = if ready {
        COLOR_FRAME_READY
    } else {
        COLOR_FRAME_IDLE
    };
    let frame_w = if ready { 2 } else { 1 };
    let frame = CreatePen(PS_SOLID, frame_w, rgb(frame_color));
    let old_pen = SelectObject(hdc, frame);
    let _ = MoveToEx(hdc, canvas.left, canvas.top, None);
    let _ = LineTo(hdc, canvas.right - 1, canvas.top);
    let _ = LineTo(hdc, canvas.right - 1, canvas.bottom - 1);
    let _ = LineTo(hdc, canvas.left, canvas.bottom - 1);
    let _ = LineTo(hdc, canvas.left, canvas.top);
    SelectObject(hdc, old_pen);
    let _ = DeleteObject(frame);

    if state.gate.phase == CalibratePhase::Ready {
        SetTextColor(hdc, rgb(COLOR_FRAME_READY));
        draw_text(
            hdc,
            canvas.left + 8,
            canvas.top + 10,
            CANVAS_SIZE - 16,
            40,
            "Ready: 自由な場所・向きで回してください",
            true,
        );
    }

    if !state.last_path.is_empty() && state.gate.phase != CalibratePhase::Recording {
        stroke_polyline_centered(hdc, &canvas, &state.last_path, COLOR_FADE, 1);
    }
    if state.gate.phase == CalibratePhase::Recording {
        let live = integrate_path(&state.current);
        if live.len() > 1 {
            let origin = state.record_origin.unwrap_or((
                (canvas.left + canvas.right) / 2,
                (canvas.top + canvas.bottom) / 2,
            ));
            stroke_polyline_from_origin(hdc, &canvas, origin, &live, COLOR_LINE, 2);
        }
    }

    let list_top = canvas.bottom + 14;
    SetTextColor(hdc, rgb(COLOR_TEXT));
    draw_text(
        hdc,
        24,
        list_top,
        client_w - 48,
        22,
        &format!(
            "Samples: {} 回記録済み（納得いくまで回せます）",
            state.accepted.len()
        ),
        false,
    );
    SetTextColor(hdc, rgb(COLOR_DIM));
    let start = state.accepted.len().saturating_sub(RECENT_LIST);
    for (offset, m) in state.accepted[start..].iter().enumerate() {
        let n = start + offset + 1;
        let line = format!(
            "#{}: {}ms | {:.0}° | aspect {:.1}",
            n, m.elapsed_ms, m.net_angle_deg, m.aspect_ratio
        );
        draw_text(
            hdc,
            24,
            list_top + 24 + offset as i32 * 20,
            client_w - 48,
            20,
            &line,
            false,
        );
    }

    let rec_top = CLIENT_H - 108;
    SetTextColor(hdc, rgb(COLOR_TEXT));
    let rec = if let Some(cfg) = &state.recommended {
        format!(
            "推奨設定: 窓 {}ms / 角 {:.0}° / 楕円比 {:.1}",
            cfg.window_ms, cfg.min_total_angle_deg, cfg.ellipse_major_over_minor_max
        )
    } else if let Some(err) = state.recommend_error {
        format!("推奨を出せません: {}", recommend_error_ja(err))
    } else if state.accepted.len() < 3 {
        "推奨設定: 3 回以上描くとここに表示されます".to_string()
    } else {
        "推奨設定: 算出中".to_string()
    };
    draw_text(hdc, 24, rec_top, client_w - 48, 36, &rec, false);
}

fn recommend_error_ja(reason: &str) -> &'static str {
    match reason {
        "mixed_direction" => "回転方向が混在しています",
        "variance_too_high" => "ばらつきが大きすぎます。やり直してください",
        "too_few_after_outliers" => "有効なサンプルが不足しています",
        "too_few_strokes" => "サンプルが不足しています",
        "too_many_strokes" => "サンプルが多すぎます",
        _ => "条件を満たしません",
    }
}

unsafe fn stroke_polyline_from_origin(
    hdc: HDC,
    canvas: &RECT,
    origin: (i32, i32),
    path: &[(f32, f32)],
    color: (u8, u8, u8),
    width: i32,
) {
    if path.len() < 2 {
        return;
    }
    let pen = CreatePen(PS_SOLID, width, rgb(color));
    let old = SelectObject(hdc, pen);
    let mut first = true;
    for (x, y) in path {
        let px = (origin.0 + x.round() as i32).clamp(canvas.left + 2, canvas.right - 3);
        let py = (origin.1 + y.round() as i32).clamp(canvas.top + 2, canvas.bottom - 3);
        if first {
            let _ = MoveToEx(hdc, px, py, None);
            first = false;
        } else {
            let _ = LineTo(hdc, px, py);
        }
    }
    SelectObject(hdc, old);
    let _ = DeleteObject(pen);
}

unsafe fn stroke_polyline_centered(
    hdc: HDC,
    canvas: &RECT,
    path: &[(f32, f32)],
    color: (u8, u8, u8),
    width: i32,
) {
    if path.len() < 2 {
        return;
    }
    let max_r = path
        .iter()
        .map(|(x, y)| x.abs().max(y.abs()))
        .fold(1.0_f32, f32::max);
    let scale = ((CANVAS_SIZE as f32 * 0.42) / max_r).min(2.5);
    let cx = (canvas.left + canvas.right) / 2;
    let cy = (canvas.top + canvas.bottom) / 2;
    let pen = CreatePen(PS_SOLID, width, rgb(color));
    let old = SelectObject(hdc, pen);
    let mut first = true;
    for (x, y) in path {
        let px = (cx + (*x * scale).round() as i32).clamp(canvas.left + 2, canvas.right - 3);
        let py = (cy + (*y * scale).round() as i32).clamp(canvas.top + 2, canvas.bottom - 3);
        if first {
            let _ = MoveToEx(hdc, px, py, None);
            first = false;
        } else {
            let _ = LineTo(hdc, px, py);
        }
    }
    SelectObject(hdc, old);
    let _ = DeleteObject(pen);
}

unsafe fn draw_text(hdc: HDC, x: i32, y: i32, w: i32, h: i32, text: &str, center: bool) {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    let mut rc = RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    };
    let mut flags = DT_WORDBREAK | DT_NOPREFIX | DT_END_ELLIPSIS;
    flags |= if center {
        DT_CENTER
    } else {
        DT_LEFT | DT_SINGLELINE
    };
    DrawTextW(hdc, &mut wide, &mut rc, flags);
}
