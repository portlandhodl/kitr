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

/// One external command to execute.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    pub argv: Vec<String>,
    pub stdin: Option<Vec<u8>>,
    pub stdout_to: Option<PathBuf>,
}

impl CommandSpec {
    pub fn new(argv: Vec<String>) -> Self {
        Self {
            argv,
            stdin: None,
            stdout_to: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandOutput {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl CommandOutput {
    pub fn success(&self) -> bool {
        self.status == 0
    }

    pub fn error_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_owned()
    }
}

/// Abstraction over process execution so the remote-file loop is testable
/// without a real ssh server.
pub trait CommandRunner {
    fn run(&mut self, spec: &CommandSpec) -> Result<CommandOutput, String>;
}

/// The default runner backed by [`std::process::Command`].
#[derive(Clone, Debug, Default)]
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&mut self, spec: &CommandSpec) -> Result<CommandOutput, String> {
        let Some(program) = spec.argv.first() else {
            return Err("empty command".to_owned());
        };
        let mut command = std::process::Command::new(program);
        command.args(&spec.argv[1..]);
        command.stdin(if spec.stdin.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        });
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());
        let mut child = command.spawn().map_err(|err| err.to_string())?;
        if let Some(data) = &spec.stdin {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(data).map_err(|err| err.to_string())?;
            }
        }
        let output = child.wait_with_output().map_err(|err| err.to_string())?;
        if let Some(path) = &spec.stdout_to {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent).map_err(|err| err.to_string())?;
                }
            }
            fs::write(path, &output.stdout).map_err(|err| err.to_string())?;
        }
        Ok(CommandOutput {
            status: output.status.code().unwrap_or(1),
            stdout: if spec.stdout_to.is_some() {
                Vec::new()
            } else {
                output.stdout
            },
            stderr: output.stderr,
        })
    }
}

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

    fn quote_remote(&self) -> String {
        crate::ssh::quote_sh(&self.remote_path)
    }

    /// Start the control master (if needed) and create a temporary destination
    /// when no destination was supplied.
    pub fn start(&mut self, runner: &mut dyn CommandRunner) -> Result<(), String> {
        for argv in self.start_commands() {
            let output = runner.run(&CommandSpec::new(argv))?;
            if !output.success() {
                return Err(format!(
                    "the ssh command failed with exit status {}: {}",
                    output.status,
                    output.error_text()
                ));
            }
        }
        if self.dest.as_os_str().is_empty() {
            let dir = std::env::temp_dir().join(format!(
                "kitty-remote-file-{}-{}",
                std::process::id(),
                auto_rename_counter()
            ));
            fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
            let name = Path::new(&self.remote_path)
                .file_name()
                .map(|name| name.to_owned())
                .unwrap_or_else(|| std::ffi::OsString::from("remote-file"));
            self.dest = dir.join(name);
            self.temp_dir = Some(dir);
        }
        Ok(())
    }

    /// Close the control master and remove any temporary directory.
    pub fn close(&mut self, runner: &mut dyn CommandRunner) {
        if let Some(argv) = self.close_command() {
            let _ = runner.run(&CommandSpec::new(argv));
        }
        if let Some(dir) = self.temp_dir.take() {
            let _ = fs::remove_dir_all(dir);
        }
    }

    pub fn is_alive(&self, runner: &mut dyn CommandRunner) -> bool {
        if self.conn.is_ssh_kitten {
            return true;
        }
        let Some(argv) = self.start_commands().into_iter().nth(1) else {
            return false;
        };
        runner
            .run(&CommandSpec::new(argv))
            .map(|output| output.success())
            .unwrap_or(false)
    }

    pub fn download(&self, runner: &mut dyn CommandRunner) -> Result<(), String> {
        if let Some(parent) = self.dest.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|err| err.to_string())?;
            }
        }
        let mut spec = CommandSpec::new(self.download_command());
        spec.stdout_to = Some(self.dest.clone());
        let output = runner.run(&spec)?;
        if output.success() {
            Ok(())
        } else {
            Err(format!(
                "The command: {} failed\n{}",
                spec.argv.join(" "),
                output.error_text()
            ))
        }
    }

    pub fn upload(
        &self,
        runner: &mut dyn CommandRunner,
        suppress_output: bool,
    ) -> Result<(), String> {
        let data = fs::read(&self.dest).map_err(|err| err.to_string())?;
        let mut spec = CommandSpec::new(self.upload_command(suppress_output));
        spec.stdin = Some(data);
        let output = runner.run(&spec)?;
        if output.success() {
            Ok(())
        } else {
            Err(format!(
                "The command: {} failed\n{}",
                spec.argv.join(" "),
                output.error_text()
            ))
        }
    }

    pub fn remote_hostname(&self, runner: &mut dyn CommandRunner) -> String {
        if self.conn.is_ssh_kitten {
            return String::new();
        }
        let Ok(output) = runner.run(&CommandSpec::new(self.hostname_command())) else {
            return String::new();
        };
        if !output.success() {
            return String::new();
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .unwrap_or("")
            .to_owned()
    }

    pub fn check_hostname_matches(
        &self,
        cli_hostname: &str,
        runner: &mut dyn CommandRunner,
    ) -> bool {
        if self.conn.is_ssh_kitten {
            return true;
        }
        let actual = self.remote_hostname(runner);
        actual.is_empty() || hostname_matches(cli_hostname, &actual)
    }

    pub fn edit_loop(
        &mut self,
        runner: &mut dyn CommandRunner,
        editor: &[String],
        poll: std::time::Duration,
    ) -> Result<(), String> {
        let metadata = fs::metadata(&self.dest).map_err(|err| err.to_string())?;
        let mut mtime = metadata.modified().map_err(|err| err.to_string())?;
        let mut argv = editor.to_vec();
        argv.push(self.dest.to_string_lossy().into_owned());
        let Some(program) = argv.first() else {
            return Err("no editor configured".to_owned());
        };
        let mut child = std::process::Command::new(program)
            .args(&argv[1..])
            .spawn()
            .map_err(|err| err.to_string())?;
        loop {
            if let Some(status) = child.try_wait().map_err(|err| err.to_string())? {
                if !self.is_alive(runner) {
                    return Err(format!(
                        "failed to upload {}, SSH master process died",
                        self.remote_path
                    ));
                }
                self.upload(runner, false)?;
                if !status.success() {
                    return Err(format!("editor exited with status {}", status));
                }
                return Ok(());
            }
            if let Ok(metadata) = fs::metadata(&self.dest) {
                if let Ok(modified) = metadata.modified() {
                    if modified > mtime {
                        mtime = modified;
                        if self.is_alive(runner) {
                            let _ = self.upload(runner, true);
                        }
                    }
                }
            }
            std::thread::sleep(poll);
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
        argv.push(self.quote_remote());
        argv
    }

    pub fn upload_command(&self, suppress_output: bool) -> Vec<String> {
        let mut argv = if suppress_output {
            self.conn.command_prefix()
        } else {
            self.conn.batch_prefix()
        };
        argv.push(self.conn.hostname.clone());
        argv.extend(["cat".to_owned(), ">".to_owned(), self.quote_remote()]);
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

fn auto_rename_counter() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0)
}

/// The editor command from `$VISUAL`/`$EDITOR`, with a small built-in fallback
/// list matching kitty's `get_editor_from_env_vars`.
pub fn get_editor() -> Vec<String> {
    for key in ["VISUAL", "EDITOR"] {
        if let Ok(value) = std::env::var(key) {
            if let Ok(parts) = crate::ssh::shlex_split(&value) {
                if !parts.is_empty() {
                    return parts;
                }
            }
        }
    }
    for candidate in [
        "vim", "nvim", "vi", "emacs", "hx", "kak", "micro", "nano", "vis",
    ] {
        if let Ok(path) = which(candidate) {
            return vec![path];
        }
    }
    vec!["vim".to_owned()]
}

fn which(name: &str) -> Result<String, ()> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(candidate.to_string_lossy().into_owned());
        }
    }
    Err(())
}

