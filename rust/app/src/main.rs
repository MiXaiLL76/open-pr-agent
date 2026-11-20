use std::ffi::OsString;
use std::process::ExitCode;

// The native CI pipeline handles the recorded CLI shapes; the wired units
// take what clap recognizes; anything else is a CLI error.
fn main() -> ExitCode {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == pr_agent::review_mcp::MODE)
    {
        return tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("MCP runtime")
            .block_on(pr_agent::review_mcp::serve_if_requested())
            .expect("MCP mode");
    }
    let argv: Vec<OsString> = std::env::args_os().collect();
    pr_agent::pipeline_try_run(&argv[1..])
}
