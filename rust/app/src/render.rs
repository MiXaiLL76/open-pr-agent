//! The review markdown renderer: the `convert_to_markdown` table port for the
//! fields the `/review` schema carries. Input is the parsed review YAML mapping
//! (insertion-ordered, `key_issues_to_review` moved last by the pipeline), so
//! rows come out in the same order the original rendered them.

use serde_yaml_ng::Value;

/// English strings from `pr_agent/settings/i18n/en.toml`; the i18n file
/// selection ports with the remaining settings machinery.
mod i18n {
    pub const ESTIMATED_EFFORT: &str = "Estimated effort to review";
    pub const NO_RELEVANT_TESTS: &str = "No relevant tests";
    pub const CONTAINS_TESTS: &str = "PR contains tests";
    pub const SECURITY_CONCERNS: &str = "Security concerns";
    pub const NO_SECURITY_CONCERNS: &str = "No security concerns identified";
    pub const FOCUS_AREAS: &str = "Recommended focus areas for review";
    pub const NO_MAJOR_ISSUES: &str = "No major issues detected";
    pub const SUGGESTED_FIX: &str = "Suggested fix";
}

/// Render the full review markdown, GFM tables when supported.
pub fn render_review_markdown(review: &Value, gfm: bool, intro: bool) -> String {
    let mut text = String::from("## PR Reviewer Guide 🔍\n\n");
    if intro {
        text.push_str("Here are some key observations to aid the review process:\n\n");
    }
    if !gfm {
        // Non-GFM providers get `###` sections; ported with the providers that need them.
        return text;
    }
    text.push_str("<table>\n");
    let Some(entries) = review.as_mapping() else {
        return text;
    };
    for (key, value) in entries {
        let key = match key.as_str() {
            Some(k) => k,
            None => continue,
        };
        render_row(&mut text, key, value);
    }
    text.push_str("</table>\n");
    text
}

fn render_row(text: &mut String, key: &str, value: &Value) {
    // Empty values are skipped except for the list-valued sections that carry
    // their own "none" rendering.
    if is_empty_value(value)
        && !matches!(
            key,
            "can_be_split"
                | "key_issues_to_review"
                | "review_priority_files"
                | "failure_modes"
                | "artifact_findings"
        )
    {
        return;
    }
    if key == "estimated_effort_to_review_[1-5]" {
        render_effort(text, value);
    } else if key == "relevant_tests" {
        render_tests(text, value);
    } else if key == "security_concerns" {
        render_security(text, value);
    } else if key == "risk_level" {
        let risk = scalar(value).trim().to_lowercase().replace('_', " ");
        let display = if risk.is_empty() {
            "Unknown".to_string()
        } else {
            capitalize(&risk)
        };
        text.push_str(&format!(
            "<tr><td>⚠️&nbsp;<strong>Risk level</strong>: {display}</td></tr>\n"
        ));
    } else if key == "merge_recommendation" {
        let recommendation = scalar(value).trim().replace('_', " ");
        let display = if recommendation.is_empty() {
            "Unknown".to_string()
        } else {
            capitalize(&recommendation)
        };
        text.push_str(&format!(
            "<tr><td>✅&nbsp;<strong>Merge recommendation</strong>: {display}</td></tr>\n"
        ));
    } else if key == "key_issues_to_review" {
        render_key_issues(text, value);
    }
    // The remaining schema sections (failure modes, artifact findings, ticket
    // compliance, ...) render with the sections' publishers when they port.
}

fn render_effort(text: &mut String, value: &Value) {
    let raw = scalar(value).trim().to_string();
    let effort = match raw
        .split(',')
        .next()
        .and_then(|first| first.trim().parse::<i64>().ok())
    {
        Some(n) => n.clamp(1, 5),
        None => return,
    };
    let bars = "🔵".repeat(effort as usize);
    let rest = "⚪".repeat(5 - effort as usize);
    text.push_str(&format!(
        "<tr><td>⏱️&nbsp;<strong>{}</strong>: {effort} {bars}{rest}</td></tr>\n",
        i18n::ESTIMATED_EFFORT
    ));
}

