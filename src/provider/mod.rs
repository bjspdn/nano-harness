//! Provider-neutral model contracts and provider adapters.

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;
use tokio::sync::mpsc;

mod fake;

pub use fake::{FakeProvider, MOCK_MODEL_NAME};

/// The input submitted to a provider for one model request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRequest {
    pub model_id: String,
    pub input: String,
}

/// The limits advertised by a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelLimits {
    pub context_window_tokens: u64,
    pub maximum_output_tokens: u64,
}

/// Local metadata describing a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelMetadata {
    pub model_id: String,
    pub display_name: String,
    pub limits: ModelLimits,
}

/// A complete tool call normalized from a provider response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub tool_call_id: String,
    pub tool_name: String,
    pub arguments: Value,
}

/// Cumulative token usage for a model request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
}

/// Normalized events emitted by a provider stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelEvent {
    TextDelta(String),
    #[allow(dead_code)] // Required contract variant; production reasoning adapters are deferred.
    ReasoningDelta(String),
    #[allow(dead_code)] // Required contract variant; production tool adapters are deferred.
    ToolCall(ToolCall),
    Usage(Usage),
    Done,
}

/// The bounded receiver returned by a provider stream.
pub type ModelStream = mpsc::Receiver<Result<ModelEvent, ProviderError>>;

/// Errors returned while setting up or consuming a provider request.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProviderError {
    #[error("unknown model: {model_id}")]
    UnknownModel { model_id: String },
    #[allow(dead_code)] // Required error category; request setup adapters are deferred.
    #[error("request setup failed: {0}")]
    RequestSetup(String),
    #[allow(dead_code)] // Required error category; streaming adapters are deferred.
    #[error("streaming failed: {0}")]
    Streaming(String),
}

/// A provider that supplies model metadata and normalized model events.
#[async_trait]
pub trait Provider: Send + Sync {
    fn model_metadata(&self, model_id: &str) -> Result<ModelMetadata, ProviderError>;

    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ProviderError>;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::{
        FakeProvider, MOCK_MODEL_NAME, ModelEvent, ModelLimits, ModelMetadata, Provider,
        ProviderError, ToolCall, Usage,
    };

    #[test]
    fn normalized_values_preserve_tool_arguments_and_usage() {
        let events = [
            ModelEvent::TextDelta("text".to_owned()),
            ModelEvent::ReasoningDelta("reasoning".to_owned()),
            ModelEvent::ToolCall(ToolCall {
                tool_call_id: "call-1".to_owned(),
                tool_name: "lookup".to_owned(),
                arguments: json!({"query": "rust", "limit": 2}),
            }),
            ModelEvent::Usage(Usage {
                input_tokens: 12,
                cached_input_tokens: 4,
                output_tokens: 8,
            }),
            ModelEvent::Done,
        ];

        assert_eq!(
            events[2],
            ModelEvent::ToolCall(ToolCall {
                tool_call_id: "call-1".to_owned(),
                tool_name: "lookup".to_owned(),
                arguments: json!({"query": "rust", "limit": 2}),
            })
        );
        assert_eq!(
            events[3],
            ModelEvent::Usage(Usage {
                input_tokens: 12,
                cached_input_tokens: 4,
                output_tokens: 8,
            })
        );
        assert_eq!(events[4], ModelEvent::Done);
    }

    #[test]
    fn provider_error_categories_have_readable_context() {
        assert_eq!(
            ProviderError::UnknownModel {
                model_id: "missing-model".to_owned(),
            }
            .to_string(),
            "unknown model: missing-model"
        );
        assert_eq!(
            ProviderError::RequestSetup("invalid request".to_owned()).to_string(),
            "request setup failed: invalid request"
        );
        assert_eq!(
            ProviderError::Streaming("connection closed".to_owned()).to_string(),
            "streaming failed: connection closed"
        );
    }

    #[test]
    fn provider_is_usable_through_a_dyn_provider() {
        let provider: Arc<dyn Provider> = Arc::new(FakeProvider::new());

        assert_eq!(
            provider.model_metadata(MOCK_MODEL_NAME),
            Ok(ModelMetadata {
                model_id: MOCK_MODEL_NAME.to_owned(),
                display_name: MOCK_MODEL_NAME.to_owned(),
                limits: ModelLimits {
                    context_window_tokens: 8_192,
                    maximum_output_tokens: 1_024,
                },
            })
        );
    }
}
