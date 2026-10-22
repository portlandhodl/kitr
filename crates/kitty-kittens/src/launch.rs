// SPDX-License-Identifier: GPL-3.0-or-later
//! Launch planning for kittens.
//!
//! This module builds the argv, environment, working directory and standard
//! I/O configuration for a kitten without starting it. A [`LaunchPlan`] can be
//! inspected by callers (and tests) or turned into a [`std::process::Command`].
//!
//! Python kittens are executed by an interpreter (`python3` by default, or the
//! manifest's `interpreter`). Native kittens and static binaries are executed
//! directly. No Python interpreter is embedded in this crate.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};

use crate::discovery::KittenEntry;
use crate::error::{Error, Result};
use crate::manifest::KittenKind;

/// How a standard stream is connected for a launched kitten.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StdioMode {
    /// Inherit the parent's stream.
    #[default]
    Inherit,
    /// Connect to `/dev/null`.
    Null,
    /// Create a pipe. The caller must consume it through [`Child`].
    Piped,
}

impl StdioMode {
    fn to_stdio(self) -> Stdio {
        match self {
            Self::Inherit => Stdio::inherit(),
            Self::Null => Stdio::null(),
            Self::Piped => Stdio::piped(),
        }
    }
}

/// How to run a Python kitten.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PythonInterpreter {
    /// Run the script with this interpreter.
    Interpreter(PathBuf),
    /// Execute the script directly, relying on its shebang line.
    Direct,
}

impl Default for PythonInterpreter {
    fn default() -> Self {
        Self::Interpreter(PathBuf::from("python3"))
    }
}

impl PythonInterpreter {
    /// The usual `python3` interpreter.
    pub fn python3() -> Self {
        Self::Interpreter(PathBuf::from("python3"))
    }
}

/// The kitty bootstrap environment shared by kitten processes.
///
/// Kitty sets `KITTY_WINDOW_ID` and `KITTY_LISTEN_ON` for children. The
/// `KITTY_KITTEN_*` variables describe the selected kitten and are an
/// extension point for Python/Go kitten runners that do not want to redo
/// discovery themselves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootstrapEnv {
    pub window_id: Option<u64>,
    /// Value for `KITTY_LISTEN_ON` (`unix:...`, `tcp:...` or `fd:N`).
    pub listen_on: Option<String>,
    pub public_key: Option<String>,
    pub pid: Option<u32>,
    pub installation_dir: Option<PathBuf>,
    pub config_dir: Option<PathBuf>,
    /// Set `KITTEN_RUNNING_AS_UI=1` for overlay/UI kittens.
    pub running_as_ui: bool,
    /// Additional variables applied last.
    pub extra: BTreeMap<String, String>,
}

impl Default for BootstrapEnv {
    fn default() -> Self {
        Self {
            window_id: None,
            listen_on: None,
            public_key: None,
            pid: Some(std::process::id()),
            installation_dir: None,
            config_dir: None,
            running_as_ui: false,
            extra: BTreeMap::new(),
        }
    }
}

impl BootstrapEnv {
    /// Read the bootstrap variables from the current process environment.
    ///
    /// The `KITTY_KITTEN_*` spelling used by launched kittens takes precedence
    /// over the plain kitty variables, and window ids accept the `id:17` form.
    pub fn from_process_env() -> Self {
        let environment = crate::env::KittenEnvironment::from_process_env();
        Self {
            window_id: environment.window_id,
            listen_on: environment.listen_on,
            public_key: environment.public_key,
            pid: std::env::var("KITTY_PID")
                .ok()
                .and_then(|value| value.parse().ok())
                .or_else(|| Some(std::process::id())),
            installation_dir: std::env::var_os("KITTY_INSTALLATION_DIR").map(PathBuf::from),
            config_dir: std::env::var_os("KITTY_CONFIG_DIRECTORY").map(PathBuf::from),
            ..Self::default()
        }
    }

