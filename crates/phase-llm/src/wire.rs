//! Provider wire formats: engine-built requests in, assistant text out.
//!
//! Both directions dispatch on [`WireProtocol`], never on the vendor, so every
//! OpenAI-compatible endpoint is served by the same two functions that serve
//! OpenAI itself. A transport calls [`build_chat_request`], performs exactly the
//! HTTP call it describes, and hands the raw body back to
//! [`extract_completion_text`]. No decision, credential handling, or payload
//! shaping happens outside this module.

use serde_json::{json, Value};

use crate::error::{LlmError, LlmResult};
use crate::prompt::LlmPrompt;
use crate::provider::{HttpHeader, HttpRequestSpec, LlmEndpointConfig, LlmProvider, WireProtocol};

/// Output budget used when a config names none. Sized for a short JSON decision
/// plus a sentence of reasoning — the prompt asks for nothing longer, and a
/// larger ceiling only buys latency on a per-priority-pass decision.
pub const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 1024;

/// Anthropic's `anthropic-version` pin. Changing it changes response shapes, so
/// it lives here beside the parser that reads them.
const ANTHROPIC_VERSION: &str = "2023-06-01";

const JSON_CONTENT_TYPE: &str = "application/json";

/// Build the exact HTTP call for one prompt.
pub fn build_chat_request(
    config: &LlmEndpointConfig,
    prompt: &LlmPrompt,
) -> LlmResult<HttpRequestSpec> {
    config.validate()?;
    let base = config.resolved_base_url()?;
    let model = config.model.trim();
    let max_tokens = config
        .max_output_tokens
        .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
    let api_key = config.api_key.trim();

    let mut headers = vec![HttpHeader::new("content-type", JSON_CONTENT_TYPE)];

    let (url, body) = match config.provider.wire() {
        WireProtocol::OpenAiChat => {
            if !api_key.is_empty() {
                headers.push(HttpHeader::new(
                    "authorization",
                    format!("Bearer {api_key}"),
                ));
            }
            let mut body = json!({
                "model": model,
                "messages": [
                    { "role": "system", "content": prompt.system },
                    { "role": "user", "content": prompt.user },
                ],
            });
            // OpenAI's reasoning-capable models reject the legacy `max_tokens`
            // key and require `max_completion_tokens`; third-party
            // OpenAI-compatible servers overwhelmingly implement only the
            // legacy key. Split on the vendor, which is the one place the two
            // contracts actually differ.
            let token_key = match config.provider {
                LlmProvider::OpenAi => "max_completion_tokens",
                _ => "max_tokens",
            };
            body[token_key] = json!(max_tokens);
            // Sent only when the player set one: OpenAI's reasoning models
            // accept the default temperature alone, so an unconditional field
            // would break them for no gain.
            if let Some(temperature) = config.temperature {
                body["temperature"] = json!(temperature);
            }
            (format!("{base}/chat/completions"), body)
        }
        WireProtocol::AnthropicMessages => {
            headers.push(HttpHeader::new("x-api-key", api_key));
            headers.push(HttpHeader::new("anthropic-version", ANTHROPIC_VERSION));
            // Anthropic blocks browser-origin calls unless the caller opts in.
            // Every consumer here IS a browser (web build and Tauri webview
            // alike), so the opt-in is unconditional.
            headers.push(HttpHeader::new(
                "anthropic-dangerous-direct-browser-access",
                "true",
            ));
            let mut body = json!({
                "model": model,
                // Required by the Messages API, unlike the OpenAI contract.
                "max_tokens": max_tokens,
                "system": prompt.system,
                "messages": [
                    { "role": "user", "content": prompt.user },
                ],
            });
            if let Some(temperature) = config.temperature {
                body["temperature"] = json!(temperature);
            }
            (format!("{base}/messages"), body)
        }
        WireProtocol::GeminiGenerateContent => {
            headers.push(HttpHeader::new("x-goog-api-key", api_key));
            let mut generation_config = json!({ "maxOutputTokens": max_tokens });
            if let Some(temperature) = config.temperature {
                generation_config["temperature"] = json!(temperature);
            }
            let body = json!({
                "systemInstruction": { "parts": [{ "text": prompt.system }] },
                "contents": [
                    { "role": "user", "parts": [{ "text": prompt.user }] },
                ],
                "generationConfig": generation_config,
            });
            (format!("{base}/models/{model}:generateContent"), body)
        }
    };

    Ok(HttpRequestSpec {
        url,
        method: "POST",
        headers,
        body: body.to_string(),
    })
}

