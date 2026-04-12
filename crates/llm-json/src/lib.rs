//! # LLM JSON Repair
//!
//! A Rust library to repair broken JSON strings, particularly useful for handling
//! malformed JSON output from Large Language Models (LLMs).
//!
//! This is a porting of the Python library json_repair (https://github.com/mangiucugna/json_repair),
//! written by Stefano Baccianella (https://github.com/mangiucugna) and published under the MIT license.
//! All credits go to the original author for the amazing work.
//!
//! ## Features
//!
//! - Fix missing quotes around keys and values
//! - Handle misplaced commas and missing brackets
//! - Repair incomplete arrays and objects
//! - Remove extra non-JSON characters
//! - Auto-complete missing values with sensible defaults
//! - Preserve Unicode characters
//!
//! ## Usage
//!
//! ```rust
//! use llm_json::{repair_json, loads, JsonRepairError};
//!
//! // Basic repair
//! let broken_json = r#"{name: 'John', age: 30,}"#;
//! let repaired = repair_json(broken_json, &Default::default()).unwrap();
//! println!("{}", repaired); // {"name": "John", "age": 30}
//!
//! // Parse directly to Value
//! let value = loads(broken_json, &Default::default()).unwrap();
//! ```

use serde_json::Value;
use std::fs;
use std::io::{self, Read};
use std::path::Path;
use thiserror::Error;

