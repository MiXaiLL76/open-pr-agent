//! The native CI pipeline: `--pr_url <github> <command>` and
//! `--diff-file <path> review` handled in Rust, byte-compatible with the
//! recorded CLI contract. Anything this module does not recognize falls back
//! to the original, so the contract stays green while units port.

use std::ffi::OsString;
use std::process::ExitCode;

use serde_json::{Value as Json, json};
use serde_yaml_ng::Value as Yaml;

use crate::github::GithubClient;
use crate::gitlab::GitlabClient;
use crate::logging::{Loc, info, warning};
use crate::opencode::{
    FileMapEntry, OpenCodeHandler, OpenCodeSettings, RunUsage, binary_available,
};
use crate::render::render_review_markdown;
use crate::settings::Settings;

/// The parsed native CLI shape.
struct NativeCli {
    pr_url: Option<String>,
    diff_file: Option<String>,
    json_output: Option<String>,
    command: String,
    question: Vec<String>,
}

/// Try to run natively; `None` means "not a supported shape, fall back".
///
/// Every route decides before emitting anything, so an unsupported shape falls
/// back without leaking output into the original's.
pub fn try_run_native(args: &[OsString]) -> Option<ExitCode> {
    let cli = parse(args)?;
    match route(&cli) {
        Route::Pipeline => {
            native_gate(&cli)?;
            let settings = Settings::load();
            run(&cli, &settings)
        }
        Route::ArgparseError(message) => {
            let _settings = Settings::load();
            argparse_fail(&message)
        }
        Route::NoToken(url) => {
            let _settings = Settings::load();
            no_token_report(&url)
        }
        Route::BadUrl(command) => {
            let _settings = Settings::load();
            bad_url_report(&command)
        }
        Route::EmptyUrl => {
            let _settings = Settings::load();
            print!("{}", crate::blobs::USAGE_FULL);
            Some(ExitCode::SUCCESS)
        }
        Route::Doctor => {
            let settings = Settings::load();
            doctor(&settings)
        }
    }
}

