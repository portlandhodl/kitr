#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Generate the GLSL programs used by kitty-render.

This is a standalone port of the relevant parts of ``kitty/shaders/slang.py``
and ``kitty/shaders.c``.  It compiles the real Slang sources from a kitty
checkout with ``slangc`` and writes the fixed-up OpenGL GLSL, per-program
uniform metadata and a Rust manifest into ``src/shaders/generated``.

The generated files are checked in so a normal ``cargo build`` does not need
``slangc``.  Re-run this script after changing kitty's shaders:

    scripts/generate_shaders.py --compile

Environment:
    KITTY_SRC        path to the kitty checkout (auto-detected)
    SLANGC           path to slangc (or found in PATH)
    KITTY_SHADER_BUILD  module cache (defaults to $KITTY_SRC/build/shaders)
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from collections import OrderedDict
from itertools import product
from pathlib import Path

HERE = Path(__file__).resolve().parent
CRATE = HERE.parent
KITTY_SRC = Path(os.environ.get("KITTY_SRC", CRATE.parent.parent.parent)).resolve()
SHADERS = Path(os.environ.get("KITTY_SHADERS", KITTY_SRC / "kitty" / "shaders")).resolve()
# The C build writes fixed-up GLSL into <kitty root>/shaders.
BUILT_GLSL = Path(os.environ.get("KITTY_BUILT_GLSL", KITTY_SRC / "shaders")).resolve()
OUT = CRATE / "src" / "shaders" / "generated"
BUILD = Path(os.environ.get("KITTY_SHADER_BUILD", KITTY_SRC / "build" / "shaders"))
SLANGC = os.environ.get("SLANGC") or shutil.which("slangc") or "/tmp/opencode/tools/bin/slangc"
GLSL_VERSION = 140

# Mirrors line.h / data-types.h for the current kitty checkout.
MARK_MASK = 3
NUM_INDEXED_IMAGE_SPECIAL_COLORS = 8
REVERSE = 5
STRIKETHROUGH = 6
DIM = 7
BLINK = 8
DECORATION = 0
MARK = 9
DECORATION_MASK = 7
COLOR_NOT_SET = 0
COLOR_IS_SPECIAL = 1
COLOR_IS_INDEX = 2
COLOR_IS_RGB = 3

# The defaults used by kitty's cell_variant() on Linux.
CELL_DEFAULTS = {
    "FG_OVERRIDE_ALGO": "0",
    "TEXT_NEW_GAMMA": "true",
    "TEXT_CONTRAST_IDENTITY": "true",
    "RENDER_MODE": "0",
}

PROGRAMS = [
    # name, source, warning suppressions, specializations
    ("cell", "cell.slang", ["41012"], None),
    ("graphics", "graphics.slang", [], [
        ("", {}),
        ("alpha_mask", {"is_alpha_mask": "true"}),
        ("premult", {"texture_is_not_premultiplied": "false"}),
    ]),
    ("indexed_graphics", "indexed_graphics.slang", [], [
        ("", {}),
        ("unscaled", {"is_scaled": "false"}),
    ]),
    ("bgimage", "bgimage.slang", [], [("", {})]),
    ("tint", "tint.slang", [], [("", {})]),
    ("trail", "trail.slang", [], [("", {})]),
    ("blit", "blit.slang", [], [("", {})]),
    ("screenshot", "screenshot.slang", [], [("", {})]),
    ("rounded_rect", "rounded_rect.slang", [], [("", {})]),
    ("border", "border.slang", [], [("", {})]),
    ("padding", "padding.slang", [], [("", {})]),
]


def cell_variants():
    values = {
        "FG_OVERRIDE_ALGO": ("0", "1", "2"),
        "RENDER_MODE": ("0", "1", "2"),
        "TEXT_NEW_GAMMA": ("false", "true"),
    }
    ans = []
    for combo in product(*values.values()):
        variant = dict(zip(values.keys(), combo))
        if variant["TEXT_NEW_GAMMA"] == "true":
            for identity in ("false", "true"):
                ans.append({**variant, "TEXT_CONTRAST_IDENTITY": identity})
        else:
            ans.append({**variant, "TEXT_CONTRAST_IDENTITY": "false"})
    return ans


