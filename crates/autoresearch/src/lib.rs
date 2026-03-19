//! AutoResearch — self-improving prompt optimization loop.
//!
//! Karpathy autoresearch pattern: Generate → Evaluate → Score → Keep/Discard → Mutate → Repeat.
//!
//! ## Targets
//! - `tool-selection` — optimize tool descriptions for correct tool choice
//! - `system-prompt` — optimize agent system prompt for decision quality
//! - `skill` — optimize any SKILL.md via custom eval criteria
//! - `decision-parser` — optimize structured output schema for parse accuracy
//!
//! ## Usage
//! ```no_run
//! use autoresearch::{AutoResearch, Config, Target};
//!
//! let config = Config {
//!     target: Target::ToolSelection,
//!     batch_size: 10,
//!     ..Default::default()
//! };
//! let ar = AutoResearch::new(config);
//! // ar.run(10).await; // 10 cycles
//! ```

use sgr_agent::types::{Message, SgrError};
use sgr_agent::{Llm, LlmConfig};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

// ─── Embedded test data ──────────────────────────────────────────────────────

const TOOL_SELECTION_TESTS: &str =
    include_str!("../../../skills/autoresearch/test_cases/tool_selection.json");
const SYSTEM_PROMPT_TESTS: &str =
    include_str!("../../../skills/autoresearch/test_cases/system_prompt.json");
const DECISION_PARSER_TESTS: &str =
    include_str!("../../../skills/autoresearch/test_cases/decision_parser.json");

// ─── Types ───────────────────────────────────────────────────────────────────

/// Persistent state between runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub best_score: i32,
    pub run_number: u32,
}

impl Default for State {
    fn default() -> Self {
        Self {
            best_score: -1,
            run_number: 0,
        }
    }
}

/// JSONL log entry for one run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunLog {
    pub run: u32,
    pub timestamp: String,
    pub score: usize,
    pub max: usize,
    pub criteria: HashMap<String, usize>,
    pub prompt_len: usize,
    pub generated: usize,
}

/// Unified test case format (covers all targets).
#[derive(Debug, Clone, Deserialize)]
pub struct TestCase {
    #[serde(default)]
    pub task: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub expected: String,
    #[serde(default)]
    pub expected_tool: String,
    #[serde(default)]
    pub expected_behavior: String,
}

impl TestCase {
    /// Primary input text for this test case.
    pub fn input(&self) -> &str {
        if !self.task.is_empty() {
            &self.task
        } else if !self.message.is_empty() {
            &self.message
        } else {
            ""
        }
    }
}

/// What to optimize.
#[derive(Debug, Clone)]
pub enum Target {
    /// Optimize tool descriptions for correct tool selection.
    ToolSelection,
    /// Optimize agent system prompt for decision quality.
    SystemPrompt,
    /// Optimize a specific SKILL.md file.
    Skill {
        name: String,
        skill_path: Option<PathBuf>,
    },
    /// Optimize structured output schema for parse accuracy.
    DecisionParser,
}

impl Target {
    /// Short name for directories and logging.
    pub fn name(&self) -> &str {
        match self {
            Self::ToolSelection => "tool-selection",
            Self::SystemPrompt => "system-prompt",
            Self::Skill { name, .. } => name,
            Self::DecisionParser => "decision-parser",
        }
    }
}

/// Runner configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub target: Target,
    pub batch_size: usize,
    pub cycle_secs: u64,
    pub data_dir: PathBuf,
    /// Model for generating test outputs (should match agent's actual model).
    pub gen_model: String,
    /// Model for evaluation and mutation (should be different from gen for objectivity).
    pub eval_model: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            target: Target::ToolSelection,
            batch_size: 10,
            cycle_secs: 120,
            data_dir: PathBuf::from("autoresearch_data"),
            gen_model: "gemini-2.5-flash".into(),
            eval_model: "claude-sonnet-4-6".into(),
        }
    }
}

/// Result of one optimization cycle.
#[derive(Debug, Clone)]
pub struct CycleResult {
    pub run: u32,
    pub score: usize,
    pub max_score: usize,
    pub improved: bool,
    pub criteria: HashMap<String, usize>,
}

// ─── Target-specific logic ───────────────────────────────────────────────────

