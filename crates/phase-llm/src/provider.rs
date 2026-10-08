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
    /// provider 401 — or, worse, putting a credential on the wire in clear.
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
        let base = self.resolved_base_url()?;
        // A credential must never leave the machine in clear text. The wire
        // builder puts the API key in an Authorization / x-api-key /
        // x-goog-api-key header, so a plaintext endpoint would expose it to
        // every hop on the path. Loopback is exempt because the traffic never
        // reaches a network -- that is the local-model case (Ollama, LM Studio)
        // the OpenAI-compatible provider exists for.
        // The endpoint must be ONE spelling, and it must be the spelling the
        // browser resolves without consulting the page. See
        // [`absolute_endpoint_parts`].
        let Some((scheme, authority)) = absolute_endpoint_parts(&base) else {
            return Err(LlmError::Configuration {
                detail: format!(
                    "{base} is not an absolute endpoint URL; it must begin with \
                     https:// (or http:// for a server on localhost)"
                ),
            });
        };
        if !matches!(scheme.as_str(), "http" | "https") {
            return Err(LlmError::Configuration {
                detail: format!("{base} must use https:// (or http:// on localhost)"),
            });
        }
        // A credential must never leave the machine in clear text.
        if !self.api_key.trim().is_empty() && scheme == "http" && !is_loopback_authority(authority)
        {
            return Err(LlmError::Configuration {
                detail: format!(
                    "refusing to send an API key over plaintext HTTP to {base}; \
                     use https:// (a local endpoint on localhost is exempt)"
                ),
            });
        }
        Ok(())
    }
}

/// Split an endpoint into its scheme and authority, accepting ONLY the
/// canonical `scheme://authority` spelling.
///
/// The previous round replaced a `://` substring search with a hand-written
/// subset of the WHATWG parser, so that `http:localhost:11434/v1` was read the
/// way `new URL()` reads it with NO base — as the host `localhost`. That is not
/// how the browser reads it. `fetch` resolves against the page's base URL, and
/// WHATWG's special-relative-or-authority state says that when the input's
/// scheme equals the base's scheme and no `//` follows, the input is RELATIVE.
/// On a page served from `http://example.com/app/`, the approved "loopback"
/// endpoint therefore resolves to
/// `http://example.com/app/localhost:11434/v1/chat/completions` — a plaintext
/// non-loopback request carrying the Authorization header, approved by a guard
/// that had judged a different URL entirely.
///
/// The same seam made an https endpoint contact the wrong host without any
/// plaintext involved: `https:api.example.com/v1` under an https page base
/// resolves to `https://app.example.com/x/api.example.com/v1`.
///
/// So this does not add another spelling exception. It removes them all. Only
/// `scheme://authority` is accepted, which is the one form whose meaning cannot
/// depend on the page: `//` after the scheme sends the parser to the authority
/// state and the base is never consulted. Validation and transmission then
/// describe the same URL by construction, rather than by a subset parser
/// agreeing with the browser.
///
/// Anything else is refused with a message naming the required form, which is
/// also what the settings UI shows and what every catalog default already is.
fn absolute_endpoint_parts(url: &str) -> Option<(String, &str)> {
    let (scheme, authority) = url.split_once("://")?;
    // ALPHA *( ALPHA / DIGIT / "+" / "-" / "." ), per RFC 3986.
    let mut characters = scheme.chars();
    if !characters.next().is_some_and(|c| c.is_ascii_alphabetic())
        || !characters.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return None;
    }
    // `https://` with nothing after it names no host.
    if authority.starts_with(['/', '?', '#']) || authority.is_empty() {
        return None;
    }
    Some((scheme.to_ascii_lowercase(), authority))
}

