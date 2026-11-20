//! CLI integration tests: the real binary against in-process stub APIs and
//! a stub engine. Self-contained — no external servers, no recorded output;
//! each test asserts behavior (exit codes, published requests, artifacts).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

const BIN: &str = env!("CARGO_BIN_EXE_pr_agent");

/// One recorded stub request: method, path (with query), body.
type Request = (String, String, String);
type Requests = Arc<Mutex<Vec<Request>>>;
type Responder = Arc<dyn Fn(&str, &str, &str) -> (u16, String) + Send + Sync>;

// ---- the stub engine --------------------------------------------------------

/// A deterministic engine stub: `/bin/sh` script emitting the JSONL event
/// stream with fixed answers per task. The answers are single-line JSON
/// (valid YAML flow style), so no newline escaping is needed beyond quotes.
const STUB_ENGINE: &str = r#"#!/bin/sh
case "$*" in
  *"Review this pull request"*)
    case "$*" in
      *"PR title: Add calculator"*)
        case "$*" in
          *'Read `.pr-agent-review.diff` first'*) ;;
          *) echo 'Review prompt did not identify the diff' >&2; exit 1 ;;
        esac
        grep -Fq '+    return a / b' .pr-agent-review.diff || exit 1
        if grep -Fq 'diff --git' .pr-agent-review.diff; then
          grep -Fq -- '-    return 0' .pr-agent-review.diff || exit 1
        fi
        test -f src/calculator.py || exit 1
        ;;
    esac
    text='{"review":{"estimated_effort_to_review_[1-5]":3,"risk_level":"medium","merge_recommendation":"needs_review","key_issues_to_review":[{"relevant_file":"src/calculator.py","issue_header":"Division without zero check","issue_content":"The divide method does not guard against a zero divisor.","start_line":12,"end_line":12,"existing_code":"return a / b","improved_code":"raise ValueError","confidence":8}],"security_concerns":"None identified","relevant_tests":"No"}}'
    ;;
  *"Describe this pull request"*)
    text='{"type":["Enhancement"],"title":"Add calculator","description":"Adds a Calculator class.","pr_files":[{"filename":"src/calculator.py","changes_title":"Added calculator","changes_summary":"New Calculator class.","label":"Enhancement"}]}'
    ;;
  *)
    text='The PR adds a calculator with add and divide methods.'
    ;;
esac
escaped=$(printf '%s' "$text" | sed 's/\\/\\\\/g; s/"/\\"/g')
printf '{"type":"text","part":{"type":"text","text":"%s"}}\n' "$escaped"
printf '%s\n' '{"type":"step_finish","part":{"type":"step-finish","cost":0.000137,"tokens":{"input":10,"output":20,"reasoning":0}}}'
"#;

/// Write the engine stub for one test; the path is absolute so it resolves
/// from any working directory the engine later runs in.
fn engine_stub(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("pr-agent-tests").join(test);
    std::fs::create_dir_all(&dir).expect("create test dir");
    let path = dir.join("stub-engine.sh");
    std::fs::write(&path, STUB_ENGINE).expect("write engine stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("make engine stub executable");
    }
    path
}

// ---- the stub HTTP server ---------------------------------------------------

/// Serve HTTP on an ephemeral port, answering every request with `respond`
/// and recording it. Returns the port and the request log.
fn spawn_server(respond: Responder) -> (u16, Requests) {
    spawn_server_with_diff(respond, 200, GH_DIFF)
}

fn spawn_server_with_diff(
    respond: Responder,
    diff_status: u16,
    diff_body: &'static str,
) -> (u16, Requests) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub server");
    let port = listener.local_addr().expect("local addr").port();
    let requests: Requests = Arc::new(Mutex::new(Vec::new()));
    let log = requests.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let respond = respond.clone();
            let log = log.clone();
            std::thread::spawn(move || {
                handle_connection(stream, respond, log, diff_status, diff_body)
            });
        }
    });
    (port, requests)
}

fn handle_connection(
    mut stream: TcpStream,
    respond: Responder,
    log: Requests,
    diff_status: u16,
    diff_body: &str,
) {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    // Read until the header block ends, then the declared body.
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        match stream.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => return,
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.lines();
    let first = lines.next().unwrap_or_default().to_string();
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let path = path
        .strip_prefix("http://")
        .and_then(|absolute| absolute.find('/').map(|slash| absolute[slash..].to_owned()))
        .unwrap_or(path);
    let content_length = head
        .to_ascii_lowercase()
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while buf.len() < header_end + 4 + content_length {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    let body = String::from_utf8_lossy(&buf[header_end + 4..]).into_owned();
    log.lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push((method.clone(), path.clone(), body.clone()));
    let has_user_agent = lines.any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case("user-agent") && !value.trim().is_empty()
        })
    });
    let wants_diff = head.lines().any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case("accept")
                && matches!(
                    value.trim(),
                    "application/vnd.github.diff" | "application/vnd.github.v3.diff"
                )
        })
    });
    let (status, out) = if path.starts_with("/repos/") && !has_user_agent {
        (403, "User-Agent required".to_string())
    } else if path == "/repos/org/repo/pulls/123" && wants_diff {
        (diff_status, diff_body.to_string())
    } else {
        respond(&method, &path, &body)
    };
    let reason = match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        403 => "Forbidden",
        406 => "Not Acceptable",
        _ => "",
    };
    let extra_headers = if path == "/repos/org/repo/pulls/124/files" {
        let addr = stream.local_addr().unwrap();
        format!("Link: <http://{addr}/repos/org/repo/pulls/124/files?page=2>; rel=\"next\"\r\n")
    } else {
        String::new()
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra_headers}\r\n{out}",
        out.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

