// SPDX-License-Identifier: GPL-3.0-or-later
//! Remote-file kitten, ported from `kittens/remote_file`.
//!
//! Handles the JSON `--ssh-connection-data` format, control-master command
//! construction, hostname validation, open/edit/save action selection and
//! safe auto-renaming. Network execution is intentionally thin and reuses
//! `ssh`/`cat` command arrays so it is testable without a server.

use std::fs;
use std::path::{Path, PathBuf};

use kitty_remote::json::{self, Value};

use crate::ssh::SshConnectionData;

pub const REMOTE_FILE_USAGE: &str = "\
Usage: kitten remote_file [options]

Ask the user what to do with a remote file. Internal use by kitty.

Options:
  -m, --mode ask|edit
  -p, --path PATH
      --hostname HOST
      --ssh-connection-data JSON
  -h, --help";

pub const IS_SSH_KITTEN_SENTINEL: &str = "!#*&$#($ssh-kitten)(##$";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RemoteFileMode {
    #[default]
    Ask,
    Edit,
}

impl RemoteFileMode {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "ask" => Self::Ask,
            "edit" => Self::Edit,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct RemoteFileOptions {
    pub mode: RemoteFileMode,
    pub path: String,
    pub hostname: String,
    pub ssh_connection_data: String,
    pub help: bool,
}

impl Default for RemoteFileOptions {
    fn default() -> Self {
        Self {
            mode: RemoteFileMode::Ask,
            path: String::new(),
            hostname: String::new(),
            ssh_connection_data: String::new(),
            help: false,
        }
    }
}

pub fn parse_remote_file_args(args: &[String]) -> Result<RemoteFileOptions, String> {
    let mut opts = RemoteFileOptions::default();
    let mut index = 0usize;
    while index < args.len() {
        let arg = &args[index];
        let (name, inline) = arg
            .split_once('=')
            .map_or((arg.as_str(), None), |(a, b)| (a, Some(b.to_owned())));
        let mut take = || -> String {
            if let Some(value) = inline.clone() {
                return value;
            }
            index += 1;
            args.get(index).cloned().unwrap_or_default()
        };
        match name {
            "-h" | "--help" => opts.help = true,
            "-m" | "--mode" => {
                let value = take();
                opts.mode =
                    RemoteFileMode::parse(&value).ok_or_else(|| format!("invalid mode: {value}"))?
            }
            "-p" | "--path" => opts.path = take(),
            "--hostname" => opts.hostname = take(),
            "--ssh-connection-data" => opts.ssh_connection_data = take(),
            other => return Err(format!("unknown option: {other}")),
        }
        index += 1;
    }
    Ok(opts)
}

/// Parse the connection-data JSON array used by kitty.
///
/// The ssh-kitten form is `[sentinel, ...cli_data, hostname]`; the plain form
/// is `[binary, hostname, port?, identity_file?]`.
pub fn parse_connection_data(raw: &str) -> Result<SshConnectionData, String> {
    let value = json::parse(raw.as_bytes()).map_err(|err| err.to_string())?;
    let Some(items) = value.as_array() else {
        return Err("invalid --ssh-connection-data: expected an array".to_owned());
    };
    if items.len() < 2 {
        return Err("invalid --ssh-connection-data: need at least binary and hostname".to_owned());
    }
    let first = items[0].as_str().unwrap_or("");
    if first == IS_SSH_KITTEN_SENTINEL {
        let hostname = items
            .last()
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let mut cmdline: Vec<String> = items[1..]
            .iter()
            .filter_map(Value::as_str)
            .filter(|item| *item != "-t")
            .map(str::to_owned)
            .collect();
        if cmdline.len() >= 2 {
            cmdline.truncate(cmdline.len() - 2);
        } else {
            cmdline.clear();
        }
        return Ok(SshConnectionData {
            hostname,
            is_ssh_kitten: true,
            ssh_kitten_cmdline: cmdline,
            ..SshConnectionData::default()
        });
    }
    let hostname = items[1].as_str().unwrap_or("").to_owned();
    let port = items
        .get(2)
        .and_then(Value::as_i64)
        .and_then(|port| u16::try_from(port).ok());
    let identity_file = items
        .get(3)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    Ok(SshConnectionData {
        binary: first.to_owned(),
        hostname,
        port,
        identity_file,
        ..SshConnectionData::default()
    })
}

pub fn hostname_matches(from_hyperlink: &str, actual: &str) -> bool {
    if from_hyperlink == actual {
        return true;
    }
    let first = |text: &str| text.split('.').next().unwrap_or("").to_owned();
    first(from_hyperlink) == first(actual)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileAction {
    Edit,
    Open,
    Save,
    Cancel,
}

impl FileAction {
    pub fn parse(key: char) -> Self {
        match key.to_ascii_lowercase() {
            'e' => Self::Edit,
            'o' => Self::Open,
            's' => Self::Save,
            _ => Self::Cancel,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Edit => "edit",
            Self::Open => "open",
            Self::Save => "save",
            Self::Cancel => "cancel",
        }
    }
}

/// Control-master operations, mirroring `actions.go`.
#[derive(Clone, Debug)]
pub struct ControlMaster {
    pub conn: SshConnectionData,
    pub remote_path: String,
    pub dest: PathBuf,
    pub temp_dir: Option<PathBuf>,
}

impl ControlMaster {
    pub fn new(conn: SshConnectionData, remote_path: impl Into<String>, dest: PathBuf) -> Self {
        Self {
            conn,
            remote_path: remote_path.into(),
            dest,
            temp_dir: None,
        }
    }

    pub fn start_commands(&self) -> Vec<Vec<String>> {
        if self.conn.is_ssh_kitten {
            return Vec::new();
        }
        let mut prefix = self.conn.command_prefix();
        let mut start = prefix.clone();
        start.extend([
            "-o".to_owned(),
            "ControlMaster=auto".to_owned(),
            "-fN".to_owned(),
            self.conn.hostname.clone(),
        ]);
        prefix.extend([
            "-o".to_owned(),
            "BatchMode=yes".to_owned(),
            "-O".to_owned(),
            "check".to_owned(),
            self.conn.hostname.clone(),
        ]);
        vec![start, prefix]
    }

    pub fn close_command(&self) -> Option<Vec<String>> {
        if self.conn.is_ssh_kitten {
            return None;
        }
        let mut argv = self.conn.batch_prefix();
        argv.extend([
            "-O".to_owned(),
            "exit".to_owned(),
            self.conn.hostname.clone(),
        ]);
        Some(argv)
    }

    pub fn download_command(&self) -> Vec<String> {
        let mut argv = self.conn.batch_prefix();
        argv.push(self.conn.hostname.clone());
        argv.push("cat".to_owned());
        argv.push(self.remote_path.clone());
        argv
    }

    pub fn upload_command(&self, suppress_output: bool) -> Vec<String> {
        let mut argv = if suppress_output {
            self.conn.command_prefix()
        } else {
            self.conn.batch_prefix()
        };
        argv.push(self.conn.hostname.clone());
        argv.extend(["cat".to_owned(), ">".to_owned(), self.remote_path.clone()]);
        argv
    }

    pub fn hostname_command(&self) -> Vec<String> {
        let mut argv = self.conn.batch_prefix();
        argv.push(self.conn.hostname.clone());
        argv.extend(["hostname".to_owned(), "-f".to_owned()]);
        argv
    }
}

/// Python-like splitext used by auto-renaming.
pub fn python_split_ext(path: &Path) -> (PathBuf, String) {
    let name = path
        .file_name()
        .map(|x| x.to_string_lossy().into_owned())
        .unwrap_or_default();
    let directory = path.parent().unwrap_or(Path::new(""));
    let leading = name.chars().take_while(|ch| *ch == '.').count();
    let remainder = &name[leading..];
    let Some(index) = remainder.rfind('.') else {
        return (path.to_path_buf(), String::new());
    };
    let split = leading + index;
    let base = PathBuf::from(&name[..split]);
    let ext = name[split..].to_owned();
    let result = if directory.as_os_str().is_empty() || directory == Path::new(".") {
        base
    } else {
        directory.join(base)
    };
    (result, ext)
}

pub fn auto_rename_dest(dest: &Path, exists: impl Fn(&Path) -> bool) -> PathBuf {
    let (base, ext) = python_split_ext(dest);
    let mut candidate = dest.to_path_buf();
    let mut counter = 1usize;
    while exists(&candidate) {
        candidate = PathBuf::from(format!("{}-{counter}{ext}", base.to_string_lossy()));
        counter += 1;
    }
    candidate
}

/// Default destination for saving a remote file under `base_dir`.
pub fn default_save_path(base_dir: &Path, remote_path: &str) -> PathBuf {
    let name = Path::new(remote_path)
        .file_name()
        .map(|x| x.to_owned())
        .unwrap_or_default();
    base_dir.join(name)
}

pub fn run_remote_file_cli(
    args: &[String],
    out: &mut dyn std::io::Write,
) -> Result<Option<String>, String> {
    let opts = parse_remote_file_args(args)?;
    if opts.help {
        writeln!(out, "{REMOTE_FILE_USAGE}").map_err(|err| err.to_string())?;
        return Ok(None);
    }
    let conn = parse_connection_data(&opts.ssh_connection_data)?;
    let master = ControlMaster::new(
        conn,
        opts.path.clone(),
        PathBuf::from(&opts.path)
            .file_name()
            .map(|name| std::env::temp_dir().join(name))
            .unwrap_or_else(|| std::env::temp_dir().join("remote-file")),
    );
    let action = match opts.mode {
        RemoteFileMode::Edit => FileAction::Edit,
        RemoteFileMode::Ask => FileAction::Cancel,
    };
    if action == FileAction::Edit {
        let _ = fs::create_dir_all(master.dest.parent().unwrap_or(Path::new(".")));
        writeln!(out, "Editing {} from {}", opts.path, opts.hostname)
            .map_err(|err| err.to_string())?;
        return Ok(Some(master.dest.to_string_lossy().into_owned()));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_connection_data() {
        let conn = parse_connection_data(r#"["/usr/bin/ssh","user@host",2222,"/home/u/.ssh/id"]"#)
            .unwrap();
        assert_eq!(conn.binary, "/usr/bin/ssh");
        assert_eq!(conn.hostname, "user@host");
        assert_eq!(conn.port, Some(2222));
        assert_eq!(conn.identity_file, "/home/u/.ssh/id");
        assert!(!conn.is_ssh_kitten);
    }

    #[test]
    fn parses_ssh_kitten_sentinel() {
        let raw = format!(
            r#"["{}", "kitten", "ssh", "-t", "user@host"]"#,
            IS_SSH_KITTEN_SENTINEL
        );
        let conn = parse_connection_data(&raw).unwrap();
        assert!(conn.is_ssh_kitten);
        assert_eq!(conn.hostname, "user@host");
        assert_eq!(conn.ssh_kitten_cmdline, vec!["kitten"]);
    }

    #[test]
    fn hostname_and_rename() {
        assert!(hostname_matches("host.example.com", "host"));
        assert!(!hostname_matches("a", "b"));
        assert_eq!(
            python_split_ext(Path::new("/x/a.b.c")),
            (PathBuf::from("/x/a.b"), ".c".to_owned())
        );
        assert_eq!(
            python_split_ext(Path::new("/x/.bashrc")),
            (PathBuf::from("/x/.bashrc"), String::new())
        );
        let dir = std::env::temp_dir().join(format!("kitty-rf-{}-1", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("a.txt");
        fs::write(&dest, b"x").unwrap();
        let renamed = auto_rename_dest(&dest, |path| path.exists());
        assert_eq!(renamed, dir.join("a-1.txt"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn control_master_commands() {
        let conn = SshConnectionData {
            binary: "ssh".to_owned(),
            hostname: "host".to_owned(),
            port: Some(22),
            ..SshConnectionData::default()
        };
        let master = ControlMaster::new(conn, "/remote/a.txt", PathBuf::from("/tmp/a.txt"));
        let start = master.start_commands();
        assert_eq!(start.len(), 2);
        assert!(start[0].contains(&"ControlMaster=auto".to_owned()));
        assert!(start[1].contains(&"check".to_owned()));
        assert_eq!(master.download_command().last().unwrap(), "/remote/a.txt");
        assert!(master.upload_command(true).contains(&">".to_owned()));
        assert_eq!(master.hostname_command().last().unwrap(), "-f");
    }

    #[test]
    fn action_keys() {
        assert_eq!(FileAction::parse('e'), FileAction::Edit);
        assert_eq!(FileAction::parse('O'), FileAction::Open);
        assert_eq!(FileAction::parse('s'), FileAction::Save);
        assert_eq!(FileAction::parse('x'), FileAction::Cancel);
    }

    #[test]
    fn parses_cli() {
        let args = ["-m", "edit", "-p", "/remote/a", "--hostname", "h"]
            .iter()
            .map(|x| (*x).to_owned())
            .collect::<Vec<_>>();
        let opts = parse_remote_file_args(&args).unwrap();
        assert_eq!(opts.mode, RemoteFileMode::Edit);
        assert_eq!(opts.path, "/remote/a");
        assert_eq!(opts.hostname, "h");
    }
}
