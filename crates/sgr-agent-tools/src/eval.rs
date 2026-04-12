//! EvalTool — execute JavaScript with workspace file access (Boa JS engine).
//!
//! Requires the `eval` feature flag (boa_engine is ~5MB).

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use sgr_agent_core::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent_core::context::AgentContext;
use sgr_agent_core::schema::json_schema_for;

use crate::backend::FileBackend;

pub struct EvalTool<B: FileBackend>(pub Arc<B>);

#[derive(Deserialize, JsonSchema)]
struct EvalArgs {
    /// JavaScript code to execute. Globals: file_0..file_N (file contents), file_paths (array of paths),
    /// workspace_date (YYYY-MM-DD string). Use JSON.parse(file_0) for JSON.
    /// Last expression = output.
    code: String,
    /// File paths to pre-read. Supports glob: "projects/*/README.MD" expands to all matching.
    /// Contents available as file_0, file_1, etc. Paths available as file_paths array.
    #[serde(default)]
    files: Vec<String>,
}

/// Expand glob patterns and pre-read files.
async fn resolve_files<B: FileBackend>(
    backend: &B,
    patterns: &[String],
) -> (Vec<String>, Vec<String>) {
    let mut file_contents = Vec::new();
    let mut file_paths = Vec::new();

    for path in patterns {
        if path.contains('*') {
            let parts: Vec<&str> = path.splitn(2, '*').collect();
            let parent = parts[0].trim_end_matches('/');
            let suffix = parts
                .get(1)
                .map(|s| s.trim_start_matches('/'))
                .unwrap_or("");
            if let Ok(listing) = backend.list(parent).await {
                for line in listing.lines().skip(1) {
                    let name = line.trim().trim_end_matches('/');
                    if name.is_empty() {
                        continue;
                    }
                    let full = if suffix.is_empty() {
                        format!("{}/{}", parent, name)
                    } else {
                        format!("{}/{}/{}", parent, name, suffix)
                    };
                    if let Ok(content) = backend.read(&full, false, 0, 0).await {
                        let clean = strip_pcm_header(content);
                        file_paths.push(full);
                        file_contents.push(clean);
                    }
                }
            }
        } else {
            match backend.read(path, false, 0, 0).await {
                Ok(content) => {
                    let clean = strip_pcm_header(content);
                    file_paths.push(path.clone());
                    file_contents.push(clean);
                }
                Err(e) => {
                    file_paths.push(path.clone());
                    file_contents.push(format!("(read error: {e})"));
                }
            }
        }
    }

    (file_paths, file_contents)
}

fn strip_pcm_header(content: String) -> String {
    if content.starts_with("$ ") {
        content
            .find('\n')
            .map(|j| content[j + 1..].to_string())
            .unwrap_or(content)
    } else {
        content
    }
}

/// Extract YYYY-MM-DD from context output.
async fn workspace_date<B: FileBackend>(backend: &B) -> String {
    backend
        .context()
        .await
        .ok()
        .and_then(|ctx| {
            let re = regex::Regex::new(r"\d{4}-\d{2}-\d{2}").ok()?;
            let cleaned = ctx.replace('"', "");
            re.find(&cleaned).map(|m| m.as_str().to_string())
        })
        .unwrap_or_else(|| "2026-01-01".to_string())
}

