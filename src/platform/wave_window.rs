//! 波動照準オーバーレイ（高コントラスト二層 GDI）

use std::io;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use crate::domain::wave::{WavePhaseKind, WaveSnapshot};
use crate::platform::layout::VirtualScreen;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateFontW, DeleteDC, DeleteObject, DrawTextW, GetDC, ReleaseDC,
    SelectObject, SetBkMode, SetTextColor, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DEFAULT_CHARSET,
    DIB_RGB_COLORS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_TOP, FW_BOLD, HDC, TRANSPARENT,
};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, GetWindowLongPtrW, RegisterClassW, SetCursor, SetWindowDisplayAffinity,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, UpdateLayeredWindowIndirect, CS_HREDRAW,
    CS_VREDRAW, GWL_EXSTYLE, HWND_TOPMOST, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA, UPDATELAYEREDWINDOWINFO,
    WDA_EXCLUDEFROMCAPTURE, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

const WAVE_CLASS: windows::core::PCWSTR = w!("PhotonPointerWarpWave");
static WAVE_HWND: AtomicIsize = AtomicIsize::new(0);
/// 波動中に `WM_SETCURSOR` で OS カーソルを消す。
static SUPPRESS_OS_CURSOR: AtomicBool = AtomicBool::new(false);

// 濃い黒ベース（字幕の縁取り）
const BASE_R: u8 = 0;
const BASE_G: u8 = 0;
const BASE_B: u8 = 0;
const BASE_A_RING: u8 = 235;
const BASE_A_ARROW: u8 = 245;
const BASE_A_GUIDE: u8 = 200;
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
    /// 現在 `WS_EX_TRANSPARENT` が付いているか。
    hit_test_transparent: bool,
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
            // WM_SETCURSOR で制御するためクラスカーソルは持たない
            hCursor: windows::Win32::UI::WindowsAndMessaging::HCURSOR::default(),
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
        SUPPRESS_OS_CURSOR.store(false, Ordering::Release);

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
            hit_test_transparent: true,
        })
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn is_hit_test_transparent(&self) -> bool {
        self.hit_test_transparent
    }

    /// マウス透過の ON/OFF。`WS_EX_NOACTIVATE` は常に維持する。
    pub unsafe fn set_hit_test_transparent(&mut self, transparent: bool) {
        if self.hit_test_transparent == transparent {
            return;
        }
        let style = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
        let new_style = if transparent {
            style | (WS_EX_TRANSPARENT.0 as isize)
        } else {
            style & !(WS_EX_TRANSPARENT.0 as isize)
        };
        let _ = SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, new_style);
        let _ = SetWindowPos(
            self.hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
        self.hit_test_transparent = transparent;
    }

    pub fn set_suppress_os_cursor(&self, suppress: bool) {
        SUPPRESS_OS_CURSOR.store(suppress, Ordering::Release);
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
            WavePhaseKind::Converging => {
                self.draw_converging(snap);
                self.draw_telemetry(snap);
            }
            WavePhaseKind::Expanding | WavePhaseKind::Frozen => {
                self.draw_wave_body(snap);
                self.draw_telemetry(snap);
            }
            _ => self.draw_wave_body(snap),
        }

        self.show_bits()?;
        self.active = true;
        Ok(())
    }

    unsafe fn draw_wave_body(&mut self, snap: &WaveSnapshot) {
        let ox = (snap.origin_x as i32 - self.vs.x).clamp(0, self.vs.width - 1);
        let oy = (snap.origin_y as i32 - self.vs.y).clamp(0, self.vs.height - 1);
        let tip_x = snap.tip_x as i32 - self.vs.x;
        let tip_y = snap.tip_y as i32 - self.vs.y;
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

        // 波源アンカー（控えめな点＋小十字）
        draw_origin_anchor(
            self.pixels,
            self.vs.width,
            self.vs.height,
            ox,
            oy,
            self.sx(4.5),
            self.sx(2.0),
        );

        // 1:1 照準ノード（黒＋シアン二重リング）
        draw_aim_node(
            self.pixels,
            self.vs.width,
            self.vs.height,
            tip_x,
            tip_y,
            self.sx(6.5),
            self.sx(2.5),
        );

        if snap.direction_locked && radius > 1 {
            let tx = snap.target_x as i32 - self.vs.x;
            let ty = snap.target_y as i32 - self.vs.y;

            // ① 波源 → 照準ノード（控えめガイド）
            stroke_thick_line(
                self.pixels,
                self.vs.width,
                self.vs.height,
                ox,
                oy,
                tip_x,
                tip_y,
                self.sx(5.0),
                BASE_R,
                BASE_G,
                BASE_B,
                BASE_A_GUIDE,
            );
            stroke_thick_line(
                self.pixels,
                self.vs.width,
                self.vs.height,
                ox,
                oy,
                tip_x,
                tip_y,
                self.sx(2.0),
                CORE_RING_R,
                CORE_RING_G,
                CORE_RING_B,
                CORE_RING_A,
            );

            // ② 照準ノード → 円周交点（主役レーザー）
            stroke_thick_line(
                self.pixels,
                self.vs.width,
                self.vs.height,
                tip_x,
                tip_y,
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
                tip_x,
                tip_y,
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
                tip_x,
                tip_y,
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
        // tip は手元ノードとして維持
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

    unsafe fn draw_telemetry(&mut self, snap: &WaveSnapshot) {
        let label = match snap.phase {
            WavePhaseKind::Expanding => "SCANNING...",
            WavePhaseKind::Frozen => "TARGET LOCKED",
            WavePhaseKind::Converging => "ENGAGING",
            _ => return,
        };
        let (ax, ay) = if snap.direction_locked {
            (snap.target_x, snap.target_y)
        } else {
            (snap.tip_x, snap.tip_y)
        };
        let dx = ax - snap.origin_x;
        let dy = ay - snap.origin_y;
        let rng = dx.hypot(dy);
        let azm = dy.atan2(dx).to_degrees();
        let lines = [
            label.to_string(),
            format!("RNG : {rng:06.1} PX"),
            format!("AZM : {azm:+06.1} DEG"),
        ];

        let anchor_x = ax as i32 - self.vs.x;
        let anchor_y = ay as i32 - self.vs.y;
        let pad = self.sx(6.0);
        let line_h = self.sx(14.0);
        let plate_w = self.sx(148.0);
        let plate_h = pad * 2 + line_h * 3;
        let ox_off = self.sx(24.0);
        let oy_off = -self.sx(18.0);
        let mut left = anchor_x + ox_off;
        let top = (anchor_y + oy_off).max(2);
        if left + plate_w > self.vs.width - 4 {
            left = anchor_x - ox_off - plate_w;
        }
        left = left.clamp(2, (self.vs.width - plate_w - 2).max(2));

        // 半透明黒プレート
        fill_rect_alpha(
            self.pixels,
            self.vs.width,
            self.vs.height,
            left,
            top,
            plate_w,
            plate_h,
            0,
            0,
            0,
            170,
        );
        // シアン枠
        stroke_rect_alpha(
            self.pixels,
            self.vs.width,
            self.vs.height,
            left,
            top,
            plate_w,
            plate_h,
            CORE_RING_R,
            CORE_RING_G,
            CORE_RING_B,
            200,
            1,
        );

        let font = CreateFontW(
            -self.sx(13.0),
            0,
            0,
            0,
            FW_BOLD.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            0,
            0,
            0,
            0,
            w!("Consolas"),
        );
        let old_font = SelectObject(self.dib_dc, font);
        SetBkMode(self.dib_dc, TRANSPARENT);
        for (i, line) in lines.iter().enumerate() {
            let mut rect = RECT {
                left: left + pad,
                top: top + pad + i as i32 * line_h,
                right: left + plate_w - pad,
                bottom: top + pad + (i as i32 + 1) * line_h,
            };
            let mut wide: Vec<u16> = line.encode_utf16().collect();
            // 影
            let mut shadow = RECT {
                left: rect.left + 1,
                top: rect.top + 1,
                right: rect.right + 1,
                bottom: rect.bottom + 1,
            };
            SetTextColor(self.dib_dc, COLORREF(0));
            let _ = DrawTextW(
                self.dib_dc,
                &mut wide,
                &mut shadow,
                DT_LEFT | DT_TOP | DT_SINGLELINE | DT_NOPREFIX,
            );
            // 本体（1行目は白寄り、他はシアン）
            let color = if i == 0 {
                COLORREF((255u32) << 16 | (255u32) << 8 | 255)
            } else {
                COLORREF((CORE_ARROW_B as u32) << 16 | (CORE_ARROW_G as u32) << 8 | CORE_ARROW_R as u32)
            };
            SetTextColor(self.dib_dc, color);
            let _ = DrawTextW(
                self.dib_dc,
                &mut wide,
                &mut rect,
                DT_LEFT | DT_TOP | DT_SINGLELINE | DT_NOPREFIX,
            );
        }
        let _ = SelectObject(self.dib_dc, old_font);
        let _ = DeleteObject(font);
        repair_gdi_text_alpha(
            self.pixels,
            self.vs.width,
            self.vs.height,
            left,
            top,
            left + plate_w,
            top + plate_h,
        );
    }

    unsafe fn draw_impact_flash(&mut self, snap: &WaveSnapshot) {
        let cx = snap.target_x as i32 - self.vs.x;
        let cy = snap.target_y as i32 - self.vs.y;
        let t = snap.progress.clamp(0.0, 1.0);
        let fade = 1.0 - t;
        let a = ((fade * 240.0) as u8).max(20);

        // 時間差で広がる 3 重衝撃波
        let rings = [
            (0.00_f32, 18.0_f32, 110.0_f32, 7.0_f32),
            (0.12, 10.0, 78.0, 5.0),
            (0.24, 4.0, 48.0, 3.5),
        ];
        for (delay, r0, r1, thick) in rings {
            let local = ((t - delay) / (1.0 - delay).max(0.01)).clamp(0.0, 1.0);
            if t < delay {
                continue;
            }
            let radius = self.sx(r0 + (r1 - r0) * local);
            let ra = ((1.0 - local) * fade * 230.0) as u8;
            stroke_ring(
                self.pixels,
                self.vs.width,
                self.vs.height,
                cx,
                cy,
                radius,
                self.sx(thick + 3.0),
                BASE_R,
                BASE_G,
                BASE_B,
                ra.max(30),
            );
            stroke_ring(
                self.pixels,
                self.vs.width,
                self.vs.height,
                cx,
                cy,
                radius,
                self.sx(thick),
                CORE_ARROW_R,
                CORE_ARROW_G,
                CORE_ARROW_B,
                ra.max(40),
            );
        }

        // 放射スパーク（10 本、決定論的）
        draw_impact_sparks(
            self.pixels,
            self.vs.width,
            self.vs.height,
            cx,
            cy,
            t,
            fade,
            self.dpi_scale,
        );

        // 射撃のみ：弾痕＋ガラスひび割れ
        if snap.is_shoot {
            draw_bullet_cracks(
                self.pixels,
                self.vs.width,
                self.vs.height,
                cx,
                cy,
                fade,
                self.dpi_scale,
            );
            let hole_a = ((fade * 255.0) as u8).max(40);
            fill_disk(
                self.pixels,
                self.vs.width,
                self.vs.height,
                cx,
                cy,
                self.sx(7.0),
                40,
                20,
                10,
                ((fade * 180.0) as u8).max(30),
            );
            fill_disk(
                self.pixels,
                self.vs.width,
                self.vs.height,
                cx,
                cy,
                self.sx(4.5),
                BASE_R,
                BASE_G,
                BASE_B,
                hole_a,
            );
        }

        let ret_a_scale = fade;
        if ret_a_scale > 0.08 {
            draw_reticle(
                self.pixels,
                self.vs.width,
                self.vs.height,
                cx,
                cy,
                self.sx(20.0),
                self.sx(3.0),
            );
            let _ = a;
        }
    }

    pub unsafe fn hide(&mut self) -> io::Result<()> {
        self.set_suppress_os_cursor(false);
        self.set_hit_test_transparent(true);
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
            SUPPRESS_OS_CURSOR.store(false, Ordering::Release);
        }
    }
}

pub fn note_wave_destroyed() {
    WAVE_HWND.store(0, Ordering::Release);
    SUPPRESS_OS_CURSOR.store(false, Ordering::Release);
}

/// `WM_SETCURSOR` 用: 波動中はカーソルを消し、それ以外は矢印。
pub unsafe fn handle_wave_wm_setcursor() -> LRESULT {
    if SUPPRESS_OS_CURSOR.load(Ordering::Acquire) {
        let _ = SetCursor(windows::Win32::UI::WindowsAndMessaging::HCURSOR::default());
        LRESULT(1)
    } else {
        crate::platform::window::handle_wm_setcursor()
    }
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

fn draw_origin_anchor(
    pixels: *mut u8,
    w: i32,
    h: i32,
    cx: i32,
    cy: i32,
    radius: i32,
    cross: i32,
) {
    fill_disk(
        pixels, w, h, cx, cy, radius + 1, BASE_R, BASE_G, BASE_B, 230,
    );
    fill_disk(
        pixels, w, h, cx, cy, radius, CORE_RING_R, CORE_RING_G, CORE_RING_B, 220,
    );
    stroke_thick_line(
        pixels,
        w,
        h,
        cx - cross,
        cy,
        cx + cross,
        cy,
        2,
        BASE_R,
        BASE_G,
        BASE_B,
        220,
    );
    stroke_thick_line(
        pixels,
        w,
        h,
        cx,
        cy - cross,
        cx,
        cy + cross,
        2,
        BASE_R,
        BASE_G,
        BASE_B,
        220,
    );
}

fn draw_aim_node(
    pixels: *mut u8,
    w: i32,
    h: i32,
    cx: i32,
    cy: i32,
    radius: i32,
    thickness: i32,
) {
    stroke_ring(
        pixels, w, h, cx, cy, radius, thickness + 3, BASE_R, BASE_G, BASE_B, 245,
    );
    stroke_ring(
        pixels,
        w,
        h,
        cx,
        cy,
        radius,
        thickness,
        CORE_ARROW_R,
        CORE_ARROW_G,
        CORE_ARROW_B,
        255,
    );
    fill_disk(
        pixels, w, h, cx, cy, 2, CORE_ARROW_R, CORE_ARROW_G, CORE_ARROW_B, 255,
    );
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
    stroke_thick_line(
        pixels, w, h, tip_xi, tip_yi, l_x, l_y, core + 3, BASE_R, BASE_G, BASE_B, BASE_A_ARROW,
    );
    stroke_thick_line(
        pixels, w, h, tip_xi, tip_yi, r_x, r_y, core + 3, BASE_R, BASE_G, BASE_B, BASE_A_ARROW,
    );
    stroke_thick_line(
        pixels, w, h, l_x, l_y, r_x, r_y, core + 2, BASE_R, BASE_G, BASE_B, BASE_A_ARROW,
    );
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

fn fill_rect_alpha(
    pixels: *mut u8,
    w: i32,
    h: i32,
    x: i32,
    y: i32,
    rw: i32,
    rh: i32,
    r: u8,
    g: u8,
    b: u8,
    a: u8,
) {
    for yy in y..(y + rh) {
        for xx in x..(x + rw) {
            unsafe { put_px(pixels, w, h, xx, yy, b, g, r, a) };
        }
    }
}

fn stroke_rect_alpha(
    pixels: *mut u8,
    w: i32,
    h: i32,
    x: i32,
    y: i32,
    rw: i32,
    rh: i32,
    r: u8,
    g: u8,
    b: u8,
    a: u8,
    thickness: i32,
) {
    let t = thickness.max(1);
    fill_rect_alpha(pixels, w, h, x, y, rw, t, r, g, b, a);
    fill_rect_alpha(pixels, w, h, x, y + rh - t, rw, t, r, g, b, a);
    fill_rect_alpha(pixels, w, h, x, y, t, rh, r, g, b, a);
    fill_rect_alpha(pixels, w, h, x + rw - t, y, t, rh, r, g, b, a);
}

fn repair_gdi_text_alpha(pixels: *mut u8, width: i32, height: i32, left: i32, top: i32, right: i32, bottom: i32) {
    if pixels.is_null() {
        return;
    }
    let left = left.max(0);
    let top = top.max(0);
    let right = right.min(width);
    let bottom = bottom.min(height);
    const TEXT_A: u8 = 235;
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
                    let aa = TEXT_A as u16;
                    *p = ((b as u16 * aa) / 255) as u8;
                    *p.offset(1) = ((g as u16 * aa) / 255) as u8;
                    *p.offset(2) = ((r as u16 * aa) / 255) as u8;
                    *p.offset(3) = TEXT_A;
                }
            }
        }
    }
}

