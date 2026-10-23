#version 140
#extension GL_ARB_explicit_attrib_location : require
// #extension GL_ARB_shader_draw_parameters : require
layout(row_major) uniform;
// layout(row_major) buffer;
struct EntryPointParams_0
{
    vec4 edges_0;
};

// layout(push_constant)
// layout(std430) uniform block_EntryPointParams_0
// {
uniform vec4 edges_0;
// }entryPointParams_0;
void main()
{
    vec2  pos_map_0[4] = vec2[](vec2(edges_0[0], edges_0[1]), vec2(edges_0[0], edges_0[3]), vec2(edges_0[2], edges_0[3]), vec2(edges_0[2], edges_0[1]));
    gl_Position = vec4(pos_map_0[uint(gl_VertexID - 0)], 0.0, 1.0);
    return;
}
