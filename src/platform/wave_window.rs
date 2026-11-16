//! 波動照準オーバーレイ（高コントラスト二層 GDI）

use std::io;
use std::sync::atomic::{AtomicIsize, Ordering};

use crate::domain::wave::{WavePhaseKind, WaveSnapshot};
use crate::platform::layout::VirtualScreen;
use crate::platform::window::load_arrow_cursor;
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HDC,
};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, RegisterClassW, SetWindowDisplayAffinity, SetWindowPos, ShowWindow,
    UpdateLayeredWindowIndirect, CS_HREDRAW, CS_VREDRAW, HWND_TOPMOST, SWP_NOACTIVATE, SW_HIDE,
    SW_SHOWNOACTIVATE, ULW_ALPHA, UPDATELAYEREDWINDOWINFO, WDA_EXCLUDEFROMCAPTURE, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

const WAVE_CLASS: windows::core::PCWSTR = w!("PhotonPointerWarpWave");
static WAVE_HWND: AtomicIsize = AtomicIsize::new(0);

// 濃い黒ベース（字幕の縁取り）
const BASE_R: u8 = 0;
const BASE_G: u8 = 0;
const BASE_B: u8 = 0;
const BASE_A_RING: u8 = 235;
const BASE_A_ARROW: u8 = 245;
// 白寄りの高輝度シアンコア
const CORE_RING_R: u8 = 120;
const CORE_RING_G: u8 = 230;
const CORE_RING_B: u8 = 235;
const CORE_RING_A: u8 = 230;
const CORE_ARROW_R: u8 = 210;
const CORE_ARROW_G: u8 = 255;
const CORE_ARROW_B: u8 = 255;
const CORE_ARROW_A: u8 = 255;
const WHITE_R: u8 = 255;
const WHITE_G: u8 = 255;
const WHITE_B: u8 = 255;

pub struct WaveOverlay {
    hwnd: HWND,
    vs: VirtualScreen,
    dib_dc: HDC,
    dib: windows::Win32::Graphics::Gdi::HBITMAP,
    pixels: *mut u8,
    active: bool,
    dpi_scale: f32,
}

unsafe impl Send for WaveOverlay {}

impl WaveOverlay {
    pub unsafe fn create(
        instance: windows::Win32::Foundation::HINSTANCE,
        wnd_proc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
    ) -> io::Result<Self> {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: instance,
            lpszClassName: WAVE_CLASS,
            hCursor: load_arrow_cursor(),
            style: CS_HREDRAW | CS_VREDRAW,
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);

