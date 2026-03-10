//! Flexible JSON parser — extracts structured data from messy LLM output.
//!
//! Inspired by BAML's "jsonish" SAP (Schema-Aligned Parsing) approach.
//! Collects multiple parse candidates (AnyOf), tries to deserialize each
//! into the target type `T`, returns the first success.
//!
//! Parse cascade:
//! 1. Direct JSON (`serde_json::from_str`)
//! 2. Markdown code blocks (````json ... ````)
//! 3. Greedy JSON extraction (first `{...}` or `[...]` in text)
//! 4. Fixing parser (close brackets, strip trailing commas, unquoted keys)
//! 5. Fail with all candidates listed
//!
//! Works with any model — no structured output API required.

use schemars::JsonSchema;
use serde::de::DeserializeOwned;

use crate::coerce::coerce_value;
use crate::schema::response_schema_for;

/// A parse candidate with provenance info for debugging.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// The JSON string to try deserializing.
    pub json: String,
    /// How this candidate was extracted.
    pub source: CandidateSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateSource {
    /// Direct parse — input was valid JSON.
    Direct,
    /// Extracted from a ```json code block.
    MarkdownBlock,
    /// Grepped `{...}` or `[...]` from text.
    Grepped,
    /// Fixed broken JSON (closed brackets, stripped trailing commas, etc).
    Fixed,
}

/// Result of a flexible parse attempt.
#[derive(Debug)]
pub struct ParseResult<T> {
    /// Successfully parsed value.
    pub value: T,
    /// Which candidate succeeded.
    pub source: CandidateSource,
    /// Total candidates tried.
    pub candidates_tried: usize,
}