/// Download a remote file to `dest` (or a temporary directory if empty).
pub fn download_remote_file(
    conn: SshConnectionData,
    remote_path: &str,
    dest: PathBuf,
    runner: &mut dyn CommandRunner,
) -> Result<ControlMaster, String> {
    let mut master = ControlMaster::new(conn, remote_path, dest);
    master.start(runner)?;
    if !master.check_hostname_matches(
        master.conn.hostname.split('@').next_back().unwrap_or(""),
        runner,
    ) {
        master.close(runner);
        return Err("remote hostname does not match the hyperlink hostname".to_owned());
    }
    master.download(runner)?;
    Ok(master)
}

/// Run the edit/open/save action used by the remote-file kitten.
pub fn handle_remote_file_action(
    action: FileAction,
    opts: &RemoteFileOptions,
    runner: &mut dyn CommandRunner,
    editor: &[String],
) -> Result<Option<String>, String> {
    if action == FileAction::Cancel {
        return Ok(None);
    }
    let conn = parse_connection_data(&opts.ssh_connection_data)?;
    match action {
        FileAction::Edit => {
            let mut master = ControlMaster::new(conn, opts.path.clone(), PathBuf::new());
            master.start(runner)?;
            if !master.check_hostname_matches(&opts.hostname, runner) {
                master.close(runner);
                return Ok(None);
            }
            if let Err(err) = master.download(runner) {
                master.close(runner);
                return Err(err);
            }
            let path = master.dest.clone();
            let result = master.edit_loop(runner, editor, std::time::Duration::from_millis(100));
            master.close(runner);
            result.map(|()| Some(path.to_string_lossy().into_owned()))
        }
        FileAction::Open => {
            let dir = std::env::temp_dir().join(format!(
                "kitty-remote-file-{}-{}",
                std::process::id(),
                auto_rename_counter()
            ));
            let name = Path::new(&opts.path)
                .file_name()
                .map(|name| name.to_owned())
                .unwrap_or_else(|| std::ffi::OsString::from("remote-file"));
            let dest = dir.join(name);
            let mut master = download_remote_file(conn, &opts.path, dest.clone(), runner)?;
            let path = master.dest.clone();
            master.close(runner);
            Ok(Some(path.to_string_lossy().into_owned()))
        }
        FileAction::Save => {
            let base = std::env::temp_dir();
            let dest = default_save_path(&base, &opts.path);
            let dest = auto_rename_dest(&dest, |path| path.exists());
            let mut master = download_remote_file(conn, &opts.path, dest.clone(), runner)?;
            let path = master.dest.clone();
            master.close(runner);
            Ok(Some(path.to_string_lossy().into_owned()))
        }
        FileAction::Cancel => Ok(None),
    }
}

