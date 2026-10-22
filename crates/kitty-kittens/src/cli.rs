// SPDX-License-Identifier: GPL-3.0-or-later
//! Library-only command-line surface for the kitten launcher.
//!
//! The parser understands:
//!
//! * `kitten run NAME [ARGS...]` (and, for kitty compatibility, `kitten NAME`);
//! * `kitten @ [ARGS...]`, a passthrough hook for remote control. This crate
//!   does **not** implement a socket server; the caller decides what to do with
//!   the [`AtInvocation`];
//! * `kitten list`, `kitten help [NAME]` and `kitten manifest [NAME]`.
//!
//! A single `--` immediately after the kitten name ends option processing and
//! is removed, so `kitten run foo -- --flag` forwards `--flag` to `foo`. Later
//! `--` arguments are forwarded unchanged.

use crate::discovery::KittenRegistry;
use crate::error::{Error, Result};
use crate::launch::{build_launch, LaunchOptions, LaunchPlan};

/// Top-level usage text.
pub const USAGE: &str = "\
Usage: kitten <command> [options]

Commands:
  run NAME [ARGS...]   Run a kitten
  @ [ARGS...]          Pass arguments to the remote-control client
  list                 List installed kittens
  help [NAME]          Show help for all kittens or one kitten
  manifest [NAME]      Emit manifest JSON for all kittens or one kitten

A single '--' after NAME is consumed and the remaining arguments are forwarded
unchanged to the kitten.";

/// A parsed launcher command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KittenCommand {
    Run {
        name: String,
        args: Vec<String>,
        /// Whether an explicit `--` separator was present.
        explicit_separator: bool,
    },
    /// Remote-control passthrough. No parsing is performed on the arguments.
    At {
        args: Vec<String>,
    },
    List,
    Help {
        name: Option<String>,
    },
    Manifest {
        name: Option<String>,
    },
}

/// A `kitten @` invocation handed back to the embedding application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtInvocation {
    args: Vec<String>,
}

impl AtInvocation {
    pub fn new(args: Vec<String>) -> Self {
        Self { args }
    }

    /// The untouched arguments after `@`.
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// The first argument, if any (usually a remote-control command name).
    pub fn command(&self) -> Option<&str> {
        self.args.first().map(String::as_str)
    }

    /// Consume the invocation and return the arguments.
    pub fn into_args(self) -> Vec<String> {
        self.args
    }
}

/// The result of executing a [`KittenCommand`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandAction {
    /// The caller should spawn this plan.
    Run(LaunchPlan),
    /// The caller should print this text.
    Print(String),
    /// Remote-control passthrough for the caller's client/socket layer.
    Passthrough(AtInvocation),
}

/// Parse command-line arguments, excluding argv[0].
pub fn parse_kitten_command<I, S>(args: I) -> Result<KittenCommand>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let args: Vec<String> = args.into_iter().map(Into::into).collect();
    let Some(first) = args.first().map(String::as_str) else {
        return Ok(KittenCommand::List);
    };
    match first {
        "run" => parse_run(&args[1..]),
        "@" => Ok(KittenCommand::At {
            args: args[1..].to_vec(),
        }),
        "list" | "--list" => Ok(KittenCommand::List),
        "help" | "--help" | "-h" => Ok(KittenCommand::Help {
            name: args.get(1).filter(|arg| !arg.starts_with('-')).cloned(),
        }),
        "manifest" | "--manifest" => Ok(KittenCommand::Manifest {
            name: args.get(1).filter(|arg| !arg.starts_with('-')).cloned(),
        }),
        other if other.starts_with('-') => Err(Error::UnknownCommand(other.to_owned())),
        // kitty permits `kitten NAME ARGS...` as a shorthand for `run`.
        other => Ok(KittenCommand::Run {
            name: other.to_owned(),
            args: args[1..].to_vec(),
            explicit_separator: false,
        }),
    }
}