    /// Apply this bootstrap environment to a map.
    pub fn apply_to(
        &self,
        entry: &KittenEntry,
        invoked_name: &str,
        env: &mut BTreeMap<OsString, OsString>,
    ) {
        if let Some(window_id) = self.window_id {
            insert(env, "KITTY_WINDOW_ID", window_id.to_string());
        }
        if let Some(listen_on) = &self.listen_on {
            insert(env, "KITTY_LISTEN_ON", listen_on.clone());
        }
        if let Some(public_key) = &self.public_key {
            insert(env, "KITTY_PUBLIC_KEY", public_key.clone());
        }
        if let Some(pid) = self.pid {
            insert(env, "KITTY_PID", pid.to_string());
        }
        if let Some(dir) = &self.installation_dir {
            insert(
                env,
                "KITTY_INSTALLATION_DIR",
                dir.to_string_lossy().into_owned(),
            );
        }
        if let Some(dir) = &self.config_dir {
            insert(
                env,
                "KITTY_CONFIG_DIRECTORY",
                dir.to_string_lossy().into_owned(),
            );
        }

        insert(env, "KITTY_KITTEN_NAME", entry.name.clone());
        insert(
            env,
            "KITTY_KITTEN_PATH",
            entry.entry_path.to_string_lossy().into_owned(),
        );
        insert(
            env,
            "KITTY_KITTEN_DIRECTORY",
            entry.directory().to_string_lossy().into_owned(),
        );
        insert(env, "KITTY_KITTEN_KIND", entry.kind.as_str());
        insert(env, "KITTY_KITTEN_BOOTSTRAPPED", "1");
        if let Some(manifest_path) = &entry.manifest_path {
            insert(
                env,
                "KITTY_KITTEN_MANIFEST",
                manifest_path.to_string_lossy().into_owned(),
            );
        }
        if invoked_name != entry.name {
            insert(env, "KITTY_KITTEN_ALIAS", invoked_name);
        }
        if let Some(run_module) = entry
            .manifest
            .as_ref()
            .and_then(|manifest| manifest.run_module.as_deref())
        {
            insert(env, "KITTY_KITTEN_RUN_MODULE", run_module);
        }
        if self.running_as_ui {
            insert(env, "KITTEN_RUNNING_AS_UI", "1");
        }
        for (key, value) in &self.extra {
            insert(env, key.clone(), value.clone());
        }
    }
}

/// Options controlling [`build_launch`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchOptions {
    /// Working directory. Defaults to the manifest `cwd` or the current
    /// directory.
    pub cwd: Option<PathBuf>,
    /// Inherit the current process environment.
    pub inherit_env: bool,
    /// Extra environment variables, applied after the bootstrap environment.
    pub env: BTreeMap<String, String>,
    pub stdin: StdioMode,
    pub stdout: StdioMode,
    pub stderr: StdioMode,
    pub bootstrap: BootstrapEnv,
    pub python: PythonInterpreter,
    /// Set `KITTY_KITTEN_PASSTHROUGH` only when an explicit `--` separator was
    /// used by the command parser. This is metadata for compatibility runners.
    pub explicit_separator: bool,
    /// Spawn without waiting. The binary uses [`LaunchPlan::spawn_detached`].
    pub detach: bool,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            cwd: None,
            inherit_env: true,
            env: BTreeMap::new(),
            stdin: StdioMode::Inherit,
            stdout: StdioMode::Inherit,
            stderr: StdioMode::Inherit,
            bootstrap: BootstrapEnv::default(),
            python: PythonInterpreter::default(),
            explicit_separator: false,
            detach: false,
        }
    }
}

impl LaunchOptions {
    /// A deterministic configuration for tests: no inherited environment.
    pub fn isolated() -> Self {
        Self {
            inherit_env: false,
            ..Self::default()
        }
    }
}

/// A fully resolved process launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchPlan {
    pub program: PathBuf,
    pub argv: Vec<OsString>,
    /// The complete environment. [`LaunchPlan::command`] always clears the
    /// inherited environment and applies this map.
    pub env: BTreeMap<OsString, OsString>,
    pub cwd: PathBuf,
    pub stdin: StdioMode,
    pub stdout: StdioMode,
    pub stderr: StdioMode,
    pub kind: KittenKind,
    pub kitten_name: String,
    pub invoked_name: String,
    /// Whether the caller should not wait for the process.
    pub detach: bool,
}

