//! The opencode agent engine, replacing pr-agent's `algo/*` compensation machinery.
//!
//! Per `plan-opencode-engine.md` (option C): pr-agent stays the transport and
//! publisher; the agent loop, context and `/ask` dialogue history are delegated
//! to `opencode run`. No diff packing, token budgeting or chunking happens here —
//! the agent reads the repository worktree itself.

// The command pipeline that drives this engine is being ported unit by unit;
// until dispatch calls it, allow the scaffolding to exist unused.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Default settings file baked into the binary at build time.
const DEFAULT_SETTINGS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/settings/opencode_settings.toml"
));

/// Settings under `[opencode]` (pr_agent/settings/opencode_settings.toml, env `OPENCODE__*`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenCodeSettings {
    /// Path or name of the opencode binary.
    #[serde(default = "default_binary")]
    pub binary: String,
    /// The agent definition opencode runs (`pr-reviewer`, `pr-describer`, ...).
    #[serde(default = "default_agent")]
    pub agent: String,
    /// Empty = the model configured in opencode itself.
    #[serde(default)]
    pub model: String,
    /// Wall-clock budget for one `opencode run` invocation.
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    /// Prefix for stable `/ask` session ids (`pr-<repo>-<n>`).
    #[serde(default = "default_prefix")]
    pub ask_session_prefix: String,
    /// Include the changed-file map in the prompt (can be disabled).
    #[serde(default = "default_true")]
    pub file_map_in_prompt: bool,
}

/// The agent name the engine passes with `--agent`.
const DEFAULT_AGENT_NAME: &str = "pr-reviewer";

fn default_binary() -> String {
    "opencode".into()
}
fn default_agent() -> String {
    DEFAULT_AGENT_NAME.into()
}
fn default_timeout() -> u64 {
    900
}
fn default_prefix() -> String {
    "pr".into()
}
fn default_true() -> bool {
    true
}

impl Default for OpenCodeSettings {
    fn default() -> Self {
        serde_json::from_value(serde_json::json!({})).unwrap_or_else(|_| OpenCodeSettings {
            binary: default_binary(),
            agent: default_agent(),
            model: String::new(),
            timeout_seconds: default_timeout(),
            ask_session_prefix: default_prefix(),
            file_map_in_prompt: true,
        })
    }
}

/// Cost and token usage of one agent run, mirrored into run details the way the
/// litellm handler reports them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunUsage {
    pub cost: Option<f64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

/// The final answer of one `opencode run` invocation.
#[derive(Debug, Clone, Default)]
pub struct AgentReply {
    /// The concatenated text parts of the session.
    pub text: String,
    /// Usage from the step-finish events, when reported.
    pub usage: RunUsage,
}

/// One row of a changed-file map: what the prompt carries instead of `{{ diff }}`.
#[derive(Debug, Clone, Serialize)]
pub struct FileMapEntry {
    pub path: String,
    pub old_path: Option<String>,
    pub edit_type: String,
    pub plus_lines: u64,
    pub minus_lines: u64,
}

/// Errors the handler raises; the strings keep the `Kind: message` shape the
/// bindings and callers already understand.
#[derive(Debug)]
pub struct OpenCodeError(pub String);

impl std::fmt::Display for OpenCodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for OpenCodeError {}

impl OpenCodeError {
    fn runtime(message: impl Into<String>) -> Self {
        Self(format!("RuntimeError: {}", message.into()))
    }
}

/// The opencode-backed AI handler.
pub struct OpenCodeHandler {
    pub settings: OpenCodeSettings,
    /// Working tree the agent reads; prepared by `prepare_worktree`.
    pub worktree_path: Option<PathBuf>,
}

impl OpenCodeHandler {
    pub fn new(settings: OpenCodeSettings) -> Self {
        Self {
            settings,
            worktree_path: None,
        }
    }

    /// Stable session id for `/ask`: `ses_pr-<owner>-<repo>-<n>`, so the
    /// dialogue history lives in the opencode session across runs. opencode
    /// validates ids start with `ses_`.
    pub fn session_for_ask(&self, repo_full_name: &str, pr_number: u64) -> String {
        let repo = repo_full_name.replace('/', "-");
        format!(
            "ses_{}-{}-{}",
            self.settings.ask_session_prefix, repo, pr_number
        )
    }