        let vs = VirtualScreen {
            x: windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                windows::Win32::UI::WindowsAndMessaging::SM_XVIRTUALSCREEN,
            ),
            y: windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                windows::Win32::UI::WindowsAndMessaging::SM_YVIRTUALSCREEN,
            ),
            width: windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                windows::Win32::UI::WindowsAndMessaging::SM_CXVIRTUALSCREEN,
            ),
            height: windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                windows::Win32::UI::WindowsAndMessaging::SM_CYVIRTUALSCREEN,
            ),
        };

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
            WAVE_CLASS,
            w!("photon-pointer-warp-wave"),
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
        .map_err(|e| io::Error::other(format!("CreateWindowExW wave: {e}")))?;

        let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
        WAVE_HWND.store(hwnd.0 as isize, Ordering::Release);

        let screen_dc = GetDC(None);
        let dib_dc = CreateCompatibleDC(screen_dc);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: vs.width,
                biHeight: -vs.height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let dib = windows::Win32::Graphics::Gdi::CreateDIBSection(
            dib_dc,
            &bmi,
            DIB_RGB_COLORS,
            &mut bits,
            None,
            0,
        )
        .map_err(|e| io::Error::other(format!("CreateDIBSection wave: {e}")))?;
        let _ = SelectObject(dib_dc, dib);
        let _ = ReleaseDC(None, screen_dc);

        let dpi = GetDpiForSystem().max(96) as f32;
        Ok(Self {
            hwnd,
            vs,
            dib_dc,
            dib,
            pixels: bits as *mut u8,
            active: false,
            dpi_scale: dpi / 96.0,
        })
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    fn sx(&self, v: f32) -> i32 {
        (v * self.dpi_scale).round().max(1.0) as i32
    }

    pub unsafe fn present_snapshot(&mut self, snap: &WaveSnapshot) -> io::Result<()> {
        if matches!(snap.phase, WavePhaseKind::Idle | WavePhaseKind::Arming) {
            return self.hide();
        }

        let stride = (self.vs.width * 4) as usize;
        let total = stride * self.vs.height as usize;
        std::ptr::write_bytes(self.pixels, 0, total);

        match snap.phase {
            WavePhaseKind::ImpactFlash => self.draw_impact_flash(snap),
            WavePhaseKind::Converging => self.draw_converging(snap),
            _ => self.draw_wave_body(snap),
        }

        self.show_bits()?;
        self.active = true;
        Ok(())
    }

    unsafe fn draw_wave_body(&mut self, snap: &WaveSnapshot) {
        let ox = (snap.origin_x as i32 - self.vs.x).clamp(0, self.vs.width - 1);
        let oy = (snap.origin_y as i32 - self.vs.y).clamp(0, self.vs.height - 1);
        let radius = snap.radius.max(0.0) as i32;

        // プライマリ波: 黒ベース 7–8px + シアンコア 3px
        if radius > 0 {
            stroke_ring(
                self.pixels,
                self.vs.width,
                self.vs.height,
                ox,
                oy,
                radius,
                self.sx(7.5),
                BASE_R,
                BASE_G,
                BASE_B,
                BASE_A_RING,
            );
            stroke_ring(
                self.pixels,
                self.vs.width,
                self.vs.height,
                ox,
                oy,
                radius,
                self.sx(3.0),
                CORE_RING_R,
                CORE_RING_G,
                CORE_RING_B,
                CORE_RING_A,
            );
        }

        // 波源ドット
        fill_disk(
            self.pixels,
            self.vs.width,
            self.vs.height,
            ox,
            oy,
            self.sx(6.0),
            BASE_R,
            BASE_G,
            BASE_B,
            245,
        );
        fill_disk(
            self.pixels,
            self.vs.width,
            self.vs.height,
            ox,
            oy,
            self.sx(3.5),
            CORE_ARROW_R,
            CORE_ARROW_G,
            CORE_ARROW_B,
            255,
        );

        if snap.direction_locked && radius > 1 {
            let tx = snap.target_x as i32 - self.vs.x;
            let ty = snap.target_y as i32 - self.vs.y;
            // 方向矢印: 黒 8–9px + 高輝度コア 4px + 矢じり
            stroke_thick_line(
                self.pixels,
                self.vs.width,
                self.vs.height,
                ox,
                oy,
                tx,
                ty,
                self.sx(8.5),
                BASE_R,
                BASE_G,
                BASE_B,
                BASE_A_ARROW,
            );
            stroke_thick_line(
                self.pixels,
                self.vs.width,
                self.vs.height,
                ox,
                oy,
                tx,
                ty,
                self.sx(4.0),
                CORE_ARROW_R,
                CORE_ARROW_G,
                CORE_ARROW_B,
                CORE_ARROW_A,
            );
            draw_arrowhead(
                self.pixels,
                self.vs.width,
                self.vs.height,
                ox,
                oy,
                tx,
                ty,
                self.sx(14.0),
                self.sx(3.0),
            );
            draw_reticle(
                self.pixels,
                self.vs.width,
                self.vs.height,
                tx,
                ty,
                self.sx(18.0),
                self.sx(2.5),
            );
        }
    }

    unsafe fn draw_converging(&mut self, snap: &WaveSnapshot) {
        let t = snap.progress.clamp(0.0, 1.0);
        let ease = t * t * (3.0 - 2.0 * t);
        let ox = snap.origin_x as i32 - self.vs.x;
        let oy = snap.origin_y as i32 - self.vs.y;
        let tx = snap.target_x as i32 - self.vs.x;
        let ty = snap.target_y as i32 - self.vs.y;
        let cx = ox as f32 + (tx - ox) as f32 * ease;
        let cy = oy as f32 + (ty - oy) as f32 * ease;
        let radius = (snap.radius * (1.0 - ease * 0.85)).max(10.0);
        let mut mid = snap.clone();
        mid.origin_x = cx + self.vs.x as f32;
        mid.origin_y = cy + self.vs.y as f32;
        mid.radius = radius;
        mid.direction_locked = true;
        mid.target_x = snap.target_x;
        mid.target_y = snap.target_y;
        self.draw_wave_body(&mid);
        draw_reticle(
            self.pixels,
            self.vs.width,
            self.vs.height,
            tx,
            ty,
            self.sx(18.0),
            self.sx(3.0),
        );
    }

    unsafe fn draw_impact_flash(&mut self, snap: &WaveSnapshot) {
        let cx = snap.target_x as i32 - self.vs.x;
        let cy = snap.target_y as i32 - self.vs.y;
        let t = snap.progress.clamp(0.0, 1.0);
        let a = ((1.0 - t) * 240.0) as u8;
        let r1 = self.sx(12.0 + 32.0 * t);
        let r2 = self.sx(22.0 + 48.0 * t);
        stroke_ring(
            self.pixels,
            self.vs.width,
            self.vs.height,
            cx,
            cy,
            r2,
            self.sx(6.0),
            BASE_R,
            BASE_G,
            BASE_B,
            a.max(50),
        );
        stroke_ring(
            self.pixels,
            self.vs.width,
            self.vs.height,
            cx,
            cy,
            r1,
            self.sx(3.5),
            CORE_ARROW_R,
            CORE_ARROW_G,
            CORE_ARROW_B,
            a.max(70),
        );
        draw_reticle(
            self.pixels,
            self.vs.width,
            self.vs.height,
            cx,
            cy,
            self.sx(20.0),
            self.sx(3.0),
        );
    }

    pub unsafe fn hide(&mut self) -> io::Result<()> {
        if self.active {
            let stride = (self.vs.width * 4) as usize;
            let total = stride * self.vs.height as usize;
            std::ptr::write_bytes(self.pixels, 0, total);
            self.show_bits()?;
        }
        let _ = ShowWindow(self.hwnd, SW_HIDE);
        self.active = false;
        Ok(())
    }

    unsafe fn show_bits(&self) -> io::Result<()> {
        let _ = SetWindowPos(
            self.hwnd,
            HWND_TOPMOST,
            self.vs.x,
            self.vs.y,
            self.vs.width,
            self.vs.height,
            SWP_NOACTIVATE,
        );
        let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        let dst = windows::Win32::Foundation::POINT {
            x: self.vs.x,
            y: self.vs.y,
        };
        let size = windows::Win32::Foundation::SIZE {
            cx: self.vs.width,
            cy: self.vs.height,
        };
        let src = windows::Win32::Foundation::POINT { x: 0, y: 0 };
        let blend = windows::Win32::Graphics::Gdi::BLENDFUNCTION {
            BlendOp: windows::Win32::Graphics::Gdi::AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: windows::Win32::Graphics::Gdi::AC_SRC_ALPHA as u8,
        };
        let info = UPDATELAYEREDWINDOWINFO {
            cbSize: std::mem::size_of::<UPDATELAYEREDWINDOWINFO>() as u32,
            hdcDst: HDC::default(),
            pptDst: &dst,
            psize: &size,
            hdcSrc: self.dib_dc,
            pptSrc: &src,
            crKey: windows::Win32::Foundation::COLORREF(0),
            pblend: &blend,
            dwFlags: ULW_ALPHA,
            prcDirty: std::ptr::null(),
        };
        UpdateLayeredWindowIndirect(self.hwnd, &info)
            .ok()
            .map_err(|e| io::Error::other(format!("UpdateLayeredWindowIndirect wave: {e}")))?;
        Ok(())
    }
}

