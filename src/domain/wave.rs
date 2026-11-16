//! 波動・照準モードの純ロジック（Win32 非依存）
//!
//! 方角は仮想 2D スティック（位置保持）で決める。角度ダイヤルは使わない。

use std::f32::consts::TAU;

/// 中押しから Expanding までの待機。
pub const ARMING_HOLD_MS: u64 = 130;
/// Arming 中にこれを超える相対移動で中ボタンドラッグとみなす。
pub const ARMING_DRAG_SLOP_PX: f32 = 5.0;
/// Frozen 移行直後のホイール無視時間。
pub const FROZEN_WHEEL_DEBOUNCE_MS: u64 = 50;
/// 候補切替 1 ステップに必要なホイール累積（標準 WHEEL_DELTA）。
pub const WHEEL_DELTA_STEP: i32 = 120;
/// 仮想スティック最大半径（相対 px 相当）。
pub const STICK_RADIUS: f32 = 70.0;
/// 方角未確定の遊び。
pub const STICK_DEADZONE: f32 = 4.0;
/// 相対移動の減速（カーソルと同方向のまま感度を下げる）。
pub const STICK_SCALE: f32 = 0.5;
/// Expanding 中の自動拡大速度。
pub const RADIUS_GROWTH_PX_PER_MS: f32 = 0.55;
/// 進む／戻すボタンでの半径変化速度。
pub const RADIUS_ADJUST_PX_PER_MS: f32 = 0.45;
/// 収束アニメ時間。
pub const CONVERGE_MS: f32 = 180.0;
/// 着弾フラッシュ時間。
pub const IMPACT_FLASH_MS: f32 = 130.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaveConfig {
    pub arming_hold_ms: u64,
    pub arming_drag_slop_px: f32,
    pub frozen_wheel_debounce_ms: u64,
    pub wheel_delta_step: i32,
    pub stick_radius: f32,
    pub stick_deadzone: f32,
    pub stick_scale: f32,
    pub radius_growth_px_per_ms: f32,
    pub radius_adjust_px_per_ms: f32,
    pub converge_ms: f32,
    pub impact_flash_ms: f32,
}