/// Pull the assistant's text out of a response, given its HTTP status.
///
/// This is the status-aware entry point every caller should use. A non-2xx
/// response is a failure NO MATTER WHAT ITS BODY LOOKS LIKE: a proxy, a gateway
/// or a misrouted path can return a 4xx/5xx whose payload still parses as a
/// completion envelope, and accepting it would let an error masquerade as a
/// decision. The vendor's own diagnostic is still lifted out of that body when
/// present, because it is the most useful thing the player can be shown.
pub fn completion_from_response(
    provider: LlmProvider,
    status: u16,
    body: &str,
) -> LlmResult<String> {
    if !(200..300).contains(&status) {
        let detail = serde_json::from_str::<Value>(body)
            .ok()
            .as_ref()
            .and_then(provider_error_detail)
            .unwrap_or_else(|| {
                // No parsable envelope: say what happened without echoing an
                // arbitrary body, which may be an HTML error page.
                format!("the endpoint returned HTTP {status}")
            });
        return Err(LlmError::Provider {
            detail: format!("HTTP {status}: {detail}"),
        });
    }
    extract_completion_text(provider, body)
}

/// Pull the assistant's text out of a raw response body.
///
/// A provider error envelope becomes [`LlmError::Provider`] rather than a parse
/// failure, so the UI can show what the vendor actually said (bad key, unknown
/// model, rate limit) instead of a generic "the AI failed".
pub fn extract_completion_text(provider: LlmProvider, body: &str) -> LlmResult<String> {
    let value: Value = serde_json::from_str(body).map_err(|error| LlmError::MalformedResponse {
        detail: format!("response was not JSON: {error}"),
    })?;

    if let Some(detail) = provider_error_detail(&value) {
        return Err(LlmError::Provider { detail });
    }

    let text = match provider.wire() {
        WireProtocol::OpenAiChat => openai_text(&value),
        WireProtocol::AnthropicMessages => anthropic_text(&value),
        WireProtocol::GeminiGenerateContent => gemini_text(&value),
    };

    match text {
        Some(text) if !text.trim().is_empty() => Ok(text),
        Some(_) | None => Err(LlmError::EmptyCompletion),
    }
}

/// The error envelope every one of these vendors shares in shape: a top-level
/// `error` object (or string) carrying a message.
fn provider_error_detail(value: &Value) -> Option<String> {
    let error = value.get("error")?;
    if let Some(message) = error.get("message").and_then(Value::as_str) {
        return Some(message.to_string());
    }
    match error {
        Value::String(message) => Some(message.clone()),
        other => Some(other.to_string()),
    }
}

fn openai_text(value: &Value) -> Option<String> {
    let message = value.get("choices")?.as_array()?.first()?.get("message")?;
    match message.get("content")? {
        Value::String(text) => Some(text.clone()),
        // Some compatible servers emit the multimodal content-part array.
        Value::Array(parts) => Some(join_text_parts(parts, "text")),
        _ => None,
    }
}

fn anthropic_text(value: &Value) -> Option<String> {
    let parts = value.get("content")?.as_array()?;
    Some(join_text_parts(parts, "text"))
}

fn gemini_text(value: &Value) -> Option<String> {
    let parts = value
        .get("candidates")?
        .as_array()?
        .first()?
        .get("content")?
        .get("parts")?
        .as_array()?;
    Some(join_text_parts(parts, "text"))
}

