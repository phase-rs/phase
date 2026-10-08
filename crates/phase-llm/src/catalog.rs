//! The engine-owned model catalog.
//!
//! The settings UI renders exactly what this publishes — vendor list, default
//! endpoint, suggested models — so the display layer never carries a hardcoded
//! list of its own. The catalog is deliberately a *convenience*, not a gate:
//! [`LlmEndpointConfig::model`](crate::provider::LlmEndpointConfig::model) is
//! free text, so a model released after this build ships is reachable by typing
//! its id. Nothing in request building consults this table.

use serde::Serialize;

use crate::provider::LlmProvider;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelOption {
    /// The exact id sent as the request's `model` field.
    pub id: &'static str,
    /// Short human label for the dropdown row.
    pub label: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCatalogEntry {
    pub provider: LlmProvider,
    /// Stable label, matching `LlmProvider::label`, for round-tripping a
    /// selection through persisted settings.
    pub value: &'static str,
    pub display_name: &'static str,
    pub default_base_url: Option<&'static str>,
    pub default_model: &'static str,
    pub requires_api_key: bool,
    /// Where a player goes to create a key. Rendered as a link beside the key
    /// field; empty for the generic compatible endpoint, which has no vendor.
    pub api_key_url: &'static str,
    pub models: &'static [ModelOption],
}

const OPENAI_MODELS: &[ModelOption] = &[
    ModelOption {
        id: "gpt-5",
        label: "GPT-5",
    },
    ModelOption {
        id: "gpt-5-mini",
        label: "GPT-5 mini",
    },
    ModelOption {
        id: "gpt-5-nano",
        label: "GPT-5 nano",
    },
    ModelOption {
        id: "gpt-4.1",
        label: "GPT-4.1",
    },
    ModelOption {
        id: "gpt-4.1-mini",
        label: "GPT-4.1 mini",
    },
    ModelOption {
        id: "gpt-4o",
        label: "GPT-4o",
    },
    ModelOption {
        id: "o4-mini",
        label: "o4-mini (reasoning)",
    },
    ModelOption {
        id: "o3",
        label: "o3 (reasoning)",
    },
];

const ANTHROPIC_MODELS: &[ModelOption] = &[
    ModelOption {
        id: "claude-opus-5",
        label: "Claude Opus 5",
    },
    ModelOption {
        id: "claude-sonnet-5",
        label: "Claude Sonnet 5",
    },
    ModelOption {
        id: "claude-fable-5-1",
        label: "Claude Fable 5.1",
    },
    ModelOption {
        id: "claude-haiku-4-5-20251001",
        label: "Claude Haiku 4.5",
    },
];

// Google publishes rolling `-latest` aliases alongside pinned ids, and the
// pinned ids are retired as the line moves on (`gemini-2.0-flash` is already
// gone from live accounts). The aliases lead the list because they are the only
// entries that cannot rot; the pinned ids follow for anyone who needs to hold a
// model steady. Access varies by account, so this is a shortlist, not a claim
// about availability — the free-text field reaches anything `ListModels`
// reports, including the preview and 3.x lines.
const GEMINI_MODELS: &[ModelOption] = &[
    ModelOption {
        id: "gemini-flash-latest",
        label: "Gemini Flash (latest)",
    },
    ModelOption {
        id: "gemini-flash-lite-latest",
        label: "Gemini Flash-Lite (latest)",
    },
    ModelOption {
        id: "gemini-pro-latest",
        label: "Gemini Pro (latest)",
    },
    ModelOption {
        id: "gemini-2.5-pro",
        label: "Gemini 2.5 Pro",
    },
    ModelOption {
        id: "gemini-2.5-flash",
        label: "Gemini 2.5 Flash",
    },
    ModelOption {
        id: "gemini-2.5-flash-lite",
        label: "Gemini 2.5 Flash-Lite",
    },
];

// DeepSeek renamed its line: the current Models & Pricing page names
// `deepseek-flash` and `deepseek-v4-pro`. The previous `deepseek-chat` /
// `deepseek-reasoner` ids are gone from the catalog rather than kept as dead
// rows -- a listed id that 404s is worse than an absent one, because the player
// has no reason to doubt it. Anything not listed remains reachable through the
// free-text field.
const DEEPSEEK_MODELS: &[ModelOption] = &[
    ModelOption {
        id: "deepseek-flash",
        label: "DeepSeek Flash",
    },
    ModelOption {
        id: "deepseek-v4-pro",
        label: "DeepSeek V4 Pro",
    },
];

/// No suggestions: a compatible endpoint's model ids are whatever the operator
/// loaded. The UI shows the free-text field alone for this provider.
const COMPATIBLE_MODELS: &[ModelOption] = &[];

const CATALOG: &[ProviderCatalogEntry] = &[
    ProviderCatalogEntry {
        provider: LlmProvider::OpenAi,
        value: "OpenAi",
        display_name: "OpenAI",
        default_base_url: Some("https://api.openai.com/v1"),
        default_model: "gpt-5",
        requires_api_key: true,
        api_key_url: "https://platform.openai.com/api-keys",
        models: OPENAI_MODELS,
    },
    ProviderCatalogEntry {
        provider: LlmProvider::Anthropic,
        value: "Anthropic",
        display_name: "Anthropic (Claude)",
        default_base_url: Some("https://api.anthropic.com/v1"),
        default_model: "claude-sonnet-5",
        requires_api_key: true,
        api_key_url: "https://console.anthropic.com/settings/keys",
        models: ANTHROPIC_MODELS,
    },
    ProviderCatalogEntry {
        provider: LlmProvider::Gemini,
        value: "Gemini",
        display_name: "Google Gemini",
        default_base_url: Some("https://generativelanguage.googleapis.com/v1beta"),
        default_model: "gemini-flash-latest",
        requires_api_key: true,
        api_key_url: "https://aistudio.google.com/apikey",
        models: GEMINI_MODELS,
    },
    ProviderCatalogEntry {
        provider: LlmProvider::DeepSeek,
        value: "DeepSeek",
        display_name: "DeepSeek",
        default_base_url: Some("https://api.deepseek.com/v1"),
        default_model: "deepseek-flash",
        requires_api_key: true,
        api_key_url: "https://platform.deepseek.com/api_keys",
        models: DEEPSEEK_MODELS,
    },
    ProviderCatalogEntry {
        provider: LlmProvider::OpenAiCompatible,
        value: "OpenAiCompatible",
        display_name: "OpenAI-compatible endpoint",
        default_base_url: None,
        default_model: "",
        requires_api_key: false,
        api_key_url: "",
        models: COMPATIBLE_MODELS,
    },
];

/// Every provider the settings UI offers, in display order.
pub fn provider_catalog() -> &'static [ProviderCatalogEntry] {
    CATALOG
}

