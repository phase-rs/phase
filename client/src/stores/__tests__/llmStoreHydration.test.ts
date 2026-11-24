// A persisted zustand store captures its storage when this file's imports are
// evaluated, so the working-localStorage install has to precede them.
import "../../test/helpers/persistedStorage";

import { beforeEach, describe, expect, it, vi } from "vitest";

import { LLM_ENDPOINTS_KEY } from "../../constants/storage";

/**
 * Hydration happens during module construction, so every case here needs a
 * FRESH module evaluation against storage that is already populated. That is
 * the path where persist may invoke `onRehydrateStorage` synchronously — and
 * where a callback touching the store binding would hit its temporal dead zone.
 */
function seed(state: unknown): void {
  localStorage.setItem(LLM_ENDPOINTS_KEY, JSON.stringify({ state, version: 0 }));
}

async function freshStore() {
  vi.resetModules();
  return import("../llmStore");
}

beforeEach(() => {
  vi.resetModules();
  localStorage.clear();
  sessionStorage.clear();
});

describe("persisted store hydration", () => {
  it("constructs against an already-populated synchronous storage", async () => {
    seed({
      profiles: [
        {
          id: "p1",
          name: "Claude",
          provider: "Anthropic",
          baseUrl: null,
          model: "claude-sonnet-5",
          maxOutputTokens: null,
          temperature: null,
          enabled: true,
        },
      ],
      seatBindings: { 0: "p1" },
      draftEnabled: false,
      draftProfileId: null,
    });

    // The regression: importing the module must not throw while persist is
    // hydrating. A store-binding reference inside the hydration callback would
    // raise "Cannot access 'useLlmStore' before initialization" here.
    const { useLlmStore } = await freshStore();

    expect(useLlmStore.getState().profiles).toHaveLength(1);
    expect(useLlmStore.getState().seatBindings).toEqual({ 0: "p1" });
  });

  it("rehydrates a stored profile without its credential", async () => {
    seed({
      profiles: [
        {
          id: "p1",
          name: "Legacy",
          provider: "OpenAi",
          baseUrl: null,
          apiKey: "sk-from-an-older-build",
          model: "gpt-5",
          maxOutputTokens: null,
          temperature: null,
          enabled: true,
        },
      ],
      seatBindings: {},
      draftEnabled: false,
      draftProfileId: null,
    });

    const { useLlmStore } = await freshStore();

    const profile = useLlmStore.getState().profiles.find((p) => p.id === "p1");
    expect(profile?.apiKey).toBe("");
    expect(profile?.name).toBe("Legacy");
  });

  it("scrubs the stored credential from disk once construction has settled", async () => {
    seed({
      profiles: [
        {
          id: "p1",
          name: "Legacy",
          provider: "OpenAi",
          baseUrl: null,
          apiKey: "sk-from-an-older-build",
          model: "gpt-5",
          maxOutputTokens: null,
          temperature: null,
          enabled: true,
        },
      ],
      seatBindings: {},
      draftEnabled: false,
      draftProfileId: null,
    });

    await freshStore();
    // The scrub is deferred to a microtask so it cannot run during
    // construction; let that drain.
    await Promise.resolve();
    await Promise.resolve();

    expect(localStorage.getItem(LLM_ENDPOINTS_KEY)).not.toContain("sk-from-an-older-build");
  });

  it("constructs cleanly when storage holds nothing at all", async () => {
    const { useLlmStore } = await freshStore();
    expect(useLlmStore.getState().profiles).toEqual([]);
  });

  it("constructs cleanly when storage holds an unreadable record", async () => {
    localStorage.setItem(LLM_ENDPOINTS_KEY, "{not json");
    const { useLlmStore } = await freshStore();
    expect(useLlmStore.getState().profiles).toEqual([]);
  });

  // ── Malformed persisted containers ───────────────────────────────────────

  /// Storage is not a trusted input: it can be hand-edited, truncated by a
  /// quota failure, or written by another build. A shape that throws during
  /// migration would take the module down at import AND leave a pre-v1
  /// credential on disk, because `partialize` never gets to run.
  it("survives a profiles payload that is not an array", async () => {
    for (const profiles of [null, 42, "profiles", { id: "p1" }, true]) {
      vi.resetModules();
      localStorage.clear();
      seed({ profiles, seatBindings: {}, draftEnabled: false, draftProfileId: null });

      const { useLlmStore } = await freshStore();

      expect(useLlmStore.getState().profiles).toEqual([]);
    }
  });

  it("drops null and non-record entries but keeps the valid profiles beside them", async () => {
    seed({
      profiles: [
        null,
        "not-a-profile",
        42,
        ["nested"],
        { name: "no id" },
        {
          id: "good",
          name: "Keeper",
          provider: "OpenAi",
          baseUrl: null,
          apiKey: "sk-should-be-scrubbed",
          model: "gpt-5",
          maxOutputTokens: null,
          temperature: null,
          enabled: true,
        },
      ],
      seatBindings: {},
      draftEnabled: false,
      draftProfileId: null,
    });

    const { useLlmStore } = await freshStore();

    const profiles = useLlmStore.getState().profiles;
    expect(profiles).toHaveLength(1);
    expect(profiles[0]?.id).toBe("good");
    // The scrub still happens, which is the point of not throwing first.
    expect(profiles[0]?.apiKey).toBe("");
  });

  it("scrubs a credential even when the record also contains malformed entries", async () => {
    seed({
      profiles: [
        null,
        {
          id: "good",
          name: "Keeper",
          provider: "OpenAi",
          baseUrl: null,
          apiKey: "sk-must-not-survive",
          model: "gpt-5",
          maxOutputTokens: null,
          temperature: null,
          enabled: true,
        },
      ],
      seatBindings: {},
      draftEnabled: false,
      draftProfileId: null,
    });

    await freshStore();
    await Promise.resolve();
    await Promise.resolve();

    expect(localStorage.getItem(LLM_ENDPOINTS_KEY)).not.toContain("sk-must-not-survive");
  });

  it("survives a persisted record that is not an object at all", async () => {
    for (const record of ['"a string"', "42", "[1,2,3]", "null"]) {
      vi.resetModules();
      localStorage.clear();
      localStorage.setItem(LLM_ENDPOINTS_KEY, JSON.stringify({ state: JSON.parse(record), version: 0 }));

      const { useLlmStore } = await freshStore();

      expect(useLlmStore.getState().profiles).toEqual([]);
    }
  });

  // ── Partial records: valid JSON, incomplete profile ──────────────────────

  /// The finding: `{ id: "p", enabled: true }` is valid JSON with a string id,
  /// so a filter-only guard admits it — and the settings UI then calls
  /// `.trim()` on an absent `model`. Normalization means anything that reaches
  /// the store is a COMPLETE profile.
  it("completes an id-only record instead of admitting a partial one", async () => {
    seed({
      profiles: [{ id: "p", enabled: true }],
      seatBindings: {},
      draftEnabled: false,
      draftProfileId: null,
    });

    const { useLlmStore } = await freshStore();

    const profile = useLlmStore.getState().profiles[0];
    expect(profile).toBeDefined();
    // Every field the UI touches is present and of the declared type.
    expect(typeof profile?.name).toBe("string");
    expect(typeof profile?.model).toBe("string");
    expect(typeof profile?.apiKey).toBe("string");
    expect(typeof profile?.enabled).toBe("boolean");
    expect(profile?.baseUrl).toBeNull();
    expect(profile?.maxOutputTokens).toBeNull();
    expect(profile?.temperature).toBeNull();
    expect(profile?.decisionContext).toBe("selfContained");
    // It claimed to be enabled but has no model, so it cannot be usable.
    expect(profile?.enabled).toBe(false);
  });

  it("coerces every field that arrived with the wrong type", async () => {
    seed({
      profiles: [
        {
          id: "  p  ",
          name: 42,
          provider: { not: "a provider" },
          baseUrl: 7,
          model: ["gpt-5"],
          maxOutputTokens: "many",
          temperature: Number.NaN,
          decisionContext: true,
          enabled: "yes",
        },
      ],
      seatBindings: {},
      draftEnabled: false,
      draftProfileId: null,
    });

    const { useLlmStore } = await freshStore();

    expect(useLlmStore.getState().profiles[0]).toEqual({
      id: "p",
      name: "",
      provider: "OpenAi",
      baseUrl: null,
      apiKey: "",
      model: "",
      maxOutputTokens: null,
      temperature: null,
      decisionContext: "selfContained",
      enabled: false,
      rememberApiKey: false,
    });
  });

  it("keeps a recognisable provider and buckets an unknown one as compatible", async () => {
    seed({
      profiles: [
        { id: "a", provider: "anthropic", model: "m" },
        { id: "b", provider: "Open-AI", model: "m" },
        { id: "c", provider: "some-self-hosted-thing", model: "m" },
        { id: "d", provider: "OpenRouter", model: "m" },
        { id: "e", provider: "oPeN-rOuTeR", model: "m" },
      ],
      seatBindings: {},
      draftEnabled: false,
      draftProfileId: null,
    });

    const { useLlmStore } = await freshStore();

    const byId = Object.fromEntries(
      useLlmStore.getState().profiles.map((profile) => [profile.id, profile.provider]),
    );
    // Same normalization the engine's `LlmProvider::from_label` applies.
    expect(byId).toEqual({
      a: "Anthropic",
      b: "OpenAi",
      c: "OpenAiCompatible",
      d: "OpenRouter",
      e: "OpenRouter",
    });
  });

  it("drops a record with no usable id rather than fabricating one", async () => {
    seed({
      profiles: [{ enabled: true, model: "m" }, { id: "   ", model: "m" }, { id: 7 }],
      seatBindings: {},
      draftEnabled: false,
      draftProfileId: null,
    });

    const { useLlmStore } = await freshStore();

    // Without a stable identity a profile cannot be bound, edited or removed.
    expect(useLlmStore.getState().profiles).toEqual([]);
  });

  it("preserves a complete record untouched apart from its credential", async () => {
    seed({
      profiles: [
        {
          id: "full",
          name: "Claude",
          provider: "Anthropic",
          baseUrl: "https://api.anthropic.com/v1",
          apiKey: "sk-must-not-survive",
          model: "claude-sonnet-5",
          maxOutputTokens: 2048,
          temperature: 0.2,
          decisionContext: "messageThread",
          enabled: true,
        },
      ],
      seatBindings: {},
      draftEnabled: false,
      draftProfileId: null,
    });

    const { useLlmStore } = await freshStore();

    expect(useLlmStore.getState().profiles[0]).toEqual({
      id: "full",
      name: "Claude",
      provider: "Anthropic",
      baseUrl: "https://api.anthropic.com/v1",
      apiKey: "",
      model: "claude-sonnet-5",
      maxOutputTokens: 2048,
      temperature: 0.2,
      decisionContext: "messageThread",
      enabled: true,
      rememberApiKey: false,
    });
  });

  it("keeps a profile saved before message threads existed on self-contained requests", async () => {
    seed({
      profiles: [
        {
          id: "older",
          name: "Claude",
          provider: "Anthropic",
          baseUrl: null,
          model: "claude-sonnet-5",
          maxOutputTokens: null,
          temperature: null,
          enabled: true,
        },
        // Only the exact opt-in value enables threads.
        { id: "odd", provider: "Anthropic", model: "m", decisionContext: "MessageThread" },
      ],
      seatBindings: {},
      draftEnabled: false,
      draftProfileId: null,
    });

    const { useLlmStore } = await freshStore();

    const contexts = useLlmStore.getState().profiles.map((profile) => profile.decisionContext);
    expect(contexts).toEqual(["selfContained", "selfContained"]);
  });

  // ── The rest of the persisted slice ──────────────────────────────────────

  /// The finding: only `profiles` was validated, so the remaining fields were
  /// spread through untyped. `profileForSeat` indexes `seatBindings` and
  /// `removeProfile` calls `Object.entries` on it, so a null value crashed at
  /// the first read rather than at hydration.
  it("survives seat bindings that are not a record", async () => {
    for (const seatBindings of [null, 42, "bindings", ["a"], true]) {
      vi.resetModules();
      localStorage.clear();
      seed({ profiles: [], seatBindings, draftEnabled: false, draftProfileId: null });

      const { useLlmStore, profileForSeat } = await freshStore();

      expect(useLlmStore.getState().seatBindings).toEqual({});
      // The read path that would have thrown.
      expect(profileForSeat(useLlmStore.getState(), 0, [])).toBeUndefined();
    }
  });

  it("keeps only seat bindings that name a seat index and a profile id or the engine", async () => {
    seed({
      profiles: [],
      seatBindings: {
        0: "good",
        1: 42,
        2: null,
        3: "",
        "-1": "negative",
        "1.5": "fractional",
        notASeat: "nope",
        4: "also-good",
      },
      draftEnabled: false,
      draftProfileId: null,
    });

    const { useLlmStore } = await freshStore();

    // `null` survives: it is the player's explicit choice of the built-in
    // engine, which must keep overriding the default opponent after a reload.
    expect(useLlmStore.getState().seatBindings).toEqual({ 0: "good", 2: null, 4: "also-good" });
  });

  it("restores the default opponent only when it is a non-empty profile id", async () => {
    seed({ profiles: [], seatBindings: {}, defaultOpponentProfileId: "p1" });
    expect((await freshStore()).useLlmStore.getState().defaultOpponentProfileId).toBe("p1");

    for (const junk of ["", 7, null, {}]) {
      seed({ profiles: [], seatBindings: {}, defaultOpponentProfileId: junk });
      expect((await freshStore()).useLlmStore.getState().defaultOpponentProfileId).toBeNull();
    }
  });

  it("removes a profile without throwing when bindings arrived malformed", async () => {
    seed({ profiles: [], seatBindings: null, draftEnabled: false, draftProfileId: null });

    const { useLlmStore } = await freshStore();
    const id = useLlmStore.getState().addProfile({ model: "m", enabled: true });

    // `removeProfile` calls `Object.entries(seatBindings)`.
    expect(() => useLlmStore.getState().removeProfile(id)).not.toThrow();
    expect(useLlmStore.getState().profiles).toEqual([]);
  });

  it("only a real boolean enables drafting", async () => {
    for (const draftEnabled of ["true", 1, {}, [], null]) {
      vi.resetModules();
      localStorage.clear();
      seed({ profiles: [], seatBindings: {}, draftEnabled, draftProfileId: null });

      const { useLlmStore } = await freshStore();

      // A truthy non-boolean must not switch an LLM into a draft pod.
      expect(useLlmStore.getState().draftEnabled).toBe(false);
    }

    vi.resetModules();
    localStorage.clear();
    seed({ profiles: [], seatBindings: {}, draftEnabled: true, draftProfileId: null });
    const { useLlmStore } = await freshStore();
    expect(useLlmStore.getState().draftEnabled).toBe(true);
  });

  it("coerces a non-string draft profile id to null", async () => {
    seed({ profiles: [], seatBindings: {}, draftEnabled: true, draftProfileId: 42 });

    const { useLlmStore } = await freshStore();

    expect(useLlmStore.getState().draftProfileId).toBeNull();
  });

  it("normalizes every field of a record whose whole slice is wrong-typed", async () => {
    seed({ profiles: "nope", seatBindings: "nope", draftEnabled: "yes", draftProfileId: [] });

    const { useLlmStore } = await freshStore();

    const state = useLlmStore.getState();
    expect(state.profiles).toEqual([]);
    expect(state.seatBindings).toEqual({});
    expect(state.draftEnabled).toBe(false);
    expect(state.draftProfileId).toBeNull();
  });

  // ── SessionStorage API key hydration ────────────────────────────────────────

  it("rehydrates API key from sessionStorage when rememberApiKey is true and destination matches", async () => {
    seed({
      profiles: [
        {
          id: "p1",
          name: "Claude",
          provider: "Anthropic",
          baseUrl: null,
          model: "claude-sonnet-5",
          maxOutputTokens: null,
          temperature: null,
          enabled: true,
        },
      ],
      seatBindings: {},
      draftEnabled: false,
      draftProfileId: null,
    });
    sessionStorage.setItem("phase-llm-remember-api-keys", JSON.stringify(["p1"]));
    sessionStorage.setItem(
      "phase-llm-api-keys",
      JSON.stringify({
        p1: { apiKey: "sk-session-restored", provider: "Anthropic", endpoint: "" },
      }),
    );

    const { useLlmStore } = await freshStore();
    await Promise.resolve();
    await Promise.resolve();

    const profile = useLlmStore.getState().profiles.find((p) => p.id === "p1");
    expect(profile?.apiKey).toBe("sk-session-restored");
    expect(profile?.rememberApiKey).toBe(true);
  });

  it("starts with rememberApiKey off in a fresh tab even if localStorage contains it", async () => {
    seed({
      profiles: [
        {
          id: "p1",
          name: "Claude",
          provider: "Anthropic",
          baseUrl: null,
          model: "claude-sonnet-5",
          maxOutputTokens: null,
          temperature: null,
          enabled: true,
          rememberApiKey: true,
        },
      ],
      seatBindings: {},
      draftEnabled: false,
      draftProfileId: null,
    });

    const { useLlmStore } = await freshStore();
    await Promise.resolve();
    await Promise.resolve();

    const profile = useLlmStore.getState().profiles.find((p) => p.id === "p1");
    expect(profile?.apiKey).toBe("");
    expect(profile?.rememberApiKey).toBe(false);
  });

  it("does not rehydrate API key from sessionStorage when rememberApiKey is not opted in for this tab", async () => {
    seed({
      profiles: [
        {
          id: "p1",
          name: "Claude",
          provider: "Anthropic",
          baseUrl: null,
          model: "claude-sonnet-5",
          maxOutputTokens: null,
          temperature: null,
          enabled: true,
        },
      ],
      seatBindings: {},
      draftEnabled: false,
      draftProfileId: null,
    });
    sessionStorage.setItem(
      "phase-llm-api-keys",
      JSON.stringify({
        p1: { apiKey: "sk-should-not-restore", provider: "Anthropic", endpoint: "" },
      }),
    );

    const { useLlmStore } = await freshStore();
    await Promise.resolve();
    await Promise.resolve();

    const profile = useLlmStore.getState().profiles.find((p) => p.id === "p1");
    expect(profile?.apiKey).toBe("");
    expect(profile?.rememberApiKey).toBe(false);
    expect(sessionStorage.getItem("phase-llm-api-keys")).toBeNull();
  });

  it("rehydrates key on reload when explicit endpoint is unchanged, and refuses when edited", async () => {
    seed({
      profiles: [
        {
          id: "p1",
          name: "Proxy",
          provider: "OpenAi",
          baseUrl: "https://proxy.a.example/v1",
          model: "gpt-5",
          maxOutputTokens: null,
          temperature: null,
          enabled: true,
        },
      ],
      seatBindings: {},
      draftEnabled: false,
      draftProfileId: null,
    });
    sessionStorage.setItem("phase-llm-remember-api-keys", JSON.stringify(["p1"]));
    sessionStorage.setItem(
      "phase-llm-api-keys",
      JSON.stringify({
        p1: { apiKey: "sk-proxy", provider: "OpenAi", endpoint: "https://proxy.a.example/v1" },
      }),
    );

    const { useLlmStore } = await freshStore();
    expect(useLlmStore.getState().profiles[0]?.apiKey).toBe("sk-proxy");

    // Simulate another tab editing the explicit endpoint before this tab reloads.
    seed({
      profiles: [
        {
          id: "p1",
          name: "Proxy",
          provider: "OpenAi",
          baseUrl: "https://proxy.b.example/v1",
          model: "gpt-5",
          maxOutputTokens: null,
          temperature: null,
          enabled: true,
        },
      ],
      seatBindings: {},
      draftEnabled: false,
      draftProfileId: null,
    });
    const { useLlmStore: reloaded } = await freshStore();
    expect(reloaded.getState().profiles[0]?.apiKey).toBe("");
    expect(sessionStorage.getItem("phase-llm-api-keys")).toBeNull();
  });

  it("rehydrates Jev key when relay origin is unchanged, and refuses when relay or endpoint changed", async () => {
    localStorage.setItem(
      "phase-multiplayer",
      JSON.stringify({ state: { hostingServer: "wss://relay-a.example" }, version: 7 }),
    );
    seed({
      profiles: [
        {
          id: "jev1",
          name: "Jev",
          provider: "Jev",
          baseUrl: null,
          model: "jev-latest",
          maxOutputTokens: null,
          temperature: null,
          enabled: true,
        },
      ],
      seatBindings: {},
      draftEnabled: false,
      draftProfileId: null,
    });
    sessionStorage.setItem("phase-llm-remember-api-keys", JSON.stringify(["jev1"]));
    sessionStorage.setItem(
      "phase-llm-api-keys",
      JSON.stringify({
        jev1: { apiKey: "jev-secret", provider: "Jev", endpoint: "https://relay-a.example" },
      }),
    );

    const { useLlmStore } = await freshStore();
    expect(useLlmStore.getState().profiles[0]?.apiKey).toBe("jev-secret");

    // Another tab selects relay B in shared multiplayer settings before reload.
    localStorage.setItem(
      "phase-multiplayer",
      JSON.stringify({ state: { hostingServer: "wss://relay-b.example" }, version: 7 }),
    );
    const { useLlmStore: afterRelayChange } = await freshStore();
    expect(afterRelayChange.getState().profiles[0]?.apiKey).toBe("");
    expect(sessionStorage.getItem("phase-llm-api-keys")).toBeNull();

    // Another tab sets an explicit custom endpoint on the Jev profile while relay A is active.
    localStorage.setItem(
      "phase-multiplayer",
      JSON.stringify({ state: { hostingServer: "wss://relay-a.example" }, version: 7 }),
    );
    seed({
      profiles: [
        {
          id: "jev1",
          name: "Jev",
          provider: "Jev",
          baseUrl: "https://custom-jev.example",
          model: "jev-latest",
          maxOutputTokens: null,
          temperature: null,
          enabled: true,
        },
      ],
      seatBindings: {},
      draftEnabled: false,
      draftProfileId: null,
    });
    sessionStorage.setItem(
      "phase-llm-api-keys",
      JSON.stringify({
        jev1: { apiKey: "jev-secret", provider: "Jev", endpoint: "https://relay-a.example" },
      }),
    );
    const { useLlmStore: afterExplicitEdit } = await freshStore();
    expect(afterExplicitEdit.getState().profiles[0]?.apiKey).toBe("");
  });

  it("refuses to restore a key if provider changed in another tab", async () => {
    seed({
      profiles: [
        {
          id: "p1",
          name: "Changed",
          provider: "Anthropic",
          baseUrl: null,
          model: "claude-sonnet-5",
          maxOutputTokens: null,
          temperature: null,
          enabled: true,
        },
      ],
      seatBindings: {},
      draftEnabled: false,
      draftProfileId: null,
    });
    sessionStorage.setItem("phase-llm-remember-api-keys", JSON.stringify(["p1"]));
    sessionStorage.setItem(
      "phase-llm-api-keys",
      JSON.stringify({
        p1: { apiKey: "sk-openai", provider: "OpenAi", endpoint: "" },
      }),
    );

    const { useLlmStore } = await freshStore();
    expect(useLlmStore.getState().profiles[0]?.apiKey).toBe("");
  });

  it("does not rehydrate API key from sessionStorage when key is absent", async () => {
    seed({
      profiles: [
        {
          id: "p1",
          name: "Claude",
          provider: "Anthropic",
          baseUrl: null,
          model: "claude-sonnet-5",
          maxOutputTokens: null,
          temperature: null,
          enabled: true,
        },
      ],
      seatBindings: {},
      draftEnabled: false,
      draftProfileId: null,
    });
    sessionStorage.setItem("phase-llm-remember-api-keys", JSON.stringify(["p1"]));

    const { useLlmStore } = await freshStore();
    await Promise.resolve();
    await Promise.resolve();

    const profile = useLlmStore.getState().profiles.find((p) => p.id === "p1");
    expect(profile?.apiKey).toBe("");
    expect(profile?.rememberApiKey).toBe(true);
  });

  it("survives malformed sessionStorage data", async () => {
    seed({
      profiles: [
        {
          id: "p1",
          name: "Claude",
          provider: "Anthropic",
          baseUrl: null,
          model: "claude-sonnet-5",
          maxOutputTokens: null,
          temperature: null,
          enabled: true,
        },
      ],
      seatBindings: {},
      draftEnabled: false,
      draftProfileId: null,
    });
    sessionStorage.setItem("phase-llm-remember-api-keys", JSON.stringify(["p1"]));
    sessionStorage.setItem("phase-llm-api-keys", "not valid json");

    const { useLlmStore } = await freshStore();
    await Promise.resolve();
    await Promise.resolve();

    const profile = useLlmStore.getState().profiles.find((p) => p.id === "p1");
    expect(profile?.apiKey).toBe("");
    expect(profile?.rememberApiKey).toBe(true);
  });

  it("survives sessionStorage quota errors and removes stale stored key on write failure", async () => {
    seed({
      profiles: [
        {
          id: "p1",
          name: "Claude",
          provider: "Anthropic",
          baseUrl: null,
          model: "claude-sonnet-5",
          maxOutputTokens: null,
          temperature: null,
          enabled: true,
        },
      ],
      seatBindings: {},
      draftEnabled: false,
      draftProfileId: null,
    });
    sessionStorage.setItem("phase-llm-remember-api-keys", JSON.stringify(["p1"]));
    sessionStorage.setItem(
      "phase-llm-api-keys",
      JSON.stringify({
        p1: { apiKey: "sk-old", provider: "Anthropic", endpoint: "" },
      }),
    );

    const { useLlmStore } = await freshStore();
    await Promise.resolve();
    await Promise.resolve();
    expect(useLlmStore.getState().profiles.find((p) => p.id === "p1")?.apiKey).toBe("sk-old");

    const setItemSpy = vi.spyOn(sessionStorage, "setItem").mockImplementation(() => {
      throw new DOMException("Quota exceeded", "QuotaExceededError");
    });
    try {
      expect(() =>
        useLlmStore.getState().updateProfile("p1", { apiKey: "sk-new" }),
      ).not.toThrow();
      expect(setItemSpy).toHaveBeenCalled();
      const profile = useLlmStore.getState().profiles.find((p) => p.id === "p1");
      expect(profile?.apiKey).toBe("sk-new");
      expect(profile?.rememberApiKey).toBe(true);
      expect(sessionStorage.getItem("phase-llm-api-keys")).toBeNull();
    } finally {
      setItemSpy.mockRestore();
    }
  });

  // ── F1 regression: cross-tab profile deletion ───────────────────────────────

  it("prunes sessionStorage key and remember flag when profile was deleted in another tab", async () => {
    // Tab A has profile p1 with remembered key
    seed({
      profiles: [
        {
          id: "p1",
          name: "Claude",
          provider: "Anthropic",
          baseUrl: null,
          model: "claude-sonnet-5",
          maxOutputTokens: null,
          temperature: null,
          enabled: true,
        },
        {
          id: "p2",
          name: "Other",
          provider: "OpenAi",
          baseUrl: null,
          model: "gpt-5",
          maxOutputTokens: null,
          temperature: null,
          enabled: true,
        },
      ],
      seatBindings: {},
      draftEnabled: false,
      draftProfileId: null,
    });
    sessionStorage.setItem("phase-llm-remember-api-keys", JSON.stringify(["p1", "p2"]));
    sessionStorage.setItem(
      "phase-llm-api-keys",
      JSON.stringify({
        p1: { apiKey: "sk-p1", provider: "Anthropic", endpoint: "" },
        p2: { apiKey: "sk-p2", provider: "OpenAi", endpoint: "" },
      }),
    );

    const { useLlmStore: tabA } = await freshStore();
    await Promise.resolve();
    await Promise.resolve();

    // Both keys restored
    expect(tabA.getState().profiles.find((p) => p.id === "p1")?.apiKey).toBe("sk-p1");
    expect(tabA.getState().profiles.find((p) => p.id === "p2")?.apiKey).toBe("sk-p2");
    expect(tabA.getState().profiles.find((p) => p.id === "p1")?.rememberApiKey).toBe(true);
    expect(tabA.getState().profiles.find((p) => p.id === "p2")?.rememberApiKey).toBe(true);

    // Tab B removes profile p1 (simulated by re-seeding localStorage without p1)
    seed({
      profiles: [
        {
          id: "p2",
          name: "Other",
          provider: "OpenAi",
          baseUrl: null,
          model: "gpt-5",
          maxOutputTokens: null,
          temperature: null,
          enabled: true,
        },
      ],
      seatBindings: {},
      draftEnabled: false,
      draftProfileId: null,
    });

    // Tab A reloads — p1 should be gone from sessionStorage too
    const { useLlmStore: tabAReloaded } = await freshStore();
    await Promise.resolve();
    await Promise.resolve();

    const reloadedProfiles = tabAReloaded.getState().profiles;
    expect(reloadedProfiles).toHaveLength(1);
    expect(reloadedProfiles[0]?.id).toBe("p2");
    expect(reloadedProfiles[0]?.apiKey).toBe("sk-p2");
    expect(reloadedProfiles[0]?.rememberApiKey).toBe(true);

    // Orphaned sessionStorage entries for p1 must be removed
    expect(sessionStorage.getItem("phase-llm-api-keys")).not.toContain("p1");
    expect(sessionStorage.getItem("phase-llm-remember-api-keys")).not.toContain("p1");
    // p2's entries must survive
    expect(sessionStorage.getItem("phase-llm-api-keys")).toContain("sk-p2");
    expect(sessionStorage.getItem("phase-llm-remember-api-keys")).toContain("p2");
  });
});
