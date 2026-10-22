// SPDX-License-Identifier: GPL-3.0-or-later
//! POSIX shared-memory transmission for the graphics protocol (`t=s`).
//!
//! The object is opened read-only, unlinked immediately after opening, mapped
//! and copied into a `Vec`, then unmapped and closed. Unlinking immediately
//! means the name disappears even if reading or mapping later fails, so a
//! client cannot leak objects by sending invalid windows.

use crate::image::GraphicsError;

#[cfg(unix)]
mod posix {
    use core::ffi::{c_char, c_int, c_void};
    use std::ffi::CString;
    use std::ptr;

    use super::GraphicsError;

    type OffT = i64;
    type ModeT = u32;

    const O_RDONLY: c_int = 0;
    const O_RDWR: c_int = 2;
    const SEEK_END: c_int = 2;
    const PROT_READ: c_int = 1;
    const MAP_PRIVATE: c_int = 2;
    const EINTR: c_int = 4;
    #[cfg(target_os = "macos")]
    const O_CREAT: c_int = 0x200;
    #[cfg(target_os = "macos")]
    const O_TRUNC: c_int = 0x400;
    #[cfg(target_os = "macos")]
    const SC_PAGESIZE: c_int = 29;
    #[cfg(not(target_os = "macos"))]
    const O_CREAT: c_int = 0o100;
    #[cfg(not(target_os = "macos"))]
    const O_TRUNC: c_int = 0o1000;
    #[cfg(not(target_os = "macos"))]
    const SC_PAGESIZE: c_int = 30;

    #[link(name = "c")]
    extern "C" {
        fn shm_open(name: *const c_char, oflag: c_int, mode: ModeT) -> c_int;
        fn shm_unlink(name: *const c_char) -> c_int;
        fn close(fd: c_int) -> c_int;
        fn lseek(fd: c_int, offset: OffT, whence: c_int) -> OffT;
        fn ftruncate(fd: c_int, length: OffT) -> c_int;
        fn mmap(
            addr: *mut c_void,
            length: usize,
            prot: c_int,
            flags: c_int,
            fd: c_int,
            offset: OffT,
        ) -> *mut c_void;
        fn munmap(addr: *mut c_void, length: usize) -> c_int;
        fn sysconf(name: c_int) -> i64;
        fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
    }

    fn name_to_cstring(name: &[u8]) -> Result<CString, GraphicsError> {
        if name.is_empty() {
            return Err(GraphicsError::BadFile(
                "empty shared memory object name".into(),
            ));
        }
        if name.len() > 2048 {
            return Err(GraphicsError::BadFile(
                "shared memory object name is too long".into(),
            ));
        }
        CString::new(name)
            .map_err(|_| GraphicsError::BadFile("shared memory object name contains NUL".into()))
    }

    fn page_size() -> u64 {
        let value = unsafe { sysconf(SC_PAGESIZE) };
        if value > 0 {
            value as u64
        } else {
            4096
        }
    }

    pub fn read(
        name: &[u8],
        offset: u32,
        size: u32,
        max_to_read: usize,
    ) -> Result<Vec<u8>, GraphicsError> {
        let cname = name_to_cstring(name)?;
        let fd = unsafe { shm_open(cname.as_ptr(), O_RDONLY, 0) };
        if fd < 0 {
            return Err(GraphicsError::BadFile(
                "failed to open shared memory object".into(),
            ));
        }
        unsafe {
            shm_unlink(cname.as_ptr());
        }
        let result = read_fd(fd, offset, size, max_to_read);
        unsafe {
            close(fd);
        }
        result
    }

