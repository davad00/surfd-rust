import bevy_pbr::render::{forward_io::VertexOutput, mesh_view_bindings::view};
import constants::MATERIAL_BIND_GROUP;

@group(MATERIAL_BIND_GROUP) @binding(0) var<uniform> opacity: f32;
@group(MATERIAL_BIND_GROUP) @binding(1) var atlas: texture_2d<f32>;
@group(MATERIAL_BIND_GROUP) @binding(2) var atlas_sampler: sampler;

fn linear_srgb(rgb: vec3<f32>) -> vec3<f32> {
    return pow((rgb / 255.0 + 0.055) / 1.055, vec3(2.4));
}

@fragment
fn fragment(vertex: VertexOutput) -> @location(0) vec4<f32> {
    let albedo = vertex.color.rgb * textureSample(atlas, atlas_sampler, vertex.uv).rgb;
    let luma = dot(albedo, vec3(0.2126, 0.7152, 0.0722));
    let deep = linear_srgb(vec3(58.0, 163.0, 196.0));
    let pale = linear_srgb(vec3(240.0, 254.0, 255.0));
    let body = mix(deep, pale, smoothstep(0.015, 0.4, luma));
    let facing = abs(dot(normalize(vertex.world_normal), normalize(view.world_position - vertex.world_position.xyz)));
    let rim = pow(1.0 - facing, 2.0);
    let color = body + linear_srgb(vec3(159.0, 243.0, 255.0)) * rim;
    return vec4(color, clamp(opacity * (0.75 + 0.65 * rim), 0.0, 1.0));
}
