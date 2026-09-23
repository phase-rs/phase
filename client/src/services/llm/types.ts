/**
 * TypeScript mirrors of the engine's LLM types (`crates/phase-llm`).
 *
 * These are wire shapes only. Nothing in this directory decides what a model is
 * asked, which endpoint is called, how a reply is interpreted, or what happens
 * when one fails — the engine owns all of that. The display layer stores the
 * player's configuration and performs the HTTP call the engine describes.
 */

/** Matches `phase_llm::provider::LlmProvider`'s serialized labels. */
export type LlmProviderId =
  | "OpenAi"
  | "Anthropic"
  | "Gemini"
  | "DeepSeek"
  | "OpenAiCompatible";

/** One row of `phase_llm::catalog::provider_catalog`. */
export interface LlmModelOption {
  id: string;
  label: string;
}

export interface LlmProviderCatalogEntry {
  provider: LlmProviderId;
  value: LlmProviderId;
  displayName: string;
  defaultBaseUrl: string | null;
  defaultModel: string;
  requiresApiKey: boolean;
  apiKeyUrl: string;
  models: LlmModelOption[];
}

/** Matches `phase_llm::provider::LlmEndpointConfig`. */
export interface LlmEndpointConfig {
  provider: LlmProviderId;
  baseUrl: string | null;
  apiKey: string;
  model: string;
  maxOutputTokens: number | null;
  temperature: number | null;
}

/** Matches `phase_llm::provider::HttpRequestSpec` — executed verbatim. */
export interface LlmHttpRequestSpec {
  url: string;
  method: string;
  headers: { name: string; value: string }[];
  body: string;
}

/** Engine output for one game decision request. */
export interface LlmDecisionRequest {
  fingerprint: string;
  optionCount: number;
  request: LlmHttpRequestSpec;
}

/** Engine output for one draft seat's pick request. */
export interface LlmDraftPickRequest {
  seat: number;
  fingerprint: string;
  optionCount: number;
  requiredPickCount: number;
  request: LlmHttpRequestSpec;
}

/** What the engine did with one seat's draft response. */
export interface LlmDraftOutcome {
  seat: number;
  used: boolean;
  reasoning?: string;
  error?: string;
}

/**
 * A saved endpoint the player can bind to an AI seat.
 *
 * `apiKey` lives in this record and is deliberately excluded from backup export
 * and cloud sync — see `LLM_ENDPOINTS_KEY` in `constants/storage.ts`.
 */
export interface LlmProfile extends LlmEndpointConfig {
  id: string;
  /** Player-chosen name shown in the seat picker ("Claude", "local llama"). */
  name: string;
  /** Off by default and off after any edit that invalidates the config. */
  enabled: boolean;
}

/** The engine-described endpoint half of a profile, without the UI fields. */
export function endpointOf(profile: LlmProfile): LlmEndpointConfig {
  return {
    provider: profile.provider,
    baseUrl: profile.baseUrl,
    apiKey: profile.apiKey,
    model: profile.model,
    maxOutputTokens: profile.maxOutputTokens,
    temperature: profile.temperature,
  };
}

/**
 * Phase-authored failure reasons, as codes rather than prose.
 *
 * The UI translates these; the accompanying `detail` is NOT translated because
 * it is data — a vendor's own diagnostic ("Incorrect API key provided",
 * "models/x is not found"), which is the most useful part of the message and
 * must survive verbatim. Only sentences this project wrote become codes.
 */
export type LlmMessageCode =
  | "engineUnavailable"
  | "requestNotBuilt"
  | "undecodable"
  | "emptyBody"
  | "cancelled"
  | "timedOut"
  | "unreachable"
  | "oversized";

export interface LlmFailure {
  code: LlmMessageCode;
  /** Provider- or engine-authored diagnostic, shown verbatim. */
  detail?: string;
}
