// SPDX-License-Identifier: GPL-3.0-or-later
//! Prefer an upgraded system FreeType when it is installed under /usr/local.
//!
//! Some distributions ship an older FreeType in /lib64 while a newer one with
//! COLR v1 support lives in /usr/local/lib. Cargo's default linker search order
//! does not necessarily match `ldconfig`, so make the preference explicit when
//! that directory exists. The symbols used by this crate are ABI compatible
//! across FreeType 2.10+.

fn main() {
    for path in ["/usr/local/lib", "/usr/lib/local"] {
        if std::path::Path::new(path).join("libfreetype.so").exists() {
            println!("cargo:rustc-link-search=native={path}");
        }
    }
}
