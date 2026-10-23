#version 140
#extension GL_ARB_explicit_attrib_location : require
// #extension GL_ARB_shader_draw_parameters : require
layout(row_major) uniform;
// layout(row_major) buffer;
struct CellRenderData_0
{
    vec4 default_fg_lin_0;
    vec4 default_bg_lin_0;
    vec4 highlight_fg_lin_0;
    vec4 highlight_bg_lin_0;
    vec4 main_cursor_fg_lin_0;
    vec4 main_cursor_bg_lin_0;
    vec4 url_color_lin_0;
    vec4 extra_cursor_fg_lin_0;
    vec4 extra_cursor_bg_lin_0;
    float use_cell_bg_for_selection_fg_0;
    float use_cell_fg_for_selection_fg_0;
    float use_cell_for_selection_bg_0;
    uint default_fg_0;
    uint url_style_0;
    uint inverted_0;
    uint main_cursor_is_special_0;
    uint extra_cursor_fg_0;
    uint extra_cursor_bg_0;
    uint columns_0;
    uint lines_0;
    uint sprites_xnum_0;
    uint sprites_ynum_0;
    uint cursor_shape_0;
    uint cell_width_0;
    uint cell_height_0;
    uint cursor_x1_0;
    uint cursor_x2_0;
    uint cursor_y1_0;
    uint cursor_y2_0;
    float cursor_opacity_0;
    float inactive_text_alpha_0;
    float fg_override_threshold_0;
    float row_offset_0;
    float dim_opacity_0;
    float blink_opacity_0;
    uint bg_colors0_0;
    uint bg_colors1_0;
    uint bg_colors2_0;
    uint bg_colors3_0;
    uint bg_colors4_0;
    uint bg_colors5_0;
    uint bg_colors6_0;
    uint bg_colors7_0;
    float bg_opacities0_0;
    float bg_opacities1_0;
    float bg_opacities2_0;
    float bg_opacities3_0;
    float bg_opacities4_0;
    float bg_opacities5_0;
    float bg_opacities6_0;
    float bg_opacities7_0;
};

// layout(binding = 2)
layout(std140) uniform block_CellRenderData_0
{
    vec4 default_fg_lin_0;
    vec4 default_bg_lin_0;
    vec4 highlight_fg_lin_0;
    vec4 highlight_bg_lin_0;
    vec4 main_cursor_fg_lin_0;
    vec4 main_cursor_bg_lin_0;
    vec4 url_color_lin_0;
    vec4 extra_cursor_fg_lin_0;
    vec4 extra_cursor_bg_lin_0;
    float use_cell_bg_for_selection_fg_0;
    float use_cell_fg_for_selection_fg_0;
    float use_cell_for_selection_bg_0;
    uint default_fg_0;
    uint url_style_0;
    uint inverted_0;
    uint main_cursor_is_special_0;
    uint extra_cursor_fg_0;
    uint extra_cursor_bg_0;
    uint columns_0;
    uint lines_0;
    uint sprites_xnum_0;
    uint sprites_ynum_0;
    uint cursor_shape_0;
    uint cell_width_0;
    uint cell_height_0;
    uint cursor_x1_0;
    uint cursor_x2_0;
    uint cursor_y1_0;
    uint cursor_y2_0;
    float cursor_opacity_0;
    float inactive_text_alpha_0;
    float fg_override_threshold_0;
    float row_offset_0;
    float dim_opacity_0;
    float blink_opacity_0;
    uint bg_colors0_0;
    uint bg_colors1_0;
    uint bg_colors2_0;
    uint bg_colors3_0;
    uint bg_colors4_0;
    uint bg_colors5_0;
    uint bg_colors6_0;
    uint bg_colors7_0;
    float bg_opacities0_0;
    float bg_opacities1_0;
    float bg_opacities2_0;
    float bg_opacities3_0;
    float bg_opacities4_0;
    float bg_opacities5_0;
    float bg_opacities6_0;
    float bg_opacities7_0;
};
struct ColorTable_0
{
    uint  color_table_0[272];
};

