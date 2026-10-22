// SPDX-License-Identifier: GPL-3.0-or-later
//! Bridge between the `kitty-remote` socket threads and the GUI thread.
//!
//! `RemoteServer` invokes [`RemoteHandler`] methods on its own connection
//! threads. Those methods package the parsed request into a [`RemoteJob`] and
//! block for the GUI event loop to execute it against the active tab. Keeping
//! all screen/PTY/GL access on the GUI thread avoids locking the whole session.

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use kitty_remote::{
    CloseTabRequest, CloseWindowRequest, CommandError, CommandResult, FocusWindowRequest,
    GetColorsRequest, GetTextRequest, RemoteHandler, RequestContext, ResizeWindowRequest,
    ScreenshotCallback, ScreenshotFormat, ScreenshotImage, ScreenshotRequest, SendKeyRequest,
    SendTextRequest, SetColorsRequest, SetWindowTitleRequest,
};

const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) enum RemoteJob {
    GetText(GetTextRequest, Sender<CommandResult>),
    SendText(SendTextRequest, Sender<CommandResult>),
    SendKey(SendKeyRequest, Sender<CommandResult>),
    ResizeWindow(ResizeWindowRequest, Sender<CommandResult>),
    CloseWindow(CloseWindowRequest, Sender<CommandResult>),
    CloseTab(CloseTabRequest, Sender<CommandResult>),
    FocusWindow(FocusWindowRequest, Sender<CommandResult>),
    SetWindowTitle(SetWindowTitleRequest, Sender<CommandResult>),
    GetColors(Sender<CommandResult>),
    SetColors(SetColorsRequest, Sender<CommandResult>),
    Screenshot(
        ScreenshotRequest,
        Sender<Result<ScreenshotImage, CommandError>>,
    ),
}

pub(crate) struct RemoteBridge {
    sender: Sender<RemoteJob>,
}

impl RemoteBridge {
    pub(crate) fn new() -> (Self, Receiver<RemoteJob>) {
        let (sender, receiver) = mpsc::channel();
        (Self { sender }, receiver)
    }

    fn call(&self, make: impl FnOnce(Sender<CommandResult>) -> RemoteJob) -> CommandResult {
        let (tx, rx) = mpsc::channel();
        self.sender
            .send(make(tx))
            .map_err(|_| CommandError::new("the kitty GUI event loop is not running"))?;
        rx.recv_timeout(REPLY_TIMEOUT)
            .map_err(|_| CommandError::new("timed out waiting for the kitty GUI event loop"))?
    }
}

impl RemoteHandler for RemoteBridge {
    fn get_text(&self, _ctx: &RequestContext, request: GetTextRequest) -> CommandResult {
        self.call(|tx| RemoteJob::GetText(request, tx))
    }

    fn send_text(&self, _ctx: &RequestContext, request: SendTextRequest) -> CommandResult {
        self.call(|tx| RemoteJob::SendText(request, tx))
    }

    fn send_key(&self, _ctx: &RequestContext, request: SendKeyRequest) -> CommandResult {
        self.call(|tx| RemoteJob::SendKey(request, tx))
    }

    fn resize_window(&self, _ctx: &RequestContext, request: ResizeWindowRequest) -> CommandResult {
        self.call(|tx| RemoteJob::ResizeWindow(request, tx))
    }

    fn close_window(&self, _ctx: &RequestContext, request: CloseWindowRequest) -> CommandResult {
        self.call(|tx| RemoteJob::CloseWindow(request, tx))
    }

    fn close_tab(&self, _ctx: &RequestContext, request: CloseTabRequest) -> CommandResult {
        self.call(|tx| RemoteJob::CloseTab(request, tx))
    }

    fn focus_window(&self, _ctx: &RequestContext, request: FocusWindowRequest) -> CommandResult {
        self.call(|tx| RemoteJob::FocusWindow(request, tx))
    }

