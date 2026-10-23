#version 140
layout(row_major) uniform;
// layout(row_major) buffer;
// layout(binding = 1)
uniform sampler2DArray sprites_0;

struct EntryPointParams_0
{
    float text_contrast_0;
    float text_gamma_adjustment_0;
};

// layout(push_constant)
// layout(std430) uniform block_EntryPointParams_0
// {
uniform float text_contrast_0;
uniform float text_gamma_adjustment_0;
// }entryPointParams_0;
vec4 load_text_foreground_color_0(vec3 sprite_pos_0, float colored_sprite_0, vec3 cell_foreground_0)
{
    vec4 text_fg_0 = (textureLod((sprites_0), (sprite_pos_0), (0.0)));
    return vec4(mix(cell_foreground_0, text_fg_0.xyz, vec3(colored_sprite_0)), text_fg_0.w);
}

float clamp_to_unit_float_0(float x_0)
{
    return clamp(x_0, 0.0, 1.0);
}

float linear2srgb_0(float x_1)
{
    return mix(12.920000076293945 * x_1, 1.0549999475479126 * pow(x_1, 0.4166666567325592) - 0.054999999701976776, step(0.0031308000907301903, x_1));
}

float srgb2linear_0(float x_2)
{
    return mix(x_2 / 12.920000076293945, pow((x_2 + 0.054999999701976776) / 1.0549999475479126, 2.4000000953674316), step(0.040449999272823334, x_2));
}

vec4 foreground_contrast_old_0(vec4 over_0, vec3 under_0)
{
    vec4 _S1 = over_0;
    float under_luminance_0 = dot(under_0, vec3(0.2125999927520752, 0.71520000696182251, 0.072200000286102295));
    float over_lumininace_0 = dot(over_0.xyz, vec3(0.2125999927520752, 0.71520000696182251, 0.072200000286102295));
    float _S2 = over_0.w;
    _S1[3] = clamp_to_unit_float_0((srgb2linear_0(linear2srgb_0(over_lumininace_0) * _S2 + linear2srgb_0(under_luminance_0) * (1.0 - _S2)) - under_luminance_0) / (over_lumininace_0 - under_luminance_0));
    return _S1;
}

vec4 foreground_contrast_0(vec4 over_1, vec3 under_1, float text_contrast_1, float text_gamma_adjustment_1)
{
    return foreground_contrast_old_0(over_1, under_1);
}

vec4 adjust_foreground_contrast_with_background_0(vec4 text_fg_1, vec3 bg_0, float text_contrast_2, float text_gamma_adjustment_2)
{
    return foreground_contrast_0(text_fg_1, bg_0, text_contrast_2, text_gamma_adjustment_2);
}

vec4 alpha_blend_0(vec4 over_2, vec4 under_2)
{
    float _S3 = under_2.w;
    float _S4 = over_2.w;
    return vec4(mix(under_2.xyz * _S3, over_2.xyz, vec3(_S4)), mix(_S3, 1.0, _S4));
}

vec4 calculate_premul_foreground_from_sprites_0(vec3 sprite_pos_1, vec3 underline_pos_0, vec3 cursor_pos_0, vec3 strike_pos_0, uint underline_exclusion_pos_0, vec4 text_fg_2, vec3 decoration_fg_0, vec4 cursor_color_premult_0, float effective_text_alpha_0)
{
    int width_0;
    int height_0;
    int layer_0;
    ((width_0) = textureSize((sprites_0), 0).x), ((height_0) = textureSize((sprites_0), 0).y), ((layer_0) = textureSize((sprites_0), 0).z);
    ivec4 _S5 = ivec4(uvec4(uvec3(ivec3(int(sprite_pos_1.x * float(width_0)), int(underline_exclusion_pos_0), int(sprite_pos_1.z))), 0U));
    return mix(alpha_blend_0(vec4(text_fg_2.xyz, min(text_fg_2.w + (textureLod((sprites_0), (strike_pos_0), (0.0))).w, 1.0) * effective_text_alpha_0), vec4(decoration_fg_0, (textureLod((sprites_0), (underline_pos_0), (0.0))).w * (1.0 - (texelFetch((sprites_0), ((_S5)).xyz, ((_S5)).w)).w) * effective_text_alpha_0)), cursor_color_premult_0, vec4((textureLod((sprites_0), (cursor_pos_0), (0.0))).w * cursor_color_premult_0.w));
}

// layout(location = 0)
out vec4 entryPointParam_fragment_main_0;

// layout(location = 0)
in vec3 vo_background_0;

// layout(location = 2)
in float vo_effective_text_alpha_0;

// layout(location = 3)
in vec3 vo_sprite_pos_0;

// layout(location = 4)
in vec3 vo_underline_pos_0;

// layout(location = 5)
in vec3 vo_cursor_pos_0;

// layout(location = 6)
in vec3 vo_strike_pos_0;

flat
in uint vo_underline_exclusion_pos_0;

// layout(location = 8)
in vec3 vo_cell_foreground_0;

// layout(location = 9)
in vec4 vo_cursor_color_premult_0;

// layout(location = 10)
in vec3 vo_decoration_fg_0;

// layout(location = 11)
in float vo_colored_sprite_0;

void main()
{
    vec4 _S6 = vec4(ivec4(0));
    vec4 ans_premul_0;
    if((vo_underline_exclusion_pos_0 & 2147483648U) == 0U)
    {
        ans_premul_0 = calculate_premul_foreground_from_sprites_0(vo_sprite_pos_0, vo_underline_pos_0, vo_cursor_pos_0, vo_strike_pos_0, vo_underline_exclusion_pos_0, adjust_foreground_contrast_with_background_0(load_text_foreground_color_0(vo_sprite_pos_0, vo_colored_sprite_0, vo_cell_foreground_0), vo_background_0, text_contrast_0, text_gamma_adjustment_0), vo_decoration_fg_0, vo_cursor_color_premult_0, vo_effective_text_alpha_0);
    }
    else
    {
        ans_premul_0 = _S6;
    }
    entryPointParam_fragment_main_0 = ans_premul_0;
    return;
}
