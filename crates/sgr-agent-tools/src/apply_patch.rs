//! ApplyPatchTool — Codex-compatible diff-based file editing via FileBackend.
//!
//! Patch DSL parser adapted from Codex (Apache-2.0 license, Copyright OpenAI).
//! See: https://github.com/openai/codex
//!
//! Requires the `patch` feature flag.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use sgr_agent_core::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent_core::context::AgentContext;
use sgr_agent_core::schema::json_schema_for;

use crate::backend::FileBackend;
use crate::helpers::backend_err;

pub struct ApplyPatchTool<B: FileBackend>(pub Arc<B>);

#[derive(Deserialize, JsonSchema)]
struct ApplyPatchArgs {
    /// The patch in Codex apply_patch DSL format
    patch: String,
}

// ---------------------------------------------------------------------------
// Patch data types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Hunk {
    AddFile {
        path: String,
        contents: String,
    },
    DeleteFile {
        path: String,
    },
    UpdateFile {
        path: String,
        move_path: Option<String>,
        chunks: Vec<Chunk>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    /// The @@ context line (without the @@ prefix).
    pub context: Option<String>,
    pub old_lines: Vec<String>,
    pub new_lines: Vec<String>,
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Parse a Codex apply_patch DSL string into a list of hunks.
pub fn parse_patch(text: &str) -> Result<Vec<Hunk>, String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut hunks = Vec::new();
    let mut i = 0;

    // Skip to "*** Begin Patch"
    while i < lines.len() {
        if lines[i].trim() == "*** Begin Patch" {
            i += 1;
            break;
        }
        i += 1;
    }

    while i < lines.len() {
        let line = lines[i].trim();

        if line == "*** End Patch" {
            break;
        }

        if let Some(path) = line.strip_prefix("*** Add File: ") {
            let path = path.trim().to_string();
            i += 1;
            let mut contents = String::new();
            while i < lines.len() {
                let l = lines[i];
                if l.starts_with("*** ") {
                    break;
                }
                if let Some(rest) = l.strip_prefix('+') {
                    if !contents.is_empty() {
                        contents.push('\n');
                    }
                    contents.push_str(rest);
                }
                i += 1;
            }
            hunks.push(Hunk::AddFile { path, contents });
        } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
            hunks.push(Hunk::DeleteFile {
                path: path.trim().to_string(),
            });
            i += 1;
        } else if let Some(path) = line.strip_prefix("*** Update File: ") {
            let path = path.trim().to_string();
            i += 1;
            let mut move_path: Option<String> = None;
            let mut chunks: Vec<Chunk> = Vec::new();

            // Check for *** Move to:
            if i < lines.len() {
                let next = lines[i].trim();
                if let Some(mp) = next.strip_prefix("*** Move to: ") {
                    move_path = Some(mp.trim().to_string());
                    i += 1;
                }
            }

            // Parse chunks (each starts with @@ or with -/+ lines)
            while i < lines.len() {
                let l = lines[i];
                if l.starts_with("*** ") {
                    break;
                }

                if l.starts_with("@@") {
                    let ctx = l.strip_prefix("@@").map(|s| s.trim().to_string());
                    let context = if ctx.as_deref() == Some("") {
                        None
                    } else {
                        ctx
                    };
                    i += 1;

                    let mut old_lines = Vec::new();
                    let mut new_lines = Vec::new();

                    while i < lines.len() {
                        let cl = lines[i];
                        if cl.starts_with("*** ") || cl.starts_with("@@") {
                            break;
                        }
                        if let Some(rest) = cl.strip_prefix('-') {
                            old_lines.push(rest.to_string());
                        } else if let Some(rest) = cl.strip_prefix('+') {
                            new_lines.push(rest.to_string());
                        } else if let Some(rest) = cl.strip_prefix(' ') {
                            // Context line — appears in both old and new
                            old_lines.push(rest.to_string());
                            new_lines.push(rest.to_string());
                        } else if cl.is_empty() {
                            // Empty line treated as context
                            old_lines.push(String::new());
                            new_lines.push(String::new());
                        } else {
                            // Unrecognized line — treat as context
                            old_lines.push(cl.to_string());
                            new_lines.push(cl.to_string());
                        }
                        i += 1;
                    }

                    chunks.push(Chunk {
                        context,
                        old_lines,
                        new_lines,
                    });
                } else {
                    // Skip unrecognized lines between chunks
                    i += 1;
                }
            }

            hunks.push(Hunk::UpdateFile {
                path,
                move_path,
                chunks,
            });
        } else {
            i += 1;
        }
    }

    if hunks.is_empty() {
        return Err("No hunks found in patch".to_string());
    }

    Ok(hunks)
}

