// SPDX-License-Identifier: GPL-3.0-or-later
//! SSH kitten engine, ported from `kittens/ssh` and `kitty/utils.py`.
//!
//! This module covers the parts that do not need a GUI: host configuration,
//! `copy`/`env` directives, ssh command-line parsing, connection metadata,
//! bootstrap environment serialization and the native askpass helpers
//! (prompt classification and TOTP generation). The remote bootstrap archive
//! and shared-memory hand-off are represented by [`BootstrapPlan`]; launching
//! ssh is provided by [`run_ssh_command`].

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub const SSH_USAGE: &str = "\
Usage: kitten ssh [ssh options] [user@]hostname [command ...]

Connect to a remote host with kitty shell integration. Configuration is read
from ssh.conf. The copy and env directives are available as
--kitten=copy=.../--kitten=env=... or as `copy`/`env` options.

Options:
      --copy SPEC              Copy a local file/directory to the remote host
      --glob                   Treat copy arguments as glob patterns
      --dest PATH              Remote destination for the copy
      --exclude PATTERN        Exclude files while copying directories
      --symlink-strategy preserve|resolve|keep-path
  -h, --help                   Show this help";

/// One `env` directive. The value is processed after all directives, like
/// kitty's `EnvInstruction`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvInstruction {
    pub key: String,
    pub value: String,
    pub delete_on_remote: bool,
    pub copy_from_local: bool,
    pub literal_quote: bool,
}

impl EnvInstruction {
    pub fn parse(spec: &str) -> Result<Self, SshError> {
        let (key, found, value) = match spec.split_once('=') {
            Some((key, value)) => (key.trim(), true, value.trim()),
            None => (spec.trim(), false, ""),
        };
        if key.is_empty() {
            return Err(SshError::Config(
                "The env directive must not be empty".to_owned(),
            ));
        }
        Ok(Self {
            key: key.to_owned(),
            value: if value == "_kitty_copy_env_var_" {
                String::new()
            } else {
                value.to_owned()
            },
            delete_on_remote: !found,
            copy_from_local: value == "_kitty_copy_env_var_",
            literal_quote: false,
        })
    }

    /// Serialize to a shell command using the supplied local environment
    /// lookup for `_kitty_copy_env_var_`.
    pub fn serialize(&self, get_local: &dyn Fn(&str) -> Option<String>) -> String {
        let key = quote_sh(&self.key);
        if self.delete_on_remote {
            return format!("unset {key}");
        }
        let value = if self.copy_from_local {
            match get_local(&self.key) {
                Some(value) => value,
                None => return String::new(),
            }
        } else {
            self.value.clone()
        };
        format!("export {key}={}", quote_for_sh(&value, self.literal_quote))
    }
}

/// Deduplicate environment directives by key, keeping the last occurrence.
pub fn final_env_instructions(
    get_local: &dyn Fn(&str) -> Option<String>,
    env: &[EnvInstruction],
) -> String {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut out: Vec<String> = Vec::new();
    for item in env {
        let command = item.serialize(get_local);
        if command.is_empty() {
            continue;
        }
        if let Some(index) = seen.get(&item.key) {
            out[*index] = command;
        } else {
            seen.insert(item.key.clone(), out.len());
            out.push(command);
        }
    }
    out.join("\n")
}

/// Quote a string for a POSIX shell, always using single quotes.
pub fn quote_sh(value: &str) -> String {
    if value.is_empty() {
        return "''".to_owned();
    }
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "_-./:@%+".contains(ch))
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// Quote for double-quoted shell expansion. When `literal` is false, `$`,
/// backticks and `"` are escaped from the GNU bash double-quote rules, except
/// that `$(` is escaped to prevent command execution.
pub fn quote_for_sh(value: &str, literal: bool) -> String {
    if literal {
        return quote_sh(value);
    }
    let mut out = String::from("\"");
    let chars: Vec<char> = value.chars().collect();
    for (index, ch) in chars.iter().enumerate() {
        if *ch == '\\'
            || *ch == '`'
            || *ch == '"'
            || (*ch == '$' && index + 1 < chars.len() && chars[index + 1] == '(')
        {
            out.push('\\');
        }
        out.push(*ch);
    }
    out.push('"');
    out
}

/// A copy directive. `local_path` is the expanded local source, `arcname` is
/// the path inside the bootstrap tar archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyInstruction {
    pub local_path: PathBuf,
    pub arcname: String,
    pub exclude_patterns: Vec<String>,
}

pub fn get_arcname(local: &Path, dest: &str, home: &Path) -> String {
    let mut arcname = if dest.is_empty() {
        let clean = local.to_string_lossy().to_string();
        match clean.strip_prefix(&*home.to_string_lossy()) {
            Some(rest) => rest.trim_start_matches('/').to_owned(),
            None => clean.clone(),
        }
    } else {
        dest.to_owned()
    };
    if arcname.is_empty() {
        arcname = local
            .file_name()
            .map(|x| x.to_string_lossy().into_owned())
            .unwrap_or_default();
    }
    let prefix = if arcname.starts_with('/') {
        "root"
    } else {
        "home/"
    };
    if prefix == "root" {
        format!("root{arcname}")
    } else {
        format!("home/{arcname}")
    }
}

/// Options accepted by the `copy` sub-directive.
#[derive(Clone, Debug, Default)]
pub struct CopyOptions {
    pub glob: bool,
    pub dest: String,
    pub exclude: Vec<String>,
    pub symlink_strategy: String,
}

/// Split a shell-like string into words, honoring single/double quotes and
/// backslash escapes. This is the `shlex.Split` subset used by kitty.
pub fn shlex_split(text: &str) -> Result<Vec<String>, SshError> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    let mut quote: Option<char> = None;
    let mut have = false;
    while let Some(ch) = chars.next() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some('"') if ch == '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            Some(_) => current.push(ch),
            None if ch == '\'' || ch == '"' => {
                quote = Some(ch);
                have = true;
            }
            None if ch == '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                    have = true;
                }
            }
            None if ch.is_whitespace() => {
                if have || !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                    have = false;
                }
            }
            None => {
                current.push(ch);
                have = true;
            }
        }
    }
    if quote.is_some() {
        return Err(SshError::Config("unterminated quote".to_owned()));
    }
    if have || !current.is_empty() {
        out.push(current);
    }
    Ok(out)
}

/// Parse a `copy` directive, expanding `~` and environment variables and
/// globbing when requested.
pub fn parse_copy_instruction(
    text: &str,
    glob: bool,
    dest: &str,
    exclude: &[String],
    symlink_strategy: &str,
    home: &Path,
) -> Result<Vec<CopyInstruction>, SshError> {
    let words = shlex_split(text)?;
    let mut locations = Vec::new();
    for word in words {
        let expanded = expand_env_and_home(&word, home);
        if glob {
            let matches = glob_path(&expanded)?;
            if matches.is_empty() {
                return Err(SshError::Config(format!("{word} matches no files")));
            }
            locations.extend(matches);
        } else {
            if !expanded.exists() {
                return Err(SshError::Config(format!("{word} does not exist")));
            }
            locations.push(expanded);
        }
    }
    if locations.is_empty() {
        return Err(SshError::Config("No files to copy specified".to_owned()));
    }
    if locations.len() > 1 && !dest.is_empty() {
        return Err(SshError::Config(
            "Specifying a remote location with more than one file is not supported".to_owned(),
        ));
    }
    let mut out = Vec::new();
    for location in locations {
        let mut local = location.clone();
        if symlink_strategy != "preserve" {
            local = fs::canonicalize(&location).unwrap_or(location.clone());
        }
        let arc_source = if symlink_strategy == "resolve" {
            local.clone()
        } else {
            location.clone()
        };
        out.push(CopyInstruction {
            local_path: local,
            arcname: get_arcname(&arc_source, dest, home),
            exclude_patterns: exclude.to_vec(),
        });
    }
    Ok(out)
}

/// Parse a full `copy` directive, including the leading `--glob`, `--dest`,
/// `--exclude` and `--symlink-strategy` options used in `ssh.conf`.
pub fn parse_copy_directive(text: &str, home: &Path) -> Result<Vec<CopyInstruction>, SshError> {
    let words = shlex_split(text)?;
    let mut glob = false;
    let mut dest = String::new();
    let mut excludes = Vec::new();
    let mut strategy = "preserve".to_owned();
    let mut locations = Vec::new();
    let mut index = 0usize;
    while index < words.len() {
        match words[index].as_str() {
            "--glob" => glob = true,
            "--dest" => {
                index += 1;
                dest = words
                    .get(index)
                    .cloned()
                    .ok_or_else(|| SshError::Config("--dest requires a value".to_owned()))?;
            }
            "--exclude" => {
                index += 1;
                excludes.push(
                    words
                        .get(index)
                        .cloned()
                        .ok_or_else(|| SshError::Config("--exclude requires a value".to_owned()))?,
                );
            }
            "--symlink-strategy" => {
                index += 1;
                strategy = words.get(index).cloned().ok_or_else(|| {
                    SshError::Config("--symlink-strategy requires a value".to_owned())
                })?;
            }
            "--" => {
                locations.extend(words[index + 1..].iter().cloned());
                break;
            }
            other if other.starts_with('-') => {
                return Err(SshError::Config(format!("Unknown copy option: {other}")))
            }
            other => locations.push(other.to_owned()),
        }
        index += 1;
    }
    let locations = locations
        .iter()
        .map(|location| quote_sh(location))
        .collect::<Vec<_>>()
        .join(" ");
    parse_copy_instruction(&locations, glob, &dest, &excludes, &strategy, home)
}

fn expand_env_and_home(text: &str, home: &Path) -> PathBuf {
    let mut value = text.to_owned();
    for (key, val) in std::env::vars() {
        value = value.replace(&format!("${key}"), &val);
        value = value.replace(&format!("${{{key}}}"), &val);
    }
    if value == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return home.join(rest);
    }
    PathBuf::from(value)
}

/// Match a simple glob against a path. Supports `*`, `?` and `**` and walks
/// directories to produce the matches, like doublestar's filepath glob.
pub fn glob_path(pattern: &Path) -> Result<Vec<PathBuf>, SshError> {
    let text = pattern.to_string_lossy().into_owned();
    if !text.contains(['*', '?', '[']) {
        return Ok(if pattern.exists() {
            vec![pattern.to_path_buf()]
        } else {
            Vec::new()
        });
    }
    let (mut base, tail) = split_glob_base(&text);
    if text.starts_with('/') && !base.starts_with('/') {
        base.insert(0, '/');
    }
    let base = if base.is_empty() {
        PathBuf::from(".")
    } else {
        PathBuf::from(base)
    };
    let mut relative = Vec::new();
    walk_glob(
        &base,
        &[],
        &tail
            .split('/')
            .filter(|x| !x.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>(),
        &mut relative,
    )?;
    let mut out: Vec<PathBuf> = relative.into_iter().map(|path| base.join(path)).collect();
    out.sort();
    Ok(out)
}

fn split_glob_base(text: &str) -> (String, String) {
    let mut base = String::new();
    let mut tail = String::new();
    let mut in_tail = false;
    for part in text.split('/') {
        if !in_tail && !part.contains(['*', '?', '[']) {
            if !base.is_empty() {
                base.push('/');
            }
            base.push_str(part);
        } else {
            in_tail = true;
            if !tail.is_empty() {
                tail.push('/');
            }
            tail.push_str(part);
        }
    }
    (base, tail)
}

fn walk_glob(
    dir: &Path,
    prefix: &[String],
    parts: &[String],
    out: &mut Vec<PathBuf>,
) -> Result<(), SshError> {
    if parts.is_empty() {
        out.push(prefix.iter().collect());
        return Ok(());
    }
    let part = &parts[0];
    if part == "**" {
        // zero directories
        walk_glob(dir, prefix, &parts[1..], out)?;
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    let mut next = prefix.to_vec();
                    next.push(entry.file_name().to_string_lossy().into_owned());
                    walk_glob(&entry.path(), &next, parts, out)?;
                }
            }
        }
        return Ok(());
    }
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if wildcard_match(part, &name) {
                let mut next = prefix.to_vec();
                next.push(name);
                if parts.len() == 1 {
                    out.push(next.iter().collect());
                } else {
                    walk_glob(&entry.path(), &next, &parts[1..], out)?;
                }
            }
        }
    }
    Ok(())
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    fn inner(p: &[char], t: &[char]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some('*') => inner(&p[1..], t) || (!t.is_empty() && inner(p, &t[1..])),
            Some('?') => !t.is_empty() && inner(&p[1..], &t[1..]),
            Some(ch) => t.first() == Some(ch) && inner(&p[1..], &t[1..]),
        }
    }
    inner(&p, &t)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RemoteKitty {
    #[default]
    IfNeeded,
    No,
    Yes,
}

