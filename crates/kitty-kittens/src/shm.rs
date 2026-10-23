// SPDX-License-Identifier: GPL-3.0-or-later
//! Small POSIX shared-memory helper used by the SSH bootstrap and askpass
//! protocols.
//!
//! The name is a normal `/dev/shm` entry. Memory is mapped read/write; callers
//! flush explicitly when another process must observe the bytes immediately.
//! This mirrors the ownership and permission checks of kitty's `go-shm`.

use std::ffi::CString;
use std::io;
use std::os::fd::RawFd;
use std::ptr::NonNull;

pub struct SharedMemory {
    name: String,
    ptr: NonNull<u8>,
    len: usize,
    owns_name: bool,
}

// The mapping is process-wide shared memory. Access is synchronized by the
// protocols using it (a single writer and a single reader).
unsafe impl Send for SharedMemory {}
unsafe impl Sync for SharedMemory {}

impl std::fmt::Debug for SharedMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedMemory")
            .field("name", &self.name)
            .field("len", &self.len)
            .finish()
    }
}

fn random_suffix() -> u64 {
    let mut bytes = [0u8; 8];
    use std::io::Read;
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        if file.read_exact(&mut bytes).is_ok() {
            return u64::from_ne_bytes(bytes);
        }
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    nanos ^ ((std::process::id() as u64) << 32)
}

fn check_open(result: RawFd, what: &str) -> io::Result<RawFd> {
    if result < 0 {
        Err(io::Error::last_os_error())
            .map_err(|err: io::Error| io::Error::new(err.kind(), format!("{what}: {err}")))
    } else {
        Ok(result)
    }
}

impl SharedMemory {
    /// Create a new shared-memory object with the given byte capacity.
    pub fn create_temp(prefix: &str, size: usize) -> io::Result<Self> {
        let size = size.max(1);
        for _ in 0..64 {
            let name = format!("/{prefix}{:016x}", random_suffix());
            let c_name = CString::new(name.clone()).expect("shm name has no NUL");
            let fd = unsafe {
                libc::shm_open(
                    c_name.as_ptr(),
                    libc::O_CREAT | libc::O_EXCL | libc::O_RDWR | libc::O_CLOEXEC,
                    0o600,
                )
            };
            if fd < 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EEXIST) {
                    continue;
                }
                return Err(io::Error::new(
                    err.kind(),
                    format!("shm_open failed: {err}"),
                ));
            }
            if unsafe { libc::ftruncate(fd, size as libc::off_t) } != 0 {
                let err = io::Error::last_os_error();
                unsafe {
                    libc::close(fd);
                    libc::shm_unlink(c_name.as_ptr());
                }
                return Err(io::Error::new(
                    err.kind(),
                    format!("ftruncate failed: {err}"),
                ));
            }
            let ptr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    size,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    fd,
                    0,
                )
            };
            unsafe { libc::close(fd) };
            if ptr == libc::MAP_FAILED {
                let err = io::Error::last_os_error();
                unsafe { libc::shm_unlink(c_name.as_ptr()) };
                return Err(io::Error::new(err.kind(), format!("mmap failed: {err}")));
            }
            return Ok(Self {
                name,
                ptr: NonNull::new(ptr as *mut u8).expect("mmap returned NULL"),
                len: size,
                owns_name: true,
            });
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not find a unique shared-memory name",
        ))
    }

    /// Open an existing shared-memory object. Verifies ownership and mode 0600.
    pub fn open_existing(name: &str) -> io::Result<Self> {
        let c_name = CString::new(name).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "shared-memory name contains NUL",
            )
        })?;
        let fd = check_open(
            unsafe { libc::shm_open(c_name.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC, 0) },
            "shm_open",
        )?;
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd, &mut stat) } != 0 {
            let err = io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(io::Error::new(err.kind(), format!("fstat failed: {err}")));
        }
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };
        if stat.st_uid != uid || stat.st_gid != gid {
            unsafe { libc::close(fd) };
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Incorrect owner on SHM file",
            ));
        }
        if stat.st_mode & 0o777 != 0o600 {
            unsafe { libc::close(fd) };
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Incorrect permissions on SHM file",
            ));
        }
        let size = stat.st_size.max(1) as usize;
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        unsafe { libc::close(fd) };
        if ptr == libc::MAP_FAILED {
            let err = io::Error::last_os_error();
            return Err(io::Error::new(err.kind(), format!("mmap failed: {err}")));
        }
        Ok(Self {
            name: name.to_owned(),
            ptr: NonNull::new(ptr as *mut u8).expect("mmap returned NULL"),
            len: size,
            owns_name: false,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }

    /// Copy `data` to the beginning of the mapping.
    pub fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        if data.len() > self.len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{} bytes do not fit in {} byte shared memory",
                    data.len(),
                    self.len
                ),
            ));
        }
        self.as_mut_slice()[..data.len()].copy_from_slice(data);
        Ok(())
    }

    /// Copy `data` at `offset`.
    pub fn write_at(&mut self, offset: usize, data: &[u8]) -> io::Result<()> {
        let end = offset
            .checked_add(data.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "offset overflow"))?;
        if end > self.len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "write is outside shared memory",
            ));
        }
        self.as_mut_slice()[offset..end].copy_from_slice(data);
        Ok(())
    }

    /// Read exactly `length` bytes starting at `offset`.
    pub fn read_at(&self, offset: usize, length: usize) -> io::Result<Vec<u8>> {
        let end = offset
            .checked_add(length)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "offset overflow"))?;
        if end > self.len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "read is outside shared memory",
            ));
        }
        Ok(self.as_slice()[offset..end].to_vec())
    }

    /// Flush dirty pages so another process sees them.
    pub fn flush(&self) -> io::Result<()> {
        if unsafe { libc::msync(self.ptr.as_ptr().cast(), self.len, libc::MS_SYNC) } != 0 {
            let err = io::Error::last_os_error();
            return Err(io::Error::new(err.kind(), format!("msync failed: {err}")));
        }
        Ok(())
    }

    /// Unlink the name immediately. Existing mappings stay valid.
    pub fn unlink(&mut self) {
        if self.owns_name {
            if let Ok(name) = CString::new(self.name.as_str()) {
                unsafe { libc::shm_unlink(name.as_ptr()) };
            }
            self.owns_name = false;
        }
    }
}

impl Drop for SharedMemory {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr.as_ptr().cast(), self.len);
        }
        self.unlink();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_write_read_and_open() {
        let mut shm = SharedMemory::create_temp("kitty-test-", 64).unwrap();
        shm.write_all(b"hello world").unwrap();
        shm.flush().unwrap();
        assert_eq!(&shm.as_slice()[..11], b"hello world");
        let name = shm.name().to_owned();
        let opened = SharedMemory::open_existing(&name).unwrap();
        assert_eq!(&opened.as_slice()[..11], b"hello world");
        shm.write_at(0, b"HELLO").unwrap();
        assert_eq!(&opened.as_slice()[..5], b"HELLO");
        drop(opened);
        shm.unlink();
        assert!(SharedMemory::open_existing(&name).is_err());
    }

    #[test]
    fn bounds_are_checked() {
        let mut shm = SharedMemory::create_temp("kitty-test-bounds-", 4).unwrap();
        shm.write_all(b"1234").unwrap();
        assert!(shm.write_all(b"12345").is_err());
        assert!(shm.read_at(2, 4).is_err());
        assert_eq!(shm.read_at(1, 2).unwrap(), b"23");
    }
}
