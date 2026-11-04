//! Phase 0 + PoC1: Raw Input 常駐 CLI（Windows のみ）

use std::collections::VecDeque;
use std::io::{self, Write};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime};

use photon_pointer_warp::domain::adaptation::{
    extract_best_near_miss, generate_suggestion, is_accidental_trigger, ConfigSuggestion,
    FeedbackEvent, HudCloseReason,
};
use photon_pointer_warp::domain::calibration::{analyze_relative_stroke, StrokeMetrics};
use photon_pointer_warp::domain::circle::{CircleDetector, GestureWindowStats, MotionSample};
use photon_pointer_warp::domain::config::{effective_hud_style, AppConfig};
use photon_pointer_warp::domain::monitor_layout::clamp_to_monitors;
use photon_pointer_warp::domain::settle::{decide_settle, policy_for, SettleDecision};
use photon_pointer_warp::domain::shake::ShakeDetector;
use photon_pointer_warp::domain::tracking::{TrackingAction, TrackingController};
use photon_pointer_warp::domain::trigger::{TriggerCoordinator, TriggerSource};
use photon_pointer_warp::platform::calibrate_window::CALIBRATE_MUTEX_NAME;
use photon_pointer_warp::platform::layout::warp_target_for_cell;
use photon_pointer_warp::platform::monitors::{capture_display_snapshot, DisplaySnapshot};
use photon_pointer_warp::platform::startup;
use photon_pointer_warp::platform::tray::{
    TrayEvent, TrayIcon, IDM_APPLY_SUGGESTION, IDM_CALIBRATE, IDM_DISMISS_SUGGESTION, IDM_EXIT,
    IDM_PAUSE, IDM_STARTUP, WM_TRAYICON,
};
use photon_pointer_warp::platform::window::HudOverlay;
use windows::core::w;
#[cfg(debug_assertions)]
use windows::Win32::Foundation::GENERIC_WRITE;
use windows::Win32::Foundation::{
    CloseHandle, BOOL, FALSE, HWND, LPARAM, LRESULT, POINT, TRUE, WPARAM,
};
#[cfg(debug_assertions)]
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_WRITE, OPEN_EXISTING,
};
#[cfg(debug_assertions)]
use windows::Win32::System::Console::{
    AllocConsole, SetConsoleCP, SetConsoleOutputCP, SetConsoleTitleW, SetStdHandle,
    STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
};
use windows::Win32::System::Console::{
    AttachConsole, GetConsoleWindow, SetConsoleCtrlHandler, ATTACH_PARENT_PROCESS,
    CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT,
};

#[cfg(debug_assertions)]
const CP_UTF8: u32 = 65001;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
use windows::Win32::UI::HiDpi::{
    GetDpiForSystem, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, ReleaseCapture, UnregisterHotKey, HOT_KEY_MODIFIERS, VK_ESCAPE,
};
use windows::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
    RAWMOUSE, RIDEV_INPUTSINK, RID_INPUT, RIM_TYPEMOUSE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW,
    GetWindowLongPtrW, KillTimer, PeekMessageW, PostMessageW, PostQuitMessage, RegisterClassW,
    RegisterWindowMessageW, SetCursorPos, SetForegroundWindow, SetTimer, SetWindowLongPtrW,
    ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, GWL_EXSTYLE, MSG, PM_REMOVE, SW_HIDE,
    WM_APP, WM_CLOSE, WM_DESTROY, WM_DISPLAYCHANGE, WM_HOTKEY, WM_INPUT, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_MBUTTONDBLCLK, WM_NCLBUTTONDOWN, WM_NCMBUTTONDBLCLK, WM_SETTINGCHANGE, WM_TIMER, WNDCLASSW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};
#[cfg(debug_assertions)]
use windows::Win32::UI::WindowsAndMessaging::{IsIconic, SW_RESTORE, SW_SHOW};

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
const TRACK_TIMER_ID: usize = 3;
const DISPLAY_TIMER_ID: usize = 4;
const ESC_HOTKEY_ID: i32 = 2;
const WM_OPEN_ADJUST: u32 = WM_APP + 2;
const DEBUG_TIMER_MS: u32 = 100;
const TRACK_TIMER_MS: u32 = 16;
const DISPLAY_DEBOUNCE_MS: u32 = 300;
const PAINT_INTERVAL_MS: u64 = 16;
/// この時間以上入力が空いたら「ストローク終了」とみなしてデバッグログを1行出す。
const IDLE_FLUSH_MS: u64 = 150;
/// サンプル間隔がこれ以上空いたら前ストロークの保留ログを先にフラッシュする。
const STROKE_GAP_MS: u64 = 100;
const DEBUG_ARC_MIN_DEG: f32 = 180.0;

/// 1 サンプルあたりの移動量（|Δ|）がこれを超えたら「大きな移動」とみなす。
const SETTLING_SPEED_THRESHOLD: f32 = 3.0;
/// 連続ワープ着地後、無操作なら HUD を消すまでの時間。
const CHAINED_WARP_TIMEOUT_MS: u64 = 2000;

