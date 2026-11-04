//! シェイク（往復）ジェスチャ検知（純 Rust / Win32 非依存）
//!
//! 入力は円検知と同じ [`MotionSample`]（`dx`/`dy` は Raw Input の整数差分を `f32` にしたもの、
//! `dt_millis` は直前サンプルからの経過）。絶対時刻は円検知と同様、`dt_millis` を積んで作る。

use super::circle::MotionSample;

const WINDOW_MS: u64 = 500;
const MIN_LEG_PX: f32 = 40.0;
const MIN_REVERSALS: u32 = 4;
/// 従軸の累積絶対移動量が主軸のこの倍率を超えたらシェイクではない。
const CROSS_AXIS_MAX_RATIO: f32 = 0.45;

#[derive(Debug, Clone, Copy)]
struct TimedDelta {
    dx: f32,
    dy: f32,
    end_time_ms: u64,
}

pub struct ShakeDetector {
    virtual_time_ms: u64,
    samples: Vec<TimedDelta>,
}

impl ShakeDetector {
    pub fn new() -> Self {
        Self {
            virtual_time_ms: 0,
            samples: Vec::new(),
        }
    }

    pub fn reset(&mut self) {
        self.virtual_time_ms = 0;
        self.samples.clear();
    }

    /// シェイク成立サンプルでのみ `true`。成立後もバッファは残るので、呼び出し側が [`Self::reset`] する。
    pub fn push(&mut self, sample: MotionSample) -> bool {
        self.virtual_time_ms = self.virtual_time_ms.saturating_add(sample.dt_millis);
        self.samples.push(TimedDelta {
            dx: sample.dx,
            dy: sample.dy,
            end_time_ms: self.virtual_time_ms,
        });
        self.purge_old_samples();
        self.evaluate_window()
    }

    fn purge_old_samples(&mut self) {
        let cutoff = self.virtual_time_ms.saturating_sub(WINDOW_MS);
        self.samples.retain(|s| s.end_time_ms > cutoff);
    }

    fn evaluate_window(&self) -> bool {
        if self.samples.len() < 2 {
            return false;
        }

        let sum_abs_x: f32 = self.samples.iter().map(|s| s.dx.abs()).sum();
        let sum_abs_y: f32 = self.samples.iter().map(|s| s.dy.abs()).sum();
        let (axis_is_x, major, minor) = if sum_abs_x >= sum_abs_y {
            (true, sum_abs_x, sum_abs_y)
        } else {
            (false, sum_abs_y, sum_abs_x)
        };
        if major <= 0.0 || minor > major * CROSS_AXIS_MAX_RATIO {
            return false;
        }

        reversal_count(&self.samples, axis_is_x) >= MIN_REVERSALS
    }
}

impl Default for ShakeDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// 主軸成分の符号が、前後の脚それぞれ 40px 以上を伴って反転した回数。
///
/// 反対方向が 40px に達するまで反転は確定しない。途中の数 px は脚の長さから差し引くだけで、
/// それまでの蓄積は捨てない。
fn reversal_count(samples: &[TimedDelta], axis_is_x: bool) -> u32 {
    let mut direction: i8 = 0;
    let mut committed = 0.0_f32;
    let mut pending_opposite = 0.0_f32;
    let mut reversals = 0_u32;

    for sample in samples {
        let component = if axis_is_x { sample.dx } else { sample.dy };
        let sign = axis_sign(component);
        if sign == 0 {
            continue;
        }

        if direction == 0 {
            direction = sign;
            committed = component.abs();
            continue;
        }

        if sign == direction {
            if pending_opposite > 0.0 {
                if component.abs() >= pending_opposite {
                    committed += component.abs() - pending_opposite;
                    pending_opposite = 0.0;
                } else {
                    pending_opposite -= component.abs();
                }
            } else {
                committed += component.abs();
            }
            continue;
        }

        pending_opposite += component.abs();
        if pending_opposite < MIN_LEG_PX {
            continue;
        }

        if committed >= MIN_LEG_PX {
            reversals += 1;
        }
        direction = sign;
        committed = pending_opposite;
        pending_opposite = 0.0;
    }

    reversals
}

