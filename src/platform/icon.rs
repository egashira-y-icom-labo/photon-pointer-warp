//! Photon 正本 ICO の読み込み。Win32 には依存しない。
//!
//! 画素は Operator の `signature_asteroid_icon` を一度ラスタライズした
//! `assets/photon-pointer-warp.ico` から取り出す。描画コードは複製しない。

use std::fmt;

/// タスクバー / タイトルバー用に正本 ICO へ入っている辺の長さ。
pub const SIGNATURE_ICON_SIZES: [u32; 7] = [16, 20, 24, 32, 40, 48, 64];

/// トレイが使う小アイコン。`SM_CXSMICON` はこの中の最も近い辺に合わせる。
pub const TRAY_ICON_EDGES: [u32; 3] = [16, 20, 24];

const SIGNATURE_ICO: &[u8] = include_bytes!("../../assets/photon-pointer-warp.ico");

/// 水色ドット。正本と同じく B、G、R、A の順。
const SUGGEST_DOT_BGRA: [u8; 4] = [230, 220, 0, 255];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IconError {
    message: &'static str,
}

impl fmt::Display for IconError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message)
    }
}

impl std::error::Error for IconError {}

/// 上から下、ストレートアルファの BGRA。ICO 内の並びのまま。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconImage {
    pub size: u32,
    pub bgra: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayGlyph {
    Active,
    Paused,
    Suggest,
}

pub fn signature_icons() -> Result<&'static [IconImage], &'static IconError> {
    use std::sync::OnceLock;
    static PARSED: OnceLock<Result<Vec<IconImage>, IconError>> = OnceLock::new();
    PARSED
        .get_or_init(|| parse_ico(SIGNATURE_ICO))
        .as_ref()
        .map(Vec::as_slice)
}

pub fn signature_image(size: u32) -> Result<&'static IconImage, IconError> {
    let images = signature_icons().map_err(|err| *err)?;
    images
        .iter()
        .find(|image| image.size == size)
        .ok_or(IconError {
            message: "icon size is not in the embedded ICO",
        })
}

/// `SM_CXSMICON` を 16 / 20 / 24 のいずれかに丸める。同距離なら小さい方。
pub fn nearest_tray_edge(metric: i32) -> u32 {
    if metric <= 0 {
        return TRAY_ICON_EDGES[0];
    }
    let metric = metric as u32;
    TRAY_ICON_EDGES
        .into_iter()
        .min_by_key(|edge| (edge.abs_diff(metric), *edge))
        .unwrap_or(TRAY_ICON_EDGES[0])
}

pub fn tray_bgra(source: &[u8], size: u32, glyph: TrayGlyph) -> Vec<u8> {
    match glyph {
        TrayGlyph::Active => source.to_vec(),
        TrayGlyph::Paused => dim_to_gray(source),
        TrayGlyph::Suggest => paint_suggest_dot(source, size),
    }
}

/// AND マスク 1 行のバイト数。16px は 2、20px と 24px は 4。
pub fn and_mask_stride(width: u32) -> usize {
    (width as usize + 15) / 16 * 2
}

/// `CreateIcon` 向けの上から下の BGRA と、ワード境界の AND マスク。
/// アルファが 0 の画素だけを透明ビットにする。
pub fn xor_and_masks(bgra: &[u8], size: u32) -> (Vec<u8>, Vec<u8>) {
    let pixels = (size as usize) * (size as usize);
    let mut xor = vec![0u8; pixels * 4];
    let stride = and_mask_stride(size);
    let mut and_mask = vec![0xFFu8; stride * size as usize];
    let count = pixels.min(bgra.len() / 4);
    xor[..count * 4].copy_from_slice(&bgra[..count * 4]);
    for index in 0..count {
        if bgra[index * 4 + 3] == 0 {
            continue;
        }
        let x = index % size as usize;
        let y = index / size as usize;
        let bit = 7 - (x % 8);
        and_mask[y * stride + x / 8] &= !(1u8 << bit);
    }
    (xor, and_mask)
}

