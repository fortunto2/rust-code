//! Multi-agent swarm — spawn, manage, and coordinate sub-agents.
//!
//! Each sub-agent runs in its own tokio task with its own LlmClient, ToolRegistry,
//! and AgentContext. Sub-agents can use different models and providers.

use crate::agent::{Agent, AgentError};
use crate::agent_loop::{LoopConfig, run_loop};
use crate::context::AgentContext;
use crate::registry::ToolRegistry;
use crate::types::Message;
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// Unique identifier for a sub-agent.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct AgentId(pub String);

impl Default for AgentId {
    fn default() -> Self {
        Self(format!("agent-{}", next_id()))
    }
}

impl AgentId {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn short(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

fn next_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Role of a sub-agent — determines default tools and model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRole {
    /// Read-only, fast model — for codebase exploration.
    Explorer,
    /// Read-write, smart model — for implementation.
    Worker,
    /// Read-only, reasoning model — for code review.
    Reviewer,
    /// User-defined role.
    Custom(String),
}

impl AgentRole {
    pub fn name(&self) -> &str {
        match self {
            Self::Explorer => "explorer",
            Self::Worker => "worker",
            Self::Reviewer => "reviewer",
            Self::Custom(n) => n,
        }
    }
}

impl fmt::Display for AgentRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// Current status of a sub-agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentStatus {
    Running,
    Completed,
    Failed(String),
    Cancelled,
}

impl fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Running => write!(f, "running"),
            Self::Completed => write!(f, "completed"),
            Self::Failed(e) => write!(f, "failed: {}", e),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// Result from a completed sub-agent.
#[derive(Debug, Clone)]
pub struct SwarmResult {
    pub id: AgentId,
    pub role: AgentRole,
    pub status: AgentStatus,
    /// Final summary from the agent (last assistant message or situation).
    pub summary: String,
    /// Number of steps taken.
    pub steps: usize,
    /// Events collected during execution.
    pub events: Vec<String>,
}

/// Configuration for spawning a sub-agent.
pub struct SpawnConfig {
    /// Role determines default model/tools.
    pub role: AgentRole,
    /// Custom system prompt (if None, use role default).
    pub system_prompt: Option<String>,
    /// Tool names to make available (if None, use role defaults).
    pub tool_names: Option<Vec<String>>,
    /// Working directory (if None, inherit from parent).
    pub cwd: Option<PathBuf>,
    /// Initial task description for the sub-agent.
    pub task: String,
    /// Max steps for the sub-agent loop.
    pub max_steps: usize,
    /// Writable roots for sandbox (if None, inherit from parent).
    pub writable_roots: Option<Vec<PathBuf>>,
}

impl SpawnConfig {
    pub fn explorer(task: impl Into<String>) -> Self {
        Self {
            role: AgentRole::Explorer,
            system_prompt: None,
            tool_names: None,
            cwd: None,
            task: task.into(),
            max_steps: 10,
            writable_roots: None,
        }
    }

    pub fn worker(task: impl Into<String>) -> Self {
        Self {
            role: AgentRole::Worker,
            system_prompt: None,
            tool_names: None,
            cwd: None,
            task: task.into(),
            max_steps: 30,
            writable_roots: None,
        }
    }

    pub fn reviewer(task: impl Into<String>) -> Self {
        Self {
            role: AgentRole::Reviewer,
            system_prompt: None,
            tool_names: None,
            cwd: None,
            task: task.into(),
            max_steps: 15,
            writable_roots: None,
        }
    }
}

/// Swarm error types.
#[derive(Debug, thiserror::Error)]
pub enum SwarmError {
    #[error("Max agents reached ({0})")]
    MaxAgents(usize),
    #[error("Max depth reached ({0})")]
    MaxDepth(usize),
    #[error("Agent not found: {0}")]
    NotFound(AgentId),
    #[error("Agent already completed: {0}")]
    AlreadyCompleted(AgentId),
    #[error("Agent error: {0}")]
    Agent(#[from] AgentError),
    #[error("Channel error")]
    Channel,
}

/// Handle to a running sub-agent.
struct AgentHandle {
    id: AgentId,
    role: AgentRole,
    cancel: CancellationToken,
    status: Arc<Mutex<AgentStatus>>,
    result_rx: Option<oneshot::Receiver<SwarmResult>>,
}

/// Notification sent to parent when a sub-agent completes.
#[derive(Debug, Clone)]
pub struct AgentNotification {
    pub id: AgentId,
    pub role: AgentRole,
    pub status: AgentStatus,
    pub summary: String,
}

/// Manages a swarm of sub-agents.
pub struct SwarmManager {
    agents: HashMap<AgentId, AgentHandle>,
    /// Channel to notify parent of completions.
    notification_tx: mpsc::Sender<AgentNotification>,
    notification_rx: Arc<Mutex<mpsc::Receiver<AgentNotification>>>,
    max_agents: usize,
    max_depth: usize,
    current_depth: usize,
}

impl SwarmManager {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel(64);
        Self {
            agents: HashMap::new(),
            notification_tx: tx,
            notification_rx: Arc::new(Mutex::new(rx)),
            max_agents: 8,
            max_depth: 3,
            current_depth: 0,
        }
    }

