//! Slash command autocomplete popup — `/`-triggered command palette.
//!
//! Implements `FocusLayer` for input routing. Non-modal: consumes
//! navigation keys (Tab, Enter, Up, Down, Esc) but passes chars through.
//!
//! Features:
//! - Navigation wrapping (Down from last → first, Up from first → last)
//! - Live preview: selected command stored for caller to update input
//! - Scroll indicators (▲/▼) when list is scrollable
//! - Prefix-priority matching (exact prefix ranked higher than substring)

use crate::focus::{point_in_rect, FocusLayer, FocusResult};
use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem};

/// Max visible items in popup (excluding border).
const MAX_VISIBLE: usize = 8;

/// A single command entry.
#[derive(Debug, Clone)]
pub struct Command {
    pub name: &'static str,
    pub description: &'static str,
}

/// Applied action from the palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteAction {
    /// Tab — insert text into input for editing
    Insert(&'static str),
    /// Enter — execute command immediately
    Execute(&'static str),
}

/// Slash command autocomplete state.
pub struct CommandPalette {
    commands: &'static [(&'static str, &'static str)],
    suggestions: Vec<&'static str>,
    selected: usize,
    scroll_offset: usize,
    popup_rect: Option<Rect>,
    /// Last applied action (set by on_key, consumed by take_applied).
    applied: Option<PaletteAction>,
    /// Currently previewed command (updated on navigation for live preview).
    preview: Option<&'static str>,
}

impl CommandPalette {
    pub fn new(commands: &'static [(&'static str, &'static str)]) -> Self {
        Self {
            commands,
            suggestions: Vec::new(),
            selected: 0,
            scroll_offset: 0,
            popup_rect: None,
            applied: None,
            preview: None,
        }
    }

    /// Take the last applied action (if any). Resets after reading.
    /// Call this after `on_key()` returns `Consumed` to get the action.
    pub fn take_applied(&mut self) -> Option<PaletteAction> {
        self.applied.take()
    }

    /// Take the current preview command (live preview on navigation).
    /// Call this after `on_key()` to update input field with selection.
    pub fn take_preview(&mut self) -> Option<&'static str> {
        self.preview.take()
    }

    /// Update suggestions based on current input text.
    /// Call this after every keystroke in the input field.
    pub fn update(&mut self, input: &str) {
        if input.starts_with('/') && !input.contains(' ') {
            let query = input.to_lowercase();
            // Score and sort: exact prefix first, then substring match
            let mut scored: Vec<(&'static str, u32)> = self
                .commands
                .iter()
                .filter_map(|(cmd, _)| {
                    let cmd_lower = cmd.to_lowercase();
                    if cmd_lower.starts_with(&query) {
                        // Exact prefix match — high score
                        Some((*cmd, 100))
                    } else if query.len() > 1 && cmd_lower.contains(&query[1..]) {
                        // Substring match (skip the `/`) — lower score
                        Some((*cmd, 50))
                    } else {
                        None
                    }
                })
                .collect();
            scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            self.suggestions = scored.into_iter().map(|(cmd, _)| cmd).collect();

            if self.selected >= self.suggestions.len() {
                self.selected = 0;
                self.scroll_offset = 0;
            }
        } else {
            self.clear();
        }
    }

    /// Clear suggestions (close popup).
    pub fn clear(&mut self) {
        self.suggestions.clear();
        self.selected = 0;
        self.scroll_offset = 0;
        self.preview = None;
    }

    /// Get the currently selected command text, if any.
    pub fn selected_command(&self) -> Option<&'static str> {
        self.suggestions.get(self.selected).copied()
    }

    /// Whether suggestions are currently shown.
    pub fn has_suggestions(&self) -> bool {
        !self.suggestions.is_empty()
    }

    /// Current suggestions list (for external rendering if needed).
    pub fn suggestions(&self) -> &[&'static str] {
        &self.suggestions
    }

    /// Current selection index.
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// Ensure scroll_offset keeps selected item visible.
    fn ensure_visible(&mut self) {
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + MAX_VISIBLE {
            self.scroll_offset = self.selected + 1 - MAX_VISIBLE;
        }
    }

    /// Move selection and set live preview.
    fn move_selection(&mut self, new_idx: usize) {
        self.selected = new_idx;
        self.ensure_visible();
        self.preview = self.selected_command();
    }

    /// Get description for a command.
    fn description_of(&self, cmd: &str) -> &'static str {
        self.commands
            .iter()
            .find(|(c, _)| *c == cmd)
            .map(|(_, d)| *d)
            .unwrap_or("")
    }

    /// Render the popup above the given input area.
    /// Stores the popup rect for mouse hit testing.
    pub fn render(&mut self, frame: &mut Frame, input_area: Rect) {
        if self.suggestions.is_empty() {
            self.popup_rect = None;
            return;
        }

        let visible_count = self.suggestions.len().min(MAX_VISIBLE);
        let visible_items =
            &self.suggestions[self.scroll_offset..self.scroll_offset + visible_count];

        let items: Vec<ListItem> = visible_items
            .iter()
            .enumerate()
            .map(|(vi, cmd)| {
                let actual_idx = self.scroll_offset + vi;
                let desc = self.description_of(cmd);
                let style = if actual_idx == self.selected {
                    Style::default().fg(Color::Black).bg(Color::Cyan)
                } else {
                    Style::default().fg(Color::Cyan)
                };
                ListItem::new(format!("{:<12} {}", cmd, desc)).style(style)
            })
            .collect();

        // Scroll indicators
        let has_above = self.scroll_offset > 0;
        let has_below = self.scroll_offset + MAX_VISIBLE < self.suggestions.len();
        let title = format!(
            " / commands {}/{} {}",
            self.selected + 1,
            self.suggestions.len(),
            match (has_above, has_below) {
                (true, true) => "▲▼",
                (true, false) => "▲",
                (false, true) => "▼",
                (false, false) => "",
            }
        );

        let popup_height = visible_count as u16 + 2; // +2 for borders
        let popup_area = Rect {
            x: input_area.x + 1,
            y: input_area.y.saturating_sub(popup_height),
            width: input_area.width.min(50),
            height: popup_height,
        };

        let popup = List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(title),
        );
        frame.render_widget(Clear, popup_area);
        frame.render_widget(popup, popup_area);
        self.popup_rect = Some(popup_area);
    }
}