fn target_criteria(target: &Target) -> Vec<String> {
    match target {
        Target::ToolSelection => vec!["correct_tool".into()],
        Target::SystemPrompt => vec![
            "correct_tool".into(),
            "coherent_situation".into(),
            "clear_task".into(),
            "no_hallucination".into(),
        ],
        Target::Skill { .. } => vec![
            "follows_instructions".into(),
            "actionable_output".into(),
            "no_hallucination".into(),
            "correct_format".into(),
        ],
        Target::DecisionParser => vec![
            "valid_json".into(),
            "has_all_fields".into(),
            "valid_tool_calls".into(),
            "situation_specific".into(),
            "task_actionable".into(),
        ],
    }
}

fn load_test_cases(target: &Target) -> Vec<TestCase> {
    let json = match target {
        Target::ToolSelection => TOOL_SELECTION_TESTS,
        Target::SystemPrompt => SYSTEM_PROMPT_TESTS,
        Target::DecisionParser => DECISION_PARSER_TESTS,
        Target::Skill { .. } => {
            // Default test inputs for skill optimization
            return vec![
                TestCase { task: "Improve error handling in this Rust project".into(), ..tc_empty() },
                TestCase { task: "Add tests for the auth module".into(), ..tc_empty() },
                TestCase { task: "Refactor database layer to use connection pooling".into(), ..tc_empty() },
                TestCase { task: "Set up CI/CD pipeline".into(), ..tc_empty() },
                TestCase { task: "Review codebase for security issues".into(), ..tc_empty() },
            ];
        }
    };
    serde_json::from_str(json).unwrap_or_default()
}

fn tc_empty() -> TestCase {
    TestCase {
        task: String::new(),
        message: String::new(),
        expected: String::new(),
        expected_tool: String::new(),
        expected_behavior: String::new(),
    }
}

// ─── Initial prompts ────────────────────────────────────────────────────────

fn initial_prompt(target: &Target) -> String {
    match target {
        Target::ToolSelection => INITIAL_TOOL_DESCRIPTIONS.into(),
        Target::SystemPrompt => initial_system_prompt(None),
        Target::Skill { skill_path, .. } => {
            if let Some(p) = skill_path {
                std::fs::read_to_string(p).unwrap_or_else(|_| "Perform the task thoroughly.".into())
            } else {
                "Perform the task thoroughly.".into()
            }
        }
        Target::DecisionParser => INITIAL_DECISION_SCHEMA.into(),
    }
}

fn initial_system_prompt(soul_path: Option<&Path>) -> String {
    if let Some(p) = soul_path {
        if let Ok(s) = std::fs::read_to_string(p) {
            return s;
        }
    }
    // Try default locations
    for p in &[".rust-code/SOUL.md", "../.rust-code/SOUL.md"] {
        if let Ok(s) = std::fs::read_to_string(p) {
            return s;
        }
    }
    DEFAULT_SYSTEM_PROMPT.into()
}

const INITIAL_TOOL_DESCRIPTIONS: &str = "\
read_file: Read file contents. Use offset/limit for large files.
write_file: Create or overwrite a file with new content.
apply_patch: Edit files. Use for ALL file modifications. Unified diff format with context lines.
bash: Run a shell command and return stdout/stderr.
bash_bg: Run a shell command in background (tmux).
search_code: Search codebase for a pattern using ripgrep.
git_status: Show git status of the working directory.
git_diff: Show git diff. Use cached=true for staged changes.
git_add: Stage files for commit.
git_commit: Create a git commit with a message.
open_editor: Open a file in the user's editor.
ask_user: Ask the user a question and wait for their response.
finish: Signal task completion with a summary of what was done.
mcp_call: Call a tool on an MCP server.
memory: Save or forget an agent memory entry.
project_map: Generate a project structure map.
dependencies: Analyze project dependencies.
task: Manage tasks: create, list, update, done.
spawn_agent: Spawn a sub-agent with a role and task.
wait_agents: Wait for sub-agents to complete.
agent_status: Check status of sub-agents.
cancel_agent: Cancel a running sub-agent.
api: Call any REST API via OpenAPI spec.
delegate_task: Delegate a task to a CLI agent (claude/gemini/codex).
delegate_status: Check status of delegated tasks.
delegate_result: Get output from a completed delegate.";

const DEFAULT_SYSTEM_PROMPT: &str = "\
You are a coding agent. You help users with software engineering tasks.