// layout(binding = 3)
layout(std140) uniform block_ColorTable_0
{
    uint  color_table_0[272];
};
struct GammaLUT_0
{
    float  gamma_lut_0[256];
};

// layout(binding = 4)
// layout(std140) uniform block_GammaLUT_0
// {
uniform float  gamma_lut_0[256];
// }glt_0;
// layout(binding = 0)
uniform usampler2D sprite_decorations_map_0;

float zero_or_one_0(float x_0)
{
    return step(1.0, x_0);
}

vec3 if_one_then_0(vec3 condition_0, vec3 thenval_0, vec3 elseval_0)
{
    return mix(elseval_0, thenval_0, condition_0);
}

float if_one_then_1(float condition_1, float thenval_1, float elseval_1)
{
    return mix(elseval_1, thenval_1, condition_1);
}

const uint  cursor_shape_map_0[5] = uint[](0U, 0U, 2U, 3U, 4U);
vec3 if_less_than_0(vec3 a_0, vec3 b_0, vec3 thenval_2, vec3 elseval_2)
{
    return mix(thenval_2, elseval_2, step(b_0, a_0));
}

const uvec2  cell_pos_map_0[4] = uvec2[](uvec2(1U, 0U), uvec2(1U, 1U), uvec2(0U, 1U), uvec2(0U, 0U));
const int  fg_index_map_0[3] = int[](0, 1, 0);
float float_x24init_0(float v_0)
{
    return v_0;
}

uint one_if_equal_zero_otherwise_0(int a_1, int b_1)
{
    return 1U - uint(float_x24init_0(zero_or_one_0(abs(float(a_1) - float(b_1)))));
}

uint resolve_color_0(uint c_0, uint defval_0)
{
    int t_0 = int(c_0 & 255U);
    uint is_one_0 = one_if_equal_zero_otherwise_0(t_0, 1);
    uint is_two_0 = one_if_equal_zero_otherwise_0(t_0, 2);
    uint _S1 = c_0 >> 8;
    return is_one_0 * color_table_0[_S1 & 255U] + is_two_0 * _S1 + (1U - is_one_0 - is_two_0) * defval_0;
}

vec3 color_to_vec_0(uint c_1)
{
    return vec3(gamma_lut_0[(c_1 >> 16) & 255U], gamma_lut_0[(c_1 >> 8) & 255U], gamma_lut_0[c_1 & 255U]);
}

uint one_if_equal_zero_otherwise_1(uint a_2, uint b_2)
{
    return 1U - uint(float_x24init_0(zero_or_one_0(abs(float(a_2) - float(b_2)))));
}

uint is_cursor_0(uint x_1, uint y_0)
{
    return one_if_equal_zero_otherwise_1(x_1, clamp(x_1, cursor_x1_0, cursor_x2_0)) * one_if_equal_zero_otherwise_1(y_0, clamp(y_0, cursor_y1_0, cursor_y2_0));
}

float one_if_equal_zero_otherwise_2(float a_3, float b_3)
{
    return float_x24init_0(1.0 - zero_or_one_0(abs(a_3 - b_3)));
}

vec3 resolve_dynamic_color_0(uint c_2, vec3 resolved_0, vec3 special_val_0, vec3 defval_1)
{
    float type_val_0 = float((c_2 >> 24) & 255U);
    return one_if_equal_zero_otherwise_2(type_val_0, 3.0) * resolved_0 + one_if_equal_zero_otherwise_2(type_val_0, 2.0) * resolved_0 + one_if_equal_zero_otherwise_2(type_val_0, 1.0) * special_val_0 + one_if_equal_zero_otherwise_2(type_val_0, 0.0) * defval_1;
}

struct ColorPair_0
{
    vec3 bg_0;
    vec3 fg_0;
};

ColorPair_0 ColorPair_x24init_0(vec3 bg_1, vec3 fg_1)
{
    ColorPair_0 _S2;
    _S2.bg_0 = bg_1;
    _S2.fg_0 = fg_1;
    return _S2;
}

float contrast_ratio_0(float under_luminance_0, float over_luminance_0)
{
    return clamp((max(under_luminance_0, over_luminance_0) + 0.05000000074505806) / (min(under_luminance_0, over_luminance_0) + 0.05000000074505806), 1.0, 21.0);
}