    /// Fresh session id for idempotent review/describe runs on the same head SHA:
    /// re-running the same push replays the same session.
    pub fn session_for_head(&self, head_sha: &str) -> String {
        format!("ses_{}_head-{}", self.settings.ask_session_prefix, head_sha)
    }

    /// Materialize the PR head as a git worktree the agent reads: a temp repo
    /// with every changed file written at its head content and one commit.
    /// The merge ref the reviewer must see arrives as file contents, so no
    /// network fetch happens here — the transport already fetched them.
    pub fn prepare_worktree_from_files(
        &mut self,
        pr_number: u64,
        head_sha: &str,
        files: &[(String, String)],
    ) -> Result<(), OpenCodeError> {
        let base = worktree_dir(&format!(
            "pr-{pr_number}-{}",
            &head_sha[..head_sha.len().min(8)]
        ));
        init_worktree(&base)?;
        for (path, content) in files {
            let target = base.join(path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| OpenCodeError::runtime(format!("cannot create {path}: {e}")))?;
            }
            std::fs::write(&target, content)
                .map_err(|e| OpenCodeError::runtime(format!("cannot write {path}: {e}")))?;
        }
        commit_worktree(&base, &format!("PR {pr_number} head {head_sha}"))?;
        self.worktree_path = Some(base);
        Ok(())
    }

    /// Materialize a plain diff as a git worktree: init a repo and apply the
    /// patch (new files apply cleanly to an empty tree), then commit.
    pub fn prepare_worktree_from_diff(&mut self, diff_text: &str) -> Result<(), OpenCodeError> {
        use std::io::Write;

        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        sha2::Digest::update(&mut hasher, diff_text.as_bytes());
        let digest = sha2::Digest::finalize(hasher);
        let tag = digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let base = worktree_dir(&format!("local-diff-{}", &tag[..12]));
        init_worktree(&base)?;
        let mut child = Command::new("git")
            .current_dir(&base)
            .args(["apply", "--whitespace=nowarn", "-"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| OpenCodeError::runtime(format!("git apply failed to start: {e}")))?;
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(diff_text.as_bytes());
        }
        drop(child.stdin.take());
        let status = child
            .wait()
            .map_err(|e| OpenCodeError::runtime(format!("git apply failed: {e}")))?;
        if !status.success() {
            return Err(OpenCodeError::runtime("git apply rejected the diff"));
        }
        commit_worktree(&base, "local diff")?;
        self.worktree_path = Some(base);
        Ok(())
    }

    /// Review the process working directory as the worktree: a CI checkout
    /// at the PR head. The caller guarantees the directory is a git root, so
    /// no repo is materialized and nothing is committed — the agent reads the
    /// checkout as-is, read-only.
    pub fn use_current_worktree(&mut self) -> Result<(), OpenCodeError> {
        let cwd = std::env::current_dir().map_err(|e| {
            OpenCodeError::runtime(format!("cannot resolve the working directory: {e}"))
        })?;
        install_packaged_agent(&cwd);
        self.worktree_path = Some(cwd);
        Ok(())
    }

    /// The doctor ping: a one-shot probe in a scratch repo, no PR context.
    pub fn ping(&self) -> Result<(), OpenCodeError> {
        let base = worktree_dir("doctor-ping");
        init_worktree(&base)?;
        let mut command = opencode_command(&self.settings.binary);
        command
            .current_dir(&base)
            .args(["run", "--format", "json", "--archived"])
            .arg("Reply with exactly: ok");
        if !self.settings.model.is_empty() {
            command.arg("--model").arg(&self.settings.model);
        }
        let output = run_with_timeout(command, Duration::from_secs(self.settings.timeout_seconds))
            .map_err(|e| OpenCodeError::runtime(format!("failed to run opencode: {e}")))?;
        match output {
            None => Err(OpenCodeError::runtime(
                "the ping exceeded the timeout and was killed",
            )),
            Some(output) if !output.status.success() => Err(OpenCodeError::runtime(format!(
                "opencode run failed with status {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ))),
            Some(output) => {
                parse_agent_reply(&output.stdout)?;
                Ok(())
            }
        }
    }

    /// Run the agent once: `opencode run --agent <agent> --format json`.
    ///
    /// `session_id` keeps the run idempotent for review/describe and stateful
    /// for `/ask`. The prompt is the task plus the YAML output contract plus
    /// (optionally) the changed-file map — never the diff itself.
    ///
    /// No `--auto` is passed, ever: the agent runs fail-closed inside its
    /// permissions (read-only tools, no network), so it cannot ask for and
    /// receive anything dangerous; a permission denial answers itself instead
    /// of hanging the run waiting for a human. The wall-clock timeout is the
    /// last line of defense and kills the run.
    pub fn chat_completion(
        &self,
        prompt: &str,
        session_id: &str,
        file_map: &[FileMapEntry],
    ) -> Result<AgentReply, OpenCodeError> {
        let worktree = self.worktree_path.as_deref().ok_or_else(|| {
            OpenCodeError::runtime("worktree not prepared; call prepare_worktree first")
        })?;

        let full_prompt = compose_prompt(prompt, file_map, self.settings.file_map_in_prompt);

        let mut command = opencode_command(&self.settings.binary);
        command
            .current_dir(worktree)
            .arg("run")
            .arg("--agent")
            .arg(&self.settings.agent)
            .arg("--format")
            .arg("json")
            .arg("--title")
            .arg(session_id)
            .arg("--session")
            .arg(session_id)
            .arg("--archived")
            .arg(&full_prompt);
        if !self.settings.model.is_empty() {
            command.arg("--model").arg(&self.settings.model);
        }

        let output = run_with_timeout(command, Duration::from_secs(self.settings.timeout_seconds))
            .map_err(|e| OpenCodeError::runtime(format!("failed to run opencode: {e}")))?;
        match output {
            None => Err(OpenCodeError::runtime(format!(
                "opencode run exceeded the {}s timeout and was killed",
                self.settings.timeout_seconds
            ))),
            Some(output) if !output.status.success() => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(OpenCodeError::runtime(format!(
                    "opencode run failed with status {}: {}",
                    output.status,
                    stderr.trim()
                )))
            }
            Some(output) => parse_agent_reply(&output.stdout),
        }
    }
}

/// Build the spawn for the configured binary. Sandboxes that copy fixture
/// files can drop the execute bit; a `.py` binary then re-spawns through
/// `python3`, so the same case file runs under every harness.
/// An interpreter that actually runs: a stale virtualenv shim on `PATH` can
/// point at a deleted interpreter, so probe the candidates once and keep the
/// first one that answers `--version`.
fn python_interpreter() -> &'static str {
    static INTERPRETER: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    INTERPRETER.get_or_init(|| {
        for candidate in ["python3", "/usr/bin/python3", "python"] {
            let works = Command::new(candidate)
                .arg("--version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            if works {
                return candidate.to_string();
            }
        }
        "python3".to_string()
    })
}

