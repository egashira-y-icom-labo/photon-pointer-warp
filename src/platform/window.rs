//! 全画面レイヤード HUD（UpdateLayeredWindowIndirect + GDI DIB）

use std::io;
use std::sync::atomic::{AtomicIsize, Ordering};

use crate::domain::config::{effective_hud_style, HudStyle};
use crate::domain::monitor_layout::{panel_limits, MinimapGeometry, Monitor, Rect};
use crate::platform::layout::{
    anchor_panel_within, grid_pin_offsets, is_pointer_out_of_bounds, minimap_cell_at,
    point_within_radius, PanelScreenRect, VirtualScreen, DISMISS_OUT_OF_BOUNDS_MARGIN_PX,
    MINIMAP_CELL_PX, MINIMAP_GRID, MINIMAP_PANEL_PX, ORIGIN_CANCEL_RADIUS_PX,
};
use crate::platform::monitors::dpi_at_screen;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateFontW, DeleteDC, DeleteObject, DrawTextW, SelectObject, SetBkMode,
    SetTextColor, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CENTER,
    DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, FW_NORMAL, HDC, HGDIOBJ, TRANSPARENT,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, GetSystemMetrics, LoadCursorW, RegisterClassW, SetCursor, SetCursorPos,
    SetWindowPos, ShowWindow, UpdateLayeredWindowIndirect, CS_HREDRAW, CS_VREDRAW, HCURSOR,
    HWND_TOPMOST, IDC_ARROW, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN, SWP_NOACTIVATE, SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA,
    UPDATELAYEREDWINDOWINFO, WNDCLASSW, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

const HUD_CLASS: windows::core::PCWSTR = w!("PhotonPointerWarpHud");

const PIN_OUTER_RADIUS: i32 = 11;
const DIRTY_PAD_PX: i32 = PIN_OUTER_RADIUS + 2;
const PANEL_LONG_PX: i32 = 280;
const PANEL_SHORT_PX: i32 = 90;
const PANEL_PAD_PX: i32 = 6;
const PANEL_LONG_MAX_PX: i32 = 560;
const BADGE_SIZE_PX: i32 = 20;
const BADGE_MARGIN_PX: i32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HudMode {
    Tracking,
    Anchored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MonitorHover {
    index: usize,
    panel_x: i32,
    panel_y: i32,
}

struct MapCache {
    w: i32,
    h: i32,
    pixels: Vec<u8>,
    generation: u64,
    highlight: Option<usize>,
}
const PIN_INNER_RADIUS: i32 = 4;
const PIN_MID_RADIUS: i32 = 7;

static HUD_HWND: AtomicIsize = AtomicIsize::new(0);

/// 標準矢印カーソルを読み込む（`RegisterClassW` の `hCursor` 等）。
pub unsafe fn load_arrow_cursor() -> HCURSOR {
    LoadCursorW(None, IDC_ARROW).unwrap_or_default()
}

/// HUD 上で砂時計にならないよう、明示的に矢印カーソルを設定する。
pub unsafe fn set_arrow_cursor() {
    let _ = SetCursor(load_arrow_cursor());
}

/// `WM_SETCURSOR` 用: 矢印を設定し、DefWindowProc へ渡さない。
pub unsafe fn handle_wm_setcursor() -> LRESULT {
    set_arrow_cursor();
    LRESULT(1)
}

pub fn note_hud_destroyed() {
    HUD_HWND.store(0, Ordering::Release);
}

/// `activate_at_cursor` 直後の座標診断（Phase A ログ用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HudActivateDiag {
    pub gesture_x: i32,
    pub gesture_y: i32,
    pub pin_x: i32,
    pub pin_y: i32,
    pub vs: VirtualScreen,
    pub panel_left: i32,
    pub panel_top: i32,
    pub panel_right: i32,
    pub panel_bottom: i32,
    pub dib_pin_x: i32,
    pub dib_pin_y: i32,
    pub dib_panel_left: i32,
    pub dib_panel_top: i32,
}

impl HudActivateDiag {
    pub fn format_line(&self, dpi: u32) -> String {
        format!(
            "[hud-diag] gesture=({}, {}) pin=({}, {}) vs=({}, {}, {}, {}) panel_screen=({}, {}, {}, {}) dib pin=({}, {}) panel=({}, {}) dpi={}",
            self.gesture_x,
            self.gesture_y,
            self.pin_x,
            self.pin_y,
            self.vs.x,
            self.vs.y,
            self.vs.width,
            self.vs.height,
            self.panel_left,
            self.panel_top,
            self.panel_right,
            self.panel_bottom,
            self.dib_pin_x,
            self.dib_pin_y,
            self.dib_panel_left,
            self.dib_panel_top,
            dpi,
        )
    }
}

pub struct HudOverlay {
    hwnd: HWND,
    virtual_screen: VirtualScreen,
    gesture_origin_screen: POINT,
    pin_screen: POINT,
    panel_screen: RECT,
    dib_dc: windows::Win32::Graphics::Gdi::HDC,
    dib_bitmap: windows::Win32::Graphics::Gdi::HBITMAP,
    pixels: *mut u8,
    pixel_len: usize,
    dib_prev: HGDIOBJ,
    active: bool,
    hover_cell: Option<(usize, usize)>,
    suggestion_hint: Option<String>,
    mode: HudMode,
    requested_style: HudStyle,
    map_style: HudStyle,
    monitors: Vec<Monitor>,
    geometry: Option<MinimapGeometry>,
    panel_w: i32,
    panel_h: i32,
    dpi: u32,
    map_generation: u64,
    monitor_hover: Option<MonitorHover>,
    map_cache: Option<MapCache>,
    /// 直前にクリアした DIB 座標の範囲。次フレームでここも消す。
    last_panel_rect: Option<RECT>,
    /// 提案バッジの画面座標。提案がある固定中だけ Some。
    badge_rect: Option<RECT>,
    badge_hot: bool,
}

