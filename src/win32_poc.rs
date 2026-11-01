//! Phase 0 + PoC1: Raw Input 常駐 CLI（Windows のみ）

use std::io::{self, Write};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime};

use photon_pointer_warp::domain::circle::{CircleDetector, GestureWindowStats, MotionSample};
use photon_pointer_warp::domain::config::AppConfig;
use photon_pointer_warp::platform::layout::warp_target_for_cell;
use photon_pointer_warp::platform::tray::{
    TrayEvent, TrayIcon, IDM_CALIBRATE, IDM_EXIT, IDM_PAUSE, WM_TRAYICON,
};
use photon_pointer_warp::platform::window::HudOverlay;
use windows::core::w;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, WPARAM, FALSE, TRUE};
#[cfg(debug_assertions)]
use windows::Win32::Foundation::GENERIC_WRITE;
#[cfg(debug_assertions)]
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Console::{
    SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT,
};
#[cfg(debug_assertions)]
use windows::Win32::System::Console::{
    AllocConsole, AttachConsole, GetConsoleWindow, SetConsoleCP, SetConsoleOutputCP,
    SetConsoleTitleW, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
};

#[cfg(debug_assertions)]
const CP_UTF8: u32 = 65001;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    GetDpiForSystem, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
    RAWMOUSE, RIDEV_INPUTSINK, RID_INPUT, RIM_TYPEMOUSE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW,
    KillTimer, PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SetCursorPos,
    SetTimer, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, MSG, SW_HIDE, WM_CLOSE,
    WM_DESTROY, WM_HOTKEY, WM_INPUT, WM_KEYDOWN, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_POPUP,
};
#[cfg(debug_assertions)]
use windows::Win32::UI::WindowsAndMessaging::{
    IsIconic, SetForegroundWindow, SW_RESTORE, SW_SHOW,
};

#[cfg(debug_assertions)]
enum ConsoleMode {
    /// cargo run した PowerShell / ターミナルに出力（別窓は出ない）
    ParentTerminal,
    /// AllocConsole で開いた専用コンソール
    Dedicated,
}

static MESSAGE_HWND: AtomicIsize = AtomicIsize::new(0);
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

const DEBUG_TIMER_ID: usize = 1;
const ESC_HOTKEY_ID: i32 = 2;
const DEBUG_TIMER_MS: u32 = 100;
/// この時間以上入力が空いたら「ストローク終了」とみなしてデバッグログを1行出す。
const IDLE_FLUSH_MS: u64 = 150;
/// サンプル間隔がこれ以上空いたら前ストロークの保留ログを先にフラッシュする。
const STROKE_GAP_MS: u64 = 100;
const DEBUG_ARC_MIN_DEG: f32 = 180.0;

/// サークル成立後、慣性が落ち着くまで HUD 展開を遅延する。
const SETTLING_WINDOW_MS: u64 = 50;
const SETTLING_MAX_WAIT_MS: u64 = 100;
/// 1 サンプルあたりの移動量（|Δ|）がこれを超えたら「大きな移動」とみなす。
const SETTLING_SPEED_THRESHOLD: f32 = 3.0;
/// 連続ワープ着地後、無操作なら HUD を消すまでの時間。
const CHAINED_WARP_TIMEOUT_MS: u64 = 2000;

struct PendingSettling {
    started_at: Instant,
    last_significant_motion_at: Instant,
    detection_stats: GestureWindowStats,
}

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
    pending_arc_debug: Option<(GestureWindowStats, String)>,
    hud: HudOverlay,
    esc_hotkey_registered: bool,
    pending_settling: Option<PendingSettling>,
    /// 連続ワープ着地（または最後のマウス移動）時刻。2 秒無操作でオートディスミス。
    chained_warp_started_at: Option<Instant>,
    detection_paused: bool,
    tray: Option<TrayIcon>,
    calibrate_child: Option<Child>,
    config_mtime: Option<SystemTime>,
}

static APP: OnceLock<Mutex<AppState>> = OnceLock::new();

