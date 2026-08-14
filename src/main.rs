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

use runtime::{HarnessEvent, MOCK_MODEL_NAME, RuntimeCommand};
use tui::{AppAction, AppState};

const LOOP_TICK: Duration = Duration::from_millis(16);

fn main() -> Result<()> {
    color_eyre::install()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .context("failed to build current-thread Tokio runtime")?;

    ratatui::run(|terminal| runtime.block_on(run(terminal))).context("failed to run app")
}

async fn run(terminal: &mut DefaultTerminal) -> Result<()> {
    let (command_sender, mut event_receiver, task_handle) = runtime::spawn_mock_runtime();
    let mut app_state = AppState::new(MOCK_MODEL_NAME);

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
                            app_state.handle_harness_event(HarnessEvent::Error(
                                "runtime event channel closed".to_owned(),
                            ));
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
        Err(join_error) => Err(eyre!("unexpected task join error: {join_error}"))
            .context("mock runtime task failed"),
    }
}

#[cfg(test)]
mod main_tests {
    use super::dispatch_action;
    use crate::runtime::{HarnessEvent, RuntimeCommand};
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

    fn settled_app_state() -> AppState {
        let mut app_state = AppState::new("mock-runtime");
        app_state.accept_submission("existing request".to_owned());
        app_state.handle_harness_event(HarnessEvent::ResponseStarted);
        app_state
            .handle_harness_event(HarnessEvent::AssistantDelta("existing response".to_owned()));
        app_state.handle_harness_event(HarnessEvent::ResponseFinished);
        app_state
    }

    #[test]
    fn successful_enqueue_commits_submission_once() {
        let (command_sender, mut command_receiver) = mpsc::channel(1);
        let mut app_state = AppState::new("mock-runtime");
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
        assert_eq!(app_state.messages().len(), 1);
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
        let mut app_state = AppState::new("mock-runtime");
        app_state.accept_submission("first request".to_owned());
        type_input(&mut app_state, "second request");

        assert!(dispatch_action(
            &mut app_state,
            &command_sender,
            key_event(KeyCode::Enter),
        ));

        assert_eq!(app_state.input(), "second request");
        assert_eq!(app_state.messages().len(), 1);
        assert!(app_state.is_responding());
        assert!(command_receiver.try_recv().is_err());
    }
}
