//! Swarm tools — spawn, wait, status, cancel sub-agents.

use crate::backend::LlmProvider;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sgr_agent::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent::context::AgentContext;
use sgr_agent::swarm::{AgentId, AgentRole, SpawnConfig, SwarmManager};
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;

// ---------------------------------------------------------------------------
// SpawnAgent
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SpawnAgentArgs {
    /// Role: "explorer" (fast, read-only), "worker" (smart, read-write), "reviewer" (read-only, thorough).
    pub role: String,
    /// Task description for the sub-agent.
    pub task: String,
    /// Optional max steps before auto-stop.
    #[serde(default)]
    pub max_steps: Option<i64>,
}

pub struct SpawnAgentTool {
    pub swarm: Arc<TokioMutex<SwarmManager>>,
    pub provider: Arc<Option<LlmProvider>>,
}

#[async_trait::async_trait]
impl Tool for SpawnAgentTool {
    fn name(&self) -> &str {
        "spawn_agent"
    }
    fn description(&self) -> &str {
        "Spawn a sub-agent with a role and task. Roles: explorer (fast, read-only), worker (smart, read-write), reviewer (thorough, read-only). Returns agent ID."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        sgr_agent::schema::json_schema_for::<SpawnAgentArgs>()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: SpawnAgentArgs = parse_args(&args)?;

        let agent_role = match args.role.as_str() {
            "explorer" => AgentRole::Explorer,
            "worker" => AgentRole::Worker,
            "reviewer" => AgentRole::Reviewer,
            other => AgentRole::Custom(other.to_string()),
        };

        let provider = match self.provider.as_ref() {
            Some(p) => p,
            None => {
                return Ok(ToolOutput::text(
                    "Cannot spawn agent: no LLM provider configured.",
                ));
            }
        };

        // Create sub-agent's LLM client + agent + tools
        let client = provider.make_llm_client();

        let sub_prompt = format!(
            "You are a {} sub-agent. Complete the task efficiently. Respond with JSON only.",
            agent_role.name()
        );
        let sub_agent = sgr_agent::agents::flexible::FlexibleAgent::new(client, sub_prompt, 3);
        let sub_tools = sgr_agent::registry::ToolRegistry::new();

        let mut config = match agent_role {
            AgentRole::Explorer => SpawnConfig::explorer(args.task.clone()),
            AgentRole::Worker => SpawnConfig::worker(args.task.clone()),
            AgentRole::Reviewer => SpawnConfig::reviewer(args.task.clone()),
            AgentRole::Custom(_) => SpawnConfig::worker(args.task.clone()),
        };
        if let Some(n) = args.max_steps {
            config.max_steps = n as usize;
        }

        let parent_ctx = sgr_agent::context::AgentContext::new();
        let mut swarm = self.swarm.lock().await;
        match swarm.spawn(config, Box::new(sub_agent), sub_tools, &parent_ctx) {
            Ok(id) => Ok(ToolOutput::text(format!(
                "Spawned {} agent: {}\nTask: {}",
                agent_role, id, args.task
            ))),
            Err(e) => Ok(ToolOutput::text(format!("Failed to spawn agent: {}", e))),
        }
    }
}

// ---------------------------------------------------------------------------
// WaitAgents
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct WaitAgentsArgs {
    /// Agent IDs to wait for. Empty = wait for all.
    #[serde(default)]
    pub agent_ids: Vec<String>,
    /// Timeout in seconds (default: 300).
    #[serde(default)]
    pub timeout_secs: Option<i64>,
}

pub struct WaitAgentsTool {
    pub swarm: Arc<TokioMutex<SwarmManager>>,
}

#[async_trait::async_trait]
impl Tool for WaitAgentsTool {
    fn name(&self) -> &str {
        "wait_agents"
    }
    fn description(&self) -> &str {
        "Wait for sub-agents to complete. Returns their results."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        sgr_agent::schema::json_schema_for::<WaitAgentsArgs>()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: WaitAgentsArgs = parse_args(&args)?;
        let timeout =
            std::time::Duration::from_secs(args.timeout_secs.map(|s| s as u64).unwrap_or(300));

        let ids: Vec<AgentId>;
        {
            let swarm = self.swarm.lock().await;
            ids = if args.agent_ids.is_empty() {
                swarm.all_agent_ids()
            } else {
                args.agent_ids.iter().map(|s| AgentId(s.clone())).collect()
            };
        }

        if ids.is_empty() {
            return Ok(ToolOutput::text("No agents to wait for."));
        }

        let mut swarm = self.swarm.lock().await;
        let results = swarm.wait_with_timeout(&ids, timeout).await;
        let output = results
            .iter()
            .map(|(id, result)| format!("[{}] {}", id, result))
            .collect::<Vec<_>>()
            .join("\n\n");
        Ok(ToolOutput::text(output))
    }
}

// ---------------------------------------------------------------------------
// AgentStatus
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct AgentStatusArgs {
    /// Agent ID to check. If omitted, shows all agents.
    #[serde(default)]
    pub agent_id: Option<String>,
}

pub struct AgentStatusTool {
    pub swarm: Arc<TokioMutex<SwarmManager>>,
}

#[async_trait::async_trait]
impl Tool for AgentStatusTool {
    fn name(&self) -> &str {
        "agent_status"
    }
    fn description(&self) -> &str {
        "Check status of sub-agents (running/completed/failed)."
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn parameters_schema(&self) -> serde_json::Value {
        sgr_agent::schema::json_schema_for::<AgentStatusArgs>()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: AgentStatusArgs = parse_args(&args)?;
        let swarm = self.swarm.lock().await;
        let output = if let Some(id) = &args.agent_id {
            let aid = AgentId(id.clone());
            match swarm.status(&aid).await {
                Some(s) => format!("[{}] {}", id, s),
                None => format!("Agent '{}' not found", id),
            }
        } else {
            swarm.status_all_formatted().await
        };
        Ok(ToolOutput::text(output))
    }
}

// ---------------------------------------------------------------------------
// CancelAgent
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CancelAgentArgs {
    /// Agent ID to cancel. Use "all" to cancel all agents.
    pub agent_id: String,
}

pub struct CancelAgentTool {
    pub swarm: Arc<TokioMutex<SwarmManager>>,
}

#[async_trait::async_trait]
impl Tool for CancelAgentTool {
    fn name(&self) -> &str {
        "cancel_agent"
    }
    fn description(&self) -> &str {
        "Cancel a running sub-agent by ID, or 'all' to cancel all."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        sgr_agent::schema::json_schema_for::<CancelAgentArgs>()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: CancelAgentArgs = parse_args(&args)?;
        let swarm = self.swarm.lock().await;
        if args.agent_id == "all" {
            swarm.cancel_all();
            Ok(ToolOutput::text("Cancelled all agents."))
        } else {
            let aid = AgentId(args.agent_id.clone());
            match swarm.cancel(&aid) {
                Ok(()) => Ok(ToolOutput::text(format!(
                    "Cancelled agent: {}",
                    args.agent_id
                ))),
                Err(e) => Ok(ToolOutput::text(format!("Failed to cancel: {}", e))),
            }
        }
    }
}