impl FocusLayer for CommandPalette {
    fn is_active(&self) -> bool {
        !self.suggestions.is_empty()
    }

    fn on_key(&mut self, key: KeyEvent) -> FocusResult {
        if self.suggestions.is_empty() {
            return FocusResult::Passed;
        }
        match key.code {
            KeyCode::Enter => {
                // Execute immediately
                if let Some(cmd) = self.selected_command() {
                    self.applied = Some(PaletteAction::Execute(cmd));
                    self.clear();
                }
                FocusResult::Consumed
            }
            KeyCode::Tab => {
                // Insert into input for editing
                if let Some(cmd) = self.selected_command() {
                    self.applied = Some(PaletteAction::Insert(cmd));
                    self.clear();
                }
                FocusResult::Consumed
            }
            KeyCode::Down => {
                if !self.suggestions.is_empty() {
                    let next = if self.selected + 1 >= self.suggestions.len() {
                        0 // wrap to first
                    } else {
                        self.selected + 1
                    };
                    self.move_selection(next);
                }
                FocusResult::Consumed
            }
            KeyCode::Up => {
                if !self.suggestions.is_empty() {
                    let next = if self.selected == 0 {
                        self.suggestions.len() - 1 // wrap to last
                    } else {
                        self.selected - 1
                    };
                    self.move_selection(next);
                }
                FocusResult::Consumed
            }
            KeyCode::Esc => {
                self.clear();
                FocusResult::Consumed
            }
            _ => FocusResult::Passed,
        }
    }

