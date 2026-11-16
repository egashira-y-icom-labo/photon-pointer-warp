//! `WH_MOUSE_LL` — 中ボタン Arming / ホイール消費 / SendInput 注入

use std::io;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEINPUT, VIRTUAL_KEY,
    VK_CONTROL, VK_LWIN, VK_MENU, VK_SHIFT, VK_RWIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, PostMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HHOOK, MSLLHOOKSTRUCT,
    WH_MOUSE_LL, WM_APP, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEWHEEL,
    WM_RBUTTONDOWN, WM_XBUTTONDOWN, WM_XBUTTONUP, XBUTTON1, XBUTTON2,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;

/// メッセージ窓向け。`wparam` = [`WaveHookKind`], `lparam` = 座標またはホイール delta。
pub const WM_WAVE_HOOK: u32 = WM_APP + 20;

#[repr(usize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaveHookKind {
    MiddleDown = 1,
    MiddleUp = 2,
    Wheel = 3,
    XButtonBackDown = 4,
    XButtonBackUp = 5,
    XButtonForwardDown = 6,
    XButtonForwardUp = 7,
    LeftDown = 8,
    RightDown = 9,
}

impl WaveHookKind {
    pub fn from_usize(v: usize) -> Option<Self> {
        Some(match v {
            1 => Self::MiddleDown,
            2 => Self::MiddleUp,
            3 => Self::Wheel,
            4 => Self::XButtonBackDown,
            5 => Self::XButtonBackUp,
            6 => Self::XButtonForwardDown,
            7 => Self::XButtonForwardUp,
            8 => Self::LeftDown,
            9 => Self::RightDown,
            _ => return None,
        })
    }
}

static HOOK: AtomicIsize = AtomicIsize::new(0);
static TARGET_HWND: AtomicIsize = AtomicIsize::new(0);
static INJECTING: AtomicBool = AtomicBool::new(false);
/// Pause / 修飾キー / マップ HUD 中など、フックを素通しする。
static PASSTHROUGH: AtomicBool = AtomicBool::new(false);
/// 中ボタン系列を消費中（Arming 以降）。
static CONSUME_MIDDLE: AtomicBool = AtomicBool::new(false);
/// Arming / Expanding 中はホイールを捨てる。Frozen では通知する。
static IGNORE_WHEEL: AtomicBool = AtomicBool::new(false);
static FORWARD_WHEEL: AtomicBool = AtomicBool::new(false);
/// 射撃残留中: 左右クリックを通知するが消費しない（OS へ素通し）。
static LINGER_WATCH: AtomicBool = AtomicBool::new(false);

pub struct MouseLlHook;

impl MouseLlHook {
    pub unsafe fn install(hwnd: HWND) -> io::Result<Self> {
        TARGET_HWND.store(hwnd.0 as isize, Ordering::Release);
        let module = GetModuleHandleW(None)
            .map_err(|e| io::Error::other(format!("GetModuleHandleW: {e}")))?;
        let hook = SetWindowsHookExW(WH_MOUSE_LL, Some(low_level_mouse_proc), module, 0)
            .map_err(|e| io::Error::other(format!("SetWindowsHookExW: {e}")))?;
        HOOK.store(hook.0 as isize, Ordering::Release);
        Ok(Self)
    }

    pub fn set_passthrough(enabled: bool) {
        PASSTHROUGH.store(enabled, Ordering::Release);
    }

    pub fn set_consume_middle(enabled: bool) {
        CONSUME_MIDDLE.store(enabled, Ordering::Release);
    }

    pub fn set_ignore_wheel(enabled: bool) {
        IGNORE_WHEEL.store(enabled, Ordering::Release);
    }

    pub fn set_forward_wheel(enabled: bool) {
        FORWARD_WHEEL.store(enabled, Ordering::Release);
    }

    pub fn set_linger_watch(enabled: bool) {
        LINGER_WATCH.store(enabled, Ordering::Release);
    }
}

