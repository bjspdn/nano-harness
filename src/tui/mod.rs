//! Terminal user interface components.

pub mod app;
mod conversation;
mod input;
pub mod model_picker;
mod status_line;

pub use app::{AppAction, AppState};

use ratatui::Frame;
use ratatui::layout::Rect;

/// Render the conversation, runtime status, and single-line input regions.
pub fn render(frame: &mut Frame, app_state: &mut AppState) {
    let area = frame.area();

    if area.height == 0 {
        app_state.update_conversation_metrics(0, 0);
        model_picker::render(frame, area, app_state.model_picker(), app_state.model_id());
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
        model_picker::render(frame, area, app_state.model_picker(), app_state.model_id());
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
        app_state.usage(),
    );
    input::render(
        frame,
        input_area,
        app_state.input(),
        app_state.cursor_byte_offset(),
    );
    model_picker::render(frame, area, app_state.model_picker(), app_state.model_id());
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    use super::app::RuntimeStatus;
    use super::{AppState, conversation, render};
    use crate::provider::{CompletionOutcome, ModelLimits, ModelMetadata, Usage};
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
        let mut app_state = AppState::new("mock-runtime", "mock-runtime");
        app_state.accept_submission("hello from user".to_owned());
        app_state.handle_harness_event(HarnessEvent::ResponseStarted);
        app_state.handle_harness_event(HarnessEvent::AssistantDelta(
            "hello from assistant".to_owned(),
        ));
        app_state.handle_harness_event(HarnessEvent::Usage(Usage {
            input_tokens: 24,
            cached_input_tokens: 8,
            output_tokens: 16,
        }));
        app_state.handle_harness_event(HarnessEvent::ResponseFinished(CompletionOutcome::Complete));
        app_state
    }

    fn model_metadata(
        model_id: &str,
        display_name: &str,
        prompt_price: Option<&str>,
        completion_price: Option<&str>,
    ) -> ModelMetadata {
        ModelMetadata {
            model_id: model_id.to_owned(),
            display_name: display_name.to_owned(),
            limits: ModelLimits {
                context_window_tokens: 16_384,
                maximum_output_tokens: Some(2_048),
            },
            prompt_price_usd_per_million_tokens: prompt_price.map(str::to_owned),
            completion_price_usd_per_million_tokens: completion_price.map(str::to_owned),
        }
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
    fn completed_usage_is_rendered_with_exact_raw_counts() {
        let mut terminal = Terminal::new(TestBackend::new(80, 4)).unwrap();
        let mut app_state = settled_conversation();

        draw(&mut terminal, &mut app_state);

        assert!(
            buffer_line(&terminal, 2)
                .contains("model: mock-runtime | idle | in 24 | cache 8 | out 16")
        );
    }

    #[test]
    fn responding_and_error_statuses_are_rendered_without_token_placeholders() {
        let mut terminal = Terminal::new(TestBackend::new(80, 4)).unwrap();
        let mut app_state = AppState::new("mock-runtime", "mock-runtime");
        app_state.accept_submission("request".to_owned());

        draw(&mut terminal, &mut app_state);
        assert!(buffer_line(&terminal, 2).contains("model: mock-runtime | responding"));
        assert!(!buffer_text(&terminal).contains("ctx"));
        assert!(!buffer_text(&terminal).contains("cache"));
        assert!(!buffer_text(&terminal).contains("out"));

        app_state.handle_harness_event(HarnessEvent::Usage(Usage {
            input_tokens: 12,
            cached_input_tokens: 4,
            output_tokens: 8,
        }));
        app_state.handle_harness_event(HarnessEvent::Error("runtime failed".to_owned()));
        draw(&mut terminal, &mut app_state);
        assert!(
            buffer_line(&terminal, 2)
                .contains("model: mock-runtime | error: runtime failed | in 12 | cache 4 | out 8")
        );
    }

    #[test]
    fn length_limited_status_is_visible_and_retry_starts_without_old_usage() {
        let mut terminal = Terminal::new(TestBackend::new(80, 6)).unwrap();
        let mut app_state = AppState::new("mock-runtime", "mock-runtime");
        app_state.accept_submission("request".to_owned());
        app_state.handle_harness_event(HarnessEvent::ResponseStarted);
        app_state.handle_harness_event(HarnessEvent::AssistantDelta("partial".to_owned()));
        app_state.handle_harness_event(HarnessEvent::Usage(Usage {
            input_tokens: 42,
            cached_input_tokens: 17,
            output_tokens: 99,
        }));
        app_state.handle_harness_event(HarnessEvent::ResponseFinished(
            CompletionOutcome::LengthLimited,
        ));

        draw(&mut terminal, &mut app_state);

        assert!(buffer_text(&terminal).contains("Assistant: partial"));
        assert!(buffer_line(&terminal, 4).contains(
            "model: mock-runtime | truncated: output length limit | in 42 | cache 17 | out 99"
        ));

        app_state.accept_submission("retry".to_owned());
        draw(&mut terminal, &mut app_state);

        assert!(buffer_line(&terminal, 4).contains("model: mock-runtime | responding"));
        assert!(!buffer_text(&terminal).contains("| in 42 | cache 17 | out 99"));
        assert!(buffer_text(&terminal).contains("Assistant: partial"));
    }

    #[test]
    fn truncated_status_stays_one_row_at_narrow_dimensions() {
        let mut terminal = Terminal::new(TestBackend::new(20, 4)).unwrap();
        let mut app_state = AppState::new("mock-runtime", "mock-runtime");
        app_state.accept_submission("request".to_owned());
        app_state.handle_harness_event(HarnessEvent::Usage(Usage {
            input_tokens: 100,
            cached_input_tokens: 50,
            output_tokens: 200,
        }));
        app_state.handle_harness_event(HarnessEvent::ResponseFinished(
            CompletionOutcome::LengthLimited,
        ));

        draw(&mut terminal, &mut app_state);

        let status_line = buffer_line(&terminal, 2);
        assert_eq!(status_line.chars().count(), 20);
        assert_eq!(status_line.trim_end(), "model: mock-runti...");
    }

    #[test]
    fn long_error_status_is_marked_without_affecting_the_input_row() {
        let mut terminal = Terminal::new(TestBackend::new(36, 4)).unwrap();
        let mut app_state = AppState::new("mock-runtime", "mock-runtime");
        type_input(&mut app_state, "draft");
        app_state.reject_submission("runtime failure with a deliberately long message".to_owned());

        draw(&mut terminal, &mut app_state);

        let status_line = buffer_line(&terminal, 2);
        assert_eq!(status_line.chars().count(), 36);
        assert!(status_line.trim_end().ends_with("..."));
        assert!(!status_line.contains("deliberately long message"));
        assert_eq!(buffer_line(&terminal, 3).trim_end(), "> draft");
    }

    #[test]
    fn wrapping_measurement_and_manual_scroll_use_the_same_lines() {
        let mut terminal = Terminal::new(TestBackend::new(12, 8)).unwrap();
        let mut app_state = AppState::new("mock-runtime", "mock-runtime");
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
        let mut app_state = AppState::new("mock-runtime", "mock-runtime");

        draw(&mut terminal, &mut app_state);

        assert_eq!(buffer_line(&terminal, 0), ">");
        assert_eq!(terminal.backend().cursor_position(), (0, 0).into());
        assert!(terminal.backend().cursor_visible());
        assert_eq!(app_state.content_lines(), 0);
        assert_eq!(app_state.viewport_height(), 0);
    }

    #[test]
    fn picker_overlay_renders_loading_search_and_owns_the_cursor() {
        let mut terminal = Terminal::new(TestBackend::new(60, 8)).unwrap();
        let mut app_state = AppState::new("current-id", "Current model");
        app_state.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        type_input(&mut app_state, "draft");
        draw(&mut terminal, &mut app_state);

        let text = buffer_text(&terminal);
        assert!(text.contains("Models"));
        assert!(text.contains("Search: "));
        assert!(text.contains("Loading models..."));
        assert!(text.contains("Enter select"));
        assert_eq!(app_state.input(), "");
        assert!(terminal.backend().cursor_position().y < 7);
    }

    #[test]
    fn picker_overlay_renders_sorted_rows_metadata_prices_and_search_filter() {
        let mut terminal = Terminal::new(TestBackend::new(100, 10)).unwrap();
        let mut app_state = AppState::new("current-id", "Current model");
        app_state.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        app_state.handle_harness_event(HarnessEvent::CatalogLoaded(vec![
            model_metadata("z-id", "Zulu", None, None),
            model_metadata("current-id", "Current model", Some("0.10"), Some("0.40")),
            model_metadata("alpha-id", "Alpha", Some("0.15"), Some("0.60")),
        ]));
        draw(&mut terminal, &mut app_state);

        let text = buffer_text(&terminal);
        assert!(text.contains("Current model | current-id | ctx 16384 | in $0.10/M | out $0.40/M"));
        assert!(text.contains("Alpha | alpha-id | ctx 16384 | in $0.15/M | out $0.60/M"));
        assert!(text.contains("Zulu | z-id | ctx 16384 | in unknown | out unknown"));

        for character in "alpha".chars() {
            app_state.handle_key(key_event(KeyCode::Char(character)));
        }
        draw(&mut terminal, &mut app_state);
        let filtered_text = buffer_text(&terminal);
        assert!(filtered_text.contains("Search: alpha"));
        assert!(filtered_text.contains("Alpha | alpha-id"));
        assert!(!filtered_text.contains("Zulu | z-id"));
        assert!(filtered_text.contains('>'));
    }

    #[test]
    fn picker_overlay_renders_failure_and_pending_selection_without_fallback() {
        let mut terminal = Terminal::new(TestBackend::new(80, 8)).unwrap();
        let alternate_model = model_metadata("alternate-id", "Alternate", None, None);
        let mut app_state = AppState::new("current-id", "Current model");
        app_state.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        app_state.handle_harness_event(HarnessEvent::CatalogLoaded(vec![alternate_model.clone()]));
        app_state.accept_model_selection("alternate-id".to_owned());
        draw(&mut terminal, &mut app_state);
        assert!(buffer_text(&terminal).contains("Waiting for runtime selection..."));
        assert!(buffer_text(&terminal).contains("Alternate | alternate-id"));
        assert_eq!(app_state.model_id(), "current-id");

        app_state.handle_harness_event(HarnessEvent::ModelSelectionFailed(
            "selection\nrejected".to_owned(),
        ));
        draw(&mut terminal, &mut app_state);
        assert!(buffer_text(&terminal).contains("Error: selection rejected"));
        assert!(app_state.model_picker().is_open());
        assert_eq!(app_state.model_id(), "current-id");

        app_state.accept_model_selection("alternate-id".to_owned());
        app_state.handle_harness_event(HarnessEvent::ModelSelected(alternate_model));
        draw(&mut terminal, &mut app_state);
        assert!(!app_state.model_picker().is_open());
        assert_eq!(app_state.model_id(), "alternate-id");
        assert!(!buffer_text(&terminal).contains("Waiting for runtime selection"));
    }

    #[test]
    fn picker_overlay_is_safe_at_tiny_and_zero_sized_dimensions() {
        let mut terminal = Terminal::new(TestBackend::new(20, 6)).unwrap();
        let mut app_state = AppState::new("current-id", "Current model");
        app_state.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        app_state.handle_harness_event(HarnessEvent::CatalogLoaded(vec![model_metadata(
            "model-id",
            "Model",
            Some("0.1"),
            Some("0.2"),
        )]));

        for (width, height) in [(1, 2), (2, 2), (1, 1), (0, 1), (2, 0), (0, 0)] {
            resize(&mut terminal, width, height);
            draw(&mut terminal, &mut app_state);
        }

        resize(&mut terminal, 20, 6);
        draw(&mut terminal, &mut app_state);
        assert!(buffer_text(&terminal).contains("Models"));
        assert_eq!(app_state.model_id(), "current-id");
    }
}
