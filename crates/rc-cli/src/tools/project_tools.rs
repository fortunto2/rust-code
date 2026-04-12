//! ProjectMap and Dependencies tools.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sgr_agent::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent::context::AgentContext;
use std::path::Path;

// ---------------------------------------------------------------------------
// ProjectMap
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ProjectMapArgs {
    /// Optional path to scope the map.
    #[serde(default)]
    pub path: Option<String>,
}

pub struct ProjectMapTool;

#[async_trait::async_trait]
impl Tool for ProjectMapTool {
    fn name(&self) -> &str {
        "project_map"
    }
    fn description(&self) -> &str {
        "Generate a project structure map."
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn parameters_schema(&self) -> serde_json::Value {
        sgr_agent::schema::json_schema_for::<ProjectMapArgs>()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: ProjectMapArgs = parse_args(&args)?;
        let dir = args.path.as_deref().unwrap_or(".");
        let map = solograph::generate_repomap(Path::new(dir));
        Ok(ToolOutput::text(map))
    }
}

// ---------------------------------------------------------------------------
// Dependencies
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct DependenciesArgs {
    /// Optional path to check dependencies.
    #[serde(default)]
    pub path: Option<String>,
}

pub struct DependenciesTool;

#[async_trait::async_trait]
impl Tool for DependenciesTool {
    fn name(&self) -> &str {
        "dependencies"
    }
    fn description(&self) -> &str {
        "Analyze project dependencies."
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn parameters_schema(&self) -> serde_json::Value {
        sgr_agent::schema::json_schema_for::<DependenciesArgs>()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: DependenciesArgs = parse_args(&args)?;
        let manifest = if let Some(p) = &args.path {
            std::path::PathBuf::from(p)
        } else {
            ["Cargo.toml", "package.json", "pyproject.toml"]
                .iter()
                .map(std::path::PathBuf::from)
                .find(|p| p.exists())
                .unwrap_or_else(|| std::path::PathBuf::from("Cargo.toml"))
        };
        let deps = solograph::parse_deps(&manifest);
        if deps.is_empty() {
            Ok(ToolOutput::text(format!(
                "No dependencies found in {}",
                manifest.display()
            )))
        } else {
            let output = deps
                .iter()
                .map(|d| {
                    let kind = match d.kind {
                        solograph::DependencyKind::Dev => " [dev]",
                        solograph::DependencyKind::Build => " [build]",
                        solograph::DependencyKind::Normal => "",
                    };
                    format!("  {} = {}{}", d.name, d.version, kind)
                })
                .collect::<Vec<_>>()
                .join("\n");
            Ok(ToolOutput::text(format!(
                "Dependencies from {}:\n{}",
                manifest.display(),
                output
            )))
        }
    }
}
