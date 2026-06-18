//! Markdown → ratatui lines, modelled on codex's `markdown_render.rs`
//! (`pulldown-cmark` parser + a stateful writer with `MarkdownStyles`). Smaller
//! feature set than codex: headings, paragraphs, emphasis/strong/strike, inline
//! code, fenced & indented code blocks, ordered/unordered lists with nesting,
//! blockquotes, horizontal rules, and links. Line wrapping is left to the TUI's
//! `Paragraph` so styled spans survive.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Style sheet, mirroring codex's `MarkdownStyles`.
struct Styles {
    h: [Style; 6],
    code: Style,
    emphasis: Style,
    strong: Style,
    strike: Style,
    link: Style,
    blockquote: Style,
    code_block: Style,
    hr: Style,
}

impl Styles {
    fn new() -> Self {
        Self {
            h: [
                Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED).fg(Color::Cyan),
                Style::default().add_modifier(Modifier::BOLD).fg(Color::Cyan),
                Style::default().add_modifier(Modifier::BOLD).fg(Color::Blue),
                Style::default().add_modifier(Modifier::BOLD).fg(Color::Blue),
                Style::default().add_modifier(Modifier::ITALIC).fg(Color::Blue),
                Style::default().add_modifier(Modifier::ITALIC).fg(Color::DarkGray),
            ],
            code: Style::default().fg(Color::Cyan),
            emphasis: Style::default().add_modifier(Modifier::ITALIC),
            strong: Style::default().add_modifier(Modifier::BOLD),
            strike: Style::default().add_modifier(Modifier::CROSSED_OUT),
            link: Style::default().fg(Color::Cyan).add_modifier(Modifier::UNDERLINED),
            blockquote: Style::default().fg(Color::Green),
            code_block: Style::default().fg(Color::Gray),
            hr: Style::default().fg(Color::DarkGray),
        }
    }
}

/// Render a markdown document into styled terminal lines.
pub fn render(input: &str) -> Vec<Line<'static>> {
    let styles = Styles::new();
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = Parser::new_ext(input, opts);

    let mut w = Writer {
        styles,
        out: Vec::new(),
        line: Vec::new(),
        inline_style: Style::default(),
        block_style: Style::default(),
        blockquote_depth: 0,
        list_stack: Vec::new(),
        code_buf: String::new(),
        in_code: false,
        pending_blank: false,
        pending_link: None,
    };
    for event in parser {
        w.event(event);
    }
    w.finish();
    w.out
}

struct Writer {
    styles: Styles,
    out: Vec<Line<'static>>,
    line: Vec<Span<'static>>,
    /// Inline style accumulated from nested emphasis/strong.
    inline_style: Style,
    /// Base style for block text (blockquote tint).
    block_style: Style,
    blockquote_depth: usize,
    /// Stack of list counters: `Some(n)` ordered, `None` unordered.
    list_stack: Vec<Option<u64>>,
    code_buf: String,
    in_code: bool,
    pending_blank: bool,
    pending_link: Option<String>,
}

