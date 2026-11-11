use std::collections::HashSet;

use bevy_math::{DVec3, Vec3};
use serde::{Deserialize, Serialize};

use crate::{
    brush_index::BrushIndex,
    course::{AuthoredZoneGeometry, Brush, BrushFriction, RecordedBrush, source_dot},
    rules::MovementRules,
};

#[path = "movement_volumes.rs"]
mod volumes;

// Source's "66 tick" movement advances by exactly 15 ms each command.
pub const TICK_HZ: f64 = 1.0 / TICK_SECONDS as f64;
pub const TICK_SECONDS: f32 = 0.015;
pub const EYE_HEIGHT: f32 = 64.0;

const HALF_WIDTH: f32 = 16.0;
const STAND_HEIGHT: f32 = 62.0;
const CROUCH_HEIGHT: f32 = 45.0;
pub(crate) const CROUCH_EYE_HEIGHT: f32 = 47.0;
const CROUCH_SHIFT: f32 = (STAND_HEIGHT - CROUCH_HEIGHT) / 2.0;
const CROUCH_SECONDS: f32 = 0.4;
const UNCROUCH_SECONDS: f32 = 0.2;

const GRAVITY: f32 = 800.0;
pub(crate) const HALF_GRAVITY_STEP: f32 = GRAVITY * TICK_SECONDS * 0.5;
const AIR_ACCELERATION: f32 = 150.0;
const GROUND_ACCELERATION: f32 = 5.0;
const GROUND_FRICTION: f32 = 4.0;
const STOP_SPEED: f32 = 75.0;
pub(crate) const RUN_SPEED: f32 = 260.0;
const AIR_WISH_CAP: f32 = 30.0;
const JUMP_IMPULSE: f32 = 301.99338;
const WALKABLE_NORMAL: f32 = 0.7;
const GROUND_PROBE: f32 = 2.0;
const STEP_HEIGHT: f32 = 18.0;
const CONTACT_EPSILON: f32 = 0.03125;
const MAX_MOVEMENT_BUMPS: usize = 4;

#[derive(Clone, Copy, Debug)]
pub struct Player {
    /// World position at the center of the player's feet, in Source units.
    pub position: Vec3,
    pub velocity: Vec3,
    pub grounded: bool,
    crouched: bool,
    crouch_fraction: f32,
    surface_friction: f32,
    needs_ground_probe: bool,
    jump_held: bool,
    ladder_normal: Option<DVec3>,
    water_jump: Option<volumes::WaterJump>,
    pub(crate) ground_brush: Option<usize>,
}

impl Player {
    pub fn new(position: Vec3) -> Self {
        Self {
            position,
            velocity: Vec3::ZERO,
            grounded: false,
            crouched: false,
            crouch_fraction: 0.0,
            surface_friction: 1.0,
            needs_ground_probe: true,
            jump_held: false,
            ladder_normal: None,
            water_jump: None,
            ground_brush: None,
        }
    }

    pub fn eye_height(&self) -> f32 {
        let fraction = self.crouch_fraction;
        let eased = fraction * fraction * (3.0 - 2.0 * fraction);
        EYE_HEIGHT + (CROUCH_EYE_HEIGHT - EYE_HEIGHT) * eased
    }

    pub fn is_crouched(&self) -> bool {
        self.crouched
    }

    pub fn hull_bounds(&self) -> (Vec3, Vec3) {
        let hull = self.hull();
        (self.position + hull.min, self.position + hull.max)
    }

    fn hull(&self) -> Hull {
        if self.crouched {
            Hull::new(CROUCH_HEIGHT)
        } else {
            Hull::new(STAND_HEIGHT)
        }
    }

    pub(crate) fn detach_ground(&mut self) {
        self.grounded = false;
        self.ground_brush = None;
        self.surface_friction = 1.0;
    }

    pub(crate) fn release_crouch(&mut self) {
        self.crouched = false;
        self.crouch_fraction = 0.0;
    }

    pub(crate) fn at_stage_restart(position: Vec3) -> Self {
        Self {
            velocity: Vec3::NEG_Y * HALF_GRAVITY_STEP,
            needs_ground_probe: false,
            ..Self::new(position)
        }
    }

