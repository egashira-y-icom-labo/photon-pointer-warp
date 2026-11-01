//! `--calibrate` 用の純 Win32 / GDI キャリブレーションウィンドウ

use std::io;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use crate::domain::calibration::{
    analyze_relative_stroke, is_usable_calibration_stroke, recommend_detector_config,
    should_finalize_idle_stroke, StrokeMetrics,
};
use crate::domain::circle::DetectorConfig;
use crate::domain::config::AppConfig;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreatePen,
    CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect, InvalidateRect,
    LineTo, MoveToEx, SelectObject, SetBkMode, SetTextColor, DEFAULT_CHARSET, DT_CENTER,
    DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_WORDBREAK, FW_NORMAL, HDC,
    PAINTSTRUCT, PS_SOLID, SRCCOPY, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
    RID_INPUT, RIM_TYPEMOUSE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRect, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetClientRect, GetMessageW, GetSystemMetrics, KillTimer, LoadCursorW, PostQuitMessage,
    RegisterClassW, SetTimer, ShowWindow, TranslateMessage, CS_HREDRAW, CS_OWNDC, CS_VREDRAW,
    HMENU, IDC_ARROW, MSG, SM_CXSCREEN, SM_CYSCREEN, SW_SHOW, WM_CLOSE, WM_COMMAND, WM_DESTROY,
    WM_ERASEBKGND, WM_INPUT, WM_PAINT, WM_TIMER, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_MINIMIZEBOX,
    WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};

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
const MAX_STROKES: usize = 5;

const COLOR_BG: (u8, u8, u8) = (30, 30, 30);
const COLOR_CANVAS: (u8, u8, u8) = (24, 24, 24);
const COLOR_TEXT: (u8, u8, u8) = (220, 220, 220);
const COLOR_DIM: (u8, u8, u8) = (140, 140, 140);
const COLOR_LINE: (u8, u8, u8) = (80, 210, 230);
const COLOR_FADE: (u8, u8, u8) = (50, 90, 100);
const COLOR_FRAME: (u8, u8, u8) = (70, 70, 70);

static STATE: OnceLock<Mutex<CalibrateState>> = OnceLock::new();

// HWND はメッセージスレッドのみで使う
unsafe impl Send for CalibrateState {}

struct CalibrateState {
    started: Instant,
    current: Vec<(f32, f32, u64)>,
    last_input_at: Option<Instant>,
    accepted: Vec<StrokeMetrics>,
    last_path: Vec<(f32, f32)>,
    recommended: Option<DetectorConfig>,
    recommend_error: Option<&'static str>,
    save_hwnd: HWND,
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
        self.recommended = None;
        self.recommend_error = None;
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
        let x = ((GetSystemMetrics(SM_CXSCREEN) - win_w) / 2).max(0);
        let y = ((GetSystemMetrics(SM_CYSCREEN) - win_h) / 2).max(0);

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
                recommended: None,
                recommend_error: None,
                save_hwnd,
            })
        });

        register_raw_input(hwnd)?;
        let _ = SetTimer(hwnd, TIMER_ID, TIMER_MS, None);
        let _ = ShowWindow(hwnd, SW_SHOW);

        println!("=== photon-pointer-warp calibrate ===");
        println!("Draw 3-5 circles, then Save. Close without Save to cancel.");
        let _ = io::Write::flush(&mut io::stdout());

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
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
        WM_TIMER => {
            if wparam.0 == TIMER_ID {
                poll_idle_finalize(hwnd);
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
        return Ok(());
    }

    if let Some(mut g) = STATE.get().and_then(|s| s.lock().ok()) {
        if g.accepted.len() >= MAX_STROKES {
            return Ok(());
        }
        let t = g.now_ms();
        g.current.push((dx, dy, t));
        g.last_input_at = Some(Instant::now());
    }
    let _ = InvalidateRect(hwnd, None, false);
    Ok(())
}