// ---- the stub APIs ----------------------------------------------------------

const GH_PR: &str = r#"{"id":123,"number":123,"url":"https://api.github.com/repos/org/repo/pulls/123","title":"Add calculator","body":"Adds a Calculator class.","html_url":"https://github.com/org/repo/pull/123","head":{"sha":"abc123","ref":"feature"},"base":{"ref":"main","sha":"base123"},"commits":2}"#;
const GH_DIFF: &str = "diff --git a/src/calculator.py b/src/calculator.py\n--- a/src/calculator.py\n+++ b/src/calculator.py\n@@ -11,2 +11,2 @@\n def divide(a, b):\n-    return 0\n+    return a / b\n";
const GH_FILES: &str = r#"[{"filename":"src/calculator.py","status":"added","additions":3,"deletions":0,"changes":3,"contents_url":"https://api.github.com/repos/org/repo/contents/src/calculator.py","patch":"@@ -0,0 +1,3 @@\n+def divide(a, b):\n+    return a / b\n"}]"#;
const FILE_CONTENT: &str = "def divide(a, b):\n    return a / b\n";

fn github_respond(method: &str, path: &str, _body: &str) -> (u16, String) {
    let ok = |value: String| (200, value);
    if path == "/user" {
        return ok(r#"{"login":"review-bot"}"#.into());
    }
    if method == "GET" && path.contains("/comments?") {
        return ok("[]".into());
    }
    if path.contains("/pulls/123/files") {
        return ok(GH_FILES.into());
    }
    if path.contains("/pulls/123/reviews") && method == "POST" {
        return ok("{}".into());
    }
    if path.contains("/pulls/123") {
        return ok(GH_PR.into()); // GET the PR, PATCH the description
    }
    if path.contains("/issues/123/comments") && method == "POST" {
        return (201, r#"{"id":101,"body":"posted"}"#.into());
    }
    if path.contains("/issues/123/labels") {
        return ok("[]".into()); // GET current, PUT replace
    }
    if path.contains("/issues/comments/") {
        return if method == "DELETE" {
            (204, String::new())
        } else {
            ok("{}".into()) // PATCH: progress comment update
        };
    }
    if path.contains("/languages") {
        return ok(r#"{"Python":100}"#.into());
    }
    if path.contains("/contents/") {
        return ok(serde_json::json!({
            "name": "calculator.py",
            "path": "src/calculator.py",
            "sha": "abc123",
            "size": FILE_CONTENT.len(),
            "url": "https://api.github.com/repos/org/repo/contents/src/calculator.py",
            "type": "file",
            "encoding": "base64",
            "content": b64(FILE_CONTENT),
            "_links": {"self": "https://api.github.com/repos/org/repo/contents/src/calculator.py"}
        })
        .to_string());
    }
    (404, r#"{"message":"not found"}"#.into())
}

const GL_MR: &str = r#"{"iid":5,"title":"Add calculator","description":"Adds a Calculator class.","web_url":"https://gitlab.com/org/repo/-/merge_requests/5","diff_refs":{"head_sha":"abc123def456"},"source_branch":"feature","target_branch":"main","author":{"username":"dev"},"labels":[]"#;
const GL_DIFFS: &str = r#"[{"new_path":"src/calculator.py","old_path":"src/calculator.py","diff":"@@ -0,0 +1,3 @@\n+def divide(a, b):\n+    return a / b\n","new_file":true,"deleted_file":false,"renamed_file":false}]"#;

fn gitlab_respond(method: &str, path: &str, _body: &str) -> (u16, String) {
    let ok = |value: String| (200, value);
    if path.trim_end_matches('?') == "/api/v4/user" {
        return ok(r#"{"username":"review-bot"}"#.into());
    }
    if method == "GET" && path.contains("/notes?") {
        return ok("[]".into());
    }
    if path.contains("/merge_requests/5/diffs") {
        return ok(GL_DIFFS.into());
    }
    if path.contains("/merge_requests/5/notes") && method == "POST" {
        return (201, r#"{"id":201,"body":"posted"}"#.into());
    }
    if path.contains("/merge_requests/5") {
        return ok(format!("{GL_MR}}}")); // GET the MR, PUT description/labels
    }
    if path.contains("/languages") {
        return ok(r#"{"Python":100}"#.into());
    }
    if path.contains("/repository/files/") {
        return ok(FILE_CONTENT.into()); // raw file content
    }
    (404, r#"{"message":"not found"}"#.into())
}

/// Standard-alphabet base64 (the encoding the GitHub contents API uses).
fn b64(data: &str) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.as_bytes().chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

// ---- the runner -------------------------------------------------------------

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.next() == Some('[') {
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Run the binary with a controlled environment: the listed overrides plus
/// a clean slate for every setting the pipeline reads.
fn run(args: &[&str], envs: &[(&str, &str)], cwd: Option<&std::path::Path>) -> Run {
    let mut cmd = Command::new(BIN);
    cmd.args(args);
    // Most fixtures exercise the legacy text transport.
    cmd.env("PR_AGENT_REVIEW_MCP", "false");
    for var in [
        "GITHUB__USER_TOKEN",
        "PR_REVIEWER__INLINE_KEY_ISSUES",
        "GITHUB__BASE_URL",
        "GITLAB__USER_TOKEN",
        "GITLAB_PERSONAL_ACCESS_TOKEN",
        "GITLAB__URL",
        "OPENCODE__BINARY",
        "OPENCODE__MODEL",
        "OPENCODE__AGENT",
        "OPENCODE__TIMEOUT_SECONDS",
        "PR_AGENT_USE_CODE_REVIEW_SKILL",
        "PR_AGENT_CODE_REVIEW_SKILL_DIR",
        "PR_AGENT_ALLOW_PROJECT_OPENCODE_CONFIG",
        "OPENCODE_CONFIG",
    ] {
        cmd.env_remove(var);
    }
    for (key, value) in envs {
        cmd.env(key, value);
    }
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let out = cmd.output().expect("run pr_agent");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: strip_ansi(&String::from_utf8_lossy(&out.stdout)),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn posted(log: &Requests, fragment: &str) -> bool {
    log.lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .any(|(_, path, body)| path.contains(fragment) || body.contains(fragment))
}

fn requested(log: &Requests, method: &str, fragment: &str) -> bool {
    log.lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .any(|(m, path, _)| m == method && path.contains(fragment))
}

// ---- the tests --------------------------------------------------------------

#[test]
fn review_keeps_findings_in_one_summary_by_default() {
    let (port, log) = spawn_server(Arc::new(github_respond));
    let out = run(
        &["--pr_url", "https://github.com/org/repo/pull/123", "review"],
        &[
            ("GITHUB__USER_TOKEN", "stub-token"),
            ("GITHUB__BASE_URL", &format!("http://127.0.0.1:{port}")),
            ("OPENCODE__BINARY", engine_stub("summary").to_str().unwrap()),
        ],
        None,
    );
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert!(!requested(&log, "POST", "pulls/123/reviews"));
    let log = log.lock().unwrap();
    let summaries: Vec<_> = log
        .iter()
        .filter(|(method, path, _)| {
            method == "POST" && path == "/repos/org/repo/issues/123/comments"
        })
        .collect();
    assert_eq!(summaries.len(), 1);
    assert!(summaries[0].2.contains("PR Reviewer Guide"));
    assert!(summaries[0].2.contains("Division without zero check"));
}

#[test]
fn review_posts_findings_and_labels() {
    let (port, log) = spawn_server(Arc::new(github_respond));
    let out = run(
        &["--pr_url", "https://github.com/org/repo/pull/123", "review"],
        &[
            ("GITHUB__USER_TOKEN", "stub-token"),
            ("PR_REVIEWER__INLINE_KEY_ISSUES", "true"),
            ("GITHUB__BASE_URL", &format!("http://127.0.0.1:{port}")),
            ("OPENCODE__BINARY", engine_stub("review").to_str().unwrap()),
        ],
        None,
    );
    assert_eq!(out.code, 0, "stdout:\n{}", out.stdout);
    assert!(
        out.stdout
            .contains("Reviewing PR: https://github.com/org/repo/pull/123")
    );
    assert!(out.stdout.contains("PR main language: Python"));
    assert!(
        !out.stdout
            .contains("Review output failed schema validation")
    );
    // The engine answered: one finding published inline, the summary posted.
    assert!(out.stdout.contains("Published 1 review finding(s)"));
    assert!(posted(&log, "PR Reviewer Guide"));
    assert_eq!(
        log.lock()
            .unwrap()
            .iter()
            .filter(|(method, path, _)| method == "GET" && path == "/repos/org/repo/pulls/123")
            .count(),
        2
    );
    assert!(requested(&log, "POST", "pulls/123/reviews"));
    // Labels: read current, publish the effort label.
    assert!(requested(&log, "GET", "issues/123/labels"));
    assert!(requested(&log, "PUT", "issues/123/labels"));
}

#[test]
fn malformed_review_is_reformatted_once_before_publication() {
    let (port, log) = spawn_server(Arc::new(github_respond));
    let path = engine_stub("review-reformat");
    std::fs::write(
        &path,
        STUB_ENGINE.replacen(
            "case \"$*\" in",
            r#"case "$*" in
  *"Reformat your previous review response"*)
    case "$*" in *"single YAML document"*) exit 2 ;; esac
    printf '%s\n' '{"type":"text","part":{"type":"text","text":"{\"review\":{\"estimated_effort_to_review_[1-5]\":2,\"security_concerns\":\"No\",\"relevant_tests\":\"No\",\"key_issues_to_review\":[]}}"}}'
    exit 0 ;;
  *"Review this pull request"*)
    case "$*" in *"Answer with a single valid JSON object"*) ;; *) exit 2 ;; esac
    printf '%s\n' '{"type":"text","part":{"type":"text","text":"review: [broken-secret"}}'
    exit 0 ;;
esac
case "$*" in"#,
            1,
        ),
    ).unwrap();
    let out = run(
        &["--pr_url", "https://github.com/org/repo/pull/123", "review"],
        &[
            ("GITHUB__BASE_URL", &format!("http://127.0.0.1:{port}")),
            ("OPENCODE__BINARY", path.to_str().unwrap()),
            ("GITHUB__USER_TOKEN", "broken-secret"),
        ],
        None,
    );
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert_eq!(out.stdout.matches("Asking OpenCode to reformat").count(), 1);
    assert!(out.stdout.contains("Review output excerpt:"));
    assert!(out.stdout.contains("review: [[redacted]"));
    assert!(!out.stdout.contains("broken-secret"));
    assert_eq!(
        log.lock()
            .unwrap()
            .iter()
            .filter(|(method, path, _)| method == "POST"
                && path == "/repos/org/repo/issues/123/comments")
            .count(),
        1
    );
}

#[test]
fn rerun_updates_owned_summary_and_passes_previous_findings_to_engine() {
    let (port, log) = spawn_server(Arc::new(|method, path, body| {
        if method == "GET" && path.contains("/issues/123/comments?") {
            return (200, serde_json::json!([
                {"id":101,"user":{"login":"review-bot"},"body":"## PR Reviewer Guide 🔍\n\n<!-- pr-agent:review:full -->\n\nPrevious summary sentinel"},
                {"id":102,"user":{"login":"other-user"},"body":"## PR Reviewer Guide 🔍\n\n<!-- pr-agent:review:full -->\n\nUntrusted sentinel"}
            ]).to_string());
        }
        if method == "GET" && path.contains("/pulls/123/comments?") {
            return (200, serde_json::json!([
                {"id":201,"user":{"login":"review-bot"},"path":"src/calculator.py","line":12,"body":"**Division without zero check**\nPrevious inline sentinel"}
            ]).to_string());
        }
        github_respond(method, path, body)
    }));
    let path = engine_stub("review-history");
    std::fs::write(
        &path,
        STUB_ENGINE.replacen(
            "case \"$*\" in",
            r#"case "$*" in
  *"Previous summary sentinel"*"Previous inline sentinel"*) ;;
  *) exit 1 ;;
esac
case "$*" in *"Untrusted sentinel"*) exit 1 ;; esac
case "$*" in"#,
            1,
        ),
    )
    .unwrap();
    let out = run(
        &["--pr_url", "https://github.com/org/repo/pull/123", "review"],
        &[
            ("GITHUB__USER_TOKEN", "stub-token"),
            ("PR_REVIEWER__INLINE_KEY_ISSUES", "true"),
            ("GITHUB__BASE_URL", &format!("http://127.0.0.1:{port}")),
            ("GITHUB__AGENT_LOGIN", "review-bot"),
            ("OPENCODE__BINARY", path.to_str().unwrap()),
        ],
        None,
    );
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert!(requested(&log, "PATCH", "issues/comments/101"));
    assert!(!requested(&log, "PATCH", "issues/comments/102"));
    assert!(!requested(&log, "POST", "issues/123/comments"));
    assert!(!requested(&log, "POST", "pulls/123/reviews"));
}

#[test]
fn github_review_uses_proxy_from_environment() {
    let (port, log) = spawn_server(Arc::new(github_respond));
    let proxy = format!("http://127.0.0.1:{port}");
    let engine = engine_stub("github-proxy");
    let out = run(
        &["--pr_url", "https://github.com/org/repo/pull/123", "review"],
        &[
            ("GITHUB__BASE_URL", "http://github-proxy-test.invalid"),
            ("GITHUB__USER_TOKEN", "stub-token"),
            ("OPENCODE__BINARY", engine.to_str().unwrap()),
            ("HTTP_PROXY", &proxy),
            ("http_proxy", &proxy),
            ("NO_PROXY", ""),
            ("no_proxy", ""),
        ],
        None,
    );
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert!(requested(&log, "POST", "issues/123/comments"));
}

#[test]
fn rerun_reviews_only_new_diff_and_reconciles_findings() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    const BEFORE: &str = "1111111111111111111111111111111111111111";
    const AFTER: &str = "2222222222222222222222222222222222222222";
    const REWRITTEN: &str = "3333333333333333333333333333333333333333";
    const EMPTY: &str = "4444444444444444444444444444444444444444";
    let run_number = Arc::new(AtomicUsize::new(0));
    let stage = run_number.clone();
    let summary = Arc::new(Mutex::new(String::new()));
    let stored_summary = summary.clone();
    let comparison = format!("/compare/{BEFORE}...{AFTER}");
    let diverged = format!("/compare/{AFTER}...{REWRITTEN}");
    let (port, log) = spawn_server(Arc::new(move |method, path, body| {
        let stage = stage.load(Ordering::SeqCst);
        if method == "GET" && path == "/repos/org/repo/pulls/123" {
            let sha = [BEFORE, AFTER, REWRITTEN, EMPTY][stage];
            return (200, GH_PR.replace("abc123", sha));
        }
        if method == "GET" && path.contains(&comparison) {
            return (200, serde_json::json!({
                "merge_base_commit": {"sha": BEFORE},
                "files": [{
                    "filename": "src/calculator.py", "status": "modified",
                    "additions": 1, "deletions": 1, "changes": 2,
                    "contents_url": "https://api.github.com/repos/org/repo/contents/src/calculator.py",
                    "patch": "@@ -1 +1 @@\n-before\n+delta-only"
                }]
            }).to_string());
        }
        if method == "GET" && path.contains(&diverged) {
            return (
                200,
                serde_json::json!({"merge_base_commit":{"sha":BEFORE}}).to_string(),
            );
        }
        if method == "GET" && path.contains(&format!("/compare/{REWRITTEN}...{EMPTY}")) {
            return (
                200,
                serde_json::json!({"merge_base_commit":{"sha":REWRITTEN},"files":[]}).to_string(),
            );
        }
        if method == "GET" && path.contains("/pulls/123/files") {
            let mut files: Vec<serde_json::Value> = serde_json::from_str(GH_FILES).unwrap();
            let mut untouched = files[0].clone();
            untouched["filename"] = "src/untouched.py".into();
            files.push(untouched);
            return (200, serde_json::to_string(&files).unwrap());
        }
        if method == "GET" && path.contains("/issues/123/comments?") {
            let summary = stored_summary.lock().unwrap();
            let comments = if summary.is_empty() {
                serde_json::json!([])
            } else {
                serde_json::json!([{"id":101,"user":{"login":"review-bot"},"body":*summary}])
            };
            return (200, comments.to_string());
        }
        if (method == "POST" && path == "/repos/org/repo/issues/123/comments")
            || (method == "PATCH" && path == "/repos/org/repo/issues/comments/101")
        {
            let body: serde_json::Value = serde_json::from_str(body).unwrap();
            *stored_summary.lock().unwrap() = body["body"].as_str().unwrap().to_owned();
        }
        github_respond(method, path, body)
    }));

    let engine = engine_stub("incremental-review");
    std::fs::write(&engine, r#"#!/bin/sh
set -e
case "$*" in
  *"incremental review"*)
    grep -Fq '+delta-only' .pr-agent-review.diff
    test ! -f src/untouched.py
    case "$*" in
      *"Old touched issue"*) ;;
      *) exit 1 ;;
    esac
    text='{"review":{"key_issues_to_review":[{"relevant_file":"src/calculator.py","issue_header":"New issue","issue_content":"A regression.","start_line":1,"end_line":1}]}}'
    ;;
  *)
    grep -Fq 'diff --git' .pr-agent-review.diff
    test -f src/untouched.py
    text='{"review":{"key_issues_to_review":[{"relevant_file":"src/calculator.py","issue_header":"Old touched issue","issue_content":"To fix.","start_line":1,"end_line":1},{"relevant_file":"src/untouched.py","issue_header":"Unchanged issue","issue_content":"Still applies.","start_line":1,"end_line":1}]}}'
    ;;