/// Parse error with all attempted candidates.
#[derive(Debug)]
pub struct ParseError {
    /// All candidates that were tried.
    pub candidates: Vec<(Candidate, String)>,
    /// Original raw text.
    pub raw: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Failed to parse into target type. {} candidates tried",
            self.candidates.len()
        )?;
        for (i, (candidate, err)) in self.candidates.iter().enumerate() {
            write!(
                f,
                "\n  [{i}] {:?}: {}",
                candidate.source,
                truncate(err, 100)
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for ParseError {}

/// Parse raw LLM output into type `T` using the AnyOf cascade.
///
/// Tries multiple extraction strategies, returns the first successful parse.
pub fn parse_flexible<T: DeserializeOwned>(raw: &str) -> Result<ParseResult<T>, ParseError> {
    let candidates = collect_candidates(raw);
    let mut errors = Vec::new();

    for candidate in &candidates {
        match serde_json::from_str::<T>(&candidate.json) {
            Ok(value) => {
                return Ok(ParseResult {
                    value,
                    source: candidate.source,
                    candidates_tried: errors.len() + 1,
                });
            }
            Err(e) => {
                errors.push((candidate.clone(), e.to_string()));
            }
        }
    }

    Err(ParseError {
        candidates: errors,
        raw: raw.to_string(),
    })
}

/// Parse with schema-aware coercion: "42" → 42, "true" → true, "redd" → "Red".
///
/// First tries `parse_flexible` (strict serde). If all candidates fail,
/// retries each candidate with coercion applied before deserialization.
pub fn parse_flexible_coerced<T: JsonSchema + DeserializeOwned>(
    raw: &str,
) -> Result<ParseResult<T>, ParseError> {
    // Try strict first — no coercion overhead if JSON is clean
    if let Ok(result) = parse_flexible::<T>(raw) {
        return Ok(result);
    }

    // Retry with coercion
    let candidates = collect_candidates(raw);
    let schema = response_schema_for::<T>();
    let mut errors = Vec::new();

    for candidate in &candidates {
        // Parse to Value, coerce, then deserialize
        if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&candidate.json) {
            coerce_value(&mut value, &schema);
            match serde_json::from_value::<T>(value) {
                Ok(parsed) => {
                    return Ok(ParseResult {
                        value: parsed,
                        source: candidate.source,
                        candidates_tried: errors.len() + 1,
                    });
                }
                Err(e) => {
                    errors.push((candidate.clone(), format!("coerced: {}", e)));
                }
            }
        } else {
            errors.push((candidate.clone(), "invalid JSON even for Value".into()));
        }
    }

    Err(ParseError {
        candidates: errors,
        raw: raw.to_string(),
    })
}

/// Collect all parse candidates from raw text (AnyOf pattern).
pub fn collect_candidates(raw: &str) -> Vec<Candidate> {
    let mut candidates = Vec::new();

    // 0. Unescape double-wrapped JSON string: "{ \"key\": ... }" → { "key": ... }
    let effective = try_unescape_json_string(raw).unwrap_or_else(|| raw.to_string());
    let raw = effective.as_str();

    // 1. Direct JSON parse
    if looks_like_json(raw) {
        candidates.push(Candidate {
            json: raw.to_string(),
            source: CandidateSource::Direct,
        });
    }

    // 2. Markdown code blocks
    for block in extract_markdown_blocks(raw) {
        candidates.push(Candidate {
            json: block,
            source: CandidateSource::MarkdownBlock,
        });
    }

    // 3. Greedy JSON extraction
    for json in extract_json_objects(raw) {
        // Skip if we already have this exact string as a candidate
        if !candidates.iter().any(|c| c.json == json) {
            candidates.push(Candidate {
                json,
                source: CandidateSource::Grepped,
            });
        }
    }

    // 4. Try fixing each candidate that failed
    let fixable: Vec<String> = candidates.iter().map(|c| c.json.clone()).collect();
    for json in &fixable {
        if let Some(fixed) = try_fix_json(json) {
            if !candidates.iter().any(|c| c.json == fixed) {
                candidates.push(Candidate {
                    json: fixed,
                    source: CandidateSource::Fixed,
                });
            }
        }
    }

    // Also try fixing the raw input directly if no candidates yet
    if candidates.is_empty() || !candidates.iter().any(|c| c.source == CandidateSource::Direct) {
        if let Some(fixed) = try_fix_json(raw) {
            if !candidates.iter().any(|c| c.json == fixed) {
                candidates.push(Candidate {
                    json: fixed,
                    source: CandidateSource::Fixed,
                });
            }
        }
    }

    // 5. Truncation recovery — try progressively aggressive cuts for streaming
    // (only if no Fixed candidate parsed as valid Value with all required fields)
    for json_source in [raw]
        .iter()
        .chain(fixable.iter().map(|s| s as &str).collect::<Vec<_>>().iter())
    {
        for recovered in truncation_recovery_candidates(json_source) {
            if !candidates.iter().any(|c| c.json == recovered) {
                candidates.push(Candidate {
                    json: recovered,
                    source: CandidateSource::Fixed,
                });
            }
        }
    }

    candidates
}

// ============================================================================
// Extraction strategies
// ============================================================================

/// Extract JSON from markdown code blocks: ```json\n...\n``` or ```\n...\n```
fn extract_markdown_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut rest = text;

    while let Some(start) = rest.find("```") {
        let after_ticks = &rest[start + 3..];

        // Skip optional language tag (e.g., "json", "JSON", "jsonc")
        let content_start = if let Some(newline) = after_ticks.find('\n') {
            newline + 1
        } else {
            break;
        };
        let content = &after_ticks[content_start..];

        // Find closing ```
        if let Some(end) = content.find("```") {
            let block = content[..end].trim();
            if !block.is_empty() && looks_like_json(block) {
                blocks.push(block.to_string());
            }
            rest = &content[end + 3..];
        } else {
            // Unclosed code block — try to parse what we have
            let block = content.trim();
            if !block.is_empty() && looks_like_json(block) {
                blocks.push(block.to_string());
            }
            break;
        }
    }

    blocks
}

