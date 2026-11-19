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
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub server");
    let port = listener.local_addr().expect("local addr").port();
    let requests: Requests = Arc::new(Mutex::new(Vec::new()));
    let log = requests.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let respond = respond.clone();
            let log = log.clone();
            std::thread::spawn(move || handle_connection(stream, respond, log));
        }
    });
    (port, requests)
}

fn handle_connection(mut stream: TcpStream, respond: Responder, log: Requests) {
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
    let (status, out) = respond(&method, &path, &body);
    let reason = match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        _ => "",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}",
        out.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

// ---- the stub APIs ----------------------------------------------------------

const GH_PR: &str = r#"{"number":123,"title":"Add calculator","body":"Adds a Calculator class.","html_url":"https://github.com/org/repo/pull/123","head":{"sha":"abc123","ref":"feature"},"base":{"ref":"main"},"user":{"login":"dev"},"commits":2}"#;
const GH_FILES: &str = r#"[{"filename":"src/calculator.py","status":"added","additions":3,"deletions":0,"patch":"@@ -0,0 +1,3 @@\n+def divide(a, b):\n+    return a / b\n"}]"#;
const FILE_CONTENT: &str = "def divide(a, b):\n    return a / b\n";

fn github_respond(method: &str, path: &str, _body: &str) -> (u16, String) {
    let ok = |value: String| (200, value);
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
        return ok(format!(r#"{{"content":"{}"}}"#, b64(FILE_CONTENT)));
    }
    (404, r#"{"message":"not found"}"#.into())
}

const GL_MR: &str = r#"{"iid":5,"title":"Add calculator","description":"Adds a Calculator class.","web_url":"https://gitlab.com/org/repo/-/merge_requests/5","diff_refs":{"head_sha":"abc123def456"},"source_branch":"feature","target_branch":"main","author":{"username":"dev"},"labels":[]"#;
const GL_DIFFS: &str = r#"[{"new_path":"src/calculator.py","old_path":"src/calculator.py","diff":"@@ -0,0 +1,3 @@\n+def divide(a, b):\n+    return a / b\n","new_file":true,"deleted_file":false,"renamed_file":false}]"#;

fn gitlab_respond(method: &str, path: &str, _body: &str) -> (u16, String) {
    let ok = |value: String| (200, value);
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
    for var in [
        "GITHUB__USER_TOKEN",
        "GITHUB__BASE_URL",
        "GITLAB__USER_TOKEN",
        "GITLAB_PERSONAL_ACCESS_TOKEN",
        "GITLAB__URL",
        "OPENCODE__BINARY",
        "OPENCODE__MODEL",
        "OPENCODE__AGENT",
        "OPENCODE__TIMEOUT_SECONDS",
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
fn review_posts_findings_and_labels() {
    let (port, log) = spawn_server(Arc::new(github_respond));
    let out = run(
        &["--pr_url", "https://github.com/org/repo/pull/123", "review"],
        &[
            ("GITHUB__USER_TOKEN", "stub-token"),
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
    // The engine answered: one finding published inline, the summary posted.
    assert!(out.stdout.contains("Published 1 review finding(s)"));
    assert!(posted(&log, "PR Reviewer Guide"));
    assert!(requested(&log, "POST", "pulls/123/reviews"));
    // Labels: read current, publish the effort label.
    assert!(requested(&log, "GET", "issues/123/labels"));
    assert!(requested(&log, "PUT", "issues/123/labels"));
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
    assert_eq!(out.code, 0, "stdout:\n{}", out.stdout);
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
    assert!(out.stderr.contains("invalid choice: 'bogus'"));
}

#[test]
fn plain_diff_review_runs_patch_only() {
    let dir = std::env::temp_dir().join("pr-agent-tests/plain-diff");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("fixture.diff"),
        "diff --git a/src/app.py b/src/app.py\nnew file mode 100644\nindex 0000000..1111111\n--- /dev/null\n+++ b/src/app.py\n@@ -0,0 +1,2 @@\n+def run():\n+    return 42\n",
    )
    .unwrap();
    let out = run(
        &["--diff-file", "fixture.diff", "review"],
        &[("OPENCODE__BINARY", engine_stub("plain").to_str().unwrap())],
        Some(&dir),
    );
    assert_eq!(out.code, 0, "stdout:\n{}", out.stdout);
    assert!(out.stdout.contains("patch-only mode"));
    assert!(out.stdout.contains("Reviewing PR: local_diff"));
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
