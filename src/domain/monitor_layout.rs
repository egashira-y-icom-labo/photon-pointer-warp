//! モニター配置の縮小写像と着地点クランプ（純 Rust / Win32 非依存）
//!
//! 矩形の `right` / `bottom` は排他。ブロックの辺は、外接矩形の原点からの
//! `floor(距離 * 共通スケール)` で写す。隣り合うモニターの共有辺は同じ値になる。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn width(self) -> i32 {
        self.right.saturating_sub(self.left).max(0)
    }

    pub fn height(self) -> i32 {
        self.bottom.saturating_sub(self.top).max(0)
    }

    pub fn is_valid(self) -> bool {
        self.right > self.left && self.bottom > self.top
    }

    pub fn contains(self, p: (i32, i32)) -> bool {
        p.0 >= self.left && p.0 < self.right && p.1 >= self.top && p.1 < self.bottom
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Monitor {
    pub rect: Rect,
    pub work: Rect,
    pub is_primary: bool,
}

/// プライマリを先頭にし、残りは左から、同じ左端なら上から並べる。
/// 外接矩形の縦横比に合わせたパネル上限。長辺は `long_max`、短辺は `short_min` 以上。
pub fn panel_limits(monitors: &[Monitor], long_max: i32, short_min: i32) -> (i32, i32) {
    let long_max = long_max.max(1);
    let short_min = short_min.clamp(1, long_max);
    let bbox = union_rects(monitors.iter().copied().map(|monitor| monitor.rect));
    let bbox_w = bbox.width();
    let bbox_h = bbox.height();
    if bbox_w <= 0 || bbox_h <= 0 {
        return (long_max, long_max);
    }
    if bbox_w >= bbox_h {
        let height = ((long_max as i64) * bbox_h as i64 / bbox_w as i64)
            .clamp(short_min as i64, long_max as i64) as i32;
        (long_max, height.max(1))
    } else {
        let width = ((long_max as i64) * bbox_w as i64 / bbox_h as i64)
            .clamp(short_min as i64, long_max as i64) as i32;
        (width.max(1), long_max)
    }
}

pub fn sort_monitors(monitors: &mut [Monitor]) {
    monitors.sort_by(monitor_order);
}

fn monitor_order(a: &Monitor, b: &Monitor) -> std::cmp::Ordering {
    b.is_primary
        .cmp(&a.is_primary)
        .then(a.rect.left.cmp(&b.rect.left))
        .then(a.rect.top.cmp(&b.rect.top))
        .then(a.rect.right.cmp(&b.rect.right))
        .then(a.rect.bottom.cmp(&b.rect.bottom))
}

/// 点がいずれかのモニター内ならそのまま返す。外なら最も近い実在点へ寄せる。
/// 距離が同じときはプライマリ、左、上の順を優先する。空なら点を返す。
pub fn clamp_to_monitors(p: (i32, i32), monitors: &[Monitor]) -> (i32, i32) {
    let mut indexed: Vec<&Monitor> = monitors.iter().filter(|m| m.rect.is_valid()).collect();
    if indexed.is_empty() {
        return p;
    }
    indexed.sort_by(|a, b| monitor_order(a, b));
    if indexed.iter().any(|m| m.rect.contains(p)) {
        return p;
    }
    let mut best: Option<(i64, (i32, i32))> = None;
    for monitor in indexed {
        let q = closest_point(p, monitor.rect);
        let dx = q.0 as i64 - p.0 as i64;
        let dy = q.1 as i64 - p.1 as i64;
        let distance = dx * dx + dy * dy;
        match best {
            Some((best_distance, _)) if distance >= best_distance => {}
            _ => best = Some((distance, q)),
        }
    }
    best.map(|(_, q)| q).unwrap_or(p)
}

fn closest_point(p: (i32, i32), rect: Rect) -> (i32, i32) {
    let max_x = rect.right.saturating_sub(1).max(rect.left);
    let max_y = rect.bottom.saturating_sub(1).max(rect.top);
    (p.0.clamp(rect.left, max_x), p.1.clamp(rect.top, max_y))
}

#[derive(Debug, Clone, PartialEq)]
pub struct MinimapGeometry {
    pub panel_w: i32,
    pub panel_h: i32,
    pub blocks: Vec<Rect>,
    monitors: Vec<Monitor>,
    bbox: Rect,
    scale: f64,
    offset_x: i32,
    offset_y: i32,
}

impl MinimapGeometry {
    /// `max_w`×`max_h` の枠に、余白 `pad` を残して相似縮小し中央へ置く。
    pub fn build(monitors: &[Monitor], max_w: i32, max_h: i32, pad: i32) -> Self {
        let max_w = max_w.max(1);
        let max_h = max_h.max(1);
        let pad = pad.max(0);
        let mut monitors: Vec<Monitor> = monitors
            .iter()
            .copied()
            .filter(|monitor| monitor.rect.is_valid())
            .collect();
        sort_monitors(&mut monitors);
        if monitors.is_empty() || max_w <= pad * 2 || max_h <= pad * 2 {
            return Self::empty(max_w, max_h);
        }

        let bbox = union_rects(monitors.iter().map(|monitor| monitor.rect));
        let bbox_w = bbox.width();
        let bbox_h = bbox.height();
        if bbox_w <= 0 || bbox_h <= 0 {
            return Self::empty(max_w, max_h);
        }
        let inner_w = max_w - pad * 2;
        let inner_h = max_h - pad * 2;
        let scale = (inner_w as f64 / bbox_w as f64).min(inner_h as f64 / bbox_h as f64);
        if !scale.is_finite() || scale <= 0.0 {
            return Self::empty(max_w, max_h);
        }

        let mapped_w = map_delta(bbox_w, scale);
        let mapped_h = map_delta(bbox_h, scale);
        let offset_x = pad + (inner_w - mapped_w).max(0) / 2;
        let offset_y = pad + (inner_h - mapped_h).max(0) / 2;
        let blocks = monitors
            .iter()
            .map(|monitor| Rect {
                left: offset_x + map_delta(delta(bbox.left, monitor.rect.left), scale),
                top: offset_y + map_delta(delta(bbox.top, monitor.rect.top), scale),
                right: offset_x + map_delta(delta(bbox.left, monitor.rect.right), scale),
                bottom: offset_y + map_delta(delta(bbox.top, monitor.rect.bottom), scale),
            })
            .collect();

        Self {
            panel_w: max_w,
            panel_h: max_h,
            blocks,
            monitors,
            bbox,
            scale,
            offset_x,
            offset_y,
        }
    }

    /// 画面座標をパネル座標へ写す。モニター内の点は、そのブロックの内側に収める。
    pub fn screen_to_panel(&self, p: (i32, i32)) -> (i32, i32) {
        if self.scale <= 0.0 {
            return (0, 0);
        }
        let raw = (
            self.offset_x + map_delta(delta(self.bbox.left, p.0), self.scale),
            self.offset_y + map_delta(delta(self.bbox.top, p.1), self.scale),
        );
        let Some(index) = self
            .monitors
            .iter()
            .position(|monitor| monitor.rect.contains(p))
        else {
            return raw;
        };
        let block = self.blocks[index];
        (
            raw.0.clamp(block.left, (block.right - 1).max(block.left)),
            raw.1.clamp(block.top, (block.bottom - 1).max(block.top)),
        )
    }

    /// パネル上の点がモニターブロック内なら、そのモニター番号と画面上の着地点。
    /// 余白やモニター間の隙間は `None`。
    pub fn hit(&self, panel_pt: (i32, i32)) -> Option<(usize, (i32, i32))> {
        let index = self
            .blocks
            .iter()
            .position(|block| block.contains(panel_pt))?;
        let screen = block_to_screen(self.blocks[index], self.monitors[index].rect, panel_pt);
        Some((index, screen))
    }

    fn empty(max_w: i32, max_h: i32) -> Self {
        Self {
            panel_w: max_w,
            panel_h: max_h,
            blocks: Vec::new(),
            monitors: Vec::new(),
            bbox: Rect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            scale: 0.0,
            offset_x: 0,
            offset_y: 0,
        }
    }

    #[cfg(test)]
    fn round_trip_tolerance_px(&self) -> i32 {
        if self.scale <= 0.0 {
            return i32::MAX;
        }
        (1.0 / self.scale).ceil() as i32
    }
}

fn delta(origin: i32, value: i32) -> i32 {
    (value as i64 - origin as i64).clamp(0, i32::MAX as i64) as i32
}

fn map_delta(delta_px: i32, scale: f64) -> i32 {
    if delta_px <= 0 {
        return 0;
    }
    let mapped = (delta_px as f64 * scale).floor();
    if mapped <= 0.0 {
        0
    } else if mapped >= i32::MAX as f64 {
        i32::MAX
    } else {
        mapped as i32
    }
}

fn union_rects(rects: impl Iterator<Item = Rect>) -> Rect {
    let mut acc: Option<Rect> = None;
    for rect in rects.filter(|rect| rect.is_valid()) {
        acc = Some(match acc {
            None => rect,
            Some(current) => Rect {
                left: current.left.min(rect.left),
                top: current.top.min(rect.top),
                right: current.right.max(rect.right),
                bottom: current.bottom.max(rect.bottom),
            },
        });
    }
    acc.unwrap_or(Rect {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    })
}

fn block_to_screen(block: Rect, monitor: Rect, panel: (i32, i32)) -> (i32, i32) {
    let block_w = block.width();
    let block_h = block.height();
    if block_w <= 0 || block_h <= 0 {
        return (monitor.left, monitor.top);
    }
    let dx = (panel.0 - block.left).clamp(0, block_w - 1) as i64;
    let dy = (panel.1 - block.top).clamp(0, block_h - 1) as i64;
    let sx = monitor.left + (dx * monitor.width() as i64 / block_w as i64) as i32;
    let sy = monitor.top + (dy * monitor.height() as i64 / block_h as i64) as i32;
    (
        sx.clamp(
            monitor.left,
            monitor.right.saturating_sub(1).max(monitor.left),
        ),
        sy.clamp(
            monitor.top,
            monitor.bottom.saturating_sub(1).max(monitor.top),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(left: i32, top: i32, right: i32, bottom: i32, is_primary: bool) -> Monitor {
        let rect = Rect {
            left,
            top,
            right,
            bottom,
        };
        Monitor {
            rect,
            work: rect,
            is_primary,
        }
    }

    fn fhd() -> Vec<Monitor> {
        vec![mon(0, 0, 1920, 1080, true)]
    }

    fn side_by_side() -> Vec<Monitor> {
        vec![mon(0, 0, 1920, 1080, true), mon(1920, 0, 3840, 1080, false)]
    }

    fn uhd_fhd_bottom() -> Vec<Monitor> {
        vec![
            mon(0, 0, 3840, 2160, true),
            mon(3840, 1080, 5760, 2160, false),
        ]
    }

    fn stacked() -> Vec<Monitor> {
        vec![mon(0, 0, 1920, 1080, true), mon(0, 1080, 1920, 2160, false)]
    }

    fn stepped() -> Vec<Monitor> {
        vec![
            mon(0, 0, 1920, 1080, true),
            mon(1920, 400, 3840, 1480, false),
        ]
    }

    fn l_shape() -> Vec<Monitor> {
        vec![
            mon(0, 0, 1920, 1080, true),
            mon(0, 1080, 1920, 2160, false),
            mon(1920, 1080, 3840, 2160, false),
        ]
    }

    fn portrait_mix() -> Vec<Monitor> {
        vec![mon(0, 0, 1080, 1920, true), mon(1080, 0, 3000, 1080, false)]
    }

    fn negative_left() -> Vec<Monitor> {
        vec![mon(0, 0, 1920, 1080, true), mon(-1920, 0, 0, 1080, false)]
    }

    fn layouts() -> Vec<(&'static str, Vec<Monitor>)> {
        vec![
            ("fhd", fhd()),
            ("side", side_by_side()),
            ("uhd_fhd", uhd_fhd_bottom()),
            ("stacked", stacked()),
            ("stepped", stepped()),
            ("l", l_shape()),
            ("portrait", portrait_mix()),
            ("negative", negative_left()),
        ]
    }

    fn assert_invariants(name: &str, monitors: &[Monitor], max_w: i32, max_h: i32, pad: i32) {
        let geo = MinimapGeometry::build(monitors, max_w, max_h, pad);
        assert_eq!(geo.blocks.len(), geo.monitors.len(), "{name} count");
        assert!(geo.monitors.first().unwrap().is_primary, "{name} primary");
        let tolerance = geo.round_trip_tolerance_px();

        for (index, block) in geo.blocks.iter().enumerate() {
            assert!(block.left >= 0 && block.top >= 0, "{name} origin");
            assert!(
                block.right <= geo.panel_w && block.bottom <= geo.panel_h,
                "{name} inside"
            );
            assert!(
                block.width() > 0 && block.height() > 0,
                "{name} area {index}"
            );
            for other in geo.blocks.iter().skip(index + 1) {
                let overlap = block.left < other.right
                    && other.left < block.right
                    && block.top < other.bottom
                    && other.top < block.bottom;
                assert!(!overlap, "{name} overlap {index}");
            }

            let monitor = geo.monitors[index].rect;
            let center = (
                monitor.left + monitor.width() / 2,
                monitor.top + monitor.height() / 2,
            );
            let panel = geo.screen_to_panel(center);
            assert!(
                block.contains(panel),
                "{name} center {index} {panel:?} {block:?}"
            );

            let mw = monitor.width() as f64;
            let mh = monitor.height() as f64;
            let aspect_error = (block.width() as f64 / mw - block.height() as f64 / mh).abs();
            assert!(aspect_error < 0.01, "{name} aspect {aspect_error}");

            for point in sample_points(monitor) {
                let panel = geo.screen_to_panel(point);
                let (hit_index, back) = geo.hit(panel).unwrap_or_else(|| {
                    panic!("{name} miss {point:?} -> {panel:?}");
                });
                assert_eq!(hit_index, index, "{name} index {point:?}");
                assert!(
                    (back.0 - point.0).abs() <= tolerance && (back.1 - point.1).abs() <= tolerance,
                    "{name} round trip {point:?} -> {back:?} tol {tolerance}"
                );
                assert!(monitor.contains(back), "{name} landing {back:?}");
            }
        }
    }

    fn sample_points(rect: Rect) -> Vec<(i32, i32)> {
        let mut points = vec![
            (rect.left, rect.top),
            (rect.right - 1, rect.top),
            (rect.left, rect.bottom - 1),
            (rect.right - 1, rect.bottom - 1),
            (rect.left + rect.width() / 2, rect.top + rect.height() / 2),
        ];
        let step_x = (rect.width() / 8).max(1);
        let step_y = (rect.height() / 8).max(1);
        let mut x = rect.left;
        while x < rect.right {
            let mut y = rect.top;
            while y < rect.bottom {
                points.push((x, y));
                y += step_y;
            }
            x += step_x;
        }
        points
    }

    #[test]
    fn layouts_keep_shape_containment_and_round_trip() {
        for (name, monitors) in layouts() {
            assert_invariants(name, &monitors, 180, 180, 8);
            assert_invariants(name, &monitors, 240, 120, 4);
        }
    }

    #[test]
    fn panel_limits_follow_the_arrangement_aspect() {
        let wide = side_by_side();
        assert_eq!(panel_limits(&wide, 280, 90), (280, 90));
        let tall = stacked();
        assert_eq!(panel_limits(&tall, 280, 90), (248, 280));
        let square = fhd();
        let (w, h) = panel_limits(&square, 280, 90);
        assert_eq!(w, 280);
        assert!(h < w);
        assert!(h >= 90);
        assert_eq!(panel_limits(&[], 280, 90), (280, 280));
    }

    #[test]
    fn horizontal_neighbors_share_an_edge() {
        let geo = MinimapGeometry::build(&side_by_side(), 180, 180, 8);
        assert_eq!(geo.blocks[0].right, geo.blocks[1].left);
        assert!(geo.blocks[0].width() > 0);
    }

    #[test]
    fn vertical_neighbors_share_an_edge() {
        let geo = MinimapGeometry::build(&stacked(), 180, 180, 8);
        assert_eq!(geo.blocks[0].bottom, geo.blocks[1].top);
    }

    #[test]
    fn bottom_aligned_4k_and_fhd_share_the_vertical_edge() {
        let geo = MinimapGeometry::build(&uhd_fhd_bottom(), 180, 180, 8);
        assert_eq!(geo.blocks[0].right, geo.blocks[1].left);
        assert_eq!(geo.blocks[0].bottom, geo.blocks[1].bottom);
        assert!(geo.blocks[1].top > geo.blocks[0].top);
    }

    #[test]
    fn l_shape_shares_only_the_touching_edges() {
        let geo = MinimapGeometry::build(&l_shape(), 180, 180, 8);
        assert_eq!(geo.blocks[0].bottom, geo.blocks[1].top);
        assert_eq!(geo.blocks[1].right, geo.blocks[2].left);
        assert!(
            geo.blocks[0].right < geo.blocks[2].left || geo.blocks[0].bottom <= geo.blocks[2].top
        );
    }

    #[test]
    fn primary_stays_first_when_it_sits_on_the_right() {
        let monitors = vec![mon(0, 0, 1920, 1080, false), mon(1920, 0, 3840, 1080, true)];
        let geo = MinimapGeometry::build(&monitors, 180, 180, 8);
        assert!(geo.monitors[0].is_primary);
        assert_eq!(geo.monitors[0].rect.left, 1920);
        let center = geo.screen_to_panel((2880, 540));
        assert_eq!(geo.hit(center).unwrap().0, 0);
    }

    #[test]
    fn gap_above_the_lower_monitor_is_not_a_target() {
        let geo = MinimapGeometry::build(&uhd_fhd_bottom(), 180, 180, 8);
        let gap = geo.screen_to_panel((4800, 200));
        assert!(geo.hit(gap).is_none());
        assert!(geo.hit((0, 0)).is_none());
        assert!(geo.hit((geo.panel_w, geo.panel_h)).is_none());
    }

    #[test]
    fn empty_and_tiny_panels_do_not_panic() {
        let geo = MinimapGeometry::build(&[], 180, 180, 8);
        assert!(geo.blocks.is_empty());
        assert!(geo.hit((10, 10)).is_none());
        assert_eq!(geo.screen_to_panel((10, 10)), (0, 0));

        let tiny = MinimapGeometry::build(&fhd(), 4, 4, 4);
        assert!(tiny.blocks.is_empty());
        assert!(tiny.hit((1, 1)).is_none());
    }

    #[test]
    fn clamp_keeps_points_inside_and_pulls_gaps_onto_a_monitor() {
        let monitors = uhd_fhd_bottom();
        assert_eq!(clamp_to_monitors((100, 100), &monitors), (100, 100));
        assert_eq!(clamp_to_monitors((3840, 1500), &monitors), (3840, 1500));
        assert_eq!(clamp_to_monitors((3839, 100), &monitors), (3839, 100));
        assert_eq!(clamp_to_monitors((4800, 360), &monitors), (4800, 1080));
        assert_eq!(clamp_to_monitors((10, 10), &[]), (10, 10));
    }

    #[test]
    fn clamp_ignores_invalid_rects_and_breaks_distance_ties_toward_primary() {
        let invalid = Monitor {
            rect: Rect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 10,
            },
            work: Rect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 10,
            },
            is_primary: true,
        };
        let only = mon(100, 100, 110, 110, false);
        assert_eq!(clamp_to_monitors((0, 0), &[invalid, only]), (100, 100));

        let primary = mon(0, 0, 10, 10, true);
        let other = mon(0, 21, 10, 31, false);
        assert_eq!(clamp_to_monitors((5, 15), &[other, primary]), (5, 9));

        let primary_below = mon(0, 21, 10, 31, true);
        let above = mon(0, 0, 10, 10, false);
        assert_eq!(clamp_to_monitors((5, 15), &[above, primary_below]), (5, 21));
    }
}
