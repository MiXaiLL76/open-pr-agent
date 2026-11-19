#![forbid(unsafe_code)]

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

// --- regexes mirrored from the `unidiff` library and pr_agent helpers ---

static RE_SOURCE_FILENAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^--- ("?[^\t\n]*"?)(?:\t([^\n]+))?"#).unwrap());
static RE_TARGET_FILENAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\+\+\+ ("?[^\t\n]*"?)(?:\t([^\n]+))?"#).unwrap());
static RE_DIFF_GIT_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^diff --git ("?a/[^\t\n]+"?) ("?b/[^\t\n]+"?)"#).unwrap());
static RE_DIFF_GIT_HEADER_URI_LIKE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^diff --git (.*://[^\t\n]+) (.*://[^\t\n]+)").unwrap());
static RE_DIFF_GIT_HEADER_NO_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^diff --git ([^\t\n]+) ([^\t\n]+)").unwrap());
static RE_DIFF_GIT_DELETED_FILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^deleted file mode (\d+)$").unwrap());
static RE_DIFF_GIT_NEW_FILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^new file mode (\d+)$").unwrap());
static RE_DIFF_GIT_OLD_MODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^old mode (\d+)$").unwrap());
static RE_DIFF_GIT_NEW_MODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^new mode (\d+)$").unwrap());
static RE_DIFF_GIT_INDEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^index [0-9a-f]+\.\.[0-9a-f]+ (\d+)$").unwrap());
static RE_HUNK_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@[ ]?(.*)").unwrap());
static RE_HUNK_BODY_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)^([- \+\\])(.*)").unwrap());
static RE_HUNK_EMPTY_BODY_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([- \+\\]?)([\r\n]{1,2})").unwrap());
static RE_BINARY_DIFF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^Binary files? ([^\t]+?)(?:\t([\s0-9:+-]+))?(?: and ([^\t]+?)(?:\t([\s0-9:+-]+))?)? (differ|has changed)")
        .unwrap()
});

// _HUNK_HEADER from pr_agent/git_providers/diff_parsing.py (hunk-length tracking)
static HUNK_ONLY_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@").unwrap());

const DEV_NULL: &str = "/dev/null";
const LINE_VALUE_NO_NEWLINE: &str = " No newline at end of file";

// _SPLITLINES_BREAK_CHARS from pr_agent/algo/git_patch_processing.py
fn is_splitlines_break_char(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r'
            | '\u{b}'
            | '\u{c}'
            | '\u{1c}'
            | '\u{1d}'
            | '\u{1e}'
            | '\u{85}'
            | '\u{2028}'
            | '\u{2029}'
    )
}

/// Split into lines keeping the `\n` terminator, like iterating an `io.StringIO`.
fn split_keep_terminators(text: &str) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    let mut start = 0;
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            out.push(&text[start..=i]);
            start = i + 1;
        }
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// Drop exactly one trailing `\n`, mirroring Python's `$` matching before a final newline.
fn strip_trailing_nl(line: &str) -> &str {
    line.strip_suffix('\n').unwrap_or(line)
}

fn parse_int_sat(s: &str) -> i64 {
    s.parse::<i64>().unwrap_or(i64::MAX)
}

// --- the subset of `unidiff` needed by diff_parsing.py ---

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineType {
    Added,
    Removed,
    Context,
    NoNewline,
    Empty,
}

impl LineType {
    fn prefix(self) -> &'static str {
        match self {
            LineType::Added => "+",
            LineType::Removed => "-",
            LineType::Context => " ",
            LineType::NoNewline => "\\",
            LineType::Empty => "",
        }
    }
}

struct Line {
    value: String,
    line_type: LineType,
}

impl Line {
    fn to_patch_string(&self) -> String {
        format!("{}{}", self.line_type.prefix(), self.value)
    }
}

struct Hunk {
    source_start: i64,
    source_length: i64,
    target_start: i64,
    target_length: i64,
    section_header: String,
    lines: Vec<Line>,
}

