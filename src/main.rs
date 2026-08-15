use std::sync::Arc;
use std::time::Duration;

mod prompt;
mod provider;
mod runtime;
mod session;
mod tools;
mod tui;

use color_eyre::Result;
use color_eyre::eyre::{Context, eyre};
use crossterm::event::{self, Event, KeyEvent};
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use provider::{ModelMetadata, OPENROUTER_DEFAULT_MODEL_ID, OpenRouterProvider, Provider};
use runtime::{HarnessEvent, RuntimeCommand};
use tui::{AppAction, AppState};

const LOOP_TICK: Duration = Duration::from_millis(16);

fn main() -> Result<()> {
    color_eyre::install()?;
    let (provider, model_metadata) = initialize_provider()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .context("failed to build current-thread Tokio runtime")?;

    ratatui::run(|terminal| {
        runtime.block_on(run(
            terminal,
            provider,
            model_metadata.model_id,
            model_metadata.display_name,
        ))
    })
    .context("failed to run app")
}

fn initialize_provider() -> Result<(Arc<dyn Provider>, ModelMetadata)> {
    let provider: Arc<dyn Provider> =
        Arc::new(OpenRouterProvider::new().context("failed to construct OpenRouter provider")?);
    let model_metadata = provider
        .model_metadata(OPENROUTER_DEFAULT_MODEL_ID)
        .context("failed to resolve OpenRouter default model metadata")?;

    Ok((provider, model_metadata))
}

async fn run(
    terminal: &mut DefaultTerminal,
    provider: Arc<dyn Provider>,
    model_id: String,
    model_display_name: String,
) -> Result<()> {
    let (command_sender, mut event_receiver, task_handle) =
        runtime::spawn_runtime(provider, model_id.clone());
    let mut app_state = AppState::new(model_id, model_display_name);

    let loop_result = async {
        draw(terminal, &mut app_state)?;
        run_application_loop(
            terminal,
            &mut app_state,
            &command_sender,
            &mut event_receiver,
        )
        .await
    }
    .await;

    drop(event_receiver);
    let shutdown_result = shutdown_runtime(command_sender, task_handle).await;

    loop_result?;
    shutdown_result
}

async fn run_application_loop(
    terminal: &mut DefaultTerminal,
    app_state: &mut AppState,
    command_sender: &mpsc::Sender<RuntimeCommand>,
    event_receiver: &mut mpsc::Receiver<HarnessEvent>,
) -> Result<()> {
    let mut event_receiver_open = true;
    let mut loop_tick = tokio::time::interval(LOOP_TICK);
    loop_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        let should_continue = tokio::select! {
            harness_event = event_receiver.recv(), if event_receiver_open => {
                match harness_event {
                    Some(harness_event) => {
                        app_state.handle_harness_event(harness_event);
                        draw(terminal, app_state)?;
                    }
                    None => {
                        event_receiver_open = false;
                        if app_state.is_responding() {
                            app_state.runtime_channel_closed();
                            draw(terminal, app_state)?;
                        }
                    }
                }
                Ok(true)
            }
            _ = loop_tick.tick() => drain_terminal_events(terminal, app_state, command_sender),
        }?;

        if !should_continue {
            break;
        }
    }

    Ok(())
}

fn drain_terminal_events(
    terminal: &mut DefaultTerminal,
    app_state: &mut AppState,
    command_sender: &mpsc::Sender<RuntimeCommand>,
) -> Result<bool> {
    let mut needs_redraw = false;

    while event::poll(Duration::ZERO).context("terminal event poll failed")? {
        let terminal_event = event::read().context("terminal event read failed")?;
        match terminal_event {
            Event::Key(key_event) => {
                needs_redraw = true;
                if !dispatch_action(app_state, command_sender, key_event) {
                    return Ok(false);
                }
            }
            Event::Resize(_, _) => needs_redraw = true,
            _ => {}
        }
    }

    if needs_redraw {
        draw(terminal, app_state)?;
    }

    Ok(true)
}