    fn set_window_title(
        &self,
        _ctx: &RequestContext,
        request: SetWindowTitleRequest,
    ) -> CommandResult {
        self.call(|tx| RemoteJob::SetWindowTitle(request, tx))
    }

    fn get_colors(&self, _ctx: &RequestContext, request: GetColorsRequest) -> CommandResult {
        let _ = request;
        self.call(RemoteJob::GetColors)
    }

    fn set_colors(&self, _ctx: &RequestContext, request: SetColorsRequest) -> CommandResult {
        self.call(|tx| RemoteJob::SetColors(request, tx))
    }

    fn screenshot(
        &self,
        _ctx: &RequestContext,
        request: ScreenshotRequest,
        callback: ScreenshotCallback,
    ) -> CommandResult {
        let (tx, rx) = mpsc::channel();
        self.sender
            .send(RemoteJob::Screenshot(request, tx))
            .map_err(|_| CommandError::new("the kitty GUI event loop is not running"))?;
        let image = rx
            .recv_timeout(REPLY_TIMEOUT)
            .map_err(|_| CommandError::new("timed out waiting for a screenshot"))?
            .map_err(|error| CommandError::new(error.message))?;
        callback(image);
        Ok(kitty_remote::Value::Null)
    }
}

/// Encode a top-down RGBA8 framebuffer as PNG without external crates.
pub(crate) fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for &byte in data {
            crc ^= byte as u32;
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xedb8_8320 & mask);
            }
        }
        !crc
    }
    fn chunk(kind: &[u8; 4], data: &[u8], out: &mut Vec<u8>) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let crc = crc32(&out[start..]);
        out.extend_from_slice(&crc.to_be_bytes());
    }

    let mut raw = Vec::with_capacity(height as usize * (1 + width as usize * 3));
    for y in 0..height as usize {
        raw.push(0);
        for x in 0..width as usize {
            let offset = (y * width as usize + x) * 4;
            raw.extend_from_slice(&rgba[offset..offset + 3]);
        }
    }

    // zlib stream with stored (uncompressed) deflate blocks.
    let mut zlib = vec![0x78, 0x01];
    let mut position = 0usize;
    while position < raw.len() {
        let remaining = raw.len() - position;
        let count = remaining.min(65_535);
        let final_block = position + count >= raw.len();
        zlib.push(u8::from(final_block));
        zlib.extend_from_slice(&(count as u16).to_le_bytes());
        zlib.extend_from_slice(&(!(count as u16)).to_le_bytes());
        zlib.extend_from_slice(&raw[position..position + count]);
        position += count;
    }
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in &raw {
        a = (a + byte as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    zlib.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut out = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &header, &mut out);
    chunk(b"IDAT", &zlib, &mut out);
    chunk(b"IEND", &[], &mut out);
    out
}

/// Encode a top-down RGBA8 framebuffer as binary PPM (P6).
pub(crate) fn encode_ppm(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = format!("P6\n{width} {height}\n255\n").into_bytes();
    out.reserve(width as usize * height as usize * 3);
    for pixel in rgba.as_chunks::<4>().0 {
        out.extend_from_slice(&pixel[..3]);
    }
    out
}

pub(crate) fn encode_screenshot(
    bytes: Vec<u8>,
    format: ScreenshotFormat,
    width: u32,
    height: u32,
) -> ScreenshotImage {
    ScreenshotImage::new(bytes, format, width, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_and_ppm_encoders_have_valid_headers() {
        let rgba = [255u8, 0, 0, 255, 0, 255, 0, 255];
        let png = encode_png(2, 1, &rgba);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[12..16], b"IHDR");
        assert!(png.windows(4).any(|window| window == b"IDAT"));
        assert!(png.ends_with(b"IEND\xaeB`\x82"));
        let ppm = encode_ppm(2, 1, &rgba);
        assert_eq!(&ppm[..11], b"P6\n2 1\n255\n");
        assert_eq!(&ppm[11..], &[255, 0, 0, 0, 255, 0]);
    }
}