impl Drop for MouseLlHook {
    fn drop(&mut self) {
        let raw = HOOK.swap(0, Ordering::AcqRel);
        if raw != 0 {
            unsafe {
                let _ = UnhookWindowsHookEx(HHOOK(raw as _));
            }
        }
        TARGET_HWND.store(0, Ordering::Release);
        CONSUME_MIDDLE.store(false, Ordering::Release);
        IGNORE_WHEEL.store(false, Ordering::Release);
        FORWARD_WHEEL.store(false, Ordering::Release);
        LINGER_WATCH.store(false, Ordering::Release);
    }
}

fn modifiers_down() -> bool {
    unsafe {
        key_down(VK_CONTROL) || key_down(VK_SHIFT) || key_down(VK_MENU) || key_down(VK_LWIN) || key_down(VK_RWIN)
    }
}

unsafe fn key_down(vk: VIRTUAL_KEY) -> bool {
    GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000 != 0
}

fn pack_point(x: i32, y: i32) -> isize {
    ((x as u32 & 0xFFFF) | ((y as u32 & 0xFFFF) << 16)) as isize
}

pub fn unpack_point(packed: isize) -> (i32, i32) {
    let v = packed as u32;
    let x = (v & 0xFFFF) as i16 as i32;
    let y = ((v >> 16) & 0xFFFF) as i16 as i32;
    (x, y)
}

fn post_hook(kind: WaveHookKind, lparam: isize) {
    let hwnd_raw = TARGET_HWND.load(Ordering::Acquire);
    if hwnd_raw == 0 {
        return;
    }
    unsafe {
        let _ = PostMessageW(
            HWND(hwnd_raw as _),
            WM_WAVE_HOOK,
            WPARAM(kind as usize),
            LPARAM(lparam),
        );
    }
}

