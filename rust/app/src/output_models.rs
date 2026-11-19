//! Output contract for the agent's YAML answers — the Rust equivalent of
//! The schema is unchanged from
//! the project: the same fields, the same strictness, the same
//! normalization the pydantic validators applied.
//!
// The literal tables and confidence helper serve the publishing post-processors
// that wire in with the git-providers port.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A file the model marked as relevant to one sub-PR (`SubPR`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubPr {
    pub relevant_files: Vec<String>,
    pub title: String,
}

/// One key issue, possibly carrying an inline improvement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyIssuesComponentLink {
    pub relevant_file: String,
    pub issue_header: String,
    pub issue_content: String,
    pub start_line: i64,
    pub end_line: i64,
    #[serde(default)]
    pub existing_code: Option<String>,
    #[serde(default)]
    pub improved_code: Option<String>,
    /// 0..=10; null when absent or out of range (the pydantic validator's
    /// behavior, ported by `finding_confidence`).
    #[serde(default)]
    pub confidence: Option<f64>,
}

/// A `/review` todo-sections row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TodoSection {
    pub relevant_file: String,
    pub line_number: i64,
    pub content: String,
}

/// Ticket-compliance row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TicketCompliance {
    pub ticket_url: String,
    pub ticket_requirements: String,
    pub fully_compliant_requirements: String,
    pub not_compliant_requirements: String,
    pub requires_further_human_verification: String,
}

/// Contribution-time cost estimate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContributionTimeCostEstimate {
    pub best_case: String,
    pub average_case: String,
    pub worst_case: String,
}

/// One failure mode of the change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FailureMode {
    pub what: String,
    pub r#where: String,
    pub trigger: String,
    pub detected_by: String,
    /// Strict bool: the string "true" must not coerce.
    pub covered_in_this_pr: bool,
}

/// The review payload inside `PRReview`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Review {
    #[serde(default)]
    pub ticket_compliance_check: Option<Vec<TicketCompliance>>,
    /// 1..=5, aliased `estimated_effort_to_review_[1-5]` in the YAML contract.
    #[serde(
        rename = "estimated_effort_to_review_[1-5]",
        default,
        deserialize_with = "de_strict_opt_int_1_5"
    )]
    pub estimated_effort_to_review: Option<i64>,
    #[serde(default, deserialize_with = "de_strip_literal")]
    pub risk_level: Option<String>,
    #[serde(default, deserialize_with = "de_strip_literal")]
    pub merge_recommendation: Option<String>,
    #[serde(default)]
    pub failure_modes: Option<Vec<FailureMode>>,
    #[serde(default)]
    pub artifact_findings: Option<Value>,
    #[serde(default)]
    pub review_priority_files: Option<Vec<String>>,
    #[serde(default)]
    pub contribution_time_cost_estimate: Option<ContributionTimeCostEstimate>,
    /// 0..=100.
    #[serde(default, deserialize_with = "de_strict_opt_int_0_100")]
    pub score: Option<i64>,
    #[serde(default, deserialize_with = "de_strip_literal")]
    pub relevant_tests: Option<String>,
    pub key_issues_to_review: Vec<KeyIssuesComponentLink>,
    #[serde(default)]
    pub security_concerns: Option<String>,
    /// Either the structured list or a free-text fallback string.
    #[serde(default)]
    pub todo_sections: Option<TodoSectionsValue>,
    /// At most three sub-PRs.
    #[serde(default)]
    pub can_be_split: Option<Vec<SubPr>>,
}

/// `todo_sections` accepts a list or a plain string.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TodoSectionsValue {
    List(Vec<TodoSection>),
    Text(String),
}

/// The top-level `/review` answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrReview {
    pub review: Review,
}

