// SPDX-License-Identifier: GPL-3.0-or-later
//! A small, dependency-free terminal transport used by the interactive
//! kittens.
//!
//! The clipboard and notification kittens need to write an OSC request and
//! read the terminal's reply without ever blocking forever. [`StdioTransport`]
//! reads from a background thread and uses [`std::sync::mpsc::Receiver`]'s
//! timeout, which works for pipes and real terminals alike. Tests can supply
//! their own implementation of [`InteractiveTransport`].

use std::io::{self, Read, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Duration;

/// Default time to wait for a terminal reply.
pub const DEFAULT_REPLY_TIMEOUT: Duration = Duration::from_millis(750);

/// Write requests and read replies from an interactive terminal.
///
/// `read` must return [`io::ErrorKind::TimedOut`] rather than blocking beyond
/// `timeout`. Implementations are allowed to return fewer bytes than the
/// buffer can hold.
pub trait InteractiveTransport {
    /// Write all bytes and flush them to the terminal.
    fn write_all(&mut self, data: &[u8]) -> io::Result<()>;

    /// Read up to `buf.len()` bytes, waiting at most `timeout`.
    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize>;

    /// Flush pending writes.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

enum Incoming {
    Data(Vec<u8>),
    Error(io::Error),
    Eof,
}

/// A transport backed by a process's standard input and output.
///
/// Reads happen on a detached thread so a terminal that never answers causes a
/// timeout instead of a hang. Dropping the transport does not join that thread;
/// the process can exit normally even while standard input is still open.
pub struct StdioTransport {
    incoming: Receiver<Incoming>,
    pending: Vec<u8>,
    output: Box<dyn Write + Send>,
    eof: bool,
}

impl Default for StdioTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl StdioTransport {
    /// Read from stdin and write to stdout.
    pub fn new() -> Self {
        Self::with_io(io::stdin(), io::stdout())
    }

    /// Use an arbitrary reader as the source of terminal replies and an
    /// arbitrary writer as the terminal.
    pub fn with_io(
        mut reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("kitten-terminal-reader".to_owned())
            .spawn(move || {
                let mut buf = [0u8; 4096];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => {
                            let _ = tx.send(Incoming::Eof);
                            break;
                        }
                        Ok(n) => {
                            if tx.send(Incoming::Data(buf[..n].to_vec())).is_err() {
                                break;
                            }
                        }
                        Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                        Err(err) => {
                            let _ = tx.send(Incoming::Error(err));
                            break;
                        }
                    }
                }
            })
            .ok();
        Self {
            incoming: rx,
            pending: Vec::new(),
            output: Box::new(writer),
            eof: false,
        }
    }
}

impl InteractiveTransport for StdioTransport {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.output.write_all(data)?;
        self.output.flush()
    }

    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        if !self.pending.is_empty() {
            let n = self.pending.len().min(buf.len());
            buf[..n].copy_from_slice(&self.pending[..n]);
            self.pending.drain(..n);
            return Ok(n);
        }
        if self.eof {
            return Ok(0);
        }
        match self.incoming.recv_timeout(timeout) {
            Ok(Incoming::Data(data)) => {
                let n = data.len().min(buf.len());
                buf[..n].copy_from_slice(&data[..n]);
                if n < data.len() {
                    self.pending.extend_from_slice(&data[n..]);
                }
                Ok(n)
            }
            Ok(Incoming::Eof) => {
                self.eof = true;
                Ok(0)
            }
            Ok(Incoming::Error(err)) => Err(err),
            Err(RecvTimeoutError::Timeout) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for terminal reply",
            )),
            Err(RecvTimeoutError::Disconnected) => {
                self.eof = true;
                Ok(0)
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

/// A deterministic in-memory transport for tests and embedders.
#[derive(Clone, Debug, Default)]
pub struct ScriptedTransport {
    replies: std::collections::VecDeque<Vec<u8>>,
    written: Vec<u8>,
    force_timeout: bool,
}

impl ScriptedTransport {
    /// Create an empty script.
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a reply. Replies are returned in insertion order.
    pub fn push_reply(&mut self, reply: impl Into<Vec<u8>>) -> &mut Self {
        self.replies.push_back(reply.into());
        self
    }

    /// Make every read time out.
    pub fn set_timeout(&mut self, timeout: bool) -> &mut Self {
        self.force_timeout = timeout;
        self
    }

    /// Everything written through the transport.
    pub fn written(&self) -> &[u8] {
        &self.written
    }

    /// The bytes written as UTF-8, lossily.
    pub fn written_text(&self) -> String {
        String::from_utf8_lossy(&self.written).into_owned()
    }
}

impl InteractiveTransport for ScriptedTransport {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.written.extend_from_slice(data);
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8], _timeout: Duration) -> io::Result<usize> {
        if self.force_timeout {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "scripted timeout"));
        }
        let Some(reply) = self.replies.pop_front() else {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "no scripted reply"));
        };
        let n = reply.len().min(buf.len());
        buf[..n].copy_from_slice(&reply[..n]);
        if n < reply.len() {
            self.replies.push_front(reply[n..].to_vec());
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripted_transport_replays_replies_in_order() {
        let mut transport = ScriptedTransport::new();
        transport.push_reply(b"one".to_vec());
        transport.push_reply(b"two".to_vec());
        let mut buf = [0u8; 4];
        assert_eq!(transport.read(&mut buf, Duration::ZERO).unwrap(), 3);
        assert_eq!(&buf[..3], b"one");
        assert_eq!(transport.read(&mut buf, Duration::ZERO).unwrap(), 3);
        assert_eq!(&buf[..3], b"two");
    }

    #[test]
    fn scripted_transport_times_out() {
        let mut transport = ScriptedTransport::new();
        let err = transport.read(&mut [0u8; 8], Duration::ZERO).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        transport.set_timeout(true);
        let err = transport.read(&mut [0u8; 8], Duration::ZERO).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn stdio_transport_reads_pipe_without_hanging() {
        let mut transport = StdioTransport::with_io(&b"hello"[..], Vec::new());
        let mut buf = [0u8; 8];
        let n = transport.read(&mut buf, Duration::from_secs(1)).unwrap();
        assert_eq!(&buf[..n], b"hello");
        assert_eq!(transport.read(&mut buf, Duration::from_secs(1)).unwrap(), 0);
    }
}