def variant_name(variant, default):
    if variant == default:
        return ""
    data = " ".join(f"{k}={variant[k]}" for k in sorted(default)).encode()
    return hashlib.md5(data, usedforsecurity=False).hexdigest()[:5]


def source_defines(name):
    if name in ("background.slang", "cell.slang"):
        return {
            "MARK_MASK": str(MARK_MASK),
            "NUM_INDEXED_IMAGE_SPECIAL_COLORS": str(NUM_INDEXED_IMAGE_SPECIAL_COLORS),
            "REVERSE_SHIFT": str(REVERSE),
            "STRIKE_SHIFT": str(STRIKETHROUGH),
            "DIM_SHIFT": str(DIM),
            "BLINK_SHIFT": str(BLINK),
            "DECORATION_SHIFT": str(DECORATION),
            "MARK_SHIFT": str(MARK),
            "DECORATION_MASK": str(DECORATION_MASK),
            "COLOR_NOT_SET": str(COLOR_NOT_SET),
            "COLOR_IS_SPECIAL": str(COLOR_IS_SPECIAL),
            "COLOR_IS_INDEX": str(COLOR_IS_INDEX),
            "COLOR_IS_RGB": str(COLOR_IS_RGB),
        }
    return {}


def run(cmd):
    proc = subprocess.run(cmd, text=True, capture_output=True)
    if proc.returncode != 0:
        sys.stderr.write("command failed:\n  " + " ".join(map(str, cmd)) + "\n")
        sys.stderr.write(proc.stdout)
        sys.stderr.write(proc.stderr)
        raise SystemExit(proc.returncode)
    return proc


def parse_module_and_imports(path):
    text = path.read_text()
    m = re.search(r"^\s*module\s+([A-Za-z_]\w*)\s*;", text, re.M)
    module = m.group(1) if m else None
    imports = set(re.findall(r"^\s*import\s+([A-Za-z_]\w*)\s*;", text, re.M))
    return module, imports


def compile_modules(build_dir):
    sources = {}
    for path in SHADERS.glob("*.slang"):
        module, imports = parse_module_and_imports(path)
        key = module or path.stem.replace("-", "_")
        sources[key] = (path, imports)
    layers = {}
    visiting = set()

    def layer(key):
        if key in layers:
            return layers[key]
        if key not in sources or key in visiting:
            return -1
        visiting.add(key)
        result = max((layer(dep) + 1 for dep in sources[key][1]), default=0)
        visiting.discard(key)
        layers[key] = result
        return result

    for key in sources:
        layer(key)
    for depth in range(max(layers.values(), default=0) + 1):
        for key, value in layers.items():
            if value != depth:
                continue
            path, _ = sources[key]
            defines = []
            for k, v in source_defines(path.name).items():
                defines.append("-D")
                defines.append(f"{k}={v}")
            module_path = build_dir / (path.stem + ".slang-module")
            cmd = [SLANGC, "-warnings-as-errors", "all", *defines,
                   "-I", str(build_dir), "-I", str(SHADERS),
                   "-target", "none", "-o", str(module_path), "--", str(path)]
            run(cmd)


def declaration_for(name, spec_vars):
    decl = spec_vars[name]
    decl = decl.rpartition("=")[0]
    if not decl:
        decl = spec_vars[name].rstrip(";")
    return decl.replace("extern ", "export ", 1)


def specializable_variables(path):
    ans = {}
    text = path.read_text()
    for m in re.finditer(r"^\s*extern\s+static\s+const\s+[^;]+?\s+([A-Za-z_]\w*)\s*(?:=[^;]*)?;", text, re.M):
        ans[m.group(1)] = m.group(0)
    return ans


def write_specialisations(build_dir):
    for name, source, _, specializations in PROGRAMS:
        if not specializations or all(not s[1] for s in specializations):
            continue
        path = SHADERS / source
        spec_vars = specializable_variables(path)
        base = build_dir / Path(source).stem
        for variant_name_, variables in specializations:
            if not variables:
                continue
            insert = f".{variant_name_}" if variant_name_ else ".default-specialization"
            dest = Path(str(base) + insert + ".slang")
            lines = []
            for key, value in variables.items():
                lines.append(f"{declaration_for(key, spec_vars)} = {value};")
            payload = "\n".join(lines)
            dest.write_text(payload + "\n")
            run([SLANGC, str(dest), "-o", str(dest) + "-module"])