Core principles:
- Act, don't ask — prefer action over clarification
- Code over reports — produce working code, not bullet points
- Read before writing — understand existing code before modifying
- Minimal changes — don't over-engineer
- Always verify — run tests after changes

When given a task:
1. Analyze the situation
2. Plan the specific action
3. Execute with the appropriate tool
4. Verify the result";

const INITIAL_DECISION_SCHEMA: &str = r#"Respond with a JSON Decision object. This is your ONLY output format.

Schema:
{
  "situation": "1-2 sentence analysis of current state",
  "task": "specific action you will take right now",
  "tool_calls": [{"tool_name": "...", "parameters": {...}}],
  "completed": false
}

Rules:
- "situation" must reference the user's actual request, not generic filler
- "task" must be a concrete single action, not a multi-step plan
- "tool_calls" must have 1-3 calls, each with valid tool_name and parameters
- Set "completed" to true ONLY with finish tool
- tool_name must be one of: read_file, write_file, apply_patch, bash, bash_bg, search_code, git_status, git_diff, git_add, git_commit, open_editor, ask_user, finish, memory, project_map, dependencies, task, spawn_agent, api, delegate_task

Return ONLY the JSON object. No markdown fences, no explanations."#;

// ─── Generation prompts ─────────────────────────────────────────────────────

fn gen_prompt(target: &Target, prompt: &str, case: &TestCase) -> String {
    match target {
        Target::ToolSelection => format!(
            "You are an AI coding agent. Given a user task, select the SINGLE most appropriate tool \
             as the FIRST action.\n\nAvailable tools:\n{prompt}\n\nUser task: {}\n\n\
             Respond with ONLY the tool name, nothing else.",
            case.input()
        ),
        Target::SystemPrompt => format!(
            "You are an AI coding agent. Here is your system prompt:\n\n<system_prompt>\n{prompt}\n\
             </system_prompt>\n\n<available_tools>\nread_file, write_file, apply_patch, bash, bash_bg, \
             search_code, git_status, git_diff, git_add, git_commit, open_editor, ask_user, finish, \
             mcp_call, memory, project_map, dependencies, task, spawn_agent, wait_agents, agent_status, \
             cancel_agent, api, delegate_task, delegate_status, delegate_result\n</available_tools>\n\n\
             User says: {}\n\nRespond with a JSON Decision:\n\
             {{\"situation\": \"...\", \"task\": \"...\", \"tool_calls\": [{{\"tool_name\": \"...\", \"parameters\": {{...}}}}]}}",
            case.input()
        ),
        Target::Skill { .. } => format!(
            "You are an AI coding agent. Follow the skill instructions below.\n\n\
             <skill_instructions>\n{prompt}\n</skill_instructions>\n\n\
             User request: {}\n\nProduce the output the skill describes.",
            case.input()
        ),
        Target::DecisionParser => format!("{prompt}\n\nUser message: {}", case.input()),
    }
}

// ─── Evaluation prompts ─────────────────────────────────────────────────────

fn eval_prompt(target: &Target, output: &str, case: &TestCase) -> String {
    match target {
        Target::ToolSelection => unreachable!("tool selection uses direct match"),
        Target::SystemPrompt => format!(
            "Evaluate this AI agent's decision. Be strict.\n\n\
             TASK: {}\nEXPECTED FIRST TOOL: {}\nEXPECTED BEHAVIOR: {}\n\n\
             AGENT'S DECISION:\n{output}\n\n\
             Criteria:\n\
             1. correct_tool: Did it pick the expected tool as FIRST action?\n\
             2. coherent_situation: Is \"situation\" a genuine analysis (not generic)?\n\
             3. clear_task: Is \"task\" specific and actionable (not vague)?\n\
             4. no_hallucination: No references to files/state not in the task?\n\n\
             Respond with JSON only:\n\
             {{\"correct_tool\": true, \"coherent_situation\": true, \"clear_task\": true, \"no_hallucination\": true, \"failures\": []}}",
            case.input(), case.expected_tool, case.expected_behavior,
        ),
        Target::Skill { .. } => format!(
            "Evaluate this AI agent output against criteria.\n\n\
             TASK INPUT: {}\n\nAGENT OUTPUT:\n---\n{output}\n---\n\n\
             Criteria:\n\
             1. follows_instructions: Output follows ALL instructions in the skill prompt\n\
             2. actionable_output: Contains specific, actionable content (not vague)\n\
             3. no_hallucination: No references to unavailable tools/files/APIs\n\
             4. correct_format: Output matches expected structure\n\n\
             Respond with JSON only:\n\
             {{\"follows_instructions\": true, \"actionable_output\": true, \"no_hallucination\": true, \"correct_format\": true, \"failures\": []}}",
            case.input(),
        ),
        Target::DecisionParser => format!(
            "Evaluate whether this LLM output is a valid Decision JSON.\n\n\
             USER MESSAGE: {}\n\nLLM OUTPUT:\n---\n{output}\n---\n\n\
             Criteria:\n\
             1. valid_json: Output is valid JSON (parseable, no markdown wrapping)\n\
             2. has_all_fields: Has situation, task, tool_calls, completed\n\
             3. valid_tool_calls: tool_calls is non-empty array with tool_name + parameters\n\
             4. situation_specific: \"situation\" references the specific user message\n\
             5. task_actionable: \"task\" describes a single concrete action\n\n\
             Respond with JSON only:\n\
             {{\"valid_json\": true, \"has_all_fields\": true, \"valid_tool_calls\": true, \"situation_specific\": true, \"task_actionable\": true, \"failures\": []}}",
            case.input(),
        ),
    }
}