esac
escaped=$(printf '%s' "$text" | sed 's/\\/\\\\/g; s/"/\\"/g')
printf '{"type":"text","part":{"type":"text","text":"%s"}}\n' "$escaped"
"#).unwrap();
    let base_url = format!("http://127.0.0.1:{port}");
    let review = || {
        run(
            &["--pr_url", "https://github.com/org/repo/pull/123", "review"],
            &[
                ("GITHUB__BASE_URL", &base_url),
                ("GITHUB__USER_TOKEN", "stub-token"),
                ("GITHUB__AGENT_LOGIN", "review-bot"),
                ("OPENCODE__BINARY", engine.to_str().unwrap()),
            ],
            None,
        )
    };

    let first = review();
    assert_eq!(first.code, 0, "{}{}", first.stdout, first.stderr);
    assert!(
        summary
            .lock()
            .unwrap()
            .contains("pr-agent:review:state:v1:")
    );

    run_number.store(1, Ordering::SeqCst);
    let next = review();
    assert_eq!(next.code, 0, "{}{}", next.stdout, next.stderr);
    assert!(next.stdout.contains("Reviewing changes from"));
    assert!(requested(&log, "PATCH", "issues/comments/101"));
    {
        let summary = summary.lock().unwrap();
        let (report, changes) = summary.split_once("Changes since last review").unwrap();
        assert!(report.contains("Unchanged issue"));
        assert!(report.contains("New issue"));
        assert!(!report.contains("Old touched issue"));
        assert!(changes.contains("**New** · `src/calculator.py` · New issue"));
        assert!(changes.contains("**Resolved** · `src/calculator.py` · Old touched issue"));
    }

    let same = review();
    assert_eq!(same.code, 0, "{}{}", same.stdout, same.stderr);
    assert!(same.stdout.contains("already has a published review"));

    run_number.store(2, Ordering::SeqCst);
    let rewritten = review();
    assert_eq!(
        rewritten.code, 0,
        "{}{}",
        rewritten.stdout, rewritten.stderr
    );
    assert!(rewritten.stdout.contains("reviewing the full pull request"));
    assert!(requested(
        &log,
        "GET",
        &format!("/compare/{AFTER}...{REWRITTEN}")
    ));

    run_number.store(3, Ordering::SeqCst);
    let empty = review();
    assert_eq!(empty.code, 0, "{}{}", empty.stdout, empty.stderr);
    assert!(
        empty
            .stdout
            .contains("No file changes since the last reviewed head")
    );
    let repeated = review();
    assert!(repeated.stdout.contains("already has a published review"));
}

