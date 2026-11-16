//! 波動・照準モードの純ロジック（Win32 非依存）
//!
//! 方角は波源起点の仮想ポインター先端 `pointer_tip`（スクリーン座標）で決める。

use std::f32::consts::TAU;

/// 中押しから Expanding までの待機。
pub const ARMING_HOLD_MS: u64 = 130;
/// Arming 中にこれを超える相対移動で中ボタンドラッグとみなす。
pub const ARMING_DRAG_SLOP_PX: f32 = 5.0;
/// Frozen 移行直後のホイール無視時間。
pub const FROZEN_WHEEL_DEBOUNCE_MS: u64 = 50;
/// 候補切替 1 ステップに必要なホイール累積。
pub const WHEEL_DELTA_STEP: i32 = 120;
/// 方角未確定の遊び（波源からの距離）。
pub const POINTER_DEADZONE_PX: f32 = 16.0;
/// 相対移動の感度（1:1 ポインティング）。
pub const POINTER_SCALE: f32 = 1.0;
/// Expanding 中の自動拡大速度。
pub const RADIUS_GROWTH_PX_PER_MS: f32 = 0.55;
/// 進む／戻すボタンでの半径変化速度。
pub const RADIUS_ADJUST_PX_PER_MS: f32 = 0.45;
/// 収束アニメ時間。
pub const CONVERGE_MS: f32 = 180.0;
/// 着弾フラッシュ時間（衝撃波・スパークを視認できる長さ）。
pub const IMPACT_FLASH_MS: f32 = 350.0;
/// マップ昇華時に対角距離へ足す余裕。
pub const ASCEND_RADIUS_PADDING_PX: f32 = 40.0;
/// 射撃残留をカーソル移動で解除する距離。
pub const LINGER_DISMISS_DIST_PX: f32 = 36.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AimBounds {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl AimBounds {
    pub fn clamp_point(self, x: f32, y: f32) -> (f32, f32) {
        // right/bottom は排他なので内側にわずかに寄せる
        let max_x = (self.right - 1.0).max(self.left);
        let max_y = (self.bottom - 1.0).max(self.top);
        (
            x.clamp(self.left, max_x),
            y.clamp(self.top, max_y),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaveConfig {
    pub arming_hold_ms: u64,
    pub arming_drag_slop_px: f32,
    pub frozen_wheel_debounce_ms: u64,
    pub wheel_delta_step: i32,
    pub pointer_deadzone_px: f32,
    pub pointer_scale: f32,
    pub radius_growth_px_per_ms: f32,
    pub radius_adjust_px_per_ms: f32,
    pub converge_ms: f32,
    pub impact_flash_ms: f32,
    pub ascend_radius_padding_px: f32,
    pub linger_dismiss_dist_px: f32,
}

impl Default for WaveConfig {
    fn default() -> Self {
        Self {
            arming_hold_ms: ARMING_HOLD_MS,
            arming_drag_slop_px: ARMING_DRAG_SLOP_PX,
            frozen_wheel_debounce_ms: FROZEN_WHEEL_DEBOUNCE_MS,
            wheel_delta_step: WHEEL_DELTA_STEP,
            pointer_deadzone_px: POINTER_DEADZONE_PX,
            pointer_scale: POINTER_SCALE,
            radius_growth_px_per_ms: RADIUS_GROWTH_PX_PER_MS,
            radius_adjust_px_per_ms: RADIUS_ADJUST_PX_PER_MS,
            converge_ms: CONVERGE_MS,
            impact_flash_ms: IMPACT_FLASH_MS,
            ascend_radius_padding_px: ASCEND_RADIUS_PADDING_PX,
            linger_dismiss_dist_px: LINGER_DISMISS_DIST_PX,
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
    /// 射撃後のガラスひび割れ残留（背後操作可）。
    ImpactLingering,
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
    Impact,
    /// 射撃フラッシュ完了後、残留オーバーレイへ。
    Linger,
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
    /// 仮想ポインター先端（スクリーン座標）。
    pub tip_x: f32,
    pub tip_y: f32,
    /// プライマリ波円周上の交点（矢じり＝着弾点）。
    pub target_x: f32,
    pub target_y: f32,
    pub selected_rank: usize,
    pub ranked_ids: Vec<u64>,
    pub progress: f32,
    /// 左クリック射撃経路（ImpactFlash / ImpactLingering 用）。WARP は false。
    pub is_shoot: bool,
    /// ひび割れクリップ用のモニター境界（なければ None）。
    pub clip: Option<AimBounds>,
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
        tip_x: f32,
        tip_y: f32,
        grow_held: bool,
        shrink_held: bool,
    },
    Frozen {
        origin_x: f32,
        origin_y: f32,
        entered_ms: u64,
        radius: f32,
        tip_x: f32,
        tip_y: f32,
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
        tip_x: f32,
        tip_y: f32,
        target_x: f32,
        target_y: f32,
        shoot: bool,
        started_ms: u64,
        last_ms: u64,
        progress: f32,
    },
    ImpactFlash {
        impact_x: f32,
        impact_y: f32,
        origin_x: f32,
        origin_y: f32,
        radius: f32,
        angle_rad: f32,
        shoot: bool,
        started_ms: u64,
        last_ms: u64,
        progress: f32,
    },
    ImpactLingering {
        impact_x: f32,
        impact_y: f32,
        origin_x: f32,
        origin_y: f32,
        radius: f32,
        angle_rad: f32,
    },
    CancelRewind {
        origin_x: f32,
        origin_y: f32,
        radius: f32,
        tip_x: f32,
        tip_y: f32,
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
    aim_bounds: Option<AimBounds>,
}

impl WaveController {
    pub fn new(config: WaveConfig) -> Self {
        Self {
            config,
            phase: Phase::Idle,
            candidates: Vec::new(),
            aim_bounds: None,
        }
    }

    pub fn set_aim_bounds(&mut self, bounds: AimBounds) {
        self.aim_bounds = Some(bounds);
    }

    pub fn phase_kind(&self) -> WavePhaseKind {
        match self.phase {
            Phase::Idle => WavePhaseKind::Idle,
            Phase::Arming { .. } => WavePhaseKind::Arming,
            Phase::Expanding { .. } => WavePhaseKind::Expanding,
            Phase::Frozen { .. } => WavePhaseKind::Frozen,
            Phase::Converging { .. } => WavePhaseKind::Converging,
            Phase::ImpactFlash { .. } => WavePhaseKind::ImpactFlash,
            Phase::ImpactLingering { .. } => WavePhaseKind::ImpactLingering,
            Phase::CancelRewind { .. } => WavePhaseKind::CancelRewind,
        }
    }

    pub fn is_active(&self) -> bool {
        !matches!(self.phase, Phase::Idle)
    }

    pub fn is_lingering(&self) -> bool {
        matches!(self.phase, Phase::ImpactLingering { .. })
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
            tip_x,
            tip_y,
            ..
        } = &mut self.phase
        {
            let (locked, angle) =
                pointer_heading(*origin_x, *origin_y, *tip_x, *tip_y, self.config.pointer_deadzone_px);
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
        let clip = self.aim_bounds;
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
                tip_x: *origin_x,
                tip_y: *origin_y,
                clip,
                ..empty_snap(ranked)
            },
            Phase::Expanding {
                origin_x,
                origin_y,
                radius,
                tip_x,
                tip_y,
                ..
            }
            | Phase::Frozen {
                origin_x,
                origin_y,
                radius,
                tip_x,
                tip_y,
                ..
            }
            | Phase::CancelRewind {
                origin_x,
                origin_y,
                radius,
                tip_x,
                tip_y,
                ..
            } => {
                let (locked, angle) = pointer_heading(
                    *origin_x,
                    *origin_y,
                    *tip_x,
                    *tip_y,
                    self.config.pointer_deadzone_px,
                );
                let (tx, ty) = if locked {
                    ray_circle_intersection(*origin_x, *origin_y, *radius, angle)
                } else {
                    (*origin_x, *origin_y)
                };
                let selected = if let Phase::Frozen { selected_rank, .. } = &self.phase {
                    *selected_rank
                } else {
                    0
                };
                let progress = if let Phase::CancelRewind { progress, .. } = &self.phase {
                    *progress
                } else {
                    0.0
                };
                let phase = match &self.phase {
                    Phase::Expanding { .. } => WavePhaseKind::Expanding,
                    Phase::Frozen { .. } => WavePhaseKind::Frozen,
                    Phase::CancelRewind { .. } => WavePhaseKind::CancelRewind,
                    _ => unreachable!(),
                };
                WaveSnapshot {
                    phase,
                    origin_x: *origin_x,
                    origin_y: *origin_y,
                    radius: *radius,
                    angle_rad: angle,
                    direction_locked: locked,
                    tip_x: *tip_x,
                    tip_y: *tip_y,
                    target_x: tx,
                    target_y: ty,
                    selected_rank: selected,
                    ranked_ids: ranked,
                    progress,
                    is_shoot: false,
                    clip,
                }
            }
            Phase::Converging {
                origin_x,
                origin_y,
                radius,
                tip_x,
                tip_y,
                target_x,
                target_y,
                shoot,
                progress,
                ..
            } => {
                let (locked, angle) = pointer_heading(
                    *origin_x,
                    *origin_y,
                    *tip_x,
                    *tip_y,
                    self.config.pointer_deadzone_px,
                );
                WaveSnapshot {
                    phase: WavePhaseKind::Converging,
                    origin_x: *origin_x,
                    origin_y: *origin_y,
                    radius: *radius,
                    angle_rad: angle,
                    direction_locked: locked,
                    tip_x: *tip_x,
                    tip_y: *tip_y,
                    target_x: *target_x,
                    target_y: *target_y,
                    selected_rank: 0,
                    ranked_ids: ranked,
                    progress: *progress,
                    is_shoot: *shoot,
                    clip,
                }
            }
            Phase::ImpactFlash {
                impact_x,
                impact_y,
                origin_x,
                origin_y,
                radius,
                angle_rad,
                shoot,
                progress,
                ..
            } => WaveSnapshot {
                phase: WavePhaseKind::ImpactFlash,
                origin_x: *origin_x,
                origin_y: *origin_y,
                radius: *radius,
                angle_rad: *angle_rad,
                direction_locked: true,
                tip_x: *impact_x,
                tip_y: *impact_y,
                target_x: *impact_x,
                target_y: *impact_y,
                progress: *progress,
                is_shoot: *shoot,
                clip,
                ..empty_snap(ranked)
            },
            Phase::ImpactLingering {
                impact_x,
                impact_y,
                origin_x,
                origin_y,
                radius,
                angle_rad,
            } => WaveSnapshot {
                phase: WavePhaseKind::ImpactLingering,
                origin_x: *origin_x,
                origin_y: *origin_y,
                radius: *radius,
                angle_rad: *angle_rad,
                direction_locked: true,
                tip_x: *impact_x,
                tip_y: *impact_y,
                target_x: *impact_x,
                target_y: *impact_y,
                progress: 1.0,
                is_shoot: true,
                clip,
                ..empty_snap(ranked)
            },
        }
    }

    pub fn aim_point(&self) -> Option<(f32, f32)> {
        let s = self.snapshot();
        match s.phase {
            WavePhaseKind::Idle | WavePhaseKind::Arming => None,
            WavePhaseKind::ImpactFlash
            | WavePhaseKind::ImpactLingering
            | WavePhaseKind::Converging => Some((s.target_x, s.target_y)),
            _ if s.direction_locked && s.radius > 0.0 => Some((s.target_x, s.target_y)),
            _ => None,
        }
    }

    pub fn selected_candidate(&self) -> Option<&WaveCandidate> {
        let s = self.snapshot();
        let id = s.ranked_ids.get(s.selected_rank)?;
        self.candidates.iter().find(|c| c.id == *id)
    }

    fn ranked_ids(&self) -> Vec<u64> {
        match &self.phase {
            Phase::Idle
            | Phase::Arming { .. }
            | Phase::ImpactFlash { .. }
            | Phase::ImpactLingering { .. } => Vec::new(),
            Phase::Expanding {
                origin_x,
                origin_y,
                radius,
                tip_x,
                tip_y,
                ..
            }
            | Phase::Frozen {
                origin_x,
                origin_y,
                radius,
                tip_x,
                tip_y,
                ..
            }
            | Phase::CancelRewind {
                origin_x,
                origin_y,
                radius,
                tip_x,
                tip_y,
                ..
            } => {
                let (locked, angle) = pointer_heading(
                    *origin_x,
                    *origin_y,
                    *tip_x,
                    *tip_y,
                    self.config.pointer_deadzone_px,
                );
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
        // 残留中の新規波動起動は残留を破棄してから Arming
        if matches!(self.phase, Phase::ImpactLingering { .. }) {
            self.phase = Phase::Idle;
        }
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
                tip_x,
                tip_y,
                grow_held,
                shrink_held,
                ..
            } => {
                self.phase = Phase::Frozen {
                    origin_x,
                    origin_y,
                    entered_ms: clock_ms,
                    radius,
                    tip_x,
                    tip_y,
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
                tip_x, tip_y, ..
            }
            | Phase::Frozen {
                tip_x, tip_y, ..
            } => {
                let scale = self.config.pointer_scale;
                let mut nx = *tip_x + dx * scale;
                let mut ny = *tip_y + dy * scale;
                if let Some(b) = self.aim_bounds {
                    let (cx, cy) = b.clamp_point(nx, ny);
                    nx = cx;
                    ny = cy;
                }
                *tip_x = nx;
                *tip_y = ny;
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
        if matches!(self.phase, Phase::ImpactLingering { .. }) {
            self.phase = Phase::Idle;
            return WaveSignal::Finished;
        }
        if matches!(self.phase, Phase::Frozen { .. }) {
            self.begin_converge(clock_ms, true)
        } else {
            WaveSignal::None
        }
    }

    pub fn on_right_click(&mut self, clock_ms: u64) -> WaveSignal {
        if matches!(self.phase, Phase::ImpactLingering { .. }) {
            self.phase = Phase::Idle;
            return WaveSignal::Finished;
        }
        match self.phase {
            Phase::Expanding {
                origin_x,
                origin_y,
                radius,
                tip_x,
                tip_y,
                ..
            }
            | Phase::Frozen {
                origin_x,
                origin_y,
                radius,
                tip_x,
                tip_y,
                ..
            } => {
                self.phase = Phase::CancelRewind {
                    origin_x,
                    origin_y,
                    radius,
                    tip_x,
                    tip_y,
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
                        tip_x: origin_x,
                        tip_y: origin_y,
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
                origin_x,
                origin_y,
                radius,
                tip_x,
                tip_y,
                ..
            } => {
                let dt = clock_ms.saturating_sub(*last_ms) as f32;
                *last_ms = clock_ms;
                *progress = (*progress + dt / self.config.converge_ms).min(1.0);
                if *progress >= 1.0 {
                    let shoot = *shoot;
                    let impact_x = *target_x;
                    let impact_y = *target_y;
                    let origin_x = *origin_x;
                    let origin_y = *origin_y;
                    let radius = *radius;
                    let tip_x = *tip_x;
                    let tip_y = *tip_y;
                    let (_, angle_fallback) = pointer_heading(
                        origin_x,
                        origin_y,
                        tip_x,
                        tip_y,
                        self.config.pointer_deadzone_px,
                    );
                    // tip がデッドゾーン内でも着弾方位は原点→着弾で固定
                    let angle_rad = if (impact_x - origin_x).hypot(impact_y - origin_y) > 0.5 {
                        (impact_y - origin_y)
                            .atan2(impact_x - origin_x)
                            .rem_euclid(TAU)
                    } else {
                        angle_fallback
                    };
                    self.phase = Phase::ImpactFlash {
                        impact_x,
                        impact_y,
                        origin_x,
                        origin_y,
                        radius,
                        angle_rad,
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
                shoot,
                impact_x,
                impact_y,
                origin_x,
                origin_y,
                radius,
                angle_rad,
                ..
            } => {
                let dt = clock_ms.saturating_sub(*last_ms) as f32;
                *last_ms = clock_ms;
                *progress = (*progress + dt / self.config.impact_flash_ms).min(1.0);
                if *progress >= 1.0 {
                    if *shoot {
                        self.phase = Phase::ImpactLingering {
                            impact_x: *impact_x,
                            impact_y: *impact_y,
                            origin_x: *origin_x,
                            origin_y: *origin_y,
                            radius: *radius,
                            angle_rad: *angle_rad,
                        };
                        return WaveSignal::Linger;
                    }
                    self.phase = Phase::Idle;
                    return WaveSignal::Finished;
                }
            }
            Phase::ImpactLingering { .. } => {}
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

    /// 残留中にカーソル絶対座標を渡し、離れすぎたら解除。
    pub fn on_cursor_sample(&mut self, x: f32, y: f32) -> WaveSignal {
        let Phase::ImpactLingering {
            impact_x, impact_y, ..
        } = self.phase
        else {
            return WaveSignal::None;
        };
        let dist = (x - impact_x).hypot(y - impact_y);
        if dist >= self.config.linger_dismiss_dist_px {
            self.phase = Phase::Idle;
            WaveSignal::Finished
        } else {
            WaveSignal::None
        }
    }

    fn begin_converge(&mut self, clock_ms: u64, shoot: bool) -> WaveSignal {
        let Phase::Frozen {
            origin_x,
            origin_y,
            radius,
            tip_x,
            tip_y,
            selected_rank,
            ..
        } = self.phase
        else {
            return WaveSignal::None;
        };
        let (locked, angle) =
            pointer_heading(origin_x, origin_y, tip_x, tip_y, self.config.pointer_deadzone_px);
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
                .unwrap_or_else(|| {
                    if locked {
                        ray_circle_intersection(origin_x, origin_y, radius, angle)
                    } else {
                        (origin_x + radius, origin_y)
                    }
                })
        } else if locked {
            ray_circle_intersection(origin_x, origin_y, radius, angle)
        } else {
            (origin_x + radius, origin_y)
        };
        self.phase = Phase::Converging {
            origin_x,
            origin_y,
            radius,
            tip_x,
            tip_y,
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
        tip_x: 0.0,
        tip_y: 0.0,
        target_x: 0.0,
        target_y: 0.0,
        selected_rank: 0,
        ranked_ids: ranked,
        progress: 0.0,
        is_shoot: false,
        clip: None,
    }
}

pub fn pointer_heading(
    origin_x: f32,
    origin_y: f32,
    tip_x: f32,
    tip_y: f32,
    deadzone: f32,
) -> (bool, f32) {
    let dx = tip_x - origin_x;
    let dy = tip_y - origin_y;
    let len = (dx * dx + dy * dy).sqrt();
    if len < deadzone {
        (false, 0.0)
    } else {
        (true, dy.atan2(dx).rem_euclid(TAU))
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

/// 波源から現在モニターの最も遠い角までの距離 + padding。
pub fn max_radius_to_farthest_corner(
    origin_x: f32,
    origin_y: f32,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    padding: f32,
) -> f32 {
    let corners = [
        (left, top),
        (right, top),
        (left, bottom),
        (right, bottom),
    ];
    let farthest = corners
        .into_iter()
        .map(|(cx, cy)| (cx - origin_x).hypot(cy - origin_y))
        .fold(0.0_f32, f32::max);
    (farthest + padding).max(1.0)
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
        w.on_middle_down(10.0, 20.0, 0);
        assert_eq!(w.on_middle_up(50), WaveSignal::PassthroughClick);
    }

    #[test]
    fn pointer_tip_matches_cursor_axes() {
        let mut w = ctrl();
        w.set_aim_bounds(AimBounds {
            left: 0.0,
            top: 0.0,
            right: 1000.0,
            bottom: 1000.0,
        });
        w.on_middle_down(100.0, 100.0, 0);
        w.on_tick(130, 500.0);
        w.on_tick(200, 500.0); // 半径を伸ばして円周交点を検証可能に
        w.on_relative_move(50.0, 0.0, 210);
        let s = w.snapshot();
        assert!(s.direction_locked);
        assert!((s.tip_x - (100.0 + 50.0 * 1.0)).abs() < 0.5);
        assert!((s.angle_rad).abs() < 0.05 || (TAU - s.angle_rad).abs() < 0.05);
        assert!(s.radius > 1.0);
        let (tx, ty) = (s.target_x, s.target_y);
        assert!(tx > s.origin_x);
        assert!((ty - s.origin_y).abs() < 1.0);
    }

    #[test]
    fn pointer_clamps_to_monitor_bounds() {
        let mut w = ctrl();
        w.set_aim_bounds(AimBounds {
            left: 0.0,
            top: 0.0,
            right: 200.0,
            bottom: 200.0,
        });
        w.on_middle_down(100.0, 100.0, 0);
        w.on_tick(130, 500.0);
        w.on_relative_move(500.0, 0.0, 140);
        let s = w.snapshot();
        assert!(s.tip_x <= 199.0);
    }

    #[test]
    fn deadzone_keeps_direction_unlocked() {
        let (locked, _) = pointer_heading(0.0, 0.0, 10.0, 0.0, 16.0);
        assert!(!locked);
        let (locked, _) = pointer_heading(0.0, 0.0, 20.0, 0.0, 16.0);
        assert!(locked);
    }

    #[test]
    fn farthest_corner_radius_covers_monitor() {
        // origin near top-left; farthest is bottom-right
        let r = max_radius_to_farthest_corner(10.0, 10.0, 0.0, 0.0, 400.0, 300.0, 40.0);
        let expected = (400.0_f32 - 10.0).hypot(300.0 - 10.0) + 40.0;
        assert!((r - expected).abs() < 1e-3);
        // nearest-edge would be ~10; farthest is much larger
        assert!(r > 200.0);
    }

    #[test]
    fn warp_path_still_impacts() {
        let mut w = WaveController::new(WaveConfig {
            converge_ms: 80.0,
            impact_flash_ms: 40.0,
            ..WaveConfig::default()
        });
        w.set_aim_bounds(AimBounds {
            left: 0.0,
            top: 0.0,
            right: 800.0,
            bottom: 600.0,
        });
        w.on_middle_down(100.0, 100.0, 0);
        w.on_tick(130, 800.0);
        w.on_relative_move(40.0, 0.0, 140);
        w.on_middle_up(150);
        assert_eq!(w.on_middle_up(200), WaveSignal::ConvergeWarp);
        assert!(!w.snapshot().is_shoot);
        assert_eq!(w.on_tick(300, 800.0), WaveSignal::Impact);
        assert!(!w.snapshot().is_shoot);
        assert_eq!(w.on_tick(360, 800.0), WaveSignal::Finished);
    }

    #[test]
    fn shoot_path_lingers_then_dismisses_on_click() {
        let mut w = WaveController::new(WaveConfig {
            converge_ms: 80.0,
            impact_flash_ms: 40.0,
            ..WaveConfig::default()
        });
        w.set_aim_bounds(AimBounds {
            left: 0.0,
            top: 0.0,
            right: 800.0,
            bottom: 600.0,
        });
        w.on_middle_down(100.0, 100.0, 0);
        w.on_tick(130, 800.0);
        w.on_relative_move(40.0, 0.0, 140);
        w.on_middle_up(150);
        assert_eq!(w.on_left_click(200), WaveSignal::ConvergeShoot);
        assert_eq!(w.on_tick(300, 800.0), WaveSignal::Impact);
        assert_eq!(w.snapshot().phase, WavePhaseKind::ImpactFlash);
        assert!(w.snapshot().is_shoot);
        assert_eq!(w.on_tick(360, 800.0), WaveSignal::Linger);
        assert_eq!(w.snapshot().phase, WavePhaseKind::ImpactLingering);
        assert_eq!(w.on_left_click(400), WaveSignal::Finished);
        assert_eq!(w.phase_kind(), WavePhaseKind::Idle);
    }

    #[test]
    fn linger_dismisses_when_cursor_leaves() {
        let mut w = WaveController::new(WaveConfig {
            converge_ms: 40.0,
            impact_flash_ms: 20.0,
            linger_dismiss_dist_px: 36.0,
            ..WaveConfig::default()
        });
        w.set_aim_bounds(AimBounds {
            left: 0.0,
            top: 0.0,
            right: 800.0,
            bottom: 600.0,
        });
        w.on_middle_down(100.0, 100.0, 0);
        w.on_tick(130, 800.0);
        w.on_relative_move(40.0, 0.0, 140);
        w.on_middle_up(150);
        w.on_left_click(160);
        assert_eq!(w.on_tick(220, 800.0), WaveSignal::Impact);
        assert_eq!(w.on_tick(260, 800.0), WaveSignal::Linger);
        let impact = w.snapshot();
        assert_eq!(
            w.on_cursor_sample(impact.target_x + 10.0, impact.target_y),
            WaveSignal::None
        );
        assert_eq!(
            w.on_cursor_sample(impact.target_x + 40.0, impact.target_y),
            WaveSignal::Finished
        );
    }

    #[test]
    fn expanding_to_farthest_ascends() {
        let mut w = WaveController::new(WaveConfig {
            radius_growth_px_per_ms: 50.0,
            ..WaveConfig::default()
        });
        w.on_middle_down(0.0, 0.0, 0);
        w.on_tick(130, 100.0);
        assert_eq!(w.on_tick(140, 100.0), WaveSignal::AscendToMap);
    }
}
