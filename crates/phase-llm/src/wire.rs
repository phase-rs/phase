//! Provider wire formats: engine-built requests in, assistant text out.
//!
//! Both directions dispatch on [`WireProtocol`], never on the vendor, so every
//! OpenAI-compatible endpoint is served by the same two functions that serve
//! OpenAI itself. A transport calls [`build_chat_request`], performs exactly the
//! HTTP call it describes, and hands the raw body back to
//! [`extract_completion_text`]. No decision, credential handling, or payload
//! shaping happens outside this module.

use serde_json::{json, Map, Value};

use crate::error::{LlmError, LlmResult};
use crate::prompt::{DecisionFrame, LlmPrompt};
use crate::provider::{
    HttpHeader, HttpRequestSpec, LlmEndpointConfig, LlmProvider, RedirectPolicy, WireProtocol,
};

/// Output budget used when a config names none. Sized for a short JSON decision
/// plus a sentence of reasoning — the prompt asks for nothing longer, and a
/// larger ceiling only buys latency on a per-priority-pass decision.
pub const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 1024;

/// Anthropic's `anthropic-version` pin. Changing it changes response shapes, so
/// it lives here beside the parser that reads them.
const ANTHROPIC_VERSION: &str = "2023-06-01";

const JSON_CONTENT_TYPE: &str = "application/json";

/// The relay reads its envelope from the body of a CORS *simple request*, which
/// the browser sends without a preflight only when the content type is one of
/// the three form types. See `phase-server`'s `jev_relay`.
const RELAY_CONTENT_TYPE: &str = "text/plain;charset=UTF-8";

/// Path of the phase-server relay to TypeSafe's System One API.
const RELAY_PATH: &str = "/jev/systemone";

/// The id of the one question every decision is posed as. Echoed back as the
/// key of the answer.
const SYSTEM_ONE_QUESTION_ID: &str = "pick";

/// Fewest and most options a System One Choice accepts: a one-option Choice
/// answers nothing, and the API caps a Choice at 255.
const SYSTEM_ONE_MIN_OPTIONS: usize = 2;
const SYSTEM_ONE_MAX_OPTIONS: usize = 255;

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
    let mut redirect = RedirectPolicy::Follow;

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
            // `x-api-key` is not stripped from a cross-origin redirect.
            redirect = RedirectPolicy::Error;
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
        WireProtocol::SystemOneRelay => {
            // No `Authorization` and no JSON content type: the relay's simple
            // request carries the key in the envelope, and the key's only
            // destination is that body.
            headers = vec![HttpHeader::new("content-type", RELAY_CONTENT_TYPE)];
            // The key is in the BODY, which a followed 307/308 would replay to
            // whatever origin `Location` names. The relay answers directly or
            // not at all.
            redirect = RedirectPolicy::Error;
            let body = json!({
                "apiKey": api_key,
                "request": system_one_request(model, &prompt.frame)?,
            });
            (format!("{base}{RELAY_PATH}"), body)
        }
        WireProtocol::GeminiGenerateContent => {
            headers.push(HttpHeader::new("x-goog-api-key", api_key));
            // Likewise not stripped from a cross-origin redirect.
            redirect = RedirectPolicy::Error;
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
        redirect,
    })
}

/// A provider's raw answer to one request, exactly as the transport received it.
#[derive(Debug, Clone, Copy)]
pub struct LlmReply<'a> {
    pub provider: LlmProvider,
    pub status: u16,
    pub body: &'a str,
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
    issued: &[String],
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
    extract_completion_text(provider, body, issued)
}

