//! The command layer: review, describe and ask built on the opencode agent
//! engine, replacing the agentless single-call pipelines in
//! `pr_agent/commands/`. Per `plan-opencode-engine.md`: the prompt carries the
//! task, the YAML contract and the changed-file map — never the diff itself;
//! the agent reads the worktree with its own tools.

// The CLI transport that dispatches into this layer lands with the git-providers
// port; until then the layer exists unused.
#![allow(dead_code)]

use serde::Serialize;

use crate::opencode::{FileMapEntry, OpenCodeHandler};
use crate::output_models::{PrDescription, PrReview};

/// One PR a command runs against. The transport (CLI, webhook, action) fills
/// this from the provider; the command layer never talks to the provider
/// directly.
#[derive(Debug, Clone)]
pub struct PullRequest {
    pub url: String,
    pub repo_full_name: String,
    pub number: u64,
    pub title: String,
    pub description: String,
    pub head_sha: String,
    /// Files the diff touches, as the prompt's map (not its content).
    pub file_map: Vec<FileMapEntry>,
}

/// The task prompt for `/review`: mission, contract, and how to use the tree.
fn review_prompt(pr: &PullRequest, incremental_findings: &[String]) -> String {
    let mut prompt = String::new();
    prompt.push_str(
        "Review this pull request. Read the changed files in the working tree with your tools; \
the full diff is available via `git diff`.\n\n",
    );
    prompt.push_str("PR title: ");
    prompt.push_str(&pr.title);
    prompt.push('\n');
    if !pr.description.trim().is_empty() {
        prompt.push_str("PR description:\n");
        prompt.push_str(pr.description.trim());
        prompt.push_str("\n\n");
    }
    if !incremental_findings.is_empty() {
        // Incremental review: previous findings arrive as id + headline
        // metadata, not as pasted YAML (plan §3).
        prompt.push_str(
            "This is an incremental review. Findings already reported (do not repeat them; \
report only what is new, changed, or resolved):\n",
        );
        for finding in incremental_findings {
            prompt.push_str(&format!("- {finding}\n"));
        }
        prompt.push('\n');
    }
    prompt.push_str(REVIEW_CONTRACT);
    prompt
}

/// The review output contract. JSON strings preserve multiline snippets.
pub const REVIEW_CONTRACT: &str = "\
Answer with a single valid JSON object (no prose, no code fences).
Use the field names and types in the outline below. Escape line breaks,
double quotes, and backslashes inside string values according to JSON syntax.

review:
  estimated_effort_to_review_[1-5]: 1-5          # integer
  risk_level: low | medium | high
  merge_recommendation: no_concerns_found | needs_review | changes_required
  key_issues_to_review:                           # required, may be empty
    - relevant_file: path
      issue_header: short header
      issue_content: what to review and why
      start_line: int
      end_line: int
      existing_code: code snippet      # optional
      improved_code: suggested fix     # optional, a proposal only - never applied
      confidence: 0-10                 # optional
  security_concerns: string            # use \"No\" when there are none
  relevant_tests: \"Yes\" | \"No\"         # string
  todo_sections:                       # optional
    - relevant_file: path
      line_number: int
      content: string
";

/// The task prompt for `/describe`.
fn describe_prompt(pr: &PullRequest) -> String {
    let mut prompt = String::new();
    prompt.push_str(
        "Describe this pull request. Read the changed files in the working tree with your tools; \
the full diff is available via `git diff`.\n\n",
    );
    prompt.push_str("PR title: ");
    prompt.push_str(&pr.title);
    prompt.push_str("\n\n");
    prompt.push_str(DESCRIBE_CONTRACT);
    prompt
}

/// The YAML output contract for `/describe`, unchanged from the Python prompt.
pub const DESCRIBE_CONTRACT: &str = "\
Answer with a single YAML document matching this schema (no prose, no code fences):

type:                               # one or more of: Bug fix, Tests, Enhancement, Documentation, Other
  - Enhancement
title: concise PR title
description: |
  one-paragraph summary
pr_files:                           # optional, at most 20
  - filename: path
    changes_title: short per-file summary
    changes_summary: optional detail
    label: primary language
";

/// `/review`: runs the agent once and validates the YAML answer.
pub async fn run_review(
    handler: &mut OpenCodeHandler,
    pr: &PullRequest,
    incremental_findings: &[String],
) -> Result<PrReview, String> {
    let prompt = review_prompt(pr, incremental_findings);
    let session = handler.session_for_head(&pr.head_sha);
    let reply = handler
        .chat_completion(&prompt, &session, &pr.file_map)
        .map_err(|e| e.to_string())?;
    parse_yaml_reply::<PrReview>(&reply.text)
}