    pub(crate) fn apply_recorded_state(
        &mut self,
        state: &crate::session::RecordedInitialState,
        ground_brush: Option<usize>,
    ) {
        self.position = state.position;
        self.velocity = state.velocity;
        self.grounded = state.grounded;
        self.ground_brush = ground_brush;
        self.crouched = state.crouched;
        self.crouch_fraction = state.crouch_fraction;
        self.surface_friction = state.surface_friction;
        self.jump_held = state.jump_held;
        self.ladder_normal = None;
        self.water_jump = None;
        self.needs_ground_probe = false;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MoveInput {
    /// Signed movement axes in the range -1 to 1.
    pub forward: f32,
    pub strafe: f32,
    /// In radians, facing -Z at zero and rotating toward -X.
    pub yaw: f32,
    pub jump: bool,
    pub crouch: bool,
}

/// Wrap Source yaw exactly as the original input controller does.
pub fn normalize_source_yaw(degrees: f64) -> f64 {
    if (-180.0..180.0).contains(&degrees) {
        degrees
    } else {
        ((degrees + 180.0) % 360.0 + 360.0) % 360.0 - 180.0
    }
}

pub fn source_yaw_to_native(degrees: f64) -> f32 {
    (degrees - 90.0)
        .to_radians()
        .rem_euclid(std::f64::consts::TAU) as f32
}

pub fn native_yaw_to_source(radians: f32) -> f64 {
    normalize_source_yaw(f64::from(radians).to_degrees() + 90.0)
}

/// Advances one fixed movement tick against static convex brushes.
pub fn step(player: &mut Player, input: MoveInput, brushes: &[Brush]) {
    step_with_index(
        player,
        input,
        brushes,
        None,
        &mut Vec::new(),
        StepRules::default(),
    );
}

pub(crate) struct StepRules<'a> {
    pub movement: MovementRules,
    pub auto_bhop: bool,
    pub blocked_start_jump: Option<&'a Brush>,
    pub no_jump_scale: Option<f32>,
    pub movement_scale: Option<f32>,
    pub gravity_scale: Option<f32>,
    pub jump_disabled: bool,
    pub push: Option<Vec3>,
    pub source_push_ground_fix: bool,
    pub recheck_ground: Option<GroundCheck>,
    pub friction: &'a [BrushFriction],
    pub recorded_brushes: &'a [RecordedBrush],
    pub vphysics_feature_fix: bool,
    pub bsp_corner_fix: bool,
    pub exact_bonus_contacts: bool,
    pub downhill_landing: bool,
    pub recorded_yaw: Option<f64>,
    pub recorded_axes: Option<[f64; 2]>,
    pub pitch: f32,
    pub ladder_ids: Option<&'a HashSet<String>>,
    pub water_hulls: &'a [AuthoredZoneGeometry],
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum GroundCheck {
    Push,
    Teleport,
}

impl Default for StepRules<'_> {
    fn default() -> Self {
        Self {
            movement: MovementRules::default(),
            auto_bhop: true,
            blocked_start_jump: None,
            no_jump_scale: None,
            movement_scale: None,
            gravity_scale: None,
            jump_disabled: false,
            push: None,
            source_push_ground_fix: false,
            recheck_ground: None,
            friction: &[],
            recorded_brushes: &[],
            vphysics_feature_fix: false,
            bsp_corner_fix: false,
            exact_bonus_contacts: false,
            downhill_landing: false,
            recorded_yaw: None,
            recorded_axes: None,
            pitch: 0.0,
            ladder_ids: None,
            water_hulls: &[],
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct MovementSegment {
    pub from: Vec3,
    pub to: Vec3,
    pub start: f64,
    pub end: f64,
}

#[derive(Default)]
pub(crate) struct MovementSegments {
    entries: [MovementSegment; MAX_MOVEMENT_BUMPS],
    len: usize,
}

impl MovementSegments {
    pub(crate) fn as_slice(&self) -> &[MovementSegment] {
        &self.entries[..self.len]
    }

    fn push(&mut self, segment: MovementSegment) {
        self.entries[self.len] = segment;
        self.len += 1;
    }

    fn last_mut(&mut self) -> Option<&mut MovementSegment> {
        self.entries[..self.len].last_mut()
    }
}

#[derive(Default)]
pub(crate) struct MovementResult {
    pub jumped: bool,
    pub blocked: bool,
    pub segments: MovementSegments,
    pub pre_collision_velocity: Vec3,
    pub had_contact: bool,
    pub landed: bool,
    pub velocity_before_downhill_landing: Option<Vec3>,
    pub cleared_gravity_scale: bool,
}

pub(crate) fn step_with_index(
    player: &mut Player,
    mut input: MoveInput,
    brushes: &[Brush],
    index: Option<&BrushIndex>,
    candidates: &mut Vec<usize>,
    rules: StepRules<'_>,
) -> MovementResult {
    let mut collision = CollisionWorld {
        brushes,
        index: index.filter(|index| index.covers(brushes)),
        candidates,
        candidate_bounds: None,
        query_margin: GROUND_PROBE + CONTACT_EPSILON,
        incline_fix: rules.movement.incline_fix,
        dt: rules
            .no_jump_scale
            .or(rules.movement_scale)
            .map_or(TICK_SECONDS, |scale| TICK_SECONDS * scale),
        friction: rules.friction,
        contact_rules: BrushContactRules {
            exact_bonus_contacts: rules.exact_bonus_contacts,
            recorded: rules.recorded_brushes,
            prop_features: rules.vphysics_feature_fix,
            box_corners: rules.bsp_corner_fix,
        },
    };
    let mut result = MovementResult::default();
    if player.needs_ground_probe || rules.recheck_ground.is_some() {
        categorize_ground(player, &mut collision);
        player.needs_ground_probe = false;
    }
    if rules.no_jump_scale.is_some()
        || rules.jump_disabled
        || (rules.recheck_ground == Some(GroundCheck::Teleport) && player.grounded)
    {
        input.jump = false;
    }
    if let Some(start) = rules.blocked_start_jump {
        input.jump = false;
        if player.grounded {
            settle_start_hop(player, start, &mut collision);
        }
    }
    let push = rules.push.map_or(Vec3::ZERO, |mut push| {
        if rules.source_push_ground_fix && player.grounded && input.jump {
            push.y = 0.0;
        } else if !rules.source_push_ground_fix && push.y > 0.0 && player.grounded {
            player.grounded = false;
            player.ground_brush = None;
            player.position.y += 1.0;
        }
        push
    });
    if rules.ladder_ids.is_some() && player.ladder_normal.is_some() {
        if player.velocity.y > 0.0 {
            player.detach_ground();
        } else {
            categorize_ground(player, &mut collision);
        }
    }
    update_crouch(player, input.crouch, &mut collision);
    if let Some(ladder_ids) = rules.ladder_ids {
        if volumes::ladder_move(
            player,
            input,
            &rules,
            ladder_ids,
            &mut collision,
            &mut result,
        ) {
            player.jump_held = input.jump;
            player.velocity += push;
            slide(player, &mut collision, &mut result, true);
            player.velocity -= push;
            return result;
        }
        player.ladder_normal = None;
    }
    let started_grounded = player.grounded;

    let dt = collision.dt;
    let gravity_scale = if result.cleared_gravity_scale {
        1.0
    } else {
        rules.gravity_scale.unwrap_or(1.0)
    };
    let half_gravity = (GRAVITY * gravity_scale) * dt * 0.5;
    if !rules.water_hulls.is_empty()
        && volumes::water_tick(
            player,
            input,
            &rules,
            push,
            half_gravity,
            &mut collision,
            &mut result,
        )
    {
        return result;
    }
    player.velocity.y -= half_gravity;
    player.velocity.y += push.y * dt;
    clamp_velocity(&mut player.velocity, rules.movement.max_velocity);
    if input.jump && player.grounded && (!player.jump_held || rules.auto_bhop) {
        player.grounded = false;
        player.ground_brush = None;
        if player.crouch_fraction > 0.0 {
            player.velocity.y = 0.0;
        }
        player.velocity.y += JUMP_IMPULSE;
        player.velocity.y -= half_gravity;
        result.jumped = true;
    }
    player.jump_held = input.jump;
    if player.grounded {
        player.velocity.y = 0.0;
        let velocity = player.velocity.as_dvec3();
        let speed = velocity.x.hypot(velocity.z);
        if speed >= 0.1 {
            let drop = speed.max(f64::from(STOP_SPEED))
                * f64::from(GROUND_FRICTION)
                * f64::from(player.surface_friction)
                * f64::from(dt);
            player.velocity = (velocity * ((speed - drop).max(0.0) / speed)).as_vec3();
        }
    }

    let (direction, wish_speed) =
        wish_direction(player, input, rules.recorded_yaw, rules.recorded_axes);
    let (target_speed, acceleration) = if player.grounded {
        (wish_speed, GROUND_ACCELERATION)
    } else {
        (wish_speed.min(AIR_WISH_CAP), AIR_ACCELERATION)
    };
    let direction = direction.as_dvec3();
    let velocity = player.velocity.as_dvec3();
    let speed_needed = target_speed - source_dot(velocity, direction) as f32;
    if speed_needed > 0.0 {
        // Source caps the target projection, while acceleration uses the uncapped wish speed.
        let gain = acceleration * dt * wish_speed * player.surface_friction;
        player.velocity = (velocity + direction * f64::from(gain.min(speed_needed))).as_vec3();
    }

    let was_grounded = player.grounded;
    player.velocity += push.with_y(0.0);
    result.pre_collision_velocity = player.velocity;
    let downhill_start =
        (rules.downhill_landing && !started_grounded && !was_grounded).then_some(player.position);
    if player.grounded {
        walk(player, &mut collision, &mut result);
    } else {
        slide(player, &mut collision, &mut result, false);
    }

    player.velocity -= push.with_y(0.0);
    let ground_normal = categorize_ground(player, &mut collision);
    result.landed = !was_grounded && player.grounded;
    clamp_velocity(&mut player.velocity, rules.movement.max_velocity);
    if rules.water_hulls.is_empty() || volumes::water_level(player, rules.water_hulls) < 2 {
        player.velocity.y -= half_gravity;
    }
    clamp_velocity(&mut player.velocity, rules.movement.max_velocity);
    if player.grounded {
        player.velocity.y = 0.0;
        if let (Some(start), Some(normal)) = (downhill_start, ground_normal)
            && let Some(velocity) = downhill_landing_velocity(
                start,
                result.pre_collision_velocity,
                normal,
                player.hull(),
                push,
                &mut collision,
            )
        {
            result.velocity_before_downhill_landing = Some(player.velocity);
            player.velocity = velocity;
        }
    }
    result
}

fn downhill_landing_velocity(
    start: Vec3,
    velocity: Vec3,
    normal: DVec3,
    hull: Hull,
    push: Vec3,
    collision: &mut CollisionWorld<'_>,
) -> Option<Vec3> {
    if velocity.y > 0.0 || normal.y == 1.0 {
        return None;
    }
    // A ground probe can find a downhill surface before the move itself hits it.
    let start = start.as_dvec3();
    let end = start + (velocity * collision.dt).as_dvec3();
    let sweep = trace_precise(start, end, hull, collision);
    if sweep.fraction < 1.0 || sweep.start_solid {
        return None;
    }

    // Round the products and additions individually in Source axis order.
    let mul = |component: f32, axis: f64| (f64::from(component) * axis) as f32;
    let approach = mul(velocity.x, normal.x) + mul(velocity.z, normal.z);
    let into = approach + mul(velocity.y, normal.y);
    let projected = Vec3::new(
        velocity.x - mul(into, normal.x),
        0.0,
        velocity.z - mul(into, normal.z),
    );
    if approach < 0.0 && horizontal_speed_squared(projected) <= horizontal_speed_squared(velocity) {
        return None;
    }
    Some(projected - push.with_y(0.0))
}

fn settle_start_hop(player: &mut Player, zone: &Brush, collision: &mut CollisionWorld<'_>) {
    let hull = player.hull();
    let floor = trace(
        player.position,
        player.position - Vec3::Y * 96.0,
        hull,
        collision,
    );
    if floor.fraction < 1.0
        && floor.walkable()
        && !floor.start_solid
        && zone.planes.iter().all(|plane| {
            floor.end.dot(plane.normal) <= hull.expanded_distance(plane.normal, plane.distance)
        })
    {
        player.position = floor.end;
        player.velocity.y = 0.0;
        categorize_ground(player, collision);
    }
}

fn wish_direction(
    player: &Player,
    input: MoveInput,
    recorded_yaw: Option<f64>,
    recorded_axes: Option<[f64; 2]>,
) -> (Vec3, f32) {
    let command = |axis: f64| {
        if axis.is_finite() {
            axis.clamp(-400.0, 400.0)
        } else {
            0.0
        }
    };
    let (sin_yaw, cos_yaw) = if let Some(degrees) = recorded_yaw {
        (degrees * std::f64::consts::PI / 180.0).sin_cos()
    } else if input.yaw.is_finite() {
        (f64::from(input.yaw) + std::f64::consts::FRAC_PI_2).sin_cos()
    } else {
        (1.0, 0.0)
    };
    let [forward, strafe] = recorded_axes
        .unwrap_or_else(|| {
            [
                f64::from(input.forward) * 400.0,
                f64::from(input.strafe) * 400.0,
            ]
        })
        .map(command);
    let wish = DVec3::new(
        cos_yaw * forward + sin_yaw * strafe,
        0.0,
        -(sin_yaw * forward - cos_yaw * strafe),
    );
    let speed = wish.x.hypot(wish.z).min(f64::from(RUN_SPEED));
    let speed = if input.crouch || player.crouched || player.crouch_fraction > 0.0 {
        speed * 0.34
    } else {
        speed
    };
    (wish.normalize_or_zero().as_vec3(), speed as f32)
}

fn clamp_velocity(velocity: &mut Vec3, limit: f32) {
    for axis in 0..3 {
        let speed = velocity[axis];
        velocity[axis] = if speed.is_finite() {
            speed.clamp(-limit, limit)
        } else {
            0.0
        };
    }
}

fn categorize_ground(player: &mut Player, collision: &mut CollisionWorld<'_>) -> Option<DVec3> {
    player.detach_ground();
    if player.velocity.y > 140.0 {
        return None;
    }

    let end = player.position - Vec3::Y * GROUND_PROBE;
    let hull = player.hull();
    let mut ground = trace(player.position, end, hull, collision);
    let probe_quarters = if collision.contact_rules.exact_bonus_contacts {
        ground.brush.is_none() || ground.normal.y < f64::from(WALKABLE_NORMAL)
    } else {
        !ground.walkable()
    };
    if probe_quarters && ground.broadphase_overlap {
        for (min_x, max_x, min_z, max_z) in [
            (-HALF_WIDTH, 0.0, -HALF_WIDTH, 0.0),
            (0.0, HALF_WIDTH, 0.0, HALF_WIDTH),
            (-HALF_WIDTH, 0.0, 0.0, HALF_WIDTH),
            (0.0, HALF_WIDTH, -HALF_WIDTH, 0.0),
        ] {
            let quarter_hull = Hull {
                min: Vec3::new(min_x, 0.0, min_z),
                max: Vec3::new(max_x, hull.max.y, max_z),
            };
            let contact = trace(player.position, end, quarter_hull, collision);
            if contact.walkable() {
                ground = contact;
                break;
            }
        }
    }

    if ground.walkable() {
        player.grounded = true;
        player.ground_brush = ground.brush_index;
        if let Some(friction) = ground
            .brush_index
            .and_then(|index| collision.friction.get(index))
        {
            let coefficient = ground
                .plane_index
                .and_then(|index| friction.planes.get(index).copied().flatten())
                .unwrap_or(friction.brush);
            player.surface_friction = (coefficient * 1.25).clamp(0.0, 1.0);
        }
        return Some(ground.normal);
    } else if player.velocity.y > 0.0 {
        player.surface_friction = 0.25;
    }
    None
}
fn update_crouch(player: &mut Player, wants_crouch: bool, collision: &mut CollisionWorld<'_>) {
    if wants_crouch {
        player.crouch_fraction = (player.crouch_fraction + collision.dt / CROUCH_SECONDS).min(1.0);
        if !player.crouched && (!player.grounded || player.crouch_fraction == 1.0) {
            if !player.grounded {
                player.position.y += CROUCH_SHIFT;
            }
            player.crouched = true;
            player.crouch_fraction = 1.0;
            categorize_ground(player, collision);
        }
        return;
    }

    if player.crouch_fraction > 0.0 {
        let mut stand_position = player.position;
        if player.crouched && !player.grounded {
            stand_position.y -= CROUCH_SHIFT;
        }
        let stand_test = trace(
            stand_position,
            stand_position,
            Hull::new(STAND_HEIGHT),
            collision,
        );
        if stand_test.start_solid {
            player.crouch_fraction = 1.0;
            return;
        }
        player.crouch_fraction =
            (player.crouch_fraction - collision.dt / UNCROUCH_SECONDS).max(0.0);
        if player.crouched && (!player.grounded || player.crouch_fraction == 0.0) {
            player.position = stand_position;
            player.crouched = false;
            player.crouch_fraction = 0.0;
            categorize_ground(player, collision);
        }
    }
}

fn walk(player: &mut Player, collision: &mut CollisionWorld<'_>, result: &mut MovementResult) {
    player.velocity.y = 0.0;
    if player.velocity.length_squared() < 1.0 {
        player.velocity = Vec3::ZERO;
        return;
    }

    let start = *player;
    if slide(player, collision, result, false) {
        let flat = *player;
        let flat_segments = std::mem::take(&mut result.segments);
        let flat_blocked = std::mem::take(&mut result.blocked);
        let flat_contact = std::mem::take(&mut result.had_contact);
        let hull = player.hull();
        *player = start;
        let rise = Vec3::Y * (STEP_HEIGHT + CONTACT_EPSILON);
        let up = trace(start.position, start.position + rise, hull, collision);
        if !up.start_solid {
            player.position = up.end;
        }
        slide(player, collision, result, false);
        let down = trace(player.position, player.position - rise, hull, collision);
        if !down.start_solid {
            player.position = down.end;
        }

        let flat_progress = horizontal_speed_squared(flat.position - start.position);
        let step_progress = horizontal_speed_squared(player.position - start.position);
        if !down.walkable() || flat_progress > step_progress {
            *player = flat;
            result.segments = flat_segments;
            result.blocked = flat_blocked;
            result.had_contact = flat_contact;
        } else {
            player.velocity.y = flat.velocity.y;
        }
    }

    let hull = player.hull();
    let up = trace(
        player.position,
        player.position + Vec3::Y * GROUND_PROBE,
        hull,
        collision,
    );
    let target = player.position - Vec3::Y * STEP_HEIGHT;
    let (down, height_delta) = if collision.contact_rules.exact_bonus_contacts {
        // The source compares the unrounded sweep height to its snap threshold.
        let start = player.position.as_dvec3() + DVec3::Y * f64::from(GROUND_PROBE) * up.fraction;
        let target = target.as_dvec3();
        let down = trace_precise(start, target, hull, collision);
        let height = start.y + (target.y - start.y) * down.fraction;
        (down, (height - f64::from(player.position.y)).abs())
    } else {
        let down = trace(up.end, target, hull, collision);
        let height_delta = f64::from((down.end.y - player.position.y).abs());
        (down, height_delta)
    };
    if down.fraction > 0.0
        && down.walkable()
        && !down.start_solid
        && height_delta > f64::from(CONTACT_EPSILON) * 0.5
    {
        player.position = down.end;
    }
}

fn horizontal_speed_squared(vector: Vec3) -> f32 {
    vector.x * vector.x + vector.z * vector.z
}

fn slide(
    player: &mut Player,
    collision: &mut CollisionWorld<'_>,
    result: &mut MovementResult,
    ladder: bool,
) -> bool {
    let hull = player.hull();
    let dt = f64::from(collision.dt);
    let mut time_left = dt;
    let mut elapsed = 0.0;
    let initial_velocity = player.velocity;
    let mut original_velocity = initial_velocity;
    let mut contact_planes = [DVec3::ZERO; MAX_MOVEMENT_BUMPS];
    let mut plane_count = 0;
    let mut moved = false;
    let mut hit = false;

    for bump in 0..contact_planes.len() {
        if player.velocity.length_squared() < 1e-20 {
            break;
        }

        let start = player.position.as_dvec3();
        let end = start + player.velocity.as_dvec3() * time_left;
        let mut sweep = trace_precise(start, end, hull, collision);
        if sweep.all_solid
            && let Some(brush) = sweep.brush
            && recover_contact(player, hull, brush, collision)
        {
            let recovered = player.position.as_dvec3();
            sweep = trace_precise(
                recovered,
                recovered + player.velocity.as_dvec3() * time_left,
                hull,
                collision,
            );
        }
        if sweep.all_solid {
            player.velocity = Vec3::ZERO;
            result.blocked = true;
            return true;
        }
        if sweep.fraction > 0.0 {
            result.segments.push(MovementSegment {
                from: player.position,
                to: sweep.end,
                start: elapsed / dt,
                end: (elapsed + time_left * sweep.fraction) / dt,
            });
            player.position = sweep.end;
            moved = true;
            original_velocity = player.velocity;
            plane_count = 0;
        }
        if sweep.fraction == 1.0 {
            break;
        }
        hit = true;
        result.had_contact = true;

        if collision.incline_fix
            && bump == 0
            && !player.grounded
            && original_velocity.y <= 140.0
            && sweep.normal.y >= WALKABLE_NORMAL.into()
            && sweep.normal.y < 1.0
            && horizontal_dot(original_velocity, sweep.normal.as_vec3()) < 0.0
            && horizontal_speed_squared(clip_velocity(original_velocity, sweep.normal).as_vec3())
                <= horizontal_speed_squared(original_velocity)
        {
            // Walkable inclines shouldn't consume horizontal speed on landing.
            player.position.y += 0.1;
            if let Some(segment) = result.segments.last_mut() {
                segment.to = player.position;
            }
            player.velocity.y = 0.0;
            return hit;
        }

        elapsed += time_left * sweep.fraction;
        time_left *= 1.0 - sweep.fraction;
        contact_planes[plane_count] = sweep.normal;
        plane_count += 1;
        let planes = &contact_planes[..plane_count];

        if plane_count == 1 && !player.grounded && !ladder {
            player.velocity = clip_velocity(original_velocity, sweep.normal).as_vec3();
            original_velocity = player.velocity;
            continue;
        }

        player.velocity = planes
            .iter()
            .enumerate()
            .find_map(|(i, &normal)| {
                let candidate = clip_velocity(original_velocity, normal);
                planes
                    .iter()
                    .enumerate()
                    .all(|(j, &plane)| i == j || source_dot(candidate, plane) >= 0.0)
                    .then_some(candidate.as_vec3())
            })
            .unwrap_or_else(|| {
                if let [a, b] = planes {
                    let crease = a.cross(*b).normalize_or_zero();
                    (crease * source_dot(crease, player.velocity.as_dvec3())).as_vec3()
                } else {
                    Vec3::ZERO
                }
            });

        if player.velocity.dot(initial_velocity) <= 0.0 {
            player.velocity = Vec3::ZERO;
            break;
        }
    }

    if !moved {
        player.velocity = Vec3::ZERO;
    }
    hit
}

fn horizontal_dot(a: Vec3, b: Vec3) -> f32 {
    a.x * b.x + a.z * b.z
}

fn clip_velocity(velocity: Vec3, normal: DVec3) -> DVec3 {
    let velocity = velocity.as_dvec3();
    let backoff = f64::from(source_dot(velocity, normal) as f32);
    let tangent = (velocity - normal * backoff).as_vec3().as_dvec3();
    tangent - normal * source_dot(tangent, normal).min(0.0)
}

fn recover_contact(
    player: &mut Player,
    hull: Hull,
    brush: &Brush,
    collision: &mut CollisionWorld<'_>,
) -> bool {
    for plane in &brush.planes {
        let distance = player.position.dot(plane.normal)
            - hull.expanded_distance(plane.normal, plane.distance);
        if distance.abs() <= CONTACT_EPSILON {
            let position = player.position + plane.normal * (CONTACT_EPSILON * 2.0);
            if !trace(position, position, hull, collision).start_solid {
                player.position = position;
                return true;
            }
        }
    }
    false
}

#[derive(Clone, Copy)]
struct Hull {
    min: Vec3,
    max: Vec3,
}

impl Hull {
    fn new(height: f32) -> Self {
        Self {
            min: Vec3::new(-HALF_WIDTH, 0.0, -HALF_WIDTH),
            max: Vec3::new(HALF_WIDTH, height, HALF_WIDTH),
        }
    }

    fn expanded_distance(self, normal: Vec3, distance: f32) -> f32 {
        // Expand the brush by the opposite support point of the feet-centered hull.
        let support = Vec3::select(normal.cmplt(Vec3::ZERO), self.max, self.min);
        distance - normal.dot(support)
    }
}

struct Trace<'a> {
    fraction: f64,
    end: Vec3,
    normal: DVec3,
    brush: Option<&'a Brush>,
    brush_index: Option<usize>,
    plane_index: Option<usize>,
    start_solid: bool,
    all_solid: bool,
    broadphase_overlap: bool,
}

impl Trace<'_> {
    fn walkable(&self) -> bool {
        self.brush.is_some() && !self.all_solid && self.normal.y >= WALKABLE_NORMAL.into()
    }
}

struct CollisionWorld<'a> {
    brushes: &'a [Brush],
    index: Option<&'a BrushIndex>,
    candidates: &'a mut Vec<usize>,
    candidate_bounds: Option<(Vec3, Vec3)>,
    query_margin: f32,
    incline_fix: bool,
    dt: f32,
    friction: &'a [BrushFriction],
    contact_rules: BrushContactRules<'a>,
}

#[derive(Clone, Copy, Default)]
struct BrushContactRules<'a> {
    exact_bonus_contacts: bool,
    recorded: &'a [RecordedBrush],
    prop_features: bool,
    box_corners: bool,
}

