//! HUD レイアウト（Win32 非依存）

pub const MINIMAP_GRID: usize = 3;
/// 3×3 ミニマップパネル一辺（px）
pub const MINIMAP_PANEL_PX: i32 = 180;
/// 1 セル一辺（px）
pub const MINIMAP_CELL_PX: i32 = MINIMAP_PANEL_PX / MINIMAP_GRID as i32;
/// 地縛霊クリックキャンセル半径（px）
pub const ORIGIN_CANCEL_RADIUS_PX: i32 = 15;
/// ミニマップ外れ自動 dismiss 用の拡張マージン（px）。パネル 180 + 両側 50 → 280 相当。
pub const DISMISS_OUT_OF_BOUNDS_MARGIN_PX: i32 = 50;
/// 提案ヒントをパネル下端から離す距離（描画専用、ヒットテスト非使用）。
pub const SUGGESTION_HINT_BELOW_GAP_PX: i32 = 10;
/// 下にはみ出すときの上側ギャップ。
pub const SUGGESTION_HINT_ABOVE_GAP_PX: i32 = 28;
pub const SUGGESTION_HINT_HEIGHT_PX: i32 = 24;
pub const SUGGESTION_HINT_WIDTH_PX: i32 = 290;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelScreenRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// パネル矩形を `margin` だけ外側に広げた領域の外にカーソルがあるか。
/// 提案ヒントの画面矩形。セル幾何には使わない。下に収まらなければ上へ反転する。
pub fn suggestion_hint_rect(panel: PanelScreenRect, vs: VirtualScreen) -> PanelScreenRect {
    let vs_bottom = vs.y + vs.height;
    let vs_right = vs.x + vs.width;
    let center_x = (panel.left + panel.right) / 2;
    let half = SUGGESTION_HINT_WIDTH_PX / 2;
    let mut left = center_x - half;
    let mut right = left + SUGGESTION_HINT_WIDTH_PX;
    if left < vs.x {
        left = vs.x;
        right = left + SUGGESTION_HINT_WIDTH_PX;
    }
    if right > vs_right {
        right = vs_right;
        left = (right - SUGGESTION_HINT_WIDTH_PX).max(vs.x);
    }

    let below_top = panel.bottom + SUGGESTION_HINT_BELOW_GAP_PX;
    let below_bottom = below_top + SUGGESTION_HINT_HEIGHT_PX;
    if below_bottom <= vs_bottom {
        PanelScreenRect {
            left,
            top: below_top,
            right,
            bottom: below_bottom,
        }
    } else {
        let top = (panel.top - SUGGESTION_HINT_ABOVE_GAP_PX - SUGGESTION_HINT_HEIGHT_PX).max(vs.y);
        PanelScreenRect {
            left,
            top,
            right,
            bottom: top + SUGGESTION_HINT_HEIGHT_PX,
        }
    }
}

