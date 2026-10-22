// SPDX-License-Identifier: GPL-3.0-or-later
//! CSI parameter storage.
//!
//! The representation is deliberately identical to `ParsedCSI` in
//! `vt-parser.c`:
//!
//! * Only parameters that were actually committed are stored. A trailing
//!   separator does not add an implicit parameter, while an empty parameter
//!   between two separators is stored as an explicit `0`.
//! * `is_sub_param[i]` is true when parameter `i` is a continuation of a
//!   sub-parameter group introduced with `:`. The first parameter of a group
//!   has the flag cleared, matching the way `_parse_sgr()` walks the array.
//! * The start modifier (`?`, `>`, `<`, `=`), the end modifier/intermediate
//!   byte and the final byte are preserved separately.

/// Maximum number of parameters kept for one CSI sequence (`MAX_CSI_PARAMS`).
pub const MAX_CSI_PARAMS: usize = 256;

/// A parsed CSI sequence's parameters.
#[derive(Clone, Default)]
pub struct CsiParams {
    params: Vec<i32>,
    is_sub_param: Vec<bool>,
    primary: u8,
    secondary: u8,
    trailer: u8,
}

impl CsiParams {
    /// Create an empty parameter list. Mostly useful to handler tests.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of committed parameters. This is `num_params` in the C code.
    pub fn len(&self) -> usize {
        self.params.len()
    }

    /// True if no parameter was committed.
    pub fn is_empty(&self) -> bool {
        self.params.is_empty()
    }

    /// Get the parameter at `index`, if present.
    pub fn get(&self, index: usize) -> Option<i32> {
        self.params.get(index).copied()
    }

    /// Get a parameter, using `default` when it is absent.
    ///
    /// This is the `num_params > n ? params[n] : default` pattern used
    /// throughout the C dispatcher.
    pub fn param_or(&self, index: usize, default: i32) -> i32 {
        self.params.get(index).copied().unwrap_or(default)
    }

    /// All committed parameters, flat, in source order.
    pub fn raw_params(&self) -> &[i32] {
        &self.params
    }

    /// `is_sub_param` for each committed parameter.
    pub fn subparam_flags(&self) -> &[bool] {
        &self.is_sub_param
    }

    /// Whether parameter `index` is a `:` continuation of its group.
    pub fn is_sub_param(&self, index: usize) -> bool {
        self.is_sub_param.get(index).copied().unwrap_or(false)
    }

    /// Iterate over all committed parameters ignoring sub-parameter grouping.
    pub fn iter_flat(&self) -> impl Iterator<Item = i32> + '_ {
        self.params.iter().copied()
    }

    /// Return the sub-parameter group whose first parameter is `index`.
    ///
    /// The slice contains the group head followed by every following
    /// parameter flagged with [`Self::is_sub_param`]. Returns `None` when
    /// `index` is out of bounds or itself a continuation byte, mirroring the
    /// C dispatcher which only starts a group at a head parameter.
    pub fn subparams(&self, index: usize) -> Option<&[i32]> {
        if index >= self.params.len() || self.is_sub_param(index) {
            return None;
        }
        let mut end = index + 1;
        while end < self.params.len() && self.is_sub_param(end) {
            end += 1;
        }
        Some(&self.params[index..end])
    }

    /// The `start_modifier` byte (`?`, `>`, `<`, `=`) or zero.
    pub fn primary(&self) -> u8 {
        self.primary
    }

    /// The `end_modifier` / intermediate byte (for example `$`, `' '`, `#`)
    /// or zero.
    pub fn secondary(&self) -> u8 {
        self.secondary
    }

    /// The final byte of the sequence (the `action` passed to
    /// [`crate::Handler::csi_dispatch`]).
    pub fn trailer(&self) -> u8 {
        self.trailer
    }

    /// The C dispatcher's `AT_MOST_ONE_PARAMETER` / `AT_MOST_N` check.
    ///
    /// Returns `Err(count)` when more than `max` parameters are present.
    pub fn max_params_constraint(&self, max: usize) -> Result<(), usize> {
        if self.params.len() > max {
            Err(self.params.len())
        } else {
            Ok(())
        }
    }

    pub(crate) fn reset(&mut self) {
        self.params.clear();
        self.is_sub_param.clear();
        self.primary = 0;
        self.secondary = 0;
        self.trailer = 0;
    }

    pub(crate) fn push_param(&mut self, value: i32, is_sub_param: bool) {
        self.params.push(value);
        self.is_sub_param.push(is_sub_param);
    }

    pub(crate) fn set_primary(&mut self, primary: u8) {
        self.primary = primary;
    }

    pub(crate) fn set_secondary(&mut self, secondary: u8) {
        self.secondary = secondary;
    }

    pub(crate) fn set_trailer(&mut self, trailer: u8) {
        self.trailer = trailer;
    }
}
