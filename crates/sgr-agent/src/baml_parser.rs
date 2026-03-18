//! Lightweight BAML parser — extracts classes, enums, functions from `.baml` files.
//!
//! NOT a full BAML compiler. Parses just enough to generate:
//! - Rust structs with `#[derive(JsonSchema, Serialize, Deserialize)]`
//! - Tool definitions with prompts
//! - Enum types with string variants
//!
//! Source of truth stays in `.baml` files.

use std::path::Path;

/// A parsed BAML class (→ Rust struct).
#[derive(Debug, Clone)]
pub struct BamlClass {
    pub name: String,
    pub fields: Vec<BamlField>,
    pub description: Option<String>,
}

/// A field within a BAML class.
#[derive(Debug, Clone)]
pub struct BamlField {
    pub name: String,
    pub ty: BamlType,
    pub description: Option<String>,
    /// Fixed string value (e.g. `task "analysis_operation"`).
    pub fixed_value: Option<String>,
}

/// BAML type representation.
#[derive(Debug, Clone)]
pub enum BamlType {
    String,
    Int,
    Float,
    Bool,
    /// A string enum: `"trim" | "keep" | "highlight"`
    StringEnum(Vec<String>),
    /// Reference to another class.
    Ref(String),
    /// Optional type (T | null).
    Optional(Box<BamlType>),
    /// Array type.
    Array(Box<BamlType>),
    /// Union of class references (for next_actions).
    Union(Vec<String>),
    /// Image type (special BAML type).
    Image,
}

/// A parsed BAML function (→ tool definition + prompt).
#[derive(Debug, Clone)]
pub struct BamlFunction {
    pub name: String,
    pub params: Vec<(String, BamlType)>,
    pub return_type: String,
    pub client: String,
    pub prompt: String,
}

/// All parsed items from BAML source files.
#[derive(Debug, Clone, Default)]
pub struct BamlModule {
    pub classes: Vec<BamlClass>,
    pub functions: Vec<BamlFunction>,
}

impl BamlModule {
    /// Parse all `.baml` files in a directory.
    pub fn parse_dir(dir: &Path) -> Result<Self, String> {
        let mut module = BamlModule::default();

        let entries =
            std::fs::read_dir(dir).map_err(|e| format!("Cannot read {}: {}", dir.display(), e))?;

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "baml") {
                let source = std::fs::read_to_string(&path)
                    .map_err(|e| format!("Cannot read {}: {}", path.display(), e))?;
                module.parse_source(&source);
            }
        }

        Ok(module)
    }

    /// Parse a single BAML source string.
    pub fn parse_source(&mut self, source: &str) {
        let lines: Vec<&str> = source.lines().collect();
        let mut i = 0;

        while i < lines.len() {
            let line = lines[i].trim();

            // Skip comments and empty lines
            if line.is_empty() || line.starts_with("//") {
                i += 1;
                continue;
            }

            // Class definition
            if line.starts_with("class ")
                && let Some((class, consumed)) = parse_class(&lines[i..])
            {
                self.classes.push(class);
                i += consumed;
                continue;
            }

            // Function definition
            if line.starts_with("function ")
                && let Some((func, consumed)) = parse_function(&lines[i..])
            {
                self.functions.push(func);
                i += consumed;
                continue;
            }

            i += 1;
        }
    }

    /// Find a class by name.
    pub fn find_class(&self, name: &str) -> Option<&BamlClass> {
        self.classes.iter().find(|c| c.name == name)
    }

    /// Find a function by name.
    pub fn find_function(&self, name: &str) -> Option<&BamlFunction> {
        self.functions.iter().find(|f| f.name == name)
    }
}

// --- Parsers ---

fn parse_class(lines: &[&str]) -> Option<(BamlClass, usize)> {
    let header = lines[0].trim();
    let name = header
        .strip_prefix("class ")?
        .trim()
        .trim_end_matches('{')
        .trim()
        .to_string();

    let mut fields = Vec::new();
    let mut i = 1;

    while i < lines.len() {
        let line = lines[i].trim();
        i += 1;

        if line == "}" {
            break;
        }
        if line.is_empty() || line.starts_with("//") {
            continue;
        }

        if let Some(field) = parse_field(line) {
            fields.push(field);
        }
    }

    Some((
        BamlClass {
            name,
            fields,
            description: None,
        },
        i,
    ))
}

