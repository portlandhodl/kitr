// SPDX-License-Identifier: GPL-3.0-or-later
#![cfg(unix)]

use std::sync::atomic::{AtomicU32, Ordering};

use kitty_graphics::{
    create_shared_memory, decode_image, parse_raw, read_shared_memory, unlink_shared_memory,
    GraphicsError, ImageLimits,
};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn dev_shm_available() -> bool {
    std::path::Path::new("/dev/shm").is_dir()
}

fn name(tag: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("/kitty-graphics-test-{tag}-{}-{n}", std::process::id())
}

fn object_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new("/dev/shm").join(name.trim_start_matches('/'))
}

fn cleanup(name: &str) {
    let _ = unlink_shared_memory(name.as_bytes());
}

#[test]
fn shm_roundtrip_with_offset_and_size() {
    if !dev_shm_available() {
        return;
    }
    let name = name("window");
    let data: Vec<u8> = (0..64).collect();
    create_shared_memory(name.as_bytes(), &data).unwrap();
    let read = read_shared_memory(name.as_bytes(), 10, 20, 1024).unwrap();
    assert_eq!(read, &data[10..30]);
    cleanup(&name);
}

#[test]
fn shm_zero_size_reads_to_end() {
    if !dev_shm_available() {
        return;
    }
    let name = name("end");
    let data = vec![7u8; 32];
    create_shared_memory(name.as_bytes(), &data).unwrap();
    let read = read_shared_memory(name.as_bytes(), 8, 0, 1024).unwrap();
    assert_eq!(read, vec![7u8; 24]);
    cleanup(&name);
}

#[test]
fn shm_max_to_read_caps_the_window() {
    if !dev_shm_available() {
        return;
    }
    let name = name("cap");
    let data: Vec<u8> = (0..64).collect();
    create_shared_memory(name.as_bytes(), &data).unwrap();
    let read = read_shared_memory(name.as_bytes(), 0, 0, 9).unwrap();
    assert_eq!(read, &data[..9]);
    cleanup(&name);
}

#[test]
fn shm_missing_object_is_a_generic_file_error() {
    if !dev_shm_available() {
        return;
    }
    let name = name("missing");
    let error = read_shared_memory(name.as_bytes(), 0, 0, 1024).unwrap_err();
    assert_eq!(error.code(), "EBADF");
}

#[test]
fn shm_offset_past_end_is_rejected_and_unlinked() {
    if !dev_shm_available() {
        return;
    }
    let name = name("offset");
    create_shared_memory(name.as_bytes(), &[1, 2, 3]).unwrap();
    let error = read_shared_memory(name.as_bytes(), 4, 0, 1024).unwrap_err();
    assert_eq!(error.code(), "EBADF");
    assert!(
        !object_path(&name).exists(),
        "object must be unlinked on error"
    );
}

#[test]
fn shm_size_past_end_is_rejected_and_unlinked() {
    if !dev_shm_available() {
        return;
    }
    let name = name("size");
    create_shared_memory(name.as_bytes(), &[1, 2, 3]).unwrap();
    let error = read_shared_memory(name.as_bytes(), 0, 10, 1024).unwrap_err();
    assert_eq!(error.code(), "EBADF");
    assert!(
        !object_path(&name).exists(),
        "object must be unlinked on error"
    );
}

#[test]
fn shm_is_unlinked_after_success() {
    if !dev_shm_available() {
        return;
    }
    let name = name("unlink");
    create_shared_memory(name.as_bytes(), &[1, 2, 3, 4]).unwrap();
    assert!(object_path(&name).exists());
    let read = read_shared_memory(name.as_bytes(), 0, 0, 1024).unwrap();
    assert_eq!(read, vec![1, 2, 3, 4]);
    assert!(!object_path(&name).exists());
}

#[test]
fn shm_rejects_empty_and_nul_names() {
    assert_eq!(
        read_shared_memory(b"", 0, 0, 16).unwrap_err().code(),
        "EBADF"
    );
    assert_eq!(
        read_shared_memory(b"bad\0name", 0, 0, 16)
            .unwrap_err()
            .code(),
        "EBADF"
    );
}

#[test]
fn shm_transmission_loads_an_image() {
    if !dev_shm_available() {
        return;
    }
    let name = name("image");
    let payload: Vec<u8> = vec![
        255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
    ];
    create_shared_memory(name.as_bytes(), &payload).unwrap();
    let mut command = parse_raw(b"a=t,f=32,t=s,s=2,v=2,i=7").unwrap();
    command.payload = name.as_bytes().to_vec();
    let image = decode_image(&command, &command.payload, &ImageLimits::default()).unwrap();
    assert_eq!((image.width, image.height), (2, 2));
    assert_eq!(image.rgba.as_deref(), Some(payload.as_slice()));
    assert!(!object_path(&name).exists());
}

#[test]
fn shm_transmission_window_matches_file_semantics() {
    if !dev_shm_available() {
        return;
    }
    let name = name("image-window");
    let payload: Vec<u8> = (0..64).collect();
    create_shared_memory(name.as_bytes(), &payload).unwrap();
    let mut command = parse_raw(b"a=t,f=32,t=s,s=2,v=2,S=16,O=4,i=7").unwrap();
    command.payload = name.as_bytes().to_vec();
    let image = decode_image(&command, &command.payload, &ImageLimits::default()).unwrap();
    assert_eq!(image.raw, &payload[4..20]);
}

#[test]
fn shm_errors_do_not_expose_object_contents() {
    if !dev_shm_available() {
        return;
    }
    let name = name("secret");
    create_shared_memory(name.as_bytes(), &[9u8; 4]).unwrap();
    let mut command = parse_raw(b"a=t,f=32,t=s,s=2,v=2,i=7").unwrap();
    command.payload = name.as_bytes().to_vec();
    let error = decode_image(&command, &command.payload, &ImageLimits::default()).unwrap_err();
    assert_eq!(error.code(), "EBADF");
    assert!(matches!(
        error,
        GraphicsError::BadFile(_) | GraphicsError::Io(_)
    ));
}
