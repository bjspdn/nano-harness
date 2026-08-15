//! Harness runtime and event flow.

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::provider::{
    CompletionOutcome, ModelEvent, ModelMetadata, Provider, ProviderError, Usage,
};
use crate::session::{FailureCategory, MessageId, RunId, Session};

const COMMAND_CHANNEL_CAPACITY: usize = 4;
const EVENT_CHANNEL_CAPACITY: usize = 8;

/// The commands accepted by the harness runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeCommand {
    DiscoverModels,
    SelectModel(String),
    Submit(String),
}

/// Events emitted by the harness runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessEvent {
    CatalogLoading,
    CatalogLoaded(Vec<ModelMetadata>),
    CatalogFailed(String),
    ModelSelected(ModelMetadata),
    ModelSelectionFailed(String),
    RunStarted {
        run_id: RunId,
        user_message_id: MessageId,
        content: String,
    },
    AssistantStarted {
        run_id: RunId,
        assistant_message_id: MessageId,
    },
    AssistantDelta {
        run_id: RunId,
        assistant_message_id: MessageId,
        text: String,
    },
    Usage {
        run_id: RunId,
        usage: Usage,
    },
    RunFinished {
        run_id: RunId,
        completion_outcome: CompletionOutcome,
    },
    RunFailed {
        run_id: RunId,
        detail: String,
    },
    SubmissionFailed {
        detail: String,
    },
}

#[derive(Debug)]
enum CatalogState {
    Loading,
    Loaded(Vec<ModelMetadata>),
    Failed,
}

struct CatalogTask {
    handle: Option<JoinHandle<Result<Vec<ModelMetadata>, ProviderError>>>,
}

impl Drop for CatalogTask {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.as_ref() {
            handle.abort();
        }
    }
}

/// Start the provider-backed harness runtime.
///
/// The caller owns the returned task handle and can abort and await it during shutdown.
pub fn spawn_runtime(
    provider: Arc<dyn Provider>,
    model_id: String,
) -> (
    mpsc::Sender<RuntimeCommand>,
    mpsc::Receiver<HarnessEvent>,
    JoinHandle<()>,
) {
    let (command_sender, mut command_receiver) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
    let (event_sender, event_receiver) = mpsc::channel(EVENT_CHANNEL_CAPACITY);

    let task_handle = tokio::spawn(async move {
        if !send_harness_event(&event_sender, HarnessEvent::CatalogLoading).await {
            return;
        }

        let mut catalog_state = CatalogState::Loading;
        let mut catalog_task = Some(spawn_catalog_task(Arc::clone(&provider)));
        let mut session = Session::new(model_id);

        loop {
            let loop_outcome = tokio::select! {
                command = command_receiver.recv() => {
                    match command {
                        Some(command) => handle_runtime_command(
                            command,
                            &provider,
                            &mut session,
                            &mut catalog_state,
                            &mut catalog_task,
                            &event_sender,
                        )
                        .await,
                        None => StreamOutcome::Exit,
                    }
                }
                catalog_result = await_catalog_task(&mut catalog_task), if catalog_task.is_some() => {
                    let catalog_result = catalog_result
                        .expect("catalog task should exist while its result is selected");
                    handle_catalog_result(
                        catalog_result,
                        &session,
                        &mut catalog_state,
                        &mut catalog_task,
                        &event_sender,
                    )
                    .await
                }
                _ = event_sender.closed() => StreamOutcome::Exit,
            };

            if loop_outcome == StreamOutcome::Exit {
                break;
            }
        }

        stop_catalog_task(catalog_task).await;
    });

    (command_sender, event_receiver, task_handle)
}

fn spawn_catalog_task(provider: Arc<dyn Provider>) -> CatalogTask {
    CatalogTask {
        handle: Some(tokio::spawn(async move { provider.models().await })),
    }
}

async fn await_catalog_task(
    catalog_task: &mut Option<CatalogTask>,
) -> Option<Result<Result<Vec<ModelMetadata>, ProviderError>, tokio::task::JoinError>> {
    let catalog_task = catalog_task.as_mut()?;
    Some(catalog_task.handle.as_mut()?.await)
}

async fn handle_runtime_command(
    command: RuntimeCommand,
    provider: &Arc<dyn Provider>,
    session: &mut Session,
    catalog_state: &mut CatalogState,
    catalog_task: &mut Option<CatalogTask>,
    event_sender: &mpsc::Sender<HarnessEvent>,
) -> StreamOutcome {
    match command {
        RuntimeCommand::DiscoverModels => {
            discover_models(provider, catalog_state, catalog_task, event_sender).await
        }
        RuntimeCommand::SelectModel(model_id) => {
            select_model(model_id, session, catalog_state, event_sender).await
        }
        RuntimeCommand::Submit(input) => {
            consume_provider_stream(provider.as_ref(), session, input, event_sender).await
        }
    }
}

async fn discover_models(
    provider: &Arc<dyn Provider>,
    catalog_state: &mut CatalogState,
    catalog_task: &mut Option<CatalogTask>,
    event_sender: &mpsc::Sender<HarnessEvent>,
) -> StreamOutcome {
    if !matches!(catalog_state, CatalogState::Failed) {
        return StreamOutcome::Continue;
    }

    if !send_harness_event(event_sender, HarnessEvent::CatalogLoading).await {
        return StreamOutcome::Exit;
    }

    *catalog_state = CatalogState::Loading;
    *catalog_task = Some(spawn_catalog_task(Arc::clone(provider)));
    StreamOutcome::Continue
}

async fn select_model(
    model_id: String,
    session: &mut Session,
    catalog_state: &CatalogState,
    event_sender: &mpsc::Sender<HarnessEvent>,
) -> StreamOutcome {
    let selection = if model_id.is_empty() {
        Err("model ID cannot be empty".to_owned())
    } else {
        match catalog_state {
            CatalogState::Loading => Err("model catalog is still loading".to_owned()),
            CatalogState::Failed => Err(
                "model catalog discovery failed; reopen Ctrl-P to retry model discovery".to_owned(),
            ),
            CatalogState::Loaded(models) => models
                .iter()
                .find(|metadata| metadata.model_id == model_id)
                .cloned()
                .ok_or_else(|| {
                    format!("model '{model_id}' is not available in the loaded catalog")
                }),
        }
    };

    let metadata = match selection {
        Ok(metadata) => metadata,
        Err(error) => {
            if send_harness_event(event_sender, HarnessEvent::ModelSelectionFailed(error)).await {
                return StreamOutcome::Continue;
            }
            return StreamOutcome::Exit;
        }
    };

    session.select_model(metadata.model_id.clone());
    if send_harness_event(event_sender, HarnessEvent::ModelSelected(metadata)).await {
        StreamOutcome::Continue
    } else {
        StreamOutcome::Exit
    }
}

async fn handle_catalog_result(
    catalog_result: Result<Result<Vec<ModelMetadata>, ProviderError>, tokio::task::JoinError>,
    session: &Session,
    catalog_state: &mut CatalogState,
    catalog_task: &mut Option<CatalogTask>,
    event_sender: &mpsc::Sender<HarnessEvent>,
) -> StreamOutcome {
    *catalog_task = None;

    let catalog_result = match catalog_result {
        Ok(catalog_result) => catalog_result,
        Err(error) => {
            *catalog_state = CatalogState::Failed;
            return report_catalog_failure(
                event_sender,
                format!("model catalog task failed: {error}"),
            )
            .await;
        }
    };

    let models = match catalog_result {
        Ok(models) => models,
        Err(error) => {
            *catalog_state = CatalogState::Failed;
            return report_catalog_failure(event_sender, error.to_string()).await;
        }
    };

    let selected_metadata = models
        .iter()
        .find(|metadata| metadata.model_id == session.current_model_id())
        .cloned();
    *catalog_state = CatalogState::Loaded(models.clone());

    if !send_harness_event(event_sender, HarnessEvent::CatalogLoaded(models)).await {
        return StreamOutcome::Exit;
    }

    let Some(selected_metadata) = selected_metadata else {
        return StreamOutcome::Continue;
    };

    if send_harness_event(event_sender, HarnessEvent::ModelSelected(selected_metadata)).await {
        StreamOutcome::Continue
    } else {
        StreamOutcome::Exit
    }
}

