use serde::{Deserialize, Serialize};

use super::super::{CompletionOutcome, ModelLimits, ModelMetadata, Usage};

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: [ChatMessage<'a>; 1],
    stream: bool,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'static str,
    content: &'a str,
}

pub(crate) fn generation_request_body(model_id: &str, input: &str) -> Result<Vec<u8>, String> {
    serde_json::to_vec(&ChatRequest {
        model: model_id,
        messages: [ChatMessage {
            role: "user",
            content: input,
        }],
        stream: true,
    })
    .map_err(|_| "unable to serialize the OpenRouter generation request".to_owned())
}

#[derive(Debug, Deserialize)]
struct CatalogResponse {
    data: Vec<CatalogModel>,
    #[serde(default)]
    links: Option<CatalogLinks>,
}

#[derive(Debug, Deserialize)]
struct CatalogLinks {
    #[serde(default)]
    next: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CatalogModel {
    id: String,
    name: String,
    context_length: u64,
    top_provider: TopProvider,
    #[serde(default)]
    pricing: Option<CatalogPricing>,
}

#[derive(Debug, Deserialize)]
struct TopProvider {
    #[serde(default)]
    max_completion_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct CatalogPricing {
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    completion: Option<String>,
}

pub(crate) fn parse_catalog_page(
    response_body: &str,
) -> Result<(Vec<ModelMetadata>, Option<String>), String> {
    let response: CatalogResponse = serde_json::from_str(response_body)
        .map_err(|_| "the OpenRouter model catalog response is malformed".to_owned())?;

    let models = response
        .data
        .into_iter()
        .map(normalize_catalog_model)
        .collect::<Result<Vec<_>, _>>()?;
    let next = response.links.and_then(|links| links.next);

    if next
        .as_deref()
        .is_some_and(|next_link| next_link.trim().is_empty())
    {
        return Err("the OpenRouter model catalog contains an empty next link".to_owned());
    }

    Ok((models, next))
}

fn normalize_catalog_model(model: CatalogModel) -> Result<ModelMetadata, String> {
    if model.id.trim().is_empty() || model.name.trim().is_empty() {
        return Err("the OpenRouter model catalog contains empty model metadata".to_owned());
    }

    let (prompt_price_usd_per_million_tokens, completion_price_usd_per_million_tokens) =
        match model.pricing {
            Some(pricing) => (
                pricing
                    .prompt
                    .as_deref()
                    .map(normalize_price)
                    .transpose()?
                    .flatten(),
                pricing
                    .completion
                    .as_deref()
                    .map(normalize_price)
                    .transpose()?
                    .flatten(),
            ),
            None => (None, None),
        };

    Ok(ModelMetadata {
        model_id: model.id,
        display_name: model.name,
        limits: ModelLimits {
            context_window_tokens: model.context_length,
            maximum_output_tokens: model.top_provider.max_completion_tokens,
        },
        prompt_price_usd_per_million_tokens,
        completion_price_usd_per_million_tokens,
    })
}

fn normalize_price(per_token_price: &str) -> Result<Option<String>, String> {
    // Router models use -1 when their price depends on the model selected at request time.
    if per_token_price == "-1" {
        return Ok(None);
    }

    let Some((integer_part, fractional_part)) = per_token_price.split_once('.') else {
        if per_token_price.is_empty() || !per_token_price.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err("the OpenRouter model catalog contains malformed pricing".to_owned());
        }

        return Ok(Some(normalize_scaled_digits(
            per_token_price,
            per_token_price.len() + 6,
        )));
    };

    if integer_part.is_empty()
        || fractional_part.is_empty()
        || !integer_part.bytes().all(|byte| byte.is_ascii_digit())
        || !fractional_part.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("the OpenRouter model catalog contains malformed pricing".to_owned());
    }

    let digits = format!("{integer_part}{fractional_part}");
    Ok(Some(normalize_scaled_digits(
        &digits,
        integer_part.len() + 6,
    )))
}

