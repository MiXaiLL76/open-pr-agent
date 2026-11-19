//! Glue from the front door to the command layer. The wired `agent-rs` crate
//! owns command resolution (the ported registry: aliases, check-run names,
//! comment-id commands); the native CI pipeline owns execution.

use std::ffi::OsString;
use std::process::ExitCode;

use clap::ArgMatches;

pub fn run(command: &str, _matches: &ArgMatches) -> ExitCode {
    // The registry resolves the canonical command through the typed spec
    // (aliases like review_pr fold onto review); an unknown name reaches the
    // pipeline as-is, which routes it to the recorded argparse error.
    let _spec = agent_rs::port::resolve_command_typed(command);
    let argv: Vec<OsString> = std::env::args_os().skip(1).collect();
    match crate::pipeline::try_run_native(&argv) {
        Some(code) => code,
        None => crate::unsupported_arguments(&argv),
    }
}
