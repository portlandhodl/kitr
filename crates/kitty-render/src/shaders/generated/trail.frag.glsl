#version 140
layout(row_major) uniform;
// layout(row_major) buffer;
struct EntryPointParams_0
{
    vec2 cursor_edge_x_0;
    vec2 cursor_edge_y_0;
    vec3 trail_color_0;
    float trail_opacity_0;
};

// layout(push_constant)
// layout(std430) uniform block_EntryPointParams_0
// {
uniform vec2 cursor_edge_x_0;
uniform vec2 cursor_edge_y_0;
uniform vec3 trail_color_0;
uniform float trail_opacity_0;
// }entryPointParams_0;
// layout(location = 0)
out vec4 entryPointParam_fragment_main_0;

// layout(location = 0)
in vec2 frag_pos_0;

void main()
{
    float opacity_0 = trail_opacity_0 * (1.0 - step(cursor_edge_x_0[0], frag_pos_0.x) * step(frag_pos_0.x, cursor_edge_x_0[1]) * (step(cursor_edge_y_0[1], frag_pos_0.y) * step(frag_pos_0.y, cursor_edge_y_0[0])));
    entryPointParam_fragment_main_0 = vec4(trail_color_0 * opacity_0, opacity_0);
    return;
}
