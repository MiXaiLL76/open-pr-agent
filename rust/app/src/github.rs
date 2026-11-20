//! GitHub operations used by the review pipeline.

use std::future::Future;
use std::time::Duration;

use base64::{Engine, engine::general_purpose::STANDARD};
use octocrab::{Octocrab, Page, models::repos::DiffEntryStatus};
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use serde_json::Value;
use tokio::runtime::{Builder, Runtime};

pub struct GithubClient {
    pub base_url: String,
    api: Octocrab,
    runtime: Runtime,
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
    pub status: DiffEntryStatus,
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
        let base_url = base_url.trim_end_matches('/').to_string();
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| format!("GitHubError: {err}"))?;
        let api = {
            let _guard = runtime.enter();
            let mut transport = reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(60));
            if let Some(path) = std::env::var_os("SSL_CERT_FILE").filter(|path| !path.is_empty()) {
                let pem = std::fs::read(&path)
                    .map_err(|err| format!("GitHubError: cannot read SSL_CERT_FILE: {err}"))?;
                let roots = reqwest::Certificate::from_pem_bundle(&pem)
                    .map_err(|err| format!("GitHubError: invalid SSL_CERT_FILE: {err}"))?;
                transport = transport.tls_certs_merge(roots);
            }
            let transport = transport
                .build()
                .map_err(|err| format!("GitHubError: {err}"))?;
            Octocrab::builder()
                .base_uri(base_url.as_str())
                .and_then(|builder| {
                    builder
                        .personal_token(token.to_owned())
                        .build_with_reqwest(transport)
                })
                .map_err(github_error)?
        };
        Ok(Self {
            base_url,
            api,
            runtime,
        })
    }

    // The CLI and webhook worker call these operations from synchronous code.
    fn query<T>(&self, request: impl Future<Output = octocrab::Result<T>>) -> Result<T, String> {
        self.runtime.block_on(request).map_err(github_error)
    }

    fn get_all(&self, path: &str) -> Result<Vec<Value>, String> {
        self.query(async {
            let page: Page<Value> = self
                .api
                .get(format!("/repos/{path}"), Some(&[("per_page", 100)]))
                .await?;
            self.api.all_pages(page).await
        })
    }

    pub fn get_pull(&self, repo: &str, number: u64) -> Result<PullRequest, String> {
        let (owner, name) = split_repo(repo)?;
        let pr = self.query(self.api.pulls(owner, name).get(number))?;
        Ok(PullRequest {
            number: pr.number,
            title: pr.title.unwrap_or_default(),
            body: pr.body.unwrap_or_default(),
            html_url: pr.html_url.map(|url| url.into()).unwrap_or_default(),
            head_sha: pr.head.sha,
            head_ref: pr.head.ref_field,
            base_ref: pr.base.ref_field,
            user_login: pr.user.map(|user| user.login).unwrap_or_default(),
            commits: pr.commits.unwrap_or_default(),
        })
    }

    pub fn get_pull_diff(&self, repo: &str, number: u64) -> Result<String, String> {
        let (owner, name) = split_repo(repo)?;
        self.query(self.api.pulls(owner, name).get_diff(number))
    }

    pub(crate) fn compare_review_heads(
        &self,
        repo: &str,
        previous: &str,
        head: &str,
    ) -> Result<Option<Vec<octocrab::models::repos::DiffEntry>>, String> {
        let comparison: Value = self.query(self.api.get(
            format!("/repos/{repo}/compare/{previous}...{head}"),
            Some(&[("per_page", 1)]),
        ))?;
        if comparison
            .pointer("/merge_base_commit/sha")
            .and_then(Value::as_str)
            != Some(previous)
        {
            return Ok(None);
        }
        let Some(files) = comparison.get("files").and_then(Value::as_array) else {
            return Ok(None);
        };
        // GitHub returns at most 300 files for the entire comparison.
        if files.len() >= 300 {
            return Ok(None);
        }
        let files = serde_json::from_value(Value::Array(files.clone()))
            .map_err(|error| format!("GitHubError: invalid comparison files: {error}"))?;
        Ok(Some(files))
    }

    pub fn get_pull_files(&self, repo: &str, number: u64) -> Result<Vec<PullFile>, String> {
        let (owner, name) = split_repo(repo)?;
        let files = self.query(async {
            let first = self.api.pulls(owner, name).list_files(number).await?;
            self.api.all_pages(first).await
        })?;
        Ok(files
            .into_iter()
            .map(|file| PullFile {
                filename: file.filename,
                status: file.status,
                additions: file.additions as i64,
                deletions: file.deletions as i64,
                patch: file.patch.unwrap_or_default(),
            })
            .collect())
    }

    /// The repository language map, used for "PR main language".
    pub fn get_languages(&self, repo: &str) -> Result<Vec<(String, i64)>, String> {
        let (owner, name) = split_repo(repo)?;
        let languages = self.query(self.api.repos(owner, name).list_languages())?;
        let mut languages: Vec<_> = languages.into_iter().collect();
        languages.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        Ok(languages)
    }

    pub fn get_issue_labels(&self, repo: &str, number: u64) -> Result<Vec<String>, String> {
        let (owner, name) = split_repo(repo)?;
        let labels = self.query(async {
            let first = self
                .api
                .issues(owner, name)
                .list_labels_for_issue(number)
                .per_page(100)
                .send()
                .await?;
            self.api.all_pages(first).await
        })?;
        Ok(labels.into_iter().map(|label| label.name).collect())
    }

    pub fn current_login(&self) -> Result<String, String> {
        let user = self.query(self.api.get("/user", None::<&()>))?;
        let login = str_field(&user, "login");
        if login.is_empty() {
            return Err("GitHub API did not identify the authenticated user".into());
        }
        Ok(login)
    }

    pub fn get_review_comments(
        &self,
        repo: &str,
        number: u64,
        inline: bool,
    ) -> Result<Vec<Value>, String> {
        let resource = if inline { "pulls" } else { "issues" };
        self.get_all(&format!("{repo}/{resource}/{number}/comments"))
    }

    pub fn create_issue_comment(
        &self,
        repo: &str,
        number: u64,
        body: &str,
    ) -> Result<Comment, String> {
        let payload = serde_json::json!({ "body": body });
        let value: Value = self.query(self.api.post(
            format!("/repos/{repo}/issues/{number}/comments"),
            Some(&payload),
        ))?;
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
        self.query::<Value>(self.api.patch(
            format!("/repos/{repo}/issues/comments/{comment_id}"),
            Some(&payload),
        ))?;
        Ok(())
    }

    pub fn delete_issue_comment(&self, repo: &str, comment_id: i64) -> Result<(), String> {
        let (owner, name) = split_repo(repo)?;
        let comment_id = u64::try_from(comment_id).map_err(|err| format!("GitHubError: {err}"))?;
        self.query(
            self.api
                .issues(owner, name)
                .delete_comment(comment_id.into()),
        )
    }

    pub fn publish_labels(&self, repo: &str, number: u64, labels: &[String]) -> Result<(), String> {
        let (owner, name) = split_repo(repo)?;
        self.query(
            self.api
                .issues(owner, name)
                .replace_all_labels(number, labels),
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
        let (owner, name) = split_repo(repo)?;
        let pulls = self.api.pulls(owner, name);
        self.query(pulls.update(number).title(title).body(body).send())?;
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
        self.query::<Value>(self.api.post(
            format!("/repos/{repo}/pulls/{number}/reviews"),
            Some(&payload),
        ))?;
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
    /// Fetch UTF-8 file content at a ref using the repository contents API.
    pub fn get_file_contents(
        &self,
        repo: &str,
        path: &str,
        git_ref: &str,
    ) -> Result<String, String> {
        let (owner, name) = split_repo(repo)?;
        let repos = self.api.repos(owner, name);
        const PATH_ESCAPE: &AsciiSet = &CONTROLS
            .add(b' ')
            .add(b'"')
            .add(b'#')
            .add(b'%')
            .add(b'<')
            .add(b'>')
            .add(b'?')
            .add(b'[')
            .add(b'\\')
            .add(b']')
            .add(b'`')
            .add(b'{')
            .add(b'}');
        let path = utf8_percent_encode(path, PATH_ESCAPE).to_string();
        let contents = self.query(repos.get_content().path(path).r#ref(git_ref).send())?;
        let file = contents
            .items
            .first()
            .ok_or("GitHubError: file content is missing")?;
        let encoded = file
            .content
            .as_deref()
            .ok_or("GitHubError: file content is missing")?;
        let cleaned: String = encoded.chars().filter(|c| !c.is_whitespace()).collect();
        let bytes = STANDARD
            .decode(cleaned)
            .map_err(|err| format!("GitHubError: {err}"))?;
        String::from_utf8(bytes).map_err(|err| format!("GitHubError: {err}"))
    }
}

fn split_repo(repo: &str) -> Result<(&str, &str), String> {
    repo.split_once('/')
        .ok_or_else(|| "GitHubError: expected owner/repository".into())
}

fn github_error(err: octocrab::Error) -> String {
    match err {
        octocrab::Error::GitHub { source, .. } => {
            format!("GitHubError: {} {}", source.status_code, source.message)
        }
        err => format!("GitHubError: {err}"),
    }
}
