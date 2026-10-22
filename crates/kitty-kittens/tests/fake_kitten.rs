// SPDX-License-Identifier: GPL-3.0-or-later
//! End-to-end launch of a fake `/bin/sh` kitten.
//!
//! The script records the bootstrap environment and forwarded arguments. It
//! exits immediately, so the test cannot hang.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use kitty_kittens::cli::{execute_command, CommandAction, KittenCommand};
use kitty_kittens::launch::{BootstrapEnv, LaunchOptions, StdioMode};
use kitty_kittens::{DiscoveryConfig, KittenRegistry, KittenSource};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "kitty-kittens-fake-{}-{}-{}",
            std::process::id(),
            label,
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

#[cfg(unix)]
#[test]
fn fake_shell_kitten_receives_bootstrap_environment_and_arguments() {
    let dir = TempDir::new("run");
    let script = dir.path.join("fake");
    let output = dir.path.join("out.txt");
    fs::write(
        &script,
        r#"#!/bin/sh
{
    printf 'window=%s\n' "$KITTY_WINDOW_ID"
    printf 'listen=%s\n' "$KITTY_LISTEN_ON"
    printf 'name=%s\n' "$KITTY_KITTEN_NAME"
    printf 'kind=%s\n' "$KITTY_KITTEN_KIND"
    printf 'bootstrapped=%s\n' "$KITTY_KITTEN_BOOTSTRAPPED"
    printf 'alias=%s\n' "$KITTY_KITTEN_ALIAS"
    printf 'arg1=%s\n' "$1"
    printf 'arg2=%s\n' "$2"
    printf 'cwd=%s\n' "$PWD"
} > "$OUT"
"#,
    )
    .unwrap();
    make_executable(&script);

    let registry = KittenRegistry::discover(
        &DiscoveryConfig::empty().with_root(&dir.path, KittenSource::Config),
    );
    let bootstrap = BootstrapEnv {
        window_id: Some(17),
        listen_on: Some("unix:/tmp/kitty-test.sock".to_owned()),
        public_key: None,
        pid: Some(123),
        installation_dir: None,
        config_dir: None,
        running_as_ui: false,
        extra: BTreeMap::new(),
    };
    let options = LaunchOptions {
        cwd: Some(dir.path.clone()),
        inherit_env: false,
        env: BTreeMap::from([("OUT".to_owned(), output.to_string_lossy().into_owned())]),
        stdin: StdioMode::Null,
        stdout: StdioMode::Piped,
        stderr: StdioMode::Piped,
        bootstrap,
        ..LaunchOptions::default()
    };
    let action = execute_command(
        &registry,
        KittenCommand::Run {
            name: "fake_alias".to_owned(),
            args: vec!["--flag".to_owned(), "two".to_owned()],
            explicit_separator: false,
        },
        &options,
    );
    // `fake_alias` is not an alias; use the discovered name for the launch.
    assert!(action.is_err());
    let action = execute_command(
        &registry,
        KittenCommand::Run {
            name: "fake".to_owned(),
            args: vec!["--flag".to_owned(), "two".to_owned()],
            explicit_separator: false,
        },
        &options,
    )
    .unwrap();
    let CommandAction::Run(plan) = action else {
        panic!("expected a launch plan");
    };
    let child = plan.spawn().expect("spawn fake kitten");
    let result = child.wait_with_output().expect("wait for fake kitten");
    assert!(result.status.success());
    let text = fs::read_to_string(&output).expect("read kitten output");
    assert!(text.contains("window=17"), "{text}");
    assert!(text.contains("listen=unix:/tmp/kitty-test.sock"), "{text}");
    assert!(text.contains("name=fake"), "{text}");
    assert!(text.contains("kind=executable"), "{text}");
    assert!(text.contains("bootstrapped=1"), "{text}");
    assert!(text.contains("arg1=--flag"), "{text}");
    assert!(text.contains("arg2=two"), "{text}");
    assert!(
        text.contains(&format!("cwd={}", dir.path.display())),
        "{text}"
    );
}