struct PendingSettling {
    started_at: Instant,
    last_significant_motion_at: Instant,
    source: TriggerSource,
    is_learning_target: bool,
}

enum HudPhase {
    Idle,
    Settling(PendingSettling),
    Tracking {
        controller: TrackingController,
        last_paint_ms: u64,
    },
    Anchored,
}

const CLASS_NAME: windows::core::PCWSTR = w!("PhotonPointerWarpRawInput");

const RI_MOUSE_LEFT_BUTTON_DOWN: u16 = 0x0001;
const RI_MOUSE_LEFT_BUTTON_UP: u16 = 0x0002;
const RI_MOUSE_RIGHT_BUTTON_DOWN: u16 = 0x0004;
const RI_MOUSE_RIGHT_BUTTON_UP: u16 = 0x0008;
const RI_MOUSE_MIDDLE_BUTTON_DOWN: u16 = 0x0010;
const RI_MOUSE_MIDDLE_BUTTON_UP: u16 = 0x0020;

struct AppState {
    coordinator: TriggerCoordinator,
    last_input_at: Option<Instant>,
    buttons_down: u32,
    pending_arc_debug: Option<(GestureWindowStats, String)>,
    hud: HudOverlay,
    display: DisplaySnapshot,
    hud_style: photon_pointer_warp::domain::config::HudStyle,
    esc_hotkey_registered: bool,
    clock_origin: Instant,
    hud_phase: HudPhase,
    /// 連続ワープ着地（または最後のマウス移動）時刻。2 秒無操作でオートディスミス。
    chained_warp_started_at: Option<Instant>,
    detection_paused: bool,
    /// キャリブレーション実行中。ユーザの一時停止とは独立。
    calibrating: bool,
    tray: Option<TrayIcon>,
    calibrate_child: Option<Child>,
    config_mtime: Option<SystemTime>,
    raw_motion_history: VecDeque<(i32, i32, u64)>,
    input_clock_ms: u64,
    last_fire_clock_ms: Option<u64>,
    last_trigger_metrics: Option<StrokeMetrics>,
    feedback_events: VecDeque<FeedbackEvent>,
    pending_suggestion: Option<ConfigSuggestion>,
    hud_opened_at: Option<Instant>,
    hud_opened_cursor: Option<(i32, i32)>,
    feedback_circle_hud: bool,
}

static APP: OnceLock<Mutex<AppState>> = OnceLock::new();

