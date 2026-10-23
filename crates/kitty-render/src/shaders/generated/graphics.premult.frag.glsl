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
// layout(location = 0)
out vec4 entryPointParam_fragment_main_0;

// layout(location = 0)
in vec2 texcoord_0;

void main()
{
    vec4 color_0 = (texture((image_0), (texcoord_0)));
    color_0[3] = color_0[3] * extra_alpha_0;
    entryPointParam_fragment_main_0 = color_0;
    return;
}
