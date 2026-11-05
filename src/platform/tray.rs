//! タスクトレイ常駐（純 Win32 / `Shell_NotifyIconW`）

use std::io;
use std::mem::size_of;

use crate::platform::startup;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIcon, CreatePopupMenu, DestroyIcon, DestroyMenu, GetCursorPos, PostMessageW,
    SetForegroundWindow, TrackPopupMenuEx, HICON, MF_CHECKED, MF_SEPARATOR, MF_STRING,
    MF_UNCHECKED, TPM_BOTTOMALIGN, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTALIGN, TPM_RIGHTBUTTON,
    WM_APP, WM_LBUTTONDBLCLK, WM_NULL, WM_RBUTTONUP,
};

pub const WM_TRAYICON: u32 = WM_APP + 1;
pub const TRAY_UID: u32 = 1;

pub const IDM_CALIBRATE: usize = 4001;
pub const IDM_PAUSE: usize = 4002;
pub const IDM_EXIT: usize = 4003;
pub const IDM_APPLY_SUGGESTION: usize = 4004;
pub const IDM_DISMISS_SUGGESTION: usize = 4005;
pub const IDM_STARTUP: usize = 4006;
pub const IDM_TOGGLE_THUMBNAIL: usize = 4007;

const TIP_ACTIVE: &str = "Photon Pointer Warp";
const TIP_PAUSED: &str = "Photon Pointer Warp (一時停止中)";
const TIP_SUGGEST: &str = "Photon Pointer Warp (感度微調整の提案あり)";

pub struct TrayIcon {
    hwnd: HWND,
    icon_active: HICON,
    icon_paused: HICON,
    icon_suggest: HICON,
    paused: bool,
    suggest_pending: bool,
}

// HICON / HWND はメッセージスレッドのみ
unsafe impl Send for TrayIcon {}