    fn on_mouse(&mut self, mouse: MouseEvent) -> FocusResult {
        let Some(rect) = self.popup_rect else {
            return FocusResult::Passed;
        };
        if !point_in_rect(mouse.column, mouse.row, rect) {
            return FocusResult::Passed;
        }
        match mouse.kind {
            MouseEventKind::ScrollDown => {
                if !self.suggestions.is_empty() {
                    let next = if self.selected + 1 >= self.suggestions.len() {
                        0
                    } else {
                        self.selected + 1
                    };
                    self.move_selection(next);
                }
                FocusResult::Consumed
            }
            MouseEventKind::ScrollUp => {
                if !self.suggestions.is_empty() {
                    let next = if self.selected == 0 {
                        self.suggestions.len() - 1
                    } else {
                        self.selected - 1
                    };
                    self.move_selection(next);
                }
                FocusResult::Consumed
            }
            MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                let inner_y = rect.y + 1; // border
                if mouse.row >= inner_y {
                    let idx = self.scroll_offset + (mouse.row - inner_y) as usize;
                    if idx < self.suggestions.len() {
                        self.selected = idx;
                        self.applied = Some(PaletteAction::Execute(self.suggestions[idx]));
                        self.clear();
                    }
                }
                FocusResult::Consumed
            }
            _ => FocusResult::Passed,
        }
    }

    fn hit_rect(&self) -> Option<Rect> {
        self.popup_rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    const TEST_COMMANDS: &[(&str, &str)] = &[
        ("/help", "Show help"),
        ("/clear", "Clear chat"),
        ("/quit", "Exit"),
        ("/config", "Show config"),
    ];

    fn make_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn update_filters_by_prefix() {
        let mut cp = CommandPalette::new(TEST_COMMANDS);
        cp.update("/");
        assert_eq!(cp.suggestions().len(), 4);
        cp.update("/cl");
        assert_eq!(cp.suggestions(), &["/clear"]);
        cp.update("/x");
        assert!(cp.suggestions().is_empty());
    }

    #[test]
    fn substring_match() {
        let mut cp = CommandPalette::new(TEST_COMMANDS);
        cp.update("/on"); // matches "/config" via substring "on"
        assert_eq!(cp.suggestions(), &["/config"]);
    }

    #[test]
    fn prefix_priority() {
        let mut cp = CommandPalette::new(TEST_COMMANDS);
        // /c matches /clear and /config by prefix
        cp.update("/c");
        assert_eq!(cp.suggestions().len(), 2);
        assert!(cp.suggestions().contains(&"/clear"));
        assert!(cp.suggestions().contains(&"/config"));
    }

    #[test]
    fn navigation_wrapping() {
        let mut cp = CommandPalette::new(TEST_COMMANDS);
        cp.update("/");
        assert_eq!(cp.selected_index(), 0);

        // Go to last
        for _ in 0..3 {
            cp.on_key(make_key(KeyCode::Down));
        }
        assert_eq!(cp.selected_index(), 3);

        // Wrap to first
        cp.on_key(make_key(KeyCode::Down));
        assert_eq!(cp.selected_index(), 0);

        // Wrap to last
        cp.on_key(make_key(KeyCode::Up));
        assert_eq!(cp.selected_index(), 3);
    }

    #[test]
    fn live_preview_on_navigation() {
        let mut cp = CommandPalette::new(TEST_COMMANDS);
        cp.update("/");

        cp.on_key(make_key(KeyCode::Down));
        let preview = cp.take_preview();
        assert!(preview.is_some());
        assert_eq!(preview.unwrap(), cp.suggestions()[1]);
    }

    #[test]
    fn enter_executes() {
        let mut cp = CommandPalette::new(TEST_COMMANDS);
        cp.update("/cl");
        assert_eq!(cp.selected_command(), Some("/clear"));

        let result = cp.on_key(make_key(KeyCode::Enter));
        assert_eq!(result, FocusResult::Consumed);
        let action = cp.take_applied();
        assert!(matches!(action, Some(PaletteAction::Execute("/clear"))));
    }

    #[test]
    fn tab_inserts() {
        let mut cp = CommandPalette::new(TEST_COMMANDS);
        cp.update("/cl");

        let result = cp.on_key(make_key(KeyCode::Tab));
        assert_eq!(result, FocusResult::Consumed);
        let action = cp.take_applied();
        assert!(matches!(action, Some(PaletteAction::Insert("/clear"))));
    }

    #[test]
    fn esc_clears() {
        let mut cp = CommandPalette::new(TEST_COMMANDS);
        cp.update("/");
        assert!(cp.has_suggestions());

        cp.on_key(make_key(KeyCode::Esc));
        assert!(!cp.has_suggestions());
    }

    #[test]
    fn chars_pass_through() {
        let mut cp = CommandPalette::new(TEST_COMMANDS);
        cp.update("/");
        let result = cp.on_key(make_key(KeyCode::Char('h')));
        assert_eq!(result, FocusResult::Passed);
    }

    #[test]
    fn inactive_when_no_suggestions() {
        let cp = CommandPalette::new(TEST_COMMANDS);
        assert!(!cp.is_active());
    }

    #[test]
    fn enter_passes_when_inactive() {
        let mut cp = CommandPalette::new(TEST_COMMANDS);
        // No suggestions — Enter must pass through
        let result = cp.on_key(make_key(KeyCode::Enter));
        assert_eq!(result, FocusResult::Passed);
    }

    #[test]
    fn selected_resets_on_out_of_bounds() {
        let mut cp = CommandPalette::new(TEST_COMMANDS);
        cp.update("/");
        cp.on_key(make_key(KeyCode::Down));
        cp.on_key(make_key(KeyCode::Down)); // selected = 2
        cp.update("/qu");
        assert_eq!(cp.selected_index(), 0); // reset
    }
}
