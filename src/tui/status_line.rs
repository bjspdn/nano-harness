use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;

use super::app::RuntimeStatus;

pub(crate) fn render(
    frame: &mut Frame,
    area: Rect,
    model_name: &str,
    runtime_status: &RuntimeStatus,
) {
    if area.is_empty() {
        return;
    }

    let status = match runtime_status {
        RuntimeStatus::Idle => "idle".to_owned(),
        RuntimeStatus::Responding => "responding".to_owned(),
        RuntimeStatus::Error(error) => format!("error: {}", single_line(error)),
    };
    let status_line = format!("model: {} | {status}", single_line(model_name));
    let area = Rect { height: 1, ..area };
    frame.render_widget(Paragraph::new(status_line), area);
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