// HWND / DIB はメッセージスレッドのみで使用する
unsafe impl Send for HudOverlay {}
unsafe impl Sync for HudOverlay {}

impl HudOverlay {
    pub unsafe fn create(
        instance: HINSTANCE,
        wnd_proc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
    ) -> io::Result<Self> {
        let vs = query_virtual_screen();
        let arrow = load_arrow_cursor();

        let wc = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: instance,
            lpszClassName: HUD_CLASS,
            style: CS_HREDRAW | CS_VREDRAW,
            hCursor: arrow,
            ..Default::default()
        };
        RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TOOLWINDOW,
            HUD_CLASS,
            w!("photon-pointer-warp HUD"),
            WS_POPUP,
            vs.x,
            vs.y,
            vs.width,
            vs.height,
            None,
            None,
            instance,
            None,
        )
        .map_err(|e| io::Error::other(format!("CreateWindowExW HUD: {e}")))?;

        HUD_HWND.store(hwnd.0 as isize, Ordering::Release);

        let dib = create_dib_section(vs.width, vs.height)?;

        let overlay = Self {
            hwnd,
            virtual_screen: vs,
            gesture_origin_screen: POINT::default(),
            pin_screen: POINT::default(),
            panel_screen: RECT::default(),
            dib_dc: dib.dc,
            dib_bitmap: dib.bitmap,
            pixels: dib.pixels,
            pixel_len: dib.len,
            dib_prev: dib.prev,
            active: false,
            hover_cell: None,
            suggestion_hint: None,
            mode: HudMode::Anchored,
            requested_style: HudStyle::Grid,
            map_style: HudStyle::Grid,
            monitors: Vec::new(),
            geometry: None,
            panel_w: MINIMAP_PANEL_PX,
            panel_h: MINIMAP_PANEL_PX,
            dpi: 96,
            map_generation: 0,
            monitor_hover: None,
            map_cache: None,
            last_panel_rect: None,
            badge_rect: None,
            badge_hot: false,
        };

        overlay.present(None)?;
        let _ = ShowWindow(hwnd, SW_HIDE);
        Ok(overlay)
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub fn virtual_screen(&self) -> VirtualScreen {
        self.virtual_screen
    }

    /// 仮想画面の位置か大きさが変わったあと、DIB とレイヤード窓を作り直す。
    /// 表示中ならキャプチャを外して隠す。呼び出し側が HUD フェーズを Idle にする。
    pub unsafe fn rebuild_display(&mut self) -> io::Result<()> {
        let vs = query_virtual_screen();
        let dib = create_dib_section(vs.width, vs.height)?;
        self.destroy_dib();
        self.dib_dc = dib.dc;
        self.dib_bitmap = dib.bitmap;
        self.dib_prev = dib.prev;
        self.pixels = dib.pixels;
        self.pixel_len = dib.len;
        self.virtual_screen = vs;
        self.last_panel_rect = None;
        self.badge_rect = None;
        self.badge_hot = false;
        self.hover_cell = None;
        self.monitor_hover = None;
        self.suggestion_hint = None;
        self.gesture_origin_screen = POINT::default();
        self.pin_screen = POINT::default();
        self.panel_screen = RECT::default();
        self.mode = HudMode::Anchored;
        if self.active {
            let _ = ReleaseCapture();
            self.active = false;
        }
        let _ = SetWindowPos(
            self.hwnd,
            HWND_TOPMOST,
            vs.x,
            vs.y,
            vs.width,
            vs.height,
            SWP_NOACTIVATE,
        );
        clear_pixels(self.pixels, self.pixel_len);
        self.present(None)?;
        let _ = ShowWindow(self.hwnd, SW_HIDE);
        Ok(())
    }

    unsafe fn destroy_dib(&mut self) {
        if !self.dib_prev.0.is_null() {
            let _ = SelectObject(self.dib_dc, self.dib_prev);
            self.dib_prev = HGDIOBJ(std::ptr::null_mut());
        }
        let _ = DeleteObject(self.dib_bitmap);
        let _ = DeleteDC(self.dib_dc);
        self.pixels = std::ptr::null_mut();
        self.pixel_len = 0;
    }

    /// 設定とモニター列から、3×3 かモニター配置かを決めてパネル寸法を更新する。
    pub fn prepare_map(&mut self, style: HudStyle, monitors: &[Monitor]) {
        let (x, y) = cursor_screen_pos();
        let dpi = dpi_at_screen(x, y);
        self.recompute_map(style, monitors, dpi);
    }

    fn note_cursor_dpi(&mut self) {
        let (x, y) = cursor_screen_pos();
        let dpi = dpi_at_screen(x, y);
        if dpi == self.dpi {
            return;
        }
        let monitors = self.monitors.clone();
        let style = self.requested_style;
        self.recompute_map(style, &monitors, dpi);
    }

    fn recompute_map(&mut self, style: HudStyle, monitors: &[Monitor], dpi: u32) {
        let effective = effective_hud_style(style, monitors.len());
        let ready = self.dpi == dpi
            && self.requested_style == style
            && self.monitors == monitors
            && self.map_style == effective
            && match effective {
                HudStyle::Grid => self.geometry.is_none(),
                HudStyle::Monitors => self
                    .geometry
                    .as_ref()
                    .is_some_and(|geo| !geo.blocks.is_empty()),
            };
        if ready {
            return;
        }
        self.requested_style = style;
        self.dpi = dpi.max(96);
        self.monitors = monitors.to_vec();
        self.map_generation = self.map_generation.wrapping_add(1);
        self.monitor_hover = None;
        if effective == HudStyle::Grid {
            self.map_style = HudStyle::Grid;
            self.geometry = None;
            self.panel_w = MINIMAP_PANEL_PX;
            self.panel_h = MINIMAP_PANEL_PX;
            return;
        }
        let long_max =
            (PANEL_LONG_PX * self.dpi as i32 / 96).clamp(PANEL_LONG_PX, PANEL_LONG_MAX_PX);
        let short_min = (PANEL_SHORT_PX * self.dpi as i32 / 96).clamp(PANEL_SHORT_PX, long_max);
        let (max_w, max_h) = panel_limits(&self.monitors, long_max, short_min);
        let geometry = MinimapGeometry::build(&self.monitors, max_w, max_h, PANEL_PAD_PX);
        if geometry.blocks.is_empty() {
            self.map_style = HudStyle::Grid;
            self.geometry = None;
            self.panel_w = MINIMAP_PANEL_PX;
            self.panel_h = MINIMAP_PANEL_PX;
            return;
        }
        self.panel_w = geometry.panel_w;
        self.panel_h = geometry.panel_h;
        self.geometry = Some(geometry);
        self.map_style = HudStyle::Monitors;
    }

    pub fn uses_monitor_map(&self) -> bool {
        self.map_style == HudStyle::Monitors && self.geometry.is_some()
    }

    /// モニターブロック内のクリックを、その画面上の着地点へ解決する。
    pub fn monitor_target_at_screen(
        &self,
        screen_x: i32,
        screen_y: i32,
    ) -> Option<(usize, (i32, i32))> {
        if !self.uses_monitor_map()
            || self.badge_contains(screen_x, screen_y)
            || self.origin_hit_test_screen(screen_x, screen_y)
        {
            return None;
        }
        let local = self.panel_local(screen_x, screen_y)?;
        self.geometry.as_ref()?.hit(local)
    }

    pub fn screen_in_panel(&self, screen_x: i32, screen_y: i32) -> bool {
        self.panel_local(screen_x, screen_y).is_some()
    }

    pub fn mode(&self) -> HudMode {
        self.mode
    }

    /// サークル発火時: アンカー一致でパネル展開（地縛霊 ≈ 物理カーソル）
    pub unsafe fn activate_at_cursor(
        &mut self,
        suggestion_hint: Option<&str>,
    ) -> io::Result<HudActivateDiag> {
        let mut origin = POINT::default();
        windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut origin)
            .map_err(|e| io::Error::other(format!("GetCursorPos: {e}")))?;

        self.mode = HudMode::Anchored;
        self.hover_cell = None;
        self.monitor_hover = None;
        self.suggestion_hint = suggestion_hint.map(str::to_string);
        self.place_at(origin);
        self.redraw()?;
        self.show_topmost()?;
        SetCursorPos(self.pin_screen.x, self.pin_screen.y)
            .map_err(|e| io::Error::other(format!("SetCursorPos snap: {e}")))?;
        self.update_hover_from_screen(self.pin_screen.x, self.pin_screen.y)?;
        self.active = true;
        Ok(self.activation_diagnostic())
    }

    /// シェイク追従の開始。カーソルは動かさず、ヒントとホバーは描かない。
    pub unsafe fn activate_tracking(&mut self, cursor_pos: POINT) -> io::Result<()> {
        self.mode = HudMode::Tracking;
        self.hover_cell = None;
        self.monitor_hover = None;
        self.suggestion_hint = None;
        self.badge_rect = None;
        self.badge_hot = false;
        self.place_at(cursor_pos);
        self.redraw()?;
        self.show_topmost()?;
        self.active = true;
        Ok(())
    }

    /// 追従中にパネルをカーソルへ移す。位置が同じなら描かない。
    pub unsafe fn move_to(&mut self, cursor_pos: POINT) -> io::Result<()> {
        if !self.active || self.mode != HudMode::Tracking {
            return Ok(());
        }
        self.note_cursor_dpi();
        let layout = self.layout_for(cursor_pos);
        if layout.panel_left == self.panel_screen.left
            && layout.panel_top == self.panel_screen.top
            && layout.pin_screen_x == self.pin_screen.x
            && layout.pin_screen_y == self.pin_screen.y
            && self.panel_screen.right - self.panel_screen.left == self.panel_w
            && self.panel_screen.bottom - self.panel_screen.top == self.panel_h
        {
            return Ok(());
        }
        self.place_at(cursor_pos);
        self.redraw()
    }

    /// 追従を終え、ホバーと提案ヒントを描いて固定する。
    pub unsafe fn anchor(&mut self, hint: Option<&str>) -> io::Result<()> {
        self.mode = HudMode::Anchored;
        self.suggestion_hint = hint.map(str::to_string);
        self.refresh_badge_rect();
        self.hover_cell = self.hover_cell_for_screen(self.pin_screen.x, self.pin_screen.y);
        self.redraw()
    }

    fn place_at(&mut self, cursor: POINT) {
        self.note_cursor_dpi();
        self.gesture_origin_screen = cursor;
        let layout = self.layout_for(cursor);
        self.pin_screen = POINT {
            x: layout.pin_screen_x,
            y: layout.pin_screen_y,
        };
        self.panel_screen = RECT {
            left: layout.panel_left,
            top: layout.panel_top,
            right: layout.panel_left + self.panel_w,
            bottom: layout.panel_top + self.panel_h,
        };
        self.refresh_badge_rect();
    }

    fn refresh_badge_rect(&mut self) {
        if self.mode != HudMode::Anchored || self.suggestion_hint.is_none() {
            self.badge_rect = None;
            self.badge_hot = false;
            return;
        }
        let left = self.panel_screen.right - BADGE_MARGIN_PX - BADGE_SIZE_PX;
        let top = self.panel_screen.top + BADGE_MARGIN_PX;
        let right = left + BADGE_SIZE_PX;
        let bottom = top + BADGE_SIZE_PX;
        if left <= self.panel_screen.left + BADGE_MARGIN_PX || bottom >= self.panel_screen.bottom {
            self.badge_rect = None;
            self.badge_hot = false;
            return;
        }
        self.badge_rect = Some(RECT {
            left,
            top,
            right,
            bottom,
        });
    }

    pub fn badge_contains(&self, screen_x: i32, screen_y: i32) -> bool {
        self.badge_rect.is_some_and(|rect| {
            screen_x >= rect.left
                && screen_x < rect.right
                && screen_y >= rect.top
                && screen_y < rect.bottom
        })
    }

    fn layout_for(&self, cursor: POINT) -> crate::platform::layout::MinimapAnchorLayout {
        let (pin_dx, pin_dy) = if self.uses_monitor_map() {
            self.geometry
                .as_ref()
                .map(|geo| geo.screen_to_panel((cursor.x, cursor.y)))
                .unwrap_or((0, 0))
        } else {
            grid_pin_offsets(cursor.x, cursor.y, self.virtual_screen)
        };
        let (left, top, width, height) = self.placement_bounds(cursor);
        anchor_panel_within(
            cursor.x,
            cursor.y,
            pin_dx,
            pin_dy,
            self.panel_w,
            self.panel_h,
            left,
            top,
            width,
            height,
        )
    }

    fn placement_bounds(&self, cursor: POINT) -> (i32, i32, i32, i32) {
        if let Some(monitor) = self
            .monitors
            .iter()
            .find(|monitor| monitor.rect.contains((cursor.x, cursor.y)))
        {
            return (
                monitor.rect.left,
                monitor.rect.top,
                monitor.rect.width(),
                monitor.rect.height(),
            );
        }
        let vs = self.virtual_screen;
        (vs.x, vs.y, vs.width, vs.height)
    }

    fn panel_local(&self, screen_x: i32, screen_y: i32) -> Option<(i32, i32)> {
        let x = screen_x - self.panel_screen.left;
        let y = screen_y - self.panel_screen.top;
        if x < 0 || y < 0 || x >= self.panel_w || y >= self.panel_h {
            None
        } else {
            Some((x, y))
        }
    }

    unsafe fn show_topmost(&self) -> io::Result<()> {
        let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        let _ = SetWindowPos(
            self.hwnd,
            HWND_TOPMOST,
            self.virtual_screen.x,
            self.virtual_screen.y,
            self.virtual_screen.width,
            self.virtual_screen.height,
            windows::Win32::UI::WindowsAndMessaging::SWP_NOACTIVATE,
        );
        let _ = SetCapture(self.hwnd);
        set_arrow_cursor();
        Ok(())
    }

    fn activation_diagnostic(&self) -> HudActivateDiag {
        let vs = self.virtual_screen;
        HudActivateDiag {
            gesture_x: self.gesture_origin_screen.x,
            gesture_y: self.gesture_origin_screen.y,
            pin_x: self.pin_screen.x,
            pin_y: self.pin_screen.y,
            vs,
            panel_left: self.panel_screen.left,
            panel_top: self.panel_screen.top,
            panel_right: self.panel_screen.right,
            panel_bottom: self.panel_screen.bottom,
            dib_pin_x: self.pin_screen.x - vs.x,
            dib_pin_y: self.pin_screen.y - vs.y,
            dib_panel_left: self.panel_screen.left - vs.x,
            dib_panel_top: self.panel_screen.top - vs.y,
        }
    }

    pub unsafe fn dismiss(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        let _ = ReleaseCapture();
        self.active = false;
        self.hover_cell = None;
        self.monitor_hover = None;
        self.suggestion_hint = None;
        self.badge_rect = None;
        self.badge_hot = false;
        self.mode = HudMode::Anchored;
        clear_pixels(self.pixels, self.pixel_len);
        self.last_panel_rect = None;
        self.present(None)?;
        let _ = ShowWindow(self.hwnd, SW_HIDE);
        Ok(())
    }

    pub fn origin_hit_test_screen(&self, screen_x: i32, screen_y: i32) -> bool {
        point_within_radius(
            screen_x,
            screen_y,
            self.pin_screen.x,
            self.pin_screen.y,
            ORIGIN_CANCEL_RADIUS_PX,
        )
    }

    pub fn origin_hit_test_client(&self, client_x: i32, client_y: i32) -> bool {
        let pin_x = self.pin_screen.x - self.virtual_screen.x;
        let pin_y = self.pin_screen.y - self.virtual_screen.y;
        point_within_radius(client_x, client_y, pin_x, pin_y, ORIGIN_CANCEL_RADIUS_PX)
    }

    pub fn minimap_cell_at_screen(&self, screen_x: i32, screen_y: i32) -> Option<(usize, usize)> {
        minimap_cell_at(
            screen_x,
            screen_y,
            self.panel_screen.left,
            self.panel_screen.top,
            MINIMAP_PANEL_PX,
        )
    }

    pub fn pointer_beyond_dismiss_margin(&self, screen_x: i32, screen_y: i32) -> bool {
        is_pointer_out_of_bounds(
            (screen_x, screen_y),
            PanelScreenRect {
                left: self.panel_screen.left,
                top: self.panel_screen.top,
                right: self.panel_screen.right,
                bottom: self.panel_screen.bottom,
            },
            DISMISS_OUT_OF_BOUNDS_MARGIN_PX,
        )
    }

    fn hover_cell_for_screen(&self, screen_x: i32, screen_y: i32) -> Option<(usize, usize)> {
        if self.badge_contains(screen_x, screen_y)
            || self.origin_hit_test_screen(screen_x, screen_y)
        {
            return None;
        }
        self.minimap_cell_at_screen(screen_x, screen_y)
    }

    pub fn update_hover_from_screen(&mut self, screen_x: i32, screen_y: i32) -> io::Result<()> {
        let on_badge = self.badge_contains(screen_x, screen_y);
        let next_monitor = if on_badge {
            None
        } else {
            self.monitor_hover_at(screen_x, screen_y)
        };
        let next_cell = if on_badge {
            None
        } else {
            self.hover_cell_for_screen(screen_x, screen_y)
        };
        let hover_changed = if self.uses_monitor_map() {
            next_monitor != self.monitor_hover
        } else {
            next_cell != self.hover_cell
        };
        if on_badge == self.badge_hot && !hover_changed {
            return Ok(());
        }
        self.badge_hot = on_badge;
        if self.uses_monitor_map() {
            self.monitor_hover = next_monitor;
        } else {
            self.hover_cell = next_cell;
        }
        self.redraw()
    }

    fn monitor_hover_at(&self, screen_x: i32, screen_y: i32) -> Option<MonitorHover> {
        if self.badge_contains(screen_x, screen_y)
            || self.origin_hit_test_screen(screen_x, screen_y)
        {
            return None;
        }
        let local = self.panel_local(screen_x, screen_y)?;
        let (index, _) = self.geometry.as_ref()?.hit(local)?;
        Some(MonitorHover {
            index,
            panel_x: local.0,
            panel_y: local.1,
        })
    }

    fn panel_bounds_dib(&self) -> RECT {
        let vs = self.virtual_screen;
        RECT {
            left: self.panel_screen.left - vs.x - DIRTY_PAD_PX,
            top: self.panel_screen.top - vs.y - DIRTY_PAD_PX,
            right: self.panel_screen.right - vs.x + DIRTY_PAD_PX,
            bottom: self.panel_screen.bottom - vs.y + DIRTY_PAD_PX,
        }
    }

    fn redraw(&mut self) -> io::Result<()> {
        let panel = self.panel_bounds_dib();
        let clear = match self.last_panel_rect {
            Some(prev) => union_rect(prev, panel),
            None => panel,
        };
        clear_rect(
            self.pixels,
            self.virtual_screen.width,
            self.virtual_screen.height,
            clear,
        );
        if self.uses_monitor_map() {
            self.compose_monitor_map()?;
        } else {
            draw_hud(
                self.pixels,
                self.virtual_screen.width,
                self.virtual_screen.height,
                self.virtual_screen,
                self.pin_screen,
                self.panel_screen,
                self.hover_cell,
            );
        }
        if let Some(rect) = self.badge_rect {
            draw_suggestion_badge(
                self.pixels,
                self.virtual_screen.width,
                self.virtual_screen.height,
                self.virtual_screen,
                rect,
                self.badge_hot,
            );
        }
        unsafe { self.present(Some(clear))? };
        self.last_panel_rect = Some(panel);
        Ok(())
    }

    unsafe fn present(&self, dirty: Option<RECT>) -> io::Result<()> {
        let dst = POINT {
            x: self.virtual_screen.x,
            y: self.virtual_screen.y,
        };
        let size = windows::Win32::Foundation::SIZE {
            cx: self.virtual_screen.width,
            cy: self.virtual_screen.height,
        };
        let src = POINT { x: 0, y: 0 };
        let blend = windows::Win32::Graphics::Gdi::BLENDFUNCTION {
            BlendOp: windows::Win32::Graphics::Gdi::AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: windows::Win32::Graphics::Gdi::AC_SRC_ALPHA as u8,
        };
        let dirty_ptr = dirty
            .as_ref()
            .map(|rect| rect as *const RECT)
            .unwrap_or(std::ptr::null());
        let info = UPDATELAYEREDWINDOWINFO {
            cbSize: std::mem::size_of::<UPDATELAYEREDWINDOWINFO>() as u32,
            hdcDst: HDC::default(),
            pptDst: &dst,
            psize: &size,
            hdcSrc: self.dib_dc,
            pptSrc: &src,
            crKey: COLORREF(0),
            pblend: &blend,
            dwFlags: ULW_ALPHA,
            prcDirty: dirty_ptr,
        };
        UpdateLayeredWindowIndirect(self.hwnd, &info)
            .ok()
            .map_err(|e| io::Error::other(format!("UpdateLayeredWindowIndirect: {e}")))?;
        Ok(())
    }
}

