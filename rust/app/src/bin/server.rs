//! Webhook server: translates GitHub and GitLab events into pipeline runs,
//! the port of `pr_agent/servers/` (github_app, gitlab_webhook).
//!
//! One process serves both hosts on separate paths; the CLI pipeline is
//! reused verbatim (`pr_agent::pipeline_try_run` on a synthetic argv), so a
//! webhook review behaves exactly like `pr_agent --pr_url ... review`.
//!
//! Auth is fail-closed: when a shared secret is configured, a request
//! without a valid signature is rejected before any work starts. Token
//! auth stays user-token based (the GitHub App JWT/installation flow is a
//! follow-up); publishing uses the same GITHUB__USER_TOKEN /
//! GITLAB__USER_TOKEN the CLI reads.
//!
//! Run: `pr_agent_server` (PORT env, default 3000).

use std::ffi::OsString;
use std::net::SocketAddr;
use std::process::ExitCode;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;

/// Per-process server state: the secrets the hooks validate against.
#[derive(Clone, Default)]
struct Hooks {
    github_secret: Option<String>,
    gitlab_secret: Option<String>,
}

impl Hooks {
    fn from_env() -> Self {
        Self {
            github_secret: std::env::var("GITHUB_APP_WEBHOOK_SECRET")
                .ok()
                .filter(|s| !s.is_empty()),
            gitlab_secret: std::env::var("GITLAB_WEBHOOK_SECRET")
                .ok()
                .filter(|s| !s.is_empty()),
        }
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    if let Some(code) = pr_agent::review_mcp::serve_if_requested().await {
        return code;
    }
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);
    let hooks = Hooks::from_env();

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/github/webhook", post(github_webhook))
        .route("/gitlab/webhook", post(gitlab_webhook))
        .with_state(hooks);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|e| panic!("cannot bind {addr}: {e}"));
    eprintln!("pr_agent_server listening on http://{addr}");
    axum::serve(listener, app).await.expect("server error");
    std::process::ExitCode::SUCCESS
}

/// GitHub pull-request events: `pull_request` with an actionable action runs
/// the review pipeline on the PR.
async fn github_webhook(
    State(hooks): State<Hooks>,
    headers: HeaderMap,
    body: String,
) -> Result<(StatusCode, &'static str), (StatusCode, String)> {
    verify_github_signature(&hooks, &headers, &body)?;
    let event: Value = serde_json::from_str(&body)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("malformed JSON: {e}")))?;
    if event.get("zen").is_some() || event.get("hook_id").is_some() {
        // The initial webhook ping: acknowledge, do nothing.
        return Ok((StatusCode::OK, "ping acknowledged"));
    }
    let action = event.get("action").and_then(Value::as_str).unwrap_or("");
    if !matches!(action, "opened" | "synchronize" | "reopened") {
        return Ok((StatusCode::OK, "ignored action"));
    }
    let Some(url) = event
        .pointer("/pull_request/html_url")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
    else {
        return Err((StatusCode::BAD_REQUEST, "no pull_request.html_url".into()));
    };
    spawn_pipeline_run("review", url);
    Ok((StatusCode::OK, "review dispatched"))
}

/// GitLab merge-request events: the `merge_request` kind with an actionable
/// state transition runs the review pipeline on the MR.
async fn gitlab_webhook(
    State(hooks): State<Hooks>,
    headers: HeaderMap,
    body: String,
) -> Result<(StatusCode, &'static str), (StatusCode, String)> {
    // Fail-closed shared-secret check.
    if let Some(secret) = &hooks.gitlab_secret {
        let token = headers
            .get("x-gitlab-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !constant_time_eq(token.as_bytes(), secret.as_bytes()) {
            return Err((StatusCode::FORBIDDEN, "invalid X-Gitlab-Token".into()));
        }
    }
    let event: Value = serde_json::from_str(&body)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("malformed JSON: {e}")))?;
    if event.get("object_kind").and_then(Value::as_str) != Some("merge_request") {
        return Ok((StatusCode::OK, "ignored object_kind"));
    }
    let attrs = event
        .get("object_attributes")
        .cloned()
        .unwrap_or(Value::Null);
    let action = attrs.get("action").and_then(Value::as_str).unwrap_or("");
    if !matches!(action, "open" | "update" | "reopen") {
        return Ok((StatusCode::OK, "ignored action"));
    }
    let Some(url) = attrs
        .get("url")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
    else {
        return Err((StatusCode::BAD_REQUEST, "no object_attributes.url".into()));
    };
    spawn_pipeline_run("review", url);
    Ok((StatusCode::OK, "review dispatched"))
}

/// Fail-closed GitHub signature check: `X-Hub-Signature-256` is an HMAC of
/// the raw body with the shared secret, spelled `sha256=<hex>`.
fn verify_github_signature(
    hooks: &Hooks,
    headers: &HeaderMap,
    body: &str,
) -> Result<(), (StatusCode, String)> {
    let Some(secret) = &hooks.github_secret else {
        return Ok(());
    };
    let signature = headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let Some(provided) = signature.strip_prefix("sha256=") else {
        return Err((StatusCode::FORBIDDEN, "missing X-Hub-Signature-256".into()));
    };
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(body.as_bytes());
    let digest = hex(&mac.finalize().into_bytes());
    if !constant_time_eq(digest.as_bytes(), provided.as_bytes()) {
        return Err((StatusCode::FORBIDDEN, "invalid X-Hub-Signature-256".into()));
    }
    Ok(())
}

/// Byte-wise constant-time equality.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Dispatch a pipeline run on the blocking pool: the pipeline is fully
/// synchronous (blocking HTTP, process spawns) and must not stall the
/// runtime. The webhook returns before the review finishes, like the
/// original's fire-and-forget handling.
fn spawn_pipeline_run(command: &str, url: String) {
    let command = command.to_string();
    tokio::task::spawn_blocking(move || {
        let argv: Vec<OsString> = vec![
            "--pr_url".into(),
            url.clone().into(),
            command.clone().into(),
        ];
        let code: ExitCode = pr_agent::pipeline_try_run(&argv);
        eprintln!("dispatch {command} on {url} finished with exit code {code:?}");
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_verification_is_exact() {
        let hooks = Hooks {
            github_secret: Some("s3cret".into()),
            gitlab_secret: None,
        };
        let body = r#"{"action":"opened"}"#;
        let mut mac = Hmac::<Sha256>::new_from_slice(b"s3cret").unwrap();
        mac.update(body.as_bytes());
        let good = format!("sha256={}", hex(&mac.finalize().into_bytes()));

        let mut headers = HeaderMap::new();
        headers.insert("x-hub-signature-256", good.parse().expect("header value"));
        assert!(verify_github_signature(&hooks, &headers, body).is_ok());

        let mut bad = HeaderMap::new();
        bad.insert("x-hub-signature-256", "sha256=deadbeef".parse().unwrap());
        assert!(verify_github_signature(&hooks, &bad, body).is_err());

        let missing = HeaderMap::new();
        assert!(verify_github_signature(&hooks, &missing, body).is_err());
    }

    #[test]
    fn no_secret_means_no_signature_required() {
        let hooks = Hooks::default();
        assert!(verify_github_signature(&hooks, &HeaderMap::new(), "{}").is_ok());
    }

    #[test]
    fn constant_time_eq_rejects_mismatches() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
