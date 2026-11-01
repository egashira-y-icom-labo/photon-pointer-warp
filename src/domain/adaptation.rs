//! 日常フィードバックからの感度微調整（純 Rust / OS 非依存）

use super::calibration::{analyze_relative_stroke, StrokeMetrics};
use super::circle::{closure_displacement_limit, DetectorConfig};

const RECENT_LIMIT: usize = 10;
const MIN_MAJORITY: usize = 2;
const MIN_LEAD: usize = 2;

const RELATIVE_STEP: f32 = 0.10;
const ANGLE_STEP_DEG: f32 = 10.0;
const ANGLE_STEP_ABS_MAX_DEG: f32 = 15.0;
const WINDOW_RELAX_MS: u64 = 40;
const WINDOW_TIGHTEN_MS: u64 = 30;
const WINDOW_STEP_ABS_MAX_MS: u64 = 50;
const CLOSURE_RELAX: f32 = 0.05;
const ASPECT_RELAX: f32 = 0.2;

const WINDOW_MIN_MS: u64 = 400;
const WINDOW_MAX_MS: u64 = 900;
const TOTAL_ANGLE_MIN: f32 = 180.0;
const TOTAL_ANGLE_MAX: f32 = 340.0;
const NET_ANGLE_MIN: f32 = 160.0;
const NET_ANGLE_MAX: f32 = 330.0;
const ASPECT_MIN: f32 = 2.4;
const ASPECT_MAX: f32 = 5.0;
const CLOSURE_MIN: f32 = 0.30;
const CLOSURE_MAX: f32 = 0.65;

const FP_ANGLE_SOFT_CAP: f32 = 330.0;
const FP_WINDOW_MIN_TO_TIGHTEN: u64 = 450;

const NEAR_MISS_LOOKBACK_MS: u64 = 1500;
const NEAR_MISS_MIN_ANGLE_DEG: f32 = 180.0;
const NEAR_MISS_MIN_RMS: f32 = 8.0;
const ACCIDENTAL_MAX_MS: u64 = 300;
const ACCIDENTAL_MAX_DISPLACEMENT_PX: f32 = 10.0;