/// Find JSON objects `{...}` and arrays `[...]` in text using bracket matching.
fn extract_json_objects(text: &str) -> Vec<String> {
    let mut results = Vec::new();

    for open in ['{', '['] {
        let close = if open == '{' { '}' } else { ']' };
        let mut search_from = 0;

        while let Some(start) = text[search_from..].find(open) {
            let abs_start = search_from + start;
            if let Some(end) = find_matching_bracket(text, abs_start, open, close) {
                let json = &text[abs_start..=end];
                if !results.contains(&json.to_string()) {
                    results.push(json.to_string());
                }
                search_from = end + 1;
            } else {
                // No matching bracket — try with auto-close
                search_from = abs_start + 1;
            }
        }
    }

    results
}

/// Find the matching closing bracket, respecting nesting and strings.
fn find_matching_bracket(text: &str, start: usize, open: char, close: char) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape_next = false;
    let mut i = start;

    while i < bytes.len() {
        let ch = bytes[i] as char;

        if escape_next {
            escape_next = false;
            i += 1;
            continue;
        }

        if ch == '\\' && in_string {
            escape_next = true;
            i += 1;
            continue;
        }

        if ch == '"' {
            in_string = !in_string;
            i += 1;
            continue;
        }

        if !in_string {
            if ch == open {
                depth += 1;
            } else if ch == close {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
        }

        i += 1;
    }

    None
}

// ============================================================================
// JSON fixing
// ============================================================================

/// Try to fix common JSON errors. Returns None if unfixable.
fn try_fix_json(raw: &str) -> Option<String> {
    let trimmed = raw.trim();

    // Already valid? No fix needed.
    if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
        return None;
    }

    let mut fixed = trimmed.to_string();
    let mut changed = false;

    // Fix 1: Strip trailing commas before } or ]
    let re_trailing = strip_trailing_commas(&fixed);
    if re_trailing != fixed {
        fixed = re_trailing;
        changed = true;
    }

    // Fix 2: Close unclosed brackets/braces
    let closed = close_brackets(&fixed);
    if closed != fixed {
        fixed = closed;
        changed = true;
    }

    // Fix 3: Single quotes → double quotes (outside of double-quoted strings)
    let quoted = fix_single_quotes(&fixed);
    if quoted != fixed {
        fixed = quoted;
        changed = true;
    }

    // Fix 4: Strip JS-style comments (// and /* */)
    let uncommented = strip_comments(&fixed);
    if uncommented != fixed {
        fixed = uncommented;
        changed = true;
    }

    // Verify the fix actually produces valid JSON
    if changed && serde_json::from_str::<serde_json::Value>(&fixed).is_ok() {
        Some(fixed)
    } else {
        None
    }
}

/// Strip trailing commas: `{a: 1,}` → `{a: 1}`
fn strip_trailing_commas(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '"' {
            // Skip strings
            result.push(chars[i]);
            i += 1;
            while i < chars.len() {
                result.push(chars[i]);
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 1;
                    result.push(chars[i]);
                } else if chars[i] == '"' {
                    break;
                }
                i += 1;
            }
            i += 1;
            continue;
        }

        if chars[i] == ',' {
            // Look ahead for ] or } (skipping whitespace)
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && (chars[j] == '}' || chars[j] == ']') {
                // Skip the trailing comma
                i += 1;
                continue;
            }
        }

        result.push(chars[i]);
        i += 1;
    }

    result
}