#[test]
fn review_uses_file_patches_when_github_rejects_the_full_diff() {
    let (port, log) = spawn_server_with_diff(
        Arc::new(github_respond),
        406,
        r#"{"message":"Sorry, the diff exceeded the maximum number of lines (20000)"}"#,
    );
    let out = run(
        &["--pr_url", "https://github.com/org/repo/pull/123", "review"],
        &[
            ("GITHUB__USER_TOKEN", "stub-token"),
            ("GITHUB__BASE_URL", &format!("http://127.0.0.1:{port}")),
            (
                "OPENCODE__BINARY",
                engine_stub("large-github-diff").to_str().unwrap(),
            ),
        ],
        None,
    );
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert!(out.stdout.contains("Reviewing available per-file patches"));
    assert!(log.lock().unwrap().iter().any(|(method, path, body)| {
        method == "POST"
            && path == "/repos/org/repo/issues/123/comments"
            && body.contains("Division without zero check")
            && body.contains("Coverage may be incomplete")
    }));
}

#[test]
fn review_reports_github_http_errors() {
    let (port, _) = spawn_server(Arc::new(|_, _, _| (403, "Forbidden".to_string())));
    let out = run(
        &["--pr_url", "https://github.com/org/repo/pull/123", "review"],
        &[
            ("GITHUB__USER_TOKEN", "stub-token"),
            ("GITHUB__BASE_URL", &format!("http://127.0.0.1:{port}")),
            (
                "OPENCODE__BINARY",
                engine_stub("github-error").to_str().unwrap(),
            ),
        ],
        None,
    );
    assert_eq!(out.code, 1);
    assert!(
        out.stderr
            .contains("Failed to fetch GitHub PR org/repo#123: GitHubError: 403 Forbidden")
    );
}