fn parse_field(line: &str) -> Option<BamlField> {
    // Examples:
    //   action "trim" | "keep" | "highlight" @description("...")
    //   input_path string | null @description("...")
    //   task "analysis_operation" @description("...") @stream.not_null
    //   target_seconds int @description("...")
    //   next_actions (Type1 | Type2)[] @description("...")

    let line = line.trim();

    // Extract description
    let description = extract_description(line);

    // Remove annotations (@description, @stream, etc.)
    let clean = remove_annotations(line);
    let clean = clean.trim();

    // Split into name and type
    let mut parts = clean.splitn(2, char::is_whitespace);
    let name = parts.next()?.trim().to_string();
    let type_str = parts.next()?.trim();

    // Check for fixed value: `task "analysis_operation"`
    if type_str.starts_with('"') && !type_str.contains('|') {
        let value = type_str.trim_matches('"').to_string();
        return Some(BamlField {
            name,
            ty: BamlType::String,
            description,
            fixed_value: Some(value),
        });
    }

    let ty = parse_type(type_str);

    Some(BamlField {
        name,
        ty,
        description,
        fixed_value: None,
    })
}

fn parse_type(s: &str) -> BamlType {
    let s = s.trim();

    // Array: T[] or (T)[]
    if s.ends_with("[]") {
        let inner = s.trim_end_matches("[]").trim();
        // Union array: (Type1 | Type2)[]
        if inner.starts_with('(') && inner.ends_with(')') {
            let inner_types = &inner[1..inner.len() - 1];
            let variants: Vec<String> = inner_types
                .split('|')
                .map(|v| v.trim().to_string())
                .collect();
            // Check if all variants are class references (start with uppercase)
            if variants
                .iter()
                .all(|v| v.starts_with(|c: char| c.is_uppercase()))
            {
                return BamlType::Array(Box::new(BamlType::Union(variants)));
            }
        }
        let inner_type = parse_type(inner);
        return BamlType::Array(Box::new(inner_type));
    }

    // Nullable: T | null
    if s.contains("| null") || s.contains("null |") {
        let base = s
            .replace("| null", "")
            .replace("null |", "")
            .trim()
            .to_string();
        return BamlType::Optional(Box::new(parse_type(&base)));
    }

    // String enum: "a" | "b" | "c"
    if s.contains('"') && s.contains('|') {
        let variants: Vec<String> = s
            .split('|')
            .map(|v| v.trim().trim_matches('"').to_string())
            .filter(|v| !v.is_empty())
            .collect();
        return BamlType::StringEnum(variants);
    }

    // Union of types (without quotes): Type1 | Type2
    if s.contains('|') {
        let variants: Vec<String> = s.split('|').map(|v| v.trim().to_string()).collect();
        if variants
            .iter()
            .all(|v| v.starts_with(|c: char| c.is_uppercase()))
        {
            return BamlType::Union(variants);
        }
    }

    // Primitives
    match s {
        "string" => BamlType::String,
        "int" => BamlType::Int,
        "float" => BamlType::Float,
        "bool" => BamlType::Bool,
        "image" => BamlType::Image,
        _ => {
            // Class reference
            if s.starts_with(|c: char| c.is_uppercase()) {
                BamlType::Ref(s.to_string())
            } else {
                BamlType::String // fallback
            }
        }
    }
}

fn extract_description(line: &str) -> Option<String> {
    let marker = "@description(\"";
    if let Some(start) = line.find(marker) {
        let rest = &line[start + marker.len()..];
        if let Some(end) = rest.find("\")") {
            return Some(rest[..end].to_string());
        }
    }
    None
}