// ─── Mutation prompts ────────────────────────────────────────────────────────

fn mutation_prompt(
    target: &Target,
    prompt: &str,
    criteria_scores: &HashMap<String, usize>,
    failures: &[String],
    batch_size: usize,
    _best_score: i32,
    max_score: usize,
) -> String {
    let scores_text: String = criteria_scores
        .iter()
        .map(|(k, v)| format!("- {k}: {v}/{batch_size}"))
        .collect::<Vec<_>>()
        .join("\n");
    let failures_text = if failures.is_empty() {
        "- None".to_string()
    } else {
        failures.iter().take(15).map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n")
    };
    let score: usize = criteria_scores.values().sum();

    match target {
        Target::ToolSelection => format!(
            "You are optimizing tool descriptions for an AI coding agent. The agent receives a task \
             and must pick the correct tool.\n\n\
             CURRENT TOOL DESCRIPTIONS:\n---\n{prompt}\n---\n\n\
             LAST BATCH ({score}/{max_score}):\n{scores_text}\n\n\
             CONFUSION PATTERNS:\n{failures_text}\n\n\
             RULES:\n\
             - Keep descriptions concise (1-2 sentences)\n\
             - Sharpen boundaries between confused tools\n\
             - Add \"Do NOT use for X\" where confusion exists\n\
             - Keep same tool names, only modify descriptions\n\
             - Format: one line per tool, \"tool_name: description\"\n\
             - Return ONLY the new descriptions block"
        ),
        Target::SystemPrompt => format!(
            "You are optimizing a system prompt for an AI coding agent.\n\n\
             CURRENT PROMPT:\n---\n{prompt}\n---\n\n\
             LAST BATCH ({score}/{max_score}):\n{scores_text}\n\n\
             FAILURES:\n{failures_text}\n\n\
             RULES:\n\
             - Keep core identity and values\n\
             - Add explicit instructions for weak criteria\n\
             - Keep under 800 words\n\
             - Return ONLY the new system prompt"
        ),
        Target::Skill { name, .. } => format!(
            "You are optimizing a Claude Code skill prompt.\n\nSKILL: {name}\n\n\
             CURRENT PROMPT:\n---\n{prompt}\n---\n\n\
             LAST BATCH ({score}/{max_score}):\n{scores_text}\n\n\
             FAILURES:\n{failures_text}\n\n\
             RULES:\n\
             - Keep the skill's core purpose intact\n\
             - Add explicit instructions for failing criteria\n\
             - Keep YAML frontmatter unchanged\n\
             - Keep under 1500 words\n\
             - Return the COMPLETE updated skill file"
        ),
        Target::DecisionParser => format!(
            "You are optimizing a structured output schema prompt.\n\n\
             CURRENT SCHEMA:\n---\n{prompt}\n---\n\n\
             LAST BATCH ({score}/{max_score}):\n{scores_text}\n\n\
             FAILURES:\n{failures_text}\n\n\
             RULES:\n\
             - Focus on RELIABILITY across diverse inputs\n\
             - If JSON fails: stronger framing (\"ONLY JSON\", \"no markdown\")\n\
             - If fields missing: list them with types\n\
             - If tool_calls malformed: add a concrete example\n\
             - Keep under 500 words\n\
             - Return ONLY the new schema prompt"
        ),
    }
}