pub fn catalog_entry(provider: LlmProvider) -> &'static ProviderCatalogEntry {
    CATALOG
        .iter()
        .find(|entry| entry.provider == provider)
        .expect("every LlmProvider variant has a catalog entry")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ACCEPTED_PROVIDER_LABELS;

    #[test]
    fn the_catalog_covers_every_provider_exactly_once() {
        assert_eq!(CATALOG.len(), ACCEPTED_PROVIDER_LABELS.len());
        for label in ACCEPTED_PROVIDER_LABELS {
            assert_eq!(
                CATALOG.iter().filter(|entry| entry.value == *label).count(),
                1,
                "{label} must appear exactly once in the catalog"
            );
        }
    }

    #[test]
    fn catalog_rows_agree_with_the_provider_enum() {
        for entry in CATALOG {
            assert_eq!(entry.value, entry.provider.label());
            assert_eq!(entry.display_name, entry.provider.display_name());
            assert_eq!(entry.default_base_url, entry.provider.default_base_url());
            assert_eq!(entry.requires_api_key, entry.provider.requires_api_key());
        }
    }

    #[test]
    fn every_suggested_default_model_is_one_of_its_own_suggestions() {
        for entry in CATALOG {
            if entry.models.is_empty() {
                assert!(entry.default_model.is_empty());
                continue;
            }
            assert!(
                entry.models.iter().any(|m| m.id == entry.default_model),
                "{} default model is not in its own list",
                entry.display_name
            );
        }
    }
}
