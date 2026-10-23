#version 140
layout(row_major) uniform;
// layout(row_major) buffer;
// layout(binding = 0)
uniform sampler2D image_0;

struct EntryPointParams_0
{
    vec4 background_0;
};

// layout(push_constant)
// layout(std430) uniform block_EntryPointParams_0
// {
uniform vec4 background_0;
// }entryPointParams_0;
vec4 alpha_blend_0(vec4 over_0, vec4 under_0)
{
    float _S1 = under_0.w;
    float _S2 = over_0.w;
    return vec4(mix(under_0.xyz * _S1, over_0.xyz, vec3(_S2)), mix(_S1, 1.0, _S2));
}

// layout(location = 0)
out vec4 entryPointParam_fragment_main_0;

// layout(location = 0)
in vec2 texcoord_0;

void main()
{
    entryPointParam_fragment_main_0 = alpha_blend_0((texture((image_0), (texcoord_0))), background_0);
    return;
}
