// A persisted zustand store captures its storage when this file's imports are
// evaluated, so the working-localStorage install has to precede them.
import "../../../test/helpers/persistedStorage";

import { beforeEach, describe, expect, it } from "vitest";

import { useMultiplayerStore } from "../../../stores/multiplayerStore";
import { useLlmStore } from "../../../stores/llmStore";
import { resolvedEndpointOf } from "../endpoint";
import type { LlmProfile } from "../types";

function profile(patch: Partial<LlmProfile>): LlmProfile {
  return {
    id: "p",
    name: "",
    provider: "OpenAi",
    baseUrl: null,
    apiKey: "k",
    model: "m",
    maxOutputTokens: null,
    temperature: null,
    enabled: true,
    ...patch,
  };
}

describe("resolvedEndpointOf", () => {
  beforeEach(() => {
    useMultiplayerStore.setState({ hostingServer: "wss://phase.example/ws" });
  });

  it("points a Jev profile at the hosting server's relay origin", () => {
    expect(resolvedEndpointOf(profile({ provider: "Jev" })).baseUrl).toBe(
      "https://phase.example",
    );
  });

  it("lets a Jev profile's own endpoint win", () => {
    expect(
      resolvedEndpointOf(profile({ provider: "Jev", baseUrl: "https://mine.example" })).baseUrl,
    ).toBe("https://mine.example");
  });

  it("leaves a Jev profile unresolved when no server address can be derived", () => {
    useMultiplayerStore.setState({ hostingServer: "not a url" });
    expect(resolvedEndpointOf(profile({ provider: "Jev" })).baseUrl).toBeNull();
  });

  it("passes every other provider through as saved", () => {
    expect(resolvedEndpointOf(profile({ provider: "OpenAi" })).baseUrl).toBeNull();
    expect(
      resolvedEndpointOf(profile({ provider: "OpenAiCompatible", baseUrl: "http://localhost:1/v1" }))
        .baseUrl,
    ).toBe("http://localhost:1/v1");
  });
});

describe("a Jev key stays bound to the relay it was entered for", () => {
  function jevWithKey(patch: Partial<LlmProfile> = {}): string {
    return useLlmStore.getState().addProfile({
      provider: "Jev",
      model: "jev-latest",
      apiKey: "jev-key",
      enabled: true,
      ...patch,
    });
  }

  function endpointFor(id: string) {
    const stored = useLlmStore.getState().profiles.find((candidate) => candidate.id === id);
    if (!stored) throw new Error("profile missing");
    return resolvedEndpointOf(stored);
  }

  beforeEach(() => {
    useLlmStore.setState({ profiles: [] });
    useMultiplayerStore.setState({ hostingServer: "wss://a.example/ws" });
  });

  it("never hands the key to a server the player switches to afterwards", () => {
    const id = jevWithKey();
    expect(endpointFor(id)).toMatchObject({ baseUrl: "https://a.example", apiKey: "jev-key" });

    // The real setter, as the host-server picker calls it.
    useMultiplayerStore.getState().setHostingServer("wss://b.example/ws");

    const after = endpointFor(id);
    expect(after.baseUrl).toBe("https://b.example");
    // The engine is handed the new relay with NO key, so it refuses to build a
    // request rather than send A's key to B.
    expect(after.apiKey).toBe("");
    expect(useLlmStore.getState().profiles[0].apiKey).toBe("");
  });

  it("never sends a stale copy's key to a server chosen while the copy was held", () => {
    // A draft round or probe holds its profile across awaits.
    const id = jevWithKey();
    const held = useLlmStore.getState().profiles.find((profile) => profile.id === id)!;
    expect(held.apiKey).toBe("jev-key");

    useMultiplayerStore.getState().setHostingServer("wss://b.example/ws");

    const resolved = resolvedEndpointOf(held);
    expect(resolved.baseUrl).toBe("https://b.example");
    expect(resolved.apiKey).toBe("");
  });

  it("gives a removed profile's stale copy no key", () => {
    const id = jevWithKey();
    const held = useLlmStore.getState().profiles.find((profile) => profile.id === id)!;
    useLlmStore.getState().removeProfile(id);

    expect(resolvedEndpointOf(held).apiKey).toBe("");
  });

  it("lets a key entered after the switch belong to the new server", () => {
    const id = jevWithKey();
    useMultiplayerStore.getState().setHostingServer("wss://b.example/ws");
    useLlmStore.getState().updateProfile(id, { apiKey: "key-for-b" });

    expect(endpointFor(id)).toMatchObject({ baseUrl: "https://b.example", apiKey: "key-for-b" });
  });

  it("keeps the key across a change that does not move the relay origin", () => {
    const id = jevWithKey();
    useMultiplayerStore.getState().setHostingServer("wss://a.example/other-path");

    expect(endpointFor(id)).toMatchObject({ baseUrl: "https://a.example", apiKey: "jev-key" });
  });

  it("keeps the key of a profile with an explicit endpoint, which does not follow the host", () => {
    const id = jevWithKey({ baseUrl: "https://relay.example" });
    useMultiplayerStore.getState().setHostingServer("wss://b.example/ws");

    expect(endpointFor(id)).toMatchObject({ baseUrl: "https://relay.example", apiKey: "jev-key" });
  });

  it("does not touch another provider's key", () => {
    const id = useLlmStore.getState().addProfile({
      provider: "OpenAi", model: "gpt-5", apiKey: "sk", enabled: true,
    });
    useMultiplayerStore.getState().setHostingServer("wss://b.example/ws");

    expect(endpointFor(id).apiKey).toBe("sk");
  });
});
