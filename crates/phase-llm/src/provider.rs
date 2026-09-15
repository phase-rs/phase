use serde::{Deserialize, Serialize};

use crate::error::{LlmError, LlmResult};

/// An LLM vendor a player can point an opponent seat at.
///
/// Modelled as an enum rather than a provider-name string so every consumer
/// (request builder, response parser, catalog, transport) matches exhaustively
/// and a new vendor cannot be half-wired. `OpenAiCompatible` is the open door
/// the product requires: any endpoint speaking the OpenAI chat-completions
/// shape works without a code change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LlmProvider {
    OpenAi,
    Anthropic,
    Gemini,
    DeepSeek,
    /// Any third-party endpoint implementing OpenAI's `/chat/completions`
    /// contract (Ollama, LM Studio, vLLM, OpenRouter, Together, Groq, ...).
    OpenAiCompatible,
}

/// The HTTP contract a provider speaks. Several vendors share one protocol;
/// request building and response parsing dispatch on THIS, never on the vendor,
/// so an OpenAI-compatible vendor needs no parser of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireProtocol {
    /// `POST {base}/chat/completions` — OpenAI, DeepSeek, and every
    /// OpenAI-compatible endpoint.
    OpenAiChat,
    /// `POST {base}/messages` — Anthropic.
    AnthropicMessages,
    /// `POST {base}/models/{model}:generateContent` — Google Gemini.
    GeminiGenerateContent,
}

/// Every label [`LlmProvider::from_label`] maps to a real provider rather than
/// failing. Mirrors `ACCEPTED_DIFFICULTY_LABELS` in `phase_ai::config`: a
/// transport that must *validate* a label references this list instead of
/// restating it. Kept explicit so an error message can name every spelling.
pub const ACCEPTED_PROVIDER_LABELS: &[&str] = &[
    "OpenAi",
    "Anthropic",
    "Gemini",
    "DeepSeek",
    "OpenAiCompatible",
];

impl LlmProvider {
    /// Parse a provider label supplied by a transport boundary (WASM bridge,
    /// persisted settings). Case-insensitive and punctuation-tolerant so
    /// `openai`, `open_ai` and `OpenAI` all land on the same variant.
    ///
    /// Unknown labels resolve to [`LlmProvider::OpenAiCompatible`] rather than
    /// erroring: an unrecognised vendor that speaks the OpenAI contract is
    /// exactly the case this variant exists for.
    pub fn from_label(label: &str) -> LlmProvider {
        let normalized: String = label
            .trim()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect();
        match normalized.as_str() {
            "openai" => LlmProvider::OpenAi,
            "anthropic" | "claude" => LlmProvider::Anthropic,
            "gemini" | "google" | "googleai" => LlmProvider::Gemini,
            "deepseek" => LlmProvider::DeepSeek,
            _ => LlmProvider::OpenAiCompatible,
        }
    }

    /// The stable label this variant serializes to.
    pub const fn label(self) -> &'static str {
        match self {
            LlmProvider::OpenAi => "OpenAi",
            LlmProvider::Anthropic => "Anthropic",
            LlmProvider::Gemini => "Gemini",
            LlmProvider::DeepSeek => "DeepSeek",
            LlmProvider::OpenAiCompatible => "OpenAiCompatible",
        }
    }

    /// Human-facing vendor name.
    pub const fn display_name(self) -> &'static str {
        match self {
            LlmProvider::OpenAi => "OpenAI",
            LlmProvider::Anthropic => "Anthropic (Claude)",
            LlmProvider::Gemini => "Google Gemini",
            LlmProvider::DeepSeek => "DeepSeek",
            LlmProvider::OpenAiCompatible => "OpenAI-compatible endpoint",
        }
    }

    pub const fn wire(self) -> WireProtocol {
        match self {
            LlmProvider::OpenAi | LlmProvider::DeepSeek | LlmProvider::OpenAiCompatible => {
                WireProtocol::OpenAiChat
            }
            LlmProvider::Anthropic => WireProtocol::AnthropicMessages,
            LlmProvider::Gemini => WireProtocol::GeminiGenerateContent,
        }
    }

    /// Default API root. `OpenAiCompatible` has none — the player supplies it,
    /// which is the whole point of that variant.
    pub const fn default_base_url(self) -> Option<&'static str> {
        match self {
            LlmProvider::OpenAi => Some("https://api.openai.com/v1"),
            LlmProvider::Anthropic => Some("https://api.anthropic.com/v1"),
            LlmProvider::Gemini => Some("https://generativelanguage.googleapis.com/v1beta"),
            LlmProvider::DeepSeek => Some("https://api.deepseek.com/v1"),
            LlmProvider::OpenAiCompatible => None,
        }
    }

    /// Whether a request can be built at all without an API key. Local
    /// OpenAI-compatible servers (Ollama, LM Studio) routinely need none, so
    /// only the hosted vendors require one.
    pub const fn requires_api_key(self) -> bool {
        !matches!(self, LlmProvider::OpenAiCompatible)
    }
}