impl Drop for HudOverlay {
    fn drop(&mut self) {
        unsafe {
            if self.active {
                let _ = ReleaseCapture();
            }
            self.destroy_dib();
        }
    }
}

pub unsafe fn query_virtual_screen() -> VirtualScreen {
    VirtualScreen {
        x: GetSystemMetrics(SM_XVIRTUALSCREEN),
        y: GetSystemMetrics(SM_YVIRTUALSCREEN),
        width: GetSystemMetrics(SM_CXVIRTUALSCREEN),
        height: GetSystemMetrics(SM_CYVIRTUALSCREEN),
    }
}

struct DibSection {
    dc: windows::Win32::Graphics::Gdi::HDC,
    bitmap: windows::Win32::Graphics::Gdi::HBITMAP,
    prev: HGDIOBJ,
    pixels: *mut u8,
    len: usize,
}

unsafe fn create_dib_section(width: i32, height: i32) -> io::Result<DibSection> {
    if width <= 0 || height <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid DIB size",
        ));
    }
    let hdc_screen = windows::Win32::Graphics::Gdi::GetDC(None);
    let mem_dc = CreateCompatibleDC(hdc_screen);
    windows::Win32::Graphics::Gdi::ReleaseDC(None, hdc_screen);

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    let bitmap = windows::Win32::Graphics::Gdi::CreateDIBSection(
        mem_dc,
        &bmi,
        DIB_RGB_COLORS,
        &mut bits,
        None,
        0,
    )
    .map_err(|e| io::Error::other(format!("CreateDIBSection: {e}")))?;

    let prev = SelectObject(mem_dc, bitmap);
    let len = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(4);
    Ok(DibSection {
        dc: mem_dc,
        bitmap,
        prev,
        pixels: bits as *mut u8,
        len,
    })
}

