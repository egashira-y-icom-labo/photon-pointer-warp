//! HUD を開く前の静止待ち（純 Rust / Win32 非依存）

use super::trigger::TriggerSource;

/// 円: 50ms 静止、または動き続けても 100ms で開く。
pub const CIRCLE_QUIET_MS: u64 = 50;
pub const CIRCLE_MAX_WAIT_MS: u64 = 100;
/// シェイク: 100ms 静止で開く。動き続けたら 1000ms で開かずに捨てる。
pub const SHAKE_QUIET_MS: u64 = 100;
pub const SHAKE_MAX_WAIT_MS: u64 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettlePolicy {
    pub quiet_ms: u64,
    pub max_wait_ms: u64,
    /// 最大待ちまで動き続けたとき、開かずに捨てる。
    pub cancel_on_timeout: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettleDecision {
    Wait,
    Open,
    Cancel,
}

pub fn policy_for(source: TriggerSource) -> SettlePolicy {
    match source {
        TriggerSource::Circle => SettlePolicy {
            quiet_ms: CIRCLE_QUIET_MS,
            max_wait_ms: CIRCLE_MAX_WAIT_MS,
            cancel_on_timeout: false,
        },
        TriggerSource::Shake => SettlePolicy {
            quiet_ms: SHAKE_QUIET_MS,
            max_wait_ms: SHAKE_MAX_WAIT_MS,
            cancel_on_timeout: true,
        },
    }
}

/// `quiet_ms` は最後の大きな移動からの経過。静止が先、タイムアウトはまだ動いているときだけ。
pub fn decide_settle(elapsed_ms: u64, quiet_ms: u64, policy: SettlePolicy) -> SettleDecision {
    if quiet_ms >= policy.quiet_ms {
        return SettleDecision::Open;
    }
    if elapsed_ms >= policy.max_wait_ms {
        return if policy.cancel_on_timeout {
            SettleDecision::Cancel
        } else {
            SettleDecision::Open
        };
    }
    SettleDecision::Wait
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circle_opens_after_50ms_quiet() {
        let policy = policy_for(TriggerSource::Circle);
        assert_eq!(policy.quiet_ms, 50);
        assert_eq!(policy.max_wait_ms, 100);
        assert!(!policy.cancel_on_timeout);
        assert_eq!(decide_settle(40, 49, policy), SettleDecision::Wait);
        assert_eq!(decide_settle(40, 50, policy), SettleDecision::Open);
    }

    #[test]
    fn circle_opens_when_still_moving_at_100ms() {
        let policy = policy_for(TriggerSource::Circle);
        assert_eq!(decide_settle(99, 10, policy), SettleDecision::Wait);
        assert_eq!(decide_settle(100, 10, policy), SettleDecision::Open);
    }

    #[test]
    fn shake_opens_after_100ms_quiet() {
        let policy = policy_for(TriggerSource::Shake);
        assert_eq!(policy.quiet_ms, 100);
        assert_eq!(policy.max_wait_ms, 1000);
        assert!(policy.cancel_on_timeout);
        assert_eq!(decide_settle(200, 99, policy), SettleDecision::Wait);
        assert_eq!(decide_settle(200, 100, policy), SettleDecision::Open);
    }

    #[test]
    fn shake_cancels_when_motion_continues_for_1000ms() {
        let policy = policy_for(TriggerSource::Shake);
        assert_eq!(decide_settle(999, 20, policy), SettleDecision::Wait);
        assert_eq!(decide_settle(1000, 20, policy), SettleDecision::Cancel);
    }

    #[test]
    fn shake_quiet_at_the_deadline_still_opens() {
        let policy = policy_for(TriggerSource::Shake);
        assert_eq!(decide_settle(1000, 100, policy), SettleDecision::Open);
    }
}
