use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use super::{
    CompletionOutcome, ModelEvent, ModelLimits, ModelMetadata, ModelRequest, ModelStream, Provider,
};
use super::{ProviderError, Usage};

/// Identifier and display name for the deterministic fake model.
pub const MOCK_MODEL_NAME: &str = "mock-runtime";

const MOCK_STREAM_CHANNEL_CAPACITY: usize = 8;
const MOCK_RESPONSE_DELAY: Duration = Duration::from_millis(25);
const MOCK_RESPONSE_CHUNKS: [&str; 3] = [
    "The mock runtime received your message. ",
    "This response is deterministic and streamed in chunks. ",
    "The mock work is complete.",
];
const MOCK_MODEL_LIMITS: ModelLimits = ModelLimits {
    context_window_tokens: 8_192,
    maximum_output_tokens: Some(1_024),
};
const MOCK_USAGE: Usage = Usage {
    input_tokens: 24,
    cached_input_tokens: 8,
    output_tokens: 16,
};

/// A zero-configuration provider with one deterministic model.
#[derive(Debug, Clone, Copy, Default)]
pub struct FakeProvider;

impl FakeProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Provider for FakeProvider {
    fn model_metadata(&self, model_id: &str) -> Result<ModelMetadata, ProviderError> {
        ensure_known_model(model_id)?;

        Ok(mock_model_metadata())
    }

    async fn models(&self) -> Result<Vec<ModelMetadata>, ProviderError> {
        Ok(vec![mock_model_metadata()])
    }

    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ProviderError> {
        ensure_known_model(&request.model_id)?;

        let (model_stream, _producer_completion) = spawn_fake_stream();
        Ok(model_stream)
    }
}

fn mock_model_metadata() -> ModelMetadata {
    ModelMetadata {
        model_id: MOCK_MODEL_NAME.to_owned(),
        display_name: MOCK_MODEL_NAME.to_owned(),
        limits: MOCK_MODEL_LIMITS,
        prompt_price_usd_per_million_tokens: None,
        completion_price_usd_per_million_tokens: None,
    }
}

fn ensure_known_model(model_id: &str) -> Result<(), ProviderError> {
    if model_id == MOCK_MODEL_NAME {
        return Ok(());
    }

    Err(ProviderError::UnknownModel {
        model_id: model_id.to_owned(),
    })
}

fn spawn_fake_stream() -> (ModelStream, oneshot::Receiver<()>) {
    let (event_sender, event_receiver) = mpsc::channel(MOCK_STREAM_CHANNEL_CAPACITY);
    let (completion_sender, completion_receiver) = oneshot::channel();

    std::mem::drop(tokio::spawn(async move {
        let _ = emit_fake_response(&event_sender).await;
        let _ = completion_sender.send(());
    }));

    (event_receiver, completion_receiver)
}