/// Pull the assistant's text out of a raw response body.
///
/// A provider error envelope becomes [`LlmError::Provider`] rather than a parse
/// failure, so the UI can show what the vendor actually said (bad key, unknown
/// model, rate limit) instead of a generic "the AI failed".
pub fn extract_completion_text(
    provider: LlmProvider,
    body: &str,
    issued: &[String],
) -> LlmResult<String> {
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
        WireProtocol::SystemOneRelay => return system_one_completion(&value, issued),
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

// ── System One (Jev) ────────────────────────────────────────────────────────
//
// Jev does not generate text. It answers a typed question with the probability
// of every option, so this protocol poses each decision as ONE Choice over the
// engine-issued options and reads the ranking back off the answer. The model
// never names an action: it still returns an option NUMBER, which the engine
// decodes through the same strict `decode_choice` and re-validates exactly as
// it does a chat reply.

/// The System One request body for one decision.
///
/// The position is the question's `state` (data), and the engine-authored brief
/// and question are its `instructions` (never data), so a card's Oracle text can
/// describe the game but cannot address the model as an instruction.
///
/// Each option's key is `"<index>: <label>"`, the index zero-padded to a common
/// width. The index makes keys unique (two identical labels are two options) and
/// is what [`system_one_completion`] reads back, and the padding keeps the
/// API's sorted key order equal to the engine's option order.
fn system_one_request(model: &str, frame: &DecisionFrame) -> LlmResult<Value> {
    let count = frame.options.len();
    if !(SYSTEM_ONE_MIN_OPTIONS..=SYSTEM_ONE_MAX_OPTIONS).contains(&count) {
        return Err(LlmError::UnsupportedDecision {
            detail: format!(
                "a Jev Choice takes {SYSTEM_ONE_MIN_OPTIONS} to {SYSTEM_ONE_MAX_OPTIONS} \
                 options and this decision has {count}"
            ),
        });
    }
    let criteria: Map<String, Value> = system_one_option_keys(&frame.options)
        .into_iter()
        .map(|key| (key, Value::Null))
        .collect();
    Ok(json!({
        "model": model,
        "state": { "position": frame.position },
        "questions": {
            SYSTEM_ONE_QUESTION_ID: {
                "type": "choice",
                "instructions": format!("{}\n\n{}", frame.brief, frame.instruction),
                "criteria": criteria,
            },
        },
    }))
}

/// The criterion name System One is given for each option, in option order.
///
/// The single authority for the key format: the request is built from it and the
/// answer is checked against it, so a key the model returns can only mean an
/// option if it is, character for character, one this engine issued.
pub fn system_one_option_keys(options: &[String]) -> Vec<String> {
    let width = options.len().saturating_sub(1).to_string().len();
    options
        .iter()
        .enumerate()
        .map(|(index, label)| format!("{index:0width$}: {label}"))
        .collect()
}

/// Turn a System One answer into the reply the shared decoder reads:
/// `{"choice": [<every option number, best first>], "reason": "..."}`.
///
/// The ranking is the answer's `choice` first, then the remaining options by
/// descending probability (ties keep engine order). A single decision takes the
/// head; a multi-card draft step takes as many as it needs, so Jev's relative
/// ranking of the pack decides the whole step and not only its first card.
///
/// `issued` is the decision's option lines. Every key the answer names — the
/// `choice` and each entry of `probabilities` — must be exactly one of the keys
/// [`system_one_option_keys`] built from them. A key with a valid number but a
/// label that was never issued is a malformed answer, not option N: the number
/// alone would turn text this engine never offered into a legal selection.
fn system_one_completion(body: &Value, issued: &[String]) -> LlmResult<String> {
    let keys: std::collections::HashMap<String, usize> = system_one_option_keys(issued)
        .into_iter()
        .enumerate()
        .map(|(index, key)| (key, index))
        .collect();
    let unissued = || LlmError::MalformedResponse {
        detail: "the System One answer names a criterion this engine did not issue".to_string(),
    };
    let answer = body
        .get("answers")
        .and_then(|answers| answers.get(SYSTEM_ONE_QUESTION_ID))
        .ok_or_else(|| LlmError::MalformedResponse {
            detail: "the System One reply has no answer to the question asked".to_string(),
        })?;
    let top = *answer
        .get("choice")
        .and_then(Value::as_str)
        .and_then(|key| keys.get(key))
        .ok_or_else(unissued)?;

    let mut ranked: Vec<(usize, f64)> = Vec::new();
    if let Some(probabilities) = answer.get("probabilities").and_then(Value::as_object) {
        for (key, probability) in probabilities {
            let index = *keys.get(key.as_str()).ok_or_else(unissued)?;
            let probability = probability
                .as_f64()
                .ok_or_else(|| LlmError::MalformedResponse {
                    detail: "the System One answer carries a non-numeric probability".to_string(),
                })?;
            if index != top {
                ranked.push((index, probability));
            }
        }
    }
    // Stable on engine order, so equal probabilities rank deterministically.
    ranked.sort_by(|(a_index, a), (b_index, b)| b.total_cmp(a).then(a_index.cmp(b_index)));

    let choice: Vec<usize> = std::iter::once(top)
        .chain(ranked.into_iter().map(|(index, _)| index))
        .collect();
    let reason = match answer.get("confidence").and_then(Value::as_f64) {
        Some(confidence) => format!("Jev, confidence {confidence:.2}"),
        None => "Jev".to_string(),
    };
    Ok(json!({ "choice": choice, "reason": reason }).to_string())
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
            frame: DecisionFrame {
                brief: "You are playing.".to_string(),
                position: "Turn 3. You have 2 lands.".to_string(),
                instruction: "Choose the best option.".to_string(),
                options: vec![
                    "Pass Priority".to_string(),
                    "Lightning Bolt — cast".to_string(),
                    "Shock — cast".to_string(),
                ],
            },
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
    fn only_a_bearer_authorization_credential_may_follow_a_redirect() {
        // `Authorization` is the one credential header fetch strips cross-origin.
        for provider in [LlmProvider::OpenAi, LlmProvider::DeepSeek] {
            let spec = build_chat_request(&config(provider), &prompt()).unwrap();
            assert_eq!(spec.redirect, RedirectPolicy::Follow, "{provider:?}");
            assert_eq!(
                serde_json::to_value(&spec).unwrap()["redirect"],
                json!("follow")
            );
        }
        // A key in `x-api-key`, `x-goog-api-key` or the body would be replayed.
        for provider in [
            LlmProvider::Anthropic,
            LlmProvider::Gemini,
            LlmProvider::Jev,
        ] {
            let mut config = config(provider);
            config.base_url = Some("https://relay.example".to_string());
            let spec = build_chat_request(&config, &prompt()).unwrap();
            assert_eq!(spec.redirect, RedirectPolicy::Error, "{provider:?}");
        }
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
            extract_completion_text(LlmProvider::OpenAi, string_form, &[]).unwrap(),
            "pick 2"
        );
        let array_form = r#"{"choices":[{"message":{"content":[{"type":"text","text":"pick "},{"type":"text","text":"2"}]}}]}"#;
        assert_eq!(
            extract_completion_text(LlmProvider::DeepSeek, array_form, &[]).unwrap(),
            "pick 2"
        );
    }

    #[test]
    fn anthropic_thinking_blocks_do_not_contaminate_the_text() {
        let body =
            r#"{"content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"answer"}]}"#;
        assert_eq!(
            extract_completion_text(LlmProvider::Anthropic, body, &[]).unwrap(),
            "answer"
        );
    }

    #[test]
    fn gemini_parts_concatenate() {
        let body = r#"{"candidates":[{"content":{"parts":[{"text":"a"},{"text":"b"}]}}]}"#;
        assert_eq!(
            extract_completion_text(LlmProvider::Gemini, body, &[]).unwrap(),
            "ab"
        );
    }

    /// The finding: a non-2xx response whose body still parses as a valid
    /// completion must not be accepted as a decision.
    #[test]
    fn a_non_2xx_response_is_refused_even_when_its_body_looks_like_a_completion() {
        let looks_fine = r#"{"choices":[{"message":{"content":"{\"choice\": 0}"}}]}"#;
        for status in [400, 401, 403, 404, 429, 500, 502, 503] {
            let result = completion_from_response(LlmProvider::OpenAi, status, looks_fine, &[]);
            assert!(
                matches!(result, Err(LlmError::Provider { .. })),
                "HTTP {status} must not yield a completion"
            );
        }
    }

    #[test]
    fn a_non_2xx_response_keeps_the_vendors_diagnostic() {
        let body = r#"{"error":{"message":"Incorrect API key provided"}}"#;
        let error = completion_from_response(LlmProvider::OpenAi, 401, body, &[]).unwrap_err();
        let LlmError::Provider { detail } = error else {
            panic!("expected a provider error");
        };
        assert!(detail.contains("401"), "{detail}");
        assert!(detail.contains("Incorrect API key provided"), "{detail}");
    }

    #[test]
    fn a_non_2xx_response_with_an_unparsable_body_still_reports_its_status() {
        let error =
            completion_from_response(LlmProvider::OpenAi, 502, "<html>Bad Gateway</html>", &[])
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
            completion_from_response(LlmProvider::OpenAi, 200, ok, &[]).unwrap(),
            "pick 1"
        );
        // Some providers return 200 with an error envelope; that is still a
        // failure.
        let soft_error = r#"{"error":{"message":"rate limited"}}"#;
        assert!(matches!(
            completion_from_response(LlmProvider::OpenAi, 200, soft_error, &[]),
            Err(LlmError::Provider { .. })
        ));
    }

    #[test]
    fn a_vendor_error_envelope_surfaces_its_message() {
        let body =
            r#"{"error":{"message":"Incorrect API key provided","type":"invalid_request_error"}}"#;
        assert_eq!(
            extract_completion_text(LlmProvider::OpenAi, body, &[]),
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
                r#"{"choices":[{"message":{"content":""}}]}"#,
                &[]
            ),
            Err(LlmError::EmptyCompletion)
        );
        assert!(matches!(
            extract_completion_text(LlmProvider::OpenAi, "not json", &[]),
            Err(LlmError::MalformedResponse { .. })
        ));
    }

    // ── Jev (System One relay) ───────────────────────────────────────────

    fn jev_config() -> LlmEndpointConfig {
        LlmEndpointConfig {
            base_url: Some("https://phase.example".to_string()),
            ..config(LlmProvider::Jev)
        }
    }

    fn jev_request() -> Value {
        let spec = build_chat_request(&jev_config(), &prompt()).unwrap();
        serde_json::from_str(&spec.body).unwrap()
    }

    #[test]
    fn jev_posts_a_simple_request_to_the_relay_with_the_key_only_in_the_envelope() {
        let spec = build_chat_request(&jev_config(), &prompt()).unwrap();
        assert_eq!(spec.url, "https://phase.example/jev/systemone");
        assert_eq!(spec.method, "POST");
        // A CORS simple request: one header, no `Authorization`.
        assert_eq!(spec.headers.len(), 1);
        assert_eq!(
            header(&spec, "content-type"),
            Some("text/plain;charset=UTF-8")
        );
        let body: Value = serde_json::from_str(&spec.body).unwrap();
        assert_eq!(body["apiKey"], json!("secret"));
        assert_eq!(body["request"]["model"], json!("model-x"));
        assert_eq!(body.as_object().unwrap().len(), 2);
        assert!(!spec.body.contains("Bearer"));
        // The key is in the body, so a redirect must fail rather than replay it.
        assert_eq!(spec.redirect, RedirectPolicy::Error);
        assert_eq!(
            serde_json::to_value(&spec).unwrap()["redirect"],
            json!("error")
        );
    }

    #[test]
    fn jev_poses_the_decision_as_one_choice_over_the_engine_options() {
        let request = jev_request()["request"].clone();
        let question = &request["questions"]["pick"];
        assert_eq!(question["type"], json!("choice"));
        let criteria = question["criteria"].as_object().unwrap();
        assert_eq!(
            criteria.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "0: Pass Priority",
                "1: Lightning Bolt — cast",
                "2: Shock — cast"
            ]
        );
        assert!(criteria.values().all(Value::is_null));
    }

    /// The fence the chat prompt draws with delimiters is drawn structurally
    /// here: the position (which carries Oracle text and player names) is the
    /// question's `state`, and only engine-authored text is its instructions.
    #[test]
    fn jev_keeps_the_position_in_state_and_out_of_the_instructions() {
        let request = jev_request()["request"].clone();
        assert_eq!(
            request["state"],
            json!({ "position": "Turn 3. You have 2 lands." })
        );
        let instructions = request["questions"]["pick"]["instructions"]
            .as_str()
            .unwrap();
        assert_eq!(instructions, "You are playing.\n\nChoose the best option.");
        assert!(!instructions.contains("Turn 3"));
    }

    #[test]
    fn jev_option_numbers_are_padded_so_key_order_is_engine_order() {
        let mut prompt = prompt();
        prompt.frame.options = (0..12).map(|n| format!("option {n}")).collect();
        let spec = build_chat_request(&jev_config(), &prompt).unwrap();
        let body: Value = serde_json::from_str(&spec.body).unwrap();
        let keys: Vec<&String> = body["request"]["questions"]["pick"]["criteria"]
            .as_object()
            .unwrap()
            .keys()
            .collect();
        assert_eq!(keys.first().map(|k| k.as_str()), Some("00: option 0"));
        assert_eq!(keys.last().map(|k| k.as_str()), Some("11: option 11"));
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }

    #[test]
    fn identical_labels_stay_distinct_options() {
        let mut prompt = prompt();
        prompt.frame.options = vec!["Pass".to_string(), "Pass".to_string()];
        let body: Value =
            serde_json::from_str(&build_chat_request(&jev_config(), &prompt).unwrap().body)
                .unwrap();
        assert_eq!(
            body["request"]["questions"]["pick"]["criteria"]
                .as_object()
                .unwrap()
                .len(),
            2
        );
    }

    /// A forced move has no question to put, and a Choice caps at 255.
    #[test]
    fn jev_refuses_a_decision_a_choice_cannot_express() {
        for count in [0usize, 1, 256] {
            let mut prompt = prompt();
            prompt.frame.options = (0..count).map(|n| n.to_string()).collect();
            assert!(
                matches!(
                    build_chat_request(&jev_config(), &prompt),
                    Err(LlmError::UnsupportedDecision { .. })
                ),
                "{count} options"
            );
        }
        let mut prompt = prompt();
        prompt.frame.options = (0..255).map(|n| n.to_string()).collect();
        assert!(build_chat_request(&jev_config(), &prompt).is_ok());
    }

    #[test]
    fn jev_needs_a_relay_a_key_and_a_safe_scheme() {
        let mut no_relay = jev_config();
        no_relay.base_url = None;
        assert!(matches!(
            build_chat_request(&no_relay, &prompt()),
            Err(LlmError::Configuration { .. })
        ));
        let mut no_key = jev_config();
        no_key.api_key = " ".to_string();
        assert!(matches!(
            build_chat_request(&no_key, &prompt()),
            Err(LlmError::Configuration { .. })
        ));
        // The key rides in the body, but plaintext to a remote host still
        // exposes it to the path.
        let mut plaintext = jev_config();
        plaintext.base_url = Some("http://phase.example".to_string());
        assert!(matches!(
            build_chat_request(&plaintext, &prompt()),
            Err(LlmError::Configuration { .. })
        ));
        let mut local = jev_config();
        local.base_url = Some("http://localhost:9374/jev/systemone/".to_string());
        assert_eq!(
            build_chat_request(&local, &prompt()).unwrap().url,
            "http://localhost:9374/jev/systemone"
        );
    }

    fn jev_answer(choice: &str, probabilities: Value, confidence: f64) -> String {
        json!({
            "model": "jev-latest",
            "answers": { "pick": {
                "type": "choice",
                "choice": choice,
                "probabilities": probabilities,
                "confidence": confidence,
            }},
            "usage": {},
        })
        .to_string()
    }

    #[test]
    fn a_jev_answer_becomes_a_ranking_the_shared_decoder_reads() {
        let body = jev_answer(
            "1: Lightning Bolt — cast",
            json!({
                "0: Pass Priority": 0.1,
                "1: Lightning Bolt — cast": 0.6,
                "2: Shock — cast": 0.3,
            }),
            0.8,
        );
        let text = completion_from_response(LlmProvider::Jev, 200, &body, &prompt().frame.options)
            .unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["choice"], json!([1, 2, 0]));
        assert_eq!(value["reason"], json!("Jev, confidence 0.80"));

        // The one decode path every provider shares: a single decision takes the
        // head, a two-card draft step the top two.
        let one = crate::prompt::decode_choice(&text, 3, 1).unwrap();
        assert_eq!(one.indices, vec![1]);
        let two = crate::prompt::decode_choice(&text, 3, 2).unwrap();
        assert_eq!(two.indices, vec![1, 2]);
    }

    #[test]
    fn jev_ties_rank_in_engine_order_and_the_stated_choice_leads() {
        let body = jev_answer(
            "2: c",
            json!({ "0: a": 0.25, "1: b": 0.25, "2: c": 0.25, "3: d": 0.25 }),
            0.1,
        );
        let issued: Vec<String> = ["a", "b", "c", "d"].map(String::from).to_vec();
        let text = completion_from_response(LlmProvider::Jev, 200, &body, &issued).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["choice"], json!([2, 0, 1, 3]));
    }

    #[test]
    fn a_jev_answer_without_a_usable_choice_is_refused() {
        for body in [
            "{}".to_string(),
            json!({ "answers": {} }).to_string(),
            jev_answer("not an option key", json!({}), 0.5),
            json!({ "answers": { "pick": { "type": "choice" } } }).to_string(),
        ] {
            assert!(
                matches!(
                    completion_from_response(LlmProvider::Jev, 200, &body, &prompt().frame.options),
                    Err(LlmError::MalformedResponse { .. })
                ),
                "{body}"
            );
        }
    }

    #[test]
    fn a_jev_failure_status_is_refused_even_with_a_decodable_body() {
        let body = jev_answer("0: a", json!({ "0: a": 1.0 }), 1.0);
        let issued = ["a".to_string(), "b".to_string()];
        for status in [401, 422, 429, 502, 504, 529] {
            assert!(
                matches!(
                    completion_from_response(LlmProvider::Jev, status, &body, &issued),
                    Err(LlmError::Provider { .. })
                ),
                "HTTP {status}"
            );
        }
    }

    /// The number alone must never be the selection: a reply whose key starts
    /// with an issued index but carries text this engine never offered names an
    /// unissued criterion, so it is refused rather than read as that option.
    #[test]
    fn a_jev_key_with_a_valid_number_but_an_unissued_label_is_refused() {
        let issued = prompt().frame.options;
        let right = json!({
            "0: Pass Priority": 0.1,
            "1: Lightning Bolt — cast": 0.6,
            "2: Shock — cast": 0.3,
        });
        let wrong_choice = jev_answer("1: Cast something else entirely", right.clone(), 0.9);
        let wrong_probability = jev_answer(
            "1: Lightning Bolt — cast",
            json!({
                "0: Pass Priority": 0.1,
                "1: Lightning Bolt — cast": 0.6,
                "2: Concede the game": 0.3,
            }),
            0.9,
        );
        let invented_extra = jev_answer(
            "1: Lightning Bolt — cast",
            json!({
                "1: Lightning Bolt — cast": 0.6,
                "7: Something never offered": 0.4,
            }),
            0.9,
        );
        // Right label, wrong padding or index: still not an issued key.
        let padded = jev_answer("01: Lightning Bolt — cast", right.clone(), 0.9);
        let swapped = jev_answer("2: Lightning Bolt — cast", right.clone(), 0.9);
        for body in [
            wrong_choice,
            wrong_probability,
            invented_extra,
            padded,
            swapped,
        ] {
            assert!(
                matches!(
                    completion_from_response(LlmProvider::Jev, 200, &body, &issued),
                    Err(LlmError::MalformedResponse { .. })
                ),
                "{body}"
            );
        }
        // The control: the same answer with the issued keys is accepted.
        let control = jev_answer("1: Lightning Bolt — cast", right, 0.9);
        assert!(completion_from_response(LlmProvider::Jev, 200, &control, &issued).is_ok());
    }

    #[test]
    fn a_jev_answer_is_checked_against_the_keys_the_request_issued() {
        let prompt = prompt();
        let request = jev_request()["request"].clone();
        let issued_keys: Vec<&String> = request["questions"]["pick"]["criteria"]
            .as_object()
            .unwrap()
            .keys()
            .collect();
        assert_eq!(
            issued_keys.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
            system_one_option_keys(&prompt.frame.options)
        );
    }

    #[test]
    fn a_jev_answer_with_a_non_numeric_probability_is_refused() {
        let body = jev_answer(
            "0: Pass Priority",
            json!({ "0: Pass Priority": "high" }),
            0.5,
        );
        assert!(matches!(
            completion_from_response(LlmProvider::Jev, 200, &body, &prompt().frame.options),
            Err(LlmError::MalformedResponse { .. })
        ));
    }
}