/// Run JavaScript code in Boa engine with injected file globals.
fn run_js(
    code: &str,
    file_paths: Vec<String>,
    file_contents: Vec<String>,
    ws_date: String,
) -> String {
    use boa_engine::{Context, JsValue, Source, js_string};

    let mut ctx = Context::default();

    // Inject file contents as globals
    for (i, content) in file_contents.iter().enumerate() {
        let name = format!("file_{i}");
        let _ = ctx.global_object().set(
            boa_engine::JsString::from(name.as_str()),
            JsValue::from(js_string!(content.as_str())),
            true,
            &mut ctx,
        );
    }

    // Inject file_paths array
    {
        let arr_code = format!(
            "[{}]",
            file_paths
                .iter()
                .map(|p| format!("\"{}\"", p.replace('\\', "\\\\").replace('"', "\\\"")))
                .collect::<Vec<_>>()
                .join(",")
        );
        let _ = ctx.eval(Source::from_bytes(&format!("var file_paths = {arr_code}")));
    }

    // Inject workspace_date
    let _ = ctx.global_object().set(
        js_string!("workspace_date"),
        JsValue::from(js_string!(ws_date.as_str())),
        true,
        &mut ctx,
    );

    // Execute JS — auto-stringify objects/arrays
    match ctx.eval(Source::from_bytes(code)) {
        Ok(val) => {
            if val.is_object() {
                let wrapped = format!(
                    "var __result = ({code}); typeof __result === 'object' ? JSON.stringify(__result, null, 2) : String(__result)"
                );
                ctx.eval(Source::from_bytes(&wrapped))
                    .and_then(|v| v.to_string(&mut ctx))
                    .map(|s| s.to_std_string_escaped())
                    .unwrap_or_else(|_| {
                        val.to_string(&mut ctx)
                            .map(|s| s.to_std_string_escaped())
                            .unwrap_or_else(|e| format!("JS error: {e}"))
                    })
            } else {
                val.to_string(&mut ctx)
                    .map(|s| s.to_std_string_escaped())
                    .unwrap_or_else(|e| format!("JS error: {e}"))
            }
        }
        Err(e) => format!("JS error: {e}"),
    }
}

#[async_trait::async_trait]
impl<B: FileBackend> Tool for EvalTool<B> {
    fn name(&self) -> &str {
        "eval"
    }
    fn description(&self) -> &str {
        "Execute JavaScript with workspace file access. Supports glob patterns in files. \
         Globals: file_0..N (contents), file_paths (array of resolved paths), workspace_date. \
         Use for: batch JSON processing, filtering, date math, counting across files. \
         Glob example: files: ['projects/*/README.MD'] reads ALL project READMEs."
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn parameters_schema(&self) -> Value {
        json_schema_for::<EvalArgs>()
    }
    async fn execute(&self, args: Value, ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        self.execute_readonly(args, ctx).await
    }
    async fn execute_readonly(
        &self,
        args: Value,
        _ctx: &AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let a: EvalArgs = parse_args(&args)?;
        let (file_paths, file_contents) = resolve_files(&*self.0, &a.files).await;
        let ws_date = workspace_date(&*self.0).await;
        let code = a.code.clone();

        let result =
            tokio::task::spawn_blocking(move || run_js(&code, file_paths, file_contents, ws_date))
                .await
                .unwrap_or_else(|e| format!("Eval failed: {e}"));

        Ok(ToolOutput::text(result))
    }
}

#[cfg(test)]
mod tests {
    use super::run_js;

    #[test]
    fn eval_basic_math() {
        let result = run_js("2 + 2", vec![], vec![], "2026-01-01".into());
        assert_eq!(result, "4");
    }

    #[test]
    fn eval_string() {
        let result = run_js("'hello ' + 'world'", vec![], vec![], "2026-01-01".into());
        assert_eq!(result, "hello world");
    }

    #[test]
    fn eval_file_global() {
        let result = run_js(
            "file_0.length",
            vec!["test.txt".into()],
            vec!["hello".into()],
            "2026-01-01".into(),
        );
        assert_eq!(result, "5");
    }

    #[test]
    fn eval_workspace_date() {
        let result = run_js("workspace_date", vec![], vec![], "2026-04-12".into());
        assert_eq!(result, "2026-04-12");
    }

    #[test]
    fn eval_json_parse() {
        let result = run_js(
            "JSON.parse(file_0).name",
            vec!["data.json".into()],
            vec![r#"{"name":"test"}"#.into()],
            "2026-01-01".into(),
        );
        assert_eq!(result, "test");
    }

    #[test]
    fn eval_object_auto_stringify() {
        let result = run_js("({a: 1, b: 2})", vec![], vec![], "2026-01-01".into());
        assert!(result.contains("\"a\"") && result.contains("1"));
    }

    #[test]
    fn eval_sandbox_no_require() {
        let result = run_js("require('fs')", vec![], vec![], "2026-01-01".into());
        assert!(result.contains("error") || result.contains("Error"));
    }
}
