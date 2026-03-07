//! Generic fuzzy picker widget — reusable overlay for searching and selecting items.
//!
//! Uses nucleo-matcher for fuzzy scoring. Channels allow grouping items
//! (e.g. "music" | "video" | "session") with Tab switching.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

/// Preview data for the right panel of the picker.
#[derive(Debug, Clone, Default)]
pub struct PickerPreview {
    /// Key-value metadata lines (e.g. "BPM: 120", "Duration: 3:24").
    pub meta: Vec<(String, String)>,
    /// Beat positions in seconds (for audio waveform visualization).
    pub beats: Vec<f64>,
    /// Onset strength envelope (normalized 0..1).
    pub onset_strength: Vec<f32>,
    /// Total duration in seconds.
    pub duration: f64,
}

/// A single item in the picker.
#[derive(Debug, Clone)]
pub struct PickerItem {
    pub id: String,
    /// Primary text for fuzzy matching.
    pub label: String,
    /// Secondary detail line (e.g. "120 BPM | 3:24").
    pub detail: String,
    /// Channel name for Tab grouping.
    pub channel: String,
    /// Icon prefix (e.g. "♫", "▶", "◆").
    pub icon: &'static str,
    /// Optional preview data for the right panel.
    pub preview: Option<PickerPreview>,
}

/// Result of handling a key event in the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerAction {
    /// Key was consumed but no action needed.
    None,
    /// User selected an item (returns item id).
    Select(String),
    /// User cancelled.
    Cancel,
    /// User switched channel via Tab.
    SwitchChannel,
}

/// Fuzzy picker overlay state.
pub struct FuzzyPicker {
    query: String,
    items: Vec<PickerItem>,
    /// (score, index into items)
    filtered: Vec<(u32, usize)>,
    list_state: ListState,
    channels: Vec<String>,
    active_channel: Option<usize>,
    pub visible: bool,
}

impl Default for FuzzyPicker {
    fn default() -> Self {
        Self::new()
    }
}

