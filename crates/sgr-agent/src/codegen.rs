//! Code generator: BAML AST → Rust source code with schemars + serde derives.
//!
//! Generates:
//! - Rust structs for each BAML class
//! - Rust enums for string unions
//! - Tool registrations for BAML classes with `task` field (= tool definitions)
//! - Prompt constants for BAML functions

use crate::baml_parser::*;

/// Generate Rust source code from parsed BAML module.
pub fn generate(module: &BamlModule) -> String {
    let mut out = String::new();

    // Header
    out.push_str("//! Auto-generated from .baml files by sgr-agent codegen.\n");
    out.push_str("//! Do not edit manually — edit the .baml source and re-run.\n\n");
    out.push_str("#![allow(dead_code, clippy::derivable_impls)]\n\n");
    out.push_str("use serde::{Deserialize, Serialize};\n");
    out.push_str("use schemars::JsonSchema;\n\n");

    // Collect all inline string enums that need separate types
    let mut enum_map: Vec<(String, Vec<String>)> = Vec::new();

    // Generate structs
    for class in &module.classes {
        generate_struct(&mut out, class, module, &mut enum_map);
    }

    // Generate collected enums
    for (name, variants) in &enum_map {
        generate_string_enum(&mut out, name, variants);
    }

    // Generate tool registry
    generate_tool_registry(&mut out, module);

    // Generate prompt constants
    generate_prompts(&mut out, module);

    out
}

fn generate_struct(
    out: &mut String,
    class: &BamlClass,
    module: &BamlModule,
    enum_map: &mut Vec<(String, Vec<String>)>,
) {
    // Doc comment
    if let Some(desc) = &class.description {
        out.push_str(&format!("/// {}\n", desc));
    }

    out.push_str("#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]\n");
    out.push_str(&format!("pub struct {} {{\n", class.name));

    for field in &class.fields {
        // Doc comment
        if let Some(desc) = &field.description {
            out.push_str(&format!("    /// {}\n", desc));
        }

        // Fixed value fields become constants (skip in struct, add as default)
        if let Some(fixed) = &field.fixed_value {
            out.push_str(&format!(
                "    /// Fixed value: \"{}\"\n",
                fixed
            ));
            out.push_str(&format!(
                "    #[serde(default = \"default_{}__{}\")]\n",
                snake_case(&class.name),
                field.name
            ));
            out.push_str(&format!("    pub {}: String,\n", field.name));
            continue;
        }

        let rust_type = baml_type_to_rust(&field.ty, &class.name, &field.name, module, enum_map);

        // Optional fields get serde skip_serializing_if
        if matches!(&field.ty, BamlType::Optional(_)) {
            out.push_str("    #[serde(skip_serializing_if = \"Option::is_none\")]\n");
        }

        out.push_str(&format!("    pub {}: {},\n", field.name, rust_type));
    }

    out.push_str("}\n\n");

    // Generate default functions for fixed-value fields
    for field in &class.fields {
        if let Some(fixed) = &field.fixed_value {
            out.push_str(&format!(
                "fn default_{}__{}() -> String {{ \"{}\".to_string() }}\n",
                snake_case(&class.name),
                field.name,
                fixed
            ));
        }
    }
    // Extra newline after defaults
    if class.fields.iter().any(|f| f.fixed_value.is_some()) {
        out.push('\n');
    }
}

fn generate_string_enum(out: &mut String, name: &str, variants: &[String]) {
    out.push_str("#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]\n");
    out.push_str(&format!("pub enum {} {{\n", name));
    for variant in variants {
        let rust_variant = pascal_case(variant);
        out.push_str(&format!("    #[serde(rename = \"{}\")]\n", variant));
        out.push_str(&format!("    {},\n", rust_variant));
    }
    out.push_str("}\n\n");
}

fn generate_tool_registry(out: &mut String, module: &BamlModule) {
    // Collect classes that have a `task` field with a fixed value — these are tools
    let tool_classes: Vec<&BamlClass> = module
        .classes
        .iter()
        .filter(|c| c.fields.iter().any(|f| f.name == "task" && f.fixed_value.is_some()))
        .collect();

    if tool_classes.is_empty() {
        return;
    }

    out.push_str("// --- Tool Registry ---\n\n");
    out.push_str("use crate::tool::ToolDef;\n\n");
    out.push_str("/// All tools extracted from BAML definitions.\n");
    out.push_str("pub fn all_tools() -> Vec<ToolDef> {\n");
    out.push_str("    vec![\n");

    for class in &tool_classes {
        let task_field = class
            .fields
            .iter()
            .find(|f| f.name == "task")
            .unwrap();
        let tool_name = task_field.fixed_value.as_deref().unwrap();
        let description = task_field
            .description
            .as_deref()
            .unwrap_or(&class.name);

        out.push_str(&format!(
            "        crate::tool::tool::<{}>(\"{}\", \"{}\"),\n",
            class.name,
            tool_name,
            escape_string(description),
        ));
    }

    out.push_str("    ]\n");
    out.push_str("}\n\n");

    // Generate ActionUnion enum for dispatch
    out.push_str("/// Union of all tool types (for dispatching tool calls).\n");
    out.push_str("#[derive(Debug, Clone, Serialize, Deserialize)]\n");
    out.push_str("#[serde(tag = \"task\")]\n");
    out.push_str("pub enum ActionUnion {\n");

    for class in &tool_classes {
        let task_field = class.fields.iter().find(|f| f.name == "task").unwrap();
        let tool_name = task_field.fixed_value.as_deref().unwrap();
        out.push_str(&format!(
            "    #[serde(rename = \"{}\")]\n",
            tool_name
        ));
        out.push_str(&format!("    {}({}),\n", class.name, class.name));
    }

    out.push_str("}\n\n");
}