#[test]
fn review_writes_json_artifact() {
    let (port, _log) = spawn_server(Arc::new(github_respond));
    let dir = std::env::temp_dir().join("pr-agent-tests/json-artifact");
    std::fs::create_dir_all(&dir).unwrap();
    let artifact = dir.join("review.json");
    let _ = std::fs::remove_file(&artifact);
    let out = run(
        &[
            "--pr_url",
            "https://github.com/org/repo/pull/123",
            "--json-output",
            "review.json",
            "review",
        ],
        &[
            ("GITHUB__USER_TOKEN", "stub-token"),
            ("GITHUB__BASE_URL", &format!("http://127.0.0.1:{port}")),
            ("OPENCODE__BINARY", engine_stub("json").to_str().unwrap()),
        ],
        Some(&dir),
    );
    assert_eq!(out.code, 0, "stdout:\n{}", out.stdout);
    let artifact = std::fs::read_to_string(&artifact).expect("json artifact");
    assert!(artifact.contains("\"review\""));
    assert!(artifact.contains("\"total_tokens\": 30"));
}

#[test]
fn describe_updates_title_and_description() {
    let (port, log) = spawn_server(Arc::new(github_respond));
    let out = run(
        &[
            "--pr_url",
            "https://github.com/org/repo/pull/123",
            "describe",
        ],
        &[
            ("GITHUB__USER_TOKEN", "stub-token"),
            ("GITHUB__BASE_URL", &format!("http://127.0.0.1:{port}")),
            (
                "OPENCODE__BINARY",
                engine_stub("describe").to_str().unwrap(),
            ),
        ],
        None,
    );
    assert_eq!(out.code, 0, "stdout:\n{}", out.stdout);
    assert!(
        out.stdout
            .contains("Generating a PR description for pr_id: org/repo/123")
    );
    // The PR title/body were updated with the model answer.
    assert!(posted(&log, "Add calculator"));
}