fn render_tests(text: &mut String, value: &Value) {
    if is_value_no(value) {
        text.push_str(&format!(
            "<tr><td>🧪&nbsp;<strong>{}</strong></td></tr>\n",
            i18n::NO_RELEVANT_TESTS
        ));
    } else {
        text.push_str(&format!(
            "<tr><td>🧪&nbsp;<strong>{}</strong></td></tr>\n",
            i18n::CONTAINS_TESTS
        ));
    }
}

fn render_security(text: &mut String, value: &Value) {
    if is_value_no(value) {
        text.push_str(&format!(
            "<tr><td>🔒&nbsp;<strong>{}</strong></td></tr>\n",
            i18n::NO_SECURITY_CONCERNS
        ));
    } else {
        text.push_str(&format!(
            "<tr><td>🔒&nbsp;<strong>{}</strong><br><br>\n\n{}</td></tr>\n",
            i18n::SECURITY_CONCERNS,
            scalar(value).trim()
        ));
    }
}

fn render_key_issues(text: &mut String, value: &Value) {
    if is_value_no(value) {
        text.push_str(&format!(
            "<tr><td>⚡&nbsp;<strong>{}</strong></td></tr>\n",
            i18n::NO_MAJOR_ISSUES
        ));
        return;
    }
    text.push_str(&format!(
        "<tr><td>⚡&nbsp;<strong>{}</strong><br><br>\n\n",
        i18n::FOCUS_AREAS
    ));
    let Some(issues) = value.as_sequence() else {
        text.push_str("</td></tr>\n");
        return;
    };
    for issue in issues {
        let Some(issue) = issue.as_mapping() else {
            continue;
        };
        let field = |name: &str| {
            issue
                .get(Value::String(name.to_string()))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .trim()
                .to_string()
        };
        // The suggested fix keeps its indentation: the original only right-strips it.
        let raw_field = |name: &str| {
            issue
                .get(Value::String(name.to_string()))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        };
        let header = field("issue_header");
        let content = field("issue_content");
        let mut issue_str = format!("<strong>{header}</strong><br>{content}");
        let improved = raw_field("improved_code");
        if !improved.trim().is_empty() {
            let fix = improved.trim_end();
            let fence = suggestion_fence(fix);
            issue_str.push_str(&format!(
                "\n\n<details><summary>{}</summary>\n\n{fence}text\n{fix}\n{fence}\n\n</details>",
                i18n::SUGGESTED_FIX
            ));
        }
        text.push_str(&issue_str);
        text.push_str("\n\n");
    }
    text.push_str("</td></tr>\n");
}

/// The original's `is_value_no`: falsy, or one of the plain negative words.
pub fn is_value_no(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(false) => true,
        Value::String(s) => matches!(s.trim().to_lowercase().as_str(), "no" | "none" | "false"),
        Value::Number(n) => n.as_f64() == Some(0.0),
        _ => false,
    }
}

fn is_empty_value(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Sequence(s) => s.is_empty(),
        Value::Mapping(m) => m.is_empty(),
        // A `False` value is not empty for the renderer: the original's check
        // (`value == ''` and friends) keeps it, and "relevant tests" renders it.
        _ => false,
    }
}

