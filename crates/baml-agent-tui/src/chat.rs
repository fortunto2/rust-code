use ratatui::{
    prelude::*,
    widgets::{Block, Borders, List, ListItem, ListState},
};

/// Shared chat panel state — messages list + scroll.
///
/// This is the core of any agent TUI: a scrollable list of messages
/// with role-based coloring and prefix detection.
pub struct ChatState {
    pub messages: Vec<String>,
    pub list_state: ListState,
}

impl ChatState {
    pub fn new() -> Self {
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        Self {
            messages: Vec::new(),
            list_state,
        }
    }

    pub fn push(&mut self, msg: String) {
        self.messages.push(msg);
        self.scroll_to_bottom();
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.list_state.select(Some(0));
    }

    pub fn scroll_to_bottom(&mut self) {
        if !self.messages.is_empty() {
            self.list_state.select(Some(self.messages.len() - 1));
        }
    }

    /// Replace the last message if it starts with `prefix`, otherwise push new.
    pub fn replace_or_push(&mut self, prefix: &str, msg: String) {
        if let Some(last) = self.messages.last_mut() {
            if last.starts_with(prefix) {
                *last = msg;
                return;
            }
        }
        self.push(msg);
    }

    /// Append text to the last message that starts with `prefix`.
    /// If no such message, push a new one with `prefix + chunk`.
    pub fn append_stream_chunk(&mut self, prefix: &str, chunk: &str) {
        if let Some(last) = self.messages.last_mut() {
            if last.starts_with(prefix) {
                last.push_str(chunk);
                self.scroll_to_bottom();
                return;
            }
        }
        self.push(format!("{}{}", prefix, chunk));
    }

    /// Render messages as a List widget with role-based styling.
    pub fn render(&mut self, area: Rect, buf: &mut Buffer, title: &str) {
        let items: Vec<ListItem> = self
            .messages
            .iter()
            .map(|m| {
                let style = Self::message_style(m);
                ListItem::new(Line::from(m.as_str()).style(style))
            })
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(Style::default());

        ratatui::widgets::StatefulWidget::render(list, area, buf, &mut self.list_state);
    }

    /// Classify message and return appropriate style.
    fn message_style(msg: &str) -> Style {
        if msg.starts_with('>') {
            // User message
            Style::default().fg(Color::Cyan)
        } else if msg.starts_with("[THINK]") || msg.starts_with("[STREAM]") {
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC)
        } else if msg.starts_with("[TOOL]") {
            Style::default().fg(Color::Green)
        } else if msg.starts_with("[ERR]") {
            Style::default().fg(Color::Red)
        } else if msg.starts_with("[WARN]") {
            Style::default().fg(Color::Yellow)
        } else if msg.starts_with("[NOTE]") {
            Style::default().fg(Color::Magenta)
        } else if msg.starts_with("Analysis:") {
            Style::default().fg(Color::White)
        } else {
            Style::default()
        }
    }
}

impl Default for ChatState {
    fn default() -> Self {
        Self::new()
    }
}