#[test]
fn ask_answers_in_a_comment() {
    let (port, log) = spawn_server(Arc::new(github_respond));
    let out = run(
        &[
            "--pr_url",
            "https://github.com/org/repo/pull/123",
            "ask",
            "what does this PR change",
        ],
        &[
            ("GITHUB__USER_TOKEN", "stub-token"),
            ("GITHUB__BASE_URL", &format!("http://127.0.0.1:{port}")),
            ("OPENCODE__BINARY", engine_stub("ask").to_str().unwrap()),
        ],
        None,
    );
    assert_eq!(out.code, 0, "stdout:\n{}", out.stdout);
    assert!(out.stdout.contains("Answering a PR question"));
    assert!(posted(&log, "calculator with add and divide"));
}

#[test]
fn github_reads_all_pull_file_pages() {
    let (port, _) = spawn_server(Arc::new(|_, path, _| {
        let filename = if path.contains("page=2") {
            "second.rs"
        } else {
            "first.rs"
        };
        (
            200,
            serde_json::json!([{"filename": filename, "status": "added", "additions": 1, "deletions": 0, "changes": 1, "contents_url": "https://api.github.com/repos/org/repo/contents/fixture"}]).to_string(),
        )
    }));
    let client =
        pr_agent::github::GithubClient::new(&format!("http://127.0.0.1:{port}"), "stub-token")
            .unwrap();
    let files = client.get_pull_files("org/repo", 124).unwrap();
    let filenames: Vec<_> = files.iter().map(|file| file.filename.as_str()).collect();
    assert_eq!(filenames, ["first.rs", "second.rs"]);
}

#[test]
fn gitlab_review_compares_new_head_after_checking_ancestry() {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

    const BEFORE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const AFTER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let state =
        URL_SAFE_NO_PAD.encode(serde_json::json!({"head": BEFORE, "findings": []}).to_string());
    let (port, log) = spawn_server(Arc::new(move |method, path, body| {
        if method == "GET" && path.contains("/repository/merge_base?") {
            return (200, serde_json::json!({"id": BEFORE}).to_string());
        }
        if method == "GET" && path.contains("/repository/compare?") {
            return (
                200,
                serde_json::json!({
                    "compare_timeout": false,
                    "diffs": serde_json::from_str::<serde_json::Value>(GL_DIFFS).unwrap()
                })
                .to_string(),
            );
        }
        if method == "GET" && path.contains("/notes?") {
            let body = format!(
                "## PR Reviewer Guide 🔍\n\n<!-- pr-agent:review:full -->\n\n<!-- pr-agent:review:state:v1:{state} -->"
            );
            return (
                200,
                serde_json::json!([{"id":201,"author":{"username":"review-bot"},"body":body}])
                    .to_string(),
            );
        }
        if method == "GET" && path.trim_end_matches('?').ends_with("/merge_requests/5") {
            return (200, format!("{}}}", GL_MR.replace("abc123def456", AFTER)));
        }
        gitlab_respond(method, path, body)
    }));
    let base_url = format!("http://127.0.0.1:{port}");
    let engine = engine_stub("gitlab-incremental");
    let out = run(
        &[
            "--pr_url",
            "https://gitlab.com/org/repo/-/merge_requests/5",
            "review",
        ],
        &[
            ("GITLAB__USER_TOKEN", "stub-token"),
            ("GITLAB__URL", &base_url),
            ("OPENCODE__BINARY", engine.to_str().unwrap()),
        ],
        None,
    );
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert!(out.stdout.contains("Reviewing changes from"));
    assert!(requested(
        &log,
        "GET",
        &format!("merge_base?refs%5B%5D={BEFORE}&refs%5B%5D={AFTER}")
    ));
    assert!(requested(
        &log,
        "GET",
        &format!("compare?from={BEFORE}&to={AFTER}&straight=true")
    ));
    assert!(requested(&log, "PUT", "merge_requests/5/notes/201"));
}