fn opencode_command(binary: &str) -> Command {
    let mut direct = Command::new(binary);
    if binary.ends_with(".py") && !is_executable(binary) {
        direct = Command::new(python_interpreter());
        direct.arg(binary);
    }
    direct
}

/// Whether a file carries any execute bit (Unix).
fn is_executable(path: &str) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Run a command to completion, killing it after `timeout`. `None` means the
/// timeout fired. Polling keeps this dependency-free and portable.
fn run_with_timeout(
    mut command: Command,
    timeout: Duration,
) -> Result<Option<std::process::Output>, String> {
    use std::io::Read;

    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = vec![];
                let mut stderr = vec![];
                if let Some(mut pipe) = child.stdout.take() {
                    let _ = pipe.read_to_end(&mut stdout);
                }
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = pipe.read_to_end(&mut stderr);
                }
                return Ok(Some(std::process::Output {
                    status,
                    stdout,
                    stderr,
                }));
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// The prompt the agent receives: the task, then the changed-file map (a map,
/// not the diff content — the agent reads the tree itself).
pub fn compose_prompt(task: &str, file_map: &[FileMapEntry], include_map: bool) -> String {
    let mut full_prompt = task.to_string();
    if include_map && !file_map.is_empty() {
        full_prompt.push_str(
            "\n\nChanged files (read them with your tools; the full diff is available via `git diff`):\n",
        );
        for entry in file_map {
            match &entry.old_path {
                Some(old) => full_prompt.push_str(&format!(
                    "- {} -> {} ({}): +{}/-{}\n",
                    old, entry.path, entry.edit_type, entry.plus_lines, entry.minus_lines
                )),
                None => full_prompt.push_str(&format!(
                    "- {} ({}): +{}/-{}\n",
                    entry.path, entry.edit_type, entry.plus_lines, entry.minus_lines
                )),
            }
        }
    }
    full_prompt
}

