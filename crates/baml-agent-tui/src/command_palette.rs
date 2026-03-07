//! Slash command autocomplete popup — `/`-triggered command palette.
//!
//! Implements `FocusLayer` for input routing. Non-modal: consumes
//! navigation keys (Tab, Enter, Up, Down, Esc) but passes chars through.

use crate::focus::{point_in_rect, FocusLayer, FocusResult};
use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem};

/// A single command entry.
#[derive(Debug, Clone)]
pub struct Command {
    pub name: &'static str,
    pub description: &'static str,
}

/// Slash command autocomplete state.
pub struct CommandPalette {
    commands: &'static [(&'static str, &'static str)],
    suggestions: Vec<&'static str>,
    selected: usize,
    popup_rect: Option<Rect>,
    /// Last applied command (set by on_key, consumed by take_applied).
    applied: Option<&'static str>,
}

impl CommandPalette {
    pub fn new(commands: &'static [(&'static str, &'static str)]) -> Self {
        Self {
            commands,
            suggestions: Vec::new(),
            selected: 0,
            popup_rect: None,
            applied: None,
        }
    }

    /// Take the last applied command (if any). Resets after reading.
    /// Call this after `on_key()` returns `Consumed` to get the selected text.
    pub fn take_applied(&mut self) -> Option<&'static str> {
        self.applied.take()
    }

    /// Update suggestions based on current input text.
    /// Call this after every keystroke in the input field.
    pub fn update(&mut self, input: &str) {
        if input.starts_with('/') && !input.contains(' ') {
            let query = input.to_lowercase();
            self.suggestions = self
                .commands
                .iter()
                .filter(|(cmd, _)| cmd.starts_with(&query))
                .map(|(cmd, _)| *cmd)
                .collect();
            if self.selected >= self.suggestions.len() {
                self.selected = 0;
            }
        } else {
            self.clear();
        }
    }

    /// Clear suggestions (close popup).
    pub fn clear(&mut self) {
        self.suggestions.clear();
        self.selected = 0;
    }

    /// Get the currently selected command text, if any.
    pub fn selected_command(&self) -> Option<&'static str> {
        self.suggestions.get(self.selected).copied()
    }

    /// Apply the selected suggestion. Returns the command string.
    /// Also stores result in `applied` for retrieval via `take_applied()`.
    pub fn apply(&mut self) -> Option<&'static str> {
        let cmd = self.selected_command();
        self.applied = cmd;
        self.clear();
        cmd
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

    /// Render the popup above the given input area.
    /// Stores the popup rect for mouse hit testing.
    pub fn render(&mut self, frame: &mut Frame, input_area: Rect) {
        if self.suggestions.is_empty() {
            self.popup_rect = None;
            return;
        }

        let items: Vec<ListItem> = self
            .suggestions
            .iter()
            .enumerate()
            .map(|(i, cmd)| {
                let desc = self
                    .commands
                    .iter()
                    .find(|(c, _)| c == cmd)
                    .map(|(_, d)| *d)
                    .unwrap_or("");
                let style = if i == self.selected {
                    Style::default().fg(Color::Black).bg(Color::Cyan)
                } else {
                    Style::default().fg(Color::Cyan)
                };
                ListItem::new(format!("{:<12} {}", cmd, desc)).style(style)
            })
            .collect();

        let popup_height = (self.suggestions.len() as u16 + 2).min(10);
        let popup_area = Rect {
            x: input_area.x + 1,
            y: input_area.y.saturating_sub(popup_height),
            width: input_area.width.min(40),
            height: popup_height,
        };

        let popup = List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(" / commands (Tab to select) "),
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
        match key.code {
            KeyCode::Tab | KeyCode::Enter => {
                self.apply();
                FocusResult::Consumed
            }
            KeyCode::Down => {
                if self.selected + 1 < self.suggestions.len() {
                    self.selected += 1;
                }
                FocusResult::Consumed
            }
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
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
                if self.selected + 1 < self.suggestions.len() {
                    self.selected += 1;
                }
                FocusResult::Consumed
            }
            MouseEventKind::ScrollUp => {
                self.selected = self.selected.saturating_sub(1);
                FocusResult::Consumed
            }
            MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                let inner_y = rect.y + 1; // border
                if mouse.row >= inner_y {
                    let idx = (mouse.row - inner_y) as usize;
                    if idx < self.suggestions.len() {
                        self.selected = idx;
                        self.apply();
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
    ];

    fn make_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn update_filters_by_prefix() {
        let mut cp = CommandPalette::new(TEST_COMMANDS);
        cp.update("/");
        assert_eq!(cp.suggestions().len(), 3);
        cp.update("/cl");
        assert_eq!(cp.suggestions(), &["/clear"]);
        cp.update("/x");
        assert!(cp.suggestions().is_empty());
    }

    #[test]
    fn navigation() {
        let mut cp = CommandPalette::new(TEST_COMMANDS);
        cp.update("/");
        assert_eq!(cp.selected_index(), 0);

        cp.on_key(make_key(KeyCode::Down));
        assert_eq!(cp.selected_index(), 1);

        cp.on_key(make_key(KeyCode::Down));
        assert_eq!(cp.selected_index(), 2);

        // Can't go past end
        cp.on_key(make_key(KeyCode::Down));
        assert_eq!(cp.selected_index(), 2);

        cp.on_key(make_key(KeyCode::Up));
        assert_eq!(cp.selected_index(), 1);
    }

    #[test]
    fn tab_applies_and_clears() {
        let mut cp = CommandPalette::new(TEST_COMMANDS);
        cp.update("/cl");
        assert_eq!(cp.selected_command(), Some("/clear"));

        let result = cp.on_key(make_key(KeyCode::Tab));
        assert_eq!(result, FocusResult::Consumed);
        assert!(!cp.has_suggestions());
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
    fn selected_resets_on_out_of_bounds() {
        let mut cp = CommandPalette::new(TEST_COMMANDS);
        cp.update("/");
        cp.on_key(make_key(KeyCode::Down));
        cp.on_key(make_key(KeyCode::Down)); // selected = 2
                                            // Now filter to single result
        cp.update("/qu");
        assert_eq!(cp.selected_index(), 0); // reset
    }
}