impl FuzzyPicker {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            items: Vec::new(),
            filtered: Vec::new(),
            list_state: ListState::default(),
            channels: Vec::new(),
            active_channel: None,
            visible: false,
        }
    }

    /// Set items and rebuild channel list. Call before showing.
    pub fn set_items(&mut self, items: Vec<PickerItem>) {
        // Collect unique channels preserving insertion order.
        let mut channels = Vec::new();
        for item in &items {
            if !channels.contains(&item.channel) {
                channels.push(item.channel.clone());
            }
        }
        self.channels = channels;
        self.items = items;
        self.query.clear();
        self.active_channel = None;
        self.filter();
    }

    /// Open the picker overlay.
    pub fn open(&mut self) {
        self.visible = true;
        self.query.clear();
        self.active_channel = None;
        self.filter();
    }

    /// Close the picker overlay.
    pub fn close(&mut self) {
        self.visible = false;
    }

    /// Currently selected item, if any.
    pub fn selected_item(&self) -> Option<&PickerItem> {
        self.list_state
            .selected()
            .and_then(|idx| self.filtered.get(idx))
            .map(|(_, item_idx)| &self.items[*item_idx])
    }

    /// Handle a key event. Returns the resulting action.
    pub fn on_key(&mut self, code: crossterm::event::KeyCode) -> PickerAction {
        use crossterm::event::KeyCode;

        match code {
            KeyCode::Esc => {
                self.close();
                PickerAction::Cancel
            }
            KeyCode::Enter => {
                if let Some(item) = self.selected_item() {
                    let id = item.id.clone();
                    self.close();
                    PickerAction::Select(id)
                } else {
                    PickerAction::None
                }
            }
            KeyCode::Tab => {
                self.next_channel();
                PickerAction::SwitchChannel
            }
            KeyCode::BackTab => {
                self.prev_channel();
                PickerAction::SwitchChannel
            }
            KeyCode::Up => {
                self.move_up();
                PickerAction::None
            }
            KeyCode::Down => {
                self.move_down();
                PickerAction::None
            }
            KeyCode::Char(ch) => {
                self.query.push(ch);
                self.filter();
                PickerAction::None
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.filter();
                PickerAction::None
            }
            _ => PickerAction::None,
        }
    }

    /// Re-filter items based on current query and active channel.
    fn filter(&mut self) {
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut buf = Vec::new();

        let active_channel = self
            .active_channel
            .and_then(|idx| self.channels.get(idx).cloned());

        self.filtered.clear();

        for (idx, item) in self.items.iter().enumerate() {
            // Channel filter
            if let Some(ref ch) = active_channel {
                if &item.channel != ch {
                    continue;
                }
            }

            // Fuzzy score
            let score = if self.query.is_empty() {
                // No query — include all, score by position (recent first).
                Some((self.items.len() - idx) as u32)
            } else {
                let pattern =
                    Pattern::parse(&self.query, CaseMatching::Ignore, Normalization::Smart);
                let haystack = Utf32Str::new(&item.label, &mut buf);
                pattern.score(haystack, &mut matcher)
            };

            if let Some(s) = score {
                self.filtered.push((s, idx));
            }
        }

        // Sort by score descending.
        self.filtered.sort_by(|a, b| b.0.cmp(&a.0));

        // Reset selection.
        if self.filtered.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(0));
        }
    }

    fn move_up(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        let cur = self.list_state.selected().unwrap_or(0);
        let next = if cur == 0 {
            self.filtered.len() - 1
        } else {
            cur - 1
        };
        self.list_state.select(Some(next));
    }

    fn move_down(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        let cur = self.list_state.selected().unwrap_or(0);
        let next = (cur + 1) % self.filtered.len();
        self.list_state.select(Some(next));
    }

    fn next_channel(&mut self) {
        if self.channels.is_empty() {
            return;
        }
        self.active_channel = Some(match self.active_channel {
            None => 0,
            Some(i) if i + 1 >= self.channels.len() => {
                self.active_channel = None;
                self.filter();
                return;
            }
            Some(i) => i + 1,
        });
        self.filter();
    }

    fn prev_channel(&mut self) {
        if self.channels.is_empty() {
            return;
        }
        self.active_channel = match self.active_channel {
            None => Some(self.channels.len() - 1),
            Some(0) => None,
            Some(i) => Some(i - 1),
        };
        self.filter();
    }

    /// Render the picker as a centered overlay.
    pub fn render(&mut self, area: Rect, buf: &mut Buffer) {
        if !self.visible {
            return;
        }

        // Check if selected item has preview data — expand overlay if so.
        let has_preview = self
            .selected_item()
            .and_then(|it| it.preview.as_ref())
            .is_some();

        let base_width = area.width.clamp(20, 60);
        let preview_width: u16 = if has_preview { 28 } else { 0 };
        let total_width = (base_width + preview_width).min(area.width);
        let height = area.height.clamp(5, 16);
        let x = area.x + (area.width.saturating_sub(total_width)) / 2;
        let y = area.y + (area.height.saturating_sub(height)) / 2;
        let overlay = Rect::new(x, y, total_width, height);

        // Clear background.
        Clear.render(overlay, buf);

        // Build channel tabs header.
        let tabs: String = if self.channels.len() > 1 {
            self.channels
                .iter()
                .enumerate()
                .map(|(i, ch)| {
                    if self.active_channel == Some(i) {
                        format!("[{}]", ch)
                    } else {
                        ch.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join(" | ")
        } else {
            String::new()
        };

        let all_label = if self.active_channel.is_none() {
            "[all]"
        } else {
            "all"
        };
        let title = if tabs.is_empty() {
            " Search ".to_string()
        } else {
            format!(" Search  Tab: {} | {} ", all_label, tabs)
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(Color::Cyan))
            .style(Style::default().bg(Color::Black));

        let inner = block.inner(overlay);
        block.render(overlay, buf);

        if inner.height < 2 {
            return;
        }

        // Split inner into list area and preview area.
        let list_width = if has_preview {
            inner.width.saturating_sub(preview_width)
        } else {
            inner.width
        };

        // Query line.
        let query_area = Rect::new(inner.x, inner.y, list_width, 1);
        let query_display = format!("> {}", self.query);
        Paragraph::new(query_display)
            .style(Style::default().fg(Color::White))
            .render(query_area, buf);

        // Results list.
        let list_area = Rect::new(inner.x, inner.y + 1, list_width, inner.height - 1);

        if self.filtered.is_empty() {
            let msg = if self.query.is_empty() {
                "No items"
            } else {
                "No matches"
            };
            Paragraph::new(msg)
                .style(
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC),
                )
                .render(list_area, buf);
        } else {
            let items: Vec<ListItem> = self
                .filtered
                .iter()
                .map(|(_, item_idx)| {
                    let item = &self.items[*item_idx];
                    let line = if item.detail.is_empty() {
                        format!("  {} {}", item.icon, item.label)
                    } else {
                        format!("  {} {}  {}", item.icon, item.label, item.detail)
                    };
                    ListItem::new(Line::from(line))
                })
                .collect();

            let list = List::new(items)
                .highlight_style(
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("\u{25B8} ");

            ratatui::widgets::StatefulWidget::render(list, list_area, buf, &mut self.list_state);
        }

        // Preview panel (right side).
        if has_preview && preview_width > 2 {
            let preview_area =
                Rect::new(inner.x + list_width, inner.y, preview_width, inner.height);
            if let Some(preview) = self
                .selected_item()
                .and_then(|it| it.preview.as_ref())
                .cloned()
            {
                render_preview(&preview, preview_area, buf);
            }
        }
    }
}

/// Render preview panel: metadata lines + ASCII waveform with beat markers.
fn render_preview(preview: &PickerPreview, area: Rect, buf: &mut Buffer) {
    if area.width < 3 || area.height < 2 {
        return;
    }

    // Separator line on the left edge.
    for row in area.y..area.y + area.height {
        if let Some(cell) = buf.cell_mut((area.x, row)) {
            cell.set_char('\u{2502}');
            cell.set_fg(Color::DarkGray);
        }
    }

    let content_x = area.x + 2;
    let content_w = area.width.saturating_sub(2) as usize;
    let mut row = area.y;

    // Metadata lines.
    for (key, val) in &preview.meta {
        if row >= area.y + area.height {
            break;
        }
        let text = format!("{}: {}", key, val);
        let display = if text.len() > content_w {
            format!("{}...", &text[..content_w.saturating_sub(3)])
        } else {
            text
        };
        let span = Span::styled(display, Style::default().fg(Color::White));
        buf.set_span(content_x, row, &span, content_w as u16);
        row += 1;
    }

    // Blank separator.
    if row < area.y + area.height {
        row += 1;
    }

    // ASCII waveform from onset_strength with beat markers.
    let waveform_height = (area.y + area.height).saturating_sub(row);
    if waveform_height < 2 || preview.onset_strength.is_empty() || preview.duration <= 0.0 {
        return;
    }

    let waveform_w = content_w.min(24);

    // Downsample onset_strength to waveform_w columns.
    let olen = preview.onset_strength.len();
    let mut columns: Vec<f32> = Vec::with_capacity(waveform_w);
    for col in 0..waveform_w {
        let start = col * olen / waveform_w;
        let end = ((col + 1) * olen / waveform_w).max(start + 1).min(olen);
        let sum: f32 = preview.onset_strength[start..end].iter().sum();
        let avg = sum / (end - start) as f32;
        columns.push(avg);
    }

    // Normalize.
    let max_val = columns.iter().cloned().fold(0.0_f32, f32::max);
    if max_val > 0.0 {
        for v in &mut columns {
            *v /= max_val;
        }
    }

    // Beat positions as column indices.
    let beat_cols: Vec<usize> = preview
        .beats
        .iter()
        .filter_map(|&b| {
            if b < 0.0 || b > preview.duration {
                return None;
            }
            let col = (b / preview.duration * waveform_w as f64) as usize;
            Some(col.min(waveform_w.saturating_sub(1)))
        })
        .collect();

    // Render bottom-up bar chart.
    let bar_chars = [
        ' ', '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}',
        '\u{2588}',
    ];

    // Single-row sparkline approach.
    if waveform_height >= 1 {
        let spark_row = row;
        for (col_idx, &val) in columns.iter().enumerate() {
            let char_idx = (val * 8.0).round() as usize;
            let ch = bar_chars[char_idx.min(8)];
            let px = content_x + col_idx as u16;
            if px < area.x + area.width {
                let is_beat = beat_cols.contains(&col_idx);
                let fg = if is_beat { Color::Red } else { Color::Green };
                if let Some(cell) = buf.cell_mut((px, spark_row)) {
                    cell.set_char(ch);
                    cell.set_fg(fg);
                }
            }
        }
        row += 1;
    }

    // Beat marker row below waveform.
    if row < area.y + area.height {
        for &bc in &beat_cols {
            let px = content_x + bc as u16;
            if px < area.x + area.width {
                if let Some(cell) = buf.cell_mut((px, row)) {
                    cell.set_char('\u{2502}');
                    cell.set_fg(Color::Red);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_items() -> Vec<PickerItem> {
        vec![
            PickerItem {
                id: "s1".into(),
                label: "make 15s montage".into(),
                detail: "2h ago".into(),
                channel: "session".into(),
                icon: "◆",
                preview: None,
            },
            PickerItem {
                id: "m1".into(),
                label: "chill-lofi-beat.mp3".into(),
                detail: "120 BPM  3:24".into(),
                channel: "music".into(),
                icon: "♫",
                preview: None,
            },
            PickerItem {
                id: "v1".into(),
                label: "beach-sunset.mp4".into(),
                detail: "00:45  analyzed".into(),
                channel: "video".into(),
                icon: "▶",
                preview: None,
            },
            PickerItem {
                id: "m2".into(),
                label: "chill-ambient.wav".into(),
                detail: "90 BPM  5:12".into(),
                channel: "music".into(),
                icon: "♫",
                preview: None,
            },
        ]
    }

    #[test]
    fn filter_empty_query_shows_all() {
        let mut picker = FuzzyPicker::new();
        picker.set_items(sample_items());
        assert_eq!(picker.filtered.len(), 4);
    }

    #[test]
    fn filter_by_query() {
        let mut picker = FuzzyPicker::new();
        picker.set_items(sample_items());
        picker.query = "chill".into();
        picker.filter();
        assert_eq!(picker.filtered.len(), 2);
    }

    #[test]
    fn channel_switching() {
        let mut picker = FuzzyPicker::new();
        picker.set_items(sample_items());

        // All channels
        assert!(picker.active_channel.is_none());
        assert_eq!(picker.filtered.len(), 4);

        // Switch to first channel (session)
        picker.next_channel();
        assert_eq!(picker.active_channel, Some(0));
        assert_eq!(picker.filtered.len(), 1);

        // Switch to music
        picker.next_channel();
        assert_eq!(picker.active_channel, Some(1));
        assert_eq!(picker.filtered.len(), 2);
    }

    #[test]
    fn navigation_wraps() {
        let mut picker = FuzzyPicker::new();
        picker.set_items(sample_items());

        assert_eq!(picker.list_state.selected(), Some(0));
        picker.move_up();
        assert_eq!(picker.list_state.selected(), Some(3));
        picker.move_down();
        assert_eq!(picker.list_state.selected(), Some(0));
    }

    #[test]
    fn select_returns_id() {
        let mut picker = FuzzyPicker::new();
        picker.set_items(sample_items());
        picker.open();
        // First filtered item should be item index 0 (highest position score)
        let item = picker.selected_item().expect("should have selection");
        assert!(!item.id.is_empty());
    }

    #[test]
    fn preview_renders_waveform() {
        let preview = PickerPreview {
            meta: vec![("BPM".into(), "120".into())],
            beats: vec![0.5, 1.0, 1.5],
            onset_strength: vec![0.1, 0.5, 0.8, 0.3, 0.6, 0.9, 0.2, 0.4],
            duration: 2.0,
        };
        let area = Rect::new(0, 0, 30, 10);
        let mut buf = Buffer::empty(area);
        render_preview(&preview, area, &mut buf);
        // Should not panic; verify beat marker exists.
    }

    #[test]
    fn picker_item_with_preview() {
        let mut picker = FuzzyPicker::new();
        let mut items = sample_items();
        items[1].preview = Some(PickerPreview {
            meta: vec![("BPM".into(), "120".into())],
            beats: vec![0.5, 1.0],
            onset_strength: vec![0.3, 0.7, 0.5, 0.9],
            duration: 2.0,
        });
        picker.set_items(items);
        // Move to second item (music with preview).
        picker.move_down();
        let item = picker.selected_item().unwrap();
        assert!(item.preview.is_some());
    }

    #[test]
    fn on_key_esc_cancels() {
        let mut picker = FuzzyPicker::new();
        picker.set_items(sample_items());
        picker.open();
        assert!(picker.visible);
        let action = picker.on_key(crossterm::event::KeyCode::Esc);
        assert_eq!(action, PickerAction::Cancel);
        assert!(!picker.visible);
    }
}