float contrast_ratio_1(vec3 a_4, vec3 b_4)
{
    return contrast_ratio_0(dot(a_4, vec3(0.2125999927520752, 0.71520000696182251, 0.072200000286102295)), dot(b_4, vec3(0.2125999927520752, 0.71520000696182251, 0.072200000286102295)));
}

float contrast_ratio_2(ColorPair_0 a_5)
{
    return contrast_ratio_1(a_5.bg_0, a_5.fg_0);
}

ColorPair_0 if_less_than_pair_0(float a_6, float b_5, ColorPair_0 thenval_3, ColorPair_0 elseval_3)
{
    vec3 _S3 = vec3(a_6);
    vec3 _S4 = vec3(b_5);
    return ColorPair_x24init_0(if_less_than_0(_S3, _S4, thenval_3.bg_0, elseval_3.bg_0), if_less_than_0(_S3, _S4, thenval_3.fg_0, elseval_3.fg_0));
}

ColorPair_0 resolve_extra_cursor_colors_for_special_cursor_0(vec3 cell_bg_0, vec3 cell_fg_0)
{
    ColorPair_0 cell_0 = ColorPair_x24init_0(cell_fg_0, cell_bg_0);
    ColorPair_0 base_0 = ColorPair_x24init_0(default_fg_lin_0.xyz, default_bg_lin_0.xyz);
    float cr_0 = contrast_ratio_2(cell_0);
    return if_less_than_pair_0(cr_0, 2.5, if_less_than_pair_0(cr_0, contrast_ratio_2(base_0), base_0, cell_0), cell_0);
}

ColorPair_0 if_one_then_pair_0(float condition_2, ColorPair_0 thenval_4, ColorPair_0 elseval_4)
{
    vec3 _S5 = vec3(condition_2);
    return ColorPair_x24init_0(if_one_then_0(_S5, thenval_4.bg_0, elseval_4.bg_0), if_one_then_0(_S5, thenval_4.fg_0, elseval_4.fg_0));
}

ColorPair_0 resolve_extra_cursor_colors_0(vec3 cell_bg_1, vec3 cell_fg_1, ColorPair_0 main_cursor_0)
{
    vec3 _S6 = main_cursor_0.bg_0;
    return if_one_then_pair_0(float_x24init_0(zero_or_one_0(abs(float(((extra_cursor_bg_0) >> 24) & 255U) - 1.0))), ColorPair_x24init_0(resolve_dynamic_color_0(extra_cursor_bg_0, extra_cursor_bg_lin_0.xyz, _S6, _S6), resolve_dynamic_color_0(extra_cursor_fg_0, extra_cursor_fg_lin_0.xyz, cell_bg_1, main_cursor_0.fg_0)), resolve_extra_cursor_colors_for_special_cursor_0(cell_bg_1, cell_fg_1));
}

float background_opacity_for_0(uint bg_2, uint colorval_0, float opacity_if_matched_0)
{
    float not_matched_0 = step(1.0, abs(float(colorval_0) - float(bg_2)));
    return not_matched_0 + opacity_if_matched_0 * (1.0 - not_matched_0);
}

float calc_background_opacity_0(uint bg_3)
{
    return background_opacity_for_0(bg_3, bg_colors0_0, bg_opacities0_0) * background_opacity_for_0(bg_3, bg_colors1_0, bg_opacities1_0) * background_opacity_for_0(bg_3, bg_colors2_0, bg_opacities2_0) * background_opacity_for_0(bg_3, bg_colors3_0, bg_opacities3_0) * background_opacity_for_0(bg_3, bg_colors4_0, bg_opacities4_0) * background_opacity_for_0(bg_3, bg_colors5_0, bg_opacities5_0) * background_opacity_for_0(bg_3, bg_colors6_0, bg_opacities6_0) * background_opacity_for_0(bg_3, bg_colors7_0, bg_opacities7_0);
}

struct CellData_0
{
    float has_cursor_0;
    float has_block_cursor_0;
    float reverse_video_cursor_0;
    uvec2 pos_0;
    uint cursor_fg_sprite_idx_0;
    ColorPair_0 cursor_0;
};