    pub fn with_limits(mut self, max_agents: usize, max_depth: usize) -> Self {
        self.max_agents = max_agents;
        self.max_depth = max_depth;
        self
    }

    pub fn with_depth(mut self, depth: usize) -> Self {
        self.current_depth = depth;
        self
    }

    /// Spawn a sub-agent. Returns its ID.
    ///
    /// The agent runs in a background tokio task. When complete, a notification
    /// is sent through the notification channel.
    pub fn spawn(
        &mut self,
        config: SpawnConfig,
        agent: Box<dyn Agent>,
        tools: ToolRegistry,
        parent_ctx: &AgentContext,
    ) -> Result<AgentId, SwarmError> {
        if self.active_count() >= self.max_agents {
            return Err(SwarmError::MaxAgents(self.max_agents));
        }
        if self.current_depth >= self.max_depth {
            return Err(SwarmError::MaxDepth(self.max_depth));
        }

        let id = AgentId::new();
        let cancel = CancellationToken::new();
        let status = Arc::new(Mutex::new(AgentStatus::Running));
        let (result_tx, result_rx) = oneshot::channel();

        // Build sub-agent context
        let mut ctx = AgentContext::new();
        ctx.cwd = config.cwd.unwrap_or_else(|| parent_ctx.cwd.clone());
        ctx.writable_roots = config
            .writable_roots
            .unwrap_or_else(|| parent_ctx.writable_roots.clone());

        // Build initial messages
        let system_prompt = config.system_prompt.unwrap_or_else(|| {
            format!(
                "You are a {} agent. Complete the assigned task efficiently.",
                config.role.name()
            )
        });
        let mut messages = vec![Message::system(&system_prompt), Message::user(&config.task)];

        let loop_config = LoopConfig {
            max_steps: config.max_steps,
            ..Default::default()
        };

        let agent_id = id.clone();
        let agent_role = config.role.clone();
        let cancel_token = cancel.clone();
        let status_clone = Arc::clone(&status);
        let notify_tx = self.notification_tx.clone();

        // Spawn the agent loop in a background task
        tokio::spawn(async move {
            let mut events: Vec<String> = Vec::new();

            let loop_result = tokio::select! {
                result = run_loop(
                    agent.as_ref(),
                    &tools,
                    &mut ctx,
                    &mut messages,
                    &loop_config,
                    |event| {
                        events.push(format!("{:?}", event));
                    },
                ) => result,
                _ = cancel_token.cancelled() => {
                    Err(AgentError::Cancelled)
                }
            };

            let (final_status, summary, steps) = match loop_result {
                Ok(steps) => {
                    let summary = messages
                        .iter()
                        .rev()
                        .find(|m| m.role == crate::types::Role::Assistant)
                        .map(|m| m.content.clone())
                        .unwrap_or_else(|| "Completed".to_string());
                    (AgentStatus::Completed, summary, steps)
                }
                Err(AgentError::Cancelled) => (AgentStatus::Cancelled, "Cancelled".to_string(), 0),
                Err(e) => (AgentStatus::Failed(e.to_string()), e.to_string(), 0),
            };

            // Update status
            *status_clone.lock().await = final_status.clone();

            let result = SwarmResult {
                id: agent_id.clone(),
                role: agent_role.clone(),
                status: final_status.clone(),
                summary: summary.clone(),
                steps,
                events,
            };

            // Send result
            let _ = result_tx.send(result);

            // Notify parent
            let _ = notify_tx
                .send(AgentNotification {
                    id: agent_id,
                    role: agent_role,
                    status: final_status,
                    summary,
                })
                .await;
        });

        self.agents.insert(
            id.clone(),
            AgentHandle {
                id: id.clone(),
                role: config.role,
                cancel,
                status,
                result_rx: Some(result_rx),
            },
        );

        Ok(id)
    }