async fn emit_fake_response(
    event_sender: &mpsc::Sender<Result<ModelEvent, ProviderError>>,
) -> Result<(), ()> {
    for (chunk_index, response_chunk) in MOCK_RESPONSE_CHUNKS.iter().enumerate() {
        if chunk_index > 0 {
            tokio::select! {
                _ = tokio::time::sleep(MOCK_RESPONSE_DELAY) => {}
                _ = event_sender.closed() => return Err(()),
            }
        }

        event_sender
            .send(Ok(ModelEvent::TextDelta((*response_chunk).to_owned())))
            .await
            .map_err(|_| ())?;
    }

    event_sender
        .send(Ok(ModelEvent::Usage(MOCK_USAGE)))
        .await
        .map_err(|_| ())?;
    event_sender
        .send(Ok(ModelEvent::Finished(CompletionOutcome::Complete)))
        .await
        .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::{
        FakeProvider, MOCK_MODEL_NAME, MOCK_RESPONSE_CHUNKS, MOCK_RESPONSE_DELAY, MOCK_USAGE,
        mock_model_metadata, spawn_fake_stream,
    };
    use crate::provider::{CompletionOutcome, ModelEvent, ModelRequest, Provider, ProviderError};

    #[tokio::test]
    async fn catalog_and_metadata_return_one_canonical_model_without_pricing() {
        let provider = FakeProvider::new();
        let expected_metadata = mock_model_metadata();

        assert_eq!(
            provider.model_metadata(MOCK_MODEL_NAME),
            Ok(expected_metadata.clone())
        );
        assert_eq!(provider.models().await, Ok(vec![expected_metadata]));
    }

    #[test]
    fn unknown_model_returns_typed_error() {
        let provider = FakeProvider::new();

        assert_eq!(
            provider.model_metadata("missing-model"),
            Err(crate::provider::ProviderError::UnknownModel {
                model_id: "missing-model".to_owned(),
            })
        );
    }

    #[tokio::test(start_paused = true)]
    async fn streams_same_ordered_response_for_different_inputs() {
        let provider = FakeProvider::new();
        let mut first_stream = provider
            .stream(ModelRequest {
                model_id: MOCK_MODEL_NAME.to_owned(),
                input: "first input".to_owned(),
            })
            .await
            .expect("known model should start streaming");
        let first_events = receive_fake_events(&mut first_stream).await;

        let mut second_stream = provider
            .stream(ModelRequest {
                model_id: MOCK_MODEL_NAME.to_owned(),
                input: "different input".to_owned(),
            })
            .await
            .expect("known model should start streaming");
        let second_events = receive_fake_events(&mut second_stream).await;

        let expected_events = vec![
            Ok(ModelEvent::TextDelta(MOCK_RESPONSE_CHUNKS[0].to_owned())),
            Ok(ModelEvent::TextDelta(MOCK_RESPONSE_CHUNKS[1].to_owned())),
            Ok(ModelEvent::TextDelta(MOCK_RESPONSE_CHUNKS[2].to_owned())),
            Ok(ModelEvent::Usage(MOCK_USAGE)),
            Ok(ModelEvent::Finished(CompletionOutcome::Complete)),
        ];

        assert_eq!(first_events, expected_events);
        assert_eq!(second_events, expected_events);
        assert_eq!(first_stream.recv().await, None);
        assert_eq!(second_stream.recv().await, None);
    }

    #[tokio::test]
    async fn stream_rejects_unknown_model() {
        let provider = FakeProvider::new();

        let result = provider
            .stream(ModelRequest {
                model_id: "missing-model".to_owned(),
                input: "input".to_owned(),
            })
            .await;

        assert!(matches!(
            result,
            Err(ProviderError::UnknownModel { model_id }) if model_id == "missing-model"
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn producer_finishes_after_receiver_is_dropped() {
        let (model_stream, producer_completion) = spawn_fake_stream();
        drop(model_stream);
        tokio::task::yield_now().await;

        producer_completion
            .await
            .expect("producer should signal completion after receiver closure");
    }

    async fn receive_fake_events(
        model_stream: &mut crate::provider::ModelStream,
    ) -> Vec<Result<ModelEvent, crate::provider::ProviderError>> {
        let mut events = Vec::new();

        for (chunk_index, expected_chunk) in MOCK_RESPONSE_CHUNKS.iter().enumerate() {
            if chunk_index > 0 {
                tokio::time::advance(MOCK_RESPONSE_DELAY).await;
            }

            let event = model_stream
                .recv()
                .await
                .expect("fake stream should emit every response chunk");
            assert_eq!(
                event,
                Ok(ModelEvent::TextDelta((*expected_chunk).to_owned()))
            );
            events.push(event);
        }

        let usage_event = model_stream
            .recv()
            .await
            .expect("fake stream should emit one usage event");
        assert_eq!(usage_event, Ok(ModelEvent::Usage(MOCK_USAGE)));
        events.push(usage_event);

        let done_event = model_stream
            .recv()
            .await
            .expect("fake stream should emit one terminal event");
        assert_eq!(
            done_event,
            Ok(ModelEvent::Finished(CompletionOutcome::Complete))
        );
        events.push(done_event);

        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    matches!(event, Ok(ModelEvent::Finished(CompletionOutcome::Complete)))
                })
                .count(),
            1
        );
        events
    }
}
