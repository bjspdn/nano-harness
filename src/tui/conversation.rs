use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::app::{Message, MessageRole};

#[derive(Debug, Clone, Default)]
pub(crate) struct WrappedLines {
    lines: Vec<Line<'static>>,
}

impl WrappedLines {
    pub(crate) fn len(&self) -> usize {
        self.lines.len()
    }

    #[cfg(test)]
    pub(crate) fn get(&self, line_index: usize) -> Option<&Line<'static>> {
        self.lines.get(line_index)
    }
}

pub(crate) fn wrap_messages(messages: &[Message], width: u16) -> WrappedLines {
    let width = usize::from(width);
    if width == 0 {
        return WrappedLines::default();
    }

    let mut lines = Vec::new();
    for (message_index, message) in messages.iter().enumerate() {
        if message_index > 0 {
            lines.push(Line::from(String::new()));
        }

        for (content_line_index, content_line) in message.content().split('\n').enumerate() {
            let formatted_line =
                format_message_line(message.role(), content_line, content_line_index);
            wrap_line(&formatted_line, width, &mut lines);
        }
    }

    WrappedLines { lines }
}

pub(crate) fn render(
    frame: &mut Frame,
    area: Rect,
    wrapped_lines: &WrappedLines,
    top_wrapped_line_offset: usize,
) {
    if area.is_empty() {
        return;
    }

    let scroll_offset = u16::try_from(top_wrapped_line_offset).unwrap_or(u16::MAX);
    let paragraph = Paragraph::new(wrapped_lines.lines.clone()).scroll((scroll_offset, 0));
    frame.render_widget(paragraph, area);
}

fn format_message_line(
    message_role: MessageRole,
    content_line: &str,
    content_line_index: usize,
) -> String {
    let label = match message_role {
        MessageRole::User => "You:",
        MessageRole::Assistant => "Assistant:",
    };

    if content_line_index == 0 {
        if content_line.is_empty() {
            return label.to_owned();
        }

        return format!("{label} {content_line}");
    }

    content_line.to_owned()
}

fn wrap_line(content: &str, width: usize, wrapped_lines: &mut Vec<Line<'static>>) {
    if content.is_empty() {
        wrapped_lines.push(Line::from(String::new()));
        return;
    }

    let source_line = Line::from(content);
    let mut current_line = String::new();

    for grapheme in source_line.styled_graphemes(Style::default()) {
        let mut candidate_line = current_line.clone();
        candidate_line.push_str(grapheme.symbol);

        if !current_line.is_empty() && display_width(&candidate_line) > width {
            wrapped_lines.push(Line::from(current_line));
            current_line = String::new();
        }

        current_line.push_str(grapheme.symbol);
    }

    wrapped_lines.push(Line::from(current_line));
}

fn display_width(content: &str) -> usize {
    Line::from(content).width()
}