impl Drop for WaveOverlay {
    fn drop(&mut self) {
        unsafe {
            let _ = self.hide();
            let _ = DeleteObject(self.dib);
            let _ = DeleteDC(self.dib_dc);
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(self.hwnd);
            WAVE_HWND.store(0, Ordering::Release);
        }
    }
}

pub fn note_wave_destroyed() {
    WAVE_HWND.store(0, Ordering::Release);
}

unsafe fn put_px(pixels: *mut u8, w: i32, h: i32, x: i32, y: i32, b: u8, g: u8, r: u8, a: u8) {
    if x < 0 || y < 0 || x >= w || y >= h || a == 0 {
        return;
    }
    let i = ((y * w + x) * 4) as isize;
    let dst_b = *pixels.offset(i) as u16;
    let dst_g = *pixels.offset(i + 1) as u16;
    let dst_r = *pixels.offset(i + 2) as u16;
    let dst_a = *pixels.offset(i + 3) as u16;
    let src_a = a as u16;
    let out_a = src_a + dst_a * (255 - src_a) / 255;
    let blend = |s: u8, d: u16| -> u8 {
        let s = (s as u16 * src_a) / 255;
        ((s + d * (255 - src_a) / 255).min(255)) as u8
    };
    *pixels.offset(i) = blend(b, dst_b);
    *pixels.offset(i + 1) = blend(g, dst_g);
    *pixels.offset(i + 2) = blend(r, dst_r);
    *pixels.offset(i + 3) = out_a.min(255) as u8;
}

