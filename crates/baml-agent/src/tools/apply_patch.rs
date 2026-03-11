//! Port of the Codex `apply_patch` logic — parse a patch in the Codex patch format
//! and apply it to files on disk.
//!
//! This module provides:
//! - `parse_patch()` — parse a patch string into a list of `Hunk`s
//! - `apply_patch_to_files()` — async: parse + apply a patch to the filesystem
//! - `APPLY_PATCH_INSTRUCTIONS` — tool instructions for the LLM
//!
//! The patch format grammar:
//! ```text
//! Patch := Begin { FileOp } End
//! Begin := "*** Begin Patch" NEWLINE
//! End   := "*** End Patch" NEWLINE
//! FileOp := AddFile | DeleteFile | UpdateFile
//! AddFile    := "*** Add File: " path NEWLINE { "+" line NEWLINE }
//! DeleteFile := "*** Delete File: " path NEWLINE
//! UpdateFile := "*** Update File: " path NEWLINE [ MoveTo ] { Hunk }
//! MoveTo     := "*** Move to: " newPath NEWLINE
//! Hunk       := "@@" [ header ] NEWLINE { HunkLine } [ "*** End of File" NEWLINE ]
//! HunkLine   := (" " | "-" | "+") text NEWLINE
//! ```

use std::path::{Path, PathBuf};

use thiserror::Error;

// ---------------------------------------------------------------------------
// Tool instructions for LLM
// ---------------------------------------------------------------------------

pub const APPLY_PATCH_INSTRUCTIONS: &str = r#"## `apply_patch`

Use the `apply_patch` tool to edit files.
Your patch language is a stripped-down, file-oriented diff format designed to be easy to parse and safe to apply.

*** Begin Patch
[ one or more file sections ]
*** End Patch

Each operation starts with one of three headers:

*** Add File: <path> - create a new file. Every following line is a + line (the initial contents).
*** Delete File: <path> - remove an existing file. Nothing follows.
*** Update File: <path> - patch an existing file in place (optionally with a rename).

May be immediately followed by *** Move to: <new path> if you want to rename the file.
Then one or more "hunks", each introduced by @@ (optionally followed by a hunk header).
Within a hunk each line starts with:

For context lines and changes:
- By default, show 3 lines of code immediately above and 3 lines immediately below each change.
- If 3 lines of context is insufficient to uniquely identify the snippet, use the @@ operator to indicate the class or function:
@@ class BaseClass
[3 lines of pre-context]
- [old_code]
+ [new_code]
[3 lines of post-context]

- If a code block is repeated so many times that even a single @@ and 3 lines of context cannot uniquely identify it, use multiple @@ statements:
@@ class BaseClass
@@   def method():
[3 lines of pre-context]
- [old_code]
+ [new_code]
[3 lines of post-context]

Example:

*** Begin Patch
*** Add File: hello.txt
+Hello world
*** Update File: src/app.py
*** Move to: src/main.py
@@ def greet():
-print("Hi")
+print("Hello, world!")
*** Delete File: obsolete.txt
*** End Patch

Rules:
- You must include a header with your intended action (Add/Delete/Update)
- You must prefix new lines with `+` even when creating a new file
- File references should be relative to the working directory
"#;

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Error, Clone)]
pub enum ParseError {
    #[error("invalid patch: {0}")]
    InvalidPatch(String),
    #[error("invalid hunk at line {line_number}: {message}")]
    InvalidHunk { message: String, line_number: usize },
}

