//! タスクトレイ常駐（純 Win32 / `Shell_NotifyIconW`）

use std::io;
use std::mem::size_of;

use crate::domain::adaptation::SuggestionDiff;
use crate::domain::language::{Language, LanguageConfig};
use crate::platform::i18n::{
    self, badge_apply, badge_dismiss, menu_calibrate, menu_dismiss_suggestion, menu_exit,
    menu_language, menu_language_auto, menu_language_en, menu_language_ja, menu_pause,
    menu_show_background, menu_startup, tip_active, tip_paused, tip_suggest,
};
use crate::platform::icon::{
    nearest_tray_edge, signature_image, tray_bgra, xor_and_masks, TrayGlyph,
};
use crate::platform::startup;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIcon, CreatePopupMenu, DestroyIcon, DestroyMenu, GetCursorPos,
    GetSystemMetrics, PostMessageW, SetForegroundWindow, TrackPopupMenuEx, HICON, MF_CHECKED,
    MF_POPUP, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, SM_CXSMICON, TPM_BOTTOMALIGN,
    TPM_NONOTIFY,
    TPM_RETURNCMD, TPM_RIGHTALIGN, TPM_RIGHTBUTTON, WM_APP, WM_LBUTTONDBLCLK, WM_NULL,
    WM_RBUTTONUP,
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
pub const IDM_LANG_AUTO: usize = 4008;
pub const IDM_LANG_EN: usize = 4009;
pub const IDM_LANG_JA: usize = 4010;

pub struct TrayIcon {
    hwnd: HWND,
    instance: HINSTANCE,
    icon_px: u32,
    icon_active: HICON,
    icon_paused: HICON,
    icon_suggest: HICON,
    paused: bool,
    suggest_pending: bool,
    language: Language,
}

// HICON / HWND はメッセージスレッドのみ
unsafe impl Send for TrayIcon {}

