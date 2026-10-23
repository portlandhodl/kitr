#version 140
layout(row_major) uniform;
// layout(row_major) buffer;
// layout(location = 0)
out vec4 entryPointParam_fragment_main_0;

// layout(location = 1)
in vec4 vo_effective_background_premul_0;

void main()
{
    entryPointParam_fragment_main_0 = vo_effective_background_premul_0;
    return;
}