fn run_git(
    dir: &Path,
    args: impl IntoIterator<Item = impl AsRef<std::ffi::OsStr>>,
) -> Result<(), String> {
    let status = Command::new("git")
        .current_dir(dir)
        .args(args)
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("exit status {status}"))
    }
}

/// A fresh worktree directory under the process temp root. A per-call
/// counter keeps concurrent runs (parallel CLI cases, server requests) from
/// sharing a path: one run's cleanup must not delete another's worktree.
/// The packaged `pr-reviewer` agent: read-only (edit/webfetch denied), bash
/// allowed for `git diff` and file reads. Installed into every worktree the
/// engine prepares, so the policy holds without global opencode config.
const PACKAGED_PR_REVIEWER_AGENT: &str = include_str!("../settings/agents/pr-reviewer.md");

/// Write the packaged agent into a worktree's `.opencode/agents/`, keeping
/// any agent definition already there (a repository may ship its own).
fn install_packaged_agent(base: &Path) {
    let agents_dir = base.join(".opencode").join("agents");
    let target = agents_dir.join(format!("{}.md", DEFAULT_AGENT_NAME));
    if target.exists() {
        return;
    }
    if std::fs::create_dir_all(&agents_dir).is_ok() {
        let _ = std::fs::write(&target, PACKAGED_PR_REVIEWER_AGENT);
    }
}

fn worktree_dir(tag: &str) -> PathBuf {
    static CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let unique = format!("{tag}-{}-{call}", std::process::id());
    let base = std::env::temp_dir().join("pr-agent-worktrees").join(unique);
    let _ = std::fs::remove_dir_all(&base);
    base
}

/// Init an empty git repo the agent can read.
fn init_worktree(base: &Path) -> Result<(), OpenCodeError> {
    std::fs::create_dir_all(base)
        .map_err(|e| OpenCodeError::runtime(format!("cannot create worktree dir: {e}")))?;
    run_git(base, ["init", "--quiet"])
        .map_err(|e| OpenCodeError::runtime(format!("git init failed: {e}")))
}

/// Stage everything and commit once, so `git diff` shows the change set.
fn commit_worktree(base: &Path, message: &str) -> Result<(), OpenCodeError> {
    run_git(base, ["add", "-A"])
        .map_err(|e| OpenCodeError::runtime(format!("git add failed: {e}")))?;
    run_git(
        base,
        [
            "-c",
            "user.email=pr-agent@localhost",
            "-c",
            "user.name=pr-agent",
            "commit",
            "--quiet",
            "-m",
            message,
        ],
    )
    .map_err(|e| OpenCodeError::runtime(format!("git commit failed: {e}")))
}

/// Whether the configured opencode binary can run: a path separator means a
/// file path, anything else is looked up on `PATH`.
pub fn binary_available(settings: &OpenCodeSettings) -> bool {
    let binary = &settings.binary;
    if binary.contains('/') {
        return std::path::Path::new(binary).is_file();
    }
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(binary).is_file()))
        .unwrap_or(false)
}

