#!/usr/bin/env python3
"""
AutoResearch Base — Generic self-improving optimization loop.

Karpathy autoresearch pattern:
  Generate → Evaluate → Score → Keep/Discard → Mutate → Repeat
"""

import argparse
import json
import time
import traceback
from abc import ABC, abstractmethod
from datetime import datetime
from pathlib import Path


class AutoResearchTarget(ABC):
    """Base class for autoresearch optimization targets."""

    name: str
    criteria: list[str]
    batch_size: int = 10

    def __init__(self, data_dir: Path | None = None):
        base = Path(__file__).resolve().parent / "data"
        self.data_dir = data_dir or base / self.name
        self.prompt_file = self.data_dir / "prompt.txt"
        self.best_prompt_file = self.data_dir / "best_prompt.txt"
        self.state_file = self.data_dir / "state.json"
        self.results_file = self.data_dir / "results.jsonl"
        self.data_dir.mkdir(parents=True, exist_ok=True)

    @abstractmethod
    def generate_batch(self, prompt: str) -> list[dict]:
        """Generate a batch of outputs with current prompt."""
        ...

    @abstractmethod
    def evaluate_one(self, output: dict) -> dict[str, bool]:
        """Evaluate one output. Returns {criterion_name: True/False}."""
        ...

    @abstractmethod
    def mutate(self, prompt: str, results: list[dict[str, bool]], best_score: int) -> str:
        """Improve the prompt based on eval results."""
        ...

    @abstractmethod
    def get_initial_prompt(self) -> str:
        """Return the starting prompt to optimize."""
        ...

    @property
    def max_score(self) -> int:
        return len(self.criteria) * self.batch_size

    def load_state(self) -> dict:
        if self.state_file.exists():
            return json.loads(self.state_file.read_text())
        return {"best_score": -1, "run_number": 0}

    def save_state(self, state: dict):
        self.state_file.write_text(json.dumps(state, indent=2))

    def load_prompt(self) -> str:
        if self.prompt_file.exists():
            return self.prompt_file.read_text().strip()
        initial = self.get_initial_prompt()
        self.prompt_file.write_text(initial)
        return initial

    def save_prompt(self, prompt: str):
        self.prompt_file.write_text(prompt)


class AutoResearchRunner:
    """Generic autoresearch loop runner."""

    def __init__(self, target: AutoResearchTarget, cycle_seconds: int = 120):
        self.target = target
        self.cycle_seconds = cycle_seconds

    def run_cycle(self, state: dict) -> dict:
        run_num = state["run_number"] + 1
        state["run_number"] = run_num
        t = self.target
        mx = t.max_score

        print(f"\n{'=' * 60}")
        print(f"RUN {run_num} | {datetime.now().strftime('%H:%M:%S')} | Best: {state['best_score']}/{mx}")
        print(f"Target: {t.name}")
        print(f"{'=' * 60}")

        # ── Generate ──
        print(f"\n  Generating {t.batch_size} outputs...")
        prompt = t.load_prompt()
        outputs = t.generate_batch(prompt)

        if not outputs:
            print("  ERROR: No outputs generated. Skipping cycle.")
            t.save_state(state)
            return state

        # ── Evaluate ──
        print(f"\n  Evaluating {len(outputs)} outputs...")
        eval_results: list[dict[str, bool]] = []

        for i, output in enumerate(outputs):
            try:
                result = t.evaluate_one(output)
                eval_results.append(result)
                passes = sum(1 for c in t.criteria if result.get(c, False))
                total = len(t.criteria)
                fails = [c for c in t.criteria if not result.get(c, False)]
                tag = "; ".join(fails) if fails else "all pass"
                print(f"    [{i + 1}/{len(outputs)}] {passes}/{total} | {tag}")
            except Exception as e:
                print(f"    [{i + 1}/{len(outputs)}] ERROR: {e}")
                eval_results.append({c: False for c in t.criteria})

        # ── Score ──
        criterion_scores: dict[str, int] = {}
        for c in t.criteria:
            criterion_scores[c] = sum(1 for r in eval_results if r.get(c, False))
        score = sum(criterion_scores.values())

        print(f"\n  SCORE: {score}/{mx}")
        for c, s in criterion_scores.items():
            print(f"    {c}: {s}/{t.batch_size}")

        # ── Log ──
        log_entry = {
            "run": run_num,
            "timestamp": datetime.now().isoformat(),
            "score": score,
            "max": mx,
            "criteria": criterion_scores,
            "prompt_len": len(prompt),
            "generated": len(outputs),
        }
        with open(t.results_file, "a") as f:
            f.write(json.dumps(log_entry) + "\n")

        # ── Keep or discard ──
        if score > state["best_score"]:
            old = state["best_score"]
            state["best_score"] = score
            t.best_prompt_file.write_text(prompt)
            print(f"\n  NEW BEST! {score}/{mx} (was {old})")
        else:
            print(f"\n  No improvement ({score} vs best {state['best_score']})")

        # ── Mutate ──
        if score < mx:
            print("\n  Mutating prompt...")
            base = t.best_prompt_file.read_text().strip() if t.best_prompt_file.exists() else prompt
            new_prompt = t.mutate(base, eval_results, state["best_score"])
            t.save_prompt(new_prompt)
            preview = new_prompt[:200].replace("\n", " ")
            print(f"  New prompt ({len(new_prompt)} chars): {preview}...")
        else:
            print(f"\n  PERFECT {mx}/{mx}! Fully optimized.")

        t.save_state(state)
        return state

    def run(self, cycles: int = 0, once: bool = False):
        t = self.target
        state = t.load_state()

        print(f"AutoResearch: {t.name}")
        print(f"  Batch size: {t.batch_size}")
        print(f"  Criteria:   {', '.join(t.criteria)}")
        print(f"  Max score:  {t.max_score}")
        print(f"  Cycle:      {self.cycle_seconds}s")
        print(f"  State:      run {state['run_number']}, best {state['best_score']}/{t.max_score}")

        if once:
            self.run_cycle(state)
            return

        max_cycles = cycles or float("inf")
        i = 0
        while i < max_cycles:
            start = time.time()
            try:
                state = self.run_cycle(state)
            except Exception as e:
                print(f"\n  CYCLE ERROR: {e}")
                traceback.print_exc()
            elapsed = time.time() - start
            i += 1

            if i < max_cycles:
                wait = max(0, self.cycle_seconds - elapsed)
                if wait > 0:
                    print(f"\n  Waiting {wait:.0f}s until next cycle...")
                    time.sleep(wait)
                else:
                    print(f"\n  Cycle took {elapsed:.0f}s (>{self.cycle_seconds}s budget)")

        print(f"\nDone. Best score: {state['best_score']}/{t.max_score}")
        if t.best_prompt_file.exists():
            print(f"Best prompt: {t.best_prompt_file}")


def base_argparser(description: str) -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=description)
    parser.add_argument("--once", action="store_true", help="Run a single cycle")
    parser.add_argument("--cycles", type=int, default=0, help="Run N cycles (0=infinite)")
    parser.add_argument("--interval", type=int, default=120, help="Seconds between cycles")
    parser.add_argument("--batch", type=int, default=10, help="Outputs per cycle")
    return parser
