#version 140
layout(row_major) uniform;
// layout(row_major) buffer;
// layout(binding = 0)
uniform sampler2D image_0;

struct EntryPointParams_0
{
    vec2 src_size_0;
};

// layout(push_constant)
// layout(std430) uniform block_EntryPointParams_0
// {
uniform vec2 src_size_0;
// }entryPointParams_0;
float srgb2linear_0(float x_0)
{
    return mix(x_0 / 12.920000076293945, pow((x_0 + 0.054999999701976776) / 1.0549999475479126, 2.4000000953674316), step(0.040449999272823334, x_0));
}

vec3 srgb2linear_1(vec3 c_0)
{
    return vec3(srgb2linear_0(c_0.x), srgb2linear_0(c_0.y), srgb2linear_0(c_0.z));
}

vec3 safe_unpremult_to_linear_0(vec4 s_0)
{
    float _S1 = s_0.w;
    float _S2 = step(9.9999997473787516e-06, _S1);
    return mix(vec3(0.0), srgb2linear_1(s_0.xyz / mix(1.0, _S1, _S2)), vec3(_S2));
}

vec3 linear2srgb_0(vec3 x_1)
{
    return mix(12.920000076293945 * x_1, 1.0549999475479126 * pow(x_1, vec3(0.4166666567325592)) - 0.054999999701976776, step(vec3(0.0031308000907301903), x_1));
}

// layout(location = 0)
out vec4 entryPointParam_fragment_main_0;

// layout(location = 0)
in vec2 texcoord_0;

void main()
{
    vec2 texel_size_0 = 1.0 / src_size_0;
    vec4 _S3 = (texture((image_0), (texcoord_0 + vec2(-0.25, -0.25) * texel_size_0)));
    vec4 _S4 = (texture((image_0), (texcoord_0 + vec2(0.25, -0.25) * texel_size_0)));
    vec4 _S5 = (texture((image_0), (texcoord_0 + vec2(-0.25, 0.25) * texel_size_0)));
    vec4 _S6 = (texture((image_0), (texcoord_0 + vec2(0.25, 0.25) * texel_size_0)));
    float _S7 = _S3.w;
    float _S8 = _S4.w;
    float _S9 = _S5.w;
    float _S10 = _S6.w;
    float _S11 = _S7 + _S8 + _S9 + _S10;
    float _S12 = step(9.9999997473787516e-06, _S11);
    entryPointParam_fragment_main_0 = vec4(linear2srgb_0(mix(vec3(0.0), (safe_unpremult_to_linear_0(_S3) * _S7 + safe_unpremult_to_linear_0(_S4) * _S8 + safe_unpremult_to_linear_0(_S5) * _S9 + safe_unpremult_to_linear_0(_S6) * _S10) / mix(1.0, _S11, _S12), vec3(_S12))), _S11 * 0.25);
    return;
}
