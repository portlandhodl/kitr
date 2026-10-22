// SPDX-License-Identifier: GPL-3.0-or-later
//! Thread-safe wakeup pipe shared by the window backends.

use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use crate::types::WindowError;

pub(crate) struct WakeupPipe {
    read: UnixStream,
    write: UnixStream,
}

impl WakeupPipe {
    pub(crate) fn new() -> Result<Self, WindowError> {
        let (read, write) =
            UnixStream::pair().map_err(|_| WindowError::new("failed to create wakeup pipe"))?;
        read.set_nonblocking(true)
            .map_err(|_| WindowError::new("failed to configure wakeup pipe"))?;
        write
            .set_nonblocking(true)
            .map_err(|_| WindowError::new("failed to configure wakeup pipe"))?;
        Ok(Self { read, write })
    }

    pub(crate) fn wake(&self) {
        let mut write = &self.write;
        let _ = write.write_all(&[1u8]);
    }

    /// Drain all pending wakeup bytes. Returns true if any were present.
    pub(crate) fn drain(&self) -> bool {
        let mut read = &self.read;
        let mut buffer = [0u8; 128];
        let mut any = false;
        loop {
            match read.read(&mut buffer) {
                Ok(0) => break,
                Ok(_) => any = true,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
        any
    }

    pub(crate) fn read_fd(&self) -> std::os::unix::io::RawFd {
        self.read.as_raw_fd()
    }
}

/// Cloneable handle that lets another thread wake a blocked event loop.
#[derive(Clone)]
pub struct WakeupHandle {
    pipe: Arc<WakeupPipe>,
}

impl WakeupHandle {
    pub(crate) fn from_pipe(pipe: Arc<WakeupPipe>) -> Self {
        Self { pipe }
    }

    pub fn wakeup(&self) {
        self.pipe.wake();
    }
}