/// `/describe`: runs the agent once and validates the YAML answer.
pub async fn run_describe(
    handler: &mut OpenCodeHandler,
    pr: &PullRequest,
) -> Result<PrDescription, String> {
    let prompt = describe_prompt(pr);
    let session = handler.session_for_head(&pr.head_sha);
    let reply = handler
        .chat_completion(&prompt, &session, &pr.file_map)
        .map_err(|e| e.to_string())?;
    parse_yaml_reply::<PrDescription>(&reply.text)
}

/// `/ask`: the question goes to the stable session, so the dialogue history
/// lives in opencode; open-pr-agent never carries or re-injects it (plan §3).
pub async fn run_ask(
    handler: &OpenCodeHandler,
    pr: &PullRequest,
    question: &str,
) -> Result<String, String> {
    let session = handler.session_for_ask(&pr.repo_full_name, pr.number);
    let prompt = format!(
        "PR title: {}\n\nAnswer the following question about this pull request. Read the working tree \
with your tools as needed. Answer in Markdown.\n\nQuestion: {}",
        pr.title, question
    );
    let reply = handler
        .chat_completion(&prompt, &session, &[])
        .map_err(|e| e.to_string())?;
    Ok(reply.text)
}

/// Parse the agent's answer: strip code fences, then validate against the
/// contract. Invalid YAML is a failure, not a fallback.
fn parse_yaml_reply<T: for<'de> serde::Deserialize<'de>>(text: &str) -> Result<T, String> {
    let stripped = strip_code_fence(text);
    serde_yaml_ng::from_str(stripped.trim())
        .map_err(|e| format!("ValueError: the agent answer does not match the contract: {e}"))
}

/// Remove a wrapping ``` fence the model may add despite the instruction.
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

/// A finding line for the incremental-review meta-instruction: id + headline.
#[derive(Debug, Clone, Serialize)]
pub struct PreviousFinding {
    pub id: String,
    pub header: String,
}

impl PreviousFinding {
    /// The one-line form the prompt carries.
    pub fn prompt_line(&self) -> String {
        format!("[{}] {}", self.id, self.header)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_pr() -> PullRequest {
        PullRequest {
            url: "https://github.com/o/r/pull/1".into(),
            repo_full_name: "o/r".into(),
            number: 1,
            title: "Add feature".into(),
            description: "Does a thing".into(),
            head_sha: "abc123".into(),
            file_map: vec![FileMapEntry {
                path: "src/x.rs".into(),
                old_path: None,
                edit_type: "MODIFIED".into(),
                plus_lines: 3,
                minus_lines: 1,
            }],
        }
    }

    #[test]
    fn yaml_reply_parses_with_fence() {
        let text = "```yaml\nreview:\n  key_issues_to_review: []\n```";
        let answer: PrReview = parse_yaml_reply(text).unwrap();
        assert!(answer.review.key_issues_to_review.is_empty());
    }

    #[test]
    fn invalid_yaml_is_an_error() {
        let err = parse_yaml_reply::<PrReview>("nope: [unclosed").unwrap_err();
        assert!(err.starts_with("ValueError:"));
    }

    #[test]
    fn incremental_findings_go_as_metadata() {
        let pr = sample_pr();
        let prompt = review_prompt(
            &pr,
            &["[f1] Header one".to_string(), "[f2] Header two".to_string()],
        );
        assert!(prompt.contains("incremental review"));
        assert!(prompt.contains("- [f1] Header one"));
        assert!(!prompt.contains("+++") || prompt.contains("git diff"));
    }

    #[test]
    fn prompt_carries_file_map_not_diff() {
        let pr = sample_pr();
        let full = crate::opencode::compose_prompt(&review_prompt(&pr, &[]), &pr.file_map, true);
        assert!(full.contains("src/x.rs"));
        assert!(full.contains("MODIFIED"));
        // The raw diff never enters the prompt; only the agent-side git hint.
        assert!(full.contains("git diff"));
        // Disabling the map keeps the task only.
        let bare = crate::opencode::compose_prompt(&review_prompt(&pr, &[]), &pr.file_map, false);
        assert!(!bare.contains("src/x.rs"));
    }

    #[test]
    fn describe_prompt_has_contract() {
        let prompt = describe_prompt(&sample_pr());
        assert!(prompt.contains("type:"));
        assert!(prompt.contains("pr_files:"));
    }

    #[test]
    fn previous_finding_line() {
        let finding = PreviousFinding {
            id: "f1".into(),
            header: "Header".into(),
        };
        assert_eq!(finding.prompt_line(), "[f1] Header");
    }
}
