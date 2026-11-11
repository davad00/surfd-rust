import bevy_pbr::render::{
    forward_io::{Vertex, decompress_vertex},
    mesh_functions,
    mesh_view_bindings::view,
    view_transformations::position_world_to_clip,
};
import constants::MATERIAL_BIND_GROUP;

struct Material {
    tint: vec3<f32>,
    opacity: f32,
    env_tint: vec3<f32>,
    alpha_cut: f32,
    env_shaping: vec3<f32>,
    seamless_scale: f32,
    seamless_offset: vec3<f32>,
    detail_factor: f32,
    fog_color: vec3<f32>,
    fog_max: f32,
    detail_scale: vec2<f32>,
    fog_range: vec2<f32>,
    lighting_response: vec2<f32>,
    prop_fade_range: vec2<f32>,
    flags: u32,
};

@group(MATERIAL_BIND_GROUP) @binding(0) var<uniform> material: Material;
@group(MATERIAL_BIND_GROUP) @binding(1) var base_map: texture_2d<f32>;
@group(MATERIAL_BIND_GROUP) @binding(2) var repeat_sampler: sampler;
@group(MATERIAL_BIND_GROUP) @binding(3) var second_map: texture_2d<f32>;
@group(MATERIAL_BIND_GROUP) @binding(4) var detail_map: texture_2d<f32>;
@group(MATERIAL_BIND_GROUP) @binding(5) var lightmap: texture_2d<f32>;
@group(MATERIAL_BIND_GROUP) @binding(6) var prop_light: texture_2d<f32>;
@group(MATERIAL_BIND_GROUP) @binding(7) var normal_map: texture_2d<f32>;
@group(MATERIAL_BIND_GROUP) @binding(8) var env_mask: texture_2d<f32>;
@group(MATERIAL_BIND_GROUP) @binding(9) var environment: texture_cube<f32>;
@group(MATERIAL_BIND_GROUP) @binding(10) var environment_sampler: sampler;

const SECOND_MAP = 0u;
const DETAIL_MAP = 1u;
const ALPHA_TEST = 2u;
const VERTEX_ALPHA = 3u;
const TRANSLUCENT = 4u;
const WORLD_LIGHT = 5u;
const RGB_EXP_LIGHT = 6u;
const PROP_LIGHT = 7u;
const UNLIT = 8u;
const COLORED_SHADOW_LIFT = 9u;
const REFLECTION_ONLY = 10u;
const ENVIRONMENT = 11u;
const NORMAL_MAP = 12u;
const NORMAL_ALPHA_MASK = 13u;
const ENV_MASK = 14u;
const ENV_SHAPING = 15u;
const PROP_FADE = 16u;
const OPACITY_EFFECT = 17u;
@if(SURFD_UNIFORM_OPAQUE_WORLD_LIGHT)
const OPAQUE_WORLD_LIGHT = 18u;
const BASE_RGB_ONLY = 24u;
const SECOND_RGB_ONLY = 25u;
const NORMAL_RGB_ONLY = 26u;

fn enabled(bit: u32) -> bool {
    return (material.flags & (1u << bit)) != 0u;
}

struct RasterVertex {
    @builtin(position) position: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) light_uv: vec2<f32>,
    @location(4) blend: f32,
    @location(5) lighting_and_opacity: vec4<f32>,
};

@vertex
fn vertex(input: Vertex) -> RasterVertex {
    // Use the builtin directly. Naga's HLSL backend applies the instance
    // offset twice when the entry-point field is bound to a local let.
    let mesh = decompress_vertex(input, input.instance_index);
    let transform = mesh_functions::get_world_from_local(input.instance_index);
    let world = mesh_functions::mesh_position_local_to_world(transform, vec4(mesh.position, 1.0));
    var out: RasterVertex;
    out.position = position_world_to_clip(world.xyz);
    out.world = world.xyz;
    out.normal = mesh_functions::mesh_normal_local_to_world(mesh.normal, input.instance_index);
    out.uv = mesh.uv;
    out.light_uv = mesh.uv_b;
    out.blend = mesh.color.x;
    var lighting = vec3(1.0);
    var opacity_factor = 1.0;
    if enabled(PROP_LIGHT) {
        let offset = mesh_functions::get_tag(input.instance_index);
        if offset != 0xffffffffu {
            let size = textureDimensions(prop_light);
            let index = offset + u32(max(mesh.color.y, 0.0));
            let coord = vec2(index % size.x, index / size.x);
            if coord.y < size.y {
                lighting = textureLoad(prop_light, coord, 0).rgb;
            }
        }
    }
    if enabled(PROP_FADE) {
        let range = material.prop_fade_range;
        let depth = distance(view.world_position, transform[3].xyz);
        opacity_factor = clamp((range.y - depth) / (range.y - range.x), 0.0, 1.0);
    }
    if enabled(OPACITY_EFFECT) {
        opacity_factor = bitcast<f32>(mesh_functions::get_tag(input.instance_index));
    }
    out.lighting_and_opacity = vec4(lighting, opacity_factor);
    return out;
}

