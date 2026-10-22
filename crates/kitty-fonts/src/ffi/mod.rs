// SPDX-License-Identifier: GPL-3.0-or-later
//! Raw FFI declarations.
//!
//! The three system libraries are linked directly, without `pkg-config` or a
//! build script. They are expected to be in the normal linker search path
//! (`libfreetype.so`, `libharfbuzz.so`, `libfontconfig.so`).

pub mod fontconfig;
pub mod freetype;
pub mod harfbuzz;
