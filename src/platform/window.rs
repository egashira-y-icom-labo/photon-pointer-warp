//! 全画面レイヤード HUD（UpdateLayeredWindow + GDI DIB）

use std::io;
use std::sync::atomic::{AtomicIsize, Ordering};

use crate::platform::layout::{
    is_pointer_out_of_bounds, minimap_anchor_layout, minimap_cell_at, point_within_radius,
    suggestion_hint_rect, PanelScreenRect, VirtualScreen, DISMISS_OUT_OF_BOUNDS_MARGIN_PX,
    MINIMAP_CELL_PX, MINIMAP_GRID, MINIMAP_PANEL_PX, ORIGIN_CANCEL_RADIUS_PX,
};
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateFontW, DeleteDC, DeleteObject, DrawTextW, SelectObject, SetBkMode,
    SetTextColor, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DEFAULT_CHARSET, DIB_RGB_COLORS,
    DT_CENTER, DT_END_ELLIPSIS, DT_NOPREFIX, DT_SINGLELINE, FW_NORMAL, HDC, TRANSPARENT,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, GetSystemMetrics, LoadCursorW, RegisterClassW, SetCursor, SetCursorPos,
    SetWindowPos, ShowWindow, UpdateLayeredWindow, CS_HREDRAW, CS_VREDRAW, HCURSOR, IDC_ARROW, HWND_TOPMOST,
    SM_CYVIRTUALSCREEN, SM_CXVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_HIDE,
    SW_SHOWNOACTIVATE, ULW_ALPHA, WNDCLASSW, WS_EX_LAYERED, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

const HUD_CLASS: windows::core::PCWSTR = w!("PhotonPointerWarpHud");

const PIN_OUTER_RADIUS: i32 = 11;
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
    active: bool,
    hover_cell: Option<(usize, usize)>,
    suggestion_hint: Option<String>,
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
            active: false,
            hover_cell: None,
            suggestion_hint: None,
        };

        overlay.present_transparent()?;
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

    pub fn hover_cell(&self) -> Option<(usize, usize)> {
        self.hover_cell
    }

    /// サークル発火時: アンカー一致でパネル展開（地縛霊 ≈ 物理カーソル）
    pub unsafe fn activate_at_cursor(
        &mut self,
        suggestion_hint: Option<&str>,
    ) -> io::Result<HudActivateDiag> {
        let mut origin = POINT::default();
        windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut origin)
            .map_err(|e| io::Error::other(format!("GetCursorPos: {e}")))?;

        self.gesture_origin_screen = origin;
        let layout = minimap_anchor_layout(origin.x, origin.y, self.virtual_screen);
        self.pin_screen = POINT {
            x: layout.pin_screen_x,
            y: layout.pin_screen_y,
        };
        self.panel_screen = RECT {
            left: layout.panel_left,
            top: layout.panel_top,
            right: layout.panel_left + MINIMAP_PANEL_PX,
            bottom: layout.panel_top + MINIMAP_PANEL_PX,
        };
        self.hover_cell = None;
        self.suggestion_hint = suggestion_hint.map(str::to_string);

        self.redraw()?;
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
        SetCursorPos(self.pin_screen.x, self.pin_screen.y)
            .map_err(|e| io::Error::other(format!("SetCursorPos snap: {e}")))?;
        self.update_hover_from_screen(self.pin_screen.x, self.pin_screen.y)?;
        self.active = true;
        Ok(self.activation_diagnostic())
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
        self.suggestion_hint = None;
        clear_pixels(self.pixels, self.pixel_len);
        self.present_transparent()?;
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
        if self.origin_hit_test_screen(screen_x, screen_y) {
            return None;
        }
        self.minimap_cell_at_screen(screen_x, screen_y)
    }

    pub fn update_hover_from_screen(&mut self, screen_x: i32, screen_y: i32) -> io::Result<()> {
        let next = self.hover_cell_for_screen(screen_x, screen_y);
        if next == self.hover_cell {
            return Ok(());
        }
        self.hover_cell = next;
        self.redraw()
    }

    fn redraw(&mut self) -> io::Result<()> {
        clear_pixels(self.pixels, self.pixel_len);
        draw_hud(
            self.pixels,
            self.virtual_screen.width,
            self.virtual_screen.height,
            self.virtual_screen,
            self.pin_screen,
            self.panel_screen,
            self.hover_cell,
        );
        if let Some(summary) = self.suggestion_hint.as_deref() {
            unsafe {
                draw_suggestion_hint(
                    self.dib_dc,
                    self.pixels,
                    self.virtual_screen,
                    self.panel_screen,
                    summary,
                );
            }
        }
        unsafe { self.present_transparent() }
    }

    unsafe fn present_transparent(&self) -> io::Result<()> {
        let size = windows::Win32::Foundation::SIZE {
            cx: self.virtual_screen.width,
            cy: self.virtual_screen.height,
        };
        let point_zero = POINT { x: 0, y: 0 };
        let blend = windows::Win32::Graphics::Gdi::BLENDFUNCTION {
            BlendOp: windows::Win32::Graphics::Gdi::AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: windows::Win32::Graphics::Gdi::AC_SRC_ALPHA as u8,
        };

        UpdateLayeredWindow(
            self.hwnd,
            None,
            Some(&POINT {
                x: self.virtual_screen.x,
                y: self.virtual_screen.y,
            }),
            Some(&size),
            self.dib_dc,
            Some(&point_zero),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        )
        .map_err(|e| io::Error::other(format!("UpdateLayeredWindow: {e}")))?;
        Ok(())
    }
}

