//! Harness runtime and event flow.

use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const COMMAND_CHANNEL_CAPACITY: usize = 4;
const EVENT_CHANNEL_CAPACITY: usize = 8;
const MOCK_RESPONSE_DELAY: Duration = Duration::from_millis(25);
const MOCK_RESPONSE_CHUNKS: [&str; 3] = [
    "The mock runtime received your message. ",
    "This response is deterministic and streamed in chunks. ",
    "The mock work is complete.",
];

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

/// Identifier for the deterministic model used by the mock runtime.
pub const MOCK_MODEL_NAME: &str = "mock-runtime";

/// Start the deterministic mock runtime.
///
/// The caller owns the returned task handle and can abort and await it during shutdown.
pub fn spawn_mock_runtime() -> (
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
                    let Some(RuntimeCommand::Submit(_submission)) = command else {
                        break;
                    };

                    if emit_mock_response(&event_sender).await.is_err() {
                        break;
                    }
                }
                _ = event_sender.closed() => break,
            }
        }
    });

    (command_sender, event_receiver, task_handle)
}

async fn emit_mock_response(event_sender: &mpsc::Sender<HarnessEvent>) -> Result<(), ()> {
    event_sender
        .send(HarnessEvent::ResponseStarted)
        .await
        .map_err(|_| ())?;

    for (chunk_index, response_chunk) in MOCK_RESPONSE_CHUNKS.iter().enumerate() {
        if chunk_index > 0 {
            tokio::time::sleep(MOCK_RESPONSE_DELAY).await;
        }

        event_sender
            .send(HarnessEvent::AssistantDelta((*response_chunk).to_owned()))
            .await
            .map_err(|_| ())?;
    }

    event_sender
        .send(HarnessEvent::ResponseFinished)
        .await
        .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::{HarnessEvent, MOCK_RESPONSE_CHUNKS, MOCK_RESPONSE_DELAY, RuntimeCommand};

    #[tokio::test(start_paused = true)]
    async fn emits_ordered_chunks_for_two_serial_submissions() {
        let (command_sender, mut event_receiver, task_handle) = super::spawn_mock_runtime();
        let expected_response = MOCK_RESPONSE_CHUNKS.concat();

        command_sender
            .send(RuntimeCommand::Submit("first submission".to_owned()))
            .await
            .expect("runtime should accept the first submission");

        assert_eq!(
            receive_mock_response(&mut event_receiver).await,
            expected_response
        );

        command_sender
            .send(RuntimeCommand::Submit("second submission".to_owned()))
            .await
            .expect("runtime should accept the second submission");

        assert_eq!(
            receive_mock_response(&mut event_receiver).await,
            expected_response
        );

        drop(command_sender);
        assert!(task_handle.await.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn closes_when_command_or_event_channel_closes() {
        let (command_sender, _event_receiver, task_handle) = super::spawn_mock_runtime();
        drop(command_sender);
        assert!(task_handle.await.is_ok());

        let (command_sender, event_receiver, task_handle) = super::spawn_mock_runtime();
        drop(event_receiver);
        assert!(task_handle.await.is_ok());
        drop(command_sender);
    }

    async fn receive_mock_response(
        event_receiver: &mut tokio::sync::mpsc::Receiver<HarnessEvent>,
    ) -> String {
        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ResponseStarted)
        );

        let mut response = String::new();
        for (chunk_index, expected_chunk) in MOCK_RESPONSE_CHUNKS.iter().enumerate() {
            if chunk_index > 0 {
                tokio::time::advance(MOCK_RESPONSE_DELAY).await;
            }

            let event = event_receiver
                .recv()
                .await
                .expect("mock response should contain every chunk");
            let HarnessEvent::AssistantDelta(response_chunk) = event else {
                panic!("mock response should emit only assistant deltas before finishing");
            };
            assert!(!response_chunk.is_empty());
            assert_eq!(response_chunk, *expected_chunk);
            response.push_str(&response_chunk);
        }

        assert_eq!(
            event_receiver.recv().await,
            Some(HarnessEvent::ResponseFinished)
        );
        response
    }
}
