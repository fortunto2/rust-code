# sgr-agent-tui

[![Crates.io](https://img.shields.io/crates/v/sgr-agent-tui)](https://crates.io/crates/sgr-agent-tui)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Shared TUI shell for [sgr-agent](https://crates.io/crates/sgr-agent) — reusable widgets, event loop, and patterns for building terminal AI coding assistants with [ratatui](https://ratatui.rs).

## Modules

| Module | What |
|--------|------|
| `focus` | **FocusLayer trait** — input routing through a stack of UI layers |
| `command_palette` | **CommandPalette** — `/`-triggered autocomplete popup with FocusLayer impl |
| `chat` | Chat panel with streaming, scroll, expand/collapse |
| `picker` | Fuzzy picker overlay (nucleo-matcher, channels, preview panel) |
| `help` | Help overlay (keybindings display) |
| `context_bar` | Project context bar (git branch, model, tokens) |
| `event` | `AppEvent<T>` enum — terminal events, ticks, agent messages |
| `agent_task` | `TuiAgent` trait + `spawn_agent_loop` — async agent integration |
| `headless` | Non-interactive mode (pipe-friendly) |
| `terminal` | Terminal init/restore, panic hook, OTEL telemetry |

## Focus Layer Pattern

The `FocusLayer` trait solves a common TUI problem: multiple overlapping UI components (popups, modals, pickers) that need to intercept keyboard and mouse input with correct priority.

Each component implements `FocusLayer`:

```rust
use sgr_agent_tui::focus::{FocusLayer, FocusResult};

impl FocusLayer for MyPopup {
    fn is_active(&self) -> bool { self.visible }
    fn on_key(&mut self, key: KeyEvent) -> FocusResult {
        match key.code {
            KeyCode::Esc => { self.close(); FocusResult::Consumed }
            KeyCode::Up | KeyCode::Down => { self.navigate(key); FocusResult::Consumed }
            _ => FocusResult::Passed
        }
    }
    fn on_mouse(&mut self, mouse: MouseEvent) -> FocusResult { FocusResult::Passed }
    fn hit_rect(&self) -> Option<Rect> { self.popup_rect }
}
```

Route events through the stack (highest priority first):

```rust
use sgr_agent_tui::focus::{route_key, route_mouse};

let result = route_key(&mut [&mut help, &mut picker, &mut slash_popup], key_event);
if result.consumed() { return; }
```

## Telemetry

TUI apps should use `init_tui_telemetry()` which redirects stderr to a file:

```rust
use sgr_agent_tui::init_tui_telemetry;
let _guard = init_tui_telemetry(".my-agent", "tui");
```

For headless/CLI mode, use `sgr_agent::init_telemetry()` directly (no stderr redirect needed).

## Usage

```toml
[dependencies]
sgr-agent-tui = "0.4"
```

```rust
use sgr_agent_tui::{
    AppEvent, ChatState, CommandPalette, FuzzyPicker, HelpOverlay,
    FocusLayer, FocusResult, route_key, route_mouse,
    init_terminal, restore_terminal, setup_panic_hook,
};
```

### Projects using this crate

- **rust-code** (`rc-cli/src/app.rs`) — AI coding agent TUI
