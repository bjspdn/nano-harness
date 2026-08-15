mod auth;
mod wire;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use tokio::sync::mpsc;

use super::{
    ModelEvent, ModelLimits, ModelMetadata, ModelRequest, ModelStream, Provider, ProviderError,
    Usage,
};

const OPENROUTER_API_BASE_URL: &str = "https://openrouter.ai/api/v1/";
pub const OPENROUTER_DEFAULT_MODEL_ID: &str = "deepseek/deepseek-v4-flash-0731";
const OPENROUTER_DEFAULT_MODEL_DISPLAY_NAME: &str = "DeepSeek: DeepSeek V4 Flash 0731";
const OPENROUTER_DEFAULT_CONTEXT_WINDOW_TOKENS: u64 = 1_048_576;
const OPENROUTER_DEFAULT_MAXIMUM_OUTPUT_TOKENS: u64 = 393_216;

const OPENROUTER_CHAT_PATH: &str = "chat/completions";
const OPENROUTER_MODELS_PATH: &str = "models";
const OPENROUTER_USER_MODELS_PATH: &str = "models/user";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const STREAM_CHANNEL_CAPACITY: usize = 16;

type CredentialResolver = Arc<dyn Fn() -> Result<String, String> + Send + Sync>;

#[derive(Debug, Clone, Default)]
struct ResponseContext {
    retry_after: Option<String>,
    generation_id: Option<String>,
}

/// The thin OpenRouter HTTP provider adapter.
pub struct OpenRouterProvider {
    client: reqwest::Client,
    base_url: reqwest::Url,
    credential_resolver: CredentialResolver,
}

impl std::fmt::Debug for OpenRouterProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenRouterProvider")
            .finish_non_exhaustive()
    }
}

impl OpenRouterProvider {
    /// Construct the production provider using the official OpenRouter endpoints.
    pub fn new() -> Result<Self, ProviderError> {
        Self::from_parts(
            reqwest::Url::parse(OPENROUTER_API_BASE_URL)
                .expect("the production OpenRouter base URL is valid"),
            Arc::new(auth::resolve_api_key),
        )
    }

    pub fn default_model_metadata() -> ModelMetadata {
        ModelMetadata {
            model_id: OPENROUTER_DEFAULT_MODEL_ID.to_owned(),
            display_name: OPENROUTER_DEFAULT_MODEL_DISPLAY_NAME.to_owned(),
            limits: ModelLimits {
                context_window_tokens: OPENROUTER_DEFAULT_CONTEXT_WINDOW_TOKENS,
                maximum_output_tokens: Some(OPENROUTER_DEFAULT_MAXIMUM_OUTPUT_TOKENS),
            },
            prompt_price_usd_per_million_tokens: None,
            completion_price_usd_per_million_tokens: None,
        }
    }

    fn from_parts(
        base_url: reqwest::Url,
        credential_resolver: CredentialResolver,
    ) -> Result<Self, ProviderError> {
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .retry(reqwest::retry::never())
            .build()
            .map_err(|error| {
                ProviderError::RequestSetup(format!(
                    "unable to create the OpenRouter HTTP client: {error}"
                ))
            })?;

        Ok(Self {
            client,
            base_url,
            credential_resolver,
        })
    }

    #[cfg(test)]
    fn new_for_tests(
        base_url: reqwest::Url,
        credential_resolver: CredentialResolver,
    ) -> Result<Self, ProviderError> {
        Self::from_parts(base_url, credential_resolver)
    }

    async fn request_models_page(
        &self,
        page_url: &reqwest::Url,
        api_key: Option<&str>,
    ) -> Result<(Vec<ModelMetadata>, Option<String>), ProviderError> {
        let mut request = self.client.get(page_url.clone());
        if let Some(api_key) = api_key {
            request = request.header("Authorization", openrouter_authorization_header(api_key)?);
        }

        let response = request.send().await.map_err(|error| {
            ProviderError::RequestSetup(format!("OpenRouter model catalog request failed: {error}"))
        })?;
        let response_context = response_context(response.headers());
        let status = response.status();
        let response_body = response.text().await.map_err(|error| {
            ProviderError::RequestSetup(format!(
                "unable to read the OpenRouter model catalog response: {error}"
            ))
        })?;

        if !status.is_success() {
            return Err(ProviderError::RequestSetup(format_http_error(
                status.as_u16(),
                &response_body,
                &response_context,
                false,
                None,
            )));
        }

        wire::parse_catalog_page(&response_body).map_err(ProviderError::RequestSetup)
    }

