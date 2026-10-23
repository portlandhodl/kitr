#version 140
layout(row_major) uniform;
// layout(row_major) buffer;
// layout(binding = 0)
uniform sampler2D image_0;

struct EntryPointParams_0
{
    vec3 amask_fg_0;
    vec4 amask_bg_premult_0;
    float extra_alpha_0;
};

// layout(push_constant)
// layout(std430) uniform block_EntryPointParams_0
// {
uniform vec3 amask_fg_0;
uniform vec4 amask_bg_premult_0;
uniform float extra_alpha_0;
// }entryPointParams_0;
vec4 vec4_premul_0(vec4 rgba_0)
{
    float _S1 = rgba_0.w;
    return vec4(rgba_0.xyz * _S1, _S1);
}

vec4 alpha_blend_premul_0(vec4 over_0, vec4 under_0)
{
    float _S2 = over_0.w;
    float inv_over_alpha_0 = 1.0 - _S2;
    return vec4(over_0.xyz + under_0.xyz * inv_over_alpha_0, _S2 + under_0.w * inv_over_alpha_0);
}

// layout(location = 0)
out vec4 entryPointParam_fragment_main_0;

// layout(location = 0)
in vec2 texcoord_0;

void main()
{
    entryPointParam_fragment_main_0 = vec4_premul_0(alpha_blend_premul_0(vec4_premul_0(vec4(amask_fg_0, (texture((image_0), (texcoord_0))).x)), amask_bg_premult_0));
    return;
}
