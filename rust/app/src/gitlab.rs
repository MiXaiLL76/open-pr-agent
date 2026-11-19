//! GitLab REST transport for the CI pipeline: the request subset the original's
//! `GitLabProvider` made for `/review`, `/describe`, `/improve` and `/ask` —
//! merge requests, diffs, raw file contents, languages, notes and labels.
//!
//! Auth is a personal access token (`PRIVATE-TOKEN`), the deployment the CI
//! containers run with. The webhook flow (shared secret validation) lives in
//! the server layer.

use std::time::Duration;

/// A minimal GitLab API client for one project's merge request.
pub struct GitlabClient {
    pub base_url: String,
    token: String,
    http: reqwest::blocking::Client,
}

/// The fields the pipeline consumes from a merge request.
#[allow(dead_code)] // fields mirror the provider surface the remaining commands consume
pub struct MergeRequest {
    pub iid: u64,
    pub title: String,
    pub description: String,
    pub web_url: String,
    /// The head SHA of the merge request (the state a reviewer must see).
    pub head_sha: String,
    pub source_branch: String,
    pub target_branch: String,
    pub author_username: String,
}

/// One changed file of a merge request, from the diffs endpoint.
#[allow(dead_code)] // the full provider surface ports with the remaining commands
pub struct MrDiff {
    pub new_path: String,
    pub old_path: String,
    /// The unified patch body.
    pub diff: String,
    pub new_file: bool,
    pub deleted_file: bool,
    pub renamed_file: bool,
}

/// A note (comment) on a merge request.
#[allow(dead_code)] // the body feeds comment reconciliation when it ports
pub struct Note {
    pub id: i64,
    pub body: String,
}