fn fill_disk(
    pixels: *mut u8,
    w: i32,
    h: i32,
    cx: i32,
    cy: i32,
    radius: i32,
    r: u8,
    g: u8,
    b: u8,
    a: u8,
) {
    let rr = radius * radius;
    for y in (cy - radius)..=(cy + radius) {
        for x in (cx - radius)..=(cx + radius) {
            let dx = x - cx;
            let dy = y - cy;
            if dx * dx + dy * dy <= rr {
                unsafe { put_px(pixels, w, h, x, y, b, g, r, a) };
            }
        }
    }
}

fn stroke_ring(
    pixels: *mut u8,
    w: i32,
    h: i32,
    cx: i32,
    cy: i32,
    radius: i32,
    thickness: i32,
    r: u8,
    g: u8,
    b: u8,
    a: u8,
) {
    let thickness = thickness.max(1);
    let outer = radius + thickness / 2;
    let inner = (radius - thickness / 2).max(0);
    let outer2 = outer * outer;
    let inner2 = inner * inner;
    for y in (cy - outer)..=(cy + outer) {
        for x in (cx - outer)..=(cx + outer) {
            let dx = x - cx;
            let dy = y - cy;
            let d2 = dx * dx + dy * dy;
            if d2 <= outer2 && d2 >= inner2 {
                unsafe { put_px(pixels, w, h, x, y, b, g, r, a) };
            }
        }
    }
}

fn stroke_thick_line(
    pixels: *mut u8,
    w: i32,
    h: i32,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    thickness: i32,
    r: u8,
    g: u8,
    b: u8,
    a: u8,
) {
    let thickness = thickness.max(1);
    let samples = ((x1 - x0).abs().max((y1 - y0).abs())).max(1);
    for i in 0..=samples {
        let t = i as f32 / samples as f32;
        let x = x0 as f32 + (x1 - x0) as f32 * t;
        let y = y0 as f32 + (y1 - y0) as f32 * t;
        fill_disk(
            pixels,
            w,
            h,
            x.round() as i32,
            y.round() as i32,
            thickness / 2,
            r,
            g,
            b,
            a,
        );
    }
}