/// Errors that can occur during JSON repair
#[derive(Debug, Error)]
pub enum JsonRepairError {
    #[error("JSON string is too broken to repair")]
    UnrepairableJson,
    #[error("IO error: {0}")]
    IoError(#[from] io::Error),
    #[error("Serde JSON error: {0}")]
    SerdeError(#[from] serde_json::Error),
    #[error("Invalid UTF-8 in input")]
    Utf8Error(#[from] std::str::Utf8Error),
}

/// Configuration options for JSON repair
#[derive(Debug, Clone)]
pub struct RepairOptions {
    /// Skip validation with serde_json for performance
    pub skip_json_loads: bool,
    /// Return objects instead of JSON strings
    pub return_objects: bool,
    /// Preserve non-ASCII characters
    pub ensure_ascii: bool,
    /// Handle streaming/incomplete JSON
    pub stream_stable: bool,
}

impl Default for RepairOptions {
    fn default() -> Self {
        Self {
            skip_json_loads: false,
            return_objects: false,
            ensure_ascii: true,
            stream_stable: false,
        }
    }
}

/// Internal parser state for tracking context
#[derive(Debug, Clone, Copy, PartialEq)]
enum ParseState {
    Root,
    Object,
    Array,
}

/// JSON repair parser
struct JsonRepairParser {
    input: Vec<char>,
    pos: usize,
    output: String,
    state_stack: Vec<ParseState>,
    options: RepairOptions,
}

impl JsonRepairParser {
    fn new(input: &str, options: RepairOptions) -> Self {
        Self {
            input: input.chars().collect(),
            pos: 0,
            output: String::new(),
            state_stack: vec![ParseState::Root],
            options,
        }
    }

    fn current_char(&self) -> Option<char> {
        self.input.get(self.pos).copied()
    }

    fn peek_char(&self, offset: usize) -> Option<char> {
        self.input.get(self.pos + offset).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let ch = self.current_char();
        if ch.is_some() {
            self.pos += 1;
        }
        ch
    }

    fn current_state(&self) -> ParseState {
        self.state_stack.last().copied().unwrap_or(ParseState::Root)
    }

    fn push_state(&mut self, state: ParseState) {
        self.state_stack.push(state);
    }

    fn pop_state(&mut self) -> Option<ParseState> {
        if self.state_stack.len() > 1 {
            self.state_stack.pop()
        } else {
            None
        }
    }

    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.current_char() {
            if ch.is_whitespace() {
                self.advance();
            } else {
                break;
            }
        }
    }

    fn skip_comments(&mut self) {
        if let (Some('/'), Some('/')) = (self.current_char(), self.peek_char(1)) {
            // Skip line comment
            while let Some(ch) = self.advance() {
                if ch == '\n' {
                    break;
                }
            }
        } else if let (Some('/'), Some('*')) = (self.current_char(), self.peek_char(1)) {
            // Skip block comment
            self.advance(); // skip '/'
            self.advance(); // skip '*'
            while let Some(ch) = self.advance() {
                if ch == '*' && self.peek_char(0) == Some('/') {
                    self.advance(); // skip '/'
                    break;
                }
            }
        }
    }

    fn append_char(&mut self, ch: char) {
        self.output.push(ch);
    }

    fn append_str(&mut self, s: &str) {
        self.output.push_str(s);
    }

    fn parse_string(&mut self) -> Result<(), JsonRepairError> {
        let quote_char = if self.current_char() == Some('"') {
            '"'
        } else if self.current_char() == Some('\'') {
            '\''
        } else {
            // Unquoted string - add quotes
            self.append_char('"');
            return self.parse_unquoted_string();
        };

        self.append_char('"'); // Always use double quotes in output
        self.advance(); // Skip opening quote

        while let Some(ch) = self.current_char() {
            if ch == quote_char {
                self.advance();
                self.append_char('"');
                return Ok(());
            } else if ch == '\\' {
                self.append_char(ch);
                self.advance();
                if let Some(escaped) = self.current_char() {
                    self.append_char(escaped);
                    self.advance();
                }
            } else if ch == '"' && quote_char == '\'' {
                // Escape double quotes inside single-quoted strings
                self.append_str("\\\"");
                self.advance();
            } else {
                if !self.options.ensure_ascii || ch.is_ascii() {
                    self.append_char(ch);
                } else {
                    self.append_str(&format!("\\u{:04x}", ch as u32));
                }
                self.advance();
            }
        }

        // Unclosed string - close it
        self.append_char('"');
        Ok(())
    }

    fn parse_unquoted_string(&mut self) -> Result<(), JsonRepairError> {
        while let Some(ch) = self.current_char() {
            match ch {
                ',' | '}' | ']' | ':' => break,
                '"' => {
                    self.append_str("\\\"");
                    self.advance();
                }
                '\\' => {
                    self.append_str("\\\\");
                    self.advance();
                }
                _ if ch.is_whitespace() => {
                    // Check if this is trailing whitespace
                    let mut temp_pos = self.pos + 1;
                    let mut found_delimiter = false;
                    while let Some(temp_ch) = self.input.get(temp_pos) {
                        if matches!(temp_ch, ',' | '}' | ']' | ':') {
                            found_delimiter = true;
                            break;
                        } else if !temp_ch.is_whitespace() {
                            break;
                        }
                        temp_pos += 1;
                    }

                    if found_delimiter {
                        break; // Stop at trailing whitespace
                    } else {
                        self.append_char(ch);
                        self.advance();
                    }
                }
                _ => {
                    if !self.options.ensure_ascii || ch.is_ascii() {
                        self.append_char(ch);
                    } else {
                        self.append_str(&format!("\\u{:04x}", ch as u32));
                    }
                    self.advance();
                }
            }
        }
        self.append_char('"');
        Ok(())
    }

    fn parse_number(&mut self) -> Result<(), JsonRepairError> {
        let start_pos = self.pos;

        // Handle negative sign
        if self.current_char() == Some('-') {
            self.append_char('-');
            self.advance();
        }

        // Parse integer part
        if self.current_char() == Some('0') {
            self.append_char('0');
            self.advance();
        } else {
            while let Some(ch) = self.current_char() {
                if ch.is_ascii_digit() {
                    self.append_char(ch);
                    self.advance();
                } else {
                    break;
                }
            }
        }

        // Parse decimal part
        if self.current_char() == Some('.') {
            self.append_char('.');
            self.advance();

            let before_digits = self.pos;
            while let Some(ch) = self.current_char() {
                if ch.is_ascii_digit() {
                    self.append_char(ch);
                    self.advance();
                } else {
                    break;
                }
            }

            // If no digits after decimal, add zero
            if self.pos == before_digits {
                self.append_char('0');
            }
        }

        // Parse exponent part
        if matches!(self.current_char(), Some('e') | Some('E')) {
            self.append_char('e');
            self.advance();

            if matches!(self.current_char(), Some('+') | Some('-')) {
                self.append_char(self.current_char().unwrap());
                self.advance();
            }

            let before_exp_digits = self.pos;
            while let Some(ch) = self.current_char() {
                if ch.is_ascii_digit() {
                    self.append_char(ch);
                    self.advance();
                } else {
                    break;
                }
            }

            // If no digits after exponent, add zero
            if self.pos == before_exp_digits {
                self.append_char('0');
            }
        }

        // If we didn't parse anything valid, it's not a number
        if self.pos == start_pos || (self.pos == start_pos + 1 && self.input[start_pos] == '-') {
            // Reset and treat as unquoted string
            self.pos = start_pos;
            self.output
                .truncate(self.output.len() - (self.pos - start_pos));
            self.append_char('"');
            return self.parse_unquoted_string();
        }

        Ok(())
    }

    fn parse_literal(&mut self) -> Result<(), JsonRepairError> {
        let start_pos = self.pos;
        let mut literal = String::new();

        while let Some(ch) = self.current_char() {
            if matches!(ch, ',' | '}' | ']' | ':') || ch.is_whitespace() {
                break;
            }
            literal.push(ch);
            self.advance();
        }

        match literal.to_lowercase().as_str() {
            "true" => self.append_str("true"),
            "false" => self.append_str("false"),
            "null" | "none" | "undefined" => self.append_str("null"),
            _ => {
                // Reset and treat as unquoted string
                self.pos = start_pos;
                self.append_char('"');
                self.parse_unquoted_string()?;
            }
        }

        Ok(())
    }

    fn parse_value(&mut self) -> Result<(), JsonRepairError> {
        self.skip_whitespace();
        self.skip_comments();
        self.skip_whitespace();

        match self.current_char() {
            None => {
                // End of input - provide default value based on context
                match self.current_state() {
                    ParseState::Array => self.append_str("null"),
                    _ => self.append_str("null"),
                }
            }
            Some('"') | Some('\'') => {
                self.parse_string()?;
            }
            Some(ch) if ch.is_ascii_digit() || ch == '-' => {
                self.parse_number()?;
            }
            Some('{') => {
                self.parse_object()?;
            }
            Some('[') => {
                self.parse_array()?;
            }
            Some(ch) if ch.is_alphabetic() => {
                self.parse_literal()?;
            }
            Some(_) => {
                // Invalid character - treat as unquoted string
                self.append_char('"');
                self.parse_unquoted_string()?;
            }
        }

        Ok(())
    }

    fn parse_object(&mut self) -> Result<(), JsonRepairError> {
        self.append_char('{');
        self.advance(); // Skip '{'
        self.push_state(ParseState::Object);

        let mut expecting_key = true;
        let mut needs_comma = false;

        loop {
            let pos_before = self.pos; // Safety check for infinite loops

            self.skip_whitespace();
            self.skip_comments();
            self.skip_whitespace();

            if needs_comma && !expecting_key {
                if let Some(ch) = self.current_char() {
                    if ch == '"' || ch == '\'' || ch.is_alphabetic() || ch == '_' {
                        self.append_char(',');
                        expecting_key = true;
                        needs_comma = false;
                    }
                }
            }

            match self.current_char() {
                None => {
                    // Incomplete object - close it
                    self.append_char('}');
                    break;
                }
                Some('}') => {
                    self.advance();
                    self.append_char('}');
                    break;
                }
                Some(',') => {
                    self.advance();
                    // Skip trailing or multiple commas
                    self.skip_whitespace();
                    if matches!(self.current_char(), Some('}') | None) {
                        // Trailing comma - ignore it
                        continue;
                    }
                    if !expecting_key {
                        self.append_char(',');
                        expecting_key = true;
                        needs_comma = false;
                    }
                    continue;
                }
                _ => {
                    if needs_comma {
                        self.append_char(',');
                    }

                    if expecting_key {
                        // Parse key
                        if matches!(self.current_char(), Some('"') | Some('\'')) {
                            self.parse_string()?;
                        } else {
                            // Unquoted key
                            self.append_char('"');
                            self.parse_unquoted_string()?;
                        }

                        // Expect colon
                        self.skip_whitespace();
                        if self.current_char() == Some(':') {
                            self.advance();
                            self.append_char(':');
                        } else {
                            self.append_char(':');
                        }

                        // Parse value
                        self.parse_value()?;

                        expecting_key = false;
                        needs_comma = true;
                    } else {
                        // We have a value but expected a key - this shouldn't happen
                        // Add a default key
                        self.append_str("\"unknown\":");
                        self.parse_value()?;
                        expecting_key = false;
                        needs_comma = true;
                    }
                }
            }

            // Safety check: ensure we're making progress
            if self.pos == pos_before && self.pos < self.input.len() {
                // We're stuck - advance one character to avoid infinite loop
                self.advance();
            }
        }

        self.pop_state();
        Ok(())
    }

    fn parse_array(&mut self) -> Result<(), JsonRepairError> {
        self.append_char('[');
        self.advance(); // Skip '['
        self.push_state(ParseState::Array);

        let mut needs_comma = false;

        loop {
            let pos_before = self.pos; // Safety check for infinite loops

            self.skip_whitespace();
            self.skip_comments();
            self.skip_whitespace();

            match self.current_char() {
                None => {
                    // Incomplete array - close it
                    self.append_char(']');
                    break;
                }
                Some(']') => {
                    self.advance();
                    self.append_char(']');
                    break;
                }
                Some(',') => {
                    self.advance();
                    // Skip trailing or multiple commas
                    self.skip_whitespace();
                    if matches!(self.current_char(), Some(']') | None) {
                        // Trailing comma - ignore it
                        continue;
                    }
                    if needs_comma {
                        self.append_char(',');
                        needs_comma = false;
                    }
                    continue;
                }
                _ => {
                    if needs_comma {
                        self.append_char(',');
                    }

                    self.parse_value()?;
                    needs_comma = true;
                }
            }

            // Safety check: ensure we're making progress
            if self.pos == pos_before && self.pos < self.input.len() {
                // We're stuck - advance one character to avoid infinite loop
                self.advance();
            }
        }

        self.pop_state();
        Ok(())
    }

    fn parse(&mut self) -> Result<(), JsonRepairError> {
        // Skip any leading non-JSON content and handle markdown code blocks
        let input_str: String = self.input.iter().collect();

        // Handle markdown code blocks
        if let Some(start) = input_str.find("```json") {
            if let Some(end) = input_str.rfind("```") {
                if end > start + 7 {
                    let json_content = &input_str[start + 7..end];
                    self.input = json_content.chars().collect();
                    self.pos = 0;
                }
            }
        }

        self.skip_whitespace();

        // Look for JSON start markers, skipping explanatory text
        while let Some(ch) = self.current_char() {
            if matches!(ch, '{' | '[' | '"' | '\'' | '-')
                || ch.is_ascii_digit()
                || ch.is_alphabetic()
            {
                break;
            }
            self.advance();
        }

        // If we find text like "Here's the JSON:", skip to the actual JSON
        let remaining: String = self.input[self.pos..].iter().collect();
        if let Some(json_start) = remaining.find('{').or_else(|| remaining.find('[')) {
            self.pos += json_start;
        }

        self.parse_value()?;

        // Skip any trailing content
        self.skip_whitespace();

        Ok(())
    }

    fn get_result(self) -> String {
        self.output
    }
}

/// Repair a broken JSON string
///
/// # Arguments
///
/// * `json_str` - The broken JSON string to repair
/// * `options` - Configuration options for the repair process
///
/// # Returns
///
/// * `Ok(String)` - The repaired JSON string
/// * `Err(JsonRepairError)` - If the JSON is too broken to repair
///
/// # Examples
///
/// ```rust
/// use llm_json::{repair_json, RepairOptions};
///
/// fn main() -> Result<(), Box<dyn std::error::Error>> {
///     let broken   = r#"{name: 'John', age: 30,}"#;
///     let repaired = repair_json(broken, &RepairOptions::default())?;
///     assert_eq!(repaired, r#"{"age":30,"name":"John"}"#);
///     Ok(())
/// }
/// ```
/// Escape literal control characters (newlines, tabs, etc.) inside JSON string values.
/// Walks the string tracking whether we're inside a quoted string or not.
fn escape_control_chars_in_strings(input: &str) -> String {
    let mut result = String::with_capacity(input.len() + 64);
    let mut in_string = false;
    let mut prev_backslash = false;

    for ch in input.chars() {
        if in_string {
            if prev_backslash {
                // Previous char was \, this is an escape sequence — emit as-is
                result.push(ch);
                prev_backslash = false;
                continue;
            }
            match ch {
                '\\' => {
                    result.push(ch);
                    prev_backslash = true;
                }
                '"' => {
                    result.push(ch);
                    in_string = false;
                }
                '\n' => result.push_str("\\n"),
                '\r' => result.push_str("\\r"),
                '\t' => result.push_str("\\t"),
                c if c.is_control() => {
                    result.push_str(&format!("\\u{:04x}", c as u32));
                }
                _ => result.push(ch),
            }
        } else {
            result.push(ch);
            if ch == '"' {
                in_string = true;
            }
        }
        if ch != '\\' {
            prev_backslash = false;
        }
    }
    result
}

pub fn repair_json(json_str: &str, options: &RepairOptions) -> Result<String, JsonRepairError> {
    if json_str.trim().is_empty() {
        return Ok("{}".to_string());
    }

    // First try to parse as-is if skip_json_loads is false
    if !options.skip_json_loads {
        if let Ok(value) = serde_json::from_str::<Value>(json_str) {
            // Already valid — return compact format
            return Ok(serde_json::to_string(&value)?);
        }
    }

    // Pre-process: escape literal control characters inside JSON string values.
    // LLMs often emit raw newlines/tabs inside strings instead of \n/\t.
    let preprocessed = escape_control_chars_in_strings(json_str);
    // If pre-processing made it valid, return early
    if !options.skip_json_loads {
        if let Ok(value) = serde_json::from_str::<Value>(&preprocessed) {
            return Ok(serde_json::to_string(&value)?);
        }
    }

    let mut parser = JsonRepairParser::new(&preprocessed, options.clone());
    parser.parse()?;

    let repaired = parser.get_result();

    // Validate the repaired JSON unless skipping validation
    if !options.skip_json_loads {
        let parsed: Value = serde_json::from_str(&repaired)?;
        // Return compact JSON format consistently
        return Ok(serde_json::to_string(&parsed)?);
    }

    Ok(repaired)
}

/// Repair and parse a JSON string, returning the parsed Value
///
/// # Arguments
///
/// * `json_str` - The broken JSON string to repair and parse
/// * `options` - Configuration options for the repair process
///
/// # Returns
///
/// * `Ok(Value)` - The parsed JSON value
/// * `Err(JsonRepairError)` - If the JSON is too broken to repair or parse
///
/// # Examples
///
/// ```rust
/// use llm_json::{loads, RepairOptions};
/// use serde_json::Value;
///
/// let broken = r#"{name: 'John', age: 30}"#;
/// let value: Value = loads(broken, &RepairOptions::default()).unwrap();
///
/// if let Value::Object(obj) = value {
///     assert_eq!(obj["name"], "John");
///     assert_eq!(obj["age"], 30);
/// }
/// ```
pub fn loads(json_str: &str, options: &RepairOptions) -> Result<Value, JsonRepairError> {
    let repaired = repair_json(json_str, options)?;
    Ok(serde_json::from_str(&repaired)?)
}

/// Repair and parse JSON from a file
///
/// # Arguments
///
/// * `path` - Path to the JSON file
/// * `options` - Configuration options for the repair process
///
/// # Returns
///
/// * `Ok(Value)` - The parsed JSON value
/// * `Err(JsonRepairError)` - If the file cannot be read or JSON cannot be repaired
pub fn from_file<P: AsRef<Path>>(
    path: P,
    options: &RepairOptions,
) -> Result<Value, JsonRepairError> {
    let content = fs::read_to_string(path)?;
    loads(&content, options)
}

/// Repair and parse JSON from a reader
///
/// # Arguments
///
/// * `reader` - A reader containing JSON data
/// * `options` - Configuration options for the repair process
///
/// # Returns
///
/// * `Ok(Value)` - The parsed JSON value
/// * `Err(JsonRepairError)` - If the reader cannot be read or JSON cannot be repaired
pub fn load<R: Read>(mut reader: R, options: &RepairOptions) -> Result<Value, JsonRepairError> {
    let mut content = String::new();
    reader.read_to_string(&mut content)?;
    loads(&content, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_basic_repair() {
        let options = RepairOptions::default();

        // Missing quotes around keys
        let result = repair_json(r#"{name: "John", age: 30}"#, &options).unwrap();
        assert_eq!(result, r#"{"age":30,"name":"John"}"#);

        // Single quotes
        let result = repair_json(r#"{'name': 'John', 'age': 30}"#, &options).unwrap();
        assert_eq!(result, r#"{"age":30,"name":"John"}"#);

        // Trailing comma
        let result = repair_json(r#"{"name": "John", "age": 30,}"#, &options).unwrap();
        assert_eq!(result, r#"{"age":30,"name":"John"}"#);
    }

    #[test]
    fn test_incomplete_json() {
        let options = RepairOptions::default();

        // Missing closing brace
        let result = repair_json(r#"{"name": "John", "age": 30"#, &options).unwrap();
        assert_eq!(result, r#"{"age":30,"name":"John"}"#);

        // Missing closing bracket
        let result = repair_json(r#"["a", "b", "c""#, &options).unwrap();
        assert_eq!(result, r#"["a","b","c"]"#);

        // Completely incomplete object
        let result = repair_json(r#"{"name": "John"#, &options).unwrap();
        assert_eq!(result, r#"{"name":"John"}"#);
    }

    #[test]
    fn test_literals() {
        let options = RepairOptions::default();

        // Boolean values
        let result = repair_json(r#"{"active": true, "inactive": false}"#, &options).unwrap();
        assert_eq!(result, r#"{"active":true,"inactive":false}"#);

        // Null values
        let result = repair_json(r#"{"value": null}"#, &options).unwrap();
        assert_eq!(result, r#"{"value":null}"#);

        // Alternative null representations
        let result = repair_json(r#"{"value": None}"#, &options).unwrap();
        assert_eq!(result, r#"{"value":null}"#);

        let result = repair_json(r#"{"value": undefined}"#, &options).unwrap();
        assert_eq!(result, r#"{"value":null}"#);
    }

    #[test]
    fn test_arrays() {
        let options = RepairOptions::default();

        // Mixed array
        let result = repair_json(r#"[1, "two", true, null]"#, &options).unwrap();
        assert_eq!(result, r#"[1,"two",true,null]"#);

        // Array with trailing comma
        let result = repair_json(r#"[1, 2, 3,]"#, &options).unwrap();
        assert_eq!(result, r#"[1,2,3]"#);

        // Array with single trailing comma only
        let result = repair_json(r#"[1, 2, 3]"#, &options).unwrap();
        assert_eq!(result, r#"[1,2,3]"#);
    }

    #[test]
    fn test_array_with_colon_does_not_loop() {
        let options = RepairOptions::default();

        let result = repair_json(r#"{"filepath":"a","edits":[old_string":"x"#, &options).unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.is_object());
    }

    #[test]
    fn test_array_with_object_closer_does_not_loop() {
        let options = RepairOptions::default();

        let result = repair_json(r#"{"items":[}"#, &options).unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.is_object());
    }

    #[test]
    fn test_numbers() {
        let options = RepairOptions::default();

        // Test simple numbers individually
        let result = repair_json(r#"{"int": 42}"#, &options).unwrap();
        assert_eq!(result, r#"{"int":42}"#);

        let result = repair_json(r#"{"float": 3.14}"#, &options).unwrap();
        assert_eq!(result, r#"{"float":3.14}"#);

        let result = repair_json(r#"{"negative": -10}"#, &options).unwrap();
        assert_eq!(result, r#"{"negative":-10}"#);

        // Invalid numbers should become strings - test with simpler case
        let result = repair_json(r#"{invalid: abc123}"#, &options).unwrap();
        assert_eq!(result, r#"{"invalid":"abc123"}"#);
    }

    #[test]
    fn test_loads_function() {
        let options = RepairOptions::default();

        let value = loads(r#"{name: "John", age: 30}"#, &options).unwrap();

        if let Value::Object(obj) = value {
            assert_eq!(obj["name"], "John");
            assert_eq!(obj["age"], 30);
        } else {
            panic!("Expected object");
        }
    }

    #[test]
    fn test_unicode_preservation() {
        let mut options = RepairOptions::default();
        options.ensure_ascii = false;

        let result = repair_json(r#"{"chinese": "统一码"}"#, &options).unwrap();
        assert!(result.contains("统一码"));
    }

    #[test]
    fn test_comments_removal() {
        let options = RepairOptions::default();

        // Line comments
        let result = repair_json(
            r#"{"name": "John", // this is a comment
        "age": 30}"#,
            &options,
        )
        .unwrap();
        assert_eq!(result, r#"{"age":30,"name":"John"}"#);

        // Block comments
        let result = repair_json(r#"{"name": "John", /* comment */ "age": 30}"#, &options).unwrap();
        assert_eq!(result, r#"{"age":30,"name":"John"}"#);
    }

    #[test]
    fn test_empty_input() {
        let options = RepairOptions::default();

        let result = repair_json("", &options).unwrap();
        assert_eq!(result, "{}");

        let result = repair_json("   ", &options).unwrap();
        assert_eq!(result, "{}");
    }

    #[test]
    fn test_nested_structures() {
        let options = RepairOptions::default();

        let result = repair_json(
            r#"{users: [{name: "John", active: true}, {name: "Jane", active: false}]}"#,
            &options,
        )
        .unwrap();
        let expected =
            r#"{"users":[{"active":true,"name":"John"},{"active":false,"name":"Jane"}]}"#;
        assert_eq!(result, expected);
    }

    #[test]
    fn test_llm_common_errors() {
        let options = RepairOptions::default();

        // LLM often adds markdown code blocks
        let result = repair_json(
            r#"```json
        {
            "name": "John",
            "age": 30
        }
        ```"#,
            &options,
        )
        .unwrap();
        assert_eq!(result, r#"{"age":30,"name":"John"}"#);

        // LLM sometimes adds explanatory text
        let result =
            repair_json(r#"Here's the JSON: {"name": "John", "age": 30}"#, &options).unwrap();
        assert_eq!(result, r#"{"age":30,"name":"John"}"#);

        // Mixed quotes and missing commas
        let result = repair_json(r#"{"name": 'John' "age": 30}"#, &options).unwrap();
        assert_eq!(result, r#"{"age":30,"name":"John"}"#);
    }

    #[test]
    fn test_string_escaping() {
        let options = RepairOptions::default();

        // String with embedded quotes
        let result = repair_json(r#"{"message": "He said \"Hello\""}"#, &options).unwrap();
        assert_eq!(result, r#"{"message":"He said \"Hello\""}"#);

        // Unescaped quotes in single-quoted strings
        let result = repair_json(r#"{'message': 'He said "Hello"'}"#, &options).unwrap();
        assert_eq!(result, r#"{"message":"He said \"Hello\""}"#);
    }

    #[test]
    fn test_file_operations() {
        let options = RepairOptions::default();

        // Test from_file function
        let mut temp_file = NamedTempFile::new().unwrap();
        let broken_json = r#"{name: "John", age: 30}"#; // Removed trailing comma
        temp_file.write_all(broken_json.as_bytes()).unwrap();

        let value = from_file(temp_file.path(), &options).unwrap();

        if let Value::Object(obj) = value {
            assert_eq!(obj["name"], "John");
            assert_eq!(obj["age"], 30);
        } else {
            panic!("Expected object");
        }
    }

    #[test]
    fn test_skip_validation() {
        let mut options = RepairOptions::default();
        options.skip_json_loads = true;

        // This should work even if the result isn't valid JSON
        let result = repair_json(r#"{name: "John"}"#, &options);
        assert!(result.is_ok());
    }

    #[test]
    fn test_extremely_broken_json() {
        let options = RepairOptions::default();

        // Multiple issues at once
        let result = repair_json(
            r#"
        {
            name: 'John',
            age: 30,
            hobbies: ['reading', 'swimming'],
            address: {
                street: "123 Main St",
                city: 'New York'
            },
            active: true,
            balance: null
        }"#,
            &options,
        )
        .unwrap(); // Removed trailing comma from hobbies array

        // Verify it's valid JSON
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.is_object());
    }

    #[test]
    fn test_scientific_notation() {
        let options = RepairOptions::default();

        let result = repair_json(r#"{"small": 1e-10, "large": 1.23e+15}"#, &options).unwrap();
        assert_eq!(result, r#"{"large":1230000000000000.0,"small":1e-10}"#);
    }

    #[test]
    fn test_performance_options() {
        let mut options = RepairOptions::default();
        options.skip_json_loads = true;

        let start = std::time::Instant::now();
        let _result = repair_json(r#"{name: "John", age: 30}"#, &options).unwrap();
        let duration = start.elapsed();

        // Just ensure it completes quickly
        assert!(duration.as_millis() < 100);
    }
}
