#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Generate src/char_props_data.rs from kitty/char-props-data.h.

This is an offline parser for the tables emitted by kitty's gen/wcwidth.py.
It intentionally does not import or run the generator (which needs network
access for the UCD files); it only reads the checked-in generated C header.

The C `CharProps` and `GraphemeSegmentationResult` types are packed little
endian bitfields inside unions.  The exact bit positions below are taken from
the little-endian branch of their declarations in kitty/char-props.h:

CharProps (u32):
   0..=8    unnamed padding
   9..=11   shifted_width
  12        is_emoji
  13..=17   category
  18        is_emoji_presentation_base
  19        is_invalid
  20        is_non_rendered
  21        is_symbol
  22        is_combining_char
  23        is_word_char
  24        is_punctuation
  25..=28   grapheme_break
  29..=30   indic_conjunct_break
  31        is_extended_pictographic

The union's alternate `grapheme_segmentation_property : 7` bitfield occupies
bits 25..=31 and is therefore exactly
    grapheme_break | (indic_conjunct_break << 4) | (is_extended_pictographic << 6)

GraphemeSegmentationResult (u16):
   0..=5    unnamed padding
   6        add_to_current_cell
   7..=10   grapheme_break
  11        incb_consonant_extended
  12        incb_consonant_extended_linker
  13        incb_consonant_extended_linker_extended
  14        emoji_modifier_sequence
  15        emoji_modifier_sequence_before_last_char

The alternate `state : 9` bitfield occupies bits 7..=15 and is the value used
by grapheme_segmentation_step() as `state << 7 | ch.segmentation_property`.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

DEFAULT_INPUT = Path("/home/qrsnap/kitty/kitty/char-props-data.h")
DEFAULT_OUTPUT = Path(__file__).resolve().parent.parent / "src" / "char_props_data.rs"

CATEGORIES = [
    "Cn",
    "Cc",
    "Zs",
    "Po",
    "Sc",
    "Ps",
    "Pe",
    "Sm",
    "Pd",
    "Nd",
    "Lu",
    "Sk",
    "Pc",
    "Ll",
    "So",
    "Lo",
    "Pi",
    "Cf",
    "No",
    "Pf",
    "Lt",
    "Lm",
    "Mn",
    "Me",
    "Mc",
    "Nl",
    "Zl",
    "Zp",
    "Cs",
    "Co",
]

GRAPHEME_BREAKS = [
    "AtStart",
    "None",
    "Prepend",
    "CR",
    "LF",
    "Control",
    "Extend",
    "Regional_Indicator",
    "SpacingMark",
    "L",
    "V",
    "T",
    "LV",
    "LVT",
    "ZWJ",
    "Private_Expecting_RI",
]

INDIC_CONJUNCT_BREAKS = ["None", "Linker", "Consonant", "Extend"]

# (name, shift, mask)
CHAR_PROPS_FIELDS = [
    ("shifted_width", 9, 0x7),
    ("is_emoji", 12, 0x1),
    ("category", 13, 0x1F),
    ("is_emoji_presentation_base", 18, 0x1),
    ("is_invalid", 19, 0x1),
    ("is_non_rendered", 20, 0x1),
    ("is_symbol", 21, 0x1),
    ("is_combining_char", 22, 0x1),
    ("is_word_char", 23, 0x1),
    ("is_punctuation", 24, 0x1),
    ("grapheme_break", 25, 0xF),
    ("indic_conjunct_break", 29, 0x3),
    ("is_extended_pictographic", 31, 0x1),
]

GRAPHEME_RESULT_FIELDS = [
    ("add_to_current_cell", 6, 0x1),
    ("grapheme_break", 7, 0xF),
    ("incb_consonant_extended", 11, 0x1),
    ("incb_consonant_extended_linker", 12, 0x1),
    ("incb_consonant_extended_linker_extended", 13, 0x1),
    ("emoji_modifier_sequence", 14, 0x1),
    ("emoji_modifier_sequence_before_last_char", 15, 0x1),
]