pub fn parse_ico(bytes: &[u8]) -> Result<Vec<IconImage>, IconError> {
    if bytes.len() < 6 {
        return Err(err("ICO header is truncated"));
    }
    let reserved = read_u16(bytes, 0)?;
    let kind = read_u16(bytes, 2)?;
    let count = read_u16(bytes, 4)? as usize;
    if reserved != 0 || kind != 1 {
        return Err(err("not a Windows icon"));
    }
    if count == 0 || bytes.len() < 6 + count * 16 {
        return Err(err("ICO directory is truncated"));
    }
    let mut images = Vec::with_capacity(count);
    for index in 0..count {
        let entry = 6 + index * 16;
        let width = bytes[entry];
        let height = bytes[entry + 1];
        let planes = read_u16(bytes, entry + 4)?;
        let bit_count = read_u16(bytes, entry + 6)?;
        let bytes_in_res = read_u32(bytes, entry + 8)? as usize;
        let offset = read_u32(bytes, entry + 12)? as usize;
        let size = if width == 0 { 256 } else { width as u32 };
        if height != width || planes != 1 || bit_count != 32 {
            return Err(err("ICO image is not a square 32bpp bitmap"));
        }
        let image = decode_dib(bytes, offset, bytes_in_res, size)?;
        if images
            .iter()
            .any(|existing: &IconImage| existing.size == size)
        {
            return Err(err("ICO contains a duplicate size"));
        }
        images.push(image);
    }
    images.sort_by_key(|image| image.size);
    Ok(images)
}

fn decode_dib(
    bytes: &[u8],
    offset: usize,
    bytes_in_res: usize,
    size: u32,
) -> Result<IconImage, IconError> {
    let xor_bytes = (size as usize) * (size as usize) * 4;
    if bytes_in_res < 40 + xor_bytes || bytes.len() < offset.saturating_add(40 + xor_bytes) {
        return Err(err("ICO bitmap is truncated"));
    }
    let header = &bytes[offset..];
    let header_size = read_u32(header, 0)?;
    let width = read_i32(header, 4)?;
    let height = read_i32(header, 8)?;
    let planes = read_u16(header, 12)?;
    let bit_count = read_u16(header, 14)?;
    let compression = read_u32(header, 16)?;
    if header_size != 40
        || width != size as i32
        || height != size as i32 * 2
        || planes != 1
        || bit_count != 32
        || compression != 0
    {
        return Err(err("ICO bitmap header is not a 32bpp DIB"));
    }
    let xor = &bytes[offset + 40..offset + 40 + xor_bytes];
    let mut bgra = vec![0u8; xor_bytes];
    let row = size as usize * 4;
    for y in 0..size as usize {
        let src = (size as usize - 1 - y) * row;
        let dst = y * row;
        bgra[dst..dst + row].copy_from_slice(&xor[src..src + row]);
    }
    Ok(IconImage { size, bgra })
}

fn dim_to_gray(bgra: &[u8]) -> Vec<u8> {
    let mut out = bgra.to_vec();
    for pixel in out.chunks_exact_mut(4) {
        let luma = (pixel[0] as u32 * 29 + pixel[1] as u32 * 150 + pixel[2] as u32 * 77) >> 8;
        let value = ((luma * 55) + 50) / 100;
        let value = value.min(255) as u8;
        pixel[0] = value;
        pixel[1] = value;
        pixel[2] = value;
    }
    out
}

fn paint_suggest_dot(bgra: &[u8], size: u32) -> Vec<u8> {
    let mut out = bgra.to_vec();
    let dot = (size / 5).max(2);
    let left = size.saturating_sub(dot + 1);
    let top = 1u32;
    if (size as usize) * (size as usize) * 4 != out.len() {
        return out;
    }
    for y in top..top.saturating_add(dot).min(size) {
        for x in left..left.saturating_add(dot).min(size) {
            let index = ((y * size + x) * 4) as usize;
            out[index..index + 4].copy_from_slice(&SUGGEST_DOT_BGRA);
        }
    }
    out
}

