//! GitHub REST transport for the CI pipeline: the request subset the original's
//! PyGithub client made for `/review`, `/describe`, `/improve` and `/ask` —
//! pull requests, files, commits, languages, labels, comments and reviews.
//!
//! The GitHub App (JWT) auth path and pagination beyond `per_page=100` stay on
//! the Python side until they are ported; this client covers the user-token
//! deployment that CI containers run with.

use serde_json::Value;

/// A minimal GitHub API client for one repository's pull request.
#[allow(dead_code)] // fields mirror the provider surface the remaining units consume
pub struct GithubClient {
    pub base_url: String,
    pub token: String,
    http: reqwest::blocking::Client,
}

/// The fields the pipeline consumes from a pull request.
#[allow(dead_code)] // the full provider surface ports with the remaining commands
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub html_url: String,
    pub head_sha: String,
    pub head_ref: String,
    pub base_ref: String,
    pub user_login: String,
    pub commits: u64,
}

/// One changed file of a pull request.
#[allow(dead_code)] // counts and statuses feed the describe tables when they port
pub struct PullFile {
    pub filename: String,
    pub status: String,
    pub additions: i64,
    pub deletions: i64,
    pub patch: String,
}

/// An issue comment the pipeline created (or found) on the pull request.
#[allow(dead_code)] // the body feeds comment reconciliation when it ports
pub struct Comment {
    pub id: i64,
    pub body: String,
}

