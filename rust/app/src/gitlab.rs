//! GitLab operations used by the review pipeline.

use std::borrow::Cow;

use axum::http::Method;
use gitlab::Gitlab;
use gitlab::api::projects::{
    merge_requests::{
        EditMergeRequest, MergeRequest as GetMergeRequest, MergeRequestDiffs,
        notes::{CreateMergeRequestNote, EditMergeRequestNote, MergeRequestNotes},
    },
    repository::{commits::CompareCommits, files::FileRaw},
};
use gitlab::api::users::CurrentUser;
use gitlab::api::{
    self, Endpoint, Pagination, Query,
    common::{NameOrId, SortOrder},
};
use serde_json::Value;

pub struct GitlabClient {
    pub base_url: String,
    api: Gitlab,
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
        let base_url = base_url.trim_end_matches('/').to_string();
        let (scheme, host) = base_url
            .split_once("://")
            .ok_or("GitlabError: expected an HTTP(S) base URL")?;
        let mut builder = Gitlab::builder(host, token);
        match scheme {
            "https" => {}
            "http" => {
                builder.insecure();
            }
            _ => return Err("GitlabError: expected an HTTP(S) base URL".into()),
        }
        let api = builder.build().map_err(gitlab_error)?;
        Ok(Self { base_url, api })
    }

    fn merge_request(&self, repo: &str, iid: u64) -> Result<Value, String> {
        GetMergeRequest::builder()
            .project(repo)
            .merge_request(iid)
            .build()
            .map_err(gitlab_error)?
            .query(&self.api)
            .map_err(gitlab_error)
    }

    pub fn get_merge_request(&self, repo: &str, iid: u64) -> Result<MergeRequest, String> {
        let value = self.merge_request(repo, iid)?;
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
        let endpoint = MergeRequestDiffs::builder()
            .project(repo)
            .merge_request(iid)
            .build()
            .map_err(gitlab_error)?;
        let diffs: Vec<Value> = api::paged(endpoint, Pagination::All)
            .query(&self.api)
            .map_err(gitlab_error)?;
        Ok(diffs.into_iter().map(MrDiff::from).collect())
    }

    pub(crate) fn compare_review_heads(
        &self,
        repo: &str,
        previous: &str,
        head: &str,
    ) -> Result<Option<Vec<MrDiff>>, String> {
        let project = NameOrId::from(repo);
        let base: Value = AdditionalEndpoint {
            method: Method::GET,
            path: format!(
                "projects/{project}/repository/merge_base?refs%5B%5D={previous}&refs%5B%5D={head}"
            ),
        }
        .query(&self.api)
        .map_err(gitlab_error)?;
        if base["id"].as_str() != Some(previous) {
            return Ok(None);
        }
        let comparison: Value = CompareCommits::builder()
            .project(repo)
            .from(previous)
            .to(head)
            .straight(true)
            .build()
            .map_err(gitlab_error)?
            .query(&self.api)
            .map_err(gitlab_error)?;
        if comparison["compare_timeout"].as_bool() != Some(false) {
            return Ok(None);
        }
        let Some(diffs) = comparison["diffs"].as_array() else {
            return Ok(None);
        };
        if diffs.iter().any(|diff| {
            diff["collapsed"].as_bool() == Some(true) || diff["too_large"].as_bool() == Some(true)
        }) {
            return Ok(None);
        }
        Ok(Some(diffs.iter().cloned().map(MrDiff::from).collect()))
    }

    /// Raw file content at a ref (`/repository/files/<path>/raw?ref=`).
    pub fn get_raw_file(&self, repo: &str, path: &str, git_ref: &str) -> Result<String, String> {
        let endpoint = FileRaw::builder()
            .project(repo)
            .file_path(path)
            .ref_(git_ref)
            .build()
            .map_err(gitlab_error)?;
        let bytes: Vec<u8> = api::raw(endpoint).query(&self.api).map_err(gitlab_error)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// The project language percentages, used for "PR main language".
    pub fn get_languages(&self, repo: &str) -> Result<Vec<(String, i64)>, String> {
        let project = NameOrId::from(repo);
        let languages: std::collections::BTreeMap<String, f64> = AdditionalEndpoint {
            method: Method::GET,
            path: format!("projects/{project}/languages"),
        }
        .query(&self.api)
        .map_err(gitlab_error)?;
        Ok(languages
            .into_iter()
            .map(|(name, percent)| (name, (percent * 100.0).round() as i64))
            .collect())
    }

    pub fn create_note(&self, repo: &str, iid: u64, body: &str) -> Result<Note, String> {
        let value: Value = CreateMergeRequestNote::builder()
            .project(repo)
            .merge_request(iid)
            .body(body)
            .build()
            .map_err(gitlab_error)?
            .query(&self.api)
            .map_err(gitlab_error)?;
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
        let note_id = u64::try_from(note_id).map_err(gitlab_error)?;
        let endpoint = EditMergeRequestNote::builder()
            .project(repo)
            .merge_request(iid)
            .note(note_id)
            .body(body)
            .build()
            .map_err(gitlab_error)?;
        api::ignore(endpoint).query(&self.api).map_err(gitlab_error)
    }

    pub fn current_username(&self) -> Result<String, String> {
        let user: Value = CurrentUser::builder()
            .build()
            .map_err(gitlab_error)?
            .query(&self.api)
            .map_err(gitlab_error)?;
        let username = str_field(&user, "username");
        if username.is_empty() {
            return Err("GitLab API did not identify the authenticated user".into());
        }
        Ok(username)
    }

    pub fn get_notes(&self, repo: &str, iid: u64) -> Result<Vec<serde_json::Value>, String> {
        let endpoint = MergeRequestNotes::builder()
            .project(repo)
            .merge_request(iid)
            .sort(SortOrder::Ascending)
            .build()
            .map_err(gitlab_error)?;
        api::paged(endpoint, Pagination::All)
            .query(&self.api)
            .map_err(gitlab_error)
    }

    pub fn delete_note(&self, repo: &str, iid: u64, note_id: i64) -> Result<(), String> {
        let project = NameOrId::from(repo);
        let endpoint = AdditionalEndpoint {
            method: Method::DELETE,
            path: format!("projects/{project}/merge_requests/{iid}/notes/{note_id}"),
        };
        api::ignore(endpoint).query(&self.api).map_err(gitlab_error)
    }

    /// The labels currently on the merge request.
    pub fn get_labels(&self, repo: &str, iid: u64) -> Result<Vec<String>, String> {
        let value = self.merge_request(repo, iid)?;
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
        let mut builder = EditMergeRequest::builder();
        builder.project(repo).merge_request(iid);
        if labels.is_empty() {
            builder.clear_labels();
        } else {
            builder.labels(labels.iter().map(String::as_str));
        }
        let endpoint = builder.build().map_err(gitlab_error)?;
        api::ignore(endpoint).query(&self.api).map_err(gitlab_error)
    }

    /// Update the merge request title/description (`/describe` publishing).
    pub fn update_merge_request(
        &self,
        repo: &str,
        iid: u64,
        title: &str,
        description: &str,
    ) -> Result<(), String> {
        let endpoint = EditMergeRequest::builder()
            .project(repo)
            .merge_request(iid)
            .title(title)
            .description(description)
            .build()
            .map_err(gitlab_error)?;
        api::ignore(endpoint).query(&self.api).map_err(gitlab_error)
    }
}

// These two operations have no endpoint builder in the GitLab SDK.
struct AdditionalEndpoint {
    method: Method,
    path: String,
}

impl Endpoint for AdditionalEndpoint {
    fn method(&self) -> Method {
        self.method.clone()
    }

    fn endpoint(&self) -> Cow<'static, str> {
        self.path.clone().into()
    }
}

fn gitlab_error(err: impl std::fmt::Display) -> String {
    format!("GitlabError: {err}")
}

impl From<Value> for MrDiff {
    fn from(diff: Value) -> Self {
        Self {
            new_path: str_field(&diff, "new_path"),
            old_path: str_field(&diff, "old_path"),
            diff: str_field(&diff, "diff"),
            new_file: diff["new_file"].as_bool().unwrap_or_default(),
            deleted_file: diff["deleted_file"].as_bool().unwrap_or_default(),
            renamed_file: diff["renamed_file"].as_bool().unwrap_or_default(),
        }
    }
}

fn str_field(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}
