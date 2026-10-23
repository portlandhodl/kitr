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
    let (base, tail) = split_glob_base(&text);
    let base = if base.is_empty() {
        PathBuf::from(".")
    } else {
        PathBuf::from(base)
    };
    let mut out = Vec::new();
    walk_glob(
        &base,
        &[],
        &tail
            .split('/')
            .filter(|x| !x.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>(),
        &mut out,
    )?;
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
}

impl SshConnectionData {
    pub fn command_prefix(&self) -> Vec<String> {
        if self.is_ssh_kitten {
            return self.ssh_kitten_cmdline.clone();
        }
        let mut argv = vec![
            self.binary.clone(),
            "-o".to_owned(),
            format!("ControlPath=~/.ssh/kitty-ssh-{}-%C", std::process::id()),
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

/// Everything needed to bootstrap a remote session. The archive itself is
/// assembled by the caller (or the Python shell-integration resource); this
/// plan carries the command line, environment script and copy list.
#[derive(Clone, Debug, Default)]
pub struct BootstrapPlan {
    pub remote_command: Vec<String>,
    pub env_script: String,
    pub copy: Vec<CopyInstruction>,
    pub request_id: String,
    pub parse_script: String,
}

pub fn build_bootstrap_plan(
    config: &SshHostConfig,
    copy: Vec<CopyInstruction>,
    env_script: String,
    remote_args: &[String],
) -> BootstrapPlan {
    let request_id = std::env::var("KITTY_PID").unwrap_or_else(|_| std::process::id().to_string());
    let window_id = std::env::var("KITTY_WINDOW_ID").unwrap_or_default();
    let request_id = if window_id.is_empty() {
        request_id
    } else {
        format!("{request_id}-{window_id}")
    };
    let mut remote_command = vec![config.interpreter.clone(), "-c".to_owned()];
    if !remote_args.is_empty() {
        remote_command.push(remote_args.join(" "));
    }
    BootstrapPlan {
        remote_command,
        env_script,
        copy,
        request_id,
        parse_script: String::new(),
    }
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
