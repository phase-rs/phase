// A persisted zustand store captures its storage when this file's imports are
// evaluated, so the working-localStorage install has to precede them.
import "../../../test/helpers/persistedStorage";

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const catalogMock = vi.hoisted(() => ({ loadProviderCatalog: vi.fn() }));
vi.mock("../../../services/llm/catalog", () => catalogMock);
const probe = vi.hoisted(() => ({
  testLlmEndpoint: vi.fn<() => Promise<{ ok: true }>>(async () => ({ ok: true })),
}));
vi.mock("../../../services/llm/probe", () => probe);

import { LlmOpponentsSection } from "../LlmOpponentsSection";
import { profileForSeat, useLlmStore } from "../../../stores/llmStore";
import type { LlmProviderCatalogEntry } from "../../../services/llm/types";

const CATALOG_ROWS = [
  { provider: "Anthropic", value: "Anthropic", displayName: "Anthropic", defaultBaseUrl: null, defaultModel: "claude-sonnet-5", requiresApiKey: true, apiKeyUrl: "", models: [] },
  { provider: "OpenAi", value: "OpenAi", displayName: "OpenAI", defaultBaseUrl: null, defaultModel: "gpt-5", requiresApiKey: true, apiKeyUrl: "", models: [] },
  { provider: "OpenAiCompatible", value: "OpenAiCompatible", displayName: "Compatible", defaultBaseUrl: null, defaultModel: "llama3", requiresApiKey: false, apiKeyUrl: "", models: [] },
] satisfies LlmProviderCatalogEntry[];

const PROMPT = /use this provider as your default opponent\?/i;
const CONNECTED_PROMPT = /connected\. use this as your default opponent\?/i;

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
  probe.testLlmEndpoint.mockClear();
});

function addProfile(patch: Parameters<ReturnType<typeof useLlmStore.getState>["addProfile"]>[0]) {
  return useLlmStore.getState().addProfile({
    name: "Claude",
    provider: "Anthropic",
    model: "claude-sonnet-5",
    apiKey: "sk-test",
    enabled: true,
    ...patch,
  });
}

describe("default-opponent prompt on a provider card", () => {
  it("is offered for a usable provider, and one click makes it the default", async () => {
    const id = addProfile({});
    render(<LlmOpponentsSection />);

    fireEvent.click(await screen.findByRole("button", { name: /use as default/i }));

    expect(useLlmStore.getState().defaultOpponentProfileId).toBe(id);
    expect(profileForSeat(useLlmStore.getState(), 0, [{ provider: "Anthropic", value: "Anthropic", displayName: "Anthropic", defaultBaseUrl: null, defaultModel: "claude-sonnet-5", requiresApiKey: true, apiKeyUrl: "", models: [] }])?.id).toBe(id);
  });

  it("collapses to a badge once the provider is the default", async () => {
    const id = addProfile({});
    useLlmStore.getState().setDefaultOpponentProfileId(id);
    render(<LlmOpponentsSection />);

    expect(screen.queryByText(PROMPT)).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /use as default/i })).not.toBeInTheDocument();
    expect(await screen.findByRole("status", { name: "" })).toHaveTextContent(/default opponent/i);
  });

  it("is not offered for a provider the game would ignore", async () => {
    // Keys are memory-only, so a reloaded profile has none: it cannot be used,
    // and inviting the player to default to it would set up a silent no-op.
    addProfile({ provider: "OpenAi", model: "gpt-5", apiKey: "" });
    render(<LlmOpponentsSection />);

    expect(await screen.findByText(/enter your API key again/i)).toBeInTheDocument();
    expect(screen.queryByText(PROMPT)).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /use as default/i })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Default opponent" }));
    expect(screen.queryByRole("option", { name: "Claude" })).not.toBeInTheDocument();
    expect(screen.getByRole("switch", { name: "Use for draft bots" })).toBeDisabled();
    expect(screen.queryByRole("button", { name: "Draft provider" })).not.toBeInTheDocument();
  });

  it("offers a normally key-required provider when its loaded catalog row is keyless", async () => {
    catalogMock.loadProviderCatalog.mockResolvedValue(CATALOG_ROWS.map((row) =>
      row.provider === "OpenAi" ? { ...row, requiresApiKey: false } : row,
    ));
    const id = addProfile({ provider: "OpenAi", model: "gpt-5", apiKey: "" });
    render(<LlmOpponentsSection />);

    expect(await screen.findByRole("button", { name: /use as default/i })).toBeInTheDocument();
    expect(screen.queryByText(/enter your API key again/i)).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Default opponent" }));
    fireEvent.click(screen.getByRole("option", { name: "Claude" }));
    expect(useLlmStore.getState().defaultOpponentProfileId).toBe(id);

    const draftSwitch = screen.getByRole("switch", { name: "Use for draft bots" });
    expect(draftSwitch).toBeEnabled();
    fireEvent.click(draftSwitch);
    fireEvent.click(await screen.findByRole("button", { name: "Draft provider" }));
    expect(screen.getByRole("option", { name: "Claude" })).toBeInTheDocument();
  });

  it("offers a catalog-keyless provider without a key", async () => {
    addProfile({ provider: "OpenAiCompatible", model: "llama3", apiKey: "" });
    render(<LlmOpponentsSection />);

    expect(await screen.findByRole("button", { name: /use as default/i })).toBeInTheDocument();
    expect(screen.queryByText(/enter your API key again/i)).not.toBeInTheDocument();
  });

  it("is not offered for a disabled provider", () => {
    addProfile({ enabled: false });
    render(<LlmOpponentsSection />);

    expect(screen.queryByRole("button", { name: /use as default/i })).not.toBeInTheDocument();
  });

  it("acknowledges a passing connection test in its wording", async () => {
    addProfile({});
    render(<LlmOpponentsSection />);
    expect(await screen.findByText(PROMPT)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /test connection/i }));

    await waitFor(() => expect(screen.getByText(CONNECTED_PROMPT)).toBeInTheDocument());
    expect(probe.testLlmEndpoint).toHaveBeenCalledTimes(1);
  });
});
