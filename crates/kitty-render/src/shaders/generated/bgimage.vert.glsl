#version 140
#extension GL_ARB_explicit_attrib_location : require
// #extension GL_ARB_shader_draw_parameters : require
layout(row_major) uniform;
// layout(row_major) buffer;
struct EntryPointParams_0
{
    float tiled_0;
    vec4 sizes_0;
    vec4 positions_0;
};

// layout(push_constant)
// layout(std430) uniform block_EntryPointParams_0
// {
uniform float tiled_0;
uniform vec4 sizes_0;
uniform vec4 positions_0;
// }entryPointParams_0;
const vec2  tex_map_0[4] = vec2[](vec2(0.0, 0.0), vec2(0.0, 1.0), vec2(1.0, 1.0), vec2(1.0, 0.0));
float scale_factor_0(float window_size_0, float image_size_0)
{
    return window_size_0 / image_size_0;
}

float tiling_factor_0(int i_0, vec4 sizes_1, float tiled_1)
{
    return tiled_1 * scale_factor_0(sizes_1[i_0], sizes_1[i_0 + 2]) + (1.0 - tiled_1);
}

// layout(location = 0)
out vec2 texcoord_0;

struct VertexOutput_0
{
    vec2 texcoord_0;
    vec4 position_0;
};

void main()
{
    vec2  pos_map_0[4] = vec2[](vec2(positions_0[0], positions_0[1]), vec2(positions_0[0], positions_0[3]), vec2(positions_0[2], positions_0[3]), vec2(positions_0[2], positions_0[1]));
    uint _S1 = uint(gl_VertexID - 0);
    VertexOutput_0 output_0;
    output_0.texcoord_0 = vec2(tex_map_0[_S1][0] * tiling_factor_0(0, sizes_0, tiled_0), tex_map_0[_S1][1] * tiling_factor_0(1, sizes_0, tiled_0));
    output_0.position_0 = vec4(pos_map_0[uint(gl_VertexID - 0)], 0.0, 1.0);
    VertexOutput_0 _S2 = output_0;
    texcoord_0 = output_0.texcoord_0;
    gl_Position = _S2.position_0;
    return;
}