fn dispatch_action(
    app_state: &mut AppState,
    command_sender: &mpsc::Sender<RuntimeCommand>,
    key_event: KeyEvent,
) -> bool {
    match app_state.handle_key(key_event) {
        AppAction::Continue => true,
        AppAction::Exit => false,
        AppAction::Submit(submission) => {
            match command_sender.try_send(RuntimeCommand::Submit(submission.clone())) {
                Ok(()) => app_state.accept_submission(submission),
                Err(mpsc::error::TrySendError::Full(_)) => {
                    app_state.reject_submission("runtime command queue is full".to_owned())
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    app_state.reject_submission("runtime command channel is closed".to_owned())
                }
            }
            true
        }
        AppAction::OpenModelPicker => {
            match command_sender.try_send(RuntimeCommand::DiscoverModels) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    app_state.reject_model_picker_open("runtime command queue is full".to_owned())
                }
                Err(mpsc::error::TrySendError::Closed(_)) => app_state
                    .reject_model_picker_open("runtime command channel is closed".to_owned()),
            }
            true
        }
        AppAction::SelectModel(model_id) => {
            match command_sender.try_send(RuntimeCommand::SelectModel(model_id.clone())) {
                Ok(()) => app_state.accept_model_selection(model_id),
                Err(mpsc::error::TrySendError::Full(_)) => {
                    app_state.reject_model_selection("runtime command queue is full".to_owned())
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    app_state.reject_model_selection("runtime command channel is closed".to_owned())
                }
            }
            true
        }
    }
}

fn draw(terminal: &mut DefaultTerminal, app_state: &mut AppState) -> Result<()> {
    terminal
        .draw(|frame| tui::render(frame, app_state))
        .context("terminal draw failed")?;
    Ok(())
}

async fn shutdown_runtime(
    command_sender: mpsc::Sender<RuntimeCommand>,
    task_handle: JoinHandle<()>,
) -> Result<()> {
    drop(command_sender);
    task_handle.abort();

    match task_handle.await {
        Ok(()) => Ok(()),
        Err(join_error) if join_error.is_cancelled() => Ok(()),
        Err(join_error) => {
            Err(eyre!("unexpected task join error: {join_error}")).context("runtime task failed")
        }
    }
}

#[cfg(test)]
mod main_tests {
    use super::{dispatch_action, initialize_provider};
    use crate::provider::{
        CompletionOutcome, ModelLimits, ModelMetadata, OPENROUTER_DEFAULT_MODEL_ID,
    };
    use crate::runtime::{HarnessEvent, RuntimeCommand};
    use crate::session::{MessageId, RunId};
    use crate::tui::AppState;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tokio::sync::mpsc;

    fn key_event(key_code: KeyCode) -> KeyEvent {
        KeyEvent::new(key_code, KeyModifiers::NONE)
    }

    fn type_input(app_state: &mut AppState, input: &str) {
        for character in input.chars() {
            app_state.handle_key(key_event(KeyCode::Char(character)));
        }
    }

    fn run_id(value: u64) -> RunId {
        RunId::from_u64(value)
    }

