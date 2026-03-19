---
name: autoresearch
description: Self-improving optimization via Karpathy autoresearch pattern. Generates → evaluates → scores → mutates prompts/descriptions in a loop. Targets — tool-selection, system-prompt, skill, decision-parser. Use when "optimize tools", "autoresearch", "improve skill X", "self-improve prompts", "optimize tool descriptions".
allowed-tools: bash, bash_bg, read_file, write_file, search_code, finish
argument-hint: "<target: tool-selection|system-prompt|skill|decision-parser> [--cycles N] [--once]"
---

# /autoresearch — Self-Improving Prompt Optimization

Karpathy autoresearch pattern: **Generate → Evaluate → Score → Keep/Discard → Mutate → Repeat**

## Architecture

Core engine: `crates/sgr-agent/src/autoresearch.rs` (Rust module, feature `genai`).
Dashboard: `skills/autoresearch/dashboard.py` (Python, standalone viewer).
Test cases: `skills/autoresearch/test_cases/*.json` (embedded at compile time via `include_str!`).

## Targets

| Target | What it optimizes | Eval metric |
|--------|------------------|-------------|
| `tool-selection` | Tool descriptions in agent | % correct tool chosen for 66 test tasks |
| `system-prompt` | Agent system prompt (SOUL.md) | Decision quality: right tool + coherent reasoning (4 criteria) |
| `skill` | Any SKILL.md file | Binary criteria pass rate (4 criteria) |
| `decision-parser` | Structured output schema | Parse success + field quality (5 criteria) |

## Usage (Rust API)

```rust
use sgr_agent::autoresearch::{AutoResearch, Config, Target};

let config = Config {
    target: Target::ToolSelection,
    batch_size: 10,
    cycle_secs: 120,
    gen_model: "gemini-2.5-flash".into(),
    eval_model: "claude-sonnet-4-6".into(),
    data_dir: "autoresearch_data/tool-selection".into(),
};
let ar = AutoResearch::new(config);
ar.run(20).await?; // 20 cycles
```

## Quick Start (CLI — when wired into rc-cli)

```bash
# Tool selection optimization (most impactful)
cargo run -- autoresearch tool-selection --cycles 20

# System prompt optimization
cargo run -- autoresearch system-prompt --cycles 10

# Optimize a specific skill
cargo run -- autoresearch skill --name delegate --cycles 15

# Optimize decision parsing
cargo run -- autoresearch decision-parser --cycles 10

# Dashboard (Python, standalone)
python3 skills/autoresearch/dashboard.py --target tool-selection --port 8501
```

## How It Works

Each cycle:
1. **Generate** N outputs with current prompt (via Gemini — same model agent uses)
2. **Evaluate** each output against binary criteria (via Claude Sonnet — different model for objectivity)
3. **Score** = sum of passed criteria across all outputs
4. **Keep** if score > best_score, **discard** otherwise
5. **Mutate** the winning prompt to try improvements
6. **Log** to JSONL for dashboard tracking
7. **Wait** for next cycle (default 2 min)

## Environment

- `GEMINI_API_KEY` — for generation (tests with same model the agent uses)
- `ANTHROPIC_API_KEY` — for evaluation and mutation (different model for objectivity)

## Tips

- **Binary evals** — yes/no criteria work best. Avoid Likert scales.
- **Don't over-constrain** — too many narrow criteria → model games the eval.
- **10-20 cycles** usually enough for significant improvement.
- **Apply results** — best prompt saved to `data/<target>/best_prompt.txt`.

## Cost

- Tool selection: ~$0.05/cycle (cheap — one LLM call per test, string match eval)
- System prompt: ~$0.20/cycle (gen + LLM eval per test)
- Skill: ~$0.30/cycle (gen + LLM eval per test, longer outputs)
- Decision parser: ~$0.15/cycle (gen + partial local eval)
- 20 cycles ≈ $1-6 depending on target. Permanent improvement.