    async fn create_generation_response(
        &self,
        request: &ModelRequest,
    ) -> Result<(reqwest::Response, String, ResponseContext), ProviderError> {
        let api_key = (self.credential_resolver)().map_err(ProviderError::RequestSetup)?;
        let request_body = wire::generation_request_body(&request.model_id, &request.messages)
            .map_err(ProviderError::RequestSetup)?;
        let authorization_header = openrouter_authorization_header(&api_key)?;

        let response = self
            .client
            .post(self.base_url.join(OPENROUTER_CHAT_PATH).map_err(|error| {
                ProviderError::RequestSetup(format!(
                    "unable to build the OpenRouter generation URL: {error}"
                ))
            })?)
            .header("Authorization", authorization_header)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .header(
                HeaderName::from_static("x-openrouter-title"),
                HeaderValue::from_static("nano"),
            )
            .header(
                HeaderName::from_static("x-openrouter-categories"),
                HeaderValue::from_static("cli-agent"),
            )
            .body(request_body)
            .send()
            .await
            .map_err(|error| {
                ProviderError::RequestSetup(format!(
                    "OpenRouter generation request failed: {error}"
                ))
            })?;

        let response_context = response_context(response.headers());
        trace_generation_id(&response_context, &api_key);
        if !response.status().is_success() {
            let status = response.status();
            let response_body = response.text().await.map_err(|error| {
                ProviderError::RequestSetup(format!(
                    "unable to read the OpenRouter generation error response: {error}"
                ))
            })?;
            return Err(ProviderError::RequestSetup(format_http_error(
                status.as_u16(),
                &response_body,
                &response_context,
                true,
                Some(&api_key),
            )));
        }

        Ok((response, api_key, response_context))
    }
}

#[async_trait]
impl Provider for OpenRouterProvider {
    fn model_metadata(&self, model_id: &str) -> Result<ModelMetadata, ProviderError> {
        if model_id == OPENROUTER_DEFAULT_MODEL_ID {
            return Ok(Self::default_model_metadata());
        }

        Err(ProviderError::UnknownModel {
            model_id: model_id.to_owned(),
        })
    }

    async fn models(&self) -> Result<Vec<ModelMetadata>, ProviderError> {
        let (catalog_path, api_key) = match (self.credential_resolver)() {
            Ok(api_key) => (OPENROUTER_USER_MODELS_PATH, Some(api_key)),
            Err(_) => (OPENROUTER_MODELS_PATH, None),
        };
        let mut next_url = self.base_url.join(catalog_path).map_err(|error| {
            ProviderError::RequestSetup(format!(
                "unable to build the OpenRouter model catalog URL: {error}"
            ))
        })?;
        let mut models = Vec::new();

        loop {
            let (page_models, next_link) = self
                .request_models_page(&next_url, api_key.as_deref())
                .await?;
            models.extend(page_models);

            let Some(next_link) = next_link else {
                return Ok(models);
            };
            next_url = resolve_catalog_next_url(&next_url, &next_link)
                .map_err(ProviderError::RequestSetup)?;
        }
    }

    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ProviderError> {
        let (response, api_key, response_context) =
            self.create_generation_response(&request).await?;
        let (event_sender, event_receiver) = mpsc::channel(STREAM_CHANNEL_CAPACITY);

        std::mem::drop(tokio::spawn(consume_generation_response(
            response,
            event_sender,
            api_key,
            response_context,
        )));

        Ok(event_receiver)
    }
}

fn resolve_catalog_next_url(
    current_url: &reqwest::Url,
    next_link: &str,
) -> Result<reqwest::Url, String> {
    let resolved_url = current_url.join(next_link).map_err(|error| {
        format!("the OpenRouter model catalog contains an invalid next link: {error}")
    })?;
    if !has_same_catalog_origin(current_url, &resolved_url) {
        return Err(
            "the OpenRouter model catalog contains a next link with a different origin".to_owned(),
        );
    }

    Ok(resolved_url)
}

fn has_same_catalog_origin(current_url: &reqwest::Url, next_url: &reqwest::Url) -> bool {
    current_url.scheme() == next_url.scheme()
        && current_url.host_str() == next_url.host_str()
        && current_url.port_or_known_default() == next_url.port_or_known_default()
}

fn response_context(headers: &HeaderMap) -> ResponseContext {
    ResponseContext {
        retry_after: header_value(headers, "retry-after"),
        generation_id: header_value(headers, "x-generation-id"),
    }
}

fn openrouter_authorization_header(api_key: &str) -> Result<HeaderValue, ProviderError> {
    let mut authorization_header = HeaderValue::from_str(&format!("Bearer {api_key}"))
        .map_err(|_| ProviderError::RequestSetup("the OpenRouter API key is invalid".to_owned()))?;
    authorization_header.set_sensitive(true);
    Ok(authorization_header)
}

