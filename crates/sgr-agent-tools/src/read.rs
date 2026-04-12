//! ReadTool — read file contents with trust metadata.
//!
//! Core read logic without workflow guards or content scanning.
//! For PAC1-specific behavior (workflow tracking, guard_content), wrap this tool.
//!
//! Supports two modes:
//! - "slice" (default): read a range of lines (start_line/end_line)
//! - "indentation": expand bidirectionally from an anchor line by indent level

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use sgr_agent_core::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent_core::context::AgentContext;
use sgr_agent_core::schema::json_schema_for;

use crate::backend::FileBackend;
use crate::helpers::backend_err;
use crate::trust::wrap_with_meta;

pub struct ReadTool<B: FileBackend>(pub Arc<B>);

#[derive(Deserialize, JsonSchema)]
struct ReadArgs {
    /// File path
    path: String,
    /// Show line numbers (like cat -n)
    #[serde(default)]
    number: bool,
    /// Start line (1-indexed, like sed)
    #[serde(default)]
    start_line: i32,
    #[serde(default)]
    end_line: i32,
    /// Reading mode: "slice" (default) or "indentation"
    #[serde(default)]
    mode: Option<String>,
    /// Anchor line for indentation mode (1-indexed)
    #[serde(default)]
    anchor_line: Option<usize>,
    /// Max indent levels to expand (0 = unlimited, default: 0)
    #[serde(default)]
    max_levels: Option<usize>,
}

/// Spaces per tab for indent calculation.
const TAB_WIDTH: usize = 4;

/// Compute the effective indent (in spaces) for each line.
/// Blank lines inherit the indent of the previous non-blank line.
fn compute_effective_indents(lines: &[&str]) -> Vec<usize> {
    let mut indents: Vec<usize> = Vec::with_capacity(lines.len());
    let mut prev_indent: usize = 0;

    for line in lines {
        if line.trim().is_empty() {
            // Blank line inherits previous indent
            indents.push(prev_indent);
        } else {
            let indent = line
                .chars()
                .take_while(|c| c.is_whitespace())
                .map(|c| if c == '\t' { TAB_WIDTH } else { 1 })
                .sum();
            indents.push(indent);
            prev_indent = indent;
        }
    }

    indents
}

/// Read a block around an anchor line based on indentation.
///
/// Expands bidirectionally from the anchor, including lines whose effective
/// indent is >= min_indent (anchor_indent - max_levels * TAB_WIDTH).
/// Stops at the first line at min_indent boundary in each direction (siblings).
fn read_indentation_block(content: &str, anchor: usize, max_levels: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() || anchor == 0 || anchor > lines.len() {
        return content.to_string();
    }

    let indents = compute_effective_indents(&lines);
    let anchor_idx = anchor - 1; // convert 1-indexed to 0-indexed
    let anchor_indent = indents[anchor_idx];

    let min_indent = if max_levels == 0 {
        0
    } else {
        anchor_indent.saturating_sub(max_levels * TAB_WIDTH)
    };

    // Expand upward from anchor: include the containing parent at min_indent,
    // but stop at the previous sibling (a second line at min_indent).
    let mut start = anchor_idx;
    for i in (0..anchor_idx).rev() {
        if indents[i] < min_indent {
            break;
        }
        if indents[i] == min_indent {
            // Found the containing parent — include it and stop
            start = i;
            break;
        }
        start = i;
    }

    // Expand downward from anchor: include children (indent > min_indent),
    // but stop before the next sibling at min_indent.
    let mut end = anchor_idx;
    for i in (anchor_idx + 1)..lines.len() {
        if indents[i] < min_indent {
            break;
        }
        if indents[i] == min_indent {
            // Next sibling — do not include it
            break;
        }
        end = i;
    }

    // Trim leading/trailing blank lines within range
    while start <= end && lines[start].trim().is_empty() {
        start += 1;
    }
    while end > start && lines[end].trim().is_empty() {
        end -= 1;
    }

    // Format with line numbers: L{n}: {content}
    let mut result = String::new();
    for i in start..=end {
        result.push_str(&format!("L{}: {}\n", i + 1, lines[i]));
    }

    result
}