#[test]
fn gitlab_review_posts_a_note() {
    let (port, log) = spawn_server(Arc::new(gitlab_respond));
    let out = run(
        &[
            "--pr_url",
            "https://gitlab.com/org/repo/-/merge_requests/5",
            "review",
        ],
        &[
            ("GITLAB__USER_TOKEN", "stub-token"),
            ("GITLAB__URL", &format!("http://127.0.0.1:{port}")),
            ("OPENCODE__BINARY", engine_stub("gitlab").to_str().unwrap()),
        ],
        None,
    );
    assert_eq!(
        out.code, 0,
        "stdout:\n{}\nstderr:\n{}",
        out.stdout, out.stderr
    );
    assert!(
        out.stdout
            .contains("Reviewing PR: https://gitlab.com/org/repo/-/merge_requests/5")
    );
    assert!(requested(&log, "POST", "merge_requests/5/notes"));
    // The note body is form-encoded, so match inside the words.
    assert!(posted(&log, "Reviewer"));
    assert!(posted(&log, "Guide"));
}

#[test]
fn missing_token_fails_with_guidance() {
    let (_port, _log) = spawn_server(Arc::new(github_respond));
    let out = run(
        &["--pr_url", "https://github.com/org/repo/pull/123", "review"],
        &[],
        None,
    );
    assert_eq!(out.code, 1);
    assert!(out.stdout.contains("no git host token configured"));
    assert!(out.stdout.contains("GITHUB__USER_TOKEN"));
    assert!(out.stdout.contains("https://github.com/org/repo/pull/123"));
}

#[test]
fn unparseable_url_fails() {
    let out = run(
        &["--pr_url", "not-a-url", "review"],
        &[("GITHUB__USER_TOKEN", "stub-token")],
        None,
    );
    assert_eq!(out.code, 1);
    assert!(out.stdout.contains("Failed to process the command"));
}

#[test]
fn unknown_command_is_an_argparse_error() {
    let out = run(
        &["--pr_url", "https://github.com/org/repo/pull/123", "bogus"],
        &[("GITHUB__USER_TOKEN", "stub-token")],
        None,
    );
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("invalid choice: \"bogus\""));
}

#[test]
fn plain_diff_review_runs_patch_only() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    std::fs::write(
        dir.join("fixture.diff"),
        "diff --git a/src/app.py b/src/app.py\nnew file mode 100644\nindex 0000000..1111111\n--- /dev/null\n+++ b/src/app.py\n@@ -0,0 +1,2 @@\n+def run():\n+    return 42\n",
    )
    .unwrap();
    let out = run(
        &["--diff-file", "fixture.diff", "review"],
        &[("OPENCODE__BINARY", engine_stub("plain").to_str().unwrap())],
        Some(dir),
    );
    assert_eq!(out.code, 0, "stdout:\n{}", out.stdout);
    assert!(out.stdout.contains("patch-only mode"));
    assert!(out.stdout.contains("Reviewing PR: local_diff"));
}

#[test]
fn doctor_reports_json_error_on_nonzero_exit() {
    let path = engine_stub("doctor-json-error");
    std::fs::write(
        &path,
        r#"#!/bin/sh
printf '%s\n' '{"type":"error","error":{"message":"Agent not found: pr-reviewer"}}'
exit 1
"#,
    )
    .unwrap();
    let out = run(
        &["doctor"],
        &[("OPENCODE__BINARY", path.to_str().unwrap())],
        None,
    );
    assert_eq!(out.code, 1);
    assert!(out.stdout.contains("Agent not found: pr-reviewer"));
}

#[test]
fn enabled_review_skill_requires_installed_files() {
    let path = engine_stub("missing-review-skill");
    let missing = path.parent().unwrap().join("missing");
    let out = run(
        &["doctor"],
        &[
            ("OPENCODE__BINARY", path.to_str().unwrap()),
            ("PR_AGENT_USE_CODE_REVIEW_SKILL", "true"),
            ("PR_AGENT_CODE_REVIEW_SKILL_DIR", missing.to_str().unwrap()),
        ],
        None,
    );
    assert_eq!(out.code, 1);
    assert!(
        out.stdout
            .contains("code-review-skill is enabled but SKILL.md is missing")
    );
}

#[test]
fn doctor_checks_binary_and_model() {
    let out = run(
        &["doctor"],
        &[("OPENCODE__BINARY", engine_stub("doctor").to_str().unwrap())],
        None,
    );
    assert_eq!(out.code, 0, "stdout:\n{}", out.stdout);
    assert!(out.stdout.contains("model completion succeeded"));
    assert!(out.stdout.contains("2 of 2 checks passed"));
}

#[test]
fn doctor_isolates_engine_environment_and_warns_on_project_config_opt_in() {
    let project = tempfile::tempdir().unwrap();
    let config = project.path().join(".open-pr-agent");
    std::fs::create_dir(&config).unwrap();
    std::fs::write(
        config.join("opencode.jsonc"),
        r#"{"username":"project-config-sentinel"}"#,
    )
    .unwrap();

    let engine = engine_stub("doctor-env-isolation");
    std::fs::write(&engine, r#"#!/bin/sh
set -eu
test -z "${GITHUB__USER_TOKEN+x}${GITLAB__USER_TOKEN+x}${PR_AGENT_LLM_API_KEY+x}${PRIVATE_CI_SECRET+x}"
test "$HTTPS_PROXY" = "http://proxy.invalid:8080"
test "$SSL_CERT_FILE" = "/operator/ca.pem"
test "$OPENCODE_DISABLE_PROJECT_CONFIG" = 1
test "$XDG_CONFIG_HOME" != "/operator/xdg"
test -d "$XDG_CONFIG_HOME"
case "$*" in
  *--standalone*) ;;
  *) exit 1 ;;
esac
grep -Fq '/operator/xdg/opencode/*' "$OPENCODE_CONFIG"
if grep -q project-config-sentinel "$OPENCODE_CONFIG"; then
    echo loaded > project-config-state