fn parse_run(args: &[String]) -> Result<KittenCommand> {
    let Some(name) = args.first() else {
        return Err(Error::MissingArgument("NAME"));
    };
    if name == "--" || name.starts_with('-') {
        return Err(Error::MissingArgument("NAME"));
    }
    let mut rest = &args[1..];
    let mut explicit_separator = false;
    if rest.first().is_some_and(|arg| arg == "--") {
        explicit_separator = true;
        rest = &rest[1..];
    }
    Ok(KittenCommand::Run {
        name: name.clone(),
        args: rest.to_vec(),
        explicit_separator,
    })
}

/// Execute a parsed command against a registry.
pub fn execute_command(
    registry: &KittenRegistry,
    command: KittenCommand,
    options: &LaunchOptions,
) -> Result<CommandAction> {
    match command {
        KittenCommand::Run {
            name,
            args,
            explicit_separator,
        } => {
            let entry = registry.resolve(&name)?;
            let mut launch_options = options.clone();
            launch_options.explicit_separator |= explicit_separator;
            Ok(CommandAction::Run(build_launch(
                entry,
                &name,
                &args,
                &launch_options,
            )?))
        }
        KittenCommand::At { args } => Ok(CommandAction::Passthrough(AtInvocation::new(args))),
        KittenCommand::List => Ok(CommandAction::Print(registry.list_text())),
        KittenCommand::Help { name: Some(name) } => {
            Ok(CommandAction::Print(registry.help_text(&name)?))
        }
        KittenCommand::Help { name: None } => Ok(CommandAction::Print(format!(
            "{USAGE}\n\n{}",
            registry.list_text()
        ))),
        KittenCommand::Manifest { name } => Ok(CommandAction::Print(
            registry.manifest_json(name.as_deref())?,
        )),
    }
}