impl GitlabClient {
    pub fn new(base_url: &str, token: &str) -> Result<Self, String> {
        // The same request-timeout policy as the GitHub client (the
        // original's default, 60s).
        let timeout = request_timeout_rs::port::get_http_request_timeout()
            .map_err(|e| format!("GitlabError: {e}"))?;
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs_f64(timeout))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http,
        })
    }

    fn get(&self, path: &str) -> Result<serde_json::Value, String> {
        let url = format!("{}/api/v4/{path}", self.base_url);
        let response = self
            .http
            .get(&url)
            .header("PRIVATE-TOKEN", &self.token)
            .send()
            .map_err(|e| format!("GitlabError: {e}"))?;
        self.json(response)
    }

    fn send_body(
        &self,
        method: reqwest::Method,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<serde_json::Value, String> {
        let url = format!("{}/api/v4/{path}", self.base_url);
        let request = self
            .http
            .request(method, &url)
            .header("PRIVATE-TOKEN", &self.token)
            .form(params);
        let response = request.send().map_err(|e| format!("GitlabError: {e}"))?;
        self.json(response)
    }

    fn json(&self, response: reqwest::blocking::Response) -> Result<serde_json::Value, String> {
        let status = response.status();
        if status == reqwest::StatusCode::NO_CONTENT {
            return Ok(serde_json::Value::Null);
        }
        let value: serde_json::Value = response
            .json()
            .map_err(|e| format!("GitlabError: malformed response body: {e}"))?;
        if !status.is_success() {
            let message = value
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("request failed")
                .to_string();
            return Err(format!("GitlabError: {status} {message}"));
        }
        Ok(value)
    }

    /// The project id for a full path (`group/subgroup/repo`): GitLab accepts
    /// the URL-encoded path anywhere an id fits, so this is just the encoding.
    fn project_path(repo: &str) -> String {
        percent_encode(repo)
    }

    pub fn get_merge_request(&self, repo: &str, iid: u64) -> Result<MergeRequest, String> {
        let value = self.get(&format!(
            "projects/{}/merge_requests/{iid}",
            Self::project_path(repo)
        ))?;
        Ok(MergeRequest {
            iid,
            title: str_field(&value, "title"),
            description: str_field(&value, "description"),
            web_url: str_field(&value, "web_url"),
            head_sha: value
                .pointer("/diff_refs/head_sha")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            source_branch: str_field(&value, "source_branch"),
            target_branch: str_field(&value, "target_branch"),
            author_username: value
                .pointer("/author/username")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        })
    }

    pub fn get_mr_diffs(&self, repo: &str, iid: u64) -> Result<Vec<MrDiff>, String> {
        let value = self.get(&format!(
            "projects/{}/merge_requests/{iid}/diffs",
            Self::project_path(repo)
        ))?;
        Ok(value
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|d| MrDiff {
                new_path: str_field(&d, "new_path"),
                old_path: str_field(&d, "old_path"),
                diff: str_field(&d, "diff"),
                new_file: d.get("new_file").and_then(|v| v.as_bool()).unwrap_or(false),
                deleted_file: d
                    .get("deleted_file")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                renamed_file: d
                    .get("renamed_file")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            })
            .collect())
    }

    /// Raw file content at a ref (`/repository/files/<path>/raw?ref=`).
    pub fn get_raw_file(&self, repo: &str, path: &str, git_ref: &str) -> Result<String, String> {
        let response = self
            .http
            .get(format!(
                "{}/api/v4/projects/{}/repository/files/{}/raw?ref={}",
                self.base_url,
                Self::project_path(repo),
                percent_encode(path),
                percent_encode(git_ref)
            ))
            .header("PRIVATE-TOKEN", &self.token)
            .send()
            .map_err(|e| format!("GitlabError: {e}"))?;
        let status = response.status();
        let text = response
            .text()
            .map_err(|e| format!("GitlabError: malformed response body: {e}"))?;
        if status.is_success() {
            Ok(text)
        } else {
            Err(format!("GitlabError: {status} {text}"))
        }
    }

    /// The project language percentages, used for "PR main language".
    pub fn get_languages(&self, repo: &str) -> Result<Vec<(String, i64)>, String> {
        let value = self.get(&format!("projects/{}/languages", Self::project_path(repo)))?;
        Ok(value
            .as_object()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(k, v)| v.as_i64().map(|n| (k, n)))
            .collect())
    }

    pub fn create_note(&self, repo: &str, iid: u64, body: &str) -> Result<Note, String> {
        let value = self.send_body(
            reqwest::Method::POST,
            &format!(
                "projects/{}/merge_requests/{iid}/notes",
                Self::project_path(repo)
            ),
            &[("body", body)],
        )?;
        Ok(Note {
            id: value.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
            body: str_field(&value, "body"),
        })
    }

    pub fn update_note(
        &self,
        repo: &str,
        iid: u64,
        note_id: i64,
        body: &str,
    ) -> Result<(), String> {
        self.send_body(
            reqwest::Method::PUT,
            &format!(
                "projects/{}/merge_requests/{iid}/notes/{note_id}",
                Self::project_path(repo)
            ),
            &[("body", body)],
        )?;
        Ok(())
    }

    /// The notes on a merge request, newest last.
    #[allow(dead_code)] // comment reconciliation ports with the hosted flow
    pub fn get_notes(&self, repo: &str, iid: u64) -> Result<Vec<Note>, String> {
        let value = self.get(&format!(
            "projects/{}/merge_requests/{iid}/notes?sort=asc",
            Self::project_path(repo)
        ))?;
        Ok(value
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|n| Note {
                id: n.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
                body: str_field(&n, "body"),
            })
            .collect())
    }

    pub fn delete_note(&self, repo: &str, iid: u64, note_id: i64) -> Result<(), String> {
        let url = format!(
            "{}/api/v4/projects/{}/merge_requests/{iid}/notes/{note_id}",
            self.base_url,
            Self::project_path(repo)
        );
        let response = self
            .http
            .delete(&url)
            .header("PRIVATE-TOKEN", &self.token)
            .send()
            .map_err(|e| format!("GitlabError: {e}"))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(format!("GitlabError: {}", response.status()))
        }
    }

    /// The labels currently on the merge request.
    pub fn get_labels(&self, repo: &str, iid: u64) -> Result<Vec<String>, String> {
        let value = self.get(&format!(
            "projects/{}/merge_requests/{iid}",
            Self::project_path(repo)
        ))?;
        Ok(value
            .get("labels")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|l| l.as_str().map(|s| s.to_string()))
            .collect())
    }

    /// Replace the merge request labels (the API replaces, not appends).
    pub fn publish_labels(&self, repo: &str, iid: u64, labels: &[String]) -> Result<(), String> {
        let joined = labels.join(",");
        self.send_body(
            reqwest::Method::PUT,
            &format!("projects/{}/merge_requests/{iid}", Self::project_path(repo)),
            &[("labels", joined.as_str())],
        )?;
        Ok(())
    }

    /// Update the merge request title/description (`/describe` publishing).
    pub fn update_merge_request(
        &self,
        repo: &str,
        iid: u64,
        title: &str,
        description: &str,
    ) -> Result<(), String> {
        self.send_body(
            reqwest::Method::PUT,
            &format!("projects/{}/merge_requests/{iid}", Self::project_path(repo)),
            &[("title", title), ("description", description)],
        )?;
        Ok(())
    }
}

fn str_field(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Percent-encode for a URL path segment (`/` becomes `%2F`).
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_segments_encode_the_separator() {
        assert_eq!(
            GitlabClient::project_path("group/subgroup/repo"),
            "group%2Fsubgroup%2Frepo"
        );
        assert_eq!(percent_encode("src/app.py"), "src%2Fapp.py");
        assert_eq!(percent_encode("plain.rs"), "plain.rs");
    }
}
