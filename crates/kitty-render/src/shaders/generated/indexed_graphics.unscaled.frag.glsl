#version 140
layout(row_major) uniform;
// layout(row_major) buffer;
// layout(binding = 0)
uniform usampler2D image_0;

struct ColorTable_0
{
    uint  color_table_0[272];
};

// layout(binding = 2)
layout(std140) uniform block_ColorTable_0
{
    uint  color_table_0[272];
};
struct GammaLUT_0
{
    float  gamma_lut_0[256];
};

// layout(binding = 3)
// layout(std140) uniform block_GammaLUT_0
// {
uniform float  gamma_lut_0[256];
// }glt_0;
struct EntryPointParams_0
{
    uint has_alpha_0;
    float extra_alpha_0;
};

// layout(push_constant)
// layout(std430) uniform block_EntryPointParams_0
// {
uniform uint has_alpha_0;
uniform float extra_alpha_0;
// }entryPointParams_0;
vec3 color_to_vec_0(uint c_0)
{
    return vec3(gamma_lut_0[(c_0 >> 16) & 255U], gamma_lut_0[(c_0 >> 8) & 255U], gamma_lut_0[c_0 & 255U]);
}

vec3 indexed_image_color_0(uint idx_0)
{
    uint _S1 = idx_0 & 255U;
    uint slot_0;
    if(_S1 < 248U)
    {
        slot_0 = _S1;
    }
    else
    {
        slot_0 = _S1 + 16U;
    }
    return color_to_vec_0(color_table_0[slot_0]);
}

vec4 premult_color_at_0(ivec2 pos_0, uint has_alpha_1)
{
    ivec3 _S2 = ivec3(uvec3(uvec2(pos_0), 0U));
    uvec4 _S3 = (texelFetch((image_0), ((_S2)).xy, ((_S2)).z));
    float alpha_0;
    if(has_alpha_1 != 0U)
    {
        alpha_0 = float(_S3.y) / 255.0;
    }
    else
    {
        alpha_0 = 1.0;
    }
    return vec4(indexed_image_color_0(_S3.x) * alpha_0, alpha_0);
}

// layout(location = 0)
out vec4 entryPointParam_fragment_main_0;

// layout(location = 0)
in vec2 texcoord_0;

void main()
{
    int width_0;
    int height_0;
    ((width_0) = textureSize((image_0), 0).x), ((height_0) = textureSize((image_0), 0).y);
    entryPointParam_fragment_main_0 = premult_color_at_0(clamp(ivec2(texcoord_0 * vec2(float(width_0), float(height_0))), ivec2(0, 0), ivec2(width_0 - 1, height_0 - 1)), has_alpha_0) * extra_alpha_0;
    return;
}
