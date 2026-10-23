#version 140
layout(row_major) uniform;
// layout(row_major) buffer;
struct EntryPointParams_0
{
    vec4 rect_0;
    vec2 params_0;
    vec4 color_0;
    vec4 background_color_0;
    vec4 outer_color_0;
};

// layout(push_constant)
// layout(std430) uniform block_EntryPointParams_0
// {
uniform vec4 rect_0;
uniform vec2 params_0;
uniform vec4 color_0;
uniform vec4 background_color_0;
uniform vec4 outer_color_0;
// }entryPointParams_0;
float rounded_rectangle_sdf_0(vec2 p_0, vec2 b_0, float r_0)
{
    vec2 q_0 = abs(p_0) - b_0;
    return length(max(q_0, vec2(0.0))) + min(max(q_0.x, q_0.y), 0.0) - r_0;
}

vec4 alpha_blend_0(vec4 over_0, vec4 under_0)
{
    float _S1 = under_0.w;
    float _S2 = over_0.w;
    return vec4(mix(under_0.xyz * _S1, over_0.xyz, vec3(_S2)), mix(_S1, 1.0, _S2));
}

// layout(location = 0)
out vec4 entryPointParam_fragment_main_0;

void main()
{
    vec2 size_0 = rect_0.zw;
    float outer_edge_0 = - rounded_rectangle_sdf_0(gl_FragCoord.xy - size_0 / 2.0 - rect_0.xy, size_0 * 0.5 - params_0[1], params_0[1]);
    float outer_coverage_0 = smoothstep(-1.0, 1.0, outer_edge_0);
    float alpha_0 = outer_coverage_0 - smoothstep(-1.0, 1.0, outer_edge_0 - params_0[0]);
    vec4 ans_0 = color_0;
    ans_0[3] = ans_0[3] * alpha_0;
    entryPointParam_fragment_main_0 = alpha_blend_0(ans_0, mix(outer_color_0, background_color_0, vec4(outer_coverage_0)));
    return;
}