impl Drop for HudOverlay {
    fn drop(&mut self) {
        unsafe {
            if self.active {
                let _ = ReleaseCapture();
            }
            let _ = DeleteObject(self.dib_bitmap);
            let _ = DeleteDC(self.dib_dc);
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

    let _old = SelectObject(mem_dc, bitmap);
    let len = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(4);
    Ok(DibSection {
        dc: mem_dc,
        bitmap,
        pixels: bits as *mut u8,
        len,
    })
}

fn clear_pixels(pixels: *mut u8, len: usize) {
    if pixels.is_null() || len == 0 {
        return;
    }
    unsafe {
        std::ptr::write_bytes(pixels, 0, len);
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

const HINT_TEXT_R: u8 = 110;
const HINT_TEXT_G: u8 = 195;
const HINT_TEXT_B: u8 = 225;
const HINT_TEXT_A: u8 = 230;

unsafe fn draw_suggestion_hint(
    dc: HDC,
    pixels: *mut u8,
    vs: VirtualScreen,
    panel_screen: RECT,
    summary: &str,
) {
    let hint = suggestion_hint_rect(
        PanelScreenRect {
            left: panel_screen.left,
            top: panel_screen.top,
            right: panel_screen.right,
            bottom: panel_screen.bottom,
        },
        vs,
    );
    let mut rect = RECT {
        left: hint.left - vs.x,
        top: hint.top - vs.y,
        right: hint.right - vs.x,
        bottom: hint.bottom - vs.y,
    };
    let repair = rect;

    let font = CreateFontW(
        -16,
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
    SetTextColor(
        dc,
        COLORREF((HINT_TEXT_B as u32) << 16 | (HINT_TEXT_G as u32) << 8 | HINT_TEXT_R as u32),
    );

    let text = format!("[ 微調整案: {summary} (トレイで適用) ]");
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    let _ = DrawTextW(
        dc,
        &mut wide,
        &mut rect,
        DT_CENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
    );

    let _ = SelectObject(dc, old_font);
    let _ = DeleteObject(font);
    repair_gdi_text_alpha(
        pixels,
        vs.width,
        vs.height,
        repair.left,
        repair.top,
        repair.right,
        repair.bottom,
    );
}

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