#[derive(Debug, Clone, PartialEq)]
pub enum MissedCause {
    /// 回転角不足（net/total が閾値未満）
    AngleDeficit,
    /// 所要時間が窓幅を超過（`metrics.elapsed_ms > config.window_ms`）
    TimeExceeded,
    /// 閉合不良（disp / rms 比率超過）
    NotClosed,
    /// 楕円潰れすぎ（aspect > ellipse_major_over_minor_max）
    AspectTooHigh,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FeedbackEvent {
    /// 取りこぼし（成功直前の惜しい弧）
    MissedStroke {
        metrics: StrokeMetrics,
        cause: MissedCause,
    },
    /// 誤爆（展開後 300ms 以内の即座キャンセル）
    AccidentalTrigger {
        metrics: StrokeMetrics,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConfigSuggestion {
    pub new_config: DetectorConfig,
    /// 実際に変動した差分のみ（例: `"窓 +40ms / 角 -10°"`）
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HudCloseReason {
    EscapeKey,
    RightClick,
    OriginClick,
    OutsideClick,
    OutOfBounds,
    AutoTimeout,
}

impl HudCloseReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EscapeKey => "escape",
            Self::RightClick => "right click",
            Self::OriginClick => "origin click",
            Self::OutsideClick => "outside click",
            Self::OutOfBounds => "out of bounds",
            Self::AutoTimeout => "auto timeout",
        }
    }

    fn is_reflex_cancel(self) -> bool {
        matches!(self, Self::EscapeKey | Self::RightClick)
    }
}

/// 成功直前 1.5 秒から、180° 以上かつ不成立の最も惜しい弧を 1 件だけ返す。
pub fn extract_best_near_miss(
    samples: &[(i32, i32, u64)],
    success_time_ms: u64,
    config: &DetectorConfig,
) -> Option<FeedbackEvent> {
    let lookback_start = success_time_ms.saturating_sub(NEAR_MISS_LOOKBACK_MS);
    let mut lookback: Vec<(f32, f32, u64)> = samples
        .iter()
        .filter(|(_, _, t)| *t <= success_time_ms && *t >= lookback_start)
        .map(|(dx, dy, t)| (*dx as f32, *dy as f32, *t))
        .collect();
    lookback.sort_by_key(|(_, _, t)| *t);
    if lookback.len() < 3 {
        return None;
    }

    let mut best: Option<StrokeMetrics> = None;
    let mut consider = |slice: &[(f32, f32, u64)]| {
        let Some(metrics) = analyze_relative_stroke(slice) else {
            return;
        };
        if !is_near_miss_candidate(&metrics, config) {
            return;
        }
        if best
            .as_ref()
            .is_some_and(|cur| near_miss_score(cur) >= near_miss_score(&metrics))
        {
            return;
        }
        best = Some(metrics);
    };

    for end in 2..lookback.len() {
        let end_t = lookback[end].2;
        let start = lookback[..end]
            .iter()
            .position(|(_, _, t)| end_t.saturating_sub(*t) <= config.window_ms)
            .unwrap_or(end);
        consider(&lookback[start..=end]);
    }
    consider(&lookback);

    let metrics = best?;
    Some(FeedbackEvent::MissedStroke {
        cause: classify_missed_cause(&metrics, config),
        metrics,
    })
}

pub fn is_accidental_trigger(
    hud_opened_at_ms: u64,
    hud_closed_at_ms: u64,
    screen_displacement_px: f32,
    close_reason: HudCloseReason,
) -> bool {
    if !close_reason.is_reflex_cancel() {
        return false;
    }
    if hud_closed_at_ms.saturating_sub(hud_opened_at_ms) >= ACCIDENTAL_MAX_MS {
        return false;
    }
    screen_displacement_px < ACCIDENTAL_MAX_DISPLACEMENT_PX
}

fn near_miss_score(metrics: &StrokeMetrics) -> f32 {
    metrics.total_angle_deg.max(metrics.net_angle_deg)
}

fn is_near_miss_candidate(metrics: &StrokeMetrics, config: &DetectorConfig) -> bool {
    if near_miss_score(metrics) < NEAR_MISS_MIN_ANGLE_DEG {
        return false;
    }
    if metrics.rms_radius < NEAR_MISS_MIN_RMS {
        return false;
    }
    !stroke_passes_config(metrics, config)
}

fn stroke_passes_config(metrics: &StrokeMetrics, config: &DetectorConfig) -> bool {
    if metrics.net_angle_deg < config.min_net_angle_deg {
        return false;
    }
    if metrics.net_angle_deg < config.min_total_angle_deg {
        return false;
    }
    if metrics.rms_radius < config.min_rms_radius || metrics.rms_radius > config.max_rms_radius {
        return false;
    }
    if metrics.aspect_ratio > config.ellipse_major_over_minor_max {
        return false;
    }
    !closure_exceeded(metrics, config)
}

fn closure_exceeded(metrics: &StrokeMetrics, config: &DetectorConfig) -> bool {
    let limit = closure_displacement_limit(
        metrics.rms_radius,
        config.closure_rms_factor,
        config.closure_disp_min_px,
    );
    metrics.closure_disp_px > limit
}

fn classify_missed_cause(metrics: &StrokeMetrics, config: &DetectorConfig) -> MissedCause {
    if metrics.elapsed_ms > config.window_ms {
        MissedCause::TimeExceeded
    } else if metrics.aspect_ratio > config.ellipse_major_over_minor_max {
        MissedCause::AspectTooHigh
    } else if closure_exceeded(metrics, config) {
        MissedCause::NotClosed
    } else {
        MissedCause::AngleDeficit
    }
}

pub fn generate_suggestion(
    current: &DetectorConfig,
    events: &[FeedbackEvent],
) -> Option<ConfigSuggestion> {
    let recent = if events.len() > RECENT_LIMIT {
        &events[events.len() - RECENT_LIMIT..]
    } else {
        events
    };

    let n_miss = recent
        .iter()
        .filter(|e| matches!(e, FeedbackEvent::MissedStroke { .. }))
        .count();
    let n_acc = recent
        .iter()
        .filter(|e| matches!(e, FeedbackEvent::AccidentalTrigger { .. }))
        .count();

    let fn_wins = n_miss >= MIN_MAJORITY && n_miss >= n_acc + MIN_LEAD;
    let fp_wins = n_acc >= MIN_MAJORITY && n_acc >= n_miss + MIN_LEAD;
    if !fn_wins && !fp_wins {
        return None;
    }

    let mut next = current.clone();
    if fn_wins {
        apply_fn_adjustment(&mut next, recent);
    } else {
        apply_fp_adjustment(&mut next);
    }

    let summary = diff_summary(current, &next)?;
    Some(ConfigSuggestion {
        new_config: next,
        summary,
    })
}

fn apply_fn_adjustment(cfg: &mut DetectorConfig, recent: &[FeedbackEvent]) {
    match dominant_missed_cause(recent) {
        Some(MissedCause::AngleDeficit) => {
            cfg.min_total_angle_deg =
                step_angle(cfg.min_total_angle_deg, -ANGLE_STEP_DEG, TOTAL_ANGLE_MIN, TOTAL_ANGLE_MAX);
            cfg.min_net_angle_deg =
                step_angle(cfg.min_net_angle_deg, -ANGLE_STEP_DEG, NET_ANGLE_MIN, NET_ANGLE_MAX);
        }
        Some(MissedCause::TimeExceeded) => {
            cfg.window_ms = step_window(cfg.window_ms, WINDOW_RELAX_MS as i64);
        }
        Some(MissedCause::NotClosed) => {
            cfg.closure_rms_factor = step_f32(
                cfg.closure_rms_factor,
                CLOSURE_RELAX,
                CLOSURE_MIN,
                CLOSURE_MAX,
                f32::MAX,
            );
            if !cfg.allow_thumb_partial_closure {
                cfg.allow_thumb_partial_closure = true;
            }
        }
        Some(MissedCause::AspectTooHigh) => {
            cfg.ellipse_major_over_minor_max = step_f32(
                cfg.ellipse_major_over_minor_max,
                ASPECT_RELAX,
                ASPECT_MIN,
                ASPECT_MAX,
                f32::MAX,
            );
        }
        None => {}
    }
}

fn apply_fp_adjustment(cfg: &mut DetectorConfig) {
    if cfg.min_total_angle_deg < FP_ANGLE_SOFT_CAP {
        cfg.min_total_angle_deg =
            step_angle(cfg.min_total_angle_deg, ANGLE_STEP_DEG, TOTAL_ANGLE_MIN, TOTAL_ANGLE_MAX);
        return;
    }
    if cfg.window_ms >= FP_WINDOW_MIN_TO_TIGHTEN {
        cfg.window_ms = step_window(cfg.window_ms, -(WINDOW_TIGHTEN_MS as i64));
    }
}

fn dominant_missed_cause(recent: &[FeedbackEvent]) -> Option<MissedCause> {
    let mut counts = [0usize; 4];
    let mut first_at = [usize::MAX; 4];
    for (i, event) in recent.iter().enumerate() {
        if let FeedbackEvent::MissedStroke { cause, .. } = event {
            let idx = cause_index(cause);
            counts[idx] += 1;
            first_at[idx] = first_at[idx].min(i);
        }
    }
    let max = *counts.iter().max().unwrap_or(&0);
    if max == 0 {
        return None;
    }
    (0..4)
        .filter(|&i| counts[i] == max)
        .min_by_key(|&i| first_at[i])
        .map(cause_from_index)
}

fn cause_index(cause: &MissedCause) -> usize {
    match cause {
        MissedCause::AngleDeficit => 0,
        MissedCause::TimeExceeded => 1,
        MissedCause::NotClosed => 2,
        MissedCause::AspectTooHigh => 3,
    }
}

fn cause_from_index(idx: usize) -> MissedCause {
    match idx {
        0 => MissedCause::AngleDeficit,
        1 => MissedCause::TimeExceeded,
        2 => MissedCause::NotClosed,
        _ => MissedCause::AspectTooHigh,
    }
}

fn step_angle(current: f32, delta: f32, min: f32, max: f32) -> f32 {
    step_f32(current, delta, min, max, ANGLE_STEP_ABS_MAX_DEG)
}

fn step_f32(current: f32, delta: f32, min: f32, max: f32, abs_cap: f32) -> f32 {
    let rel_cap = (current.abs() * RELATIVE_STEP).max(0.0);
    let cap = rel_cap.min(abs_cap);
    let limited = delta.clamp(-cap, cap);
    (current + limited).clamp(min, max)
}

fn step_window(current: u64, delta: i64) -> u64 {
    let rel_cap = ((current as f32) * RELATIVE_STEP).round() as u64;
    let cap = rel_cap.min(WINDOW_STEP_ABS_MAX_MS).max(0);
    let mag = (delta.unsigned_abs()).min(cap);
    let next = if delta >= 0 {
        current.saturating_add(mag)
    } else {
        current.saturating_sub(mag)
    };
    next.clamp(WINDOW_MIN_MS, WINDOW_MAX_MS)
}

fn diff_summary(before: &DetectorConfig, after: &DetectorConfig) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();