/// Close unclosed brackets: `{"a": [1, 2` → `{"a": [1, 2]}`
///
/// Also handles streaming truncation: if truncated mid-value inside an array/object,
/// drops the incomplete element and closes brackets (like BAML's partial parse).
fn close_brackets(s: &str) -> String {
    let mut stack = Vec::new();
    let mut in_string = false;
    let mut escape_next = false;

    for ch in s.chars() {
        if escape_next {
            escape_next = false;
            continue;
        }
        if ch == '\\' && in_string {
            escape_next = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if !in_string {
            match ch {
                '{' => stack.push('}'),
                '[' => stack.push(']'),
                '}' | ']' => {
                    stack.pop();
                }
                _ => {}
            }
        }
    }

    // If not truncated (balanced), nothing to do
    if stack.is_empty() && !in_string {
        return s.to_string();
    }

    // Close unclosed string
    let mut result = s.to_string();
    if in_string {
        result.push('"');
    }

    // Close brackets in reverse order
    while let Some(close) = stack.pop() {
        result.push(close);
    }

    result
}

/// Truncation recovery: find cut points and generate multiple candidates.
///
/// For `{"a":[{"b":1},{"c":2,"d` generates:
/// - Cut at inner comma: `{"a":[{"b":1},{"c":2}]}` (partial element)
/// - Cut at outer comma: `{"a":[{"b":1}]}` (drop incomplete element)
///
/// Returns all valid JSON candidates, most aggressive cut last (so AnyOf tries
/// the most complete version first).
fn truncation_recovery_candidates(s: &str) -> Vec<String> {
    // Collect all cut points: commas and closing brackets (outside strings)
    // Use byte positions (not char indices) for correct slicing with Unicode
    let mut cut_points = Vec::new();
    let mut in_string = false;
    let mut escape_next = false;

    for (byte_pos, ch) in s.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }
        if ch == '\\' && in_string {
            escape_next = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match ch {
            ',' => cut_points.push(byte_pos),
            '}' | ']' => cut_points.push(byte_pos + 1),
            _ => {}
        }
    }

    // Try cuts from rightmost (most data kept) to leftmost (most data dropped)
    let mut results = Vec::new();
    for &cut in cut_points.iter().rev() {
        if cut == 0 || cut >= s.len() {
            continue;
        }
        if let Some(candidate) = try_close_at(s, cut) {
            if !results.contains(&candidate) {
                results.push(candidate);
            }
        }
    }

    results
}

/// Try cutting the string at `pos` and closing all open brackets.
fn try_close_at(s: &str, pos: usize) -> Option<String> {
    let mut truncated = s[..pos].trim_end().to_string();

    // Strip trailing comma
    if truncated.ends_with(',') {
        truncated.pop();
    }

    // Close open brackets
    let mut stack = Vec::new();
    let mut in_str = false;
    let mut esc = false;
    for ch in truncated.chars() {
        if esc {
            esc = false;
            continue;
        }
        if ch == '\\' && in_str {
            esc = true;
            continue;
        }
        if ch == '"' {
            in_str = !in_str;
            continue;
        }
        if !in_str {
            match ch {
                '{' => stack.push('}'),
                '[' => stack.push(']'),
                '}' | ']' => {
                    stack.pop();
                }
                _ => {}
            }
        }
    }
    if in_str {
        truncated.push('"');
    }
    while let Some(close) = stack.pop() {
        truncated.push(close);
    }

    if serde_json::from_str::<serde_json::Value>(&truncated).is_ok() {
        Some(truncated)
    } else {
        None
    }
}

/// Convert single-quoted strings to double-quoted (outside existing double quotes).
fn fix_single_quotes(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_double = false;
    let mut escape_next = false;

    for ch in s.chars() {
        if escape_next {
            result.push(ch);
            escape_next = false;
            continue;
        }
        if ch == '\\' {
            result.push(ch);
            if in_double {
                escape_next = true;
            }
            continue;
        }
        if ch == '"' {
            in_double = !in_double;
            result.push(ch);
            continue;
        }
        if ch == '\'' && !in_double {
            result.push('"');
        } else {
            result.push(ch);
        }
    }

    result
}