fn axis_sign(v: f32) -> i8 {
    if v > 0.0 {
        1
    } else if v < 0.0 {
        -1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_all(det: &mut ShakeDetector, samples: &[MotionSample]) -> Vec<bool> {
        samples.iter().copied().map(|s| det.push(s)).collect()
    }

    fn assert_all_false(results: &[bool]) {
        for (i, &fired) in results.iter().enumerate() {
            assert!(!fired, "expected false at index {i}, got true");
        }
    }

    /// `legs` 本の直線往復。偶数脚が正方向。各脚は 4 サンプルに分割する。
    fn axis_legs(leg_px: f32, legs: usize, total_ms: u64, horizontal: bool) -> Vec<MotionSample> {
        const SAMPLES_PER_LEG: usize = 4;
        let n = legs * SAMPLES_PER_LEG;
        let dt = (total_ms / n as u64).max(1);
        let step = leg_px / SAMPLES_PER_LEG as f32;
        let mut samples = Vec::with_capacity(n);
        for leg in 0..legs {
            let sign = if leg % 2 == 0 { 1.0 } else { -1.0 };
            for _ in 0..SAMPLES_PER_LEG {
                let delta = sign * step;
                samples.push(if horizontal {
                    MotionSample {
                        dx: delta,
                        dy: 0.0,
                        dt_millis: dt,
                    }
                } else {
                    MotionSample {
                        dx: 0.0,
                        dy: delta,
                        dt_millis: dt,
                    }
                });
            }
        }
        samples
    }

    fn circle_samples(radius: f32, steps: u32, total_ms: u64) -> Vec<MotionSample> {
        let dt = (total_ms / steps as u64).max(1);
        let mut prev_x = radius;
        let mut prev_y = 0.0_f32;
        let mut out = Vec::with_capacity(steps as usize);
        for i in 1..=steps {
            let theta = (i as f32) / (steps as f32) * std::f32::consts::TAU;
            let x = radius * theta.cos();
            let y = radius * theta.sin();
            out.push(MotionSample {
                dx: x - prev_x,
                dy: y - prev_y,
                dt_millis: dt,
            });
            prev_x = x;
            prev_y = y;
        }
        out
    }

    #[test]
    fn test_horizontal_two_round_trips() {
        let mut det = ShakeDetector::new();
        // 右→左→右→左→右。反転 4 回、各脚 40px、400ms。
        let samples = axis_legs(40.0, 5, 400, true);
        let results = push_all(&mut det, &samples);
        assert!(
            results.iter().any(|fired| *fired),
            "horizontal shake should fire"
        );
    }

    #[test]
    fn test_vertical_two_round_trips() {
        let mut det = ShakeDetector::new();
        let samples = axis_legs(40.0, 5, 400, false);
        let results = push_all(&mut det, &samples);
        assert!(
            results.iter().any(|fired| *fired),
            "vertical shake should fire"
        );
    }

    #[test]
    fn test_single_round_trip() {
        let mut det = ShakeDetector::new();
        // 右→左→右 は反転 2 回。
        let samples = axis_legs(80.0, 3, 240, true);
        assert_all_false(&push_all(&mut det, &samples));
    }

    #[test]
    fn test_small_amplitude() {
        let mut det = ShakeDetector::new();
        let samples = axis_legs(20.0, 8, 400, true);
        assert_all_false(&push_all(&mut det, &samples));
    }

    #[test]
    fn test_circle_rejected_by_cross_axis() {
        let mut det = ShakeDetector::new();
        let samples = circle_samples(48.0, 24, 360);
        assert_all_false(&push_all(&mut det, &samples));
    }

    #[test]
    fn test_slow_round_trips_fall_out_of_window() {
        let mut det = ShakeDetector::new();
        let samples = axis_legs(40.0, 5, 1200, true);
        let span: u64 = samples.iter().map(|s| s.dt_millis).sum();
        assert!(span > WINDOW_MS);
        assert_all_false(&push_all(&mut det, &samples));
    }

    #[test]
    fn test_reset_discards_partial_shake() {
        let mut det = ShakeDetector::new();
        let partial = axis_legs(40.0, 3, 240, true);
        assert_all_false(&push_all(&mut det, &partial));
        det.reset();
        let full = axis_legs(40.0, 5, 400, true);
        assert!(push_all(&mut det, &full).iter().any(|fired| *fired));
    }

    #[test]
    fn test_leg_length_boundary_40_fires_39_does_not() {
        let mut at_limit = ShakeDetector::new();
        assert!(push_all(&mut at_limit, &axis_legs(40.0, 5, 400, true))
            .iter()
            .any(|fired| *fired));

        let mut under = ShakeDetector::new();
        // 10+10+10+9 = 39px。40px 未満の脚は反転として確定しない。
        let mut short = Vec::new();
        for leg in 0..5 {
            let sign = if leg % 2 == 0 { 1.0 } else { -1.0 };
            for step in [10.0, 10.0, 10.0, 9.0] {
                short.push(MotionSample {
                    dx: sign * step,
                    dy: 0.0,
                    dt_millis: 16,
                });
            }
        }
        assert_all_false(&push_all(&mut under, &short));
    }

    #[test]
    fn test_three_reversals_do_not_fire() {
        let mut det = ShakeDetector::new();
        // 4 脚は反転 3 回。
        assert_all_false(&push_all(&mut det, &axis_legs(40.0, 4, 320, true)));
    }

    #[test]
    fn test_cross_axis_ratio_at_0_45_fires_above_does_not() {
        let mut at_limit = ShakeDetector::new();
        let mut exact = axis_legs(40.0, 5, 400, true);
        let major: f32 = exact.iter().map(|s| s.dx.abs()).sum();
        exact[0].dy = major * 0.45;
        assert!(
            push_all(&mut at_limit, &exact).iter().any(|fired| *fired),
            "ratio 0.45 is still a shake"
        );

        let mut over = ShakeDetector::new();
        let mut too_much = axis_legs(40.0, 5, 400, true);
        let major: f32 = too_much.iter().map(|s| s.dx.abs()).sum();
        too_much[0].dy = major * 0.46;
        assert_all_false(&push_all(&mut over, &too_much));
    }

    #[test]
    fn test_diagonal_round_trip_rejected() {
        let mut det = ShakeDetector::new();
        let mut samples = Vec::new();
        for leg in 0..5 {
            let sign = if leg % 2 == 0 { 1.0 } else { -1.0 };
            for _ in 0..4 {
                samples.push(MotionSample {
                    dx: sign * 10.0,
                    dy: sign * 10.0,
                    dt_millis: 20,
                });
            }
        }
        assert_all_false(&push_all(&mut det, &samples));
    }

    #[test]
    fn test_five_px_blip_does_not_cancel_shake() {
        let mut det = ShakeDetector::new();
        let mut samples = Vec::new();
        {
            let mut push_run = |dx: f32, n: usize| {
                for _ in 0..n {
                    samples.push(MotionSample {
                        dx,
                        dy: 0.0,
                        dt_millis: 15,
                    });
                }
            };
            // 最初の脚の途中に 5px の逆向き。脚全体は 40px を超えるので反転は確定する。
            push_run(10.0, 2);
            push_run(-5.0, 1);
            push_run(10.0, 3);
            push_run(-10.0, 4);
            push_run(10.0, 4);
            push_run(-10.0, 4);
            push_run(10.0, 4);
        }
        assert!(push_all(&mut det, &samples).iter().any(|fired| *fired));
    }

    #[test]
    fn test_stays_fired_until_reset() {
        let mut det = ShakeDetector::new();
        let samples = axis_legs(40.0, 5, 400, true);
        let results = push_all(&mut det, &samples);
        assert!(results.iter().any(|fired| *fired));
        assert!(det.push(MotionSample {
            dx: 10.0,
            dy: 0.0,
            dt_millis: 10,
        }));
        det.reset();
        assert!(!det.push(MotionSample {
            dx: 10.0,
            dy: 0.0,
            dt_millis: 10,
        }));
    }

    #[test]
    fn test_shake_samples_do_not_fire_circle_detector() {
        use super::super::circle::{CircleDetector, DetectorConfig};
        use super::super::config::AppConfig;

        let samples = axis_legs(40.0, 5, 400, true);
        let mut shake = ShakeDetector::new();
        assert!(push_all(&mut shake, &samples).iter().any(|fired| *fired));

        for config in [DetectorConfig::default(), AppConfig::default().detector] {
            let mut circle = CircleDetector::new(config);
            let fired = samples.iter().copied().any(|sample| circle.push(sample));
            assert!(!fired, "horizontal shake must not count as a circle");
        }
    }
}