fn remove_annotations(line: &str) -> String {
    let mut result = line.to_string();
    // Remove @description("...")
    while let Some(start) = result.find("@description(\"") {
        if let Some(end) = result[start..].find("\")") {
            result = format!("{}{}", &result[..start], &result[start + end + 2..]);
        } else {
            break;
        }
    }
    // Remove @stream.not_null and other @annotations
    while let Some(start) = result.find('@') {
        let rest = &result[start + 1..];
        let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        result = format!("{}{}", &result[..start], &result[start + 1 + end..]);
    }
    result
}

fn parse_function(lines: &[&str]) -> Option<(BamlFunction, usize)> {
    let header = lines[0].trim();

    // function Name(param: Type, ...) -> ReturnType {
    let rest = header.strip_prefix("function ")?;

    // Extract name
    let paren_start = rest.find('(')?;
    let name = rest[..paren_start].trim().to_string();

    // Extract params
    let paren_end = rest.find(')')?;
    let params_str = &rest[paren_start + 1..paren_end];
    let params: Vec<(String, BamlType)> = if params_str.trim().is_empty() {
        vec![]
    } else {
        params_str
            .split(',')
            .filter_map(|p| {
                let p = p.trim();
                let mut parts = p.splitn(2, ':');
                let pname = parts.next()?.trim().to_string();
                let ptype = parse_type(parts.next()?.trim());
                Some((pname, ptype))
            })
            .collect()
    };

    // Extract return type
    let arrow = rest.find("->")?;
    let return_rest = rest[arrow + 2..].trim();
    let return_type = return_rest.trim_end_matches('{').trim().to_string();

    // Extract body (client + prompt)
    let mut client = String::new();
    let mut prompt_lines = Vec::new();
    let mut in_prompt = false;
    let mut i = 1;

    while i < lines.len() {
        let line = lines[i].trim();
        i += 1;

        if line == "}" && !in_prompt {
            break;
        }

        if line.starts_with("client ") {
            client = line
                .strip_prefix("client ")
                .unwrap_or("")
                .trim()
                .trim_matches('"')
                .to_string();
            continue;
        }

        if line.starts_with("prompt #\"") {
            in_prompt = true;
            // Content after prompt #"
            let after = line.strip_prefix("prompt #\"").unwrap_or("");
            if !after.is_empty() {
                prompt_lines.push(after.to_string());
            }
            continue;
        }

        if in_prompt {
            if line.contains("\"#") {
                let before = line.trim_end_matches("\"#").trim_end();
                if !before.is_empty() {
                    prompt_lines.push(before.to_string());
                }
                in_prompt = false;
                continue;
            }
            prompt_lines.push(lines[i - 1].to_string());
        }
    }

    Some((
        BamlFunction {
            name,
            params,
            return_type,
            client,
            prompt: prompt_lines.join("\n"),
        },
        i,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_class() {
        let source = r#"
class CutDecision {
  action "trim" | "keep" | "highlight" @description("Editing action")
  reason string @description("Short reasoning")
}
"#;
        let mut module = BamlModule::default();
        module.parse_source(source);

        assert_eq!(module.classes.len(), 1);
        let cls = &module.classes[0];
        assert_eq!(cls.name, "CutDecision");
        assert_eq!(cls.fields.len(), 2);

        let action = &cls.fields[0];
        assert_eq!(action.name, "action");
        match &action.ty {
            BamlType::StringEnum(variants) => {
                assert_eq!(variants, &["trim", "keep", "highlight"]);
            }
            other => panic!("Expected StringEnum, got {:?}", other),
        }
        assert_eq!(action.description.as_deref(), Some("Editing action"));
    }

    #[test]
    fn parses_class_with_optional_and_array() {
        let source = r#"
class FfmpegTask {
  task "ffmpeg_operation" @description("FFmpeg ops") @stream.not_null
  operation "convert" | "trim" | "concat"
  input_path string | null
  custom_args string[] | null
  overwrite bool | null
}
"#;
        let mut module = BamlModule::default();
        module.parse_source(source);

        let cls = &module.classes[0];
        assert_eq!(cls.name, "FfmpegTask");

        // task has fixed value
        assert_eq!(
            cls.fields[0].fixed_value.as_deref(),
            Some("ffmpeg_operation")
        );

        // input_path is Optional<String>
        assert!(matches!(cls.fields[2].ty, BamlType::Optional(_)));

        // custom_args is Optional<Array<String>>
        match &cls.fields[3].ty {
            BamlType::Optional(inner) => {
                assert!(matches!(inner.as_ref(), BamlType::Array(_)));
            }
            other => panic!("Expected Optional(Array), got {:?}", other),
        }
    }

    #[test]
    fn parses_union_array() {
        let source = r#"
class MontageAgentNextStep {
  intent "display" | "montage"
  next_actions (AnalysisTask | FfmpegTask | ProjectTask)[] @description("Tools to execute")
}
"#;
        let mut module = BamlModule::default();
        module.parse_source(source);

        let cls = &module.classes[0];
        let actions_field = &cls.fields[1];
        match &actions_field.ty {
            BamlType::Array(inner) => match inner.as_ref() {
                BamlType::Union(variants) => {
                    assert_eq!(variants, &["AnalysisTask", "FfmpegTask", "ProjectTask"]);
                }
                other => panic!("Expected Union inside Array, got {:?}", other),
            },
            other => panic!("Expected Array, got {:?}", other),
        }
    }

    #[test]
    fn parses_function() {
        let source = r##"
function AnalyzeSegmentSgr(genre: string, scene: string) -> SgrSegmentDecision {
  client AgentFallback
  prompt #"
    You are a video editor.
    Genre: {{ genre }}
    {{ ctx.output_format }}
  "#
}
"##;
        let mut module = BamlModule::default();
        module.parse_source(source);

        assert_eq!(module.functions.len(), 1);
        let func = &module.functions[0];
        assert_eq!(func.name, "AnalyzeSegmentSgr");
        assert_eq!(func.params.len(), 2);
        assert_eq!(func.return_type, "SgrSegmentDecision");
        assert_eq!(func.client, "AgentFallback");
        assert!(func.prompt.contains("video editor"));
    }

    #[test]
    fn parses_real_montage_baml() {
        let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.pop(); // sgr-agent
        path.pop(); // crates
        path.pop(); // rust-code
        path.push("startups");
        path.push("active");
        path.push("video-analyzer");
        path.push("crates");
        path.push("va-agent");
        path.push("baml_src");
        path.push("montage");
        path.set_extension("baml");
        if !path.exists() {
            eprintln!("Skipping: montage.baml not found at {}", path.display());
            return;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        let mut module = BamlModule::default();
        module.parse_source(&source);

        // Should find all major classes
        assert!(module.find_class("CutDecision").is_some());
        assert!(module.find_class("MontageAgentNextStep").is_some());
        assert!(module.find_class("AnalysisTask").is_some());
        assert!(module.find_class("FfmpegTask").is_some());
        assert!(module.find_class("ProjectTask").is_some());
        assert!(module.find_class("ReportTaskCompletion").is_some());

        // Should find major functions
        assert!(module.find_function("AnalyzeSegmentSgr").is_some());
        assert!(module.find_function("DecideMontageNextStepSgr").is_some());
        assert!(module.find_function("SummarizeTranscriptSgr").is_some());

        // MontageAgentNextStep should have union array for next_actions
        let step = module.find_class("MontageAgentNextStep").unwrap();
        let actions = step
            .fields
            .iter()
            .find(|f| f.name == "next_actions")
            .unwrap();
        match &actions.ty {
            BamlType::Array(inner) => match inner.as_ref() {
                BamlType::Union(variants) => {
                    assert!(variants.contains(&"AnalysisTask".to_string()));
                    assert!(variants.contains(&"FfmpegTask".to_string()));
                    assert!(
                        variants.len() >= 10,
                        "Should have 16 tool types, got {}",
                        variants.len()
                    );
                }
                other => panic!("Expected Union, got {:?}", other),
            },
            other => panic!("Expected Array, got {:?}", other),
        }
    }
}
