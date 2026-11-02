//! シェイク後の HUD 追従（純 Rust / Win32 非依存）

const WINDOW_MS: u64 = 100;
const STILL_PX: i32 = 6;
const TIMEOUT_MS: u64 = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackingAction {
    Continue,
    Anchor,
    TimeoutDismiss,
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    t: u64,
    x: i32,
    y: i32,
}

pub struct TrackingController {
    start_ms: u64,
    samples: Vec<Sample>,
}

impl TrackingController {
    pub fn new(start_time_ms: u64, start_pos: (i32, i32)) -> Self {
        Self {
            start_ms: start_time_ms,
            samples: vec![Sample {
                t: start_time_ms,
                x: start_pos.0,
                y: start_pos.1,
            }],
        }
    }

    /// 直近 100ms の始点と終点が 6px 以内で、その窓が 100ms あるとき固定する。
    /// 2 秒動き続けたら破棄する。固定条件を先に見る。
    pub fn update(&mut self, current_time_ms: u64, current_pos: (i32, i32)) -> TrackingAction {
        let t = current_time_ms.max(self.samples.last().map(|s| s.t).unwrap_or(self.start_ms));
        self.samples.push(Sample {
            t,
            x: current_pos.0,
            y: current_pos.1,
        });
        self.samples
            .retain(|sample| t.saturating_sub(sample.t) <= WINDOW_MS);

        if self.still_for_window() {
            return TrackingAction::Anchor;
        }
        if t.saturating_sub(self.start_ms) >= TIMEOUT_MS {
            return TrackingAction::TimeoutDismiss;
        }
        TrackingAction::Continue
    }

    fn still_for_window(&self) -> bool {
        let Some(first) = self.samples.first() else {
            return false;
        };
        let Some(last) = self.samples.last() else {
            return false;
        };
        if last.t.saturating_sub(first.t) < WINDOW_MS {
            return false;
        }
        let dx = last.x - first.x;
        let dy = last.y - first.y;
        dx * dx + dy * dy <= STILL_PX * STILL_PX
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reversal_pause_keeps_tracking() {
        let mut tracker = TrackingController::new(0, (0, 0));
        let path = [
            (25, (50, 0)),
            (40, (50, 0)),
            (70, (50, 0)),
            (100, (20, 0)),
        ];
        for (t, pos) in path {
            assert_eq!(
                tracker.update(t, pos),
                TrackingAction::Continue,
                "t={t} pos={pos:?}"
            );
        }
    }

    #[test]
    fn still_for_100ms_anchors() {
        let mut tracker = TrackingController::new(0, (10, 20));
        assert_eq!(tracker.update(99, (10, 20)), TrackingAction::Continue);
        assert_eq!(tracker.update(100, (16, 20)), TrackingAction::Anchor);
    }

    #[test]
    fn motion_past_2000ms_dismisses() {
        let mut tracker = TrackingController::new(0, (0, 0));
        assert_eq!(tracker.update(1999, (400, 0)), TrackingAction::Continue);
        assert_eq!(
            tracker.update(2000, (800, 0)),
            TrackingAction::TimeoutDismiss
        );
    }

    #[test]
    fn stillness_at_the_deadline_anchors() {
        let mut tracker = TrackingController::new(0, (4, 4));
        assert_eq!(tracker.update(1900, (4, 4)), TrackingAction::Continue);
        assert_eq!(tracker.update(2000, (4, 4)), TrackingAction::Anchor);
    }
}
