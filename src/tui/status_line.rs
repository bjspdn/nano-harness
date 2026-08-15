use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::app::RuntimeStatus;
use crate::provider::Usage;

const STATUS_ELLIPSIS: &str = "...";

pub(crate) fn render(
    frame: &mut Frame,
    area: Rect,
    model_name: &str,
    runtime_status: &RuntimeStatus,
    usage: Option<Usage>,
) {
    if area.is_empty() {
        return;
    }

    let status = match runtime_status {
        RuntimeStatus::Idle => "idle".to_owned(),
        RuntimeStatus::Responding => "responding".to_owned(),
        RuntimeStatus::Truncated => "truncated: output length limit".to_owned(),
        RuntimeStatus::Error(error) => format!("error: {}", single_line(error)),
    };
    let usage = usage
        .map(|usage| {
            format!(
                " | in {} | cache {} | out {}",
                usage.input_tokens, usage.cached_input_tokens, usage.output_tokens
            )
        })
        .unwrap_or_default();
    let status_line = format!("model: {} | {status}{usage}", single_line(model_name));
    let area = Rect { height: 1, ..area };
    let status_line = truncate_status_line(&status_line, usize::from(area.width));
    frame.render_widget(Paragraph::new(status_line), area);
}

fn truncate_status_line(content: &str, maximum_width: usize) -> String {
    let line = Line::from(content);
    if line.width() <= maximum_width {
        return content.to_owned();
    }

    let ellipsis_width = Line::from(STATUS_ELLIPSIS).width();
    if maximum_width < ellipsis_width {
        return take_grapheme_prefix(&line, maximum_width);
    }

    let prefix = take_grapheme_prefix(&line, maximum_width - ellipsis_width);
    format!("{prefix}{STATUS_ELLIPSIS}")
}

fn take_grapheme_prefix(line: &Line<'_>, maximum_width: usize) -> String {
    let mut prefix = String::new();
    let mut prefix_width = 0;

    for grapheme in line.styled_graphemes(Style::default()) {
        let grapheme_width = Line::from(grapheme.symbol).width();
        if grapheme_width > maximum_width.saturating_sub(prefix_width) {
            break;
        }

        prefix.push_str(grapheme.symbol);
        prefix_width += grapheme_width;
    }

    prefix
}

fn single_line(content: &str) -> String {
    content
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use ratatui::text::Line;

    use super::truncate_status_line;

    #[test]
    fn short_and_exact_fit_status_lines_are_unchanged() {
        assert_eq!(truncate_status_line("idle", 10), "idle");
        assert_eq!(truncate_status_line("exact", 5), "exact");
    }

    #[test]
    fn ascii_status_lines_are_marked_when_truncated() {
        assert_eq!(truncate_status_line("abcdefghijk", 8), "abcde...");
    }

    #[test]
    fn unicode_truncation_keeps_double_width_graphemes_together() {
        let truncated = truncate_status_line("a👨‍👩‍👧‍👦bcdx", 6);

        assert_eq!(truncated, "a👨‍👩‍👧‍👦...");
        assert_eq!(Line::from(truncated.as_str()).width(), 6);
    }

    #[test]
    fn tiny_widths_use_a_safe_grapheme_prefix() {
        assert_eq!(truncate_status_line("abcdef", 0), "");
        assert_eq!(truncate_status_line("abcdef", 1), "a");
        assert_eq!(truncate_status_line("abcdef", 2), "ab");
        assert_eq!(truncate_status_line("界abcdef", 1), "");
        assert_eq!(truncate_status_line("界abcdef", 2), "界");
    }
}
