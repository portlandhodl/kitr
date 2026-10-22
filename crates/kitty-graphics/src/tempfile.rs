// SPDX-License-Identifier: GPL-3.0-or-later
//! Temporary-file policy for `t=t` graphics transmissions.
//!
//! kitty only deletes temporary files whose name contains the marker
//! `tty-graphics-protocol`, and it trusts paths passed by the client after
//! asking the boss about read permission. This port takes the stricter route
//! required for a library: a temporary transmission must resolve to a regular
//! file inside one of the configured temporary directories, must not traverse
//! `..`, must not itself be a symbolic link, and is read exactly once and
//! removed immediately afterwards. Removal is repeated on every error path
//! once the path has passed validation, so a client cannot leak files by
//! sending malformed image data.
//!
//! The policy is intentionally independent of the regular-file (`t=f`)
//! transmission path. `t=f` keeps kitty's behaviour (any readable regular file
//! is allowed); `t=t` is the medium where the terminal is expected to clean up
//! after the client.

use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::image::{read_image_file, GraphicsError, MAX_DATA_SIZE};

/// Policy applied to `t=t` transmissions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TempFilePolicy {
    /// Canonical directories a temporary file may live in.
    pub roots: Vec<PathBuf>,
    /// Maximum number of bytes read from a temporary file.
    pub max_size: usize,
}

impl Default for TempFilePolicy {
    fn default() -> Self {
        Self::new(default_temp_roots(), MAX_DATA_SIZE)
    }
}

impl TempFilePolicy {
    /// Build a policy from directories. Non-existent roots are ignored; roots
    /// are canonicalised so comparisons are not fooled by symlinked temp
    /// directories such as `/tmp` on some platforms.
    pub fn new(roots: impl IntoIterator<Item = PathBuf>, max_size: usize) -> Self {
        let mut canonical = Vec::new();
        for root in roots {
            if let Ok(path) = fs::canonicalize(&root) {
                if !canonical.contains(&path) {
                    canonical.push(path);
                }
            }
        }
        Self {
            roots: canonical,
            max_size,
        }
    }

    /// True when `path` resides below one of the policy roots.
    pub fn contains(&self, path: &Path) -> bool {
        self.roots.iter().any(|root| path.starts_with(root))
    }

    /// Validate a `t=t` path without opening it.
    ///
    /// Returns the canonical path on success. Relative paths, `..` traversal,
    /// symbolic links and paths outside the policy roots are rejected.
    pub fn validate(&self, path: &Path) -> Result<PathBuf, GraphicsError> {
        if path.as_os_str().is_empty() {
            return Err(temp_error("empty temporary file name"));
        }
        if !path.is_absolute() {
            return Err(temp_error("temporary file name must be absolute"));
        }
        for component in path.components() {
            match component {
                Component::ParentDir => {
                    return Err(temp_error("temporary file path contains '..' traversal"))
                }
                Component::CurDir => {
                    return Err(temp_error("temporary file path contains a '.' component"))
                }
                _ => {}
            }
        }
        let metadata = fs::symlink_metadata(path)
            .map_err(|e| temp_error(format!("cannot access temporary file: {e}")))?;
        if metadata.file_type().is_symlink() {
            return Err(temp_error("temporary file is a symbolic link"));
        }
        // Reject symlinked ancestor directories too. Besides the final
        // component, a client could otherwise hide a path traversal behind a
        // symlinked directory that happens to resolve back into the temp root.
        for ancestor in path.ancestors().skip(1) {
            match fs::symlink_metadata(ancestor) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(temp_error("temporary file path contains a symbolic link"))
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        if !metadata.is_file() {
            return Err(temp_error("temporary file is not a regular file"));
        }
        let canonical =
            fs::canonicalize(path).map_err(|e| temp_error(format!("cannot resolve path: {e}")))?;
        if !self.contains(&canonical) {
            return Err(temp_error(
                "temporary file is outside the allowed temporary directories",
            ));
        }
        // canonicalize() resolves parent symlinks. Refuse a canonical target
        // that still names a symlink (defence in depth against races).
        if fs::symlink_metadata(&canonical)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Err(temp_error("temporary file is a symbolic link"));
        }
        Ok(canonical)
    }

    /// Remove a validated temporary file. Refuses to delete anything that is
    /// not a regular file inside a policy root.
    pub fn safe_unlink(&self, path: &Path) -> Result<bool, GraphicsError> {
        let canonical = match self.validate(path) {
            Ok(path) => path,
            // The file may already be gone (a client or the kernel cleaned it
            // up); that is success for our purposes.
            Err(_) if !path.exists() => return Ok(false),
            Err(error) => return Err(error),
        };
        match fs::remove_file(&canonical) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(temp_error(format!("cannot remove temporary file: {error}"))),
        }
    }
}