unsafe fn discard_dib(dib: DibSection) {
    if !dib.prev.0.is_null() {
        let _ = SelectObject(dib.dc, dib.prev);
    }
    let _ = DeleteObject(dib.bitmap);
    let _ = DeleteDC(dib.dc);
}

fn union_rect(a: RECT, b: RECT) -> RECT {
    RECT {
        left: a.left.min(b.left),
        top: a.top.min(b.top),
        right: a.right.max(b.right),
        bottom: a.bottom.max(b.bottom),
    }
}

fn clear_rect(pixels: *mut u8, width: i32, height: i32, rect: RECT) {
    if pixels.is_null() || width <= 0 || height <= 0 {
        return;
    }
    let left = rect.left.max(0);
    let top = rect.top.max(0);
    let right = rect.right.min(width);
    let bottom = rect.bottom.min(height);
    if left >= right || top >= bottom {
        return;
    }
    let row_bytes = (right - left) as usize * 4;
    unsafe {
        for y in top..bottom {
            let idx = ((y * width + left) * 4) as isize;
            std::ptr::write_bytes(pixels.offset(idx), 0, row_bytes);
        }
    }
}

fn clear_pixels(pixels: *mut u8, len: usize) {
    if pixels.is_null() || len == 0 {
        return;
    }
    unsafe {
        std::ptr::write_bytes(pixels, 0, len);
    }
}

