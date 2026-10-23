#version 140
#extension GL_ARB_explicit_attrib_location : require
// #extension GL_ARB_shader_draw_parameters : require
layout(row_major) uniform;
// layout(row_major) buffer;
struct EntryPointParams_0
{
    vec4 x_coords_0;
    vec4 y_coords_0;
};

// layout(push_constant)
// layout(std430) uniform block_EntryPointParams_0
// {
uniform vec4 x_coords_0;
uniform vec4 y_coords_0;
// }entryPointParams_0;
// layout(location = 0)
out vec2 frag_pos_0;

struct VertexOutput_0
{
    vec2 frag_pos_0;
    vec4 position_0;
};

void main()
{
    vec2 pos_0 = vec2(x_coords_0[uint(gl_VertexID - 0)], y_coords_0[uint(gl_VertexID - 0)]);
    VertexOutput_0 output_0;
    output_0.position_0 = vec4(pos_0, 1.0, 1.0);
    output_0.frag_pos_0 = pos_0;
    VertexOutput_0 _S1 = output_0;
    frag_pos_0 = output_0.frag_pos_0;
    gl_Position = _S1.position_0;
    return;
}
