//! サークル・ジェスチャ検知（純 Rust / Win32 非依存）

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionSample {
    pub dx: f32,
    pub dy: f32,
    pub dt_millis: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DetectorConfig {
    /// 角度・半径を評価するスライディング時間窓（ミリ秒）
    pub window_ms: u64,
    /// 窓内の符号付き角度増分の総和（度）の下限
    pub min_total_angle_deg: f32,
    /// 始点→終点ベクトルの位相ずれ（度）の下限（閉曲線性）
    pub min_net_angle_deg: f32,
    /// 窓内経路の RMS 半径（抽象ピクセル）の下限
    pub min_rms_radius: f32,
    pub max_rms_radius: f32,
    /// 許容する楕円の長径/短径比の上限
    pub ellipse_major_over_minor_max: f32,
    /// 230〜280° の短い親指ストロークを、緩い閉合で成立させる（実機向け）
    pub allow_thumb_partial_closure: bool,
    /// 始点–終点距離の許容: `disp <= max(rms * factor, min_px)`
    pub closure_rms_factor: f32,
    pub closure_disp_min_px: f32,
    /// 最終→最初の Δθ は、始点–終点がほぼ一致したときだけ加算
    pub closure_angle_disp_max_px: f32,
}

impl Default for DetectorConfig {
    fn default() -> Self {
        Self {
            window_ms: 550,
            min_total_angle_deg: 230.0,
            min_net_angle_deg: 200.0,
            min_rms_radius: 8.0,
            max_rms_radius: 2000.0,
            ellipse_major_over_minor_max: 3.6,
            allow_thumb_partial_closure: false,
            closure_rms_factor: 0.65,
            closure_disp_min_px: 10.0,
            closure_angle_disp_max_px: 2.5,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TimedDelta {
    pub dx: f32,
    pub dy: f32,
    pub end_time_ms: u64,
}

pub struct CircleDetector {
    config: DetectorConfig,
    is_dragging: bool,
    virtual_time_ms: u64,
    samples: Vec<TimedDelta>,
    last_detection: Option<GestureWindowStats>,
}

/// 現在のスライディング窓の計測値（PoC デバッグ・表示用）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GestureWindowStats {
    pub elapsed_ms: u64,
    pub signed_angle_deg: f32,
    pub net_angle_deg: f32,
}

impl CircleDetector {
    pub fn new(config: DetectorConfig) -> Self {
        Self {
            config,
            is_dragging: false,
            virtual_time_ms: 0,
            samples: Vec::new(),
            last_detection: None,
        }
    }

    pub fn set_drag_state(&mut self, is_dragging: bool) {
        if is_dragging {
            self.clear_window();
        }
        self.is_dragging = is_dragging;
    }

    /// サークル完成と判定したサンプルでのみ `true`。完成後は内部で [`Self::reset`] 相当。
    pub fn push(&mut self, sample: MotionSample) -> bool {
        if self.is_dragging {
            return false;
        }

        self.virtual_time_ms = self.virtual_time_ms.saturating_add(sample.dt_millis);
        self.samples.push(TimedDelta {
            dx: sample.dx,
            dy: sample.dy,
            end_time_ms: self.virtual_time_ms,
        });
        self.purge_old_samples();

        if self.evaluate_window() {
            self.last_detection = Some(self.window_stats());
            self.clear_window();
            return true;
        }

        false
    }

    /// 直近の成功検知時に記録された窓統計（`push` が `true` を返した直後のみ有効）。
    pub fn last_detection_stats(&self) -> Option<GestureWindowStats> {
        self.last_detection
    }

    /// 現在窓内の累積角度と、最古〜最新サンプル間の時間幅（≤ `window_ms`）。
    pub fn window_stats(&self) -> GestureWindowStats {
        let signed_angle_deg = self.cumulative_signed_angle_deg();
        GestureWindowStats {
            elapsed_ms: self.window_span_ms(),
            signed_angle_deg,
            net_angle_deg: signed_angle_deg.abs(),
        }
    }

    /// 180° 超の弧でサークル不成立のとき、最初に満たさない条件（デバッグ用）。
    pub fn circle_reject_reason(&self) -> Option<&'static str> {
        self.evaluate_window_failure()
    }

    pub fn reset(&mut self) {
        self.last_detection = None;
        self.clear_window();
    }

    fn clear_window(&mut self) {
        self.virtual_time_ms = 0;
        self.samples.clear();
    }

    fn purge_old_samples(&mut self) {
        let cutoff = self
            .virtual_time_ms
            .saturating_sub(self.config.window_ms);
        self.samples.retain(|s| s.end_time_ms > cutoff);
    }

    /// スライディング窓内に残っているサンプルの時間幅（最古の end_time 〜 最新の end_time）。
    fn window_span_ms(&self) -> u64 {
        match (self.samples.first(), self.samples.last()) {
            (Some(first), Some(last)) if !self.samples.is_empty() => {
                last.end_time_ms.saturating_sub(first.end_time_ms)
            }
            _ => 0,
        }
    }

    fn evaluate_window(&self) -> bool {
        self.evaluate_window_failure().is_none()
    }

    fn evaluate_window_failure(&self) -> Option<&'static str> {
        let n = self.samples.len();
        if n < 3 {
            return Some("too_few_samples");
        }

        let signed_angle_deg = self.cumulative_signed_angle_deg();
        let net_angle_deg = signed_angle_deg.abs();
        if net_angle_deg < self.config.min_net_angle_deg {
            return Some("net_angle");
        }
        if net_angle_deg < self.config.min_total_angle_deg {
            return Some("total_angle");
        }

        let positions = self.cumulative_positions();
        let rms = rms_radius(&positions);
        if rms < self.config.min_rms_radius {
            return Some("rms_low");
        }
        if rms > self.config.max_rms_radius {
            return Some("rms_high");
        }

        let aspect = ellipse_aspect_ratio(&positions);
        if aspect > self.config.ellipse_major_over_minor_max {
            return Some("aspect");
        }

        let (disp_x, disp_y) = net_displacement_from(&self.samples);
        let disp = disp_x.hypot(disp_y);
        if !Self::closure_satisfied(
            net_angle_deg,
            rms,
            disp,
            self.config.min_total_angle_deg,
            self.config.allow_thumb_partial_closure,
            self.config.closure_rms_factor,
            self.config.closure_disp_min_px,
        ) {
            return Some("not_closed");
        }

        None
    }

    /// 350°〜: 閉合許容内なら成立。230〜280° は `allow_thumb_partial_closure` 時のみ同様。
    fn closure_satisfied(
        net_angle_deg: f32,
        rms: f32,
        disp: f32,
        min_total_angle_deg: f32,
        allow_thumb_partial_closure: bool,
        closure_rms_factor: f32,
        closure_disp_min_px: f32,
    ) -> bool {
        let limit = closure_displacement_limit(rms, closure_rms_factor, closure_disp_min_px);
        if allow_thumb_partial_closure
            && net_angle_deg >= min_total_angle_deg
            && net_angle_deg < 280.0
        {
            return disp <= limit;
        }
        if (280.0..350.0).contains(&net_angle_deg) {
            return false;
        }
        if net_angle_deg >= 350.0 {
            return disp <= limit;
        }
        false
    }

    fn cumulative_signed_angle_deg(&self) -> f32 {
        signed_angle_deg_from_deltas(&self.samples, self.config.closure_angle_disp_max_px)
    }

    fn cumulative_positions(&self) -> Vec<(f32, f32)> {
        cumulative_positions_from(&self.samples)
    }
}

const MIN_VECTOR_LEN_SQ: f32 = 1e-4;

pub(crate) fn closure_displacement_limit(rms: f32, factor: f32, min_px: f32) -> f32 {
    (rms * factor).max(min_px)
}

pub(crate) fn closure_angle_addition_allowed(disp: f32, max_px: f32) -> bool {
    disp <= max_px
}

pub(crate) fn signed_angle_deg_from_deltas(
    samples: &[TimedDelta],
    closure_angle_disp_max_px: f32,
) -> f32 {
    let mut total_rad = 0.0_f32;
    for i in 1..samples.len() {
        let a = samples[i - 1];
        let b = samples[i];
        total_rad += signed_angle_rad(a.dx, a.dy, b.dx, b.dy);
    }
    if samples.len() >= 3 {
        let (dx, dy) = net_displacement_from(samples);
        let disp = dx.hypot(dy);
        if closure_angle_addition_allowed(disp, closure_angle_disp_max_px) {
            let last = samples[samples.len() - 1];
            let first = samples[0];
            total_rad += signed_angle_rad(last.dx, last.dy, first.dx, first.dy);
        }
    }
    total_rad.to_degrees()
}

pub(crate) fn unsigned_angle_deg_from_deltas(samples: &[TimedDelta]) -> f32 {
    let mut total_rad = 0.0_f32;
    for i in 1..samples.len() {
        let a = samples[i - 1];
        let b = samples[i];
        total_rad += signed_angle_rad(a.dx, a.dy, b.dx, b.dy).abs();
    }
    total_rad.to_degrees()
}

pub(crate) fn cumulative_positions_from(samples: &[TimedDelta]) -> Vec<(f32, f32)> {
    let mut positions = Vec::with_capacity(samples.len());
    let mut x = 0.0_f32;
    let mut y = 0.0_f32;
    for s in samples {
        x += s.dx;
        y += s.dy;
        positions.push((x, y));
    }
    positions
}

fn signed_angle_rad(ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    if ax * ax + ay * ay < MIN_VECTOR_LEN_SQ || bx * bx + by * by < MIN_VECTOR_LEN_SQ {
        return 0.0;
    }
    let cross = ax * by - ay * bx;
    let dot = ax * bx + ay * by;
    cross.atan2(dot)
}

pub(crate) fn net_displacement_from(samples: &[TimedDelta]) -> (f32, f32) {
    let mut x = 0.0_f32;
    let mut y = 0.0_f32;
    for s in samples {
        x += s.dx;
        y += s.dy;
    }
    (x, y)
}

pub(crate) fn rms_radius(positions: &[(f32, f32)]) -> f32 {
    if positions.is_empty() {
        return 0.0;
    }
    let n = positions.len() as f32;
    let cx = positions.iter().map(|p| p.0).sum::<f32>() / n;
    let cy = positions.iter().map(|p| p.1).sum::<f32>() / n;
    let mean_sq = positions
        .iter()
        .map(|(x, y)| {
            let dx = x - cx;
            let dy = y - cy;
            dx * dx + dy * dy
        })
        .sum::<f32>()
        / n;
    mean_sq.sqrt()
}

pub(crate) fn ellipse_aspect_ratio(positions: &[(f32, f32)]) -> f32 {
    if positions.len() < 3 {
        return f32::INFINITY;
    }
    let n = positions.len() as f32;
    let cx = positions.iter().map(|p| p.0).sum::<f32>() / n;
    let cy = positions.iter().map(|p| p.1).sum::<f32>() / n;

    let mut xx = 0.0_f32;
    let mut yy = 0.0_f32;
    let mut xy = 0.0_f32;
    for (x, y) in positions {
        let dx = x - cx;
        let dy = y - cy;
        xx += dx * dx;
        yy += dy * dy;
        xy += dx * dy;
    }
    xx /= n;
    yy /= n;
    xy /= n;

    let trace = xx + yy;
    let det = xx * yy - xy * xy;
    let half_trace = trace * 0.5;
    let disc = (half_trace * half_trace - det).max(0.0);
    let lambda1 = half_trace + disc.sqrt();
    let lambda2 = half_trace - disc.sqrt();
    if lambda2 <= 1e-6 {
        return f32::INFINITY;
    }
    (lambda1 / lambda2).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_detector() -> CircleDetector {
        CircleDetector::new(DetectorConfig::default())
    }

    /// 等速円（楕円）軌道の相対移動列。+x 右、+y 下。θ 増加で画面上時計回り。
    fn circle_deltas(
        r: f32,
        steps: u32,
        total_ms: u64,
        clockwise: bool,
        aspect_y: f32,
    ) -> Vec<MotionSample> {
        assert!(steps >= 3, "steps must allow a closed curve");
        let dt = total_ms / steps as u64;
        let dt = dt.max(1);

        let pos = |theta: f32| -> (f32, f32) {
            (r * theta.cos(), (r / aspect_y) * theta.sin())
        };

        let mut samples = Vec::with_capacity(steps as usize);
        for i in 0..steps {
            let t0 = std::f32::consts::TAU * i as f32 / steps as f32;
            let t1 = std::f32::consts::TAU * (i + 1) as f32 / steps as f32;
            let (t0, t1) = if clockwise { (t0, t1) } else { (-t0, -t1) };
            let (x0, y0) = pos(t0);
            let (x1, y1) = pos(t1);
            samples.push(MotionSample {
                dx: x1 - x0,
                dy: y1 - y0,
                dt_millis: dt,
            });
        }
        samples
    }

    fn line_deltas(dx: f32, dy: f32, steps: u32, total_ms: u64) -> Vec<MotionSample> {
        let dt = (total_ms / steps as u64).max(1);
        (0..steps)
            .map(|_| MotionSample {
                dx,
                dy,
                dt_millis: dt,
            })
            .collect()
    }

    fn push_all(det: &mut CircleDetector, samples: &[MotionSample]) -> Vec<bool> {
        samples.iter().map(|s| det.push(*s)).collect()
    }

    fn assert_only_last_true(results: &[bool]) {
        assert!(!results.is_empty(), "need at least one sample");
        for (i, &fired) in results.iter().enumerate() {
            if i + 1 == results.len() {
                assert!(fired, "expected true only on the final sample");
            } else {
                assert!(!fired, "unexpected true at index {i}");
            }
        }
    }

    fn assert_all_false(results: &[bool]) {
        for (i, &fired) in results.iter().enumerate() {
            assert!(!fired, "expected false at index {i}, got true");
        }
    }

    // --- 正常系 ---

    #[test]
    fn test_clockwise_circle() {
        let mut det = default_detector();
        let samples = circle_deltas(40.0, 16, 320, true, 1.0);
        let results = push_all(&mut det, &samples);
        assert_only_last_true(&results);
    }

    #[test]
    fn test_counter_clockwise_circle() {
        let mut det = default_detector();
        let samples = circle_deltas(40.0, 16, 320, false, 1.0);
        let results = push_all(&mut det, &samples);
        assert_only_last_true(&results);
    }

    #[test]
    fn test_elliptical_circle() {
        let mut det = default_detector();
        let samples = circle_deltas(40.0, 16, 320, true, 2.0);
        let results = push_all(&mut det, &samples);
        assert_only_last_true(&results);
    }

    #[test]
    fn test_low_polling_rate_sparse_points() {
        let mut det = default_detector();
        let samples = circle_deltas(40.0, 15, 300, true, 1.0);
        let results = push_all(&mut det, &samples);
        assert_only_last_true(&results);
    }

    // --- 異常系 ---

    #[test]
    fn test_linear_swipe() {
        let mut det = default_detector();
        let samples = line_deltas(25.0, 0.0, 20, 200);
        let results = push_all(&mut det, &samples);
        assert_all_false(&results);
    }

    #[test]
    fn test_zigzag_motion() {
        let mut det = default_detector();
        let mut samples = Vec::new();
        for _ in 0..10 {
            samples.push(MotionSample {
                dx: 15.0,
                dy: 0.0,
                dt_millis: 15,
            });
            samples.push(MotionSample {
                dx: -15.0,
                dy: 0.0,
                dt_millis: 15,
            });
        }
        let results = push_all(&mut det, &samples);
        assert_all_false(&results);
    }

    #[test]
    fn test_drag_suppression() {
        let mut det = default_detector();
        let samples = circle_deltas(40.0, 16, 320, true, 1.0);
        let split = samples.len() - 1;

        let mut results = Vec::new();
        for s in &samples[..split] {
            results.push(det.push(*s));
        }
        det.set_drag_state(true);
        for s in &samples[split..] {
            results.push(det.push(*s));
        }
        for _ in 0..samples.len() {
            results.push(det.push(samples[0]));
        }
        assert_all_false(&results);
    }

    #[test]
    fn test_too_slow() {
        let mut det = default_detector();
        let samples = circle_deltas(40.0, 16, 1000, true, 1.0);
        let results = push_all(&mut det, &samples);
        assert_all_false(&results);
    }

    #[test]
    fn test_too_small_jitter() {
        let mut det = default_detector();
        const JITTER: [(f32, f32); 32] = [
            (0.6, 0.2),
            (-0.4, 0.5),
            (0.3, -0.7),
            (-0.8, -0.1),
            (0.2, 0.4),
            (-0.5, 0.3),
            (0.7, -0.2),
            (-0.1, -0.6),
            (0.4, 0.1),
            (-0.3, 0.8),
            (0.5, -0.4),
            (-0.6, 0.0),
            (0.1, -0.5),
            (-0.2, 0.6),
            (0.8, 0.3),
            (-0.7, -0.3),
            (0.0, 0.5),
            (-0.5, -0.4),
            (0.3, 0.2),
            (-0.4, -0.8),
            (0.6, -0.1),
            (-0.2, 0.4),
            (0.4, -0.6),
            (-0.8, 0.2),
            (0.1, 0.7),
            (-0.3, -0.2),
            (0.5, 0.0),
            (-0.6, 0.5),
            (0.2, -0.3),
            (-0.1, 0.4),
            (0.7, -0.5),
            (-0.4, 0.1),
        ];
        let samples: Vec<MotionSample> = JITTER
            .iter()
            .map(|(dx, dy)| MotionSample {
                dx: *dx,
                dy: *dy,
                dt_millis: 300 / 32,
            })
            .collect();
        let results = push_all(&mut det, &samples);
        assert_all_false(&results);
    }

    // --- 追加（境界・契約） ---

    #[test]
    fn test_partial_arc_270deg() {
        let mut det = default_detector();
        let full_steps = 16u32;
        let full = circle_deltas(40.0, full_steps, 280, true, 1.0);
        let arc_len = (full.len() as f32 * 270.0 / 360.0).round() as usize;
        let samples = &full[..arc_len];
        let results = push_all(&mut det, samples);
        assert_all_false(&results);
    }

    #[test]
    fn test_circle_then_immediate_second() {
        let mut det = default_detector();
        let samples = circle_deltas(40.0, 16, 320, true, 1.0);

        let first = push_all(&mut det, &samples);
        assert_only_last_true(&first);

        let second = push_all(&mut det, &samples);
        assert_only_last_true(&second);
    }

    #[test]
    fn test_drag_start_clears_progress() {
        let mut det = default_detector();
        let full = circle_deltas(40.0, 16, 320, true, 1.0);
        let partial_len = (full.len() as f32 * 150.0 / 360.0).round() as usize;

        for s in &full[..partial_len] {
            assert!(!det.push(*s));
        }
        det.set_drag_state(true);
        det.set_drag_state(false);

        let results = push_all(&mut det, &full);
        assert_only_last_true(&results);
    }

    #[test]
    fn test_window_sliding() {
        let mut det = default_detector();
        // 1周 900ms: 終了時点の 550ms 窓には約 220° 分しか入らない（230° 閾値未満）
        let samples = circle_deltas(40.0, 16, 900, true, 1.0);
        let results = push_all(&mut det, &samples);
        assert_all_false(&results);
    }
}
