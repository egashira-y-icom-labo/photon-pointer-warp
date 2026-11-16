//! 波動照準オーバーレイ（GDI レイヤード / 二層描画）

use std::io;
use std::sync::atomic::{AtomicIsize, Ordering};

use crate::domain::wave::{ray_circle_intersection, WavePhaseKind, WaveSnapshot};
use crate::platform::layout::VirtualScreen;
use crate::platform::window::load_arrow_cursor;
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
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

// コア色（高輝度シアン／エメラルド）
const CORE_R: u8 = 80;
const CORE_G: u8 = 245;
const CORE_B: u8 = 230;
const CORE_A: u8 = 245;
// アウトライン（暗色）
const OUT_R: u8 = 10;
const OUT_G: u8 = 20;
const OUT_B: u8 = 28;
const OUT_A: u8 = 230;

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

        let ox = (snap.origin_x as i32 - self.vs.x).clamp(0, self.vs.width - 1);
        let oy = (snap.origin_y as i32 - self.vs.y).clamp(0, self.vs.height - 1);

        match snap.phase {
            WavePhaseKind::ImpactFlash => {
                self.draw_impact_flash(snap);
            }
            WavePhaseKind::Converging => {
                self.draw_converging(snap, ox, oy);
            }
            WavePhaseKind::CancelRewind => {
                self.draw_wave_body(snap, ox, oy, snap.radius as i32);
            }
            _ => {
                self.draw_wave_body(snap, ox, oy, snap.radius.max(0.0) as i32);
            }
        }

        self.show_bits()?;
        self.active = true;
        Ok(())
    }

    unsafe fn draw_wave_body(&mut self, snap: &WaveSnapshot, ox: i32, oy: i32, radius: i32) {
        let ring_core = self.sx(4.5);
        let ray_core = self.sx(3.0);
        let outline = self.sx(2.5);

        if radius > 0 {
            stroke_ring(
                self.pixels,
                self.vs.width,
                self.vs.height,
                ox,
                oy,
                radius,
                ring_core + outline,
                OUT_R,
                OUT_G,
                OUT_B,
                OUT_A,
            );
            stroke_ring(
                self.pixels,
                self.vs.width,
                self.vs.height,
                ox,
                oy,
                radius,
                ring_core,
                CORE_R,
                CORE_G,
                CORE_B,
                CORE_A,
            );
        }

        // 波源リング
        let origin_r = self.sx(7.0);
        stroke_ring(
            self.pixels,
            self.vs.width,
            self.vs.height,
            ox,
            oy,
            origin_r,
            self.sx(3.0) + outline / 2,
            OUT_R,
            OUT_G,
            OUT_B,
            OUT_A,
        );
        stroke_ring(
            self.pixels,
            self.vs.width,
            self.vs.height,
            ox,
            oy,
            origin_r,
            self.sx(3.0),
            CORE_R,
            CORE_G,
            CORE_B,
            CORE_A,
        );
        fill_disk(
            self.pixels,
            self.vs.width,
            self.vs.height,
            ox,
            oy,
            self.sx(3.0),
            255,
            255,
            255,
            230,
        );

        // 仮想スティックガイド
        if snap.stick_x.abs() > 0.5 || snap.stick_y.abs() > 0.5 {
            let guide_scale = self.dpi_scale;
            let gx = ox + (snap.stick_x * guide_scale).round() as i32;
            let gy = oy + (snap.stick_y * guide_scale).round() as i32;
            fill_disk(
                self.pixels,
                self.vs.width,
                self.vs.height,
                gx,
                gy,
                self.sx(4.0) + 1,
                OUT_R,
                OUT_G,
                OUT_B,
                OUT_A,
            );
            fill_disk(
                self.pixels,
                self.vs.width,
                self.vs.height,
                gx,
                gy,
                self.sx(4.0),
                255,
                220,
                80,
                240,
            );
        }

        if snap.direction_locked && radius > 1 {
            let (ax, ay) = ray_circle_intersection(
                snap.origin_x,
                snap.origin_y,
                snap.radius,
                snap.angle_rad,
            );
            let ax = ax as i32 - self.vs.x;
            let ay = ay as i32 - self.vs.y;
            stroke_thick_line(
                self.pixels,
                self.vs.width,
                self.vs.height,
                ox,
                oy,
                ax,
                ay,
                ray_core + outline,
                OUT_R,
                OUT_G,
                OUT_B,
                OUT_A,
            );
            stroke_thick_line(
                self.pixels,
                self.vs.width,
                self.vs.height,
                ox,
                oy,
                ax,
                ay,
                ray_core,
                CORE_R,
                CORE_G,
                CORE_B,
                CORE_A,
            );
            draw_corner_brackets(
                self.pixels,
                self.vs.width,
                self.vs.height,
                ax,
                ay,
                self.sx(16.0),
                self.sx(2.0),
                outline,
            );
        } else if radius > 1 {
            // 未確定でも薄いガイド放射線（右向き初期）は出さず、円だけ
        }
    }

    unsafe fn draw_converging(&mut self, snap: &WaveSnapshot, ox: i32, oy: i32) {
        let t = snap.progress.clamp(0.0, 1.0);
        let ease = t * t * (3.0 - 2.0 * t);
        let tx = snap.target_x as i32 - self.vs.x;
        let ty = snap.target_y as i32 - self.vs.y;
        let cx = ox as f32 + (tx - ox) as f32 * ease;
        let cy = oy as f32 + (ty - oy) as f32 * ease;
        let radius = (snap.radius * (1.0 - ease * 0.85)).max(8.0) as i32;
        let mut mid = WaveSnapshot {
            origin_x: cx + self.vs.x as f32,
            origin_y: cy + self.vs.y as f32,
            radius: radius as f32,
            direction_locked: true,
            ..snap.clone()
        };
        // 収束中は照準をターゲットへ寄せる
        mid.angle_rad = (ty as f32 - cy).atan2(tx as f32 - cx);
        self.draw_wave_body(&mid, cx as i32, cy as i32, radius);
        draw_corner_brackets(
            self.pixels,
            self.vs.width,
            self.vs.height,
            tx,
            ty,
            self.sx(16.0),
            self.sx(2.5),
            self.sx(2.0),
        );
    }

    unsafe fn draw_impact_flash(&mut self, snap: &WaveSnapshot) {
        let cx = snap.target_x as i32 - self.vs.x;
        let cy = snap.target_y as i32 - self.vs.y;
        let t = snap.progress.clamp(0.0, 1.0);
        let pulse = 1.0 - (t - 0.5).abs() * 2.0;
        let pulse = pulse.clamp(0.0, 1.0);
        let r1 = self.sx(10.0 + 28.0 * t);
        let r2 = self.sx(18.0 + 40.0 * t);
        let a = (220.0 * (1.0 - t) * (0.4 + 0.6 * pulse)) as u8;
        stroke_ring(
            self.pixels,
            self.vs.width,
            self.vs.height,
            cx,
            cy,
            r2,
            self.sx(3.0),
            OUT_R,
            OUT_G,
            OUT_B,
            a.max(40),
        );
        stroke_ring(
            self.pixels,
            self.vs.width,
            self.vs.height,
            cx,
            cy,
            r1,
            self.sx(4.0),
            CORE_R,
            CORE_G,
            CORE_B,
            a.max(60),
        );
        draw_corner_brackets(
            self.pixels,
            self.vs.width,
            self.vs.height,
            cx,
            cy,
            self.sx(18.0),
            self.sx(3.0),
            self.sx(2.0),
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
    let premul = |s: u8, d: u16| -> u8 {
        let s = (s as u16 * src_a) / 255;
        ((s + d * (255 - src_a) / 255).min(255)) as u8
    };
    *pixels.offset(i) = premul(b, dst_b);
    *pixels.offset(i + 1) = premul(g, dst_g);
    *pixels.offset(i + 2) = premul(r, dst_r);
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

fn draw_corner_brackets(
    pixels: *mut u8,
    w: i32,
    h: i32,
    cx: i32,
    cy: i32,
    size: i32,
    core: i32,
    outline: i32,
) {
    let half = size / 2;
    let arm = size / 3;
    let corners = [
        (cx - half, cy - half, 1, 1),
        (cx + half, cy - half, -1, 1),
        (cx - half, cy + half, 1, -1),
        (cx + half, cy + half, -1, -1),
    ];
    for (x, y, sx, sy) in corners {
        stroke_thick_line(
            pixels, w, h, x, y, x + sx * arm, y, core + outline, OUT_R, OUT_G, OUT_B, OUT_A,
        );
        stroke_thick_line(
            pixels, w, h, x, y, x, y + sy * arm, core + outline, OUT_R, OUT_G, OUT_B, OUT_A,
        );
        stroke_thick_line(
            pixels, w, h, x, y, x + sx * arm, y, core, CORE_R, CORE_G, CORE_B, CORE_A,
        );
        stroke_thick_line(
            pixels, w, h, x, y, x, y + sy * arm, core, CORE_R, CORE_G, CORE_B, CORE_A,
        );
    }
}

#[allow(dead_code)]
fn _rect_ty(_: RECT) {}
