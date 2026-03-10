//! Swarm tools — tools for the parent agent to manage sub-agents.
//!
//! These tools are registered in the parent's ToolRegistry, allowing the agent
//! to spawn, wait, query status, and cancel sub-agents via normal tool calls.

use crate::agent_tool::{parse_args, Tool, ToolError, ToolOutput};
use crate::context::AgentContext;
use crate::swarm::{AgentId, AgentRole, SwarmManager};
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Shared swarm manager reference for tools.
pub type SharedSwarm = Arc<Mutex<SwarmManager>>;

/// Create a shared swarm manager.
pub fn shared_swarm(manager: SwarmManager) -> SharedSwarm {
    Arc::new(Mutex::new(manager))
}

// --- SpawnAgentTool ---

#[derive(Deserialize)]
struct SpawnArgs {
    /// Role: "explorer", "worker", "reviewer", or custom name
    role: String,
    /// Task description for the sub-agent
    task: String,
    /// Optional system prompt override
    system_prompt: Option<String>,
    /// Optional max steps (default: role-dependent)
    max_steps: Option<usize>,
    /// Optional working directory
    cwd: Option<String>,
}

/// Tool for spawning sub-agents.
pub struct SpawnAgentTool {
    swarm: SharedSwarm,
    /// Factory function to create agent + tools for a given role.
    /// The parent must provide this to wire up LlmClient, tools, etc.
    factory: Arc<dyn AgentFactory>,
}

/// Factory for creating agent + tool registry based on role.
///
/// Implementors provide the actual LlmClient and tools for each role.
#[async_trait::async_trait]
pub trait AgentFactory: Send + Sync {
    /// Create an agent and its tool registry for the given role.
    async fn create(
        &self,
        role: &AgentRole,
        system_prompt: Option<&str>,
    ) -> Result<(Box<dyn crate::agent::Agent>, crate::registry::ToolRegistry), String>;
}

impl SpawnAgentTool {
    pub fn new(swarm: SharedSwarm, factory: Arc<dyn AgentFactory>) -> Self {
        Self { swarm, factory }
    }
}

#[async_trait::async_trait]
impl Tool for SpawnAgentTool {
    fn name(&self) -> &str {
        "spawn_agent"
    }

    fn description(&self) -> &str {
        "Spawn a sub-agent with a specific role and task. Roles: explorer (fast, read-only), worker (smart, read-write), reviewer (read-only, thorough)."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "required": ["role", "task"],
            "properties": {
                "role": {
                    "type": "string",
                    "description": "Agent role: explorer, worker, reviewer, or custom name"
                },
                "task": {
                    "type": "string",
                    "description": "Task description for the sub-agent"
                },
                "system_prompt": {
                    "type": "string",
                    "description": "Optional system prompt override"
                },
                "max_steps": {
                    "type": "integer",
                    "description": "Optional max steps for the agent loop"
                },
                "cwd": {
                    "type": "string",
                    "description": "Optional working directory"
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let args: SpawnArgs = parse_args(&args)?;

        let role = match args.role.as_str() {
            "explorer" => AgentRole::Explorer,
            "worker" => AgentRole::Worker,
            "reviewer" => AgentRole::Reviewer,
            other => AgentRole::Custom(other.to_string()),
        };

        let (agent, tools) = self
            .factory
            .create(&role, args.system_prompt.as_deref())
            .await
            .map_err(ToolError::Execution)?;

        let config = crate::swarm::SpawnConfig {
            role: role.clone(),
            system_prompt: args.system_prompt,
            tool_names: None,
            cwd: args.cwd.map(std::path::PathBuf::from),
            task: args.task.clone(),
            max_steps: args.max_steps.unwrap_or(match &role {
                AgentRole::Explorer => 10,
                AgentRole::Worker => 30,
                AgentRole::Reviewer => 15,
                AgentRole::Custom(_) => 20,
            }),
            writable_roots: None,
        };

        let mut swarm = self.swarm.lock().await;
        let id = swarm
            .spawn(config, agent, tools, ctx)
            .map_err(|e| ToolError::Execution(e.to_string()))?;

        Ok(ToolOutput::text(format!(
            "Spawned {} agent (id: {}): {}",
            role.name(),
            id,
            args.task
        )))
    }
}

// --- WaitAgentsTool ---

#[derive(Deserialize)]
struct WaitArgs {
    /// Agent IDs to wait for. If empty, wait for all.
    #[serde(default)]
    ids: Vec<String>,
    /// Timeout in seconds (default: 300)
    timeout_secs: Option<u64>,
}

/// Tool for waiting on sub-agents to complete.
pub struct WaitAgentsTool {
    swarm: SharedSwarm,
}

impl WaitAgentsTool {
    pub fn new(swarm: SharedSwarm) -> Self {
        Self { swarm }
    }
}

#[async_trait::async_trait]
impl Tool for WaitAgentsTool {
    fn name(&self) -> &str {
        "wait_agents"
    }

    fn description(&self) -> &str {
        "Wait for sub-agents to complete. Provide specific IDs or wait for all."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "ids": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Agent IDs to wait for. Empty = wait all."
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 300)"
                }
            }
        })
    }

    async fn execute(&self, args: Value, _ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let args: WaitArgs = parse_args(&args)?;
        let timeout =
            std::time::Duration::from_secs(args.timeout_secs.unwrap_or(300));

        // Take receivers under lock, then drop lock before awaiting (avoid deadlock)
        let receivers = {
            let mut swarm = self.swarm.lock().await;
            if args.ids.is_empty() {
                swarm.take_all_receivers()
            } else {
                let mut rxs = Vec::new();
                for id_str in &args.ids {
                    let id = AgentId::from(id_str.as_str());
                    match swarm.take_receiver(&id) {
                        Ok(rx) => rxs.push((id, rx)),
                        Err(e) => {
                            return Err(ToolError::Execution(format!(
                                "Error for {}: {}",
                                id_str, e
                            )))
                        }
                    }
                }
                rxs
            }
        }; // lock dropped here

        // Await results without holding the lock
        let mut results = Vec::new();
        for (id, rx) in receivers {
            match tokio::time::timeout(timeout, rx).await {
                Ok(Ok(result)) => results.push(result),
                Ok(Err(_)) => {
                    return Err(ToolError::Execution(format!(
                        "Channel closed for {}",
                        id
                    )))
                }
                Err(_) => {
                    return Err(ToolError::Execution(format!(
                        "Timeout waiting for {}",
                        id
                    )))
                }
            }
        }

        // Cleanup completed agents
        {
            let mut swarm = self.swarm.lock().await;
            for r in &results {
                swarm.cleanup(&r.id);
            }
        }

        let mut output = String::new();
        for r in &results {
            output.push_str(&format!(
                "<agent_result id=\"{}\" role=\"{}\" status=\"{}\">\n{}\n</agent_result>\n",
                r.id, r.role, r.status, r.summary
            ));
        }

        if output.is_empty() {
            output = "No agents to wait for.".to_string();
        }

        Ok(ToolOutput::text(output))
    }
}

