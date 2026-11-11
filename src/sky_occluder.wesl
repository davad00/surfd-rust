import bevy_pbr::render::{
    forward_io::VertexOutput,
    mesh_view_bindings::view,
    utils::coords_to_viewport_uv,
};
import constants::MATERIAL_BIND_GROUP;

@group(MATERIAL_BIND_GROUP) @binding(0) var sky: texture_cube<f32>;
@group(MATERIAL_BIND_GROUP) @binding(1) var sky_sampler: sampler;

@fragment
fn fragment(vertex: VertexOutput) -> @location(0) vec4<f32> {
    // In a cube-only scene this surface writes the same color as the skybox
    // would at this pixel, with its authored depth to cover world geometry.
    let uv = coords_to_viewport_uv(vertex.position.xy, view.main_pass_viewport);
    let clip = vec4(uv * vec2(2.0, -2.0) + vec2(-1.0, 1.0), 1.0, 1.0);
    let point = view.view_from_clip * clip;
    let ray = normalize((view.world_from_view * vec4(point.xyz / point.w, 0.0)).xyz);
    return textureSample(sky, sky_sampler, ray * vec3(1.0, 1.0, -1.0));
}