/// Parse and execute in one step.
pub fn execute_args<I, S>(
    registry: &KittenRegistry,
    args: I,
    options: &LaunchOptions,
) -> Result<CommandAction>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    execute_command(registry, parse_kitten_command(args)?, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{DiscoveryConfig, KittenRegistry, KittenSource};
    use crate::manifest::KittenKind;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "kitty-kittens-cli-{}-{}-{}",
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

    fn registry() -> KittenRegistry {
        let dir = TempDir::new("registry");
        fs::write(dir.path.join("demo.py"), "").unwrap();
        let mut registry = KittenRegistry::discover(
            &DiscoveryConfig::empty().with_root(&dir.path, KittenSource::Config),
        );
        let entry = crate::discovery::KittenEntry::new(
            "native",
            dir.path.join("demo.py"),
            KittenKind::Executable,
            KittenSource::System,
        );
        registry.register(entry);
        // Keep the temp dir alive for the lifetime of the process test.
        std::mem::forget(dir);
        registry
    }

    #[test]
    fn parses_run_with_arguments() {
        let command = parse_kitten_command(["run", "demo", "a", "-b", "--long"]).unwrap();
        assert_eq!(
            command,
            KittenCommand::Run {
                name: "demo".to_owned(),
                args: vec!["a".to_owned(), "-b".to_owned(), "--long".to_owned()],
                explicit_separator: false,
            }
        );
    }

    #[test]
    fn consumes_only_the_first_double_dash() {
        let command = parse_kitten_command(["run", "demo", "--", "--flag", "--"]).unwrap();
        assert_eq!(
            command,
            KittenCommand::Run {
                name: "demo".to_owned(),
                args: vec!["--flag".to_owned(), "--".to_owned()],
                explicit_separator: true,
            }
        );
    }

    #[test]
    fn at_is_passed_through_verbatim() {
        let command = parse_kitten_command(["@", "--to", "unix:/tmp/x", "ls", "--", "x"]).unwrap();
        assert_eq!(
            command,
            KittenCommand::At {
                args: vec![
                    "--to".to_owned(),
                    "unix:/tmp/x".to_owned(),
                    "ls".to_owned(),
                    "--".to_owned(),
                    "x".to_owned(),
                ]
            }
        );
    }

    #[test]
    fn parses_list_help_and_manifest_commands() {
        assert_eq!(
            parse_kitten_command(Vec::<String>::new()).unwrap(),
            KittenCommand::List
        );
        assert_eq!(parse_kitten_command(["list"]).unwrap(), KittenCommand::List);
        assert_eq!(
            parse_kitten_command(["help", "demo"]).unwrap(),
            KittenCommand::Help {
                name: Some("demo".to_owned())
            }
        );
        assert_eq!(
            parse_kitten_command(["manifest", "--json"]).unwrap(),
            KittenCommand::Manifest { name: None }
        );
    }

    #[test]
    fn bare_name_is_a_kitty_compatible_run() {
        assert_eq!(
            parse_kitten_command(["demo", "x"]).unwrap(),
            KittenCommand::Run {
                name: "demo".to_owned(),
                args: vec!["x".to_owned()],
                explicit_separator: false,
            }
        );
    }

    #[test]
    fn missing_name_and_unknown_options_are_errors() {
        assert!(matches!(
            parse_kitten_command(["run"]),
            Err(Error::MissingArgument("NAME"))
        ));
        assert!(matches!(
            parse_kitten_command(["run", "--"]),
            Err(Error::MissingArgument("NAME"))
        ));
        assert!(matches!(
            parse_kitten_command(["--bogus"]),
            Err(Error::UnknownCommand(_))
        ));
    }

    #[test]
    fn execute_run_builds_a_launch_plan() {
        let registry = registry();
        let action = execute_command(
            &registry,
            KittenCommand::Run {
                name: "demo".to_owned(),
                args: vec!["x".to_owned()],
                explicit_separator: true,
            },
            &LaunchOptions::isolated(),
        )
        .unwrap();
        let CommandAction::Run(plan) = action else {
            panic!("expected run");
        };
        assert_eq!(plan.kitten_name, "demo");
        assert_eq!(
            plan.argv.last().map(|x| x.to_string_lossy().into_owned()),
            Some("x".to_owned())
        );
        assert_eq!(plan.env_str("KITTY_KITTEN_PASSTHROUGH"), Some("1"));
    }

    #[test]
    fn execute_list_help_manifest_and_passthrough() {
        let registry = registry();
        let list =
            execute_command(&registry, KittenCommand::List, &LaunchOptions::isolated()).unwrap();
        assert!(matches!(list, CommandAction::Print(text) if text.contains("demo")));

        let help = execute_command(
            &registry,
            KittenCommand::Help {
                name: Some("demo".to_owned()),
            },
            &LaunchOptions::isolated(),
        )
        .unwrap();
        assert!(
            matches!(help, CommandAction::Print(text) if text.contains("Usage: kitten run demo"))
        );

        let manifest = execute_command(
            &registry,
            KittenCommand::Manifest {
                name: Some("demo".to_owned()),
            },
            &LaunchOptions::isolated(),
        )
        .unwrap();
        assert!(matches!(manifest, CommandAction::Print(text) if text.contains("\"demo\"")));

        let at = execute_command(
            &registry,
            KittenCommand::At {
                args: vec!["ls".to_owned()],
            },
            &LaunchOptions::isolated(),
        )
        .unwrap();
        assert!(
            matches!(at, CommandAction::Passthrough(ref invocation) if invocation.command() == Some("ls"))
        );
    }

    #[test]
    fn missing_kitten_is_reported() {
        let registry = registry();
        let err = execute_command(
            &registry,
            KittenCommand::Run {
                name: "missing".to_owned(),
                args: Vec::new(),
                explicit_separator: false,
            },
            &LaunchOptions::isolated(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::NotFound { .. }));
    }
}