#[derive(Clone, Debug, Default)]
pub struct SshHostConfig {
    pub hostname: String,
    pub interpreter: String,
    pub remote_dir: String,
    pub copy: Vec<String>,
    pub env: Vec<String>,
    pub shell_integration: String,
    pub login_shell: String,
    pub cwd: String,
    pub color_scheme: String,
    pub remote_kitty: RemoteKitty,
    pub share_connections: bool,
    pub askpass: String,
    pub delegate: String,
    pub forward_remote_control: bool,
    pub password: String,
    pub totp_secret: String,
    pub totp_digits: u32,
    pub totp_period: u64,
}

impl SshHostConfig {
    pub fn new() -> Self {
        Self {
            interpreter: "sh".to_owned(),
            remote_dir: ".local/share/kitty-ssh-kitten".to_owned(),
            shell_integration: "inherited".to_owned(),
            remote_kitty: RemoteKitty::IfNeeded,
            share_connections: true,
            askpass: "unless-set".to_owned(),
            totp_digits: 6,
            totp_period: 30,
            ..Self::default()
        }
    }

    /// Apply one `key value` line from ssh.conf.
    pub fn apply(&mut self, key: &str, value: &str) -> Result<(), SshError> {
        let value = value.trim();
        match key {
            "hostname" => self.hostname = value.to_owned(),
            "interpreter" => self.interpreter = value.to_owned(),
            "remote_dir" => self.remote_dir = value.to_owned(),
            "copy" => self.copy.push(value.to_owned()),
            "env" => self.env.push(value.to_owned()),
            "shell_integration" => self.shell_integration = value.to_owned(),
            "login_shell" => self.login_shell = value.to_owned(),
            "cwd" => self.cwd = value.to_owned(),
            "color_scheme" => self.color_scheme = value.to_owned(),
            "remote_kitty" => {
                self.remote_kitty = match value {
                    "no" => RemoteKitty::No,
                    "yes" => RemoteKitty::Yes,
                    _ => RemoteKitty::IfNeeded,
                }
            }
            "share_connections" => self.share_connections = truthy(value),
            "askpass" => self.askpass = value.to_owned(),
            "delegate" => self.delegate = value.to_owned(),
            "forward_remote_control" => self.forward_remote_control = truthy(value),
            "password" => self.password = resolve_secret("password", value)?,
            "totp_secret" => self.totp_secret = resolve_secret("totp_secret", value)?,
            "totp_digits" => {
                self.totp_digits = value
                    .parse()
                    .map_err(|_| SshError::Config(format!("invalid totp_digits: {value}")))?
            }
            "totp_period" => {
                self.totp_period = value
                    .parse()
                    .map_err(|_| SshError::Config(format!("invalid totp_period: {value}")))?
            }
            other => {
                return Err(SshError::Config(format!(
                    "Unknown configuration key: {other:?}"
                )))
            }
        }
        Ok(())
    }
}

fn truthy(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "yes" | "y" | "true" | "1" | "on"
    )
}

fn resolve_secret(key: &str, value: &str) -> Result<String, SshError> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(String::new());
    }
    match value.split_once(':') {
        Some((backend, secret)) if backend.trim().eq_ignore_ascii_case("text") => {
            Ok(secret.trim().to_owned())
        }
        Some((backend, _)) => Err(SshError::Config(format!(
            "Unsupported secret backend {backend} for {key}. Supported backends: text"
        ))),
        None => Err(SshError::Config(format!(
            "No secret backend specified for: {key}"
        ))),
    }
}

/// A set of host blocks. A `hostname` line starts a new block; lines before
/// the first host block are defaults for every host.
#[derive(Clone, Debug, Default)]
pub struct SshConfigSet {
    pub default: SshHostConfig,
    pub hosts: Vec<SshHostConfig>,
}

impl SshConfigSet {
    pub fn new() -> Self {
        Self {
            default: SshHostConfig::new(),
            hosts: Vec::new(),
        }
    }

    pub fn parse(text: &str) -> Result<Self, SshError> {
        let mut set = Self::new();
        let mut current: Option<SshHostConfig> = None;
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line
                .split_once(char::is_whitespace)
                .map_or((line, ""), |(a, b)| (a, b.trim()));
            if key == "hostname" {
                if let Some(config) = current.take() {
                    set.hosts.push(config);
                }
                let mut config = SshHostConfig::new();
                config.hostname = value.to_owned();
                current = Some(config);
            } else if let Some(config) = current.as_mut() {
                config.apply(key, value)?;
            } else {
                set.default.apply(key, value)?;
            }
        }
        if let Some(config) = current {
            set.hosts.push(config);
        }
        Ok(set)
    }

    pub fn for_host(&self, hostname: &str, username: &str) -> SshHostConfig {
        let target = if username.is_empty() {
            hostname.to_owned()
        } else {
            format!("{username}@{hostname}")
        };
        for config in &self.hosts {
            if config.hostname.split_whitespace().any(|pattern| {
                wildcard_match(pattern, &target) || wildcard_match(pattern, hostname)
            }) {
                return merge_host_config(&self.default, config);
            }
        }
        self.default.clone()
    }
}

fn merge_host_config(base: &SshHostConfig, host: &SshHostConfig) -> SshHostConfig {
    let defaults = SshHostConfig::new();
    let mut out = base.clone();
    macro_rules! take {
        ($field:ident) => {
            if host.$field != defaults.$field {
                out.$field = host.$field.clone();
            }
        };
    }
    take!(interpreter);
    take!(remote_dir);
    take!(shell_integration);
    take!(login_shell);
    take!(cwd);
    take!(color_scheme);
    take!(remote_kitty);
    take!(share_connections);
    take!(askpass);
    take!(delegate);
    take!(forward_remote_control);
    take!(password);
    take!(totp_secret);
    take!(totp_digits);
    take!(totp_period);
    out.hostname = host.hostname.clone();
    out.copy = base.copy.iter().chain(host.copy.iter()).cloned().collect();
    out.env = base.env.iter().chain(host.env.iter()).cloned().collect();
    out
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SshConnectionData {
    pub binary: String,
    pub hostname: String,
    pub port: Option<u16>,
    pub identity_file: String,
    pub is_ssh_kitten: bool,
    pub ssh_kitten_cmdline: Vec<String>,
    /// Process id used in the shared ControlPath. Zero means this process.
    pub control_pid: u32,
}

impl SshConnectionData {
    pub fn command_prefix(&self) -> Vec<String> {
        if self.is_ssh_kitten {
            return self.ssh_kitten_cmdline.clone();
        }
        let pid = if self.control_pid == 0 {
            std::process::id()
        } else {
            self.control_pid
        };
        let mut argv = vec![
            self.binary.clone(),
            "-o".to_owned(),
            format!("ControlPath=~/.ssh/kitty-ssh-{pid}-%C"),
            "-o".to_owned(),
            "TCPKeepAlive=yes".to_owned(),
            "-o".to_owned(),
            "ControlPersist=yes".to_owned(),
        ];
        if let Some(port) = self.port {
            argv.push("-p".to_owned());
            argv.push(port.to_string());
        }
        if !self.identity_file.is_empty() {
            argv.push("-i".to_owned());
            argv.push(self.identity_file.clone());
        }
        argv
    }

    pub fn batch_prefix(&self) -> Vec<String> {
        let mut argv = self.command_prefix();
        if !self.is_ssh_kitten {
            argv.push("-o".to_owned());
            argv.push("BatchMode=yes".to_owned());
        }
        argv
    }
}

const SSH_BOOLEAN_OPTIONS: &str = "46AaCfGgKkMNnqsTtVvXxYy";
const SSH_VALUE_OPTIONS: &str = "BbccdDEeFIiJLlmOopQRSWw";

/// Parsed ssh arguments: `(ssh_options, server_args, passthrough, extra)`.
pub type ParsedSshArgs = (Vec<String>, Vec<String>, bool, Vec<String>);

/// Split ssh arguments into options handled by ssh and the host/command. The
/// returned `passthrough` is true for options that require an untouched tty.
pub fn parse_ssh_args(args: &[String], extra_args: &[&str]) -> Result<ParsedSshArgs, SshError> {
    let mut ssh_args = Vec::new();
    let mut server_args = Vec::new();
    let mut found_extra = Vec::new();
    let mut passthrough = false;
    let mut expecting_value = false;
    let mut stop = false;
    let mut extra_value: Option<String> = None;
    for argument in args {
        if stop || server_args.len() > 1 {
            server_args.push(argument.clone());
            continue;
        }
        if argument.starts_with('-') && !expecting_value {
            if argument == "--" {
                stop = true;
                continue;
            }
            if let Some(name) = extra_args
                .iter()
                .find(|name| argument == **name || argument.starts_with(&format!("{name}=")))
            {
                if let Some((_, value)) = argument.split_once('=') {
                    found_extra.push((*name).to_owned());
                    found_extra.push(value.to_owned());
                } else {
                    extra_value = Some((*name).to_owned());
                    expecting_value = true;
                }
                continue;
            }
            let chars: Vec<char> = argument[1..].chars().collect();
            let mut index = 0usize;
            while index < chars.len() {
                let ch = chars[index];
                if "NnfGT".contains(ch) {
                    passthrough = true;
                }
                if SSH_BOOLEAN_OPTIONS.contains(ch) {
                    ssh_args.push(format!("-{ch}"));
                    index += 1;
                    continue;
                }
                if SSH_VALUE_OPTIONS.contains(ch) {
                    ssh_args.push(format!("-{ch}"));
                    let rest: String = chars[index + 1..].iter().collect();
                    if !rest.is_empty() {
                        ssh_args.push(rest);
                    } else {
                        expecting_value = true;
                    }
                    break;
                }
                return Err(SshError::Usage(format!("unknown option -- {ch}")));
            }
            continue;
        }
        if expecting_value {
            if let Some(name) = extra_value.take() {
                found_extra.push(name);
            }
            ssh_args.push(argument.clone());
            expecting_value = false;
            continue;
        }
        server_args.push(argument.clone());
    }
    Ok((ssh_args, server_args, passthrough, found_extra))
}

/// Extract the connection details from an ssh command line.
pub fn connection_data(args: &[String]) -> Option<SshConnectionData> {
    let mut found_ssh = String::new();
    let mut hostname = String::new();
    let mut port = None;
    let mut identity = String::new();
    let mut expecting_hostname = false;
    let mut expecting_port = false;
    let mut expecting_identity = false;
    for arg in args {
        if found_ssh.is_empty() {
            let base = Path::new(arg)
                .file_name()
                .map(|x| x.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if base == "ssh" || base == "ssh.exe" {
                found_ssh = arg.clone();
            }
            continue;
        }
        if expecting_hostname {
            hostname = arg.clone();
            expecting_hostname = false;
            continue;
        }
        if arg.starts_with('-') {
            if arg == "--" {
                expecting_hostname = true;
                continue;
            }
            if let Some(value) = arg.strip_prefix("-p") {
                if value.is_empty() {
                    expecting_port = true;
                } else {
                    port = value.parse().ok();
                }
                continue;
            }
            if let Some(value) = arg.strip_prefix("-i") {
                if value.is_empty() {
                    expecting_identity = true;
                } else {
                    identity = value.to_owned();
                }
                continue;
            }
            continue;
        }
        if expecting_port {
            port = arg.parse().ok();
            expecting_port = false;
            continue;
        }
        if expecting_identity {
            identity = arg.clone();
            expecting_identity = false;
            continue;
        }
        if hostname.is_empty() {
            hostname = arg.clone();
        }
    }
    if hostname.is_empty() {
        return None;
    }
    if let Some(rest) = hostname.strip_prefix("ssh://") {
        let (user, hostport) = rest
            .split_once('@')
            .map_or((String::new(), rest), |(u, h)| (u.to_owned(), h));
        let (host, url_port) = hostport
            .split_once(':')
            .map_or((hostport.to_owned(), None), |(h, p)| {
                (h.to_owned(), p.parse().ok())
            });
        hostname = if user.is_empty() {
            host
        } else {
            format!("{user}@{host}")
        };
        if port.is_none() {
            port = url_port;
        }
    }
    if !identity.is_empty() && !Path::new(&identity).is_absolute() {
        identity = expand_tilde(&identity);
    }
    Some(SshConnectionData {
        binary: found_ssh,
        hostname,
        port,
        identity_file: identity,
        is_ssh_kitten: false,
        ssh_kitten_cmdline: Vec::new(),
        control_pid: 0,
    })
}

fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join(rest)
                .to_string_lossy()
                .into_owned();
        }
    }
    path.to_owned()
}

