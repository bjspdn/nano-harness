use crate::provider::{CompletionOutcome, ModelMetadata, Usage};
use crate::runtime::HarnessEvent;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::model_picker::{ModelPickerAction, ModelPickerState};

/// The role of a displayable conversation message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
}

/// A displayable conversation message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    role: MessageRole,
    content: String,
}

impl Message {
    pub fn new(role: MessageRole, content: String) -> Self {
        Self { role, content }
    }

    pub fn role(&self) -> MessageRole {
        self.role
    }

    pub fn content(&self) -> &str {
        &self.content
    }
}

/// The transient status shown for the runtime turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeStatus {
    Idle,
    Responding,
    Truncated,
    Error(String),
}

/// The result of handling one logical key event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppAction {
    Continue,
    Submit(String),
    OpenModelPicker,
    SelectModel(String),
    Exit,
}

/// Transient state projected by the TUI and consumed by rendering and orchestration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppState {
    model_id: String,
    model_name: String,
    model_picker: ModelPickerState,
    messages: Vec<Message>,
    input: String,
    cursor_byte_offset: usize,
    runtime_status: RuntimeStatus,
    usage: Option<Usage>,
    active_assistant_message: Option<usize>,
    top_wrapped_line_offset: usize,
    auto_follow: bool,
    content_lines: usize,
    viewport_height: usize,
}