#[derive(Debug, Error)]
pub enum ApplyPatchError {
    #[error(transparent)]
    Parse(#[from] ParseError),
    #[error("failed to find match: {0}")]
    MatchFailed(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

// ---------------------------------------------------------------------------
// Patch AST
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Clone)]
pub enum Hunk {
    AddFile {
        path: PathBuf,
        contents: String,
    },
    DeleteFile {
        path: PathBuf,
    },
    UpdateFile {
        path: PathBuf,
        move_path: Option<PathBuf>,
        chunks: Vec<UpdateFileChunk>,
    },
}

impl Hunk {
    /// Resolve the hunk's path relative to `cwd`.
    pub fn resolve_path(&self, cwd: &Path) -> PathBuf {
        match self {
            Hunk::AddFile { path, .. } => cwd.join(path),
            Hunk::DeleteFile { path } => cwd.join(path),
            Hunk::UpdateFile { path, .. } => cwd.join(path),
        }
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct UpdateFileChunk {
    /// Optional context line (e.g. class/function signature) to locate position.
    pub change_context: Option<String>,
    /// Lines to find and remove.
    pub old_lines: Vec<String>,
    /// Lines to insert in place of `old_lines`.
    pub new_lines: Vec<String>,
    /// If true, `old_lines` must match at end of file.
    pub is_end_of_file: bool,
}

/// Result of applying a patch — which files were affected.
#[derive(Debug, Default)]
pub struct PatchResult {
    pub added: Vec<PathBuf>,
    pub modified: Vec<PathBuf>,
    pub deleted: Vec<PathBuf>,
}

impl std::fmt::Display for PatchResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for p in &self.added {
            writeln!(f, "A {}", p.display())?;
        }
        for p in &self.modified {
            writeln!(f, "M {}", p.display())?;
        }
        for p in &self.deleted {
            writeln!(f, "D {}", p.display())?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Marker constants
// ---------------------------------------------------------------------------

const BEGIN_PATCH: &str = "*** Begin Patch";
const END_PATCH: &str = "*** End Patch";
const ADD_FILE: &str = "*** Add File: ";
const DELETE_FILE: &str = "*** Delete File: ";
const UPDATE_FILE: &str = "*** Update File: ";
const MOVE_TO: &str = "*** Move to: ";
const EOF_MARKER: &str = "*** End of File";
const CONTEXT_MARKER: &str = "@@ ";
const EMPTY_CONTEXT: &str = "@@";

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Parse a patch string into a list of hunks.
pub fn parse_patch(patch: &str) -> Result<Vec<Hunk>, ParseError> {
    let input = patch.trim();
    let lines: Vec<&str> = input.lines().collect();

    // Try parsing as-is first (includes heredoc detection)
    match check_boundaries(&lines) {
        Ok(lines) => return parse_patch_inner(lines),
        Err(_) => {}
    }

    // Postel's law: auto-fix missing Begin/End markers if content looks like a patch
    let has_begin = lines.first().map(|l| l.trim()) == Some(BEGIN_PATCH);
    let has_end = lines.last().map(|l| l.trim()) == Some(END_PATCH);
    let looks_like_patch = input.contains("*** Add File:")
        || input.contains("*** Update File:")
        || input.contains("*** Delete File:")
        || input.contains("--- ");

    if !looks_like_patch {
        // Not a patch at all — return original error
        check_boundaries(&lines)?;
        unreachable!();
    }

    let fixed = match (has_begin, has_end) {
        (false, false) => format!("{}\n{}\n{}", BEGIN_PATCH, input, END_PATCH),
        (true, false) => format!("{}\n{}", input, END_PATCH),
        (false, true) => format!("{}\n{}", BEGIN_PATCH, input),
        (true, true) => unreachable!(), // would have succeeded above
    };
    let fixed_lines: Vec<&str> = fixed.lines().collect();
    let fixed_lines = check_boundaries(&fixed_lines)?;
    parse_patch_inner(fixed_lines)
}

fn parse_patch_inner(lines: &[&str]) -> Result<Vec<Hunk>, ParseError> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let last = lines.len().saturating_sub(1);
    let mut remaining = &lines[1..last];
    let mut line_no = 2;

    while !remaining.is_empty() {
        let (hunk, consumed) = parse_one_hunk(remaining, line_no)?;
        hunks.push(hunk);
        line_no += consumed;
        remaining = &remaining[consumed..];
    }

    Ok(hunks)
}

/// Check Begin/End markers, handling optional heredoc wrappers (lenient mode).
fn check_boundaries<'a>(lines: &'a [&'a str]) -> Result<&'a [&'a str], ParseError> {
    if check_markers(lines).is_ok() {
        return Ok(lines);
    }

    // Lenient: try stripping heredoc wrapper
    if let [first, .., last] = lines {
        if (*first == "<<EOF" || *first == "<<'EOF'" || *first == "<<\"EOF\"")
            && last.ends_with("EOF")
            && lines.len() >= 4
        {
            let inner = &lines[1..lines.len() - 1];
            check_markers(inner)?;
            return Ok(inner);
        }
    }

    check_markers(lines)?;
    Ok(lines)
}

fn check_markers(lines: &[&str]) -> Result<(), ParseError> {
    let first = lines.first().map(|l| l.trim());
    let last = lines.last().map(|l| l.trim());

    match (first, last) {
        (Some(f), Some(l)) if f == BEGIN_PATCH && l == END_PATCH => Ok(()),
        (Some(f), _) if f != BEGIN_PATCH => Err(ParseError::InvalidPatch(
            "The first line of the patch must be '*** Begin Patch'".into(),
        )),
        _ => Err(ParseError::InvalidPatch(
            "The last line of the patch must be '*** End Patch'".into(),
        )),
    }
}

fn parse_one_hunk(lines: &[&str], line_no: usize) -> Result<(Hunk, usize), ParseError> {
    let first = lines[0].trim();

    // --- Unified diff tolerance (Postel's law) ---
    // Models often generate standard unified diff instead of our format:
    //   --- a/path/to/file
    //   +++ b/path/to/file
    //   @@ -N,N +N,N @@
    //   context/-/+ lines
    // Convert on-the-fly to our UpdateFile format.
    if first.starts_with("--- ") {
        return parse_unified_diff_hunk(lines, line_no);
    }

    if let Some(path) = first.strip_prefix(ADD_FILE) {
        let mut contents = String::new();
        let mut consumed = 1;
        for line in &lines[1..] {
            if line.starts_with("@@") {
                // Skip unified diff hunk headers (@@ -0,0 +1,N @@) — tolerate hybrid format
                consumed += 1;
                continue;
            }
            if let Some(rest) = line.strip_prefix('+') {
                contents.push_str(rest);
                contents.push('\n');
                consumed += 1;
            } else {
                break;
            }
        }
        return Ok((
            Hunk::AddFile {
                path: PathBuf::from(path),
                contents,
            },
            consumed,
        ));
    }

    if let Some(path) = first.strip_prefix(DELETE_FILE) {
        return Ok((
            Hunk::DeleteFile {
                path: PathBuf::from(path),
            },
            1,
        ));
    }

    if let Some(path) = first.strip_prefix(UPDATE_FILE) {
        let mut remaining = &lines[1..];
        let mut consumed = 1;

        // Optional move
        let move_path = remaining.first().and_then(|l| l.strip_prefix(MOVE_TO));
        if move_path.is_some() {
            remaining = &remaining[1..];
            consumed += 1;
        }

        let mut chunks = Vec::new();
        while !remaining.is_empty() {
            // Skip blank lines between chunks
            if remaining[0].trim().is_empty() {
                consumed += 1;
                remaining = &remaining[1..];
                continue;
            }
            // Stop at next file-level marker
            if remaining[0].starts_with("***") {
                break;
            }

            let (chunk, n) =
                parse_update_file_chunk(remaining, line_no + consumed, chunks.is_empty())?;
            chunks.push(chunk);
            consumed += n;
            remaining = &remaining[n..];
        }

        if chunks.is_empty() {
            return Err(ParseError::InvalidHunk {
                message: format!("Update file hunk for path '{path}' is empty"),
                line_number: line_no,
            });
        }

        return Ok((
            Hunk::UpdateFile {
                path: PathBuf::from(path),
                move_path: move_path.map(PathBuf::from),
                chunks,
            },
            consumed,
        ));
    }

    Err(ParseError::InvalidHunk {
        message: format!(
            "'{first}' is not a valid hunk header. \
             Valid headers: '*** Add File: {{path}}', '*** Delete File: {{path}}', '*** Update File: {{path}}'"
        ),
        line_number: line_no,
    })
}

/// Parse a unified diff hunk (`--- a/path` / `+++ b/path` / `@@ ... @@` / lines).
/// Converts it into our `Hunk::UpdateFile` or `Hunk::AddFile` format.
fn parse_unified_diff_hunk(lines: &[&str], line_no: usize) -> Result<(Hunk, usize), ParseError> {
    let first = lines[0].trim();

    // Extract path from "--- a/path" or "--- path" (or "--- /dev/null" for new files)
    let old_path = first
        .strip_prefix("--- a/")
        .or_else(|| first.strip_prefix("--- "))
        .unwrap_or("");
    let is_new_file = old_path == "/dev/null" || old_path.is_empty();

    // Expect "+++ b/path" or "+++ path" on next line
    if lines.len() < 2 {
        return Err(ParseError::InvalidHunk {
            message: "Unified diff: expected +++ line after ---".into(),
            line_number: line_no,
        });
    }
    let second = lines[1].trim();
    let new_path = second
        .strip_prefix("+++ b/")
        .or_else(|| second.strip_prefix("+++ "))
        .unwrap_or("");
    let is_delete = new_path == "/dev/null" || new_path.is_empty();

    let path = if is_new_file { new_path } else { old_path };
    if path.is_empty() || path == "/dev/null" {
        return Err(ParseError::InvalidHunk {
            message: "Unified diff: could not determine file path from --- / +++ lines".into(),
            line_number: line_no,
        });
    }

    let mut consumed = 2;
    let mut remaining = &lines[2..];

    // Handle delete
    if is_delete {
        return Ok((
            Hunk::DeleteFile {
                path: PathBuf::from(path),
            },
            consumed,
        ));
    }

    // Parse @@ hunks
    let mut chunks = Vec::new();
    let mut add_contents = String::new(); // for new files, collect all + lines

    while !remaining.is_empty() {
        let line = remaining[0].trim();

        // Stop at next file (--- or ***) or end
        if line.starts_with("--- ") || line.starts_with("***") {
            break;
        }

        // Parse @@ header
        if line.starts_with("@@") {
            let ctx = if let Some(rest) = line.strip_prefix("@@ ") {
                strip_unified_diff_header(rest)
            } else {
                String::new()
            };

            consumed += 1;
            remaining = &remaining[1..];

            // Collect diff lines for this hunk
            let mut old_lines = Vec::new();
            let mut new_lines = Vec::new();

            while !remaining.is_empty() {
                let dl = remaining[0];
                match dl.chars().next() {
                    Some(' ') => {
                        old_lines.push(dl[1..].to_string());
                        new_lines.push(dl[1..].to_string());
                    }
                    Some('-') => {
                        old_lines.push(dl[1..].to_string());
                    }
                    Some('+') => {
                        new_lines.push(dl[1..].to_string());
                        if is_new_file {
                            add_contents.push_str(&dl[1..]);
                            add_contents.push('\n');
                        }
                    }
                    Some('\\') => {
                        // "\ No newline at end of file" — skip
                    }
                    None => {
                        // Empty line = context
                        old_lines.push(String::new());
                        new_lines.push(String::new());
                    }
                    _ => break, // next @@ or file marker
                }
                consumed += 1;
                remaining = &remaining[1..];
            }

            if !is_new_file && (!old_lines.is_empty() || !new_lines.is_empty()) {
                chunks.push(UpdateFileChunk {
                    change_context: if ctx.is_empty() { None } else { Some(ctx) },
                    old_lines,
                    new_lines,
                    is_end_of_file: false,
                });
            }
            continue;
        }

        // Non-@@ line at top level — might be a diff line without @@ header
        // (some models skip the @@ header for simple diffs)
        match line.chars().next() {
            Some(' ') | Some('-') | Some('+') => {
                // Collect as a single hunk without context marker
                let mut old_lines = Vec::new();
                let mut new_lines = Vec::new();

                while !remaining.is_empty() {
                    let dl = remaining[0];
                    match dl.chars().next() {
                        Some(' ') => {
                            old_lines.push(dl[1..].to_string());
                            new_lines.push(dl[1..].to_string());
                        }
                        Some('-') => old_lines.push(dl[1..].to_string()),
                        Some('+') => {
                            new_lines.push(dl[1..].to_string());
                            if is_new_file {
                                add_contents.push_str(&dl[1..]);
                                add_contents.push('\n');
                            }
                        }
                        Some('\\') => {}
                        None => {
                            old_lines.push(String::new());
                            new_lines.push(String::new());
                        }
                        _ => break,
                    }
                    consumed += 1;
                    remaining = &remaining[1..];
                }

                if !is_new_file && (!old_lines.is_empty() || !new_lines.is_empty()) {
                    chunks.push(UpdateFileChunk {
                        change_context: None,
                        old_lines,
                        new_lines,
                        is_end_of_file: false,
                    });
                }
            }
            _ => break,
        }
    }

    // New file: return AddFile
    if is_new_file {
        return Ok((
            Hunk::AddFile {
                path: PathBuf::from(path),
                contents: add_contents,
            },
            consumed,
        ));
    }

    if chunks.is_empty() {
        return Err(ParseError::InvalidHunk {
            message: format!("Unified diff for '{path}' has no changes"),
            line_number: line_no,
        });
    }

    Ok((
        Hunk::UpdateFile {
            path: PathBuf::from(path),
            move_path: None,
            chunks,
        },
        consumed,
    ))
}

/// Strip unified diff line-number header from a `@@` line.
///
/// Input is the part AFTER the leading "@@ " prefix. Examples:
///   "-2,6 +2,7 @@"          → ""           (no trailing context)
///   "-2,6 +2,7 @@ fn foo"   → "fn foo"     (has trailing context)
///   "def bar():"             → "def bar():" (not unified diff, pass through)
fn strip_unified_diff_header(ctx: &str) -> String {
    // Unified diff pattern: starts with `-` followed by digits
    if ctx.starts_with('-') && ctx.contains("@@") {
        // Find the closing "@@" and take everything after it (trimmed)
        if let Some(pos) = ctx.find("@@") {
            let after = ctx[pos + 2..].trim();
            return after.to_string();
        }
    }
    ctx.to_string()
}

fn parse_update_file_chunk(
    lines: &[&str],
    line_no: usize,
    allow_missing_context: bool,
) -> Result<(UpdateFileChunk, usize), ParseError> {
    if lines.is_empty() {
        return Err(ParseError::InvalidHunk {
            message: "Update hunk does not contain any lines".into(),
            line_number: line_no,
        });
    }

    let (context, start) = if lines[0] == EMPTY_CONTEXT {
        (None, 1)
    } else if let Some(ctx) = lines[0].strip_prefix(CONTEXT_MARKER) {
        // Handle unified diff format: "@@ -2,6 +2,7 @@" or "@@ -2,6 +2,7 @@ fn foo"
        // Strip the line-number portion, keep only trailing context (if any).
        let ctx = strip_unified_diff_header(ctx);
        if ctx.is_empty() {
            (None, 1)
        } else {
            (Some(ctx), 1)
        }
    } else {
        if !allow_missing_context {
            return Err(ParseError::InvalidHunk {
                message: format!(
                    "Expected update hunk to start with a @@ context marker, got: '{}'",
                    lines[0]
                ),
                line_number: line_no,
            });
        }
        (None, 0)
    };

    if start >= lines.len() {
        return Err(ParseError::InvalidHunk {
            message: "Update hunk does not contain any lines".into(),
            line_number: line_no + 1,
        });
    }

    let mut chunk = UpdateFileChunk {
        change_context: context,
        old_lines: Vec::new(),
        new_lines: Vec::new(),
        is_end_of_file: false,
    };

    let mut parsed = 0;
    for line in &lines[start..] {
        if *line == EOF_MARKER {
            if parsed == 0 {
                return Err(ParseError::InvalidHunk {
                    message: "Update hunk does not contain any lines".into(),
                    line_number: line_no + 1,
                });
            }
            chunk.is_end_of_file = true;
            parsed += 1;
            break;
        }

        match line.chars().next() {
            None => {
                // Empty line = context
                chunk.old_lines.push(String::new());
                chunk.new_lines.push(String::new());
            }
            Some(' ') => {
                chunk.old_lines.push(line[1..].to_string());
                chunk.new_lines.push(line[1..].to_string());
            }
            Some('+') => {
                chunk.new_lines.push(line[1..].to_string());
            }
            Some('-') => {
                chunk.old_lines.push(line[1..].to_string());
            }
            _ => {
                if parsed == 0 {
                    return Err(ParseError::InvalidHunk {
                        message: format!(
                            "Unexpected line in update hunk: '{line}'. \
                             Lines must start with ' ', '+', or '-'"
                        ),
                        line_number: line_no + 1,
                    });
                }
                // Start of next hunk
                break;
            }
        }
        parsed += 1;
    }

    Ok((chunk, parsed + start))
}

// ---------------------------------------------------------------------------
// 4-tier fuzzy matching: seek_sequence
// ---------------------------------------------------------------------------

/// Find `pattern` lines within `lines` starting at or after `start`.
///
/// Matching tiers (decreasing strictness):
/// 1. Exact byte-for-byte match
/// 2. Match after trimming trailing whitespace
/// 3. Match after trimming both sides
/// 4. Match after Unicode normalization (typographic → ASCII)
///
/// When `eof` is true, prefer matching at the end of the file first.
fn seek_sequence(lines: &[String], pattern: &[String], start: usize, eof: bool) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start);
    }
    if pattern.len() > lines.len() {
        return None;
    }

