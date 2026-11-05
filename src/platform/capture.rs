//! HUD 展開時の縮小キャプチャ。画素はメモリ上だけに置き、破棄時にゼロにする。

use std::io;

use crate::domain::monitor_layout::Rect;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    SetStretchBltMode, StretchBlt, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, COLORONCOLOR,
    DIB_RGB_COLORS, HBITMAP, HDC, SRCCOPY,
};

/// パネル座標上のモニター 1 枚分。画素は上から下、premultiplied BGRA。
pub struct ThumbnailBlock {
    pub dest: Rect,
    pixels: Vec<u8>,
}

impl ThumbnailBlock {
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
}

impl Drop for ThumbnailBlock {
    fn drop(&mut self) {
        self.pixels.fill(0);
    }
}

/// 1 回の HUD 展開で取得した、モニターブロックごとの背景画像。
pub struct ThumbnailLayer {
    blocks: Vec<ThumbnailBlock>,
}

impl ThumbnailLayer {
    pub fn monitor_count(&self) -> usize {
        self.blocks.len()
    }

    pub fn blocks(&self) -> &[ThumbnailBlock] {
        &self.blocks
    }

    pub fn clear(&mut self) {
        for thumb in &mut self.blocks {
            thumb.pixels.fill(0);
        }
        self.blocks.clear();
    }
}

impl Drop for ThumbnailLayer {
    fn drop(&mut self) {
        self.clear();
    }
}

/// ワーカーへ渡す、画面上の取得領域とパネル内の配置先。
#[derive(Clone, Copy)]
pub struct CaptureRequest {
    pub src: Rect,
    pub dest: Rect,
}

/// 各モニターをブロックサイズへ `StretchBlt` し、暗く半透明な premultiplied BGRA にする。
/// `CAPTUREBLT` は使わない。
pub unsafe fn capture_thumbnails(
    requests: &[CaptureRequest],
    dim: f32,
) -> io::Result<ThumbnailLayer> {
    if requests.is_empty() {
        return Ok(ThumbnailLayer { blocks: Vec::new() });
    }
    let screen = GetDC(None);
    if screen.0.is_null() {
        return Err(io::Error::other("GetDC(NULL) failed"));
    }
    let result = capture_into(screen, requests, dim);
    ReleaseDC(None, screen);
    result
}

unsafe fn capture_into(
    screen: HDC,
    requests: &[CaptureRequest],
    dim: f32,
) -> io::Result<ThumbnailLayer> {
    let mut blocks = Vec::with_capacity(requests.len());
    for request in requests {
        let src_w = request.src.width();
        let src_h = request.src.height();
        let dest_w = request.dest.width();
        let dest_h = request.dest.height();
        if src_w <= 0 || src_h <= 0 || dest_w <= 0 || dest_h <= 0 {
            continue;
        }
        let pixels = blit_monitor(screen, request.src, dest_w, dest_h, dim)?;
        blocks.push(ThumbnailBlock {
            dest: request.dest,
            pixels,
        });
    }
    Ok(ThumbnailLayer { blocks })
}

unsafe fn blit_monitor(
    screen: HDC,
    src: Rect,
    dest_w: i32,
    dest_h: i32,
    dim: f32,
) -> io::Result<Vec<u8>> {
    let mem = CreateCompatibleDC(screen);
    if mem.0.is_null() {
        return Err(io::Error::other("CreateCompatibleDC failed"));
    }
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    let bitmap = match create_topdown_dib(mem, dest_w, dest_h, &mut bits) {
        Ok(bitmap) => bitmap,
        Err(err) => {
            let _ = DeleteDC(mem);
            return Err(err);
        }
    };
    let previous = SelectObject(mem, bitmap);
    SetStretchBltMode(mem, COLORONCOLOR);
    let copied = StretchBlt(
        mem,
        0,
        0,
        dest_w,
        dest_h,
        screen,
        src.left,
        src.top,
        src.width(),
        src.height(),
        SRCCOPY,
    );
    let len = (dest_w as usize)
        .saturating_mul(dest_h as usize)
        .saturating_mul(4);
    let mut pixels = vec![0u8; len];
    if copied.as_bool() && !bits.is_null() {
        std::ptr::copy_nonoverlapping(bits as *const u8, pixels.as_mut_ptr(), len);
        std::ptr::write_bytes(bits as *mut u8, 0, len);
    }
    if !previous.0.is_null() {
        SelectObject(mem, previous);
    }
    let _ = DeleteObject(bitmap);
    let _ = DeleteDC(mem);
    if !copied.as_bool() {
        pixels.fill(0);
        return Err(io::Error::other("StretchBlt failed"));
    }
    dim_to_premultiplied(&mut pixels, dim);
    Ok(pixels)
}

unsafe fn create_topdown_dib(
    dc: HDC,
    width: i32,
    height: i32,
    bits: *mut *mut core::ffi::c_void,
) -> io::Result<HBITMAP> {
    let info = BITMAPINFO {
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
    CreateDIBSection(dc, &info, DIB_RGB_COLORS, bits, None, 0)
        .map_err(|err| io::Error::other(format!("CreateDIBSection: {err}")))
}

const THUMBNAIL_ALPHA: u16 = 225;

/// 画面キャプチャの不定なアルファを捨て、HUD 用の固定アルファへ変換する。
fn dim_to_premultiplied(pixels: &mut [u8], dim: f32) {
    let dim = dim.clamp(0.2, 0.8);
    let factor = (dim * THUMBNAIL_ALPHA as f32).round() as u16;
    for pixel in pixels.chunks_exact_mut(4) {
        pixel[0] = ((pixel[0] as u16 * factor) / 255) as u8;
        pixel[1] = ((pixel[1] as u16 * factor) / 255) as u8;
        pixel[2] = ((pixel[2] as u16 * factor) / 255) as u8;
        pixel[3] = THUMBNAIL_ALPHA as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::dim_to_premultiplied;

    #[test]
    fn dimming_produces_fixed_premultiplied_alpha() {
        let mut pixels = vec![255, 128, 64, 0];
        dim_to_premultiplied(&mut pixels, 0.4);
        assert_eq!(pixels[3], 225);
        assert!(pixels[0] <= 90);
        assert!(pixels[1] < pixels[0]);
        assert!(pixels[2] < pixels[1]);
    }
}
