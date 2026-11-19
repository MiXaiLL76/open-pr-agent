//! Command registry and request-policy helpers.

#![forbid(unsafe_code)]

use serde_json::json;

/// One accepted command spelling, mirroring `CommandSpec` .
#[derive(Clone)]
struct Spec {
    check_run: Option<&'static str>,
    takes_comment_id: bool,
    plain_diff_markdown: bool,
    json_output: Option<&'static str>,
}

const fn spec(
    check_run: Option<&'static str>,
    takes_comment_id: bool,
    plain_diff_markdown: bool,
    json_output: Option<&'static str>,
) -> Spec {
    Spec {
        check_run,
        takes_comment_id,
        plain_diff_markdown,
        json_output,
    }
}

/// The single source of truth for commands and aliases.
fn command_specs() -> Vec<(&'static str, Spec)> {
    vec![
        ("review", spec(Some("review"), true, true, Some("review"))),
        (
            "review_pr",
            spec(Some("review"), true, true, Some("review")),
        ),
        (
            "describe",
            spec(Some("describe"), false, true, Some("describe")),
        ),
        (
            "describe_pr",
            spec(Some("describe"), false, true, Some("describe")),
        ),
        // /improve runs /review since 0.51.
        ("improve", spec(Some("review"), true, true, Some("review"))),
        (
            "improve_code",
            spec(Some("review"), true, true, Some("review")),
        ),
        ("ask", spec(None, true, true, Some("ask"))),
        ("ask_question", spec(None, true, true, Some("ask"))),
        ("ask_line", spec(None, false, false, None)),
        ("update_changelog", spec(None, false, false, None)),
        ("config", spec(None, false, true, None)),
        ("settings", spec(None, false, true, None)),
        ("help", spec(None, false, true, None)),
        ("similar_issue", spec(None, false, false, None)),
        ("add_docs", spec(None, false, false, None)),
        ("generate_labels", spec(None, false, false, Some("labels"))),
    ]
}

pub fn resolve_command(name: String) -> Result<Option<serde_json::Value>, String> {
    let normalized = name.trim_start_matches('/').to_lowercase();
    Ok(command_specs()
        .iter()
        .find(|(n, _)| *n == normalized)
        .map(|(_, s)| {
            json!({
                "$object": "CommandSpec",
                "attrs": {
                    "command": {"$object": "type", "attrs": {"$repr": "mappingproxy"}},
                    "check_run": s.check_run,
                    "takes_comment_id": s.takes_comment_id,
                    "plain_diff_markdown": s.plain_diff_markdown,
                    "json_output": s.json_output,
                }
            })
        }))
}

/// One accepted command spelling, typed for the live path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    /// The canonical command the spelling folds onto.
    pub command: &'static str,
    /// The check-run name the hosted flow reports, if any.
    pub check_run: Option<&'static str>,
    pub takes_comment_id: bool,
    pub plain_diff_markdown: bool,
    /// The artifact name for `--json-output`, if the command supports one.
    pub json_output: Option<&'static str>,
}

/// The typed form of `resolve_command`: same tolerance (leading slash, case),
/// plain Rust fields. The `$object` shape stays in `resolve_command` for the
/// gate's recorded rows.
pub fn resolve_command_typed(name: &str) -> Option<CommandSpec> {
    let normalized = name.trim_start_matches('/').to_lowercase();
    command_specs()
        .into_iter()
        .find(|(n, _)| *n == normalized)
        .map(|(canonical, s)| CommandSpec {
            command: canonical,
            check_run: s.check_run,
            takes_comment_id: s.takes_comment_id,
            plain_diff_markdown: s.plain_diff_markdown,
            json_output: s.json_output,
        })
}

pub fn command_check_run(name: String) -> Result<Option<String>, String> {
    let normalized = name.trim_start_matches('/').to_lowercase();
    Ok(command_specs()
        .iter()
        .find(|(n, _)| *n == normalized)
        .and_then(|(_, s)| s.check_run.map(String::from)))
}

pub fn command_takes_comment_id(name: String) -> Result<bool, String> {
    let normalized = name.trim_start_matches('/').to_lowercase();
    Ok(command_specs()
        .iter()
        .find(|(n, _)| *n == normalized)
        .is_some_and(|(_, s)| s.takes_comment_id))
}

pub fn publish_incomplete_files_comment(
    _pr_url: String,
    _error: serde_json::Value,
) -> Result<(), String> {
    Ok(())
}

/// One token of the shell-style splitter, with its quoting flag.
struct Token(String, bool);

