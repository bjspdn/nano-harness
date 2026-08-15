//! Harness runtime and event flow.

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::provider::{ModelEvent, ModelRequest, Provider};

const COMMAND_CHANNEL_CAPACITY: usize = 4;
const EVENT_CHANNEL_CAPACITY: usize = 8;

/// The commands accepted by the harness runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeCommand {
    Submit(String),
}

/// Events emitted by the harness runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessEvent {
    ResponseStarted,
    AssistantDelta(String),
    ResponseFinished,
    Error(String),
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
        loop {
            tokio::select! {
                command = command_receiver.recv() => {
                    let Some(RuntimeCommand::Submit(input)) = command else {
                        break;
                    };

                    if consume_provider_stream(
                        provider.as_ref(),
                        &model_id,
                        input,
                        &event_sender,
                    )
                    .await
                        == StreamOutcome::Exit
                    {
                        break;
                    }
                }
                _ = event_sender.closed() => break,
            }
        }
    });

    (command_sender, event_receiver, task_handle)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamOutcome {
    Continue,
    Exit,
}

async fn consume_provider_stream(
    provider: &dyn Provider,
    model_id: &str,
    input: String,
    event_sender: &mpsc::Sender<HarnessEvent>,
) -> StreamOutcome {
    let request = ModelRequest {
        model_id: model_id.to_owned(),
        input,
    };

    let stream_result = tokio::select! {
        stream_result = provider.stream(request) => stream_result,
        _ = event_sender.closed() => return StreamOutcome::Exit,
    };
    let mut model_stream = match stream_result {
        Ok(model_stream) => model_stream,
        Err(error) => {
            return report_error(event_sender, error.to_string()).await;
        }
    };

    let mut response_started = false;
    loop {
        let model_event = tokio::select! {
            model_event = model_stream.recv() => model_event,
            _ = event_sender.closed() => return StreamOutcome::Exit,
        };

        let Some(model_event) = model_event else {
            return report_error(event_sender, "provider stream ended before Done".to_owned())
                .await;
        };

        match model_event {
            Ok(ModelEvent::TextDelta(text_delta)) => {
                if text_delta.is_empty() {
                    continue;
                }

                if !response_started {
                    if !send_harness_event(event_sender, HarnessEvent::ResponseStarted).await {
                        return StreamOutcome::Exit;
                    }
                    response_started = true;
                }

                if !send_harness_event(event_sender, HarnessEvent::AssistantDelta(text_delta)).await
                {
                    return StreamOutcome::Exit;
                }
            }
            Ok(ModelEvent::Done) => {
                if !send_harness_event(event_sender, HarnessEvent::ResponseFinished).await {
                    return StreamOutcome::Exit;
                }
                return StreamOutcome::Continue;
            }
            Ok(ModelEvent::ReasoningDelta(_))
            | Ok(ModelEvent::ToolCall(_))
            | Ok(ModelEvent::Usage(_)) => {}
            Err(error) => return report_error(event_sender, error.to_string()).await,
        }
    }
}

