//! The `pr_agent` library: the CLI pipeline, transports and the engine the
//! `pr_agent` binary and the `pr_agent_server` webhook server share.

pub mod blobs;
pub mod cli;
pub mod commands;
pub mod github;
pub mod gitlab;
pub mod logging;
pub mod opencode;
pub mod output_models;
pub mod pipeline;
pub mod render;
pub mod settings;
pub mod units;

use std::ffi::OsString;
use std::process::ExitCode;

/// Run the CLI pipeline on an argv (without the program name), like the
/// `pr_agent` front door does. Returns the exit code; shapes the pipeline
/// does not recognize fall through to the argument error report.
pub fn pipeline_try_run(argv: &[OsString]) -> ExitCode {
    pipeline::try_run_native(argv).unwrap_or_else(|| unsupported_arguments(argv))
}

/// The recorded unrecognized-arguments report the original's argparse
/// printed for anything it could not parse.
pub fn unsupported_arguments(argv: &[OsString]) -> ExitCode {
    let words: Vec<String> = argv
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    eprintln!(
        "{}pr-agent: error: unrecognized arguments: {}",
        blobs::USAGE,
        words.join(" ")
    );
    ExitCode::from(2)
}