fn hash2(x: i32, y: i32) -> u32 {
    let mut n = (x as u32)
        .wrapping_mul(374761393)
        .wrapping_add((y as u32).wrapping_mul(668265263))
        .wrapping_add(0x9e3779b9);
    n = (n ^ (n >> 13)).wrapping_mul(1274126177);
    n ^ (n >> 16)
}

fn next_u32(state: &mut u32) -> u32 {
    *state = state
        .wrapping_mul(1664525)
        .wrapping_add(1013904223);
    *state
}

fn next_f32(state: &mut u32) -> f32 {
    (next_u32(state) >> 8) as f32 / (1u32 << 24) as f32
}

fn draw_impact_sparks(
    pixels: *mut u8,
    w: i32,
    h: i32,
    cx: i32,
    cy: i32,
    t: f32,
    fade: f32,
    dpi: f32,
) {
    let mut rng = hash2(cx, cy ^ 0x5A17);
    let count = 10;
    for i in 0..count {
        let base_ang = (i as f32 / count as f32) * std::f32::consts::TAU
            + next_f32(&mut rng) * 0.35;
        let len0 = (28.0 + next_f32(&mut rng) * 42.0) * dpi;
        let delay = next_f32(&mut rng) * 0.15;
        let local = ((t - delay) / (1.0 - delay).max(0.01)).clamp(0.0, 1.0);
        if t < delay {
            continue;
        }
        // 減速: ease-out
        let ease = 1.0 - (1.0 - local) * (1.0 - local);
        let travel = len0 * ease;
        let spark_len = (len0 * 0.35 * (1.0 - local * 0.7)).max(4.0 * dpi);
        let ux = base_ang.cos();
        let uy = base_ang.sin();
        let x0 = cx as f32 + ux * (travel - spark_len).max(0.0);
        let y0 = cy as f32 + uy * (travel - spark_len).max(0.0);
        let x1 = cx as f32 + ux * travel;
        let y1 = cy as f32 + uy * travel;
        let a = ((1.0 - local) * fade * 255.0) as u8;
        stroke_thick_line(
            pixels,
            w,
            h,
            x0.round() as i32,
            y0.round() as i32,
            x1.round() as i32,
            y1.round() as i32,
            (2.0 * dpi).round().max(1.0) as i32 + 2,
            BASE_R,
            BASE_G,
            BASE_B,
            a.max(20),
        );
        stroke_thick_line(
            pixels,
            w,
            h,
            x0.round() as i32,
            y0.round() as i32,
            x1.round() as i32,
            y1.round() as i32,
            (2.0 * dpi).round().max(1.0) as i32,
            CORE_ARROW_R,
            CORE_ARROW_G,
            CORE_ARROW_B,
            a.max(30),
        );
    }
}

