//! The opencode agent engine: pr-agent stays the transport and publisher;
//! the agent loop, context and `/ask` dialogue history run in `opencode run`.
//! No diff packing, token budgeting or chunking happens here — the agent
//! reads the repository worktree itself.
//!
//! Engine configuration: the user's config wins (see `resolve_engine_env`).
//! A project may ship `.open-pr-agent/opencode.jsonc` — MCP servers, extra
//! agents, anything opencode understands, minus providers/models: those are
//! injected from the PR_AGENT_LLM_* environment when the review needs its
//! own model endpoint.

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
    /// The concatenated text parts of the final assistant message.
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

    /// Keep the provider diff next to the changed files without overwriting
    /// a file from the PR with the same name.
    pub fn write_review_diff(&self, diff: &str) -> Result<String, OpenCodeError> {
        use std::io::{ErrorKind, Write};

        let base = self.worktree_path.as_deref().ok_or_else(|| {
            OpenCodeError::runtime("worktree not prepared; call prepare_worktree first")
        })?;
        for suffix in 0.. {
            let name = if suffix == 0 {
                ".pr-agent-review.diff".to_string()
            } else {
                format!(".pr-agent-review-{suffix}.diff")
            };
            let mut file = match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(base.join(&name))
            {
                Ok(file) => file,
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(OpenCodeError::runtime(format!(
                        "cannot create review diff: {error}"
                    )));
                }
            };
            file.write_all(diff.as_bytes()).map_err(|error| {
                OpenCodeError::runtime(format!("cannot write review diff: {error}"))
            })?;
            return Ok(name);
        }
        unreachable!()
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
        self.worktree_path = Some(cwd);
        Ok(())
    }

    /// The doctor ping: a one-shot probe in a scratch repo, no PR context.
    pub fn ping(&self) -> Result<(), OpenCodeError> {
        let base = worktree_dir("doctor-ping");
        init_worktree(&base)?;
        let mut command = opencode_command(&self.settings.binary);
        // The engine config the user chose (or the env-injected one).
        let engine_config = resolve_engine_env()?;
        if !engine_config.is_empty() {
            command.env("OPENCODE_CONFIG", engine_config);
        }
        command
            .current_dir(&base)
            .args(["run", "--format", "json", "--archived", "--agent"])
            .arg(&self.settings.agent)
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
            Some(output) if !output.status.success() => Err(engine_failure(&output)),
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
        // The engine config the user chose (or the env-injected one).
        let engine_config = resolve_engine_env()?;
        if !engine_config.is_empty() {
            command.env("OPENCODE_CONFIG", engine_config);
        }
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
            Some(output) if !output.status.success() => Err(engine_failure(&output)),
            Some(output) => parse_agent_reply(&output.stdout),
        }
    }
}

/// The model+provider env the review carries, if any.
struct LlmEnv {
    base_url: String,
    api_key: String,
    /// `provider/model`, the engine spelling.
    model: String,
}

/// Read the LLM endpoint from the environment. All three values or none:
/// a partial set is a deployment error reported by `engine_env` callers.
fn llm_env() -> Option<LlmEnv> {
    let base_url = std::env::var("PR_AGENT_LLM_BASE_URL").ok()?;
    let api_key = std::env::var("PR_AGENT_LLM_API_KEY").ok()?;
    let model = std::env::var("PR_AGENT_MODEL").ok()?;
    if base_url.is_empty() || api_key.is_empty() || model.is_empty() {
        return None;
    }
    Some(LlmEnv {
        base_url,
        api_key,
        model,
    })
}

const CODE_REVIEW_SKILL: &str = "code-review-skill";

pub(crate) fn review_skill_enabled() -> bool {
    std::env::var("PR_AGENT_USE_CODE_REVIEW_SKILL")
        .is_ok_and(|value| value.trim().eq_ignore_ascii_case("true"))
}