/// デバッグ時のみログ窓を確保し、可能なら前面に出す。
#[cfg(debug_assertions)]
fn init_console() -> ConsoleMode {
    unsafe {
        let mode = if GetConsoleWindow().0.is_null() {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
            if GetConsoleWindow().0.is_null() {
                let _ = AllocConsole();
                ConsoleMode::Dedicated
            } else {
                ConsoleMode::ParentTerminal
            }
        } else {
            ConsoleMode::ParentTerminal
        };

        let _ = SetConsoleTitleW(w!("photon-pointer-warp PoC - Ctrl+C to exit"));
        let _ = SetConsoleOutputCP(CP_UTF8);
        let _ = SetConsoleCP(CP_UTF8);
        redirect_stdio_to_console();
        show_console_window();
        mode
    }
}

#[cfg(debug_assertions)]
unsafe fn show_console_window() {
    let con = GetConsoleWindow();
    if con.0.is_null() {
        return;
    }
    if IsIconic(con).as_bool() {
        let _ = ShowWindow(con, SW_RESTORE);
    }
    let _ = ShowWindow(con, SW_SHOW);
    let _ = SetForegroundWindow(con);
}

#[cfg(debug_assertions)]
unsafe fn redirect_stdio_to_console() {
    let conout = CreateFileW(
        w!("CONOUT$"),
        GENERIC_WRITE.0,
        FILE_SHARE_WRITE,
        None,
        OPEN_EXISTING,
        FILE_ATTRIBUTE_NORMAL,
        None,
    );
    let conerr = CreateFileW(
        w!("CONERR$"),
        GENERIC_WRITE.0,
        FILE_SHARE_WRITE,
        None,
        OPEN_EXISTING,
        FILE_ATTRIBUTE_NORMAL,
        None,
    );
    if let Ok(h) = conout {
        let _ = SetStdHandle(STD_OUTPUT_HANDLE, h);
    }
    if let Ok(h) = conerr {
        let _ = SetStdHandle(STD_ERROR_HANDLE, h);
    }
}