impl GithubClient {
    pub fn new(base_url: &str, token: &str) -> Result<Self, String> {
        // The ported request-timeout policy (the original's default, 60s).
        let timeout = request_timeout_rs::port::get_http_request_timeout()
            .map_err(|e| format!("GitHubError: {e}"))?;
        let http = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs_f64(timeout))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http,
        })
    }

    fn get(&self, path: &str) -> Result<Value, String> {
        let url = format!("{}/repos/{path}", self.base_url);
        let response = self
            .http
            .get(&url)
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .send()
            .map_err(|e| format!("GitHubError: {e}"))?;
        self.json(response)
    }

    fn send_body(
        &self,
        method: reqwest::Method,
        path: &str,
        body: &Value,
    ) -> Result<Value, String> {
        let url = format!("{}/repos/{path}", self.base_url);
        let request = self
            .http
            .request(method, &url)
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .json(body);
        let response = request.send().map_err(|e| format!("GitHubError: {e}"))?;
        self.json(response)
    }

    fn json(&self, response: reqwest::blocking::Response) -> Result<Value, String> {
        let status = response.status();
        if status == reqwest::StatusCode::NO_CONTENT {
            return Ok(Value::Null);
        }
        let value: Value = response
            .json()
            .map_err(|e| format!("GitHubError: malformed response body: {e}"))?;
        if !status.is_success() {
            let message = value
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("request failed")
                .to_string();
            return Err(format!("GitHubError: {status} {message}"));
        }
        Ok(value)
    }

    /// Fetch the repository object (default branch, owner) for the pipeline.
    #[allow(dead_code)] // the describe and repo-context units consume it when they port
    pub fn get_repo(&self, repo: &str) -> Result<Value, String> {
        self.get(repo)
    }

    pub fn get_pull(&self, repo: &str, number: u64) -> Result<PullRequest, String> {
        let value = self.get(&format!("{repo}/pulls/{number}"))?;
        Ok(PullRequest {
            number,
            title: str_field(&value, "title"),
            body: str_field(&value, "body"),
            html_url: str_field(&value, "html_url"),
            head_sha: value
                .pointer("/head/sha")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            head_ref: value
                .pointer("/head/ref")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            base_ref: value
                .pointer("/base/ref")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            user_login: value
                .pointer("/user/login")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            commits: value.get("commits").and_then(|v| v.as_i64()).unwrap_or(0) as u64,
        })
    }

    pub fn get_pull_files(&self, repo: &str, number: u64) -> Result<Vec<PullFile>, String> {
        let value = self.get(&format!("{repo}/pulls/{number}/files?per_page=100"))?;
        let files = value.as_array().cloned().unwrap_or_default();
        Ok(files
            .into_iter()
            .map(|f| PullFile {
                filename: str_field(&f, "filename"),
                status: str_field(&f, "status"),
                additions: f.get("additions").and_then(|v| v.as_i64()).unwrap_or(0),
                deletions: f.get("deletions").and_then(|v| v.as_i64()).unwrap_or(0),
                patch: f
                    .get("patch")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            })
            .collect())
    }

    /// The repository language map, used for "PR main language".
    pub fn get_languages(&self, repo: &str) -> Result<Vec<(String, i64)>, String> {
        let value = self.get(&format!("{repo}/languages"))?;
        let map = value.as_object().cloned().unwrap_or_default();
        Ok(map
            .into_iter()
            .filter_map(|(k, v)| v.as_i64().map(|n| (k, n)))
            .collect())
    }

    pub fn get_issue_labels(&self, repo: &str, number: u64) -> Result<Vec<String>, String> {
        let value = self.get(&format!("{repo}/issues/{number}/labels"))?;
        Ok(value
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|l| l.get("name").and_then(|n| n.as_str()))
            .map(|s| s.to_string())
            .collect())
    }

    pub fn create_issue_comment(
        &self,
        repo: &str,
        number: u64,
        body: &str,
    ) -> Result<Comment, String> {
        let payload = serde_json::json!({ "body": body });
        let value = self.send_body(
            reqwest::Method::POST,
            &format!("{repo}/issues/{number}/comments"),
            &payload,
        )?;
        Ok(Comment {
            id: value.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
            body: str_field(&value, "body"),
        })
    }

    pub fn update_issue_comment(
        &self,
        repo: &str,
        comment_id: i64,
        body: &str,
    ) -> Result<(), String> {
        let payload = serde_json::json!({ "body": body });
        self.send_body(
            reqwest::Method::PATCH,
            &format!("{repo}/issues/comments/{comment_id}"),
            &payload,
        )?;
        Ok(())
    }

    pub fn delete_issue_comment(&self, repo: &str, comment_id: i64) -> Result<(), String> {
        let url = format!(
            "{}/repos/{repo}/issues/comments/{comment_id}",
            self.base_url
        );
        let response = self
            .http
            .delete(&url)
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .send()
            .map_err(|e| format!("GitHubError: {e}"))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(format!("GitHubError: {}", response.status()))
        }
    }

    pub fn publish_labels(&self, repo: &str, number: u64, labels: &[String]) -> Result<(), String> {
        let payload = serde_json::json!({ "labels": labels });
        self.send_body(
            reqwest::Method::PUT,
            &format!("{repo}/issues/{number}/labels"),
            &payload,
        )?;
        Ok(())
    }

    /// Update the pull request title/body (`/describe` publishing).
    pub fn update_pull(
        &self,
        repo: &str,
        number: u64,
        title: &str,
        body: &str,
    ) -> Result<(), String> {
        let payload = serde_json::json!({ "title": title, "body": body });
        self.send_body(
            reqwest::Method::PATCH,
            &format!("{repo}/pulls/{number}"),
            &payload,
        )?;
        Ok(())
    }

    /// Publish review findings as one review with inline comments.
    pub fn create_review(
        &self,
        repo: &str,
        number: u64,
        body: &str,
        comments: &[Value],
    ) -> Result<(), String> {
        let payload = serde_json::json!({ "body": body, "event": "COMMENT", "comments": comments });
        self.send_body(
            reqwest::Method::POST,
            &format!("{repo}/pulls/{number}/reviews"),
            &payload,
        )?;
        Ok(())
    }
}

fn str_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

impl GithubClient {
    /// Full file content at a ref, base64-decoded (`/contents/<path>?ref=`).
    pub fn get_file_contents(
        &self,
        repo: &str,
        path: &str,
        git_ref: &str,
    ) -> Result<String, String> {
        let value = self.get(&format!("{repo}/contents/{path}?ref={git_ref}"))?;
        let encoded = value.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let cleaned: String = encoded.chars().filter(|c| !c.is_whitespace()).collect();
        decode_base64(&cleaned).ok_or_else(|| "GitHubError: malformed content encoding".to_string())
    }
}

/// Standard-alphabet base64 decode (the contents API encoding).
fn decode_base64(input: &str) -> Option<String> {
    let mut out = Vec::new();
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    for ch in input.chars() {
        if ch == '=' {
            break;
        }
        let value = match ch {
            'A'..='Z' => ch as u32 - 'A' as u32,
            'a'..='z' => ch as u32 - 'a' as u32 + 26,
            '0'..='9' => ch as u32 - '0' as u32 + 52,
            '+' => 62,
            '/' => 63,
            _ => return None,
        };
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_token_message() {
        // The no-token error path reports the missing dynaconf key.
        let client = GithubClient::new("http://127.0.0.1:1", "").unwrap();
        let err = client.get_repo("org/repo").unwrap_err();
        assert!(err.starts_with("GitHubError:"));
    }
}
