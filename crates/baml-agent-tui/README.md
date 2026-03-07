# baml-agent-tui

Shared TUI shell for BAML SGR agents — reusable widgets, event loop, and patterns for building terminal AI coding assistants with [ratatui](https://ratatui.rs).

## Modules

| Module | What |
|--------|------|
| `focus` | **FocusLayer trait** — input routing through a stack of UI layers |
| `chat` | Chat panel with streaming, scroll, expand/collapse |
| `picker` | Fuzzy picker overlay (nucleo-matcher, channels, preview panel) |
| `help` | Help overlay (keybindings display) |
| `context_bar` | Project context bar (git branch, model, tokens) |
| `event` | `AppEvent<T>` enum — terminal events, ticks, agent messages |
| `agent_task` | `TuiAgent` trait + `spawn_agent_loop` — async agent integration |
| `headless` | Non-interactive mode (pipe-friendly) |
| `terminal` | Terminal init/restore, panic hook, telemetry |

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
| Slash popup* | No | Tab, Enter, Up, Down, Esc | Char input |

*Slash popup is in `rc-cli` (app-specific), not yet using the trait.

## What's Missing / TODO

### Focus System
- [ ] **SlashPopup as FocusLayer** — extract from app.rs into a reusable `CommandPalette` widget with `FocusLayer` impl
- [ ] **Wire `route_key`/`route_mouse` in app.rs** — replace manual `if` chain with `route_key(&mut [...])`. Blocked by ownership: layers are fields of `App`, need to split borrows or use `RefCell`
- [ ] **Focus stack struct** — `FocusStack` that owns layers as `Box<dyn FocusLayer>`, auto-routes events. Would solve the split-borrow problem
- [ ] **`on_mouse` for HelpOverlay** — click outside closes, scroll inside does nothing
- [ ] **`on_mouse` for FuzzyPicker** — click on item selects, scroll navigates list

### Widgets
- [ ] **CommandPalette** — generic `/`-triggered autocomplete popup (extract from app.rs slash logic)
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

## Usage

```toml
[dependencies]
baml-agent-tui = { version = "0.3", path = "../baml-agent-tui" }
```

```rust
use baml_agent_tui::{
    AppEvent, ChatState, FuzzyPicker, HelpOverlay,
    FocusLayer, FocusResult, route_key,
    init_terminal, restore_terminal, setup_panic_hook,
};
```

See `rc-cli/src/app.rs` for a full implementation example.