impl TrayIcon {
    pub unsafe fn add(hwnd: HWND, instance: HINSTANCE, language: Language) -> io::Result<Self> {
        let icon_px = current_small_icon_px();
        let (icon_active, icon_paused, icon_suggest) = create_state_icons(instance, icon_px)?;
        let tray = Self {
            hwnd,
            instance,
            icon_px,
            icon_active,
            icon_paused,
            icon_suggest,
            paused: false,
            suggest_pending: false,
            language,
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

    /// Explorer 再起動後にアイコンを付け直す。DPI が変わっていれば画素も作り直す。
    pub unsafe fn readd(&mut self) {
        let _ = self.rebuild_icons_if_needed();
        let nid = self.nid();
        if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    /// `SM_CXSMICON` が変わったときだけアイコンを作り直し、トレイへ反映する。
    pub unsafe fn sync_small_icon_size(&mut self) -> io::Result<()> {
        if !self.rebuild_icons_if_needed()? {
            return Ok(());
        }
        let nid = self.nid();
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        Ok(())
    }

    unsafe fn rebuild_icons_if_needed(&mut self) -> io::Result<bool> {
        let icon_px = current_small_icon_px();
        if icon_px == self.icon_px {
            return Ok(false);
        }
        let (active, paused, suggest) = create_state_icons(self.instance, icon_px)?;
        let _ = DestroyIcon(self.icon_active);
        let _ = DestroyIcon(self.icon_paused);
        let _ = DestroyIcon(self.icon_suggest);
        self.icon_active = active;
        self.icon_paused = paused;
        self.icon_suggest = suggest;
        self.icon_px = icon_px;
        Ok(true)
    }

    pub unsafe fn set_paused(&mut self, paused: bool) {
        if self.paused == paused {
            return;
        }
        self.paused = paused;
        let nid = self.nid();
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }

    pub unsafe fn set_language(&mut self, language: Language) {
        if self.language == language {
            return;
        }
        self.language = language;
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
    /// `anchor` が None のときはカーソル位置から右上へ開く（トレイ）。
    pub unsafe fn track_menu(
        hwnd: HWND,
        paused: bool,
        thumbnail_enabled: bool,
        suggestion: Option<&[SuggestionDiff]>,
        language: Language,
        language_config: LanguageConfig,
        kind: PopupMenu,
        anchor: Option<PopupAnchor>,
    ) -> u32 {
        let (x, y, align) = if let Some(anchor) = anchor {
            let align = if anchor.open_downward {
                0u32
            } else {
                TPM_BOTTOMALIGN.0 | TPM_RIGHTALIGN.0
            };
            (anchor.x, anchor.y, align)
        } else {
            let mut pt = POINT::default();
            if GetCursorPos(&mut pt).is_err() {
                return 0;
            }
            (pt.x, pt.y, TPM_BOTTOMALIGN.0 | TPM_RIGHTALIGN.0)
        };
        let Ok(menu) = CreatePopupMenu() else {
            return 0;
        };
        if kind == PopupMenu::SuggestionOnly {
            let apply_label = utf16_z(badge_apply(language));
            let dismiss_label = utf16_z(badge_dismiss(language));
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
        } else {
            append_full_menu(
                menu,
                paused,
                thumbnail_enabled,
                suggestion,
                language,
                language_config,
            );
        }

        let _ = SetForegroundWindow(hwnd);
        let flags = TPM_RETURNCMD.0 | TPM_NONOTIFY.0 | TPM_RIGHTBUTTON.0 | align;
        let cmd = TrackPopupMenuEx(menu, flags, x, y, hwnd, None);
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
                tip_paused(self.language)
            } else if self.suggest_pending {
                tip_suggest(self.language)
            } else {
                tip_active()
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupMenu {
    Full,
    SuggestionOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PopupAnchor {
    pub x: i32,
    pub y: i32,
    pub open_downward: bool,
}

unsafe fn append_full_menu(
    menu: windows::Win32::UI::WindowsAndMessaging::HMENU,
    paused: bool,
    thumbnail_enabled: bool,
    suggestion: Option<&[SuggestionDiff]>,
    language: Language,
    language_config: LanguageConfig,
) {
    if let Some(diffs) = suggestion {
        let diff = i18n::format_diffs(language, diffs);
        let apply_label = utf16_z(&i18n::menu_apply_with_diff(language, &diff));
        let dismiss_label = utf16_z(menu_dismiss_suggestion(language));
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
    append_labeled(menu, MF_STRING, IDM_CALIBRATE, menu_calibrate(language));
    let startup_flags = if startup::is_startup_enabled() {
        MF_STRING | MF_CHECKED
    } else {
        MF_STRING | MF_UNCHECKED
    };
    append_labeled(menu, startup_flags, IDM_STARTUP, menu_startup(language));
    let thumbnail_flags = if thumbnail_enabled {
        MF_STRING | MF_CHECKED
    } else {
        MF_STRING | MF_UNCHECKED
    };
    append_labeled(
        menu,
        thumbnail_flags,
        IDM_TOGGLE_THUMBNAIL,
        menu_show_background(language),
    );
    let pause_flags = if paused {
        MF_STRING | MF_CHECKED
    } else {
        MF_STRING | MF_UNCHECKED
    };
    append_labeled(menu, pause_flags, IDM_PAUSE, menu_pause(language));
    append_language_submenu(menu, language_config);
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, w!(""));
    append_labeled(menu, MF_STRING, IDM_EXIT, menu_exit(language));
}

unsafe fn append_language_submenu(
    menu: windows::Win32::UI::WindowsAndMessaging::HMENU,
    current: LanguageConfig,
) {
    let Ok(submenu) = CreatePopupMenu() else {
        return;
    };
    append_labeled(
        submenu,
        language_item_flags(current, LanguageConfig::Auto),
        IDM_LANG_AUTO,
        menu_language_auto(),
    );
    append_labeled(
        submenu,
        language_item_flags(current, LanguageConfig::En),
        IDM_LANG_EN,
        menu_language_en(),
    );
    append_labeled(
        submenu,
        language_item_flags(current, LanguageConfig::Ja),
        IDM_LANG_JA,
        menu_language_ja(),
    );
    let title = utf16_z(menu_language());
    let _ = AppendMenuW(
        menu,
        MF_STRING | MF_POPUP,
        submenu.0 as usize,
        PCWSTR(title.as_ptr()),
    );
}

fn language_item_flags(
    current: LanguageConfig,
    item: LanguageConfig,
) -> windows::Win32::UI::WindowsAndMessaging::MENU_ITEM_FLAGS {
    if current == item {
        MF_STRING | MF_CHECKED
    } else {
        MF_STRING | MF_UNCHECKED
    }
}

unsafe fn append_labeled(
    menu: windows::Win32::UI::WindowsAndMessaging::HMENU,
    flags: windows::Win32::UI::WindowsAndMessaging::MENU_ITEM_FLAGS,
    id: usize,
    label: &str,
) {
    let wide = utf16_z(label);
    let _ = AppendMenuW(menu, flags, id, PCWSTR(wide.as_ptr()));
}

fn copy_tip(dst: &mut [u16; 128], text: &str) {
    dst.fill(0);
    let encoded: Vec<u16> = text.encode_utf16().take(127).collect();
    dst[..encoded.len()].copy_from_slice(&encoded);
}

fn utf16_z(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn current_small_icon_px() -> u32 {
    nearest_tray_edge(unsafe { GetSystemMetrics(SM_CXSMICON) })
}

unsafe fn create_state_icons(instance: HINSTANCE, size: u32) -> io::Result<(HICON, HICON, HICON)> {
    let active = create_glyph_icon(instance, size, TrayGlyph::Active)?;
    let paused = match create_glyph_icon(instance, size, TrayGlyph::Paused) {
        Ok(icon) => icon,
        Err(err) => {
            let _ = DestroyIcon(active);
            return Err(err);
        }
    };
    let suggest = match create_glyph_icon(instance, size, TrayGlyph::Suggest) {
        Ok(icon) => icon,
        Err(err) => {
            let _ = DestroyIcon(active);
            let _ = DestroyIcon(paused);
            return Err(err);
        }
    };
    Ok((active, paused, suggest))
}

unsafe fn create_glyph_icon(instance: HINSTANCE, size: u32, glyph: TrayGlyph) -> io::Result<HICON> {
    let image = signature_image(size).map_err(|err| io::Error::other(err.to_string()))?;
    let bgra = tray_bgra(&image.bgra, size, glyph);
    let (xor, and_mask) = xor_and_masks(&bgra, size);
    CreateIcon(
        instance,
        size as i32,
        size as i32,
        1,
        32,
        and_mask.as_ptr(),
        xor.as_ptr(),
    )
    .map_err(|err| io::Error::other(format!("CreateIcon: {err}")))
}