fn draw_bullet_cracks(
    pixels: *mut u8,
    w: i32,
    h: i32,
    cx: i32,
    cy: i32,
    fade: f32,
    dpi: f32,
) {
    let mut rng = hash2(cx.wrapping_mul(3), cy.wrapping_mul(7));
    let rays = 8 + (next_u32(&mut rng) % 3) as i32; // 8..10
    let a = ((fade * 230.0) as u8).max(25);
    let mut ends: Vec<(i32, i32, f32)> = Vec::with_capacity(rays as usize);

    for i in 0..rays {
        let ang = (i as f32 / rays as f32) * std::f32::consts::TAU
            + (next_f32(&mut rng) - 0.5) * 0.4;
        let len = (30.0 + next_f32(&mut rng) * 55.0) * dpi;
        let mut x = cx as f32;
        let mut y = cy as f32;
        let segs = 3 + (next_u32(&mut rng) % 3) as i32;
        let mut prev_x = x;
        let mut prev_y = y;
        for s in 1..=segs {
            let t = s as f32 / segs as f32;
            let jag = (next_f32(&mut rng) - 0.5) * 10.0 * dpi;
            let px = -ang.sin();
            let py = ang.cos();
            x = cx as f32 + ang.cos() * len * t + px * jag;
            y = cy as f32 + ang.sin() * len * t + py * jag;
            stroke_thick_line(
                pixels,
                w,
                h,
                prev_x.round() as i32,
                prev_y.round() as i32,
                x.round() as i32,
                y.round() as i32,
                (1.5 * dpi).round().max(1.0) as i32 + 1,
                BASE_R,
                BASE_G,
                BASE_B,
                a,
            );
            stroke_thick_line(
                pixels,
                w,
                h,
                prev_x.round() as i32,
                prev_y.round() as i32,
                x.round() as i32,
                y.round() as i32,
                (1.2 * dpi).round().max(1.0) as i32,
                220,
                245,
                255,
                a,
            );
            prev_x = x;
            prev_y = y;
        }
        ends.push((x.round() as i32, y.round() as i32, len));
    }

    // 同心円状の短い亀裂（放射線を繋ぐ）
    for ring in 0..3 {
        let frac = 0.28 + ring as f32 * 0.22;
        for i in 0..ends.len() {
            let (x0, y0, len0) = ends[i];
            let (x1, y1, len1) = ends[(i + 1) % ends.len()];
            let r0 = len0 * frac;
            let r1 = len1 * frac;
            let a0 = ((y0 - cy) as f32).atan2((x0 - cx) as f32);
            let a1 = ((y1 - cy) as f32).atan2((x1 - cx) as f32);
            let sx0 = cx as f32 + a0.cos() * r0;
            let sy0 = cy as f32 + a0.sin() * r0;
            let sx1 = cx as f32 + a1.cos() * r1;
            let sy1 = cy as f32 + a1.sin() * r1;
            // 短い弧の代わりに隣接点を結ぶ
            if next_f32(&mut rng) > 0.25 {
                stroke_thick_line(
                    pixels,
                    w,
                    h,
                    sx0.round() as i32,
                    sy0.round() as i32,
                    sx1.round() as i32,
                    sy1.round() as i32,
                    (1.0 * dpi).round().max(1.0) as i32,
                    200,
                    235,
                    245,
                    ((a as f32) * 0.75) as u8,
                );
            }
        }
    }
}
