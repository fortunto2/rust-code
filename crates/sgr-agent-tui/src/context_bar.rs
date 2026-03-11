//! Compact project context bar widget.
//!
//! Renders a single-line status showing project info:
//! `📂 my-trip.l2f | 3/5 files | 42 seg | ⭐ 0.87 | ♫ song.mp3 | ⏱ 15.0s`

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

/// Project context data for display in the status bar.
#[derive(Debug, Clone, Default)]
pub struct ProjectContext {
    pub name: String,
    pub files_total: usize,
    pub files_analyzed: usize,
    pub segments_total: usize,
    pub top_score: f32,
    pub timeline_secs: Option<f64>,
    pub music_track: Option<String>,
}

impl ProjectContext {
    /// Render as a single-line bar in the given area.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if self.name.is_empty() {
            let widget = Paragraph::new("No project")
                .style(Style::default().fg(Color::DarkGray))
                .block(Block::default().borders(Borders::ALL).title(" Context "));
            widget.render(area, buf);
            return;
        }

        let mut parts = vec![
            format!("\u{1F4C2} {}", self.name),
            format!("{}/{} files", self.files_analyzed, self.files_total),
            format!("{} seg", self.segments_total),
        ];

        if self.top_score > 0.0 {
            parts.push(format!("\u{2B50} {:.2}", self.top_score));
        }

        if let Some(ref track) = self.music_track {
            // Truncate long names.
            let name = if track.len() > 20 {
                format!("{}...", &track[..17])
            } else {
                track.clone()
            };
            parts.push(format!("\u{266B} {}", name));
        }

        if let Some(secs) = self.timeline_secs {
            parts.push(format!("\u{23F1} {:.1}s", secs));
        }

        let text = parts.join(" \u{2502} ");
        let widget = Paragraph::new(text)
            .style(Style::default().fg(Color::White))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Context ")
                    .border_style(Style::default().fg(Color::DarkGray)),
            );
        widget.render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;

    #[test]
    fn empty_context_renders() {
        let ctx = ProjectContext::default();
        let area = Rect::new(0, 0, 40, 3);
        let mut buf = Buffer::empty(area);
        ctx.render(area, &mut buf);
        // Should not panic.
    }

    #[test]
    fn full_context_renders() {
        let ctx = ProjectContext {
            name: "trip.l2f".into(),
            files_total: 5,
            files_analyzed: 3,
            segments_total: 42,
            top_score: 0.87,
            timeline_secs: Some(15.0),
            music_track: Some("song.mp3".into()),
        };
        let area = Rect::new(0, 0, 80, 3);
        let mut buf = Buffer::empty(area);
        ctx.render(area, &mut buf);
    }
}