def shader_name_from_path(path):
    parts = Path(path).name.split(".")
    if len(parts) > 1 and parts[1] in ("vert", "frag", "glsl"):
        return parts[0]
    return ".".join(parts[:2])


GLSL_LOOSE_UNIFORM_BLOCKS = frozenset({"GammaLUT"})


def fixup_opengl_code(glsl_code, shader_name, existing_metadata):
    is_fragment_shader = existing_metadata is None
    shader_name += ".frag.glsl" if is_fragment_shader else ".vert.glsl"
    lines = []
    in_uniform_block = False
    in_uniform_block_contents = False
    uniform_block_is_struct = False
    current_uniform_struct_members = {}
    current_uniform_struct_name = ""
    uniform_blocks = {}
    current_uniform_names = []
    uniform_names = {}
    uniform_structs = {}
    uniform_struct_names = {}
    input_locations = {}
    named_interface_blocks = set()
    pipeline_io_vars = {}
    replacements = {
        "gl_VertexIndex": "gl_VertexID",
        "gl_BaseVertex": "0",
        "gl_InstanceIndex": "gl_InstanceID",
        "gl_BaseInstance": "0",
    }
    fragment_inputs = {} if existing_metadata is None else existing_metadata["fragment_inputs"].copy()

    def register_pipeline_boundary_io(line, next_line):
        m = re.search(r"location = (\d+)", line)
        assert m is not None
        name = next_line.split()[-1].rstrip(";")
        location = int(m.group(1))
        if existing_metadata is None:
            if not next_line.startswith("out "):
                pipeline_io_vars[location] = name
        else:
            try:
                replacements[name] = fragment_inputs.pop(location)
            except KeyError:
                pass

    def add_uniform_name(name, target=uniform_names):
        name = name.rstrip(";")
        uniform_name = name.rpartition("_")[0]
        if uniform_name in target:
            raise KeyError(f"The uniform name {uniform_name} is used with multiple suffixes in {shader_name}")
        if "[" in name:
            name = name.partition("[")[0] + "[0]"
        target[uniform_name] = name
        return name

    src_lines = glsl_code.splitlines()
    for i, line in enumerate(src_lines):
        next_line = src_lines[i + 1] if i + 1 < len(src_lines) else ""
        if in_uniform_block:
            if in_uniform_block_contents:
                if line.startswith("}"):
                    in_uniform_block = in_uniform_block_contents = False
                    block_name = line.lstrip("}").rstrip(";").strip()
                    if uniform_block_is_struct:
                        uniform_structs[current_uniform_struct_name] = current_uniform_struct_members
                        named_interface_blocks.add(block_name)
                        line = "};"
                    else:
                        uniform_blocks[block_name] = current_uniform_names
                        line = "// " + line
                    current_uniform_names = []
                else:
                    if uniform_block_is_struct:
                        current_uniform_names.append(add_uniform_name(line.split()[-1], current_uniform_struct_members))
                    else:
                        line = line.strip()
                        current_uniform_names.append(add_uniform_name(line.split()[-1]))
                        line = "uniform " + line
            elif line.startswith("{"):
                if not uniform_block_is_struct:
                    line = "// " + line
                in_uniform_block_contents = True
                current_uniform_names = []
        else:
            if line.startswith("#version "):
                line = f"#version {GLSL_VERSION}"
                if not is_fragment_shader:
                    line += "\n#extension GL_ARB_explicit_attrib_location : require"
            elif line.startswith("#extension ") or line in ("layout(row_major) buffer;", "layout(push_constant)"):
                line = "// " + line
            elif line.startswith("layout(binding ="):
                line = "// " + line
            elif line.startswith("layout(location =") and (is_fragment_shader or next_line.startswith("out ")):
                register_pipeline_boundary_io(line, next_line)
                line = "// " + line
            elif line.startswith("flat layout(location ="):
                register_pipeline_boundary_io(line[len("flat "):], next_line)
                line = "flat"
            elif line:
                words = line.split()
                if "uniform" in words and line.startswith("layout("):
                    in_uniform_block = True
                    in_uniform_block_contents = False
                    block_name = words[-1]
                    slang_block_name = block_name[len("block_"):].rpartition("_")[0] if block_name.startswith("block_") else ""
                    uniform_block_is_struct = line.startswith("layout(std140") and slang_block_name not in GLSL_LOOSE_UNIFORM_BLOCKS
                    if uniform_block_is_struct:
                        current_uniform_struct_name = slang_block_name
                        assert block_name.startswith("block_")
                        current_uniform_struct_members = {}
                        uniform_struct_names[current_uniform_struct_name] = block_name
                    else:
                        line = "// " + line
                elif words[0] == "uniform" and len(words) > 2 and words[1].removeprefix("u").removeprefix("i").startswith("sampler"):
                    add_uniform_name(words[2])
                elif not is_fragment_shader and words[0] == "in":
                    name = words[-1].rstrip(";")
                    input_locations[name.rpartition("_")[0]] = int(lines[-1].split()[-1].rstrip(")"))
        lines.append(line)
    if fragment_inputs:
        raise ValueError(
            f"Could not match vertex outputs to fragment inputs for shader: {shader_name}. "
            f"Leftover fragment inputs: {', '.join(fragment_inputs.values())}"
        )
    ans = "\n".join(lines)
    for block_name, names in uniform_blocks.items():
        for u in names:
            u = u.partition("[")[0]
            replacements[f"{block_name}.{u}"] = u
    for x in named_interface_blocks:
        replacements[f"{x}."] = ""

    def sub(m):
        return replacements[m.group(1)]

    ans = re.sub(r"\b(" + "|".join(re.escape(word) for word in replacements) + r")\b", sub, ans)
    ans = re.sub(
        r"\b([A-Za-z_]\w*)\s+(\w+)\s*=\s*\{([^}]*)\}",
        lambda m: f"{m.group(1)} {m.group(2)} = {m.group(1)}({m.group(3)})",
        ans,
    )
    metadata = {
        "loose_uniforms": uniform_names,
        "uniform_structs": uniform_structs,
        "input_locations": input_locations,
        "uniform_struct_names": uniform_struct_names,
        "fragment_inputs": pipeline_io_vars if is_fragment_shader else {},
    }
    return ans, metadata


