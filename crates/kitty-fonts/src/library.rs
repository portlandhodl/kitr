// SPDX-License-Identifier: GPL-3.0-or-later
//! RAII wrapper around `FT_Library`.

use std::sync::OnceLock;

use crate::error::{ft_check, Error, Result};
use crate::ffi::freetype;

/// An initialized FreeType library object.
pub struct FreeTypeLibrary {
    ptr: freetype::FT_Library,
}

// FreeType's library object is safe to use from multiple threads. Face objects
// are not `Sync`, and the safe wrappers below do not implement `Sync` for them.
unsafe impl Send for FreeTypeLibrary {}
unsafe impl Sync for FreeTypeLibrary {}

impl FreeTypeLibrary {
    pub fn new() -> Result<Self> {
        let mut ptr = std::ptr::null_mut();
        let code = unsafe { freetype::FT_Init_FreeType(&mut ptr) };
        ft_check("FT_Init_FreeType", code)?;
        Ok(Self { ptr })
    }

    /// The process-wide FreeType library used by [`crate::Face`].
    pub fn global() -> Result<&'static Self> {
        static LIBRARY: OnceLock<std::result::Result<FreeTypeLibrary, Error>> = OnceLock::new();
        match LIBRARY.get_or_init(Self::new) {
            Ok(lib) => Ok(lib),
            Err(err) => Err(err.clone()),
        }
    }

    pub fn as_ptr(&self) -> freetype::FT_Library {
        self.ptr
    }
}

impl Drop for FreeTypeLibrary {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                freetype::FT_Done_FreeType(self.ptr);
            }
            self.ptr = std::ptr::null_mut();
        }
    }
}
