use crate::render::renderable::Renderable;
use crate::render::renderable::RowRenderable;
use crate::style::accent_style;
use ratatui::style::Style;
use ratatui::style::Styled as _;
use ratatui::style::Stylize as _;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use unicode_width::UnicodeWidthStr;

pub(crate) fn selection_option_row(
    index: usize,
    label: String,
    is_selected: bool,
) -> Box<dyn Renderable> {
    selection_option_row_with_dim(index, label, is_selected, /*dim*/ false)
}

pub(crate) fn plain_selection_option_row(label: String, is_selected: bool) -> Box<dyn Renderable> {
    let prefix = if is_selected { "› " } else { "  " };
    let style = if is_selected {
        accent_style()
    } else {
        Style::default()
    };
    let mut row = RowRenderable::new();
    row.push(
        UnicodeWidthStr::width(prefix) as u16,
        prefix.set_style(style),
    );
    row.push(
        u16::MAX,
        Paragraph::new(label)
            .style(style)
            .wrap(Wrap { trim: false }),
    );
    row.into()
}

pub(crate) fn selection_option_row_indented(
    index: usize,
    label: String,
    is_selected: bool,
    indent: usize,
) -> Box<dyn Renderable> {
    let prefix = if is_selected {
        format!("{}› {}. ", " ".repeat(indent), index + 1)
    } else {
        format!("{}  {}. ", " ".repeat(indent), index + 1)
    };
    let style = if is_selected {
        accent_style()
    } else {
        Style::default()
    };
    let prefix_width = UnicodeWidthStr::width(prefix.as_str()) as u16;
    let mut row = RowRenderable::new();
    row.push(prefix_width, prefix.set_style(style));
    row.push(
        u16::MAX,
        Paragraph::new(label)
            .style(style)
            .wrap(Wrap { trim: false }),
    );
    row.into()
}

pub(crate) fn selection_option_row_with_dim(
    index: usize,
    label: String,
    is_selected: bool,
    dim: bool,
) -> Box<dyn Renderable> {
    let prefix = if is_selected {
        format!("› {}. ", index + 1)
    } else {
        format!("  {}. ", index + 1)
    };
    let style = if is_selected {
        accent_style()
    } else if dim {
        Style::default().dim()
    } else {
        Style::default()
    };
    let prefix_width = UnicodeWidthStr::width(prefix.as_str()) as u16;
    let mut row = RowRenderable::new();
    row.push(prefix_width, prefix.set_style(style));
    row.push(
        u16::MAX,
        Paragraph::new(label)
            .style(style)
            .wrap(Wrap { trim: false }),
    );
    row.into()
}