def compile_variant(source, variant_label, variables, warnings):
    base = Path(source).stem
    insert = f".{variant_label}" if variant_label else ""
    spec_insert = f".{variant_label}" if variant_label else ".default-specialization"
    module = BUILD / f"{base}.slang-module"
    spec_module = BUILD / f"{base}{spec_insert}.slang-module" if variables else None
    result = {}
    metadata = None
    for stage, entry in (("fragment", "fragment_main"), ("vertex", "vertex_main")):
        ext = "vert" if stage == "vertex" else "frag"
        out = OUT / f"{base}{insert}.{ext}.glsl"
        cmd = [SLANGC, "-warnings-as-errors", "all"]
        for warning in warnings:
            cmd += ["-warnings-disable", warning]
        cmd += ["-I", str(BUILD), str(module)]
        if spec_module is not None:
            cmd.insert(-1, str(spec_module))
        cmd += ["-line-directive-mode", "none", "-target", "glsl",
                "-profile", f"glsl_{max(150, GLSL_VERSION)}",
                "-entry", entry, "-stage", stage, "-o", str(out)]
        run(cmd)
        text = out.read_text()
        shader_name = f"{base}{insert}"
        if stage == "fragment":
            text, metadata = fixup_opengl_code(text, shader_name, None)
        else:
            text, vertex_metadata = fixup_opengl_code(text, shader_name, metadata)
            for key in ("loose_uniforms", "uniform_structs", "input_locations", "uniform_struct_names"):
                metadata[key].update(vertex_metadata[key])
        out.write_text(text)
        result[stage] = text
    (OUT / f"{base}{insert}.glsl.json").write_text(json.dumps(metadata, indent=2, sort_keys=True) + "\n")
    return result, metadata


def rust_str(value):
    return json.dumps(value)


def rust_pairs(mapping):
    items = ", ".join(f"({rust_str(k)}, {rust_str(v)})" for k, v in sorted(mapping.items()))
    return f"&[{items}]"


