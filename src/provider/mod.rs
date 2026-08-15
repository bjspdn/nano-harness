//! Provider-neutral model contracts and provider adapters.

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;
use tokio::sync::mpsc;

#[cfg(test)]
mod fake;
mod openrouter;

#[cfg(test)]
pub use fake::{FakeProvider, MOCK_MODEL_NAME};
pub use openrouter::{OPENROUTER_DEFAULT_MODEL_ID, OpenRouterProvider};

/// One ordered provider-neutral message in a model request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelMessage {
    User {
        content: String,
    },
    #[allow(dead_code)] // Required contract variant; session history producers are deferred.
    Assistant {
        content: String,
        tool_calls: Vec<ToolCall>,
    },
    #[allow(dead_code)] // Required contract variant; session history producers are deferred.
    ToolResult {
        tool_call_id: String,
        content: String,
    },
}

/// The ordered messages submitted to a provider for one model request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRequest {
    pub model_id: String,
    pub messages: Vec<ModelMessage>,
}

/// The limits advertised by a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelLimits {
    pub context_window_tokens: u64,
    pub maximum_output_tokens: Option<u64>,
}

/// Local metadata describing a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelMetadata {
    pub model_id: String,
    pub display_name: String,
    pub limits: ModelLimits,
    pub prompt_price_usd_per_million_tokens: Option<String>,
    pub completion_price_usd_per_million_tokens: Option<String>,
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

/// The normalized reason a provider stream finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionOutcome {
    Complete,
    LengthLimited,
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
    Finished(CompletionOutcome),
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

    async fn models(&self) -> Result<Vec<ModelMetadata>, ProviderError>;

    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ProviderError>;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::{
        CompletionOutcome, FakeProvider, MOCK_MODEL_NAME, ModelEvent, ModelLimits, ModelMessage,
        ModelMetadata, ModelRequest, Provider, ProviderError, ToolCall, Usage,
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
            ModelEvent::Finished(CompletionOutcome::Complete),
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
        assert_eq!(events[4], ModelEvent::Finished(CompletionOutcome::Complete));
        assert_ne!(
            ModelEvent::Finished(CompletionOutcome::Complete),
            ModelEvent::Finished(CompletionOutcome::LengthLimited)
        );
    }

    #[test]
    fn model_request_preserves_ordered_neutral_message_data() {
        let request = ModelRequest {
            model_id: "provider/model".to_owned(),
            messages: vec![
                ModelMessage::User {
                    content: "find a Rust book".to_owned(),
                },
                ModelMessage::Assistant {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        tool_call_id: "call-1".to_owned(),
                        tool_name: "search".to_owned(),
                        arguments: json!({"query": "Rust"}),
                    }],
                },
                ModelMessage::ToolResult {
                    tool_call_id: "call-1".to_owned(),
                    content: "Rust book found".to_owned(),
                },
                ModelMessage::User {
                    content: "summarize it".to_owned(),
                },
            ],
        };

        assert_eq!(
            request.messages,
            vec![
                ModelMessage::User {
                    content: "find a Rust book".to_owned(),
                },
                ModelMessage::Assistant {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        tool_call_id: "call-1".to_owned(),
                        tool_name: "search".to_owned(),
                        arguments: json!({"query": "Rust"}),
                    }],
                },
                ModelMessage::ToolResult {
                    tool_call_id: "call-1".to_owned(),
                    content: "Rust book found".to_owned(),
                },
                ModelMessage::User {
                    content: "summarize it".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn model_metadata_preserves_optional_normalized_pricing() {
        let metadata = ModelMetadata {
            model_id: "priced-model".to_owned(),
            display_name: "Priced model".to_owned(),
            limits: ModelLimits {
                context_window_tokens: 16_384,
                maximum_output_tokens: Some(2_048),
            },
            prompt_price_usd_per_million_tokens: Some("0.15".to_owned()),
            completion_price_usd_per_million_tokens: Some("0.60".to_owned()),
        };

        assert_eq!(
            metadata,
            ModelMetadata {
                model_id: "priced-model".to_owned(),
                display_name: "Priced model".to_owned(),
                limits: ModelLimits {
                    context_window_tokens: 16_384,
                    maximum_output_tokens: Some(2_048),
                },
                prompt_price_usd_per_million_tokens: Some("0.15".to_owned()),
                completion_price_usd_per_million_tokens: Some("0.60".to_owned()),
            }
        );
    }

    #[test]
    fn model_limits_distinguish_known_and_unknown_output_limits() {
        let known_limits = ModelLimits {
            context_window_tokens: 16_384,
            maximum_output_tokens: Some(2_048),
        };
        let unknown_limits = ModelLimits {
            context_window_tokens: 16_384,
            maximum_output_tokens: None,
        };

        assert_ne!(known_limits, unknown_limits);
        assert_eq!(known_limits.maximum_output_tokens, Some(2_048));
        assert_eq!(unknown_limits.maximum_output_tokens, None);
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
                    maximum_output_tokens: Some(1_024),
                },
                prompt_price_usd_per_million_tokens: None,
                completion_price_usd_per_million_tokens: None,
            })
        );
    }

    #[tokio::test]
    async fn provider_catalog_is_usable_through_a_dyn_provider() {
        let provider: Arc<dyn Provider> = Arc::new(FakeProvider::new());

        assert_eq!(
            provider.models().await,
            Ok(vec![ModelMetadata {
                model_id: MOCK_MODEL_NAME.to_owned(),
                display_name: MOCK_MODEL_NAME.to_owned(),
                limits: ModelLimits {
                    context_window_tokens: 8_192,
                    maximum_output_tokens: Some(1_024),
                },
                prompt_price_usd_per_million_tokens: None,
                completion_price_usd_per_million_tokens: None,
            }])
        );
    }
}