impl TrayIcon {
    pub unsafe fn add(hwnd: HWND, instance: HINSTANCE) -> io::Result<Self> {
        let icon_active = create_circle_icon(instance, 50, 140, 230, false)?;
        let icon_paused = create_circle_icon(instance, 120, 120, 120, false)?;
        let icon_suggest = create_circle_icon(instance, 50, 140, 230, true)?;
        let tray = Self {
            hwnd,
            icon_active,
            icon_paused,
            icon_suggest,
            paused: false,
            suggest_pending: false,
        };
        let nid = tray.nid();
        if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
            // 既に残っている場合は更新で復帰
            if !Shell_NotifyIconW(NIM_MODIFY, &nid).as_bool() {
                return Err(io::Error::other("Shell_NotifyIconW NIM_ADD failed"));
            }
        }
        Ok(tray)
    }

    pub unsafe fn remove(&mut self) {
        let nid = self.nid();
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }

    /// Explorer 再起動後にアイコンを付け直す。
    pub unsafe fn readd(&mut self) {
        let nid = self.nid();
        if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    pub unsafe fn set_paused(&mut self, paused: bool) {
        if self.paused == paused {
            return;
        }
        self.paused = paused;
        let nid = self.nid();
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }

    pub unsafe fn set_suggest_pending(&mut self, pending: bool) {
        if self.suggest_pending == pending {
            return;
        }
        self.suggest_pending = pending;
        let nid = self.nid();
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// モーダルメニューを出し、選択 ID を返す（キャンセルは 0）。`WM_COMMAND` は送らない。
    pub unsafe fn track_menu(
        hwnd: HWND,
        paused: bool,
        thumbnail_enabled: bool,
        suggestion: Option<&str>,
    ) -> u32 {
        let mut pt = POINT::default();
        if GetCursorPos(&mut pt).is_err() {
            return 0;
        }
        let Ok(menu) = CreatePopupMenu() else {
            return 0;
        };
        let apply_label;
        if let Some(summary) = suggestion {
            apply_label = utf16_z(&format!("★ 感度の微調整を適用: {summary}"));
            let dismiss_label = utf16_z("提案を見送る");
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                IDM_APPLY_SUGGESTION,
                PCWSTR(apply_label.as_ptr()),
            );
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                IDM_DISMISS_SUGGESTION,
                PCWSTR(dismiss_label.as_ptr()),
            );
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, w!(""));
        }
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            IDM_CALIBRATE,
            w!("設定 / キャリブレーション..."),
        );
        let startup_flags = if startup::is_startup_enabled() {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING | MF_UNCHECKED
        };
        let _ = AppendMenuW(menu, startup_flags, IDM_STARTUP, w!("Windows 起動時に実行"));
        let thumbnail_flags = if thumbnail_enabled {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING | MF_UNCHECKED
        };
        let _ = AppendMenuW(
            menu,
            thumbnail_flags,
            IDM_TOGGLE_THUMBNAIL,
            w!("デスクトップ背景表示"),
        );
        let pause_flags = if paused {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING | MF_UNCHECKED
        };
        let _ = AppendMenuW(menu, pause_flags, IDM_PAUSE, w!("サークル検知を一時停止"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, w!(""));
        let _ = AppendMenuW(menu, MF_STRING, IDM_EXIT, w!("終了"));

        let _ = SetForegroundWindow(hwnd);
        let flags = TPM_RETURNCMD.0
            | TPM_NONOTIFY.0
            | TPM_RIGHTBUTTON.0
            | TPM_BOTTOMALIGN.0
            | TPM_RIGHTALIGN.0;
        let cmd = TrackPopupMenuEx(menu, flags, pt.x, pt.y, hwnd, None);
        let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        cmd.0.max(0) as u32
    }

    pub fn callback_event(lparam: LPARAM) -> Option<TrayEvent> {
        match lparam.0 as u32 {
            WM_RBUTTONUP => Some(TrayEvent::ContextMenu),
            WM_LBUTTONDBLCLK => Some(TrayEvent::OpenCalibrate),
            _ => None,
        }
    }

    fn nid(&self) -> NOTIFYICONDATAW {
        let mut nid = NOTIFYICONDATAW::default();
        nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = self.hwnd;
        nid.uID = TRAY_UID;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = WM_TRAYICON;
        nid.hIcon = if self.paused {
            self.icon_paused
        } else if self.suggest_pending {
            self.icon_suggest
        } else {
            self.icon_active
        };
        copy_tip(
            &mut nid.szTip,
            if self.paused {
                TIP_PAUSED
            } else if self.suggest_pending {
                TIP_SUGGEST
            } else {
                TIP_ACTIVE
            },
        );
        nid
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        unsafe {
            self.remove();
            let _ = DestroyIcon(self.icon_active);
            let _ = DestroyIcon(self.icon_paused);
            let _ = DestroyIcon(self.icon_suggest);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    ContextMenu,
    OpenCalibrate,
}

fn copy_tip(dst: &mut [u16; 128], text: &str) {
    dst.fill(0);
    let encoded: Vec<u16> = text.encode_utf16().take(127).collect();
    dst[..encoded.len()].copy_from_slice(&encoded);
}

fn utf16_z(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn paint_pixel(xor: &mut [u8], and_mask: &mut [u8], x: usize, y: usize, r: u8, g: u8, b: u8) {
    let i = (y * 16 + x) * 4;
    xor[i] = b;
    xor[i + 1] = g;
    xor[i + 2] = r;
    xor[i + 3] = 255;
    and_mask[y * 2 + x / 8] &= !(1u8 << (7 - (x % 8)));
}

unsafe fn create_circle_icon(
    instance: HINSTANCE,
    r: u8,
    g: u8,
    b: u8,
    suggest_dot: bool,
) -> io::Result<HICON> {
    let mut xor = vec![0u8; 16 * 16 * 4];
    let mut and_mask = vec![0xFFu8; 16 * 2];
    for y in 0..16 {
        for x in 0..16 {
            let dx = x as i32 - 8;
            let dy = y as i32 - 8;
            if dx * dx + dy * dy <= 36 {
                paint_pixel(&mut xor, &mut and_mask, x, y, r, g, b);
            }
        }
    }
    if suggest_dot {
        for y in 1..4 {
            for x in 12..15 {
                paint_pixel(&mut xor, &mut and_mask, x, y, 0, 220, 230);
            }
        }
    }
    CreateIcon(instance, 16, 16, 1, 32, and_mask.as_ptr(), xor.as_ptr())
        .map_err(|e| io::Error::other(format!("CreateIcon: {e}")))
}