    /// Get the status of a sub-agent.
    pub async fn status(&self, id: &AgentId) -> Option<AgentStatus> {
        if let Some(handle) = self.agents.get(id) {
            Some(handle.status.lock().await.clone())
        } else {
            None
        }
    }

    /// Get status of all agents.
    pub async fn status_all(&self) -> Vec<(AgentId, AgentRole, AgentStatus)> {
        let mut result = Vec::new();
        for handle in self.agents.values() {
            let status = handle.status.lock().await.clone();
            result.push((handle.id.clone(), handle.role.clone(), status));
        }
        result
    }

    /// Take the result receiver for an agent (non-async, fast).
    /// Call this under the lock, then drop the lock before awaiting.
    pub fn take_receiver(
        &mut self,
        id: &AgentId,
    ) -> Result<oneshot::Receiver<SwarmResult>, SwarmError> {
        let handle = self
            .agents
            .get_mut(id)
            .ok_or_else(|| SwarmError::NotFound(id.clone()))?;

        handle
            .result_rx
            .take()
            .ok_or_else(|| SwarmError::AlreadyCompleted(id.clone()))
    }

    /// Take all pending result receivers (non-async, fast).
    pub fn take_all_receivers(&mut self) -> Vec<(AgentId, oneshot::Receiver<SwarmResult>)> {
        let mut receivers = Vec::new();
        for (id, handle) in &mut self.agents {
            if let Some(rx) = handle.result_rx.take() {
                receivers.push((id.clone(), rx));
            }
        }
        receivers
    }

    /// Wait for a specific agent to complete. Returns its result.
    /// Cleans up the agent handle after completion.
    pub async fn wait(&mut self, id: &AgentId) -> Result<SwarmResult, SwarmError> {
        let rx = self.take_receiver(id)?;
        let result = rx.await.map_err(|_| SwarmError::Channel)?;
        self.agents.remove(id); // cleanup completed agent
        Ok(result)
    }

    /// Wait for all agents to complete.
    /// Cleans up all agent handles after completion.
    pub async fn wait_all(&mut self) -> Vec<SwarmResult> {
        let receivers = self.take_all_receivers();
        let mut results = Vec::new();
        for (id, rx) in receivers {
            if let Ok(result) = rx.await {
                results.push(result);
                self.agents.remove(&id);
            }
        }
        results
    }

    /// Cancel a specific agent.
    pub fn cancel(&self, id: &AgentId) -> Result<(), SwarmError> {
        let handle = self
            .agents
            .get(id)
            .ok_or_else(|| SwarmError::NotFound(id.clone()))?;
        handle.cancel.cancel();
        Ok(())
    }

    /// Cancel all running agents.
    pub fn cancel_all(&self) {
        for handle in self.agents.values() {
            handle.cancel.cancel();
        }
    }

    /// Receive the next completion notification (non-blocking).
    pub async fn try_recv_notification(&self) -> Option<AgentNotification> {
        let mut rx = self.notification_rx.lock().await;
        rx.try_recv().ok()
    }

    /// Receive notifications, blocking until one arrives or timeout.
    pub async fn recv_notification(
        &self,
        timeout: std::time::Duration,
    ) -> Option<AgentNotification> {
        let mut rx = self.notification_rx.lock().await;
        tokio::time::timeout(timeout, rx.recv())
            .await
            .ok()
            .flatten()
    }

    /// Remove a completed agent handle (cleanup).
    pub fn cleanup(&mut self, id: &AgentId) {
        self.agents.remove(id);
    }

    /// Number of agents (including completed with pending results).
    pub fn agent_count(&self) -> usize {
        self.agents.len()
    }

    /// Number of agents still holding a result receiver (not yet waited).
    pub fn active_count(&self) -> usize {
        self.agents
            .values()
            .filter(|h| h.result_rx.is_some())
            .count()
    }

    /// Get all agent IDs.
    pub fn all_agent_ids(&self) -> Vec<AgentId> {
        self.agents.keys().cloned().collect()
    }