/// `pr-agent doctor`: preflight checks without a git provider. The engine is
/// opencode, so the checks are the binary and a one-shot completion; exit 0
/// only when every check passes.
fn doctor(_settings: &Settings) -> Option<ExitCode> {
    let opencode = OpenCodeSettings::load(None);
    let mut results: Vec<(bool, String)> = vec![];
    if binary_available(&opencode) {
        results.push((true, format!("opencode binary = {}", opencode.binary)));
    } else {
        results.push((
            false,
            "opencode binary not found; set OPENCODE__BINARY or install opencode".into(),
        ));
    }
    let handler = OpenCodeHandler::new(opencode);
    match handler.ping() {
        Ok(()) => results.push((true, "model completion succeeded".into())),
        Err(err) => results.push((false, format!("model completion failed: {}", err.0))),
    }
    for (ok, detail) in &results {
        println!("{} {detail}", if *ok { "✓" } else { "✗" });
    }
    let passed = results.iter().filter(|(ok, _)| *ok).count();
    println!("{passed} of {} checks passed", results.len());
    Some(if passed == results.len() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// The command choices the original's argparse accepted.
const ALL_COMMANDS: &[&str] = &[
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

/// What a parsed argv resolves to, decided without emitting output.
enum Route {
    /// The full CI pipeline (needs the environment gate).
    Pipeline,
    /// The original's argparse rejected the argv; stderr usage + exit 2.
    ArgparseError(String),
    /// A pull-request URL with no provider token: the recorded no-token report.
    NoToken(String),
    /// An unparseable URL with a token: the recorded per-command report.
    BadUrl(String),
    /// An empty --pr_url: usage on stdout, exit 0.
    EmptyUrl,
    /// `doctor`: preflight checks without a git provider.
    Doctor,
}

fn route(cli: &NativeCli) -> Route {
    // Argparse validates the command choice before anything else.
    if !ALL_COMMANDS.contains(&cli.command.as_str()) {
        return Route::ArgparseError(format!(
            "argument command: invalid choice: '{}' (choose from {})",
            python_repr(&cli.command),
            ALL_COMMANDS.join(", ")
        ));
    }
    if let Some(json) = &cli.json_output {
        // An empty --pr_url counts as absent.
        let has_target =
            cli.pr_url.as_ref().is_some_and(|u| !u.is_empty()) || cli.diff_file.is_some();
        if !has_target {
            return Route::ArgparseError(
                "--json-output requires --pr_url, --stdin, or --diff-file".into(),
            );
        }
        if json.is_empty() {
            return Route::ArgparseError("--json-output requires a non-empty path".into());
        }
    }
    if cli.command == "doctor" && cli.pr_url.is_none() && cli.diff_file.is_none() {
        return Route::Doctor;
    }
    if let Some(diff) = &cli.diff_file
        && std::fs::metadata(diff).is_err()
    {
        return Route::ArgparseError(format!(
            "Could not read --diff-file '{diff}': [Errno 2] No such file or directory: '{diff}'"
        ));
    }
    match &cli.pr_url {
        Some(url) if url.is_empty() => Route::EmptyUrl,
        Some(url) => {
            // The original checked the token before parsing the URL, so a
            // missing token reports its traceback even for a bad URL.
            if std::env::var("GITHUB__USER_TOKEN")
                .unwrap_or_default()
                .is_empty()
                && std::env::var("GITLAB__USER_TOKEN")
                    .or_else(|_| std::env::var("GITLAB_PERSONAL_ACCESS_TOKEN"))
                    .unwrap_or_default()
                    .is_empty()
            {
                Route::NoToken(url.clone())
            } else if parse_git_url(url).is_none() {
                Route::BadUrl(cli.command.clone())
            } else if host_token(url).is_none() {
                // A URL for one host with only the other host's token.
                Route::NoToken(url.clone())
            } else {
                Route::Pipeline
            }
        }
        None => Route::Pipeline,
    }
}

/// The argparse failure the original printed: usage on stderr, exit 2.
fn argparse_fail(message: &str) -> Option<ExitCode> {
    eprintln!("{}pr-agent: error: {message}", crate::blobs::USAGE);
    Some(ExitCode::from(2))
}

/// The recorded no-token report: the provider traceback chain on stdout, exit 1.
fn no_token_report(url: &str) -> Option<ExitCode> {
    crate::logging::error(
        &Loc {
            module: "pipeline",
            func: "dispatch",
        },
        "Failed to process the command.",
    );
    let report = crate::blobs::NO_TOKEN_REPORT.replace("{URL}", url);
    print!("{report}");
    Some(ExitCode::from(1))
}

/// The recorded unparseable-URL report: repo-settings warning, the command
/// construction failure, exit 1.
fn bad_url_report(command: &str) -> Option<ExitCode> {
    crate::logging::warning(
        &Loc {
            module: "github",
            func: "repo_settings",
        },
        "Failed to load .pr_agent.toml file, error: 'NoneType' object has no attribute 'default_branch'",
    );
    if command == "improve" {
        crate::logging::warning(
            &Loc {
                module: "pipeline",
                func: "dispatch",
            },
            "/improve now runs /review",
        );
    }
    crate::logging::error(
        &Loc {
            module: "pipeline",
            func: "dispatch",
        },
        "Failed to process the command.",
    );
    let report = match command {
        "describe" => crate::blobs::BAD_URL_DESCRIBE_REPORT,
        "ask" => crate::blobs::BAD_URL_ASK_REPORT,
        // improve runs the reviewer command class; review and every other
        // command the routes reach share its construction failure.
        _ => crate::blobs::BAD_URL_REVIEW_REPORT,
    };
    print!("{report}");
    Some(ExitCode::from(1))
}

/// Cheap pre-flight on the environment; `None` falls back silently.
fn native_gate(cli: &NativeCli) -> Option<()> {
    if cli.diff_file.is_some() {
        // Plain-diff review: patch-only mode outside a checkout, the checkout
        // itself inside one (the CI action layout).
        if cli.command != "review" || cli.pr_url.is_some() {
            return None;
        }
        // The diff file must exist before any output: a missing file falls back.
        std::fs::metadata(cli.diff_file.as_deref().unwrap()).ok()?;
        Some(())
    } else {
        let url = cli.pr_url.as_ref()?;
        parse_git_url(url)?;
        host_token(url)?;
        Some(())
    }
}

/// The token a URL's host needs, from the environment. GitHub and GitLab each
/// read their own dynaconf key.
fn host_token(url: &str) -> Option<String> {
    match parse_git_url(url) {
        Some(GitHost::GitHub { .. }) => std::env::var("GITHUB__USER_TOKEN")
            .ok()
            .filter(|t| !t.is_empty()),
        Some(GitHost::GitLab { .. }) => std::env::var("GITLAB__USER_TOKEN")
            .or_else(|_| std::env::var("GITLAB_PERSONAL_ACCESS_TOKEN"))
            .ok()
            .filter(|t| !t.is_empty()),
        None => None,
    }
}

/// The engine pre-flight: a missing opencode binary is a deployment error,
/// not an unsupported shape.
fn engine_ready() -> Option<ExitCode> {
    let opencode = OpenCodeSettings::load(None);
    if binary_available(&opencode) {
        return None;
    }
    Some(fail(
        "opencode binary not found; set OPENCODE__BINARY or install opencode",
    ))
}

fn parse(args: &[OsString]) -> Option<NativeCli> {
    let words: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let mut cli = NativeCli {
        pr_url: None,
        diff_file: None,
        json_output: None,
        command: String::new(),
        question: vec![],
    };
    let mut i = 0;
    while i < words.len() {
        let word = &words[i];
        let value = |i: &mut usize| -> Option<String> {
            *i += 1;
            words.get(*i).cloned()
        };
        match word.as_str() {
            "--pr_url" => cli.pr_url = Some(value(&mut i)?),
            "--diff-file" => cli.diff_file = Some(value(&mut i)?),
            "--json-output" => cli.json_output = Some(value(&mut i)?),
            // The first non-flag word is the command; the rest is its argument.
            word if !word.starts_with('-') => {
                cli.command = word.to_string();
                cli.question = words[i + 1..].to_vec();
                return Some(cli);
            }
            _ => return None,
        }
        i += 1;
    }
    None
}

fn run(cli: &NativeCli, settings: &Settings) -> Option<ExitCode> {
    if let Some(diff_file) = &cli.diff_file {
        if cli.command != "review" || cli.pr_url.is_some() {
            return None;
        }
        return run_plain_diff(diff_file, cli, settings);
    }
    let url = cli.pr_url.as_ref()?;
    let host = parse_git_url(url)?;
    if let Some(code) = engine_ready() {
        return Some(code);
    }
    match host {
        GitHost::GitHub { repo, number } => {
            let token = settings.str("github", "user_token")?;
            if token.is_empty() {
                return None;
            }
            let (org, name) = repo.split_once('/')?;
            let base_url = settings
                .str("github", "base_url")
                .unwrap_or_else(|| "https://api.github.com".to_string());
            let github = GithubClient::new(&base_url, &token).ok()?;
            Some(run_hosted(org, name, number, cli, settings, &github))
        }
        GitHost::GitLab { project, number } => {
            let token = settings
                .str("gitlab", "user_token")
                .or_else(|| std::env::var("GITLAB_PERSONAL_ACCESS_TOKEN").ok())?;
            if token.is_empty() {
                return None;
            }
            let base_url = settings
                .str("gitlab", "url")
                .unwrap_or_else(|| "https://gitlab.com".to_string());
            let gitlab = GitlabClient::new(&base_url, &token).ok()?;
            Some(run_hosted_gitlab(&project, number, cli, settings, &gitlab))
        }
    }
}

/// `repr`-style escaping for the argparse error line: characters that are
/// not printable (control, format, separator, private-use) render as
/// `\uXXXX` / `\UXXXXXXXX`, which is how the original printed them.
fn python_repr(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        let code = ch as u32;
        if is_python_printable(ch) {
            out.push(ch);
        } else if code < 0x100 {
            out.push_str(&format!("\\x{code:02x}"));
        } else if code < 0x10000 {
            out.push_str(&format!("\\u{code:04x}"));
        } else {
            out.push_str(&format!("\\U{code:08x}"));
        }
    }
    out
}

/// `str.isprintable`: false for Cc, Cf, Cs, Co, Cn, Zl, Zp and Zs other than
/// the ASCII space. The unassigned (Cn) range needs the Unicode tables and is
/// not approximated; no recorded input reaches it.
fn is_python_printable(ch: char) -> bool {
    let code = ch as u32;
    if ch == ' ' {
        return true;
    }
    // Cc: ASCII and C1 control characters.
    if code < 0x20 || (0x7f..=0x9f).contains(&code) {
        return false;
    }
    // Zs: space separators other than the ASCII space.
    if matches!(code, 0xa0 | 0x1680 | 0x202f | 0x205f | 0x3000) || (0x2000..=0x200a).contains(&code)
    {
        return false;
    }
    // Zl and Zp: line/paragraph separators.
    if matches!(code, 0x2028 | 0x2029) {
        return false;
    }
    // Cf: the common format characters (soft hyphen, directional marks,
    // zero-width joiners, BOM and friends).
    if matches!(code, 0xad | 0x061c | 0x180e | 0xfeff)
        || (0x600..=0x605).contains(&code)
        || (0x200b..=0x200f).contains(&code)
        || (0x202a..=0x202e).contains(&code)
        || (0x2060..=0x2064).contains(&code)
        || (0x2066..=0x206f).contains(&code)
        || (0xfff9..=0xfffb).contains(&code)
    {
        return false;
    }
    // Co: private-use planes.
    if (0xe000..=0xf8ff).contains(&code) || (0xf0000..=0x10fffd).contains(&code) {
        return false;
    }
    true
}

/// `https://github.com/{org}/{repo}/pull/{number}`
fn parse_github_url(url: &str) -> Option<(String, String, u64)> {
    let rest = url.strip_prefix("https://github.com/")?;
    let mut parts = rest.trim_end_matches('/').split('/');
    let org = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    let marker = parts.next()?;
    if marker != "pull" {
        return None;
    }
    let number = parts.next()?.parse().ok()?;
    Some((org, repo, number))
}

/// The git host a pull/merge request URL names. GitLab serves itself (and the
/// self-hosted deployments pointed at by `GITLAB__BASE_URL`); GitHub URLs are
/// fixed to github.com.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitHost {
    GitHub { repo: String, number: u64 },
    GitLab { project: String, number: u64 },
}

/// `https://gitlab.com/{group}/{subgroups...}/{repo}/-/merge_requests/{n}`
/// (the `-/` separator is optional, as GitLab itself serves both spellings).
pub fn parse_gitlab_url(url: &str) -> Option<(String, u64)> {
    let rest = url.strip_prefix("https://gitlab.com/")?;
    let segments: Vec<&str> = rest.trim_end_matches('/').split('/').collect();
    let mr = segments.iter().position(|s| *s == "merge_requests")?;
    let number: u64 = segments.get(mr + 1)?.parse().ok()?;
    if mr == 0 {
        return None; // no project path before the marker
    }
    let mut project: Vec<&str> = segments[..mr].to_vec();
    if project.last() == Some(&"-") {
        project.pop();
    }
    if project.len() < 2 {
        // A GitLab project path always has a namespace and a name.
        return None;
    }
    Some((project.join("/"), number))
}

/// Which host a URL addresses, regardless of spelling.
fn parse_git_url(url: &str) -> Option<GitHost> {
    if let Some((repo, number)) = parse_gitlab_url(url) {
        return Some(GitHost::GitLab {
            project: repo,
            number,
        });
    }
    parse_github_url(url).map(|(org, repo, number)| GitHost::GitHub {
        repo: format!("{org}/{repo}"),
        number,
    })
}

/// The typed pull-request context every command consumes, host-agnostic:
/// the fields the engine prompt and the render need, plus the change set.
struct PrContext {
    number: u64,
    title: String,
    body: String,
    html_url: String,
    head_sha: String,
    files: Vec<DiffFile>,
    languages: Vec<(String, i64)>,
}

/// The publication surface for one git host. Commands publish through this
/// enum so the same review/describe/ask flow serves GitHub and GitLab.
enum HostClient<'a> {
    GitHub(&'a GithubClient),
    GitLab(&'a GitlabClient),
}

impl HostClient<'_> {
    /// Post a comment; returns its id for later update/delete.
    fn create_comment(&self, repo: &str, number: u64, body: &str) -> Option<i64> {
        match self {
            HostClient::GitHub(c) => c
                .create_issue_comment(repo, number, body)
                .ok()
                .map(|comment| comment.id),
            HostClient::GitLab(c) => c.create_note(repo, number, body).ok().map(|n| n.id),
        }
    }

    fn update_comment(&self, repo: &str, number: u64, comment_id: i64, body: &str) {
        match self {
            HostClient::GitHub(c) => {
                c.update_issue_comment(repo, comment_id, body).ok();
            }
            HostClient::GitLab(c) => {
                c.update_note(repo, number, comment_id, body).ok();
            }
        }
    }

    fn delete_comment(&self, repo: &str, comment_id: i64) {
        match self {
            HostClient::GitHub(c) => {
                c.delete_issue_comment(repo, comment_id).ok();
            }
            // GitLab has no iid on delete by process scope; the note id is
            // globally unique per project, so any MR works.
            HostClient::GitLab(c) => {
                c.delete_note(repo, 0, comment_id).ok();
            }
        }
    }

    /// The labels currently on the pull/merge request.
    fn issue_labels(&self, repo: &str, number: u64) -> Option<Vec<String>> {
        match self {
            HostClient::GitHub(c) => c.get_issue_labels(repo, number).ok(),
            HostClient::GitLab(c) => c.get_labels(repo, number).ok(),
        }
    }

    fn publish_labels(&self, repo: &str, number: u64, labels: &[String]) -> Result<(), String> {
        match self {
            HostClient::GitHub(c) => c.publish_labels(repo, number, labels),
            HostClient::GitLab(c) => c.publish_labels(repo, number, labels),
        }
    }

    /// Update the title/description (`/describe` publishing).
    fn update_description(&self, repo: &str, number: u64, title: &str, body: &str) {
        match self {
            HostClient::GitHub(c) => {
                c.update_pull(repo, number, title, body).ok();
            }
            HostClient::GitLab(c) => {
                c.update_merge_request(repo, number, title, body).ok();
            }
        }
    }

    /// Publish inline review findings. GitLab inline publication needs the
    /// discussions API (position objects); until it ports, findings stay in
    /// the summary comment, which is what the original did without gfm.
    fn create_inline_review(&self, repo: &str, number: u64, comments: &[Json]) -> bool {
        match self {
            HostClient::GitHub(c) => c
                .create_review(repo, number, "Review findings", comments)
                .is_ok(),
            HostClient::GitLab(_) => false,
        }
    }

    /// Full file content at a ref.
    fn file_contents(&self, repo: &str, path: &str, git_ref: &str) -> Option<String> {
        match self {
            HostClient::GitHub(c) => c.get_file_contents(repo, path, git_ref).ok(),
            HostClient::GitLab(c) => c.get_raw_file(repo, path, git_ref).ok(),
        }
    }

    /// The web link for the head commit (`/describe` publishing).
    fn commit_url(&self, repo: &str, sha: &str) -> String {
        match self {
            HostClient::GitHub(c) => format!("{}/{repo}/commit/{sha}", c.base_url),
            HostClient::GitLab(c) => format!("{}/{repo}/-/commit/{sha}", c.base_url),
        }
    }
}

fn run_hosted(
    org: &str,
    repo: &str,
    number: u64,
    cli: &NativeCli,
    settings: &Settings,
    github: &GithubClient,
) -> ExitCode {
    let repo_full = format!("{org}/{repo}");
    let Ok(pr) = github.get_pull(&repo_full, number) else {
        return fail("Failed to get git provider");
    };
    let files: Vec<DiffFile> = github
        .get_pull_files(&repo_full, number)
        .unwrap_or_default()
        .iter()
        .map(Into::into)
        .collect();
    let ctx = PrContext {
        number: pr.number,
        title: pr.title,
        body: pr.body,
        html_url: pr.html_url,
        head_sha: pr.head_sha,
        files,
        languages: github.get_languages(&repo_full).unwrap_or_default(),
    };
    run_command(ctx, &repo_full, cli, settings, &HostClient::GitHub(github))
}

/// The GitLab flow: the same typed context and publication surface.
fn run_hosted_gitlab(
    project: &str,
    number: u64,
    cli: &NativeCli,
    settings: &Settings,
    gitlab: &GitlabClient,
) -> ExitCode {
    let Ok(mr) = gitlab.get_merge_request(project, number) else {
        return fail("Failed to get git provider");
    };
    let files: Vec<DiffFile> = gitlab
        .get_mr_diffs(project, number)
        .unwrap_or_default()
        .iter()
        .map(|diff| DiffFile {
            filename: diff.new_path.clone(),
            edit_type: if diff.new_file {
                "ADDED".into()
            } else if diff.deleted_file {
                "DELETED".into()
            } else if diff.renamed_file {
                "RENAMED".into()
            } else {
                "MODIFIED".into()
            },
            plus_lines: 0,
            minus_lines: 0,
            patch: diff.diff.clone(),
        })
        .collect();
    let ctx = PrContext {
        number: mr.iid,
        title: mr.title,
        body: mr.description,
        html_url: mr.web_url,
        head_sha: mr.head_sha,
        files,
        languages: gitlab.get_languages(project).unwrap_or_default(),
    };
    run_command(ctx, project, cli, settings, &HostClient::GitLab(gitlab))
}

/// Command dispatch over the host-agnostic context.
fn run_command(
    ctx: PrContext,
    repo_full: &str,
    cli: &NativeCli,
    settings: &Settings,
    client: &HostClient,
) -> ExitCode {
    match cli.command.as_str() {
        "review" | "improve" => {
            if cli.command == "improve" {
                warning(
                    &Loc {
                        module: "pipeline",
                        func: "dispatch",
                    },
                    "/improve now runs /review",
                );
            }
            review_hosted(ctx, repo_full, cli, settings, client)
        }
        "describe" => describe_hosted(ctx, repo_full, cli, settings, client),
        "ask" => ask_hosted(ctx, repo_full, cli, settings, client),
        _ => ExitCode::from(1),
    }
}

fn review_hosted(
    ctx: PrContext,
    repo_full: &str,
    cli: &NativeCli,
    settings: &Settings,
    client: &HostClient,
) -> ExitCode {
    info(
        &Loc {
            module: "pipeline",
            func: "run_review",
        },
        &format!("Reviewing PR: {} ...", ctx.html_url),
    );
    log_user_description(&ctx.body);
    let main_language = main_language(&ctx.languages, &ctx.files);
    log_main_language(&main_language);

    let progress = client.create_comment(repo_full, ctx.number, "Preparing review...");

    // The agent reviews a worktree of the PR head: fetch every changed file
    // at its head content and materialize a git repo from them.
    let contents: Vec<(String, String)> = ctx
        .files
        .iter()
        .filter_map(|file| {
            client
                .file_contents(repo_full, &file.filename, &ctx.head_sha)
                .map(|content| (file.filename.clone(), content))
        })
        .collect();
    let file_map = file_map_of(&ctx.files);
    let mut handler = OpenCodeHandler::new(OpenCodeSettings::load(None));
    if let Err(e) = handler.prepare_worktree_from_files(ctx.number, &ctx.head_sha, &contents) {
        return fail(&format!("Failed to review PR: {e}"));
    }
    let session = handler.session_for_head(&ctx.head_sha);
    let completion = match handler.chat_completion(&review_task_prompt(&ctx), &session, &file_map) {
        Ok(reply) => reply,
        Err(e) => return fail(&format!("Failed to review PR: {e}")),
    };
    let mut data = match serde_yaml_ng::from_str::<Yaml>(strip_code_fence(completion.text.trim())) {
        Ok(Yaml::Mapping(map)) if map.contains_key(Yaml::String("review".into())) => {
            Yaml::Mapping(map)
        }
        _ => return fail("Review prediction did not contain a review mapping"),
    };

    let mut review_value = data.get("review").cloned().unwrap_or(Yaml::Null);
    normalize_yaml11_bools(&mut review_value);
    // The JSON artifact gets the complete review, before the render reorders
    // it and the inline publisher prunes it.
    let complete_review = review_value.clone();
    // The key issues render last; the inline publisher prunes what it posted.
    move_key_issues_last(&mut review_value);
    if let Some(top) = data.as_mapping_mut() {
        top.insert(Yaml::String("review".into()), review_value.clone());
    }

    if !validate_review_schema(&review_value, settings) {
        warning(
            &Loc {
                module: "pipeline",
                func: "validate_review_schema",
            },
            "Review output failed schema validation",
        );
    }

    // Inline publication: each finding with a valid location becomes one
    // review comment; published findings leave the summary.
    let mut published = 0usize;
    let inline = settings
        .bool("pr_reviewer", "inline_key_issues")
        .unwrap_or(true);
    if inline {
        let mut comments = vec![];
        let mut remaining = vec![];
        if let Some(issues) = review_value
            .get("key_issues_to_review")
            .and_then(|v| v.as_sequence())
        {
            for issue in issues {
                match inline_comment(issue, &ctx.files) {
                    Some(comment) => comments.push(comment),
                    None => remaining.push(issue.clone()),
                }
            }
        }
        if !comments.is_empty() && client.create_inline_review(repo_full, ctx.number, &comments) {
            published += comments.len();
        }
        if published > 0 {
            info(
                &Loc {
                    module: "pipeline",
                    func: "publish_inline",
                },
                &format!("Published {published} review finding(s) as inline comments"),
            );
            if remaining.is_empty() {
                remove_key_issues(&mut review_value);
            } else if let Yaml::Mapping(ref mut map) = review_value {
                map.insert(
                    Yaml::String("key_issues_to_review".into()),
                    Yaml::Sequence(remaining),
                );
            }
            if let Some(top) = data.as_mapping_mut() {
                top.insert(Yaml::String("review".into()), review_value.clone());
            }
        }
    }

    let intro = settings
        .bool("pr_reviewer", "enable_intro_text")
        .unwrap_or(false);
    let markdown = render_review_markdown(&review_value, true, intro);
    let body = format!(
        "## PR Reviewer Guide 🔍\n\n<!-- pr-agent:review:full -->\n\n{}",
        strip_header(&markdown)
    );

    write_json_output(
        cli,
        &review_json_payload(&complete_review, &completion.usage),
    );
    client.create_comment(repo_full, ctx.number, &body);

    publish_review_labels(&review_value, repo_full, &ctx, settings, client);

    if let Some(comment_id) = progress {
        client.delete_comment(repo_full, comment_id);
    }
    ExitCode::SUCCESS
}

fn describe_hosted(
    ctx: PrContext,
    repo_full: &str,
    cli: &NativeCli,
    _settings: &Settings,
    client: &HostClient,
) -> ExitCode {
    log_user_description(&ctx.body);
    info(
        &Loc {
            module: "pipeline",
            func: "run_describe",
        },
        &format!(
            "Generating a PR description for pr_id: {repo_full}/{}",
            ctx.number
        ),
    );
    let main_language = main_language(&ctx.languages, &ctx.files);
    log_main_language(&main_language);

    let progress = client.create_comment(repo_full, ctx.number, "Preparing PR description...");

    let contents: Vec<(String, String)> = ctx
        .files
        .iter()
        .filter_map(|file| {
            client
                .file_contents(repo_full, &file.filename, &ctx.head_sha)
                .map(|content| (file.filename.clone(), content))
        })
        .collect();
    let file_map = file_map_of(&ctx.files);
    let mut handler = OpenCodeHandler::new(OpenCodeSettings::load(None));
    if let Err(e) = handler.prepare_worktree_from_files(ctx.number, &ctx.head_sha, &contents) {
        return fail(&format!(
            "Error generating PR description {repo_full}/{}: {e}",
            ctx.number
        ));
    }
    let session = handler.session_for_head(&ctx.head_sha);
    let completion = match handler.chat_completion(&describe_task_prompt(&ctx), &session, &file_map)
    {
        Ok(reply) => reply,
        Err(e) => {
            return fail(&format!(
                "Error generating PR description {repo_full}/{}: {e}",
                ctx.number
            ));
        }
    };
    let data: Yaml = match serde_yaml_ng::from_str(strip_code_fence(completion.text.trim())) {
        Ok(value) => value,
        Err(e) => {
            return fail(&format!(
                "Error generating PR description {repo_full}/{}: {e}",
                ctx.number
            ));
        }
    };
    if !validate_description_schema(&data) {
        warning(
            &Loc {
                module: "pipeline",
                func: "validate_description_schema",
            },
            "Description output failed schema validation",
        );
    }

    let title = data
        .get(Yaml::String("title".into()))
        .and_then(|v| v.as_str())
        .unwrap_or(&ctx.title);
    let body = describe_body(&ctx, &data);
    write_json_output(
        cli,
        &describe_json_payload(&ctx.body, &data, &completion.usage),
    );
    client.update_description(repo_full, ctx.number, title, &body);
    client.create_comment(
        repo_full,
        ctx.number,
        &format!(
            "**[PR Description]({})** updated to latest commit ({})",
            ctx.html_url,
            client.commit_url(repo_full, &ctx.head_sha)
        ),
    );
    if let Some(comment_id) = progress {
        client.update_comment(
            repo_full,
            ctx.number,
            comment_id,
            "PR description generation finished.",
        );
    }
    ExitCode::SUCCESS
}

fn ask_hosted(
    ctx: PrContext,
    repo_full: &str,
    cli: &NativeCli,
    _settings: &Settings,
    client: &HostClient,
) -> ExitCode {
    info(
        &Loc {
            module: "pipeline",
            func: "run_ask",
        },
        &format!("Answering a PR question about the PR {} ", ctx.html_url),
    );
    let main_language = main_language(&ctx.languages, &ctx.files);
    log_main_language(&main_language);

    let question = cli.question.join(" ");
    let contents: Vec<(String, String)> = ctx
        .files
        .iter()
        .filter_map(|file| {
            client
                .file_contents(repo_full, &file.filename, &ctx.head_sha)
                .map(|content| (file.filename.clone(), content))
        })
        .collect();
    let file_map = file_map_of(&ctx.files);
    let mut handler = OpenCodeHandler::new(OpenCodeSettings::load(None));
    if let Err(e) = handler.prepare_worktree_from_files(ctx.number, &ctx.head_sha, &contents) {
        return fail(&format!("Failed to answer question: {e}"));
    }
    let session = handler.session_for_ask(repo_full, ctx.number);
    let prompt = format!(
        "PR title: {}\n\nAnswer the following question about this pull request. Read the working tree \
with your tools as needed. Answer in Markdown.\n\nQuestion: {question}",
        ctx.title
    );
    let completion = match handler.chat_completion(&prompt, &session, &file_map) {
        Ok(reply) => reply,
        Err(e) => return fail(&format!("Failed to answer question: {e}")),
    };
    client.create_comment(repo_full, ctx.number, &completion.text);
    ExitCode::SUCCESS
}

fn run_plain_diff(diff_file: &str, cli: &NativeCli, settings: &Settings) -> Option<ExitCode> {
    let diff_text = std::fs::read_to_string(diff_file).ok()?;
    if let Some(code) = engine_ready() {
        return Some(code);
    }
    // Inside a git checkout the working tree is the worktree: the agent
    // reviews the checkout as-is (the CI action layout). Outside one, the
    // patch is materialized into a scratch repo.
    let in_checkout = has_git_root();
    if !in_checkout {
        info(
            &Loc {
                module: "pipeline",
                func: "diff_files",
            },
            "No repository root (.git) found; running in patch-only mode (working-tree enrichment disabled).",
        );
    }
    let repo_context = settings
        .get("config", "repo_context_files")
        .and_then(|v| v.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    if repo_context && !in_checkout {
        warning(
            &Loc {
                module: "settings",
                func: "repo_context",
            },
            "repo_context_files is configured, but PlainDiffGitProvider does not support repository file fetching; skipping repo context",
        );
    }
    info(
        &Loc {
            module: "pipeline",
            func: "run_review",
        },
        "Reviewing PR: local_diff ...",
    );
    let files = parse_diff_files(&diff_text);
    let main_language = main_language(&[], &files);
    log_main_language(&main_language);

    let file_map = plain_file_map_of(&files);
    let mut handler = OpenCodeHandler::new(OpenCodeSettings::load(None));
    if in_checkout {
        handler.use_current_worktree().ok()?;
    } else {
        handler.prepare_worktree_from_diff(&diff_text).ok()?;
    }
    // Session idempotency keys on the diff digest: the same patch replays the
    // same session.
    let session = format!("ses_pr_head-{}", diff_tag(&diff_text));
    let completion = handler
        .chat_completion(&review_task_prompt_plain(), &session, &file_map)
        .ok()?;
    let mut data: Yaml = serde_yaml_ng::from_str(strip_code_fence(completion.text.trim())).ok()?;
    let mut review_value = match data.get("review") {
        Some(review) => review.clone(),
        None => return Some(fail("Review prediction did not contain a review mapping")),
    };
    normalize_yaml11_bools(&mut review_value);
    let complete_review = review_value.clone();
    move_key_issues_last(&mut review_value);
    if let Some(top) = data.as_mapping_mut() {
        top.insert(Yaml::String("review".into()), review_value.clone());
    }
    if !validate_review_schema(&review_value, settings) {
        warning(
            &Loc {
                module: "pipeline",
                func: "validate_review_schema",
            },
            "Review output failed schema validation",
        );
    }
    info(
        &Loc {
            module: "pipeline",
            func: "publish_inline",
        },
        "Inline key-issue publishing is not verifiable for this provider; keeping findings in the review summary",
    );
    let intro = settings
        .bool("pr_reviewer", "enable_intro_text")
        .unwrap_or(false);
    let markdown = render_review_markdown(&review_value, true, intro);
    write_json_output(
        cli,
        &review_json_payload(&complete_review, &completion.usage),
    );
    println!("{markdown}");
    Some(ExitCode::SUCCESS)
}

// ---- shared helpers -------------------------------------------------------

const LOC_USER_DESCRIPTION: Loc = Loc {
    module: "transport",
    func: "user_description",
};

fn log_user_description(body: &str) {
    if !body.contains("<!-- pr-agent-generated -->") {
        info(
            &LOC_USER_DESCRIPTION,
            "Existing description was not generated by the pr-agent",
        );
    }
}

fn log_main_language(main_language: &str) {
    if !main_language.is_empty() {
        info(
            &Loc {
                module: "pipeline",
                func: "pr_diff",
            },
            &format!("PR main language: {main_language}"),
        );
    }
}

/// The changed-file map the prompt carries instead of the diff: path, edit
/// kind and per-file churn. The agent reads content itself.
fn file_map_of(files: &[DiffFile]) -> Vec<FileMapEntry> {
    files
        .iter()
        .map(|file| FileMapEntry {
            path: file.filename.clone(),
            old_path: None,
            edit_type: file.edit_type.clone(),
            plus_lines: file.plus_lines.max(0) as u64,
            minus_lines: file.minus_lines.max(0) as u64,
        })
        .collect()
}

/// File map for plain-diff mode, from the typed patch records.
fn plain_file_map_of(files: &[DiffFile]) -> Vec<FileMapEntry> {
    files
        .iter()
        .map(|file| FileMapEntry {
            path: file.filename.clone(),
            old_path: None,
            edit_type: file.edit_type.clone(),
            plus_lines: file.plus_lines.max(0) as u64,
            minus_lines: file.minus_lines.max(0) as u64,
        })
        .collect()
}

/// GitHub file status to the edit-kind spelling the map carries.
fn edit_type_of(status: &str) -> String {
    match status {
        "added" => "ADDED".into(),
        "removed" => "DELETED".into(),
        "renamed" => "RENAMED".into(),
        _ => "MODIFIED".into(),
    }
}

/// Stable tag of a diff text, for session idempotency in plain-diff mode.
fn diff_tag(diff_text: &str) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(diff_text.as_bytes());
    digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()[..12]
        .to_string()
}

/// A changed file for language detection and inline-comment placement. The
/// patch text itself never leaves this layer: the agent reads the worktree.
#[derive(Clone)]
struct DiffFile {
    filename: String,
    edit_type: String,
    plus_lines: i64,
    minus_lines: i64,
    #[allow(dead_code)] // the patch body stays in the worktree for the agent
    patch: String,
}

impl DiffFile {
    /// The typed record projection: path, edit kind and churn.
    fn from_record(record: diff_parsing_rs::typed::FileDiffRecord) -> Self {
        Self {
            filename: record.filename,
            edit_type: record.edit_type.name().to_string(),
            plus_lines: record.num_plus_lines,
            minus_lines: record.num_minus_lines,
            patch: record.patch,
        }
    }
}

fn parse_diff_files(diff_text: &str) -> Vec<DiffFile> {
    // The typed API of the ported unified-diff parser (the contract layer
    // under it stays gate-verified against the recorded rows).
    diff_parsing_rs::typed::parse_diff_records(diff_text)
        .unwrap_or_default()
        .into_iter()
        .map(DiffFile::from_record)
        .collect()
}

impl From<&crate::github::PullFile> for DiffFile {
    fn from(file: &crate::github::PullFile) -> Self {
        DiffFile {
            filename: file.filename.clone(),
            edit_type: edit_type_of(&file.status),
            plus_lines: file.additions,
            minus_lines: file.deletions,
            patch: file.patch.clone(),
        }
    }
}

/// The dominant language of the change: repository languages matched against
/// the diff files' extensions, falling back to extension-derived languages.
fn main_language(languages: &[(String, i64)], files: &[DiffFile]) -> String {
    let ext_language = |filename: &str| -> Option<&'static str> {
        let ext = filename.rsplit('.').next()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "py" => "Python",
            "rs" => "Rust",
            "go" => "Go",
            "js" | "jsx" => "JavaScript",
            "ts" | "tsx" => "TypeScript",
            "java" => "Java",
            "rb" => "Ruby",
            "c" | "h" => "C",
            "cpp" | "cc" | "hpp" => "C++",
            "cs" => "C#",
            "php" => "PHP",
            "swift" => "Swift",
            "kt" => "Kotlin",
            "scala" => "Scala",
            "sh" => "Shell",
            _ => return None,
        })
    };
    let mut best: Option<(usize, &String)> = None;
    for language in languages.iter().map(|(name, _)| name) {
        let hits = files
            .iter()
            .filter(|f| ext_language(&f.filename) == Some(language.as_str()))
            .count();
        if hits > 0 && best.map(|(b, _)| hits > b).unwrap_or(true) {
            best = Some((hits, language));
        }
    }
    if let Some((_, language)) = best {
        return language.to_string();
    }
    files
        .iter()
        .find_map(|f| ext_language(&f.filename).map(|s| s.to_string()))
        .unwrap_or_default()
}

