//! HUD レイアウト（Win32 非依存）

/// 原点を中心に `panel` 四方の矩形を置き、仮想デスクトップ `(vx, vy, vw, vh)` 内に収める。
pub fn minimap_panel_origin(origin_x: i32, origin_y: i32, panel: i32, virtual_screen: VirtualScreen) -> (i32, i32) {
    let VirtualScreen { x: vx, y: vy, width: vw, height: vh } = virtual_screen;
    let half = panel / 2;
    let mut left = origin_x - half;
    let mut top = origin_y - half;
    if vw > panel {
        left = left.clamp(vx, vx + vw - panel);
    } else {
        left = vx;
    }
    if vh > panel {
        top = top.clamp(vy, vy + vh - panel);
    } else {
        top = vy;
    }
    (left, top)
}

pub const MINIMAP_GRID: usize = 3;
/// 3×3 ミニマップパネル一辺（px）
pub const MINIMAP_PANEL_PX: i32 = 180;
/// 1 セル一辺（px）
pub const MINIMAP_CELL_PX: i32 = MINIMAP_PANEL_PX / MINIMAP_GRID as i32;
/// 地縛霊クリックキャンセル半径（px）
pub const ORIGIN_CANCEL_RADIUS_PX: i32 = 15;

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

/// 仮想デスクトップ上のジェスチャ起点を、ミニマップパネル内のスクリーン座標へ射影する。
pub fn minimap_pin_screen_from_gesture(
    gesture_x: i32,
    gesture_y: i32,
    panel_left: i32,
    panel_top: i32,
    panel_size: i32,
    vs: VirtualScreen,
) -> (i32, i32) {
    let norm_x =
        ((gesture_x - vs.x) as f32 / vs.width.max(1) as f32).clamp(0.0, 1.0);
    let norm_y =
        ((gesture_y - vs.y) as f32 / vs.height.max(1) as f32).clamp(0.0, 1.0);
    let pin_x = panel_left + (norm_x * panel_size as f32).round() as i32;
    let pin_y = panel_top + (norm_y * panel_size as f32).round() as i32;
    (pin_x, pin_y)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualScreen {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_centered_when_room() {
        let panel = MINIMAP_PANEL_PX;
        let (l, t) = minimap_panel_origin(
            500,
            400,
            panel,
            VirtualScreen {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            },
        );
        assert_eq!(l, 500 - panel / 2);
        assert_eq!(t, 400 - panel / 2);
    }

    #[test]
    fn panel_clamped_at_right_edge() {
        let panel = MINIMAP_PANEL_PX;
        let (l, t) = minimap_panel_origin(
            1900,
            400,
            panel,
            VirtualScreen {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            },
        );
        assert_eq!(l, 1920 - panel);
        assert_eq!(t, 400 - panel / 2);
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
    }

    #[test]
    fn proportional_pin_at_desktop_bottom_left() {
        let vs = VirtualScreen {
            x: 0,
            y: 0,
            width: 1920,
            height: 1200,
        };
        let panel = MINIMAP_PANEL_PX;
        let (panel_left, panel_top) = minimap_panel_origin(96, 1140, panel, vs);
        let (pin_x, pin_y) = minimap_pin_screen_from_gesture(
            96,
            1140,
            panel_left,
            panel_top,
            panel,
            vs,
        );
        let rel_x = pin_x - panel_left;
        let rel_y = pin_y - panel_top;
        assert!(rel_x < panel / 3, "pin should sit in left third, got rel_x={rel_x}");
        assert!(rel_y >= panel * 2 / 3, "pin should sit in bottom third, got rel_y={rel_y}");
        let cell = minimap_cell_at(pin_x, pin_y, panel_left, panel_top, panel).unwrap();
        assert_eq!(cell, (0, 2));
    }

    #[test]
    fn proportional_pin_at_desktop_center() {
        let vs = VirtualScreen {
            x: 0,
            y: 0,
            width: 1000,
            height: 1000,
        };
        let panel = MINIMAP_PANEL_PX;
        let (panel_left, panel_top) = (200, 200);
        let (pin_x, pin_y) =
            minimap_pin_screen_from_gesture(500, 500, panel_left, panel_top, panel, vs);
        assert_eq!(pin_x - panel_left, 90);
        assert_eq!(pin_y - panel_top, 90);
    }
}