/// Build the environment exported to the remote login shell.
pub fn serialize_ssh_env(
    config: &SshHostConfig,
    literal_env: &BTreeMap<String, String>,
    remote_home: &str,
    local_env: &dyn Fn(&str) -> Option<String>,
) -> String {
    let mut env = Vec::new();
    for (key, value) in literal_env {
        env.push(EnvInstruction {
            key: key.clone(),
            value: value.clone(),
            delete_on_remote: false,
            copy_from_local: false,
            literal_quote: true,
        });
    }
    for (key, value) in [
        ("TERM", local_env("TERM")),
        ("COLORTERM", Some("truecolor".to_owned())),
        ("KITTY_WINDOW_ID", local_env("KITTY_WINDOW_ID")),
        ("WINDOWID", local_env("WINDOWID")),
        ("KITTY_PUBLIC_KEY", local_env("KITTY_PUBLIC_KEY")),
    ] {
        if let Some(value) = value {
            if !value.is_empty() {
                env.push(EnvInstruction {
                    key: key.to_owned(),
                    value,
                    delete_on_remote: false,
                    copy_from_local: false,
                    literal_quote: true,
                });
            }
        }
    }
    for spec in &config.env {
        if let Ok(instruction) = EnvInstruction::parse(spec) {
            env.push(instruction);
        }
    }
    if !config.remote_dir.is_empty() {
        env.push(EnvInstruction {
            key: "KITTY_SSH_KITTEN_DATA_DIR".to_owned(),
            value: config.remote_dir.clone(),
            delete_on_remote: false,
            copy_from_local: false,
            literal_quote: false,
        });
    }
    if !config.login_shell.is_empty() {
        env.push(EnvInstruction {
            key: "KITTY_LOGIN_SHELL".to_owned(),
            value: config.login_shell.clone(),
            delete_on_remote: false,
            copy_from_local: false,
            literal_quote: false,
        });
    }
    if !config.cwd.is_empty() {
        env.push(EnvInstruction {
            key: "KITTY_LOGIN_CWD".to_owned(),
            value: config.cwd.clone(),
            delete_on_remote: false,
            copy_from_local: false,
            literal_quote: false,
        });
    }
    if !remote_home.is_empty() {
        env.push(EnvInstruction {
            key: "HOME".to_owned(),
            value: remote_home.to_owned(),
            delete_on_remote: false,
            copy_from_local: false,
            literal_quote: true,
        });
    }
    final_env_instructions(local_env, &env)
}

/// Timeout used when probing or re-creating a stale SSH ControlMaster.
pub const STALE_MASTER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Result of one ssh child command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SshCommandOutput {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl SshCommandOutput {
    pub fn success(&self) -> bool {
        self.status == 0
    }

    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).trim().to_owned()
    }
}

/// Abstraction over ssh process execution so ControlMaster recovery is
/// testable without a real server.
pub trait SshCommandRunner {
    fn run(
        &mut self,
        argv: &[String],
        timeout: Option<std::time::Duration>,
    ) -> io::Result<SshCommandOutput>;
}

/// The default ssh runner with an optional hard timeout.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemSshRunner;

impl SshCommandRunner for SystemSshRunner {
    fn run(
        &mut self,
        argv: &[String],
        timeout: Option<std::time::Duration>,
    ) -> io::Result<SshCommandOutput> {
        let Some(program) = argv.first() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty ssh command",
            ));
        };
        let mut command = Command::new(program);
        command
            .args(&argv[1..])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = command.spawn()?;
        if let Some(timeout) = timeout {
            let deadline = std::time::Instant::now() + timeout;
            loop {
                if child.try_wait()?.is_some() {
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let output = child.wait_with_output()?;
                    return Ok(SshCommandOutput {
                        status: 124,
                        stdout: output.stdout,
                        stderr: output.stderr,
                    });
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let output = child.wait_with_output()?;
            Ok(SshCommandOutput {
                status: output.status.code().unwrap_or(1),
                stdout: output.stdout,
                stderr: output.stderr,
            })
        } else {
            let output = child.wait_with_output()?;
            Ok(SshCommandOutput {
                status: output.status.code().unwrap_or(1),
                stdout: output.stdout,
                stderr: output.stderr,
            })
        }
    }
}

/// A resolved port-forward target parsed from `KITTY_LISTEN_ON`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForwardTarget {
    UnixPath(String),
    Tcp(String),
}

impl ForwardTarget {
    pub fn as_ssh_destination(&self) -> &str {
        match self {
            Self::UnixPath(path) | Self::Tcp(path) => path,
        }
    }
}

/// Parse `unix:/path`, a bare socket path, or `tcp:host:port`.
pub fn parse_forward_target(listen_on: &str) -> Result<ForwardTarget, SshError> {
    if listen_on.is_empty() {
        return Err(SshError::Config(
            "KITTY_LISTEN_ON is empty; cannot forward remote control".to_owned(),
        ));
    }
    if let Some(rest) = listen_on.strip_prefix("unix:") {
        if rest.starts_with('@') {
            return Err(SshError::Config(format!(
                "Cannot forward kitty remote control socket when an abstract UNIX socket ({rest}) is used, due to limitations in OpenSSH. Use either a path based one or a TCP socket"
            )));
        }
        return Ok(ForwardTarget::UnixPath(rest.to_owned()));
    }
    if let Some(rest) = listen_on.strip_prefix("tcp:") {
        if rest.is_empty() {
            return Err(SshError::Config(format!(
                "Invalid KITTY_LISTEN_ON: {listen_on:?}"
            )));
        }
        return Ok(ForwardTarget::Tcp(rest.to_owned()));
    }
    if listen_on.starts_with('@') {
        return Err(SshError::Config(format!(
            "Cannot forward kitty remote control socket when an abstract UNIX socket ({listen_on}) is used, due to limitations in OpenSSH. Use either a path based one or a TCP socket"
        )));
    }
    Ok(ForwardTarget::UnixPath(listen_on.to_owned()))
}

/// `-o ControlMaster=auto` and the matching `ControlPath`, appended to an ssh
/// command before the destination.
pub fn connection_sharing_args(pid: u32) -> Vec<String> {
    vec![
        "-o".to_owned(),
        "ControlMaster=auto".to_owned(),
        "-o".to_owned(),
        "ControlPersist=yes".to_owned(),
        "-o".to_owned(),
        format!("ControlPath=~/.ssh/kitty-ssh-{pid}-%C"),
    ]
}

fn master_base(conn: &SshConnectionData) -> Vec<String> {
    let mut argv = conn.command_prefix();
    if !argv.iter().any(|arg| arg == "ControlMaster=auto") {
        argv.insert(1, "ControlMaster=auto".to_owned());
        argv.insert(1, "-o".to_owned());
    }
    argv
}

/// `ssh -O check` for an existing ControlMaster.
pub fn control_master_check_command(conn: &SshConnectionData) -> Vec<String> {
    let mut argv = master_base(conn);
    argv.extend(["-O".to_owned(), "check".to_owned(), "--".to_owned()]);
    argv.push(conn.hostname.clone());
    argv
}

/// `ssh -O exit` for a stale ControlMaster. It only talks to the local socket,
/// so it is safe even when the network connection is dead.
pub fn control_master_exit_command(conn: &SshConnectionData) -> Vec<String> {
    let mut argv = master_base(conn);
    argv.extend(["-O".to_owned(), "exit".to_owned(), "--".to_owned()]);
    argv.push(conn.hostname.clone());
    argv
}

/// `ssh -N -f` to start a new background ControlMaster.
pub fn control_master_start_command(conn: &SshConnectionData) -> Vec<String> {
    let mut argv = master_base(conn);
    argv.extend(["-N".to_owned(), "-f".to_owned(), "--".to_owned()]);
    argv.push(conn.hostname.clone());
    argv
}

/// Request a remote forward (`-R 0:target -O forward`) and return the command.
pub fn port_forward_command(
    conn: &SshConnectionData,
    listen_on: &str,
) -> Result<Vec<String>, SshError> {
    let target = parse_forward_target(listen_on)?;
    let mut argv = master_base(conn);
    argv.extend([
        "-R".to_owned(),
        format!("0:{}", target.as_ssh_destination()),
        "-O".to_owned(),
        "forward".to_owned(),
        "--".to_owned(),
    ]);
    argv.push(conn.hostname.clone());
    Ok(argv)
}

/// Check whether the ControlMaster currently answers on its local socket.
pub fn control_master_responds(
    conn: &SshConnectionData,
    runner: &mut dyn SshCommandRunner,
) -> bool {
    runner
        .run(
            &control_master_check_command(conn),
            Some(STALE_MASTER_TIMEOUT),
        )
        .map(|output| output.success())
        .unwrap_or(false)
}

fn stop_stale_master(conn: &SshConnectionData, runner: &mut dyn SshCommandRunner) {
    let _ = runner.run(
        &control_master_exit_command(conn),
        Some(STALE_MASTER_TIMEOUT),
    );
}