unsafe extern "system" fn low_level_mouse_proc(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(HHOOK(HOOK.load(Ordering::Acquire) as _), code, wparam, lparam);
    }
    if INJECTING.load(Ordering::Acquire) {
        return CallNextHookEx(HHOOK(HOOK.load(Ordering::Acquire) as _), code, wparam, lparam);
    }

    let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
    // 注入イベントは素通し
    if info.flags & 0x01 != 0 {
        // LLMHF_INJECTED
        return CallNextHookEx(HHOOK(HOOK.load(Ordering::Acquire) as _), code, wparam, lparam);
    }

    let msg = wparam.0 as u32;
    let passthrough = PASSTHROUGH.load(Ordering::Acquire) || modifiers_down();

    match msg {
        WM_MBUTTONDOWN => {
            if passthrough {
                return CallNextHookEx(HHOOK(HOOK.load(Ordering::Acquire) as _), code, wparam, lparam);
            }
            // Arming 開始候補。消費して本体へ。
            CONSUME_MIDDLE.store(true, Ordering::Release);
            IGNORE_WHEEL.store(true, Ordering::Release);
            post_hook(
                WaveHookKind::MiddleDown,
                pack_point(info.pt.x, info.pt.y),
            );
            return LRESULT(1);
        }
        WM_MBUTTONUP => {
            if CONSUME_MIDDLE.load(Ordering::Acquire) {
                post_hook(WaveHookKind::MiddleUp, 0);
                return LRESULT(1);
            }
        }
        WM_MOUSEWHEEL => {
            if IGNORE_WHEEL.load(Ordering::Acquire) {
                return LRESULT(1);
            }
            if FORWARD_WHEEL.load(Ordering::Acquire) {
                let delta = ((info.mouseData >> 16) as i16) as i32;
                post_hook(WaveHookKind::Wheel, delta as isize);
                return LRESULT(1);
            }
        }
        WM_XBUTTONDOWN | WM_XBUTTONUP => {
            if !CONSUME_MIDDLE.load(Ordering::Acquire) && !FORWARD_WHEEL.load(Ordering::Acquire) {
                // 波動外は素通し
            } else {
                let hi = ((info.mouseData >> 16) & 0xFFFF) as u16;
                let down = msg == WM_XBUTTONDOWN;
                let kind = if hi == XBUTTON1 {
                    if down {
                        WaveHookKind::XButtonBackDown
                    } else {
                        WaveHookKind::XButtonBackUp
                    }
                } else if hi == XBUTTON2 {
                    if down {
                        WaveHookKind::XButtonForwardDown
                    } else {
                        WaveHookKind::XButtonForwardUp
                    }
                } else {
                    return CallNextHookEx(
                        HHOOK(HOOK.load(Ordering::Acquire) as _),
                        code,
                        wparam,
                        lparam,
                    );
                };
                if CONSUME_MIDDLE.load(Ordering::Acquire) || FORWARD_WHEEL.load(Ordering::Acquire) {
                    post_hook(kind, 0);
                    return LRESULT(1);
                }
            }
        }
        WM_LBUTTONDOWN => {
            if LINGER_WATCH.load(Ordering::Acquire) {
                // 残留解除用に通知するが、クリック自体は OS へ通す
                post_hook(
                    WaveHookKind::LeftDown,
                    pack_point(info.pt.x, info.pt.y),
                );
                return CallNextHookEx(
                    HHOOK(HOOK.load(Ordering::Acquire) as _),
                    code,
                    wparam,
                    lparam,
                );
            }
            if FORWARD_WHEEL.load(Ordering::Acquire) || CONSUME_MIDDLE.load(Ordering::Acquire) {
                // Frozen 中のみ本体で射撃解釈。Expanding 中も消費して誤クリック防止。
                if FORWARD_WHEEL.load(Ordering::Acquire) {
                    post_hook(WaveHookKind::LeftDown, 0);
                    return LRESULT(1);
                }
            }
        }
        WM_RBUTTONDOWN => {
            if LINGER_WATCH.load(Ordering::Acquire) {
                post_hook(
                    WaveHookKind::RightDown,
                    pack_point(info.pt.x, info.pt.y),
                );
                return CallNextHookEx(
                    HHOOK(HOOK.load(Ordering::Acquire) as _),
                    code,
                    wparam,
                    lparam,
                );
            }
            if FORWARD_WHEEL.load(Ordering::Acquire) || CONSUME_MIDDLE.load(Ordering::Acquire) {
                if FORWARD_WHEEL.load(Ordering::Acquire) || CONSUME_MIDDLE.load(Ordering::Acquire) {
                    post_hook(WaveHookKind::RightDown, 0);
                    return LRESULT(1);
                }
            }
        }
        _ => {}
    }

    CallNextHookEx(HHOOK(HOOK.load(Ordering::Acquire) as _), code, wparam, lparam)
}

fn with_injecting(f: impl FnOnce()) {
    INJECTING.store(true, Ordering::Release);
    f();
    INJECTING.store(false, Ordering::Release);
}

fn send_mouse_flags(flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS) {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        let _ = SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

/// 消費済みの中押しをドラッグとして再開する。
pub fn inject_middle_down() {
    with_injecting(|| {
        CONSUME_MIDDLE.store(false, Ordering::Release);
        IGNORE_WHEEL.store(false, Ordering::Release);
        send_mouse_flags(MOUSEEVENTF_MIDDLEDOWN);
    });
}

/// 短押し中クリックを再現する。
pub fn inject_middle_click() {
    with_injecting(|| {
        CONSUME_MIDDLE.store(false, Ordering::Release);
        IGNORE_WHEEL.store(false, Ordering::Release);
        send_mouse_flags(MOUSEEVENTF_MIDDLEDOWN);
        send_mouse_flags(MOUSEEVENTF_MIDDLEUP);
    });
}

pub fn inject_left_click() {
    with_injecting(|| {
        send_mouse_flags(MOUSEEVENTF_LEFTDOWN);
        send_mouse_flags(MOUSEEVENTF_LEFTUP);
    });
}

pub fn clear_consume_flags() {
    CONSUME_MIDDLE.store(false, Ordering::Release);
    IGNORE_WHEEL.store(false, Ordering::Release);
    FORWARD_WHEEL.store(false, Ordering::Release);
    LINGER_WATCH.store(false, Ordering::Release);
}

#[allow(dead_code)]
fn _hinstance_ty(_: HINSTANCE) {}