fn decode_luxel(coord: vec2<i32>, max_coord: vec2<i32>) -> vec3<f32> {
    let texel = textureLoad(lightmap, clamp(coord, vec2(0), max_coord), 0);
    @if(SURFD_OPAQUE_WORLD_LIGHT)
    return texel.rgb * exp2(round(texel.a * 255.0) - 128.0);
    @if(!SURFD_OPAQUE_WORLD_LIGHT)
    if enabled(RGB_EXP_LIGHT) {
        return texel.rgb * exp2(round(texel.a * 255.0) - 128.0);
    }
    @if(!SURFD_OPAQUE_WORLD_LIGHT)
    return texel.rgb;
}

fn baked_light(uv: vec2<f32>) -> vec3<f32> {
    let size = textureDimensions(lightmap);
    let texel = uv * vec2<f32>(size) - 0.5;
    let blend = fract(texel);
    let base = vec2<i32>(floor(texel));
    let max_coord = vec2<i32>(size) - 1;
    let top = mix(decode_luxel(base, max_coord), decode_luxel(base + vec2(1, 0), max_coord), blend.x);
    let bottom = mix(decode_luxel(base + vec2(0, 1), max_coord), decode_luxel(base + vec2(1, 1), max_coord), blend.x);
    return mix(top, bottom, blend.y);
}

fn source_axes(value: vec3<f32>) -> vec3<f32> {
    return vec3(value.x, -value.z, value.y);
}

fn sample_base(map: texture_2d<f32>, vertex: RasterVertex) -> vec4<f32> {
    if material.seamless_scale != 0.0 {
        let authored = source_axes(vertex.world) * material.seamless_scale + material.seamless_offset;
        let weights = pow(abs(source_axes(vertex.normal)), vec3(4.0));
        let n = weights / max(dot(weights, vec3(1.0)), 0.00001);
        return textureSample(map, repeat_sampler, authored.yz) * n.x
            + textureSample(map, repeat_sampler, authored.xz) * n.y
            + textureSample(map, repeat_sampler, authored.xy) * n.z;
    }
    return textureSample(map, repeat_sampler, vertex.uv);
}

fn reflection(vertex: RasterVertex) -> vec3<f32> {
    var normal = normalize(vertex.normal);
    let bump = textureSample(normal_map, repeat_sampler, vertex.uv);
    if enabled(NORMAL_MAP) {
        let q0 = dpdx(vertex.world);
        let q1 = -dpdy(vertex.world);
        let st0 = dpdx(vertex.uv);
        let st1 = -dpdy(vertex.uv);
        let q1_perp = cross(q1, normal);
        let q0_perp = cross(normal, q0);
        let tangent = q1_perp * st0.x + q0_perp * st1.x;
        let bitangent = q1_perp * st0.y + q0_perp * st1.y;
        let weight = inverseSqrt(max(max(dot(tangent, tangent), dot(bitangent, bitangent)), 0.00001));
        normal = normalize(mat3x3(tangent * weight, bitangent * weight, normal) * (bump.xyz * 2.0 - 1.0));
    }
    let direction = normalize(vertex.world - view.world_position);
    let mask = select(1.0, bump.a, enabled(NORMAL_ALPHA_MASK) && !enabled(NORMAL_RGB_ONLY));
    var specular = textureSample(environment, environment_sampler, reflect(direction, normal)).rgb
        * material.env_tint * mask;
    if enabled(ENV_MASK) {
        specular *= textureSample(env_mask, repeat_sampler, vertex.uv).rgb;
    }
    if enabled(ENV_SHAPING) {
        specular = mix(specular, specular * specular, material.env_shaping.x);
        specular = mix(vec3(dot(specular, vec3(0.299, 0.587, 0.114))), specular, material.env_shaping.y);
        let fresnel = pow(1.0 - clamp(dot(normal, -direction), 0.0, 1.0), 5.0);
        specular *= mix(fresnel, 1.0, material.env_shaping.z);
    }
    return specular;
}