fn normalize_scaled_digits(digits: &str, decimal_position: usize) -> String {
    let mut scaled_integer = String::new();
    let (integer_part, fractional_part) = if decimal_position == 0 {
        ("0", digits)
    } else if decimal_position >= digits.len() {
        scaled_integer.push_str(digits);
        scaled_integer.push_str(&"0".repeat(decimal_position - digits.len()));
        (scaled_integer.as_str(), "")
    } else {
        digits.split_at(decimal_position)
    };

    let normalized_integer_part = integer_part.trim_start_matches('0');
    let normalized_integer_part = if normalized_integer_part.is_empty() {
        "0"
    } else {
        normalized_integer_part
    };
    let normalized_fractional_part = fractional_part.trim_end_matches('0');

    if normalized_fractional_part.is_empty() {
        normalized_integer_part.to_owned()
    } else {
        format!("{normalized_integer_part}.{normalized_fractional_part}")
    }
}

#[derive(Debug, Deserialize)]
struct ChatChunk {
    choices: Option<Vec<ChatChoice>>,
    #[serde(default)]
    usage: Option<ChatUsage>,
    #[serde(default)]
    error: Option<ChatError>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    delta: ChatDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
struct ChatUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    #[serde(default)]
    prompt_tokens_details: Option<ChatPromptTokenDetails>,
}

#[derive(Debug, Deserialize)]
struct ChatPromptTokenDetails {
    #[serde(default)]
    cached_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ChatError {
    message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FinishReason {
    Complete,
    LengthLimited,
    Error,
    Unsupported,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ParsedChatChunk {
    pub(crate) text_deltas: Vec<String>,
    pub(crate) usage: Option<Usage>,
    pub(crate) finish_reason: Option<FinishReason>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ParsedSseEvent {
    Done,
    Chunk(ParsedChatChunk),
    Error(String),
}

pub(crate) fn parse_sse_data(data: &str) -> Result<ParsedSseEvent, String> {
    if data.trim() == "[DONE]" {
        return Ok(ParsedSseEvent::Done);
    }

    let chunk: ChatChunk = serde_json::from_str(data)
        .map_err(|_| "the OpenRouter stream contained malformed JSON".to_owned())?;
    if let Some(error) = chunk.error {
        return Ok(ParsedSseEvent::Error(if error.message.is_empty() {
            "OpenRouter returned a stream error".to_owned()
        } else {
            error.message
        }));
    }

    let choices = chunk
        .choices
        .ok_or_else(|| "the OpenRouter stream omitted choices".to_owned())?;
    let mut text_deltas = Vec::new();
    let mut finish_reason = None;
    for choice in choices {
        if choice
            .delta
            .tool_calls
            .as_ref()
            .is_some_and(|tool_calls| !tool_calls.is_empty())
        {
            return Err("the OpenRouter stream returned unsupported tool calls".to_owned());
        }

        if let Some(content) = choice.delta.content.filter(|content| !content.is_empty()) {
            text_deltas.push(content);
        }

        if let Some(reason) = choice.finish_reason {
            let normalized_reason = match reason.as_str() {
                "stop" => FinishReason::Complete,
                "length" => FinishReason::LengthLimited,
                "error" => FinishReason::Error,
                _ => FinishReason::Unsupported,
            };

            if finish_reason.replace(normalized_reason).is_some() {
                return Err("the OpenRouter stream returned multiple finish reasons".to_owned());
            }
        }
    }

    let usage = chunk.usage.map(|usage| Usage {
        input_tokens: usage.prompt_tokens,
        cached_input_tokens: usage
            .prompt_tokens_details
            .and_then(|details| details.cached_tokens)
            .unwrap_or(0),
        output_tokens: usage.completion_tokens,
    });

    Ok(ParsedSseEvent::Chunk(ParsedChatChunk {
        text_deltas,
        usage,
        finish_reason,
    }))
}

pub(crate) fn completion_outcome(finish_reason: FinishReason) -> Option<CompletionOutcome> {
    match finish_reason {
        FinishReason::Complete => Some(CompletionOutcome::Complete),
        FinishReason::LengthLimited => Some(CompletionOutcome::LengthLimited),
        FinishReason::Error | FinishReason::Unsupported => None,
    }
}

pub(crate) fn finish_reason_error(finish_reason: FinishReason) -> &'static str {
    match finish_reason {
        FinishReason::Error => "the OpenRouter stream reported an error completion",
        FinishReason::Unsupported => {
            "the OpenRouter stream returned an unsupported completion reason"
        }
        FinishReason::Complete | FinishReason::LengthLimited => "",
    }
}

pub(crate) fn api_error_message(response_body: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct ErrorEnvelope {
        error: ChatError,
    }

    serde_json::from_str::<ErrorEnvelope>(response_body)
        .ok()
        .and_then(|error| (!error.error.message.is_empty()).then_some(error.error.message))
}

#[cfg(test)]
mod tests {
    use super::{
        FinishReason, ParsedSseEvent, completion_outcome, generation_request_body, normalize_price,
        parse_catalog_page, parse_sse_data,
    };
    use crate::provider::{CompletionOutcome, ModelLimits, Usage};

    #[test]
    fn generation_body_contains_only_the_independent_user_request() {
        let body = generation_request_body("provider/model", "hello\nworld")
            .expect("request body should serialize");

        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).expect("body should be JSON"),
            serde_json::json!({
                "model": "provider/model",
                "messages": [{"role": "user", "content": "hello\nworld"}],
                "stream": true,
            })
        );
    }

    #[test]
    fn catalog_metadata_and_prices_are_normalized_without_floating_point_rounding() {
        let (models, next) = parse_catalog_page(
            r#"{
                "data": [{
                    "id": "provider/model",
                    "name": "Provider Model",
                    "context_length": 131072,
                    "top_provider": {"max_completion_tokens": 8192},
                    "pricing": {"prompt": "0.000001", "completion": "0.0000025"}
                }],
                "links": {"next": "/api/v1/models?page=2"}
            }"#,
        )
        .expect("catalog should parse");

        assert_eq!(next.as_deref(), Some("/api/v1/models?page=2"));
        assert_eq!(
            models,
            vec![crate::provider::ModelMetadata {
                model_id: "provider/model".to_owned(),
                display_name: "Provider Model".to_owned(),
                limits: ModelLimits {
                    context_window_tokens: 131072,
                    maximum_output_tokens: Some(8192),
                },
                prompt_price_usd_per_million_tokens: Some("1".to_owned()),
                completion_price_usd_per_million_tokens: Some("2.5".to_owned()),
            }]
        );
    }