impl Default for WaveConfig {
    fn default() -> Self {
        Self {
            arming_hold_ms: ARMING_HOLD_MS,
            arming_drag_slop_px: ARMING_DRAG_SLOP_PX,
            frozen_wheel_debounce_ms: FROZEN_WHEEL_DEBOUNCE_MS,
            wheel_delta_step: WHEEL_DELTA_STEP,
            stick_radius: STICK_RADIUS,
            stick_deadzone: STICK_DEADZONE,
            stick_scale: STICK_SCALE,
            radius_growth_px_per_ms: RADIUS_GROWTH_PX_PER_MS,
            radius_adjust_px_per_ms: RADIUS_ADJUST_PX_PER_MS,
            converge_ms: CONVERGE_MS,
            impact_flash_ms: IMPACT_FLASH_MS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WavePhaseKind {
    Idle,
    Arming,
    Expanding,
    Frozen,
    Converging,
    ImpactFlash,
    CancelRewind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaveSignal {
    None,
    PassthroughDrag,
    PassthroughClick,
    StartExpanding,
    Freeze,
    AscendToMap,
    ConvergeShoot,
    ConvergeWarp,
    CancelRewind,
    /// 収束完了。カーソルを着弾点へ移してよい（フラッシュ開始）。
    Impact,
    /// フラッシュ終了。オーバーレイを閉じてよい。
    Finished,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaveCandidate {
    pub id: u64,
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WaveSnapshot {
    pub phase: WavePhaseKind,
    pub origin_x: f32,
    pub origin_y: f32,
    pub radius: f32,
    pub angle_rad: f32,
    pub direction_locked: bool,
    pub stick_x: f32,
    pub stick_y: f32,
    pub selected_rank: usize,
    pub ranked_ids: Vec<u64>,
    /// 0..1 — Converging / ImpactFlash / CancelRewind
    pub progress: f32,
    pub target_x: f32,
    pub target_y: f32,
}

#[derive(Debug, Clone, PartialEq)]
enum Phase {
    Idle,
    Arming {
        origin_x: f32,
        origin_y: f32,
        started_ms: u64,
        move_accum: f32,
    },
    Expanding {
        origin_x: f32,
        origin_y: f32,
        started_ms: u64,
        last_ms: u64,
        radius: f32,
        stick_x: f32,
        stick_y: f32,
        grow_held: bool,
        shrink_held: bool,
    },
    Frozen {
        origin_x: f32,
        origin_y: f32,
        entered_ms: u64,
        radius: f32,
        stick_x: f32,
        stick_y: f32,
        selected_rank: usize,
        wheel_accum: i32,
        grow_held: bool,
        shrink_held: bool,
        last_ms: u64,
    },
    Converging {
        origin_x: f32,
        origin_y: f32,
        radius: f32,
        stick_x: f32,
        stick_y: f32,
        target_x: f32,
        target_y: f32,
        shoot: bool,
        started_ms: u64,
        last_ms: u64,
        progress: f32,
    },
    ImpactFlash {
        x: f32,
        y: f32,
        shoot: bool,
        started_ms: u64,
        last_ms: u64,
        progress: f32,
    },
    CancelRewind {
        origin_x: f32,
        origin_y: f32,
        radius: f32,
        stick_x: f32,
        stick_y: f32,
        started_ms: u64,
        last_ms: u64,
        progress: f32,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct WaveController {
    config: WaveConfig,
    phase: Phase,
    candidates: Vec<WaveCandidate>,
}

impl WaveController {
    pub fn new(config: WaveConfig) -> Self {
        Self {
            config,
            phase: Phase::Idle,
            candidates: Vec::new(),
        }
    }

    pub fn phase_kind(&self) -> WavePhaseKind {
        match self.phase {
            Phase::Idle => WavePhaseKind::Idle,
            Phase::Arming { .. } => WavePhaseKind::Arming,
            Phase::Expanding { .. } => WavePhaseKind::Expanding,
            Phase::Frozen { .. } => WavePhaseKind::Frozen,
            Phase::Converging { .. } => WavePhaseKind::Converging,
            Phase::ImpactFlash { .. } => WavePhaseKind::ImpactFlash,
            Phase::CancelRewind { .. } => WavePhaseKind::CancelRewind,
        }
    }

    pub fn is_active(&self) -> bool {
        !matches!(self.phase, Phase::Idle)
    }

    pub fn ignores_wheel(&self) -> bool {
        matches!(
            self.phase,
            Phase::Arming { .. }
                | Phase::Expanding { .. }
                | Phase::Converging { .. }
                | Phase::ImpactFlash { .. }
                | Phase::CancelRewind { .. }
        )
    }

    pub fn set_candidates(&mut self, candidates: Vec<WaveCandidate>) {
        self.candidates = candidates;
        if let Phase::Frozen {
            selected_rank,
            origin_x,
            origin_y,
            radius,
            stick_x,
            stick_y,
            ..
        } = &mut self.phase
        {
            let (locked, angle) = stick_heading(*stick_x, *stick_y, self.config.stick_deadzone);
            let ranked = rank_candidates(
                &self.candidates,
                *origin_x,
                *origin_y,
                *radius,
                angle,
                locked,
            );
            *selected_rank = (*selected_rank).min(ranked.len().saturating_sub(1));
        }
    }

    pub fn snapshot(&self) -> WaveSnapshot {
        let ranked = self.ranked_ids();
        match &self.phase {
            Phase::Idle => empty_snap(ranked),
            Phase::Arming {
                origin_x,
                origin_y,
                ..
            } => WaveSnapshot {
                phase: WavePhaseKind::Arming,
                origin_x: *origin_x,
                origin_y: *origin_y,
                ..empty_snap(ranked)
            },
            Phase::Expanding {
                origin_x,
                origin_y,
                radius,
                stick_x,
                stick_y,
                ..
            } => {
                let (locked, angle) = stick_heading(*stick_x, *stick_y, self.config.stick_deadzone);
                WaveSnapshot {
                    phase: WavePhaseKind::Expanding,
                    origin_x: *origin_x,
                    origin_y: *origin_y,
                    radius: *radius,
                    angle_rad: angle,
                    direction_locked: locked,
                    stick_x: *stick_x,
                    stick_y: *stick_y,
                    selected_rank: 0,
                    ranked_ids: ranked,
                    progress: 0.0,
                    target_x: 0.0,
                    target_y: 0.0,
                }
            }
            Phase::Frozen {
                origin_x,
                origin_y,
                radius,
                stick_x,
                stick_y,
                selected_rank,
                ..
            } => {
                let (locked, angle) = stick_heading(*stick_x, *stick_y, self.config.stick_deadzone);
                WaveSnapshot {
                    phase: WavePhaseKind::Frozen,
                    origin_x: *origin_x,
                    origin_y: *origin_y,
                    radius: *radius,
                    angle_rad: angle,
                    direction_locked: locked,
                    stick_x: *stick_x,
                    stick_y: *stick_y,
                    selected_rank: *selected_rank,
                    ranked_ids: ranked,
                    progress: 0.0,
                    target_x: 0.0,
                    target_y: 0.0,
                }
            }
            Phase::Converging {
                origin_x,
                origin_y,
                radius,
                stick_x,
                stick_y,
                target_x,
                target_y,
                progress,
                ..
            } => {
                let (locked, angle) = stick_heading(*stick_x, *stick_y, self.config.stick_deadzone);
                WaveSnapshot {
                    phase: WavePhaseKind::Converging,
                    origin_x: *origin_x,
                    origin_y: *origin_y,
                    radius: *radius,
                    angle_rad: angle,
                    direction_locked: locked,
                    stick_x: *stick_x,
                    stick_y: *stick_y,
                    selected_rank: 0,
                    ranked_ids: ranked,
                    progress: *progress,
                    target_x: *target_x,
                    target_y: *target_y,
                }
            }
            Phase::ImpactFlash {
                x,
                y,
                progress,
                ..
            } => WaveSnapshot {
                phase: WavePhaseKind::ImpactFlash,
                origin_x: *x,
                origin_y: *y,
                radius: 0.0,
                angle_rad: 0.0,
                direction_locked: true,
                stick_x: 0.0,
                stick_y: 0.0,
                selected_rank: 0,
                ranked_ids: ranked,
                progress: *progress,
                target_x: *x,
                target_y: *y,
            },
            Phase::CancelRewind {
                origin_x,
                origin_y,
                radius,
                stick_x,
                stick_y,
                progress,
                ..
            } => {
                let (locked, angle) = stick_heading(*stick_x, *stick_y, self.config.stick_deadzone);
                WaveSnapshot {
                    phase: WavePhaseKind::CancelRewind,
                    origin_x: *origin_x,
                    origin_y: *origin_y,
                    radius: *radius,
                    angle_rad: angle,
                    direction_locked: locked,
                    stick_x: *stick_x,
                    stick_y: *stick_y,
                    selected_rank: 0,
                    ranked_ids: ranked,
                    progress: *progress,
                    target_x: 0.0,
                    target_y: 0.0,
                }
            }
        }
    }

    pub fn aim_point(&self) -> Option<(f32, f32)> {
        let s = self.snapshot();
        match s.phase {
            WavePhaseKind::Idle | WavePhaseKind::Arming => None,
            WavePhaseKind::ImpactFlash => Some((s.target_x, s.target_y)),
            WavePhaseKind::Converging => Some((s.target_x, s.target_y)),
            _ => {
                if !s.direction_locked || s.radius <= 0.0 {
                    return None;
                }
                Some(ray_circle_intersection(
                    s.origin_x,
                    s.origin_y,
                    s.radius,
                    s.angle_rad,
                ))
            }
        }
    }

    pub fn selected_candidate(&self) -> Option<&WaveCandidate> {
        let s = self.snapshot();
        let id = s.ranked_ids.get(s.selected_rank)?;
        self.candidates.iter().find(|c| c.id == *id)
    }

    fn ranked_ids(&self) -> Vec<u64> {
        match &self.phase {
            Phase::Idle | Phase::Arming { .. } | Phase::ImpactFlash { .. } => Vec::new(),
            Phase::Expanding {
                origin_x,
                origin_y,
                radius,
                stick_x,
                stick_y,
                ..
            }
            | Phase::Frozen {
                origin_x,
                origin_y,
                radius,
                stick_x,
                stick_y,
                ..
            }
            | Phase::CancelRewind {
                origin_x,
                origin_y,
                radius,
                stick_x,
                stick_y,
                ..
            } => {
                let (locked, angle) = stick_heading(*stick_x, *stick_y, self.config.stick_deadzone);
                rank_candidates(
                    &self.candidates,
                    *origin_x,
                    *origin_y,
                    *radius,
                    angle,
                    locked,
                )
            }
            Phase::Converging {
                target_x,
                target_y,
                ..
            } => self
                .candidates
                .iter()
                .filter(|c| (c.x - *target_x).abs() < 0.5 && (c.y - *target_y).abs() < 0.5)
                .map(|c| c.id)
                .collect(),
        }
    }

    pub fn on_middle_down(&mut self, x: f32, y: f32, clock_ms: u64) -> WaveSignal {
        if !matches!(self.phase, Phase::Idle) {
            return WaveSignal::None;
        }
        self.phase = Phase::Arming {
            origin_x: x,
            origin_y: y,
            started_ms: clock_ms,
            move_accum: 0.0,
        };
        WaveSignal::None
    }

    pub fn on_middle_up(&mut self, clock_ms: u64) -> WaveSignal {
        match self.phase {
            Phase::Arming { .. } => {
                self.phase = Phase::Idle;
                WaveSignal::PassthroughClick
            }
            Phase::Expanding {
                origin_x,
                origin_y,
                radius,
                stick_x,
                stick_y,
                grow_held,
                shrink_held,
                ..
            } => {
                self.phase = Phase::Frozen {
                    origin_x,
                    origin_y,
                    entered_ms: clock_ms,
                    radius,
                    stick_x,
                    stick_y,
                    selected_rank: 0,
                    wheel_accum: 0,
                    grow_held,
                    shrink_held,
                    last_ms: clock_ms,
                };
                WaveSignal::Freeze
            }
            Phase::Frozen { .. } => self.begin_converge(clock_ms, false),
            _ => WaveSignal::None,
        }
    }

    pub fn on_relative_move(&mut self, dx: f32, dy: f32, _clock_ms: u64) -> WaveSignal {
        match &mut self.phase {
            Phase::Arming { move_accum, .. } => {
                *move_accum += (dx * dx + dy * dy).sqrt();
                if *move_accum >= self.config.arming_drag_slop_px {
                    self.phase = Phase::Idle;
                    return WaveSignal::PassthroughDrag;
                }
            }
            Phase::Expanding {
                stick_x, stick_y, ..
            }
            | Phase::Frozen {
                stick_x, stick_y, ..
            } => {
                let (nx, ny) = integrate_stick(
                    *stick_x,
                    *stick_y,
                    dx,
                    dy,
                    self.config.stick_scale,
                    self.config.stick_radius,
                );
                *stick_x = nx;
                *stick_y = ny;
            }
            _ => {}
        }
        WaveSignal::None
    }

    pub fn on_wheel(&mut self, delta: i32, clock_ms: u64) -> WaveSignal {
        if self.ignores_wheel() {
            return WaveSignal::None;
        }
        let Phase::Frozen {
            entered_ms,
            selected_rank,
            wheel_accum,
            ..
        } = &mut self.phase
        else {
            return WaveSignal::None;
        };
        if clock_ms.saturating_sub(*entered_ms) < self.config.frozen_wheel_debounce_ms {
            return WaveSignal::None;
        }
        *wheel_accum += delta;
        let step = self.config.wheel_delta_step;
        while *wheel_accum >= step {
            *wheel_accum -= step;
            let n = self.candidates.len().max(1);
            *selected_rank = (*selected_rank + 1) % n;
        }
        while *wheel_accum <= -step {
            *wheel_accum += step;
            let n = self.candidates.len().max(1);
            *selected_rank = (*selected_rank + n - 1) % n;
        }
        WaveSignal::None
    }

    pub fn on_grow_button(&mut self, down: bool) {
        match &mut self.phase {
            Phase::Expanding { grow_held, .. } | Phase::Frozen { grow_held, .. } => {
                *grow_held = down;
            }
            _ => {}
        }
    }

    pub fn on_shrink_button(&mut self, down: bool) {
        match &mut self.phase {
            Phase::Expanding { shrink_held, .. } | Phase::Frozen { shrink_held, .. } => {
                *shrink_held = down;
            }
            _ => {}
        }
    }

    pub fn on_left_click(&mut self, clock_ms: u64) -> WaveSignal {
        if matches!(self.phase, Phase::Frozen { .. }) {
            self.begin_converge(clock_ms, true)
        } else {
            WaveSignal::None
        }
    }

    pub fn on_right_click(&mut self, clock_ms: u64) -> WaveSignal {
        match self.phase {
            Phase::Expanding {
                origin_x,
                origin_y,
                radius,
                stick_x,
                stick_y,
                ..
            }
            | Phase::Frozen {
                origin_x,
                origin_y,
                radius,
                stick_x,
                stick_y,
                ..
            } => {
                self.phase = Phase::CancelRewind {
                    origin_x,
                    origin_y,
                    radius,
                    stick_x,
                    stick_y,
                    started_ms: clock_ms,
                    last_ms: clock_ms,
                    progress: 0.0,
                };
                WaveSignal::CancelRewind
            }
            _ => WaveSignal::None,
        }
    }

    pub fn on_tick(&mut self, clock_ms: u64, max_radius: f32) -> WaveSignal {
        match &mut self.phase {
            Phase::Arming {
                origin_x,
                origin_y,
                started_ms,
                ..
            } => {
                if clock_ms.saturating_sub(*started_ms) >= self.config.arming_hold_ms {
                    let origin_x = *origin_x;
                    let origin_y = *origin_y;
                    let started_ms = *started_ms;
                    self.phase = Phase::Expanding {
                        origin_x,
                        origin_y,
                        started_ms,
                        last_ms: clock_ms,
                        radius: 0.0,
                        stick_x: 0.0,
                        stick_y: 0.0,
                        grow_held: false,
                        shrink_held: false,
                    };
                    return WaveSignal::StartExpanding;
                }
            }
            Phase::Expanding {
                last_ms,
                radius,
                grow_held,
                shrink_held,
                ..
            } => {
                let dt = clock_ms.saturating_sub(*last_ms) as f32;
                *last_ms = clock_ms;
                let mut rate = self.config.radius_growth_px_per_ms;
                if *grow_held {
                    rate += self.config.radius_adjust_px_per_ms;
                }
                if *shrink_held {
                    rate -= self.config.radius_adjust_px_per_ms;
                }
                let cap = max_radius.max(1.0);
                *radius = (*radius + rate * dt).clamp(0.0, cap);
                if *radius >= cap {
                    return WaveSignal::AscendToMap;
                }
            }
            Phase::Frozen {
                last_ms,
                radius,
                grow_held,
                shrink_held,
                ..
            } => {
                let dt = clock_ms.saturating_sub(*last_ms) as f32;
                *last_ms = clock_ms;
                let mut delta = 0.0;
                if *grow_held {
                    delta += self.config.radius_adjust_px_per_ms * dt;
                }
                if *shrink_held {
                    delta -= self.config.radius_adjust_px_per_ms * dt;
                }
                if delta != 0.0 {
                    *radius = (*radius + delta).clamp(1.0, max_radius.max(1.0));
                }
            }
            Phase::Converging {
                last_ms,
                progress,
                shoot,
                target_x,
                target_y,
                ..
            } => {
                let dt = clock_ms.saturating_sub(*last_ms) as f32;
                *last_ms = clock_ms;
                *progress = (*progress + dt / self.config.converge_ms).min(1.0);
                if *progress >= 1.0 {
                    let shoot = *shoot;
                    let x = *target_x;
                    let y = *target_y;
                    self.phase = Phase::ImpactFlash {
                        x,
                        y,
                        shoot,
                        started_ms: clock_ms,
                        last_ms: clock_ms,
                        progress: 0.0,
                    };
                    return WaveSignal::Impact;
                }
            }
            Phase::ImpactFlash {
                last_ms,
                progress,
                ..
            } => {
                let dt = clock_ms.saturating_sub(*last_ms) as f32;
                *last_ms = clock_ms;
                *progress = (*progress + dt / self.config.impact_flash_ms).min(1.0);
                if *progress >= 1.0 {
                    self.phase = Phase::Idle;
                    return WaveSignal::Finished;
                }
            }
            Phase::CancelRewind {
                last_ms,
                progress,
                radius,
                ..
            } => {
                let dt = clock_ms.saturating_sub(*last_ms) as f32;
                *last_ms = clock_ms;
                *progress = (*progress + dt / 160.0).min(1.0);
                *radius *= 1.0 - (dt / 160.0).min(1.0) * 0.35;
                if *progress >= 1.0 {
                    self.phase = Phase::Idle;
                    return WaveSignal::Finished;
                }
            }
            Phase::Idle => {}
        }
        WaveSignal::None
    }

    pub fn reset(&mut self) {
        self.phase = Phase::Idle;
        self.candidates.clear();
    }

    fn begin_converge(&mut self, clock_ms: u64, shoot: bool) -> WaveSignal {
        let Phase::Frozen {
            origin_x,
            origin_y,
            radius,
            stick_x,
            stick_y,
            selected_rank,
            ..
        } = self.phase
        else {
            return WaveSignal::None;
        };
        let (locked, angle) = stick_heading(stick_x, stick_y, self.config.stick_deadzone);
        let ranked = rank_candidates(
            &self.candidates,
            origin_x,
            origin_y,
            radius,
            angle,
            locked,
        );
        let (target_x, target_y) = if let Some(id) = ranked.get(selected_rank) {
            self.candidates
                .iter()
                .find(|c| c.id == *id)
                .map(|c| (c.x, c.y))
                .unwrap_or_else(|| ray_circle_intersection(origin_x, origin_y, radius, angle))
        } else if locked {
            ray_circle_intersection(origin_x, origin_y, radius, angle)
        } else {
            // 未確定でも中心方向（右）へフォールバックせず、波源近傍を避けるため +x
            ray_circle_intersection(origin_x, origin_y, radius, 0.0)
        };
        self.phase = Phase::Converging {
            origin_x,
            origin_y,
            radius,
            stick_x,
            stick_y,
            target_x,
            target_y,
            shoot,
            started_ms: clock_ms,
            last_ms: clock_ms,
            progress: 0.0,
        };
        if shoot {
            WaveSignal::ConvergeShoot
        } else {
            WaveSignal::ConvergeWarp
        }
    }
}

fn empty_snap(ranked: Vec<u64>) -> WaveSnapshot {
    WaveSnapshot {
        phase: WavePhaseKind::Idle,
        origin_x: 0.0,
        origin_y: 0.0,
        radius: 0.0,
        angle_rad: 0.0,
        direction_locked: false,
        stick_x: 0.0,
        stick_y: 0.0,
        selected_rank: 0,
        ranked_ids: ranked,
        progress: 0.0,
        target_x: 0.0,
        target_y: 0.0,
    }
}

/// カーソルと同じ上下左右感覚でスティックを更新し、最大半径でクランプ。
pub fn integrate_stick(
    stick_x: f32,
    stick_y: f32,
    dx: f32,
    dy: f32,
    scale: f32,
    max_radius: f32,
) -> (f32, f32) {
    let mut x = stick_x + dx * scale;
    let mut y = stick_y + dy * scale;
    let len = (x * x + y * y).sqrt();
    if len > max_radius && len > 0.0 {
        x = x / len * max_radius;
        y = y / len * max_radius;
    }
    (x, y)
}

pub fn stick_heading(stick_x: f32, stick_y: f32, deadzone: f32) -> (bool, f32) {
    let len = (stick_x * stick_x + stick_y * stick_y).sqrt();
    if len < deadzone {
        (false, 0.0)
    } else {
        (true, stick_y.atan2(stick_x).rem_euclid(TAU))
    }
}

pub fn rank_candidates(
    candidates: &[WaveCandidate],
    origin_x: f32,
    origin_y: f32,
    radius: f32,
    angle_rad: f32,
    direction_locked: bool,
) -> Vec<u64> {
    let mut scored: Vec<(u64, f32)> = candidates
        .iter()
        .map(|c| {
            let score = if direction_locked {
                let (ix, iy) = ray_circle_intersection(origin_x, origin_y, radius, angle_rad);
                (c.x - ix).hypot(c.y - iy)
            } else {
                let dist = (c.x - origin_x).hypot(c.y - origin_y);
                (dist - radius).abs()
            };
            (c.id, score)
        })
        .collect();
    scored.sort_by(|a, b| {
        a.1.partial_cmp(&b.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    scored.into_iter().map(|(id, _)| id).collect()
}

pub fn ray_circle_intersection(ox: f32, oy: f32, radius: f32, angle_rad: f32) -> (f32, f32) {
    (
        ox + radius * angle_rad.cos(),
        oy + radius * angle_rad.sin(),
    )
}

/// 波源から現在モニターの各辺までの最短距離（端まで広がり切る最大射程）。
pub fn max_radius_to_monitor_edge(
    origin_x: f32,
    origin_y: f32,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
) -> f32 {
    let to_left = origin_x - left;
    let to_right = right - origin_x;
    let to_top = origin_y - top;
    let to_bottom = bottom - origin_y;
    [to_left, to_right, to_top, to_bottom]
        .into_iter()
        .filter(|d| *d > 0.0)
        .fold(f32::INFINITY, f32::min)
        .max(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctrl() -> WaveController {
        WaveController::new(WaveConfig::default())
    }

    #[test]
    fn arming_short_click_passthrough() {
        let mut w = ctrl();
        assert_eq!(w.on_middle_down(10.0, 20.0, 0), WaveSignal::None);
        assert_eq!(w.on_middle_up(50), WaveSignal::PassthroughClick);
        assert_eq!(w.phase_kind(), WavePhaseKind::Idle);
    }

    #[test]
    fn arming_drag_bypasses_to_passthrough() {
        let mut w = ctrl();
        w.on_middle_down(0.0, 0.0, 0);
        assert_eq!(w.on_relative_move(3.0, 0.0, 10), WaveSignal::None);
        assert_eq!(w.on_relative_move(3.0, 0.0, 20), WaveSignal::PassthroughDrag);
    }

    #[test]
    fn arming_hold_starts_expanding() {
        let mut w = ctrl();
        w.on_middle_down(100.0, 100.0, 0);
        assert_eq!(w.on_tick(130, 400.0), WaveSignal::StartExpanding);
        assert_eq!(w.phase_kind(), WavePhaseKind::Expanding);
    }

    #[test]
    fn stick_matches_cursor_directions() {
        let (x, y) = integrate_stick(0.0, 0.0, 20.0, 0.0, 0.5, 70.0);
        assert!((x - 10.0).abs() < 1e-3);
        assert!(y.abs() < 1e-3);
        let (locked, angle) = stick_heading(x, y, 4.0);
        assert!(locked);
        assert!(angle.abs() < 0.05 || (TAU - angle).abs() < 0.05);

        let (x2, y2) = integrate_stick(0.0, 0.0, 0.0, 20.0, 0.5, 70.0);
        let (locked2, angle2) = stick_heading(x2, y2, 4.0);
        assert!(locked2);
        // 下方向 ≈ +π/2（スクリーン座標）
        assert!((angle2 - std::f32::consts::FRAC_PI_2).abs() < 0.05);
    }

    #[test]
    fn stick_clamps_to_radius_preserving_angle() {
        let (x, y) = integrate_stick(0.0, 0.0, 400.0, 0.0, 0.5, 70.0);
        let len = (x * x + y * y).sqrt();
        assert!((len - 70.0).abs() < 1e-2);
        assert!(y.abs() < 1e-3);
    }

    #[test]
    fn stick_aim_point_follows_right_then_down() {
        let mut w = ctrl();
        w.on_middle_down(0.0, 0.0, 0);
        w.on_tick(130, 500.0);
        w.on_relative_move(20.0, 0.0, 140); // stick ≈ (10,0)
        w.on_tick(160, 500.0);
        w.on_middle_up(170);
        let aim = w.aim_point().expect("aim");
        assert!(aim.0 > 0.0);
        assert!(aim.1.abs() < 1.0);
    }

    #[test]
    fn release_during_expand_freezes_at_rank_one() {
        let mut w = ctrl();
        w.on_middle_down(0.0, 0.0, 0);
        w.on_tick(130, 500.0);
        w.set_candidates(vec![
            WaveCandidate {
                id: 10,
                x: 50.0,
                y: 0.0,
            },
            WaveCandidate {
                id: 20,
                x: 80.0,
                y: 0.0,
            },
        ]);
        w.on_tick(200, 500.0);
        assert_eq!(w.on_middle_up(210), WaveSignal::Freeze);
        assert_eq!(w.snapshot().selected_rank, 0);
    }

    #[test]
    fn expanding_to_edge_ascends_to_map() {
        let mut w = WaveController::new(WaveConfig {
            radius_growth_px_per_ms: 10.0,
            ..WaveConfig::default()
        });
        w.on_middle_down(50.0, 50.0, 0);
        w.on_tick(130, 100.0);
        assert_eq!(w.on_tick(150, 100.0), WaveSignal::AscendToMap);
        assert_eq!(w.phase_kind(), WavePhaseKind::Expanding);
        w.reset();
    }

    #[test]
    fn warp_converges_then_impacts_then_finishes() {
        let mut w = WaveController::new(WaveConfig {
            converge_ms: 100.0,
            impact_flash_ms: 50.0,
            ..WaveConfig::default()
        });
        w.on_middle_down(0.0, 0.0, 0);
        w.on_tick(130, 500.0);
        w.on_relative_move(30.0, 0.0, 140);
        w.on_middle_up(150);
        assert_eq!(w.on_middle_up(200), WaveSignal::ConvergeWarp);
        assert_eq!(w.on_tick(250, 500.0), WaveSignal::None);
        assert_eq!(w.on_tick(320, 500.0), WaveSignal::Impact);
        assert_eq!(w.phase_kind(), WavePhaseKind::ImpactFlash);
        assert_eq!(w.on_tick(400, 500.0), WaveSignal::Finished);
        assert_eq!(w.phase_kind(), WavePhaseKind::Idle);
    }

    #[test]
    fn max_radius_is_nearest_monitor_edge() {
        // origin (100,50) in [0,0)-(400,300)
        let r = max_radius_to_monitor_edge(100.0, 50.0, 0.0, 0.0, 400.0, 300.0);
        assert!((r - 50.0).abs() < 1e-3); // top edge nearest
    }

    #[test]
    fn undecided_ranks_by_circumference_distance() {
        let ranked = rank_candidates(
            &[
                WaveCandidate {
                    id: 1,
                    x: 100.0,
                    y: 0.0,
                },
                WaveCandidate {
                    id: 2,
                    x: 50.0,
                    y: 0.0,
                },
                WaveCandidate {
                    id: 3,
                    x: 130.0,
                    y: 0.0,
                },
            ],
            0.0,
            0.0,
            100.0,
            0.0,
            false,
        );
        assert_eq!(ranked, vec![1, 3, 2]);
    }

    #[test]
    fn locked_ranks_by_aim_point_distance() {
        let ranked = rank_candidates(
            &[
                WaveCandidate {
                    id: 1,
                    x: 100.0,
                    y: 0.0,
                },
                WaveCandidate {
                    id: 2,
                    x: 0.0,
                    y: 100.0,
                },
                WaveCandidate {
                    id: 3,
                    x: 80.0,
                    y: 10.0,
                },
            ],
            0.0,
            0.0,
            100.0,
            0.0,
            true,
        );
        assert_eq!(ranked[0], 1);
    }
}
