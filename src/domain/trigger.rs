//! 円とシェイクの起動調停（純 Rust / Win32 非依存）

use super::circle::{CircleDetector, MotionSample};
use super::shake::ShakeDetector;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerSource {
    Circle,
    Shake,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TriggerResult {
    pub source: TriggerSource,
    pub is_learning_target: bool,
}

pub struct TriggerCoordinator {
    pub circle: CircleDetector,
    pub shake: ShakeDetector,
}

impl TriggerCoordinator {
    pub fn new(circle: CircleDetector, shake: ShakeDetector) -> Self {
        Self { circle, shake }
    }

    pub fn reset(&mut self) {
        self.circle.reset();
        self.shake.reset();
    }

    /// 抑制中は両方を捨てて `None`。成立時は円を優先し、返した直後に両方を空にする。
    pub fn push(
        &mut self,
        sample: MotionSample,
        is_button_down: bool,
        is_paused: bool,
        is_calibrating: bool,
    ) -> Option<TriggerResult> {
        if is_button_down || is_paused || is_calibrating {
            self.reset();
            return None;
        }

        let circle_fired = self.circle.push(sample);
        let shake_fired = self.shake.push(sample);
        if !circle_fired && !shake_fired {
            return None;
        }

        let source = if circle_fired {
            TriggerSource::Circle
        } else {
            TriggerSource::Shake
        };
        self.reset();
        Some(TriggerResult {
            source,
            is_learning_target: source == TriggerSource::Circle,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::circle::DetectorConfig;

    fn coordinator() -> TriggerCoordinator {
        TriggerCoordinator::new(
            CircleDetector::new(DetectorConfig::default()),
            ShakeDetector::new(),
        )
    }

    fn axis_legs(leg_px: f32, legs: usize, total_ms: u64) -> Vec<MotionSample> {
        const SAMPLES_PER_LEG: usize = 4;
        let n = legs * SAMPLES_PER_LEG;
        let dt = (total_ms / n as u64).max(1);
        let step = leg_px / SAMPLES_PER_LEG as f32;
        let mut samples = Vec::with_capacity(n);
        for leg in 0..legs {
            let sign = if leg % 2 == 0 { 1.0 } else { -1.0 };
            for _ in 0..SAMPLES_PER_LEG {
                samples.push(MotionSample {
                    dx: sign * step,
                    dy: 0.0,
                    dt_millis: dt,
                });
            }
        }
        samples
    }

    fn circle_samples() -> Vec<MotionSample> {
        let steps = 16_u32;
        let total_ms = 320_u64;
        let radius = 40.0_f32;
        let dt = (total_ms / steps as u64).max(1);
        let mut samples = Vec::with_capacity(steps as usize);
        for i in 0..steps {
            let t0 = std::f32::consts::TAU * i as f32 / steps as f32;
            let t1 = std::f32::consts::TAU * (i + 1) as f32 / steps as f32;
            samples.push(MotionSample {
                dx: radius * (t1.cos() - t0.cos()),
                dy: radius * (t1.sin() - t0.sin()),
                dt_millis: dt,
            });
        }
        samples
    }

    #[test]
    fn circle_sample_is_learning_target() {
        let mut coord = coordinator();
        let samples = circle_samples();
        let mut found = None;
        for (i, sample) in samples.iter().copied().enumerate() {
            let result = coord.push(sample, false, false, false);
            if i + 1 < samples.len() {
                assert!(result.is_none(), "circle fired early at {i}");
            } else {
                found = result;
            }
        }
        assert_eq!(
            found,
            Some(TriggerResult {
                source: TriggerSource::Circle,
                is_learning_target: true,
            })
        );
    }

    #[test]
    fn shake_sample_is_not_learning_target() {
        let mut coord = coordinator();
        let samples = axis_legs(40.0, 5, 400);
        let mut found = None;
        for sample in samples {
            if let Some(result) = coord.push(sample, false, false, false) {
                found = Some(result);
                break;
            }
        }
        assert_eq!(
            found,
            Some(TriggerResult {
                source: TriggerSource::Shake,
                is_learning_target: false,
            })
        );
    }

    #[test]
    fn simultaneous_fire_prefers_circle_and_resets_both() {
        let mut coord = coordinator();
        let circle = circle_samples();
        let last = *circle.last().unwrap();
        assert!(last.dx > 0.0);
        for sample in &circle[..circle.len() - 1] {
            assert!(!coord.circle.push(*sample));
        }

        for sample in axis_legs(40.0, 4, 320) {
            assert!(!coord.shake.push(sample));
        }
        let primed = 40.0 - last.dx + 0.01;
        assert!(primed < 40.0);
        assert!(!coord.shake.push(MotionSample {
            dx: primed,
            dy: 0.0,
            dt_millis: 10,
        }));

        let result = coord
            .push(last, false, false, false)
            .expect("both detectors are one sample from firing");
        assert_eq!(
            result,
            TriggerResult {
                source: TriggerSource::Circle,
                is_learning_target: true,
            }
        );
        assert!(coord.circle.last_detection_stats().is_none());
        assert!(coord
            .push(
                MotionSample {
                    dx: 1.0,
                    dy: 0.0,
                    dt_millis: 10,
                },
                false,
                false,
                false,
            )
            .is_none());
    }

    #[test]
    fn suppression_discards_partial_shake() {
        for (button, paused, calibrating) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            let mut coord = coordinator();
            let samples = axis_legs(40.0, 5, 400);
            for sample in &samples[..12] {
                assert!(coord.push(*sample, false, false, false).is_none());
            }
            assert!(coord
                .push(samples[12], button, paused, calibrating)
                .is_none());
            for sample in &samples[13..] {
                assert!(
                    coord.push(*sample, false, false, false).is_none(),
                    "tail after button={button} paused={paused} calibrating={calibrating}"
                );
            }
        }
    }

    #[test]
    fn firing_resets_so_the_next_sample_does_not_fire_again() {
        let mut coord = coordinator();
        let samples = axis_legs(40.0, 5, 400);
        let mut fired = false;
        for sample in samples {
            if coord.push(sample, false, false, false).is_some() {
                fired = true;
                break;
            }
        }
        assert!(fired);
        assert!(coord
            .push(
                MotionSample {
                    dx: 10.0,
                    dy: 0.0,
                    dt_millis: 10,
                },
                false,
                false,
                false,
            )
            .is_none());
    }
}