else
    echo ignored > project-config-state
fi
printf '%s\n' '{"type":"text","part":{"type":"text","text":"ok"}}'
printf '%s\n' "$OPENCODE_CONFIG" > engine-config-path
printf '%s\n' "$XDG_CONFIG_HOME" > engine-config-home
"#).unwrap();
    let mut script = std::fs::read_to_string(&engine).unwrap();
    for filename in [
        "engine-config-path",
        "engine-config-home",
        "project-config-state",
    ] {
        script = script.replace(
            &format!("> {filename}"),
            &format!("> '{}'", project.path().join(filename).display()),
        );
    }
    std::fs::write(&engine, script).unwrap();

    for (opt_in, expect_warning) in [("false", false), ("true", true)] {
        let out = run(
            &["doctor"],
            &[
                ("OPENCODE__BINARY", engine.to_str().unwrap()),
                ("GITHUB_ACTIONS", "true"),
                ("GITHUB__USER_TOKEN", "github-secret"),
                ("GITLAB__USER_TOKEN", "gitlab-secret"),
                ("PR_AGENT_LLM_API_KEY", "model-secret"),
                ("PR_AGENT_LLM_BASE_URL", "http://llm.invalid"),
                ("PR_AGENT_MODEL", "test-model"),
                ("PRIVATE_CI_SECRET", "private-secret"),
                ("HTTPS_PROXY", "http://proxy.invalid:8080"),
                ("SSL_CERT_FILE", "/operator/ca.pem"),
                ("XDG_CONFIG_HOME", "/operator/xdg"),
                ("PR_AGENT_ALLOW_PROJECT_OPENCODE_CONFIG", opt_in),
            ],
            Some(project.path()),
        );
        assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
        assert_eq!(
            out.stderr
                .contains("::warning::Project OpenCode config is enabled"),
            expect_warning
        );
        let state = std::fs::read_to_string(project.path().join("project-config-state")).unwrap();
        assert_eq!(
            state.trim(),
            if expect_warning { "loaded" } else { "ignored" }
        );
        for filename in ["engine-config-path", "engine-config-home"] {
            let path = std::fs::read_to_string(project.path().join(filename)).unwrap();
            assert!(!std::path::Path::new(path.trim()).exists());
        }
    }
}

#[test]
fn review_mcp_validates_and_saves_multiline_code() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let dir = tempfile::tempdir().unwrap();
    let result = dir.path().join("review.json");
    let mut child = Command::new(BIN)
        .arg("--internal-review-mcp")
        .arg(&result)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut request = |message: serde_json::Value| {
        writeln!(input, "{message}").unwrap();
        if message.get("id").is_none() {
            return serde_json::Value::Null;
        }
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        serde_json::from_str::<serde_json::Value>(&line).unwrap()
    };
    request(serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion":"2025-03-26", "capabilities":{},
            "clientInfo":{"name":"test","version":"1.0.0"}}
    }));
    request(serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let tools = request(serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 1);
    assert_eq!(tools["result"]["tools"][0]["name"], "save_result");

    let tool_call = |id, review: serde_json::Value| {
        serde_json::json!({
            "jsonrpc":"2.0","id":id,"method":"tools/call",
            "params":{"name":"save_result","arguments":{"review":review}}
        })
    };
    let invalid = request(tool_call(
        3,
        serde_json::json!({"key_issues_to_review": false}),
    ));
    assert!(invalid.get("error").is_some() || invalid["result"]["isError"] == true);
    assert!(!result.exists());

    let code = "if ready {\n    println!(\"left: right\\n\");\n}\n";
    let saved = request(tool_call(
        4,
        serde_json::json!({
            "key_issues_to_review": [{
                "relevant_file":"main.rs", "issue_header":"Header: issue",
                "issue_content":"Details", "start_line":1, "end_line":2,
                "improved_code":code
            }]
        }),
    ));
    assert_ne!(saved["result"]["isError"], true, "{saved}");
    let repeated = request(tool_call(5, serde_json::json!({"key_issues_to_review":[]})));
    assert_eq!(repeated["result"]["isError"], true, "{repeated}");

    drop(input);
    assert!(child.wait().unwrap().success());
    let log = std::fs::read_to_string(result.with_extension("mcp.log")).unwrap();
    assert!(log.contains("MCP tool pr_result_save_result called."));
    assert!(log.contains("arguments failed review schema validation."));
    assert!(log.contains("accepted and saved the review."));
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(result).unwrap()).unwrap();
    assert_eq!(
        saved["review"]["key_issues_to_review"][0]["improved_code"],
        code
    );
}

#[test]
fn review_does_not_publish_text_without_mcp_submission() {
    let (port, log) = spawn_server(Arc::new(github_respond));
    let out = run(
        &["--pr_url", "https://github.com/org/repo/pull/123", "review"],
        &[
            ("GITHUB__BASE_URL", &format!("http://127.0.0.1:{port}")),
            ("GITHUB__USER_TOKEN", "stub-token"),
            ("OPENCODE__BINARY", engine_stub("review").to_str().unwrap()),
            ("PR_AGENT_REVIEW_MCP", "true"),
        ],
        None,
    );
    assert_ne!(out.code, 0, "{}", out.stdout);
    assert!(
        out.stderr.contains("pr_result_save_result"),
        "{}",
        out.stderr
    );
    assert_eq!(
        out.stdout
            .matches("Continuing the same session once")
            .count(),
        1
    );
    assert!(out.stdout.contains("did not start the review MCP server"));
    assert!(!requested(&log, "POST", "pulls/123/reviews"));
    assert!(!requested(&log, "POST", "issues/123/comments"));
}
