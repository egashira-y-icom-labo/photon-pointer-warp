//! HUD 上のクリックとホバーを、一つの優先順位で分類する。

use crate::domain::monitor_layout::Rect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HudHit {
    Badge,
    HeaderLeft,
    Origin,
    Monitor { index: usize, target: Point },
    /// 3×3 グリッドのセル。`index` は行優先で 0 から 8。
    Cell { index: usize },
    PanelEmpty,
    Outside,
}

/// 画面上の点を分類するための領域。`monitor` はブロック（または 3×3 セル）に入るときだけ Some。
#[derive(Debug, Clone, Copy)]
pub struct HudHitRegions {
    pub panel: Rect,
    pub badge: Option<Rect>,
    pub header_left: Option<Rect>,
    pub pin: Point,
    pub origin_radius: i32,
    pub monitor: Option<(usize, Point)>,
    pub cell: Option<usize>,
}

/// 優先順位: バッジ、ヘッダー左、原点、モニター、グリッドセル、パネル内の空き、外側。
pub fn classify_hud_hit(screen: Point, regions: HudHitRegions) -> HudHit {
    if regions
        .badge
        .is_some_and(|rect| rect.contains((screen.x, screen.y)))
    {
        return HudHit::Badge;
    }
    if regions
        .header_left
        .is_some_and(|rect| rect.contains((screen.x, screen.y)))
    {
        return HudHit::HeaderLeft;
    }
    if within_radius(screen, regions.pin, regions.origin_radius) {
        return HudHit::Origin;
    }
    if let Some((index, target)) = regions.monitor {
        return HudHit::Monitor { index, target };
    }
    if let Some(index) = regions.cell {
        return HudHit::Cell { index };
    }
    if regions.panel.contains((screen.x, screen.y)) {
        return HudHit::PanelEmpty;
    }
    HudHit::Outside
}

fn within_radius(point: Point, center: Point, radius: i32) -> bool {
    if radius < 0 {
        return false;
    }
    let dx = point.x as i64 - center.x as i64;
    let dy = point.y as i64 - center.y as i64;
    let radius = radius as i64;
    dx * dx + dy * dy <= radius * radius
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regions() -> HudHitRegions {
        HudHitRegions {
            panel: Rect {
                left: 0,
                top: 0,
                right: 200,
                bottom: 120,
            },
            badge: Some(Rect {
                left: 176,
                top: 2,
                right: 196,
                bottom: 22,
            }),
            header_left: Some(Rect {
                left: 0,
                top: 0,
                right: 90,
                bottom: 24,
            }),
            pin: Point { x: 20, y: 30 },
            origin_radius: 15,
            monitor: None,
            cell: None,
        }
    }

    #[test]
    fn badge_beats_header_origin_and_monitor() {
        let mut regions = regions();
        regions.header_left = Some(Rect {
            left: 0,
            top: 0,
            right: 200,
            bottom: 24,
        });
        regions.pin = Point { x: 186, y: 12 };
        regions.monitor = Some((1, Point { x: 10, y: 10 }));
        assert_eq!(
            classify_hud_hit(Point { x: 180, y: 10 }, regions),
            HudHit::Badge
        );
    }

    #[test]
    fn header_left_beats_the_origin_circle() {
        let regions = regions();
        assert_eq!(
            classify_hud_hit(Point { x: 20, y: 16 }, regions),
            HudHit::HeaderLeft
        );
    }

    #[test]
    fn origin_beats_a_monitor_block() {
        let mut regions = regions();
        regions.monitor = Some((0, Point { x: 400, y: 300 }));
        assert_eq!(
            classify_hud_hit(Point { x: 20, y: 30 }, regions),
            HudHit::Origin
        );
    }

    #[test]
    fn monitor_returns_the_supplied_landing_point() {
        let mut regions = regions();
        let target = Point { x: 1280, y: 400 };
        regions.monitor = Some((2, target));
        assert_eq!(
            classify_hud_hit(Point { x: 40, y: 70 }, regions),
            HudHit::Monitor { index: 2, target }
        );
    }

    #[test]
    fn empty_header_and_panel_gaps_are_not_clicks() {
        let regions = regions();
        assert_eq!(
            classify_hud_hit(Point { x: 140, y: 8 }, regions),
            HudHit::PanelEmpty
        );
        assert_eq!(
            classify_hud_hit(Point { x: 40, y: 50 }, regions),
            HudHit::PanelEmpty
        );
    }

    #[test]
    fn outside_the_panel_is_outside_unless_it_is_the_origin() {
        let regions = regions();
        assert_eq!(
            classify_hud_hit(Point { x: -1, y: 40 }, regions),
            HudHit::Outside
        );
        assert_eq!(
            classify_hud_hit(Point { x: 8, y: 30 }, regions),
            HudHit::Origin
        );
    }

    #[test]
    fn header_and_origin_beat_a_grid_cell() {
        let mut regions = regions();
        regions.cell = Some(4);
        assert_eq!(
            classify_hud_hit(Point { x: 20, y: 16 }, regions),
            HudHit::HeaderLeft
        );
        assert_eq!(
            classify_hud_hit(Point { x: 20, y: 30 }, regions),
            HudHit::Origin
        );
        assert_eq!(
            classify_hud_hit(Point { x: 40, y: 70 }, regions),
            HudHit::Cell { index: 4 }
        );
    }

    #[test]
    fn missing_badge_does_not_claim_the_bulb_slot() {
        let mut regions = regions();
        regions.badge = None;
        assert_eq!(
            classify_hud_hit(Point { x: 180, y: 10 }, regions),
            HudHit::PanelEmpty
        );
    }
}