    let search_start = if eof && lines.len() >= pattern.len() {
        lines.len() - pattern.len()
    } else {
        start
    };

    // Try forward from search_start first, then wrap-around from 0
    if let Some(idx) = seek_sequence_range(lines, pattern, search_start) {
        return Some(idx);
    }
    // Wrap-around: search from beginning up to search_start
    if search_start > 0 {
        if let Some(idx) = seek_sequence_range(lines, pattern, 0) {
            return Some(idx);
        }
    }
    None
}

/// Inner search across 5 tiers of progressively looser matching.
fn seek_sequence_range(lines: &[String], pattern: &[String], start: usize) -> Option<usize> {
    let end = lines.len().saturating_sub(pattern.len());

    // Tiers ordered from strictest to most lenient.
    // Each tier gets a full pass before falling through to the next.
    let tiers: &[fn(&str, &str) -> bool] = &[
        |a, b| a == b,                                 // 1: exact
        |a, b| a.trim_end() == b.trim_end(),           // 2: trailing ws
        |a, b| a.trim() == b.trim(),                   // 3: both sides
        |a, b| normalise_line(a) == normalise_line(b), // 4: unicode
        |a, b| collapse_ws(a) == collapse_ws(b),       // 5: ws collapse
    ];

    for cmp in tiers {
        for i in start..=end {
            if pattern
                .iter()
                .enumerate()
                .all(|(j, p)| cmp(&lines[i + j], p))
            {
                return Some(i);
            }
        }
    }

    None
}

/// Normalize Unicode typography to ASCII.
fn normalise_line(s: &str) -> String {
    s.trim()
        .chars()
        .map(|c| match c {
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => '-',
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
            | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200A}' | '\u{202F}' | '\u{205F}'
            | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

/// Collapse all runs of whitespace to a single space, then trim.
fn collapse_ws(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_ws = true; // start true to skip leading whitespace
    for c in s.chars() {
        if c.is_whitespace() {
            if !in_ws {
                result.push(' ');
                in_ws = true;
            }
        } else {
            result.push(c);
            in_ws = false;
        }
    }
    // Trim trailing space
    if result.ends_with(' ') {
        result.pop();
    }
    result
}

/// Substring context match: find a line containing `ctx` as a substring.
/// Used when `@@ normalize` should match `  normalize(data: any) {`.
fn seek_context_substring(lines: &[String], ctx: &str, start: usize) -> Option<usize> {
    let ctx_trimmed = ctx.trim();
    if ctx_trimmed.is_empty() {
        return None;
    }
    // Forward search
    for i in start..lines.len() {
        if lines[i].contains(ctx_trimmed) {
            return Some(i);
        }
    }
    // Wrap-around
    for i in 0..start {
        if lines[i].contains(ctx_trimmed) {
            return Some(i);
        }
    }
    None
}

/// Find the closest matching single line for error reporting.
fn find_closest_line(lines: &[String], target: &str, start: usize) -> String {
    let target_lower = target.trim().to_lowercase();
    let mut best = ("(no lines in file)", usize::MAX);
    for (i, line) in lines.iter().enumerate() {
        let line_lower = line.trim().to_lowercase();
        let dist = if line_lower.contains(&target_lower) || target_lower.contains(&line_lower) {
            0
        } else {
            levenshtein_bounded(&target_lower, &line_lower, 50)
        };
        if dist < best.1 {
            best = (line, dist);
        }
        // Prefer lines near start
        if dist == best.1 && i >= start && i < start + 20 {
            best = (line, dist);
        }
    }
    format!("line: '{}'", best.0.trim())
}

/// Find the closest matching block for error reporting.
fn find_closest_block(lines: &[String], pattern: &[String], start: usize) -> String {
    if lines.is_empty() || pattern.is_empty() {
        return "(empty)".into();
    }
    // Find the line most similar to pattern[0]
    let target = pattern[0].trim().to_lowercase();
    let mut best_idx = start.min(lines.len().saturating_sub(1));
    let mut best_dist = usize::MAX;
    for (i, line) in lines.iter().enumerate() {
        let dist = levenshtein_bounded(&target, &line.trim().to_lowercase(), 80);
        if dist < best_dist {
            best_dist = dist;
            best_idx = i;
        }
    }
    // Show context around best match
    let show_start = best_idx;
    let show_end = (best_idx + pattern.len()).min(lines.len());
    lines[show_start..show_end]
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{:4}: {}", show_start + i + 1, l))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Bounded Levenshtein distance (stops early if distance exceeds bound).
fn levenshtein_bounded(a: &str, b: &str, bound: usize) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();
    if m.abs_diff(n) > bound {
        return bound + 1;
    }
    let mut prev = vec![0usize; n + 1];
    let mut curr = vec![0usize; n + 1];
    for j in 0..=n {
        prev[j] = j;
    }
    for i in 1..=m {
        curr[0] = i;
        let mut row_min = i;
        for j in 1..=n {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
            row_min = row_min.min(curr[j]);
        }
        if row_min > bound {
            return bound + 1;
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

// ---------------------------------------------------------------------------
// Core: compute_replacements + apply_replacements
// ---------------------------------------------------------------------------

/// Compute a list of `(start_index, old_len, new_lines)` replacements.
fn compute_replacements(
    original_lines: &[String],
    path: &Path,
    chunks: &[UpdateFileChunk],
) -> Result<Vec<(usize, usize, Vec<String>)>, ApplyPatchError> {
    let mut replacements: Vec<(usize, usize, Vec<String>)> = Vec::new();
    let mut line_index: usize = 0;

    for chunk in chunks {
        // Seek to change_context if present (the @@ header text)
        if let Some(ctx) = &chunk.change_context {
            if let Some(idx) =
                seek_sequence(original_lines, std::slice::from_ref(ctx), line_index, false)
            {
                line_index = idx + 1;
            } else if let Some(idx) = seek_context_substring(original_lines, ctx, line_index) {
                // Fallback: substring match — model sent bare name like "normalize"
                // but file has "  normalize(data: any) {"
                line_index = idx + 1;
            } else {
                let closest = find_closest_line(original_lines, ctx, line_index);
                return Err(ApplyPatchError::MatchFailed(format!(
                    "Failed to find context '{}' in {}\n\
                     Closest match: {}",
                    ctx,
                    path.display(),
                    closest,
                )));
            }
        }

        if chunk.old_lines.is_empty() {
            // Pure addition — insert at end of file
            let idx = if original_lines.last().is_some_and(String::is_empty) {
                original_lines.len() - 1
            } else {
                original_lines.len()
            };
            replacements.push((idx, 0, chunk.new_lines.clone()));
            continue;
        }

        // Try to locate old_lines in the file
        let mut pattern: &[String] = &chunk.old_lines;
        let mut found = seek_sequence(original_lines, pattern, line_index, chunk.is_end_of_file);
        let mut new_slice: &[String] = &chunk.new_lines;

        // Retry without trailing empty line (represents final newline)
        if found.is_none() && pattern.last().is_some_and(String::is_empty) {
            pattern = &pattern[..pattern.len() - 1];
            if new_slice.last().is_some_and(String::is_empty) {
                new_slice = &new_slice[..new_slice.len() - 1];
            }
            found = seek_sequence(original_lines, pattern, line_index, chunk.is_end_of_file);
        }

        if let Some(start) = found {
            replacements.push((start, pattern.len(), new_slice.to_vec()));
            line_index = start + pattern.len();
        } else {
            // Find the closest matching region to help the model self-correct
            let closest = find_closest_block(original_lines, pattern, line_index);
            return Err(ApplyPatchError::MatchFailed(format!(
                "Failed to find expected lines in {}:\n{}\n\
                 \n--- Closest match in file (use read_file to see actual content): ---\n{}",
                path.display(),
                chunk.old_lines.join("\n"),
                closest,
            )));
        }
    }

    replacements.sort_by_key(|(idx, _, _)| *idx);
    Ok(replacements)
}

/// Apply replacements in reverse order so indices remain valid.
fn apply_replacements(
    mut lines: Vec<String>,
    replacements: &[(usize, usize, Vec<String>)],
) -> Vec<String> {
    for (start, old_len, new_segment) in replacements.iter().rev() {
        let start = *start;
        let old_len = *old_len;

        for _ in 0..old_len {
            if start < lines.len() {
                lines.remove(start);
            }
        }
        for (offset, new_line) in new_segment.iter().enumerate() {
            lines.insert(start + offset, new_line.clone());
        }
    }
    lines
}

/// Derive new file contents by applying chunks to the file at `path`.
/// Returns `(original_contents, new_contents)`.
fn derive_new_contents(
    original_contents: &str,
    path: &Path,
    chunks: &[UpdateFileChunk],
) -> Result<String, ApplyPatchError> {
    let mut original_lines: Vec<String> = original_contents.split('\n').map(String::from).collect();

    // Drop trailing empty element from final newline
    if original_lines.last().is_some_and(String::is_empty) {
        original_lines.pop();
    }

    let replacements = compute_replacements(&original_lines, path, chunks)?;
    let mut new_lines = apply_replacements(original_lines, &replacements);

    // Ensure trailing newline
    if !new_lines.last().is_some_and(String::is_empty) {
        new_lines.push(String::new());
    }

    Ok(new_lines.join("\n"))
}

// ---------------------------------------------------------------------------
// Public async API
// ---------------------------------------------------------------------------

/// Apply a patch string to files on disk (async I/O).
///
/// `cwd` is the working directory for resolving relative paths in the patch.
/// Returns a `PatchResult` listing affected files.
pub async fn apply_patch_to_files(patch: &str, cwd: &Path) -> Result<PatchResult, ApplyPatchError> {
    let hunks = parse_patch(patch)?;

    if hunks.is_empty() {
        return Err(ApplyPatchError::Other("No file operations in patch".into()));
    }

    let mut result = PatchResult::default();

    for hunk in &hunks {
        match hunk {
            Hunk::AddFile { path, contents } => {
                let full = cwd.join(path);
                if let Some(parent) = full.parent() {
                    if !parent.as_os_str().is_empty() {
                        tokio::fs::create_dir_all(parent).await?;
                    }
                }
                tokio::fs::write(&full, contents).await?;
                result.added.push(full);
            }
            Hunk::DeleteFile { path } => {
                let full = cwd.join(path);
                tokio::fs::remove_file(&full).await?;
                result.deleted.push(full);
            }
            Hunk::UpdateFile {
                path,
                move_path,
                chunks,
            } => {
                let full = cwd.join(path);
                let original = tokio::fs::read_to_string(&full).await.map_err(|e| {
                    ApplyPatchError::Other(format!("Failed to read {}: {}", full.display(), e))
                })?;

                let new_contents = derive_new_contents(&original, &full, chunks)?;

                if let Some(dest) = move_path {
                    let dest_full = cwd.join(dest);
                    if let Some(parent) = dest_full.parent() {
                        if !parent.as_os_str().is_empty() {
                            tokio::fs::create_dir_all(parent).await?;
                        }
                    }
                    tokio::fs::write(&dest_full, &new_contents).await?;
                    tokio::fs::remove_file(&full).await?;
                    result.modified.push(dest_full);
                } else {
                    tokio::fs::write(&full, &new_contents).await?;
                    result.modified.push(full);
                }
            }
        }
    }

    Ok(result)
}

/// Synchronous version of `apply_patch_to_files` for use in tests and non-async contexts.
pub fn apply_patch_to_files_sync(patch: &str, cwd: &Path) -> Result<PatchResult, ApplyPatchError> {
    let hunks = parse_patch(patch)?;

    if hunks.is_empty() {
        return Err(ApplyPatchError::Other("No file operations in patch".into()));
    }

    let mut result = PatchResult::default();

    for hunk in &hunks {
        match hunk {
            Hunk::AddFile { path, contents } => {
                let full = cwd.join(path);
                if let Some(parent) = full.parent() {
                    if !parent.as_os_str().is_empty() {
                        std::fs::create_dir_all(parent)?;
                    }
                }
                std::fs::write(&full, contents)?;
                result.added.push(full);
            }
            Hunk::DeleteFile { path } => {
                let full = cwd.join(path);
                std::fs::remove_file(&full)?;
                result.deleted.push(full);
            }
            Hunk::UpdateFile {
                path,
                move_path,
                chunks,
            } => {
                let full = cwd.join(path);
                let original = std::fs::read_to_string(&full).map_err(|e| {
                    ApplyPatchError::Other(format!("Failed to read {}: {}", full.display(), e))
                })?;

                let new_contents = derive_new_contents(&original, &full, chunks)?;

                if let Some(dest) = move_path {
                    let dest_full = cwd.join(dest);
                    if let Some(parent) = dest_full.parent() {
                        if !parent.as_os_str().is_empty() {
                            std::fs::create_dir_all(parent)?;
                        }
                    }
                    std::fs::write(&dest_full, &new_contents)?;
                    std::fs::remove_file(&full)?;
                    result.modified.push(dest_full);
                } else {
                    std::fs::write(&full, &new_contents)?;
                    result.modified.push(full);
                }
            }
        }
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn wrap(body: &str) -> String {
        format!("*** Begin Patch\n{body}\n*** End Patch")
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    // -- Parser tests --

    #[test]
    fn test_parse_empty_patch() {
        let hunks = parse_patch("*** Begin Patch\n*** End Patch").unwrap();
        assert!(hunks.is_empty());
    }

    #[test]
    fn test_parse_bad_first_line() {
        let err = parse_patch("bad\n*** End Patch").unwrap_err();
        assert!(matches!(err, ParseError::InvalidPatch(_)));
    }

    #[test]
    fn test_parse_bad_last_line() {
        let err = parse_patch("*** Begin Patch\nbad").unwrap_err();
        assert!(matches!(err, ParseError::InvalidPatch(_)));
    }

    #[test]
    fn test_parse_add_file() {
        let hunks = parse_patch(&wrap("*** Add File: foo.txt\n+hello\n+world")).unwrap();
        assert_eq!(
            hunks,
            vec![Hunk::AddFile {
                path: PathBuf::from("foo.txt"),
                contents: "hello\nworld\n".into(),
            }]
        );
    }

    #[test]
    fn test_parse_delete_file() {
        let hunks = parse_patch(&wrap("*** Delete File: old.txt")).unwrap();
        assert_eq!(
            hunks,
            vec![Hunk::DeleteFile {
                path: PathBuf::from("old.txt"),
            }]
        );
    }

    #[test]
    fn test_parse_update_file_with_move() {
        let patch = wrap(
            "*** Update File: src.py\n\
             *** Move to: dst.py\n\
             @@ def f():\n\
             -    pass\n\
             +    return 1",
        );
        let hunks = parse_patch(&patch).unwrap();
        assert_eq!(
            hunks,
            vec![Hunk::UpdateFile {
                path: PathBuf::from("src.py"),
                move_path: Some(PathBuf::from("dst.py")),
                chunks: vec![UpdateFileChunk {
                    change_context: Some("def f():".into()),
                    old_lines: vec!["    pass".into()],
                    new_lines: vec!["    return 1".into()],
                    is_end_of_file: false,
                }],
            }]
        );
    }

    #[test]
    fn test_parse_update_no_explicit_context() {
        let patch = "*** Begin Patch\n\
                     *** Update File: file.py\n \
                     import foo\n\
                     +bar\n\
                     *** End Patch";
        let hunks = parse_patch(patch).unwrap();
        assert_eq!(hunks.len(), 1);
        if let Hunk::UpdateFile { chunks, .. } = &hunks[0] {
            assert_eq!(chunks[0].old_lines, vec!["import foo".to_string()]);
            assert_eq!(
                chunks[0].new_lines,
                vec!["import foo".to_string(), "bar".to_string()]
            );
        } else {
            panic!("Expected UpdateFile");
        }
    }

    #[test]
    fn test_parse_multiple_hunks() {
        let patch = wrap(
            "*** Add File: a.txt\n+x\n\
             *** Delete File: b.txt\n\
             *** Update File: c.txt\n@@\n-old\n+new",
        );
        let hunks = parse_patch(&patch).unwrap();
        assert_eq!(hunks.len(), 3);
        assert!(matches!(&hunks[0], Hunk::AddFile { .. }));
        assert!(matches!(&hunks[1], Hunk::DeleteFile { .. }));
        assert!(matches!(&hunks[2], Hunk::UpdateFile { .. }));
    }

    #[test]
    fn test_parse_empty_update_fails() {
        let patch = wrap("*** Update File: test.py");
        let err = parse_patch(&patch).unwrap_err();
        assert!(matches!(err, ParseError::InvalidHunk { .. }));
    }

    #[test]
    fn test_parse_heredoc_lenient() {
        let inner = "*** Begin Patch\n*** Add File: f.txt\n+ok\n*** End Patch";
        let with_heredoc = format!("<<'EOF'\n{inner}\nEOF\n");
        let hunks = parse_patch(&with_heredoc).unwrap();
        assert_eq!(hunks.len(), 1);
    }

    #[test]
    fn test_parse_eof_marker() {
        let patch = wrap("*** Update File: f.txt\n@@\n+line\n*** End of File");
        let hunks = parse_patch(&patch).unwrap();
        if let Hunk::UpdateFile { chunks, .. } = &hunks[0] {
            assert!(chunks[0].is_end_of_file);
        } else {
            panic!("Expected UpdateFile");
        }
    }

    // -- seek_sequence tests --

    #[test]
    fn test_seek_exact() {
        let lines = s(&["foo", "bar", "baz"]);
        let pattern = s(&["bar", "baz"]);
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(1));
    }

    #[test]
    fn test_seek_trim_end() {
        let lines = s(&["foo   ", "bar\t\t"]);
        let pattern = s(&["foo", "bar"]);
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
    }

    #[test]
    fn test_seek_trim_both() {
        let lines = s(&["    foo   ", "   bar\t"]);
        let pattern = s(&["foo", "bar"]);
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
    }

    #[test]
    fn test_seek_unicode_normalize() {
        // EN DASH in file, ASCII dash in pattern
        let lines = s(&["hello \u{2013} world"]);
        let pattern = s(&["hello - world"]);
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
    }

    #[test]
    fn test_seek_pattern_too_long() {
        let lines = s(&["one"]);
        let pattern = s(&["a", "b", "c"]);
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), None);
    }

    #[test]
    fn test_seek_empty_pattern() {
        let lines = s(&["foo"]);
        assert_eq!(seek_sequence(&lines, &[], 0, false), Some(0));
    }

    #[test]
    fn test_seek_eof_prefers_end() {
        let lines = s(&["dup", "middle", "dup"]);
        let pattern = s(&["dup"]);
        // Without eof, finds first
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
        // With eof, finds last
        assert_eq!(seek_sequence(&lines, &pattern, 0, true), Some(2));
    }

    // -- Integration tests: apply_patch_to_files_sync --

    #[test]
    fn test_add_file() {
        let dir = tempdir().unwrap();
        let patch = wrap("*** Add File: hello.txt\n+Hello\n+World");
        let result = apply_patch_to_files_sync(&patch, dir.path()).unwrap();
        assert_eq!(result.added.len(), 1);
        let contents = fs::read_to_string(dir.path().join("hello.txt")).unwrap();
        assert_eq!(contents, "Hello\nWorld\n");
    }

    #[test]
    fn test_add_file_nested_dir() {
        let dir = tempdir().unwrap();
        let patch = wrap("*** Add File: a/b/c.txt\n+nested");
        let result = apply_patch_to_files_sync(&patch, dir.path()).unwrap();
        assert_eq!(result.added.len(), 1);
        let contents = fs::read_to_string(dir.path().join("a/b/c.txt")).unwrap();
        assert_eq!(contents, "nested\n");
    }

    #[test]
    fn test_delete_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("del.txt");
        fs::write(&path, "x").unwrap();
        let patch = wrap("*** Delete File: del.txt");
        let result = apply_patch_to_files_sync(&patch, dir.path()).unwrap();
        assert_eq!(result.deleted.len(), 1);
        assert!(!path.exists());
    }

    #[test]
    fn test_update_file() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("f.txt"), "foo\nbar\n").unwrap();
        let patch = wrap("*** Update File: f.txt\n@@\n foo\n-bar\n+baz");
        let result = apply_patch_to_files_sync(&patch, dir.path()).unwrap();
        assert_eq!(result.modified.len(), 1);
        let contents = fs::read_to_string(dir.path().join("f.txt")).unwrap();
        assert_eq!(contents, "foo\nbaz\n");
    }

    #[test]
    fn test_update_file_move() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src.txt");
        fs::write(&src, "line\n").unwrap();
        let patch = wrap(
            "*** Update File: src.txt\n\
             *** Move to: dst.txt\n\
             @@\n-line\n+line2",
        );
        let result = apply_patch_to_files_sync(&patch, dir.path()).unwrap();
        assert_eq!(result.modified.len(), 1);
        assert!(!src.exists());
        let contents = fs::read_to_string(dir.path().join("dst.txt")).unwrap();
        assert_eq!(contents, "line2\n");
    }

    #[test]
    fn test_multiple_chunks_single_file() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("m.txt"), "foo\nbar\nbaz\nqux\n").unwrap();
        let patch = wrap(
            "*** Update File: m.txt\n\
             @@\n foo\n-bar\n+BAR\n\
             @@\n baz\n-qux\n+QUX",
        );
        let result = apply_patch_to_files_sync(&patch, dir.path()).unwrap();
        assert_eq!(result.modified.len(), 1);
        let contents = fs::read_to_string(dir.path().join("m.txt")).unwrap();
        assert_eq!(contents, "foo\nBAR\nbaz\nQUX\n");
    }

    #[test]
    fn test_interleaved_changes() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("i.txt"), "a\nb\nc\nd\ne\nf\n").unwrap();
        let patch = wrap(
            "*** Update File: i.txt\n\
             @@\n a\n-b\n+B\n\
             @@\n c\n d\n-e\n+E\n\
             @@\n f\n+g\n*** End of File",
        );
        apply_patch_to_files_sync(&patch, dir.path()).unwrap();
        let contents = fs::read_to_string(dir.path().join("i.txt")).unwrap();
        assert_eq!(contents, "a\nB\nc\nd\nE\nf\ng\n");
    }

    #[test]
    fn test_pure_addition_then_removal() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("p.txt"), "line1\nline2\nline3\n").unwrap();
        let patch = wrap(
            "*** Update File: p.txt\n\
             @@\n+after-context\n+second-line\n\
             @@\n line1\n-line2\n-line3\n+line2-replacement",
        );
        apply_patch_to_files_sync(&patch, dir.path()).unwrap();
        let contents = fs::read_to_string(dir.path().join("p.txt")).unwrap();
        assert_eq!(
            contents,
            "line1\nline2-replacement\nafter-context\nsecond-line\n"
        );
    }

    #[test]
    fn test_unicode_dash_matching() {
        let dir = tempdir().unwrap();
        // File has EN DASH and NON-BREAKING HYPHEN
        let original = "import asyncio  # local \u{2013} avoids top\u{2011}level dep\n";
        fs::write(dir.path().join("u.py"), original).unwrap();

        // Patch uses plain ASCII
        let patch = wrap(
            "*** Update File: u.py\n\
             @@\n\
             -import asyncio  # local - avoids top-level dep\n\
             +import asyncio  # HELLO",
        );
        apply_patch_to_files_sync(&patch, dir.path()).unwrap();
        let contents = fs::read_to_string(dir.path().join("u.py")).unwrap();
        assert_eq!(contents, "import asyncio  # HELLO\n");
    }

    #[test]
    fn test_context_search_with_at_header() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("ctx.py"),
            "class Foo:\n    def bar(self):\n        pass\n    def baz(self):\n        pass\n",
        )
        .unwrap();
        let patch = wrap(
            "*** Update File: ctx.py\n\
             @@ def baz(self):\n\
             -        pass\n\
             +        return 42",
        );
        apply_patch_to_files_sync(&patch, dir.path()).unwrap();
        let contents = fs::read_to_string(dir.path().join("ctx.py")).unwrap();
        assert!(contents.contains("return 42"));
        // First `pass` (in bar) should remain
        assert!(contents.contains("        pass"));
    }

    #[test]
    fn test_full_pipeline_multiple_ops() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("exist.txt"), "keep\nremove\n").unwrap();
        fs::write(dir.path().join("gone.txt"), "bye").unwrap();

        let patch = wrap(
            "*** Add File: new.txt\n+fresh\n\
             *** Update File: exist.txt\n@@\n keep\n-remove\n+replaced\n\
             *** Delete File: gone.txt",
        );
        let result = apply_patch_to_files_sync(&patch, dir.path()).unwrap();
        assert_eq!(result.added.len(), 1);
        assert_eq!(result.modified.len(), 1);
        assert_eq!(result.deleted.len(), 1);

        assert_eq!(
            fs::read_to_string(dir.path().join("new.txt")).unwrap(),
            "fresh\n"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("exist.txt")).unwrap(),
            "keep\nreplaced\n"
        );
        assert!(!dir.path().join("gone.txt").exists());
    }

    #[test]
    fn test_patch_result_display() {
        let r = PatchResult {
            added: vec![PathBuf::from("a.txt")],
            modified: vec![PathBuf::from("m.txt")],
            deleted: vec![PathBuf::from("d.txt")],
        };
        let s = format!("{r}");
        assert!(s.contains("A a.txt"));
        assert!(s.contains("M m.txt"));
        assert!(s.contains("D d.txt"));
    }

    // -- Unified diff header tolerance --

    #[test]
    fn test_strip_unified_diff_header() {
        assert_eq!(strip_unified_diff_header("-2,6 +2,7 @@"), "");
        assert_eq!(strip_unified_diff_header("-2,6 +2,7 @@ fn foo"), "fn foo");
        assert_eq!(
            strip_unified_diff_header("-1,4 +1,5 @@ class Foo:"),
            "class Foo:"
        );
        // Not unified diff — pass through
        assert_eq!(strip_unified_diff_header("def bar():"), "def bar():");
        assert_eq!(strip_unified_diff_header("class Baz"), "class Baz");
    }

    #[test]
    fn test_parse_unified_diff_context_marker() {
        // Gemini sends "@@ -2,6 +2,7 @@" instead of "@@"
        let patch = wrap("*** Update File: f.txt\n@@ -1,3 +1,3 @@\n foo\n-bar\n+baz");
        let hunks = parse_patch(&patch).unwrap();
        if let Hunk::UpdateFile { chunks, .. } = &hunks[0] {
            assert!(chunks[0].change_context.is_none());
            assert_eq!(chunks[0].old_lines, vec!["foo", "bar"]);
            assert_eq!(chunks[0].new_lines, vec!["foo", "baz"]);
        } else {
            panic!("Expected UpdateFile");
        }
    }

    #[test]
    fn test_parse_unified_diff_with_trailing_context() {
        // "@@ -5,3 +5,4 @@ def greet():" — should use "def greet():" as context
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("g.py"),
            "import os\n\ndef greet():\n    print(\"hi\")\n    return\n",
        )
        .unwrap();
        let patch = wrap(
            "*** Update File: g.py\n@@ -3,3 +3,4 @@ def greet():\n-    print(\"hi\")\n+    print(\"hello\")\n+    print(\"world\")",
        );
        apply_patch_to_files_sync(&patch, dir.path()).unwrap();
        let contents = fs::read_to_string(dir.path().join("g.py")).unwrap();
        assert!(contents.contains("hello"));
        assert!(contents.contains("world"));
        assert!(!contents.contains("hi"));
    }

    // -- Unified diff tolerance (Postel's law) --

    #[test]
    fn test_unified_diff_update_file() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("app.py"),
            "import os\n\ndef main():\n    print(\"hello\")\n    return 0\n",
        )
        .unwrap();

        let patch = wrap("--- a/app.py\n+++ b/app.py\n@@ -3,3 +3,3 @@\n def main():\n-    print(\"hello\")\n+    print(\"world\")\n     return 0");
        apply_patch_to_files_sync(&patch, dir.path()).unwrap();
        let contents = fs::read_to_string(dir.path().join("app.py")).unwrap();
        assert!(contents.contains("world"));
        assert!(!contents.contains("hello"));
    }

    #[test]
    fn test_unified_diff_new_file() {
        let dir = tempdir().unwrap();
        let patch = wrap("--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1,2 @@\n+hello\n+world");
        let result = apply_patch_to_files_sync(&patch, dir.path()).unwrap();
        assert_eq!(result.added.len(), 1);
        let contents = fs::read_to_string(dir.path().join("new.txt")).unwrap();
        assert_eq!(contents, "hello\nworld\n");
    }

    #[test]
    fn test_unified_diff_delete_file() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("old.txt"), "bye").unwrap();
        let patch = wrap("--- a/old.txt\n+++ /dev/null");
        let result = apply_patch_to_files_sync(&patch, dir.path()).unwrap();
        assert_eq!(result.deleted.len(), 1);
        assert!(!dir.path().join("old.txt").exists());
    }

    #[test]
    fn test_unified_diff_multiple_hunks() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("multi.py"), "a\nb\nc\nd\ne\nf\n").unwrap();

        let patch = wrap("--- a/multi.py\n+++ b/multi.py\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n@@ -4,3 +4,3 @@\n d\n-e\n+E\n f");
        apply_patch_to_files_sync(&patch, dir.path()).unwrap();
        let contents = fs::read_to_string(dir.path().join("multi.py")).unwrap();
        assert_eq!(contents, "a\nB\nc\nd\nE\nf\n");
    }

    #[test]
    fn test_unified_diff_with_context_function() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("ctx.py"),
            "import os\n\ndef greet():\n    pass\n\ndef other():\n    pass\n",
        )
        .unwrap();

        let patch = wrap(
            "--- a/ctx.py\n+++ b/ctx.py\n@@ -3,2 +3,2 @@ def greet():\n-    pass\n+    return 42",
        );
        apply_patch_to_files_sync(&patch, dir.path()).unwrap();
        let contents = fs::read_to_string(dir.path().join("ctx.py")).unwrap();
        assert!(contents.contains("return 42"));
        assert!(contents.contains("    pass")); // other() pass remains
    }

    #[test]
    fn test_unified_diff_mixed_with_codex_format() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "old\n").unwrap();

        let patch =
            wrap("--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n*** Add File: b.txt\n+fresh");
        let result = apply_patch_to_files_sync(&patch, dir.path()).unwrap();
        assert_eq!(result.modified.len(), 1);
        assert_eq!(result.added.len(), 1);
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "new\n"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "fresh\n"
        );
    }

    // -- Async test --

    #[tokio::test]
    async fn test_apply_patch_async() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("async.txt"), "old\n").unwrap();
        let patch = wrap("*** Update File: async.txt\n@@\n-old\n+new");
        let result = apply_patch_to_files(&patch, dir.path()).await.unwrap();
        assert_eq!(result.modified.len(), 1);
        let contents = fs::read_to_string(dir.path().join("async.txt")).unwrap();
        assert_eq!(contents, "new\n");
    }

    #[test]
    fn test_auto_fix_missing_end_patch() {
        let patch = "*** Begin Patch\n*** Add File: test.txt\n+hello";
        let hunks = parse_patch(patch).unwrap();
        assert_eq!(hunks.len(), 1);
    }

    #[test]
    fn test_auto_fix_missing_begin_patch() {
        let patch = "*** Add File: test.txt\n+hello\n*** End Patch";
        let hunks = parse_patch(patch).unwrap();
        assert_eq!(hunks.len(), 1);
    }

    #[test]
    fn test_auto_fix_missing_both_markers() {
        let patch = "*** Add File: test.txt\n+hello";
        let hunks = parse_patch(patch).unwrap();
        assert_eq!(hunks.len(), 1);
    }

    #[test]
    fn test_context_substring_match() {
        // Model sends "@@ normalize" but file has "  normalize(data: any) {"
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("mod.ts"),
            "export const m = {\n  id: 'test',\n  normalize(data: any) {\n    return data + 1;\n  }\n};\n",
        )
        .unwrap();
        let patch = wrap(
            "*** Update File: mod.ts\n@@ normalize\n-    return data + 1;\n+    return data * 2;",
        );
        apply_patch_to_files_sync(&patch, dir.path()).unwrap();
        let contents = fs::read_to_string(dir.path().join("mod.ts")).unwrap();
        assert!(contents.contains("data * 2"));
        assert!(!contents.contains("data + 1"));
    }

    #[test]
    fn test_wrap_around_search() {
        // Context line is above the start position — should wrap around
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("wrap.txt"), "aaa\nbbb\nccc\nddd\n").unwrap();
        // Two chunks: first matches "ccc" (line 3), second matches "aaa" (line 1, before)
        let patch = wrap("*** Update File: wrap.txt\n@@\n-ccc\n+CCC\n@@\n-aaa\n+AAA");
        apply_patch_to_files_sync(&patch, dir.path()).unwrap();
        let contents = fs::read_to_string(dir.path().join("wrap.txt")).unwrap();
        assert!(contents.contains("AAA"));
        assert!(contents.contains("CCC"));
    }

    #[test]
    fn test_whitespace_collapse_match() {
        // Model sends "  if (x  &&  y)" but file has "    if (x && y)"
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("ws.ts"),
            "function f() {\n    if (x && y) {\n        return true;\n    }\n}\n",
        )
        .unwrap();
        let patch = wrap("*** Update File: ws.ts\n@@\n-  if (x  &&  y) {\n+  if (x || y) {");
        apply_patch_to_files_sync(&patch, dir.path()).unwrap();
        let contents = fs::read_to_string(dir.path().join("ws.ts")).unwrap();
        assert!(contents.contains("x || y"));
    }

    #[test]
    fn test_error_shows_closest_match() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("err.ts"),
            "const x = 1;\nconst y = 2;\nconst z = 3;\n",
        )
        .unwrap();
        let patch = wrap("*** Update File: err.ts\n@@\n-const y = 999;\n+const y = 0;");
        let err = apply_patch_to_files_sync(&patch, dir.path()).unwrap_err();
        let msg = err.to_string();
        // Error should contain the closest match hint
        assert!(
            msg.contains("Closest match"),
            "Error should show closest match: {}",
            msg
        );
        assert!(
            msg.contains("const y = 2"),
            "Error should show actual line: {}",
            msg
        );
    }

    #[test]
    fn test_add_file_with_unified_diff_header() {
        // Model sends hybrid: *** Add File + @@ -0,0 +1,N @@ header
        let patch = wrap("*** Add File: new.ts\n@@ -0,0 +1,3 @@\n+line 1\n+line 2\n+line 3");
        let dir = tempdir().unwrap();
        apply_patch_to_files_sync(&patch, dir.path()).unwrap();
        let contents = fs::read_to_string(dir.path().join("new.ts")).unwrap();
        assert!(contents.contains("line 1"));
        assert!(contents.contains("line 3"));
    }

    #[test]
    fn test_add_file_with_bare_at_header() {
        // Model sends *** Add File + bare @@
        let patch = wrap("*** Add File: bare.ts\n@@\n+hello\n+world");
        let dir = tempdir().unwrap();
        apply_patch_to_files_sync(&patch, dir.path()).unwrap();
        let contents = fs::read_to_string(dir.path().join("bare.ts")).unwrap();
        assert!(contents.contains("hello"));
        assert!(contents.contains("world"));
    }
}
