// SPDX-License-Identifier: GPL-3.0-or-later
//! Shell integration handoff environment.
//!
//! Kitty starts a shell with a few environment variables (and sometimes an
//! extra argv element) so that the shell's startup files can source the
//! integration scripts. This module ports that handoff logic from
//! `kitty/shell_integration.py` for bash, zsh and fish.
//!
//! Nothing here injects code into a running shell. [`ShellIntegration::handoff`]
//! returns the adjusted argv and environment; the caller then spawns the shell
//! as usual (or, later, feeds the environment to an existing shell).
//!
//! The zsh implementation deliberately does not run `zsh` to discover a global
//! `ZDOTDIR`. If no `ZDOTDIR` is configured and the user has a new zsh install,
//! the handoff is left untouched instead of spawning a subprocess.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// A supported shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

impl Shell {
    /// Stable lowercase name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
        }
    }

    /// Detect a shell from an executable path, ignoring a leading `-` (login
    /// shell) and a trailing `.exe`.
    pub fn from_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_string_lossy().to_lowercase();
        let name = name.strip_suffix(".exe").unwrap_or(&name);
        let name = name.strip_prefix('-').unwrap_or(name);
        match name {
            "bash" => Some(Self::Bash),
            "zsh" => Some(Self::Zsh),
            "fish" => Some(Self::Fish),
            _ => None,
        }
    }

    /// Parse a shell name (`bash`, `zsh`, `fish`).
    pub fn parse(name: &str) -> Option<Self> {
        Self::from_path(Path::new(name))
    }
}

impl fmt::Display for Shell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Options for [`ShellIntegration::handoff`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellIntegrationOptions {
    /// Effective value of kitty's `shell_integration` option, space separated.
    pub ksi: String,
    /// Whether startup files may be modified. `no-rc` or `disabled` in `ksi`
    /// also disables this.
    pub allow_rc_modification: bool,
    /// Home directory used for history and zsh checks.
    pub home: Option<PathBuf>,
}

impl Default for ShellIntegrationOptions {
    fn default() -> Self {
        Self {
            ksi: "enabled".to_owned(),
            allow_rc_modification: true,
            home: std::env::var_os("HOME").map(PathBuf::from),
        }
    }
}

/// An initialised shell integration resource directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellIntegration {
    directory: PathBuf,
}

impl ShellIntegration {
    /// Use an explicit resource directory.
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    /// Discover the resource directory from the environment and conventional
    /// kitty install locations.
    pub fn discover() -> Option<Self> {
        discover_shell_integration_dir().map(Self::new)
    }

    /// Discover the first existing directory in `roots`.
    pub fn discover_from<I, P>(roots: I) -> Option<Self>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        discover_shell_integration_dir_from(roots).map(Self::new)
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Directory containing the scripts for a shell.
    pub fn resource_dir(&self, shell: Shell) -> PathBuf {
        self.directory.join(shell.name())
    }

    /// Whether a shell's resource directory exists.
    pub fn has_resources(&self, shell: Shell) -> bool {
        self.resource_dir(shell).is_dir()
    }

    /// Compute the handoff argv/environment for a shell.
    pub fn handoff(
        &self,
        shell_path: &Path,
        argv: &[String],
        env: &BTreeMap<String, String>,
        options: &ShellIntegrationOptions,
    ) -> Result<ShellHandoff> {
        let shell = Shell::from_path(shell_path)
            .ok_or_else(|| Error::UnsupportedShell(shell_path.display().to_string()))?;
        self.handoff_for_shell(shell, argv, env, options)
    }