impl Hunk {
    fn added(&self) -> i64 {
        self.lines
            .iter()
            .filter(|l| l.line_type == LineType::Added)
            .count() as i64
    }
    fn removed(&self) -> i64 {
        self.lines
            .iter()
            .filter(|l| l.line_type == LineType::Removed)
            .count() as i64
    }
    fn to_patch_string(&self) -> String {
        let mut out = format!(
            "@@ -{},{} +{},{} @@{}{}\n",
            self.source_start,
            self.source_length,
            self.target_start,
            self.target_length,
            if self.section_header.is_empty() {
                ""
            } else {
                " "
            },
            self.section_header
        );
        for line in &self.lines {
            out.push_str(&line.to_patch_string());
        }
        out
    }
}

struct PatchedFile {
    patch_info: Option<usize>, // index into the parse arena of extended-info lines
    source_file: String,
    target_file: String,
    source_timestamp: Option<String>,
    target_timestamp: Option<String>,
    is_binary_file: bool,
    hunks: Vec<Hunk>,
}

impl PatchedFile {
    fn is_rename(&self) -> bool {
        let tail = |s: &str| s.chars().skip(2).collect::<String>();
        self.source_file != DEV_NULL
            && self.target_file != DEV_NULL
            && tail(&self.source_file) != tail(&self.target_file)
    }
    fn is_added_file(&self) -> bool {
        if self.source_file == DEV_NULL {
            return true;
        }
        self.hunks.len() == 1 && self.hunks[0].source_start == 0 && self.hunks[0].source_length == 0
    }
    fn is_removed_file(&self) -> bool {
        if self.target_file == DEV_NULL {
            return true;
        }
        self.hunks.len() == 1 && self.hunks[0].target_start == 0 && self.hunks[0].target_length == 0
    }
    fn to_patch_string(&self, infos: &[Vec<String>]) -> String {
        let mut out = String::new();
        if let Some(id) = self.patch_info {
            for l in &infos[id] {
                out.push_str(l);
            }
        }
        if !self.is_binary_file && !self.hunks.is_empty() {
            out.push_str(&format!("--- {}", self.source_file));
            if let Some(ts) = &self.source_timestamp {
                out.push('\t');
                out.push_str(ts);
            }
            out.push('\n');
            out.push_str(&format!("+++ {}", self.target_file));
            if let Some(ts) = &self.target_timestamp {
                out.push('\t');
                out.push_str(ts);
            }
            out.push('\n');
        }
        for hunk in &self.hunks {
            out.push_str(&hunk.to_patch_string());
        }
        out
    }
}

struct PatchSet {
    files: Vec<PatchedFile>,
    infos: Vec<Vec<String>>,
}

