use super::*;

#[derive(Clone, Copy, Debug)]
pub(super) struct WaterJump {
    remaining_ms: f32,
    velocity: Vec3,
}

fn command_axes(input: MoveInput, rules: &StepRules<'_>) -> [f32; 2] {
    rules
        .recorded_axes
        .unwrap_or([
            f64::from(input.forward) * 400.0,
            f64::from(input.strafe) * 400.0,
        ])
        .map(|axis| {
            if axis.is_finite() {
                axis.clamp(-400.0, 400.0) as f32
            } else {
                0.0
            }
        })
}

fn view_vectors(input: MoveInput, rules: &StepRules<'_>) -> (Vec3, Vec3) {
    const DEG_TO_RAD: f32 = std::f32::consts::PI / 180.0;
    let yaw = rules
        .recorded_yaw
        .unwrap_or_else(|| native_yaw_to_source(input.yaw)) as f32;
    let pitch = -f64::from(rules.pitch).to_degrees().clamp(-89.0, 89.0) as f32;
    let (sin_yaw, cos_yaw) = f64::from(yaw * DEG_TO_RAD).sin_cos();
    let (sin_pitch, cos_pitch) = f64::from(pitch * DEG_TO_RAD).sin_cos();
    let (sin_yaw, cos_yaw) = (sin_yaw as f32, cos_yaw as f32);
    let cos_pitch = cos_pitch as f32;
    (
        Vec3::new(cos_pitch * cos_yaw, -sin_pitch as f32, -cos_pitch * sin_yaw),
        Vec3::new(sin_yaw, 0.0, cos_yaw),
    )
}

// Surfd rounds each product and addition in its ladder and water math.
fn rounded_dot(a: Vec3, b: DVec3) -> f32 {
    (f64::from(a.x) * b.x) as f32 + (f64::from(a.z) * b.z) as f32 + (f64::from(a.y) * b.y) as f32
}

fn rounded_length(vector: Vec3) -> f32 {
    rounded_dot(vector, vector.as_dvec3()).sqrt()
}

fn normalized(vector: Vec3, length: f32) -> Vec3 {
    if length == 0.0 {
        Vec3::ZERO
    } else {
        vector / length
    }
}

pub(super) fn ladder_move(
    player: &mut Player,
    input: MoveInput,
    rules: &StepRules<'_>,
    ids: &HashSet<String>,
    collision: &mut CollisionWorld<'_>,
    result: &mut MovementResult,
) -> bool {
    const LADDER_SPEED: f32 = 200.0;
    let (forward, right) = view_vectors(input, rules);
    let [forward_axis, strafe_axis] = command_axes(input, rules);
    let offset = if let Some(normal) = player.ladder_normal {
        (-normal * 10.0).as_vec3()
    } else {
        let wish = forward * forward_axis + right * strafe_axis;
        let length = rounded_length(wish);
        if length == 0.0 {
            return false;
        }
        (wish / length) * 2.0
    };
    let hit = trace(
        player.position,
        player.position + offset,
        player.hull(),
        collision,
    );
    if hit.fraction == 1.0
        || hit.normal.y == 1.0
        || !hit
            .brush_index
            .and_then(|index| collision.contact_rules.recorded.get(index))
            .and_then(|brush| brush.id.as_ref())
            .is_some_and(|id| ids.contains(id))
    {
        return false;
    }

    let normal = hit.normal;
    player.ladder_normal = Some(normal);
    let point = player.position - Vec3::Y;
    let point_hull = Hull {
        min: Vec3::ZERO,
        max: Vec3::ZERO,
    };
    let on_floor = trace(point, point, point_hull, collision).start_solid || player.grounded;
    result.cleared_gravity_scale = true;
    if input.jump {
        player.velocity = (normal * 270.0).as_vec3();
        return false;
    }

    let speed = if input.crouch {
        LADDER_SPEED * 0.34
    } else {
        LADDER_SPEED
    };
    let axis_speed = |axis: f32| {
        if axis == 0.0 {
            0.0
        } else {
            axis.signum() * speed
        }
    };
    let wish = forward * axis_speed(forward_axis) + right * axis_speed(strafe_axis);
    let side = Vec3::new(normal.z as f32, 0.0, -normal.x as f32);
    let side = normalized(side, rounded_length(side));
    let into = rounded_dot(wish, normal);
    let normal_part = (normal * f64::from(into)).as_vec3();
    let mut lateral = wish - normal_part;
    let vertical = rounded_cross(normal, side.as_dvec3());
    let up_speed = rounded_dot(lateral, vertical.as_dvec3());
    let side_speed = rounded_dot(lateral, side.as_dvec3());
    let along = side * side_speed + normal_part;
    if rounded_dot(normalized(along, rounded_length(along)), normal) < -0.707_f32 {
        lateral = vertical * up_speed + (side * 0.2) * side_speed;
    }
    player.velocity = lateral + vertical * -into;
    if on_floor && into > 0.0 {
        player.velocity += (normal * f64::from(LADDER_SPEED)).as_vec3();
    }
    true
}