    /// Format all agent statuses as a human-readable string.
    pub async fn status_all_formatted(&self) -> String {
        let statuses = self.status_all().await;
        if statuses.is_empty() {
            return "No agents.".to_string();
        }
        statuses
            .iter()
            .map(|(id, role, status)| format!("[{}] {} — {}", id, role, status))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Wait for multiple agents with timeout. Returns (id, formatted_result) pairs.
    pub async fn wait_with_timeout(
        &mut self,
        ids: &[AgentId],
        timeout: std::time::Duration,
    ) -> Vec<(AgentId, String)> {
        let mut results = Vec::new();
        for id in ids {
            let rx = match self.take_receiver(id) {
                Ok(rx) => rx,
                Err(e) => {
                    results.push((id.clone(), format!("Error: {}", e)));
                    continue;
                }
            };
            match tokio::time::timeout(timeout, rx).await {
                Ok(Ok(result)) => {
                    let summary = format!(
                        "{} ({}, {} steps): {}",
                        result.status,
                        result.role,
                        result.steps,
                        if result.summary.len() > 500 {
                            format!("{}...", &result.summary[..500])
                        } else {
                            result.summary.clone()
                        }
                    );
                    self.agents.remove(id);
                    results.push((id.clone(), summary));
                }
                Ok(Err(_)) => {
                    results.push((id.clone(), "Channel closed".into()));
                }
                Err(_) => {
                    results.push((id.clone(), format!("Timeout after {}s", timeout.as_secs())));
                }
            }
        }
        results
    }

    /// Format active agents as a status summary (for environment context).
    pub async fn status_summary(&self) -> String {
        let mut lines = Vec::new();
        for handle in self.agents.values() {
            let status = handle.status.lock().await;
            lines.push(format!("  {} ({}) — {}", handle.id, handle.role, *status));
        }
        if lines.is_empty() {
            "  (none)".to_string()
        } else {
            lines.join("\n")
        }
    }
}

impl Default for SwarmManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{Agent, AgentError, Decision};
    use crate::agent_tool::{Tool, ToolError, ToolOutput};
    use crate::types::{Message, ToolCall};
    use serde_json::Value;

    struct SimpleAgent {}

    #[async_trait::async_trait]
    impl Agent for SimpleAgent {
        async fn decide(
            &self,
            _messages: &[Message],
            _tools: &ToolRegistry,
        ) -> Result<Decision, AgentError> {
            // Complete immediately
            Ok(Decision {
                situation: "Task done.".into(),
                task: vec![],
                tool_calls: vec![],
                completed: true,
            })
        }
    }

    struct StepAgent {
        steps: usize,
    }

    #[async_trait::async_trait]
    impl Agent for StepAgent {
        async fn decide(
            &self,
            msgs: &[Message],
            _tools: &ToolRegistry,
        ) -> Result<Decision, AgentError> {
            // Count tool messages to determine step
            let tool_msgs = msgs
                .iter()
                .filter(|m| m.role == crate::types::Role::Tool)
                .count();
            if tool_msgs >= self.steps {
                Ok(Decision {
                    situation: "All steps done.".into(),
                    task: vec![],
                    tool_calls: vec![],
                    completed: true,
                })
            } else {
                Ok(Decision {
                    situation: format!("Step {}", tool_msgs + 1),
                    task: vec![],
                    tool_calls: vec![ToolCall {
                        id: format!("call_{}", tool_msgs),
                        name: "echo".into(),
                        arguments: serde_json::json!({}),
                    }],
                    completed: false,
                })
            }
        }
    }

    struct EchoTool;

