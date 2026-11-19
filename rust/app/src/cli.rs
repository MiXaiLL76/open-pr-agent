use std::ffi::OsString;

use clap::{Arg, Command, value_parser};

/// The command `units::dispatch` gets when argv names no subcommand.
pub const ROOT: &str = "pr_agent";

/// The commands the original's argparse accepted.
pub const COMMANDS: &[&str] = &[
    "review",
    "review_pr",
    "describe",
    "describe_pr",
    "improve",
    "improve_code",
    "ask",
    "ask_question",
    "ask_line",
    "update_changelog",
    "config",
    "settings",
    "help",
    "similar_issue",
    "add_docs",
    "generate_labels",
    "doctor",
];

fn rest() -> Arg {
    Arg::new("args")
        .num_args(0..)
        .trailing_var_arg(true)
        .allow_hyphen_values(true)
        .value_parser(value_parser!(OsString))
}

/// Routes argv to a command. Anything this parser rejects still reaches the
/// native pipeline first (it owns the recorded shapes); this layer exists for
/// the wired units, so it never has to match every way the original accepts
/// its arguments.
pub fn command() -> Command {
    let mut app = Command::new(ROOT)
        .disable_help_flag(true)
        .disable_version_flag(true)
        .disable_help_subcommand(true)
        .arg(rest());
    for name in COMMANDS {
        app = app.subcommand(Command::new(*name).arg(rest()));
    }
    app
}