impl AppState {
    pub fn new(model_id: impl Into<String>, model_name: impl Into<String>) -> Self {
        Self {
            model_id: model_id.into(),
            model_name: model_name.into(),
            model_picker: ModelPickerState::new(),
            messages: Vec::new(),
            input: String::new(),
            cursor_byte_offset: 0,
            runtime_status: RuntimeStatus::Idle,
            usage: None,
            active_assistant_message: None,
            top_wrapped_line_offset: 0,
            auto_follow: true,
            content_lines: 0,
            viewport_height: 0,
        }
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub fn model_name(&self) -> &str {
        &self.model_name
    }

    pub fn model_picker(&self) -> &ModelPickerState {
        &self.model_picker
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn input(&self) -> &str {
        &self.input
    }

    /// The editor cursor as a byte offset, always on a UTF-8 character boundary.
    pub fn cursor_byte_offset(&self) -> usize {
        self.cursor_byte_offset
    }

    pub fn runtime_status(&self) -> &RuntimeStatus {
        &self.runtime_status
    }

    pub fn usage(&self) -> Option<Usage> {
        self.usage
    }

    pub fn is_responding(&self) -> bool {
        matches!(self.runtime_status, RuntimeStatus::Responding)
    }

    #[cfg(test)]
    pub fn active_assistant_message(&self) -> Option<&Message> {
        self.active_assistant_message
            .and_then(|message_index| self.messages.get(message_index))
    }

    pub fn top_wrapped_line_offset(&self) -> usize {
        self.top_wrapped_line_offset
    }

    #[cfg(test)]
    pub fn is_auto_following(&self) -> bool {
        self.auto_follow
    }

    #[cfg(test)]
    pub fn content_lines(&self) -> usize {
        self.content_lines
    }

    #[cfg(test)]
    pub fn viewport_height(&self) -> usize {
        self.viewport_height
    }

    pub fn handle_key(&mut self, key_event: KeyEvent) -> AppAction {
        if key_event.kind == KeyEventKind::Release {
            return AppAction::Continue;
        }

        let has_control_modifier = key_event.modifiers.contains(KeyModifiers::CONTROL);

        if has_control_modifier && is_control_c(key_event.code) {
            return AppAction::Exit;
        }

        if has_control_modifier && is_control_p(key_event.code) {
            if self.is_responding() {
                return AppAction::Continue;
            }

            self.model_picker.open();
            return AppAction::OpenModelPicker;
        }

        if self.model_picker.is_open() {
            return match self.model_picker.handle_key(key_event, &self.model_id) {
                ModelPickerAction::Continue => AppAction::Continue,
                ModelPickerAction::SelectModel(model_id) => AppAction::SelectModel(model_id),
            };
        }

        if key_event.code == KeyCode::Esc {
            if self.input.is_empty() {
                return AppAction::Exit;
            }

            self.input.clear();
            self.cursor_byte_offset = 0;
            return AppAction::Continue;
        }

        if has_control_modifier && key_event.code == KeyCode::End {
            self.auto_follow = true;
            self.top_wrapped_line_offset = self.maximum_top_wrapped_line_offset();
            return AppAction::Continue;
        }

        match key_event.code {
            KeyCode::Char(character) => {
                if !key_event
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && !character.is_control()
                {
                    self.input.insert(self.cursor_byte_offset, character);
                    self.cursor_byte_offset += character.len_utf8();
                }
            }
            KeyCode::Left => self.move_cursor_left(),
            KeyCode::Right => self.move_cursor_right(),
            KeyCode::Home => self.cursor_byte_offset = 0,
            KeyCode::End => self.cursor_byte_offset = self.input.len(),
            KeyCode::Backspace => self.delete_previous_character(),
            KeyCode::Delete => self.delete_next_character(),
            KeyCode::Enter => {
                if self.is_responding() || self.input.is_empty() {
                    return AppAction::Continue;
                }

                return AppAction::Submit(self.input.clone());
            }
            KeyCode::PageUp => {
                self.auto_follow = false;
                self.top_wrapped_line_offset = self
                    .top_wrapped_line_offset
                    .saturating_sub(self.viewport_height);
            }
            KeyCode::PageDown => {
                self.auto_follow = false;
                self.top_wrapped_line_offset = self
                    .top_wrapped_line_offset
                    .saturating_add(self.viewport_height)
                    .min(self.maximum_top_wrapped_line_offset());
            }
            _ => {}
        }

        AppAction::Continue
    }

    /// Commit a submission after the runtime command has been enqueued successfully.
    pub fn accept_submission(&mut self, submission: String) {
        if submission.is_empty() || self.is_responding() {
            return;
        }

        self.messages
            .push(Message::new(MessageRole::User, submission));
        self.input.clear();
        self.cursor_byte_offset = 0;
        self.runtime_status = RuntimeStatus::Responding;
        self.usage = None;
        self.active_assistant_message = None;
    }

    /// Record a failed command enqueue without losing the draft or conversation.
    pub fn reject_submission(&mut self, error: String) {
        self.runtime_status = RuntimeStatus::Error(error);
    }

    /// Record a successful model-selection command enqueue without changing the active model.
    pub fn accept_model_selection(&mut self, model_id: String) {
        self.model_picker.accept_selection_enqueue(model_id);
    }

    /// Record a failed model-selection command enqueue without changing the active model.
    pub fn reject_model_selection(&mut self, error: String) {
        self.model_picker.reject_selection_enqueue(error);
    }

    /// Record a failed model-discovery command enqueue inside the open picker.
    pub fn reject_model_picker_open(&mut self, error: String) {
        self.model_picker.reject_discovery_enqueue(error);
    }

    pub fn handle_harness_event(&mut self, harness_event: HarnessEvent) {
        match harness_event {
            HarnessEvent::ResponseStarted => {
                if !self.is_responding() || self.active_assistant_message.is_some() {
                    return;
                }

                let message_index = self.messages.len();
                self.messages
                    .push(Message::new(MessageRole::Assistant, String::new()));
                self.active_assistant_message = Some(message_index);
            }
            HarnessEvent::AssistantDelta(response_delta) => {
                if !self.is_responding() {
                    return;
                }

                let Some(message_index) = self.active_assistant_message else {
                    return;
                };
                let Some(message) = self.messages.get_mut(message_index) else {
                    return;
                };
                if message.role != MessageRole::Assistant {
                    return;
                }

                message.content.push_str(&response_delta);
            }
            HarnessEvent::ResponseFinished(completion_outcome) => {
                if !self.is_responding() {
                    return;
                }

                self.active_assistant_message = None;
                self.runtime_status = match completion_outcome {
                    CompletionOutcome::Complete => RuntimeStatus::Idle,
                    CompletionOutcome::LengthLimited => RuntimeStatus::Truncated,
                };
            }
            HarnessEvent::Error(error) => {
                if !self.is_responding() {
                    return;
                }

                self.active_assistant_message = None;
                self.runtime_status = RuntimeStatus::Error(error);
            }
            HarnessEvent::Usage(usage) => {
                if !self.is_responding() {
                    return;
                }

                self.usage = Some(usage);
            }
            HarnessEvent::CatalogLoading => self.model_picker.catalog_loading(),
            HarnessEvent::CatalogLoaded(models) => self.model_picker.catalog_loaded(models),
            HarnessEvent::CatalogFailed(error) => self.model_picker.catalog_failed(error),
            HarnessEvent::ModelSelected(model_metadata) => {
                self.apply_selected_model(model_metadata);
            }
            HarnessEvent::ModelSelectionFailed(error) => {
                self.model_picker.selection_failed(error);
            }
        }
    }

    fn apply_selected_model(&mut self, model_metadata: ModelMetadata) {
        self.model_picker
            .selection_acknowledged(&model_metadata.model_id);
        self.model_id = model_metadata.model_id;
        self.model_name = model_metadata.display_name;
    }

    pub fn update_conversation_metrics(&mut self, content_lines: usize, viewport_height: usize) {
        self.content_lines = content_lines;
        self.viewport_height = viewport_height;

        if self.auto_follow {
            self.top_wrapped_line_offset = self.maximum_top_wrapped_line_offset();
            return;
        }

        self.top_wrapped_line_offset = self
            .top_wrapped_line_offset
            .min(self.maximum_top_wrapped_line_offset());
    }

    fn maximum_top_wrapped_line_offset(&self) -> usize {
        self.content_lines.saturating_sub(self.viewport_height)
    }

    fn move_cursor_left(&mut self) {
        if self.cursor_byte_offset == 0 {
            return;
        }

        let previous_character = self.input[..self.cursor_byte_offset]
            .chars()
            .next_back()
            .expect("editor cursor must be at the end of a character");
        self.cursor_byte_offset -= previous_character.len_utf8();
    }

    fn move_cursor_right(&mut self) {
        let Some(next_character) = self.input[self.cursor_byte_offset..].chars().next() else {
            return;
        };

        self.cursor_byte_offset += next_character.len_utf8();
    }

    fn delete_previous_character(&mut self) {
        if self.cursor_byte_offset == 0 {
            return;
        }

        let previous_character = self.input[..self.cursor_byte_offset]
            .chars()
            .next_back()
            .expect("editor cursor must be at the end of a character");
        let previous_character_start = self.cursor_byte_offset - previous_character.len_utf8();
        self.input
            .drain(previous_character_start..self.cursor_byte_offset);
        self.cursor_byte_offset = previous_character_start;
    }

    fn delete_next_character(&mut self) {
        let Some(next_character) = self.input[self.cursor_byte_offset..].chars().next() else {
            return;
        };

        let next_character_end = self.cursor_byte_offset + next_character.len_utf8();
        self.input
            .drain(self.cursor_byte_offset..next_character_end);
    }
}

fn is_control_c(key_code: KeyCode) -> bool {
    matches!(key_code, KeyCode::Char('c' | 'C'))
}

fn is_control_p(key_code: KeyCode) -> bool {
    matches!(key_code, KeyCode::Char('p' | 'P'))
}

#[cfg(test)]
mod tests {
    use super::{AppAction, AppState, MessageRole, RuntimeStatus};
    use crate::provider::{CompletionOutcome, ModelLimits, ModelMetadata, Usage};
    use crate::runtime::HarnessEvent;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    fn key_event(key_code: KeyCode) -> KeyEvent {
        KeyEvent::new(key_code, KeyModifiers::NONE)
    }

    fn modified_key_event(key_code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(key_code, modifiers)
    }

    fn assert_user_message(app_state: &AppState, expected_content: &str) {
        assert_eq!(app_state.messages().len(), 1);
        assert_eq!(app_state.messages()[0].role(), MessageRole::User);
        assert_eq!(app_state.messages()[0].content(), expected_content);
    }

    fn model_metadata(model_id: &str, display_name: &str) -> ModelMetadata {
        ModelMetadata {
            model_id: model_id.to_owned(),
            display_name: display_name.to_owned(),
            limits: ModelLimits {
                context_window_tokens: 16_384,
                maximum_output_tokens: Some(2_048),
            },
            prompt_price_usd_per_million_tokens: Some("0.10".to_owned()),
            completion_price_usd_per_million_tokens: Some("0.40".to_owned()),
        }
    }

    #[test]
    fn editor_inserts_and_moves_across_utf8_boundaries() {
        let mut app_state = AppState::new("test-model", "test-model");

        assert_eq!(
            app_state.handle_key(key_event(KeyCode::Char('a'))),
            AppAction::Continue
        );
        assert_eq!(
            app_state.handle_key(key_event(KeyCode::Char('é'))),
            AppAction::Continue
        );
        assert_eq!(
            app_state.handle_key(key_event(KeyCode::Char('界'))),
            AppAction::Continue
        );
        assert_eq!(app_state.input(), "aé界");
        assert_eq!(app_state.cursor_byte_offset(), "aé界".len());

        app_state.handle_key(key_event(KeyCode::Left));
        app_state.handle_key(key_event(KeyCode::Backspace));
        assert_eq!(app_state.input(), "a界");
        assert_eq!(app_state.cursor_byte_offset(), "a".len());

        app_state.handle_key(key_event(KeyCode::Delete));
        assert_eq!(app_state.input(), "a");
        assert_eq!(app_state.cursor_byte_offset(), "a".len());

        app_state.handle_key(key_event(KeyCode::Home));
        app_state.handle_key(key_event(KeyCode::Right));
        app_state.handle_key(key_event(KeyCode::Char('中')));
        app_state.handle_key(key_event(KeyCode::End));
        assert_eq!(app_state.input(), "a中");
        assert_eq!(app_state.cursor_byte_offset(), "a中".len());
    }

    #[test]
    fn printable_q_and_modified_character_rules_are_explicit() {
        let mut app_state = AppState::new("test-model", "test-model");

        assert_eq!(
            app_state.handle_key(key_event(KeyCode::Char('q'))),
            AppAction::Continue
        );
        app_state.handle_key(modified_key_event(
            KeyCode::Char('x'),
            KeyModifiers::CONTROL,
        ));
        app_state.handle_key(modified_key_event(KeyCode::Char('y'), KeyModifiers::ALT));
        app_state.handle_key(modified_key_event(KeyCode::Char('Z'), KeyModifiers::SHIFT));

        assert_eq!(app_state.input(), "qZ");
    }

    #[test]
    fn enter_ignores_empty_input_and_preserves_draft_while_responding() {
        let mut app_state = AppState::new("test-model", "test-model");

        assert_eq!(
            app_state.handle_key(key_event(KeyCode::Enter)),
            AppAction::Continue
        );

        app_state.handle_key(key_event(KeyCode::Char('d')));
        assert_eq!(
            app_state.handle_key(key_event(KeyCode::Enter)),
            AppAction::Submit("d".to_owned())
        );
        app_state.accept_submission("d".to_owned());

        app_state.handle_key(key_event(KeyCode::Char('r')));
        assert_eq!(
            app_state.handle_key(key_event(KeyCode::Enter)),
            AppAction::Continue
        );
        assert_eq!(app_state.input(), "r");
        assert!(app_state.is_responding());
    }

    #[test]
    fn esc_clears_non_empty_draft_then_exits_when_empty() {
        let mut app_state = AppState::new("test-model", "test-model");
        app_state.handle_key(key_event(KeyCode::Char('x')));

        assert_eq!(
            app_state.handle_key(key_event(KeyCode::Esc)),
            AppAction::Continue
        );
        assert_eq!(app_state.input(), "");
        assert_eq!(app_state.cursor_byte_offset(), 0);
        assert_eq!(
            app_state.handle_key(key_event(KeyCode::Esc)),
            AppAction::Exit
        );
    }

    #[test]
    fn ctrl_c_exits_and_release_events_do_not_mutate_state() {
        let mut app_state = AppState::new("test-model", "test-model");
        let mut release_event = key_event(KeyCode::Char('x'));
        release_event.kind = KeyEventKind::Release;

        assert_eq!(app_state.handle_key(release_event), AppAction::Continue);
        assert_eq!(app_state.input(), "");
        assert_eq!(
            app_state.handle_key(modified_key_event(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL,
            )),
            AppAction::Exit
        );
    }

    #[test]
    fn accepted_submission_commits_all_state_together() {
        let mut app_state = AppState::new("test-model", "test-model");
        for character in "  hello  ".chars() {
            app_state.handle_key(key_event(KeyCode::Char(character)));
        }
        app_state.handle_key(key_event(KeyCode::Left));
        let cursor_before_submission = app_state.cursor_byte_offset();
        let submitted_text = match app_state.handle_key(key_event(KeyCode::Enter)) {
            AppAction::Submit(submitted_text) => submitted_text,
            action => panic!("expected submission action, got {action:?}"),
        };

        assert_eq!(app_state.input(), "  hello  ");
        assert_eq!(app_state.cursor_byte_offset(), cursor_before_submission);
        assert!(app_state.messages().is_empty());

        app_state.accept_submission(submitted_text);

        assert_user_message(&app_state, "  hello  ");
        assert_eq!(app_state.input(), "");
        assert_eq!(app_state.cursor_byte_offset(), 0);
        assert_eq!(app_state.runtime_status(), &RuntimeStatus::Responding);
        assert_eq!(app_state.usage(), None);
    }

    #[test]
    fn rejected_submission_preserves_draft_cursor_and_messages() {
        let mut app_state = AppState::new("test-model", "test-model");
        app_state.handle_key(key_event(KeyCode::Char('a')));
        app_state.handle_key(key_event(KeyCode::Char('é')));
        app_state.handle_key(key_event(KeyCode::Left));
        let input_before_rejection = app_state.input().to_owned();
        let cursor_before_rejection = app_state.cursor_byte_offset();

        app_state.reject_submission("queue is full".to_owned());

        assert_eq!(app_state.input(), input_before_rejection);
        assert_eq!(app_state.cursor_byte_offset(), cursor_before_rejection);
        assert!(app_state.messages().is_empty());
        assert_eq!(
            app_state.runtime_status(),
            &RuntimeStatus::Error("queue is full".to_owned())
        );
        assert!(!app_state.is_responding());
        assert_eq!(
            app_state.handle_key(key_event(KeyCode::Enter)),
            AppAction::Submit("aé".to_owned())
        );
    }

    #[test]
    fn complete_runtime_events_append_one_ordered_assistant_message() {
        let mut app_state = AppState::new("test-model", "test-model");
        app_state.accept_submission("request".to_owned());
        app_state.handle_harness_event(HarnessEvent::ResponseStarted);
        app_state.handle_harness_event(HarnessEvent::AssistantDelta("first ".to_owned()));
        app_state.handle_harness_event(HarnessEvent::AssistantDelta("second".to_owned()));

        assert!(app_state.is_responding());
        assert_eq!(app_state.messages().len(), 2);
        assert_eq!(app_state.messages()[1].role(), MessageRole::Assistant);
        assert_eq!(app_state.messages()[1].content(), "first second");

        app_state.handle_harness_event(HarnessEvent::ResponseFinished(CompletionOutcome::Complete));

        assert_eq!(app_state.runtime_status(), &RuntimeStatus::Idle);
        assert!(!app_state.is_responding());
        assert!(app_state.active_assistant_message().is_none());
    }

    #[test]
    fn usage_is_recorded_for_the_current_turn_and_cleared_by_the_next_accept() {
        let first_usage = Usage {
            input_tokens: 42,
            cached_input_tokens: 17,
            output_tokens: 99,
        };
        let second_usage = Usage {
            input_tokens: 8,
            cached_input_tokens: 3,
            output_tokens: 5,
        };
        let mut app_state = AppState::new("test-model", "test-model");

        app_state.accept_submission("first request".to_owned());
        assert_eq!(app_state.usage(), None);
        app_state.handle_harness_event(HarnessEvent::Usage(first_usage));
        assert_eq!(app_state.usage(), Some(first_usage));
        app_state.handle_harness_event(HarnessEvent::ResponseFinished(CompletionOutcome::Complete));
        assert_eq!(app_state.usage(), Some(first_usage));

        app_state.accept_submission("second request".to_owned());
        assert_eq!(app_state.runtime_status(), &RuntimeStatus::Responding);
        assert_eq!(app_state.usage(), None);

        app_state.handle_harness_event(HarnessEvent::Usage(second_usage));
        assert_eq!(app_state.usage(), Some(second_usage));
    }

    #[test]
    fn rejected_submission_preserves_previous_turn_usage() {
        let usage = Usage {
            input_tokens: 12,
            cached_input_tokens: 4,
            output_tokens: 8,
        };
        let mut app_state = AppState::new("test-model", "test-model");
        app_state.accept_submission("request".to_owned());
        app_state.handle_harness_event(HarnessEvent::Usage(usage));
        app_state.handle_harness_event(HarnessEvent::ResponseFinished(CompletionOutcome::Complete));

        app_state.reject_submission("queue is full".to_owned());

        assert_eq!(
            app_state.runtime_status(),
            &RuntimeStatus::Error("queue is full".to_owned())
        );
        assert_eq!(app_state.usage(), Some(usage));
    }

    #[test]
    fn length_limited_completion_preserves_partial_text_and_allows_retry() {
        let usage = Usage {
            input_tokens: 21,
            cached_input_tokens: 13,
            output_tokens: 144,
        };
        let mut app_state = AppState::new("test-model", "test-model");
        app_state.accept_submission("first request".to_owned());
        app_state.handle_harness_event(HarnessEvent::ResponseStarted);
        app_state.handle_harness_event(HarnessEvent::AssistantDelta("partial response".to_owned()));
        app_state.handle_harness_event(HarnessEvent::Usage(usage));
        app_state.handle_harness_event(HarnessEvent::ResponseFinished(
            CompletionOutcome::LengthLimited,
        ));

        assert_eq!(app_state.messages()[1].content(), "partial response");
        assert_eq!(app_state.runtime_status(), &RuntimeStatus::Truncated);
        assert!(!app_state.is_responding());
        assert!(app_state.active_assistant_message().is_none());
        assert_eq!(app_state.usage(), Some(usage));

        app_state.accept_submission("retry request".to_owned());

        assert_eq!(app_state.runtime_status(), &RuntimeStatus::Responding);
        assert_eq!(app_state.usage(), None);
        assert_eq!(app_state.messages()[1].content(), "partial response");
    }

    #[test]
    fn no_text_response_finishes_without_an_assistant_message() {
        let mut app_state = AppState::new("test-model", "test-model");
        app_state.accept_submission("request".to_owned());

        app_state.handle_harness_event(HarnessEvent::ResponseFinished(CompletionOutcome::Complete));

        assert_eq!(app_state.runtime_status(), &RuntimeStatus::Idle);
        assert!(!app_state.is_responding());
        assert_user_message(&app_state, "request");
        assert!(app_state.active_assistant_message().is_none());
    }

    #[test]
    fn runtime_error_keeps_partial_response_and_allows_recovery() {
        let mut app_state = AppState::new("test-model", "test-model");
        app_state.accept_submission("first request".to_owned());
        app_state.handle_harness_event(HarnessEvent::ResponseStarted);
        app_state.handle_harness_event(HarnessEvent::AssistantDelta("partial".to_owned()));
        let usage = Usage {
            input_tokens: 10,
            cached_input_tokens: 6,
            output_tokens: 4,
        };
        app_state.handle_harness_event(HarnessEvent::Usage(usage));
        app_state.handle_harness_event(HarnessEvent::Error("runtime failed".to_owned()));

        assert_eq!(app_state.messages()[1].content(), "partial");
        assert_eq!(
            app_state.runtime_status(),
            &RuntimeStatus::Error("runtime failed".to_owned())
        );
        assert!(!app_state.is_responding());
        assert_eq!(app_state.usage(), Some(usage));

        app_state.handle_key(key_event(KeyCode::Char('n')));
        let next_submission = match app_state.handle_key(key_event(KeyCode::Enter)) {
            AppAction::Submit(next_submission) => next_submission,
            action => panic!("expected recovery submission action, got {action:?}"),
        };
        app_state.accept_submission(next_submission);

        assert_eq!(app_state.runtime_status(), &RuntimeStatus::Responding);
        assert_eq!(app_state.usage(), None);
        assert_eq!(app_state.messages()[0].content(), "first request");
        assert_eq!(app_state.messages()[2].content(), "n");
    }

    #[test]
    fn runtime_error_before_response_started_unlocks_submission() {
        let mut app_state = AppState::new("test-model", "test-model");
        app_state.accept_submission("first request".to_owned());

        app_state.handle_harness_event(HarnessEvent::Error("runtime failed".to_owned()));

        assert_eq!(
            app_state.runtime_status(),
            &RuntimeStatus::Error("runtime failed".to_owned())
        );
        assert!(!app_state.is_responding());
        assert_eq!(app_state.messages().len(), 1);
        assert_eq!(app_state.messages()[0].role(), MessageRole::User);
        assert_eq!(app_state.messages()[0].content(), "first request");

        app_state.accept_submission("second request".to_owned());

        assert_eq!(app_state.runtime_status(), &RuntimeStatus::Responding);
        assert_eq!(app_state.messages().len(), 2);
        assert_eq!(app_state.messages()[1].role(), MessageRole::User);
        assert_eq!(app_state.messages()[1].content(), "second request");
    }

    #[test]
    fn stale_runtime_events_do_not_mutate_state() {
        let mut app_state = AppState::new("test-model", "test-model");

        let stale_usage = Usage {
            input_tokens: 1,
            cached_input_tokens: 1,
            output_tokens: 1,
        };
        app_state.handle_harness_event(HarnessEvent::Usage(stale_usage));
        app_state.handle_harness_event(HarnessEvent::AssistantDelta("stale".to_owned()));
        app_state.handle_harness_event(HarnessEvent::ResponseFinished(CompletionOutcome::Complete));
        app_state.handle_harness_event(HarnessEvent::Error("stale error".to_owned()));
        assert!(app_state.messages().is_empty());
        assert_eq!(app_state.runtime_status(), &RuntimeStatus::Idle);
        assert_eq!(app_state.usage(), None);

        app_state.accept_submission("request".to_owned());
        app_state.handle_harness_event(HarnessEvent::ResponseStarted);
        let current_usage = Usage {
            input_tokens: 9,
            cached_input_tokens: 5,
            output_tokens: 7,
        };
        app_state.handle_harness_event(HarnessEvent::Usage(current_usage));
        app_state.handle_harness_event(HarnessEvent::ResponseFinished(CompletionOutcome::Complete));
        let messages_after_finish = app_state.messages().to_owned();

        app_state.handle_harness_event(HarnessEvent::Usage(stale_usage));
        app_state.handle_harness_event(HarnessEvent::AssistantDelta("stale".to_owned()));
        app_state.handle_harness_event(HarnessEvent::ResponseStarted);
        app_state.handle_harness_event(HarnessEvent::ResponseFinished(
            CompletionOutcome::LengthLimited,
        ));
        app_state.handle_harness_event(HarnessEvent::Error("stale error".to_owned()));

        assert_eq!(app_state.messages(), messages_after_finish.as_slice());
        assert_eq!(app_state.runtime_status(), &RuntimeStatus::Idle);
        assert_eq!(app_state.usage(), Some(current_usage));
    }

    #[test]
    fn scroll_metrics_follow_bottom_and_manual_navigation_is_stable() {
        let mut app_state = AppState::new("test-model", "test-model");
        app_state.update_conversation_metrics(20, 5);
        assert_eq!(app_state.top_wrapped_line_offset(), 15);
        assert!(app_state.is_auto_following());

        app_state.handle_key(key_event(KeyCode::PageUp));
        assert_eq!(app_state.top_wrapped_line_offset(), 10);
        assert!(!app_state.is_auto_following());

        app_state.update_conversation_metrics(30, 5);
        assert_eq!(app_state.top_wrapped_line_offset(), 10);
        app_state.handle_key(key_event(KeyCode::PageDown));
        assert_eq!(app_state.top_wrapped_line_offset(), 15);
        assert!(!app_state.is_auto_following());

        app_state.update_conversation_metrics(8, 5);
        assert_eq!(app_state.top_wrapped_line_offset(), 3);
        assert!(!app_state.is_auto_following());

        app_state.handle_key(key_event(KeyCode::PageDown));
        assert_eq!(app_state.top_wrapped_line_offset(), 3);
        assert!(!app_state.is_auto_following());

        app_state.update_conversation_metrics(40, 5);
        assert_eq!(app_state.top_wrapped_line_offset(), 3);
        app_state.handle_key(modified_key_event(KeyCode::End, KeyModifiers::CONTROL));
        assert_eq!(app_state.top_wrapped_line_offset(), 35);
        assert!(app_state.is_auto_following());
    }

    #[test]
    fn scroll_metrics_clamp_without_underflow_for_zero_sized_values() {
        let mut app_state = AppState::new("test-model", "test-model");
        app_state.update_conversation_metrics(0, 0);
        assert_eq!(app_state.top_wrapped_line_offset(), 0);

        app_state.handle_key(key_event(KeyCode::PageDown));
        assert_eq!(app_state.top_wrapped_line_offset(), 0);
        assert!(!app_state.is_auto_following());

        app_state.update_conversation_metrics(4, 0);
        assert_eq!(app_state.top_wrapped_line_offset(), 0);

        app_state.handle_key(key_event(KeyCode::PageDown));
        assert_eq!(app_state.top_wrapped_line_offset(), 0);
        app_state.handle_key(modified_key_event(KeyCode::End, KeyModifiers::CONTROL));
        assert_eq!(app_state.top_wrapped_line_offset(), 4);

        app_state.update_conversation_metrics(0, 4);
        assert_eq!(app_state.top_wrapped_line_offset(), 0);
        assert!(app_state.is_auto_following());
    }

    #[test]
    fn idle_ctrl_p_owns_modal_keys_without_mutating_draft_conversation_or_usage() {
        let mut app_state = AppState::new("current-id", "Current model");
        app_state.accept_submission("existing request".to_owned());
        app_state.handle_harness_event(HarnessEvent::ResponseStarted);
        app_state
            .handle_harness_event(HarnessEvent::AssistantDelta("existing response".to_owned()));
        let usage = Usage {
            input_tokens: 12,
            cached_input_tokens: 4,
            output_tokens: 8,
        };
        app_state.handle_harness_event(HarnessEvent::Usage(usage));
        app_state.handle_harness_event(HarnessEvent::ResponseFinished(CompletionOutcome::Complete));
        app_state.handle_key(key_event(KeyCode::Char('d')));
        let messages_before_picker = app_state.messages().to_owned();

        assert_eq!(
            app_state.handle_key(modified_key_event(
                KeyCode::Char('p'),
                KeyModifiers::CONTROL,
            )),
            AppAction::OpenModelPicker
        );
        assert!(app_state.model_picker().is_open());
        assert_eq!(app_state.input(), "d");
        assert_eq!(app_state.messages(), messages_before_picker.as_slice());
        assert_eq!(app_state.usage(), Some(usage));

        app_state.handle_key(key_event(KeyCode::Char('q')));
        assert_eq!(app_state.model_picker().query(), "q");
        assert_eq!(app_state.input(), "d");
        assert_eq!(
            app_state.handle_key(key_event(KeyCode::Esc)),
            AppAction::Continue
        );
        assert!(!app_state.model_picker().is_open());
        assert_eq!(app_state.input(), "d");
    }

    #[test]
    fn responding_ctrl_p_is_ignored_but_ctrl_c_remains_global() {
        let mut app_state = AppState::new("current-id", "Current model");
        app_state.accept_submission("request".to_owned());
        app_state.handle_key(key_event(KeyCode::Char('d')));

        assert_eq!(
            app_state.handle_key(modified_key_event(
                KeyCode::Char('P'),
                KeyModifiers::CONTROL,
            )),
            AppAction::Continue
        );
        assert!(!app_state.model_picker().is_open());
        assert_eq!(app_state.input(), "d");
        assert_eq!(
            app_state.handle_key(modified_key_event(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL,
            )),
            AppAction::Exit
        );
    }

    #[test]
    fn catalog_and_startup_selection_events_update_projection_without_closing_open_picker() {
        let current_model = model_metadata("current-id", "Initial name");
        let alternate_model = model_metadata("alternate-id", "Alternate name");
        let mut app_state = AppState::new("current-id", "Initial name");
        app_state.handle_harness_event(HarnessEvent::CatalogLoading);
        app_state.handle_key(modified_key_event(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL,
        ));
        app_state.handle_key(key_event(KeyCode::Char('a')));
        app_state.handle_harness_event(HarnessEvent::CatalogLoaded(vec![
            current_model.clone(),
            alternate_model.clone(),
        ]));
        assert_eq!(app_state.model_picker().query(), "a");
        app_state.handle_harness_event(HarnessEvent::ModelSelected(current_model.clone()));

        assert_eq!(app_state.model_id(), "current-id");
        assert_eq!(app_state.model_name(), "Initial name");
        assert!(app_state.model_picker().is_open());
        assert_eq!(app_state.model_picker().pending_model_id(), None);

        app_state.handle_harness_event(HarnessEvent::CatalogFailed("provider\nfailed".to_owned()));
        assert!(matches!(
            app_state.model_picker().catalog_state(),
            crate::tui::model_picker::CatalogState::Failed(message) if message == "provider\nfailed"
        ));
        assert!(app_state.model_picker().is_open());
    }

    #[test]
    fn model_selection_changes_only_after_matching_runtime_acknowledgment() {
        let alternate_model = model_metadata("alternate-id", "Alternate name");
        let mut app_state = AppState::new("current-id", "Initial name");
        app_state.handle_key(modified_key_event(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL,
        ));
        app_state.handle_harness_event(HarnessEvent::CatalogLoaded(vec![alternate_model.clone()]));
        app_state.accept_model_selection("alternate-id".to_owned());

        assert_eq!(app_state.model_id(), "current-id");
        assert_eq!(app_state.model_name(), "Initial name");
        assert!(app_state.model_picker().is_open());

        app_state.handle_harness_event(HarnessEvent::ModelSelectionFailed(
            "model is no longer available".to_owned(),
        ));
        assert!(app_state.model_picker().is_open());
        assert_eq!(app_state.model_picker().pending_model_id(), None);
        assert_eq!(app_state.model_id(), "current-id");
        assert_eq!(app_state.model_name(), "Initial name");
        assert_eq!(
            app_state.model_picker().error(),
            Some("model is no longer available")
        );

        app_state.accept_model_selection("alternate-id".to_owned());
        app_state.handle_harness_event(HarnessEvent::ModelSelected(alternate_model));
        assert_eq!(app_state.model_id(), "alternate-id");
        assert_eq!(app_state.model_name(), "Alternate name");
        assert!(!app_state.model_picker().is_open());
    }
}