    #[async_trait::async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "echo"
        }
        fn parameters_schema(&self) -> Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(&self, _: Value, _: &mut AgentContext) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::text("echoed"))
        }
    }

    #[tokio::test]
    async fn spawn_and_wait() {
        let mut swarm = SwarmManager::new();
        let ctx = AgentContext::new();

        let id = swarm
            .spawn(
                SpawnConfig::explorer("Find all Rust files"),
                Box::new(SimpleAgent {}),
                ToolRegistry::new(),
                &ctx,
            )
            .unwrap();

        let result = swarm.wait(&id).await.unwrap();
        assert_eq!(result.status, AgentStatus::Completed);
        assert!(result.summary.contains("Task done"));
    }

    #[tokio::test]
    async fn spawn_with_tools() {
        let mut swarm = SwarmManager::new();
        let ctx = AgentContext::new();
        let tools = ToolRegistry::new().register(EchoTool);

        let id = swarm
            .spawn(
                SpawnConfig::worker("Do 2 steps"),
                Box::new(StepAgent { steps: 2 }),
                tools,
                &ctx,
            )
            .unwrap();

        let result = swarm.wait(&id).await.unwrap();
        assert_eq!(result.status, AgentStatus::Completed);
        assert!(result.steps >= 2);
    }

    #[tokio::test]
    async fn cancel_agent() {
        let mut swarm = SwarmManager::new();
        let ctx = AgentContext::new();

        // Agent that would run many steps
        let id = swarm
            .spawn(
                SpawnConfig {
                    role: AgentRole::Worker,
                    system_prompt: None,
                    tool_names: None,
                    cwd: None,
                    task: "Long task".into(),
                    max_steps: 100,
                    writable_roots: None,
                },
                Box::new(StepAgent { steps: 100 }),
                ToolRegistry::new().register(EchoTool),
                &ctx,
            )
            .unwrap();

        // Cancel immediately
        swarm.cancel(&id).unwrap();

        let result = swarm.wait(&id).await.unwrap();
        assert!(
            result.status == AgentStatus::Cancelled
                || matches!(result.status, AgentStatus::Failed(_))
                || result.status == AgentStatus::Completed // might complete before cancel
        );
    }

    #[tokio::test]
    async fn max_agents_limit() {
        let mut swarm = SwarmManager::new().with_limits(2, 3);
        let ctx = AgentContext::new();

        // Spawn 2 agents (should work)
        let _id1 = swarm
            .spawn(
                SpawnConfig::explorer("Task 1"),
                Box::new(SimpleAgent {}),
                ToolRegistry::new(),
                &ctx,
            )
            .unwrap();

        let _id2 = swarm
            .spawn(
                SpawnConfig::explorer("Task 2"),
                Box::new(SimpleAgent {}),
                ToolRegistry::new(),
                &ctx,
            )
            .unwrap();

        // 3rd should fail
        let err = swarm
            .spawn(
                SpawnConfig::explorer("Task 3"),
                Box::new(SimpleAgent {}),
                ToolRegistry::new(),
                &ctx,
            )
            .err()
            .unwrap();
        assert!(matches!(err, SwarmError::MaxAgents(2)));
    }

    #[tokio::test]
    async fn max_depth_limit() {
        let mut swarm = SwarmManager::new().with_limits(8, 3).with_depth(3);
        let ctx = AgentContext::new();

        let err = swarm
            .spawn(
                SpawnConfig::explorer("Task"),
                Box::new(SimpleAgent {}),
                ToolRegistry::new(),
                &ctx,
            )
            .err()
            .unwrap();
        assert!(matches!(err, SwarmError::MaxDepth(3)));
    }

    #[tokio::test]
    async fn status_tracking() {
        let mut swarm = SwarmManager::new();
        let ctx = AgentContext::new();

        let id = swarm
            .spawn(
                SpawnConfig::explorer("Quick task"),
                Box::new(SimpleAgent {}),
                ToolRegistry::new(),
                &ctx,
            )
            .unwrap();

        // Wait for completion — agent is cleaned up after wait
        let result = swarm.wait(&id).await.unwrap();
        assert_eq!(result.status, AgentStatus::Completed);

        // After wait, agent is removed (cleanup)
        assert!(swarm.status(&id).await.is_none());
    }

    #[tokio::test]
    async fn wait_all_returns_results() {
        let mut swarm = SwarmManager::new();
        let ctx = AgentContext::new();

        let _id1 = swarm
            .spawn(
                SpawnConfig::explorer("Task 1"),
                Box::new(SimpleAgent {}),
                ToolRegistry::new(),
                &ctx,
            )
            .unwrap();

        let _id2 = swarm
            .spawn(
                SpawnConfig::worker("Task 2"),
                Box::new(SimpleAgent {}),
                ToolRegistry::new(),
                &ctx,
            )
            .unwrap();

        let results = swarm.wait_all().await;
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.status == AgentStatus::Completed));
    }

    #[test]
    fn agent_role_display() {
        assert_eq!(AgentRole::Explorer.name(), "explorer");
        assert_eq!(AgentRole::Worker.name(), "worker");
        assert_eq!(AgentRole::Reviewer.name(), "reviewer");
        assert_eq!(AgentRole::Custom("planner".into()).name(), "planner");
    }

    #[test]
    fn spawn_config_constructors() {
        let cfg = SpawnConfig::explorer("Find files");
        assert_eq!(cfg.role, AgentRole::Explorer);
        assert_eq!(cfg.max_steps, 10);

        let cfg = SpawnConfig::worker("Implement feature");
        assert_eq!(cfg.role, AgentRole::Worker);
        assert_eq!(cfg.max_steps, 30);

        let cfg = SpawnConfig::reviewer("Review code");
        assert_eq!(cfg.role, AgentRole::Reviewer);
        assert_eq!(cfg.max_steps, 15);
    }
}