fn ensure_master(
    conn: &SshConnectionData,
    runner: &mut dyn SshCommandRunner,
) -> Result<(), SshError> {
    if control_master_responds(conn, runner) {
        return Ok(());
    }
    let output = runner
        .run(&control_master_start_command(conn), None)
        .map_err(|err| SshError::Config(format!("failed to start SSH ControlMaster: {err}")))?;
    if !output.success() {
        return Err(SshError::Config(format!(
            "failed to start SSH ControlMaster with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    if !control_master_responds(conn, runner) {
        return Err(SshError::Config(
            "SSH ControlMaster not functional after being started explicitly".to_owned(),
        ));
    }
    Ok(())
}

/// Result of forwarding kitty's remote-control socket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwardedControl {
    pub listen_on: String,
    pub recovered: bool,
}

/// Forward the local kitty remote-control socket through an SSH ControlMaster.
///
/// When an existing shared connection is dead but has not noticed, the forward
/// request times out. The stale master is stopped and re-created, and the
/// forward is retried exactly once.
pub fn forward_remote_control(
    conn: &SshConnectionData,
    listen_on: &str,
    runner: &mut dyn SshCommandRunner,
) -> Result<ForwardedControl, SshError> {
    // Validate before touching the master.
    parse_forward_target(listen_on)?;
    ensure_master(conn, runner)?;
    let argv = port_forward_command(conn, listen_on)?;
    let mut recovered = false;
    let mut output = runner
        .run(&argv, Some(STALE_MASTER_TIMEOUT))
        .map_err(|err| SshError::Config(format!("failed to request SSH forward: {err}")))?;
    let timed_out = output.status == 124;
    if timed_out || !output.success() {
        if timed_out {
            stop_stale_master(conn, runner);
            ensure_master(conn, runner)?;
            recovered = true;
            output = runner
                .run(&argv, Some(STALE_MASTER_TIMEOUT))
                .map_err(|err| SshError::Config(format!("failed to retry SSH forward: {err}")))?;
        }
        if !output.success() {
            return Err(SshError::Config(format!(
                "Setup of port forward in SSH ControlMaster failed with status {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
    }
    let port = output.stdout_text();
    let resolved = if let Ok(port) = port.parse::<u16>() {
        format!("tcp:localhost:{port}")
    } else if !port.is_empty() {
        port
    } else {
        listen_on.to_owned()
    };
    Ok(ForwardedControl {
        listen_on: resolved,
        recovered,
    })
}

/// Execute ssh with the computed arguments. Returns the exit status.
pub fn run_ssh_command(mut argv: Vec<String>, env: HashMap<String, String>) -> io::Result<i32> {
    if argv.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty ssh command",
        ));
    }
    let program = argv.remove(0);
    let status = Command::new(program).args(argv).envs(env).status()?;
    Ok(status.code().unwrap_or(1))
}

/// Askpass prompt classification, ported from `kittens/ssh/askpass.go`.
pub fn is_password_prompt(message: &str) -> bool {
    let q = message.to_ascii_lowercase();
    !q.contains("passphrase") && q.contains("password")
}

pub fn is_otp_prompt(message: &str) -> bool {
    let q = message.to_ascii_lowercase();
    if q.contains("passphrase") {
        return false;
    }
    [
        "verification code",
        "one-time password",
        "one time password",
        "authenticator code",
        "authentication code",
        "two-factor",
        "2fa",
        "otp",
        "passcode",
    ]
    .iter()
    .any(|needle| q.contains(needle))
}

fn base32_decode(text: &str) -> Result<Vec<u8>, SshError> {
    let mut bits = 0u32;
    let mut nbits = 0u32;
    let mut out = Vec::new();
    for ch in text.chars() {
        let value = match ch.to_ascii_uppercase() {
            'A'..='Z' => ch.to_ascii_uppercase() as u8 - b'A',
            '2'..='7' => ch as u8 - b'2' + 26,
            '=' | ' ' | '\n' | '\r' | '\t' => continue,
            _ => return Err(SshError::Config(format!("invalid base32 character {ch:?}"))),
        };
        bits = (bits << 5) | value as u32;
        nbits += 5;
        if nbits >= 8 {
            nbits -= 8;
            out.push(((bits >> nbits) & 0xff) as u8);
        }
    }
    Ok(out)
}

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0];
    let mut message = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());
    let (chunks, _) = message.as_chunks::<64>();
    for chunk in chunks {
        let mut w = [0u32; 80];
        let (words, _) = chunk.as_chunks::<4>();
        for (i, word) in words.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5a827999),
                20..=39 => (b ^ c ^ d, 0x6ed9eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1bbcdc),
                _ => (b ^ c ^ d, 0xca62c1d6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, value) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&value.to_be_bytes());
    }
    out
}

fn hmac_sha1(key: &[u8], message: &[u8]) -> [u8; 20] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..20].copy_from_slice(&sha1(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Vec::with_capacity(64 + message.len());
    for byte in block {
        inner.push(byte ^ 0x36);
    }
    inner.extend_from_slice(message);
    let inner_hash = sha1(&inner);
    let mut outer = Vec::with_capacity(84);
    for byte in block {
        outer.push(byte ^ 0x5c);
    }
    outer.extend_from_slice(&inner_hash);
    sha1(&outer)
}

/// Generate a TOTP code using HMAC-SHA1, matching `generateTOTP`.
pub fn generate_totp(
    secret: &str,
    digits: u32,
    period: u64,
    time: SystemTime,
) -> Result<String, SshError> {
    let key = base32_decode(&secret.replace(' ', ""))?;
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let counter = seconds / period.max(1);
    let hash = hmac_sha1(&key, &counter.to_be_bytes());
    let offset = (hash[19] & 0x0f) as usize;
    let code = ((hash[offset] as u32 & 0x7f) << 24)
        | ((hash[offset + 1] as u32) << 16)
        | ((hash[offset + 2] as u32) << 8)
        | hash[offset + 3] as u32;
    let modulus = 10u32.pow(digits.min(9));
    Ok(format!(
        "{:0width$}",
        code % modulus,
        width = digits as usize
    ))
}

/// Run the askpass helper: use configured password/TOTP for matching prompts,
/// otherwise emit a `@kitty-ask` request over the controlling terminal.
pub fn askpass_response(
    message: &str,
    config: &SshHostConfig,
    now: SystemTime,
) -> Result<Option<String>, SshError> {
    if is_password_prompt(message) && !config.password.is_empty() {
        return Ok(Some(config.password.clone()));
    }
    if is_otp_prompt(message) && !config.totp_secret.is_empty() {
        return Ok(Some(generate_totp(
            &config.totp_secret,
            config.totp_digits,
            config.totp_period,
            now,
        )?));
    }
    Ok(None)
}

pub fn ask_escape_code(prompt: &str, confirm: bool) -> Vec<u8> {
    if confirm {
        format!("\x1bP@kitty-ask|confirm:{prompt}\x1b\\").into_bytes()
    } else {
        format!("\x1bP@kitty-ask|get_line:{prompt}\x1b\\").into_bytes()
    }
}

#[derive(Debug)]
pub enum SshError {
    Config(String),
    Usage(String),
    Io(io::Error),
}

impl std::fmt::Display for SshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(msg) | Self::Usage(msg) => f.write_str(msg),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for SshError {}

impl From<io::Error> for SshError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// Minimal CLI used by `kitten ssh`; options are separated from ssh arguments.
pub fn parse_ssh_cli(args: &[String]) -> Result<(SshHostConfig, Vec<String>), SshError> {
    let mut config = SshHostConfig::new();
    let mut passthrough = Vec::new();
    let mut index = 0usize;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            passthrough.extend_from_slice(&args[index + 1..]);
            break;
        }
        let (name, inline) = arg
            .split_once('=')
            .map_or((arg.as_str(), None), |(a, b)| (a, Some(b.to_owned())));
        let mut take = |option: &'static str| -> Result<String, SshError> {
            if let Some(value) = &inline {
                return Ok(value.clone());
            }
            index += 1;
            args.get(index)
                .cloned()
                .ok_or_else(|| SshError::Usage(format!("{option} requires a value")))
        };
        match name {
            "-h" | "--help" => {
                config.hostname = "__help__".to_owned();
            }
            "--copy" | "--kitten=copy" => config.copy.push(take("--copy")?),
            "--env" | "--kitten=env" => config.env.push(take("--env")?),
            "--glob" => config.apply("glob", "yes")?,
            "--dest" => config.apply("dest", &take("--dest")?)?,
            "--exclude" => {
                let pattern = take("--exclude")?;
                config.env.push(format!("EXCLUDE={pattern}"));
            }
            "--symlink-strategy" => {
                let value = take("--symlink-strategy")?;
                config.env.push(format!("SYMLINK_STRATEGY={value}"));
            }
            "--delegate" => config.delegate = take("--delegate")?,
            "--config" | "--override" | "--kitten" | "-o" | "-c" => {
                // Consume config/override values; config loading itself is
                // handled by `SshConfigSet::parse` in embedding code.
                let _ = take("--config")?;
            }
            other if other.starts_with("--kitten=") => {
                if let Some(rest) = other.strip_prefix("--kitten=") {
                    let (key, value) = rest.split_once('=').unwrap_or((rest, ""));
                    match key {
                        "copy" => config.copy.push(value.to_owned()),
                        "env" => config.env.push(value.to_owned()),
                        _ => {}
                    }
                }
            }
            other => passthrough.push(other.to_owned()),
        }
        index += 1;
    }
    Ok((config, passthrough))
}

// Bootstrap archive and askpass flow //////////////////////////////////////////

const EMBEDDED_BOOTSTRAP_SH: &str = include_str!("../resources/ssh/bootstrap.sh");
const EMBEDDED_BOOTSTRAP_PY: &str = include_str!("../resources/ssh/bootstrap.py");
const EMBEDDED_BOOTSTRAP_UTILS_SH: &str = include_str!("../resources/ssh/bootstrap-utils.sh");
const EMBEDDED_KITTY: &str = include_str!("../resources/ssh/kitty");
const EMBEDDED_KITTEN: &str = include_str!("../resources/ssh/kitten");
const EMBEDDED_TERMINFO: &[u8] = include_bytes!("../resources/ssh/terminfo/x/xterm-kitty");
const EMBEDDED_BASH: &[u8] = include_bytes!("../resources/ssh/shell-integration/bash/kitty.bash");
const EMBEDDED_ZSH_ENV: &[u8] = include_bytes!("../resources/ssh/shell-integration/zsh/.zshenv");
const EMBEDDED_ZSH_INTEGRATION: &[u8] =
    include_bytes!("../resources/ssh/shell-integration/zsh/kitty-integration");
const EMBEDDED_FISH: &[u8] = include_bytes!(
    "../resources/ssh/shell-integration/fish/vendor_conf.d/kitty-shell-integration.fish"
);

/// Bootstrap files bundled into the remote tar archive.
#[derive(Clone, Debug)]
pub struct BootstrapResources {
    pub bootstrap_sh: &'static str,
    pub bootstrap_py: &'static str,
    pub bootstrap_utils_sh: &'static str,
    pub kitty: &'static str,
    pub kitten: &'static str,
    pub terminfo: &'static [u8],
    pub shell_integration: Vec<(&'static str, &'static [u8])>,
}