struct BackgroundOutput_0
{
    vec4 position_0;
    vec3 background_rgb_0;
    float bg_alpha_0;
    float cell_has_default_bg_0;
    vec3 bg_4;
    vec3 foreground_0;
    uint fg_as_uint_0;
    uint text_attrs_0;
    uint is_reversed_0;
    CellData_0 cell_data_0;
};

BackgroundOutput_0 compute_background_0(uvec3 colors_0, uvec2 sprite_idx_0, uint is_selected_0, uint instance_id_0, uint vertex_id_0)
{
    uvec2 default_colors_0 = uvec2(default_fg_0, bg_colors0_0);
    uint is_reversed_1 = ((sprite_idx_0[1]) >> 5) & 1U;
    uint is_inverted_0 = is_reversed_1 + inverted_0;
    int bg_index_0 = 1 - fg_index_map_0[is_inverted_0];
    int mark_0 = int((sprite_idx_0[1]) >> 9) & 3;
    uint has_mark_0 = uint(step(1.0, float(mark_0)));
    uint _S7 = 1U - has_mark_0;
    uint bg_as_uint_0 = has_mark_0 * color_table_0[256 + mark_0 - 1] + _S7 * resolve_color_0(colors_0[bg_index_0], default_colors_0[bg_index_0]);
    float cell_has_default_bg_1 = 1.0 - step(1.0, abs(float(bg_as_uint_0 - bg_colors0_0)));
    vec3 bg_5 = color_to_vec_0(bg_as_uint_0);
    uint fg_as_uint_1 = has_mark_0 * color_table_0[259 + mark_0] + _S7 * resolve_color_0(colors_0[fg_index_map_0[is_inverted_0]], default_colors_0[fg_index_map_0[is_inverted_0]]);
    vec3 foreground_1 = color_to_vec_0(fg_as_uint_1);
    float dx_0 = 2.0 / float(columns_0);
    float dy_0 = 2.0 / float(lines_0);
    uint row_0 = instance_id_0 / columns_0;
    uint column_0 = instance_id_0 - row_0 * columns_0;
    float left_0 = -1.0 + float(column_0) * dx_0;
    float top_0 = 1.0 - (float(row_0) + row_offset_0) * dy_0;
    BackgroundOutput_0 bo_0;
    bo_0.position_0 = vec4(vec2(left_0, left_0 + dx_0)[cell_pos_map_0[vertex_id_0].x], vec2(top_0, top_0 - dy_0)[cell_pos_map_0[vertex_id_0].y], 0.0, 1.0);
    float has_main_cursor_0 = float(is_cursor_0(column_0, row_0));
    float final_cursor_shape_0 = float_x24init_0(if_one_then_1(has_main_cursor_0, float(cursor_shape_0), float_x24init_0(if_one_then_1(float((is_selected_0 >> 4) & 1U), float(cursor_shape_0), float((is_selected_0 >> 2) & 3U)))));
    float has_cursor_1 = float_x24init_0(zero_or_one_0(final_cursor_shape_0));
    float is_block_cursor_0 = has_cursor_1 * one_if_equal_zero_otherwise_2(final_cursor_shape_0, 1.0);
    ColorPair_0 main_cursor_1;
    main_cursor_1.bg_0 = main_cursor_bg_lin_0.xyz;
    main_cursor_1.fg_0 = main_cursor_fg_lin_0.xyz;
    ColorPair_0 cursor_1 = if_one_then_pair_0(has_main_cursor_0, main_cursor_1, resolve_extra_cursor_colors_0(bg_5, if_one_then_0(vec3(float(((sprite_idx_0[1]) >> 7) & 1U)), mix(bg_5, foreground_1, vec3(dim_opacity_0)), foreground_1), main_cursor_1));
    uint extra_cursor_bg_type_0 = ((extra_cursor_bg_0) >> 24) & 255U;
    float explicit_extra_reverse_video_0 = float(one_if_equal_zero_otherwise_1(extra_cursor_bg_type_0, 1U));
    float inherited_reverse_video_0 = float(main_cursor_is_special_0) * float(one_if_equal_zero_otherwise_1(extra_cursor_bg_type_0, 0U)) * float(one_if_equal_zero_otherwise_1(((extra_cursor_fg_0) >> 24) & 255U, 0U));
    CellData_0 cell_data_1;
    cell_data_1.has_cursor_0 = has_cursor_1;
    cell_data_1.has_block_cursor_0 = is_block_cursor_0;
    cell_data_1.reverse_video_cursor_0 = float_x24init_0(if_one_then_1(has_main_cursor_0, float(main_cursor_is_special_0), max(explicit_extra_reverse_video_0, inherited_reverse_video_0)));
    cell_data_1.pos_0 = cell_pos_map_0[vertex_id_0];
    cell_data_1.cursor_fg_sprite_idx_0 = cursor_shape_map_0[int(final_cursor_shape_0)];
    cell_data_1.cursor_0 = cursor_1;
    float bg_alpha_1 = calc_background_opacity_0(bg_as_uint_0);
    float _S8 = float(is_selected_0 & 1U);
    float is_special_cell_0 = float_x24init_0(zero_or_one_0(is_block_cursor_0 + _S8 + float(is_reversed_1)));
    float cell_has_default_bg_2 = float_x24init_0(if_one_then_1(is_special_cell_0, 0.0, cell_has_default_bg_1));
    float bg_alpha_2 = float_x24init_0(if_one_then_1(is_block_cursor_0, max(cursor_opacity_0, bg_alpha_1), float_x24init_0(if_one_then_1(is_special_cell_0, 1.0, bg_alpha_1))));
    vec3 bg_6 = if_one_then_0(vec3(_S8), if_one_then_0(vec3(use_cell_for_selection_bg_0), foreground_1, highlight_bg_lin_0.xyz), bg_5);
    bo_0.background_rgb_0 = if_one_then_0(vec3(is_block_cursor_0), mix(bg_6, cell_data_1.cursor_0.bg_0, vec3(cursor_opacity_0)), bg_6);
    bo_0.bg_alpha_0 = bg_alpha_2;
    bo_0.cell_has_default_bg_0 = cell_has_default_bg_2;
    bo_0.bg_4 = bg_5;
    bo_0.foreground_0 = foreground_1;
    bo_0.fg_as_uint_0 = fg_as_uint_1;
    bo_0.text_attrs_0 = sprite_idx_0[1];
    bo_0.is_reversed_0 = is_reversed_1;
    bo_0.cell_data_0 = cell_data_1;
    return bo_0;
}