// --- GetStatusTool ---

/// Tool for checking status of sub-agents.
pub struct GetStatusTool {
    swarm: SharedSwarm,
}

impl GetStatusTool {
    pub fn new(swarm: SharedSwarm) -> Self {
        Self { swarm }
    }
}

#[async_trait::async_trait]
impl Tool for GetStatusTool {
    fn name(&self) -> &str {
        "agent_status"
    }

    fn description(&self) -> &str {
        "Get status of all active sub-agents."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({"type": "object"})
    }

    async fn execute(&self, _args: Value, _ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let swarm = self.swarm.lock().await;
        let statuses = swarm.status_all().await;

        if statuses.is_empty() {
            return Ok(ToolOutput::text("No active agents."));
        }

        let mut output = String::new();
        for (id, role, status) in &statuses {
            output.push_str(&format!("- {} ({}) — {}\n", id, role, status));
        }

        Ok(ToolOutput::text(output))
    }
}

// --- CancelAgentTool ---

#[derive(Deserialize)]
struct CancelArgs {
    /// Agent ID to cancel. "all" to cancel all.
    id: String,
}

/// Tool for cancelling sub-agents.
pub struct CancelAgentTool {
    swarm: SharedSwarm,
}

impl CancelAgentTool {
    pub fn new(swarm: SharedSwarm) -> Self {
        Self { swarm }
    }
}

#[async_trait::async_trait]
impl Tool for CancelAgentTool {
    fn name(&self) -> &str {
        "cancel_agent"
    }

    fn description(&self) -> &str {
        "Cancel a running sub-agent by ID, or 'all' to cancel all agents."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "required": ["id"],
            "properties": {
                "id": {
                    "type": "string",
                    "description": "Agent ID to cancel, or 'all'"
                }
            }
        })
    }

    async fn execute(&self, args: Value, _ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let args: CancelArgs = parse_args(&args)?;

        let swarm = self.swarm.lock().await;

        if args.id == "all" {
            swarm.cancel_all();
            Ok(ToolOutput::text("Cancelled all agents."))
        } else {
            let id = AgentId::from(args.id.as_str());
            swarm
                .cancel(&id)
                .map_err(|e| ToolError::Execution(e.to_string()))?;
            Ok(ToolOutput::text(format!("Cancelled agent {}.", args.id)))
        }
    }
}

// Helper: construct AgentId from string
impl From<&str> for AgentId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_id_from_str() {
        let id = AgentId::from("abc123");
        assert_eq!(id.short(), "abc123");
        assert_eq!(format!("{}", id), "abc123");
    }

    #[test]
    fn agent_role_names() {
        assert_eq!(AgentRole::Explorer.name(), "explorer");
        assert_eq!(AgentRole::Custom("planner".into()).name(), "planner");
    }

    #[test]
    fn shared_swarm_creates() {
        let swarm = shared_swarm(SwarmManager::new());
        // Should compile and not panic
        drop(swarm);
    }
}
