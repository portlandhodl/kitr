#version 140
layout(row_major) uniform;
// layout(row_major) buffer;
struct EntryPointParams_0
{
    vec4 tint_color_0;
};

// layout(push_constant)
// layout(std430) uniform block_EntryPointParams_0
// {
uniform vec4 tint_color_0;
// }entryPointParams_0;
// layout(location = 0)
out vec4 entryPointParam_fragment_main_0;

void main()
{
    entryPointParam_fragment_main_0 = tint_color_0;
    return;
}