    let dw = after.window_ms as i64 - before.window_ms as i64;
    if dw != 0 {
        parts.push(format!("窓 {dw:+}ms"));
    }

    let d_total = after.min_total_angle_deg - before.min_total_angle_deg;
    let d_net = after.min_net_angle_deg - before.min_net_angle_deg;
    if d_total.abs() >= 0.05 || d_net.abs() >= 0.05 {
        let shown = if d_total.abs() >= 0.05 { d_total } else { d_net };
        parts.push(format!("角 {:+.0}°", shown));
    }

    let d_close = after.closure_rms_factor - before.closure_rms_factor;
    if d_close.abs() >= 0.001 {
        parts.push(format!("閉合緩和 {d_close:+.2}"));
    } else if after.allow_thumb_partial_closure && !before.allow_thumb_partial_closure {
        parts.push("親指部分閉合".to_string());
    }

    let d_aspect = after.ellipse_major_over_minor_max - before.ellipse_major_over_minor_max;
    if d_aspect.abs() >= 0.01 {
        parts.push(format!("楕円 {d_aspect:+.1}"));
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" / "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::calibration::RotationDirection;

    fn sample_metrics() -> StrokeMetrics {
        StrokeMetrics {
            elapsed_ms: 480,
            total_angle_deg: 220.0,
            net_angle_deg: 210.0,
            signed_angle_deg: 210.0,
            rms_radius: 18.0,
            aspect_ratio: 1.4,
            closure_disp_px: 12.0,
            closure_ratio: 0.7,
            direction: RotationDirection::Clockwise,
        }
    }

    fn missed(cause: MissedCause) -> FeedbackEvent {
        FeedbackEvent::MissedStroke {
            metrics: sample_metrics(),
            cause,
        }
    }

    fn accidental() -> FeedbackEvent {
        FeedbackEvent::AccidentalTrigger {
            metrics: sample_metrics(),
        }
    }

    fn n_missed(n: usize, cause: MissedCause) -> Vec<FeedbackEvent> {
        (0..n).map(|_| missed(cause.clone())).collect()
    }

    fn n_accidental(n: usize) -> Vec<FeedbackEvent> {
        (0..n).map(|_| accidental()).collect()
    }

    #[test]
    fn too_few_events_is_none() {
        let cfg = DetectorConfig::default();
        assert!(generate_suggestion(&cfg, &n_missed(1, MissedCause::AngleDeficit)).is_none());
    }

    #[test]
    fn tied_fn_and_fp_is_none() {
        let cfg = DetectorConfig::default();
        let mut events = n_missed(2, MissedCause::AngleDeficit);
        events.extend(n_accidental(2));
        assert!(generate_suggestion(&cfg, &events).is_none());
    }

    #[test]
    fn fn_angle_deficit_relaxes_angles() {
        let cfg = DetectorConfig::default();
        let suggestion =
            generate_suggestion(&cfg, &n_missed(2, MissedCause::AngleDeficit)).expect("suggest");
        assert_eq!(suggestion.new_config.min_total_angle_deg, 220.0);
        assert_eq!(suggestion.new_config.min_net_angle_deg, 190.0);
        assert_eq!(suggestion.summary, "角 -10°");
        assert_eq!(suggestion.new_config.window_ms, cfg.window_ms);
    }

    #[test]
    fn fn_not_closed_relaxes_closure_factor() {
        let mut cfg = DetectorConfig::default();
        cfg.closure_rms_factor = 0.50;
        cfg.allow_thumb_partial_closure = true;
        let suggestion =
            generate_suggestion(&cfg, &n_missed(2, MissedCause::NotClosed)).expect("suggest");
        assert!((suggestion.new_config.closure_rms_factor - 0.55).abs() < 1e-5);
        assert_eq!(suggestion.summary, "閉合緩和 +0.05");
    }

    #[test]
    fn fn_time_exceeded_extends_window() {
        let cfg = DetectorConfig::default();
        let suggestion =
            generate_suggestion(&cfg, &n_missed(2, MissedCause::TimeExceeded)).expect("suggest");
        assert_eq!(suggestion.new_config.window_ms, 590);
        assert_eq!(suggestion.summary, "窓 +40ms");
    }

    #[test]
    fn fp_tightens_angle_or_window() {
        let cfg = DetectorConfig::default();
        let suggestion = generate_suggestion(&cfg, &n_accidental(2)).expect("suggest");
        assert_eq!(suggestion.new_config.min_total_angle_deg, 240.0);
        assert_eq!(suggestion.summary, "角 +10°");
        assert_eq!(suggestion.new_config.window_ms, cfg.window_ms);
    }

    #[test]
    fn angle_floor_does_not_go_below_180() {
        let mut cfg = DetectorConfig::default();
        cfg.min_total_angle_deg = 180.0;
        cfg.min_net_angle_deg = 160.0;
        assert!(generate_suggestion(&cfg, &n_missed(3, MissedCause::AngleDeficit)).is_none());
    }

    fn arc_i32(
        r: f32,
        steps: u32,
        total_ms: u64,
        t0: u64,
        clockwise: bool,
        aspect_y: f32,
        turns: f32,
    ) -> Vec<(i32, i32, u64)> {
        let pos = |theta: f32| (r * theta.cos(), (r / aspect_y) * theta.sin());
        let denom = (steps - 1) as u64;
        (0..steps)
            .map(|i| {
                let a0 = std::f32::consts::TAU * turns * i as f32 / steps as f32;
                let a1 = std::f32::consts::TAU * turns * (i + 1) as f32 / steps as f32;
                let (a0, a1) = if clockwise { (a0, a1) } else { (-a0, -a1) };
                let (x0, y0) = pos(a0);
                let (x1, y1) = pos(a1);
                let t = t0 + total_ms.saturating_mul(i as u64) / denom;
                ((x1 - x0).round() as i32, (y1 - y0).round() as i32, t)
            })
            .collect()
    }

    #[test]
    fn extract_near_miss_none_for_line() {
        let cfg = DetectorConfig::default();
        let samples: Vec<(i32, i32, u64)> =
            (0..20).map(|i| (8, 0, 100 + i * 10)).collect();
        assert!(extract_best_near_miss(&samples, 400, &cfg).is_none());
    }

    #[test]
    fn extract_near_miss_picks_largest_angle() {
        let cfg = DetectorConfig::default();
        let mut samples = arc_i32(80.0, 28, 400, 0, true, 1.0, 210.0 / 360.0);
        samples.extend(arc_i32(80.0, 32, 400, 500, true, 1.0, 270.0 / 360.0));
        let event = extract_best_near_miss(&samples, 950, &cfg).expect("near miss");
        match event {
            FeedbackEvent::MissedStroke { metrics, .. } => {
                assert!(
                    metrics.total_angle_deg.max(metrics.net_angle_deg) >= 240.0,
                    "score {}",
                    metrics.total_angle_deg.max(metrics.net_angle_deg)
                );
            }
            FeedbackEvent::AccidentalTrigger { .. } => panic!("expected missed stroke"),
        }
    }

    #[test]
    fn extract_near_miss_classifies_time_exceeded() {
        let mut cfg = DetectorConfig::default();
        cfg.window_ms = 400;
        let samples = arc_i32(80.0, 36, 800, 0, true, 1.0, 0.75);
        let event = extract_best_near_miss(&samples, 800, &cfg).expect("near miss");
        match event {
            FeedbackEvent::MissedStroke { cause, metrics } => {
                assert_eq!(cause, MissedCause::TimeExceeded);
                assert!(metrics.elapsed_ms > cfg.window_ms);
            }
            FeedbackEvent::AccidentalTrigger { .. } => panic!("expected missed stroke"),
        }
    }

    #[test]
    fn extract_near_miss_classifies_aspect() {
        let mut cfg = DetectorConfig::default();
        cfg.ellipse_major_over_minor_max = 2.4;
        let samples = arc_i32(90.0, 40, 420, 0, true, 4.2, 1.0);
        let event = extract_best_near_miss(&samples, 420, &cfg).expect("near miss");
        match event {
            FeedbackEvent::MissedStroke { cause, .. } => {
                assert_eq!(cause, MissedCause::AspectTooHigh);
            }
            FeedbackEvent::AccidentalTrigger { .. } => panic!("expected missed stroke"),
        }
    }

    #[test]
    fn extract_near_miss_classifies_angle_deficit_when_closed_but_short() {
        let mut cfg = DetectorConfig::default();
        cfg.min_total_angle_deg = 400.0;
        cfg.min_net_angle_deg = 390.0;
        let samples = arc_i32(80.0, 36, 400, 0, true, 1.0, 1.0);
        let event = extract_best_near_miss(&samples, 400, &cfg).expect("near miss");
        match event {
            FeedbackEvent::MissedStroke { cause, .. } => {
                assert_eq!(cause, MissedCause::AngleDeficit);
            }
            FeedbackEvent::AccidentalTrigger { .. } => panic!("expected missed stroke"),
        }
    }

    #[test]
    fn accidental_trigger_requires_reflex_cancel_within_300ms_and_10px() {
        assert!(is_accidental_trigger(
            1000,
            1200,
            4.0,
            HudCloseReason::EscapeKey
        ));
        assert!(is_accidental_trigger(
            1000,
            1299,
            9.5,
            HudCloseReason::RightClick
        ));
        assert!(!is_accidental_trigger(
            1000,
            1300,
            4.0,
            HudCloseReason::EscapeKey
        ));
        assert!(!is_accidental_trigger(
            1000,
            1100,
            10.0,
            HudCloseReason::EscapeKey
        ));
        assert!(!is_accidental_trigger(
            1000,
            1100,
            2.0,
            HudCloseReason::AutoTimeout
        ));
        assert!(!is_accidental_trigger(
            1000,
            1100,
            2.0,
            HudCloseReason::OutOfBounds
        ));
        assert!(!is_accidental_trigger(
            1000,
            1100,
            2.0,
            HudCloseReason::OriginClick
        ));
    }
}