/// Port of `unidiff.PatchSet._parse` plus `PatchedFile._parse_hunk` (metadata_only=False).
fn parse_patch_set(text: &str) -> Result<PatchSet, String> {
    let lines = split_keep_terminators(text);
    let mut set = PatchSet {
        files: Vec::new(),
        infos: Vec::new(),
    };
    let mut current: Option<usize> = None;
    let mut patch_info: Option<usize> = None;
    let mut source_file: Option<String> = None;
    let mut source_timestamp: Option<String> = None;

    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        i += 1;

        // git file rename / header
        let git_header = RE_DIFF_GIT_HEADER
            .captures(line)
            .or_else(|| RE_DIFF_GIT_HEADER_URI_LIKE.captures(line))
            .or_else(|| RE_DIFF_GIT_HEADER_NO_PREFIX.captures(line));
        if let Some(c) = git_header {
            let info = set.infos.len();
            set.infos.push(Vec::new());
            patch_info = Some(info);
            let source = c[1].to_string();
            let target = c[2].to_string();
            current = Some(set.files.len());
            set.files.push(PatchedFile {
                patch_info: Some(info),
                source_file: source,
                target_file: target,
                source_timestamp: None,
                target_timestamp: None,
                is_binary_file: false,
                hunks: Vec::new(),
            });
            set.infos[info].push(line.to_string());
            continue;
        }

        // git new file
        if RE_DIFF_GIT_NEW_FILE
            .captures(strip_trailing_nl(line))
            .is_some()
        {
            let (cur, info) = need_file_and_info(current, patch_info)
                .ok_or_else(|| format!("UnidiffParseError: Unexpected new file found: {line}"))?;
            set.files[cur].source_file = DEV_NULL.to_string();
            set.infos[info].push(line.to_string());
            continue;
        }

        // git deleted file
        if RE_DIFF_GIT_DELETED_FILE
            .captures(strip_trailing_nl(line))
            .is_some()
        {
            let (cur, info) = need_file_and_info(current, patch_info).ok_or_else(|| {
                format!("UnidiffParseError: Unexpected deleted file found: {line}")
            })?;
            set.files[cur].target_file = DEV_NULL.to_string();
            set.infos[info].push(line.to_string());
            continue;
        }

        if let (Some(_cur), Some(info)) = (current, patch_info) {
            if RE_DIFF_GIT_OLD_MODE
                .captures(strip_trailing_nl(line))
                .is_some()
            {
                set.infos[info].push(line.to_string());
                continue;
            }
            if RE_DIFF_GIT_NEW_MODE
                .captures(strip_trailing_nl(line))
                .is_some()
            {
                set.infos[info].push(line.to_string());
                continue;
            }
            if RE_DIFF_GIT_INDEX
                .captures(strip_trailing_nl(line))
                .is_some()
            {
                set.infos[info].push(line.to_string());
                continue;
            }
        }

        // source file header
        if let Some(c) = RE_SOURCE_FILENAME.captures(line) {
            source_file = Some(c[1].to_string());
            source_timestamp = c.get(2).map(|m| m.as_str().to_string());
            if let Some(cur) = current {
                if patch_info.is_none() {
                    current = None;
                } else {
                    set.files[cur].source_timestamp = source_timestamp.clone();
                }
            }
            continue;
        }

        // target file header
        if let Some(c) = RE_TARGET_FILENAME.captures(line) {
            let target_file = c[1].to_string();
            let target_timestamp = c.get(2).map(|m| m.as_str().to_string());
            if let Some(cur) = current {
                if set.files[cur].target_file != target_file {
                    return Err(format!("UnidiffParseError: Target without source: {line}"));
                }
                set.files[cur].target_timestamp = target_timestamp;
            } else {
                let Some(src) = source_file.take() else {
                    return Err(format!("UnidiffParseError: Target without source: {line}"));
                };
                let ts = source_timestamp.take();
                current = Some(set.files.len());
                set.files.push(PatchedFile {
                    patch_info,
                    source_file: src,
                    target_file,
                    source_timestamp: ts,
                    target_timestamp: None,
                    is_binary_file: false,
                    hunks: Vec::new(),
                });
                patch_info = None;
            }
            continue;
        }

        // hunk header
        if let Some(c) = RE_HUNK_HEADER.captures(line) {
            patch_info = None;
            let Some(cur) = current else {
                return Err(format!("UnidiffParseError: Unexpected hunk found: {line}"));
            };
            parse_hunk(&mut set, cur, &c, &lines, &mut i)?;
            continue;
        }

        // no newline marker
        if line.starts_with("\\ No newline at end of file") {
            let Some(cur) = current else {
                return Err(format!("UnidiffParseError: Unexpected marker: {line}"));
            };
            let file = &mut set.files[cur];
            if file.hunks.is_empty() {
                return Err(format!(
                    "UnidiffParseError: Unexpected marker:{LINE_VALUE_NO_NEWLINE}"
                ));
            }
            let last = file.hunks.len() - 1;
            file.hunks[last].lines.push(Line {
                value: format!("{LINE_VALUE_NO_NEWLINE}\n"),
                line_type: LineType::NoNewline,
            });
            continue;
        }

        // trailing empty line after a hunk
        if line == "\n" && current.is_some_and(|cur| !set.files[cur].hunks.is_empty()) {
            let file = &mut set.files[current.unwrap()];
            let last = file.hunks.len() - 1;
            file.hunks[last].lines.push(Line {
                value: "\n".to_string(),
                line_type: LineType::Empty,
            });
            continue;
        }

        // anything else is patch info; start a detached one if needed
        if patch_info.is_none() {
            current = None;
            patch_info = Some(set.infos.len());
            set.infos.push(Vec::new());
        }

        if let Some(c) = RE_BINARY_DIFF.captures(line) {
            let src = c[1].to_string();
            let tgt = c
                .get(3)
                .map(|m| m.as_str().to_string())
                .unwrap_or_else(|| src.clone());
            set.infos[patch_info.unwrap()].push(line.to_string());
            if let Some(cur) = current {
                set.files[cur].is_binary_file = true;
            } else {
                let info = patch_info;
                set.files.push(PatchedFile {
                    patch_info: info,
                    source_file: src,
                    target_file: tgt,
                    source_timestamp: None,
                    target_timestamp: None,
                    is_binary_file: true,
                    hunks: Vec::new(),
                });
            }
            patch_info = None;
            current = None;
            continue;
        }

        if line == "GIT binary patch\n" {
            let Some(cur) = current else {
                return Err(format!(
                    "UnidiffParseError: Unexpected binary patch marker: {line}"
                ));
            };
            set.files[cur].is_binary_file = true;
            patch_info = None;
            current = None;
            continue;
        }

        set.infos[patch_info.unwrap()].push(line.to_string());
    }
    Ok(set)
}