    fn read_fd(
        fd: c_int,
        offset: u32,
        size: u32,
        max_to_read: usize,
    ) -> Result<Vec<u8>, GraphicsError> {
        let total = unsafe { lseek(fd, 0, SEEK_END) };
        if total < 0 {
            return Err(GraphicsError::BadFile(
                "failed to determine shared memory size".into(),
            ));
        }
        let total = total as u64;
        let offset64 = u64::from(offset);
        if offset64 > total {
            return Err(GraphicsError::BadFile(
                "shared memory offset is past the end of the object".into(),
            ));
        }
        let available = total - offset64;
        let requested = if size == 0 {
            available
        } else {
            u64::from(size)
        };
        let requested = requested.min(max_to_read as u64);
        if requested > available {
            return Err(GraphicsError::BadFile(
                "shared memory object is smaller than requested".into(),
            ));
        }
        if requested == 0 {
            return Ok(Vec::new());
        }
        let requested = usize::try_from(requested)
            .map_err(|_| GraphicsError::DataTooLarge { limit: max_to_read })?;
        let page = page_size();
        let map_start = offset64 & !(page - 1);
        let delta = offset64 - map_start;
        let map_len = usize::try_from(delta)
            .ok()
            .and_then(|d| d.checked_add(requested))
            .ok_or(GraphicsError::DataTooLarge { limit: max_to_read })?;
        let ptr = unsafe {
            mmap(
                ptr::null_mut(),
                map_len,
                PROT_READ,
                MAP_PRIVATE,
                fd,
                map_start as OffT,
            )
        };
        if ptr == usize::MAX as *mut c_void {
            return Err(GraphicsError::BadFile(
                "failed to map shared memory object".into(),
            ));
        }
        let mut out = vec![0u8; requested];
        unsafe {
            ptr::copy_nonoverlapping(
                (ptr as *const u8).add(delta as usize),
                out.as_mut_ptr(),
                requested,
            );
            munmap(ptr, map_len);
        }
        Ok(out)
    }

    pub fn create(name: &[u8], data: &[u8]) -> Result<(), GraphicsError> {
        let cname = name_to_cstring(name)?;
        let fd = unsafe { shm_open(cname.as_ptr(), O_RDWR | O_CREAT | O_TRUNC, 0o600) };
        if fd < 0 {
            return Err(GraphicsError::BadFile(
                "failed to create shared memory object".into(),
            ));
        }
        let result = (|| {
            if unsafe { ftruncate(fd, data.len() as OffT) } != 0 {
                return Err(GraphicsError::BadFile(
                    "failed to size shared memory object".into(),
                ));
            }
            if data.is_empty() {
                return Ok(());
            }
            let mut written = 0usize;
            while written < data.len() {
                let n = unsafe {
                    write(
                        fd,
                        data[written..].as_ptr() as *const c_void,
                        data.len() - written,
                    )
                };
                if n < 0 {
                    if std::io::Error::last_os_error().raw_os_error() == Some(EINTR) {
                        continue;
                    }
                    return Err(GraphicsError::BadFile(
                        "failed to write shared memory object".into(),
                    ));
                }
                written += n as usize;
            }
            Ok(())
        })();
        unsafe {
            close(fd);
        }
        result
    }

    pub fn unlink(name: &[u8]) -> Result<(), GraphicsError> {
        let cname = name_to_cstring(name)?;
        if unsafe { shm_unlink(cname.as_ptr()) } != 0 {
            return Err(GraphicsError::BadFile(
                "failed to unlink shared memory object".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(unix)]
pub use posix::{create, read, unlink};

/// Read a window from a POSIX shared-memory object and unlink it.
#[cfg(unix)]
pub fn read_shared_memory(
    name: &[u8],
    offset: u32,
    size: u32,
    max_to_read: usize,
) -> Result<Vec<u8>, GraphicsError> {
    read(name, offset, size, max_to_read)
}

/// Create a POSIX shared-memory object with `data`, for clients and tests.
#[cfg(unix)]
pub fn create_shared_memory(name: &[u8], data: &[u8]) -> Result<(), GraphicsError> {
    create(name, data)
}

/// Remove a POSIX shared-memory name.
#[cfg(unix)]
pub fn unlink_shared_memory(name: &[u8]) -> Result<(), GraphicsError> {
    unlink(name)
}

#[cfg(not(unix))]
pub fn read_shared_memory(
    _name: &[u8],
    _offset: u32,
    _size: u32,
    _max_to_read: usize,
) -> Result<Vec<u8>, GraphicsError> {
    Err(GraphicsError::UnsupportedTransmission(
        crate::command::TransmissionType::SharedMemory,
    ))
}