// ─── Runner ──────────────────────────────────────────────────────────────────

/// The autoresearch optimization engine.
pub struct AutoResearch {
    config: Config,
    gen_llm: Llm,
    eval_llm: Llm,
    test_cases: Vec<TestCase>,
    criteria: Vec<String>,
}

impl AutoResearch {
    /// Create a new autoresearch runner.
    pub fn new(config: Config) -> Self {
        let gen_llm = Llm::new(&LlmConfig::auto(&config.gen_model));
        let eval_llm = Llm::new(&LlmConfig::auto(&config.eval_model));
        let test_cases = load_test_cases(&config.target);
        let criteria = target_criteria(&config.target);

        std::fs::create_dir_all(&config.data_dir).ok();

        Self {
            config,
            gen_llm,
            eval_llm,
            test_cases,
            criteria,
        }
    }

    pub fn max_score(&self) -> usize {
        self.criteria.len() * self.config.batch_size
    }

    fn prompt_file(&self) -> PathBuf {
        self.config.data_dir.join("prompt.txt")
    }
    fn best_prompt_file(&self) -> PathBuf {
        self.config.data_dir.join("best_prompt.txt")
    }
    fn state_file(&self) -> PathBuf {
        self.config.data_dir.join("state.json")
    }
    fn results_file(&self) -> PathBuf {
        self.config.data_dir.join("results.jsonl")
    }

