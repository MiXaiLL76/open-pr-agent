//! Typed projection of the diff parser for the live path.
//!
//! `port` keeps the recorded contract shape (Python object records) that the
//! gate replays; live callers want structs, so this module projects those
//! records onto plain Rust types once, at the boundary.

use serde_json::Value;

/// The edit kinds a changed file can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditType {
    Added,
    Deleted,
    Modified,
    Renamed,
}

impl EditType {
    /// The spelling the original's enum carried (`ADDED`, `DELETED`, ...).
    pub fn name(self) -> &'static str {
        match self {
            EditType::Added => "ADDED",
            EditType::Deleted => "DELETED",
            EditType::Modified => "MODIFIED",
            EditType::Renamed => "RENAMED",
        }
    }
}

/// One changed file of a diff, typed.
#[derive(Debug, Clone)]
pub struct FileDiffRecord {
    /// The path reviewers see (target path; source path for deletions).
    pub filename: String,
    /// The pre-rename path, when the file is a rename.
    pub old_filename: Option<String>,
    /// The unified patch body for this file.
    pub patch: String,
    /// The edit kind.
    pub edit_type: EditType,
    /// Added line count across hunks.
    pub num_plus_lines: i64,
    /// Removed line count across hunks.
    pub num_minus_lines: i64,
}

/// Parse a unified diff into typed file records. Binary files are skipped,
/// matching the contract layer. Non-diff text parses to no records (the
/// contract layer is lenient, like the original).
pub fn parse_diff_records(diff_text: &str) -> Result<Vec<FileDiffRecord>, String> {
    let files = crate::port::parse_unified_diff(diff_text.to_string())?;
    Ok(files.iter().filter_map(project_record).collect())
}

/// Project one recorded record onto the typed struct; records missing the
/// projected fields are skipped rather than failing the whole parse.
fn project_record(file: &Value) -> Option<FileDiffRecord> {
    let attrs = file.get("attrs")?;
    let filename = attrs.get("filename")?.as_str()?.to_string();
    let patch = attrs
        .get("patch")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let old_filename = attrs
        .get("old_filename")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    let num_plus_lines = attrs
        .get("num_plus_lines")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let num_minus_lines = attrs
        .get("num_minus_lines")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let edit_type = match attrs
        .pointer("/edit_type/attrs/_name_")
        .and_then(Value::as_str)
    {
        Some("ADDED") => EditType::Added,
        Some("DELETED") => EditType::Deleted,
        Some("RENAMED") => EditType::Renamed,
        _ => EditType::Modified,
    };
    Some(FileDiffRecord {
        filename,
        old_filename,
        patch,
        edit_type,
        num_plus_lines,
        num_minus_lines,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "\
diff --git a/src/app.py b/src/app.py
new file mode 100644
index 0000000..1111111
--- /dev/null
+++ b/src/app.py
@@ -0,0 +1,2 @@
+def run():
+    return 42
";

    #[test]
    fn records_carry_the_contract_fields() {
        let records = parse_diff_records(DIFF).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].filename, "src/app.py");
        assert_eq!(records[0].edit_type, EditType::Added);
        assert_eq!(records[0].edit_type.name(), "ADDED");
        assert_eq!(records[0].num_plus_lines, 2);
        assert_eq!(records[0].num_minus_lines, 0);
        assert!(records[0].patch.contains("@@ -0,0 +1,2 @@"));
    }

    #[test]
    fn non_diff_text_yields_no_records() {
        // The contract layer is lenient like the original: garbage parses to
        // no files rather than an error.
        assert!(parse_diff_records("not a diff at all").unwrap().is_empty());
    }
}
