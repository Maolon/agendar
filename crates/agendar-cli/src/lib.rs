#![forbid(unsafe_code)]

//! Agendar CLI library: argument parsing, JSON envelopes and command dispatch.

pub mod args;
pub mod board;
pub mod commands;
pub mod delivery;
pub mod diff;
pub mod hooks;
pub mod mcp;
pub mod output;

use clap::Parser;
use clap::error::ErrorKind;

use agendar_core::error::{AgendarError, ErrorCode};

/// Parse one invocation and return the process exit code.
pub fn run() -> i32 {
    let cli = match args::Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => match error.kind() {
            // `--help` and `--version` are the documented text-only exceptions.
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
                let _ = error.print();
                return 0;
            }
            _ => {
                // Every other parse failure becomes the stable E_USAGE envelope,
                // never clap's default prose.
                let hint = error.to_string().replace('\n', " ");
                let usage = AgendarError::new(
                    ErrorCode::Usage,
                    "Invalid argument; the command line could not be parsed.",
                )
                .with_field("/args")
                .with_detail("cli_parse")
                .with_hint(hint);
                return output::error(&usage);
            }
        },
    };
    commands::dispatch(cli)
}