/// Read a `t=t` file window and immediately unlink it.
///
/// Validation happens before opening so a denied path cannot leak whether the
/// file exists. Once the path is accepted, the file is unlinked regardless of
/// whether the read or a later decode succeeds.
pub fn read_temp_image_file(
    path: &Path,
    offset: u32,
    size: u32,
    max_to_read: usize,
    policy: &TempFilePolicy,
) -> Result<Vec<u8>, GraphicsError> {
    let canonical = policy.validate(path)?;
    let limit = max_to_read.min(policy.max_size);
    let result = read_image_file(&canonical, offset, size, limit);
    let _ = policy.safe_unlink(&canonical);
    result
}

fn temp_error(message: impl Into<String>) -> GraphicsError {
    GraphicsError::BadFile(message.into())
}

/// The default set of temporary directories, mirroring `utils.is_path_in_temp_dir`.
pub fn default_temp_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(tmpdir) = std::env::var_os("TMPDIR") {
        roots.push(PathBuf::from(tmpdir));
    }
    roots.push(std::env::temp_dir());
    #[cfg(unix)]
    {
        roots.push(PathBuf::from("/tmp"));
        roots.push(PathBuf::from("/dev/shm"));
    }
    roots
}

/// True when `path` canonicalises to a location below a temporary directory.
pub fn is_path_in_temp_dir(path: &Path) -> bool {
    let Ok(path) = fs::canonicalize(path) else {
        return false;
    };
    TempFilePolicy::new(default_temp_roots(), MAX_DATA_SIZE).contains(&path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!(
                "kitty-graphics-temp-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn policy(dir: &TempDir) -> TempFilePolicy {
        TempFilePolicy::new([dir.0.clone()], 1024)
    }

    #[test]
    fn accepts_and_unlinks_inside_the_temp_root() {
        let dir = TempDir::new("ok");
        let path = dir.join("image.bin");
        fs::write(&path, b"0123456789").unwrap();
        let policy = policy(&dir);
        let data = read_temp_image_file(&path, 2, 4, 1024, &policy).unwrap();
        assert_eq!(data, b"2345");
        assert!(!path.exists(), "temp file must be removed after reading");
    }

    #[test]
    fn removes_the_file_even_when_the_read_fails() {
        let dir = TempDir::new("short");
        let path = dir.join("short.bin");
        fs::write(&path, b"abc").unwrap();
        let policy = policy(&dir);
        let error = read_temp_image_file(&path, 0, 100, 1024, &policy).unwrap_err();
        assert_eq!(error.code(), "EBADF");
        assert!(!path.exists(), "failed reads must still clean up");
    }

    #[test]
    fn rejects_relative_parent_and_outside_paths() {
        let dir = TempDir::new("evil");
        let policy = policy(&dir);
        assert!(policy.validate(Path::new("relative.bin")).is_err());
        let outside = std::env::temp_dir().join("kitty-graphics-outside-not-here");
        fs::write(&outside, b"x").unwrap();
        assert!(policy.validate(&outside).is_err());
        let _ = fs::remove_file(outside);
        let traversal = dir.join("..").join("escape.bin");
        fs::write(&traversal, b"x").unwrap();
        assert!(policy.validate(&traversal).is_err());
        let _ = fs::remove_file(traversal);
    }

    #[test]
    fn rejects_symlinks_even_when_the_target_is_inside() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let dir = TempDir::new("symlink");
            let real = dir.join("real.bin");
            fs::write(&real, b"data").unwrap();
            let link = dir.join("link.bin");
            symlink(&real, &link).unwrap();
            let policy = policy(&dir);
            let error = policy.validate(&link).unwrap_err();
            assert!(error.to_string().contains("symbolic link"));
            assert!(real.exists(), "the symlink target must not be deleted");
            assert!(link.exists());
        }
    }

    #[test]
    fn enforces_the_policy_size_limit_and_cleans_up() {
        let dir = TempDir::new("size");
        let path = dir.join("big.bin");
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(&[7u8; 4096]).unwrap();
        drop(file);
        let policy = TempFilePolicy::new([dir.0.clone()], 8);
        let data = read_temp_image_file(&path, 0, 0, 4096, &policy).unwrap();
        assert_eq!(data.len(), 8, "reads must be capped by the policy limit");
        assert!(!path.exists());
    }

    #[test]
    fn rejects_directories_and_missing_files() {
        let dir = TempDir::new("dirs");
        let policy = policy(&dir);
        assert!(policy.validate(&dir.0).is_err());
        assert!(policy.validate(&dir.join("missing")).is_err());
    }
}