    fn load_state(&self) -> State {
        std::fs::read_to_string(self.state_file())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save_state(&self, state: &State) {
        if let Ok(json) = serde_json::to_string_pretty(state) {
            std::fs::write(self.state_file(), json).ok();
        }
    }

    fn load_prompt(&self) -> String {
        if let Ok(p) = std::fs::read_to_string(self.prompt_file()) {
            let trimmed = p.trim().to_string();
            if !trimmed.is_empty() {
                return trimmed;
            }
        }
        let initial = initial_prompt(&self.config.target);
        std::fs::write(self.prompt_file(), &initial).ok();
        initial
    }

    fn save_prompt(&self, prompt: &str) {
        std::fs::write(self.prompt_file(), prompt).ok();
    }

    /// Select test cases for this cycle (deterministic rotation).
    fn select_cases(&self, run: u32) -> Vec<TestCase> {
        if self.test_cases.is_empty() {
            return vec![];
        }
        let n = self.test_cases.len();
        let offset = (run as usize).wrapping_mul(7) % n; // prime stride for coverage
        let mut cases = Vec::with_capacity(self.config.batch_size);
        for i in 0..self.config.batch_size {
            let idx = (offset + i * 3) % n; // stride of 3 for variety
            cases.push(self.test_cases[idx].clone());
        }
        cases
    }

    /// Run one optimization cycle.
    pub async fn run_cycle(&self) -> Result<CycleResult, SgrError> {
        let mut state = self.load_state();
        let run_num = state.run_number + 1;
        state.run_number = run_num;
        let mx = self.max_score();

        info!(
            "============================================================\n\
             RUN {} | Best: {}/{} | Target: {}\n\
             ============================================================",
            run_num,
            state.best_score,
            mx,
            self.config.target.name(),
        );

        let prompt = self.load_prompt();
        let cases = self.select_cases(run_num);

        // ── Generate ─────────────────────────────────────────────
        info!("Generating {} outputs...", cases.len());
        let mut outputs: Vec<(TestCase, String)> = Vec::new();

        for case in &cases {
            let gp = gen_prompt(&self.config.target, &prompt, case);
            match self.gen_llm.generate(&[Message::user(gp)]).await {
                Ok(text) => outputs.push((case.clone(), text.trim().to_string())),
                Err(e) => {
                    warn!("GEN ERROR: {e}");
                    outputs.push((case.clone(), String::new()));
                }
            }
        }

        if outputs.iter().all(|(_, o)| o.is_empty()) {
            warn!("No outputs generated. Skipping cycle.");
            self.save_state(&state);
            return Ok(CycleResult {
                run: run_num,
                score: 0,
                max_score: mx,
                improved: false,
                criteria: HashMap::new(),
            });
        }

        // ── Evaluate ─────────────────────────────────────────────
        info!("Evaluating {} outputs...", outputs.len());
        let mut eval_results: Vec<HashMap<String, bool>> = Vec::new();
        let mut all_failures: Vec<String> = Vec::new();

        for (i, (case, output)) in outputs.iter().enumerate() {
            if output.is_empty() {
                eval_results.push(self.criteria.iter().map(|c| (c.clone(), false)).collect());
                continue;
            }

            // Tool selection: simple string match, no LLM eval needed
            if matches!(self.config.target, Target::ToolSelection) {
                let picked = output
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_lowercase();
                let picked = picked.split_whitespace().next().unwrap_or("");
                let correct = picked == case.expected;
                if !correct {
                    all_failures.push(format!(
                        "\"{}\" → picked: {picked}, expected: {}",
                        case.input(),
                        case.expected
                    ));
                }
                info!(
                    "  [{}/{}] {} | {}",
                    i + 1,
                    outputs.len(),
                    if correct { "PASS" } else { "FAIL" },
                    case.input()
                );
                let mut r = HashMap::new();
                r.insert("correct_tool".into(), correct);
                eval_results.push(r);
                continue;
            }

            // Decision parser: local JSON check first
            if matches!(self.config.target, Target::DecisionParser) {
                let valid = try_parse_decision_json(output);
                if !valid {
                    info!("  [{}/{}] FAIL (invalid JSON) | {}", i + 1, outputs.len(), case.input());
                    eval_results.push(self.criteria.iter().map(|c| (c.clone(), false)).collect());
                    all_failures.push(format!("Invalid JSON for: {}", case.input()));
                    continue;
                }
            }

            // LLM-based eval
            let ep = eval_prompt(&self.config.target, output, case);
            match self.eval_llm.generate(&[Message::user(ep)]).await {
                Ok(resp) => match parse_eval_json(&resp, &self.criteria) {
                    Ok((result, failures)) => {
                        let passes = self
                            .criteria
                            .iter()
                            .filter(|c| *result.get(c.as_str()).unwrap_or(&false))
                            .count();
                        let tag = if failures.is_empty() {
                            "all pass".to_string()
                        } else {
                            failures.join("; ")
                        };
                        info!("  [{}/{}] {}/{} | {tag}", i + 1, outputs.len(), passes, self.criteria.len());
                        all_failures.extend(failures);
                        eval_results.push(result);
                    }
                    Err(e) => {
                        warn!("  [{}/{}] PARSE ERROR: {e}", i + 1, outputs.len());
                        eval_results.push(self.criteria.iter().map(|c| (c.clone(), false)).collect());
                    }
                },
                Err(e) => {
                    warn!("  [{}/{}] EVAL ERROR: {e}", i + 1, outputs.len());
                    eval_results.push(self.criteria.iter().map(|c| (c.clone(), false)).collect());
                }
            }
        }

        // ── Score ────────────────────────────────────────────────
        let mut criterion_scores: HashMap<String, usize> = HashMap::new();
        for c in &self.criteria {
            let count = eval_results
                .iter()
                .filter(|r| *r.get(c.as_str()).unwrap_or(&false))
                .count();
            criterion_scores.insert(c.clone(), count);
        }
        let score: usize = criterion_scores.values().sum();

        info!("SCORE: {score}/{mx}");
        for (c, s) in &criterion_scores {
            info!("  {c}: {s}/{}", self.config.batch_size);
        }

        // ── Log ──────────────────────────────────────────────────
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let log = RunLog {
            run: run_num,
            timestamp: format!("{ts}"),
            score,
            max: mx,
            criteria: criterion_scores.clone(),
            prompt_len: prompt.len(),
            generated: outputs.len(),
        };
        if let Ok(line) = serde_json::to_string(&log) {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.results_file())
            {
                writeln!(f, "{line}").ok();
            }
        }

        // ── Keep or discard ──────────────────────────────────────
        let improved = score as i32 > state.best_score;
        if improved {
            let old = state.best_score;
            state.best_score = score as i32;
            std::fs::write(self.best_prompt_file(), &prompt).ok();
            info!("NEW BEST! {score}/{mx} (was {old})");
        } else {
            info!("No improvement ({score} vs best {})", state.best_score);
        }

        // ── Mutate ───────────────────────────────────────────────
        if score < mx {
            info!("Mutating prompt...");
            let base = std::fs::read_to_string(self.best_prompt_file())
                .unwrap_or_else(|_| prompt.clone());
            let mp = mutation_prompt(
                &self.config.target,
                &base,
                &criterion_scores,
                &all_failures,
                self.config.batch_size,
                state.best_score,
                mx,
            );
            match self.eval_llm.generate(&[Message::user(mp)]).await {
                Ok(new_prompt) => {
                    let new_prompt = new_prompt.trim().to_string();
                    self.save_prompt(&new_prompt);
                    let preview: String = new_prompt.chars().take(200).collect();
                    info!("New prompt ({} chars): {preview}...", new_prompt.len());
                }
                Err(e) => warn!("MUTATE ERROR: {e}"),
            }
        } else {
            info!("PERFECT {mx}/{mx}! Fully optimized.");
        }

        self.save_state(&state);

        Ok(CycleResult {
            run: run_num,
            score,
            max_score: mx,
            improved,
            criteria: criterion_scores,
        })
    }