fn review_skill_root() -> Result<Option<PathBuf>, OpenCodeError> {
    if !review_skill_enabled() {
        return Ok(None);
    }
    let root = std::env::var_os("PR_AGENT_CODE_REVIEW_SKILL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/opt/pr-agent/skills"));
    if !root.join(CODE_REVIEW_SKILL).join("SKILL.md").is_file() {
        return Err(OpenCodeError::runtime(format!(
            "code-review-skill is enabled but SKILL.md is missing under {}; use an image that includes it or set PR_AGENT_CODE_REVIEW_SKILL_DIR",
            root.display()
        )));
    }
    root.canonicalize().map(Some).map_err(|error| {
        OpenCodeError::runtime(format!(
            "cannot resolve code-review-skill directory: {error}"
        ))
    })
}

/// Resolve the environment the engine process runs with.
///
/// Precedence:
/// 1. `OPENCODE_CONFIG` already set — the user drives the engine config
///    entirely; pr-agent never touches it.
/// 2. `PR_AGENT_LLM_BASE_URL` + `PR_AGENT_LLM_API_KEY` + `PR_AGENT_MODEL`
///    set — the project's `.open-pr-agent/opencode.jsonc` (or an empty
///    config) gains a provider with that endpoint and model; the merged
///    config goes to a temp file, exported as `OPENCODE_CONFIG` for the
///    engine run only. The user config may define MCP servers, agents and
///    anything else — but not providers/models for this provider id.
/// 3. No LLM env — leave model discovery to OpenCode and supply the reviewer.
fn resolve_engine_env() -> Result<std::ffi::OsString, OpenCodeError> {
    let skill_root = review_skill_root()?;
    if std::env::var_os("OPENCODE_CONFIG").is_some_and(|v| !v.is_empty()) {
        return Ok(std::env::var_os("OPENCODE_CONFIG").expect("checked above"));
    }
    let llm = llm_env();
    let mut config = llm
        .as_ref()
        .and_then(|_| std::fs::read_to_string(".open-pr-agent/opencode.jsonc").ok())
        .and_then(|text| parse_jsonc(&text))
        .filter(Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    let defaults: serde_json::Map<String, Value> =
        serde_json::from_str(include_str!("../settings/opencode_defaults.json"))
            .expect("packaged OpenCode defaults must be valid");
    let config_fields = config.as_object_mut().expect("config is an object");
    for (key, value) in defaults {
        let field = config_fields.entry(key).or_insert_with(|| value.clone());
        if let (Some(fields), Value::Object(defaults)) = (field.as_object_mut(), value) {
            for (key, value) in defaults {
                fields.entry(key).or_insert(value);
            }
        }
    }
    let agents = config_fields
        .entry("agents")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| OpenCodeError::runtime("OpenCode agents must be an object"))?;
    let reviewer = agents
        .entry(DEFAULT_AGENT_NAME)
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| {
            OpenCodeError::runtime("agents.pr-reviewer must be an OpenCode agent object")
        })?;
    let defaults = [
        (
            "description",
            Value::String("Review changes for correctness and regressions".into()),
        ),
        ("mode", Value::String("primary".into())),
        ("system", Value::String(PACKAGED_PR_REVIEWER_PROMPT.into())),
    ];
    for (key, value) in defaults {
        reviewer.entry(key).or_insert(value);
    }
    let permissions = reviewer
        .entry("permissions")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .ok_or_else(|| {
            OpenCodeError::runtime(
                "agents.pr-reviewer.permissions must be an OpenCode v2 rule array",
            )
        })?;
    // OpenCode applies the last matching rule. Project grants cannot relax
    // the baseline, and project denies can further restrict its exceptions.
    let project_denials: Vec<_> = permissions
        .iter()
        .filter(|rule| {
            rule.get("effect").and_then(Value::as_str) == Some("deny")
                && rule
                    .get("action")
                    .and_then(Value::as_str)
                    .is_some_and(|action| action == "*" || action == "shell")
        })
        .map(|rule| {
            let mut rule = rule.clone();
            rule["action"] = Value::String("shell".into());
            rule
        })
        .collect();
    permissions.extend([
        serde_json::json!({"action": "edit", "resource": "*", "effect": "deny"}),
        serde_json::json!({"action": "webfetch", "resource": "*", "effect": "deny"}),
        serde_json::json!({"action": "websearch", "resource": "*", "effect": "deny"}),
        serde_json::json!({"action": "shell", "resource": "*", "effect": "deny"}),
        serde_json::json!({"action": "shell", "resource": "git diff", "effect": "allow"}),
        serde_json::json!({"action": "external_directory", "resource": "*", "effect": "deny"}),
        serde_json::json!({"action": "subagent", "resource": "*", "effect": "deny"}),
    ]);
    permissions.extend(project_denials);

    if let Some(root) = skill_root {
        let skill = root.join(CODE_REVIEW_SKILL);
        permissions.extend([
            serde_json::json!({"action": "skill", "resource": CODE_REVIEW_SKILL, "effect": "allow"}),
            serde_json::json!({"action": "external_directory", "resource": format!("{}/*", skill.display()), "effect": "allow"}),
        ]);
        let sources = config
            .as_object_mut()
            .expect("config is an object")
            .entry("skills")
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .ok_or_else(|| OpenCodeError::runtime("OpenCode skills must be an array"))?;
        let source = serde_json::json!(root);
        if !sources.contains(&source) {
            sources.push(source);
        }
    }

    if let Some(llm) = llm {
        // The model id: `provider/model` names the model under the injected
        // provider (the prefix is informational — the provider id is fixed); a
        // bare name is the model id itself.
        let model = llm
            .model
            .rsplit_once('/')
            .map(|(_, model)| model)
            .unwrap_or(llm.model.as_str());

        // Inject the provider and select the model. Existing providers with a
        // different id stay; the same id is overwritten — the env wins for it.
        let provider_id = "pr-agent-llm";
        if !config
            .get("providers")
            .map(Value::is_object)
            .unwrap_or(false)
        {
            config["providers"] = serde_json::json!({});
        }
        config["providers"][provider_id] = serde_json::json!({
            "name": "PR Agent LLM",
            "package": "@opencode/ai/providers/openai-compatible",
            "settings": {
                "baseURL": llm.base_url,
                "apiKey": llm.api_key,
            },
            "models": {
                model: {
                    "name": model,
                    "capabilities": {"tools": true, "input": ["text"], "output": ["text"]},
                },
            },
        });
        config["model"] = Value::String(format!("{provider_id}/{model}"));
    }

    let config_dir = std::env::temp_dir().join("pr-agent-worktrees");
    std::fs::create_dir_all(&config_dir).map_err(|e| {
        OpenCodeError::runtime(format!("cannot create engine config directory: {e}"))
    })?;
    let path = config_dir.join(format!("opencode-config-{}.json", std::process::id()));
    let text = serde_json::to_string_pretty(&config)
        .map_err(|e| OpenCodeError::runtime(format!("cannot serialize engine config: {e}")))?;
    std::fs::write(&path, text)
        .map_err(|e| OpenCodeError::runtime(format!("cannot write engine config: {e}")))?;
    Ok(path.into_os_string())
}