fn cursor_screen_pos() -> (i32, i32) {
    let mut point = POINT::default();
    if unsafe { windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut point) }.is_err() {
        return (0, 0);
    }
    (point.x, point.y)
}

impl HudOverlay {
    fn compose_monitor_map(&mut self) -> io::Result<()> {
        let highlight = if self.mode == HudMode::Anchored {
            self.monitor_hover.map(|hover| hover.index)
        } else {
            None
        };
        self.ensure_map_cache(highlight)?;
        let vs = self.virtual_screen;
        let dest_x = self.panel_screen.left - vs.x;
        let dest_y = self.panel_screen.top - vs.y;
        if let Some(cache) = self.map_cache.as_ref() {
            blit_premul(
                self.pixels,
                vs.width,
                vs.height,
                dest_x,
                dest_y,
                &cache.pixels,
                cache.w,
                cache.h,
            );
        }
        if self.mode == HudMode::Anchored && !self.badge_hot {
            if let Some(hover) = self.monitor_hover {
                draw_crosshair(
                    self.pixels,
                    vs.width,
                    vs.height,
                    dest_x + hover.panel_x,
                    dest_y + hover.panel_y,
                );
            }
        }
        draw_pin(
            self.pixels,
            vs.width,
            vs.height,
            self.pin_screen.x - vs.x,
            self.pin_screen.y - vs.y,
        );
        draw_pin_cross(
            self.pixels,
            vs.width,
            vs.height,
            self.pin_screen.x - vs.x,
            self.pin_screen.y - vs.y,
        );
        Ok(())
    }