    /// Run multiple optimization cycles.
    pub async fn run(&self, cycles: usize) -> Result<(), SgrError> {
        let state = self.load_state();
        info!(
            "AutoResearch: {}\n  Batch: {}\n  Criteria: {}\n  Max: {}\n  Cycle: {}s\n  State: run {}, best {}/{}",
            self.config.target.name(),
            self.config.batch_size,
            self.criteria.join(", "),
            self.max_score(),
            self.config.cycle_secs,
            state.run_number,
            state.best_score,
            self.max_score(),
        );

        let max_cycles = if cycles == 0 { usize::MAX } else { cycles };

        for i in 0..max_cycles {
            let start = std::time::Instant::now();
            match self.run_cycle().await {
                Ok(result) => {
                    if result.score == result.max_score {
                        info!("Perfect score reached. Stopping.");
                        break;
                    }
                }
                Err(e) => warn!("CYCLE ERROR: {e}"),
            }

            if i + 1 < max_cycles {
                let elapsed = start.elapsed().as_secs();
                let wait = self.config.cycle_secs.saturating_sub(elapsed);
                if wait > 0 {
                    info!("Waiting {wait}s until next cycle...");
                    tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                }
            }
        }

        let final_state = self.load_state();
        info!(
            "Done. Best: {}/{}. Prompt: {:?}",
            final_state.best_score,
            self.max_score(),
            self.best_prompt_file()
        );
        Ok(())
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Try to parse Decision JSON from output (for quick local validation).
fn try_parse_decision_json(text: &str) -> bool {
    let cleaned = strip_markdown_fences(text);
    serde_json::from_str::<serde_json::Value>(&cleaned)
        .map(|v| v.is_object())
        .unwrap_or(false)
}

/// Parse evaluation JSON response from LLM.
fn parse_eval_json(
    text: &str,
    criteria: &[String],
) -> Result<(HashMap<String, bool>, Vec<String>), String> {
    let cleaned = strip_markdown_fences(text);
    let value: serde_json::Value =
        serde_json::from_str(&cleaned).map_err(|e| format!("JSON parse: {e}"))?;

    let obj = value.as_object().ok_or("Expected JSON object")?;
    let mut result = HashMap::new();
    for c in criteria {
        result.insert(c.clone(), obj.get(c).and_then(|v| v.as_bool()).unwrap_or(false));
    }

    let failures: Vec<String> = obj
        .get("failures")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    Ok((result, failures))
}

/// Strip markdown code fences from LLM output.
fn strip_markdown_fences(text: &str) -> String {
    let text = text.trim();
    if text.contains("```") {
        let parts: Vec<&str> = text.split("```").collect();
        if parts.len() >= 3 {
            let inner = parts[1];
            // Skip language tag (e.g., "json\n")
            if let Some(pos) = inner.find('\n') {
                return inner[pos + 1..].trim().to_string();
            }
            return inner.trim().to_string();
        }
    }
    text.to_string()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_tool_selection_cases() {
        let cases = load_test_cases(&Target::ToolSelection);
        assert!(cases.len() >= 60, "Expected 60+ tool selection tests, got {}", cases.len());
        assert!(!cases[0].task.is_empty());
        assert!(!cases[0].expected.is_empty());
    }

    #[test]
    fn test_load_system_prompt_cases() {
        let cases = load_test_cases(&Target::SystemPrompt);
        assert!(cases.len() >= 15, "Expected 15+ system prompt tests, got {}", cases.len());
        assert!(!cases[0].expected_tool.is_empty());
    }

    #[test]
    fn test_load_decision_parser_cases() {
        let cases = load_test_cases(&Target::DecisionParser);
        assert!(cases.len() >= 25, "Expected 25+ decision parser tests, got {}", cases.len());
        assert!(!cases[0].message.is_empty());
    }

    #[test]
    fn test_load_skill_default_cases() {
        let cases = load_test_cases(&Target::Skill {
            name: "test".into(),
            skill_path: None,
        });
        assert_eq!(cases.len(), 5);
    }

    #[test]
    fn test_target_criteria() {
        assert_eq!(target_criteria(&Target::ToolSelection).len(), 1);
        assert_eq!(target_criteria(&Target::SystemPrompt).len(), 4);
        assert_eq!(
            target_criteria(&Target::Skill {
                name: "x".into(),
                skill_path: None
            })
            .len(),
            4
        );
        assert_eq!(target_criteria(&Target::DecisionParser).len(), 5);
    }

    #[test]
    fn test_strip_markdown_fences() {
        assert_eq!(
            strip_markdown_fences("```json\n{\"a\": 1}\n```"),
            "{\"a\": 1}"
        );
        assert_eq!(strip_markdown_fences("{\"a\": 1}"), "{\"a\": 1}");
        assert_eq!(
            strip_markdown_fences("some text ```\nhello\n``` more"),
            "hello"
        );
    }

    #[test]
    fn test_parse_eval_json() {
        let criteria = vec!["correct_tool".to_string()];
        let json = r#"{"correct_tool": true, "failures": []}"#;
        let (result, failures) = parse_eval_json(json, &criteria).unwrap();
        assert!(result["correct_tool"]);
        assert!(failures.is_empty());
    }

    #[test]
    fn test_parse_eval_json_with_fences() {
        let criteria = vec!["valid_json".to_string(), "has_all_fields".to_string()];
        let json = "```json\n{\"valid_json\": true, \"has_all_fields\": false, \"failures\": [\"missing task\"]}\n```";
        let (result, failures) = parse_eval_json(json, &criteria).unwrap();
        assert!(result["valid_json"]);
        assert!(!result["has_all_fields"]);
        assert_eq!(failures, vec!["missing task"]);
    }

    #[test]
    fn test_try_parse_decision_json() {
        assert!(try_parse_decision_json(r#"{"situation": "x", "task": "y", "tool_calls": []}"#));
        assert!(!try_parse_decision_json("not json"));
        assert!(try_parse_decision_json("```json\n{\"a\": 1}\n```"));
    }

    #[test]
    fn test_select_cases_coverage() {
        let config = Config {
            batch_size: 5,
            ..Default::default()
        };
        let ar = AutoResearch {
            config,
            gen_llm: Llm::new(&LlmConfig::auto("test")),
            eval_llm: Llm::new(&LlmConfig::auto("test")),
            test_cases: load_test_cases(&Target::ToolSelection),
            criteria: target_criteria(&Target::ToolSelection),
        };

        let cases1 = ar.select_cases(1);
        let cases2 = ar.select_cases(2);
        assert_eq!(cases1.len(), 5);
        assert_eq!(cases2.len(), 5);
        // Different runs should select different cases
        let inputs1: Vec<_> = cases1.iter().map(|c| c.input()).collect();
        let inputs2: Vec<_> = cases2.iter().map(|c| c.input()).collect();
        assert_ne!(inputs1, inputs2);
    }

    #[test]
    fn test_state_default() {
        let s = State::default();
        assert_eq!(s.best_score, -1);
        assert_eq!(s.run_number, 0);
    }

    #[test]
    fn test_gen_prompt_tool_selection() {
        let case = TestCase {
            task: "Run tests".into(),
            expected: "bash".into(),
            ..tc_empty()
        };
        let prompt = gen_prompt(&Target::ToolSelection, "bash: Run commands", &case);
        assert!(prompt.contains("Run tests"));
        assert!(prompt.contains("bash: Run commands"));
    }
}