def write_manifest(entries):
    parts = []
    parts.append("// SPDX-License-Identifier: GPL-3.0-or-later
// @generated by scripts/generate_shaders.py; do not edit.\n")
    parts.append("use super::{GeneratedMetadata, GeneratedProgram};\n\n")
    parts.append("pub static BUILTIN_PROGRAMS: &[GeneratedProgram] = &[\n")
    for e in entries:
        structs = []
        for struct_name, members in sorted(e["metadata"]["uniform_structs"].items()):
            structs.append(f"({rust_str(struct_name)}, {rust_pairs(members)})")
        struct_slice = f"&[{', '.join(structs)}]" if structs else "&[]"
        metadata = (
            "GeneratedMetadata {\n"
            f"            loose_uniforms: {rust_pairs(e['metadata']['loose_uniforms'])},\n"
            f"            uniform_structs: {struct_slice},\n"
            f"            input_locations: {rust_pairs({k: str(v) for k, v in e['metadata']['input_locations'].items()})},\n"
            f"            uniform_struct_names: {rust_pairs(e['metadata']['uniform_struct_names'])},\n"
            "        }"
        )
        defines = ", ".join(f"({rust_str(k)}, {rust_str(v)})" for k, v in sorted(e["defines"].items()))
        parts.append("    GeneratedProgram {\n")
        parts.append(f"        program: {rust_str(e['program'])},\n")
        parts.append(f"        variant: {rust_str(e['variant'])},\n")
        parts.append(f"        defines: &[{defines}],\n")
        parts.append(f"        vertex: include_str!({rust_str(str(Path('generated') / (e['asset'] + '.vert.glsl')))}),\n")
        parts.append(f"        fragment: include_str!({rust_str(str(Path('generated') / (e['asset'] + '.frag.glsl')))}),\n")
        parts.append(f"        metadata: {metadata},\n")
        parts.append("    },\n")
    parts.append("];\n")
    (CRATE / "src" / "shaders" / "generated.rs").write_text("".join(parts))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--compile", action="store_true", help="run slangc instead of reusing checked in GLSL")
    args = parser.parse_args()

    OUT.mkdir(parents=True, exist_ok=True)
    if not Path(SLANGC).exists() and not shutil.which(SLANGC):
        raise SystemExit(f"slangc not found: {SLANGC} (set SLANGC or run scripts/build_c_kitty.sh)")

    if args.compile:
        BUILD.mkdir(parents=True, exist_ok=True)
        compile_modules(BUILD)
        write_specialisations(BUILD)

    entries = []
    for name, source, warnings, specializations in PROGRAMS:
        if specializations is None:
            specializations = [(variant_name(v, CELL_DEFAULTS), v) for v in cell_variants()]
            # Default variant is emitted twice by the product above only if the
            # defaults are not part of the grid; de-duplicate defensively.
            seen = set()
            unique = []
            for label, variables in specializations:
                if label in seen:
                    continue
                seen.add(label)
                unique.append((label, variables))
            specializations = unique
        for label, variables in specializations:
            base = Path(source).stem
            insert = f".{label}" if label else ""
            asset = f"{base}{insert}"
            if args.compile:
                compile_variant(source, label, variables, warnings)
            vert = (OUT / f"{asset}.vert.glsl")
            frag = (OUT / f"{asset}.frag.glsl")
            meta_path = OUT / f"{asset}.glsl.json"
            if not vert.exists() or not frag.exists() or not meta_path.exists():
                # Fall back to the C kitty build output when checked in files
                # are missing. This makes a fresh checkout usable without slangc.
                for src in (BUILT_GLSL / f"{asset}.vert.glsl", BUILT_GLSL / f"{asset}.frag.glsl"):
                    if not src.exists():
                        raise SystemExit(f"missing generated shader {src}; run with --compile")
                    shutil.copyfile(src, OUT / src.name)
                shutil.copyfile(BUILT_GLSL / f"{asset}.glsl.json", meta_path)
            metadata = json.loads(meta_path.read_text())
            defines = dict(variables)
            if name in ("cell",):
                defines = {**CELL_DEFAULTS, **variables}
            entries.append({
                "program": name,
                "variant": label or "default",
                "asset": asset,
                "defines": defines,
                "metadata": metadata,
            })
    write_manifest(entries)
    print(f"wrote {len(entries)} program variants to {OUT}")


if __name__ == "__main__":
    main()