/// Parse a JSONC document (comments, trailing commas) into a value; plain
/// JSON parses as-is.
fn parse_jsonc(text: &str) -> Option<Value> {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        return Some(value);
    }
    let mut cleaned = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            cleaned.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                cleaned.push(c);
            }
            '/' if chars.get(i + 1) == Some(&'/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
                continue;
            }
            _ => cleaned.push(c),
        }
        i += 1;
    }
    // Trailing commas before a closer.
    let cleaned = strip_trailing_commas(&cleaned);
    serde_json::from_str(&cleaned).ok()
}

/// Remove commas directly before a closing brace or bracket.
fn strip_trailing_commas(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == ',' {
            let mut rest = chars.peek();
            while let Some(&w) = rest {
                if w.is_whitespace() {
                    chars.next();
                    rest = chars.peek();
                } else {
                    if w != '}' && w != ']' {
                        out.push(c);
                    }
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
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

#[derive(Default)]
struct EngineProgress {
    steps: u64,
    input: Option<u64>,
    output: Option<u64>,
    reasoning: Option<u64>,
}

impl EngineProgress {
    fn observe(&mut self, event: &Value) {
        if !matches!(event_type(event), Some("step_finish" | "step-finish")) {
            return;
        }
        self.steps += 1;
        let tokens = event.get("part").unwrap_or(event).get("tokens");
        for (total, kind) in [
            (&mut self.input, "input"),
            (&mut self.output, "output"),
            (&mut self.reasoning, "reasoning"),
        ] {
            if let Some(count) = tokens
                .and_then(|tokens| tokens.get(kind))
                .and_then(Value::as_u64)
            {
                *total = Some(total.unwrap_or(0).saturating_add(count));
            }
        }
    }

    fn log(&self, phase: &str, elapsed: Duration) {
        let mut counts = Vec::new();
        for (kind, count) in [
            ("input", self.input),
            ("output", self.output),
            ("reasoning", self.reasoning),
        ] {
            if let Some(count) = count {
                counts.push(format!("{kind}={count}"));
            }
        }
        let tokens = if counts.is_empty() {
            "tokens not reported yet".to_string()
        } else {
            format!("reported tokens: {}", counts.join(", "))
        };
        crate::logging::info(
            &crate::logging::Loc {
                module: "opencode",
                func: "run",
            },
            &format!(
                "OpenCode {phase} after {}s; {} completed steps; {tokens}",
                elapsed.as_secs(),
                self.steps
            ),
        );
    }
}

const PROGRESS_INTERVAL: Duration = Duration::from_secs(10);

/// Drain both streams while OpenCode runs so a full pipe cannot stall it.
fn run_with_timeout(
    mut command: Command,
    timeout: Duration,
) -> Result<Option<std::process::Output>, String> {
    use std::io::{BufRead, BufReader, Read};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let start = Instant::now();
    let mut next_report = PROGRESS_INTERVAL;
    let progress = Arc::new(Mutex::new(EngineProgress::default()));
    let report = Arc::clone(&progress);
    let mut stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
    let mut stderr = child.stderr.take().expect("piped stderr");
    let stdout_reader = std::thread::spawn(move || -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        let mut offset = 0;
        while stdout.read_until(b'\n', &mut bytes)? != 0 {
            if let Ok(event) = serde_json::from_slice::<Value>(&bytes[offset..]) {
                report.lock().expect("progress lock").observe(&event);
            }
            offset = bytes.len();
        }
        Ok(bytes)
    });
    let stderr_reader = std::thread::spawn(move || -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes)?;
        Ok(bytes)
    });

    loop {
        let status = child.try_wait().map_err(|e| e.to_string())?;
        if let Some(status) = status
            && stdout_reader.is_finished()
            && stderr_reader.is_finished()
        {
            let stdout = stdout_reader
                .join()
                .map_err(|_| "OpenCode stdout reader panicked")?
                .map_err(|e| format!("cannot read OpenCode stdout: {e}"))?;
            let stderr = stderr_reader
                .join()
                .map_err(|_| "OpenCode stderr reader panicked")?
                .map_err(|e| format!("cannot read OpenCode stderr: {e}"))?;
            progress.lock().expect("progress lock").log(
                if status.success() {
                    "finished"
                } else {
                    "exited with an error"
                },
                start.elapsed(),
            );
            return Ok(Some(std::process::Output {
                status,
                stdout,
                stderr,
            }));
        }
        let elapsed = start.elapsed();
        if elapsed >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            progress
                .lock()
                .expect("progress lock")
                .log("timed out", elapsed);
            return Ok(None);
        }
        if elapsed >= next_report {
            progress
                .lock()
                .expect("progress lock")
                .log("still running", elapsed);
            next_report += PROGRESS_INTERVAL;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The prompt the agent receives: the task, then the changed-file map (a map,
/// not the diff content — the agent reads the tree itself).
pub fn compose_prompt(task: &str, file_map: &[FileMapEntry], include_map: bool) -> String {
    let mut full_prompt = task.to_string();
    if include_map && !file_map.is_empty() {
        full_prompt.push_str("\n\nChanged files:\n");
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

/// The reviewer system prompt shipped with the CLI.
const PACKAGED_PR_REVIEWER_PROMPT: &str = include_str!("../settings/agents/pr-reviewer.md");

/// A fresh worktree directory under the process temp root.
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
            "--allow-empty",
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

fn engine_failure(output: &std::process::Output) -> OpenCodeError {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let event_message = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event.get("type").and_then(Value::as_str) == Some("error"))
        .find_map(|event| engine_error_message(&event).map(str::to_owned));
    let detail = event_message.as_deref().unwrap_or_else(|| stderr.trim());
    let detail = if detail.is_empty() {
        "no error details reported"
    } else {
        detail
    };
    let safe_detail = std::env::var("PR_AGENT_LLM_API_KEY")
        .ok()
        .filter(|key| !key.is_empty())
        .map_or_else(
            || detail.to_owned(),
            |key| detail.replace(&key, "[redacted]"),
        );
    OpenCodeError::runtime(format!(
        "opencode run failed with status {}: {safe_detail}",
        output.status
    ))
}

fn engine_error_message(event: &Value) -> Option<&str> {
    event
        .pointer("/error/message")
        .or_else(|| event.pointer("/error/data/message"))
        .and_then(Value::as_str)
}

fn event_type(event: &Value) -> Option<&str> {
    event
        .get("type")
        .and_then(Value::as_str)
        .or_else(|| event.pointer("/part/type").and_then(Value::as_str))
}

/// Parse the JSONL stream `opencode run --format json` prints: one event per
/// line, the payload under `part`. Keep text from the final assistant
/// message and collect usage from step-finish events.
pub fn parse_agent_reply(stdout: &[u8]) -> Result<AgentReply, OpenCodeError> {
    let text = String::from_utf8_lossy(stdout);
    let mut reply = AgentReply::default();
    let mut message_id = None;
    for line in text.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue; // non-JSON chatter on stdout is ignored
        };
        // The stream spells the part type at the top level with underscores
        // ("step_finish") and inside the part with dashes ("step-finish");
        // match both, then read the payload from `part`.
        let part = event.get("part").unwrap_or(&event);
        match event_type(&event) {
            Some("text" | "text-delta" | "text_delta") => {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    if let Some(id) = part.get("messageID").and_then(Value::as_str)
                        && message_id.as_deref() != Some(id)
                    {
                        if message_id.is_some() {
                            reply.text.clear();
                        }
                        message_id = Some(id.to_owned());
                    }
                    reply.text.push_str(text);
                }
            }
            Some("step-finish" | "step_finish") => {
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
                let message = engine_error_message(&event).unwrap_or("unknown opencode error");
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

    use crate::TEST_ENV_LOCK as ENV_LOCK;

    #[test]
    fn progress_accumulates_only_reported_step_tokens() {
        let mut progress = EngineProgress::default();
        for event in [
            serde_json::json!({"type":"step_start","part":{"tokens":{"input":900}}}),
            serde_json::json!({"type":"step_finish","part":{"tokens":{"input":10,"output":4}}}),
            serde_json::json!({"part":{"type":"step-finish","tokens":{"input":20}}}),
        ] {
            progress.observe(&event);
        }
        assert_eq!(progress.steps, 2);
        assert_eq!(progress.input, Some(30));
        assert_eq!(progress.output, Some(4));
        assert_eq!(progress.reasoning, None);
    }

    #[cfg(unix)]
    #[test]
    fn drains_large_stdout_and_stderr_before_opencode_exits() {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "head -c 262144 /dev/zero; head -c 262144 /dev/zero >&2",
        ]);
        let result = run_with_timeout(command, Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout.len(), 262144);
        assert_eq!(result.stderr.len(), 262144);
    }

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
    fn reply_excludes_text_from_intermediate_assistant_messages() {
        let stream = concat!(
            "{\"type\":\"text\",\"part\":{\"messageID\":\"msg_1\",\"text\":\"I will read the diff.\\n\"}}\n",
            "{\"type\":\"tool_use\",\"part\":{\"messageID\":\"msg_1\",\"tool\":\"read\"}}\n",
            "{\"type\":\"text\",\"part\":{\"messageID\":\"msg_2\",\"text\":\"review:\\n\"}}\n",
            "{\"type\":\"text\",\"part\":{\"messageID\":\"msg_2\",\"text\":\"  relevant_tests: No\"}}\n",
        );
        let reply = parse_agent_reply(stream.as_bytes()).unwrap();
        assert_eq!(reply.text, "review:\n  relevant_tests: No");
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

    #[test]
    fn jsonc_parses_comments_and_trailing_commas() {
        let text = r#"{
  // an MCP server the project wants in reviews
  "mcp": {
    "fetch": {"command": "uvx", "args": ["mcp-server-fetch"]}, // trailing comment
  },
  "agents": {"pr-reviewer": {"model": "other/x"}}, /* block
     comment */
}"#;
        let value = parse_jsonc(text).expect("jsonc parses");
        assert!(value.pointer("/mcp/fetch/command").and_then(Value::as_str) == Some("uvx"));
        assert!(value.pointer("/agents/pr-reviewer/model").is_some());
    }

    #[test]
    fn jsonc_plain_json_still_works() {
        let value = parse_jsonc(r#"{"a": 1}"#).expect("plain json parses");
        assert_eq!(value.get("a").and_then(Value::as_i64), Some(1));
    }

    #[test]
    fn user_opencode_config_is_never_overwritten() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // SAFETY: single-threaded test under ENV_LOCK, no other thread reads
        // the environment.
        unsafe {
            std::env::set_var("OPENCODE_CONFIG", "/user/chosen/config.json");
            std::env::set_var("PR_AGENT_LLM_BASE_URL", "http://x");
            std::env::set_var("PR_AGENT_LLM_API_KEY", "k");
            std::env::set_var("PR_AGENT_MODEL", "p/m");
            let config = resolve_engine_env().expect("resolves");
            assert_eq!(config, "/user/chosen/config.json");
            std::env::remove_var("OPENCODE_CONFIG");
            std::env::remove_var("PR_AGENT_LLM_BASE_URL");
            std::env::remove_var("PR_AGENT_LLM_API_KEY");
            std::env::remove_var("PR_AGENT_MODEL");
        }
    }

    #[test]
    fn llm_env_injects_the_provider_and_model() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = std::env::temp_dir().join("pr-agent-tests/engine-config/.open-pr-agent");
        std::fs::create_dir_all(&dir).unwrap();
        // A project engine config: MCP the review may use, no providers.
        std::fs::write(
            dir.join("opencode.jsonc"),
            r#"{
                "mcp": {"fetch": {"type": "local", "command": ["uvx"]}},
                "agents": {"pr-reviewer": {"permissions": [
                    {"action": "edit", "resource": "*", "effect": "allow"},
                    {"action": "webfetch", "resource": "*", "effect": "allow"},
                    {"action": "shell", "resource": "*", "effect": "deny"}
                ]}}
            }"#,
        )
        .unwrap();
        let dir = dir.parent().unwrap();
        // SAFETY: single-threaded test under ENV_LOCK, no other thread reads
        // the environment.
        unsafe {
            std::env::remove_var("OPENCODE_CONFIG");
            std::env::set_var("PR_AGENT_LLM_BASE_URL", "http://llm.example");
            std::env::set_var("PR_AGENT_LLM_API_KEY", "sk-test");
            std::env::set_var("PR_AGENT_MODEL", "pr-agent-llm/my-model");
        }

        let cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir).unwrap();
        let config_path = resolve_engine_env().expect("resolves");
        std::env::set_current_dir(cwd).unwrap();
        let text = std::fs::read_to_string(config_path.to_string_lossy().as_ref()).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        // The project's MCP config survived the injection.
        assert_eq!(value["mcp"]["fetch"]["command"][0], "uvx");
        let rules = value["agents"]["pr-reviewer"]["permissions"]
            .as_array()
            .unwrap();
        for action in ["edit", "webfetch", "shell"] {
            let effect = rules
                .iter()
                .rev()
                .find(|rule| rule["action"] == action)
                .and_then(|rule| rule["effect"].as_str());
            assert_eq!(effect, Some("deny"), "{action}");
        }
        // The provider endpoint and model selection arrived from env.
        assert_eq!(
            value
                .pointer("/providers/pr-agent-llm/settings/baseURL")
                .and_then(Value::as_str),
            Some("http://llm.example")
        );
        assert_eq!(
            value
                .pointer("/providers/pr-agent-llm/settings/apiKey")
                .and_then(Value::as_str),
            Some("sk-test")
        );
        assert_eq!(
            value.get("model").and_then(Value::as_str),
            Some("pr-agent-llm/my-model")
        );

        // SAFETY: same lock, cleanup.
        unsafe {
            std::env::remove_var("PR_AGENT_LLM_BASE_URL");
            std::env::remove_var("PR_AGENT_LLM_API_KEY");
            std::env::remove_var("PR_AGENT_MODEL");
        }
        let _ = std::fs::remove_file(config_path.to_string_lossy().as_ref());
    }

    #[test]
    fn bare_model_name_works_without_the_provider_prefix() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = std::env::temp_dir().join("pr-agent-tests/engine-config-bare");
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: single-threaded test under ENV_LOCK.
        unsafe {
            std::env::remove_var("OPENCODE_CONFIG");
            std::env::set_var("PR_AGENT_LLM_BASE_URL", "http://llm.example");
            std::env::set_var("PR_AGENT_LLM_API_KEY", "sk-test");
            std::env::set_var("PR_AGENT_MODEL", "gpt-oss");
        }
        let cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();
        let config_path = resolve_engine_env().expect("bare model name resolves");
        std::env::set_current_dir(cwd).unwrap();
        let text = std::fs::read_to_string(config_path.to_string_lossy().as_ref()).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        // The bare name became the model under the injected provider.
        assert_eq!(
            value.get("model").and_then(Value::as_str),
            Some("pr-agent-llm/gpt-oss")
        );
        assert!(
            value
                .pointer("/providers/pr-agent-llm/models/gpt-oss")
                .is_some()
        );
        // SAFETY: cleanup under the same lock.
        unsafe {
            std::env::remove_var("PR_AGENT_LLM_BASE_URL");
            std::env::remove_var("PR_AGENT_LLM_API_KEY");
            std::env::remove_var("PR_AGENT_MODEL");
        }
        let _ = std::fs::remove_file(config_path.to_string_lossy().as_ref());
    }

    #[test]
    fn no_llm_env_supplies_reviewer_without_selecting_a_model() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // SAFETY: single-threaded test under ENV_LOCK.
        unsafe {
            std::env::remove_var("OPENCODE_CONFIG");
            std::env::remove_var("PR_AGENT_LLM_BASE_URL");
            std::env::remove_var("PR_AGENT_LLM_API_KEY");
            std::env::remove_var("PR_AGENT_MODEL");
        }
        let path = std::path::PathBuf::from(resolve_engine_env().unwrap());
        let config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            config
                .pointer("/agents/pr-reviewer/mode")
                .and_then(Value::as_str),
            Some("primary")
        );
        assert_eq!(config["update"], "disable");
        assert_eq!(config["share"], "disabled");
        assert_eq!(config["compaction"]["auto"], true);
        assert!(config.get("providers").is_none());
        assert!(config.get("model").is_none());
        std::fs::remove_file(path).unwrap();
    }
}