/// Parse the JSONL stream `opencode run --format json` prints: one event per
/// line, the payload under `part`. `text` events concatenate into the answer,
/// the last `step-finish` carries cost and tokens.
pub fn parse_agent_reply(stdout: &[u8]) -> Result<AgentReply, OpenCodeError> {
    let text = String::from_utf8_lossy(stdout);
    let mut reply = AgentReply::default();
    for line in text.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue; // non-JSON chatter on stdout is ignored
        };
        // The stream spells the part type at the top level with underscores
        // ("step_finish") and inside the part with dashes ("step-finish");
        // match both, then read the payload from `part`.
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .or_else(|| event.pointer("/part/type").and_then(Value::as_str))
            .map(|kind| kind.replace('_', "-"));
        let part = event.get("part").unwrap_or(&event);
        match event_type.as_deref() {
            Some("text") | Some("text-delta") => {
                if let Some(part) = part.get("text").and_then(Value::as_str) {
                    reply.text.push_str(part);
                }
            }
            Some("step-finish") => {
                // One number at the top level of the part, not a /cost/total path.
                if let Some(cost) = part.get("cost").and_then(Value::as_f64) {
                    reply.usage.cost = Some(cost);
                }
                if let Some(tokens) = part.get("tokens") {
                    reply.usage.input_tokens = tokens.get("input").and_then(Value::as_u64);
                    reply.usage.output_tokens = tokens.get("output").and_then(Value::as_u64);
                }
            }
            Some("error") => {
                let message = event
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown opencode error");
                return Err(OpenCodeError::runtime(message));
            }
            _ => {}
        }
    }
    if reply.text.trim().is_empty() {
        return Err(OpenCodeError::runtime(
            "opencode returned an empty response",
        ));
    }
    Ok(reply)
}

impl OpenCodeSettings {
    /// Load settings from the packaged defaults, an optional repository
    /// `.pr_agent.toml`, and `OPENCODE__*` environment overrides, in that order.
    /// A setting may arrive as a TOML list, a plain string or an env value
    /// Dynaconf would parse as TOML; scalars here are read tolerantly.
    pub fn load(repo_root: Option<&Path>) -> Self {
        let mut merged = toml_defaults();
        if let Some(root) = repo_root {
            let repo_file = root.join(".pr_agent.toml");
            if let Ok(text) = std::fs::read_to_string(&repo_file)
                && let Some(data) = parse_toml_section(&text)
            {
                merge_section(&mut merged, &data);
            }
        }
        // Environment overrides: OPENCODE__KEY (Dynaconf spells SECTION__KEY).
        for field in ["binary", "agent", "model", "ask_session_prefix"] {
            if let Ok(value) = std::env::var(format!("OPENCODE__{}", field.to_uppercase())) {
                merged.insert(field.to_string(), Value::String(value));
            }
        }
        // A relative binary path (bare name or ./script) resolves against the
        // working directory the process started in: the agent later runs with
        // the worktree as cwd, where the same relative path would not resolve.
        if let Some(Value::String(binary)) = merged.get("binary")
            && !binary.is_empty()
            && !std::path::Path::new(binary).is_absolute()
            && let Ok(absolute) = std::fs::canonicalize(binary)
        {
            merged.insert(
                "binary".into(),
                Value::String(absolute.to_string_lossy().into_owned()),
            );
        }
        if let Ok(value) = std::env::var("OPENCODE__TIMEOUT_SECONDS")
            && let Ok(number) = value.parse::<u64>()
        {
            merged.insert("timeout_seconds".to_string(), serde_json::json!(number));
        }
        if let Ok(value) = std::env::var("OPENCODE__FILE_MAP_IN_PROMPT") {
            let flag = matches!(value.to_lowercase().as_str(), "true" | "1" | "yes");
            merged.insert("file_map_in_prompt".into(), serde_json::json!(flag));
        }
        serde_json::from_value(Value::Object(merged)).unwrap_or_default()
    }
}

/// The `[opencode]` section of the packaged defaults as a JSON object.
fn toml_defaults() -> serde_json::Map<String, Value> {
    parse_toml_section(DEFAULT_SETTINGS).unwrap_or_default()
}

/// Parse the `[opencode]` table of a TOML document into a JSON object.
fn parse_toml_section(text: &str) -> Option<serde_json::Map<String, Value>> {
    let parsed: toml::Value = toml::from_str(text).ok()?;
    let section = parsed.get("opencode")?.as_table()?;
    let json = serde_json::to_value(section).ok()?;
    json.as_object().cloned()
}