impl LaunchPlan {
    /// Build a [`Command`] from this plan.
    ///
    /// [`LaunchPlan::argv`] is the complete argv vector, including `argv[0]`.
    pub fn command(&self) -> Command {
        let program = self
            .argv
            .first()
            .map(OsString::as_os_str)
            .unwrap_or_else(|| self.program.as_os_str());
        let args = self.argv.get(1..).unwrap_or_default();
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(&self.cwd)
            .env_clear()
            .envs(&self.env)
            .stdin(self.stdin.to_stdio())
            .stdout(self.stdout.to_stdio())
            .stderr(self.stderr.to_stdio());
        command
    }

    /// Spawn the kitten.
    pub fn spawn(&self) -> io::Result<Child> {
        self.command().spawn()
    }

    /// Spawn the kitten and return immediately. The child is not reaped by
    /// this process; the caller owns the [`Child`] handle.
    pub fn spawn_detached(&self) -> io::Result<Child> {
        self.command().spawn()
    }

    /// Run the kitten and wait for it.
    pub fn status(&self) -> io::Result<ExitStatus> {
        self.command().status()
    }

    /// Look up an environment variable as UTF-8.
    pub fn env_str(&self, key: &str) -> Option<&str> {
        self.env
            .get(OsStr::new(key))
            .and_then(|value| value.to_str())
    }
}

/// Build a launch plan for `entry`.
pub fn build_launch(
    entry: &KittenEntry,
    invoked_name: &str,
    args: &[String],
    options: &LaunchOptions,
) -> Result<LaunchPlan> {
    if !entry.entry_path.is_file() {
        return Err(Error::MissingEntry(entry.entry_path.clone()));
    }
    if entry.kind.is_native() && !is_executable(&entry.entry_path) {
        return Err(Error::NotExecutable(entry.entry_path.clone()));
    }

    let mut argv: Vec<OsString> = Vec::new();
    let program = match entry.kind {
        KittenKind::Python => {
            let interpreter = entry
                .manifest
                .as_ref()
                .and_then(|manifest| manifest.interpreter.clone())
                .unwrap_or_else(|| match &options.python {
                    PythonInterpreter::Interpreter(path) => path.clone(),
                    PythonInterpreter::Direct => entry.entry_path.clone(),
                });
            let direct = matches!(options.python, PythonInterpreter::Direct)
                && entry
                    .manifest
                    .as_ref()
                    .and_then(|m| m.interpreter.as_ref())
                    .is_none();
            if direct {
                argv.push(entry.entry_path.clone().into_os_string());
            } else {
                argv.push(interpreter.clone().into_os_string());
                argv.push(entry.entry_path.clone().into_os_string());
            }
            interpreter
        }
        KittenKind::Executable | KittenKind::StaticBinary => {
            argv.push(entry.entry_path.clone().into_os_string());
            entry.entry_path.clone()
        }
    };

    for fixed in entry.fixed_args() {
        argv.push(OsString::from(fixed));
    }
    for arg in args {
        argv.push(OsString::from(arg));
    }

    let mut env: BTreeMap<OsString, OsString> = BTreeMap::new();
    if options.inherit_env {
        for (key, value) in std::env::vars_os() {
            env.insert(key, value);
        }
    }
    if let Some(manifest) = &entry.manifest {
        for (key, value) in &manifest.env {
            insert(&mut env, key.clone(), value.clone());
        }
    }
    options.bootstrap.apply_to(entry, invoked_name, &mut env);
    for (key, value) in &options.env {
        insert(&mut env, key.clone(), value.clone());
    }
    if options.explicit_separator {
        insert(&mut env, "KITTY_KITTEN_PASSTHROUGH", "1");
    }

    let cwd = resolve_cwd(entry, options)?;
    insert(&mut env, "PWD", cwd.to_string_lossy().into_owned());

    Ok(LaunchPlan {
        program,
        argv,
        env,
        cwd,
        stdin: options.stdin,
        stdout: options.stdout,
        stderr: options.stderr,
        kind: entry.kind,
        kitten_name: entry.name.clone(),
        invoked_name: invoked_name.to_owned(),
        detach: options.detach,
    })
}