pub(crate) fn clip_camera(
    start: Vec3,
    end: Vec3,
    brushes: &[Brush],
    index: Option<&BrushIndex>,
    candidates: &mut Vec<usize>,
) -> Vec3 {
    let radius = Vec3::splat(4.0);
    let hull = Hull {
        min: -radius,
        max: radius,
    };
    let mut collision = CollisionWorld {
        brushes,
        index: index.filter(|index| index.covers(brushes)),
        candidates,
        candidate_bounds: None,
        query_margin: 0.0,
        incline_fix: false,
        dt: TICK_SECONDS,
        friction: &[],
        contact_rules: BrushContactRules::default(),
    };
    let hit = trace(start, end, hull, &mut collision);
    if hit.start_solid { start } else { hit.end }
}

fn trace<'a>(start: Vec3, end: Vec3, hull: Hull, collision: &mut CollisionWorld<'a>) -> Trace<'a> {
    trace_precise(start.as_dvec3(), end.as_dvec3(), hull, collision)
}

fn trace_precise<'a>(
    start: DVec3,
    end: DVec3,
    hull: Hull,
    collision: &mut CollisionWorld<'a>,
) -> Trace<'a> {
    let min =
        (start.min(end) + hull.min.as_dvec3() - DVec3::splat(CONTACT_EPSILON.into())).as_vec3();
    let max =
        (start.max(end) + hull.max.as_dvec3() + DVec3::splat(CONTACT_EPSILON.into())).as_vec3();
    let brushes = collision.brushes;
    if let Some(index) = collision.index {
        // The movement sweep usually contains the subsequent ground probe.
        // Padding the query lets both traces share an ordered candidate list.
        if collision
            .candidate_bounds
            .is_none_or(|(query_min, query_max)| {
                !min.cmpge(query_min).all() || !max.cmple(query_max).all()
            })
        {
            let margin = Vec3::splat(collision.query_margin);
            let query_min = min - margin;
            let query_max = max + margin;
            index.query(query_min, query_max, collision.candidates);
            collision.candidate_bounds = Some((query_min, query_max));
        }
        let nearby = collision
            .candidates
            .iter()
            .map(|&index| (index, &brushes[index]));
        trace_candidates(start, end, hull, min, max, nearby, collision.contact_rules)
    } else {
        trace_candidates(
            start,
            end,
            hull,
            min,
            max,
            brushes.iter().enumerate(),
            collision.contact_rules,
        )
    }
}

