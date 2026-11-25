// A persisted zustand store captures its storage when this file's imports are
// evaluated, so the working-localStorage install has to precede them.
import "../../../test/helpers/persistedStorage";

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const catalogMock = vi.hoisted(() => ({ loadProviderCatalog: vi.fn() }));
vi.mock("../../../services/llm/catalog", () => catalogMock);
vi.mock("../../../services/llm/probe", () => ({
  testLlmEndpoint: vi.fn(async () => ({ ok: true })),
}));

import { LlmOpponentsSection } from "../LlmOpponentsSection";
import { useLlmStore } from "../../../stores/llmStore";
import type { LlmProviderCatalogEntry } from "../../../services/llm/types";

const CATALOG_ROWS = [
  {
    provider: "OpenAi",
    value: "OpenAi",
    displayName: "OpenAI",
    defaultBaseUrl: null,
    defaultModel: "gpt-5",
    requiresApiKey: true,
    apiKeyUrl: "",
    models: [],
    decisionContexts: ["selfContained"],
  },
] satisfies LlmProviderCatalogEntry[];

afterEach(cleanup);

beforeEach(() => {
  catalogMock.loadProviderCatalog.mockReset();
  catalogMock.loadProviderCatalog.mockResolvedValue(CATALOG_ROWS);
  sessionStorage.clear();
  useLlmStore.setState({
    profiles: [],
    seatBindings: {},
    defaultOpponentProfileId: null,
    draftEnabled: false,
    draftProfileId: null,
  });
});

describe("remember-API-key toggle on a provider card", () => {
  it("is off by default, uses 44px minimum touch-target sizing on its label, and toggles sessionStorage persistence", async () => {
    const id = useLlmStore.getState().addProfile({
      name: "GPT",
      provider: "OpenAi",
      model: "gpt-5",
      apiKey: "sk-test",
    });
    render(<LlmOpponentsSection />);

    const checkbox = await screen.findByRole("checkbox", { name: /remember for this tab/i });
    expect(checkbox).not.toBeChecked();
    expect(checkbox.closest("label")).toHaveClass("min-h-[44px]");
    expect(sessionStorage.getItem("phase-llm-api-keys")).toBeNull();

    // Clicking the enclosing label toggles the checkbox and stores the key in sessionStorage.
    fireEvent.click(screen.getByText("Remember for this tab (sessionStorage)"));
    expect(useLlmStore.getState().profiles.find((p) => p.id === id)?.rememberApiKey).toBe(true);
    expect(sessionStorage.getItem("phase-llm-api-keys")).not.toBeNull();

    fireEvent.click(checkbox);
    expect(useLlmStore.getState().profiles.find((p) => p.id === id)?.rememberApiKey).toBe(false);
    expect(sessionStorage.getItem("phase-llm-api-keys")).toBeNull();
  });
});
