//! The connection probe: one real request, validated by the real decoder.
//!
//! A "test connection" button that only checks whether bytes came back is worse
//! than no button, because it reports success for the two failures a player is
//! most likely to hit — a rejected key and a wrong model — both of which arrive
//! as a well-formed HTTP response carrying a provider error. The probe
//! therefore exercises the SAME path a game decision does: the engine builds
//! the request, the transport performs it, and the engine extracts and decodes
//! the reply. Anything the game path would refuse, the probe refuses too.

use crate::error::LlmResult;
use crate::prompt::{decode_choice, DecisionFrame, LlmPrompt};
use crate::provider::{HttpRequestSpec, LlmEndpointConfig, LlmProvider, WireProtocol};
use crate::wire::{build_chat_request, build_thread_request, completion_from_response, ThreadTurn};

/// The option count a chat probe offers. One option means a working model has
/// exactly one legal answer, so a decode failure is a real signal about the
/// endpoint rather than a judgement call the model got wrong.
const PROBE_OPTION_COUNT: usize = 1;

/// The option count a System One probe offers: a typed Choice needs at least
/// two options, and either is a legal answer to the probe.
const SYSTEM_ONE_PROBE_OPTION_COUNT: usize = 2;

/// A minimal decision in the same shape as a real one.
///
/// Deliberately tiny: the probe is about reachability and contract, not about
/// play skill, and a player testing a key should not be billed for a full board
/// render.
pub fn connection_probe_prompt() -> LlmPrompt {
    LlmPrompt {
        system: "You are being checked for connectivity by a Magic: The Gathering \
                 client. Answer with JSON and nothing else."
            .to_string(),
        user: "Your legal options:\n  [0] Pass priority\n\nReply with ONLY this JSON \
               object: {\"choice\": 0, \"reason\": \"ok\"}"
            .to_string(),
        frame: DecisionFrame {
            brief: "You are being checked for connectivity by a Magic: The Gathering client."
                .to_string(),
            position: "Nothing is happening: this is a connection test.".to_string(),
            instruction: "Choose either option.".to_string(),
            options: vec!["Pass priority".to_string(), "Take no action".to_string()],
        },
    }
}

/// The probe request for `config`: the [`connection_probe_prompt`] in the
/// shape the endpoint's game decisions use.
///
/// For a profile that keeps its seats on Anthropic message threads, that is a
/// thread create (the `thread` field, the beta header and the top-level
/// `cache_control`), the first request of every seat that uses a thread (Medium
/// difficulty and above), so a probe that passes proves a proxy or gateway in
/// front of the API accepts them too. The thread it creates is never continued, and
/// the API drops it after five idle minutes. Every other profile is probed with
/// exactly the request [`build_chat_request`] builds.
///
/// # Errors
///
/// Whatever the request builder refuses, as [`LlmError`](crate::LlmError).
pub fn build_probe_request(config: &LlmEndpointConfig) -> LlmResult<HttpRequestSpec> {
    let prompt = connection_probe_prompt();
    if config.uses_message_threads() {
        return build_thread_request(config, ThreadTurn::Create { prompt: &prompt });
    }
    build_chat_request(config, &prompt)
}