@if(SURFD_OPAQUE_WORLD_LIGHT)
@fragment
fn fragment(vertex: RasterVertex) -> @location(0) vec4<f32> {
    let color = textureSample(base_map, repeat_sampler, vertex.uv).rgb;
    var lighting = baked_light(vertex.light_uv);
    let brightest = max(max(lighting.r, lighting.g), lighting.b);
    let shadow = select(vec3(1.0), lighting / max(brightest, 0.001), enabled(COLORED_SHADOW_LIFT));
    lighting = lighting * material.lighting_response.x
        + shadow * material.lighting_response.y * (1.0 - clamp(brightest, 0.0, 1.0));
    let result = color * material.tint * lighting;
    let depth = -(view.view_from_world * vec4(vertex.world, 1.0)).z;
    let fog = clamp((depth - material.fog_range.x) / (material.fog_range.y - material.fog_range.x), 0.0, material.fog_max);
    return vec4(mix(result, material.fog_color, fog), 1.0);
}

@if(!SURFD_OPAQUE_WORLD_LIGHT)
@fragment
fn fragment(vertex: RasterVertex) -> @location(0) vec4<f32> {
    @if(SURFD_UNIFORM_OPAQUE_WORLD_LIGHT)
    if enabled(OPAQUE_WORLD_LIGHT) {
        let color = textureSample(base_map, repeat_sampler, vertex.uv).rgb;
        var lighting = baked_light(vertex.light_uv);
        let brightest = max(max(lighting.r, lighting.g), lighting.b);
        let shadow = select(vec3(1.0), lighting / max(brightest, 0.001), enabled(COLORED_SHADOW_LIFT));
        lighting = lighting * material.lighting_response.x
            + shadow * material.lighting_response.y * (1.0 - clamp(brightest, 0.0, 1.0));
        let result = color * material.tint * lighting;
        let depth = -(view.view_from_world * vec4(vertex.world, 1.0)).z;
        let fog = clamp((depth - material.fog_range.x) / (material.fog_range.y - material.fog_range.x), 0.0, material.fog_max);
        return vec4(mix(result, material.fog_color, fog), 1.0);
    }

    var base = sample_base(base_map, vertex);
    base.a = select(base.a, 1.0, enabled(BASE_RGB_ONLY));
    if enabled(SECOND_MAP) {
        var second = sample_base(second_map, vertex);
        second.a = select(second.a, 1.0, enabled(SECOND_RGB_ONLY));
        base = mix(base, second, clamp(vertex.blend, 0.0, 1.0));
    }
    var color = base.rgb;
    if enabled(DETAIL_MAP) {
        let detail = textureSample(detail_map, repeat_sampler, vertex.uv * material.detail_scale).rgb;
        color *= mix(vec3(1.0), 2.0 * detail, material.detail_factor);
    }
    var lighting = vec3(1.0);
    if !enabled(UNLIT) {
        if enabled(PROP_LIGHT) {
            lighting = vertex.lighting_and_opacity.rgb;
        } else if enabled(WORLD_LIGHT) {
            lighting = baked_light(vertex.light_uv);
        }
        let brightest = max(max(lighting.r, lighting.g), lighting.b);
        let shadow = select(vec3(1.0), lighting / max(brightest, 0.001), enabled(COLORED_SHADOW_LIFT));
        lighting = lighting * material.lighting_response.x
            + shadow * material.lighting_response.y * (1.0 - clamp(brightest, 0.0, 1.0));
    }
    var result = select(color * material.tint * lighting, vec3(0.0), enabled(REFLECTION_ONLY));
    if enabled(ENVIRONMENT) {
        result += reflection(vertex);
    }
    let effect = select(1.0, vertex.lighting_and_opacity.a, enabled(OPACITY_EFFECT));
    var alpha = base.a * material.opacity * effect * select(1.0, vertex.blend, enabled(VERTEX_ALPHA));
    if enabled(ALPHA_TEST) && alpha < material.alpha_cut {
        discard;
    }
    if enabled(PROP_FADE) {
        alpha *= vertex.lighting_and_opacity.a;
        if alpha <= 1.0 / 255.0 {
            discard;
        }
    }
    let depth = -(view.view_from_world * vec4(vertex.world, 1.0)).z;
    let fog = clamp((depth - material.fog_range.x) / (material.fog_range.y - material.fog_range.x), 0.0, material.fog_max);
    return vec4(mix(result, material.fog_color, fog), select(1.0, alpha, enabled(TRANSLUCENT)));
}
