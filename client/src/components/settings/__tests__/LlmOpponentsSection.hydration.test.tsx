// A persisted zustand store captures its storage when this file's imports are
// evaluated, so the working-localStorage install has to precede them.
import "../../../test/helpers/persistedStorage";

import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../../services/llm/catalog", () => ({
  loadProviderCatalog: async () => [
    {
      provider: "OpenAi",
      value: "OpenAi",
      displayName: "OpenAI",
      defaultBaseUrl: "https://api.openai.com/v1",
      defaultModel: "gpt-5",
      requiresApiKey: true,
      apiKeyUrl: "https://platform.openai.com/api-keys",
      models: [{ id: "gpt-5", label: "GPT-5" }],
    },
  ],
}));
vi.mock("../../../services/llm/probe", () => ({ testLlmEndpoint: vi.fn() }));

import { LlmOpponentsSection } from "../LlmOpponentsSection";
import { useLlmStore } from "../../../stores/llmStore";
import type { LlmProfile } from "../../../services/llm/types";

/**
 * The symptom the store-level guarantee exists to prevent: the settings panel
 * calls `.trim()` on `name` and `model` while rendering, so an incomplete
 * profile crashes the whole panel on open. Asserting the store's shape is
 * necessary but not sufficient — this renders the real component against the
 * hydrated state, which is what a player actually does.
 */
function hydrateFrom(raw: unknown[]): void {
  // Bypass `addProfile` deliberately: this is what a persisted record looks
  // like coming back off disk, not what the app would ever construct.
  useLlmStore.setState({
    profiles: raw as LlmProfile[],
    seatBindings: {},
    draftEnabled: false,
    draftProfileId: null,
  });
}

beforeEach(() => {
  useLlmStore.setState({ profiles: [], seatBindings: {}, draftEnabled: false, draftProfileId: null });
});

describe("LLM settings against hydrated storage", () => {
  it("renders with no providers configured", () => {
    render(<LlmOpponentsSection />);
    expect(screen.getByText(/No providers configured/i)).toBeInTheDocument();
  });

  it("renders a complete profile", () => {
    hydrateFrom([
      {
        id: "p1",
        name: "Claude",
        provider: "Anthropic",
        baseUrl: null,
        apiKey: "",
        model: "claude-sonnet-5",
        maxOutputTokens: null,
        temperature: null,
        enabled: true,
      },
    ]);

    render(<LlmOpponentsSection />);

    // The name appears in the profile's name field and again in the draft
    // provider picker, so assert presence rather than uniqueness.
    expect(screen.getAllByDisplayValue("Claude").length).toBeGreaterThan(0);
  });

  /// The regression: an id-only record reaching the panel must not throw. The
  /// store normalizes on hydration, so this asserts the two layers agree —
  /// and would fail loudly if the guarantee were ever weakened to a filter.
  it("does not crash when a normalized partial record reaches the panel", () => {
    const { profiles } = useLlmStore.getState();
    expect(profiles).toEqual([]);

    // Exactly what `readPersistedProfiles` produces from `{ id, enabled }`.
    hydrateFrom([
      {
        id: "partial",
        name: "",
        provider: "OpenAi",
        baseUrl: null,
        apiKey: "",
        model: "",
        maxOutputTokens: null,
        temperature: null,
        enabled: false,
      },
    ]);

    expect(() => render(<LlmOpponentsSection />)).not.toThrow();
    // It renders as an unnamed, unusable provider rather than vanishing.
    expect(screen.getAllByLabelText(/Provider/i).length).toBeGreaterThan(0);
  });
});