    /// Same as [`Self::handoff`] with an already detected shell.
    pub fn handoff_for_shell(
        &self,
        shell: Shell,
        argv: &[String],
        env: &BTreeMap<String, String>,
        options: &ShellIntegrationOptions,
    ) -> Result<ShellHandoff> {
        let mut final_argv = argv.to_vec();
        let mut final_env = env.clone();
        let ksi = options.ksi.trim();
        if ksi.is_empty() || ksi.split_whitespace().any(|word| word == "disabled") {
            return Ok(ShellHandoff {
                argv: final_argv,
                env: final_env,
            });
        }
        final_env.insert("KITTY_SHELL_INTEGRATION".to_owned(), ksi.to_owned());
        let allow_rc =
            options.allow_rc_modification && !ksi.split_whitespace().any(|word| word == "no-rc");
        if !allow_rc {
            return Ok(ShellHandoff {
                argv: final_argv,
                env: final_env,
            });
        }
        match shell {
            Shell::Bash => setup_bash(&self.directory, &mut final_argv, &mut final_env, options),
            Shell::Zsh => setup_zsh(&self.directory, &mut final_env, options),
            Shell::Fish => setup_fish(&self.directory, &mut final_env),
        }
        Ok(ShellHandoff {
            argv: final_argv,
            env: final_env,
        })
    }

    /// Serialize an environment as shell commands.
    pub fn serialize_env(&self, shell: Shell, env: &BTreeMap<String, String>) -> String {
        serialize_env(shell, env)
    }
}

/// The adjusted argv and environment for a shell launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellHandoff {
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
}

/// Find the first candidate directory that exists.
pub fn discover_shell_integration_dir_from<I, P>(roots: I) -> Option<PathBuf>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    roots
        .into_iter()
        .map(|path| path.as_ref().to_path_buf())
        .find(|path| path.is_dir())
}

/// Build the conventional list of shell-integration resource directories and
/// return the first that exists.
pub fn discover_shell_integration_dir() -> Option<PathBuf> {
    let mut roots = Vec::new();
    if let Some(dir) = std::env::var_os("KITTY_SHELL_INTEGRATION_DIR") {
        roots.push(PathBuf::from(dir));
    }
    if let Some(dir) = std::env::var_os("KITTY_INSTALLATION_DIR") {
        roots.push(PathBuf::from(dir).join("shell-integration"));
    }
    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME") {
        roots.push(PathBuf::from(data_home).join("kitty/shell-integration"));
    }
    if let Some(data_dirs) = std::env::var_os("XDG_DATA_DIRS") {
        for dir in std::env::split_paths(&data_dirs) {
            roots.push(dir.join("kitty/shell-integration"));
        }
    }
    for dir in [
        "/usr/local/share/kitty/shell-integration",
        "/usr/share/kitty/shell-integration",
        "/usr/local/lib/kitty/shell-integration",
        "/usr/lib/kitty/shell-integration",
        "/opt/homebrew/share/kitty/shell-integration",
    ] {
        roots.push(PathBuf::from(dir));
    }
    discover_shell_integration_dir_from(roots)
}

fn setup_fish(directory: &Path, env: &mut BTreeMap<String, String>) {
    let directory = directory.to_string_lossy().into_owned();
    env.insert("KITTY_FISH_XDG_DATA_DIR".to_owned(), directory.clone());
    match env.get("XDG_DATA_DIRS").filter(|value| !value.is_empty()) {
        Some(value) => {
            let mut dirs: Vec<&str> = value.split(':').filter(|part| !part.is_empty()).collect();
            dirs.insert(0, &directory);
            env.insert("XDG_DATA_DIRS".to_owned(), dirs.join(":"));
        }
        None => {
            env.insert("XDG_DATA_DIRS".to_owned(), directory);
        }
    }
}

fn setup_zsh(
    directory: &Path,
    env: &mut BTreeMap<String, String>,
    options: &ShellIntegrationOptions,
) {
    let zdotdir = env.get("ZDOTDIR").cloned();
    if is_new_zsh_install(env, zdotdir.as_deref(), options.home.as_deref()) {
        return;
    }
    match &zdotdir {
        Some(value) => {
            env.insert("KITTY_ORIG_ZDOTDIR".to_owned(), value.clone());
        }
        None => {
            env.remove("KITTY_ORIG_ZDOTDIR");
        }
    }
    env.insert(
        "ZDOTDIR".to_owned(),
        directory.join("zsh").to_string_lossy().into_owned(),
    );
}