uvec3 to_sprite_coords_0(uint idx_0)
{
    uint sprites_per_page_0 = sprites_xnum_0 * sprites_ynum_0;
    uint z_0 = idx_0 / sprites_per_page_0;
    uint num_on_last_page_0 = idx_0 - sprites_per_page_0 * z_0;
    uint y_1 = num_on_last_page_0 / sprites_xnum_0;
    return uvec3(num_on_last_page_0 - sprites_xnum_0 * y_1, y_1, z_0);
}

vec3 to_sprite_pos_0(uvec2 pos_1, uint idx_1)
{
    uvec3 c_3 = to_sprite_coords_0(idx_1);
    float _S9 = float(c_3.x);
    vec2 s_xpos_0 = vec2(_S9, _S9 + 1.0) * (1.0 / float(sprites_xnum_0));
    float _S10 = float(c_3.y);
    vec2 s_ypos_0 = vec2(_S10, _S10 + 1.0) * (1.0 / float(sprites_ynum_0));
    s_ypos_0[1] = s_ypos_0[1] - 1.0 / float((cell_height_0 + 1U) * sprites_ynum_0);
    return vec3(s_xpos_0[pos_1.x], s_ypos_0[pos_1.y], float(c_3.z));
}

vec3 to_color_0(uint c_4, uint defval_2)
{
    return color_to_vec_0(resolve_color_0(c_4, defval_2));
}