/// One configured endpoint: which vendor, where, with what credential, running
/// which model. This is the persisted unit a player creates in Settings.
///
/// `api_key` reaches Rust only to be placed into the outgoing request's headers
/// and is never logged, echoed into a prompt, or serialized back out.
// No `Eq`: `temperature` is an `f32`, and a float has no total equality.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmEndpointConfig {
    pub provider: LlmProvider,
    /// Overrides [`LlmProvider::default_base_url`]. Trailing slashes are
    /// tolerated.
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: String,
    /// Free-text by design: the catalog is a convenience list, never a gate.
    /// A model released after this build ships works by typing its id.
    pub model: String,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
}

impl LlmEndpointConfig {
    /// The API root this config resolves to.
    ///
    /// Normalizes whatever the player pasted down to the root, because the
    /// natural thing to paste is the URL their provider's docs show — which is
    /// the full per-call URL, not the root. Left unnormalized, appending this
    /// protocol's own path to it produces a doubled path that the vendor 404s
    /// WITHOUT CORS headers, which surfaces in a browser as an unexplained
    /// "Failed to fetch" rather than as a readable provider error.
    pub fn resolved_base_url(&self) -> LlmResult<String> {
        let raw = self
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .or(self.provider.default_base_url())
            .ok_or_else(|| LlmError::Configuration {
                detail: format!(
                    "{} has no default endpoint; enter the base URL of your server",
                    self.provider.display_name()
                ),
            })?;
        Ok(normalize_base_url(self.provider.wire(), raw))
    }

    /// Reject a config that cannot produce a usable request before any network
    /// call is attempted, so the UI can explain the gap instead of surfacing a
    /// provider 401.
    pub fn validate(&self) -> LlmResult<()> {
        if self.model.trim().is_empty() {
            return Err(LlmError::Configuration {
                detail: "no model selected".to_string(),
            });
        }
        if self.provider.requires_api_key() && self.api_key.trim().is_empty() {
            return Err(LlmError::Configuration {
                detail: format!("{} requires an API key", self.provider.display_name()),
            });
        }
        self.resolved_base_url()?;
        Ok(())
    }
}

/// Strip a protocol's own call path off a pasted URL, leaving the API root.
///
/// Idempotent: a URL that is already a root passes through unchanged, so a
/// correctly-entered endpoint is never altered.
fn normalize_base_url(wire: WireProtocol, raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    match wire {
        WireProtocol::OpenAiChat => strip_suffix_path(trimmed, "/chat/completions"),
        WireProtocol::AnthropicMessages => strip_suffix_path(trimmed, "/messages"),
        WireProtocol::GeminiGenerateContent => strip_gemini_call_path(trimmed),
    }
}

fn strip_suffix_path(url: &str, suffix: &str) -> String {
    url.strip_suffix(suffix)
        .unwrap_or(url)
        .trim_end_matches('/')
        .to_string()
}

/// Gemini names the model IN the path (`{root}/models/{model}:{method}`), so
/// recovering the root means dropping everything from `/models/` onward —
/// along with any query string that came with it, which is where a pasted URL
/// carries an inline `?key=`. The key belongs in the API-key field, and a root
/// is the one thing this function is allowed to return.
fn strip_gemini_call_path(url: &str) -> String {
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .trim_end_matches('/');
    if let Some(index) = path.rfind("/models/") {
        return path[..index].to_string();
    }
    strip_suffix_path(path, "/models")
}

/// A fully-formed HTTP call for a transport to execute verbatim.
///
/// The transport's entire job is `fetch(url, { method, headers, body })`. It
/// chooses nothing: not the URL, not the auth scheme, not the payload shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpRequestSpec {
    pub url: String,
    pub method: &'static str,
    pub headers: Vec<HttpHeader>,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpHeader {
    pub name: String,
    pub value: String,
}

impl HttpHeader {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        HttpHeader {
            name: name.into(),
            value: value.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_accepted_label_round_trips() {
        for label in ACCEPTED_PROVIDER_LABELS {
            assert_eq!(LlmProvider::from_label(label).label(), *label);
        }
    }

    #[test]
    fn every_variant_appears_in_the_accepted_labels() {
        // Wildcard-free so a new variant fails to compile here rather than
        // silently escaping the accepted-label list.
        for provider in [
            LlmProvider::OpenAi,
            LlmProvider::Anthropic,
            LlmProvider::Gemini,
            LlmProvider::DeepSeek,
            LlmProvider::OpenAiCompatible,
        ] {
            let label = match provider {
                LlmProvider::OpenAi => "OpenAi",
                LlmProvider::Anthropic => "Anthropic",
                LlmProvider::Gemini => "Gemini",
                LlmProvider::DeepSeek => "DeepSeek",
                LlmProvider::OpenAiCompatible => "OpenAiCompatible",
            };
            assert!(ACCEPTED_PROVIDER_LABELS.contains(&label));
            assert_eq!(LlmProvider::from_label(label), provider);
        }
    }