fn is_new_zsh_install(
    env: &BTreeMap<String, String>,
    zdotdir: Option<&str>,
    option_home: Option<&Path>,
) -> bool {
    let zdotdir = match zdotdir {
        Some(value) => value.to_owned(),
        None => env
            .get("HOME")
            .cloned()
            .or_else(|| option_home.map(|path| path.to_string_lossy().into_owned()))
            .unwrap_or_default(),
    };
    if zdotdir == "~" {
        return true;
    }
    if zdotdir.is_empty() {
        return true;
    }
    [".zshrc", ".zshenv", ".zprofile", ".zlogin"]
        .iter()
        .all(|name| !Path::new(&zdotdir).join(name).exists())
}

fn setup_bash(
    directory: &Path,
    argv: &mut Vec<String>,
    env: &mut BTreeMap<String, String>,
    options: &ShellIntegrationOptions,
) {
    let mut inject: Vec<&str> = vec!["1"];
    let mut posix_env = String::new();
    let mut rcfile = String::new();
    let mut remove_args = std::collections::BTreeSet::new();
    let mut expecting_multi_chars_opt = true;
    let mut expecting_option_arg = false;
    let mut interactive_opt = false;
    let mut expecting_file_arg = false;
    let mut file_arg_set = false;

    for i in 1..argv.len() {
        let arg = &argv[i];
        if expecting_file_arg {
            file_arg_set = true;
            break;
        }
        if expecting_option_arg {
            expecting_option_arg = false;
            continue;
        }
        if arg == "-" || arg == "--" {
            expecting_file_arg = true;
            continue;
        }
        if arg.len() > 1
            && arg.as_bytes()[1] != b'-'
            && (arg.starts_with('-') || arg.starts_with("+O"))
        {
            expecting_multi_chars_opt = false;
            let options_text = arg.trim_start_matches(['-', '+']);
            let options_text = if let Some((before, after)) = options_text.split_once('O') {
                if after.is_empty() {
                    expecting_option_arg = true;
                }
                before
            } else {
                options_text
            };
            if options_text.contains('c') {
                return;
            }
            if options_text.contains('s') {
                break;
            }
            if options_text.contains('i') {
                interactive_opt = true;
            }
        } else if arg.starts_with("--") && expecting_multi_chars_opt {
            match arg.as_str() {
                "--posix" => {
                    if !inject.contains(&"posix") {
                        inject.push("posix");
                    }
                    posix_env = env.get("ENV").cloned().unwrap_or_default();
                    remove_args.insert(i);
                }
                "--norc" => {
                    if !inject.contains(&"no-rc") {
                        inject.push("no-rc");
                    }
                    remove_args.insert(i);
                }
                "--noprofile" => {
                    if !inject.contains(&"no-profile") {
                        inject.push("no-profile");
                    }
                    remove_args.insert(i);
                }
                "--rcfile" | "--init-file" if i + 1 < argv.len() => {
                    expecting_option_arg = true;
                    rcfile = argv[i + 1].clone();
                    remove_args.insert(i);
                    remove_args.insert(i + 1);
                }
                _ => {}
            }
        } else {
            file_arg_set = true;
            break;
        }
    }

    if file_arg_set && !interactive_opt {
        return;
    }

    env.insert(
        "ENV".to_owned(),
        directory
            .join("bash/kitty.bash")
            .to_string_lossy()
            .into_owned(),
    );
    env.insert("KITTY_BASH_INJECT".to_owned(), inject.join(" "));
    if !posix_env.is_empty() {
        env.insert("KITTY_BASH_POSIX_ENV".to_owned(), posix_env);
    }
    if !rcfile.is_empty() {
        env.insert("KITTY_BASH_RCFILE".to_owned(), rcfile);
    }
    for index in remove_args.into_iter().rev() {
        if index < argv.len() {
            argv.remove(index);
        }
    }
    if !env.contains_key("HISTFILE") && !inject.contains(&"posix") {
        let home = options
            .home
            .clone()
            .or_else(|| env.get("HOME").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("~"));
        env.insert(
            "HISTFILE".to_owned(),
            home.join(".bash_history").to_string_lossy().into_owned(),
        );
        env.insert("KITTY_BASH_UNEXPORT_HISTFILE".to_owned(), "1".to_owned());
    }
    if !argv.is_empty() {
        argv.insert(1, "--posix".to_owned());
    }
}