impl Writer {
    fn event(&mut self, ev: Event) {
        match ev {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => {
                if self.in_code {
                    self.code_buf.push_str(&t);
                } else {
                    let style = self.inline_style.patch(self.block_style);
                    self.push_text(&t, style);
                }
            }
            Event::Code(t) => {
                let style = self.styles.code.patch(self.block_style);
                self.push_text(&t, style);
            }
            Event::SoftBreak | Event::HardBreak => {
                if self.in_code {
                    self.code_buf.push('\n');
                } else {
                    self.flush_line();
                }
            }
            Event::Rule => {
                self.flush_line();
                let width = crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(60);
                let bar: String = std::iter::repeat('─').take(width.max(4)).collect();
                self.out.push(Line::from(Span::styled(bar, self.styles.hr)));
                self.pending_blank = true;
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => self.consume_blank(),
            Tag::Heading { level, .. } => {
                self.flush_line();
                let idx = (level as usize).saturating_sub(1).min(5);
                self.inline_style = self.inline_style.patch(self.styles.h[idx]);
            }
            Tag::Emphasis => self.inline_style = self.inline_style.patch(self.styles.emphasis),
            Tag::Strong => self.inline_style = self.inline_style.patch(self.styles.strong),
            Tag::Strikethrough => self.inline_style = self.inline_style.patch(self.styles.strike),
            Tag::BlockQuote(_) => {
                self.blockquote_depth += 1;
                self.block_style = self.block_style.patch(self.styles.blockquote);
                self.consume_blank();
            }
            Tag::CodeBlock(_) => {
                self.flush_line();
                self.in_code = true;
                self.code_buf.clear();
            }
            Tag::List(start) => {
                self.consume_blank();
                self.list_stack.push(start);
            }
            Tag::Item => {
                self.flush_line();
            }
            Tag::Link { dest_url, .. } => {
                self.inline_style = self.inline_style.patch(self.styles.link);
                self.pending_link = Some(dest_url.into_string());
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_) | TagEnd::Paragraph => {
                self.flush_line();
                self.inline_style = self.block_style;
                self.pending_blank = true;
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.inline_style = self.block_style;
            }
            TagEnd::BlockQuote => {
                if self.blockquote_depth > 0 {
                    self.blockquote_depth -= 1;
                }
                self.rebuild_block_style();
                self.pending_blank = true;
            }
            TagEnd::CodeBlock => {
                self.in_code = false;
                self.flush_code_block();
                self.pending_blank = true;
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
                self.pending_blank = true;
            }
            TagEnd::Item => {
                self.flush_line();
            }
            TagEnd::Link => {
                if let Some(url) = self.pending_link.take() {
                    if !url.is_empty() {
                        let style = self.styles.link.patch(self.block_style);
                        self.push_text(&format!(" ({url})"), style);
                    }
                }
                self.inline_style = self.block_style;
            }
            _ => {}
        }
    }

    fn rebuild_block_style(&mut self) {
        let mut s = Style::default();
        for _ in 0..self.blockquote_depth {
            s = s.patch(self.styles.blockquote);
        }
        self.block_style = s;
    }

    fn consume_blank(&mut self) {
        if self.pending_blank {
            self.pending_blank = false;
            self.out.push(Line::default());
        }
    }

    fn push_text(&mut self, t: &str, style: Style) {
        if self.line.is_empty() {
            let prefix = self.current_prefix();
            if !prefix.is_empty() {
                self.line.push(Span::raw(prefix));
            }
        }
        self.line.push(Span::styled(t.to_string(), style));
    }

    /// Leading indentation + list marker for the current line.
    fn current_prefix(&self) -> String {
        let mut s = String::new();
        for _ in 0..self.blockquote_depth {
            s.push_str("│ ");
        }
        let depth = self.list_stack.len();
        if depth > 0 {
            for _ in 0..depth.saturating_sub(1) {
                s.push_str("  ");
            }
            let marker = match self.list_stack.last().copied().flatten() {
                Some(n) => format!("{}. ", n),
                None => "• ".to_string(),
            };
            s.push_str(&marker);
        }
        s
    }

    fn flush_line(&mut self) {
        if !self.line.is_empty() {
            self.out.push(Line::from(std::mem::take(&mut self.line)));
        }
    }

    fn flush_code_block(&mut self) {
        let body = std::mem::take(&mut self.code_buf);
        let prefix = "  ".repeat(self.blockquote_depth);
        for l in body.lines() {
            let mut spans: Vec<Span<'static>> = Vec::new();
            if !prefix.is_empty() {
                spans.push(Span::raw(prefix.clone()));
            }
            spans.push(Span::styled(l.to_string(), self.styles.code_block));
            self.out.push(Line::from(spans));
        }
    }

    fn finish(&mut self) {
        self.flush_line();
        if self.in_code {
            self.flush_code_block();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_heading_and_code() {
        let lines = render("# Title\n\nSome `code` here.\n\n```\nlet x = 1;\n```");
        assert!(lines.iter().any(|l| l.spans.iter().any(|s| s.content.contains("Title"))));
        assert!(lines.iter().any(|l| l.spans.iter().any(|s| s.content.contains("let x = 1;"))));
    }

    #[test]
    fn renders_list_items() {
        let lines = render("- a\n- b\n");
        assert!(lines.iter().any(|l| l.spans.iter().any(|s| s.content.contains("• "))));
    }

    #[test]
    fn renders_bold_inline() {
        let lines = render("**bold**\n");
        let joined: String = lines.iter().flat_map(|l| l.spans.iter()).map(|s| s.content.clone()).collect();
        assert!(joined.contains("bold"));
    }
}
