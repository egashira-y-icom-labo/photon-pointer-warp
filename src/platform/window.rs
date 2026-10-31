//! 全画面レイヤード HUD（UpdateLayeredWindow + GDI DIB）

use std::io;
use std::sync::atomic::{AtomicIsize, Ordering};

use crate::platform::layout::{minimap_panel_origin, VirtualScreen};
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DeleteDC, DeleteObject, SelectObject, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, DIB_RGB_COLORS,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, GetSystemMetrics, RegisterClassW, SetWindowPos, ShowWindow, UpdateLayeredWindow,
    CS_HREDRAW, CS_VREDRAW, HWND_TOPMOST, SM_CYVIRTUALSCREEN, SM_CXVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

const HUD_CLASS: windows::core::PCWSTR = w!("PhotonPointerWarpHud");

/// 3×3 セル一辺（160〜200px 要件の中央値）
pub const MINIMAP_CELL_PX: i32 = 180;
pub const MINIMAP_GRID: i32 = 3;
pub const MINIMAP_PANEL_PX: i32 = MINIMAP_CELL_PX * MINIMAP_GRID;

const PIN_OUTER_RADIUS: i32 = 22;
const PIN_INNER_RADIUS: i32 = 10;
const PIN_MID_RADIUS: i32 = 16;
/// 地縛霊マーカー上の左クリック判定（外円 + 余白）
pub const ORIGIN_HIT_RADIUS_PX: i32 = PIN_OUTER_RADIUS + 8;

static HUD_HWND: AtomicIsize = AtomicIsize::new(0);

pub fn note_hud_destroyed() {
    HUD_HWND.store(0, Ordering::Release);
}

pub struct HudOverlay {
    hwnd: HWND,
    virtual_screen: VirtualScreen,
    pin_screen: POINT,
    panel_screen: RECT,
    dib_dc: windows::Win32::Graphics::Gdi::HDC,
    dib_bitmap: windows::Win32::Graphics::Gdi::HBITMAP,
    pixels: *mut u8,
    pixel_len: usize,
    active: bool,
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

        let wc = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: instance,
            lpszClassName: HUD_CLASS,
            style: CS_HREDRAW | CS_VREDRAW,
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
            pin_screen: POINT::default(),
            panel_screen: RECT::default(),
            dib_dc: dib.dc,
            dib_bitmap: dib.bitmap,
            pixels: dib.pixels,
            pixel_len: dib.len,
            active: false,
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

    /// サークル発火時: 地縛霊記録 → 描画 → 表示 → SetCapture
    pub unsafe fn activate_at_cursor(&mut self) -> io::Result<()> {
        let mut pin = POINT::default();
        windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pin)
            .map_err(|e| io::Error::other(format!("GetCursorPos: {e}")))?;

        let (left, top) = minimap_panel_origin(
            pin.x,
            pin.y,
            MINIMAP_PANEL_PX,
            self.virtual_screen,
        );
        self.pin_screen = pin;
        self.panel_screen = RECT {
            left,
            top,
            right: left + MINIMAP_PANEL_PX,
            bottom: top + MINIMAP_PANEL_PX,
        };

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
        self.active = true;
        Ok(())
    }

    pub unsafe fn dismiss(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        let _ = ReleaseCapture();
        self.active = false;
        clear_pixels(self.pixels, self.pixel_len);
        self.present_transparent()?;
        let _ = ShowWindow(self.hwnd, SW_HIDE);
        Ok(())
    }

    pub fn origin_hit_test_client(&self, client_x: i32, client_y: i32) -> bool {
        let pin_x = self.pin_screen.x - self.virtual_screen.x;
        let pin_y = self.pin_screen.y - self.virtual_screen.y;
        let dx = client_x - pin_x;
        let dy = client_y - pin_y;
        dx * dx + dy * dy <= ORIGIN_HIT_RADIUS_PX * ORIGIN_HIT_RADIUS_PX
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
        );
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

    let cell = MINIMAP_CELL_PX;
    for i in 1..MINIMAP_GRID {
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
