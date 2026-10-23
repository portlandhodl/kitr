#version 140
#extension GL_ARB_explicit_attrib_location : require
// #extension GL_ARB_shader_draw_parameters : require
layout(row_major) uniform;
// layout(row_major) buffer;
struct EntryPointParams_0
{
    vec4 src_rect_0;
    vec4 dest_rect_0;
};

// layout(push_constant)
// layout(std430) uniform block_EntryPointParams_0
// {
uniform vec4 src_rect_0;
uniform vec4 dest_rect_0;
// }entryPointParams_0;
const ivec2  vertex_pos_map_0[4] = ivec2[](ivec2(2, 1), ivec2(2, 3), ivec2(0, 3), ivec2(0, 1));
struct BlitOutput_0
{
    vec2 texcoord_0;
    vec2 position_0;
};

BlitOutput_0 get_coords_for_blit_0(uint vertex_id_0, vec4 src_rect_1, vec4 dest_rect_1)
{
    BlitOutput_0 output_0;
    int _S1 = vertex_pos_map_0[vertex_id_0].x;
    int _S2 = vertex_pos_map_0[vertex_id_0].y;
    output_0.texcoord_0 = vec2(src_rect_1[_S1], src_rect_1[_S2]);
    output_0.position_0 = vec2(dest_rect_1[_S1], dest_rect_1[_S2]);
    return output_0;
}

struct VSOutput_0
{
    vec2 texcoord_1;
    vec4 position_1;
};

VSOutput_0 VSOutput_x24init_0(vec2 texcoord_2, vec4 position_2)
{
    VSOutput_0 _S3;
    _S3.texcoord_1 = texcoord_2;
    _S3.position_1 = position_2;
    return _S3;
}

// layout(location = 0)
out vec2 texcoord_0;

void main()
{
    BlitOutput_0 ans_0 = get_coords_for_blit_0(uint(gl_VertexID - 0), src_rect_0, dest_rect_0);
    VSOutput_0 _S4 = VSOutput_x24init_0(ans_0.texcoord_0, vec4(ans_0.position_0[0], ans_0.position_0[1], 0.0, 1.0));
    texcoord_0 = _S4.texcoord_1;
    gl_Position = _S4.position_1;
    return;
}