async fn report_error(event_sender: &mpsc::Sender<HarnessEvent>, error: String) -> StreamOutcome {
    if send_harness_event(event_sender, HarnessEvent::Error(error)).await {
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

    use async_trait::async_trait;
    use tokio::sync::{mpsc, oneshot};

    use super::{HarnessEvent, RuntimeCommand};
    use crate::provider::{
        ModelEvent, ModelLimits, ModelMetadata, ModelRequest, ModelStream, Provider, ProviderError,
        ToolCall, Usage,
    };

    const TEST_MODEL_ID: &str = "test-model";

    #[tokio::test(start_paused = true)]
    async fn streams_the_fake_response_without_changing_its_visible_text() {
        let provider: Arc<dyn Provider> = Arc::new(crate::provider::FakeProvider::new());
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider, crate::provider::MOCK_MODEL_NAME.to_owned());

        command_sender
            .send(RuntimeCommand::Submit("fake input".to_owned()))
            .await
            .expect("runtime should accept the fake submission");

        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ResponseStarted)
        );

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

            assert_eq!(
                event_receiver.recv().await,
                Some(HarnessEvent::AssistantDelta((*expected_chunk).to_owned()))
            );
            response.push_str(expected_chunk);
        }

        assert_eq!(
            response,
            "The mock runtime received your message. This response is deterministic and streamed in chunks. The mock work is complete."
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ResponseFinished)
        );

        drop(command_sender);
        assert!(task_handle.await.is_ok());
    }

    #[tokio::test]
    async fn forwards_exact_requests_and_keeps_turns_serial() {
        let provider = Arc::new(ScriptedProvider::new(vec![
            StreamScript::Events(vec![Ok(ModelEvent::Done)]),
            StreamScript::Events(vec![Ok(ModelEvent::Done)]),
        ]));
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider.clone(), TEST_MODEL_ID.to_owned());

        command_sender
            .send(RuntimeCommand::Submit(" first\nsubmission ".to_owned()))
            .await
            .expect("runtime should accept the first submission");
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ResponseFinished)
        );
        assert_eq!(
            provider.requests(),
            vec![ModelRequest {
                model_id: TEST_MODEL_ID.to_owned(),
                input: " first\nsubmission ".to_owned(),
            }]
        );

        command_sender
            .send(RuntimeCommand::Submit("second submission".to_owned()))
            .await
            .expect("runtime should accept the second submission");
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ResponseFinished)
        );
        assert_eq!(
            provider.requests(),
            vec![
                ModelRequest {
                    model_id: TEST_MODEL_ID.to_owned(),
                    input: " first\nsubmission ".to_owned(),
                },
                ModelRequest {
                    model_id: TEST_MODEL_ID.to_owned(),
                    input: "second submission".to_owned(),
                },
            ]
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
            Ok(ModelEvent::Done),
        ])]));
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider, TEST_MODEL_ID.to_owned());

        command_sender
            .send(RuntimeCommand::Submit("input".to_owned()))
            .await
            .expect("runtime should accept the submission");

        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ResponseStarted)
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::AssistantDelta("visible".to_owned()))
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ResponseFinished)
        );
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
            Ok(ModelEvent::Done),
        ])]));
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider, TEST_MODEL_ID.to_owned());

        command_sender
            .send(RuntimeCommand::Submit("input".to_owned()))
            .await
            .expect("runtime should accept the submission");

        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ResponseFinished)
        );
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
                Ok(ModelEvent::Done),
            ]),
        ]));
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider, TEST_MODEL_ID.to_owned());

        command_sender
            .send(RuntimeCommand::Submit("first".to_owned()))
            .await
            .expect("runtime should accept the first submission");
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::Error(
                "request setup failed: invalid request".to_owned()
            ))
        );
        assert!(event_receiver.try_recv().is_err());

        command_sender
            .send(RuntimeCommand::Submit("recovery".to_owned()))
            .await
            .expect("runtime should accept the recovery submission");
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ResponseStarted)
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::AssistantDelta("recovered".to_owned()))
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ResponseFinished)
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
                    Ok(ModelEvent::Done),
                ],
                completion_sender,
            },
            StreamScript::Events(vec![
                Ok(ModelEvent::TextDelta("recovered".to_owned())),
                Ok(ModelEvent::Done),
            ]),
        ]));
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider, TEST_MODEL_ID.to_owned());

        command_sender
            .send(RuntimeCommand::Submit("first".to_owned()))
            .await
            .expect("runtime should accept the first submission");
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ResponseStarted)
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::AssistantDelta("partial".to_owned()))
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::Error(
                "streaming failed: connection lost".to_owned()
            ))
        );
        assert!(event_receiver.try_recv().is_err());
        tokio::time::timeout(std::time::Duration::from_secs(1), completion_receiver)
            .await
            .expect("provider receiver should be dropped after a stream error")
            .expect("provider should report receiver closure");

        command_sender
            .send(RuntimeCommand::Submit("recovery".to_owned()))
            .await
            .expect("runtime should accept the recovery submission");
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ResponseStarted)
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::AssistantDelta("recovered".to_owned()))
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ResponseFinished)
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
                Ok(ModelEvent::Done),
            ]),
        ]));
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider, TEST_MODEL_ID.to_owned());

        command_sender
            .send(RuntimeCommand::Submit("first".to_owned()))
            .await
            .expect("runtime should accept the first submission");
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ResponseStarted)
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::AssistantDelta("partial".to_owned()))
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::Error(
                "provider stream ended before Done".to_owned()
            ))
        );
        assert!(event_receiver.try_recv().is_err());

        command_sender
            .send(RuntimeCommand::Submit("recovery".to_owned()))
            .await
            .expect("runtime should accept the recovery submission");
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ResponseStarted)
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::AssistantDelta("recovered".to_owned()))
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ResponseFinished)
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
                    Ok(ModelEvent::Done),
                    Ok(ModelEvent::TextDelta("late text".to_owned())),
                    Err(ProviderError::Streaming("late error".to_owned())),
                ],
                completion_sender,
            },
        ]));
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider, TEST_MODEL_ID.to_owned());

        command_sender
            .send(RuntimeCommand::Submit("input".to_owned()))
            .await
            .expect("runtime should accept the submission");
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ResponseFinished)
        );
        assert!(event_receiver.try_recv().is_err());
        tokio::time::timeout(std::time::Duration::from_secs(1), completion_receiver)
            .await
            .expect("provider receiver should be dropped after Done")
            .expect("provider should report receiver closure");

        drop(command_sender);
        assert!(task_handle.await.is_ok());
    }

    #[tokio::test]
    async fn closed_harness_output_drops_active_provider_work() {
        let (completion_sender, completion_receiver) = oneshot::channel();
        let provider = Arc::new(ScriptedProvider::new(vec![
            StreamScript::WaitForReceiverDrop {
                events: vec![Ok(ModelEvent::TextDelta("partial".to_owned()))],
                completion_sender,
            },
        ]));
        let (command_sender, mut event_receiver, task_handle) =
            super::spawn_runtime(provider, TEST_MODEL_ID.to_owned());

        command_sender
            .send(RuntimeCommand::Submit("input".to_owned()))
            .await
            .expect("runtime should accept the submission");
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ResponseStarted)
        );
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::AssistantDelta("partial".to_owned()))
        );

        drop(event_receiver);
        tokio::time::timeout(std::time::Duration::from_secs(1), completion_receiver)
            .await
            .expect("provider receiver should be dropped after output closure")
            .expect("provider should report receiver closure");
        assert!(task_handle.await.is_ok());
        drop(command_sender);
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
        scripts: Mutex<VecDeque<StreamScript>>,
    }

    impl ScriptedProvider {
        fn new(scripts: Vec<StreamScript>) -> Self {
            Self {
                requests: Arc::new(Mutex::new(Vec::new())),
                scripts: Mutex::new(scripts.into()),
            }
        }

        fn requests(&self) -> Vec<ModelRequest> {
            self.requests
                .lock()
                .expect("request recording lock should not be poisoned")
                .clone()
        }
    }

    #[derive(Debug)]
    enum StreamScript {
        SetupError(ProviderError),
        Events(Vec<Result<ModelEvent, ProviderError>>),
        WaitForReceiverDrop {
            events: Vec<Result<ModelEvent, ProviderError>>,
            completion_sender: oneshot::Sender<()>,
        },
    }

    #[async_trait]
    impl Provider for ScriptedProvider {
        fn model_metadata(&self, model_id: &str) -> Result<ModelMetadata, ProviderError> {
            Ok(ModelMetadata {
                model_id: model_id.to_owned(),
                display_name: model_id.to_owned(),
                limits: ModelLimits {
                    context_window_tokens: 1,
                    maximum_output_tokens: 1,
                },
            })
        }

        async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ProviderError> {
            self.requests
                .lock()
                .expect("request recording lock should not be poisoned")
                .push(request);

            let script = self
                .scripts
                .lock()
                .expect("script lock should not be poisoned")
                .pop_front()
                .expect("the test provider should have a script for every request");

            match script {
                StreamScript::SetupError(error) => Err(error),
                StreamScript::Events(events) => Ok(buffered_stream(events).await),
                StreamScript::WaitForReceiverDrop {
                    events,
                    completion_sender,
                } => Ok(stream_that_reports_receiver_drop(events, completion_sender)),
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