/// 親コンソールがあるときだけ接続する。`AllocConsole` はしない（サイレント常駐を維持）。
fn attach_parent_console() {
    unsafe {
        if GetConsoleWindow().0.is_null() {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

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
    attach_parent_console();

    #[cfg(debug_assertions)]
    {
        let console_mode = init_console();
        match console_mode {
            ConsoleMode::ParentTerminal => {
                println!(
                    "=== photon-pointer-warp PoC (PID {}) ===",
                    std::process::id()
                );
                println!("Logs go to THIS PowerShell window (no separate console).");
                println!("Exit: tray menu or Ctrl+C");
            }
            ConsoleMode::Dedicated => {
                println!(
                    "=== photon-pointer-warp PoC (PID {}) ===",
                    std::process::id()
                );
                println!("Dedicated console opened. Exit: tray menu or Ctrl+C");
            }
        }
        println!(
            "Tray: calibrate / pause / quit. Draw a circle or shake the pointer to open the HUD."
        );
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
        TASKBAR_CREATED.store(
            RegisterWindowMessageW(w!("TaskbarCreated")),
            Ordering::Release,
        );

        let mut hud = HudOverlay::create(instance, hud_wnd_proc)
            .map_err(|e| io::Error::other(format!("HudOverlay::create: {e}")))?;
        let display = capture_display_snapshot();
        if !display.monitors.is_empty() && display.virtual_screen != hud.virtual_screen() {
            hud.rebuild_display()
                .map_err(|e| io::Error::other(format!("HUD display rebuild: {e}")))?;
        }
        println!(
            "[config] hud_style={} effective={} monitors={}",
            config.hud_style.as_str(),
            effective_hud_style(config.hud_style, display.monitors.len()).as_str(),
            display.monitors.len()
        );
        let tray =
            TrayIcon::add(hwnd, instance).map_err(|e| io::Error::other(format!("tray: {e}")))?;

        APP.get_or_init(|| {
            Mutex::new(AppState {
                coordinator: TriggerCoordinator::new(
                    CircleDetector::new(config.detector.clone()),
                    ShakeDetector::new(),
                ),
                last_input_at: None,
                buttons_down: 0,
                pending_arc_debug: None,
                hud,
                display,
                hud_style: config.hud_style,
                esc_hotkey_registered: false,
                clock_origin: Instant::now(),
                hud_phase: HudPhase::Idle,
                chained_warp_started_at: None,
                detection_paused: false,
                calibrating: false,
                tray: Some(tray),
                calibrate_child: None,
                config_mtime: config_file_mtime(),
                raw_motion_history: VecDeque::new(),
                input_clock_ms: 0,
                last_fire_clock_ms: None,
                last_trigger_metrics: None,
                feedback_events: VecDeque::new(),
                pending_suggestion: None,
                hud_opened_at: None,
                hud_opened_cursor: None,
                feedback_circle_hud: false,
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
            } else if wparam.0 == TRACK_TIMER_ID {
                if let Some(mut guard) = APP.get().and_then(|a| a.try_lock().ok()) {
                    match poll_tracking(&mut guard, Instant::now()) {
                        Ok(true) => {
                            drop(guard);
                            dismiss_hud_with_log(HudCloseReason::TrackingTimeout);
                        }
                        Err(e) => eprintln!("tracking: {e}"),
                        Ok(false) => {}
                    }
                }
            } else if wparam.0 == DISPLAY_TIMER_ID {
                if let Some(mut guard) = APP.get().and_then(|a| a.try_lock().ok()) {
                    let _ = KillTimer(hwnd, DISPLAY_TIMER_ID);
                    if let Err(e) = refresh_display_if_stale(&mut guard) {
                        eprintln!("[display] refresh failed: {e}");
                    }
                }
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
            schedule_display_refresh(hwnd);
            LRESULT(0)
        }
        WM_TRAYICON => {
            handle_tray_callback(lparam);
            LRESULT(0)
        }
        WM_OPEN_ADJUST => {
            unsafe { foreground_menu_owner(hwnd) };
            open_adjust_menu();
            unsafe { restore_menu_owner(hwnd) };
            LRESULT(0)
        }
        WM_HOTKEY => {
            if wparam.0 as i32 == ESC_HOTKEY_ID {
                dismiss_hud_with_log(HudCloseReason::EscapeKey);
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if wparam.0 == VK_ESCAPE.0 as usize {
                dismiss_hud_with_log(HudCloseReason::EscapeKey);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = KillTimer(hwnd, DEBUG_TIMER_ID);
            let _ = KillTimer(hwnd, DISPLAY_TIMER_ID);
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
    state.coordinator.circle.replace_config(cfg.detector);
    if cfg.hud_style != state.hud_style {
        println!("[config] hud_style={}", cfg.hud_style.as_str());
        let _ = io::stdout().flush();
        state.hud_style = cfg.hud_style;
    }
    cancel_settling(state);
    state.config_mtime = config_file_mtime();
}

fn schedule_display_refresh(hwnd: HWND) {
    unsafe {
        let _ = SetTimer(hwnd, DISPLAY_TIMER_ID, DISPLAY_DEBOUNCE_MS, None);
    }
}

/// 幾何が変わって HUD を閉じたとき `true`。列挙が空のときは現状を維持する。
fn refresh_display_if_stale(guard: &mut AppState) -> io::Result<bool> {
    let fresh = capture_display_snapshot();
    if fresh.monitors.is_empty() || fresh == guard.display {
        return Ok(false);
    }
    let was_active = guard.hud.is_active();
    let hwnd = HWND(MESSAGE_HWND.load(Ordering::Acquire) as _);
    if was_active {
        dismiss_hud_locked(guard, hwnd, HudCloseReason::DisplayChanged);
    }
    if fresh.virtual_screen != guard.hud.virtual_screen() {
        unsafe { guard.hud.rebuild_display()? };
    }
    println!("[display] monitors refreshed ({})", fresh.monitors.len());
    let _ = io::stdout().flush();
    guard.display = fresh;
    Ok(was_active)
}

unsafe fn discard_queued_mouse_clicks() {
    let mut msg = MSG::default();
    while PeekMessageW(&mut msg, None, WM_LBUTTONDOWN, WM_MBUTTONDBLCLK, PM_REMOVE).into() {}
    while PeekMessageW(
        &mut msg,
        None,
        WM_NCLBUTTONDOWN,
        WM_NCMBUTTONDBLCLK,
        PM_REMOVE,
    )
    .into()
    {}
}

/// メニューの所有者を前面にする。`WS_EX_NOACTIVATE` のままだと表示直後に閉じる。
unsafe fn foreground_menu_owner(hwnd: HWND) {
    let _ = ReleaseCapture();
    discard_queued_mouse_clicks();
    let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
    let _ = SetWindowLongPtrW(
        hwnd,
        GWL_EXSTYLE,
        style & !(WS_EX_NOACTIVATE.0 as isize),
    );
    let _ = SetForegroundWindow(hwnd);
}

unsafe fn restore_menu_owner(hwnd: HWND) {
    let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
    let _ = SetWindowLongPtrW(
        hwnd,
        GWL_EXSTYLE,
        style | WS_EX_NOACTIVATE.0 as isize,
    );
}

fn open_adjust_menu() {
    let prepared = {
        let Some(mut guard) = APP.get().and_then(|a| a.try_lock().ok()) else {
            return;
        };
        let Some(tray) = guard.tray.as_ref() else {
            spawn_calibrate(&mut guard);
            return;
        };
        Some((
            guard.detection_paused,
            tray.hwnd(),
            guard.pending_suggestion.as_ref().map(|s| s.summary.clone()),
        ))
    };
    let Some((paused, hwnd, suggestion)) = prepared else {
        return;
    };
    let cmd_id = unsafe { TrayIcon::track_menu(hwnd, paused, suggestion.as_deref()) };
    if cmd_id != 0 {
        handle_tray_command_sync(hwnd, cmd_id);
    }
}

fn spawn_calibrate(state: &mut AppState) {
    if let Some(child) = state.calibrate_child.as_mut() {
        if matches!(child.try_wait(), Ok(None)) {
            enter_calibrating(state);
            return;
        }
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    match Command::new(exe).arg("--calibrate").spawn() {
        Ok(child) => {
            state.calibrate_child = Some(child);
            enter_calibrating(state);
        }
        Err(e) => eprintln!("failed to launch --calibrate: {e}"),
    }
}

fn enter_calibrating(state: &mut AppState) {
    state.calibrating = true;
    state.hud_phase = HudPhase::Idle;
    stop_tracking_timer();
    if !state.hud.is_active() {
        return;
    }
    clear_hud_feedback_clock(state);
    state.chained_warp_started_at = None;
    if let Err(e) = unsafe { state.hud.dismiss() } {
        eprintln!("HUD dismiss error: {e}");
        return;
    }
    let raw = MESSAGE_HWND.load(Ordering::Acquire);
    if raw != 0 {
        unregister_esc_hotkey_state(state, HWND(raw as _));
    }
}

fn is_calibrate_mutex_present() -> bool {
    unsafe {
        match OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, CALIBRATE_MUTEX_NAME) {
            Ok(handle) => {
                let _ = CloseHandle(handle);
                true
            }
            Err(_) => false,
        }
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

    let mutex_held = is_calibrate_mutex_present();
    if mutex_held {
        enter_calibrating(&mut guard);
    } else if guard.calibrate_child.is_some() {
        guard.calibrating = true;
    } else {
        guard.calibrating = false;
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
            let (paused, hwnd, suggestion) = {
                let guard = match APP.get().and_then(|a| a.try_lock().ok()) {
                    Some(g) => g,
                    None => return,
                };
                let Some(tray) = guard.tray.as_ref() else {
                    return;
                };
                (
                    guard.detection_paused,
                    tray.hwnd(),
                    guard.pending_suggestion.as_ref().map(|s| s.summary.clone()),
                )
            };

            let cmd_id = unsafe { TrayIcon::track_menu(hwnd, paused, suggestion.as_deref()) };
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
                    cancel_settling(&mut guard);
                }
                let paused = guard.detection_paused;
                if let Some(tray) = guard.tray.as_mut() {
                    unsafe { tray.set_paused(paused) };
                }
            }
        }
        IDM_STARTUP => {
            let enabled = startup::is_startup_enabled();
            if let Err(e) = startup::set_startup_enabled(!enabled) {
                eprintln!("startup toggle: {e}");
            }
        }
        IDM_APPLY_SUGGESTION => {
            if let Some(mut guard) = APP.get().and_then(|a| a.try_lock().ok()) {
                apply_pending_suggestion(&mut guard);
            }
        }
        IDM_DISMISS_SUGGESTION => {
            if let Some(mut guard) = APP.get().and_then(|a| a.try_lock().ok()) {
                clear_suggestion_state(&mut guard);
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
    let track_dismiss = poll_tracking(&mut guard, Instant::now()).unwrap_or(false);
    let should_dismiss_oob =
        !track_dismiss && is_anchored(&guard) && hud_pointer_out_of_bounds(&guard.hud);
    let should_auto_timeout = !track_dismiss
        && !should_dismiss_oob
        && is_anchored(&guard)
        && guard
            .chained_warp_started_at
            .is_some_and(|t| chained_warp_auto_dismiss_due(t, Instant::now()));
    drop(guard);
    if track_dismiss {
        dismiss_hud_with_log(HudCloseReason::TrackingTimeout);
    } else if should_dismiss_oob {
        dismiss_hud_with_log(HudCloseReason::OutOfBounds);
    } else if should_auto_timeout {
        dismiss_hud_with_log(HudCloseReason::AutoTimeout);
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

fn mono_ms(origin: Instant, now: Instant) -> u64 {
    now.saturating_duration_since(origin).as_millis() as u64
}

fn is_anchored(state: &AppState) -> bool {
    matches!(state.hud_phase, HudPhase::Anchored)
}

fn cancel_settling(state: &mut AppState) {
    if matches!(state.hud_phase, HudPhase::Settling(_)) {
        state.hud_phase = HudPhase::Idle;
    }
}

fn stop_tracking_timer() {
    let raw = MESSAGE_HWND.load(Ordering::Acquire);
    if raw != 0 {
        unsafe {
            let _ = KillTimer(HWND(raw as _), TRACK_TIMER_ID);
        }
    }
}

fn begin_tracking(guard: &mut AppState, now: Instant) -> io::Result<()> {
    let _ = refresh_display_if_stale(guard)?;
    bind_hud_map(guard);
    let (x, y) = cursor_screen_pos().unwrap_or((0, 0));
    let t = mono_ms(guard.clock_origin, now);
    unsafe { guard.hud.activate_tracking(POINT { x, y })? };
    guard.hud_phase = HudPhase::Tracking {
        controller: TrackingController::new(t, (x, y)),
        last_paint_ms: t,
    };
    guard.feedback_circle_hud = false;
    let raw = MESSAGE_HWND.load(Ordering::Acquire);
    if raw != 0 {
        let hwnd = HWND(raw as _);
        unsafe {
            let _ = SetTimer(hwnd, TRACK_TIMER_ID, TRACK_TIMER_MS, None);
        }
        unsafe { register_esc_hotkey_if_needed(hwnd, guard)? };
    }
    println!("[detect] Shake (tracking)");
    let _ = io::stdout().flush();
    Ok(())
}

fn anchor_tracking(guard: &mut AppState) -> io::Result<()> {
    if let Some((x, y)) = cursor_screen_pos() {
        unsafe { guard.hud.move_to(POINT { x, y })? };
    }
    let hint = guard.pending_suggestion.as_ref().map(|s| s.summary.clone());
    unsafe { guard.hud.anchor(hint.as_deref())? };
    guard.hud_phase = HudPhase::Anchored;
    stop_tracking_timer();
    guard.feedback_circle_hud = false;
    println!("[detect] Shake anchored");
    let _ = io::stdout().flush();
    Ok(())
}

/// `true` のとき呼び出し側が HUD を閉じる。
fn poll_tracking(guard: &mut AppState, now: Instant) -> io::Result<bool> {
    if !matches!(guard.hud_phase, HudPhase::Tracking { .. }) {
        return Ok(false);
    }
    let Some((x, y)) = cursor_screen_pos() else {
        return Ok(false);
    };
    let t = mono_ms(guard.clock_origin, now);
    let (action, paint) = {
        let HudPhase::Tracking {
            controller,
            last_paint_ms,
        } = &mut guard.hud_phase
        else {
            return Ok(false);
        };
        let action = controller.update(t, (x, y));
        let paint = matches!(action, TrackingAction::Continue)
            && t.saturating_sub(*last_paint_ms) >= PAINT_INTERVAL_MS;
        if paint {
            *last_paint_ms = t;
        }
        (action, paint)
    };
    match action {
        TrackingAction::Continue => {
            if paint {
                bind_hud_map(guard);
                unsafe { guard.hud.move_to(POINT { x, y })? };
            }
            Ok(false)
        }
        TrackingAction::Anchor => {
            anchor_tracking(guard)?;
            Ok(false)
        }
        TrackingAction::TimeoutDismiss => Ok(true),
    }
}

fn settle_decision(pending: &PendingSettling, now: Instant) -> SettleDecision {
    let elapsed_ms = now
        .saturating_duration_since(pending.started_at)
        .as_millis() as u64;
    let quiet_ms = now
        .saturating_duration_since(pending.last_significant_motion_at)
        .as_millis() as u64;
    decide_settle(elapsed_ms, quiet_ms, policy_for(pending.source))
}

fn take_settling(guard: &mut AppState) -> Option<PendingSettling> {
    if !matches!(guard.hud_phase, HudPhase::Settling(_)) {
        return None;
    }
    match std::mem::replace(&mut guard.hud_phase, HudPhase::Idle) {
        HudPhase::Settling(pending) => Some(pending),
        other => {
            guard.hud_phase = other;
            None
        }
    }
}

fn apply_settle_decision(guard: &mut AppState, decision: SettleDecision) -> io::Result<()> {
    match decision {
        SettleDecision::Wait => Ok(()),
        SettleDecision::Open => {
            let pending = take_settling(guard).expect("pending_settling");
            open_hud_after_settle(guard, pending.source, pending.is_learning_target)
        }
        SettleDecision::Cancel => {
            let pending = take_settling(guard).expect("pending_settling");
            println!(
                "[detect] {:?} settle cancelled (still moving)",
                pending.source
            );
            let _ = io::stdout().flush();
            Ok(())
        }
    }
}

fn advance_settling(guard: &mut AppState, dx: f32, dy: f32, now: Instant) -> io::Result<()> {
    let decision = {
        let HudPhase::Settling(pending) = &mut guard.hud_phase else {
            return Ok(());
        };
        settling_note_motion(pending, dx, dy, now);
        settle_decision(pending, now)
    };
    apply_settle_decision(guard, decision)
}

fn open_hud_after_settle(
    guard: &mut AppState,
    source: TriggerSource,
    is_learning_target: bool,
) -> io::Result<()> {
    let _ = refresh_display_if_stale(guard)?;
    bind_hud_map(guard);
    let message_hwnd = HWND(MESSAGE_HWND.load(Ordering::Acquire) as _);
    let hint = guard.pending_suggestion.as_ref().map(|s| s.summary.clone());
    let hud_diag = unsafe {
        guard
            .hud
            .activate_at_cursor(hint.as_deref())
            .map_err(|e| io::Error::other(format!("HUD activate: {e}")))?
    };
    let dpi = unsafe { GetDpiForSystem() };
    println!("{}", hud_diag.format_line(dpi));
    let _ = io::stdout().flush();
    unsafe { register_esc_hotkey_if_needed(message_hwnd, guard)? };
    println!("[detect] {:?} (HUD open; click cell to warp)", source);
    let _ = io::stdout().flush();
    if is_learning_target {
        note_circle_hud_opened(guard);
    } else {
        guard.feedback_circle_hud = false;
    }
    guard.hud_phase = HudPhase::Anchored;
    Ok(())
}

fn try_finish_settling(guard: &mut AppState) -> io::Result<()> {
    if !matches!(guard.hud_phase, HudPhase::Settling(_)) {
        return Ok(());
    }
    advance_settling(guard, 0.0, 0.0, Instant::now())
}

fn update_pending_arc_debug(state: &mut AppState) {
    let stats = state.coordinator.circle.window_stats();
    if stats.net_angle_deg <= DEBUG_ARC_MIN_DEG {
        return;
    }
    let Some(reason) = state.coordinator.circle.circle_reject_reason() else {
        return;
    };
    state.pending_arc_debug = Some((stats, reason.to_string()));
}

unsafe fn handle_wm_input(lparam: LPARAM) -> io::Result<()> {
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

    let mouse = raw.data.mouse;

    let mut guard = match APP.get().and_then(|a| a.try_lock().ok()) {
        Some(g) => g,
        None => return Ok(()),
    };

    if guard.hud.is_active() {
        if matches!(guard.hud_phase, HudPhase::Tracking { .. }) {
            let dismiss = poll_tracking(&mut guard, Instant::now())?;
            drop(guard);
            if dismiss {
                dismiss_hud_with_log(HudCloseReason::TrackingTimeout);
            }
            return Ok(());
        }
        let dx = mouse.lLastX as f32;
        let dy = mouse.lLastY as f32;
        if is_anchored(&guard) && (dx != 0.0 || dy != 0.0) {
            if let Some((x, y)) = cursor_screen_pos() {
                if guard.hud.pointer_beyond_dismiss_margin(x, y) {
                    drop(guard);
                    dismiss_hud_with_log(HudCloseReason::OutOfBounds);
                    return Ok(());
                }
                let _ = guard.hud.update_hover_from_screen(x, y);
                note_chained_warp_activity(&mut guard);
            }
        }
        return Ok(());
    }

    apply_button_flags(&mut guard, &mouse);

    if guard.detection_paused || guard.calibrating || guard.buttons_down > 0 {
        guard.coordinator.reset();
    }

    if guard.detection_paused || guard.calibrating {
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
        }
        dt
    } else {
        0
    };

    if dx != 0.0 || dy != 0.0 {
        push_motion_history(&mut guard, dx, dy, dt_millis);
    }

    if matches!(guard.hud_phase, HudPhase::Settling(_)) {
        advance_settling(&mut guard, dx, dy, now)?;
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
    let button_down = guard.buttons_down > 0;
    let paused = guard.detection_paused;
    let calibrating = guard.calibrating;

    if let Some(result) = guard
        .coordinator
        .push(sample, button_down, paused, calibrating)
    {
        guard.pending_arc_debug = None;
        match result.source {
            TriggerSource::Circle => {
                if result.is_learning_target {
                    guard.last_fire_clock_ms = Some(guard.input_clock_ms);
                }
                guard.hud_phase = HudPhase::Settling(PendingSettling {
                    started_at: now,
                    last_significant_motion_at: now,
                    source: result.source,
                    is_learning_target: result.is_learning_target,
                });
            }
            TriggerSource::Shake => begin_tracking(&mut guard, now)?,
        }
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
        .coordinator
        .circle
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

const MOTION_HISTORY_MS: u64 = 2000;
const FEEDBACK_EVENT_CAP: usize = 10;

fn push_motion_history(state: &mut AppState, dx: f32, dy: f32, dt_millis: u64) {
    state.input_clock_ms = state.input_clock_ms.saturating_add(dt_millis.max(1));
    state.raw_motion_history.push_back((
        dx.round() as i32,
        dy.round() as i32,
        state.input_clock_ms,
    ));
    while let Some((_, _, t)) = state.raw_motion_history.front() {
        if state.input_clock_ms.saturating_sub(*t) > MOTION_HISTORY_MS {
            state.raw_motion_history.pop_front();
        } else {
            break;
        }
    }
}

fn push_feedback_event(state: &mut AppState, event: FeedbackEvent) {
    if state.feedback_events.len() >= FEEDBACK_EVENT_CAP {
        state.feedback_events.pop_front();
    }
    state.feedback_events.push_back(event);
    refresh_pending_suggestion(state);
}

fn refresh_pending_suggestion(state: &mut AppState) {
    let events: Vec<FeedbackEvent> = state.feedback_events.iter().cloned().collect();
    state.pending_suggestion = generate_suggestion(state.coordinator.circle.config(), &events);
    sync_tray_suggestion(state);
    if let Some(suggestion) = &state.pending_suggestion {
        eprintln!("[adapt] suggestion: {}", suggestion.summary);
        let _ = io::stderr().flush();
    }
}

fn sync_tray_suggestion(state: &mut AppState) {
    let pending = state.pending_suggestion.is_some();
    if let Some(tray) = state.tray.as_mut() {
        unsafe {
            tray.set_suggest_pending(pending);
        }
    }
}

fn clear_suggestion_state(state: &mut AppState) {
    state.feedback_events.clear();
    state.pending_suggestion = None;
    sync_tray_suggestion(state);
}

fn apply_pending_suggestion(state: &mut AppState) {
    let Some(suggestion) = state.pending_suggestion.take() else {
        return;
    };
    let mut cfg = AppConfig::load_or_default();
    cfg.detector = suggestion.new_config.clone();
    if let Err(e) = cfg.save() {
        eprintln!("failed to save suggestion: {e}");
        state.pending_suggestion = Some(suggestion);
        return;
    }
    state
        .coordinator
        .circle
        .replace_config(suggestion.new_config);
    cancel_settling(state);
    state.config_mtime = config_file_mtime();
    clear_suggestion_state(state);
}

fn stroke_metrics_ending_at(state: &AppState, end_ms: u64) -> Option<StrokeMetrics> {
    let window = state.coordinator.circle.config().window_ms;
    let start = end_ms.saturating_sub(window);
    let samples: Vec<(f32, f32, u64)> = state
        .raw_motion_history
        .iter()
        .filter(|(_, _, t)| *t >= start && *t <= end_ms)
        .map(|(dx, dy, t)| (*dx as f32, *dy as f32, *t))
        .collect();
    analyze_relative_stroke(&samples)
}

fn note_circle_hud_opened(state: &mut AppState) {
    let success_t = state.last_fire_clock_ms.unwrap_or(state.input_clock_ms);
    let history: Vec<(i32, i32, u64)> = state.raw_motion_history.iter().copied().collect();
    if let Some(event) =
        extract_best_near_miss(&history, success_t, state.coordinator.circle.config())
    {
        push_feedback_event(state, event);
    }
    state.last_trigger_metrics = stroke_metrics_ending_at(state, success_t);
    state.hud_opened_at = Some(Instant::now());
    state.hud_opened_cursor = cursor_screen_pos();
    state.feedback_circle_hud = true;
}

fn maybe_record_accidental(state: &mut AppState, reason: HudCloseReason) {
    if !state.feedback_circle_hud {
        return;
    }
    let Some(opened_at) = state.hud_opened_at else {
        return;
    };
    let Some((ox, oy)) = state.hud_opened_cursor else {
        return;
    };
    let Some(metrics) = state.last_trigger_metrics.clone() else {
        return;
    };
    let elapsed_ms = Instant::now()
        .saturating_duration_since(opened_at)
        .as_millis() as u64;
    let (cx, cy) = cursor_screen_pos().unwrap_or((ox, oy));
    let displacement = ((cx - ox) as f32).hypot((cy - oy) as f32);
    if is_accidental_trigger(0, elapsed_ms, displacement, reason) {
        push_feedback_event(state, FeedbackEvent::AccidentalTrigger { metrics });
    }
}

fn clear_hud_feedback_clock(state: &mut AppState) {
    state.hud_opened_at = None;
    state.hud_opened_cursor = None;
    state.feedback_circle_hud = false;
}

fn dismiss_hud_with_log(reason: HudCloseReason) {
    let raw = MESSAGE_HWND.load(Ordering::Acquire);
    if raw == 0 {
        return;
    }
    let message_hwnd = HWND(raw as _);
    let mut guard = match APP.get().and_then(|a| a.try_lock().ok()) {
        Some(g) => g,
        None => return,
    };
    dismiss_hud_locked(&mut guard, message_hwnd, reason);
}

fn dismiss_hud_locked(guard: &mut AppState, message_hwnd: HWND, reason: HudCloseReason) {
    if !guard.hud.is_active() {
        return;
    }
    maybe_record_accidental(guard, reason);
    clear_hud_feedback_clock(guard);
    guard.hud_phase = HudPhase::Idle;
    stop_tracking_timer();
    guard.chained_warp_started_at = None;
    if let Err(e) = unsafe { guard.hud.dismiss() } {
        eprintln!("HUD dismiss error: {e}");
        return;
    }
    unregister_esc_hotkey_state(guard, message_hwnd);
    println!("[hud] dismissed ({})", reason.as_str());
    let _ = io::stdout().flush();
}

fn bind_hud_map(guard: &mut AppState) {
    let style = guard.hud_style;
    let monitors = guard.display.monitors.clone();
    guard.hud.prepare_map(style, &monitors);
}

fn warp_hud_to_cell(col: usize, row: usize) {
    let mut guard = match APP.get().and_then(|a| a.try_lock().ok()) {
        Some(g) => g,
        None => return,
    };
    if !guard.hud.is_active() {
        return;
    }
    match refresh_display_if_stale(&mut guard) {
        Ok(true) => {
            println!("[warp] skipped; display changed");
            let _ = io::stdout().flush();
            return;
        }
        Err(e) => {
            eprintln!("[display] refresh failed: {e}");
            return;
        }
        Ok(false) => {}
    }
    let vs = guard.hud.virtual_screen();
    let raw = warp_target_for_cell(col, row, vs);
    let (target_x, target_y) = clamp_to_monitors(raw, &guard.display.monitors);
    if (target_x, target_y) != raw {
        println!(
            "[warp] clamped ({}, {}) -> ({target_x}, {target_y})",
            raw.0, raw.1
        );
    }
    drop(guard);
    warp_hud_to_point(target_x, target_y, format!("cell: {col}, {row}"));
}

fn warp_hud_to_point(target_x: i32, target_y: i32, detail: String) {
    let mut guard = match APP.get().and_then(|a| a.try_lock().ok()) {
        Some(g) => g,
        None => return,
    };
    if !guard.hud.is_active() {
        return;
    }
    if let Err(e) = unsafe { SetCursorPos(target_x, target_y) } {
        eprintln!("SetCursorPos error: {e}");
        return;
    }
    println!("[warp] Jumped to ({target_x}, {target_y}) [{detail}]");
    let _ = io::stdout().flush();
    bind_hud_map(&mut guard);
    let hint = guard.pending_suggestion.as_ref().map(|s| s.summary.clone());
    if let Err(e) = unsafe { guard.hud.activate_at_cursor(hint.as_deref()) } {
        eprintln!("[hud] Failed to re-activate at landing point: {e}");
        return;
    }
    guard.hud_phase = HudPhase::Anchored;
    stop_tracking_timer();
    guard.chained_warp_started_at = Some(Instant::now());
    guard.hud_opened_at = Some(Instant::now());
    guard.hud_opened_cursor = cursor_screen_pos();
    guard.feedback_circle_hud = false;
}

fn handle_hud_left_button_up() {
    let mut point = POINT::default();
    if unsafe { GetCursorPos(&mut point) }.is_err() {
        return;
    }
    let mut guard = match APP.get().and_then(|a| a.try_lock().ok()) {
        Some(g) => g,
        None => return,
    };
    if !guard.hud.is_active() {
        return;
    }
    if matches!(guard.hud_phase, HudPhase::Tracking { .. }) {
        if let Err(e) = anchor_tracking(&mut guard) {
            eprintln!("anchor: {e}");
        }
        return;
    }
    if !is_anchored(&guard) {
        return;
    }
    if guard.hud.badge_contains(point.x, point.y) {
        drop(guard);
        let raw = MESSAGE_HWND.load(Ordering::Acquire);
        if raw == 0 {
            return;
        }
        let hwnd = HWND(raw as _);
        // ボタンアップの処理中に開くと、その離した操作がメニュー外クリックになる。
        unsafe { foreground_menu_owner(hwnd) };
        dismiss_hud_with_log(HudCloseReason::OpenAdjust);
        let posted = unsafe { PostMessageW(hwnd, WM_OPEN_ADJUST, WPARAM(0), LPARAM(0)) };
        if posted.is_err() {
            unsafe { restore_menu_owner(hwnd) };
        }
        return;
    }
    if guard.hud.origin_hit_test_screen(point.x, point.y) {
        drop(guard);
        dismiss_hud_with_log(HudCloseReason::OriginClick);
        return;
    }
    if guard.hud.uses_monitor_map() {
        if let Some((index, target)) = guard.hud.monitor_target_at_screen(point.x, point.y) {
            drop(guard);
            warp_hud_to_point(target.0, target.1, format!("monitor {}", index + 1));
            return;
        }
        if guard.hud.screen_in_panel(point.x, point.y) {
            return;
        }
        drop(guard);
        dismiss_hud_with_log(HudCloseReason::OutsideClick);
        return;
    }
    if let Some((col, row)) = guard.hud.minimap_cell_at_screen(point.x, point.y) {
        drop(guard);
        warp_hud_to_cell(col, row);
        return;
    }
    drop(guard);
    dismiss_hud_with_log(HudCloseReason::OutsideClick);
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
                    if guard.hud.is_active() && matches!(guard.hud_phase, HudPhase::Tracking { .. })
                    {
                        match poll_tracking(&mut guard, Instant::now()) {
                            Ok(true) => {
                                drop(guard);
                                dismiss_hud_with_log(HudCloseReason::TrackingTimeout);
                            }
                            Err(e) => eprintln!("tracking: {e}"),
                            Ok(false) => {}
                        }
                        return LRESULT(0);
                    }
                    if guard.hud.is_active() && is_anchored(&guard) {
                        if guard.hud.pointer_beyond_dismiss_margin(point.x, point.y) {
                            drop(guard);
                            dismiss_hud_with_log(HudCloseReason::OutOfBounds);
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
            dismiss_hud_with_log(HudCloseReason::RightClick);
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
                if PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0)).is_ok() {
                    return TRUE;
                }
            }
            FALSE
        }
        _ => FALSE,
    }
}