/// Serialize an environment as bash/zsh/fish commands.
pub fn serialize_env(shell: Shell, env: &BTreeMap<String, String>) -> String {
    let mut lines = Vec::with_capacity(env.len());
    for (key, value) in env {
        let line = match shell {
            Shell::Bash | Shell::Zsh => format!(
                "builtin export {}{}{}",
                as_str_literal(key),
                "=",
                as_str_literal(value)
            ),
            Shell::Fish => format!(
                "set -gx {} {}",
                as_fish_str_literal(key),
                as_fish_str_literal(value)
            ),
        };
        lines.push(line);
    }
    lines.join("\n")
}

/// Serialize an environment for a shell detected from its executable path.
pub fn serialize_env_for_path(shell_path: &Path, env: &BTreeMap<String, String>) -> Result<String> {
    let shell = Shell::from_path(shell_path)
        .ok_or_else(|| Error::UnsupportedShell(shell_path.display().to_string()))?;
    Ok(serialize_env(shell, env))
}

/// Quote a string for a POSIX shell, splitting embedded single quotes.
pub fn as_str_literal(value: &str) -> String {
    let parts: Vec<String> = value.split('\'').map(|part| format!("'{part}'")).collect();
    parts.join("\"'\"")
}

/// Quote a string for fish.
pub fn as_fish_str_literal(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('\'', "\\'");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "kitty-kittens-shell-{}-{}-{}",
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

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn integration() -> ShellIntegration {
        ShellIntegration::new("/opt/kitty/shell-integration")
    }

    #[test]
    fn detects_shells_from_paths() {
        assert_eq!(Shell::from_path(Path::new("/bin/bash")), Some(Shell::Bash));
        assert_eq!(
            Shell::from_path(Path::new("/usr/bin/-zsh")),
            Some(Shell::Zsh)
        );
        assert_eq!(
            Shell::from_path(Path::new("C:/Program Files/fish.exe")),
            Some(Shell::Fish)
        );
        assert_eq!(Shell::from_path(Path::new("/bin/sh")), None);
        assert_eq!(Shell::parse("zsh"), Some(Shell::Zsh));
    }

    #[test]
    fn discovered_directory_resources_are_addressed_per_shell() {
        let dir = TempDir::new("resources");
        fs::create_dir_all(dir.path.join("bash")).unwrap();
        fs::create_dir_all(dir.path.join("fish")).unwrap();
        let discovered =
            ShellIntegration::discover_from([dir.path.join("missing"), dir.path.clone()]).unwrap();
        assert_eq!(discovered.directory(), dir.path);
        assert!(discovered.has_resources(Shell::Bash));
        assert!(!discovered.has_resources(Shell::Zsh));
        assert_eq!(discovered.resource_dir(Shell::Fish), dir.path.join("fish"));
        assert_eq!(discovered.resource_dir(Shell::Bash), dir.path.join("bash"));
    }

    #[test]
    fn disabled_integration_leaves_everything_untouched() {
        let argv = vec!["bash".to_owned()];
        let env = env(&[("A", "1")]);
        let options = ShellIntegrationOptions {
            ksi: "disabled".to_owned(),
            ..ShellIntegrationOptions::default()
        };
        let handoff = integration()
            .handoff_for_shell(Shell::Bash, &argv, &env, &options)
            .unwrap();
        assert_eq!(handoff.argv, argv);
        assert_eq!(handoff.env, env);
    }

    #[test]
    fn unsupported_shell_is_an_error() {
        let options = ShellIntegrationOptions::default();
        let err = integration()
            .handoff(
                Path::new("/bin/sh"),
                &["sh".to_owned()],
                &BTreeMap::new(),
                &options,
            )
            .unwrap_err();
        assert!(matches!(err, Error::UnsupportedShell(_)));
    }

    #[test]
    fn bash_non_interactive_shell_is_untouched() {
        let argv = vec!["bash".to_owned(), "-c".to_owned(), "echo hi".to_owned()];
        let env = env(&[]);
        let options = ShellIntegrationOptions::default();
        let handoff = integration()
            .handoff_for_shell(Shell::Bash, &argv, &env, &options)
            .unwrap();
        assert_eq!(handoff.argv, argv);
        assert_eq!(
            handoff
                .env
                .get("KITTY_SHELL_INTEGRATION")
                .map(String::as_str),
            Some("enabled")
        );
        assert!(!handoff.env.contains_key("ENV"));
    }

    #[test]
    fn bash_interactive_handoff_sets_env_and_argv() {
        let temp = TempDir::new("bash-home");
        let argv = vec!["bash".to_owned()];
        let env = env(&[("HOME", temp.path.to_str().unwrap())]);
        let options = ShellIntegrationOptions {
            home: Some(temp.path.clone()),
            ..ShellIntegrationOptions::default()
        };
        let handoff = integration()
            .handoff_for_shell(Shell::Bash, &argv, &env, &options)
            .unwrap();
        assert_eq!(handoff.argv, vec!["bash".to_owned(), "--posix".to_owned()]);
        assert_eq!(
            handoff.env.get("ENV").map(String::as_str),
            Some("/opt/kitty/shell-integration/bash/kitty.bash")
        );
        assert_eq!(
            handoff.env.get("KITTY_BASH_INJECT").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            handoff.env.get("HISTFILE").map(String::as_str),
            Some(temp.path.join(".bash_history").to_str().unwrap())
        );
        assert_eq!(
            handoff
                .env
                .get("KITTY_BASH_UNEXPORT_HISTFILE")
                .map(String::as_str),
            Some("1")
        );
    }

    #[test]
    fn bash_posix_and_rcfile_options_are_forwarded() {
        let temp = TempDir::new("bash-options");
        let argv = vec![
            "bash".to_owned(),
            "--posix".to_owned(),
            "--rcfile".to_owned(),
            "/tmp/myrc".to_owned(),
        ];
        let env = env(&[
            ("HOME", temp.path.to_str().unwrap()),
            ("ENV", "/tmp/orig-env".to_owned().as_str()),
        ]);
        let options = ShellIntegrationOptions {
            home: Some(temp.path.clone()),
            ..ShellIntegrationOptions::default()
        };
        let handoff = integration()
            .handoff_for_shell(Shell::Bash, &argv, &env, &options)
            .unwrap();
        assert_eq!(
            handoff.env.get("KITTY_BASH_INJECT").map(String::as_str),
            Some("1 posix")
        );
        assert_eq!(
            handoff.env.get("KITTY_BASH_POSIX_ENV").map(String::as_str),
            Some("/tmp/orig-env")
        );
        assert_eq!(
            handoff.env.get("KITTY_BASH_RCFILE").map(String::as_str),
            Some("/tmp/myrc")
        );
        assert!(!handoff.argv.contains(&"--rcfile".to_owned()));
        assert!(!handoff.env.contains_key("HISTFILE"));
        assert_eq!(handoff.argv, vec!["bash".to_owned(), "--posix".to_owned()]);
    }

    #[test]
    fn zsh_new_install_is_left_alone() {
        let temp = TempDir::new("zsh-new");
        let argv = vec!["zsh".to_owned()];
        let env = env(&[("HOME", temp.path.to_str().unwrap())]);
        let options = ShellIntegrationOptions {
            home: Some(temp.path.clone()),
            ..ShellIntegrationOptions::default()
        };
        let handoff = integration()
            .handoff_for_shell(Shell::Zsh, &argv, &env, &options)
            .unwrap();
        assert!(!handoff.env.contains_key("ZDOTDIR"));
        assert!(!handoff.env.contains_key("KITTY_ORIG_ZDOTDIR"));
    }

    #[test]
    fn zsh_existing_config_redirects_zdotdir() {
        let temp = TempDir::new("zsh-existing");
        fs::write(temp.path.join(".zshrc"), "").unwrap();
        let argv = vec!["zsh".to_owned()];
        let env = env(&[
            ("HOME", temp.path.to_str().unwrap()),
            ("ZDOTDIR", temp.path.to_str().unwrap()),
        ]);
        let options = ShellIntegrationOptions {
            home: Some(temp.path.clone()),
            ..ShellIntegrationOptions::default()
        };
        let handoff = integration()
            .handoff_for_shell(Shell::Zsh, &argv, &env, &options)
            .unwrap();
        assert_eq!(
            handoff.env.get("KITTY_ORIG_ZDOTDIR").map(String::as_str),
            Some(temp.path.to_str().unwrap())
        );
        assert_eq!(
            handoff.env.get("ZDOTDIR").map(String::as_str),
            Some("/opt/kitty/shell-integration/zsh")
        );
    }

    #[test]
    fn fish_handoff_prepends_data_dir() {
        let argv = vec!["fish".to_owned()];
        let env = env(&[("XDG_DATA_DIRS", "/usr/share:/opt/share")]);
        let options = ShellIntegrationOptions::default();
        let handoff = integration()
            .handoff_for_shell(Shell::Fish, &argv, &env, &options)
            .unwrap();
        assert_eq!(
            handoff
                .env
                .get("KITTY_FISH_XDG_DATA_DIR")
                .map(String::as_str),
            Some("/opt/kitty/shell-integration")
        );
        assert_eq!(
            handoff.env.get("XDG_DATA_DIRS").map(String::as_str),
            Some("/opt/kitty/shell-integration:/usr/share:/opt/share")
        );
        assert_eq!(handoff.argv, argv);
    }

    #[test]
    fn fish_handoff_handles_missing_data_dirs() {
        let handoff = integration()
            .handoff_for_shell(
                Shell::Fish,
                &["fish".to_owned()],
                &BTreeMap::new(),
                &ShellIntegrationOptions::default(),
            )
            .unwrap();
        assert_eq!(
            handoff.env.get("XDG_DATA_DIRS").map(String::as_str),
            Some("/opt/kitty/shell-integration")
        );
    }

    #[test]
    fn no_rc_disables_startup_file_modification() {
        let argv = vec!["bash".to_owned()];
        let options = ShellIntegrationOptions {
            ksi: "enabled no-rc".to_owned(),
            ..ShellIntegrationOptions::default()
        };
        let handoff = integration()
            .handoff_for_shell(Shell::Bash, &argv, &BTreeMap::new(), &options)
            .unwrap();
        assert_eq!(handoff.argv, argv);
        assert!(!handoff.env.contains_key("ENV"));
        assert_eq!(
            handoff
                .env
                .get("KITTY_SHELL_INTEGRATION")
                .map(String::as_str),
            Some("enabled no-rc")
        );
    }

    #[test]
    fn serializes_posix_and_fish_environments() {
        let env = env(&[("A", "one"), ("B", "two'three")]);
        let posix = serialize_env(Shell::Bash, &env);
        assert!(posix.contains("builtin export 'A'='one'"));
        assert!(posix.contains("builtin export 'B'='two'\"'\"'three'"));
        let fish = serialize_env(Shell::Fish, &env);
        assert!(fish.contains("set -gx 'A' 'one'"));
        assert!(fish.contains("set -gx 'B' 'two\\'three'"));
        assert!(serialize_env_for_path(Path::new("/bin/zsh"), &env).is_ok());
        assert!(serialize_env_for_path(Path::new("/bin/sh"), &env).is_err());
    }
}