fn need_file_and_info(current: Option<usize>, patch_info: Option<usize>) -> Option<(usize, usize)> {
    match (current, patch_info) {
        (Some(c), Some(p)) => Some((c, p)),
        _ => None,
    }
}

/// Port of `PatchedFile._parse_hunk` (metadata_only=False).
fn parse_hunk(
    set: &mut PatchSet,
    cur: usize,
    header: &regex::Captures<'_>,
    lines: &[&str],
    i: &mut usize,
) -> Result<(), String> {
    let src_len = header.get(2).map_or(1, |m| parse_int_sat(m.as_str()));
    let tgt_len = header.get(4).map_or(1, |m| parse_int_sat(m.as_str()));
    let mut hunk = Hunk {
        source_start: parse_int_sat(&header[1]),
        source_length: src_len,
        target_start: parse_int_sat(&header[3]),
        target_length: tgt_len,
        section_header: header[5].to_string(),
        lines: Vec::new(),
    };

    let mut source_line_no = hunk.source_start;
    let mut target_line_no = hunk.target_start;
    let expected_source_end = source_line_no + hunk.source_length;
    let expected_target_end = target_line_no + hunk.target_length;

    while *i < lines.len() {
        let line = lines[*i];
        *i += 1;

        let (line_type, value): (LineType, String) =
            if let Some(c) = RE_HUNK_BODY_LINE.captures(line) {
                match &c[1][..1] {
                    "+" => (LineType::Added, c[2].to_string()),
                    "-" => (LineType::Removed, c[2].to_string()),
                    " " => (LineType::Context, c[2].to_string()),
                    "\\" => (LineType::NoNewline, c[2].to_string()),
                    _ => unreachable!(),
                }
            } else if let Some(c) = RE_HUNK_EMPTY_BODY_LINE.captures(line) {
                // LINE_TYPE_EMPTY becomes LINE_TYPE_CONTEXT, keeping the raw value
                (LineType::Context, c[2].to_string())
            } else {
                return Err(format!(
                    "UnidiffParseError: Hunk diff line expected: {line}"
                ));
            };

        match line_type {
            LineType::Added => {
                target_line_no += 1;
            }
            LineType::Removed => {
                source_line_no += 1;
            }
            LineType::Context => {
                target_line_no += 1;
                source_line_no += 1;
            }
            LineType::NoNewline | LineType::Empty => {}
        }

        if source_line_no > expected_source_end || target_line_no > expected_target_end {
            return Err("UnidiffParseError: Hunk is longer than expected".to_string());
        }

        hunk.lines.push(Line { value, line_type });

        if source_line_no == expected_source_end && target_line_no == expected_target_end {
            break;
        }
    }

    if source_line_no < expected_source_end || target_line_no < expected_target_end {
        return Err("UnidiffParseError: Hunk is shorter than expected".to_string());
    }

    set.files[cur].hunks.push(hunk);
    Ok(())
}

