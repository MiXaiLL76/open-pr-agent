//! Command dispatch from the front door to the pipeline.

use std::process::ExitCode;

use clap::ArgMatches;

mod agent;

pub fn dispatch(command: &str, matches: &ArgMatches) -> Option<ExitCode> {
    match command {
        "review" | "review_pr" | "describe" | "describe_pr" | "improve" | "improve_code"
        | "ask" | "ask_question" | "ask_line" | "update_changelog" | "config" | "settings"
        | "help" | "similar_issue" | "add_docs" | "generate_labels" | "doctor" => {
            Some(agent::run(command, matches))
        }
        _ => None,
    }
}
