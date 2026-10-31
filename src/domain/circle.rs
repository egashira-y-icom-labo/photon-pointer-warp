//! サークル・ジェスチャ検知（純 Rust / Win32 非依存）

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionSample {
    pub dx: f32,
    pub dy: f32,
    pub dt_millis: u64,
}

#[derive(Debug, Clone, PartialEq)]
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
}

impl Default for DetectorConfig {
    fn default() -> Self {
        Self {
            window_ms: 350,
            min_total_angle_deg: 300.0,
            min_net_angle_deg: 270.0,
            min_rms_radius: 8.0,
            max_rms_radius: 2000.0,
            ellipse_major_over_minor_max: 2.8,
        }
    }
}

pub struct CircleDetector {
    config: DetectorConfig,
    is_dragging: bool,
}

impl CircleDetector {
    pub fn new(config: DetectorConfig) -> Self {
        Self {
            config,
            is_dragging: false,
        }
    }

    pub fn set_drag_state(&mut self, is_dragging: bool) {
        self.is_dragging = is_dragging;
    }

    /// サークル完成と判定したサンプルでのみ `true`。完成後は内部で [`Self::reset`] 相当。
    pub fn push(&mut self, _sample: MotionSample) -> bool {
        todo!("CircleDetector::push")
    }

    pub fn reset(&mut self) {
        todo!("CircleDetector::reset")
    }
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
        let samples = circle_deltas(40.0, 16, 800, true, 1.0);
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
        let arc_steps = (full_steps as f32 * 270.0 / 360.0).round() as u32;
        let samples = circle_deltas(40.0, arc_steps, 280, true, 1.0);
        let results = push_all(&mut det, &samples);
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
        // 1周 500ms: 終了時点の 350ms 窓には約 252° 分しか入らない
        let samples = circle_deltas(40.0, 16, 500, true, 1.0);
        let results = push_all(&mut det, &samples);
        assert_all_false(&results);
    }
}
