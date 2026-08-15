use ratatui::Frame;
use ratatui::layout::Rect;
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

    let wrap_options = textwrap::Options::new(width).break_words(true);
    for wrapped_content in textwrap::wrap(content, wrap_options) {
        wrapped_lines.push(Line::from(wrapped_content.into_owned()));
    }
}

#[cfg(test)]
mod tests {
    use ratatui::text::Line;

    use super::{wrap_line, wrap_messages};
    use crate::session::MessageId;
    use crate::tui::app::{Message, MessageRole};

    fn line_texts(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(ToString::to_string).collect()
    }

    fn wrapped_line_texts(messages: &[Message], width: u16) -> Vec<String> {
        let wrapped_lines = wrap_messages(messages, width);
        (0..wrapped_lines.len())
            .map(|line_index| {
                wrapped_lines
                    .get(line_index)
                    .expect("line index should be within wrapped lines")
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn prose_wraps_at_word_boundaries() {
        let mut wrapped_lines = Vec::new();

        wrap_line("some that", 8, &mut wrapped_lines);

        assert_eq!(line_texts(&wrapped_lines), ["some", "that"]);
    }

    #[test]
    fn overlong_words_still_break_within_width() {
        let mut wrapped_lines = Vec::new();

        wrap_line("abcdefgh", 3, &mut wrapped_lines);

        assert_eq!(line_texts(&wrapped_lines), ["abc", "def", "gh"]);
    }

    #[test]
    fn unicode_line_breaks_respect_display_width() {
        let mut wrapped_lines = Vec::new();

        wrap_line("日本語東京", 6, &mut wrapped_lines);

        assert_eq!(line_texts(&wrapped_lines), ["日本語", "東京"]);
        assert!(wrapped_lines.iter().all(|line| line.width() <= 6));
    }

    #[test]
    fn explicit_empty_lines_and_zero_width_are_preserved() {
        let messages = [Message::new(
            MessageId::from_u64(1),
            MessageRole::User,
            "\nsecond\n".to_owned(),
        )];

        assert_eq!(wrapped_line_texts(&messages, 10), ["You:", "second", ""]);
        assert!(wrap_messages(&messages, 0).lines.is_empty());
    }

    #[test]
    fn role_prefix_only_applies_to_the_first_content_line() {
        let messages = [Message::new(
            MessageId::from_u64(1),
            MessageRole::Assistant,
            "first\nsecond".to_owned(),
        )];

        assert_eq!(
            wrapped_line_texts(&messages, 20),
            ["Assistant: first", "second"]
        );
    }
}
