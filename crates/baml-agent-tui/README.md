# baml-agent-tui

Shared TUI shell for BAML SGR agents — reusable widgets, event loop, and patterns for building terminal AI coding assistants with [ratatui](https://ratatui.rs).

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

### Problem

Without a focus system, input handling becomes a mess of nested `if` checks:

```rust
// Bad: order-dependent spaghetti, easy to break
if help.visible {
    if key == Esc { help.close(); return; }
    return; // swallow all keys
}
if picker.visible {
    match key { ... }
    return;
}
if !slash_suggestions.is_empty() {
    match key { ... }
}
// finally, normal input handling
```

### Solution

Each component implements `FocusLayer`:

```rust
use baml_agent_tui::focus::{FocusLayer, FocusResult};

impl FocusLayer for MyPopup {
    fn is_active(&self) -> bool {
        self.visible
    }

    fn on_key(&mut self, key: KeyEvent) -> FocusResult {
        match key.code {
            KeyCode::Esc => { self.close(); FocusResult::Consumed }
            KeyCode::Up | KeyCode::Down => { self.navigate(key); FocusResult::Consumed }
            _ => FocusResult::Passed // let typing fall through
        }
    }

    fn on_mouse(&mut self, mouse: MouseEvent) -> FocusResult {
        // optional: handle scroll/click inside popup
        FocusResult::Passed
    }

    fn hit_rect(&self) -> Option<Rect> {
        // optional: bounding box for mouse hit testing
        self.popup_rect
    }
}
```

Route events through the stack (highest priority first):

```rust
use baml_agent_tui::focus::{route_key, route_mouse};

// In your event loop:
let result = route_key(
    &mut [&mut help, &mut picker, &mut slash_popup],
    key_event,
);
if result.consumed() { return; }

// Normal input handling below — no popup is active
```

### Key Design Decisions

- **`Consumed` vs `Passed`** — a layer can handle some keys and let others fall through (e.g. popup consumes arrows but passes typing to the input)
- **Modal layers** return `Consumed` for all keys (help overlay, fuzzy picker)
- **Non-modal layers** only consume relevant keys (slash autocomplete consumes Tab/arrows, passes chars)
- **Mouse uses `hit_rect()`** — only events inside the bounding box are routed to the layer

### Current Implementations

| Component | Modal? | Consumes | Passes |
|-----------|--------|----------|--------|
| `HelpOverlay` | Yes | All keys (Esc/q closes) | Nothing |
| `FuzzyPicker` | Yes | All keys (delegates to `on_key`) | Nothing |
| `CommandPalette` | No | Tab, Enter, Up, Down, Esc | Char input |

### `take_applied()` pattern

`FocusLayer::on_key()` returns only `Consumed/Passed`. For components like `CommandPalette` where the caller needs to know *what* was selected, use `take_applied()`:

```rust
if self.command_palette.on_key(key_event).consumed() {
    if let Some(cmd) = self.command_palette.take_applied() {
        self.set_input_text(cmd); // apply selected command to input
    }
    return;
}
```

Same for mouse events:
```rust
if self.command_palette.on_mouse(mouse_event).consumed() {
    if let Some(cmd) = self.command_palette.take_applied() {
        self.set_input_text(cmd);
    }
    continue;
}
```

## What's Missing / TODO

### Focus System
- [x] ~~**SlashPopup as FocusLayer**~~ — done: `CommandPalette` widget with FocusLayer impl
- [x] ~~**Wire in app.rs**~~ — done: `on_key()`/`on_mouse()` replace manual if-chains
- [ ] **Focus stack struct** — `FocusStack` that owns layers as `Box<dyn FocusLayer>`, auto-routes events. Would solve split-borrow issues for `route_key(&mut [...])`
- [ ] **`on_mouse` for HelpOverlay** — click outside closes, scroll inside does nothing
- [ ] **`on_mouse` for FuzzyPicker** — click on item selects, scroll navigates list

### Widgets
- [x] ~~**CommandPalette**~~ — done: `/`-triggered autocomplete with keyboard + mouse + scroll
- [ ] **DiffPreview** — inline diff viewer widget for file changes
- [ ] **CostBar** — token count + cost display widget
- [ ] **StatusLine** — bottom status bar with mode indicator, git branch, model name
- [ ] **ConfirmDialog** — yes/no modal (for destructive operations like git reset)

### Agent Integration
- [ ] **Streaming widget** — render partial BAML response with cursor animation
- [ ] **ToolProgress** — show tool execution progress (spinner + name + elapsed)
- [ ] **Multi-action panel** — show parallel tool executions in split view

### Infrastructure
- [ ] **Theming** — `Theme` struct with named colors, user-configurable
- [ ] **Layout presets** — common layouts (chat+sidebar, chat+preview, fullscreen picker)
- [ ] **Keybinding config** — user-remappable keybindings loaded from TOML
- [ ] **Accessibility** — screen reader hints via `ratatui` semantics

## Telemetry

TUI apps should use `init_tui_telemetry()` which redirects stderr to a file (prevents BAML's raw output from corrupting ratatui alternate screen):

```rust
use baml_agent_tui::init_tui_telemetry;

// stderr → .my-agent/stderr.log
// telemetry → .my-agent/tui-YYYY-MM-DD.jsonl
let _guard = init_tui_telemetry(".my-agent", "tui");
```

For headless/CLI mode, use `baml_agent::init_telemetry()` directly (no stderr redirect needed).

Both produce OTEL-aware structured JSONL with trace_id, span context, and timestamps. See `baml-agent/README.md` for details.

## Usage

```toml
[dependencies]
baml-agent-tui = { version = "0.3", path = "../baml-agent-tui" }
```

```rust
use baml_agent_tui::{
    AppEvent, ChatState, CommandPalette, FuzzyPicker, HelpOverlay,
    FocusLayer, FocusResult, route_key, route_mouse,
    init_terminal, restore_terminal, setup_panic_hook,
};
```

### Projects using this crate

- **rust-code** (`rc-cli/src/app.rs`) — AI coding agent TUI
- **souffleur** (`souffleur-tui/`) — Sales coaching agent TUI
