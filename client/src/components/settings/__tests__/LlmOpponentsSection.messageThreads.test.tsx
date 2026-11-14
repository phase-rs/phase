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
  { provider: "Anthropic", value: "Anthropic", displayName: "Anthropic (Claude)", defaultBaseUrl: null, defaultModel: "claude-sonnet-5", requiresApiKey: true, apiKeyUrl: "", models: [], decisionContexts: ["selfContained", "messageThread"] },
  { provider: "OpenAi", value: "OpenAi", displayName: "OpenAI", defaultBaseUrl: null, defaultModel: "gpt-5", requiresApiKey: true, apiKeyUrl: "", models: [], decisionContexts: ["selfContained"] },
] satisfies LlmProviderCatalogEntry[];

const TOGGLE = { name: "Remember earlier decisions" };

afterEach(cleanup);

beforeEach(() => {
  catalogMock.loadProviderCatalog.mockReset();
  catalogMock.loadProviderCatalog.mockResolvedValue(CATALOG_ROWS);
  useLlmStore.setState({
    profiles: [],
    seatBindings: {},
    defaultOpponentProfileId: null,
    draftEnabled: false,
    draftProfileId: null,
  });
});

function addProfile(provider: "Anthropic" | "OpenAi"): string {
  return useLlmStore.getState().addProfile({
    name: provider,
    provider,
    model: provider === "Anthropic" ? "claude-sonnet-5" : "gpt-5",
    apiKey: "sk-test",
  });
}

function decisionContextOf(id: string) {
  return useLlmStore.getState().profiles.find((profile) => profile.id === id)?.decisionContext;
}

describe("message-thread toggle on a provider card", () => {
  it("is off by default and switches the profile onto a message thread", async () => {
    const id = addProfile("Anthropic");
    render(<LlmOpponentsSection />);

    const toggle = await screen.findByRole("switch", TOGGLE);
    expect(toggle).toHaveAttribute("aria-checked", "false");

    fireEvent.click(toggle);
    expect(decisionContextOf(id)).toBe("messageThread");
    expect(screen.getByRole("switch", TOGGLE)).toHaveAttribute("aria-checked", "true");

    // The label is part of the click target.
    fireEvent.click(screen.getByText(TOGGLE.name));
    expect(decisionContextOf(id)).toBe("selfContained");
  });

  it("is only offered for a provider the engine can thread", async () => {
    addProfile("OpenAi");
    render(<LlmOpponentsSection />);

    // The card has rendered its catalog-driven fields...
    expect(await screen.findByText("OpenAI")).toBeInTheDocument();
    // ...and no thread toggle among them.
    expect(screen.queryByRole("switch", TOGGLE)).not.toBeInTheDocument();
  });

  it("goes back to self-contained requests when the provider changes", async () => {
    const id = addProfile("Anthropic");
    useLlmStore.getState().updateProfile(id, { decisionContext: "messageThread" });
    render(<LlmOpponentsSection />);

    fireEvent.click(await screen.findByRole("button", { name: "Provider" }));
    fireEvent.click(await screen.findByRole("option", { name: "OpenAI" }));

    expect(decisionContextOf(id)).toBe("selfContained");
    expect(screen.queryByRole("switch", TOGGLE)).not.toBeInTheDocument();
  });
});