uint read_sprite_decorations_idx_0(uvec2 sprite_idx_1)
{
    int idx_2 = int((sprite_idx_1[0]) & 2147483647U);
    int width_0;
    int height_0;
    ((width_0) = textureSize((sprite_decorations_map_0), 0).x), ((height_0) = textureSize((sprite_decorations_map_0), 0).y);
    int _S11 = width_0;
    int y_2 = idx_2 / width_0;
    ivec3 _S12 = ivec3(uvec3(uvec2(ivec2(idx_2 - y_2 * _S11, y_2)), 0U));
    return (texelFetch((sprite_decorations_map_0), ((_S12)).xy, ((_S12)).z)).x;
}

uvec2 get_decorations_indices_0(uvec2 sprite_idx_2, uint in_url_0, uint text_attrs_1)
{
    uint decorations_idx_0 = read_sprite_decorations_idx_0(sprite_idx_2);
    uint underline_style_0 = in_url_0 * url_style_0 + (1U - in_url_0) * ((text_attrs_1 >> 0) & 7U);
    return uint(float_x24init_0(zero_or_one_0(float(decorations_idx_0)))) * uvec2(decorations_idx_0 * ((text_attrs_1 >> 6) & 1U), uint(step(0.5, float(underline_style_0))) * (decorations_idx_0 + underline_style_0));
}

uint to_underline_exclusion_pos_0(uvec2 sprite_idx_3)
{
    uvec3 c_5 = to_sprite_coords_0(sprite_idx_3[0]);
    return c_5.y * (cell_height_0 + 1U) + cell_height_0;
}

// layout(location = 0)
out vec3 vo_background_0;

// layout(location = 1)
out vec4 entryPointParam_vertex_main_effective_background_premul_0;

// layout(location = 2)
out float vo_effective_text_alpha_0;

// layout(location = 3)
out vec3 vo_sprite_pos_0;

// layout(location = 4)
out vec3 vo_underline_pos_0;

// layout(location = 5)
out vec3 vo_cursor_pos_0;

// layout(location = 6)
out vec3 vo_strike_pos_0;

flat
out uint vo_underline_exclusion_pos_0;

// layout(location = 8)
out vec3 vo_cell_foreground_0;

// layout(location = 9)
out vec4 vo_cursor_color_premult_0;

// layout(location = 10)
out vec3 vo_decoration_fg_0;

// layout(location = 11)
out float vo_colored_sprite_0;

layout(location = 0)
in uvec3 colors_1;

layout(location = 1)
in uvec2 sprite_idx_4;

layout(location = 2)
in uint is_selected_1;

struct VertexOutput_0
{
    vec4 position_1;
    vec3 background_0;
    vec4 effective_background_premul_0;
    float effective_text_alpha_0;
    vec3 sprite_pos_0;
    vec3 underline_pos_0;
    vec3 cursor_pos_0;
    vec3 strike_pos_0;
    uint underline_exclusion_pos_0;
    vec3 cell_foreground_0;
    vec4 cursor_color_premult_0;
    vec3 decoration_fg_0;
    float colored_sprite_0;
};

