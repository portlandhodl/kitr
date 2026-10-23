#version 140
layout(row_major) uniform;
// layout(row_major) buffer;
// layout(binding = 0)
uniform sampler2D image_0;

vec3 linear2srgb_0(vec3 x_0)
{
    return mix(12.920000076293945 * x_0, 1.0549999475479126 * pow(x_0, vec3(0.4166666567325592)) - 0.054999999701976776, step(vec3(0.0031308000907301903), x_0));
}

vec4 vec4_premul_0(vec3 rgb_0, float a_0)
{
    return vec4(rgb_0 * a_0, a_0);
}

// layout(location = 0)
out vec4 entryPointParam_fragment_main_0;

// layout(location = 0)
in vec2 texcoord_0;

void main()
{
    vec4 color_premul_0 = (texture((image_0), (texcoord_0)));
    float _S1 = color_premul_0.w;
    entryPointParam_fragment_main_0 = vec4_premul_0(linear2srgb_0(color_premul_0.xyz / _S1), _S1);
    return;
}