fn header_value(headers: &HeaderMap, name: &'static str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn trace_generation_id(response_context: &ResponseContext, api_key: &str) {
    if let Some(generation_id) = response_context.generation_id.as_deref() {
        let generation_id = redact_secret(generation_id, api_key);
        tracing::debug!(generation_id = %generation_id, "OpenRouter generation response received");
    }
}

fn format_http_error(
    status: u16,
    response_body: &str,
    response_context: &ResponseContext,
    model_request: bool,
    api_key: Option<&str>,
) -> String {
    let mut message =
        wire::api_error_message(response_body).unwrap_or_else(|| format!("HTTP status {status}"));
    if let Some(api_key) = api_key {
        message = redact_secret(&message, api_key);
    }

    if let Some(retry_after) = response_context.retry_after.as_deref() {
        let retry_after = api_key
            .map(|api_key| redact_secret(retry_after, api_key))
            .unwrap_or_else(|| retry_after.to_owned());
        message.push_str(&format!("; retry after {retry_after}"));
    }
    if let Some(generation_id) = response_context.generation_id.as_deref() {
        let generation_id = api_key
            .map(|api_key| redact_secret(generation_id, api_key))
            .unwrap_or_else(|| generation_id.to_owned());
        message.push_str(&format!("; generation id {generation_id}"));
    }
    if model_request && is_model_selection_error(status, &message) {
        message.push_str("; open Ctrl-P and choose another model");
    }

    message
}

fn is_model_selection_error(status: u16, message: &str) -> bool {
    let lowercase_message = message.to_ascii_lowercase();
    status == 404
        || (status == 400 && lowercase_message.contains("model"))
        || (status == 503 && is_provider_routing_availability_error(&lowercase_message))
}

fn is_provider_routing_availability_error(lowercase_message: &str) -> bool {
    lowercase_message.contains("no allowed providers are available")
        || lowercase_message
            .contains("no available model provider that meets your routing requirements")
}

fn format_stream_error(message: &str, response_context: &ResponseContext, api_key: &str) -> String {
    let mut message = redact_secret(message, api_key);
    if let Some(retry_after) = response_context.retry_after.as_deref() {
        message.push_str(&format!(
            "; retry after {}",
            redact_secret(retry_after, api_key)
        ));
    }
    if let Some(generation_id) = response_context.generation_id.as_deref() {
        message.push_str(&format!(
            "; generation id {}",
            redact_secret(generation_id, api_key)
        ));
    }
    message
}

fn redact_secret(message: &str, api_key: &str) -> String {
    if api_key.is_empty() {
        return message.to_owned();
    }

    message.replace(api_key, "[redacted]")
}

async fn consume_generation_response(
    response: reqwest::Response,
    event_sender: mpsc::Sender<Result<ModelEvent, ProviderError>>,
    api_key: String,
    response_context: ResponseContext,
) {
    let event_stream = response.bytes_stream().eventsource();
    tokio::pin!(event_stream);
    let mut completion_outcome = None;
    let mut latest_usage: Option<Usage> = None;

    loop {
        let next_event = tokio::select! {
            _ = event_sender.closed() => return,
            next_event = event_stream.next() => next_event,
        };
        let Some(next_event) = next_event else {
            let _ = send_stream_error(
                &event_sender,
                format_stream_error(
                    "OpenRouter stream ended before [DONE]",
                    &response_context,
                    &api_key,
                ),
            )
            .await;
            return;
        };

        let event = match next_event {
            Ok(event) => event,
            Err(_error) => {
                let _ = send_stream_error(
                    &event_sender,
                    format_stream_error(
                        "OpenRouter SSE transport or parser error",
                        &response_context,
                        &api_key,
                    ),
                )
                .await;
                tracing::debug!("OpenRouter SSE stream stopped");
                return;
            }
        };

        let parsed_event = match wire::parse_sse_data(&event.data) {
            Ok(parsed_event) => parsed_event,
            Err(error) => {
                let _ = send_stream_error(
                    &event_sender,
                    format_stream_error(&error, &response_context, &api_key),
                )
                .await;
                return;
            }
        };

        match parsed_event {
            wire::ParsedSseEvent::Done => {
                let Some(completion_outcome) = completion_outcome else {
                    let _ = send_stream_error(
                        &event_sender,
                        format_stream_error(
                            "OpenRouter stream ended with [DONE] but no completion outcome",
                            &response_context,
                            &api_key,
                        ),
                    )
                    .await;
                    return;
                };

                if let Some(usage) = latest_usage
                    && !send_model_event(&event_sender, ModelEvent::Usage(usage)).await
                {
                    return;
                }
                let _ =
                    send_model_event(&event_sender, ModelEvent::Finished(completion_outcome)).await;
                return;
            }
            wire::ParsedSseEvent::Error(error) => {
                let _ = send_stream_error(
                    &event_sender,
                    format_stream_error(&error, &response_context, &api_key),
                )
                .await;
                return;
            }
            wire::ParsedSseEvent::Chunk(chunk) => {
                if let Some(finish_reason) = chunk.finish_reason {
                    if let Some(outcome) = wire::completion_outcome(finish_reason) {
                        if completion_outcome.is_some_and(|current| current != outcome) {
                            let _ = send_stream_error(
                                &event_sender,
                                format_stream_error(
                                    "OpenRouter stream returned conflicting completion outcomes",
                                    &response_context,
                                    &api_key,
                                ),
                            )
                            .await;
                            return;
                        }
                        completion_outcome = Some(outcome);
                    } else {
                        let _ = send_stream_error(
                            &event_sender,
                            format_stream_error(
                                wire::finish_reason_error(finish_reason),
                                &response_context,
                                &api_key,
                            ),
                        )
                        .await;
                        return;
                    }
                }

                for text_delta in chunk.text_deltas {
                    if !send_model_event(&event_sender, ModelEvent::TextDelta(text_delta)).await {
                        return;
                    }
                }
                if chunk.usage.is_some() {
                    latest_usage = chunk.usage;
                }
            }
        }
    }
}

