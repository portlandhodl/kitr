// SPDX-License-Identifier: GPL-3.0-or-later
//! Discovery and launch-planning overhead benchmark.
//!
//! Run with `cargo bench -p kitty-kittens --bench kittens`. No external
//! benchmarking crate is used.

use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kitty_kittens::discovery::{DiscoveryConfig, KittenRegistry, KittenSource};
use kitty_kittens::launch::{build_launch, LaunchOptions};

fn make_tree(root: &Path, count: usize) -> PathBuf {
    let root = root.join("kittens");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    for index in 0..count {
        let directory = root.join(format!("kitten{index}"));
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("main.py"), "print('bench')\n").unwrap();
        let executable = root.join(format!("native{index}"));
        fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&executable).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&executable, perms).unwrap();
        }
    }
    root
}

fn report(label: &str, rounds: usize, elapsed: Duration) {
    let seconds = elapsed.as_secs_f64();
    println!(
        "{label}: {rounds} iterations in {seconds:.4}s = {:.1} us/iteration",
        seconds * 1e6 / rounds as f64
    );
}

fn main() {
    let temp = std::env::temp_dir().join(format!("kitty-kittens-bench-{}", std::process::id()));
    let root = make_tree(&temp, 50);
    let config = DiscoveryConfig::empty().with_root(&root, KittenSource::Config);

    let discovery_rounds = 50;
    let start = Instant::now();
    for _ in 0..discovery_rounds {
        let registry = black_box(KittenRegistry::discover(black_box(&config)));
        black_box(registry.len());
    }
    report("discovery (100 kittens)", discovery_rounds, start.elapsed());

    let registry = KittenRegistry::discover(&config);
    let entry = registry.resolve("kitten0").unwrap();
    let options = LaunchOptions::isolated();
    let launch_rounds = 20_000;
    let start = Instant::now();
    for _ in 0..launch_rounds {
        let plan = black_box(build_launch(
            black_box(entry),
            "kitten0",
            black_box(&["--flag".to_owned(), "value".to_owned()]),
            black_box(&options),
        ))
        .unwrap();
        black_box(plan);
    }
    report("launch planning", launch_rounds, start.elapsed());

    let start = Instant::now();
    for _ in 0..1000 {
        let output = black_box(registry.manifest_json(Some("kitten0")).unwrap());
        black_box(output.len());
    }
    report("manifest JSON", 1000, start.elapsed());

    let _ = fs::remove_dir_all(&temp);
}