impl Review {
    /// The risk literals the contract accepts.
    pub const RISK_LEVELS: [&'static str; 3] = ["low", "medium", "high"];
    /// The merge-recommendation literals the contract accepts.
    pub const MERGE_RECOMMENDATIONS: [&'static str; 3] =
        ["no_concerns_found", "needs_review", "changes_required"];
    /// The relevant-tests literals the contract accepts.
    pub const RELEVANT_TESTS: [&'static str; 2] = ["Yes", "No"];
}

/// A `/describe` file row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileDescription {
    pub filename: String,
    #[serde(default)]
    pub changes_summary: Option<String>,
    pub changes_title: String,
    pub label: String,
}

/// The `/describe` answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrDescription {
    /// One or more PR types; string items are stripped before validation.
    #[serde(rename = "type", deserialize_with = "de_pr_types")]
    pub pr_type: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub title: String,
    #[serde(default)]
    pub changes_diagram: Option<String>,
    /// At most 20 files.
    #[serde(default)]
    pub pr_files: Option<Vec<FileDescription>>,
}

impl PrDescription {
    /// The PR-type literals the contract accepts.
    pub const TYPES: [&'static str; 5] =
        ["Bug fix", "Tests", "Enhancement", "Documentation", "Other"];
}

/// The confidence a finding carries, ported from `finding_confidence`: null
/// unless the value parses as a number in 0..=10 (booleans never do).
pub fn finding_confidence(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    if value.is_boolean() {
        return None;
    }
    let text = match value {
        Value::Number(n) => return clamp_confidence(n.as_f64()?),
        Value::String(s) => s.trim().to_string(),
        _ => return None,
    };
    text.parse::<f64>().ok().and_then(clamp_confidence)
}

fn clamp_confidence(value: f64) -> Option<f64> {
    (0.0..=10.0).contains(&value).then_some(value)
}

/// Strict optional integer in 1..=5 for the review-effort score.
fn de_strict_opt_int_1_5<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    de_strict_opt_int_in(deserializer, 1, 5)
}

/// Strict optional integer in 0..=100 for the review score.
fn de_strict_opt_int_0_100<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    de_strict_opt_int_in(deserializer, 0, 100)
}

fn de_strict_opt_int_in<'de, D>(
    deserializer: D,
    low: i64,
    high: i64,
) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(Value::Number(n)) if n.is_i64() => {
            let i = n.as_i64().unwrap_or_default();
            if (low..=high).contains(&i) {
                Ok(Some(i))
            } else {
                Err(serde::de::Error::custom(format!(
                    "value {i} outside {low}..={high}"
                )))
            }
        }
        Some(other) => Err(serde::de::Error::custom(format!(
            "expected a strict integer in {low}..={high}, got {other}"
        ))),
    }
}

/// Strip a literal value the way the pydantic before-validator did.
fn de_strip_literal<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(Value::String(s)) => Some(s.trim().to_string()),
        Some(Value::Null) | None => None,
        Some(other) => Some(other.to_string()),
    })
}

/// PR types: strip string items before enum validation.
fn de_pr_types<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<Value>::deserialize(deserializer)?;
    if raw.is_empty() {
        return Err(serde::de::Error::custom("at least one PR type is required"));
    }
    let mut out = Vec::with_capacity(raw.len());
    for item in raw {
        match item {
            Value::String(s) => out.push(s.trim().to_string()),
            other => out.push(other.to_string()),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REVIEW_YAML: &str = r#"
review:
  estimated_effort_to_review_[1-5]: 3
  risk_level: "medium "
  merge_recommendation: "needs_review"
  key_issues_to_review:
    - relevant_file: "src/main.rs"
      issue_header: "Header"
      issue_content: "Content"
      start_line: 1
      end_line: 5
      confidence: 7
  security_concerns: "none"
"#;

    #[test]
    fn review_contract_parses() {
        let answer: PrReview = serde_yaml_ng::from_str(REVIEW_YAML).unwrap();
        let review = answer.review;
        assert_eq!(review.estimated_effort_to_review, Some(3));
        assert_eq!(review.risk_level.as_deref(), Some("medium"));
        assert_eq!(review.merge_recommendation.as_deref(), Some("needs_review"));
        assert_eq!(review.key_issues_to_review.len(), 1);
        assert_eq!(review.key_issues_to_review[0].confidence, Some(7.0));
    }

    #[test]
    fn strict_int_rejects_strings() {
        let yaml = "review:\n  score: \"90\"\n  key_issues_to_review: []\n";
        assert!(serde_yaml_ng::from_str::<PrReview>(yaml).is_err());
    }

    #[test]
    fn score_out_of_range_rejected() {
        let yaml = "review:\n  score: 101\n  key_issues_to_review: []\n";
        assert!(serde_yaml_ng::from_str::<PrReview>(yaml).is_err());
    }

    #[test]
    fn describe_contract_parses() {
        let yaml = r#"
type:
  - "Enhancement
"
title: "Add feature"
pr_files:
  - filename: "a.rs"
    changes_title: "changed"
    label: "Rust"
"#;
        let answer: PrDescription = serde_yaml_ng::from_str(yaml).unwrap();
        // Trailing whitespace in block-scalar items is stripped before enum check.
        assert_eq!(answer.pr_type, vec!["Enhancement".to_string()]);
        assert_eq!(answer.title, "Add feature");
        assert_eq!(answer.pr_files.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn confidence_bounds() {
        assert_eq!(finding_confidence(None), None);
        assert_eq!(finding_confidence(Some(&serde_json::json!(true))), None);
        assert_eq!(finding_confidence(Some(&serde_json::json!(11))), None);
        assert_eq!(
            finding_confidence(Some(&serde_json::json!(" 8 "))),
            Some(8.0)
        );
        assert_eq!(finding_confidence(Some(&serde_json::json!("nan"))), None);
    }
}