impl Default for BootstrapResources {
    fn default() -> Self {
        Self {
            bootstrap_sh: EMBEDDED_BOOTSTRAP_SH,
            bootstrap_py: EMBEDDED_BOOTSTRAP_PY,
            bootstrap_utils_sh: EMBEDDED_BOOTSTRAP_UTILS_SH,
            kitty: EMBEDDED_KITTY,
            kitten: EMBEDDED_KITTEN,
            terminfo: EMBEDDED_TERMINFO,
            shell_integration: vec![
                ("bash/kitty.bash", EMBEDDED_BASH),
                ("zsh/.zshenv", EMBEDDED_ZSH_ENV),
                ("zsh/kitty-integration", EMBEDDED_ZSH_INTEGRATION),
                (
                    "fish/vendor_conf.d/kitty-shell-integration.fish",
                    EMBEDDED_FISH,
                ),
            ],
        }
    }
}

/// Options controlling how a bootstrap archive and remote command are built.
#[derive(Clone, Debug)]
pub struct BootstrapOptions {
    pub request_data: bool,
    pub echo_on: bool,
    pub script_type: String,
    pub test_script: String,
    pub remote_command: String,
    pub remote_home: String,
    pub create_shared_memory: bool,
    pub resources: BootstrapResources,
}

impl Default for BootstrapOptions {
    fn default() -> Self {
        Self {
            request_data: true,
            echo_on: false,
            script_type: "sh".to_owned(),
            test_script: String::new(),
            remote_command: String::new(),
            remote_home: std::env::var("HOME").unwrap_or_default(),
            create_shared_memory: true,
            resources: BootstrapResources::default(),
        }
    }
}

#[derive(Default)]
struct TarWriter {
    out: Vec<u8>,
}

fn octal_field(value: u64, width: usize) -> Vec<u8> {
    let text = format!("{value:0width$o}", width = width - 1);
    let mut out = text.into_bytes();
    out.truncate(width - 1);
    while out.len() < width - 1 {
        out.insert(0, b'0');
    }
    out.push(0);
    out
}

#[allow(clippy::too_many_arguments)]
fn tar_header(
    name: &str,
    mode: u32,
    uid: u32,
    gid: u32,
    size: u64,
    mtime: i64,
    typeflag: u8,
    linkname: &str,
) -> [u8; 512] {
    let mut header = [0u8; 512];
    let name = name.strip_prefix('/').unwrap_or(name);
    let name_bytes = name.as_bytes();
    let link_bytes = linkname.as_bytes();
    let (short_name, prefix) = if name_bytes.len() <= 100 {
        (name_bytes, &b""[..])
    } else if name_bytes.len() <= 255 {
        let mut split = name_bytes.len().saturating_sub(100);
        while split < name_bytes.len() && name_bytes[split] != b'/' {
            split += 1;
        }
        if split >= name_bytes.len() {
            (&name_bytes[..100], &b""[..])
        } else {
            (&name_bytes[split + 1..], &name_bytes[..split])
        }
    } else {
        (&name_bytes[..100], &b""[..])
    };
    header[..short_name.len()].copy_from_slice(short_name);
    header[100..108].copy_from_slice(&octal_field(u64::from(mode & 0o7777), 8));
    header[108..116].copy_from_slice(&octal_field(u64::from(uid), 8));
    header[116..124].copy_from_slice(&octal_field(u64::from(gid), 8));
    header[124..136].copy_from_slice(&octal_field(size, 12));
    header[136..148].copy_from_slice(&octal_field(mtime.max(0) as u64, 12));
    header[148..156].fill(b' ');
    header[156] = typeflag;
    header[157..157 + link_bytes.len().min(100)]
        .copy_from_slice(&link_bytes[..link_bytes.len().min(100)]);
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    header[345..345 + prefix.len()].copy_from_slice(prefix);
    let checksum: u64 = header.iter().map(|byte| u64::from(*byte)).sum();
    let text = format!("{checksum:06o}\0 ");
    header[148..156].copy_from_slice(text.as_bytes());
    header
}

fn pax_record(path: &str) -> Vec<u8> {
    let body = format!("path={path}\n");
    let mut length = body.len() + 3;
    loop {
        let prefix = format!("{length} ");
        let total = prefix.len() + body.len();
        if total == length {
            return format!("{prefix}{body}").into_bytes();
        }
        length = total;
    }
}

impl TarWriter {
    #[allow(clippy::too_many_arguments)]
    fn append(
        &mut self,
        name: &str,
        mode: u32,
        uid: u32,
        gid: u32,
        mtime: i64,
        typeflag: u8,
        linkname: &str,
        data: &[u8],
    ) {
        let clean_name = name.strip_prefix('/').unwrap_or(name);
        if clean_name.len() > 100 {
            let record = pax_record(clean_name);
            let header = tar_header(
                "././@PaxHeader",
                0o644,
                0,
                0,
                record.len() as u64,
                mtime,
                b'x',
                "",
            );
            self.out.extend_from_slice(&header);
            self.out.extend_from_slice(&record);
            self.pad();
        }
        let short_name = if clean_name.len() > 100 {
            &clean_name[..clean_name.len().min(100)]
        } else {
            clean_name
        };
        let header = tar_header(
            short_name,
            mode,
            uid,
            gid,
            data.len() as u64,
            mtime,
            typeflag,
            linkname,
        );
        self.out.extend_from_slice(&header);
        self.out.extend_from_slice(data);
        self.pad();
    }

    fn pad(&mut self) {
        let remainder = self.out.len() % 512;
        if remainder != 0 {
            self.out.resize(self.out.len() + (512 - remainder), 0);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        self.out.resize(self.out.len() + 1024, 0);
        self.out
    }
}

fn gzip(data: &[u8]) -> io::Result<Vec<u8>> {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(data)?;
    encoder.finish()
}

fn should_exclude(patterns: &[String], path: &Path) -> bool {
    let text = path.to_string_lossy();
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    for pattern in patterns {
        if pattern.is_empty() {
            continue;
        }
        let target = if pattern.contains('/') {
            text.as_ref()
        } else {
            name.as_ref()
        };
        if wildcard_match(pattern, target) || wildcard_match(pattern, text.as_ref()) {
            return true;
        }
    }
    false
}

struct TarContext {
    seen: HashMap<(u64, u64), String>,
    exclude: Vec<String>,
}

fn add_tar_path(
    writer: &mut TarWriter,
    context: &mut TarContext,
    local: &Path,
    arcname: &str,
) -> Result<(), SshError> {
    let metadata = fs::symlink_metadata(local)?;
    let mode = metadata.permissions().mode() & 0o777;
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let uid = metadata.uid();
    let gid = metadata.gid();
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(local)?;
        writer.append(
            arcname,
            mode | 0o600,
            uid,
            gid,
            mtime,
            b'2',
            &target.to_string_lossy(),
            &[],
        );
        return Ok(());
    }
    if metadata.is_dir() {
        writer.append(
            &format!("{}/", arcname.trim_end_matches('/')),
            mode | 0o600,
            uid,
            gid,
            mtime,
            b'5',
            "",
            &[],
        );
        let mut entries: Vec<_> = fs::read_dir(local)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            let child_local = entry.path();
            if should_exclude(&context.exclude, &child_local) {
                continue;
            }
            let child_arc = format!("{}/{}", arcname.trim_end_matches('/'), name);
            add_tar_path(writer, context, &child_local, &child_arc)?;
        }
        return Ok(());
    }
    if metadata.is_file() {
        let key = (metadata.dev(), metadata.ino());
        if let Some(first) = context.seen.get(&key) {
            writer.append(arcname, mode | 0o600, uid, gid, mtime, b'1', first, &[]);
            return Ok(());
        }
        context.seen.insert(key, arcname.to_owned());
        let data = fs::read(local)?;
        writer.append(arcname, mode | 0o600, uid, gid, mtime, b'0', "", &data);
    }
    // Sockets, FIFOs and device nodes cannot be represented safely in the
    // bootstrap archive and are skipped, matching the Go kitten.
    Ok(())
}

/// Build the gzip-compressed tar archive uploaded to the remote host.
pub fn make_bootstrap_tar(
    config: &SshHostConfig,
    copies: &[CopyInstruction],
    env_script: &str,
    resources: &BootstrapResources,
) -> Result<Vec<u8>, SshError> {
    let mut writer = TarWriter::default();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    writer.append("data.sh", 0o644, 0, 0, now, b'0', "", env_script.as_bytes());
    writer.append(
        "bootstrap-utils.sh",
        0o644,
        0,
        0,
        now,
        b'0',
        "",
        resources.bootstrap_utils_sh.as_bytes(),
    );
    writer.append(
        "bootstrap.py",
        0o644,
        0,
        0,
        now,
        b'0',
        "",
        resources.bootstrap_py.as_bytes(),
    );
    let remote_dir = config.remote_dir.trim_matches('/');
    if !remote_dir.is_empty() {
        for (relative, data) in &resources.shell_integration {
            let arcname = format!("home/{remote_dir}/shell-integration/{relative}");
            writer.append(&arcname, 0o644, 0, 0, now, b'0', "", data);
        }
        let version = format!("kitty-kittens {}\n", env!("CARGO_PKG_VERSION"));
        writer.append(
            &format!("home/{remote_dir}/kitty/version"),
            0o644,
            0,
            0,
            now,
            b'0',
            "",
            version.as_bytes(),
        );
        writer.append(
            &format!("home/{remote_dir}/kitty/bin/kitty"),
            0o755,
            0,
            0,
            now,
            b'0',
            "",
            resources.kitty.as_bytes(),
        );
        writer.append(
            &format!("home/{remote_dir}/kitty/bin/kitten"),
            0o755,
            0,
            0,
            now,
            b'0',
            "",
            resources.kitten.as_bytes(),
        );
    }
    writer.append(
        "home/.terminfo/x/xterm-kitty",
        0o644,
        0,
        0,
        now,
        b'0',
        "",
        resources.terminfo,
    );
    let mut context = TarContext {
        seen: HashMap::new(),
        exclude: Vec::new(),
    };
    for copy in copies {
        context.exclude = copy
            .exclude_patterns
            .iter()
            .cloned()
            .chain([
                "**/__pycache__".to_owned(),
                "**/__pycache__/**".to_owned(),
                "**/.DS_Store".to_owned(),
            ])
            .collect();
        add_tar_path(&mut writer, &mut context, &copy.local_path, &copy.arcname)?;
    }
    Ok(gzip(&writer.finish())?)
}

/// Standard kitty config locations for `ssh.conf`, highest priority first.
pub fn ssh_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(dir) = std::env::var_os("KITTY_CONFIG_DIRECTORY") {
        paths.push(PathBuf::from(dir).join("ssh.conf"));
        return paths;
    }
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        paths.push(PathBuf::from(dir).join("kitty/ssh.conf"));
    } else if let Some(home) = std::env::var_os("HOME") {
        paths.push(PathBuf::from(home).join(".config/kitty/ssh.conf"));
    }
    let dirs = std::env::var("XDG_CONFIG_DIRS").unwrap_or_else(|_| "/etc/xdg".to_owned());
    for dir in dirs.split(':').filter(|dir| !dir.is_empty()) {
        paths.push(PathBuf::from(dir).join("kitty/ssh.conf"));
    }
    paths
}