fn rounded_cross(a: DVec3, b: DVec3) -> Vec3 {
    let product = |a: f64, b: f64| (a * b) as f32;
    Vec3::new(
        product(a.y, b.z) - product(a.z, b.y),
        product(a.z, b.x) - product(a.x, b.z),
        product(a.x, b.y) - product(a.y, b.x),
    )
}

pub(super) fn water_level(player: &Player, hulls: &[AuthoredZoneGeometry]) -> u8 {
    let hull = player.hull();
    let wet = |height: f32| {
        let point = player.position.with_y(height).as_dvec3();
        hulls.iter().any(|hull| {
            point.cmpgt(hull.min).all()
                && point.cmplt(hull.max).all()
                && hull
                    .planes
                    .iter()
                    .all(|&(normal, distance)| source_dot(normal, point) < distance)
        })
    };
    if !wet((player.position.y + hull.min.y) + 1.0) {
        0
    } else if !wet(player.position.y + (hull.min.y + hull.max.y) * 0.5) {
        1
    } else if !wet(player.position.y + player.eye_height()) {
        2
    } else {
        3
    }
}

pub(super) fn water_tick(
    player: &mut Player,
    input: MoveInput,
    rules: &StepRules<'_>,
    push: Vec3,
    half_gravity: f32,
    collision: &mut CollisionWorld<'_>,
    result: &mut MovementResult,
) -> bool {
    let level = water_level(player, rules.water_hulls);
    let was_grounded = player.grounded;
    if let Some(mut jump) = player.water_jump {
        if level < 2 {
            player.velocity.y -= half_gravity;
            player.velocity.y += push.y * collision.dt;
            clamp_velocity(&mut player.velocity, rules.movement.max_velocity);
        }
        jump.remaining_ms -= 1000.0 * collision.dt;
        player.water_jump = (jump.remaining_ms > 0.0 && level != 0).then_some(jump);
        player.velocity = jump.velocity.with_y(player.velocity.y);
        player.jump_held = input.jump;
        slide(player, collision, result, false);
        return true;
    }
    if level < 2 {
        return false;
    }

    let (forward, right) = view_vectors(input, rules);
    if level == 2 {
        check_water_jump(player, forward, collision);
    }
    if player.velocity.y < 0.0 {
        player.water_jump = None;
    }
    if input.jump {
        if let Some(mut jump) = player.water_jump {
            jump.remaining_ms -= collision.dt;
            player.water_jump = (jump.remaining_ms >= 0.0).then_some(jump);
        } else {
            player.grounded = false;
            player.ground_brush = None;
            player.velocity.y = 100.0;
        }
    }
    player.jump_held = input.jump;
    water_accelerate(player, input, rules, forward, right, collision.dt);
    player.velocity += push;
    water_slide(player, collision, result);
    player.velocity -= push;
    categorize_ground(player, collision);
    if player.grounded {
        player.velocity.y = 0.0;
    }
    result.landed = !was_grounded && player.grounded;
    true
}