// ---------------------------------------------------------------------------
// Seek sequence — fuzzy line matching with 4 levels
// ---------------------------------------------------------------------------

/// Normalize a string: trim whitespace, normalize unicode (NFKC-like ASCII fold).
fn normalize_line(s: &str) -> String {
    s.chars()
        .map(|c| {
            // Fold common unicode variants to ASCII equivalents
            match c {
                '\u{00A0}' => ' ',               // non-breaking space
                '\u{2018}' | '\u{2019}' => '\'', // smart quotes
                '\u{201C}' | '\u{201D}' => '"',
                '\u{2013}' | '\u{2014}' => '-', // en/em dash
                '\u{2026}' => '.',              // ellipsis (simplify)
                _ => c,
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Find the position of `needle` lines within `haystack` lines,
/// starting search from `start_pos`. Uses 4-level fuzzy matching.
///
/// Returns the index in haystack where the match begins, or None.
fn seek_sequence(haystack: &[String], needle: &[String], start_pos: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(start_pos);
    }
    if haystack.is_empty() || start_pos + needle.len() > haystack.len() {
        return None;
    }

    // Level 0: exact match
    for i in start_pos..=haystack.len() - needle.len() {
        if haystack[i..i + needle.len()]
            .iter()
            .zip(needle.iter())
            .all(|(h, n)| h == n)
        {
            return Some(i);
        }
    }

    // Level 1: trim_end match
    for i in start_pos..=haystack.len() - needle.len() {
        if haystack[i..i + needle.len()]
            .iter()
            .zip(needle.iter())
            .all(|(h, n)| h.trim_end() == n.trim_end())
        {
            return Some(i);
        }
    }

    // Level 2: trim (both ends) match
    for i in start_pos..=haystack.len() - needle.len() {
        if haystack[i..i + needle.len()]
            .iter()
            .zip(needle.iter())
            .all(|(h, n)| h.trim() == n.trim())
        {
            return Some(i);
        }
    }

    // Level 3: unicode-normalized + trimmed match
    for i in start_pos..=haystack.len() - needle.len() {
        if haystack[i..i + needle.len()]
            .iter()
            .zip(needle.iter())
            .all(|(h, n)| normalize_line(h) == normalize_line(n))
        {
            return Some(i);
        }
    }

    None
}

/// Apply chunks to file content. Returns the new file content.
fn apply_chunks(content: &str, chunks: &[Chunk]) -> Result<String, String> {
    let lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
    let mut result_lines: Vec<String> = Vec::new();
    let mut pos: usize = 0;

    for chunk in chunks {
        // Find context line first, if present
        let search_start = if let Some(ref ctx) = chunk.context {
            let ctx_needle = vec![ctx.clone()];
            match seek_sequence(&lines, &ctx_needle, pos) {
                Some(found) => found,
                None => {
                    return Err(format!("Could not find context line: '{}'", ctx));
                }
            }
        } else {
            pos
        };

        // Find the old_lines sequence starting from context position
        if chunk.old_lines.is_empty() {
            // Pure insertion at context point
            // Copy everything up to search_start (inclusive of context line)
            let insert_at = if chunk.context.is_some() {
                search_start + 1
            } else {
                search_start
            };
            // Copy lines from pos to insert_at
            for line in &lines[pos..insert_at] {
                result_lines.push(line.clone());
            }
            // Insert new lines
            for line in &chunk.new_lines {
                result_lines.push(line.clone());
            }
            pos = insert_at;
        } else {
            // Find old_lines in the file
            let match_start =
                seek_sequence(&lines, &chunk.old_lines, search_start).ok_or_else(|| {
                    let preview: String = chunk
                        .old_lines
                        .iter()
                        .take(3)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("\n");
                    format!("Could not find old lines starting with: '{}'", preview)
                })?;

            // Copy everything from current pos to match start
            for line in &lines[pos..match_start] {
                result_lines.push(line.clone());
            }

            // Replace old lines with new lines
            for line in &chunk.new_lines {
                result_lines.push(line.clone());
            }

            pos = match_start + chunk.old_lines.len();
        }
    }

    // Copy remaining lines
    for line in &lines[pos..] {
        result_lines.push(line.clone());
    }

    Ok(result_lines.join("\n"))
}

// ---------------------------------------------------------------------------
// Tool implementation
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl<B: FileBackend> Tool for ApplyPatchTool<B> {
    fn name(&self) -> &str {
        "apply_patch"
    }
    fn description(&self) -> &str {
        "Apply a diff patch to files. Uses Codex apply_patch DSL format:\n\
         *** Begin Patch\n\
         *** Update File: path\n\
         @@ context_line\n\
         -old line\n\
         +new line\n\
          context line\n\
         *** End Patch\n\n\
         Supports: Add File, Delete File, Update File (with Move to).\n\
         Fuzzy matching: handles trailing whitespace and unicode variants."
    }
    fn parameters_schema(&self) -> Value {
        json_schema_for::<ApplyPatchArgs>()
    }
    async fn execute(&self, args: Value, _ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let a: ApplyPatchArgs = parse_args(&args)?;

        let hunks = parse_patch(&a.patch).map_err(|e| ToolError::Execution(e))?;

        let mut added: Vec<String> = Vec::new();
        let mut modified: Vec<String> = Vec::new();
        let mut deleted: Vec<String> = Vec::new();
        let mut moved: Vec<(String, String)> = Vec::new();

        for hunk in &hunks {
            match hunk {
                Hunk::AddFile { path, contents } => {
                    self.0
                        .write(path, contents, 0, 0)
                        .await
                        .map_err(backend_err)?;
                    added.push(path.clone());
                }
                Hunk::DeleteFile { path } => {
                    self.0.delete(path).await.map_err(backend_err)?;
                    deleted.push(path.clone());
                }
                Hunk::UpdateFile {
                    path,
                    move_path,
                    chunks,
                } => {
                    let content = self.0.read(path, false, 0, 0).await.map_err(backend_err)?;

                    let new_content =
                        apply_chunks(&content, chunks).map_err(|e| ToolError::Execution(e))?;

                    let target = move_path.as_deref().unwrap_or(path);
                    self.0
                        .write(target, &new_content, 0, 0)
                        .await
                        .map_err(backend_err)?;

                    if let Some(mp) = move_path {
                        self.0.delete(path).await.map_err(backend_err)?;
                        moved.push((path.clone(), mp.clone()));
                    } else {
                        modified.push(path.clone());
                    }
                }
            }
        }

        let mut summary = Vec::new();
        if !modified.is_empty() {
            summary.push(format!("Modified: {}", modified.join(", ")));
        }
        if !added.is_empty() {
            summary.push(format!("Added: {}", added.join(", ")));
        }
        if !deleted.is_empty() {
            summary.push(format!("Deleted: {}", deleted.join(", ")));
        }
        for (from, to) in &moved {
            summary.push(format!("Moved: {} -> {}", from, to));
        }

        Ok(ToolOutput::text(summary.join("\n")))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_add_file() {
        let patch = "\
*** Begin Patch
*** Add File: hello.txt
+Hello
+World
*** End Patch";

        let hunks = parse_patch(patch).unwrap();
        assert_eq!(hunks.len(), 1);
        match &hunks[0] {
            Hunk::AddFile { path, contents } => {
                assert_eq!(path, "hello.txt");
                assert_eq!(contents, "Hello\nWorld");
            }
            _ => panic!("Expected AddFile"),
        }
    }

    #[test]
    fn parse_delete_file() {
        let patch = "\
*** Begin Patch
*** Delete File: old.txt
*** End Patch";

        let hunks = parse_patch(patch).unwrap();
        assert_eq!(hunks.len(), 1);
        match &hunks[0] {
            Hunk::DeleteFile { path } => assert_eq!(path, "old.txt"),
            _ => panic!("Expected DeleteFile"),
        }
    }

    #[test]
    fn parse_update_file() {
        let patch = "\
*** Begin Patch
*** Update File: src/main.rs
@@ fn main() {
-    println!(\"old\");
+    println!(\"new\");
*** End Patch";

        let hunks = parse_patch(patch).unwrap();
        assert_eq!(hunks.len(), 1);
        match &hunks[0] {
            Hunk::UpdateFile {
                path,
                move_path,
                chunks,
            } => {
                assert_eq!(path, "src/main.rs");
                assert!(move_path.is_none());
                assert_eq!(chunks.len(), 1);
                assert_eq!(chunks[0].context.as_deref(), Some("fn main() {"));
                assert_eq!(chunks[0].old_lines, vec!["    println!(\"old\");"]);
                assert_eq!(chunks[0].new_lines, vec!["    println!(\"new\");"]);
            }
            _ => panic!("Expected UpdateFile"),
        }
    }

    #[test]
    fn parse_move_file() {
        let patch = "\
*** Begin Patch
*** Update File: old/path.rs
*** Move to: new/path.rs
@@ use std;
-old line
+new line
*** End Patch";

        let hunks = parse_patch(patch).unwrap();
        assert_eq!(hunks.len(), 1);
        match &hunks[0] {
            Hunk::UpdateFile {
                path,
                move_path,
                chunks,
            } => {
                assert_eq!(path, "old/path.rs");
                assert_eq!(move_path.as_deref(), Some("new/path.rs"));
                assert_eq!(chunks.len(), 1);
            }
            _ => panic!("Expected UpdateFile with move"),
        }
    }

    #[test]
    fn parse_multi_hunk() {
        let patch = "\
*** Begin Patch
*** Add File: new.txt
+content
*** Delete File: old.txt
*** Update File: src/lib.rs
@@ fn foo() {
-    old();
+    new();
*** End Patch";

        let hunks = parse_patch(patch).unwrap();
        assert_eq!(hunks.len(), 3);
        assert!(matches!(&hunks[0], Hunk::AddFile { .. }));
        assert!(matches!(&hunks[1], Hunk::DeleteFile { .. }));
        assert!(matches!(&hunks[2], Hunk::UpdateFile { .. }));
    }

    #[test]
    fn parse_empty_patch_fails() {
        let patch = "*** Begin Patch\n*** End Patch";
        assert!(parse_patch(patch).is_err());
    }

    #[test]
    fn seek_exact() {
        let haystack: Vec<String> = vec!["a", "b", "c", "d"]
            .into_iter()
            .map(String::from)
            .collect();
        let needle: Vec<String> = vec!["b", "c"].into_iter().map(String::from).collect();
        assert_eq!(seek_sequence(&haystack, &needle, 0), Some(1));
    }

    #[test]
    fn seek_trim_end() {
        let haystack: Vec<String> = vec!["a  ", "b  "].into_iter().map(String::from).collect();
        let needle: Vec<String> = vec!["a", "b"].into_iter().map(String::from).collect();
        assert_eq!(seek_sequence(&haystack, &needle, 0), Some(0));
    }

    #[test]
    fn seek_trim_both() {
        let haystack: Vec<String> = vec!["  a  ", "  b  "]
            .into_iter()
            .map(String::from)
            .collect();
        let needle: Vec<String> = vec!["a", "b"].into_iter().map(String::from).collect();
        assert_eq!(seek_sequence(&haystack, &needle, 0), Some(0));
    }

    #[test]
    fn seek_unicode_normalize() {
        // Smart quotes vs straight quotes
        let haystack: Vec<String> = vec!["\u{201C}hello\u{201D}"]
            .into_iter()
            .map(String::from)
            .collect();
        let needle: Vec<String> = vec!["\"hello\""].into_iter().map(String::from).collect();
        assert_eq!(seek_sequence(&haystack, &needle, 0), Some(0));
    }

    #[test]
    fn seek_not_found() {
        let haystack: Vec<String> = vec!["a", "b"].into_iter().map(String::from).collect();
        let needle: Vec<String> = vec!["x"].into_iter().map(String::from).collect();
        assert_eq!(seek_sequence(&haystack, &needle, 0), None);
    }

    #[test]
    fn apply_simple_replacement() {
        let content = "line1\nline2\nline3";
        let chunks = vec![Chunk {
            context: None,
            old_lines: vec!["line2".to_string()],
            new_lines: vec!["replaced".to_string()],
        }];
        let result = apply_chunks(content, &chunks).unwrap();
        assert_eq!(result, "line1\nreplaced\nline3");
    }

    #[test]
    fn apply_with_context() {
        let content = "fn main() {\n    println!(\"old\");\n}";
        let chunks = vec![Chunk {
            context: Some("fn main() {".to_string()),
            old_lines: vec!["    println!(\"old\");".to_string()],
            new_lines: vec!["    println!(\"new\");".to_string()],
        }];
        let result = apply_chunks(content, &chunks).unwrap();
        assert_eq!(result, "fn main() {\n    println!(\"new\");\n}");
    }

    #[test]
    fn apply_multi_line_replacement() {
        let content = "a\nb\nc\nd\ne";
        let chunks = vec![Chunk {
            context: None,
            old_lines: vec!["b".to_string(), "c".to_string(), "d".to_string()],
            new_lines: vec!["x".to_string(), "y".to_string()],
        }];
        let result = apply_chunks(content, &chunks).unwrap();
        assert_eq!(result, "a\nx\ny\ne");
    }

    #[test]
    fn apply_deletion_chunk() {
        let content = "a\nb\nc";
        let chunks = vec![Chunk {
            context: None,
            old_lines: vec!["b".to_string()],
            new_lines: vec![],
        }];
        let result = apply_chunks(content, &chunks).unwrap();
        assert_eq!(result, "a\nc");
    }

    #[test]
    fn apply_insertion_with_context() {
        let content = "a\nb\nc";
        let chunks = vec![Chunk {
            context: Some("b".to_string()),
            old_lines: vec![],
            new_lines: vec!["inserted".to_string()],
        }];
        let result = apply_chunks(content, &chunks).unwrap();
        assert_eq!(result, "a\nb\ninserted\nc");
    }

    #[test]
    fn parse_context_lines_in_chunk() {
        let patch = "\
*** Begin Patch
*** Update File: test.rs
@@ fn example() {
 fn example() {
-    old();
+    new();
 }
*** End Patch";

        let hunks = parse_patch(patch).unwrap();
        match &hunks[0] {
            Hunk::UpdateFile { chunks, .. } => {
                assert_eq!(chunks.len(), 1);
                let c = &chunks[0];
                // Context lines appear in both old and new
                assert_eq!(c.old_lines, vec!["fn example() {", "    old();", "}"]);
                assert_eq!(c.new_lines, vec!["fn example() {", "    new();", "}"]);
            }
            _ => panic!("Expected UpdateFile"),
        }
    }
}