fn resolve_cwd(entry: &KittenEntry, options: &LaunchOptions) -> Result<PathBuf> {
    if let Some(cwd) = &options.cwd {
        return Ok(absolute(cwd));
    }
    if let Some(cwd) = entry
        .manifest
        .as_ref()
        .and_then(|manifest| manifest.cwd.as_ref())
    {
        let path = if cwd.is_absolute() {
            cwd.clone()
        } else {
            entry.directory().join(cwd)
        };
        return Ok(absolute(&path));
    }
    std::env::current_dir().map_err(Error::from)
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

fn insert(
    env: &mut BTreeMap<OsString, OsString>,
    key: impl Into<OsString>,
    value: impl Into<String>,
) {
    env.insert(key.into(), OsString::from(value.into()));
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::KittenSource;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "kitty-kittens-launch-{}-{}-{}",
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

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    #[cfg(unix)]
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).unwrap();
    }

    fn bootstrap() -> BootstrapEnv {
        BootstrapEnv {
            window_id: Some(7),
            listen_on: Some("unix:/tmp/kitty.sock".to_owned()),
            public_key: Some("v1:abc".to_owned()),
            pid: Some(42),
            installation_dir: Some(PathBuf::from("/opt/kitty")),
            config_dir: Some(PathBuf::from("/home/user/.config/kitty")),
            running_as_ui: true,
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn python_argv_uses_interpreter_then_script() {
        let dir = TempDir::new("python-argv");
        let script = dir.path.join("demo.py");
        write(&script, "print('x')");
        let entry = KittenEntry::new("demo", &script, KittenKind::Python, KittenSource::Config);
        let plan = build_launch(
            &entry,
            "demo",
            &["--flag".to_owned(), "value".to_owned()],
            &LaunchOptions::isolated(),
        )
        .unwrap();
        assert_eq!(plan.program, PathBuf::from("python3"));
        assert_eq!(
            plan.argv,
            vec![
                OsString::from("python3"),
                script.clone().into_os_string(),
                OsString::from("--flag"),
                OsString::from("value")
            ]
        );
        assert_eq!(plan.kind, KittenKind::Python);
    }

    #[test]
    fn manifest_interpreter_and_fixed_args_are_honoured() {
        let dir = TempDir::new("manifest-args");
        let script = dir.path.join("main.py");
        write(&script, "");
        let mut entry = KittenEntry::new("demo", &script, KittenKind::Python, KittenSource::Config);
        entry.manifest = Some(crate::manifest::KittenManifest {
            interpreter: Some(PathBuf::from("/usr/bin/python3.12")),
            args: vec!["--fixed".to_owned()],
            ..crate::manifest::KittenManifest::default()
        });
        let plan = build_launch(
            &entry,
            "demo",
            &["user".to_owned()],
            &LaunchOptions::isolated(),
        )
        .unwrap();
        assert_eq!(plan.program, PathBuf::from("/usr/bin/python3.12"));
        assert_eq!(
            plan.argv,
            vec![
                OsString::from("/usr/bin/python3.12"),
                script.into_os_string(),
                OsString::from("--fixed"),
                OsString::from("user")
            ]
        );
    }

    #[test]
    fn direct_python_launch_executes_script_as_program() {
        let dir = TempDir::new("python-direct");
        let script = dir.path.join("demo.py");
        write(&script, "#!/usr/bin/env python3\n");
        let entry = KittenEntry::new("demo", &script, KittenKind::Python, KittenSource::Config);
        let options = LaunchOptions {
            python: PythonInterpreter::Direct,
            ..LaunchOptions::isolated()
        };
        let plan = build_launch(&entry, "demo", &[], &options).unwrap();
        assert_eq!(plan.program, script);
        assert_eq!(plan.argv.len(), 1);
    }

    #[test]
    fn executable_argv_is_entry_then_arguments() {
        let dir = TempDir::new("exe-argv");
        let exe = dir.path.join("native");
        write(&exe, "#!/bin/sh\n");
        #[cfg(unix)]
        make_executable(&exe);
        let entry = KittenEntry::new("native", &exe, KittenKind::Executable, KittenSource::System);
        let plan = build_launch(
            &entry,
            "native",
            &["a".to_owned(), "b".to_owned()],
            &LaunchOptions::isolated(),
        )
        .unwrap();
        assert_eq!(plan.program, exe);
        assert_eq!(
            plan.argv,
            vec![
                exe.clone().into_os_string(),
                OsString::from("a"),
                OsString::from("b")
            ]
        );
    }

    #[test]
    fn bootstrap_environment_is_complete() {
        let dir = TempDir::new("bootstrap");
        let script = dir.path.join("demo.py");
        write(&script, "");
        let entry = KittenEntry::new("demo", &script, KittenKind::Python, KittenSource::Config);
        let options = LaunchOptions {
            bootstrap: bootstrap(),
            env: BTreeMap::from([("EXTRA".to_owned(), "1".to_owned())]),
            ..LaunchOptions::isolated()
        };
        let plan = build_launch(&entry, "demo_alias", &[], &options).unwrap();
        assert_eq!(plan.env_str("KITTY_WINDOW_ID"), Some("7"));
        assert_eq!(
            plan.env_str("KITTY_LISTEN_ON"),
            Some("unix:/tmp/kitty.sock")
        );
        assert_eq!(plan.env_str("KITTY_PUBLIC_KEY"), Some("v1:abc"));
        assert_eq!(plan.env_str("KITTY_PID"), Some("42"));
        assert_eq!(plan.env_str("KITTY_KITTEN_NAME"), Some("demo"));
        assert_eq!(
            plan.env_str("KITTY_KITTEN_PATH"),
            Some(script.to_string_lossy().as_ref())
        );
        assert_eq!(plan.env_str("KITTY_KITTEN_KIND"), Some("python"));
        assert_eq!(plan.env_str("KITTY_KITTEN_ALIAS"), Some("demo_alias"));
        assert_eq!(plan.env_str("KITTEN_RUNNING_AS_UI"), Some("1"));
        assert_eq!(plan.env_str("EXTRA"), Some("1"));
        assert_eq!(
            plan.env_str("PWD"),
            Some(plan.cwd.to_string_lossy().as_ref())
        );
    }

    #[test]
    fn manifest_cwd_is_relative_to_the_kitten_directory() {
        let dir = TempDir::new("cwd");
        let script = dir.path.join("demo/main.py");
        write(&script, "");
        let mut entry = KittenEntry::new("demo", &script, KittenKind::Python, KittenSource::Config);
        entry.manifest = Some(crate::manifest::KittenManifest {
            cwd: Some(PathBuf::from("work")),
            ..crate::manifest::KittenManifest::default()
        });
        let plan = build_launch(&entry, "demo", &[], &LaunchOptions::isolated()).unwrap();
        assert_eq!(plan.cwd, dir.path.join("demo/work"));
    }

    #[test]
    fn missing_entry_and_non_executable_are_errors() {
        let dir = TempDir::new("launch-errors");
        let missing = dir.path.join("missing.py");
        let entry = KittenEntry::new(
            "missing",
            &missing,
            KittenKind::Python,
            KittenSource::Config,
        );
        assert!(matches!(
            build_launch(&entry, "missing", &[], &LaunchOptions::isolated()),
            Err(Error::MissingEntry(_))
        ));

        let script = dir.path.join("not-executable");
        write(&script, "#!/bin/sh\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o644);
            fs::set_permissions(&script, perms).unwrap();
            let entry =
                KittenEntry::new("x", &script, KittenKind::Executable, KittenSource::Config);
            assert!(matches!(
                build_launch(&entry, "x", &[], &LaunchOptions::isolated()),
                Err(Error::NotExecutable(_))
            ));
        }
    }

    #[test]
    fn explicit_separator_sets_compatibility_variable() {
        let dir = TempDir::new("separator");
        let script = dir.path.join("demo.py");
        write(&script, "");
        let entry = KittenEntry::new("demo", &script, KittenKind::Python, KittenSource::Config);
        let options = LaunchOptions {
            explicit_separator: true,
            ..LaunchOptions::isolated()
        };
        let plan = build_launch(&entry, "demo", &["--".to_owned()], &options).unwrap();
        assert_eq!(plan.env_str("KITTY_KITTEN_PASSTHROUGH"), Some("1"));
    }
}
