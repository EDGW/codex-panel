//! Shared card styling and wrapping for host and destination-owned settings.
use ratatui::{
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};

pub(crate) fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

pub(crate) fn primary() -> Style {
    Style::default().fg(Color::Reset)
}

pub(crate) fn secondary() -> Style {
    Style::default().fg(super::theme::secondary_color())
}

pub(crate) fn accent() -> Style {
    Style::default().fg(Color::Blue)
}

pub(crate) fn warning() -> Style {
    Style::default()
        .fg(Color::Yellow)
        .add_modifier(ratatui::style::Modifier::BOLD)
}

pub(crate) fn field(label: &str, value: &str, style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{}: ", clean(label)), secondary()),
        Span::styled(clean(value), style),
    ])
}

pub(crate) fn paragraph<'a>(title: &'a str, text: impl Into<Text<'a>>) -> Paragraph<'a> {
    Paragraph::new(text)
        .style(primary())
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .title(Span::styled(
                    title,
                    accent().add_modifier(ratatui::style::Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(secondary()),
        )
}

pub(crate) fn height(paragraph: &Paragraph<'_>, width: u16) -> u16 {
    paragraph.line_count(width.max(3)).min(u16::MAX as usize) as u16
}