fn poll_idle_finalize(hwnd: HWND) {
    let mut guard = match STATE.get().and_then(|s| s.lock().ok()) {
        Some(g) => g,
        None => return,
    };
    let idle = guard
        .last_input_at
        .map(|t| t.elapsed().as_millis() as u64)
        .unwrap_or(0);
    if !should_finalize_idle_stroke(idle, guard.current.len()) {
        return;
    }
    let stroke = std::mem::take(&mut guard.current);
    guard.last_input_at = None;
    if let Some(metrics) = analyze_relative_stroke(&stroke) {
        if is_usable_calibration_stroke(&metrics) && guard.accepted.len() < MAX_STROKES {
            guard.last_path = integrate_path(&stroke);
            guard.accepted.push(metrics);
            guard.refresh_recommendation();
            let can_save = guard.recommended.is_some();
            let save_hwnd = guard.save_hwnd;
            drop(guard);
            unsafe {
                let _ = EnableWindow(save_hwnd, can_save);
            }
            unsafe {
                let _ = InvalidateRect(hwnd, None, false);
            }
            return;
        }
    }
    guard.last_path.clear();
    drop(guard);
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
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
        "トラックボールで普段の感覚のサークルを 3〜5 回描いてください",
        true,
    );

    let canvas_l = (client_w - CANVAS_SIZE) / 2;
    let canvas_t = CANVAS_TOP;
    let canvas = RECT {
        left: canvas_l,
        top: canvas_t,
        right: canvas_l + CANVAS_SIZE,
        bottom: canvas_t + CANVAS_SIZE,
    };
    let canvas_brush = CreateSolidBrush(rgb(COLOR_CANVAS));
    FillRect(hdc, &canvas, canvas_brush);
    let _ = DeleteObject(canvas_brush);
    let frame = CreatePen(PS_SOLID, 1, rgb(COLOR_FRAME));
    let old_pen = SelectObject(hdc, frame);
    let _ = MoveToEx(hdc, canvas.left, canvas.top, None);
    let _ = LineTo(hdc, canvas.right - 1, canvas.top);
    let _ = LineTo(hdc, canvas.right - 1, canvas.bottom - 1);
    let _ = LineTo(hdc, canvas.left, canvas.bottom - 1);
    let _ = LineTo(hdc, canvas.left, canvas.top);
    SelectObject(hdc, old_pen);
    let _ = DeleteObject(frame);

    if !state.last_path.is_empty() {
        stroke_polyline(hdc, &canvas, &state.last_path, COLOR_FADE, 1);
    }
    let live = integrate_path(&state.current);
    if live.len() > 1 {
        stroke_polyline(hdc, &canvas, &live, COLOR_LINE, 2);
    }

    let list_top = canvas.bottom + 14;
    SetTextColor(hdc, rgb(COLOR_TEXT));
    draw_text(
        hdc,
        24,
        list_top,
        client_w - 48,
        22,
        &format!("Samples: {}/{MAX_STROKES}", state.accepted.len()),
        false,
    );
    SetTextColor(hdc, rgb(COLOR_DIM));
    for (i, m) in state.accepted.iter().enumerate() {
        let line = format!(
            "#{}: {}ms | {:.0}° | aspect {:.1}",
            i + 1,
            m.elapsed_ms,
            m.net_angle_deg,
            m.aspect_ratio
        );
        draw_text(
            hdc,
            24,
            list_top + 24 + i as i32 * 20,
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

unsafe fn stroke_polyline(
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
        let px = cx + (*x * scale).round() as i32;
        let py = cy + (*y * scale).round() as i32;
        let px = px.clamp(canvas.left + 2, canvas.right - 3);
        let py = py.clamp(canvas.top + 2, canvas.bottom - 3);
        if first {
            let _ = MoveToEx(hdc, px, py, None);
            first = false;
        } else {
            let _ = LineTo(hdc, px, py);
        }
    }
    SelectObject(hdc, old);
    let _ = DeleteObject(pen);
    let _ = first;
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