void main()
{
    BackgroundOutput_0 bo_1 = compute_background_0(colors_1, sprite_idx_4, is_selected_1, uint(gl_InstanceID - 0), uint(gl_VertexID - 0));
    VertexOutput_0 vo_0;
    vo_0.position_1 = bo_1.position_0;
    vec3 _S13 = to_sprite_pos_0(bo_1.cell_data_0.pos_0, (sprite_idx_4[0]) & 2147483647U);
    vo_0.sprite_pos_0 = _S13;
    vo_0.colored_sprite_0 = float(((sprite_idx_4[0]) & 2147483648U) >> 31U);
    vo_0.effective_text_alpha_0 = float_x24init_0(inactive_text_alpha_0 * if_one_then_1(float(((bo_1.text_attrs_0) >> 7) & 1U), mix(dim_opacity_0, 1.0, bo_1.cell_data_0.has_block_cursor_0 * bo_1.cell_data_0.reverse_video_cursor_0 * cursor_opacity_0), 1.0) * if_one_then_1(float(((bo_1.text_attrs_0) >> 8) & 1U), blink_opacity_0, 1.0));
    float in_url_1 = float((is_selected_1 >> 1) & 1U);
    vec3 _S14 = if_one_then_0(vec3(in_url_1), url_color_lin_0.xyz, to_color_0(colors_1[2], bo_1.fg_as_uint_0));
    vo_0.decoration_fg_0 = _S14;
    vec3 selection_color_0 = if_one_then_0(vec3(use_cell_fg_for_selection_fg_0), bo_1.foreground_0, if_one_then_0(vec3(use_cell_bg_for_selection_fg_0), bo_1.bg_4, highlight_fg_lin_0.xyz));
    vec3 foreground_2 = if_one_then_0(vec3(float(is_selected_1 & 1U)), selection_color_0, bo_1.foreground_0);
    vec3 _S15 = if_one_then_0(vec3(float(is_selected_1 & 1U)), selection_color_0, _S14);
    vo_0.decoration_fg_0 = _S15;
    uvec2 decs_0 = get_decorations_indices_0(sprite_idx_4, uint(in_url_1), bo_1.text_attrs_0);
    vec3 _S16 = to_sprite_pos_0(bo_1.cell_data_0.pos_0, decs_0[0]);
    vo_0.strike_pos_0 = _S16;
    vec3 _S17 = to_sprite_pos_0(bo_1.cell_data_0.pos_0, decs_0[1]);
    vo_0.underline_pos_0 = _S17;
    vo_0.cursor_color_premult_0 = vec4(bo_1.cell_data_0.cursor_0.bg_0 * cursor_opacity_0, cursor_opacity_0);
    vec3 final_cursor_text_color_0 = mix(foreground_2, bo_1.cell_data_0.cursor_0.fg_0, vec3(cursor_opacity_0));
    vec3 _S18 = vec3(bo_1.cell_data_0.has_block_cursor_0);
    vec3 _S19 = if_one_then_0(_S18, final_cursor_text_color_0, foreground_2);
    vo_0.decoration_fg_0 = if_one_then_0(_S18, final_cursor_text_color_0, _S15);
    uint cursor_sprite_0 = bo_1.cell_data_0.cursor_fg_sprite_idx_0 * uint(bo_1.cell_data_0.has_cursor_0);
    vec3 _S20 = to_sprite_pos_0(bo_1.cell_data_0.pos_0, cursor_sprite_0);
    vo_0.cursor_pos_0 = _S20;
    bool _S21;
    if(((sprite_idx_4[0]) & 2147483647U) == 0U)
    {
        _S21 = (decs_0[0]) == 0U;
    }
    else
    {
        _S21 = false;
    }
    if(_S21)
    {
        _S21 = (decs_0[1]) == 0U;
    }
    else
    {
        _S21 = false;
    }
    if(_S21)
    {
        _S21 = cursor_sprite_0 == 0U;
    }
    else
    {
        _S21 = false;
    }
    uint skip_text_0 = uint(_S21);
    uint _S22 = to_underline_exclusion_pos_0(sprite_idx_4);
    vo_0.underline_exclusion_pos_0 = _S22 | (skip_text_0 * 2147483648U);
    vo_0.background_0 = bo_1.background_rgb_0;
    vo_0.cell_foreground_0 = _S19;
    VertexOutput_0 _S23 = vo_0;
    gl_Position = vo_0.position_1;
    vo_background_0 = _S23.background_0;
    entryPointParam_vertex_main_effective_background_premul_0 = _S23.effective_background_premul_0;
    vo_effective_text_alpha_0 = _S23.effective_text_alpha_0;
    vo_sprite_pos_0 = _S23.sprite_pos_0;
    vo_underline_pos_0 = _S23.underline_pos_0;
    vo_cursor_pos_0 = _S23.cursor_pos_0;
    vo_strike_pos_0 = _S23.strike_pos_0;
    vo_underline_exclusion_pos_0 = _S23.underline_exclusion_pos_0;
    vo_cell_foreground_0 = _S23.cell_foreground_0;
    vo_cursor_color_premult_0 = _S23.cursor_color_premult_0;
    vo_decoration_fg_0 = _S23.decoration_fg_0;
    vo_colored_sprite_0 = _S23.colored_sprite_0;
    return;
}
