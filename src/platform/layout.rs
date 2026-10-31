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
        let (l, t) = minimap_panel_origin(
            500,
            400,
            540,
            VirtualScreen {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            },
        );
        assert_eq!(l, 500 - 270);
        assert_eq!(t, 400 - 270);
    }

    #[test]
    fn panel_clamped_at_right_edge() {
        let (l, t) = minimap_panel_origin(
            1900,
            400,
            540,
            VirtualScreen {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            },
        );
        assert_eq!(l, 1920 - 540);
        assert_eq!(t, 400 - 270);
    }
}
