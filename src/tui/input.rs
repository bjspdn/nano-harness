use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;

const PROMPT: &str = "> ";

pub(crate) fn render(frame: &mut Frame, area: Rect, input: &str, cursor_byte_offset: usize) {
    if area.is_empty() {
        return;
    }

    let area = Rect { height: 1, ..area };
    let cursor_byte_offset = valid_cursor_byte_offset(input, cursor_byte_offset);

    if area.width == 1 {
        frame.render_widget(Line::from(">"), area);
        frame.set_cursor_position((area.x, area.y));
        return;
    }

    let input_width = usize::from(area.width.saturating_sub(PROMPT.len() as u16));
    let cursor_display_width = display_width(&input[..cursor_byte_offset]);
    let visible_input_start =
        visible_input_start(input, cursor_byte_offset, cursor_display_width, input_width);
    let relative_cursor_width = display_width(&input[visible_input_start..cursor_byte_offset]);
    let visible_input = clip_to_width(&input[visible_input_start..], input_width);

    let mut rendered_input = String::with_capacity(PROMPT.len() + visible_input.len());
    rendered_input.push_str(PROMPT);
    rendered_input.push_str(&visible_input);
    frame.render_widget(Line::from(rendered_input), area);

    let cursor_offset = PROMPT
        .len()
        .saturating_add(relative_cursor_width)
        .min(usize::from(area.width).saturating_sub(1));
    let cursor_offset = u16::try_from(cursor_offset).unwrap_or(u16::MAX);
    let cursor_x = area.x.saturating_add(cursor_offset);
    frame.set_cursor_position((cursor_x, area.y));
}

fn valid_cursor_byte_offset(input: &str, cursor_byte_offset: usize) -> usize {
    let mut cursor_byte_offset = cursor_byte_offset.min(input.len());
    while cursor_byte_offset > 0 && !input.is_char_boundary(cursor_byte_offset) {
        cursor_byte_offset -= 1;
    }
    cursor_byte_offset
}

fn visible_input_start(
    input: &str,
    cursor_byte_offset: usize,
    cursor_display_width: usize,
    input_width: usize,
) -> usize {
    let maximum_cursor_width = input_width.saturating_sub(1);
    if cursor_display_width <= maximum_cursor_width {
        return 0;
    }

    for (byte_offset, _) in input[..cursor_byte_offset].char_indices() {
        if display_width(&input[byte_offset..cursor_byte_offset]) <= maximum_cursor_width {
            return byte_offset;
        }
    }

    cursor_byte_offset
}

fn clip_to_width(input: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }

    let mut clipped = String::new();
    for character in input.chars() {
        let mut candidate = clipped.clone();
        candidate.push(character);
        if display_width(&candidate) > width {
            break;
        }
        clipped.push(character);
    }
    clipped
}

fn display_width(content: &str) -> usize {
    Line::from(content).width()
}
