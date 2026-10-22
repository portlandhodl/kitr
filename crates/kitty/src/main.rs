// SPDX-License-Identifier: GPL-3.0-or-later
use std::process::ExitCode;

fn main() -> ExitCode {
    match kitty::run(std::env::args_os().skip(1)) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("kitr: {err}");
            ExitCode::from(2)
        }
    }
}
