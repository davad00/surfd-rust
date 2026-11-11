import bevy_pbr::render::{
    forward_io::{Vertex, VertexOutput, decompress_vertex},
    mesh_functions,
    mesh_view_bindings::view,
    view_transformations::position_world_to_clip,
};

@vertex
fn vertex(in: Vertex) -> VertexOutput {
    // Naga's HLSL backend applies the first-instance offset twice to a named alias.
    let model = mesh_functions::get_world_from_local(in.instance_index);
    let player = decompress_vertex(in, in.instance_index);
    let distance_to_player = distance(view.world_position, model[3].xyz);
    let opacity = clamp((distance_to_player - 32.0) / 96.0, 0.0, 1.0);

    var out: VertexOutput;
    out.world_position = mesh_functions::mesh_position_local_to_world(model, vec4<f32>(player.position, 1.0));
    out.position = position_world_to_clip(out.world_position.xyz);
    if opacity <= 0.01 {
        // Clip the entire mesh before rasterizing an invisible player.
        out.position = vec4<f32>(2.0, 0.0, 0.0, 1.0);
    }
    out.world_normal = mesh_functions::mesh_normal_local_to_world(player.normal, in.instance_index);
    out.uv = player.uv;
    out.color = vec4<f32>(player.color.rgb, player.color.a * opacity);
    out.instance_index = in.instance_index;
    @if(VISIBILITY_RANGE_DITHER)
    out.visibility_range_dither = mesh_functions::get_visibility_range_dither_level(in.instance_index, model[3]);
    return out;
}