// --- helpers from pr_agent ---

/// Port of `pr_agent.mosaico.diff_provider._unquote_git_path`.
fn unquote_git_path(path: &str) -> String {
    let bytes = path.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'"' || bytes[bytes.len() - 1] != b'"' {
        return path.to_string();
    }
    let inner = &path[1..path.len() - 1];
    let ib = inner.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0usize;
    while i < ib.len() {
        if ib[i] == b'\\' {
            let octal =
                i + 4 <= ib.len() && ib[i + 1..i + 4].iter().all(|b| (b'0'..=b'7').contains(b));
            if octal {
                let v = (ib[i + 1] - b'0') as u32 * 64
                    + (ib[i + 2] - b'0') as u32 * 8
                    + (ib[i + 3] - b'0') as u32;
                out.push(v as u8);
                i += 4;
                continue;
            }
            if i + 1 < ib.len() && ib[i + 1] != b'\n' {
                let rep = match ib[i + 1] {
                    b'a' => 0x07,
                    b'b' => 0x08,
                    b't' => b'\t',
                    b'n' => b'\n',
                    b'v' => 0x0B,
                    b'f' => 0x0C,
                    b'r' => b'\r',
                    other => other,
                };
                out.push(rep);
                i += 2;
                continue;
            }
        }
        out.push(ib[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Port of `pr_agent.git_providers.diff_parsing._strip_prefix`.
fn strip_prefix(path: &str) -> Option<String> {
    let p = unquote_git_path(path);
    Some(if p.starts_with("a/") || p.starts_with("b/") {
        p[2..].to_string()
    } else {
        p
    })
}

/// Port of `pr_agent.git_providers.diff_parsing._normalize_metadata_line_endings`.
fn normalize_metadata_line_endings(diff_text: &str) -> String {
    let mut out = String::new();
    let mut hunk_lines_remaining: i64 = 0;
    for line in split_keep_terminators(diff_text) {
        let owned;
        let line = if hunk_lines_remaining == 0 && line.ends_with("\r\n") {
            owned = format!("{}\n", &line[..line.len() - 2]);
            owned.as_str()
        } else {
            line
        };
        out.push_str(line);

        if let Some(h) = HUNK_ONLY_HEADER.captures(line) {
            let source_length = h.get(2).map_or(1, |m| parse_int_sat(m.as_str()));
            let target_length = h.get(4).map_or(1, |m| parse_int_sat(m.as_str()));
            hunk_lines_remaining = source_length + target_length;
        } else if hunk_lines_remaining > 0 && !line.starts_with('\\') {
            hunk_lines_remaining -= if line.starts_with('+') || line.starts_with('-') {
                1
            } else {
                2
            };
        }
    }
    out
}

// --- public ports ---

pub fn to_hunk_only_patch(patch_str: String) -> Result<String, String> {
    // Port of pr_agent.algo.git_patch_processing.to_hunk_only_patch: drop everything before
    // the first hunk header that begins a line.
    let mut from = 0usize;
    while let Some(rel) = patch_str[from..].find("@@") {
        let at = from + rel;
        let starts_line = at == 0
            || patch_str[..at]
                .chars()
                .next_back()
                .is_some_and(is_splitlines_break_char);
        if starts_line {
            return Ok(patch_str[at..].to_string());
        }
        from = at + 2;
    }
    Ok(String::new())
}

pub fn parse_unified_diff(diff_text: String) -> Result<Vec<serde_json::Value>, String> {
    let set = parse_patch_set(&normalize_metadata_line_endings(&diff_text))?;
    let mut files: Vec<Value> = Vec::new();
    for pf in &set.files {
        if pf.is_binary_file {
            continue;
        }
        let (edit_name, edit_value, sort_order) = if pf.is_added_file() {
            ("ADDED", 1, 0)
        } else if pf.is_removed_file() {
            ("DELETED", 2, 1)
        } else if pf.is_rename() {
            ("RENAMED", 4, 3)
        } else {
            ("MODIFIED", 3, 2)
        };
        let edit_type = json!({
            "$object": "EDIT_TYPE",
            "attrs": {
                "_value_": edit_value,
                "_name_": edit_name,
                "__objclass__": {"$object": "EnumType", "attrs": {"$repr": "mappingproxy"}},
                "_sort_order_": sort_order,
            }
        });
        let target = if pf.is_removed_file() {
            &pf.source_file
        } else {
            &pf.target_file
        };
        let filename = strip_prefix(target);
        let old_filename = if pf.is_rename() {
            strip_prefix(&pf.source_file)
        } else {
            None
        };
        files.push(json!({
            "$object": "FilePatchInfo",
            "attrs": {
                "base_file": "",
                "head_file": "",
                "patch": pf.to_patch_string(&set.infos),
                "filename": filename,
                "tokens": -1,
                "edit_type": edit_type,
                "old_filename": old_filename,
                "num_plus_lines": pf.hunks.iter().map(Hunk::added).sum::<i64>(),
                "num_minus_lines": pf.hunks.iter().map(Hunk::removed).sum::<i64>(),
                "language": null,
                "ai_file_summary": null,
                "head_file_is_complete": true,
                "content_fetch_failed": false,
            }
        }));
    }
    Ok(files)
}

/// Port of `split_diff_lines` from `pr_agent/algo/git_patch_processing.py`.
fn split_diff_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if !lines.is_empty() && lines.last() == Some(&"") {
        lines.pop();
    }
    lines
        .into_iter()
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
        .collect()
}

pub fn reconstruct_base_file_checked(
    head_file_str: String,
    patch_str: String,
) -> Result<Option<String>, String> {
    let set = match parse_patch_set(&patch_str) {
        Ok(s) => s,
        Err(_) => return Ok(None),
    };
    if set.files.len() != 1 {
        return Ok(None);
    }

    let head_lines = split_diff_lines(&head_file_str);
    let mut base_lines: Vec<String> = Vec::new();
    let mut head_idx = 0usize;
    let mut base_ends_with_newline: Option<bool> = None;

    for hunk in &set.files[0].hunks {
        let hunk_head_start = if hunk.target_length == 0 {
            hunk.target_start
        } else {
            hunk.target_start - 1
        };
        if hunk_head_start < head_idx as i64 || hunk_head_start > head_lines.len() as i64 {
            return Ok(None);
        }
        let hunk_head_start = hunk_head_start as usize;
        base_lines.extend(head_lines[head_idx..hunk_head_start].iter().cloned());
        head_idx = hunk_head_start;

        let mut previous_added: Option<bool> = None;
        for line in &hunk.lines {
            if line.line_type == LineType::NoNewline {
                if let Some(prev_added) = previous_added {
                    if !prev_added {
                        base_ends_with_newline = Some(false);
                    } else if base_ends_with_newline.is_none() {
                        base_ends_with_newline = Some(true);
                    }
                }
                continue;
            }
            previous_added = Some(line.line_type == LineType::Added);
            let value = line.value.strip_suffix('\n').unwrap_or(&line.value);
            let value = value.strip_suffix('\r').unwrap_or(value);
            match line.line_type {
                LineType::Context => {
                    if head_idx >= head_lines.len() || head_lines[head_idx] != value {
                        return Ok(None);
                    }
                    base_lines.push(head_lines[head_idx].clone());
                    head_idx += 1;
                }
                LineType::Added => {
                    if head_idx >= head_lines.len() || head_lines[head_idx] != value {
                        return Ok(None);
                    }
                    head_idx += 1;
                }
                LineType::Removed => {
                    base_lines.push(value.to_string());
                }
                LineType::NoNewline | LineType::Empty => {}
            }
        }
    }

    base_lines.extend(head_lines[head_idx..].iter().cloned());
    let mut result = base_lines.join("\n");
    if base_ends_with_newline.is_none() {
        base_ends_with_newline = Some(if head_file_str.is_empty() {
            true
        } else {
            head_file_str.ends_with('\n')
        });
    }
    if !base_lines.is_empty() && base_ends_with_newline.unwrap_or(false) {
        result.push('\n');
    }
    Ok(Some(result))
}
