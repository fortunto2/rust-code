use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use syntect::{
    easy::HighlightLines, highlighting::ThemeSet, parsing::SyntaxSet, util::LinesWithEndings,
};

pub struct CodeHighlighter {
    ps: SyntaxSet,
    ts: ThemeSet,
}

impl CodeHighlighter {
    pub fn new() -> Self {
        Self {
            ps: SyntaxSet::load_defaults_newlines(),
            ts: ThemeSet::load_defaults(),
        }
    }

    pub fn highlight<'a>(&self, code: &'a str, path: &str) -> Vec<Line<'a>> {
        let syntax = self
            .ps
            .find_syntax_for_file(path)
            .unwrap_or(None)
            .unwrap_or_else(|| self.ps.find_syntax_plain_text());

        let theme = &self.ts.themes["base16-ocean.dark"];
        let mut h = HighlightLines::new(syntax, theme);

        let mut lines = Vec::new();
        for line_str in LinesWithEndings::from(code) {
            let ranges: Vec<(syntect::highlighting::Style, &str)> =
                h.highlight_line(line_str, &self.ps).unwrap_or_default();

            let mut spans = Vec::new();
            for (style, text) in ranges {
                let fg = style.foreground;
                let tui_style = Style::default().fg(Color::Rgb(fg.r, fg.g, fg.b));
                spans.push(Span::styled(text.to_string(), tui_style));
            }
            lines.push(Line::from(spans));
        }

        lines
    }
}