/// Load and merge `ssh.conf` from the standard locations.
pub fn load_ssh_config() -> SshConfigSet {
    let mut set = SshConfigSet::new();
    for path in ssh_config_paths() {
        if let Ok(text) = fs::read_to_string(path) {
            if let Ok(parsed) = SshConfigSet::parse(&text) {
                if !parsed.hosts.is_empty() {
                    set.hosts.extend(parsed.hosts);
                }
                let defaults = parsed.default;
                if !defaults.hostname.is_empty() {
                    set.default.hostname = defaults.hostname;
                }
                set.default.copy.extend(defaults.copy);
                set.default.env.extend(defaults.env);
            }
        }
    }
    set
}

fn replace_template_idents(script: &str, replacements: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(script.len());
    let bytes = script.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        let start = index;
        while index < bytes.len() && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
        {
            index += 1;
        }
        if start == index {
            out.push(bytes[index] as char);
            index += 1;
            continue;
        }
        let word = &script[start..index];
        if let Some(value) = replacements.get(word) {
            out.push_str(value);
        } else {
            out.push_str(word);
        }
    }
    out
}

/// Substitute the bootstrap placeholders, respecting identifier boundaries.
pub fn prepare_bootstrap_script(script: &str, replacements: &HashMap<String, String>) -> String {
    replace_template_idents(script, replacements)
}

/// Build the command line executed by the remote login shell.
pub fn wrap_bootstrap_command(config: &SshHostConfig, script: &str) -> Vec<String> {
    let interpreter = &config.interpreter;
    let is_python = Path::new(interpreter)
        .file_name()
        .map(|name| {
            name.to_string_lossy()
                .to_ascii_lowercase()
                .contains("python")
        })
        .unwrap_or(false);
    if is_python {
        let encoded = base64_encode(script.as_bytes());
        vec![
            "exec".to_owned(),
            interpreter.clone(),
            "-c".to_owned(),
            "\"import base64, sys; eval(compile(base64.standard_b64decode(sys.argv[-1]), 'bootstrap.py', 'exec'))\"".to_owned(),
            encoded,
        ]
    } else {
        let encoded = format!(
            "'{}'",
            script
                .replace('\'', "\u{b}")
                .replace('\\', "\u{c}")
                .replace('\n', "\r")
                .replace('!', "\u{8}")
        );
        vec![
            "exec".to_owned(),
            interpreter.clone(),
            "-c".to_owned(),
            "'eval \"$(echo \"$0\" | tr \\v\\f\\r\\b \\047\\134\\n\\041)\"' ".to_owned(),
            encoded,
        ]
    }
}

fn base64_encode(data: &[u8]) -> String {
    String::from_utf8(kitty_remote::base64::encode(data, true)).expect("base64 is ASCII")
}