pub fn is_pointer_out_of_bounds(
    cursor: (i32, i32),
    panel: PanelScreenRect,
    margin: i32,
) -> bool {
    cursor.0 < panel.left - margin
        || cursor.0 > panel.right + margin
        || cursor.1 < panel.top - margin
        || cursor.1 > panel.bottom + margin
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualScreen {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// ミニマップのアンカー一致レイアウト（地縛霊ピン ≈ 物理カーソル）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MinimapAnchorLayout {
    pub panel_left: i32,
    pub panel_top: i32,
    pub pin_screen_x: i32,
    pub pin_screen_y: i32,
}

fn gesture_pin_offsets(gesture_x: i32, gesture_y: i32, panel_size: i32, vs: VirtualScreen) -> (i32, i32) {
    let norm_x =
        ((gesture_x - vs.x) as f32 / vs.width.max(1) as f32).clamp(0.0, 1.0);
    let norm_y =
        ((gesture_y - vs.y) as f32 / vs.height.max(1) as f32).clamp(0.0, 1.0);
    let pin_offset_x = (norm_x * panel_size as f32).round() as i32;
    let pin_offset_y = (norm_y * panel_size as f32).round() as i32;
    (pin_offset_x, pin_offset_y)
}

/// 地縛霊ピンが `gesture` と重なるようパネルを配置（はみ出し時のみクランプ）。
pub fn minimap_anchor_layout(
    gesture_x: i32,
    gesture_y: i32,
    vs: VirtualScreen,
) -> MinimapAnchorLayout {
    let panel_size = MINIMAP_PANEL_PX;
    let (pin_offset_x, pin_offset_y) = gesture_pin_offsets(gesture_x, gesture_y, panel_size, vs);

    let ideal_left = gesture_x - pin_offset_x;
    let ideal_top = gesture_y - pin_offset_y;

    let max_left = if vs.width > panel_size {
        vs.x + vs.width - panel_size
    } else {
        vs.x
    };
    let max_top = if vs.height > panel_size {
        vs.y + vs.height - panel_size
    } else {
        vs.y
    };
    let panel_left = ideal_left.clamp(vs.x, max_left);
    let panel_top = ideal_top.clamp(vs.y, max_top);

    MinimapAnchorLayout {
        panel_left,
        panel_top,
        pin_screen_x: panel_left + pin_offset_x,
        pin_screen_y: panel_top + pin_offset_y,
    }
}

/// 仮想デスクトップ 3×3 グリッドのセル中心座標（スクリーン座標）。
pub fn warp_target_for_cell(col: usize, row: usize, virtual_screen: VirtualScreen) -> (i32, i32) {
    debug_assert!(col < MINIMAP_GRID && row < MINIMAP_GRID);
    let VirtualScreen { x, y, width, height } = virtual_screen;
    let target_x = x as f32 + (col as f32 + 0.5) * (width as f32 / MINIMAP_GRID as f32);
    let target_y = y as f32 + (row as f32 + 0.5) * (height as f32 / MINIMAP_GRID as f32);
    (target_x.round() as i32, target_y.round() as i32)
}

/// ミニマップパネル内のセル `(col, row)`。パネル外は `None`。
pub fn minimap_cell_at(
    screen_x: i32,
    screen_y: i32,
    panel_left: i32,
    panel_top: i32,
    panel_size: i32,
) -> Option<(usize, usize)> {
    if panel_size <= 0 {
        return None;
    }
    let right = panel_left.saturating_add(panel_size);
    let bottom = panel_top.saturating_add(panel_size);
    if screen_x < panel_left || screen_y < panel_top || screen_x >= right || screen_y >= bottom {
        return None;
    }
    let cell = panel_size / MINIMAP_GRID as i32;
    if cell <= 0 {
        return None;
    }
    let col = ((screen_x - panel_left) / cell) as usize;
    let row = ((screen_y - panel_top) / cell) as usize;
    if col >= MINIMAP_GRID || row >= MINIMAP_GRID {
        return None;
    }
    Some((col, row))
}

pub fn point_within_radius(
    screen_x: i32,
    screen_y: i32,
    center_x: i32,
    center_y: i32,
    radius_px: i32,
) -> bool {
    let dx = screen_x - center_x;
    let dy = screen_y - center_y;
    dx * dx + dy * dy <= radius_px * radius_px
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vs_1920x1200() -> VirtualScreen {
        VirtualScreen {
            x: 0,
            y: 0,
            width: 1920,
            height: 1200,
        }
    }

    fn assert_panel_inside_vs(layout: MinimapAnchorLayout, vs: VirtualScreen) {
        assert!(layout.panel_left >= vs.x);
        assert!(layout.panel_top >= vs.y);
        assert!(layout.panel_left + MINIMAP_PANEL_PX <= vs.x + vs.width);
        assert!(layout.panel_top + MINIMAP_PANEL_PX <= vs.y + vs.height);
    }

    #[test]
    fn anchor_pin_matches_gesture_at_desktop_center() {
        let vs = vs_1920x1200();
        let layout = minimap_anchor_layout(960, 600, vs);
        assert_eq!(layout.pin_screen_x, 960);
        assert_eq!(layout.pin_screen_y, 600);
        assert_panel_inside_vs(layout, vs);
    }

    #[test]
    fn anchor_pin_matches_gesture_at_desktop_bottom_left() {
        let vs = vs_1920x1200();
        let layout = minimap_anchor_layout(96, 1140, vs);
        assert_eq!(layout.pin_screen_x, 96);
        assert_eq!(layout.pin_screen_y, 1140);
        assert_panel_inside_vs(layout, vs);
        let cell = minimap_cell_at(
            layout.pin_screen_x,
            layout.pin_screen_y,
            layout.panel_left,
            layout.panel_top,
            MINIMAP_PANEL_PX,
        )
        .unwrap();
        assert_eq!(cell, (0, 2));
    }

    #[test]
    fn anchor_pin_matches_gesture_at_desktop_bottom_right() {
        let vs = vs_1920x1200();
        let layout = minimap_anchor_layout(1824, 1140, vs);
        assert_eq!(layout.pin_screen_x, 1824);
        assert_eq!(layout.pin_screen_y, 1140);
        assert_panel_inside_vs(layout, vs);
    }

    #[test]
    fn anchor_pin_matches_gesture_at_screen_corners() {
        let vs = vs_1920x1200();
        for (gx, gy) in [(0, 0), (1920, 0), (0, 1200), (1920, 1200)] {
            let layout = minimap_anchor_layout(gx, gy, vs);
            assert_eq!(
                (layout.pin_screen_x, layout.pin_screen_y),
                (gx, gy),
                "gesture=({gx},{gy})"
            );
            assert_panel_inside_vs(layout, vs);
        }
    }

    #[test]
    fn warp_target_cell_centers() {
        let vs = VirtualScreen {
            x: 100,
            y: 50,
            width: 3000,
            height: 1500,
        };
        assert_eq!(warp_target_for_cell(0, 0, vs), (600, 300));
        assert_eq!(warp_target_for_cell(2, 1, vs), (2600, 800));
    }

    #[test]
    fn minimap_cell_indexing() {
        let panel = MINIMAP_PANEL_PX;
        let (c, r) = minimap_cell_at(70, 130, 10, 10, panel).unwrap();
        assert_eq!(c, 1);
        assert_eq!(r, 2);
        assert!(minimap_cell_at(9, 10, 10, 10, panel).is_none());
    }

    #[test]
    fn origin_cancel_radius_constant() {
        assert_eq!(ORIGIN_CANCEL_RADIUS_PX, 15);
        assert_eq!(MINIMAP_CELL_PX, 60);
        assert_eq!(DISMISS_OUT_OF_BOUNDS_MARGIN_PX, 50);
    }

    #[test]
    fn dismiss_out_of_bounds_margin() {
        let panel = PanelScreenRect {
            left: 100,
            top: 100,
            right: 280,
            bottom: 280,
        };
        let margin = DISMISS_OUT_OF_BOUNDS_MARGIN_PX;

        assert!(!is_pointer_out_of_bounds((150, 150), panel, margin));

        assert!(!is_pointer_out_of_bounds((80, 150), panel, margin));
        assert!(!is_pointer_out_of_bounds((300, 150), panel, margin));

        assert!(is_pointer_out_of_bounds((39, 150), panel, margin));
        assert!(is_pointer_out_of_bounds((341, 150), panel, margin));
    }

    #[test]
    fn suggestion_hint_sits_below_panel_when_it_fits() {
        let vs = VirtualScreen {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let panel = PanelScreenRect {
            left: 100,
            top: 100,
            right: 280,
            bottom: 280,
        };
        let hint = suggestion_hint_rect(panel, vs);
        assert_eq!(hint.left, 45);
        assert_eq!(hint.right, 335);
        assert_eq!(hint.right - hint.left, SUGGESTION_HINT_WIDTH_PX);
        assert_eq!(hint.top, 290);
        assert_eq!(hint.bottom, 314);
    }

    #[test]
    fn suggestion_hint_flips_above_when_below_clips() {
        let vs = VirtualScreen {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let panel = PanelScreenRect {
            left: 100,
            top: 900,
            right: 280,
            bottom: 1080,
        };
        let hint = suggestion_hint_rect(panel, vs);
        assert_eq!(hint.bottom, 872);
        assert_eq!(hint.top, 848);
        assert_eq!(hint.right - hint.left, SUGGESTION_HINT_WIDTH_PX);
        assert!(hint.bottom <= panel.top);
        assert!(hint.top >= vs.y);
    }
}