fn trace_candidates<'a>(
    start: DVec3,
    end: DVec3,
    hull: Hull,
    min: Vec3,
    max: Vec3,
    brushes: impl IntoIterator<Item = (usize, &'a Brush)>,
    rules: BrushContactRules<'_>,
) -> Trace<'a> {
    let mut result = Trace {
        fraction: 1.0,
        end: end.as_vec3(),
        normal: DVec3::ZERO,
        brush: None,
        brush_index: None,
        plane_index: None,
        start_solid: false,
        all_solid: false,
        broadphase_overlap: false,
    };

    for (index, brush) in brushes {
        if max.cmplt(brush.min).any() || min.cmpgt(brush.max).any() {
            continue;
        }
        let recorded = rules.recorded.get(index);
        if recorded.is_some_and(|profile| {
            !profile.leaf_bounds.is_empty()
                && !profile.leaf_bounds.iter().any(|(leaf_min, leaf_max)| {
                    max.cmpge(*leaf_min).all() && min.cmple(*leaf_max).all()
                })
        }) {
            continue;
        }
        result.broadphase_overlap = true;
        match trace_brush(start, end, hull, brush, recorded, rules) {
            BrushContact::None => {}
            BrushContact::StartsInside => result.start_solid = true,
            BrushContact::EntirelyInside => {
                return Trace {
                    fraction: 0.0,
                    end: start.as_vec3(),
                    normal: if rules.exact_bonus_contacts {
                        result.normal
                    } else {
                        DVec3::ZERO
                    },
                    brush: Some(brush),
                    brush_index: Some(index),
                    plane_index: None,
                    start_solid: true,
                    all_solid: true,
                    broadphase_overlap: true,
                };
            }
            BrushContact::Hit {
                fraction,
                normal,
                plane,
            } if fraction < result.fraction => {
                result.fraction = fraction;
                result.normal = normal;
                result.brush = Some(brush);
                result.brush_index = Some(index);
                result.plane_index = Some(plane);
            }
            BrushContact::Hit { .. } => {}
        }
    }

    result.end = (start + (end - start) * result.fraction).as_vec3();
    result
}