fn generate_prompts(out: &mut String, module: &BamlModule) {
    if module.functions.is_empty() {
        return;
    }

    out.push_str("// --- Prompt Constants ---\n\n");

    for func in &module.functions {
        let const_name = screaming_snake_case(&func.name);
        // Escape the prompt for a raw string
        out.push_str(&format!(
            "pub const {}_PROMPT: &str = r##\"\n{}\"##;\n\n",
            const_name,
            func.prompt.trim(),
        ));
    }
}

// --- Type conversion ---

fn baml_type_to_rust(
    ty: &BamlType,
    class_name: &str,
    field_name: &str,
    module: &BamlModule,
    enum_map: &mut Vec<(String, Vec<String>)>,
) -> String {
    match ty {
        BamlType::String => "String".to_string(),
        BamlType::Int => "i64".to_string(),
        BamlType::Float => "f64".to_string(),
        BamlType::Bool => "bool".to_string(),
        BamlType::Image => "String".to_string(), // base64 or URL
        BamlType::Ref(name) => {
            if module.find_class(name).is_some() {
                name.clone()
            } else {
                // Might be an enum we haven't seen — treat as String
                "String".to_string()
            }
        }
        BamlType::Optional(inner) => {
            let inner_rust = baml_type_to_rust(inner, class_name, field_name, module, enum_map);
            format!("Option<{}>", inner_rust)
        }
        BamlType::Array(inner) => {
            let inner_rust = baml_type_to_rust(inner, class_name, field_name, module, enum_map);
            format!("Vec<{}>", inner_rust)
        }
        BamlType::StringEnum(variants) => {
            // Create a named enum type
            let enum_name = format!(
                "{}{}",
                class_name,
                pascal_case(field_name)
            );
            if !enum_map.iter().any(|(n, _)| n == &enum_name) {
                enum_map.push((enum_name.clone(), variants.clone()));
            }
            enum_name
        }
        BamlType::Union(variants) => {
            // For now, use serde_json::Value for complex unions
            // Could generate a proper enum with #[serde(untagged)]
            if variants.len() <= 4 {
                // Small union → generate enum
                let enum_name = format!(
                    "{}{}",
                    class_name,
                    pascal_case(field_name)
                );
                if !enum_map.iter().any(|(n, _)| n == &enum_name) {
                    // This is a class union, not string enum — skip for now
                    // Would need #[serde(untagged)] enum
                }
            }
            "serde_json::Value".to_string()
        }
    }
}

// --- String helpers ---

fn snake_case(s: &str) -> String {
    let mut result = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                result.push('_');
            }
            result.push(c.to_ascii_lowercase());
        } else {
            result.push(c);
        }
    }
    result
}

fn pascal_case(s: &str) -> String {
    s.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

fn screaming_snake_case(s: &str) -> String {
    snake_case(s).to_uppercase()
}

fn escape_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_from_simple_baml() {
        let source = r#"
class CutDecision {
  action "trim" | "keep" | "highlight" @description("Editing action")
  reason string @description("Short reasoning")
}
"#;
        let mut module = BamlModule::default();
        module.parse_source(source);

        let code = generate(&module);
        assert!(code.contains("pub struct CutDecision"));
        assert!(code.contains("pub action: CutDecisionAction"));
        assert!(code.contains("pub reason: String"));
        assert!(code.contains("pub enum CutDecisionAction"));
        assert!(code.contains("#[serde(rename = \"trim\")]"));
    }

    #[test]
    fn generates_tools_from_baml() {
        let source = r#"
class FfmpegTask {
  task "ffmpeg_operation" @description("FFmpeg operations") @stream.not_null
  operation "convert" | "trim"
  input_path string | null
}
"#;
        let mut module = BamlModule::default();
        module.parse_source(source);

        let code = generate(&module);
        assert!(code.contains("pub fn all_tools()"));
        assert!(code.contains("\"ffmpeg_operation\""));
        assert!(code.contains("pub enum ActionUnion"));
        assert!(code.contains("FfmpegTask(FfmpegTask)"));
    }

    #[test]
    fn generates_from_real_montage_baml() {
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

        let code = generate(&module);

        // Should have all major structs
        assert!(code.contains("pub struct MontageAgentNextStep"));
        assert!(code.contains("pub struct AnalysisTask"));
        assert!(code.contains("pub struct FfmpegTask"));
        assert!(code.contains("pub struct ProjectTask"));

        // Should have tool registry
        assert!(code.contains("pub fn all_tools()"));
        assert!(code.contains("\"analysis_operation\""));
        assert!(code.contains("\"ffmpeg_operation\""));
        assert!(code.contains("\"project_operation\""));

        // Should have prompts
        assert!(code.contains("DECIDE_MONTAGE_NEXT_STEP_SGR_PROMPT"));
        assert!(code.contains("ANALYZE_SEGMENT_SGR_PROMPT"));
    }
}