fn draw_arrowhead(
    pixels: *mut u8,
    w: i32,
    h: i32,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    size: i32,
    core: i32,
) {
    let dx = (x1 - x0) as f32;
    let dy = (y1 - y0) as f32;
    let len = (dx * dx + dy * dy).sqrt().max(1.0);
    let ux = dx / len;
    let uy = dy / len;
    let px = -uy;
    let py = ux;
    let tip_x = x1 as f32;
    let tip_y = y1 as f32;
    let base_x = tip_x - ux * size as f32;
    let base_y = tip_y - uy * size as f32;
    let wing = size as f32 * 0.55;
    let l_x = (base_x + px * wing).round() as i32;
    let l_y = (base_y + py * wing).round() as i32;
    let r_x = (base_x - px * wing).round() as i32;
    let r_y = (base_y - py * wing).round() as i32;
    let tip_xi = tip_x.round() as i32;
    let tip_yi = tip_y.round() as i32;
    // 黒ベース
    stroke_thick_line(
        pixels, w, h, tip_xi, tip_yi, l_x, l_y, core + 3, BASE_R, BASE_G, BASE_B, BASE_A_ARROW,
    );
    stroke_thick_line(
        pixels, w, h, tip_xi, tip_yi, r_x, r_y, core + 3, BASE_R, BASE_G, BASE_B, BASE_A_ARROW,
    );
    stroke_thick_line(
        pixels, w, h, l_x, l_y, r_x, r_y, core + 2, BASE_R, BASE_G, BASE_B, BASE_A_ARROW,
    );
    // 高輝度コア
    stroke_thick_line(
        pixels,
        w,
        h,
        tip_xi,
        tip_yi,
        l_x,
        l_y,
        core,
        CORE_ARROW_R,
        CORE_ARROW_G,
        CORE_ARROW_B,
        CORE_ARROW_A,
    );
    stroke_thick_line(
        pixels,
        w,
        h,
        tip_xi,
        tip_yi,
        r_x,
        r_y,
        core,
        CORE_ARROW_R,
        CORE_ARROW_G,
        CORE_ARROW_B,
        CORE_ARROW_A,
    );
}

/// 黒 + 白 + シアンの三重コントラスト（四隅ブラケットと中心十字）。
fn draw_reticle(
    pixels: *mut u8,
    w: i32,
    h: i32,
    cx: i32,
    cy: i32,
    size: i32,
    core: i32,
) {
    let half = size / 2;
    let arm = size / 3;
    let cross = size / 4;
    let layers = [
        (core + 4, BASE_R, BASE_G, BASE_B, 245u8),
        (core + 2, WHITE_R, WHITE_G, WHITE_B, 255u8),
        (core, CORE_ARROW_R, CORE_ARROW_G, CORE_ARROW_B, 255u8),
    ];
    for (th, r, g, b, a) in layers {
        let corners = [
            (cx - half, cy - half, 1, 1),
            (cx + half, cy - half, -1, 1),
            (cx - half, cy + half, 1, -1),
            (cx + half, cy + half, -1, -1),
        ];
        for (x, y, sx, sy) in corners {
            stroke_thick_line(pixels, w, h, x, y, x + sx * arm, y, th, r, g, b, a);
            stroke_thick_line(pixels, w, h, x, y, x, y + sy * arm, th, r, g, b, a);
        }
        stroke_thick_line(
            pixels,
            w,
            h,
            cx - cross,
            cy,
            cx + cross,
            cy,
            th,
            r,
            g,
            b,
            a,
        );
        stroke_thick_line(
            pixels,
            w,
            h,
            cx,
            cy - cross,
            cx,
            cy + cross,
            th,
            r,
            g,
            b,
            a,
        );
    }
}