fn check_water_jump(player: &mut Player, forward: Vec3, collision: &mut CollisionWorld<'_>) {
    if player.water_jump.is_some() || player.velocity.y < -180.0 {
        return;
    }
    let velocity = player.velocity.with_y(0.0);
    let speed = rounded_length(velocity);
    let horizontal = forward.with_y(0.0);
    let direction = normalized(horizontal, rounded_length(horizontal));
    if speed != 0.0 && rounded_dot(normalized(velocity, speed), direction.as_dvec3()) < 0.0 {
        return;
    }
    let hull = player.hull();
    let across = direction * 24.0;
    let mid = player.position + Vec3::Y * ((hull.min.y + hull.max.y) * 0.5);
    let ledge = trace(mid, mid + across, hull, collision);
    if ledge.fraction == 1.0 {
        return;
    }
    let above = player
        .position
        .with_y((player.position.y + player.eye_height()) + 8.0);
    let beyond = above + across;
    if trace(above, beyond, hull, collision).fraction == 1.0 {
        let surface = trace(beyond, beyond - Vec3::Y * 1024.0, hull, collision);
        if surface.fraction < 1.0 && surface.normal.y >= f64::from(WALKABLE_NORMAL) {
            player.velocity.y = 256.0;
            player.water_jump = Some(WaterJump {
                remaining_ms: 2000.0,
                velocity: (ledge.normal * -50.0).as_vec3(),
            });
        }
    }
}

fn water_accelerate(
    player: &mut Player,
    input: MoveInput,
    rules: &StepRules<'_>,
    forward: Vec3,
    right: Vec3,
    dt: f32,
) {
    let [mut ahead, mut sideways] = command_axes(input, rules);
    let input_length_squared = ahead * ahead + sideways * sideways;
    if input_length_squared > RUN_SPEED * RUN_SPEED {
        let scale = RUN_SPEED / input_length_squared.sqrt();
        ahead *= scale;
        sideways *= scale;
    }
    if player.crouched && player.grounded {
        ahead *= 0.34;
        sideways *= 0.34;
    }
    let mut wish = forward * ahead + right * sideways;
    wish.y += if input.jump {
        RUN_SPEED
    } else if ahead == 0.0 && sideways == 0.0 {
        -60.0
    } else {
        ((ahead * forward.y) * 2.0).clamp(0.0, RUN_SPEED)
    };
    let mut wish_speed = rounded_length(wish);
    if wish_speed > RUN_SPEED {
        wish *= RUN_SPEED / wish_speed;
        wish_speed = RUN_SPEED;
    }
    wish_speed *= 0.8;

    let speed = rounded_length(player.velocity);
    let mut remaining = 0.0;
    if speed != 0.0 {
        remaining = speed - (((dt * speed) * GROUND_FRICTION) * player.surface_friction);
        if remaining < 0.1 {
            remaining = 0.0;
        }
        player.velocity *= remaining / speed;
    }
    if wish_speed >= 0.1 && wish_speed > remaining {
        let gain = (wish_speed - remaining)
            .min(((GROUND_ACCELERATION * wish_speed) * dt) * player.surface_friction);
        player.velocity += normalized(wish, rounded_length(wish)) * gain;
    }
}

fn water_slide(
    player: &mut Player,
    collision: &mut CollisionWorld<'_>,
    result: &mut MovementResult,
) {
    let from = player.position;
    let to = from + player.velocity * collision.dt;
    let hull = player.hull();
    if trace(from, to, hull, collision).fraction == 1.0 {
        let down = trace(to + Vec3::Y * (STEP_HEIGHT + 1.0), to, hull, collision);
        if !down.start_solid && !down.all_solid {
            player.position = down.end;
            result.segments.push(MovementSegment {
                from,
                to: down.end,
                start: 0.0,
                end: 1.0,
            });
        } else {
            slide(player, collision, result, false);
        }
        return;
    }
    if !player.grounded {
        slide(player, collision, result, false);
        return;
    }

    // Keep the better of a straight slide and a slide from one step higher.
    let initial = *player;
    slide(player, collision, result, false);
    let flat = *player;
    let flat_result = std::mem::take(result);
    *player = initial;
    let rise = Vec3::Y * (STEP_HEIGHT + CONTACT_EPSILON);
    let up = trace(from, from + rise, hull, collision);
    if !up.start_solid && !up.all_solid {
        player.position = up.end;
    }
    slide(player, collision, result, false);
    let down = trace(player.position, player.position - rise, hull, collision);
    if down.normal.y >= f64::from(WALKABLE_NORMAL) {
        if !down.start_solid && !down.all_solid {
            player.position = down.end;
        }
        let horizontal_progress = |position: Vec3| {
            let delta = position.as_dvec3() - from.as_dvec3();
            delta.x * delta.x + delta.z * delta.z
        };
        if horizontal_progress(flat.position) <= horizontal_progress(player.position) {
            player.velocity.y = flat.velocity.y;
            return;
        }
    }
    *player = flat;
    *result = flat_result;
}