/// Whether an authority names this machine.
///
/// Covers `localhost` and any `*.localhost` subdomain (RFC 6761 reserves the
/// whole TLD for loopback), the entire `127.0.0.0/8` range, and IPv6 `[::1]`.
/// Userinfo is stripped first so `http://127.0.0.1@evil.example` — which
/// actually resolves to `evil.example` — is not mistaken for loopback.
fn is_loopback_authority(rest: &str) -> bool {
    // `\\` ends the authority exactly as `/` does: a special-scheme URL treats
    // the two interchangeably.
    let authority = rest.split(['/', '\\', '?', '#']).next().unwrap_or_default();
    // Everything before the last '@' is userinfo, not the host.
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = match host_port.strip_prefix('[') {
        // IPv6 literal: the bracketed span is the host, port follows the ']'.
        Some(inner) => inner.split(']').next().unwrap_or_default(),
        None => host_port.split(':').next().unwrap_or_default(),
    };

    if host.eq_ignore_ascii_case("localhost")
        || host.to_ascii_lowercase().ends_with(".localhost")
        || host == "::1"
    {
        return true;
    }
    // 127.0.0.0/8 -- the whole block is loopback, not just 127.0.0.1.
    let mut octets = host.split('.');
    let first = octets.next().and_then(|part| part.parse::<u8>().ok());
    let remaining: Vec<&str> = octets.collect();
    first == Some(127)
        && remaining.len() == 3
        && remaining.iter().all(|part| part.parse::<u8>().is_ok())
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

    fn keyed(url: &str) -> LlmResult<()> {
        LlmEndpointConfig {
            provider: LlmProvider::OpenAiCompatible,
            base_url: Some(url.to_string()),
            api_key: "sk-secret".to_string(),
            model: "m".to_string(),
            max_output_tokens: None,
            temperature: None,
        }
        .validate()
    }

    #[test]
    fn a_credential_is_refused_over_plaintext_http_to_a_remote_host() {
        for url in [
            "http://api.example.com/v1",
            "http://192.168.1.50:8080/v1",
            "HTTP://API.EXAMPLE.COM/v1",
            // Userinfo that merely LOOKS like loopback: this resolves to
            // evil.example, so it must not be treated as local.
            "http://127.0.0.1@evil.example/v1",
        ] {
            assert!(
                matches!(keyed(url), Err(LlmError::Configuration { .. })),
                "must refuse a key over: {url}"
            );
        }
    }

    #[test]
    fn a_credential_is_allowed_to_loopback_and_to_https() {
        for url in [
            "http://localhost:11434/v1",
            "http://127.0.0.1:1234/v1",
            "http://127.1.2.3:1234/v1",
            "http://[::1]:8080/v1",
            "http://ollama.localhost/v1",
            "https://api.example.com/v1",
        ] {
            assert_eq!(keyed(url), Ok(()), "must allow a key over: {url}");
        }
    }

    /// The rule is about protecting a CREDENTIAL, not about banning plaintext:
    /// a keyless local server stays reachable over http.
    #[test]
    fn a_keyless_endpoint_is_unaffected_by_the_plaintext_rule() {
        let config = LlmEndpointConfig {
            provider: LlmProvider::OpenAiCompatible,
            base_url: Some("http://gpu-box.internal:8000/v1".to_string()),
            api_key: String::new(),
            model: "llama-3".to_string(),
            max_output_tokens: None,
            temperature: None,
        };
        assert_eq!(config.validate(), Ok(()));
    }

    /// `build_chat_request` calls `validate` first, so the refusal happens
    /// before a credential is ever placed into a header.
    #[test]
    fn no_request_is_built_for_a_credentialed_plaintext_endpoint() {
        let config = LlmEndpointConfig {
            provider: LlmProvider::OpenAiCompatible,
            base_url: Some("http://api.example.com/v1".to_string()),
            api_key: "sk-secret".to_string(),
            model: "m".to_string(),
            max_output_tokens: None,
            temperature: None,
        };
        let prompt = crate::prompt::LlmPrompt {
            system: "s".to_string(),
            user: "u".to_string(),
        };
        assert!(matches!(
            crate::wire::build_chat_request(&config, &prompt),
            Err(LlmError::Configuration { .. })
        ));
    }

    /// The finding: the safety check searched for a literal `://`, so a URL
    /// spelled without one was reported safe while the browser's parser
    /// resolved it to the very host the check exists to refuse.
    ///
    /// Every string here was run through `new URL(..)` and observed to
    /// normalize to `http://example.com/v1`, which is what makes them
    /// credential-exposing rather than merely odd.
    #[test]
    fn a_credential_is_refused_over_plaintext_spellings_that_omit_the_double_slash() {
        for url in [
            // The two reported spellings.
            "http:example.com/v1",
            "http:/example.com/v1",
            // The rest of the same class: case, backslashes (a special-scheme
            // URL treats `\` as `/`), longer slash runs, mixed separators, and
            // a tab, which the parser strips from anywhere in the input.
            "HTTP:example.com/v1",
            r"http:\\example.com/v1",
            "http:///example.com/v1",
            "http:////example.com/v1",
            r"http:/\example.com/v1",
            "http:\t/example.com/v1",
            "  http:example.com/v1  ",
        ] {
            assert!(
                matches!(keyed(url), Err(LlmError::Configuration { .. })),
                "must refuse a key over: {url:?}"
            );
        }
    }

    /// The allow-list refuses what it cannot prove safe. A relative or
    /// protocol-relative endpoint inherits the PAGE's scheme, which the engine
    /// cannot see, so it is not a form a credential may ride on.
    #[test]
    fn a_credential_is_refused_for_endpoints_with_no_absolute_scheme() {
        for url in [
            "//example.com/v1",
            "example.com/v1",
            "/api/llm-proxy",
            "ftp://example.com/v1",
            "https://",
            "",
        ] {
            assert!(
                matches!(keyed(url), Err(LlmError::Configuration { .. })),
                "must refuse a key over: {url:?}"
            );
        }
    }

    /// INVERTED from the previous round, which asserted these were allowed.
    ///
    /// They were approved because a hand-written parser read them the way
    /// `new URL()` does with NO base. `fetch` has a base — the page — and under
    /// WHATWG's special-relative-or-authority rule a same-scheme input with no
    /// `//` is RELATIVE. So `http:localhost:11434/v1` on a page served from
    /// `http://example.com/app/` is not loopback at all; it is
    /// `http://example.com/app/localhost:11434/v1`, plaintext, non-loopback,
    /// carrying the key. Approving them was the defect.
    #[test]
    fn base_dependent_spellings_of_loopback_and_https_are_refused() {
        for url in [
            "http:localhost:11434/v1",
            "http:/127.0.0.1:1234/v1",
            "http:///localhost:8080/v1",
            "https:api.example.com/v1",
            "HTTPS:api.example.com/v1",
        ] {
            assert!(
                matches!(keyed(url), Err(LlmError::Configuration { .. })),
                "must refuse a base-dependent spelling: {url:?}"
            );
        }
    }

    /// The canonical positive controls: these keep working, and they are the
    /// only form the settings UI and every catalog default produce.
    #[test]
    fn canonical_https_and_loopback_endpoints_are_allowed() {
        for url in [
            "https://api.example.com/v1",
            "HTTPS://API.EXAMPLE.COM/v1",
            "http://localhost:11434/v1",
            "http://127.0.0.1:1234/v1",
            "http://127.1.2.3:1234/v1",
            "http://[::1]:8080/v1",
            "http://ollama.localhost/v1",
        ] {
            assert_eq!(keyed(url), Ok(()), "must allow: {url:?}");
        }
    }

    /// The end of the path the finding actually walked: a Request is built and
    /// the Authorization header rides along. Asserted on the built spec rather
    /// than on `validate`, so this fails if the refusal is ever moved out of
    /// `build_chat_request`.
    #[test]
    fn no_credentialed_request_is_ever_built_for_a_noncanonical_plaintext_url() {
        let prompt = crate::prompt::LlmPrompt {
            system: "s".to_string(),
            user: "u".to_string(),
        };
        for url in ["http:example.com/v1", "http:/example.com/v1"] {
            let config = LlmEndpointConfig {
                provider: LlmProvider::OpenAiCompatible,
                base_url: Some(url.to_string()),
                api_key: "sk-secret".to_string(),
                model: "m".to_string(),
                max_output_tokens: None,
                temperature: None,
            };
            let built = crate::wire::build_chat_request(&config, &prompt);
            assert!(
                matches!(built, Err(LlmError::Configuration { .. })),
                "a request was built for {url:?}: {built:?}"
            );
            // Nothing carrying the key exists to be sent.
            if let Ok(spec) = built {
                assert!(
                    !spec
                        .headers
                        .iter()
                        .any(|header| header.value.contains("sk-secret")),
                    "credential reached a header for {url:?}"
                );
            }
        }
    }

    /// A keyless endpoint still may not be base-dependent. No credential is at
    /// stake, but the host the engine validated would not be the host contacted
    /// — the request would silently go to a path on the page's own origin. The
    /// canonical plaintext spelling keeps working: the rule protects a
    /// credential, it does not ban plaintext.
    #[test]
    fn a_keyless_endpoint_must_still_be_absolute() {
        let keyless = |url: &str| {
            LlmEndpointConfig {
                provider: LlmProvider::OpenAiCompatible,
                base_url: Some(url.to_string()),
                api_key: String::new(),
                model: "llama-3".to_string(),
                max_output_tokens: None,
                temperature: None,
            }
            .validate()
        };
        assert_eq!(keyless("http://gpu-box.internal:8000/v1"), Ok(()));
        for url in ["http:gpu-box.internal:8000/v1", "//gpu-box.internal/v1"] {
            assert!(
                matches!(keyless(url), Err(LlmError::Configuration { .. })),
                "keyless must refuse a base-dependent spelling: {url:?}"
            );
        }
    }

    /// The regression the previous round lacked: the property asserted at the
    /// REQUEST BOUNDARY, under a real document base, using an established URL
    /// implementation rather than this module's own reading.
    ///
    /// The earlier tests normalized with no base, which is precisely the case
    /// that does not occur — `fetch` always resolves against the page. This
    /// resolves each emitted request URL the way a browser would, from three
    /// document bases including an HTTP-served page, and requires the host
    /// contacted to be the host `validate` approved.
    #[test]
    fn every_emitted_request_url_resolves_to_the_validated_host_from_any_document_base() {
        use url::Url;

        // An HTTP-served app is the case that broke: a same-scheme base is what
        // makes a `//`-less input relative.
        let bases = [
            None,
            Some("http://example.com/app/"),
            Some("https://app.example.com/x/"),
        ];
        let prompt = crate::prompt::LlmPrompt {
            system: "s".to_string(),
            user: "u".to_string(),
        };

        for (provider, endpoint, expected_host) in [
            (
                LlmProvider::OpenAiCompatible,
                "http://localhost:11434/v1",
                "localhost",
            ),
            (
                LlmProvider::OpenAiCompatible,
                "http://127.0.0.1:1234/v1",
                "127.0.0.1",
            ),
            (
                LlmProvider::OpenAi,
                "https://api.openai.com/v1",
                "api.openai.com",
            ),
            (
                LlmProvider::Anthropic,
                "https://api.anthropic.com/v1",
                "api.anthropic.com",
            ),
            (
                LlmProvider::Gemini,
                "https://generativelanguage.googleapis.com/v1beta",
                "generativelanguage.googleapis.com",
            ),
        ] {
            let config = LlmEndpointConfig {
                provider,
                base_url: Some(endpoint.to_string()),
                api_key: "sk-secret".to_string(),
                model: "m".to_string(),
                max_output_tokens: None,
                temperature: None,
            };
            let spec = crate::wire::build_chat_request(&config, &prompt)
                .unwrap_or_else(|error| panic!("{endpoint} must build: {error:?}"));

            for base in bases {
                let resolved = match base {
                    Some(base) => Url::options()
                        .base_url(Some(&Url::parse(base).expect("base parses")))
                        .parse(&spec.url),
                    None => Url::parse(&spec.url),
                }
                .unwrap_or_else(|error| panic!("{endpoint} from base {base:?}: {error:?}"));

                assert_eq!(
                    resolved.host_str(),
                    Some(expected_host),
                    "{endpoint} resolved to the wrong host from base {base:?}: {resolved}"
                );
                // A loopback approval must stay loopback, and an https approval
                // must stay encrypted, from every base.
                if endpoint.starts_with("https://") {
                    assert_eq!(resolved.scheme(), "https", "{endpoint} lost https");
                }
            }
        }
    }

    /// The same property, stated as the negative it protects: were a
    /// base-dependent spelling ever approved again, this shows what it would
    /// mean — the emitted URL resolving onto the page's own origin, with the
    /// credential attached.
    #[test]
    fn a_base_dependent_spelling_would_resolve_off_loopback_which_is_why_it_is_refused() {
        use url::Url;

        let page = Url::parse("http://example.com/app/").expect("base parses");
        let resolved = page
            .join("http:localhost:11434/v1/chat/completions")
            .expect("joins");
        assert_eq!(resolved.host_str(), Some("example.com"));
        assert_eq!(resolved.scheme(), "http");

        // Which is exactly why the config that would have emitted it is refused.
        assert!(matches!(
            keyed("http:localhost:11434/v1"),
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