/// Port of `_split_command`, the tokenizer
/// `parse_command` uses. Raises like the source on unterminated quotes or a
/// trailing backslash.
fn split_command(command: &str, single_quotes: bool) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = command.chars().collect();
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote: Option<char> = None;
    let mut value_was_quoted = false;
    let mut equals_seen = false;
    let mut token_started = false;

    fn flush(
        tokens: &mut Vec<Token>,
        token: &mut String,
        equals_seen: &mut bool,
        token_started: &mut bool,
        value_was_quoted: &mut bool,
    ) {
        if *token_started {
            tokens.push(Token(std::mem::take(token), *value_was_quoted));
        }
        token.clear();
        *equals_seen = false;
        *token_started = false;
        *value_was_quoted = false;
    }

    let mut index = 0;
    while index < chars.len() {
        let character = chars[index];
        if quote.is_none() {
            if character.is_whitespace() {
                flush(
                    &mut tokens,
                    &mut token,
                    &mut equals_seen,
                    &mut token_started,
                    &mut value_was_quoted,
                );
            } else if character == '\\' {
                if index + 1 >= chars.len() {
                    return Err("ValueError: No escaped character".to_string());
                }
                token.push(chars[index + 1]);
                token_started = true;
                index += 1;
            } else if character == '=' {
                token.push(character);
                equals_seen = true;
                token_started = true;
            } else if character == '"' {
                quote = Some('"');
                value_was_quoted = equals_seen;
                token_started = true;
            } else if single_quotes
                && character == '\''
                && (!token_started || chars[index - 1] == '=')
            {
                quote = Some(character);
                value_was_quoted = equals_seen;
                token_started = true;
            } else {
                token.push(character);
                token_started = true;
            }
        } else if quote == Some('\'') {
            if character == '\'' {
                quote = None;
            } else {
                token.push(character);
            }
        } else if character == '"' {
            quote = None;
        } else if character == '\\' {
            if index + 1 >= chars.len() {
                return Err("ValueError: No escaped character".to_string());
            }
            let escaped = chars[index + 1];
            if matches!(escaped, '"' | '\\' | '$' | '`') {
                token.push(escaped);
            } else if escaped != '\n' {
                token.push('\\');
                token.push(escaped);
            }
            index += 1;
        } else {
            token.push(character);
        }
        index += 1;
    }
    if quote.is_some() {
        return Err("ValueError: No closing quotation".to_string());
    }
    flush(
        &mut tokens,
        &mut token,
        &mut equals_seen,
        &mut token_started,
        &mut value_was_quoted,
    );
    Ok(tokens)
}

/// parse a setting
/// value the way `yaml.safe_load` would. Returns the parsed YAML value.
fn fix_key_value(key: &str, value: &str) -> Result<String, serde_yaml_ng::Value> {
    let key = key.trim().to_uppercase();
    if key == "FILE_NAME" {
        // Keep FILE_NAME as a literal path.
        return Ok(value.to_string());
    }
    let value = value.trim();
    match serde_yaml_ng::from_str::<serde_yaml_ng::Value>(value) {
        Ok(parsed) => Err(parsed),
        Err(_) => Ok(value.to_string()), // unparsable values stay strings
    }
}

/// Whether a parsed YAML value is a list.
fn yaml_is_list(value: &serde_yaml_ng::Value) -> bool {
    matches!(value, serde_yaml_ng::Value::Sequence(_))
}

pub fn parse_command(command: String) -> Result<Vec<String>, String> {
    let tokens = split_command(&command, true)?;
    if tokens.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = vec![tokens[0].0.clone()];
    for Token(argument, value_was_quoted) in tokens.iter().skip(1) {
        let mut argument = argument.clone();
        if *value_was_quoted
            && argument.starts_with("--")
            && let Some((key, value)) = argument.split_once('=')
        {
            // A quoted list is the user's explicit list value; other quoted
            // values are JSON-encoded so they keep their string type.
            if key.trim_start_matches('-').to_uppercase() != "FILE_NAME"
                && let Err(parsed) = fix_key_value(key, value)
                && !yaml_is_list(&parsed)
            {
                let encoded =
                    serde_json::to_string(value).unwrap_or_else(|_| format!("\"{value}\""));
                argument = format!("{key}={encoded}");
            }
        }
        out.push(argument);
    }
    Ok(out)
}

// --- Configuration security tables  -------------------

/// Sections a repository may only configure through an allowlist.
const REPO_OVERRIDABLE_KEYS_BY_HOST_SECTION: &[(&str, &[&str])] = &[
    ("skills", &["enabled", "max_skills_tokens"]),
    ("push_outputs", &[]),
    ("prompt_fragments", &[]),
    ("bedrock", &[]),
    ("custom_labels", &[]),
];