async fn send_model_event(
    event_sender: &mpsc::Sender<Result<ModelEvent, ProviderError>>,
    event: ModelEvent,
) -> bool {
    event_sender.send(Ok(event)).await.is_ok()
}

async fn send_stream_error(
    event_sender: &mpsc::Sender<Result<ModelEvent, ProviderError>>,
    message: String,
) -> bool {
    event_sender
        .send(Err(ProviderError::Streaming(message)))
        .await
        .is_ok()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::oneshot;

    use super::{
        CredentialResolver, OPENROUTER_DEFAULT_MODEL_ID, OpenRouterProvider, ResponseContext,
        format_http_error, is_model_selection_error, resolve_catalog_next_url,
    };
    use crate::provider::{
        CompletionOutcome, ModelEvent, ModelMessage, ModelRequest, Provider, ProviderError,
        ToolCall, Usage,
    };

    #[derive(Debug, Clone)]
    struct ResponseSpec {
        status: u16,
        headers: Vec<(&'static str, &'static str)>,
        body: &'static str,
        hold_connection_open: bool,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct CapturedRequest {
        method: String,
        path: String,
        headers: HashMap<String, String>,
        body: String,
    }

    impl ResponseSpec {
        fn json(body: &'static str) -> Self {
            Self {
                status: 200,
                headers: vec![("content-type", "application/json")],
                body,
                hold_connection_open: false,
            }
        }

        fn sse(body: &'static str) -> Self {
            Self {
                status: 200,
                headers: vec![("content-type", "text/event-stream")],
                body,
                hold_connection_open: false,
            }
        }
    }

    #[tokio::test]
    async fn default_metadata_is_sync_and_only_default_id_is_known() {
        let provider = test_provider(
            "http://127.0.0.1:1/",
            Arc::new(|| Ok("unused-secret".to_owned())),
        );

        assert_eq!(
            provider
                .model_metadata(OPENROUTER_DEFAULT_MODEL_ID)
                .expect("default metadata should not use the network"),
            OpenRouterProvider::default_model_metadata()
        );
        assert!(matches!(
            provider.model_metadata("live/model"),
            Err(ProviderError::UnknownModel { .. })
        ));
    }

    #[test]
    fn resolves_same_origin_catalog_links_and_rejects_cross_origin_links() {
        let current_url = reqwest::Url::parse("https://openrouter.example/api/v1/models?offset=0")
            .expect("current catalog URL should parse");
        let expected_root_relative_url =
            reqwest::Url::parse("https://openrouter.example/api/v1/models?offset=500&limit=500")
                .expect("root-relative catalog URL should parse");
        let expected_absolute_url = reqwest::Url::parse(
            "https://openrouter.example:443/api/v1/models?offset=1000&limit=500",
        )
        .expect("absolute catalog URL should parse");
        let expected_path_relative_url =
            reqwest::Url::parse("https://openrouter.example/api/v1/models?offset=1500")
                .expect("path-relative catalog URL should parse");
        let expected_query_relative_url =
            reqwest::Url::parse("https://openrouter.example/api/v1/models?offset=2000")
                .expect("query-relative catalog URL should parse");

        assert_eq!(
            resolve_catalog_next_url(&current_url, "/api/v1/models?offset=500&limit=500")
                .expect("root-relative link should resolve"),
            expected_root_relative_url
        );
        assert_eq!(
            resolve_catalog_next_url(&current_url, expected_absolute_url.as_str())
                .expect("absolute link should resolve"),
            expected_absolute_url
        );
        assert_eq!(
            resolve_catalog_next_url(&current_url, "models?offset=1500")
                .expect("path-relative link should resolve"),
            expected_path_relative_url
        );
        assert_eq!(
            resolve_catalog_next_url(&current_url, "?offset=2000")
                .expect("query-relative link should resolve"),
            expected_query_relative_url
        );

        for cross_origin_link in [
            "https://other.example/api/v1/models",
            "http://openrouter.example/api/v1/models",
            "https://openrouter.example:444/api/v1/models",
        ] {
            let error = resolve_catalog_next_url(&current_url, cross_origin_link)
                .expect_err("cross-origin link should be rejected");
            assert!(error.contains("different origin"));
        }
    }

    #[tokio::test]
    async fn generation_request_serializes_ordered_messages_and_no_provider_state() {
        let (base_url, request_receiver, server_handle) = spawn_fixture(vec![ResponseSpec::sse(
            r#"data: {"choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}]}

data: [DONE]

"#,
        )])
        .await;
        let provider = test_provider(&base_url, Arc::new(|| Ok("test-secret".to_owned())));

        let mut stream = provider
            .stream(ModelRequest {
                model_id: "provider/model".to_owned(),
                messages: vec![
                    ModelMessage::User {
                        content: "hello".to_owned(),
                    },
                    ModelMessage::Assistant {
                        content: String::new(),
                        tool_calls: vec![ToolCall {
                            tool_call_id: "call-1".to_owned(),
                            tool_name: "lookup".to_owned(),
                            arguments: serde_json::json!({"query": "rust"}),
                        }],
                    },
                    ModelMessage::ToolResult {
                        tool_call_id: "call-1".to_owned(),
                        content: "lookup result".to_owned(),
                    },
                    ModelMessage::User {
                        content: "follow up".to_owned(),
                    },
                ],
            })
            .await
            .expect("generation should start");
        let _ = receive_all(&mut stream).await;

        let requests = request_receiver
            .await
            .expect("fixture should capture request");
        let request = requests
            .into_iter()
            .next()
            .expect("one request should exist");
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/v1/chat/completions");
        assert_eq!(
            request.body,
            r#"{"model":"provider/model","messages":[{"role":"user","content":"hello"},{"role":"assistant","content":"","tool_calls":[{"id":"call-1","type":"function","function":{"name":"lookup","arguments":"{\"query\":\"rust\"}"}}]},{"role":"tool","tool_call_id":"call-1","content":"lookup result"},{"role":"user","content":"follow up"}],"stream":true}"#
        );
        assert_eq!(
            request.headers.get("authorization"),
            Some(&"Bearer test-secret".to_owned())
        );
        assert_eq!(
            request.headers.get("content-type"),
            Some(&"application/json".to_owned())
        );
        assert_eq!(
            request.headers.get("x-openrouter-title"),
            Some(&"nano".to_owned())
        );
        assert_eq!(
            request.headers.get("x-openrouter-categories"),
            Some(&"cli-agent".to_owned())
        );
        assert!(!request.body.contains("tools"));
        assert!(!request.body.contains("history"));
        assert!(!request.body.contains("cache"));
        assert!(!request.body.contains("session"));
        server_handle.await.expect("fixture should finish");
    }

    #[tokio::test]
    async fn catalog_uses_public_fallback_without_credentials_and_normalizes_models() {
        let (base_url, request_receiver, server_handle) = spawn_fixture(vec![
            ResponseSpec::json(
                r#"{"data":[{"id":"first/model","name":"First","context_length":100,"top_provider":{"max_completion_tokens":20},"pricing":{"prompt":"0.000001","completion":"0.000002"}}],"links":{"next":"/api/v1/models?page=2"}}"#,
            ),
            ResponseSpec::json(
                r#"{"data":[{"id":"second/model","name":"Second","context_length":200,"top_provider":{"max_completion_tokens":null}},{"id":"third/model","name":"Third","context_length":300,"top_provider":{},"pricing":{"prompt":"0.000001"}}],"links":{"next":null}}"#,
            ),
        ])
        .await;
        let provider = test_provider(
            &base_url,
            Arc::new(|| Err("no OpenRouter API key".to_owned())),
        );

        let models = provider.models().await.expect("catalog should load");
        assert_eq!(models.len(), 3);
        assert_eq!(
            models[0].prompt_price_usd_per_million_tokens,
            Some("1".to_owned())
        );
        assert_eq!(
            models[0].completion_price_usd_per_million_tokens,
            Some("2".to_owned())
        );
        assert_eq!(models[1].prompt_price_usd_per_million_tokens, None);
        assert_eq!(models[1].completion_price_usd_per_million_tokens, None);
        assert_eq!(
            models[2].prompt_price_usd_per_million_tokens,
            Some("1".to_owned())
        );
        assert_eq!(models[2].completion_price_usd_per_million_tokens, None);
        assert_eq!(models[0].limits.maximum_output_tokens, Some(20));
        assert_eq!(models[1].limits.maximum_output_tokens, None);
        assert_eq!(models[2].limits.maximum_output_tokens, None);

        let requests = request_receiver
            .await
            .expect("fixture should capture requests");
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].path, "/api/v1/models");
        assert_eq!(requests[1].path, "/api/v1/models?page=2");
        assert!(!requests[0].headers.contains_key("authorization"));
        assert!(!requests[1].headers.contains_key("authorization"));
        server_handle.await.expect("fixture should finish");
    }

    #[tokio::test]
    async fn authenticated_catalog_uses_user_endpoint_and_retains_auth_for_pagination() {
        let (base_url, request_receiver, server_handle) = spawn_fixture(vec![
            ResponseSpec::json(
                r#"{"data":[{"id":"user/first","name":"User First","context_length":100,"top_provider":{"max_completion_tokens":20}}],"links":{"next":"/api/v1/models/user?page=2"}}"#,
            ),
            ResponseSpec::json(
                r#"{"data":[{"id":"user/second","name":"User Second","context_length":200,"top_provider":{"max_completion_tokens":30}}],"links":{"next":null}}"#,
            ),
        ])
        .await;
        let provider = test_provider(&base_url, Arc::new(|| Ok("user-secret".to_owned())));

        let models = provider
            .models()
            .await
            .expect("authenticated catalog should load");

        assert_eq!(
            models
                .iter()
                .map(|model| model.model_id.as_str())
                .collect::<Vec<_>>(),
            ["user/first", "user/second"]
        );
        let requests = request_receiver
            .await
            .expect("fixture should capture requests");
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].path, "/api/v1/models/user");
        assert_eq!(requests[1].path, "/api/v1/models/user?page=2");
        assert_eq!(
            requests[0].headers.get("authorization"),
            Some(&"Bearer user-secret".to_owned())
        );
        assert_eq!(
            requests[1].headers.get("authorization"),
            Some(&"Bearer user-secret".to_owned())
        );
        server_handle.await.expect("fixture should finish");
    }

    #[tokio::test]
    async fn authenticated_catalog_failure_does_not_fall_back_to_public_catalog() {
        let (base_url, request_receiver, server_handle) = spawn_fixture(vec![ResponseSpec {
            status: 503,
            headers: Vec::new(),
            body: r#"{"error":{"message":"No allowed providers are available for this model"}}"#,
            hold_connection_open: false,
        }])
        .await;
        let provider = test_provider(&base_url, Arc::new(|| Ok("user-secret".to_owned())));

        let catalog_error = provider
            .models()
            .await
            .expect_err("authenticated catalog failure should be returned");

        assert!(
            catalog_error
                .to_string()
                .contains("No allowed providers are available for this model")
        );
        let requests = request_receiver
            .await
            .expect("fixture should capture the request");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path, "/api/v1/models/user");
        assert_eq!(
            requests[0].headers.get("authorization"),
            Some(&"Bearer user-secret".to_owned())
        );
        server_handle.await.expect("fixture should finish");
    }

    #[test]
    fn model_selection_hint_only_matches_openrouter_provider_routing_503s() {
        let provider_routing_message = "No allowed providers are available for this model";
        assert!(is_model_selection_error(503, provider_routing_message));
        assert!(is_model_selection_error(
            503,
            "There is no available model provider that meets your routing requirements"
        ));
        assert!(!is_model_selection_error(
            503,
            "OpenRouter is temporarily unavailable"
        ));
        assert!(!is_model_selection_error(
            503,
            "The model service is temporarily unavailable"
        ));

        let formatted_error = format_http_error(
            503,
            r#"{"error":{"message":"No allowed providers are available for this model"}}"#,
            &ResponseContext::default(),
            true,
            None,
        );
        assert!(formatted_error.contains("open Ctrl-P and choose another model"));
    }

    #[tokio::test]
    async fn sse_handles_fragmented_comments_multiline_usage_and_length_completion() {
        let body = r#": comment

data: {
data: "choices":[{"delta":{"content":"hel"}}]
data: }

data: {"choices":[{"delta":{"content":"lo"}}]}

data: {"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":4}}

