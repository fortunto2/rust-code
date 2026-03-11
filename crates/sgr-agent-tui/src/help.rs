//! Help overlay widget — shows keybindings and shortcuts.

use crate::focus::{FocusLayer, FocusResult};
use crossterm::event::KeyCode;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

/// Keybinding entry for the help overlay.
pub struct HelpEntry {
    pub key: &'static str,
    pub description: &'static str,
}

/// Help overlay state.
pub struct HelpOverlay {
    pub visible: bool,
    pub entries: Vec<HelpEntry>,
}

impl Default for HelpOverlay {
    fn default() -> Self {
        Self {
            visible: false,
            entries: vec![
                HelpEntry {
                    key: "Enter",
                    description: "Send message / toggle expand",
                },
                HelpEntry {
                    key: "Ctrl+O",
                    description: "View last tool output",
                },
                HelpEntry {
                    key: "Ctrl+P",
                    description: "Open fuzzy picker",
                },
                HelpEntry {
                    key: "Ctrl+H",
                    description: "Toggle this help",
                },
                HelpEntry {
                    key: "Ctrl+C/Q",
                    description: "Quit",
                },
                HelpEntry {
                    key: "PageUp/Down",
                    description: "Scroll chat",
                },
                HelpEntry {
                    key: "Up/Down",
                    description: "Input history",
                },
                HelpEntry {
                    key: "Esc",
                    description: "Close picker / help",
                },
                HelpEntry {
                    key: "Tab",
                    description: "Switch picker channel",
                },
                HelpEntry {
                    key: "Shift+Tab",
                    description: "Previous picker channel",
                },
            ],
        }
    }
}

impl FocusLayer for HelpOverlay {
    fn is_active(&self) -> bool {
        self.visible
    }

    fn on_key(&mut self, key: crossterm::event::KeyEvent) -> FocusResult {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.close();
                FocusResult::Consumed
            }
            // Help overlay is modal — consume all keys while visible
            _ => FocusResult::Consumed,
        }
    }
}

impl HelpOverlay {
    pub fn toggle(&mut self) {
        self.visible = !self.visible;
    }

    pub fn close(&mut self) {
        self.visible = false;
    }

    /// Render as a centered overlay.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if !self.visible {
            return;
        }

        let width = area.width.clamp(30, 50);
        let height = (self.entries.len() as u16 + 4).min(area.height.saturating_sub(2));
        let x = area.x + (area.width.saturating_sub(width)) / 2;
        let y = area.y + (area.height.saturating_sub(height)) / 2;
        let overlay = Rect::new(x, y, width, height);

        Clear.render(overlay, buf);

        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Keybindings ")
            .border_style(Style::default().fg(Color::Yellow))
            .style(Style::default().bg(Color::Black));

        let inner = block.inner(overlay);
        block.render(overlay, buf);

        let key_col_width = 14;
        let lines: Vec<Line> = self
            .entries
            .iter()
            .map(|e| {
                let key_padded = format!("{:<width$}", e.key, width = key_col_width);
                Line::from(vec![
                    Span::styled(
                        key_padded,
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(e.description, Style::default().fg(Color::White)),
                ])
            })
            .collect();

        Paragraph::new(Text::from(lines)).render(inner, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_toggle() {
        let mut help = HelpOverlay::default();
        assert!(!help.visible);
        help.toggle();
        assert!(help.visible);
        help.toggle();
        assert!(!help.visible);
    }

    #[test]
    fn help_renders() {
        let help = HelpOverlay::default();
        let area = Rect::new(0, 0, 60, 20);
        let mut buf = Buffer::empty(area);
        help.render(area, &mut buf);
    }
}
