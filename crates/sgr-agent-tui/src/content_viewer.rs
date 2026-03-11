//! Content viewer overlay — scrollable full-screen popup for long text.
//!
//! Opened via Ctrl+O on a tool result. Modal: consumes all keys while visible.
//! Esc/q closes, arrows/PageUp/Down/Home/End scroll.

use crate::focus::{FocusLayer, FocusResult};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

/// Scrollable content viewer overlay.
pub struct ContentViewer {
    pub visible: bool,
    title: String,
    lines: Vec<String>,
    scroll: u16,
    total_lines: u16,
    viewport_height: u16,
}

impl Default for ContentViewer {
    fn default() -> Self {
        Self {
            visible: false,
            title: String::new(),
            lines: Vec::new(),
            scroll: 0,
            total_lines: 0,
            viewport_height: 20,
        }
    }
}

impl FocusLayer for ContentViewer {
    fn is_active(&self) -> bool {
        self.visible
    }

    fn on_key(&mut self, key: KeyEvent) -> FocusResult {
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('q'), KeyModifiers::NONE) => {
                self.close();
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
                self.scroll = self.scroll.saturating_sub(1);
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
                self.scroll_down(1);
            }
            (KeyCode::PageUp, _) => {
                self.scroll = self.scroll.saturating_sub(self.viewport_height);
            }
            (KeyCode::PageDown, _) => {
                self.scroll_down(self.viewport_height);
            }
            (KeyCode::Home, _) | (KeyCode::Char('g'), _) => {
                self.scroll = 0;
            }
            (KeyCode::End, _) | (KeyCode::Char('G'), KeyModifiers::SHIFT) => {
                self.scroll = self.total_lines.saturating_sub(self.viewport_height);
            }
            _ => {}
        }
        // Modal — consume all keys.
        FocusResult::Consumed
    }
}

impl ContentViewer {
    /// Open the viewer with content. Title is shown in the border.
    pub fn open(&mut self, title: String, content: String) {
        self.title = title;
        self.lines = content.lines().map(String::from).collect();
        self.total_lines = self.lines.len() as u16;
        self.scroll = 0;
        self.visible = true;
    }

    pub fn close(&mut self) {
        self.visible = false;
        self.lines.clear();
    }

    fn scroll_down(&mut self, amount: u16) {
        let max = self.total_lines.saturating_sub(self.viewport_height);
        self.scroll = (self.scroll + amount).min(max);
    }

    /// Render as a near-full-screen overlay with 2-cell margin.
    pub fn render(&mut self, area: Rect, buf: &mut Buffer) {
        if !self.visible {
            return;
        }

        let margin = 2;
        let overlay = Rect::new(
            area.x + margin,
            area.y + margin,
            area.width.saturating_sub(margin * 2),
            area.height.saturating_sub(margin * 2),
        );

        // Update viewport height for scroll calculations.
        self.viewport_height = overlay.height.saturating_sub(2); // borders

        let scroll_info = format!(" {}/{} ", self.scroll + 1, self.total_lines.max(1));
        let title = format!(" {} {} Esc close | arrows scroll ", self.title, scroll_info);

        Clear.render(overlay, buf);

        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(Color::Cyan))
            .style(Style::default().bg(Color::Black));

        let text = self.lines.join("\n");
        let paragraph = Paragraph::new(text)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((self.scroll, 0))
            .style(Style::default().fg(Color::White));

        paragraph.render(overlay, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_and_close() {
        let mut v = ContentViewer::default();
        assert!(!v.visible);
        v.open("test".into(), "line1\nline2\nline3".into());
        assert!(v.visible);
        assert_eq!(v.lines.len(), 3);
        assert_eq!(v.total_lines, 3);
        v.close();
        assert!(!v.visible);
        assert!(v.lines.is_empty());
    }

    #[test]
    fn scroll_bounds() {
        let mut v = ContentViewer::default();
        let content = (0..100)
            .map(|i| format!("line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        v.open("test".into(), content);
        v.viewport_height = 20;
        v.scroll_down(10);
        assert_eq!(v.scroll, 10);
        v.scroll_down(200);
        assert_eq!(v.scroll, 80); // 100 - 20
        v.scroll = 0;
        // Can't scroll past 0.
        v.scroll = v.scroll.saturating_sub(5);
        assert_eq!(v.scroll, 0);
    }

    #[test]
    fn renders_without_panic() {
        let mut v = ContentViewer::default();
        v.open("test".into(), "hello\nworld".into());
        let area = Rect::new(0, 0, 60, 20);
        let mut buf = Buffer::empty(area);
        v.render(area, &mut buf);
    }
}