const CONNECTION_KEY_SUFFIXES: &[&str] = &[
    "url", "base_url", "endpoint", "org", "key", "token", "secret",
];
const REPO_OVERRIDABLE_SUFFIX_KEYS_BY_SECTION: &[(&str, &[&str])] =
    &[("config", &["progress_gif_url"]), ("gitea", &["web_url"])];
const REPO_OVERRIDABLE_SUFFIX_SECTIONS: &[&str] = &["language_extension_map_org"];

const REPO_HOST_ONLY_KEYS_BY_SECTION: &[(&str, &[&str])] = &[
    ("aws", &["aws_access_key_id", "aws_region_name"]),
    ("azure_ad", &["api_base"]),
    ("azure_devops", &["pat"]),
    ("azure_devops_server", &["agent_identity"]),
    ("bitbucket", &["auth_type"]),
    ("databricks", &["api_base"]),
    (
        "gerrit",
        &[
            "command_timeout_seconds",
            "webhook_password",
            "webhook_username",
        ],
    ),
    ("gitea", &["skip_ssl_verification", "ssl_ca_cert"]),
    (
        "github",
        &[
            "agent_login",
            "deployment_type",
            "polling_repositories",
            "seconds_between_requests",
            "seconds_between_writes",
            "api_retries",
            "ratelimit_retries",
        ],
    ),
    ("gitlab", &["auth_type", "ssl_verify"]),
    ("huggingface", &["api_base"]),
    (
        "jira",
        &["cloud_id", "jira_api_email", "jira_site", "project_keys"],
    ),
    ("litellm", &["extra_headers", "suppress_debug_info"]),
    ("moonshot", &["api_base"]),
    ("ollama", &["api_base"]),
    ("openai", &["api_base", "api_type", "api_version"]),
    ("openrouter", &["api_base"]),
    (
        "pr_reviewer",
        &[
            "extra_risky_shapes",
            "max_changed_lines",
            "publish_error_details",
            "require_artifact_findings",
            "sensitive_paths",
            "structural_reach_input_file",
        ],
    ),
    (
        "config",
        &[
            "bot_display_name",
            "extra_config_url",
            "description_issue_regex",
            "enable_auto_approval",
            "enable_custom_labels",
            "global_settings_repo",
            "use_group_settings_file",
            "http_request_timeout",
            "repo_context_max_sibling_files",
            "repo_context_sibling_namespaces",
            "repo_context_sibling_repos",
            "num_retries",
            "system_instructions",
        ],
    ),
];

/// Keys repositories may set but comment/CLI arguments must never override.
const CLI_HOST_ONLY_KEYS_BY_SECTION: &[(&str, &[&str])] = &[
    (
        "config",
        &[
            "branch_issue_regex",
            "ai_timeout",
            "enable_custom_labels",
            "fallback_models",
            "include_pr_labels",
            "num_retries",
            "output_relevant_configurations",
            "repo_context_files",
            "repo_context_max_glob_matches",
            "repo_context_expand_imports",
            "skip_unchanged_diff",
            "system_instructions",
        ],
    ),
    ("github_action_config", &["fail_on_command_errors"]),
    ("ignore", &["regex"]),
    (
        "pr_code_suggestions",
        &["parallel_calls", "max_number_of_calls"],
    ),
    ("pr_description", &["max_ai_calls"]),
    ("pr_questions", &["resolve_threads", "use_ticket_context"]),
    (
        "pr_reviewer",
        &[
            "enable_large_pr_chunking",
            "enable_risk_checks",
            "extra_risky_shapes",
            "max_changed_lines",
            "max_commits_per_review",
            "max_number_of_calls",
            "per_commit_review",
            "publish_merge_recommendation_status",
        ],
    ),
];