def die(message: str) -> None:
    raise SystemExit(f"gen_char_props.py: {message}")


def parse_array(text: str, name: str) -> list[int]:
    match = re.search(rf"\b{name}\[(\d+)\]\s*=\s*\{{(.*?)\n\}};", text, re.DOTALL)
    if match is None:
        die(f"could not find C array {name}")
    declared = int(match.group(1))
    values = [int(x) for x in match.group(2).replace(",", " ").split()]
    if len(values) != declared:
        die(f"{name}: declared {declared} values, parsed {len(values)}")
    if any(x < 0 for x in values):
        die(f"{name}: negative value")
    return values


def parse_initializers(text: str, name: str) -> list[dict[str, str]]:
    match = re.search(rf"\b{name}\[(\d+)\]\s*=\s*\{{(.*?)\n\}};", text, re.DOTALL)
    if match is None:
        die(f"could not find C array {name}")
    declared = int(match.group(1))
    body = match.group(2)
    entries: list[dict[str, str]] = []
    for init in re.finditer(r"\{([^{}]*)\}", body):
        fields = dict(re.findall(r"\.(\w+)\s*=\s*([A-Za-z0-9_]+)", init.group(1)))
        if not fields:
            die(f"{name}: unparsable initializer: {init.group(0)!r}")
        entries.append(fields)
    if len(entries) != declared:
        die(f"{name}: declared {declared} entries, parsed {len(entries)}")
    return entries


def enum_postfix(value: str, prefix: str, allowed: list[str], what: str) -> int:
    if value.startswith(prefix):
        value = value[len(prefix) :]
    try:
        return allowed.index(value)
    except ValueError:
        die(f"unknown {what} value {value!r}")


def encode_char_props(fields: dict[str, str]) -> int:
    missing = {name for name, _, _ in CHAR_PROPS_FIELDS} - fields.keys()
    if missing:
        die(f"CharProps entry missing fields: {sorted(missing)}")
    val = 0
    for name, shift, mask in CHAR_PROPS_FIELDS:
        raw = fields[name]
        if name == "category":
            x = enum_postfix(raw, "UC_", CATEGORIES, "UnicodeCategory")
        elif name == "grapheme_break":
            x = enum_postfix(raw, "GBP_", GRAPHEME_BREAKS, "GraphemeBreakProperty")
        elif name == "indic_conjunct_break":
            x = enum_postfix(raw, "ICB_", INDIC_CONJUNCT_BREAKS, "IndicConjunctBreak")
        else:
            x = int(raw, 0)
        if x & ~mask:
            die(f"CharProps.{name}={x} does not fit in {mask.bit_length()} bits")
        val |= x << shift

    # The union's alternate view must agree with the individual fields.
    gbp = (val >> 25) & 0xF
    icb = (val >> 29) & 0x3
    ep = (val >> 31) & 0x1
    expected = gbp | (icb << 4) | (ep << 6)
    actual = (val >> 25) & 0x7F
    if actual != expected:
        die(f"CharProps segmentation property mismatch: {actual} != {expected}")
    return val


def encode_grapheme_result(fields: dict[str, str]) -> int:
    missing = {name for name, _, _ in GRAPHEME_RESULT_FIELDS} - fields.keys()
    if missing:
        die(f"GraphemeSegmentationResult entry missing fields: {sorted(missing)}")
    val = 0
    for name, shift, mask in GRAPHEME_RESULT_FIELDS:
        raw = fields[name]
        if name == "grapheme_break":
            x = enum_postfix(raw, "GBP_", GRAPHEME_BREAKS, "GraphemeBreakProperty")
        else:
            x = int(raw, 0)
        if x & ~mask:
            die(f"GraphemeSegmentationResult.{name}={x} does not fit in {mask.bit_length()} bits")
        val |= x << shift
    return val