    #[test]
    fn catalog_keeps_models_with_missing_or_partial_pricing() {
        let (models, _) = parse_catalog_page(
            r#"{
                "data": [
                    {
                        "id": "provider/no-pricing",
                        "name": "No Pricing",
                        "context_length": 100,
                        "top_provider": {"max_completion_tokens": 20}
                    },
                    {
                        "id": "provider/prompt-only",
                        "name": "Prompt Only",
                        "context_length": 200,
                        "top_provider": {"max_completion_tokens": 30},
                        "pricing": {"prompt": "0.000001"}
                    },
                    {
                        "id": "provider/completion-only",
                        "name": "Completion Only",
                        "context_length": 300,
                        "top_provider": {"max_completion_tokens": 40},
                        "pricing": {"completion": "0.000002"}
                    }
                ]
            }"#,
        )
        .expect("catalog models with partial pricing should parse");

        assert_eq!(models.len(), 3);
        assert_eq!(models[0].prompt_price_usd_per_million_tokens, None);
        assert_eq!(models[0].completion_price_usd_per_million_tokens, None);
        assert_eq!(
            models[1].prompt_price_usd_per_million_tokens,
            Some("1".to_owned())
        );
        assert_eq!(models[1].completion_price_usd_per_million_tokens, None);
        assert_eq!(models[2].prompt_price_usd_per_million_tokens, None);
        assert_eq!(
            models[2].completion_price_usd_per_million_tokens,
            Some("2".to_owned())
        );
    }

    #[test]
    fn catalog_retains_models_with_known_and_unknown_output_limits() {
        let (models, _) = parse_catalog_page(
            r#"{
                "data": [
                    {
                        "id": "provider/known-limit",
                        "name": "Known Limit",
                        "context_length": 100,
                        "top_provider": {"max_completion_tokens": 8192}
                    },
                    {
                        "id": "provider/null-limit",
                        "name": "Null Limit",
                        "context_length": 200,
                        "top_provider": {"max_completion_tokens": null}
                    },
                    {
                        "id": "provider/omitted-limit",
                        "name": "Omitted Limit",
                        "context_length": 300,
                        "top_provider": {}
                    }
                ]
            }"#,
        )
        .expect("catalog models with unknown output limits should parse");

        assert_eq!(models.len(), 3);
        assert_eq!(models[0].limits.maximum_output_tokens, Some(8192));
        assert_eq!(models[1].limits.maximum_output_tokens, None);
        assert_eq!(models[2].limits.maximum_output_tokens, None);
    }

    #[test]
    fn catalog_rejects_malformed_required_metadata_and_pricing() {
        for body in [
            r#"{"data":[{"name":"model","context_length":1,"top_provider":{"max_completion_tokens":1}}]}"#,
            r#"{"data":[{"id":"model","name":"model","context_length":1}]}"#,
            r#"{"data":[{"id":"model","name":"model","context_length":1,"top_provider":{"max_completion_tokens":"bad"}}]}"#,
            r#"{"data":[{"id":"model","name":"model","context_length":1,"top_provider":{"max_completion_tokens":1},"pricing":{"prompt":"bad","completion":"0"}}]}"#,
        ] {
            assert!(parse_catalog_page(body).is_err());
        }
    }

    #[test]
    fn prices_are_scaled_exactly() {
        assert_eq!(normalize_price("0"), Ok(Some("0".to_owned())));
        assert_eq!(normalize_price("1"), Ok(Some("1000000".to_owned())));
        assert_eq!(normalize_price("0.0000001"), Ok(Some("0.1".to_owned())));
        assert_eq!(
            normalize_price("0001.500000"),
            Ok(Some("1500000".to_owned()))
        );
        assert_eq!(normalize_price("-1"), Ok(None));
        assert!(normalize_price("-2").is_err());
        assert!(normalize_price("1e-6").is_err());
    }

    #[test]
    fn catalog_retains_router_models_with_variable_pricing() {
        let (models, _) = parse_catalog_page(
            r#"{
                "data": [{
                    "id": "openrouter/auto",
                    "name": "Auto Router",
                    "context_length": 100,
                    "top_provider": {"max_completion_tokens": null},
                    "pricing": {"prompt": "-1", "completion": "-1"}
                }]
            }"#,
        )
        .expect("router models with variable pricing should parse");

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].prompt_price_usd_per_million_tokens, None);
        assert_eq!(models[0].completion_price_usd_per_million_tokens, None);
    }

    #[test]
    fn sse_mapping_preserves_text_usage_cache_and_terminal_reason() {
        assert_eq!(
            parse_sse_data(r#"{"choices":[{"delta":{"content":"hello"},"finish_reason":null}]}"#,),
            Ok(ParsedSseEvent::Chunk(super::ParsedChatChunk {
                text_deltas: vec!["hello".to_owned()],
                usage: None,
                finish_reason: None,
            }))
        );
        assert_eq!(
            parse_sse_data(
                r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":6}}}"#,
            ),
            Ok(ParsedSseEvent::Chunk(super::ParsedChatChunk {
                text_deltas: Vec::new(),
                usage: Some(Usage {
                    input_tokens: 10,
                    cached_input_tokens: 6,
                    output_tokens: 4,
                }),
                finish_reason: None,
            }))
        );
        assert_eq!(
            parse_sse_data(r#"{"choices":[{"delta":{},"finish_reason":"length"}]}"#),
            Ok(ParsedSseEvent::Chunk(super::ParsedChatChunk {
                text_deltas: Vec::new(),
                usage: None,
                finish_reason: Some(FinishReason::LengthLimited),
            }))
        );
        assert_eq!(
            completion_outcome(FinishReason::Complete),
            Some(CompletionOutcome::Complete)
        );
        assert_eq!(
            completion_outcome(FinishReason::LengthLimited),
            Some(CompletionOutcome::LengthLimited)
        );
    }

    #[test]
    fn sse_mapping_rejects_malformed_data_and_tools_and_maps_api_errors() {
        assert!(parse_sse_data("not-json").is_err());
        assert!(
            parse_sse_data(r#"{"choices":[{"delta":{"tool_calls":[{"id":"secret-call"}]}}]}"#)
                .expect_err("tool calls should be rejected")
                .contains("tool calls")
        );
        assert_eq!(
            parse_sse_data(r#"{"error":{"message":"provider failed"}}"#),
            Ok(ParsedSseEvent::Error("provider failed".to_owned()))
        );
        assert_eq!(parse_sse_data("[DONE]"), Ok(ParsedSseEvent::Done));
        assert_eq!(
            parse_sse_data(
                r#"{"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":2}}"#,
            ),
            Ok(ParsedSseEvent::Chunk(super::ParsedChatChunk {
                text_deltas: Vec::new(),
                usage: Some(Usage {
                    input_tokens: 1,
                    cached_input_tokens: 0,
                    output_tokens: 2,
                }),
                finish_reason: Some(FinishReason::Complete),
            }))
        );
    }
}
