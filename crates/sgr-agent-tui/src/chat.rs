use ratatui::{
    prelude::*,
    widgets::{Block, Borders, List, ListItem, ListState},
};

/// Maximum chars for tool output preview before collapsing.
const TOOL_OUTPUT_PREVIEW_LEN: usize = 120;

/// Shared chat panel state — messages list + scroll.
///
/// This is the core of any agent TUI: a scrollable list of messages
/// with role-based coloring and prefix detection.
pub struct ChatState {
    pub messages: Vec<ChatMessage>,
    pub list_state: ListState,
}

/// A single chat message with metadata.
pub struct ChatMessage {
    pub text: String,
    pub timestamp: String,
    /// For tool output: whether the full text is expanded.
    pub expanded: bool,
}

impl ChatMessage {
    fn new(text: String) -> Self {
        let timestamp = chrono_now();
        Self {
            text,
            timestamp,
            expanded: false,
        }
    }

    /// Is this a long tool output that can be collapsed?
    fn is_collapsible(&self) -> bool {
        self.text.starts_with("  = ") && self.text.len() > TOOL_OUTPUT_PREVIEW_LEN
    }

    /// Display text, respecting collapsed state.
    fn display_text(&self) -> String {
        if self.is_collapsible() && !self.expanded {
            let lines: Vec<&str> = self.text.lines().collect();
            let first_line = lines.first().map_or("", |l| l);
            let remaining = lines.len().saturating_sub(1);
            if remaining > 0 {
                format!("{} [+{} lines]", first_line, remaining)
            } else {
                let preview = &self.text[..TOOL_OUTPUT_PREVIEW_LEN.min(self.text.len())];
                format!("{}... [+more]", preview)
            }
        } else {
            self.text.clone()
        }
    }
}

fn chrono_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let hours = (now % 86400) / 3600;
    let minutes = (now % 3600) / 60;
    format!("{:02}:{:02}", hours, minutes)
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
        self.messages.push(ChatMessage::new(msg));
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

    /// Scroll up by one page (10 messages).
    pub fn page_up(&mut self) {
        let cur = self.list_state.selected().unwrap_or(0);
        let next = cur.saturating_sub(10);
        self.list_state.select(Some(next));
    }

    /// Scroll down by one page (10 messages).
    pub fn page_down(&mut self) {
        let cur = self.list_state.selected().unwrap_or(0);
        let max = self.messages.len().saturating_sub(1);
        let next = (cur + 10).min(max);
        self.list_state.select(Some(next));
    }

    /// Toggle expanded state of currently selected message.
    pub fn toggle_expand(&mut self) {
        if let Some(idx) = self.list_state.selected()
            && let Some(msg) = self.messages.get_mut(idx)
            && msg.is_collapsible()
        {
            msg.expanded = !msg.expanded;
        }
    }

    /// Replace the last message if it starts with `prefix`, otherwise push new.
    pub fn replace_or_push(&mut self, prefix: &str, msg: String) {
        if let Some(last) = self.messages.last_mut()
            && last.text.starts_with(prefix)
        {
            last.text = msg;
            return;
        }
        self.push(msg);
    }

    /// Append text to the last message that starts with `prefix`.
    /// If no such message, push a new one with `prefix + chunk`.
    pub fn append_stream_chunk(&mut self, prefix: &str, chunk: &str) {
        if let Some(last) = self.messages.last_mut()
            && last.text.starts_with(prefix)
        {
            last.text.push_str(chunk);
            self.scroll_to_bottom();
            return;
        }
        self.push(format!("{}{}", prefix, chunk));
    }

    /// Render messages as a List widget with role-based styling and timestamps.
    pub fn render(&mut self, area: Rect, buf: &mut Buffer, title: &str) {
        let inner_width = area.width.saturating_sub(2) as usize; // borders

        let items: Vec<ListItem> = self
            .messages
            .iter()
            .map(|m| {
                let style = Self::message_style(&m.text);
                let display = m.display_text();

                // Timestamp prefix for non-system messages.
                let line_text = if m.text.starts_with('=') || m.text.starts_with("Type ") {
                    display
                } else {
                    format!("[{}] {}", m.timestamp, display)
                };

                // Word-wrap long lines.
                let wrapped = wrap_text(&line_text, inner_width);
                let lines: Vec<Line> = wrapped
                    .into_iter()
                    .map(|l| Line::from(l).style(style))
                    .collect();

                ListItem::new(Text::from(lines))
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
        } else if msg.starts_with("[DONE]") {
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD)
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

/// Simple word-wrap: split into lines that fit within `max_width` (in chars, not bytes).
fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![text.to_string()];
    }

    let mut lines = Vec::new();
    for input_line in text.lines() {
        if input_line.chars().count() <= max_width {
            lines.push(input_line.to_string());
        } else {
            let mut remaining = input_line;
            while remaining.chars().count() > max_width {
                // Find byte offset of the max_width-th char.
                let byte_limit = remaining
                    .char_indices()
                    .nth(max_width)
                    .map_or(remaining.len(), |(i, _)| i);
                // Try to break at a space within that range.
                let break_at = remaining[..byte_limit]
                    .rfind(' ')
                    .map_or(byte_limit, |pos| pos + 1);
                lines.push(remaining[..break_at].trim_end().to_string());
                remaining = &remaining[break_at..];
            }
            if !remaining.is_empty() {
                lines.push(remaining.to_string());
            }
        }
    }

    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_short_text() {
        let result = wrap_text("hello world", 80);
        assert_eq!(result, vec!["hello world"]);
    }

    #[test]
    fn wrap_long_text() {
        let result = wrap_text("the quick brown fox jumps over the lazy dog", 20);
        assert!(result.len() > 1);
        for line in &result {
            assert!(line.len() <= 20);
        }
    }

    #[test]
    fn wrap_utf8_cyrillic() {
        // Must not panic on multibyte chars.
        let cyrillic = "найди файл джобса в фикстурах и сделай клип под музыку из лучших моментов";
        let result = wrap_text(cyrillic, 40);
        assert!(result.len() >= 2);
        for line in &result {
            assert!(line.chars().count() <= 40);
        }
    }

    #[test]
    fn collapsible_tool_output() {
        let long_output = format!("  = {}", "x".repeat(200));
        let msg = ChatMessage::new(long_output.clone());
        assert!(msg.is_collapsible());
        assert!(msg.display_text().contains("[+more]"));
    }

    #[test]
    fn page_up_down() {
        let mut chat = ChatState::new();
        for i in 0..30 {
            chat.push(format!("msg {}", i));
        }
        assert_eq!(chat.list_state.selected(), Some(29));
        chat.page_up();
        assert_eq!(chat.list_state.selected(), Some(19));
        chat.page_down();
        assert_eq!(chat.list_state.selected(), Some(29));
    }
}