fn ask_action_from_stdin(opts: &RemoteFileOptions) -> Result<FileAction, String> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        return Ok(FileAction::Cancel);
    }
    eprintln!(
        "What would you like to do with the remote file on {}:",
        opts.hostname
    );
    eprintln!("{}", opts.path);
    eprintln!("E)dit  O)pen  S)ave  C)ancel");
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|err| err.to_string())?;
    Ok(FileAction::parse(line.chars().next().unwrap_or('c')))
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
    let action = match opts.mode {
        RemoteFileMode::Edit => FileAction::Edit,
        RemoteFileMode::Ask => ask_action_from_stdin(&opts)?,
    };
    if action == FileAction::Cancel {
        return Ok(None);
    }
    writeln!(
        out,
        "{} {} from {}",
        match action {
            FileAction::Edit => "Editing",
            FileAction::Open => "Opening",
            FileAction::Save => "Saving",
            FileAction::Cancel => "Canceling",
        },
        opts.path,
        opts.hostname
    )
    .map_err(|err| err.to_string())?;
    let mut runner = SystemRunner;
    handle_remote_file_action(action, &opts, &mut runner, &get_editor())
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

    #[derive(Default)]
    struct FakeRunner {
        downloads: usize,
        uploads: Vec<Vec<u8>>,
        commands: Vec<Vec<String>>,
        hostname: String,
    }

    impl CommandRunner for FakeRunner {
        fn run(&mut self, spec: &CommandSpec) -> Result<CommandOutput, String> {
            self.commands.push(spec.argv.clone());
            if spec.argv.iter().any(|arg| arg == "hostname") {
                let mut out = self.hostname.clone().into_bytes();
                out.push(b'\n');
                return Ok(CommandOutput {
                    status: 0,
                    stdout: out,
                    stderr: Vec::new(),
                });
            }
            if spec
                .argv
                .iter()
                .any(|arg| arg == "cat" && spec.stdout_to.is_some())
            {
                if let Some(path) = &spec.stdout_to {
                    fs::write(path, b"remote contents").unwrap();
                    self.downloads += 1;
                }
                return Ok(CommandOutput::default());
            }
            if spec
                .argv
                .iter()
                .any(|arg| arg == "cat" && spec.stdin.is_some())
            {
                self.uploads.push(spec.stdin.clone().unwrap_or_default());
                return Ok(CommandOutput::default());
            }
            Ok(CommandOutput::default())
        }
    }

    fn ssh_data(hostname: &str) -> String {
        format!(r#"["/usr/bin/ssh","{hostname}","22","/home/u/.ssh/id"]"#)
    }

    #[test]
    fn open_downloads_and_edit_uploads_changed_file() {
        let mut runner = FakeRunner {
            hostname: "host.example.com".to_owned(),
            ..FakeRunner::default()
        };
        let opts = RemoteFileOptions {
            mode: RemoteFileMode::Ask,
            path: "/remote/a.txt".to_owned(),
            hostname: "host.example.com".to_owned(),
            ssh_connection_data: ssh_data("u@host.example.com"),
            ..RemoteFileOptions::default()
        };
        let path = handle_remote_file_action(FileAction::Open, &opts, &mut runner, &[])
            .unwrap()
            .unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "remote contents");
        assert_eq!(runner.downloads, 1);
        let _ = fs::remove_dir_all(Path::new(&path).parent().unwrap());

        let mut runner = FakeRunner {
            hostname: "host.example.com".to_owned(),
            ..FakeRunner::default()
        };
        let opts = RemoteFileOptions {
            mode: RemoteFileMode::Edit,
            path: "/remote/a.txt".to_owned(),
            hostname: "host.example.com".to_owned(),
            ssh_connection_data: ssh_data("u@host.example.com"),
            ..RemoteFileOptions::default()
        };
        let editor = vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "printf changed > \"$1\"".to_owned(),
            "sh".to_owned(),
        ];
        let path = handle_remote_file_action(FileAction::Edit, &opts, &mut runner, &editor)
            .unwrap()
            .unwrap();
        assert_eq!(runner.downloads, 1);
        assert_eq!(runner.uploads, vec![b"changed".to_vec()]);
        assert!(!Path::new(&path).exists());
    }

    #[test]
    fn hostname_mismatch_aborts_before_download() {
        let mut runner = FakeRunner {
            hostname: "other.example.com".to_owned(),
            ..FakeRunner::default()
        };
        let opts = RemoteFileOptions {
            mode: RemoteFileMode::Ask,
            path: "/remote/a.txt".to_owned(),
            hostname: "host.example.com".to_owned(),
            ssh_connection_data: ssh_data("u@host.example.com"),
            ..RemoteFileOptions::default()
        };
        let result = handle_remote_file_action(FileAction::Open, &opts, &mut runner, &[]);
        assert!(result.is_err());
        assert_eq!(runner.downloads, 0);
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