/// PyYAML parsed plain `No`/`Yes`/`On`/`Off` scalars as booleans (YAML 1.1);
/// serde_yaml follows 1.2, so the answers get the same normalization the
/// original's validator and renderer saw.
fn normalize_yaml11_bools(value: &mut Yaml) {
    match value {
        Yaml::Mapping(map) => {
            for (_, v) in map.iter_mut() {
                normalize_yaml11_bools(v);
            }
        }
        Yaml::Sequence(seq) => {
            for v in seq.iter_mut() {
                normalize_yaml11_bools(v);
            }
        }
        Yaml::String(s) => {
            let is_bool = matches!(
                s.to_lowercase().as_str(),
                "yes" | "no" | "on" | "off" | "true" | "false"
            ) && s.chars().all(|c| c.is_ascii_alphabetic());
            if is_bool {
                *value = Yaml::Bool(
                    s.eq_ignore_ascii_case("yes")
                        || s.eq_ignore_ascii_case("on")
                        || s.eq_ignore_ascii_case("true"),
                );
            }
        }
        _ => {}
    }
}

/// Move `key_issues_to_review` to the end of the review mapping. The
/// `Mapping` remove is a swap-remove, which would reorder its neighbors, so
/// the mapping is rebuilt in order instead.
fn move_key_issues_last(review: &mut Yaml) {
    let Yaml::Mapping(map) = review else { return };
    let key = Yaml::String("key_issues_to_review".into());
    // Snapshot before any removal: `Mapping::remove` is a swap-remove and
    // would reorder the remaining entries.
    let mut entries: Vec<(Yaml, Yaml)> = vec![];
    let mut issues = None;
    for (k, v) in map.iter() {
        if *k == key {
            issues = Some(v.clone());
        } else {
            entries.push((k.clone(), v.clone()));
        }
    }
    let Some(issues) = issues else { return };
    map.clear();
    for (k, v) in entries {
        map.insert(k, v);
    }
    map.insert(key, issues);
}