fn scalar(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Backtick fence for a suggestion block: at least three ticks, extended past
/// any run of backticks inside the code so the block cannot close early.
fn suggestion_fence(code: &str) -> String {
    let mut longest: usize = 0;
    let mut run = 0;
    for ch in code.chars() {
        if ch == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let ticks = longest.saturating_add(1).max(3);
    "`".repeat(ticks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_yaml_ng::Mapping;

    fn review_mapping() -> Value {
        let yaml = "estimated_effort_to_review_[1-5]: 3\n\
                    risk_level: medium\n\
                    merge_recommendation: needs_review\n\
                    key_issues_to_review:\n  - relevant_file: src/calculator.py\n    issue_header: Division without zero check\n    issue_content: The divide method does not guard against a zero divisor.\n    start_line: 12\n    end_line: 12\n    existing_code: \"x\"\n    improved_code: \"    def divide(self, a, b):\n        if b == 0:\n            raise ValueError(\\\"cannot divide by zero\\\")\n        return a / b\"\n    confidence: 8\n\
                    security_concerns: None identified\n\
                    relevant_tests: No\n";
        serde_yaml_ng::from_str::<Value>(yaml).unwrap()
    }

    #[test]
    fn renders_the_contract_table() {
        let mut review = review_mapping();
        // The pipeline moves the key issues to the end before rendering.
        let mapping = review.as_mapping_mut().unwrap();
        let issues = mapping
            .remove(Value::String("key_issues_to_review".into()))
            .unwrap();
        mapping.insert(Value::String("key_issues_to_review".into()), issues);
        let text = render_review_markdown(&review, true, true);
        assert!(text.starts_with(
            "## PR Reviewer Guide 🔍\n\nHere are some key observations to aid the review process:\n\n<table>\n"
        ));
        assert!(text.contains(
            "<tr><td>⏱️&nbsp;<strong>Estimated effort to review</strong>: 3 🔵🔵🔵⚪⚪</td></tr>\n"
        ));
        assert!(text.contains("<tr><td>⚠️&nbsp;<strong>Risk level</strong>: Medium</td></tr>\n"));
        assert!(text.contains(
            "<tr><td>✅&nbsp;<strong>Merge recommendation</strong>: Needs review</td></tr>\n"
        ));
        assert!(text.contains("Security concerns</strong><br><br>\n\nNone identified"));
        assert!(text.contains("<tr><td>🧪&nbsp;<strong>No relevant tests</strong></td></tr>\n"));
        assert!(text.contains("<strong>Recommended focus areas for review</strong><br><br>\n\n"));
        assert!(text.contains("<strong>Division without zero check</strong><br>The divide method does not guard against a zero divisor."));
        assert!(text.contains("<details><summary>Suggested fix</summary>\n\n```text\n"));
        assert!(text.ends_with("</details>\n\n</td></tr>\n</table>\n"));
    }

    #[test]
    fn key_issues_move_to_the_end() {
        // The pipeline reorders the mapping before rendering; verify the full row order.
        // Mapping::remove is a swap-remove, so the reorder rebuilds in order.
        let raw = review_mapping();
        let mut ordered = Mapping::new();
        let mut issues = None;
        for (k, v) in raw.as_mapping().unwrap().iter() {
            if k.as_str() == Some("key_issues_to_review") {
                issues = Some(v.clone());
            } else {
                ordered.insert(k.clone(), v.clone());
            }
        }
        if let Some(issues) = issues {
            ordered.insert(Value::String("key_issues_to_review".into()), issues);
        }
        let review = Value::Mapping(ordered);
        let text = render_review_markdown(&review, true, true);
        let positions = [
            text.find("Estimated effort to review").unwrap(),
            text.find("Risk level").unwrap(),
            text.find("Merge recommendation").unwrap(),
            text.find("Security concerns").unwrap(),
            text.find("No relevant tests").unwrap(),
            text.find("Recommended focus areas").unwrap(),
        ];
        assert!(
            positions.windows(2).all(|w| w[0] < w[1]),
            "rows out of order: {positions:?}"
        );
    }

    #[test]
    fn value_no_matches_the_original() {
        assert!(is_value_no(&Value::Bool(false)));
        assert!(is_value_no(
            &serde_yaml_ng::from_str::<Value>("No").unwrap()
        ));
        assert!(!is_value_no(&Value::String("None identified".into())));
    }

    #[test]
    fn fence_extends_past_inner_backticks() {
        assert_eq!(suggestion_fence("plain"), "```");
        assert_eq!(suggestion_fence("a ``` b"), "````");
    }

    #[test]
    fn mapping_preserves_yaml_order() {
        let value = review_mapping();
        let keys: Vec<String> = value
            .as_mapping()
            .unwrap()
            .keys()
            .filter_map(|k| k.as_str().map(|s| s.to_string()))
            .collect();
        assert_eq!(
            keys.first().map(String::as_str),
            Some("estimated_effort_to_review_[1-5]")
        );
        assert_eq!(keys.last().map(String::as_str), Some("relevant_tests"));
        // Unused-variant lint appeasement for Mapping import in tests.
        let _ = Mapping::new();
    }
}
