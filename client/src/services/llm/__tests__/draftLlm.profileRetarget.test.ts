// A persisted zustand store captures its storage when this file's imports are
// evaluated, so the working-localStorage install has to precede them.
import "../../../test/helpers/persistedStorage";

import { beforeEach, describe, expect, it, vi } from "vitest";

// The real production service. The set catalog (a cold fetch) is the boundary
// the test holds open; the engine lease is a pass-through stand-in that records
// the endpoint it is asked to build for. Credential resolution happens inside the
// lease callback, after the catalog await, so the round holds its profile copy
// across a real asynchronous gap.
const gates = vi.hoisted(() => ({
  catalog: null as null | { promise: Promise<unknown>; resolve: (value: unknown) => void },
  endpoints: [] as string[],
}));

vi.mock("../../setCatalog", () => ({
  ensureSetCatalog: () => gates.catalog!.promise,
}));
vi.mock("../../../adapter/draft-adapter", () => ({
  withDraftEngineOperation: async (work: (lease: unknown) => unknown) =>
    work({
      buildLlmDraftPickRequests: (endpointJson: string) => {
        gates.endpoints.push(endpointJson);
        return [];
      },
    }),
}));
vi.mock("../llmClient", () => ({ executeLlmRequest: vi.fn() }));
vi.mock("../../../game/debugLog", () => ({ debugLog: vi.fn() }));

import { cancelLlmDraftRun, collectLlmDraftResponses, resetLlmDraftBreaker } from "../draftLlm";
import { useLlmStore } from "../../../stores/llmStore";
import { useMultiplayerStore } from "../../../stores/multiplayerStore";
import type { LlmEndpointConfig, LlmProfile } from "../types";

function openCatalogGate(): void {
  let resolve!: (value: unknown) => void;
  const promise = new Promise((done) => {
    resolve = done;
  });
  gates.catalog = { promise, resolve };
}

beforeEach(() => {
  gates.endpoints = [];
  openCatalogGate();
  resetLlmDraftBreaker();
  cancelLlmDraftRun();
  useLlmStore.setState({ profiles: [] });
  useMultiplayerStore.setState({ hostingServer: "wss://a.example/ws" });
});

/** Start a round holding `held`, change the store while it awaits, then let it resume. */
async function roundWhile(held: LlmProfile, whileAwaiting: () => void): Promise<LlmEndpointConfig> {
  const round = collectLlmDraftResponses(held, () => true);
  // The round is now parked on the catalog: `held` is a copy across an await.
  whileAwaiting();
  gates.catalog!.resolve({});
  await round;
  expect(gates.endpoints).toHaveLength(1);
  return JSON.parse(gates.endpoints[0]) as LlmEndpointConfig;
}

describe("a draft round resolves its credential and endpoint together", () => {
  function heldJevProfile(): { id: string; held: LlmProfile } {
    const id = useLlmStore.getState().addProfile({
      provider: "Jev",
      model: "jev-latest",
      apiKey: "key-entered-for-a",
      enabled: true,
    });
    const held = useLlmStore.getState().profiles.find((profile) => profile.id === id)!;
    return { id, held };
  }

  it("sends no key when the endpoint is retargeted and re-keyed while the round awaits", async () => {
    const { id, held } = heldJevProfile();

    const endpoint = await roundWhile(held, () => {
      // The player points the profile at relay B and enters B's key.
      useLlmStore.getState().updateProfile(id, {
        baseUrl: "https://b.example",
        apiKey: "key-entered-for-b",
      });
    });

    // The held copy still derives relay A, so B's key must not ride to it.
    expect(endpoint.provider).toBe("Jev");
    expect(endpoint.baseUrl).toBe("https://a.example");
    expect(endpoint.apiKey).toBe("");
    expect(gates.endpoints[0]).not.toContain("key-entered-for-b");
    expect(gates.endpoints[0]).not.toContain("key-entered-for-a");
  });

  it("sends no key when the provider is switched and re-keyed while the round awaits", async () => {
    const { id, held } = heldJevProfile();

    const endpoint = await roundWhile(held, () => {
      useLlmStore.getState().updateProfile(id, {
        provider: "OpenAi",
        model: "gpt-5",
        apiKey: "sk-entered-for-openai",
      });
    });

    expect(endpoint.provider).toBe("Jev");
    expect(endpoint.apiKey).toBe("");
    expect(gates.endpoints[0]).not.toContain("sk-entered-for-openai");
  });

  it("still sends the key for the same endpoint, including one re-entered while it awaits", async () => {
    const { id, held } = heldJevProfile();

    const endpoint = await roundWhile(held, () => {
      useLlmStore.getState().updateProfile(id, { apiKey: "key-rotated-for-a" });
    });

    expect(endpoint.baseUrl).toBe("https://a.example");
    expect(endpoint.apiKey).toBe("key-rotated-for-a");
  });

  it("sends no key to a server the player switched to while the round awaits", async () => {
    const { held } = heldJevProfile();

    const endpoint = await roundWhile(held, () => {
      useMultiplayerStore.getState().setHostingServer("wss://b.example/ws");
    });

    expect(endpoint.baseUrl).toBe("https://b.example");
    expect(endpoint.apiKey).toBe("");
  });
});
