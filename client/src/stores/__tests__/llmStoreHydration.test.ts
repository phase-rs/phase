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
      enabled: false,
    });
  });

  it("keeps a recognisable provider and buckets an unknown one as compatible", async () => {
    seed({
      profiles: [
        { id: "a", provider: "anthropic", model: "m" },
        { id: "b", provider: "Open-AI", model: "m" },
        { id: "c", provider: "some-self-hosted-thing", model: "m" },
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
    expect(byId).toEqual({ a: "Anthropic", b: "OpenAi", c: "OpenAiCompatible" });
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
      enabled: true,
    });
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
      expect(profileForSeat(useLlmStore.getState(), 0)).toBeUndefined();
    }
  });

  it("keeps only seat bindings that name a seat index and a profile id", async () => {
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

    expect(useLlmStore.getState().seatBindings).toEqual({ 0: "good", 4: "also-good" });
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
});