async fn report_catalog_failure(
    event_sender: &mpsc::Sender<HarnessEvent>,
    error: String,
) -> StreamOutcome {
    if send_harness_event(event_sender, HarnessEvent::CatalogFailed(error)).await {
        StreamOutcome::Continue
    } else {
        StreamOutcome::Exit
    }
}

async fn stop_catalog_task(catalog_task: Option<CatalogTask>) {
    let Some(mut catalog_task) = catalog_task else {
        return;
    };

    let Some(handle) = catalog_task.handle.take() else {
        return;
    };
    handle.abort();
    let _ = handle.await;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamOutcome {
    Continue,
    Exit,
}

async fn consume_provider_stream(
    provider: &dyn Provider,
    session: &mut Session,
    input: String,
    event_sender: &mpsc::Sender<HarnessEvent>,
) -> StreamOutcome {
    let submitted_content = input.clone();
    let run_start = match session.start_run(input) {
        Ok(run_start) => run_start,
        Err(error) => return report_submission_failure(event_sender, error.to_string()).await,
    };
    let run_id = run_start.run_id();
    if !send_harness_event(
        event_sender,
        HarnessEvent::RunStarted {
            run_id,
            user_message_id: run_start.user_message_id(),
            content: submitted_content,
        },
    )
    .await
    {
        return StreamOutcome::Exit;
    }
    let request = run_start.request().clone();

    let stream_result = tokio::select! {
        stream_result = provider.stream(request) => stream_result,
        _ = event_sender.closed() => return StreamOutcome::Exit,
    };
    let mut model_stream = match stream_result {
        Ok(model_stream) => model_stream,
        Err(error) => {
            return report_run_failure(
                session,
                run_id,
                FailureCategory::RequestSetup,
                error.to_string(),
                event_sender,
            )
            .await;
        }
    };

    loop {
        let model_event = tokio::select! {
            model_event = model_stream.recv() => model_event,
            _ = event_sender.closed() => return StreamOutcome::Exit,
        };

        let Some(model_event) = model_event else {
            return report_run_failure(
                session,
                run_id,
                FailureCategory::IncompleteStream,
                "provider stream ended before a terminal event".to_owned(),
                event_sender,
            )
            .await;
        };

        match model_event {
            Ok(ModelEvent::TextDelta(text_delta)) => {
                if text_delta.is_empty() {
                    continue;
                }

                let assistant_delta =
                    match session.append_assistant_delta(run_id, text_delta.clone()) {
                        Ok(assistant_delta) => assistant_delta,
                        Err(_) => return StreamOutcome::Exit,
                    };
                let Some(assistant_message_id) = assistant_delta.assistant_message_id() else {
                    return StreamOutcome::Exit;
                };

                if assistant_delta.assistant_message_created()
                    && !send_harness_event(
                        event_sender,
                        HarnessEvent::AssistantStarted {
                            run_id,
                            assistant_message_id,
                        },
                    )
                    .await
                {
                    return StreamOutcome::Exit;
                }

                if !send_harness_event(
                    event_sender,
                    HarnessEvent::AssistantDelta {
                        run_id,
                        assistant_message_id,
                        text: text_delta,
                    },
                )
                .await
                {
                    return StreamOutcome::Exit;
                }
            }
            Ok(ModelEvent::Finished(completion_outcome)) => {
                if session.finish_run(run_id, completion_outcome).is_err() {
                    return StreamOutcome::Exit;
                }
                if !send_harness_event(
                    event_sender,
                    HarnessEvent::RunFinished {
                        run_id,
                        completion_outcome,
                    },
                )
                .await
                {
                    return StreamOutcome::Exit;
                }
                return StreamOutcome::Continue;
            }
            Ok(ModelEvent::Usage(usage)) => {
                if session.record_usage(run_id, usage).is_err() {
                    return StreamOutcome::Exit;
                }
                if !send_harness_event(event_sender, HarnessEvent::Usage { run_id, usage }).await {
                    return StreamOutcome::Exit;
                }
            }
            Ok(ModelEvent::ReasoningDelta(_)) | Ok(ModelEvent::ToolCall(_)) => {}
            Err(error) => {
                return report_run_failure(
                    session,
                    run_id,
                    FailureCategory::Streaming,
                    error.to_string(),
                    event_sender,
                )
                .await;
            }
        }
    }
}

async fn report_run_failure(
    session: &mut Session,
    run_id: RunId,
    category: FailureCategory,
    detail: String,
    event_sender: &mpsc::Sender<HarnessEvent>,
) -> StreamOutcome {
    if session.fail_run(run_id, category, detail.clone()).is_err() {
        return StreamOutcome::Exit;
    }

    if send_harness_event(event_sender, HarnessEvent::RunFailed { run_id, detail }).await {
        StreamOutcome::Continue
    } else {
        StreamOutcome::Exit
    }
}

async fn report_submission_failure(
    event_sender: &mpsc::Sender<HarnessEvent>,
    detail: String,
) -> StreamOutcome {
    if send_harness_event(event_sender, HarnessEvent::SubmissionFailed { detail }).await {
        StreamOutcome::Continue
    } else {
        StreamOutcome::Exit
    }
}

async fn send_harness_event(
    event_sender: &mpsc::Sender<HarnessEvent>,
    harness_event: HarnessEvent,
) -> bool {
    event_sender.send(harness_event).await.is_ok()
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use tokio::sync::{mpsc, oneshot};

    use super::{HarnessEvent, RuntimeCommand};
    use crate::provider::{
        CompletionOutcome, ModelEvent, ModelLimits, ModelMessage, ModelMetadata, ModelRequest,
        ModelStream, Provider, ProviderError, ToolCall, Usage,
    };
    use crate::session::{MessageId, RunId, Session};

    const TEST_MODEL_ID: &str = "test-model";

    fn test_model_metadata(model_id: &str) -> ModelMetadata {
        ModelMetadata {
            model_id: model_id.to_owned(),
            display_name: model_id.to_owned(),
            limits: ModelLimits {
                context_window_tokens: 1,
                maximum_output_tokens: Some(1),
            },
            prompt_price_usd_per_million_tokens: None,
            completion_price_usd_per_million_tokens: None,
        }
    }

    async fn settle_default_catalog(event_receiver: &mut mpsc::Receiver<HarnessEvent>) {
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::CatalogLoading)
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::CatalogLoaded(vec![test_model_metadata(
                TEST_MODEL_ID,
            )]))
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ModelSelected(test_model_metadata(
                TEST_MODEL_ID
            )))
        );
    }

    async fn expect_run_started(
        event_receiver: &mut mpsc::Receiver<HarnessEvent>,
        expected_content: &str,
    ) -> (RunId, MessageId) {
        let Some(HarnessEvent::RunStarted {
            run_id,
            user_message_id,
            content,
        }) = event_receiver.recv().await
        else {
            panic!("expected a run-started event");
        };
        assert_eq!(content, expected_content);
        (run_id, user_message_id)
    }

    async fn expect_assistant_started(
        event_receiver: &mut mpsc::Receiver<HarnessEvent>,
        run_id: RunId,
        assistant_message_id: MessageId,
    ) {
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::AssistantStarted {
                run_id,
                assistant_message_id,
            })
        );
    }

    async fn expect_assistant_delta(
        event_receiver: &mut mpsc::Receiver<HarnessEvent>,
        run_id: RunId,
        assistant_message_id: MessageId,
        text: &str,
    ) {
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::AssistantDelta {
                run_id,
                assistant_message_id,
                text: text.to_owned(),
            })
        );
    }

    async fn expect_usage(
        event_receiver: &mut mpsc::Receiver<HarnessEvent>,
        run_id: RunId,
        usage: Usage,
    ) {
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::Usage { run_id, usage })
        );
    }

    async fn expect_run_finished(
        event_receiver: &mut mpsc::Receiver<HarnessEvent>,
        run_id: RunId,
        completion_outcome: CompletionOutcome,
    ) {
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::RunFinished {
                run_id,
                completion_outcome,
            })
        );
    }

    async fn expect_run_failed(
        event_receiver: &mut mpsc::Receiver<HarnessEvent>,
        run_id: RunId,
        detail: &str,
    ) {
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::RunFailed {
                run_id,
                detail: detail.to_owned(),
            })
        );
    }

    async fn wait_for_catalog_request(provider: &ScriptedProvider) {
        for _ in 0..64 {
            if provider.catalog_request_count() > 0 {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("the catalog request should have started");
    }

    #[tokio::test]
    async fn rejected_session_start_emits_submission_failure_without_a_run_projection() {
        let provider = ScriptedProvider::new(Vec::new());
        let mut session = Session::new(TEST_MODEL_ID.to_owned());
        session
            .start_run("active request".to_owned())
            .expect("the first run should start");
        let (event_sender, mut event_receiver) = mpsc::channel(1);

        assert_eq!(
            super::consume_provider_stream(
                &provider,
                &mut session,
                "rejected request".to_owned(),
                &event_sender,
            )
            .await,
            super::StreamOutcome::Continue
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::SubmissionFailed {
                detail: "run RunId(1) is still active".to_owned(),
            })
        );
        assert!(event_receiver.try_recv().is_err());
        assert!(provider.requests().is_empty());
        assert_eq!(session.runs().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn streams_the_fake_response_without_changing_its_visible_text() {
        let provider: Arc<dyn Provider> = Arc::new(crate::provider::FakeProvider::new());
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider, crate::provider::MOCK_MODEL_NAME.to_owned());

        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::CatalogLoading)
        );
        let fake_model_metadata = crate::provider::FakeProvider::new()
            .models()
            .await
            .expect("fake catalog should succeed")
            .pop()
            .expect("fake catalog should contain one model");
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::CatalogLoaded(vec![
                fake_model_metadata.clone()
            ]))
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ModelSelected(fake_model_metadata))
        );

        command_sender
            .send(RuntimeCommand::Submit("fake input".to_owned()))
            .await
            .expect("runtime should accept the fake submission");

        let (run_id, user_message_id) = expect_run_started(&mut event_receiver, "fake input").await;
        assert_eq!(run_id.as_u64(), 1);
        assert_eq!(user_message_id.as_u64(), 1);
        let assistant_message_id = MessageId::from_u64(2);
        expect_assistant_started(&mut event_receiver, run_id, assistant_message_id).await;

        let expected_chunks = [
            "The mock runtime received your message. ",
            "This response is deterministic and streamed in chunks. ",
            "The mock work is complete.",
        ];
        let mut response = String::new();
        for (chunk_index, expected_chunk) in expected_chunks.iter().enumerate() {
            if chunk_index > 0 {
                tokio::time::advance(std::time::Duration::from_millis(25)).await;
            }

            expect_assistant_delta(
                &mut event_receiver,
                run_id,
                assistant_message_id,
                expected_chunk,
            )
            .await;
            response.push_str(expected_chunk);
        }

        assert_eq!(
            response,
            "The mock runtime received your message. This response is deterministic and streamed in chunks. The mock work is complete."
        );
        expect_usage(
            &mut event_receiver,
            run_id,
            Usage {
                input_tokens: 24,
                cached_input_tokens: 8,
                output_tokens: 16,
            },
        )
        .await;
        expect_run_finished(&mut event_receiver, run_id, CompletionOutcome::Complete).await;

        drop(command_sender);
        assert!(task_handle.await.is_ok());
    }

    #[tokio::test]
    async fn forwards_exact_requests_and_keeps_turns_serial() {
        let provider = Arc::new(ScriptedProvider::new(vec![
            StreamScript::Events(vec![
                Ok(ModelEvent::TextDelta("first response".to_owned())),
                Ok(ModelEvent::Finished(CompletionOutcome::Complete)),
            ]),
            StreamScript::Events(vec![Ok(ModelEvent::Finished(CompletionOutcome::Complete))]),
        ]));
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider.clone(), TEST_MODEL_ID.to_owned());
        settle_default_catalog(&mut event_receiver).await;

        command_sender
            .send(RuntimeCommand::Submit(" first\nsubmission ".to_owned()))
            .await
            .expect("runtime should accept the first submission");
        let (first_run_id, first_user_message_id) =
            expect_run_started(&mut event_receiver, " first\nsubmission ").await;
        assert_eq!(first_run_id.as_u64(), 1);
        assert_eq!(first_user_message_id.as_u64(), 1);
        let first_assistant_message_id = MessageId::from_u64(2);
        expect_assistant_started(
            &mut event_receiver,
            first_run_id,
            first_assistant_message_id,
        )
        .await;
        expect_assistant_delta(
            &mut event_receiver,
            first_run_id,
            first_assistant_message_id,
            "first response",
        )
        .await;
        expect_run_finished(
            &mut event_receiver,
            first_run_id,
            CompletionOutcome::Complete,
        )
        .await;
        assert_eq!(
            provider.requests(),
            vec![ModelRequest {
                model_id: TEST_MODEL_ID.to_owned(),
                messages: vec![ModelMessage::User {
                    content: " first\nsubmission ".to_owned(),
                }],
            }]
        );

        command_sender
            .send(RuntimeCommand::Submit("second submission".to_owned()))
            .await
            .expect("runtime should accept the second submission");
        let (second_run_id, second_user_message_id) =
            expect_run_started(&mut event_receiver, "second submission").await;
        assert_eq!(second_run_id.as_u64(), 2);
        assert_eq!(second_user_message_id.as_u64(), 3);
        expect_run_finished(
            &mut event_receiver,
            second_run_id,
            CompletionOutcome::Complete,
        )
        .await;
        assert_eq!(
            provider.requests(),
            vec![
                ModelRequest {
                    model_id: TEST_MODEL_ID.to_owned(),
                    messages: vec![ModelMessage::User {
                        content: " first\nsubmission ".to_owned(),
                    }],
                },
                ModelRequest {
                    model_id: TEST_MODEL_ID.to_owned(),
                    messages: vec![
                        ModelMessage::User {
                            content: " first\nsubmission ".to_owned(),
                        },
                        ModelMessage::Assistant {
                            content: "first response".to_owned(),
                            tool_calls: Vec::new(),
                        },
                        ModelMessage::User {
                            content: "second submission".to_owned(),
                        },
                    ],
                },
            ]
        );

        drop(command_sender);
        assert!(task_handle.await.is_ok());
    }

    #[tokio::test]
    async fn scripted_response_can_depend_on_first_turn_history() {
        let provider = Arc::new(ScriptedProvider::new(vec![
            StreamScript::ResponseFromFirstUser,
            StreamScript::ResponseFromFirstUser,
        ]));
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider.clone(), TEST_MODEL_ID.to_owned());
        settle_default_catalog(&mut event_receiver).await;

        command_sender
            .send(RuntimeCommand::Submit("first anchor".to_owned()))
            .await
            .expect("runtime should accept the first submission");
        let (first_run_id, first_user_message_id) =
            expect_run_started(&mut event_receiver, "first anchor").await;
        assert_eq!(first_run_id.as_u64(), 1);
        assert_eq!(first_user_message_id.as_u64(), 1);
        let first_assistant_message_id = MessageId::from_u64(2);
        expect_assistant_started(
            &mut event_receiver,
            first_run_id,
            first_assistant_message_id,
        )
        .await;
        expect_assistant_delta(
            &mut event_receiver,
            first_run_id,
            first_assistant_message_id,
            "remembered: first anchor",
        )
        .await;
        expect_run_finished(
            &mut event_receiver,
            first_run_id,
            CompletionOutcome::Complete,
        )
        .await;

        command_sender
            .send(RuntimeCommand::Submit("second turn".to_owned()))
            .await
            .expect("runtime should accept the second submission");
        let (second_run_id, second_user_message_id) =
            expect_run_started(&mut event_receiver, "second turn").await;
        assert_eq!(second_run_id.as_u64(), 2);
        assert_eq!(second_user_message_id.as_u64(), 3);
        let second_assistant_message_id = MessageId::from_u64(4);
        expect_assistant_started(
            &mut event_receiver,
            second_run_id,
            second_assistant_message_id,
        )
        .await;
        expect_assistant_delta(
            &mut event_receiver,
            second_run_id,
            second_assistant_message_id,
            "remembered: first anchor",
        )
        .await;
        expect_run_finished(
            &mut event_receiver,
            second_run_id,
            CompletionOutcome::Complete,
        )
        .await;

        assert_eq!(
            provider.requests(),
            vec![
                ModelRequest {
                    model_id: TEST_MODEL_ID.to_owned(),
                    messages: vec![ModelMessage::User {
                        content: "first anchor".to_owned(),
                    }],
                },
                ModelRequest {
                    model_id: TEST_MODEL_ID.to_owned(),
                    messages: vec![
                        ModelMessage::User {
                            content: "first anchor".to_owned(),
                        },
                        ModelMessage::Assistant {
                            content: "remembered: first anchor".to_owned(),
                            tool_calls: Vec::new(),
                        },
                        ModelMessage::User {
                            content: "second turn".to_owned(),
                        },
                    ],
                },
            ]
        );

        drop(command_sender);
        assert!(task_handle.await.is_ok());
    }

    #[tokio::test]
    async fn startup_discovery_emits_loading_catalog_and_live_selected_metadata() {
        let models = vec![
            ModelMetadata {
                model_id: TEST_MODEL_ID.to_owned(),
                display_name: "Live test model".to_owned(),
                limits: ModelLimits {
                    context_window_tokens: 16_384,
                    maximum_output_tokens: Some(2_048),
                },
                prompt_price_usd_per_million_tokens: Some("0.10".to_owned()),
                completion_price_usd_per_million_tokens: Some("0.40".to_owned()),
            },
            test_model_metadata("second-model"),
        ];
        let provider = Arc::new(ScriptedProvider::with_catalog_scripts(
            vec![CatalogScript::Result(Ok(models.clone()))],
            Vec::new(),
        ));
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider.clone(), TEST_MODEL_ID.to_owned());

        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::CatalogLoading)
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::CatalogLoaded(models.clone()))
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ModelSelected(models[0].clone()))
        );
        assert_eq!(provider.catalog_request_count(), 1);

        drop(command_sender);
        assert!(task_handle.await.is_ok());
    }

    #[tokio::test]
    async fn failed_catalog_retries_only_after_discover_command() {
        let provider = Arc::new(ScriptedProvider::with_catalog_scripts(
            vec![
                CatalogScript::Result(Err(ProviderError::RequestSetup(
                    "catalog unavailable".to_owned(),
                ))),
                CatalogScript::Result(Ok(vec![test_model_metadata(TEST_MODEL_ID)])),
            ],
            Vec::new(),
        ));
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider.clone(), TEST_MODEL_ID.to_owned());

        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::CatalogLoading)
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::CatalogFailed(
                "request setup failed: catalog unavailable".to_owned()
            ))
        );

        command_sender
            .send(RuntimeCommand::SelectModel(TEST_MODEL_ID.to_owned()))
            .await
            .expect("runtime should accept a model selection command");
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ModelSelectionFailed(
                "model catalog discovery failed; reopen Ctrl-P to retry model discovery".to_owned()
            ))
        );

        command_sender
            .send(RuntimeCommand::DiscoverModels)
            .await
            .expect("runtime should accept a discovery retry command");
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::CatalogLoading)
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::CatalogLoaded(vec![test_model_metadata(
                TEST_MODEL_ID,
            )]))
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ModelSelected(test_model_metadata(
                TEST_MODEL_ID
            )))
        );

        command_sender
            .send(RuntimeCommand::DiscoverModels)
            .await
            .expect("runtime should accept a duplicate discovery command");
        command_sender
            .send(RuntimeCommand::SelectModel("missing-model".to_owned()))
            .await
            .expect("runtime should accept a stale selection command");
        assert!(matches!(
            event_receiver.recv().await,
            Some(HarnessEvent::ModelSelectionFailed(message))
                if message.contains("not available in the loaded catalog")
        ));
        assert!(event_receiver.try_recv().is_err());
        assert_eq!(provider.catalog_request_count(), 2);

        drop(command_sender);
        assert!(task_handle.await.is_ok());
    }

    #[tokio::test]
    async fn valid_selection_changes_the_next_exact_provider_request() {
        let alternate_model = test_model_metadata("alternate-model");
        let provider = Arc::new(ScriptedProvider::with_catalog_scripts(
            vec![CatalogScript::Result(Ok(vec![alternate_model.clone()]))],
            vec![StreamScript::Events(vec![Ok(ModelEvent::Finished(
                CompletionOutcome::Complete,
            ))])],
        ));
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider.clone(), TEST_MODEL_ID.to_owned());

        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::CatalogLoading)
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::CatalogLoaded(vec![alternate_model.clone()]))
        );
        assert!(event_receiver.try_recv().is_err());

        command_sender
            .send(RuntimeCommand::SelectModel(
                alternate_model.model_id.clone(),
            ))
            .await
            .expect("runtime should accept a valid model selection");
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ModelSelected(alternate_model.clone()))
        );

        command_sender
            .send(RuntimeCommand::Submit("selected model input".to_owned()))
            .await
            .expect("runtime should accept a submission");
        let (run_id, user_message_id) =
            expect_run_started(&mut event_receiver, "selected model input").await;
        assert_eq!(run_id.as_u64(), 1);
        assert_eq!(user_message_id.as_u64(), 1);
        expect_run_finished(&mut event_receiver, run_id, CompletionOutcome::Complete).await;
        assert_eq!(
            provider.requests(),
            vec![ModelRequest {
                model_id: alternate_model.model_id,
                messages: vec![ModelMessage::User {
                    content: "selected model input".to_owned(),
                }],
            }]
        );

        drop(command_sender);
        assert!(task_handle.await.is_ok());
    }

    #[tokio::test]
    async fn accepted_model_switch_preserves_history_and_changes_next_run() {
        let alternate_model = test_model_metadata("alternate-model");
        let provider = Arc::new(ScriptedProvider::with_catalog_scripts(
            vec![CatalogScript::Result(Ok(vec![
                test_model_metadata(TEST_MODEL_ID),
                alternate_model.clone(),
            ]))],
            vec![
                StreamScript::Events(vec![
                    Ok(ModelEvent::TextDelta("first answer".to_owned())),
                    Ok(ModelEvent::Finished(CompletionOutcome::Complete)),
                ]),
                StreamScript::Events(vec![Ok(ModelEvent::Finished(CompletionOutcome::Complete))]),
            ],
        ));
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider.clone(), TEST_MODEL_ID.to_owned());

        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::CatalogLoading)
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::CatalogLoaded(vec![
                test_model_metadata(TEST_MODEL_ID),
                alternate_model.clone(),
            ]))
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ModelSelected(test_model_metadata(
                TEST_MODEL_ID
            )))
        );

        command_sender
            .send(RuntimeCommand::Submit("first request".to_owned()))
            .await
            .expect("runtime should accept the first submission");
        let (first_run_id, _) = expect_run_started(&mut event_receiver, "first request").await;
        let first_assistant_message_id = MessageId::from_u64(2);
        expect_assistant_started(
            &mut event_receiver,
            first_run_id,
            first_assistant_message_id,
        )
        .await;
        expect_assistant_delta(
            &mut event_receiver,
            first_run_id,
            first_assistant_message_id,
            "first answer",
        )
        .await;
        expect_run_finished(
            &mut event_receiver,
            first_run_id,
            CompletionOutcome::Complete,
        )
        .await;

        command_sender
            .send(RuntimeCommand::SelectModel(
                alternate_model.model_id.clone(),
            ))
            .await
            .expect("runtime should accept the alternate model selection");
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ModelSelected(alternate_model.clone()))
        );

        command_sender
            .send(RuntimeCommand::Submit("second request".to_owned()))
            .await
            .expect("runtime should accept the second submission");
        let (second_run_id, second_user_message_id) =
            expect_run_started(&mut event_receiver, "second request").await;
        assert_eq!(second_run_id.as_u64(), 2);
        assert_eq!(second_user_message_id.as_u64(), 3);
        expect_run_finished(
            &mut event_receiver,
            second_run_id,
            CompletionOutcome::Complete,
        )
        .await;

        assert_eq!(
            provider.requests()[1],
            ModelRequest {
                model_id: alternate_model.model_id,
                messages: vec![
                    ModelMessage::User {
                        content: "first request".to_owned(),
                    },
                    ModelMessage::Assistant {
                        content: "first answer".to_owned(),
                        tool_calls: Vec::new(),
                    },
                    ModelMessage::User {
                        content: "second request".to_owned(),
                    },
                ],
            }
        );

        drop(command_sender);
        assert!(task_handle.await.is_ok());
    }

    #[tokio::test]
    async fn rejected_model_switch_preserves_history_and_current_request_model() {
        let alternate_model = test_model_metadata("alternate-model");
        let provider = Arc::new(ScriptedProvider::with_catalog_scripts(
            vec![CatalogScript::Result(Ok(vec![
                test_model_metadata(TEST_MODEL_ID),
                alternate_model.clone(),
            ]))],
            vec![
                StreamScript::Events(vec![
                    Ok(ModelEvent::TextDelta("first answer".to_owned())),
                    Ok(ModelEvent::Finished(CompletionOutcome::Complete)),
                ]),
                StreamScript::Events(vec![Ok(ModelEvent::Finished(CompletionOutcome::Complete))]),
            ],
        ));
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider.clone(), TEST_MODEL_ID.to_owned());
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::CatalogLoading)
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::CatalogLoaded(vec![
                test_model_metadata(TEST_MODEL_ID),
                alternate_model,
            ]))
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ModelSelected(test_model_metadata(
                TEST_MODEL_ID
            )))
        );

        command_sender
            .send(RuntimeCommand::Submit("first request".to_owned()))
            .await
            .expect("runtime should accept the first submission");
        let (first_run_id, _) = expect_run_started(&mut event_receiver, "first request").await;
        let first_assistant_message_id = MessageId::from_u64(2);
        expect_assistant_started(
            &mut event_receiver,
            first_run_id,
            first_assistant_message_id,
        )
        .await;
        expect_assistant_delta(
            &mut event_receiver,
            first_run_id,
            first_assistant_message_id,
            "first answer",
        )
        .await;
        expect_run_finished(
            &mut event_receiver,
            first_run_id,
            CompletionOutcome::Complete,
        )
        .await;

        command_sender
            .send(RuntimeCommand::SelectModel("missing-model".to_owned()))
            .await
            .expect("runtime should accept the rejected selection command");
        assert!(matches!(
            event_receiver.recv().await,
            Some(HarnessEvent::ModelSelectionFailed(detail))
                if detail == "model 'missing-model' is not available in the loaded catalog"
        ));

        command_sender
            .send(RuntimeCommand::Submit("second request".to_owned()))
            .await
            .expect("runtime should accept the second submission");
        let (second_run_id, _) = expect_run_started(&mut event_receiver, "second request").await;
        expect_run_finished(
            &mut event_receiver,
            second_run_id,
            CompletionOutcome::Complete,
        )
        .await;

        assert_eq!(provider.requests()[1].model_id, TEST_MODEL_ID);
        assert_eq!(
            provider.requests()[1].messages,
            vec![
                ModelMessage::User {
                    content: "first request".to_owned(),
                },
                ModelMessage::Assistant {
                    content: "first answer".to_owned(),
                    tool_calls: Vec::new(),
                },
                ModelMessage::User {
                    content: "second request".to_owned(),
                },
            ]
        );

        drop(command_sender);
        assert!(task_handle.await.is_ok());
    }

    #[tokio::test]
    async fn invalid_and_empty_selection_preserve_the_active_model() {
        let provider = Arc::new(ScriptedProvider::with_catalog_scripts(
            vec![CatalogScript::Result(Ok(vec![
                test_model_metadata(TEST_MODEL_ID),
                test_model_metadata("other-model"),
            ]))],
            vec![StreamScript::Events(vec![Ok(ModelEvent::Finished(
                CompletionOutcome::Complete,
            ))])],
        ));
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider.clone(), TEST_MODEL_ID.to_owned());
        let expected_models = vec![
            test_model_metadata(TEST_MODEL_ID),
            test_model_metadata("other-model"),
        ];
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::CatalogLoading)
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::CatalogLoaded(expected_models))
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ModelSelected(test_model_metadata(
                TEST_MODEL_ID
            )))
        );

        for model_id in ["stale-model", ""] {
            command_sender
                .send(RuntimeCommand::SelectModel(model_id.to_owned()))
                .await
                .expect("runtime should accept a selection command");
            assert!(matches!(
                event_receiver.recv().await,
                Some(HarnessEvent::ModelSelectionFailed(_))
            ));
        }

        command_sender
            .send(RuntimeCommand::Submit("preserve active model".to_owned()))
            .await
            .expect("runtime should accept a submission");
        let (run_id, _) = expect_run_started(&mut event_receiver, "preserve active model").await;
        expect_run_finished(&mut event_receiver, run_id, CompletionOutcome::Complete).await;
        assert_eq!(provider.requests()[0].model_id, TEST_MODEL_ID);

        drop(command_sender);
        assert!(task_handle.await.is_ok());
    }

    #[tokio::test]
    async fn selection_while_loading_preserves_the_initial_model() {
        let (catalog_completion_sender, catalog_completion_receiver) = oneshot::channel();
        let provider = Arc::new(ScriptedProvider::with_catalog_scripts(
            vec![CatalogScript::WaitForCancellation {
                completion_sender: catalog_completion_sender,
            }],
            vec![StreamScript::Events(vec![Ok(ModelEvent::Finished(
                CompletionOutcome::Complete,
            ))])],
        ));
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider.clone(), TEST_MODEL_ID.to_owned());

        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::CatalogLoading)
        );
        wait_for_catalog_request(&provider).await;
        command_sender
            .send(RuntimeCommand::DiscoverModels)
            .await
            .expect("runtime should accept a loading-state discovery command");
        command_sender
            .send(RuntimeCommand::SelectModel("other-model".to_owned()))
            .await
            .expect("runtime should accept a loading-state selection");
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ModelSelectionFailed(
                "model catalog is still loading".to_owned()
            ))
        );
        assert_eq!(provider.catalog_request_count(), 1);

        command_sender
            .send(RuntimeCommand::Submit("loading catalog input".to_owned()))
            .await
            .expect("runtime should accept a submission while discovery is pending");
        let (run_id, _) = expect_run_started(&mut event_receiver, "loading catalog input").await;
        expect_run_finished(&mut event_receiver, run_id, CompletionOutcome::Complete).await;
        assert_eq!(provider.requests()[0].model_id, TEST_MODEL_ID);

        drop(event_receiver);
        tokio::time::timeout(Duration::from_secs(1), catalog_completion_receiver)
            .await
            .expect("catalog request should stop after output closure")
            .expect("catalog provider should observe cancellation");
        assert!(task_handle.await.is_ok());
        drop(command_sender);
    }

    #[tokio::test]
    async fn forwards_usage_before_a_length_limited_completion() {
        let usage = Usage {
            input_tokens: 42,
            cached_input_tokens: 17,
            output_tokens: 99,
        };
        let provider = Arc::new(ScriptedProvider::new(vec![StreamScript::Events(vec![
            Ok(ModelEvent::Usage(usage)),
            Ok(ModelEvent::Finished(CompletionOutcome::LengthLimited)),
        ])]));
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider.clone(), TEST_MODEL_ID.to_owned());
        settle_default_catalog(&mut event_receiver).await;

        command_sender
            .send(RuntimeCommand::Submit("limited input".to_owned()))
            .await
            .expect("runtime should accept a submission");
        let (run_id, _) = expect_run_started(&mut event_receiver, "limited input").await;
        expect_usage(&mut event_receiver, run_id, usage).await;
        expect_run_finished(
            &mut event_receiver,
            run_id,
            CompletionOutcome::LengthLimited,
        )
        .await;
        assert!(event_receiver.try_recv().is_err());

        drop(command_sender);
        assert!(task_handle.await.is_ok());
    }

    #[tokio::test]
    async fn complete_limited_and_no_content_runs_build_history_from_retained_output() {
        let limited_usage = Usage {
            input_tokens: 20,
            cached_input_tokens: 8,
            output_tokens: 7,
        };
        let no_content_usage = Usage {
            input_tokens: 30,
            cached_input_tokens: 12,
            output_tokens: 0,
        };
        let provider = Arc::new(ScriptedProvider::new(vec![
            StreamScript::Events(vec![
                Ok(ModelEvent::TextDelta("complete answer".to_owned())),
                Ok(ModelEvent::Finished(CompletionOutcome::Complete)),
            ]),
            StreamScript::Events(vec![
                Ok(ModelEvent::TextDelta("limited answer".to_owned())),
                Ok(ModelEvent::Usage(limited_usage)),
                Ok(ModelEvent::Finished(CompletionOutcome::LengthLimited)),
            ]),
            StreamScript::Events(vec![
                Ok(ModelEvent::Usage(no_content_usage)),
                Ok(ModelEvent::Finished(CompletionOutcome::Complete)),
            ]),
        ]));
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider.clone(), TEST_MODEL_ID.to_owned());
        settle_default_catalog(&mut event_receiver).await;

        command_sender
            .send(RuntimeCommand::Submit("first".to_owned()))
            .await
            .expect("runtime should accept the first submission");
        let (first_run_id, _) = expect_run_started(&mut event_receiver, "first").await;
        let first_assistant_message_id = MessageId::from_u64(2);
        expect_assistant_started(
            &mut event_receiver,
            first_run_id,
            first_assistant_message_id,
        )
        .await;
        expect_assistant_delta(
            &mut event_receiver,
            first_run_id,
            first_assistant_message_id,
            "complete answer",
        )
        .await;
        expect_run_finished(
            &mut event_receiver,
            first_run_id,
            CompletionOutcome::Complete,
        )
        .await;

        command_sender
            .send(RuntimeCommand::Submit("second".to_owned()))
            .await
            .expect("runtime should accept the second submission");
        let (second_run_id, _) = expect_run_started(&mut event_receiver, "second").await;
        let second_assistant_message_id = MessageId::from_u64(4);
        expect_assistant_started(
            &mut event_receiver,
            second_run_id,
            second_assistant_message_id,
        )
        .await;
        expect_assistant_delta(
            &mut event_receiver,
            second_run_id,
            second_assistant_message_id,
            "limited answer",
        )
        .await;
        expect_usage(&mut event_receiver, second_run_id, limited_usage).await;
        expect_run_finished(
            &mut event_receiver,
            second_run_id,
            CompletionOutcome::LengthLimited,
        )
        .await;

        command_sender
            .send(RuntimeCommand::Submit("third".to_owned()))
            .await
            .expect("runtime should accept the third submission");
        let (third_run_id, third_user_message_id) =
            expect_run_started(&mut event_receiver, "third").await;
        assert_eq!(third_run_id.as_u64(), 3);
        assert_eq!(third_user_message_id.as_u64(), 5);
        expect_usage(&mut event_receiver, third_run_id, no_content_usage).await;
        expect_run_finished(
            &mut event_receiver,
            third_run_id,
            CompletionOutcome::Complete,
        )
        .await;

        assert_eq!(
            provider.requests()[2],
            ModelRequest {
                model_id: TEST_MODEL_ID.to_owned(),
                messages: vec![
                    ModelMessage::User {
                        content: "first".to_owned(),
                    },
                    ModelMessage::Assistant {
                        content: "complete answer".to_owned(),
                        tool_calls: Vec::new(),
                    },
                    ModelMessage::User {
                        content: "second".to_owned(),
                    },
                    ModelMessage::Assistant {
                        content: "limited answer".to_owned(),
                        tool_calls: Vec::new(),
                    },
                    ModelMessage::User {
                        content: "third".to_owned(),
                    },
                ],
            }
        );

        drop(command_sender);
        assert!(task_handle.await.is_ok());
    }

    #[tokio::test]
    async fn ignores_empty_and_non_text_events_while_forwarding_text() {
        let provider = Arc::new(ScriptedProvider::new(vec![StreamScript::Events(vec![
            Ok(ModelEvent::TextDelta(String::new())),
            Ok(ModelEvent::ReasoningDelta("hidden reasoning".to_owned())),
            Ok(ModelEvent::ToolCall(ToolCall {
                tool_call_id: "call-1".to_owned(),
                tool_name: "hidden-tool".to_owned(),
                arguments: serde_json::Value::Null,
            })),
            Ok(ModelEvent::Usage(Usage {
                input_tokens: 4,
                cached_input_tokens: 2,
                output_tokens: 3,
            })),
            Ok(ModelEvent::TextDelta("visible".to_owned())),
            Ok(ModelEvent::TextDelta(String::new())),
            Ok(ModelEvent::Finished(CompletionOutcome::Complete)),
        ])]));
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider, TEST_MODEL_ID.to_owned());
        settle_default_catalog(&mut event_receiver).await;

        command_sender
            .send(RuntimeCommand::Submit("input".to_owned()))
            .await
            .expect("runtime should accept the submission");

        let (run_id, _) = expect_run_started(&mut event_receiver, "input").await;
        expect_usage(
            &mut event_receiver,
            run_id,
            Usage {
                input_tokens: 4,
                cached_input_tokens: 2,
                output_tokens: 3,
            },
        )
        .await;
        let assistant_message_id = MessageId::from_u64(2);
        expect_assistant_started(&mut event_receiver, run_id, assistant_message_id).await;
        expect_assistant_delta(&mut event_receiver, run_id, assistant_message_id, "visible").await;
        expect_run_finished(&mut event_receiver, run_id, CompletionOutcome::Complete).await;
        assert!(event_receiver.try_recv().is_err());

        drop(command_sender);
        assert!(task_handle.await.is_ok());
    }

    #[tokio::test]
    async fn no_text_done_emits_only_response_finished() {
        let provider = Arc::new(ScriptedProvider::new(vec![StreamScript::Events(vec![
            Ok(ModelEvent::TextDelta(String::new())),
            Ok(ModelEvent::ReasoningDelta("hidden reasoning".to_owned())),
            Ok(ModelEvent::ToolCall(ToolCall {
                tool_call_id: "call-1".to_owned(),
                tool_name: "hidden-tool".to_owned(),
                arguments: serde_json::Value::Null,
            })),
            Ok(ModelEvent::Usage(Usage {
                input_tokens: 4,
                cached_input_tokens: 2,
                output_tokens: 3,
            })),
            Ok(ModelEvent::Finished(CompletionOutcome::Complete)),
        ])]));
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider, TEST_MODEL_ID.to_owned());
        settle_default_catalog(&mut event_receiver).await;

        command_sender
            .send(RuntimeCommand::Submit("input".to_owned()))
            .await
            .expect("runtime should accept the submission");

        let (run_id, _) = expect_run_started(&mut event_receiver, "input").await;
        expect_usage(
            &mut event_receiver,
            run_id,
            Usage {
                input_tokens: 4,
                cached_input_tokens: 2,
                output_tokens: 3,
            },
        )
        .await;
        expect_run_finished(&mut event_receiver, run_id, CompletionOutcome::Complete).await;
        assert!(event_receiver.try_recv().is_err());

        drop(command_sender);
        assert!(task_handle.await.is_ok());
    }

    #[tokio::test]
    async fn setup_error_is_readable_and_the_next_turn_recovers() {
        let provider = Arc::new(ScriptedProvider::new(vec![
            StreamScript::SetupError(ProviderError::RequestSetup("invalid request".to_owned())),
            StreamScript::Events(vec![
                Ok(ModelEvent::TextDelta("recovered".to_owned())),
                Ok(ModelEvent::Finished(CompletionOutcome::Complete)),
            ]),
        ]));
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider.clone(), TEST_MODEL_ID.to_owned());
        settle_default_catalog(&mut event_receiver).await;

        command_sender
            .send(RuntimeCommand::Submit("first".to_owned()))
            .await
            .expect("runtime should accept the first submission");
        let (first_run_id, first_user_message_id) =
            expect_run_started(&mut event_receiver, "first").await;
        assert_eq!(first_run_id.as_u64(), 1);
        assert_eq!(first_user_message_id.as_u64(), 1);
        expect_run_failed(
            &mut event_receiver,
            first_run_id,
            "request setup failed: invalid request",
        )
        .await;
        assert!(event_receiver.try_recv().is_err());

        command_sender
            .send(RuntimeCommand::Submit("recovery".to_owned()))
            .await
            .expect("runtime should accept the recovery submission");
        let (recovery_run_id, recovery_user_message_id) =
            expect_run_started(&mut event_receiver, "recovery").await;
        assert_eq!(recovery_run_id.as_u64(), 2);
        assert_eq!(recovery_user_message_id.as_u64(), 2);
        let recovery_assistant_message_id = MessageId::from_u64(3);
        expect_assistant_started(
            &mut event_receiver,
            recovery_run_id,
            recovery_assistant_message_id,
        )
        .await;
        expect_assistant_delta(
            &mut event_receiver,
            recovery_run_id,
            recovery_assistant_message_id,
            "recovered",
        )
        .await;
        expect_run_finished(
            &mut event_receiver,
            recovery_run_id,
            CompletionOutcome::Complete,
        )
        .await;

        assert_eq!(
            provider.requests(),
            vec![
                ModelRequest {
                    model_id: TEST_MODEL_ID.to_owned(),
                    messages: vec![ModelMessage::User {
                        content: "first".to_owned(),
                    }],
                },
                ModelRequest {
                    model_id: TEST_MODEL_ID.to_owned(),
                    messages: vec![
                        ModelMessage::User {
                            content: "first".to_owned(),
                        },
                        ModelMessage::User {
                            content: "recovery".to_owned(),
                        },
                    ],
                },
            ]
        );

        drop(command_sender);
        assert!(task_handle.await.is_ok());
    }

    #[tokio::test]
    async fn stream_error_preserves_partial_text_drops_late_events_and_recovers() {
        let (completion_sender, completion_receiver) = oneshot::channel();
        let provider = Arc::new(ScriptedProvider::new(vec![
            StreamScript::WaitForReceiverDrop {
                events: vec![
                    Ok(ModelEvent::TextDelta("partial".to_owned())),
                    Err(ProviderError::Streaming("connection lost".to_owned())),
                    Ok(ModelEvent::Finished(CompletionOutcome::Complete)),
                ],
                completion_sender,
            },
            StreamScript::Events(vec![
                Ok(ModelEvent::TextDelta("recovered".to_owned())),
                Ok(ModelEvent::Finished(CompletionOutcome::Complete)),
            ]),
        ]));
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider.clone(), TEST_MODEL_ID.to_owned());
        settle_default_catalog(&mut event_receiver).await;

        command_sender
            .send(RuntimeCommand::Submit("first".to_owned()))
            .await
            .expect("runtime should accept the first submission");
        let (first_run_id, first_user_message_id) =
            expect_run_started(&mut event_receiver, "first").await;
        assert_eq!(first_run_id.as_u64(), 1);
        assert_eq!(first_user_message_id.as_u64(), 1);
        let first_assistant_message_id = MessageId::from_u64(2);
        expect_assistant_started(
            &mut event_receiver,
            first_run_id,
            first_assistant_message_id,
        )
        .await;
        expect_assistant_delta(
            &mut event_receiver,
            first_run_id,
            first_assistant_message_id,
            "partial",
        )
        .await;
        expect_run_failed(
            &mut event_receiver,
            first_run_id,
            "streaming failed: connection lost",
        )
        .await;
        assert!(event_receiver.try_recv().is_err());
        tokio::time::timeout(std::time::Duration::from_secs(1), completion_receiver)
            .await
            .expect("provider receiver should be dropped after a stream error")
            .expect("provider should report receiver closure");

        command_sender
            .send(RuntimeCommand::Submit("recovery".to_owned()))
            .await
            .expect("runtime should accept the recovery submission");
        let (recovery_run_id, recovery_user_message_id) =
            expect_run_started(&mut event_receiver, "recovery").await;
        assert_eq!(recovery_run_id.as_u64(), 2);
        assert_eq!(recovery_user_message_id.as_u64(), 3);
        let recovery_assistant_message_id = MessageId::from_u64(4);
        expect_assistant_started(
            &mut event_receiver,
            recovery_run_id,
            recovery_assistant_message_id,
        )
        .await;
        expect_assistant_delta(
            &mut event_receiver,
            recovery_run_id,
            recovery_assistant_message_id,
            "recovered",
        )
        .await;
        expect_run_finished(
            &mut event_receiver,
            recovery_run_id,
            CompletionOutcome::Complete,
        )
        .await;

        assert_eq!(
            provider.requests()[1].messages,
            vec![
                ModelMessage::User {
                    content: "first".to_owned(),
                },
                ModelMessage::User {
                    content: "recovery".to_owned(),
                },
            ]
        );

        drop(command_sender);
        assert!(task_handle.await.is_ok());
    }

    #[tokio::test]
    async fn incomplete_stream_reports_error_without_finishing_and_recovers() {
        let provider = Arc::new(ScriptedProvider::new(vec![
            StreamScript::Events(vec![Ok(ModelEvent::TextDelta("partial".to_owned()))]),
            StreamScript::Events(vec![
                Ok(ModelEvent::TextDelta("recovered".to_owned())),
                Ok(ModelEvent::Finished(CompletionOutcome::Complete)),
            ]),
        ]));
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider.clone(), TEST_MODEL_ID.to_owned());
        settle_default_catalog(&mut event_receiver).await;

        command_sender
            .send(RuntimeCommand::Submit("first".to_owned()))
            .await
            .expect("runtime should accept the first submission");
        let (first_run_id, first_user_message_id) =
            expect_run_started(&mut event_receiver, "first").await;
        assert_eq!(first_run_id.as_u64(), 1);
        assert_eq!(first_user_message_id.as_u64(), 1);
        let first_assistant_message_id = MessageId::from_u64(2);
        expect_assistant_started(
            &mut event_receiver,
            first_run_id,
            first_assistant_message_id,
        )
        .await;
        expect_assistant_delta(
            &mut event_receiver,
            first_run_id,
            first_assistant_message_id,
            "partial",
        )
        .await;
        expect_run_failed(
            &mut event_receiver,
            first_run_id,
            "provider stream ended before a terminal event",
        )
        .await;
        assert!(event_receiver.try_recv().is_err());

        command_sender
            .send(RuntimeCommand::Submit("recovery".to_owned()))
            .await
            .expect("runtime should accept the recovery submission");
        let (recovery_run_id, recovery_user_message_id) =
            expect_run_started(&mut event_receiver, "recovery").await;
        assert_eq!(recovery_run_id.as_u64(), 2);
        assert_eq!(recovery_user_message_id.as_u64(), 3);
        let recovery_assistant_message_id = MessageId::from_u64(4);
        expect_assistant_started(
            &mut event_receiver,
            recovery_run_id,
            recovery_assistant_message_id,
        )
        .await;
        expect_assistant_delta(
            &mut event_receiver,
            recovery_run_id,
            recovery_assistant_message_id,
            "recovered",
        )
        .await;
        expect_run_finished(
            &mut event_receiver,
            recovery_run_id,
            CompletionOutcome::Complete,
        )
        .await;

        assert_eq!(
            provider.requests()[1].messages,
            vec![
                ModelMessage::User {
                    content: "first".to_owned(),
                },
                ModelMessage::User {
                    content: "recovery".to_owned(),
                },
            ]
        );

        drop(command_sender);
        assert!(task_handle.await.is_ok());
    }

    #[tokio::test]
    async fn first_done_drops_the_provider_stream_and_suppresses_late_events() {
        let (completion_sender, completion_receiver) = oneshot::channel();
        let provider = Arc::new(ScriptedProvider::new(vec![
            StreamScript::WaitForReceiverDrop {
                events: vec![
                    Ok(ModelEvent::Finished(CompletionOutcome::Complete)),
                    Ok(ModelEvent::TextDelta("late text".to_owned())),
                    Err(ProviderError::Streaming("late error".to_owned())),
                ],
                completion_sender,
            },
        ]));
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider, TEST_MODEL_ID.to_owned());
        settle_default_catalog(&mut event_receiver).await;

        command_sender
            .send(RuntimeCommand::Submit("input".to_owned()))
            .await
            .expect("runtime should accept the submission");
        let (run_id, _) = expect_run_started(&mut event_receiver, "input").await;
        expect_run_finished(&mut event_receiver, run_id, CompletionOutcome::Complete).await;
        assert!(event_receiver.try_recv().is_err());
        tokio::time::timeout(std::time::Duration::from_secs(1), completion_receiver)
            .await
            .expect("provider receiver should be dropped after Done")
            .expect("provider should report receiver closure");

        drop(command_sender);
        assert!(task_handle.await.is_ok());
    }

    #[tokio::test]
    async fn closed_harness_output_drops_active_generation_and_catalog_work() {
        let (catalog_completion_sender, catalog_completion_receiver) = oneshot::channel();
        let (stream_completion_sender, stream_completion_receiver) = oneshot::channel();
        let provider = Arc::new(ScriptedProvider::with_catalog_scripts(
            vec![CatalogScript::WaitForCancellation {
                completion_sender: catalog_completion_sender,
            }],
            vec![StreamScript::WaitForReceiverDrop {
                events: vec![Ok(ModelEvent::TextDelta("partial".to_owned()))],
                completion_sender: stream_completion_sender,
            }],
        ));
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider.clone(), TEST_MODEL_ID.to_owned());
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::CatalogLoading)
        );
        wait_for_catalog_request(&provider).await;

        command_sender
            .send(RuntimeCommand::Submit("input".to_owned()))
            .await
            .expect("runtime should accept the submission");
        let (run_id, _) = expect_run_started(&mut event_receiver, "input").await;
        let assistant_message_id = MessageId::from_u64(2);
        expect_assistant_started(&mut event_receiver, run_id, assistant_message_id).await;
        expect_assistant_delta(&mut event_receiver, run_id, assistant_message_id, "partial").await;

        drop(event_receiver);
        tokio::time::timeout(Duration::from_secs(1), stream_completion_receiver)
            .await
            .expect("provider receiver should be dropped after output closure")
            .expect("provider should report receiver closure");
        tokio::time::timeout(Duration::from_secs(1), catalog_completion_receiver)
            .await
            .expect("catalog request should be dropped after output closure")
            .expect("catalog provider should report cancellation");
        assert!(task_handle.await.is_ok());
        drop(command_sender);
    }

    #[tokio::test]
    async fn aborting_runtime_drops_an_active_catalog_request() {
        let (catalog_completion_sender, catalog_completion_receiver) = oneshot::channel();
        let provider = Arc::new(ScriptedProvider::with_catalog_scripts(
            vec![CatalogScript::WaitForCancellation {
                completion_sender: catalog_completion_sender,
            }],
            Vec::new(),
        ));
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider.clone(), TEST_MODEL_ID.to_owned());

        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::CatalogLoading)
        );
        wait_for_catalog_request(&provider).await;

        task_handle.abort();
        assert!(task_handle.await.is_err());
        tokio::time::timeout(Duration::from_secs(1), catalog_completion_receiver)
            .await
            .expect("catalog request should stop with the runtime task")
            .expect("catalog provider should observe runtime cancellation");
        drop(command_sender);
        drop(event_receiver);
    }

    #[tokio::test]
    async fn closes_when_the_command_channel_closes() {
        let provider: Arc<dyn Provider> = Arc::new(ScriptedProvider::new(Vec::new()));
        let (command_sender, _event_receiver, task_handle) =
            super::spawn_runtime(provider, TEST_MODEL_ID.to_owned());

        drop(command_sender);
        assert!(task_handle.await.is_ok());
    }

    #[derive(Debug)]
    struct ScriptedProvider {
        requests: Arc<Mutex<Vec<ModelRequest>>>,
        catalog_requests: Arc<Mutex<usize>>,
        catalog_scripts: Mutex<VecDeque<CatalogScript>>,
        scripts: Mutex<VecDeque<StreamScript>>,
    }

    impl ScriptedProvider {
        fn new(scripts: Vec<StreamScript>) -> Self {
            Self::with_catalog_scripts(
                vec![CatalogScript::Result(Ok(vec![test_model_metadata(
                    TEST_MODEL_ID,
                )]))],
                scripts,
            )
        }

        fn with_catalog_scripts(
            catalog_scripts: Vec<CatalogScript>,
            scripts: Vec<StreamScript>,
        ) -> Self {
            Self {
                requests: Arc::new(Mutex::new(Vec::new())),
                catalog_requests: Arc::new(Mutex::new(0)),
                catalog_scripts: Mutex::new(catalog_scripts.into()),
                scripts: Mutex::new(scripts.into()),
            }
        }

        fn requests(&self) -> Vec<ModelRequest> {
            self.requests
                .lock()
                .expect("request recording lock should not be poisoned")
                .clone()
        }

        fn catalog_request_count(&self) -> usize {
            *self
                .catalog_requests
                .lock()
                .expect("catalog request lock should not be poisoned")
        }
    }

    #[derive(Debug)]
    enum CatalogScript {
        Result(Result<Vec<ModelMetadata>, ProviderError>),
        WaitForCancellation {
            completion_sender: oneshot::Sender<()>,
        },
    }

    #[derive(Debug)]
    enum StreamScript {
        SetupError(ProviderError),
        Events(Vec<Result<ModelEvent, ProviderError>>),
        ResponseFromFirstUser,
        WaitForReceiverDrop {
            events: Vec<Result<ModelEvent, ProviderError>>,
            completion_sender: oneshot::Sender<()>,
        },
    }

    #[async_trait]
    impl Provider for ScriptedProvider {
        fn model_metadata(&self, model_id: &str) -> Result<ModelMetadata, ProviderError> {
            Ok(test_model_metadata(model_id))
        }

        async fn models(&self) -> Result<Vec<ModelMetadata>, ProviderError> {
            *self
                .catalog_requests
                .lock()
                .expect("catalog request lock should not be poisoned") += 1;
            let catalog_script = self
                .catalog_scripts
                .lock()
                .expect("catalog script lock should not be poisoned")
                .pop_front()
                .expect("the test provider should have a script for every catalog request");

            match catalog_script {
                CatalogScript::Result(result) => result,
                CatalogScript::WaitForCancellation { completion_sender } => {
                    let _completion_guard = NotifyOnDrop(Some(completion_sender));
                    std::future::pending().await
                }
            }
        }

        async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ProviderError> {
            self.requests
                .lock()
                .expect("request recording lock should not be poisoned")
                .push(request.clone());

            let script = self
                .scripts
                .lock()
                .expect("script lock should not be poisoned")
                .pop_front()
                .expect("the test provider should have a script for every request");

            match script {
                StreamScript::SetupError(error) => Err(error),
                StreamScript::Events(events) => Ok(buffered_stream(events).await),
                StreamScript::ResponseFromFirstUser => {
                    let first_user_content = request
                        .messages
                        .iter()
                        .find_map(|message| match message {
                            ModelMessage::User { content } => Some(content.clone()),
                            _ => None,
                        })
                        .expect("the scripted request should contain a user message");
                    Ok(buffered_stream(vec![
                        Ok(ModelEvent::TextDelta(format!(
                            "remembered: {first_user_content}"
                        ))),
                        Ok(ModelEvent::Finished(CompletionOutcome::Complete)),
                    ])
                    .await)
                }
                StreamScript::WaitForReceiverDrop {
                    events,
                    completion_sender,
                } => Ok(stream_that_reports_receiver_drop(events, completion_sender)),
            }
        }
    }

    #[derive(Debug)]
    struct NotifyOnDrop(Option<oneshot::Sender<()>>);

    impl Drop for NotifyOnDrop {
        fn drop(&mut self) {
            if let Some(completion_sender) = self.0.take() {
                let _ = completion_sender.send(());
            }
        }
    }

    async fn buffered_stream(events: Vec<Result<ModelEvent, ProviderError>>) -> ModelStream {
        let capacity = events.len().max(1);
        let (event_sender, event_receiver) = mpsc::channel(capacity);
        for event in events {
            event_sender
                .send(event)
                .await
                .expect("the scripted stream receiver should be alive");
        }
        event_receiver
    }

    fn stream_that_reports_receiver_drop(
        events: Vec<Result<ModelEvent, ProviderError>>,
        completion_sender: oneshot::Sender<()>,
    ) -> ModelStream {
        let capacity = events.len().max(1);
        let (event_sender, event_receiver) = mpsc::channel(capacity);
        tokio::spawn(async move {
            for event in events {
                if event_sender.send(event).await.is_err() {
                    break;
                }
            }
            event_sender.closed().await;
            let _ = completion_sender.send(());
        });
        event_receiver
    }
}