/// Validate a probe response exactly as a game decision would be validated.
///
/// Surfaces, in order: a non-2xx status (with the vendor's own message when the
/// body carries one); a provider error envelope on an otherwise-2xx response;
/// an empty completion; and a reply the engine cannot bind to a legal option.
pub fn validate_probe_response(provider: LlmProvider, status: u16, body: &str) -> LlmResult<()> {
    let issued = connection_probe_prompt().frame.options;
    let completion = completion_from_response(provider, status, body, &issued)?;
    let option_count = match provider.wire() {
        WireProtocol::SystemOneRelay => SYSTEM_ONE_PROBE_OPTION_COUNT,
        WireProtocol::OpenAiChat
        | WireProtocol::AnthropicMessages
        | WireProtocol::GeminiGenerateContent => PROBE_OPTION_COUNT,
    };
    decode_choice(&completion, option_count, 1)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::LlmError;

    #[test]
    fn a_well_formed_answer_passes() {
        let body = r#"{"choices":[{"message":{"content":"{\"choice\": 0}"}}]}"#;
        assert_eq!(
            validate_probe_response(LlmProvider::OpenAi, 200, body),
            Ok(())
        );
    }

    /// The case a bytes-came-back check gets wrong: a rejected key arrives as a
    /// perfectly well-formed response body.
    #[test]
    fn a_provider_error_envelope_fails_with_the_vendors_message() {
        let body = r#"{"error":{"message":"Incorrect API key provided"}}"#;
        assert_eq!(
            validate_probe_response(LlmProvider::OpenAi, 200, body),
            Err(LlmError::Provider {
                detail: "Incorrect API key provided".to_string()
            })
        );
    }

    /// The other one: an unknown model is a 404 whose body is still JSON.
    #[test]
    fn an_unknown_model_error_fails() {
        let body =
            r#"{"error":{"message":"models/nope is not found for API version v1beta","code":404}}"#;
        assert!(matches!(
            validate_probe_response(LlmProvider::Gemini, 404, body),
            Err(LlmError::Provider { .. })
        ));
    }

    #[test]
    fn a_malformed_body_fails_rather_than_passing() {
        for body in ["", "not json", "<!doctype html><html>404</html>", "{}"] {
            assert!(
                validate_probe_response(LlmProvider::OpenAi, 200, body).is_err(),
                "must reject: {body:?}"
            );
        }
    }

    #[test]
    fn an_empty_completion_fails() {
        let body = r#"{"choices":[{"message":{"content":""}}]}"#;
        assert_eq!(
            validate_probe_response(LlmProvider::OpenAi, 200, body),
            Err(LlmError::EmptyCompletion)
        );
    }

    /// A reachable endpoint whose model will not answer in the agreed shape is
    /// reported, not passed: the game path would refuse it too.
    /// A probe must not report Connected for a non-2xx response, even when its
    /// body would otherwise decode.
    #[test]
    fn a_non_2xx_probe_response_fails_even_with_a_decodable_body() {
        let body = r#"{"choices":[{"message":{"content":"{\"choice\": 0}"}}]}"#;
        for status in [400, 401, 403, 404, 429, 500] {
            assert!(
                validate_probe_response(LlmProvider::OpenAi, status, body).is_err(),
                "HTTP {status} must not report success"
            );
        }
    }

    #[test]
    fn a_reply_the_decoder_cannot_bind_fails() {
        let body = r#"{"choices":[{"message":{"content":"Sure! I would pass priority."}}]}"#;
        assert!(matches!(
            validate_probe_response(LlmProvider::OpenAi, 200, body),
            Err(LlmError::UndecodableChoice { .. })
        ));
    }

    #[test]
    fn the_probe_prompt_offers_exactly_the_option_it_validates_against() {
        let prompt = connection_probe_prompt();
        assert!(prompt.user.contains("[0]"));
        assert!(!prompt.user.contains("[1]"));
    }

    // ── The request a probe sends ────────────────────────────────────────

    fn probed(provider: LlmProvider, context: crate::provider::DecisionContext) -> HttpRequestSpec {
        build_probe_request(&LlmEndpointConfig {
            provider,
            base_url: None,
            api_key: "key".to_string(),
            model: "model-x".to_string(),
            max_output_tokens: None,
            temperature: None,
            decision_context: context,
        })
        .unwrap()
    }

    fn body(spec: &HttpRequestSpec) -> serde_json::Value {
        serde_json::from_str(&spec.body).unwrap()
    }

    /// A profile on message threads is probed with the shape its first game
    /// decision sends, so a gateway that rejects the thread field or the beta
    /// fails the probe instead of every decision of the game.
    #[test]
    fn a_threaded_profile_is_probed_with_a_thread_create() {
        use crate::provider::DecisionContext;
        let spec = probed(LlmProvider::Anthropic, DecisionContext::MessageThread);
        assert_eq!(
            body(&spec)["thread"],
            serde_json::json!({ "type": "create" })
        );
        assert_eq!(
            body(&spec)["cache_control"],
            serde_json::json!({ "type": "ephemeral" })
        );
        assert!(spec
            .headers
            .iter()
            .any(|header| header.name == "anthropic-beta"));
    }

    #[test]
    fn every_other_profile_is_probed_with_todays_request() {
        use crate::provider::DecisionContext;
        for (provider, context) in [
            (LlmProvider::Anthropic, DecisionContext::SelfContained),
            (LlmProvider::OpenAi, DecisionContext::MessageThread),
            (LlmProvider::Gemini, DecisionContext::MessageThread),
        ] {
            let config = LlmEndpointConfig {
                provider,
                base_url: None,
                api_key: "key".to_string(),
                model: "model-x".to_string(),
                max_output_tokens: None,
                temperature: None,
                decision_context: context,
            };
            assert_eq!(
                build_probe_request(&config).unwrap(),
                build_chat_request(&config, &connection_probe_prompt()).unwrap(),
                "{provider:?} under {context:?}"
            );
        }
    }

    /// An organization that cannot use threads fails the probe with the
    /// thread error, which names the cause, rather than with a decode failure.
    #[test]
    fn a_refused_thread_fails_the_probe_with_the_thread_error() {
        let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"no","details":{"error_code":"thread_unsupported_request"}}}"#;
        assert_eq!(
            validate_probe_response(LlmProvider::Anthropic, 400, body),
            Err(LlmError::ThreadUnavailable {
                code: crate::error::ThreadErrorCode::UnsupportedRequest
            })
        );
    }
}