/// Concatenate the `field` of every part that carries one. Shared by all three
/// protocols, which differ only in where the part array lives — thinking blocks
/// and tool blocks carry no `text` and drop out on their own.
fn join_text_parts(parts: &[Value], field: &str) -> String {
    parts
        .iter()
        .filter_map(|part| part.get(field).and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt() -> LlmPrompt {
        LlmPrompt {
            system: "sys".to_string(),
            user: "usr".to_string(),
        }
    }

    fn config(provider: LlmProvider) -> LlmEndpointConfig {
        LlmEndpointConfig {
            provider,
            base_url: None,
            api_key: "secret".to_string(),
            model: "model-x".to_string(),
            max_output_tokens: None,
            temperature: None,
        }
    }

    fn header<'a>(spec: &'a HttpRequestSpec, name: &str) -> Option<&'a str> {
        spec.headers
            .iter()
            .find(|header| header.name == name)
            .map(|header| header.value.as_str())
    }

    #[test]
    fn openai_requests_use_bearer_auth_and_the_completion_token_key() {
        let spec = build_chat_request(&config(LlmProvider::OpenAi), &prompt()).unwrap();
        assert_eq!(spec.url, "https://api.openai.com/v1/chat/completions");
        assert_eq!(header(&spec, "authorization"), Some("Bearer secret"));
        let body: Value = serde_json::from_str(&spec.body).unwrap();
        assert_eq!(
            body["max_completion_tokens"],
            json!(DEFAULT_MAX_OUTPUT_TOKENS)
        );
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("temperature").is_none());
    }

    #[test]
    fn compatible_endpoints_keep_the_legacy_token_key() {
        let mut config = config(LlmProvider::OpenAiCompatible);
        config.base_url = Some("http://localhost:1234/v1".to_string());
        let spec = build_chat_request(&config, &prompt()).unwrap();
        assert_eq!(spec.url, "http://localhost:1234/v1/chat/completions");
        let body: Value = serde_json::from_str(&spec.body).unwrap();
        assert_eq!(body["max_tokens"], json!(DEFAULT_MAX_OUTPUT_TOKENS));
        assert!(body.get("max_completion_tokens").is_none());
    }

    #[test]
    fn anthropic_requests_carry_the_version_and_browser_access_headers() {
        let spec = build_chat_request(&config(LlmProvider::Anthropic), &prompt()).unwrap();
        assert_eq!(spec.url, "https://api.anthropic.com/v1/messages");
        assert_eq!(header(&spec, "x-api-key"), Some("secret"));
        assert_eq!(header(&spec, "anthropic-version"), Some(ANTHROPIC_VERSION));
        assert_eq!(
            header(&spec, "anthropic-dangerous-direct-browser-access"),
            Some("true")
        );
        let body: Value = serde_json::from_str(&spec.body).unwrap();
        assert_eq!(body["system"], json!("sys"));
        assert_eq!(body["max_tokens"], json!(DEFAULT_MAX_OUTPUT_TOKENS));
    }

    #[test]
    fn gemini_requests_name_the_model_in_the_path() {
        let spec = build_chat_request(&config(LlmProvider::Gemini), &prompt()).unwrap();
        assert_eq!(
            spec.url,
            "https://generativelanguage.googleapis.com/v1beta/models/model-x:generateContent"
        );
        assert_eq!(header(&spec, "x-goog-api-key"), Some("secret"));
        let body: Value = serde_json::from_str(&spec.body).unwrap();
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], json!("sys"));
        assert_eq!(body["contents"][0]["parts"][0]["text"], json!("usr"));
    }

    /// The request this builds must match Google's own documented call for
    /// `generateContent`, including when the player pastes that documented URL
    /// into the endpoint field instead of the API root.
    #[test]
    fn a_gemini_request_matches_googles_documented_call() {
        const DOCUMENTED_URL: &str =
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-flash-latest:generateContent";

        for pasted in [
            // The API root, entered correctly.
            "https://generativelanguage.googleapis.com/v1beta",
            // The full per-call URL, copied from the docs.
            DOCUMENTED_URL,
        ] {
            let config = LlmEndpointConfig {
                provider: LlmProvider::Gemini,
                base_url: Some(pasted.to_string()),
                api_key: "test-key".to_string(),
                model: "gemini-flash-latest".to_string(),
                max_output_tokens: None,
                temperature: None,
            };
            let spec = build_chat_request(&config, &prompt()).unwrap();

            assert_eq!(spec.url, DOCUMENTED_URL, "pasted: {pasted}");
            assert_eq!(spec.method, "POST");
            assert_eq!(header(&spec, "content-type"), Some(JSON_CONTENT_TYPE));
            assert_eq!(header(&spec, "x-goog-api-key"), Some("test-key"));

            // The documented body shape: `contents[].parts[].text`.
            let body: Value = serde_json::from_str(&spec.body).unwrap();
            assert_eq!(body["contents"][0]["parts"][0]["text"], json!("usr"));
        }
    }

    #[test]
    fn a_configured_temperature_reaches_every_protocol() {
        for provider in [
            LlmProvider::OpenAi,
            LlmProvider::Anthropic,
            LlmProvider::Gemini,
        ] {
            let mut config = config(provider);
            config.temperature = Some(0.25);
            let spec = build_chat_request(&config, &prompt()).unwrap();
            let body: Value = serde_json::from_str(&spec.body).unwrap();
            let temperature = body
                .get("temperature")
                .or_else(|| {
                    body.get("generationConfig")
                        .and_then(|c| c.get("temperature"))
                })
                .cloned()
                .unwrap_or(Value::Null);
            assert_eq!(temperature, json!(0.25), "{provider:?}");
        }
    }

    #[test]
    fn openai_completions_decode_from_string_and_part_array_content() {
        let string_form = r#"{"choices":[{"message":{"content":"pick 2"}}]}"#;
        assert_eq!(
            extract_completion_text(LlmProvider::OpenAi, string_form).unwrap(),
            "pick 2"
        );
        let array_form = r#"{"choices":[{"message":{"content":[{"type":"text","text":"pick "},{"type":"text","text":"2"}]}}]}"#;
        assert_eq!(
            extract_completion_text(LlmProvider::DeepSeek, array_form).unwrap(),
            "pick 2"
        );
    }

    #[test]
    fn anthropic_thinking_blocks_do_not_contaminate_the_text() {
        let body =
            r#"{"content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"answer"}]}"#;
        assert_eq!(
            extract_completion_text(LlmProvider::Anthropic, body).unwrap(),
            "answer"
        );
    }

    #[test]
    fn gemini_parts_concatenate() {
        let body = r#"{"candidates":[{"content":{"parts":[{"text":"a"},{"text":"b"}]}}]}"#;
        assert_eq!(
            extract_completion_text(LlmProvider::Gemini, body).unwrap(),
            "ab"
        );
    }

    /// The finding: a non-2xx response whose body still parses as a valid
    /// completion must not be accepted as a decision.
    #[test]
    fn a_non_2xx_response_is_refused_even_when_its_body_looks_like_a_completion() {
        let looks_fine = r#"{"choices":[{"message":{"content":"{\"choice\": 0}"}}]}"#;
        for status in [400, 401, 403, 404, 429, 500, 502, 503] {
            let result = completion_from_response(LlmProvider::OpenAi, status, looks_fine);
            assert!(
                matches!(result, Err(LlmError::Provider { .. })),
                "HTTP {status} must not yield a completion"
            );
        }
    }

    #[test]
    fn a_non_2xx_response_keeps_the_vendors_diagnostic() {
        let body = r#"{"error":{"message":"Incorrect API key provided"}}"#;
        let error = completion_from_response(LlmProvider::OpenAi, 401, body).unwrap_err();
        let LlmError::Provider { detail } = error else {
            panic!("expected a provider error");
        };
        assert!(detail.contains("401"), "{detail}");
        assert!(detail.contains("Incorrect API key provided"), "{detail}");
    }

    #[test]
    fn a_non_2xx_response_with_an_unparsable_body_still_reports_its_status() {
        let error = completion_from_response(LlmProvider::OpenAi, 502, "<html>Bad Gateway</html>")
            .unwrap_err();
        let LlmError::Provider { detail } = error else {
            panic!("expected a provider error");
        };
        assert!(detail.contains("502"), "{detail}");
        // The raw HTML is not echoed back at the player.
        assert!(!detail.contains("<html>"), "{detail}");
    }

    #[test]
    fn a_2xx_response_decodes_normally_and_still_honours_an_error_envelope() {
        let ok = r#"{"choices":[{"message":{"content":"pick 1"}}]}"#;
        assert_eq!(
            completion_from_response(LlmProvider::OpenAi, 200, ok).unwrap(),
            "pick 1"
        );
        // Some providers return 200 with an error envelope; that is still a
        // failure.
        let soft_error = r#"{"error":{"message":"rate limited"}}"#;
        assert!(matches!(
            completion_from_response(LlmProvider::OpenAi, 200, soft_error),
            Err(LlmError::Provider { .. })
        ));
    }

    #[test]
    fn a_vendor_error_envelope_surfaces_its_message() {
        let body =
            r#"{"error":{"message":"Incorrect API key provided","type":"invalid_request_error"}}"#;
        assert_eq!(
            extract_completion_text(LlmProvider::OpenAi, body),
            Err(LlmError::Provider {
                detail: "Incorrect API key provided".to_string()
            })
        );
    }

    #[test]
    fn an_empty_completion_is_distinguishable_from_a_parse_failure() {
        assert_eq!(
            extract_completion_text(
                LlmProvider::OpenAi,
                r#"{"choices":[{"message":{"content":""}}]}"#
            ),
            Err(LlmError::EmptyCompletion)
        );
        assert!(matches!(
            extract_completion_text(LlmProvider::OpenAi, "not json"),
            Err(LlmError::MalformedResponse { .. })
        ));
    }
}
