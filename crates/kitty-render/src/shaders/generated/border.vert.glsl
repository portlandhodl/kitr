#version 140
#extension GL_ARB_explicit_attrib_location : require
// #extension GL_ARB_shader_draw_parameters : require
layout(row_major) uniform;
// layout(row_major) buffer;
struct GammaLUT_0
{
    float  gamma_lut_0[256];
};

// layout(binding = 0)
// layout(std140) uniform block_GammaLUT_0
// {
uniform float  gamma_lut_0[256];
// }glt_0;
struct Colors_0
{
    uint  colors_0[9];
};

// layout(binding = 1)
layout(std140) uniform block_Colors_0
{
    uint  colors_0[9];
};
struct EntryPointParams_0
{
    float background_opacity_0;
};

// layout(push_constant)
// layout(std430) uniform block_EntryPointParams_0
// {
uniform float background_opacity_0;
// }entryPointParams_0;
float if_one_then_0(float condition_0, float thenval_0, float elseval_0)
{
    return mix(elseval_0, thenval_0, condition_0);
}

float zero_or_one_0(float x_0)
{
    return step(1.0, x_0);
}

vec3 if_one_then_1(vec3 condition_1, vec3 thenval_1, vec3 elseval_1)
{
    return mix(elseval_1, thenval_1, condition_1);
}

const uvec2  pos_map_0[4] = uvec2[](uvec2(2U, 1U), uvec2(2U, 3U), uvec2(0U, 3U), uvec2(0U, 1U));
float to_color_0(uint c_0)
{
    return gamma_lut_0[c_0 & 255U];
}

vec3 as_color_vector_0(uint c_1, int shift_0)
{
    return vec3(to_color_0(c_1 >> shift_0), to_color_0(c_1 >> (shift_0 - 8)), to_color_0(c_1 >> (shift_0 - 16)));
}

float is_integer_value_0(uint c_2, int x_1)
{
    return 1.0 - step(0.5, abs(float(c_2) - float(x_1)));
}

float float_x24init_0(float v_0)
{
    return v_0;
}

vec4 vec4_premul_0(vec3 rgb_0, float a_0)
{
    return vec4(rgb_0 * a_0, a_0);
}

// layout(location = 0)
out vec4 color_premul_0;

layout(location = 0)
in vec4 rect_0;

layout(location = 1)
in uint rect_color_0;

struct VertexOutput_0
{
    vec4 color_premul_0;
    vec4 position_0;
};

void main()
{
    uint _S1 = uint(gl_VertexID - 0);
    VertexOutput_0 output_0;
    output_0.position_0 = vec4(rect_0[pos_map_0[_S1].x], rect_0[pos_map_0[_S1].y], 0.0, 1.0);
    uint rc_0 = rect_color_0 & 255U;
    float _S2 = float(rc_0);
    output_0.color_premul_0 = vec4_premul_0(if_one_then_1(vec3(is_integer_value_0(rc_0, 3)), as_color_vector_0(rect_color_0, 24), as_color_vector_0(colors_0[rc_0], 16)), float_x24init_0(if_one_then_0(float_x24init_0(zero_or_one_0(abs((_S2 - 1.0) * (_S2 - 2.0) * (_S2 - 4.0) * (_S2 - 7.0) * (_S2 - 8.0)))), background_opacity_0, 1.0)));
    VertexOutput_0 _S3 = output_0;
    color_premul_0 = output_0.color_premul_0;
    gl_Position = _S3.position_0;
    return;
}
