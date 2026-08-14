//! Terminal user interface components.

pub mod app;
mod conversation;
mod input;
mod status_line;

pub use app::{AppAction, AppState};

use ratatui::Frame;
use ratatui::layout::Rect;

/// Render the conversation, runtime status, and single-line input regions.
pub fn render(frame: &mut Frame, app_state: &mut AppState) {
    let area = frame.area();

    if area.height == 0 {
        app_state.update_conversation_metrics(0, 0);
        return;
    }

    if area.height == 1 {
        app_state.update_conversation_metrics(0, 0);
        input::render(
            frame,
            area,
            app_state.input(),
            app_state.cursor_byte_offset(),
        );
        return;
    }

    let conversation_height = area.height.saturating_sub(2);
    let conversation_area = Rect::new(area.x, area.y, area.width, conversation_height);
    let status_y = area.y.saturating_add(conversation_height);
    let status_area = Rect::new(area.x, status_y, area.width, 1);
    let input_y = status_y.saturating_add(1);
    let input_area = Rect::new(area.x, input_y, area.width, 1);

    let wrapped_lines = conversation::wrap_messages(app_state.messages(), conversation_area.width);
    app_state
        .update_conversation_metrics(wrapped_lines.len(), usize::from(conversation_area.height));

    conversation::render(
        frame,
        conversation_area,
        &wrapped_lines,
        app_state.top_wrapped_line_offset(),
    );
    status_line::render(
        frame,
        status_area,
        app_state.model_name(),
        app_state.runtime_status(),
    );
    input::render(
        frame,
        input_area,
        app_state.input(),
        app_state.cursor_byte_offset(),
    );
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    use super::app::RuntimeStatus;
    use super::{AppState, conversation, render};
    use crate::runtime::HarnessEvent;

    fn draw(terminal: &mut Terminal<TestBackend>, app_state: &mut AppState) {
        terminal
            .draw(|frame| render(frame, app_state))
            .expect("TestBackend drawing should succeed");
    }

    fn resize(terminal: &mut Terminal<TestBackend>, width: u16, height: u16) {
        terminal.backend_mut().resize(width, height);
        terminal
            .resize(Rect::new(0, 0, width, height))
            .expect("TestBackend resizing should succeed");
    }

    fn buffer_line(terminal: &Terminal<TestBackend>, row: u16) -> String {
        let buffer = terminal.backend().buffer();
        let mut line = String::new();
        for column in 0..buffer.area.width {
            line.push_str(buffer[(column, row)].symbol());
        }
        line
    }

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|row| buffer_line(terminal, row))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn key_event(key_code: KeyCode) -> KeyEvent {
        KeyEvent::new(key_code, KeyModifiers::NONE)
    }

    fn type_input(app_state: &mut AppState, input: &str) {
        for character in input.chars() {
            app_state.handle_key(key_event(KeyCode::Char(character)));
        }
    }

    fn settled_conversation() -> AppState {
        let mut app_state = AppState::new("mock-runtime");
        app_state.accept_submission("hello from user".to_owned());
        app_state.handle_harness_event(HarnessEvent::ResponseStarted);
        app_state.handle_harness_event(HarnessEvent::AssistantDelta(
            "hello from assistant".to_owned(),
        ));
        app_state.handle_harness_event(HarnessEvent::ResponseFinished);
        app_state
    }

    #[test]
    fn normal_render_projects_conversation_status_input_and_utf8_cursor() {
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
        let mut app_state = settled_conversation();
        type_input(&mut app_state, "aé界");

        draw(&mut terminal, &mut app_state);

        let text = buffer_text(&terminal);
        assert!(text.contains("You: hello from user"));
        assert!(text.contains("Assistant: hello from assistant"));
        assert!(text.contains("model: mock-runtime | idle"));
        assert!(text.contains("> aé界"));
        assert_eq!(app_state.runtime_status(), &RuntimeStatus::Idle);
        assert_eq!(app_state.content_lines(), 3);
        assert_eq!(app_state.viewport_height(), 6);
        assert!(terminal.backend().cursor_visible());
        assert_eq!(terminal.backend().cursor_position(), (6, 7).into());
    }

    #[test]
    fn responding_and_error_statuses_are_rendered_without_token_placeholders() {
        let mut terminal = Terminal::new(TestBackend::new(60, 4)).unwrap();
        let mut app_state = AppState::new("mock-runtime");
        app_state.accept_submission("request".to_owned());

        draw(&mut terminal, &mut app_state);
        assert!(buffer_line(&terminal, 2).contains("model: mock-runtime | responding"));
        assert!(!buffer_text(&terminal).contains("ctx"));
        assert!(!buffer_text(&terminal).contains("cache"));
        assert!(!buffer_text(&terminal).contains("out"));

        app_state.reject_submission("queue is full".to_owned());
        draw(&mut terminal, &mut app_state);
        assert!(buffer_line(&terminal, 2).contains("model: mock-runtime | error: queue is full"));
    }

    #[test]
    fn wrapping_measurement_and_manual_scroll_use_the_same_lines() {
        let mut terminal = Terminal::new(TestBackend::new(12, 8)).unwrap();
        let mut app_state = AppState::new("mock-runtime");
        app_state
            .accept_submission("abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz".to_owned());
        app_state.handle_harness_event(HarnessEvent::ResponseStarted);
        app_state.handle_harness_event(HarnessEvent::AssistantDelta(
            "0123456789012345678901234567890123456789".to_owned(),
        ));

        draw(&mut terminal, &mut app_state);

        let wrapped_lines = conversation::wrap_messages(app_state.messages(), 12);
        assert_eq!(app_state.content_lines(), wrapped_lines.len());
        assert_eq!(app_state.viewport_height(), 6);
        assert_eq!(
            app_state.top_wrapped_line_offset(),
            wrapped_lines.len().saturating_sub(6)
        );
        assert_eq!(
            buffer_line(&terminal, 0).trim_end(),
            wrapped_lines
                .get(app_state.top_wrapped_line_offset())
                .expect("the auto-follow offset should point at a rendered line")
                .to_string()
        );

        app_state.handle_key(key_event(KeyCode::PageUp));
        app_state.handle_key(key_event(KeyCode::PageDown));
        let manual_offset = app_state.top_wrapped_line_offset();
        assert!(!app_state.is_auto_following());
        assert!(manual_offset > 0);
        draw(&mut terminal, &mut app_state);
        assert_eq!(app_state.top_wrapped_line_offset(), manual_offset);

        app_state.handle_harness_event(HarnessEvent::AssistantDelta(
            " additional streamed content that grows the response".to_owned(),
        ));
        draw(&mut terminal, &mut app_state);
        assert_eq!(app_state.top_wrapped_line_offset(), manual_offset);
        let wrapped_lines_after_growth = conversation::wrap_messages(app_state.messages(), 12);
        assert!(
            buffer_line(&terminal, 0).trim_end()
                == wrapped_lines_after_growth
                    .get(manual_offset)
                    .expect("the manual offset should point at a rendered line")
                    .to_string()
        );

        resize(&mut terminal, 5, 4);
        draw(&mut terminal, &mut app_state);
        assert_eq!(app_state.top_wrapped_line_offset(), manual_offset);
        assert_eq!(
            app_state.content_lines(),
            conversation::wrap_messages(app_state.messages(), 5).len()
        );

        resize(&mut terminal, 60, 8);
        draw(&mut terminal, &mut app_state);
        let clamped_offset = app_state
            .content_lines()
            .saturating_sub(app_state.viewport_height());
        assert_eq!(clamped_offset, 0);
        assert_eq!(app_state.top_wrapped_line_offset(), 0);
    }

    #[test]
    fn status_and_input_take_priority_when_only_two_rows_are_available() {
        let mut terminal = Terminal::new(TestBackend::new(30, 2)).unwrap();
        let mut app_state = settled_conversation();
        type_input(&mut app_state, "draft");

        draw(&mut terminal, &mut app_state);

        assert!(buffer_line(&terminal, 0).contains("model: mock-runtime | idle"));
        assert!(buffer_line(&terminal, 1).starts_with("> draft"));
        assert_eq!(app_state.viewport_height(), 0);
    }

    #[test]
    fn tiny_and_zero_sized_terminals_do_not_panic_or_change_app_content() {
        let mut terminal = Terminal::new(TestBackend::new(20, 6)).unwrap();
        let mut app_state = settled_conversation();
        type_input(&mut app_state, "draft");
        app_state.reject_submission("temporary error".to_owned());

        let expected_messages = app_state.messages().to_owned();
        let expected_input = app_state.input().to_owned();
        let expected_cursor = app_state.cursor_byte_offset();
        let expected_status = app_state.runtime_status().clone();

        for (width, height) in [(1, 2), (2, 2), (1, 1), (0, 1), (2, 0), (0, 0)] {
            resize(&mut terminal, width, height);
            draw(&mut terminal, &mut app_state);

            assert_eq!(app_state.messages(), expected_messages.as_slice());
            assert_eq!(app_state.input(), expected_input);
            assert_eq!(app_state.cursor_byte_offset(), expected_cursor);
            assert_eq!(app_state.runtime_status(), &expected_status);
        }

        resize(&mut terminal, 20, 6);
        draw(&mut terminal, &mut app_state);
        assert_eq!(app_state.messages(), expected_messages.as_slice());
        assert_eq!(app_state.input(), expected_input);
        assert_eq!(app_state.cursor_byte_offset(), expected_cursor);
        assert_eq!(app_state.runtime_status(), &expected_status);
        assert!(buffer_text(&terminal).contains("You: hello from user"));
    }

    #[test]
    fn empty_conversation_is_renderable_at_narrow_dimensions() {
        let mut terminal = Terminal::new(TestBackend::new(1, 1)).unwrap();
        let mut app_state = AppState::new("mock-runtime");

        draw(&mut terminal, &mut app_state);

        assert_eq!(buffer_line(&terminal, 0), ">");
        assert_eq!(terminal.backend().cursor_position(), (0, 0).into());
        assert!(terminal.backend().cursor_visible());
        assert_eq!(app_state.content_lines(), 0);
        assert_eq!(app_state.viewport_height(), 0);
    }
}