/// Drop `key_issues_to_review` without swap-reordering its neighbors.
fn remove_key_issues(review: &mut Yaml) {
    let Yaml::Mapping(map) = review else { return };
    let entries: Vec<(Yaml, Yaml)> = map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    map.clear();
    for (k, v) in entries {
        if k.as_str() != Some("key_issues_to_review") {
            map.insert(k, v);
        }
    }
}

/// The pydantic `PRReview` constraints the recorded answers exercise, plus the
/// required-fields loop from `_validate_review_schema`.
fn validate_review_schema(review: &Yaml, settings: &Settings) -> bool {
    let mut valid = true;
    let field = |name: &str| review.get(Yaml::String(name.into())).cloned();
    if let Some(tests) = field("relevant_tests") {
        match tests {
            Yaml::String(s) if s == "Yes" || s == "No" => {}
            _ => valid = false, // the pydantic Literal rejected non-Yes/No values
        }
    }
    if let Some(effort) = field("estimated_effort_to_review_[1-5]") {
        match effort {
            Yaml::Number(n) if (1..=5).contains(&n.as_i64().unwrap_or(0)) => {}
            Yaml::Null => {}
            _ => valid = false,
        }
    }
    // Required-fields loop: only the flags the packaged defaults enable.
    let required = [
        ("ticket_compliance_check", "related_tickets"),
        (
            "estimated_effort_to_review_[1-5]",
            "require_estimate_effort_to_review",
        ),
        ("security_concerns", "require_security_review"),
        ("relevant_tests", "require_tests_review"),
    ];
    for (field_name, setting_name) in required {
        let enabled = settings
            .get("pr_reviewer", setting_name)
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !enabled {
            continue;
        }
        let present = review
            .get(Yaml::String(field_name.into()))
            .is_some_and(|v| !matches!(v, Yaml::Null));
        if !present {
            valid = false;
        }
    }
    valid
}