    #[test]
    fn unknown_vendors_fall_back_to_the_openai_contract() {
        assert_eq!(
            LlmProvider::from_label("my-self-hosted-llama"),
            LlmProvider::OpenAiCompatible
        );
        assert_eq!(
            LlmProvider::OpenAiCompatible.wire(),
            WireProtocol::OpenAiChat
        );
    }

    #[test]
    fn base_url_overrides_win_and_lose_their_trailing_slash() {
        let config = LlmEndpointConfig {
            provider: LlmProvider::OpenAi,
            base_url: Some("https://proxy.internal/v1/".to_string()),
            api_key: "k".to_string(),
            model: "gpt-5".to_string(),
            max_output_tokens: None,
            temperature: None,
        };
        assert_eq!(
            config.resolved_base_url().unwrap(),
            "https://proxy.internal/v1"
        );
    }

    fn base(provider: LlmProvider, url: &str) -> String {
        LlmEndpointConfig {
            provider,
            base_url: Some(url.to_string()),
            api_key: "k".to_string(),
            model: "m".to_string(),
            max_output_tokens: None,
            temperature: None,
        }
        .resolved_base_url()
        .unwrap()
    }

    #[test]
    fn a_pasted_gemini_call_url_normalizes_to_its_root() {
        // Exactly what Google's docs show, which is what a player pastes.
        assert_eq!(
            base(
                LlmProvider::Gemini,
                "https://generativelanguage.googleapis.com/v1beta/models/gemini-flash-latest:generateContent"
            ),
            "https://generativelanguage.googleapis.com/v1beta"
        );
    }

    #[test]
    fn a_pasted_gemini_url_does_not_carry_an_inline_key_into_the_root() {
        assert_eq!(
            base(
                LlmProvider::Gemini,
                "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:streamGenerateContent?key=SECRET"
            ),
            "https://generativelanguage.googleapis.com/v1beta"
        );
    }

    #[test]
    fn a_pasted_openai_or_anthropic_call_url_normalizes_to_its_root() {
        assert_eq!(
            base(
                LlmProvider::OpenAi,
                "https://api.openai.com/v1/chat/completions"
            ),
            "https://api.openai.com/v1"
        );
        assert_eq!(
            base(
                LlmProvider::Anthropic,
                "https://api.anthropic.com/v1/messages"
            ),
            "https://api.anthropic.com/v1"
        );
    }

    #[test]
    fn normalizing_a_root_that_is_already_correct_changes_nothing() {
        for (provider, url) in [
            (
                LlmProvider::Gemini,
                "https://generativelanguage.googleapis.com/v1beta",
            ),
            (LlmProvider::OpenAi, "https://api.openai.com/v1"),
            (LlmProvider::Anthropic, "https://api.anthropic.com/v1"),
            (LlmProvider::OpenAiCompatible, "http://localhost:11434/v1"),
        ] {
            assert_eq!(base(provider, url), url, "{provider:?}");
            // Idempotent: normalizing the result again is a no-op.
            assert_eq!(base(provider, &base(provider, url)), url, "{provider:?}");
        }
    }

    #[test]
    fn a_gemini_root_ending_in_models_is_still_a_root() {
        assert_eq!(
            base(
                LlmProvider::Gemini,
                "https://generativelanguage.googleapis.com/v1beta/models"
            ),
            "https://generativelanguage.googleapis.com/v1beta"
        );
    }

    #[test]
    fn an_openai_compatible_proxy_keeps_a_path_that_merely_resembles_a_call_path() {
        // Only the OWN protocol's call path is stripped: a compatible endpoint
        // is never stripped of "/messages", which is Anthropic's.
        assert_eq!(
            base(
                LlmProvider::OpenAiCompatible,
                "https://proxy.internal/messages"
            ),
            "https://proxy.internal/messages"
        );
    }

    #[test]
    fn a_compatible_endpoint_without_a_base_url_is_a_configuration_error() {
        let config = LlmEndpointConfig {
            provider: LlmProvider::OpenAiCompatible,
            base_url: None,
            api_key: String::new(),
            model: "llama-3".to_string(),
            max_output_tokens: None,
            temperature: None,
        };
        assert!(matches!(
            config.validate(),
            Err(LlmError::Configuration { .. })
        ));
    }

    #[test]
    fn a_local_compatible_endpoint_needs_no_api_key() {
        let config = LlmEndpointConfig {
            provider: LlmProvider::OpenAiCompatible,
            base_url: Some("http://localhost:11434/v1".to_string()),
            api_key: String::new(),
            model: "llama-3".to_string(),
            max_output_tokens: None,
            temperature: None,
        };
        assert_eq!(config.validate(), Ok(()));
    }
}