enum BrushContact {
    None,
    StartsInside,
    EntirelyInside,
    Hit {
        fraction: f64,
        normal: DVec3,
        plane: usize,
    },
}

fn trace_brush(
    start: DVec3,
    end: DVec3,
    hull: Hull,
    brush: &Brush,
    recorded: Option<&RecordedBrush>,
    rules: BrushContactRules<'_>,
) -> BrushContact {
    let mut enter = -1.0;
    let mut leave: f64 = 1.0;
    let mut start_outside = false;
    let mut end_outside = false;
    let mut normal = DVec3::ZERO;
    let mut contact_plane = 0;
    let mut unpadded_entry = f64::NEG_INFINITY;
    let mut unpadded_exit = f64::INFINITY;
    let prop = recorded.is_some_and(|brush| brush.prop);
    let check_unpadded =
        prop || (rules.box_corners && recorded.is_some_and(|brush| brush.axis_aligned_box));
    let mut prop_contact = None;
    let epsilon = f64::from(CONTACT_EPSILON);
    let displacement = if !rules.recorded.is_empty() {
        brush.planes.displacement_planes()
    } else {
        None
    };
    let plane_count = displacement.map_or_else(|| brush.planes.len(), <[_]>::len);
    let precise_planes = displacement.or_else(|| recorded.map(|brush| brush.planes.as_slice()));
    let on_plane_is_outside = precise_planes.is_none();

    for index in 0..plane_count {
        let (plane_normal, plane_distance) = precise_planes
            .and_then(|planes| planes.get(index).copied())
            .unwrap_or_else(|| {
                let plane = brush.planes[index];
                (plane.normal.as_dvec3(), f64::from(plane.distance))
            });
        let support = DVec3::select(
            plane_normal.cmplt(DVec3::ZERO),
            hull.max.as_dvec3(),
            hull.min.as_dvec3(),
        );
        let distance = plane_distance - source_dot(plane_normal, support);
        let from = source_dot(start, plane_normal) - distance;
        let to = source_dot(end, plane_normal) - distance;
        if check_unpadded {
            if from > to {
                let entry = from / (from - to);
                if entry > unpadded_entry {
                    unpadded_entry = entry;
                    if prop && rules.prop_features {
                        // Pick the actual entry face before applying the collision cushion.
                        prop_contact = Some((
                            ((from - epsilon) / (from - to)).max(0.0),
                            plane_normal,
                            index,
                        ));
                    }
                }
            } else if from < to {
                unpadded_exit = unpadded_exit.min(from / (from - to));
            }
        }
        let outside = |distance: f64| distance > 0.0 || (on_plane_is_outside && distance == 0.0);
        start_outside |= outside(from);
        end_outside |= outside(to);

        if outside(from) && outside(to) && (!prop || to >= from - 1e-5) {
            return BrushContact::None;
        }
        if !outside(from) && !outside(to) {
            continue;
        }

        if from > to {
            let entry = ((from - epsilon) / (from - to)).max(0.0);
            if entry > enter {
                enter = entry;
                normal = plane_normal;
                contact_plane = index;
            }
        } else {
            leave = leave.min((from + epsilon) / (from - to));
        }
    }

    if unpadded_exit < 1.0 && unpadded_entry > unpadded_exit {
        return BrushContact::None;
    }
    if let Some(contact) = prop_contact {
        (enter, normal, contact_plane) = contact;
    }
    if !start_outside {
        if end_outside {
            BrushContact::StartsInside
        } else {
            BrushContact::EntirelyInside
        }
    } else if enter >= 0.0 && enter < leave {
        BrushContact::Hit {
            fraction: enter,
            normal,
            plane: contact_plane,
        }
    } else {
        BrushContact::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::course::Plane;

    fn box_brush(min: Vec3, max: Vec3) -> Brush {
        Brush {
            min,
            max,
            planes: vec![
                Plane {
                    normal: Vec3::X,
                    distance: max.x,
                },
                Plane {
                    normal: Vec3::NEG_X,
                    distance: -min.x,
                },
                Plane {
                    normal: Vec3::Y,
                    distance: max.y,
                },
                Plane {
                    normal: Vec3::NEG_Y,
                    distance: -min.y,
                },
                Plane {
                    normal: Vec3::Z,
                    distance: max.z,
                },
                Plane {
                    normal: Vec3::NEG_Z,
                    distance: -min.z,
                },
            ]
            .into(),
        }
    }

    #[test]
    fn ground_probe_adds_downhill_speed_without_changing_the_landing_position() {
        let mut slope = box_brush(Vec3::splat(-100.0), Vec3::splat(100.0));
        slope.planes.push(Plane {
            normal: Vec3::new(0.6, 0.8, 0.0),
            distance: 0.0,
        });
        let brushes = [slope];
        let spawn = Vec3::new(0.0, 14.2, 0.0);
        let initial = Vec3::new(200.0, -180.0, 0.0);
        let mut legacy = Player::new(spawn);
        legacy.velocity = initial;
        step(&mut legacy, MoveInput::default(), &brushes);

        let mut player = Player::new(spawn);
        player.velocity = initial;
        let result = step_with_index(
            &mut player,
            MoveInput::default(),
            &brushes,
            None,
            &mut Vec::new(),
            StepRules {
                downhill_landing: true,
                ..Default::default()
            },
        );
        assert!(result.landed);
        assert!(!result.had_contact);
        assert_eq!(player.position, legacy.position);
        assert_eq!(
            result.velocity_before_downhill_landing,
            Some(legacy.velocity)
        );
        assert!((player.velocity.x - 217.28).abs() < 0.0001);
    }

    #[test]
    fn authored_box_corner_rejects_a_sweep_that_only_hits_the_collision_padding() {
        let brush = box_brush(Vec3::ZERO, Vec3::ONE);
        let recorded = RecordedBrush {
            id: None,
            prop: false,
            axis_aligned_box: true,
            planes: brush
                .planes
                .iter()
                .map(|plane| (plane.normal.as_dvec3(), f64::from(plane.distance)))
                .collect(),
            leaf_bounds: Vec::new(),
        };
        let hull = Hull {
            min: Vec3::ZERO,
            max: Vec3::ZERO,
        };
        let start = DVec3::new(-0.01, 0.02, 0.5);
        let end = DVec3::new(0.01, 2.02, 0.5);
        let rules = BrushContactRules {
            recorded: std::slice::from_ref(&recorded),
            ..Default::default()
        };
        assert!(matches!(
            trace_brush(start, end, hull, &brush, Some(&recorded), rules),
            BrushContact::Hit { .. }
        ));
        let rules = BrushContactRules {
            box_corners: true,
            ..rules
        };
        assert!(matches!(
            trace_brush(start, end, hull, &brush, Some(&recorded), rules),
            BrushContact::None
        ));
    }

    #[test]
    fn indexed_sweeps_preserve_exact_brush_order_and_movement() {
        let mut brushes = Vec::new();
        for i in -128..128 {
            let point = Vec3::new(i as f32 * 80.0, -50.0, 1800.0);
            brushes.push(Brush::box_at(point, point + Vec3::splat(40.0)));
        }
        brushes.insert(
            48,
            Brush::box_at(
                Vec3::new(-500.0, -32.0, -500.0),
                Vec3::new(500.0, 0.0, 500.0),
            ),
        );
        for x in [0.0, -64.0] {
            brushes.insert(
                96,
                Brush::box_at(Vec3::new(x, 0.0, -230.0), Vec3::new(x + 64.0, 80.0, -200.0)),
            );
        }
        let index = BrushIndex::new(&brushes).expect("large world has a brush index");
        let mut scratch = Vec::new();
        let mut indexed = Player::new(Vec3::Y);
        let mut linear = indexed;
        for tick in 0..220 {
            let command = MoveInput {
                forward: 1.0,
                strafe: if tick > 120 { 1.0 } else { 0.0 },
                jump: tick > 60,
                crouch: tick > 180,
                ..Default::default()
            };
            step(&mut linear, command, &brushes);
            step_with_index(
                &mut indexed,
                command,
                &brushes,
                Some(&index),
                &mut scratch,
                StepRules::default(),
            );
            assert_eq!(indexed.position, linear.position);
            assert_eq!(indexed.velocity, linear.velocity);
            assert_eq!(indexed.grounded, linear.grounded);
            assert_eq!(indexed.eye_height(), linear.eye_height());
        }
        assert!(scratch.len() < brushes.len() / 2);
    }

    #[test]
    fn air_acceleration_uses_a_directional_cap() {
        let mut player = Player::new(Vec3::Y * 100.0);
        player.velocity.z = -500.0;
        step(
            &mut player,
            MoveInput {
                strafe: 1.0,
                ..Default::default()
            },
            &[],
        );
        assert_eq!(player.velocity.x, 30.0);
        assert_eq!(player.velocity.z, -500.0);
        assert_eq!(player.velocity.y, -12.0);
    }

    #[test]
    fn steep_ramp_turns_gravity_into_forward_speed() {
        let mut ramp = box_brush(
            Vec3::new(-500.0, -1600.0, -100.0),
            Vec3::new(500.0, 700.0, 100.0),
        );
        ramp.planes.push(Plane {
            normal: Vec3::new(0.8, 0.6, 0.0),
            distance: 0.0,
        });

        let mut player = Player::new(Vec3::Y * 60.0);
        for _ in 0..45 {
            step(
                &mut player,
                MoveInput::default(),
                std::slice::from_ref(&ramp),
            );
        }
        assert!(!player.grounded);
        assert!((player.position.x - 68.94498).abs() < 0.01, "{player:?}");
        assert!((player.velocity.x - 256.31992).abs() < 0.01, "{player:?}");
        assert!(
            player.position.dot(Vec3::new(0.8, 0.6, 0.0)) >= 12.79,
            "{player:?}"
        );
    }

    #[test]
    fn held_jump_rebounds_without_releasing_the_key() {
        let floor = box_brush(Vec3::splat(-1000.0), Vec3::new(1000.0, 0.0, 1000.0));
        let mut player = Player::new(Vec3::Y * 0.05);
        let jump = MoveInput {
            jump: true,
            ..Default::default()
        };
        step(&mut player, jump, std::slice::from_ref(&floor));
        assert!(player.velocity.y > 280.0, "{player:?}");
        assert!(!player.grounded);
        let mut rebounds = 0;
        for _ in 0..100 {
            let previous_velocity = player.velocity.y;
            step(&mut player, jump, std::slice::from_ref(&floor));
            if previous_velocity <= 0.0 && player.velocity.y > 280.0 {
                rebounds += 1;
            }
        }
        assert!(rebounds >= 2, "{player:?}");
        assert!(player.position.y >= 0.0);
    }

    #[test]
    fn contact_face_overrides_brush_surface_friction() {
        let floor = Brush::box_at(Vec3::splat(-1000.0), Vec3::new(1000.0, 0.0, 1000.0));
        let plane = floor
            .planes
            .iter()
            .position(|plane| plane.normal == Vec3::Y)
            .unwrap();
        let mut planes = vec![None; floor.planes.len()];
        planes[plane] = Some(0.2);
        let friction = [BrushFriction { brush: 1.0, planes }];
        let mut player = Player::new(Vec3::Y * 0.05);
        player.velocity.x = 200.0;
        step_with_index(
            &mut player,
            MoveInput::default(),
            &[floor],
            None,
            &mut Vec::new(),
            StepRules {
                friction: &friction,
                ..StepRules::default()
            },
        );
        assert!(player.grounded);
        assert!((player.velocity.x - 197.0).abs() < 0.001);
    }
}