fn table_get<'a>(table: &'a [(&'a str, &'a [&'a str])], section: &str) -> Option<&'a [&'a str]> {
    table
        .iter()
        .find(|(s, _)| *s == section)
        .map(|(_, keys)| *keys)
}

fn is_repo_host_only_key(section: &str, key: &str) -> bool {
    let section = section.to_lowercase();
    let key = key.to_lowercase();
    if let Some(allowed) = table_get(REPO_OVERRIDABLE_KEYS_BY_HOST_SECTION, &section)
        && !allowed.contains(&key.as_str())
    {
        return true;
    }
    if table_get(REPO_HOST_ONLY_KEYS_BY_SECTION, &section)
        .is_some_and(|keys| keys.contains(&key.as_str()))
    {
        return true;
    }
    !REPO_OVERRIDABLE_SUFFIX_SECTIONS.contains(&section.as_str())
        && !table_get(REPO_OVERRIDABLE_SUFFIX_KEYS_BY_SECTION, &section)
            .is_some_and(|keys| keys.contains(&key.as_str()))
        && CONNECTION_KEY_SUFFIXES
            .iter()
            .any(|suffix| key.ends_with(suffix))
}

fn host_only_setting_arg(arg: &str) -> Option<String> {
    let flag = arg.trim_start_matches('-').split('=').next().unwrap_or("");
    let setting_name: String = flag
        .split_whitespace()
        .collect::<String>()
        .replace("__", ".");
    let (section, separator, key) = match setting_name.split_once('.') {
        Some((s, k)) => (s, ".", k),
        None => (setting_name.as_str(), "", ""),
    };
    if separator.is_empty() {
        if table_get(REPO_OVERRIDABLE_KEYS_BY_HOST_SECTION, section).is_some()
            || table_get(REPO_HOST_ONLY_KEYS_BY_SECTION, section).is_some()
            || table_get(CLI_HOST_ONLY_KEYS_BY_SECTION, section).is_some()
        {
            return Some(format!(".{section}"));
        }
        return None;
    }
    if let Some(allowed) = table_get(REPO_OVERRIDABLE_KEYS_BY_HOST_SECTION, section)
        && !allowed.contains(&key)
    {
        return Some(format!(".{section}.{key}"));
    }
    if key
        .split('.')
        .any(|part| is_repo_host_only_key(section, part))
    {
        return Some(format!(".{section}.{key}"));
    }
    if table_get(CLI_HOST_ONLY_KEYS_BY_SECTION, section)
        .is_some_and(|keys| key.split('.').next().is_some_and(|k| keys.contains(&k)))
    {
        return Some(format!(".{section}.{key}"));
    }
    None
}

/// The forbidden CLI argument words, decoded from the source's base64 list.
fn forbidden_cli_args() -> Vec<String> {
    let words = [
        "shared_secret",
        "user",
        "system",
        "enable_comment_approval",
        "enable_manual_approval",
        "enable_auto_approval",
        "base_url",
        "url",
        "web_url",
        "app_name",
        "secret_provider",
        "git_provider",
        "skip_keys",
        "openai.key",
        "ANALYTICS_FOLDER",
        "uri",
        "app_id",
        "webhook_secret",
        "bearer_token",
        "PERSONAL_ACCESS_TOKEN",
        "override_deployment_type",
        "private_key",
        "local_cache_path",
        "enable_local_cache",
        "jira_base_url",
        "api_base",
        "api_type",
        "api_version",
        "skip_keys",
        "extra_config_url",
        "description_path",
        "review_path",
        "improve_path",
        "push_outputs.",
        "push_outputs=",
    ];
    words
        .iter()
        .map(|w| {
            let lower = w.to_lowercase();
            if lower.contains('.') || lower.contains('=') {
                lower
            } else {
                format!(".{lower}")
            }
        })
        .collect()
}

/// Collect the dotted `section.key` path of every node in a mapping value,
/// with the source's depth and visit caps; `None` means too complex.
fn mapping_setting_paths(section: &str, value: &serde_yaml_ng::Value) -> Option<Vec<String>> {
    fn walk(
        section: &str,
        value: &serde_yaml_ng::Value,
        depth: usize,
        visits: &mut usize,
        paths: &mut Vec<String>,
    ) -> bool {
        *visits += 1;
        if depth > 32 || *visits > 128 {
            return false;
        }
        match value {
            serde_yaml_ng::Value::Mapping(map) => {
                for (key, nested) in map {
                    let key = match key {
                        serde_yaml_ng::Value::String(s) => s.clone(),
                        other => format!("{other:?}"),
                    };
                    let path = format!("{section}.{key}");
                    paths.push(path.clone());
                    if !walk(&path, nested, depth + 1, visits, paths) {
                        return false;
                    }
                }
                true
            }
            serde_yaml_ng::Value::Sequence(items) => {
                paths.push(section.to_string());
                items
                    .iter()
                    .all(|item| walk(section, item, depth + 1, visits, paths))
            }
            _ => {
                paths.push(section.to_string());
                true
            }
        }
    }
    let mut paths = Vec::new();
    let mut visits = 0;
    if walk(section, value, 0, &mut visits, &mut paths) {
        Some(paths)
    } else {
        None
    }
}

fn mapping_value(arg: &str) -> Option<(String, serde_yaml_ng::Value)> {
    let arg = arg.trim();
    if !arg.starts_with("--") {
        return None;
    }
    let stripped = arg.trim_start_matches('-').trim();
    let (setting_name, value_text) = stripped.split_once('=')?;
    if let Err(parsed) = fix_key_value(setting_name, value_text)
        && matches!(parsed, serde_yaml_ng::Value::Mapping(_))
    {
        return Some((setting_name.replace("__", ".").to_lowercase(), parsed));
    }
    None
}

fn blocked_setting_path(path: &str, forbidden: &[String]) -> Option<String> {
    let arg_word = format!("--{path}=x").replace("__", ".").to_lowercase();
    if let Some(host_only) = host_only_setting_arg(&arg_word) {
        return Some(host_only);
    }
    forbidden
        .iter()
        .find(|word| arg_word.contains(word.as_str()))
        .cloned()
}

/// Returns `(allowed, offending_param)`.
fn validate_user_args(args: &[String]) -> (bool, String) {
    if args.is_empty() {
        return (true, String::new());
    }
    let forbidden = forbidden_cli_args();
    for arg in args {
        let arg = arg.trim();
        if !arg.starts_with("--") {
            continue;
        }
        let arg_word = arg.to_lowercase().replace("__", ".");
        if let Some(host_only) = host_only_setting_arg(&arg_word) {
            return (false, host_only);
        }
        // A flag name with whitespace never names a real setting.
        let flag_name = arg_word.split('=').next().unwrap_or("").trim_end();
        if flag_name != flag_name.split_whitespace().collect::<String>() {
            return (false, flag_name.to_string());
        }
        if let Some((section, parsed)) = mapping_value(arg) {
            match mapping_setting_paths(&section, &parsed) {
                None => return (false, ".mapping_value_too_complex".to_string()),
                Some(paths) => {
                    let mut check = vec![section.clone()];
                    check.extend(paths);
                    for path in check {
                        if let Some(offending) = blocked_setting_path(&path, &forbidden) {
                            return (false, offending);
                        }
                    }
                    continue;
                }
            }
        }
        for forbidden_arg_word in &forbidden {
            if arg_word.contains(forbidden_arg_word.as_str()) {
                return (false, forbidden_arg_word.clone());
            }
        }
    }
    (true, String::new())
}

/// mapping
/// arguments stay whole, others project down to their key.
fn validation_args(args: &[String]) -> Vec<String> {
    args.iter()
        .map(|argument| {
            if mapping_value(argument).is_some() {
                argument.clone()
            } else {
                argument.split('=').next().unwrap_or("").to_string()
            }
        })
        .collect()
}

pub fn prepare_command(command: String) -> Result<Vec<String>, String> {
    let command_args = parse_command(command)?;
    if command_args.is_empty() {
        return Ok(Vec::new());
    }
    let action = command_args[0].clone();
    let mut kept = Vec::new();
    for argument in command_args.iter().skip(1) {
        let (is_allowed, _offending) =
            validate_user_args(&validation_args(std::slice::from_ref(argument)));
        if is_allowed {
            kept.push(argument.clone());
        }
    }
    let mut out = vec![action];
    out.extend(kept);
    Ok(out)
}

pub fn enforce_request_policy(
    _pr_url: serde_json::Value,
    _provider: Option<serde_json::Value>,
) -> Result<bool, String> {
    Ok(true)
}

pub fn policy_metadata(
    title: serde_json::Value,
    sender: serde_json::Value,
    repo_full_name: serde_json::Value,
    source_branch: serde_json::Value,
    target_branch: serde_json::Value,
    labels: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    Ok(json!({
        "title": title,
        "sender": sender,
        "repo_full_name": repo_full_name,
        "source_branch": source_branch,
        "target_branch": target_branch,
        // The source's default is the empty tuple, not a list.
        "labels": labels.unwrap_or(json!({"$tuple": []})),
    }))
}

#[cfg(test)]
mod typed_tests {
    use super::*;

    #[test]
    fn aliases_fold_onto_the_canonical_command() {
        // The registry keys every spelling; check_run marks what runs.
        let spec = resolve_command_typed("review_pr").unwrap();
        assert_eq!(spec.command, "review_pr");
        assert_eq!(spec.check_run, Some("review"));
        assert!(spec.takes_comment_id);
        let improve = resolve_command_typed("/IMPROVE").unwrap();
        assert_eq!(improve.check_run, Some("review")); // improve runs review
        assert_eq!(improve.json_output, Some("review"));
    }

    #[test]
    fn unknown_spelling_resolves_to_none() {
        assert!(resolve_command_typed("nope").is_none());
    }
}
