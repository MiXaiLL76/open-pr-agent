use std::ffi::OsString;
use std::process::ExitCode;

// The native CI pipeline handles the recorded CLI shapes; the wired units
// take what clap recognizes; anything else is a CLI error.
fn main() -> ExitCode {
    let argv: Vec<OsString> = std::env::args_os().collect();
    pr_agent::pipeline_try_run(&argv[1..])
}
