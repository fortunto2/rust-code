//! Demo: agent with sgr-agent-tools (LocalFs backend).
//!
//! Shows how to build a coding agent using the standard tool set.
//!
//! Run: cargo run -p sgr-agent --features "agent,tools-all,tools-local-fs" --example agent_demo
//! Custom: cargo run -p sgr-agent --features "agent,tools-all,tools-local-fs" --example agent_demo -- "your prompt"

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use sgr_agent::agent_loop::{LoopConfig, LoopEvent, run_loop};
use sgr_agent::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent::agents::tool_calling::ToolCallingAgent;
use sgr_agent::context::AgentContext;
use sgr_agent::registry::ToolRegistry;
use sgr_agent::schema::json_schema_for;
use sgr_agent::tools::{
    ApplyPatchTool, DeleteTool, FindTool, ListTool, LocalFs, MkDirTool, MoveTool, ReadAllTool,
    ReadTool, SearchTool, ShellTool, TreeTool, WriteTool,
};
use sgr_agent::types::Message;
use sgr_agent::{Llm, LlmConfig};

// ─── Custom tools (not in sgr-agent-tools) ──────────────────────────────────

/// Git status tool
struct GitStatusTool;

#[derive(Deserialize, JsonSchema)]
struct GitStatusArgs {
    #[serde(default)]
    short: Option<bool>,
}

#[async_trait::async_trait]
impl Tool for GitStatusTool {
    fn name(&self) -> &str {
        "git_status"
    }
    fn description(&self) -> &str {
        "Show git repository status"
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json_schema_for::<GitStatusArgs>()
    }
    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let a: GitStatusArgs = parse_args(&args)?;
        let mut cmd = std::process::Command::new("git");
        cmd.arg("status");
        if a.short.unwrap_or(true) {
            cmd.arg("--short");
        }
        cmd.current_dir(&ctx.cwd);
        let output = cmd.output().map_err(ToolError::exec)?;
        Ok(ToolOutput::text(String::from_utf8_lossy(&output.stdout)))
    }
}

/// Git diff tool
struct GitDiffTool;

#[derive(Deserialize, JsonSchema)]
struct GitDiffArgs {
    #[serde(default)]
    staged: Option<bool>,
    #[serde(default)]
    path: Option<String>,
}

#[async_trait::async_trait]
impl Tool for GitDiffTool {
    fn name(&self) -> &str {
        "git_diff"
    }
    fn description(&self) -> &str {
        "Show git diff (unstaged by default, use staged=true for staged)"
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json_schema_for::<GitDiffArgs>()
    }
    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let a: GitDiffArgs = parse_args(&args)?;
        let mut cmd = std::process::Command::new("git");
        cmd.arg("diff");
        if a.staged.unwrap_or(false) {
            cmd.arg("--cached");
        }
        if let Some(p) = &a.path {
            cmd.arg("--").arg(p);
        }
        cmd.current_dir(&ctx.cwd);
        let output = cmd.output().map_err(ToolError::exec)?;
        Ok(ToolOutput::text(String::from_utf8_lossy(&output.stdout)))
    }
}

/// Finish tool — signals task completion
struct FinishTool;

#[derive(Deserialize, JsonSchema)]
struct FinishArgs {
    /// Summary of what was accomplished
    summary: String,
}

#[async_trait::async_trait]
impl Tool for FinishTool {
    fn name(&self) -> &str {
        "finish"
    }
    fn description(&self) -> &str {
        "Call when the task is FULLY complete. Provide a summary."
    }
    fn is_system(&self) -> bool {
        true
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json_schema_for::<FinishArgs>()
    }
    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let a: FinishArgs = parse_args(&args)?;
        Ok(ToolOutput::done(a.summary))
    }
}

// ─── Main ───────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let prompt = std::env::args().nth(1).unwrap_or_else(|| {
        "Read the README.md in the current directory and summarize it in 2 sentences.".to_string()
    });

    // Backend: local filesystem rooted at cwd
    let cwd = std::env::current_dir().unwrap();
    let fs = Arc::new(LocalFs::new(&cwd));

    // Register tools: 13 from sgr-agent-tools + 3 custom
    let tools = ToolRegistry::new()
        // File tools (from sgr-agent-tools, generic over FileBackend)
        .register(ReadTool(fs.clone()))
        .register(WriteTool(fs.clone()))
        .register(DeleteTool(fs.clone()))
        .register(SearchTool(fs.clone()))
        .register(ListTool(fs.clone()))
        .register(TreeTool(fs.clone()))
        .register(ReadAllTool(fs.clone()))
        .register(ShellTool)
        .register(ApplyPatchTool(fs.clone()))
        // Deferred tools (loaded on demand)
        .register_deferred(MkDirTool(fs.clone()))
        .register_deferred(MoveTool(fs.clone()))
        .register_deferred(FindTool(fs.clone()))
        // Custom tools
        .register(GitStatusTool)
        .register(GitDiffTool)
        .register(FinishTool);

    // LLM client
    let config = LlmConfig::auto("gpt-4o");
    let llm = Llm::new(&config);

    // Agent
    let agent = ToolCallingAgent::new(
        llm,
        "You are a helpful coding assistant. Use the provided tools to complete the task. \
         Call finish() when done.",
    );

    // Run
    let mut ctx = AgentContext::new().with_cwd(&cwd);
    let mut messages = vec![Message::user(&prompt)];
    let loop_config = LoopConfig {
        max_steps: 15,
        ..Default::default()
    };

    eprintln!("Agent starting: {}", prompt);
    eprintln!("Tools: {} registered", tools.len());

    match run_loop(
        &agent,
        &tools,
        &mut ctx,
        &mut messages,
        &loop_config,
        |event| match event {
            LoopEvent::StepStart { step } => eprintln!("\n--- Step {} ---", step),
            LoopEvent::ToolResult { name, output } => {
                let preview = if output.len() > 200 {
                    &output[..200]
                } else {
                    output.as_str()
                };
                eprintln!("  {} → {}...", name, preview.replace('\n', " "));
            }
            LoopEvent::Completed { steps } => eprintln!("\nDone in {} steps.", steps),
            _ => {}
        },
    )
    .await
    {
        Ok(steps) => eprintln!("Completed in {} steps", steps),
        Err(e) => eprintln!("Error: {}", e),
    }
}
