//! ストローク解析と推奨検知設定（純 Rust / OS 非依存）

use super::circle::{
    cumulative_positions_from, ellipse_aspect_ratio, net_displacement_from, rms_radius,
    signed_angle_deg_from_deltas, unsigned_angle_deg_from_deltas, DetectorConfig, TimedDelta,
};

const MIN_SAMPLES: usize = 3;
const MIN_STROKES: usize = 3;
const WINDOW_MARGIN: f32 = 1.35;
const WINDOW_MIN_MS: u64 = 400;
const WINDOW_MAX_MS: u64 = 900;
const ANGLE_MIN_FACTOR: f32 = 0.88;
const ANGLE_FLOOR_DEG: f32 = 180.0;
const NET_ANGLE_SLACK_DEG: f32 = 20.0;
const NET_ANGLE_FLOOR_DEG: f32 = 160.0;
const ASPECT_MARGIN: f32 = 1.20;
const ASPECT_MIN: f32 = 2.4;
const ASPECT_MAX: f32 = 5.0;
const RMS_MARGIN: f32 = 0.65;
const RMS_FLOOR: f32 = 5.0;
const CLOSURE_MARGIN: f32 = 1.15;
const CLOSURE_FACTOR_MIN: f32 = 0.55;
const CLOSURE_FACTOR_MAX: f32 = 1.10;
const THUMB_PARTIAL_ANGLE_DEG: f32 = 280.0;
const MAX_ANGLE_CV: f32 = 0.30;
const MAX_ELAPSED_CV: f32 = 0.45;
const DIRECTION_EPS_DEG: f32 = 8.0;
/// キャリブレーションでストローク確定とみなす無入力時間。
pub const CALIBRATION_STROKE_IDLE_MS: u64 = 350;
/// キャンバス進入後、Ready に入るための静止時間。
pub const CALIBRATION_READY_STILL_MS: u64 = 200;
/// 1 サンプルあたりの移動量がこれを超えたら「有意な移動」。
pub const CALIBRATION_SIGNIFICANT_MOVE: f32 = 3.0;
const USABLE_MIN_NET_ANGLE_DEG: f32 = 120.0;
const USABLE_MIN_RMS: f32 = 8.0;
const USABLE_MIN_ELAPSED_MS: u64 = 80;
const USABLE_MAX_ELAPSED_MS: u64 = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationDirection {
    Clockwise,
    CounterClockwise,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrokeMetrics {
    pub elapsed_ms: u64,
    pub total_angle_deg: f32,
    pub net_angle_deg: f32,
    pub signed_angle_deg: f32,
    pub rms_radius: f32,
    pub aspect_ratio: f32,
    pub closure_disp_px: f32,
    pub closure_ratio: f32,
    pub direction: RotationDirection,
}

/// 相対移動 `(dx, dy, timestamp_ms)` からストローク指標を求める。
pub fn analyze_stroke(samples: &[(i32, i32, u64)]) -> Option<StrokeMetrics> {
    let floats: Vec<(f32, f32, u64)> = samples
        .iter()
        .map(|(dx, dy, t)| (*dx as f32, *dy as f32, *t))
        .collect();
    analyze_relative_stroke(&floats)
}

/// Raw Input などサブピクセル精度の相対列向け。
pub fn analyze_relative_stroke(samples: &[(f32, f32, u64)]) -> Option<StrokeMetrics> {
    if samples.len() < MIN_SAMPLES {
        return None;
    }
    let first_t = samples.first()?.2;
    let last_t = samples.last()?.2;
    if last_t < first_t {
        return None;
    }
    let moved = samples.iter().any(|(dx, dy, _)| dx.abs() + dy.abs() > 0.0);
    if !moved {
        return None;
    }

    let defaults = DetectorConfig::default();
    let deltas: Vec<TimedDelta> = samples
        .iter()
        .map(|(dx, dy, t)| TimedDelta {
            dx: *dx,
            dy: *dy,
            end_time_ms: t.saturating_sub(first_t),
        })
        .collect();

    let signed_angle_deg =
        signed_angle_deg_from_deltas(&deltas, defaults.closure_angle_disp_max_px);
    let total_angle_deg = unsigned_angle_deg_from_deltas(&deltas);
    let net_angle_deg = signed_angle_deg.abs();
    if net_angle_deg < DIRECTION_EPS_DEG && total_angle_deg < DIRECTION_EPS_DEG {
        return None;
    }

    let positions = cumulative_positions_from(&deltas);
    let rms = rms_radius(&positions);
    if !rms.is_finite() || rms <= 0.0 {
        return None;
    }
    let aspect = ellipse_aspect_ratio(&positions);
    if !aspect.is_finite() {
        return None;
    }
    let (disp_x, disp_y) = net_displacement_from(&deltas);
    let disp = disp_x.hypot(disp_y);
    let direction = if signed_angle_deg >= 0.0 {
        RotationDirection::Clockwise
    } else {
        RotationDirection::CounterClockwise
    };

    Some(StrokeMetrics {
        elapsed_ms: last_t.saturating_sub(first_t),
        total_angle_deg,
        net_angle_deg,
        signed_angle_deg,
        rms_radius: rms,
        aspect_ratio: aspect,
        closure_disp_px: disp,
        closure_ratio: disp / rms,
        direction,
    })
}

/// 微小ジッターや直線スワイプをキャリブレーション標本から除外する。
pub fn is_usable_calibration_stroke(metrics: &StrokeMetrics) -> bool {
    metrics.net_angle_deg >= USABLE_MIN_NET_ANGLE_DEG
        && metrics.rms_radius >= USABLE_MIN_RMS
        && metrics.elapsed_ms >= USABLE_MIN_ELAPSED_MS
        && metrics.elapsed_ms <= USABLE_MAX_ELAPSED_MS
}

pub fn should_finalize_idle_stroke(idle_ms: u64, point_count: usize) -> bool {
    point_count >= MIN_SAMPLES && idle_ms >= CALIBRATION_STROKE_IDLE_MS
}

pub fn motion_is_significant(dx: f32, dy: f32) -> bool {
    dx.hypot(dy) > CALIBRATION_SIGNIFICANT_MOVE
}

/// バウンディングボックス中心が原点になるよう平行移動する。
pub fn center_path_on_bounds(path: &[(f32, f32)]) -> Vec<(f32, f32)> {
    if path.is_empty() {
        return Vec::new();
    }
    let mut min_x = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for &(x, y) in path {
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    let cx = (min_x + max_x) * 0.5;
    let cy = (min_y + max_y) * 0.5;
    path.iter().map(|&(x, y)| (x - cx, y - cy)).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalibratePhase {
    Outside,
    Positioning,
    Ready,
    Recording,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalibrateGateAction {
    Ignore,
    /// バッファを空にしてからこの移動を Recording に入れる。
    StartRecording,
    Record,
    Finalize,
}

/// キャンバス内外と静止待機を管理する純 Rust ゲート。
#[derive(Debug, Clone)]
pub struct CalibrateGate {
    pub phase: CalibratePhase,
    last_significant_at: Option<std::time::Instant>,
    last_motion_at: Option<std::time::Instant>,
}

impl Default for CalibrateGate {
    fn default() -> Self {
        Self {
            phase: CalibratePhase::Outside,
            last_significant_at: None,
            last_motion_at: None,
        }
    }
}

impl CalibrateGate {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn note_outside(&mut self) {
        self.phase = CalibratePhase::Outside;
        self.last_significant_at = None;
        self.last_motion_at = None;
    }

    /// カーソルがキャンバス内。`motion` は Raw Input の `(dx, dy)`。ティックは `None`。
    pub fn note_inside(
        &mut self,
        now: std::time::Instant,
        motion: Option<(f32, f32)>,
    ) -> CalibrateGateAction {
        if self.phase == CalibratePhase::Outside {
            self.phase = CalibratePhase::Positioning;
            self.last_significant_at = Some(now);
            self.last_motion_at = Some(now);
            return CalibrateGateAction::Ignore;
        }

        let any_move = motion.is_some_and(|(dx, dy)| dx != 0.0 || dy != 0.0);
        let significant = motion.is_some_and(|(dx, dy)| motion_is_significant(dx, dy));
        if significant {
            self.last_significant_at = Some(now);
        }
        if any_move {
            self.last_motion_at = Some(now);
        }

        match self.phase {
            CalibratePhase::Outside => CalibrateGateAction::Ignore,
            CalibratePhase::Positioning => {
                if self.still_ms(now) >= CALIBRATION_READY_STILL_MS {
                    self.phase = CalibratePhase::Ready;
                }
                CalibrateGateAction::Ignore
            }
            CalibratePhase::Ready => {
                if any_move {
                    self.phase = CalibratePhase::Recording;
                    return CalibrateGateAction::StartRecording;
                }
                CalibrateGateAction::Ignore
            }
            CalibratePhase::Recording => {
                if any_move {
                    return CalibrateGateAction::Record;
                }
                if self.idle_ms(now) >= CALIBRATION_STROKE_IDLE_MS {
                    self.phase = CalibratePhase::Positioning;
                    self.last_significant_at = Some(now);
                    self.last_motion_at = None;
                    return CalibrateGateAction::Finalize;
                }
                CalibrateGateAction::Ignore
            }
        }
    }

    fn still_ms(&self, now: std::time::Instant) -> u64 {
        self.last_significant_at
            .map(|t| now.saturating_duration_since(t).as_millis() as u64)
            .unwrap_or(0)
    }

    fn idle_ms(&self, now: std::time::Instant) -> u64 {
        self.last_motion_at
            .map(|t| now.saturating_duration_since(t).as_millis() as u64)
            .unwrap_or(0)
    }
}

/// 3 本以上の有効ストロークから推奨 `DetectorConfig` を導出する（上限なし）。
pub fn recommend_detector_config(strokes: &[StrokeMetrics]) -> Result<DetectorConfig, &'static str> {
    if strokes.len() < MIN_STROKES {
        return Err("too_few_strokes");
    }

    let direction = strokes[0].direction;
    if strokes.iter().any(|s| s.direction != direction) {
        return Err("mixed_direction");
    }

    let filtered = reject_angle_outliers(strokes);
    if filtered.len() < MIN_STROKES {
        return Err("too_few_after_outliers");
    }

    let angles: Vec<f32> = filtered.iter().map(|s| s.net_angle_deg).collect();
    let elapsed: Vec<f32> = filtered.iter().map(|s| s.elapsed_ms as f32).collect();
    if coeff_var(&angles) > MAX_ANGLE_CV || coeff_var(&elapsed) > MAX_ELAPSED_CV {
        return Err("variance_too_high");
    }

    let max_elapsed = filtered
        .iter()
        .map(|s| s.elapsed_ms)
        .max()
        .unwrap_or(0);
    let window_ms = ((max_elapsed as f32 * WINDOW_MARGIN).round() as u64)
        .clamp(WINDOW_MIN_MS, WINDOW_MAX_MS);

    let min_angle = min_f32(&angles);
    let mean_angle = mean_f32(&angles);
    let std_angle = sample_std(&angles);
    let min_total_angle_deg = (min_angle * ANGLE_MIN_FACTOR)
        .min(mean_angle - std_angle)
        .max(ANGLE_FLOOR_DEG);
    let min_net_angle_deg = (min_total_angle_deg - NET_ANGLE_SLACK_DEG).max(NET_ANGLE_FLOOR_DEG);

    let max_aspect = filtered
        .iter()
        .map(|s| s.aspect_ratio)
        .fold(0.0_f32, f32::max);
    let ellipse_major_over_minor_max = (max_aspect * ASPECT_MARGIN).clamp(ASPECT_MIN, ASPECT_MAX);

    let min_rms = filtered
        .iter()
        .map(|s| s.rms_radius)
        .fold(f32::INFINITY, f32::min);
    let min_rms_radius = (min_rms * RMS_MARGIN).max(RMS_FLOOR);

    let max_closure = filtered
        .iter()
        .map(|s| s.closure_ratio)
        .fold(0.0_f32, f32::max);
    let closure_rms_factor = (max_closure * CLOSURE_MARGIN).clamp(CLOSURE_FACTOR_MIN, CLOSURE_FACTOR_MAX);

    let allow_thumb_partial_closure = min_angle < THUMB_PARTIAL_ANGLE_DEG;

    Ok(DetectorConfig {
        window_ms,
        min_total_angle_deg,
        min_net_angle_deg,
        min_rms_radius,
        max_rms_radius: DetectorConfig::default().max_rms_radius,
        ellipse_major_over_minor_max,
        allow_thumb_partial_closure,
        closure_rms_factor,
        closure_disp_min_px: DetectorConfig::default().closure_disp_min_px,
        closure_angle_disp_max_px: DetectorConfig::default().closure_angle_disp_max_px,
    })
}

fn reject_angle_outliers(strokes: &[StrokeMetrics]) -> Vec<StrokeMetrics> {
    let mut angles: Vec<f32> = strokes.iter().map(|s| s.net_angle_deg).collect();
    angles.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let (lo, hi) = iqr_fences(&angles);
    strokes
        .iter()
        .copied()
        .filter(|s| s.net_angle_deg >= lo && s.net_angle_deg <= hi)
        .collect()
}

fn iqr_fences(sorted: &[f32]) -> (f32, f32) {
    if sorted.len() < 4 {
        return (f32::NEG_INFINITY, f32::INFINITY);
    }
    let q1 = percentile_sorted(sorted, 0.25);
    let q3 = percentile_sorted(sorted, 0.75);
    let iqr = q3 - q1;
    (q1 - 1.5 * iqr, q3 + 1.5 * iqr)
}

fn percentile_sorted(sorted: &[f32], p: f32) -> f32 {
    let n = sorted.len();
    if n == 0 {
        return 0.0;
    }
    let idx = p * (n.saturating_sub(1) as f32);
    let lo = idx.floor() as usize;
    let hi = idx.ceil() as usize;
    if lo == hi {
        return sorted[lo];
    }
    let t = idx - lo as f32;
    sorted[lo] * (1.0 - t) + sorted[hi] * t
}

fn mean_f32(xs: &[f32]) -> f32 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.iter().sum::<f32>() / xs.len() as f32
}

fn min_f32(xs: &[f32]) -> f32 {
    xs.iter().copied().fold(f32::INFINITY, f32::min)
}

fn sample_std(xs: &[f32]) -> f32 {
    let n = xs.len();
    if n < 2 {
        return 0.0;
    }
    let mean = mean_f32(xs);
    let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / (n - 1) as f32;
    var.max(0.0).sqrt()
}

fn coeff_var(xs: &[f32]) -> f32 {
    let mean = mean_f32(xs).abs();
    if mean <= f32::EPSILON {
        return 0.0;
    }
    sample_std(xs) / mean
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    fn arc_samples(
        r: f32,
        steps: u32,
        total_ms: u64,
        clockwise: bool,
        aspect_y: f32,
        turns: f32,
    ) -> Vec<(f32, f32, u64)> {
        assert!(steps >= 3);
        let pos = |theta: f32| (r * theta.cos(), (r / aspect_y) * theta.sin());
        let mut out = Vec::with_capacity(steps as usize);
        let denom = (steps - 1) as u64;
        for i in 0..steps {
            let t0 = std::f32::consts::TAU * turns * i as f32 / steps as f32;
            let t1 = std::f32::consts::TAU * turns * (i + 1) as f32 / steps as f32;
            let (t0, t1) = if clockwise { (t0, t1) } else { (-t0, -t1) };
            let (x0, y0) = pos(t0);
            let (x1, y1) = pos(t1);
            let t = total_ms.saturating_mul(i as u64) / denom;
            out.push((x1 - x0, y1 - y0, t));
        }
        out
    }

    fn to_i32(samples: &[(f32, f32, u64)]) -> Vec<(i32, i32, u64)> {
        samples
            .iter()
            .map(|(dx, dy, t)| (dx.round() as i32, dy.round() as i32, *t))
            .collect()
    }

    fn circle_metrics() -> StrokeMetrics {
        analyze_relative_stroke(&arc_samples(80.0, 36, 400, true, 1.0, 1.0)).unwrap()
    }

    #[test]
    fn center_path_on_bounds_moves_bbox_midpoint_to_origin() {
        let path = [(10.0, 0.0), (30.0, 0.0), (30.0, 20.0), (10.0, 20.0)];
        let centered = center_path_on_bounds(&path);
        assert_eq!(centered, [(-10.0, -10.0), (10.0, -10.0), (10.0, 10.0), (-10.0, 10.0)]);
    }

    #[test]
    fn usable_stroke_accepts_circle_rejects_line() {
        let circle = circle_metrics();
        assert!(is_usable_calibration_stroke(&circle));
        let line = StrokeMetrics {
            elapsed_ms: 200,
            total_angle_deg: 4.0,
            net_angle_deg: 4.0,
            signed_angle_deg: 4.0,
            rms_radius: 40.0,
            aspect_ratio: 8.0,
            closure_disp_px: 80.0,
            closure_ratio: 2.0,
            direction: RotationDirection::Clockwise,
        };
        assert!(!is_usable_calibration_stroke(&line));
        assert!(!should_finalize_idle_stroke(349, 10));
        assert!(should_finalize_idle_stroke(350, 10));
        assert!(!should_finalize_idle_stroke(400, 2));
    }

    #[test]
    fn analyze_too_few_samples_is_none() {
        assert!(analyze_stroke(&[(1, 0, 0), (0, 1, 10)]).is_none());
    }

    #[test]
    fn analyze_stationary_is_none() {
        assert!(analyze_stroke(&[(0, 0, 0), (0, 0, 10), (0, 0, 20)]).is_none());
    }

    #[test]
    fn analyze_perfect_circle() {
        let m = circle_metrics();
        assert_eq!(m.elapsed_ms, 400);
        assert_relative_eq!(m.net_angle_deg, 360.0, max_relative = 0.03);
        assert_relative_eq!(m.total_angle_deg, 360.0, max_relative = 0.03);
        assert_relative_eq!(m.rms_radius, 80.0, max_relative = 0.08);
        assert!(m.aspect_ratio < 1.15, "aspect {}", m.aspect_ratio);
        assert!(m.closure_ratio < 0.08, "closure {}", m.closure_ratio);
        assert_eq!(m.direction, RotationDirection::Clockwise);
    }

    #[test]
    fn analyze_counter_clockwise_circle() {
        let m = analyze_relative_stroke(&arc_samples(80.0, 36, 400, false, 1.0, 1.0)).unwrap();
        assert_eq!(m.direction, RotationDirection::CounterClockwise);
        assert_relative_eq!(m.net_angle_deg, 360.0, max_relative = 0.03);
    }

    #[test]
    fn analyze_ellipse() {
        let m = analyze_relative_stroke(&arc_samples(80.0, 36, 420, true, 2.0, 1.0)).unwrap();
        assert!(
            (1.7..2.4).contains(&m.aspect_ratio),
            "aspect {}",
            m.aspect_ratio
        );
        assert_relative_eq!(m.net_angle_deg, 360.0, max_relative = 0.05);
    }

    #[test]
    fn analyze_partial_arc_270() {
        let m = analyze_relative_stroke(&arc_samples(80.0, 27, 360, true, 1.0, 0.75)).unwrap();
        assert!(
            (240.0..300.0).contains(&m.net_angle_deg),
            "net {}",
            m.net_angle_deg
        );
        assert!(m.closure_ratio > 0.4, "partial arc should not close tightly");
        assert_eq!(m.elapsed_ms, 360);
    }

    #[test]
    fn analyze_stroke_i32_api_matches_rounded_circle() {
        let raw = arc_samples(80.0, 36, 400, true, 1.0, 1.0);
        let m = analyze_stroke(&to_i32(&raw)).unwrap();
        assert_relative_eq!(m.net_angle_deg, 360.0, max_relative = 0.08);
        assert_eq!(m.direction, RotationDirection::Clockwise);
    }

    #[test]
    fn recommend_rejects_too_few_but_allows_more_than_five() {
        let one = circle_metrics();
        assert_eq!(
            recommend_detector_config(&[one, one]).unwrap_err(),
            "too_few_strokes"
        );
        let many = [one; 7];
        assert!(recommend_detector_config(&many).is_ok());
    }

    #[test]
    fn calibrate_gate_ignores_entry_then_waits_for_stillness() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let mut gate = CalibrateGate::new();
        assert_eq!(gate.phase, CalibratePhase::Outside);

        assert_eq!(
            gate.note_inside(t0, Some((12.0, 0.0))),
            CalibrateGateAction::Ignore
        );
        assert_eq!(gate.phase, CalibratePhase::Positioning);

        assert_eq!(
            gate.note_inside(t0 + Duration::from_millis(80), Some((8.0, 0.0))),
            CalibrateGateAction::Ignore
        );
        assert_eq!(gate.phase, CalibratePhase::Positioning);

        assert_eq!(
            gate.note_inside(t0 + Duration::from_millis(280), None),
            CalibrateGateAction::Ignore
        );
        assert_eq!(gate.phase, CalibratePhase::Ready);

        assert_eq!(
            gate.note_inside(t0 + Duration::from_millis(290), Some((4.0, 1.0))),
            CalibrateGateAction::StartRecording
        );
        assert_eq!(gate.phase, CalibratePhase::Recording);

        assert_eq!(
            gate.note_inside(t0 + Duration::from_millis(300), Some((3.0, 2.0))),
            CalibrateGateAction::Record
        );
        assert_eq!(
            gate.note_inside(t0 + Duration::from_millis(660), None),
            CalibrateGateAction::Finalize
        );
        assert_eq!(gate.phase, CalibratePhase::Positioning);

        gate.note_outside();
        assert_eq!(gate.phase, CalibratePhase::Outside);
        assert_eq!(
            gate.note_inside(t0 + Duration::from_millis(700), Some((20.0, 0.0))),
            CalibrateGateAction::Ignore
        );
        assert_eq!(gate.phase, CalibratePhase::Positioning);
    }

    #[test]
    fn recommend_rejects_mixed_direction() {
        let cw = circle_metrics();
        let ccw = analyze_relative_stroke(&arc_samples(80.0, 36, 400, false, 1.0, 1.0)).unwrap();
        assert_eq!(
            recommend_detector_config(&[cw, cw, ccw]).unwrap_err(),
            "mixed_direction"
        );
    }

    #[test]
    fn recommend_rejects_high_variance() {
        let full = circle_metrics();
        let tiny = analyze_relative_stroke(&arc_samples(80.0, 18, 400, true, 1.0, 0.35)).unwrap();
        assert_eq!(
            recommend_detector_config(&[full, full, tiny]).unwrap_err(),
            "variance_too_high"
        );
    }

    #[test]
    fn recommend_from_three_circles() {
        let a = circle_metrics();
        let b = analyze_relative_stroke(&arc_samples(80.0, 36, 420, true, 1.0, 1.0)).unwrap();
        let c = analyze_relative_stroke(&arc_samples(78.0, 36, 390, true, 1.0, 1.0)).unwrap();
        let cfg = recommend_detector_config(&[a, b, c]).expect("recommend");

        let expected_window = ((420.0 * WINDOW_MARGIN).round() as u64).clamp(400, 900);
        assert_eq!(cfg.window_ms, expected_window);
        assert!(
            (180.0..340.0).contains(&cfg.min_total_angle_deg),
            "min_total {}",
            cfg.min_total_angle_deg
        );
        assert!(cfg.min_net_angle_deg < cfg.min_total_angle_deg);
        assert!(cfg.min_net_angle_deg >= NET_ANGLE_FLOOR_DEG);
        assert!((ASPECT_MIN..=ASPECT_MAX).contains(&cfg.ellipse_major_over_minor_max));
        assert!(cfg.min_rms_radius >= RMS_FLOOR);
        assert!(!cfg.allow_thumb_partial_closure);
        assert!((CLOSURE_FACTOR_MIN..=CLOSURE_FACTOR_MAX).contains(&cfg.closure_rms_factor));
    }

    #[test]
    fn recommend_partial_arcs_enable_thumb_relaxation() {
        let strokes: Vec<StrokeMetrics> = (0..3)
            .map(|i| {
                analyze_relative_stroke(&arc_samples(
                    80.0,
                    27,
                    350 + i * 8,
                    true,
                    1.0,
                    0.72,
                ))
                .unwrap()
            })
            .collect();
        let cfg = recommend_detector_config(&strokes).expect("recommend partial");
        assert!(cfg.allow_thumb_partial_closure);
        assert!(cfg.min_total_angle_deg >= ANGLE_FLOOR_DEG);
        assert!(cfg.min_total_angle_deg < 280.0);
    }

    #[test]
    fn recommend_ellipse_widens_aspect() {
        let strokes: Vec<StrokeMetrics> = (0..3)
            .map(|_| analyze_relative_stroke(&arc_samples(80.0, 36, 400, true, 2.0, 1.0)).unwrap())
            .collect();
        let cfg = recommend_detector_config(&strokes).expect("recommend ellipse");
        assert!(
            cfg.ellipse_major_over_minor_max >= 2.0,
            "aspect max {}",
            cfg.ellipse_major_over_minor_max
        );
    }
}
