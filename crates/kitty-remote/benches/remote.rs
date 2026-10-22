// SPDX-License-Identifier: GPL-3.0-or-later
//! Socket round-trip and throughput benchmark for the remote-control layer.
//!
//! Run with `cargo bench -p kitty-remote --bench remote`. No external
//! benchmarking crate is used.

use kitty_remote::{
    Client, CommandResult, GetTextRequest, RemoteHandler, RemoteServer, RequestContext,
    ServerConfig, Value,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct BenchHandler {
    text: String,
}

impl RemoteHandler for BenchHandler {
    fn get_text(&self, _ctx: &RequestContext, _request: GetTextRequest) -> CommandResult {
        Ok(Value::String(self.text.clone()))
    }
}

fn socket_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "kitty-remote-bench-{}-{}.sock",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

fn command(name: &str, payload: Value) -> Value {
    Value::object_from([("cmd", Value::from(name)), ("payload", payload)])
}

fn main() {
    let payload_size = 64 * 1024;
    let handler = Arc::new(BenchHandler {
        text: "x".repeat(payload_size),
    });
    let path = socket_path();
    let config = ServerConfig {
        idle_timeout: Duration::from_secs(30),
        ..ServerConfig::default()
    };
    let server = RemoteServer::bind(&path, handler, config).unwrap();
    let public_key = server.key().public_key().to_vec();
    let mut handle = server.start().unwrap();

    let mut encrypted = Client::connect_encrypted(&path, &public_key).unwrap();
    let mut plain = Client::connect_plain(&path).unwrap();

    // Warm up both transports.
    for _ in 0..200 {
        encrypted
            .command(command("version", Value::object()))
            .unwrap();
        plain.command(command("version", Value::object())).unwrap();
    }

    let small_rounds = 5000usize;
    let start = Instant::now();
    for _ in 0..small_rounds {
        let response = encrypted
            .command(command("version", Value::object()))
            .unwrap();
        assert_eq!(response.get("ok").and_then(Value::as_bool), Some(true));
    }
    let elapsed = start.elapsed();
    report_round_trips("encrypted small request/response", small_rounds, elapsed);

    let start = Instant::now();
    for _ in 0..small_rounds {
        let response = plain.command(command("version", Value::object())).unwrap();
        assert_eq!(response.get("ok").and_then(Value::as_bool), Some(true));
    }
    let elapsed = start.elapsed();
    report_round_trips("plain small request/response", small_rounds, elapsed);

    let large_rounds = 500usize;
    let start = Instant::now();
    for _ in 0..large_rounds {
        let response = encrypted
            .command(command("get-text", Value::object()))
            .unwrap();
        assert_eq!(
            response.get("data").and_then(Value::as_str).map(str::len),
            Some(payload_size)
        );
    }
    let elapsed = start.elapsed();
    report_throughput(
        "encrypted get-text 64 KiB",
        large_rounds,
        payload_size,
        elapsed,
    );

    let start = Instant::now();
    for _ in 0..large_rounds {
        let response = plain.command(command("get-text", Value::object())).unwrap();
        assert_eq!(
            response.get("data").and_then(Value::as_str).map(str::len),
            Some(payload_size)
        );
    }
    let elapsed = start.elapsed();
    report_throughput("plain get-text 64 KiB", large_rounds, payload_size, elapsed);

    drop(encrypted);
    drop(plain);
    handle.shutdown();
    let _ = std::fs::remove_file(&path);
}

fn report_round_trips(label: &str, rounds: usize, elapsed: Duration) {
    let seconds = elapsed.as_secs_f64();
    println!(
        "{label}: {rounds} round-trips in {seconds:.3}s = {:.1} round-trips/s ({:.1} us/rt)",
        rounds as f64 / seconds,
        seconds * 1e6 / rounds as f64
    );
}

fn report_throughput(label: &str, rounds: usize, bytes: usize, elapsed: Duration) {
    let seconds = elapsed.as_secs_f64();
    let total = (rounds * bytes) as f64;
    println!(
        "{label}: {rounds} responses, {:.1} MiB in {seconds:.3}s = {:.1} MiB/s",
        total / (1024.0 * 1024.0),
        total / (1024.0 * 1024.0) / seconds
    );
}
