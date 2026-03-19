#!/usr/bin/env python3
"""
AutoResearch Target: Tool Selection — optimize tool descriptions for correct tool choice.

Generates N tasks, asks LLM to pick a tool, compares with expected.
Mutates tool descriptions to improve selection accuracy.

Usage:
    python3 tool_selection.py --once
    python3 tool_selection.py --cycles 20
"""

import json
import os
import random
import sys
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

from base import AutoResearchRunner, AutoResearchTarget, base_argparser

TEST_CASES_FILE = Path(__file__).parent / "test_cases" / "tool_selection.json"

# Use same model agent uses for generation, different model for eval
GEN_MODEL = "gemini-2.5-flash"
EVAL_MODEL = "claude-sonnet-4-6"

SELECTION_PROMPT = """You are an AI coding agent. Given a user task, select the SINGLE most appropriate tool to use as the FIRST action.

Available tools:
{tool_descriptions}

User task: {task}

Respond with ONLY the tool name, nothing else. Example: read_file"""

MUTATION_TEMPLATE = """I have tool descriptions for an AI coding agent. The agent picks a tool based on these descriptions, but some picks are wrong.

CURRENT TOOL DESCRIPTIONS:
---
{current_prompt}
---

WRONG PICKS ({correct}/{total} correct):
{confusions}

Task: For EACH wrong pick above, write a REPLACEMENT line for the tool that SHOULD have been picked. Make its description clearer so the agent picks it next time. Add "Do NOT use for X" constraints if needed.

Output format — one replacement per line:
tool_name: improved description here

ONLY output replacement lines for confused tools. Do NOT rewrite all tools."""


class ToolSelectionTarget(AutoResearchTarget):
    name = "tool-selection"
    criteria = ["correct_tool"]

    def __init__(self, batch_size: int = 10):
        super().__init__()
        self.batch_size = batch_size
        self.test_cases = json.loads(TEST_CASES_FILE.read_text())
        self._last_outputs: list[dict] = []
        self.anthropic = None
        self.genai = None
        self.use_gemini = False

        # Init clients — prefer Gemini (what agent uses), Anthropic for eval if available
        try:
            from google import genai
            self.genai = genai.Client(api_key=os.environ["GEMINI_API_KEY"])
            self.use_gemini = True
        except (ImportError, KeyError):
            pass

        try:
            import anthropic
            if os.getenv("ANTHROPIC_API_KEY"):
                self.anthropic = anthropic.Anthropic()
        except ImportError:
            pass

        if not self.use_gemini and not self.anthropic:
            raise RuntimeError("Need at least GEMINI_API_KEY or ANTHROPIC_API_KEY")
        if not self.anthropic:
            print("  Note: No ANTHROPIC_API_KEY, using Gemini for mutation too")

    def get_initial_prompt(self) -> str:
        return """read_file: Read file contents. Use offset/limit for large files.
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
delegate_result: Get output from a completed delegate."""

    def _generate_one(self, prompt: str, case: dict) -> dict:
        task_text = case["task"]
        full_prompt = SELECTION_PROMPT.format(tool_descriptions=prompt, task=task_text)
        try:
            if self.use_gemini:
                from google.genai import types
                resp = self.genai.models.generate_content(
                    model=GEN_MODEL,
                    contents=full_prompt,
                    config=types.GenerateContentConfig(temperature=0.0, max_output_tokens=50),
                )
                raw = resp.text or ""
                if not raw.strip():
                    return {"task": task_text, "expected": case["expected"], "picked": "__empty__"}
                picked = raw.strip().lower().split("\n")[0].split()[0]
            elif self.anthropic:
                resp = self.anthropic.messages.create(
                    model=EVAL_MODEL, max_tokens=50, temperature=0.0,
                    messages=[{"role": "user", "content": full_prompt}],
                )
                picked = resp.content[0].text.strip().lower().split("\n")[0].split()[0]
            else:
                raise RuntimeError("No LLM client available")
            return {"task": task_text, "expected": case["expected"], "picked": picked}
        except Exception as e:
            print(f"    GEN ERROR for '{task_text[:40]}': {e}")
            return {"task": task_text, "expected": case["expected"], "picked": "__error__"}

    def generate_batch(self, prompt: str) -> list[dict]:
        cases = random.sample(self.test_cases, min(self.batch_size, len(self.test_cases)))
        results = []
        # Parallel generation
        with ThreadPoolExecutor(max_workers=5) as pool:
            futures = {pool.submit(self._generate_one, prompt, c): c for c in cases}
            for f in as_completed(futures):
                results.append(f.result())
        self._last_outputs = results
        return results

    def evaluate_one(self, output: dict) -> dict[str, bool]:
        """Binary: did the model pick the expected tool? No LLM needed."""
        return {"correct_tool": output["picked"] == output["expected"]}

    def mutate(self, prompt: str, results: list[dict[str, bool]], best_score: int) -> str:
        # Skip empty outputs from scoring confusions
        valid = [o for o in self._last_outputs if o["picked"] not in ("__error__", "__empty__")]
        confusions = []
        for output in valid:
            if output["picked"] != output["expected"]:
                confusions.append(
                    f"  Task: \"{output['task']}\" → picked: {output['picked']}, expected: {output['expected']}"
                )
        confusion_text = "\n".join(confusions[:15]) if confusions else "  None"
        correct = sum(1 for o in valid if o["picked"] == o["expected"])

        mutation_prompt = MUTATION_TEMPLATE.format(
            current_prompt=prompt,
            score=best_score,
            max_score=self.max_score,
            correct=correct,
            total=len(valid),
            confusions=confusion_text,
        )
        if self.anthropic:
            resp = self.anthropic.messages.create(
                model=EVAL_MODEL, max_tokens=2048,
                messages=[{"role": "user", "content": mutation_prompt}],
            )
            patches_text = resp.content[0].text.strip()
        else:
            from google.genai import types
            resp = self.genai.models.generate_content(
                model=GEN_MODEL, contents=mutation_prompt,
                config=types.GenerateContentConfig(max_output_tokens=2048),
            )
            patches_text = (resp.text or "").strip()

        # Strip markdown fences
        if "```" in patches_text:
            lines = patches_text.split("\n")
            lines = [l for l in lines if not l.startswith("```")]
            patches_text = "\n".join(lines).strip()

        # Apply patches: replace matching tool lines in original prompt
        patched = prompt
        for line in patches_text.split("\n"):
            line = line.strip()
            if ":" not in line:
                continue
            tool_name = line.split(":")[0].strip()
            # Find and replace matching tool line in prompt
            for orig_line in prompt.split("\n"):
                if orig_line.strip().startswith(tool_name + ":"):
                    patched = patched.replace(orig_line.strip(), line)
                    print(f"  PATCH: {tool_name}")
                    break

        if patched == prompt:
            print(f"  WARNING: No patches applied")
        return patched


def main():
    parser = base_argparser("AutoResearch: Tool Selection Optimization")
    args = parser.parse_args()

    if not os.getenv("GEMINI_API_KEY") and not os.getenv("ANTHROPIC_API_KEY"):
        print("ERROR: Need GEMINI_API_KEY or ANTHROPIC_API_KEY", file=sys.stderr)
        sys.exit(1)

    target = ToolSelectionTarget(batch_size=args.batch)
    runner = AutoResearchRunner(target, cycle_seconds=args.interval)
    runner.run(cycles=args.cycles, once=args.once)


if __name__ == "__main__":
    main()