    fn ensure_map_cache(&mut self, highlight: Option<usize>) -> io::Result<()> {
        if let Some(cache) = self.map_cache.as_ref() {
            if cache.w == self.panel_w
                && cache.h == self.panel_h
                && cache.generation == self.map_generation
                && cache.highlight == highlight
            {
                return Ok(());
            }
        }
        let Some(geometry) = self.geometry.clone() else {
            return Ok(());
        };
        let w = self.panel_w.max(1);
        let h = self.panel_h.max(1);
        let dib = unsafe { create_dib_section(w, h)? };
        paint_monitor_blocks(dib.pixels, w, h, &geometry.blocks, highlight);
        unsafe { paint_monitor_labels(dib.dc, dib.pixels, w, h, &geometry.blocks) };
        let mut pixels = vec![0u8; dib.len];
        unsafe {
            std::ptr::copy_nonoverlapping(
                dib.pixels,
                pixels.as_mut_ptr(),
                dib.len.min(pixels.len()),
            );
            discard_dib(dib);
        }
        self.map_cache = Some(MapCache {
            w,
            h,
            pixels,
            generation: self.map_generation,
            highlight,
        });
        Ok(())
    }
}

fn draw_hud(
    pixels: *mut u8,
    width: i32,
    height: i32,
    vs: VirtualScreen,
    pin_screen: POINT,
    panel_screen: RECT,
    hover_cell: Option<(usize, usize)>,
) {
    let pin_x = pin_screen.x - vs.x;
    let pin_y = pin_screen.y - vs.y;

    let panel_left = panel_screen.left - vs.x;
    let panel_top = panel_screen.top - vs.y;
    fill_rect_premul(
        pixels,
        width,
        height,
        panel_left,
        panel_top,
        MINIMAP_PANEL_PX,
        MINIMAP_PANEL_PX,
        20,
        22,
        28,
        200,
    );

    if let Some((col, row)) = hover_cell {
        let cell = MINIMAP_CELL_PX;
        fill_rect_premul(
            pixels,
            width,
            height,
            panel_left + col as i32 * cell,
            panel_top + row as i32 * cell,
            cell,
            cell,
            120,
            180,
            255,
            140,
        );
    }

    let cell = MINIMAP_CELL_PX;
    for i in 1..MINIMAP_GRID as i32 {
        let x = panel_left + i * cell;
        let y = panel_top + i * cell;
        fill_rect_premul(
            pixels,
            width,
            height,
            x,
            panel_top,
            1,
            MINIMAP_PANEL_PX,
            255,
            255,
            255,
            100,
        );
        fill_rect_premul(
            pixels,
            width,
            height,
            panel_left,
            y,
            MINIMAP_PANEL_PX,
            1,
            255,
            255,
            255,
            100,
        );
    }
    stroke_rect_premul(
        pixels,
        width,
        height,
        panel_left,
        panel_top,
        MINIMAP_PANEL_PX,
        MINIMAP_PANEL_PX,
        255,
        255,
        255,
        140,
    );

    fill_circle_premul(
        pixels,
        width,
        height,
        pin_x,
        pin_y,
        PIN_OUTER_RADIUS,
        80,
        180,
        255,
        90,
    );
    stroke_circle_premul(
        pixels,
        width,
        height,
        pin_x,
        pin_y,
        PIN_MID_RADIUS,
        120,
        200,
        255,
        160,
        2,
    );
    fill_circle_premul(
        pixels,
        width,
        height,
        pin_x,
        pin_y,
        PIN_INNER_RADIUS,
        200,
        230,
        255,
        200,
    );
}

