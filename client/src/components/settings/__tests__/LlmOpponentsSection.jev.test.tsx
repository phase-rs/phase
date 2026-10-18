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
import { profileForSeat, useLlmStore } from "../../../stores/llmStore";
import type { LlmProviderCatalogEntry } from "../../../services/llm/types";

const CATALOG_ROWS = [
  { provider: "OpenAi", value: "OpenAi", displayName: "OpenAI", defaultBaseUrl: "https://api.openai.com/v1", defaultModel: "gpt-5", requiresApiKey: true, apiKeyUrl: "", models: [] },
  { provider: "Jev", value: "Jev", displayName: "Jev (TypeSafe)", defaultBaseUrl: null, defaultModel: "jev-latest", requiresApiKey: true, apiKeyUrl: "https://docs.typesafe.ai/api", models: [{ id: "jev-latest", label: "Jev (latest)" }] },
] satisfies LlmProviderCatalogEntry[];

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

function addJev(patch: Record<string, unknown> = {}): string {
  return useLlmStore.getState().addProfile({
    name: "Jev",
    provider: "Jev",
    model: "jev-latest",
    apiKey: "jev-key",
    enabled: true,
    ...patch,
  });
}

describe("Jev as an AI opponent provider", () => {
  it("is offered beside the other providers, from the engine catalog", async () => {
    addJev();
    render(<LlmOpponentsSection />);

    expect(await screen.findByRole("button", { name: "Provider" })).toHaveTextContent(
      "Jev (TypeSafe)",
    );
    expect(screen.getByRole("button", { name: "Model" })).toHaveTextContent("Jev (latest)");
  });

  it("explains that it is reached through a phase server relay, and only for Jev", async () => {
    const jev = addJev();
    useLlmStore.getState().addProfile({
      name: "GPT", provider: "OpenAi", model: "gpt-5", apiKey: "sk", enabled: true,
    });
    render(<LlmOpponentsSection />);

    const hints = await screen.findAllByText(/relays each request to TypeSafe/i);
    expect(hints).toHaveLength(1);
    expect(useLlmStore.getState().profiles[0].id).toBe(jev);
    // With no endpoint of its own the field says which server it will use.
    expect(screen.getByPlaceholderText("Defaults to your multiplayer server")).toBeInTheDocument();
  });

  it("can seat a game opponent and draft the pod's bots like any other provider", async () => {
    const id = addJev();
    useLlmStore.getState().bindSeat(0, id);
    useLlmStore.getState().setDraftEnabled(true);
    render(<LlmOpponentsSection />);
    await screen.findAllByText(/relays each request to TypeSafe/i);

    expect(profileForSeat(useLlmStore.getState(), 0, CATALOG_ROWS)?.id).toBe(id);
    expect(profileForSeat(useLlmStore.getState(), 1, CATALOG_ROWS)).toBeUndefined();
    expect(screen.getByRole("switch", { name: "Use for draft bots" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
  });

  it("is unusable without a key, like every hosted provider", async () => {
    const id = addJev({ apiKey: "" });
    useLlmStore.getState().bindSeat(0, id);
    render(<LlmOpponentsSection />);
    await screen.findAllByText(/relays each request to TypeSafe/i);

    expect(profileForSeat(useLlmStore.getState(), 0, CATALOG_ROWS)).toBeUndefined();
    fireEvent.click(screen.getByRole("button", { name: "Test connection" }));
  });
});