#[async_trait::async_trait]
impl<B: FileBackend> Tool for ReadTool<B> {
    fn name(&self) -> &str {
        "read"
    }
    fn description(&self) -> &str {
        "Read file contents. Use number=true to see line numbers (like cat -n). \
         Use start_line/end_line to read a specific range (like sed -n '5,10p'). \
         For large files: first read with number=true, then read specific ranges. \
         Indentation mode: mode=\"indentation\", anchor_line=N expands around line N \
         by indent level (max_levels=0 for full scope)."
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn parameters_schema(&self) -> Value {
        json_schema_for::<ReadArgs>()
    }
    async fn execute(&self, args: Value, ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        self.execute_readonly(args, ctx).await
    }
    async fn execute_readonly(
        &self,
        args: Value,
        _ctx: &AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let a: ReadArgs = parse_args(&args)?;

        if a.mode.as_deref() == Some("indentation") {
            let anchor = a.anchor_line.unwrap_or(1);
            let max_levels = a.max_levels.unwrap_or(0);

            // Read full file (no line numbers, no range)
            let content = self
                .0
                .read(&a.path, false, 0, 0)
                .await
                .map_err(backend_err)?;

            let block = read_indentation_block(&content, anchor, max_levels);
            return Ok(ToolOutput::text(wrap_with_meta(&a.path, &block)));
        }

        // Default slice mode — delegate to backend
        let result = self
            .0
            .read(&a.path, a.number, a.start_line, a.end_line)
            .await
            .map_err(backend_err)?;
        Ok(ToolOutput::text(wrap_with_meta(&a.path, &result)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_indents_basic() {
        let lines = vec!["def foo():", "    x = 1", "    y = 2", ""];
        let indents = compute_effective_indents(&lines);
        assert_eq!(indents, vec![0, 4, 4, 4]); // blank inherits
    }

    #[test]
    fn effective_indents_tabs() {
        let lines = vec!["\tdef foo():", "\t\tx = 1"];
        let indents = compute_effective_indents(&lines);
        assert_eq!(indents, vec![4, 8]);
    }

    #[test]
    fn indentation_block_simple() {
        let content = "class Foo:\n    def bar(self):\n        x = 1\n        y = 2\n    def baz(self):\n        z = 3\n";
        // Anchor on "x = 1" (line 3), max_levels=1 should expand within bar()
        let result = read_indentation_block(content, 3, 1);
        assert!(result.contains("def bar"));
        assert!(result.contains("x = 1"));
        assert!(result.contains("y = 2"));
        // Should stop before baz
        assert!(!result.contains("baz"));
    }

    #[test]
    fn indentation_block_unlimited() {
        let content = "a\n  b\n    c\n  d\ne\n";
        // Anchor on "c" (line 3), unlimited levels
        let result = read_indentation_block(content, 3, 0);
        // min_indent = 0, so everything is included until boundary
        assert!(result.contains("L1: a"));
        assert!(result.contains("L3:     c"));
    }

    #[test]
    fn indentation_block_anchor_out_of_range() {
        let content = "line1\nline2";
        let result = read_indentation_block(content, 99, 0);
        // Returns full content when anchor out of range
        assert_eq!(result, content);
    }

    #[test]
    fn indentation_block_blank_lines_trimmed() {
        let content = "\n\ndef foo():\n    x = 1\n\n\n";
        // Anchor on "x = 1" (line 4), max_levels=1
        let result = read_indentation_block(content, 4, 1);
        assert!(result.contains("def foo()"));
        assert!(result.contains("x = 1"));
        // Leading blank lines should be trimmed
        assert!(!result.starts_with("L1: \n"));
    }

    #[test]
    fn indentation_block_nested() {
        let content = "\
fn outer() {
    fn inner() {
        let x = 1;
        let y = 2;
    }
    fn other() {
        let z = 3;
    }
}";
        // Anchor on "let x = 1;" (line 3), max_levels=1
        let result = read_indentation_block(content, 3, 1);
        assert!(result.contains("fn inner()"));
        assert!(result.contains("let x = 1"));
        assert!(result.contains("let y = 2"));
        // Should not include other()
        assert!(!result.contains("other"));
    }
}