fn err(message: &'static str) -> IconError {
    IconError { message }
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, IconError> {
    bytes
        .get(offset..offset + 2)
        .and_then(|chunk| chunk.try_into().ok())
        .map(u16::from_le_bytes)
        .ok_or(err("ICO read is out of range"))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, IconError> {
    bytes
        .get(offset..offset + 4)
        .and_then(|chunk| chunk.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or(err("ICO read is out of range"))
}

fn read_i32(bytes: &[u8], offset: usize) -> Result<i32, IconError> {
    Ok(read_u32(bytes, offset)? as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_ico_contains_seven_32bpp_bgra_images() {
        let images = parse_ico(SIGNATURE_ICO).unwrap();
        assert_eq!(images.len(), SIGNATURE_ICON_SIZES.len());
        for (image, size) in images.iter().zip(SIGNATURE_ICON_SIZES) {
            assert_eq!(image.size, size);
            assert_eq!(image.bgra.len(), (size * size * 4) as usize);
            let center = pixel(image, size / 2, size / 2);
            assert_eq!(center[3], 255, "{size}px center is opaque");
            assert!(
                center[0] > center[2] && center[2] > center[1],
                "{size}px center is violet stored as BGRA, pixel={center:?}"
            );
            let last = size - 1;
            for (x, y) in [(0, 0), (last, 0), (0, last), (last, last)] {
                let alpha = pixel(image, x, y)[3];
                if size >= 24 {
                    assert_eq!(alpha, 0, "{size}px corner ({x},{y}) is outside the plate");
                } else {
                    assert!(
                        alpha > 0 && alpha < 255,
                        "{size}px corner ({x},{y}) is the antialiased plate edge, alpha={alpha}"
                    );
                }
            }
        }
    }

    #[test]
    fn paused_glyph_is_gray_at_about_half_brightness_and_keeps_alpha() {
        let source = [40u8, 30, 240, 200, 22, 17, 16, 255];
        let paused = tray_bgra(&source, 1, TrayGlyph::Paused);
        assert_eq!(paused[3], 200);
        assert_eq!(paused[7], 255);
        assert_eq!(paused[0], paused[1]);
        assert_eq!(paused[1], paused[2]);
        let luma = (40u32 * 29 + 30 * 150 + 240 * 77) >> 8;
        assert_eq!(paused[0] as u32, (luma * 55 + 50) / 100);
        assert!(paused[0] < 180);
    }

    #[test]
    fn suggest_glyph_paints_a_cyan_dot_at_the_top_right_only() {
        let image = signature_image(16).unwrap();
        let painted = tray_bgra(&image.bgra, 16, TrayGlyph::Suggest);
        let center = 8usize * 16 + 8;
        assert_eq!(
            &painted[center * 4..center * 4 + 4],
            &image.bgra[center * 4..center * 4 + 4]
        );
        let dot = (1usize * 16 + 13) * 4;
        assert_eq!(&painted[dot..dot + 4], &SUGGEST_DOT_BGRA);
        assert_ne!(&image.bgra[dot..dot + 4], &SUGGEST_DOT_BGRA);
    }

    #[test]
    fn and_mask_stride_pads_20_and_24_to_four_bytes() {
        assert_eq!(and_mask_stride(16), 2);
        assert_eq!(and_mask_stride(20), 4);
        assert_eq!(and_mask_stride(24), 4);
        assert_eq!(and_mask_stride(32), 4);
    }

    #[test]
    fn and_mask_clears_bits_only_where_alpha_is_present() {
        let image = signature_image(24).unwrap();
        let (xor, and_mask) = xor_and_masks(&image.bgra, 24);
        assert_eq!(xor.len(), 24 * 24 * 4);
        assert_eq!(and_mask.len(), 4 * 24);
        assert_ne!(
            and_mask[0] & 0b1000_0000,
            0,
            "transparent corner stays masked"
        );
        let center = 12usize;
        let bit = 7 - (center % 8);
        assert_eq!(
            and_mask[center * 4 + center / 8] & (1u8 << bit),
            0,
            "opaque center is drawn"
        );
        let src = (center * 24 + center) * 4;
        assert_eq!(&xor[src..src + 4], &image.bgra[src..src + 4]);
    }

    #[test]
    fn tray_edge_picks_the_nearest_embedded_size() {
        assert_eq!(nearest_tray_edge(0), 16);
        assert_eq!(nearest_tray_edge(16), 16);
        assert_eq!(nearest_tray_edge(20), 20);
        assert_eq!(nearest_tray_edge(24), 24);
        assert_eq!(nearest_tray_edge(18), 16);
        assert_eq!(nearest_tray_edge(22), 20);
        assert_eq!(nearest_tray_edge(32), 24);
    }

    fn pixel(image: &IconImage, x: u32, y: u32) -> [u8; 4] {
        let index = ((y * image.size + x) * 4) as usize;
        image.bgra[index..index + 4].try_into().unwrap()
    }
}