fn random_token_hex(bytes: usize) -> String {
    let mut data = vec![0u8; bytes];
    use std::io::Read;
    if let Ok(mut file) = fs::File::open("/dev/urandom") {
        if file.read_exact(&mut data).is_ok() {
            return data.iter().map(|byte| format!("{byte:02x}")).collect();
        }
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{nanos:032x}")
}

fn split_destination(hostname: &str) -> (String, String) {
    if let Some(rest) = hostname.strip_prefix("ssh://") {
        let (user, host) = match rest.split_once('@') {
            Some((user, host)) => (user.to_owned(), host.to_owned()),
            None => (String::new(), rest.to_owned()),
        };
        let host = host.split(':').next().unwrap_or(&host).to_owned();
        return (user, host);
    }
    hostname
        .split_once('@')
        .map_or((String::new(), hostname.to_owned()), |(user, host)| {
            (user.to_owned(), host.to_owned())
        })
}

/// Create the shared-memory object holding the bootstrap JSON consumed by the
/// terminal's ssh handler. The returned name is what is sent in the DCS.
pub fn create_bootstrap_data_shm(
    tarfile: &[u8],
    password: &str,
    hostname: &str,
    username: &str,
) -> Result<(String, crate::shm::SharedMemory), SshError> {
    let data = kitty_remote::json::Value::object_from([
        (
            "tarfile",
            kitty_remote::json::Value::from(base64_encode(tarfile).as_str()),
        ),
        ("pw", kitty_remote::json::Value::from(password)),
        ("hostname", kitty_remote::json::Value::from(hostname)),
        ("username", kitty_remote::json::Value::from(username)),
    ])
    .to_json()
    .map_err(|err| SshError::Config(err.to_string()))?;
    let mut shm = crate::shm::SharedMemory::create_temp("kssh-", data.len() + 8)?;
    shm.write_all(data.as_bytes())?;
    shm.flush()?;
    Ok((shm.name().to_owned(), shm))
}

/// The DCS request that asks the terminal to send the bootstrap data to the
/// remote child process.
pub fn encode_ssh_data_request(request_id: &str, shm_name: &str, password: &str) -> String {
    format!("\x1bP@kitty-ssh|id={request_id}:pwfile={shm_name}:pw={password}\x1b\\")
}

#[derive(Clone, Debug, Default)]
pub struct BootstrapPlan {
    pub remote_command: Vec<String>,
    pub env_script: String,
    pub copy: Vec<CopyInstruction>,
    pub request_id: String,
    pub parse_script: String,
    pub tar: Vec<u8>,
    pub bootstrap_script: String,
    pub shared_memory_name: Option<String>,
    #[allow(dead_code)]
    pub shared_memory: Option<std::sync::Arc<crate::shm::SharedMemory>>,
    pub data_password: String,
    pub request_data: bool,
    pub script_type: String,
}

/// Build a complete bootstrap plan. This includes the tar archive, shared
/// memory object and the wrapped remote command, so it can be executed without
/// a second round of host-side setup.
pub fn build_bootstrap_plan(
    config: &SshHostConfig,
    copy: Vec<CopyInstruction>,
    env_script: String,
    remote_args: &[String],
) -> BootstrapPlan {
    build_bootstrap_plan_with_options(
        config,
        copy,
        env_script,
        remote_args,
        &BootstrapOptions::default(),
    )
}

pub fn build_bootstrap_plan_with_options(
    config: &SshHostConfig,
    copy: Vec<CopyInstruction>,
    env_script: String,
    remote_args: &[String],
    options: &BootstrapOptions,
) -> BootstrapPlan {
    let request_id = std::env::var("KITTY_PID").unwrap_or_else(|_| std::process::id().to_string());
    let window_id = std::env::var("KITTY_WINDOW_ID").unwrap_or_default();
    let request_id = if window_id.is_empty() {
        request_id
    } else {
        format!("{request_id}-{window_id}")
    };
    let (username, hostname) = split_destination(&config.hostname);
    let tar =
        make_bootstrap_tar(config, &copy, &env_script, &options.resources).unwrap_or_default();
    let data_password = random_token_hex(32);
    let mut shared_memory_name = None;
    let mut shared_memory = None;
    if options.create_shared_memory && !tar.is_empty() {
        if let Ok((name, shm)) =
            create_bootstrap_data_shm(&tar, &data_password, &hostname, &username)
        {
            shared_memory_name = Some(name);
            shared_memory = Some(std::sync::Arc::new(shm));
        }
    }
    let is_python = options.script_type == "py";
    let exec_cmd = if remote_args.is_empty() {
        String::new()
    } else if is_python {
        base64_encode(remote_args.join(" ").as_bytes())
    } else {
        let joined = remote_args
            .iter()
            .map(|arg| arg.replace('\'', "'\"'\"'"))
            .collect::<Vec<_>>()
            .join(" ");
        format!("unset KITTY_SHELL_INTEGRATION; exec \"$login_shell\" -c '{joined}'")
    };
    let remote_cmd = if options.remote_command.is_empty() {
        String::new()
    } else if is_python {
        base64_encode(options.remote_command.as_bytes())
    } else {
        format!("{}\n\nexit", options.remote_command)
    };
    let export_home = if options.remote_home.is_empty() {
        String::new()
    } else if is_python {
        base64_encode(options.remote_home.as_bytes())
    } else {
        format!(
            "export HOME={}; cd \"$HOME\"",
            quote_sh(&options.remote_home)
        )
    };
    let mut replacements: HashMap<String, String> = HashMap::from([
        ("EXPORT_HOME_CMD".to_owned(), export_home),
        ("EXEC_CMD".to_owned(), exec_cmd),
        ("REMOTE_CMD".to_owned(), remote_cmd),
        ("TEST_SCRIPT".to_owned(), options.test_script.clone()),
        ("REQUEST_ID".to_owned(), request_id.clone()),
        (
            "PASSWORD_FILENAME".to_owned(),
            shared_memory_name.clone().unwrap_or_default(),
        ),
        ("DATA_PASSWORD".to_owned(), data_password.clone()),
        (
            "REQUEST_DATA".to_owned(),
            if options.request_data { "1" } else { "0" }.to_owned(),
        ),
        (
            "ECHO_ON".to_owned(),
            if options.echo_on { "1" } else { "0" }.to_owned(),
        ),
    ]);
    replacements
        .entry("HAS_REMOTE_CMD".to_owned())
        .or_insert_with(|| {
            if options.remote_command.is_empty() {
                "n".to_owned()
            } else {
                "y".to_owned()
            }
        });
    let template = if is_python {
        options.resources.bootstrap_py
    } else {
        options.resources.bootstrap_sh
    };
    let bootstrap_script = prepare_bootstrap_script(template, &replacements);
    let remote_command = wrap_bootstrap_command(config, &bootstrap_script);
    BootstrapPlan {
        remote_command,
        env_script,
        copy,
        request_id,
        parse_script: String::new(),
        tar,
        bootstrap_script,
        shared_memory_name,
        shared_memory,
        data_password,
        request_data: options.request_data,
        script_type: options.script_type.clone(),
    }
}

/// A complete host-side ssh launch, including the bootstrap archive and the
/// DCS that asks the terminal to hand the archive to the remote child.
#[derive(Clone, Debug)]
pub struct SshSessionPlan {
    pub ssh_command: Vec<String>,
    pub environment: HashMap<String, String>,
    pub bootstrap: BootstrapPlan,
    pub data_request: String,
}

/// Build an ssh session plan without executing it. `ssh_args` are the options
/// intended for ssh itself and `server_args` start with the destination.
pub fn prepare_ssh_session(
    config: &SshHostConfig,
    ssh_args: &[String],
    server_args: &[String],
    literal_env: &BTreeMap<String, String>,
    options: &BootstrapOptions,
    get_local: &dyn Fn(&str) -> Option<String>,
) -> Result<SshSessionPlan, SshError> {
    let Some(hostname) = server_args.first() else {
        return Err(SshError::Usage(
            "Must specify a destination host for ssh".to_owned(),
        ));
    };
    let home = get_local("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let mut copies = Vec::new();
    for spec in &config.copy {
        copies.extend(parse_copy_directive(spec, &home)?);
    }
    let env_script = serialize_ssh_env(config, literal_env, &options.remote_home, get_local);
    let remote_args = &server_args[1..];
    let bootstrap =
        build_bootstrap_plan_with_options(config, copies, env_script, remote_args, options);
    let mut ssh_command = vec![
        "ssh".to_owned(),
        "-o".to_owned(),
        "RemoteCommand=none".to_owned(),
    ];
    ssh_command.extend_from_slice(ssh_args);
    if remote_args.is_empty() {
        ssh_command.push("-t".to_owned());
    }
    ssh_command.push("--".to_owned());
    ssh_command.push(hostname.clone());
    ssh_command.extend(bootstrap.remote_command.iter().cloned());
    let mut environment: HashMap<String, String> = HashMap::new();
    let use_kitty_askpass = match config.askpass.as_str() {
        "native" => true,
        "unless-set" => get_local("SSH_ASKPASS").is_none(),
        _ => false,
    };
    if use_kitty_askpass {
        if let Ok(exe) = std::env::current_exe() {
            environment.insert("SSH_ASKPASS".to_owned(), exe.to_string_lossy().into_owned());
            environment.insert(
                "KITTY_KITTEN_RUN_MODULE".to_owned(),
                "ssh_askpass".to_owned(),
            );
            let (username, host) = split_destination(hostname);
            environment.insert("KITTY_SSH_ASKPASS_HOST".to_owned(), host);
            environment.insert("KITTY_SSH_ASKPASS_USER".to_owned(), username);
            environment.insert("KITTY_SSH_ASKPASS_OVERRIDES".to_owned(), "[]".to_owned());
            environment.insert("SSH_ASKPASS_REQUIRE".to_owned(), "force".to_owned());
        }
    }
    let data_request = encode_ssh_data_request(
        &bootstrap.request_id,
        bootstrap.shared_memory_name.as_deref().unwrap_or(""),
        &bootstrap.data_password,
    );
    Ok(SshSessionPlan {
        ssh_command,
        environment,
        bootstrap,
        data_request,
    })
}

// Askpass ////////////////////////////////////////////////////////////////////

/// A question asked through the terminal's `@kitty-ask` protocol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskpassRequest {
    pub message: String,
    pub kind: String,
    pub is_password: bool,
}

impl AskpassRequest {
    pub fn from_message(message: &str, confirm: bool) -> Self {
        let fingerprint = message.contains("(yes/no/[fingerprint])");
        Self {
            message: message.to_owned(),
            kind: if confirm { "confirm" } else { "get_line" }.to_owned(),
            is_password: !fingerprint,
        }
    }

    pub fn to_json(&self) -> String {
        kitty_remote::json::Value::object_from([
            (
                "message",
                kitty_remote::json::Value::from(self.message.as_str()),
            ),
            ("type", kitty_remote::json::Value::from(self.kind.as_str())),
            (
                "is_password",
                kitty_remote::json::Value::from(self.is_password),
            ),
        ])
        .to_json()
        .unwrap_or_else(|_| "{}".to_owned())
    }
}

/// Where askpass sends the DCS and waits for the terminal response.
pub trait AskpassHost {
    fn trigger(&mut self, shm_name: &str) -> io::Result<()>;
    /// Wait until the first byte of shared memory becomes 1.
    fn wait_for_response(&mut self, shm: &crate::shm::SharedMemory) -> io::Result<()>;
}

fn parse_json_prefix(data: &[u8]) -> Option<kitty_remote::json::Value> {
    // The terminal writes the answer over the question. A shorter answer can
    // leave old bytes after the JSON value, so accept the longest valid prefix.
    let end = data
        .iter()
        .rposition(|byte| *byte != 0 && !byte.is_ascii_whitespace())
        .map(|index| index + 1)?;
    for candidate in (1..=end).rev() {
        if let Ok(value) = kitty_remote::json::parse(&data[..candidate]) {
            return Some(value);
        }
    }
    None
}

/// Ask the terminal a question using shared memory, returning the text that
/// OpenSSH should print. `confirm` questions return `yes`/`no`.
pub fn perform_askpass(
    request: &AskpassRequest,
    host: &mut dyn AskpassHost,
) -> Result<String, SshError> {
    let json = request.to_json();
    let mut shm = crate::shm::SharedMemory::create_temp("askpass-", json.len() + 32)?;
    shm.as_mut_slice()[0] = 0;
    shm.write_at(1, json.as_bytes())?;
    shm.flush()?;
    host.trigger(shm.name())?;
    host.wait_for_response(&shm)?;
    let payload = shm.read_at(1, shm.len() - 1)?;
    let end = payload
        .iter()
        .rposition(|byte| *byte != 0)
        .map(|index| index + 1)
        .unwrap_or(0);
    let value = parse_json_prefix(&payload[..end])
        .ok_or_else(|| SshError::Config("Failed to parse askpass response".to_owned()))?;
    if request.kind == "confirm" {
        let yes = value.as_bool().unwrap_or(false);
        return Ok(if yes { "yes" } else { "no" }.to_owned());
    }
    let mut response = value.as_str().unwrap_or("").to_owned();
    if request.message.contains("(yes/no/[fingerprint])") {
        match response.to_ascii_lowercase().as_str() {
            "y" => response = "yes".to_owned(),
            "n" => response = "no".to_owned(),
            _ => {}
        }
    }
    Ok(response)
}

/// The default askpass transport: write to and poll the controlling terminal.
pub struct TtyAskpassHost {
    tty: io::BufReader<fs::File>,
    writer: fs::File,
    timeout: std::time::Duration,
}

impl TtyAskpassHost {
    pub fn open() -> io::Result<Self> {
        let tty = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")?;
        let writer = tty.try_clone()?;
        Ok(Self {
            tty: io::BufReader::new(tty),
            writer,
            timeout: std::time::Duration::from_secs(300),
        })
    }

    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl AskpassHost for TtyAskpassHost {
    fn trigger(&mut self, shm_name: &str) -> io::Result<()> {
        self.writer
            .write_all(format!("\x1bP@kitty-ask|{shm_name}\x1b\\").as_bytes())?;
        self.writer.flush()
    }

    fn wait_for_response(&mut self, shm: &crate::shm::SharedMemory) -> io::Result<()> {
        let deadline = std::time::Instant::now() + self.timeout;
        while std::time::Instant::now() < deadline {
            if shm.as_slice().first().copied() == Some(1) {
                return Ok(());
            }
            let mut byte = [0u8; 1];
            let _ = std::io::Read::read(&mut self.tty, &mut byte);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "timed out waiting for the terminal to answer the askpass prompt",
        ))
    }
}

fn askpass_env_config() -> Option<SshHostConfig> {
    let host = std::env::var("KITTY_SSH_ASKPASS_HOST").ok()?;
    if host.is_empty() {
        return None;
    }
    let user = std::env::var("KITTY_SSH_ASKPASS_USER").unwrap_or_default();
    let overrides: Vec<String> = std::env::var("KITTY_SSH_ASKPASS_OVERRIDES")
        .ok()
        .and_then(|text| kitty_remote::json::parse(text.as_bytes()).ok())
        .and_then(|value| {
            value.as_array().map(|items| {
                items
                    .iter()
                    .filter_map(kitty_remote::json::Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
        })
        .unwrap_or_default();
    let set = load_ssh_config();
    let mut config = set.for_host(&host, &user);
    for override_value in overrides {
        if let Some((key, value)) = override_value.split_once('=') {
            let _ = config.apply(key, value);
        }
    }
    Some(config)
}

/// The `ssh_askpass` kitten entry point. Returns a process exit status.
pub fn run_askpass_cli() -> i32 {
    let Some(message) = std::env::args().next_back() else {
        return 1;
    };
    let confirm = std::env::var("SSH_ASKPASS_PROMPT").as_deref() == Ok("confirm");
    let fingerprint = message.contains("(yes/no/[fingerprint])");
    if !confirm && !fingerprint {
        if let Some(config) = askpass_env_config() {
            if is_password_prompt(&message) && !config.password.is_empty() {
                println!("{}", config.password);
                return 0;
            }
            if is_otp_prompt(&message) && !config.totp_secret.is_empty() {
                if let Ok(code) = generate_totp(
                    &config.totp_secret,
                    config.totp_digits,
                    config.totp_period,
                    SystemTime::now(),
                ) {
                    println!("{code}");
                    return 0;
                }
            }
        }
    }
    let request = AskpassRequest::from_message(&message, confirm);
    let mut host = match TtyAskpassHost::open() {
        Ok(host) => host,
        Err(err) => {
            eprintln!("{err}");
            return 1;
        }
    };
    match perform_askpass(&request, &mut host) {
        Ok(response) => {
            if !response.is_empty() {
                println!("{response}");
            }
            0
        }
        Err(err) => {
            eprintln!("{err}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_instruction_roundtrip_and_quoting() {
        let item = EnvInstruction::parse("FOO=bar baz").unwrap();
        assert_eq!(item.serialize(&|_| None), "export FOO=\"bar baz\"");
        let copy = EnvInstruction::parse("HOME=_kitty_copy_env_var_").unwrap();
        assert_eq!(
            copy.serialize(&|key| (key == "HOME").then(|| "/home/u".to_owned())),
            "export HOME=\"/home/u\""
        );
        let unset = EnvInstruction::parse("GONE").unwrap();
        assert_eq!(unset.serialize(&|_| None), "unset GONE");
    }

    #[test]
    fn host_config_matching_and_defaults() {
        let set = SshConfigSet::parse(
            "env GLOBAL=1\nhostname *.example.com\nremote_dir .kitty\nhostname other\ninterpreter python3\n",
        )
        .unwrap();
        let config = set.for_host("a.example.com", "u");
        assert_eq!(config.remote_dir, ".kitty");
        assert_eq!(config.env, vec!["GLOBAL=1"]);
        let other = set.for_host("other", "");
        assert_eq!(other.interpreter, "python3");
    }

    #[test]
    fn ssh_args_and_connection_data() {
        let args = ["-p", "2222", "-i", "id_ed25519", "user@example.com", "ls"]
            .iter()
            .map(|x| (*x).to_owned())
            .collect::<Vec<_>>();
        let (ssh, server, _, _) = parse_ssh_args(&args, &[]).unwrap();
        assert_eq!(ssh, vec!["-p", "2222", "-i", "id_ed25519"]);
        assert_eq!(server, vec!["user@example.com", "ls"]);
        let data = connection_data(&[
            "ssh".to_owned(),
            "-p".to_owned(),
            "2222".to_owned(),
            "user@example.com".to_owned(),
        ])
        .unwrap();
        assert_eq!(data.hostname, "user@example.com");
        assert_eq!(data.port, Some(2222));
    }

    #[test]
    fn totp_rfc6238_vector() {
        let code = generate_totp(
            "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ",
            8,
            30,
            UNIX_EPOCH + std::time::Duration::from_secs(59),
        )
        .unwrap();
        assert_eq!(code, "94287082");
    }

    #[test]
    fn copy_directive_parses_inline_options() {
        let dir = std::env::temp_dir().join(format!(
            "kitty-ssh-copyopts-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.txt"), b"a").unwrap();
        fs::write(dir.join("skip.tmp"), b"s").unwrap();
        let copies = parse_copy_directive(
            &format!(
                "--glob --exclude *.tmp --dest remote/ {}/a*",
                dir.to_string_lossy()
            ),
            &dir,
        )
        .unwrap();
        assert_eq!(copies.len(), 1);
        assert!(copies
            .iter()
            .all(|copy| copy.arcname.starts_with("home/remote/")));
        assert!(copies
            .iter()
            .all(|copy| copy.exclude_patterns.iter().any(|p| p == "*.tmp")));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn copy_directive_and_arcname() {
        let dir = std::env::temp_dir().join(format!("kitty-ssh-{}-{}", std::process::id(), 1));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.txt"), b"x").unwrap();
        let copies = parse_copy_instruction(
            &format!("{}", dir.join("a.txt").display()),
            false,
            "",
            &[],
            "preserve",
            &dir,
        )
        .unwrap();
        assert_eq!(copies.len(), 1);
        assert!(copies[0].arcname.starts_with("home/"));
        assert_eq!(
            get_arcname(Path::new("/etc/passwd"), "", &dir),
            "root/etc/passwd"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    fn read_tar_names(data: &[u8]) -> Vec<String> {
        use flate2::read::GzDecoder;
        let mut decoder = GzDecoder::new(data);
        let mut tar = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut tar).unwrap();
        let mut names = Vec::new();
        let mut offset = 0usize;
        while offset + 512 <= tar.len() {
            let header = &tar[offset..offset + 512];
            if header.iter().all(|byte| *byte == 0) {
                break;
            }
            let name_end = header[..100].iter().position(|b| *b == 0).unwrap_or(100);
            let name = String::from_utf8_lossy(&header[..name_end]).into_owned();
            let size_text = String::from_utf8_lossy(&header[124..136]);
            let size = usize::from_str_radix(size_text.trim_matches('\0').trim(), 8).unwrap_or(0);
            let typeflag = header[156];
            names.push(format!("{typeflag}:{name}"));
            offset += 512 + size.div_ceil(512) * 512;
        }
        names
    }

    struct FakeAskpassHost {
        response: String,
    }

    impl AskpassHost for FakeAskpassHost {
        fn trigger(&mut self, shm_name: &str) -> io::Result<()> {
            let mut shm = crate::shm::SharedMemory::open_existing(shm_name)?;
            let question_end = shm.as_slice()[1..]
                .iter()
                .rposition(|byte| *byte != 0)
                .map(|index| index + 2)
                .unwrap_or(1);
            let question = std::str::from_utf8(&shm.as_slice()[1..question_end]).unwrap();
            assert!(question.contains("\"type\""));
            let response = if self.response == "true" || self.response == "false" {
                self.response.clone()
            } else {
                serde_json_quote(&self.response)
            };
            shm.write_at(1, response.as_bytes())?;
            shm.as_mut_slice()[0] = 1;
            shm.flush()
        }

        fn wait_for_response(&mut self, _shm: &crate::shm::SharedMemory) -> io::Result<()> {
            Ok(())
        }
    }

    fn serde_json_quote(text: &str) -> String {
        let mut out = String::from("\"");
        for ch in text.chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                _ => out.push(ch),
            }
        }
        out.push('"');
        out
    }

    #[test]
    fn bootstrap_tar_contains_scripts_copies_and_terminfo() {
        let dir = std::env::temp_dir().join(format!(
            "kitty-ssh-tar-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("tree/sub")).unwrap();
        fs::write(dir.join("tree/a.txt"), b"hello").unwrap();
        fs::write(dir.join("tree/sub/b.txt"), b"world").unwrap();
        let copies = parse_copy_instruction(
            &dir.join("tree").to_string_lossy(),
            false,
            "dest",
            &[],
            "preserve",
            &dir,
        )
        .unwrap();
        let config = SshHostConfig::new();
        let tar = make_bootstrap_tar(
            &config,
            &copies,
            "export TEST=1\n",
            &BootstrapResources::default(),
        )
        .unwrap();
        let names = read_tar_names(&tar);
        assert!(names.iter().any(|name| name.contains("data.sh")));
        assert!(names.iter().any(|name| name.contains("bootstrap-utils.sh")));
        assert!(names.iter().any(|name| name.contains("dest/a.txt")));
        assert!(names.iter().any(|name| name.contains("dest/sub/b.txt")));
        assert!(names
            .iter()
            .any(|name| name.contains("home/.terminfo/x/xterm-kitty")));
        assert!(names.iter().any(|name| name.contains("kitty/bin/kitten")));
        let plan = build_bootstrap_plan(
            &config,
            copies,
            "export TEST=1\n".to_owned(),
            &["echo hi".to_owned()],
        );
        assert!(!plan.tar.is_empty());
        assert!(plan.remote_command[0] == "exec");
        assert!(plan.bootstrap_script.contains("KITTY_DATA_START"));
        assert!(plan.bootstrap_script.contains("echo hi"));
        if let Some(name) = &plan.shared_memory_name {
            let shm = crate::shm::SharedMemory::open_existing(name).unwrap();
            let text = String::from_utf8_lossy(shm.as_slice()).into_owned();
            assert!(text.contains("\"tarfile\""));
            assert!(text.contains("\"pw\""));
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn askpass_shared_memory_flow_confirm_and_line() {
        let request = AskpassRequest::from_message("Password:", false);
        let mut host = FakeAskpassHost {
            response: "hunter2".to_owned(),
        };
        assert_eq!(perform_askpass(&request, &mut host).unwrap(), "hunter2");
        let confirm = AskpassRequest::from_message("Continue?", true);
        let mut host = FakeAskpassHost {
            response: "true".to_owned(),
        };
        assert_eq!(perform_askpass(&confirm, &mut host).unwrap(), "yes");
        let fingerprint = AskpassRequest::from_message(
            "The authenticity of host x can't be established (yes/no/[fingerprint])?",
            false,
        );
        let mut host = FakeAskpassHost {
            response: "y".to_owned(),
        };
        assert_eq!(perform_askpass(&fingerprint, &mut host).unwrap(), "yes");
    }

    #[test]
    fn prepare_ssh_session_sets_bootstrap_and_askpass_environment() {
        let mut config = SshHostConfig::new();
        config.hostname = "u@example.com".to_owned();
        config.askpass = "native".to_owned();
        let options = BootstrapOptions::default();
        let env = BTreeMap::from([("TERM".to_owned(), "xterm-kitty".to_owned())]);
        let plan = prepare_ssh_session(
            &config,
            &["-p".to_owned(), "2222".to_owned()],
            &["u@example.com".to_owned(), "uname -a".to_owned()],
            &env,
            &options,
            &|key| (key == "HOME").then(|| "/home/u".to_owned()),
        )
        .unwrap();
        assert_eq!(plan.ssh_command[0], "ssh");
        assert!(plan.ssh_command.contains(&"2222".to_owned()));
        assert!(plan.ssh_command.contains(&"exec".to_owned()));
        assert_eq!(
            plan.environment
                .get("KITTY_SSH_ASKPASS_HOST")
                .map(String::as_str),
            Some("example.com")
        );
        assert!(plan.data_request.starts_with("\x1bP@kitty-ssh|"));
        assert!(plan
            .data_request
            .contains(plan.bootstrap.request_id.as_str()));
    }

    #[test]
    fn askpass_classification_and_autofill() {
        assert!(is_password_prompt("Password:"));
        assert!(!is_password_prompt("Passphrase for key:"));
        assert!(is_otp_prompt("Verification code:"));
        let mut config = SshHostConfig::new();
        config.password = "secret".to_owned();
        config.totp_secret = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ".to_owned();
        config.totp_digits = 8;
        let now = UNIX_EPOCH + std::time::Duration::from_secs(59);
        assert_eq!(
            askpass_response("Password:", &config, now)
                .unwrap()
                .as_deref(),
            Some("secret")
        );
        assert_eq!(
            askpass_response("OTP:", &config, now).unwrap().as_deref(),
            Some("94287082")
        );
    }
}

#[cfg(test)]
mod control_master_tests {
    use super::*;

    struct FakeRunner {
        checks: usize,
        forward_calls: usize,
        commands: Vec<Vec<String>>,
        forward_times_out_first: bool,
    }

    impl SshCommandRunner for FakeRunner {
        fn run(
            &mut self,
            argv: &[String],
            _timeout: Option<std::time::Duration>,
        ) -> io::Result<SshCommandOutput> {
            self.commands.push(argv.to_vec());
            if argv.iter().any(|arg| arg == "check") {
                self.checks += 1;
                return Ok(SshCommandOutput {
                    status: if self.checks >= 2 { 0 } else { 255 },
                    ..SshCommandOutput::default()
                });
            }
            if argv.iter().any(|arg| arg == "forward") {
                self.forward_calls += 1;
                if self.forward_calls == 1 && self.forward_times_out_first {
                    return Ok(SshCommandOutput {
                        status: 124,
                        ..SshCommandOutput::default()
                    });
                }
                return Ok(SshCommandOutput {
                    status: 0,
                    stdout: b"4321\n".to_vec(),
                    stderr: Vec::new(),
                });
            }
            Ok(SshCommandOutput::default())
        }
    }

    fn conn() -> SshConnectionData {
        SshConnectionData {
            binary: "ssh".to_owned(),
            hostname: "user@host".to_owned(),
            ..SshConnectionData::default()
        }
    }

    #[test]
    fn parses_forward_targets_and_rejects_abstract_sockets() {
        assert_eq!(
            parse_forward_target("unix:/tmp/kitty.sock").unwrap(),
            ForwardTarget::UnixPath("/tmp/kitty.sock".to_owned())
        );
        assert_eq!(
            parse_forward_target("tcp:localhost:1234").unwrap(),
            ForwardTarget::Tcp("localhost:1234".to_owned())
        );
        assert_eq!(
            parse_forward_target("/tmp/plain.sock").unwrap(),
            ForwardTarget::UnixPath("/tmp/plain.sock".to_owned())
        );
        assert!(parse_forward_target("unix:@abstract").is_err());
        assert!(parse_forward_target("").is_err());
    }

    #[test]
    fn control_master_commands_share_one_path() {
        let sharing = connection_sharing_args(42);
        assert!(sharing.contains(&"ControlMaster=auto".to_owned()));
        assert!(sharing.iter().any(|arg| arg.contains("kitty-ssh-42-%C")));
        let check = control_master_check_command(&conn());
        assert_eq!(&check[..2], ["ssh", "-o"]);
        assert!(check.contains(&"check".to_owned()));
        assert_eq!(check.last().unwrap(), "user@host");
        let forward = port_forward_command(&conn(), "unix:/tmp/kitty.sock").unwrap();
        assert!(forward.contains(&"0:/tmp/kitty.sock".to_owned()));
        assert!(forward.contains(&"forward".to_owned()));
    }

    #[test]
    fn forward_recovers_a_stale_master_once() {
        let mut runner = FakeRunner {
            checks: 0,
            forward_calls: 0,
            commands: Vec::new(),
            forward_times_out_first: true,
        };
        let forwarded =
            forward_remote_control(&conn(), "unix:/tmp/kitty.sock", &mut runner).unwrap();
        assert!(forwarded.recovered);
        assert_eq!(forwarded.listen_on, "tcp:localhost:4321");
        let forwarded_calls = runner
            .commands
            .iter()
            .filter(|argv| argv.iter().any(|arg| arg == "forward"))
            .count();
        assert_eq!(forwarded_calls, 2);
        let exit_calls = runner
            .commands
            .iter()
            .filter(|argv| argv.iter().any(|arg| arg == "exit"))
            .count();
        assert_eq!(exit_calls, 1);
        let start_calls = runner
            .commands
            .iter()
            .filter(|argv| argv.iter().any(|arg| arg == "-N"))
            .count();
        assert!(start_calls >= 1);
    }

    #[test]
    fn forward_without_a_stale_master_does_not_recover() {
        let mut runner = FakeRunner {
            checks: 1,
            forward_calls: 0,
            commands: Vec::new(),
            forward_times_out_first: false,
        };
        let forwarded = forward_remote_control(&conn(), "tcp:localhost:2222", &mut runner).unwrap();
        assert!(!forwarded.recovered);
        assert_eq!(forwarded.listen_on, "tcp:localhost:4321");
        assert_eq!(runner.forward_calls, 1);
        assert!(!runner
            .commands
            .iter()
            .any(|argv| argv.iter().any(|arg| arg == "-N")));
    }
}