def format_numbers(values: list[int], fmt: str, per_line: int = 16) -> str:
    lines = []
    for i in range(0, len(values), per_line):
        lines.append("\t" + ", ".join(fmt.format(x) for x in values[i : i + per_line]) + ",")
    return "\n".join(lines)


def format_wrapped(values: list[int], ctor: str, per_line: int = 8) -> str:
    lines = []
    for i in range(0, len(values), per_line):
        lines.append("\t" + " ".join(f"{ctor}({x:#x})," for x in values[i : i + per_line]))
    return "\n".join(lines)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, default=DEFAULT_INPUT)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    args = parser.parse_args(argv)

    text = args.input.read_text(encoding="utf-8")
    version_match = re.search(r"Unicode Standard (\d+\.\d+\.\d+)", text)
    if version_match is None:
        die("could not find the Unicode version in the input header")
    unicode_version = version_match.group(1)

    cp_mask_match = re.search(r"CharProps_mask\s*=\s*(\d+)u", text)
    cp_shift_match = re.search(r"CharProps_shift\s*=\s*(\d+)u", text)
    gs_mask_match = re.search(r"GraphemeSegmentationResult_mask\s*=\s*(\d+)u", text)
    gs_shift_match = re.search(r"GraphemeSegmentationResult_shift\s*=\s*(\d+)u", text)
    if not all((cp_mask_match, cp_shift_match, gs_mask_match, gs_shift_match)):
        die("could not find table mask/shift constants")
    cp_mask = int(cp_mask_match.group(1))
    cp_shift = int(cp_shift_match.group(1))
    gs_mask = int(gs_mask_match.group(1))
    gs_shift = int(gs_shift_match.group(1))

    cp_t1 = parse_array(text, "CharProps_t1")
    cp_t2 = parse_array(text, "CharProps_t2")
    cp_t3_raw = parse_initializers(text, "CharProps_t3")
    cp_t3 = [encode_char_props(x) for x in cp_t3_raw]

    gs_t1 = parse_array(text, "GraphemeSegmentationResult_t1")
    gs_t2_raw = parse_initializers(text, "GraphemeSegmentationResult_t2")
    gs_t2 = [encode_grapheme_result(x) for x in gs_t2_raw]

    max_unicode = (len(cp_t1) << cp_shift) - 1
    if max_unicode != 0x10FFFF:
        die(f"CharProps_t1 implies MAX_UNICODE={max_unicode}, expected 0x10FFFF")
    if len(cp_t1) != 1 + (max_unicode >> cp_shift):
        die("CharProps_t1 length does not match MAX_UNICODE")
    expected_t2 = (max(cp_t1) + 1) << cp_shift
    if len(cp_t2) != expected_t2:
        die(f"CharProps_t2 length {len(cp_t2)} != {expected_t2}")
    for value in cp_t1:
        if (value << cp_shift) + cp_mask >= len(cp_t2):
            die(f"CharProps_t1 value {value} points outside CharProps_t2")
    for value in cp_t2:
        if value >= len(cp_t3):
            die(f"CharProps_t2 value {value} points outside CharProps_t3")

    expected_gs_t2 = (max(gs_t1) + 1) << gs_shift
    if len(gs_t2) != expected_gs_t2:
        die(f"GraphemeSegmentationResult_t2 length {len(gs_t2)} != {expected_gs_t2}")
    for value in gs_t1:
        if (value << gs_shift) + gs_mask >= len(gs_t2):
            die(f"GraphemeSegmentationResult_t1 value {value} points outside t2")
    # A step reads key >> gs_shift with key < (1 << 16).
    if len(gs_t1) != (1 << 16) >> gs_shift:
        die("GraphemeSegmentationResult_t1 length does not cover the 16 bit key space")

    size_t1 = len(cp_t1)
    size_t2 = len(cp_t2)
    size_t3 = len(cp_t3) * 4
    gs_size = len(gs_t1) + len(gs_t2) * 2
    print(f"Unicode version: {unicode_version}")
    print(f"CharProps: t1={len(cp_t1)} bytes, t2={len(cp_t2)} bytes, t3={len(cp_t3)}*4={size_t3} bytes, total={size_t1 + size_t2 + size_t3} bytes")
    print(f"GraphemeSegmentationResult: t1={len(gs_t1)} bytes, t2={len(gs_t2)}*2={len(gs_t2) * 2} bytes, total={gs_size} bytes")

    out = []
    out.append("// SPDX-License-Identifier: GPL-3.0-or-later")
    out.append("// @generated by tools/gen_char_props.py from kitty/char-props-data.h. DO NOT EDIT.")
    out.append(f"// Unicode data, built from the Unicode Standard {unicode_version}")
    out.append("//")
    out.append("// CharProps is the little-endian packed bitfield union from char-props.h,")
    out.append("// stored as a u32. See tools/gen_char_props.py for the bit map.")
    out.append("// GraphemeSegmentationResult is the little-endian packed u16 union.")
    out.append("")
    out.append("use crate::char_props::{CharProps, GraphemeSegmentationResult};")
    out.append("")
    out.append(f'pub const UNICODE_VERSION: &str = "{unicode_version}";')
    out.append(f"pub const MAX_UNICODE: u32 = {max_unicode};")
    out.append(f"pub const CHAR_PROPS_MASK: u32 = {cp_mask};")
    out.append(f"pub const CHAR_PROPS_SHIFT: u32 = {cp_shift};")
    out.append(f"pub const GRAPHEME_SEGMENTATION_RESULT_MASK: u32 = {gs_mask};")
    out.append(f"pub const GRAPHEME_SEGMENTATION_RESULT_SHIFT: u32 = {gs_shift};")
    out.append("")
    out.append("#[rustfmt::skip]")
    out.append(f"pub(crate) static CHAR_PROPS_T1: [u8; {len(cp_t1)}] = [")
    out.append(format_numbers(cp_t1, "{}", 32))
    out.append("];")
    out.append("")
    out.append("#[rustfmt::skip]")
    out.append(f"pub(crate) static CHAR_PROPS_T2: [u8; {len(cp_t2)}] = [")
    out.append(format_numbers(cp_t2, "{}", 32))
    out.append("];")
    out.append("")
    out.append("#[rustfmt::skip]")
    out.append(f"pub(crate) static CHAR_PROPS_T3: [CharProps; {len(cp_t3)}] = [")
    out.append(format_wrapped(cp_t3, "CharProps::from_bits", 6))
    out.append("];")
    out.append("")
    out.append("#[rustfmt::skip]")
    out.append(f"pub(crate) static GRAPHEME_SEGMENTATION_RESULT_T1: [u8; {len(gs_t1)}] = [")
    out.append(format_numbers(gs_t1, "{}", 32))
    out.append("];")
    out.append("")
    out.append("#[rustfmt::skip]")
    out.append(
        f"pub(crate) static GRAPHEME_SEGMENTATION_RESULT_T2: [GraphemeSegmentationResult; {len(gs_t2)}] = ["
    )
    out.append(format_wrapped(gs_t2, "GraphemeSegmentationResult::from_bits", 6))
    out.append("];")
    out.append("")
    out.append("const _: () = {")
    out.append(f"    assert!(CHAR_PROPS_T1.len() == {len(cp_t1)});")
    out.append(f"    assert!(CHAR_PROPS_T2.len() == {len(cp_t2)});")
    out.append(f"    assert!(CHAR_PROPS_T3.len() == {len(cp_t3)});")
    out.append(f"    assert!(GRAPHEME_SEGMENTATION_RESULT_T1.len() == {len(gs_t1)});")
    out.append(f"    assert!(GRAPHEME_SEGMENTATION_RESULT_T2.len() == {len(gs_t2)});")
    out.append("};")
    out.append("")

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text("\n".join(out), encoding="utf-8")
    print(f"Wrote {args.output}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