/// Merge a section into the settings map; overlapping values are replaced,
/// never appended, matching the repository's configuration-file semantics.
fn merge_section(
    target: &mut serde_json::Map<String, Value>,
    source: &serde_json::Map<String, Value>,
) {
    for (key, value) in source {
        target.insert(key.clone(), value.clone());
    }
}

/// The wall-clock timeout of one run, as a `Duration`.
impl OpenCodeSettings {
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_seconds)
    }
}

impl Default for OpenCodeHandler {
    fn default() -> Self {
        Self::new(OpenCodeSettings::default())
    }
}

/// `binary_available` without the settings: a bare name on `PATH`.
fn binary_available_path(binary: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(binary).is_file()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The environment is process-global; settings tests share this lock so
    /// they never observe each other's overrides.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn parse_jsonl_reply() {
        // The real v2.3 stream: payload under `part`, types at both levels.
        let stream = concat!(
            "{\"type\":\"step_start\",\"sessionID\":\"ses_x\",\"part\":{\"type\":\"step-start\"}}\n",
            "{\"type\":\"text\",\"sessionID\":\"ses_x\",\"part\":{\"type\":\"text\",\"text\":\"YAML answer part 1\\n\"}}\n",
            "{\"type\":\"step_finish\",\"sessionID\":\"ses_x\",\"part\":{\"type\":\"step-finish\",\"cost\":0.42,\"tokens\":{\"input\":100,\"output\":50}}}\n",
            "{\"type\":\"text\",\"sessionID\":\"ses_x\",\"part\":{\"type\":\"text\",\"text\":\"part 2\"}}\n",
        );
        let reply = parse_agent_reply(stream.as_bytes()).unwrap();
        assert_eq!(reply.text, "YAML answer part 1\npart 2");
        assert_eq!(reply.usage.cost, Some(0.42));
        assert_eq!(reply.usage.input_tokens, Some(100));
        assert_eq!(reply.usage.output_tokens, Some(50));
    }

    #[test]
    fn error_event_fails_the_run() {
        let stream = "{\"type\":\"error\",\"sessionID\":\"\",\"error\":{\"type\":\"unknown\",\"message\":\"Agent not found: x\"}}\n";
        let err = parse_agent_reply(stream.as_bytes()).unwrap_err();
        assert!(err.0.contains("Agent not found"));
    }

    #[test]
    fn empty_reply_is_an_error() {
        assert!(parse_agent_reply(b"{} no events\n").is_err());
    }

    #[test]
    fn ask_session_is_stable() {
        let handler = OpenCodeHandler::default();
        // opencode validates that session ids start with `ses_`.
        assert_eq!(
            handler.session_for_ask("owner/repo", 54),
            "ses_pr-owner-repo-54"
        );
        assert_eq!(handler.session_for_head("abc123"), "ses_pr_head-abc123");
    }

    #[test]
    fn settings_load_from_packaged_defaults() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let settings = OpenCodeSettings::load(None);
        assert_eq!(settings.binary, "opencode");
        assert_eq!(settings.agent, "pr-reviewer");
        assert_eq!(settings.timeout_seconds, 900);
        assert!(settings.file_map_in_prompt);
        assert_eq!(settings.model, "");
    }

    #[test]
    fn settings_env_overrides() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // set_var/remove_var are unsafe in edition 2024; exercise them once here.
        // SAFETY: single-threaded test, no other thread reads the environment.
        unsafe {
            std::env::set_var("OPENCODE__TIMEOUT_SECONDS", "60");
            std::env::set_var("OPENCODE__AGENT", "pr-describer");
        }
        let settings = OpenCodeSettings::load(None);
        // SAFETY: same single-threaded test teardown.
        unsafe {
            std::env::remove_var("OPENCODE__TIMEOUT_SECONDS");
            std::env::remove_var("OPENCODE__AGENT");
        }
        assert_eq!(settings.timeout_seconds, 60);
        assert_eq!(settings.agent, "pr-describer");
    }
}