    fn message_id(value: u64) -> MessageId {
        MessageId::from_u64(value)
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

    fn settled_app_state() -> AppState {
        let mut app_state = AppState::new("mock-runtime", "mock-runtime");
        app_state.accept_submission("existing request".to_owned());
        app_state.handle_harness_event(HarnessEvent::RunStarted {
            run_id: run_id(1),
            user_message_id: message_id(1),
            content: "existing request".to_owned(),
        });
        app_state.handle_harness_event(HarnessEvent::AssistantStarted {
            run_id: run_id(1),
            assistant_message_id: message_id(2),
        });
        app_state.handle_harness_event(HarnessEvent::AssistantDelta {
            run_id: run_id(1),
            assistant_message_id: message_id(2),
            text: "existing response".to_owned(),
        });
        app_state.handle_harness_event(HarnessEvent::RunFinished {
            run_id: run_id(1),
            completion_outcome: CompletionOutcome::Complete,
        });
        app_state
    }

    #[test]
    fn startup_initializes_the_pinned_openrouter_default_without_credentials_or_network() {
        let (_provider, metadata) =
            initialize_provider().expect("OpenRouter startup should not require credentials");

        assert_eq!(
            OPENROUTER_DEFAULT_MODEL_ID,
            "deepseek/deepseek-v4-flash-0731"
        );
        assert_eq!(metadata.model_id, OPENROUTER_DEFAULT_MODEL_ID);
        assert_eq!(metadata.display_name, "DeepSeek: DeepSeek V4 Flash 0731");
        assert_eq!(metadata.limits.context_window_tokens, 1_048_576);
        assert_eq!(metadata.limits.maximum_output_tokens, Some(393_216));
        assert_eq!(metadata.prompt_price_usd_per_million_tokens, None);
        assert_eq!(metadata.completion_price_usd_per_million_tokens, None);
    }

    #[test]
    fn successful_enqueue_commits_submission_once() {
        let (command_sender, mut command_receiver) = mpsc::channel(1);
        let mut app_state = AppState::new("mock-runtime", "mock-runtime");
        type_input(&mut app_state, "hello");

        assert!(dispatch_action(
            &mut app_state,
            &command_sender,
            key_event(KeyCode::Enter),
        ));

        assert_eq!(
            command_receiver.try_recv(),
            Ok(RuntimeCommand::Submit("hello".to_owned()))
        );
        assert!(app_state.messages().is_empty());
        assert_eq!(app_state.input(), "");
        assert!(app_state.is_responding());
        assert!(command_receiver.try_recv().is_err());
    }

    #[test]
    fn full_enqueue_preserves_state_and_allows_retry() {
        let (command_sender, mut command_receiver) = mpsc::channel(1);
        command_sender
            .try_send(RuntimeCommand::Submit("already queued".to_owned()))
            .expect("the test command should fill the bounded channel");

        let mut app_state = settled_app_state();
        type_input(&mut app_state, "retry");
        let messages_before_rejection = app_state.messages().to_owned();

        assert!(dispatch_action(
            &mut app_state,
            &command_sender,
            key_event(KeyCode::Enter),
        ));

        assert_eq!(app_state.messages(), messages_before_rejection.as_slice());
        assert_eq!(app_state.input(), "retry");
        assert!(matches!(
            app_state.runtime_status(),
            crate::tui::app::RuntimeStatus::Error(error) if error.contains("full")
        ));

        assert_eq!(
            command_receiver.try_recv(),
            Ok(RuntimeCommand::Submit("already queued".to_owned()))
        );
        assert!(dispatch_action(
            &mut app_state,
            &command_sender,
            key_event(KeyCode::Enter),
        ));
        assert_eq!(
            command_receiver.try_recv(),
            Ok(RuntimeCommand::Submit("retry".to_owned()))
        );
        assert_eq!(app_state.input(), "");
        assert!(app_state.is_responding());
    }

    #[test]
    fn closed_enqueue_preserves_state_and_exit_remains_available() {
        let (command_sender, command_receiver) = mpsc::channel(1);
        drop(command_receiver);

        let mut app_state = settled_app_state();
        type_input(&mut app_state, "draft");
        let messages_before_rejection = app_state.messages().to_owned();

        assert!(dispatch_action(
            &mut app_state,
            &command_sender,
            key_event(KeyCode::Enter),
        ));
        assert_eq!(app_state.messages(), messages_before_rejection.as_slice());
        assert_eq!(app_state.input(), "draft");
        assert!(matches!(
            app_state.runtime_status(),
            crate::tui::app::RuntimeStatus::Error(error) if error.contains("closed")
        ));

        assert!(dispatch_action(
            &mut app_state,
            &command_sender,
            key_event(KeyCode::Enter),
        ));
        assert_eq!(app_state.input(), "draft");
        assert!(!dispatch_action(
            &mut app_state,
            &command_sender,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        ));
    }

    #[test]
    fn responding_enter_does_not_enqueue_or_clear_next_draft() {
        let (command_sender, mut command_receiver) = mpsc::channel(1);
        let mut app_state = AppState::new("mock-runtime", "mock-runtime");
        app_state.accept_submission("first request".to_owned());
        type_input(&mut app_state, "second request");

        assert!(dispatch_action(
            &mut app_state,
            &command_sender,
            key_event(KeyCode::Enter),
        ));

        assert_eq!(app_state.input(), "second request");
        assert!(app_state.messages().is_empty());
        assert!(app_state.is_responding());
        assert!(command_receiver.try_recv().is_err());
    }

    #[test]
    fn picker_actions_enqueue_runtime_commands_and_wait_for_selection_acknowledgment() {
        let (command_sender, mut command_receiver) = mpsc::channel(4);
        let alternate_model = model_metadata("alternate-id", "Alternate model");
        let mut app_state = AppState::new("current-id", "Current model");
        type_input(&mut app_state, "draft");

        assert!(dispatch_action(
            &mut app_state,
            &command_sender,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        ));
        assert_eq!(
            command_receiver.try_recv(),
            Ok(RuntimeCommand::DiscoverModels)
        );
        app_state.handle_harness_event(HarnessEvent::CatalogLoaded(vec![alternate_model.clone()]));

        assert!(dispatch_action(
            &mut app_state,
            &command_sender,
            key_event(KeyCode::Enter),
        ));
        assert_eq!(
            command_receiver.try_recv(),
            Ok(RuntimeCommand::SelectModel("alternate-id".to_owned()))
        );
        assert_eq!(app_state.model_id(), "current-id");
        assert_eq!(app_state.input(), "draft");
        assert_eq!(
            app_state.model_picker().pending_model_id(),
            Some("alternate-id")
        );
        assert!(app_state.model_picker().is_open());

        app_state.handle_harness_event(HarnessEvent::ModelSelected(alternate_model));
        assert_eq!(app_state.model_id(), "alternate-id");
        assert_eq!(app_state.model_name(), "Alternate model");
        assert_eq!(app_state.input(), "draft");
        assert!(!app_state.model_picker().is_open());
    }

    #[test]
    fn picker_enqueue_rejection_preserves_selection_and_draft() {
        let (command_sender, mut command_receiver) = mpsc::channel(1);
        let alternate_model = model_metadata("alternate-id", "Alternate model");
        let mut app_state = AppState::new("current-id", "Current model");
        type_input(&mut app_state, "draft");
        dispatch_action(
            &mut app_state,
            &command_sender,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        );
        command_receiver
            .try_recv()
            .expect("discovery command should be queued");
        app_state.handle_harness_event(HarnessEvent::CatalogLoaded(vec![alternate_model]));
        command_sender
            .try_send(RuntimeCommand::Submit("already queued".to_owned()))
            .expect("the test command should fill the queue");

        assert!(dispatch_action(
            &mut app_state,
            &command_sender,
            key_event(KeyCode::Enter),
        ));
        assert_eq!(app_state.model_id(), "current-id");
        assert_eq!(app_state.input(), "draft");
        assert!(app_state.model_picker().is_open());
        assert_eq!(app_state.model_picker().pending_model_id(), None);
        assert!(matches!(
            app_state.model_picker().error(),
            Some(error) if error.contains("full")
        ));
    }

    #[test]
    fn picker_open_rejection_is_local_to_the_open_modal() {
        let (command_sender, command_receiver) = mpsc::channel(1);
        drop(command_receiver);
        let mut app_state = AppState::new("current-id", "Current model");
        type_input(&mut app_state, "draft");

        assert!(dispatch_action(
            &mut app_state,
            &command_sender,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        ));
        assert!(app_state.model_picker().is_open());
        assert_eq!(app_state.model_id(), "current-id");
        assert_eq!(app_state.input(), "draft");
        assert!(matches!(
            app_state.model_picker().error(),
            Some(error) if error.contains("closed")
        ));
    }
}
