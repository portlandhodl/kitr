#version 140
#extension GL_ARB_explicit_attrib_location : require
// #extension GL_ARB_shader_draw_parameters : require
layout(row_major) uniform;
// layout(row_major) buffer;
const vec2  vertex_pos_map_0[4] = vec2[](vec2(1.0, 1.0), vec2(1.0, -1.0), vec2(-1.0, -1.0), vec2(-1.0, 1.0));
void main()
{
    gl_Position = vec4(vertex_pos_map_0[uint(gl_VertexID - 0)], 0.0, 1.0);
    return;
}