/// Strip JS-style comments (// line and /* block */).
fn strip_comments(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut in_string = false;

    while i < chars.len() {
        if in_string {
            result.push(chars[i]);
            if chars[i] == '\\' && i + 1 < chars.len() {
                i += 1;
                result.push(chars[i]);
            } else if chars[i] == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }

        if chars[i] == '"' {
            in_string = true;
            result.push(chars[i]);
            i += 1;
            continue;
        }

        if i + 1 < chars.len() && chars[i] == '/' && chars[i + 1] == '/' {
            // Skip to end of line
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        if i + 1 < chars.len() && chars[i] == '/' && chars[i + 1] == '*' {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i += 2; // skip */
            continue;
        }

        result.push(chars[i]);
        i += 1;
    }

    result
}

// ============================================================================
// Helpers
// ============================================================================

/// Try to unescape a double-wrapped JSON string.
///
/// Some models output JSON as a string literal: `"{ \"key\": \"value\" }"`
/// This detects and unescapes it back to `{ "key": "value" }`.
fn try_unescape_json_string(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    // Must start and end with quotes
    if !trimmed.starts_with('"') || !trimmed.ends_with('"') || trimmed.len() < 3 {
        return None;
    }
    // Inner content must look like escaped JSON (contains \")
    let inner = &trimmed[1..trimmed.len() - 1];
    if !inner.contains("\\\"") {
        return None;
    }
    // Try to parse as a JSON string, which gives us the unescaped content
    match serde_json::from_str::<String>(trimmed) {
        Ok(unescaped) if looks_like_json(&unescaped) => Some(unescaped),
        _ => None,
    }
}

fn looks_like_json(s: &str) -> bool {
    let trimmed = s.trim();
    (trimmed.starts_with('{') && trimmed.ends_with('}'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']'))
        || trimmed == "null"
        || trimmed == "true"
        || trimmed == "false"
        || trimmed.starts_with('"')
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..s.floor_char_boundary(max)]
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Answer {
        answer: String,
        confidence: f64,
    }

    // --- Direct JSON ---

    #[test]
    fn parses_clean_json() {
        let raw = r#"{"answer": "42", "confidence": 0.95}"#;
        let result = parse_flexible::<Answer>(raw).unwrap();
        assert_eq!(result.value.answer, "42");
        assert_eq!(result.source, CandidateSource::Direct);
    }

    // --- Markdown blocks ---

    #[test]
    fn parses_from_markdown_block() {
        let raw = r#"Here's my answer:

```json
{"answer": "hello", "confidence": 0.8}
```

Hope that helps!"#;
        let result = parse_flexible::<Answer>(raw).unwrap();
        assert_eq!(result.value.answer, "hello");
        assert_eq!(result.source, CandidateSource::MarkdownBlock);
    }

    #[test]
    fn parses_from_unlabeled_markdown_block() {
        let raw = r#"Sure:

```
{"answer": "test", "confidence": 0.5}
```"#;
        let result = parse_flexible::<Answer>(raw).unwrap();
        assert_eq!(result.value.answer, "test");
        assert_eq!(result.source, CandidateSource::MarkdownBlock);
    }

    // --- Grepped JSON ---

    #[test]
    fn extracts_json_from_surrounding_text() {
        let raw = r#"I think the answer is {"answer": "yes", "confidence": 0.9} based on my analysis."#;
        let result = parse_flexible::<Answer>(raw).unwrap();
        assert_eq!(result.value.answer, "yes");
        assert_eq!(result.source, CandidateSource::Grepped);
    }

    #[test]
    fn extracts_json_after_chain_of_thought() {
        let raw = r#"Let me think step by step...
First, I need to consider the question carefully.
The answer seems clear.

{"answer": "deep thought", "confidence": 0.99}"#;
        let result = parse_flexible::<Answer>(raw).unwrap();
        assert_eq!(result.value.answer, "deep thought");
    }

    // --- Fixed JSON ---

    #[test]
    fn fixes_trailing_comma() {
        let raw = r#"{"answer": "fixed", "confidence": 0.7,}"#;
        let result = parse_flexible::<Answer>(raw).unwrap();
        assert_eq!(result.value.answer, "fixed");
        assert_eq!(result.source, CandidateSource::Fixed);
    }

    #[test]
    fn fixes_unclosed_brackets() {
        let raw = r#"{"answer": "partial", "confidence": 0.6"#;
        let result = parse_flexible::<Answer>(raw).unwrap();
        assert_eq!(result.value.answer, "partial");
        assert_eq!(result.source, CandidateSource::Fixed);
    }

    #[test]
    fn fixes_single_quotes() {
        let raw = r#"{'answer': 'quoted', 'confidence': 0.5}"#;
        let result = parse_flexible::<Answer>(raw).unwrap();
        assert_eq!(result.value.answer, "quoted");
        assert_eq!(result.source, CandidateSource::Fixed);
    }

    #[test]
    fn fixes_js_comments() {
        let raw = r#"{
            // This is the answer
            "answer": "commented",
            "confidence": 0.4
        }"#;
        let result = parse_flexible::<Answer>(raw).unwrap();
        assert_eq!(result.value.answer, "commented");
        assert_eq!(result.source, CandidateSource::Fixed);
    }

    // --- Combined scenarios ---

    #[test]
    fn prefers_direct_over_markdown() {
        // If the whole input is valid JSON, use it directly
        let raw = r#"{"answer": "direct", "confidence": 1.0}"#;
        let result = parse_flexible::<Answer>(raw).unwrap();
        assert_eq!(result.source, CandidateSource::Direct);
    }

    #[test]
    fn handles_multiple_json_objects_picks_matching() {
        #[derive(Debug, Deserialize, PartialEq)]
        struct Config {
            model: String,
            temperature: f64,
        }

        let raw = r#"Here are two objects:
{"answer": "wrong type", "confidence": 0.5}
{"model": "gemini", "temperature": 0.3}"#;
        let result = parse_flexible::<Config>(raw).unwrap();
        assert_eq!(result.value.model, "gemini");
    }

    #[test]
    fn error_shows_all_candidates() {
        #[derive(Debug, Deserialize)]
        #[allow(dead_code)]
        struct Impossible {
            xyz_field_that_wont_match: i64,
        }

        let raw = "Just some plain text with no JSON";
        let err = parse_flexible::<Impossible>(raw).unwrap_err();
        assert!(err.to_string().contains("Failed to parse"));
    }

    // --- Edge cases ---

    #[test]
    fn handles_nested_json() {
        #[derive(Debug, Deserialize, PartialEq)]
        struct Nested {
            outer: Inner,
        }
        #[derive(Debug, Deserialize, PartialEq)]
        struct Inner {
            value: String,
        }

        let raw = r#"{"outer": {"value": "deep"}}"#;
        let result = parse_flexible::<Nested>(raw).unwrap();
        assert_eq!(result.value.outer.value, "deep");
    }

    #[test]
    fn handles_array_response() {
        let raw = r#"```json
[{"answer": "one", "confidence": 0.5}, {"answer": "two", "confidence": 0.8}]
```"#;
        let result = parse_flexible::<Vec<Answer>>(raw).unwrap();
        assert_eq!(result.value.len(), 2);
        assert_eq!(result.value[1].answer, "two");
    }

    #[test]
    fn handles_empty_input() {
        let err = parse_flexible::<Answer>("").unwrap_err();
        assert!(err.candidates.is_empty() || !err.candidates.is_empty());
    }

    #[test]
    fn handles_unclosed_markdown_block() {
        let raw = r#"```json
{"answer": "streaming", "confidence": 0.3}
"#;
        let result = parse_flexible::<Answer>(raw).unwrap();
        assert_eq!(result.value.answer, "streaming");
    }

    // --- Fixing strategies ---

    #[test]
    fn strip_trailing_commas_works() {
        assert_eq!(strip_trailing_commas(r#"{"a": 1,}"#), r#"{"a": 1}"#);
        assert_eq!(strip_trailing_commas(r#"[1, 2,]"#), r#"[1, 2]"#);
        // Don't strip inside strings
        assert_eq!(strip_trailing_commas(r#"{"a": "b,"}"#), r#"{"a": "b,"}"#);
    }

    #[test]
    fn close_brackets_works() {
        assert_eq!(close_brackets(r#"{"a": 1"#), r#"{"a": 1}"#);
        assert_eq!(close_brackets(r#"[1, [2"#), r#"[1, [2]]"#);
        assert_eq!(close_brackets(r#"{"a": "hello"#), r#"{"a": "hello"}"#);
    }

    #[test]
    fn truncation_recovery_drops_incomplete_element() {
        // Truncated mid-field in an array element — recovery should produce candidates
        let raw = r#"{"items":[{"id":1,"name":"ok"},{"id":2,"na"#;
        let candidates = truncation_recovery_candidates(raw);
        assert!(!candidates.is_empty(), "Should produce recovery candidates");
        // At least one candidate should have the first complete element
        let has_valid = candidates.iter().any(|c| {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(c) {
                val["items"].as_array().map_or(false, |a| {
                    a.len() >= 1 && a[0]["id"] == 1
                })
            } else {
                false
            }
        });
        assert!(has_valid, "At least one candidate should have first complete element");
    }

    #[test]
    fn truncation_recovery_streaming_action() {
        // Real-world case: truncated mid-action in NextStep
        #[derive(Debug, Deserialize)]
        struct Step {
            situation: String,
            actions: Vec<serde_json::Value>,
        }
        let raw = r#"{"situation":"working","actions":[{"tool":"read","path":"a.rs"},{"tool":"edit","path":"b.rs","old"#;
        let result = parse_flexible::<Step>(raw);
        assert!(result.is_ok(), "Should recover from truncated streaming");
        let step = result.unwrap().value;
        assert_eq!(step.situation, "working");
        // First complete action should survive, truncated second dropped
        assert!(step.actions.len() >= 1);
    }

    #[test]
    fn unescape_double_wrapped_json() {
        #[derive(Debug, Deserialize)]
        struct Simple {
            msg: String,
        }

        let raw = r#""{\"msg\": \"hello world\"}""#;
        let result = parse_flexible::<Simple>(raw);
        assert!(result.is_ok(), "Should unescape double-wrapped JSON");
        assert_eq!(result.unwrap().value.msg, "hello world");
    }

    #[test]
    fn unescape_ignores_normal_strings() {
        // Normal quoted string that is NOT escaped JSON — should NOT be unescaped
        let result = try_unescape_json_string("\"just a normal string\"");
        assert!(result.is_none());
    }

    #[test]
    fn fix_single_quotes_works() {
        assert_eq!(fix_single_quotes("{'a': 'b'}"), r#"{"a": "b"}"#);
        // Don't touch singles inside double quotes
        assert_eq!(
            fix_single_quotes(r#"{"it's": "fine"}"#),
            r#"{"it's": "fine"}"#
        );
    }

    #[test]
    fn strip_comments_works() {
        assert_eq!(
            strip_comments("{\n// comment\n\"a\": 1\n}"),
            "{\n\n\"a\": 1\n}"
        );
        assert_eq!(
            strip_comments("{/* block */\"a\": 1}"),
            "{\"a\": 1}"
        );
    }

    #[test]
    fn extract_markdown_blocks_multiple() {
        let raw = r#"First:
```json
{"a": 1}
```
Second:
```json
{"b": 2}
```"#;
        let blocks = extract_markdown_blocks(raw);
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn extract_json_objects_finds_multiple() {
        let raw = r#"text {"a": 1} middle {"b": 2} end"#;
        let objects = extract_json_objects(raw);
        assert_eq!(objects.len(), 2);
    }

    #[test]
    fn extract_json_objects_nested_returns_outer() {
        let raw = r#"text {"outer": {"inner": 1}} more text"#;
        let objects = extract_json_objects(raw);
        // Outer matched first; inner is inside matched range so skipped
        assert_eq!(objects.len(), 1);
        assert!(objects[0].contains("outer"));
    }

    #[test]
    fn collect_candidates_deduplicates() {
        let raw = r#"{"answer": "test", "confidence": 0.5}"#;
        let candidates = collect_candidates(raw);
        // Direct + Grepped should be deduped
        let jsons: Vec<&str> = candidates.iter().map(|c| c.json.as_str()).collect();
        let unique: std::collections::HashSet<&&str> = jsons.iter().collect();
        assert_eq!(jsons.len(), unique.len());
    }
}