data: {"choices":[{"delta":{},"finish_reason":"length"}]}

data: {"choices":[{"delta":{},"finish_reason":"length"}]}

data: [DONE]

"#;
        let (base_url, _request_receiver, server_handle) =
            spawn_fixture(vec![ResponseSpec::sse(body)]).await;
        let provider = test_provider(&base_url, Arc::new(|| Ok("test-secret".to_owned())));
        let mut stream = provider
            .stream(ModelRequest {
                model_id: "provider/model".to_owned(),
                messages: vec![ModelMessage::User {
                    content: "input".to_owned(),
                }],
            })
            .await
            .expect("generation should start");

        let events = receive_all(&mut stream).await;
        assert_eq!(
            events,
            vec![
                Ok(ModelEvent::TextDelta("hel".to_owned())),
                Ok(ModelEvent::TextDelta("lo".to_owned())),
                Ok(ModelEvent::Usage(Usage {
                    input_tokens: 10,
                    cached_input_tokens: 0,
                    output_tokens: 4,
                })),
                Ok(ModelEvent::Finished(CompletionOutcome::LengthLimited)),
            ]
        );
        server_handle.await.expect("fixture should finish");
    }

    #[tokio::test]
    async fn pre_stream_and_mid_stream_errors_are_safe_and_do_not_finish() {
        let (base_url, _request_receiver, server_handle) = spawn_fixture(vec![ResponseSpec {
            status: 400,
            headers: vec![("retry-after", "7"), ("x-generation-id", "generation-1")],
            body: r#"{"error":{"message":"model is unavailable"}}"#,
            hold_connection_open: false,
        }])
        .await;
        let provider = test_provider(&base_url, Arc::new(|| Ok("super-secret".to_owned())));
        let setup_error = provider
            .stream(ModelRequest {
                model_id: "missing/model".to_owned(),
                messages: vec![ModelMessage::User {
                    content: "input".to_owned(),
                }],
            })
            .await
            .expect_err("HTTP failure should be setup error");
        let setup_error = setup_error.to_string();
        assert!(setup_error.contains("model is unavailable"));
        assert!(setup_error.contains("retry after 7"));
        assert!(setup_error.contains("generation-1"));
        assert!(setup_error.contains("Ctrl-P"));
        assert!(!setup_error.contains("super-secret"));
        server_handle.await.expect("fixture should finish");

        let (base_url, _request_receiver, server_handle) = spawn_fixture(vec![ResponseSpec::sse(
            r#"data: {"error":{"message":"stream failed"}}

"#,
        )])
        .await;
        let provider = test_provider(&base_url, Arc::new(|| Ok("super-secret".to_owned())));
        let mut stream = provider
            .stream(ModelRequest {
                model_id: "provider/model".to_owned(),
                messages: vec![ModelMessage::User {
                    content: "input".to_owned(),
                }],
            })
            .await
            .expect("generation should start");
        let events = receive_all(&mut stream).await;
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Err(ProviderError::Streaming(_))));
        assert!(
            !events[0]
                .as_ref()
                .expect_err("expected error")
                .to_string()
                .contains("super-secret")
        );
        server_handle.await.expect("fixture should finish");
    }

    #[tokio::test]
    async fn malformed_incomplete_and_tool_streams_fail_without_finished() {
        for body in [
            "data: not-json\n\n",
            r#"data: {"choices":[{"delta":{"tool_calls":[{}]}}]}

data: [DONE]

"#,
            r#"data: {"choices":[{"delta":{"content":"partial"}}]}

"#,
            "data: [DONE]\n\n",
            r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}

data: {"choices":[{"delta":{},"finish_reason":"length"}]}

data: [DONE]

"#,
        ] {
            let (base_url, _request_receiver, server_handle) =
                spawn_fixture(vec![ResponseSpec::sse(body)]).await;
            let provider = test_provider(&base_url, Arc::new(|| Ok("test-secret".to_owned())));
            let mut stream = provider
                .stream(ModelRequest {
                    model_id: "provider/model".to_owned(),
                    messages: vec![ModelMessage::User {
                        content: "input".to_owned(),
                    }],
                })
                .await
                .expect("generation should start");
            let events = receive_all(&mut stream).await;
            assert!(events.iter().any(|event| event.is_err()));
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, Ok(ModelEvent::Finished(_))))
            );
            server_handle.await.expect("fixture should finish");
        }
    }

    #[tokio::test]
    async fn dropping_model_stream_closes_the_http_body() {
        let (listener, base_url) = bind_fixture().await;
        let (body_closed_sender, body_closed_receiver) = oneshot::channel();
        let server_handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("fixture should accept");
            let _request = read_request(&mut socket)
                .await
                .expect("request should be readable");
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
                )
                .await
                .expect("fixture should write initial event");
            let mut buffer = [0_u8; 1];
            let _ = socket.read(&mut buffer).await;
            let _ = body_closed_sender.send(());
        });
        let provider = test_provider(&base_url, Arc::new(|| Ok("test-secret".to_owned())));
        let mut stream = provider
            .stream(ModelRequest {
                model_id: "provider/model".to_owned(),
                messages: vec![ModelMessage::User {
                    content: "input".to_owned(),
                }],
            })
            .await
            .expect("generation should start");
        let _ = stream.recv().await.expect("partial text should arrive");
        drop(stream);

        tokio::time::timeout(Duration::from_secs(1), body_closed_receiver)
            .await
            .expect("body should close promptly")
            .expect("fixture should observe body closure");
        server_handle.await.expect("fixture should finish");
    }

    fn test_provider(
        base_url: &str,
        credential_resolver: CredentialResolver,
    ) -> OpenRouterProvider {
        OpenRouterProvider::new_for_tests(
            reqwest::Url::parse(base_url).expect("fixture URL should parse"),
            credential_resolver,
        )
        .expect("test provider should construct")
    }

    async fn receive_all(
        stream: &mut crate::provider::ModelStream,
    ) -> Vec<Result<ModelEvent, ProviderError>> {
        let mut events = Vec::new();
        while let Some(event) = stream.recv().await {
            events.push(event);
        }
        events
    }

    async fn bind_fixture() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fixture should bind");
        let address = listener.local_addr().expect("fixture address should exist");
        (listener, format!("http://{address}/api/v1/"))
    }

    async fn spawn_fixture(
        responses: Vec<ResponseSpec>,
    ) -> (
        String,
        oneshot::Receiver<Vec<CapturedRequest>>,
        tokio::task::JoinHandle<()>,
    ) {
        let (listener, base_url) = bind_fixture().await;
        let (request_sender, request_receiver) = oneshot::channel();
        let server_handle = tokio::spawn(async move {
            let mut captured_requests = Vec::new();
            for response in responses {
                let (mut socket, _) = listener.accept().await.expect("fixture should accept");
                captured_requests.push(
                    read_request(&mut socket)
                        .await
                        .expect("request should parse"),
                );
                if let Err(error) = write_response(&mut socket, &response).await {
                    assert!(
                        matches!(
                            error.kind(),
                            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
                        ),
                        "fixture response write should only fail after client cancellation: {error}"
                    );
                }
            }
            let _ = request_sender.send(captured_requests);
        });
        (base_url, request_receiver, server_handle)
    }

    async fn read_request(socket: &mut TcpStream) -> std::io::Result<CapturedRequest> {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        let header_end;
        loop {
            let bytes_read = socket.read(&mut buffer).await?;
            if bytes_read == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "request ended before headers",
                ));
            }
            bytes.extend_from_slice(&buffer[..bytes_read]);
            if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                header_end = position + 4;
                break;
            }
        }

        let header_text = String::from_utf8_lossy(&bytes[..header_end]);
        let mut lines = header_text.split("\r\n");
        let request_line = lines.next().expect("request line should exist");
        let mut request_line_parts = request_line.split_whitespace();
        let method = request_line_parts.next().unwrap_or_default().to_owned();
        let path = request_line_parts.next().unwrap_or_default().to_owned();
        let mut headers = HashMap::new();
        let mut content_length = 0;
        for line in lines.filter(|line| !line.is_empty()) {
            if let Some((name, value)) = line.split_once(':') {
                let name = name.to_ascii_lowercase();
                let value = value.trim().to_owned();
                if name == "content-length" {
                    content_length = value.parse().unwrap_or_default();
                }
                headers.insert(name, value);
            }
        }

        while bytes.len() < header_end + content_length {
            let bytes_read = socket.read(&mut buffer).await?;
            if bytes_read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..bytes_read]);
        }
        let body = String::from_utf8_lossy(
            &bytes[header_end..bytes.len().min(header_end + content_length)],
        )
        .into_owned();

        Ok(CapturedRequest {
            method,
            path,
            headers,
            body,
        })
    }

    async fn write_response(
        socket: &mut TcpStream,
        response: &ResponseSpec,
    ) -> std::io::Result<()> {
        let reason = match response.status {
            200 => "OK",
            400 => "Bad Request",
            404 => "Not Found",
            _ => "Error",
        };
        let mut response_head = format!(
            "HTTP/1.1 {} {}\r\nConnection: close\r\nContent-Length: {}\r\n",
            response.status,
            reason,
            response.body.len()
        );
        for (name, value) in &response.headers {
            response_head.push_str(&format!("{name}: {value}\r\n"));
        }
        response_head.push_str("\r\n");
        socket.write_all(response_head.as_bytes()).await?;
        for fragment in response.body.as_bytes().chunks(7) {
            socket.write_all(fragment).await?;
            tokio::task::yield_now().await;
        }
        if response.hold_connection_open {
            let mut buffer = [0_u8; 1];
            let _ = socket.read(&mut buffer).await?;
        }
        Ok(())
    }
}