fn paint_monitor_blocks(
    pixels: *mut u8,
    width: i32,
    height: i32,
    blocks: &[Rect],
    highlight: Option<usize>,
) {
    fill_rect_premul(pixels, width, height, 0, 0, width, height, 12, 14, 18, 176);
    for (index, block) in blocks.iter().enumerate() {
        let hot = highlight == Some(index);
        let (r, g, b, a) = if hot {
            (72, 132, 188, 220)
        } else {
            (34, 46, 62, 214)
        };
        fill_rect_premul(
            pixels,
            width,
            height,
            block.left,
            block.top,
            block.width(),
            block.height(),
            r,
            g,
            b,
            a,
        );
        let (br, bg, bb, ba) = if hot {
            (196, 232, 255, 240)
        } else {
            (118, 186, 226, 210)
        };
        stroke_rect_premul(
            pixels,
            width,
            height,
            block.left,
            block.top,
            block.width().max(1),
            block.height().max(1),
            br,
            bg,
            bb,
            ba,
        );
    }
}

unsafe fn paint_monitor_labels(dc: HDC, pixels: *mut u8, width: i32, height: i32, blocks: &[Rect]) {
    for (index, block) in blocks.iter().enumerate() {
        if block.width() < 12 || block.height() < 12 {
            continue;
        }
        let font_px = (block.height().min(block.width()) / 2).clamp(12, 28);
        let font = CreateFontW(
            -font_px,
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
        let old_font = SelectObject(dc, font);
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, COLORREF((255u32) << 16 | (244u32) << 8 | 236));
        let mut rect = RECT {
            left: block.left,
            top: block.top,
            right: block.right,
            bottom: block.bottom,
        };
        let repair = rect;
        let text = format!("{}", index + 1);
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        let _ = DrawTextW(
            dc,
            &mut wide,
            &mut rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        let _ = SelectObject(dc, old_font);
        let _ = DeleteObject(font);
        repair_gdi_text_alpha(
            pixels,
            width,
            height,
            repair.left,
            repair.top,
            repair.right,
            repair.bottom,
        );
    }
}

fn draw_pin(pixels: *mut u8, width: i32, height: i32, pin_x: i32, pin_y: i32) {
    fill_circle_premul(
        pixels,
        width,
        height,
        pin_x,
        pin_y,
        PIN_OUTER_RADIUS,
        80,
        180,
        255,
        90,
    );
    stroke_circle_premul(
        pixels,
        width,
        height,
        pin_x,
        pin_y,
        PIN_MID_RADIUS,
        120,
        200,
        255,
        160,
        2,
    );
    fill_circle_premul(
        pixels,
        width,
        height,
        pin_x,
        pin_y,
        PIN_INNER_RADIUS,
        200,
        230,
        255,
        200,
    );
}

fn draw_pin_cross(pixels: *mut u8, width: i32, height: i32, pin_x: i32, pin_y: i32) {
    fill_rect_premul(
        pixels,
        width,
        height,
        pin_x - 7,
        pin_y,
        15,
        1,
        210,
        240,
        255,
        230,
    );
    fill_rect_premul(
        pixels,
        width,
        height,
        pin_x,
        pin_y - 7,
        1,
        15,
        210,
        240,
        255,
        230,
    );
}

fn draw_crosshair(pixels: *mut u8, width: i32, height: i32, x: i32, y: i32) {
    fill_rect_premul(pixels, width, height, x - 5, y, 11, 1, 255, 214, 120, 240);
    fill_rect_premul(pixels, width, height, x, y - 5, 1, 11, 255, 214, 120, 240);
    fill_rect_premul(
        pixels,
        width,
        height,
        x - 1,
        y - 1,
        3,
        3,
        255,
        236,
        170,
        255,
    );
}

fn blit_premul(
    dst: *mut u8,
    dst_w: i32,
    dst_h: i32,
    dest_x: i32,
    dest_y: i32,
    src: &[u8],
    src_w: i32,
    src_h: i32,
) {
    if dst.is_null() || dst_w <= 0 || dst_h <= 0 || src_w <= 0 || src_h <= 0 {
        return;
    }
    for row in 0..src_h {
        let y = dest_y + row;
        if y < 0 || y >= dst_h {
            continue;
        }
        for col in 0..src_w {
            let x = dest_x + col;
            if x < 0 || x >= dst_w {
                continue;
            }
            let src_idx = ((row * src_w + col) * 4) as usize;
            if src_idx + 3 >= src.len() {
                continue;
            }
            let dst_idx = ((y * dst_w + x) * 4) as isize;
            unsafe {
                let p = dst.offset(dst_idx);
                *p = src[src_idx];
                *p.offset(1) = src[src_idx + 1];
                *p.offset(2) = src[src_idx + 2];
                *p.offset(3) = src[src_idx + 3];
            }
        }
    }
}

fn draw_suggestion_badge(
    pixels: *mut u8,
    width: i32,
    height: i32,
    vs: VirtualScreen,
    screen: RECT,
    hot: bool,
) {
    let x = screen.left - vs.x;
    let y = screen.top - vs.y;
    let (r, g, b, a) = if hot {
        (255, 236, 140, 245)
    } else {
        (232, 186, 64, 210)
    };
    let (sr, sg, sb, sa) = if hot {
        (255, 248, 210, 250)
    } else {
        (255, 220, 120, 230)
    };
    fill_circle_premul(pixels, width, height, x + 10, y + 8, 6, r, g, b, a);
    fill_circle_premul(pixels, width, height, x + 8, y + 6, 2, sr, sg, sb, sa);
    fill_rect_premul(pixels, width, height, x + 7, y + 13, 6, 2, r, g, b, a);
    fill_rect_premul(pixels, width, height, x + 6, y + 15, 8, 3, r, g, b, a);
}

const HINT_TEXT_A: u8 = 230;

fn repair_gdi_text_alpha(
    pixels: *mut u8,
    width: i32,
    height: i32,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
) {
    if pixels.is_null() {
        return;
    }
    let left = left.max(0);
    let top = top.max(0);
    let right = right.min(width);
    let bottom = bottom.min(height);
    for y in top..bottom {
        for x in left..right {
            let idx = ((y * width + x) * 4) as isize;
            unsafe {
                let p = pixels.offset(idx);
                let b = *p;
                let g = *p.offset(1);
                let r = *p.offset(2);
                let a = *p.offset(3);
                if a == 0 && (b | g | r) != 0 {
                    let aa = HINT_TEXT_A as u16;
                    *p = ((b as u16 * aa) / 255) as u8;
                    *p.offset(1) = ((g as u16 * aa) / 255) as u8;
                    *p.offset(2) = ((r as u16 * aa) / 255) as u8;
                    *p.offset(3) = HINT_TEXT_A;
                }
            }
        }
    }
}

fn set_pixel_premul(
    pixels: *mut u8,
    width: i32,
    height: i32,
    x: i32,
    y: i32,
    r: u8,
    g: u8,
    b: u8,
    a: u8,
) {
    if x < 0 || y < 0 || x >= width || y >= height || a == 0 {
        return;
    }
    let idx = ((y * width + x) * 4) as isize;
    unsafe {
        let p = pixels.offset(idx);
        let (pb, pg, pr, pa) = (
            ((b as u16 * a as u16) / 255) as u8,
            ((g as u16 * a as u16) / 255) as u8,
            ((r as u16 * a as u16) / 255) as u8,
            a,
        );
        *p = pb;
        *p.offset(1) = pg;
        *p.offset(2) = pr;
        *p.offset(3) = pa;
    }
}

fn fill_rect_premul(
    pixels: *mut u8,
    width: i32,
    height: i32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    r: u8,
    g: u8,
    b: u8,
    a: u8,
) {
    for dy in 0..h {
        for dx in 0..w {
            set_pixel_premul(pixels, width, height, x + dx, y + dy, r, g, b, a);
        }
    }
}

fn stroke_rect_premul(
    pixels: *mut u8,
    width: i32,
    height: i32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    r: u8,
    g: u8,
    b: u8,
    a: u8,
) {
    fill_rect_premul(pixels, width, height, x, y, w, 1, r, g, b, a);
    fill_rect_premul(pixels, width, height, x, y + h - 1, w, 1, r, g, b, a);
    fill_rect_premul(pixels, width, height, x, y, 1, h, r, g, b, a);
    fill_rect_premul(pixels, width, height, x + w - 1, y, 1, h, r, g, b, a);
}

fn fill_circle_premul(
    pixels: *mut u8,
    width: i32,
    height: i32,
    cx: i32,
    cy: i32,
    radius: i32,
    r: u8,
    g: u8,
    b: u8,
    a: u8,
) {
    let r2 = radius * radius;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            if dx * dx + dy * dy <= r2 {
                set_pixel_premul(pixels, width, height, cx + dx, cy + dy, r, g, b, a);
            }
        }
    }
}

fn stroke_circle_premul(
    pixels: *mut u8,
    width: i32,
    height: i32,
    cx: i32,
    cy: i32,
    radius: i32,
    r: u8,
    g: u8,
    b: u8,
    a: u8,
    thickness: i32,
) {
    let outer = radius + thickness / 2;
    let inner = (radius - thickness / 2).max(0);
    let outer2 = outer * outer;
    let inner2 = inner * inner;
    for dy in -outer..=outer {
        for dx in -outer..=outer {
            let d2 = dx * dx + dy * dy;
            if d2 <= outer2 && d2 >= inner2 {
                set_pixel_premul(pixels, width, height, cx + dx, cy + dy, r, g, b, a);
            }
        }
    }
}