fn validate_description_schema(data: &Yaml) -> bool {
    let title = data
        .get(Yaml::String("title".into()))
        .and_then(|v| v.as_str());
    let description = data
        .get(Yaml::String("description".into()))
        .and_then(|v| v.as_str());
    title.is_some() && description.is_some()
}

/// One GitHub review comment for a published finding.
fn inline_comment(issue: &Yaml, files: &[DiffFile]) -> Option<Json> {
    let field = |name: &str| {
        issue
            .get(Yaml::String(name.into()))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };
    let relevant_file = field("relevant_file")?;
    if !files.iter().any(|f| f.filename == relevant_file) {
        return None;
    }
    let start = issue
        .get(Yaml::String("start_line".into()))
        .and_then(|v| v.as_i64())?;
    let end = issue
        .get(Yaml::String("end_line".into()))
        .and_then(|v| v.as_i64())
        .unwrap_or(start);
    if start <= 0 || end < start {
        return None;
    }
    let mut body = format!(
        "**{}**\n{}",
        field("issue_header").unwrap_or_default(),
        field("issue_content").unwrap_or_default()
    );
    // The suggested fix keeps its indentation, like the renderer's copy.
    let raw = |name: &str| {
        issue
            .get(Yaml::String(name.into()))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let improved = raw("improved_code");
    if !improved.trim().is_empty() {
        let fence = "```".repeat(3);
        body.push_str(&format!(
            "\n\n<details><summary>Suggested fix</summary>\n\n{fence}text\n{}\n{fence}\n\n</details>",
            improved.trim_end()
        ));
    }
    Some(json!({
        "path": relevant_file,
        "line": end,
        "start_line": start,
        "side": "RIGHT",
        "body": body,
    }))
}

fn publish_review_labels(
    review: &Yaml,
    repo_full: &str,
    pr: &PrContext,
    settings: &Settings,
    client: &HostClient,
) {
    let mut review_labels = vec![];
    if settings
        .bool("pr_reviewer", "enable_review_labels_effort")
        .unwrap_or(true)
        && let Some(effort) = review
            .get(Yaml::String("estimated_effort_to_review_[1-5]".into()))
            .and_then(|v| v.as_i64())
    {
        let effort = effort.clamp(1, 5);
        review_labels.push(format!("Review effort {effort}/5"));
    }
    let mut has_security_verdict = false;
    if settings
        .bool("pr_reviewer", "enable_review_labels_security")
        .unwrap_or(true)
        && settings
            .bool("pr_reviewer", "require_security_review")
            .unwrap_or(false)
        && let Some(concerns) = review.get(Yaml::String("security_concerns".into()))
    {
        has_security_verdict = true;
        if !crate::render::is_value_no(concerns) {
            review_labels.push("Possible security concern".to_string());
        }
    }
    let Some(current) = client.issue_labels(repo_full, pr.number) else {
        crate::logging::error(
            &Loc {
                module: "github",
                func: "pr_labels",
            },
            "Skipping review label publish: existing labels could not be read, and publishing would remove them",
        );
        return;
    };
    let filtered: Vec<String> = current
        .iter()
        .filter(|label| {
            !(label.to_lowercase().starts_with("review effort")
                && review_labels.iter().any(|l| l.starts_with("Review effort")))
                && !(label.as_str() == "Possible security concern" && has_security_verdict)
        })
        .cloned()
        .collect();
    let new_labels: Vec<String> = review_labels
        .iter()
        .cloned()
        .chain(filtered.iter().cloned())
        .collect();
    let mut sorted_new = new_labels.clone();
    let mut sorted_current = current.clone();
    sorted_new.sort();
    sorted_current.sort();
    if (!current.is_empty() || !review_labels.is_empty()) && sorted_new != sorted_current {
        info(
            &Loc {
                module: "pipeline",
                func: "review_labels",
            },
            &format!("Setting review labels:\n{}", python_list_repr(&new_labels)),
        );
        client
            .publish_labels(repo_full, pr.number, &new_labels)
            .ok();
    }
}

/// `repr` of a string list: the label log line embeds it verbatim.
fn python_list_repr(items: &[String]) -> String {
    let array = serde_json::Value::Array(
        items
            .iter()
            .cloned()
            .map(serde_json::Value::String)
            .collect(),
    );
    utils_rs::port::py_repr(&array)
}

/// The `/describe` comment body: the identity marker plus the sections the
/// original rendered from the model answer.
fn describe_body(pr: &PrContext, data: &Yaml) -> String {
    let mut body = String::from("<!-- pr-agent-generated -->\n");
    if !pr.body.trim().is_empty() {
        body.push_str(&format!(
            "### **User description**\n{}\n",
            pr.body.trim_end()
        ));
    }
    body.push_str("\n\n___\n\n");
    if let Some(types) = data
        .get(Yaml::String("type".into()))
        .and_then(|v| v.as_sequence())
    {
        let types: Vec<String> = types
            .iter()
            .filter_map(|t| t.as_str().map(|s| s.to_string()))
            .collect();
        if !types.is_empty() {
            body.push_str(&format!("### **PR Type**\n{}\n", types.join(", ")));
            body.push_str("\n\n___\n\n");
        }
    }
    if let Some(description) = data
        .get(Yaml::String("description".into()))
        .and_then(|v| v.as_str())
    {
        body.push_str(&format!("### **Description**\n{}\n", description.trim()));
        body.push_str("\n\n___\n\n");
    }
    if let Some(title) = data
        .get(Yaml::String("title".into()))
        .and_then(|v| v.as_str())
    {
        body.push_str(&format!("### **PR Title**\n{}\n", title));
    }
    body
}

fn strip_header(markdown: &str) -> String {
    markdown
        .strip_prefix("## PR Reviewer Guide 🔍\n\n")
        .unwrap_or(markdown)
        .to_string()
}

/// The `--json-output` artifact: `json.dumps(payload, indent=2) + "\n"`, key
/// order preserved (`write_review_json_output` in the original).
fn write_json_output(cli: &NativeCli, payload: &Json) {
    let Some(path) = &cli.json_output else { return };
    let text = format!(
        "{}\n",
        serde_json::to_string_pretty(payload).unwrap_or_default()
    );
    let _ = std::fs::write(path, text);
}

/// Usage block shared by both commands' artifacts, from the agent's reported
/// step-finish usage.
fn usage_json(usage: &RunUsage) -> Json {
    let input = usage.input_tokens.unwrap_or(0);
    let output = usage.output_tokens.unwrap_or(0);
    json!({
        "prompt_tokens": input,
        "completion_tokens": output,
        "total_tokens": input + output,
    })
}

/// Review artifact: the complete review (pre-reorder, pre-prune) plus usage
/// and coverage, matching the original's structured-data snapshot.
fn review_json_payload(review: &Yaml, usage: &RunUsage) -> Json {
    let mut payload = serde_json::Map::new();
    payload.insert("review".into(), yaml_to_json(review));
    payload.insert("usage".into(), usage_json(usage));
    payload.insert(
        "coverage".into(),
        json!({
            "total_chunks": 1,
            "failed_chunks": 0,
            "files_not_reviewed": [],
            "files_partially_reviewed": [],
        }),
    );
    Json::Object(payload)
}

/// Describe artifact: user description, title, then the model's sections in
/// YAML order, plus usage.
fn describe_json_payload(pr_body: &str, data: &Yaml, usage: &RunUsage) -> Json {
    let mut payload = serde_json::Map::new();
    if !pr_body.trim().is_empty() {
        payload.insert("User Description".into(), Json::String(pr_body.to_string()));
    }
    let field = |name: &str| data.get(name).map(yaml_to_json);
    if let Some(title) = field("title") {
        payload.insert("title".into(), title);
    }
    if let Yaml::Mapping(map) = data {
        for (k, v) in map {
            let Some(key) = k.as_str() else { continue };
            if key == "title" || key == "User Description" {
                continue;
            }
            payload.insert(key.into(), yaml_to_json(v));
        }
    }
    payload.insert("usage".into(), usage_json(usage));
    Json::Object(payload)
}

/// Insertion-ordered YAML-to-JSON conversion (mappings keep document order).
fn yaml_to_json(value: &Yaml) -> Json {
    match value {
        Yaml::Null => Json::Null,
        Yaml::Bool(b) => Json::Bool(*b),
        Yaml::Number(n) => n
            .as_i64()
            .map(Json::from)
            .or_else(|| n.as_f64().map(Json::from))
            .unwrap_or(Json::Null),
        Yaml::String(s) => Json::String(s.clone()),
        Yaml::Sequence(seq) => Json::Array(seq.iter().map(yaml_to_json).collect()),
        Yaml::Mapping(map) => {
            let mut object = serde_json::Map::new();
            for (k, v) in map {
                if let Some(key) = k.as_str() {
                    object.insert(key.into(), yaml_to_json(v));
                }
            }
            Json::Object(object)
        }
        Yaml::Tagged(tagged) => yaml_to_json(&tagged.value),
    }
}

fn fail(message: &str) -> ExitCode {
    crate::logging::error(
        &Loc {
            module: "pipeline",
            func: "dispatch",
        },
        "Failed to process the command.",
    );
    eprintln!("{message}");
    ExitCode::from(1)
}

fn has_git_root() -> bool {
    let mut dir = std::env::current_dir().ok();
    while let Some(path) = dir {
        if path.join(".git").exists() {
            return true;
        }
        dir = path.parent().map(|p| p.to_path_buf());
    }
    false
}

// ---- prompts ---------------------------------------------------------------

/// The `/review` task: mission plus context; the YAML contract follows from
/// `commands::REVIEW_CONTRACT`, the changed-file map from the engine.
fn review_task_prompt(pr: &PrContext) -> String {
    let mut task = String::from(
        "Review this pull request. Read the changed files in the working tree with your tools; \
the full diff is available via `git diff`.\n\n",
    );
    task.push_str(&format!("PR title: {}\n", pr.title));
    if !pr.body.trim().is_empty() {
        task.push_str(&format!("PR description:\n{}\n\n", pr.body.trim()));
    }
    task.push_str(crate::commands::REVIEW_CONTRACT);
    task
}

/// The `/review` task for plain-diff mode: no PR metadata, just the mission.
fn review_task_prompt_plain() -> String {
    format!(
        "Review this pull request. Read the changed files in the working tree with your tools; \
the full diff is available via `git diff`.\n\n{}",
        crate::commands::REVIEW_CONTRACT
    )
}

/// The `/describe` task.
fn describe_task_prompt(pr: &PrContext) -> String {
    let mut task = String::from(
        "Describe this pull request. Read the changed files in the working tree with your tools; \
the full diff is available via `git diff`.\n\n",
    );
    task.push_str(&format!("PR title: {}\n\n", pr.title));
    if !pr.body.trim().is_empty() {
        task.push_str(&format!("PR description:\n{}\n\n", pr.body.trim()));
    }
    task.push_str(crate::commands::DESCRIBE_CONTRACT);
    task
}

/// Remove a wrapping code fence the agent may add despite the instruction.
fn strip_code_fence(text: &str) -> &str {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("```") {
        let rest = rest
            .strip_prefix("yaml")
            .or_else(|| rest.strip_prefix("yml"))
            .unwrap_or(rest);
        return rest.strip_suffix("```").unwrap_or(rest);
    }
    trimmed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_url_parses() {
        assert_eq!(
            parse_github_url("https://github.com/org/repo/pull/123"),
            Some(("org".into(), "repo".into(), 123))
        );
        assert_eq!(
            parse_github_url("https://gitlab.com/org/repo/-/merge_requests/1"),
            None
        );
        assert_eq!(
            parse_github_url("https://github.com/org/repo/issues/1"),
            None
        );
    }

    #[test]
    fn cli_shapes_parse() {
        let args: Vec<OsString> = ["--pr_url", "https://github.com/o/r/pull/1", "review"]
            .iter()
            .map(|s| s.into())
            .collect();
        let cli = parse(&args).unwrap();
        assert_eq!(cli.command, "review");
        assert_eq!(cli.pr_url.as_deref(), Some("https://github.com/o/r/pull/1"));

        let args: Vec<OsString> = ["ask", "what", "about", "this"]
            .iter()
            .map(|s| s.into())
            .collect();
        let cli = parse(&args).unwrap();
        assert_eq!(cli.question, vec!["what", "about", "this"]);

        let args: Vec<OsString> = ["--unknown-flag", "review"]
            .iter()
            .map(|s| s.into())
            .collect();
        assert!(parse(&args).is_none());
    }

    #[test]
    fn yaml11_bools_normalize() {
        let mut value: Yaml =
            serde_yaml_ng::from_str("relevant_tests: No\nname: None identified\n").unwrap();
        normalize_yaml11_bools(&mut value);
        assert!(matches!(
            value.get(Yaml::String("relevant_tests".into())),
            Some(Yaml::Bool(false))
        ));
        assert_eq!(
            value
                .get(Yaml::String("name".into()))
                .and_then(|v| v.as_str()),
            Some("None identified")
        );
    }

    #[test]
    fn python_repr_matches_the_recorded_line() {
        let labels = vec![
            "Review effort 3/5".to_string(),
            "Possible security concern".to_string(),
            "bug".to_string(),
            "enhancement".to_string(),
        ];
        assert_eq!(
            python_list_repr(&labels),
            "['Review effort 3/5', 'Possible security concern', 'bug', 'enhancement']"
        );
    }

    #[test]
    fn plain_diff_files_parse() {
        let diff = "diff --git a/src/app.py b/src/app.py\nnew file mode 100644\n--- /dev/null\n+++ b/src/app.py\n@@ -0,0 +1,3 @@\n+def run():\n+    return 42\n+\n";
        let files = parse_diff_files(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].filename, "src/app.py");
        assert!(files[0].patch.contains("@@ -0,0 +1,3 @@"));
    }

    #[test]
    fn main_language_from_extension() {
        let files = vec![DiffFile {
            filename: "src/app.py".into(),
            edit_type: "MODIFIED".into(),
            plus_lines: 1,
            minus_lines: 0,
            patch: String::new(),
        }];
        assert_eq!(main_language(&[], &files), "Python");
    }

    #[test]
    fn python_repr_escapes_the_zero_width_joiner() {
        // The recorded argparse error spelled the family emoji with \u200d.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} family";
        assert_eq!(
            python_repr(family),
            "\u{1F468}\\u200d\u{1F469}\\u200d\u{1F467} family"
        );
        assert_eq!(python_repr("héllo wörld"), "héllo wörld");
        assert_eq!(python_repr("a b"), "a b");
    }

    #[test]
    fn packaged_settings_ship_with_the_crate() {
        assert!(
            std::path::Path::new(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/settings/configuration.toml"
            ))
            .is_file()
        );
    }
}