pub fn run(config: AppConfig) -> io::Result<()> {
    #[cfg(debug_assertions)]
    {
        let console_mode = init_console();
        match console_mode {
            ConsoleMode::ParentTerminal => {
                println!("=== photon-pointer-warp PoC (PID {}) ===", std::process::id());
                println!("Logs go to THIS PowerShell window (no separate console).");
                println!("Exit: tray menu or Ctrl+C");
            }
            ConsoleMode::Dedicated => {
                println!("=== photon-pointer-warp PoC (PID {}) ===", std::process::id());
                println!("Dedicated console opened. Exit: tray menu or Ctrl+C");
            }
        }
        println!("Tray: calibrate / pause / quit. Draw a circle to open the HUD.");
        let _ = io::stdout().flush();
    }

    unsafe {
        // マニフェストの代わりに Per-Monitor V2（Windows 10 1703+）
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    unsafe {
        SetConsoleCtrlHandler(Some(console_ctrl_handler), true)
            .map_err(|e| io::Error::other(format!("SetConsoleCtrlHandler: {e}")))?;
    }

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
            None,
            None,
            instance,
            None,
        )
        .map_err(|e| io::Error::other(format!("CreateWindowExW: {e}")))?;

        MESSAGE_HWND.store(hwnd.0 as isize, Ordering::Release);
        TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::Release);

        let hud = HudOverlay::create(instance, hud_wnd_proc)
            .map_err(|e| io::Error::other(format!("HudOverlay::create: {e}")))?;
        let tray = TrayIcon::add(hwnd, instance)
            .map_err(|e| io::Error::other(format!("tray: {e}")))?;

        APP.get_or_init(|| {
            Mutex::new(AppState {
                detector: CircleDetector::new(config.detector.clone()),
                last_input_at: None,
                buttons_down: 0,
                pending_arc_debug: None,
                hud,
                esc_hotkey_registered: false,
                pending_settling: None,
                chained_warp_started_at: None,
                detection_paused: false,
                tray: Some(tray),
                calibrate_child: None,
                config_mtime: config_file_mtime(),
            })
        });

        register_raw_input(hwnd)?;

        let _ = SetTimer(hwnd, DEBUG_TIMER_ID, DEBUG_TIMER_MS, None);

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
    if msg != 0 && msg == TASKBAR_CREATED.load(Ordering::Acquire) {
        readd_tray_icon();
        return LRESULT(0);
    }
    match msg {
        WM_INPUT => {
            if let Err(e) = handle_wm_input(lparam) {
                eprintln!("WM_INPUT error: {e}");
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == DEBUG_TIMER_ID {
                flush_pending_on_idle();
                poll_config_and_calibrate_child();
            }
            LRESULT(0)
        }
        WM_TRAYICON => {
            handle_tray_callback(lparam);
            LRESULT(0)
        }
        WM_HOTKEY => {
            if wparam.0 as i32 == ESC_HOTKEY_ID {
                dismiss_hud_with_log("escape");
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if wparam.0 == VK_ESCAPE.0 as usize {
                dismiss_hud_with_log("escape");
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = KillTimer(hwnd, DEBUG_TIMER_ID);
            if let Some(mut guard) = APP.get().and_then(|a| a.try_lock().ok()) {
                unregister_esc_hotkey_state(&mut guard, hwnd);
                let _ = guard.hud.dismiss();
                guard.tray = None;
            }
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn flush_pending_debug(state: &mut AppState) {
    if let Some((stats, reason)) = state.pending_arc_debug.take() {
        eprintln!(
            "[debug] arc {:.0}deg in {}ms - not a circle ({reason})",
            stats.net_angle_deg, stats.elapsed_ms
        );
        let _ = io::stderr().flush();
    }
}

fn cursor_screen_pos() -> Option<(i32, i32)> {
    let mut point = POINT::default();
    unsafe { GetCursorPos(&mut point) }.ok()?;
    Some((point.x, point.y))
}

fn hud_pointer_out_of_bounds(hud: &HudOverlay) -> bool {
    cursor_screen_pos().is_some_and(|(x, y)| hud.pointer_beyond_dismiss_margin(x, y))
}

fn chained_warp_auto_dismiss_due(started_at: Instant, now: Instant) -> bool {
    now.saturating_duration_since(started_at).as_millis() as u64 >= CHAINED_WARP_TIMEOUT_MS
}

fn note_chained_warp_activity(state: &mut AppState) {
    if state.chained_warp_started_at.is_some() {
        state.chained_warp_started_at = Some(Instant::now());
    }
}

fn config_file_mtime() -> Option<SystemTime> {
    std::fs::metadata(AppConfig::default_path())
        .and_then(|m| m.modified())
        .ok()
}

fn reload_detector_from_disk(state: &mut AppState) {
    let cfg = AppConfig::load_or_default();
    state.detector.replace_config(cfg.detector);
    state.pending_settling = None;
    state.config_mtime = config_file_mtime();
}

fn spawn_calibrate(state: &mut AppState) {
    if let Some(child) = state.calibrate_child.as_mut() {
        if matches!(child.try_wait(), Ok(None)) {
            return;
        }
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    match Command::new(exe).arg("--calibrate").spawn() {
        Ok(child) => state.calibrate_child = Some(child),
        Err(e) => eprintln!("failed to launch --calibrate: {e}"),
    }
}

fn poll_config_and_calibrate_child() {
    let mut guard = match APP.get().and_then(|a| a.try_lock().ok()) {
        Some(g) => g,
        None => return,
    };
    let mut child_exited = false;
    if let Some(child) = guard.calibrate_child.as_mut() {
        match child.try_wait() {
            Ok(Some(_)) => child_exited = true,
            Ok(None) => {}
            Err(_) => child_exited = true,
        }
    }
    if child_exited {
        guard.calibrate_child = None;
    }
    let mtime = config_file_mtime();
    let file_changed = mtime.is_some() && mtime != guard.config_mtime;
    if child_exited || file_changed {
        reload_detector_from_disk(&mut guard);
    }
}

fn handle_tray_callback(lparam: LPARAM) {
    match TrayIcon::callback_event(lparam) {
        Some(TrayEvent::ContextMenu) => {
            let (paused, hwnd) = {
                let guard = match APP.get().and_then(|a| a.try_lock().ok()) {
                    Some(g) => g,
                    None => return,
                };
                let Some(tray) = guard.tray.as_ref() else {
                    return;
                };
                (guard.detection_paused, tray.hwnd())
            };

            let cmd_id = unsafe { TrayIcon::track_menu(hwnd, paused) };
            if cmd_id != 0 {
                handle_tray_command_sync(hwnd, cmd_id);
            }
        }
        Some(TrayEvent::OpenCalibrate) => {
            if let Some(mut guard) = APP.get().and_then(|a| a.try_lock().ok()) {
                spawn_calibrate(&mut guard);
            }
        }
        None => {}
    }
}

fn handle_tray_command_sync(hwnd: HWND, id: u32) {
    match id as usize {
        IDM_CALIBRATE => {
            if let Some(mut guard) = APP.get().and_then(|a| a.try_lock().ok()) {
                spawn_calibrate(&mut guard);
            }
        }
        IDM_PAUSE => {
            if let Some(mut guard) = APP.get().and_then(|a| a.try_lock().ok()) {
                guard.detection_paused = !guard.detection_paused;
                if guard.detection_paused {
                    guard.pending_settling = None;
                }
                let paused = guard.detection_paused;
                if let Some(tray) = guard.tray.as_mut() {
                    unsafe { tray.set_paused(paused) };
                }
            }
        }
        IDM_EXIT => unsafe {
            let _ = DestroyWindow(hwnd);
        },
        _ => {}
    }
}

fn readd_tray_icon() {
    if let Some(mut guard) = APP.get().and_then(|a| a.try_lock().ok()) {
        if let Some(tray) = guard.tray.as_mut() {
            unsafe {
                tray.readd();
            }
        }
    }
}

fn flush_pending_on_idle() {
    let mut guard = match APP.get().and_then(|a| a.try_lock().ok()) {
        Some(g) => g,
        None => return,
    };
    let idle = guard
        .last_input_at
        .map(|t| t.elapsed().as_millis() as u64)
        .unwrap_or(IDLE_FLUSH_MS);
    if idle >= IDLE_FLUSH_MS {
        flush_pending_debug(&mut guard);
    }
    let _ = try_finish_settling(&mut guard);
    let should_dismiss_oob = guard.hud.is_active() && hud_pointer_out_of_bounds(&guard.hud);
    let should_auto_timeout = !should_dismiss_oob
        && guard.hud.is_active()
        && guard
            .chained_warp_started_at
            .is_some_and(|t| chained_warp_auto_dismiss_due(t, Instant::now()));
    drop(guard);
    if should_dismiss_oob {
        dismiss_hud_with_log("out of bounds");
    } else if should_auto_timeout {
        dismiss_hud_with_log("auto timeout");
    }
}

fn settling_motion_significant(dx: f32, dy: f32) -> bool {
    dx.hypot(dy) > SETTLING_SPEED_THRESHOLD
}

fn settling_note_motion(pending: &mut PendingSettling, dx: f32, dy: f32, now: Instant) {
    if settling_motion_significant(dx, dy) {
        pending.last_significant_motion_at = now;
    }
}

fn settling_ready(pending: &PendingSettling, now: Instant) -> bool {
    let elapsed_ms = now
        .saturating_duration_since(pending.started_at)
        .as_millis() as u64;
    let quiet_ms = now
        .saturating_duration_since(pending.last_significant_motion_at)
        .as_millis() as u64;
    if elapsed_ms >= SETTLING_MAX_WAIT_MS {
        return true;
    }
    if quiet_ms >= SETTLING_WINDOW_MS {
        return true;
    }
    false
}

fn open_hud_after_settle(guard: &mut AppState, stats: GestureWindowStats) -> io::Result<()> {
    let message_hwnd = HWND(MESSAGE_HWND.load(Ordering::Acquire) as _);
    let hud_diag = unsafe {
        guard
            .hud
            .activate_at_cursor()
            .map_err(|e| io::Error::other(format!("HUD activate: {e}")))?
    };
    let dpi = unsafe { GetDpiForSystem() };
    println!("{}", hud_diag.format_line(dpi));
    let _ = io::stdout().flush();
    unsafe { register_esc_hotkey_if_needed(message_hwnd, guard)? };
    println!(
        "[detect] Circle gesture - {:.0}deg in {}ms (HUD open; click cell to warp)",
        stats.net_angle_deg, stats.elapsed_ms
    );
    let _ = io::stdout().flush();
    Ok(())
}

fn try_finish_settling(guard: &mut AppState) -> io::Result<()> {
    let now = Instant::now();
    let should_open = guard
        .pending_settling
        .as_ref()
        .is_some_and(|p| settling_ready(p, now));
    if !should_open {
        return Ok(());
    }
    let stats = guard
        .pending_settling
        .take()
        .expect("pending_settling")
        .detection_stats;
    open_hud_after_settle(guard, stats)
}

fn update_pending_arc_debug(state: &mut AppState) {
    let stats = state.detector.window_stats();
    if stats.net_angle_deg <= DEBUG_ARC_MIN_DEG {
        return;
    }
    let Some(reason) = state.detector.circle_reject_reason() else {
        return;
    };
    state.pending_arc_debug = Some((stats, reason.to_string()));
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

    let mut guard = match APP.get().and_then(|a| a.try_lock().ok()) {
        Some(g) => g,
        None => return Ok(()),
    };

    if guard.hud.is_active() {
        let dx = mouse.lLastX as f32;
        let dy = mouse.lLastY as f32;
        if dx != 0.0 || dy != 0.0 {
            if let Some((x, y)) = cursor_screen_pos() {
                if guard.hud.pointer_beyond_dismiss_margin(x, y) {
                    drop(guard);
                    dismiss_hud_with_log("out of bounds");
                    return Ok(());
                }
                let _ = guard.hud.update_hover_from_screen(x, y);
                note_chained_warp_activity(&mut guard);
            }
        }
        return Ok(());
    }

    apply_button_flags(&mut guard, &mouse);

    if guard.detection_paused {
        return Ok(());
    }

    let dx = mouse.lLastX as f32;
    let dy = mouse.lLastY as f32;
    let now = Instant::now();

    let dt_millis = if dx != 0.0 || dy != 0.0 {
        let dt = guard
            .last_input_at
            .map(|t| now.saturating_duration_since(t).as_millis() as u64)
            .unwrap_or(0);
        guard.last_input_at = Some(now);
        if dt >= STROKE_GAP_MS {
            flush_pending_debug(&mut guard);
            guard.pending_settling = None;
        }
        dt
    } else {
        0
    };

    if let Some(ref mut pending) = guard.pending_settling {
        settling_note_motion(pending, dx, dy, now);
        if settling_ready(pending, now) {
            let stats = guard.pending_settling.take().unwrap().detection_stats;
            open_hud_after_settle(&mut guard, stats)?;
        }
        return Ok(());
    }

    if dx == 0.0 && dy == 0.0 {
        return Ok(());
    }

    let sample = MotionSample {
        dx,
        dy,
        dt_millis: dt_millis.max(1),
    };

    let fired = guard.detector.push(sample);
    if fired {
        guard.pending_arc_debug = None;
        let stats = guard
            .detector
            .last_detection_stats()
            .unwrap_or_else(|| guard.detector.window_stats());
        guard.pending_settling = Some(PendingSettling {
            started_at: now,
            last_significant_motion_at: now,
            detection_stats: stats,
        });
    } else {
        update_pending_arc_debug(&mut guard);
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

unsafe fn register_esc_hotkey_if_needed(
    message_hwnd: HWND,
    state: &mut AppState,
) -> io::Result<()> {
    if state.esc_hotkey_registered {
        return Ok(());
    }
    RegisterHotKey(
        message_hwnd,
        ESC_HOTKEY_ID,
        HOT_KEY_MODIFIERS(0),
        VK_ESCAPE.0 as u32,
    )
    .map_err(|e| io::Error::other(format!("RegisterHotKey: {e}")))?;
    state.esc_hotkey_registered = true;
    Ok(())
}

fn unregister_esc_hotkey_state(state: &mut AppState, message_hwnd: HWND) {
    if !state.esc_hotkey_registered {
        return;
    }
    unsafe {
        let _ = UnregisterHotKey(message_hwnd, ESC_HOTKEY_ID);
    }
    state.esc_hotkey_registered = false;
}

fn dismiss_hud_with_log(reason: &str) {
    let raw = MESSAGE_HWND.load(Ordering::Acquire);
    if raw == 0 {
        return;
    }
    let message_hwnd = HWND(raw as _);
    let mut guard = match APP.get().and_then(|a| a.try_lock().ok()) {
        Some(g) => g,
        None => return,
    };
    if !guard.hud.is_active() {
        return;
    }
    guard.chained_warp_started_at = None;
    if let Err(e) = unsafe { guard.hud.dismiss() } {
        eprintln!("HUD dismiss error: {e}");
        return;
    }
    unregister_esc_hotkey_state(&mut guard, message_hwnd);
    println!("[hud] dismissed ({reason})");
    let _ = io::stdout().flush();
}

fn warp_hud_to_cell(col: usize, row: usize) {
    let mut guard = match APP.get().and_then(|a| a.try_lock().ok()) {
        Some(g) => g,
        None => return,
    };
    if !guard.hud.is_active() {
        return;
    }
    let vs = guard.hud.virtual_screen();
    let (target_x, target_y) = warp_target_for_cell(col, row, vs);
    if let Err(e) = unsafe { SetCursorPos(target_x, target_y) } {
        eprintln!("SetCursorPos error: {e}");
        return;
    }
    println!("[warp] Jumped to ({target_x}, {target_y}) [cell: {col}, {row}]");
    let _ = io::stdout().flush();
    if let Err(e) = unsafe { guard.hud.activate_at_cursor() } {
        eprintln!("[hud] Failed to re-activate at landing point: {e}");
        return;
    }
    guard.chained_warp_started_at = Some(Instant::now());
}

fn handle_hud_left_button_up() {
    let mut point = POINT::default();
    if unsafe { GetCursorPos(&mut point) }.is_err() {
        return;
    }
    let guard = match APP.get().and_then(|a| a.try_lock().ok()) {
        Some(g) => g,
        None => return,
    };
    if !guard.hud.is_active() {
        return;
    }
    if guard.hud.origin_hit_test_screen(point.x, point.y) {
        drop(guard);
        dismiss_hud_with_log("origin click");
        return;
    }
    if let Some((col, row)) = guard.hud.minimap_cell_at_screen(point.x, point.y) {
        drop(guard);
        warp_hud_to_cell(col, row);
        return;
    }
    drop(guard);
    dismiss_hud_with_log("outside click");
}

unsafe extern "system" fn hud_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{
        WM_DESTROY, WM_LBUTTONUP, WM_MOUSEMOVE, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR,
    };

    match msg {
        WM_SETCURSOR => photon_pointer_warp::platform::window::handle_wm_setcursor(),
        WM_MOUSEMOVE => {
            let mut point = POINT::default();
            if GetCursorPos(&mut point).is_ok() {
                if let Some(mut guard) = APP.get().and_then(|a| a.try_lock().ok()) {
                    if guard.hud.is_active() {
                        if guard.hud.pointer_beyond_dismiss_margin(point.x, point.y) {
                            drop(guard);
                            dismiss_hud_with_log("out of bounds");
                            return LRESULT(0);
                        }
                        let _ = guard.hud.update_hover_from_screen(point.x, point.y);
                        note_chained_warp_activity(&mut guard);
                    }
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            handle_hud_left_button_up();
            LRESULT(0)
        }
        WM_RBUTTONDOWN | WM_RBUTTONUP => {
            dismiss_hud_with_log("right click");
            LRESULT(0)
        }
        WM_DESTROY => {
            photon_pointer_warp::platform::window::note_hud_destroyed();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> BOOL {
    match ctrl_type {
        CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT => {
            let raw = MESSAGE_HWND.load(Ordering::Acquire);
            if raw != 0 {
                let hwnd = HWND(raw as _);
                let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            TRUE
        }
        _ => FALSE,
    }
}
