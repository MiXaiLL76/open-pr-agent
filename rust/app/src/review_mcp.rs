//! Receive a validated review from OpenCode over a local stdio MCP connection.

use std::io::Write;
use std::path::{Path, PathBuf};

use rmcp::{
    ServiceExt, handler::server::common::schema_for_input, handler::server::wrapper::Parameters,
    tool, tool_router, transport::stdio,
};
use serde_json::Value;

use crate::output_models::PrReview;

pub const MODE: &str = "--internal-review-mcp";

#[derive(Clone)]
struct ReviewResultServer {
    destination: PathBuf,
}

#[tool_router(server_handler)]
impl ReviewResultServer {
    #[tool(
        description = "Submit the complete final PR review, with an empty findings list if there are no issues",
        input_schema = schema_for_input::<PrReview>().expect("valid review tool schema")
    )]
    fn save_result(&self, Parameters(arguments): Parameters<Value>) -> Result<String, String> {
        record(
            &self.destination,
            "INFO",
            "MCP tool pr_result_save_result called.",
        );
        let review = serde_json::from_value::<PrReview>(arguments).map_err(|error| {
            record(
                &self.destination,
                "WARN",
                "Rejected pr_result_save_result: arguments failed review schema validation.",
            );
            error.to_string()
        })?;
        self.save(review).inspect_err(|_| {
            record(
                &self.destination,
                "WARN",
                "pr_result_save_result could not save the review.",
            );
        })?;
        record(
            &self.destination,
            "INFO",
            "pr_result_save_result accepted and saved the review.",
        );
        Ok("Review saved.".into())
    }
}

impl ReviewResultServer {
    fn save(&self, review: PrReview) -> Result<(), String> {
        let mut file = tempfile::NamedTempFile::new_in(
            self.destination
                .parent()
                .ok_or("Missing review result directory")?,
        )
        .map_err(|error| error.to_string())?;
        let mut review = serde_json::to_value(review).map_err(|error| error.to_string())?;
        if let Some(fields) = review["review"].as_object_mut() {
            fields.retain(|_, value| !value.is_null());
        }
        serde_json::to_writer(file.as_file_mut(), &review).map_err(|error| error.to_string())?;
        file.persist_noclobber(&self.destination)
            .map_err(|error| error.error.to_string())?;
        Ok(())
    }
}

fn log_path(destination: &Path) -> PathBuf {
    destination.with_extension("mcp.log")
}

// OpenCode owns the child's stdout and stderr. Relay diagnostics separately
// so the caller sees them even when the CLI suppresses failed tool events.
fn record(destination: &Path, level: &str, message: &str) {
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path(destination))
    {
        let _ = file.write_all(format!("{level}\t{message}\n").as_bytes());
    }
}

pub(crate) fn relay_diagnostics(destination: &Path, session: &str) {
    use crate::logging::{Loc, info, warning};
    let path = log_path(destination);
    let loc = Loc {
        module: "review_mcp",
        func: "submit",
    };
    match std::fs::read_to_string(&path) {
        Ok(log) => {
            for (level, message) in log.lines().filter_map(|line| line.split_once('\t')) {
                let message = format!("[{session}] {message}");
                if level == "INFO" {
                    info(&loc, &message);
                } else {
                    warning(&loc, &message);
                }
            }
            let _ = std::fs::remove_file(path);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            warning(
                &loc,
                &format!("[{session}] OpenCode did not start the review MCP server."),
            );
        }
        Err(error) => warning(
            &loc,
            &format!("[{session}] Could not read review MCP diagnostics: {error}"),
        ),
    }
}

/// Handle the private subprocess entry point before normal CLI or webhook startup.
pub async fn serve_if_requested() -> Option<std::process::ExitCode> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new(MODE)) {
        return None;
    }
    let Some(destination) = args.next().filter(|_| args.next().is_none()) else {
        return Some(std::process::ExitCode::FAILURE);
    };
    let server = ReviewResultServer {
        destination: PathBuf::from(destination),
    };
    let destination = server.destination.clone();
    record(
        &destination,
        "INFO",
        "Starting review MCP server over stdio.",
    );
    Some(match server.serve(stdio()).await {
        Ok(service) => {
            record(
                &destination,
                "INFO",
                "OpenCode connected to the review MCP server.",
            );
            let _ = service.waiting().await;
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            record(
                &destination,
                "WARN",
                "Review MCP session initialization failed.",
            );
            eprintln!("review_mcp:serve {error}");
            std::process::ExitCode::FAILURE
        }
    })
}

pub(crate) fn engine_config(destination: &Path) -> std::io::Result<Value> {
    Ok(serde_json::json!({
        "mcp": {"servers": {"pr_result": {
            "type": "local",
            "codemode": false,
            "command": [std::env::current_exe()?, MODE, destination],
        }}},
    }))
}
