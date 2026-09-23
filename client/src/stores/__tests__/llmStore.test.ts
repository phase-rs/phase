// A persisted zustand store captures its storage when this file's imports are
// evaluated, so the working-localStorage install has to precede them.
import "../../test/helpers/persistedStorage";

import { beforeEach, describe, expect, it } from "vitest";

import { LLM_ENDPOINTS_KEY } from "../../constants/storage";
import { draftProfile, isProfileUsable, profileForSeat, useLlmStore } from "../llmStore";
import type { LlmProfile } from "../../services/llm/types";

function reset(): void {
  useLlmStore.setState({
    profiles: [],
    seatBindings: {},
    draftEnabled: false,
    draftProfileId: null,
  });
}

function addUsable(name: string): string {
  const id = useLlmStore.getState().addProfile({ name, model: "gpt-5", enabled: true });
  return id;
}

beforeEach(reset);

describe("llmStore", () => {
  it("starts with no providers, so no LLM path is reachable", () => {
    const state = useLlmStore.getState();
    expect(state.profiles).toEqual([]);
    expect(profileForSeat(state, 0)).toBeUndefined();
    expect(draftProfile(state)).toBeUndefined();
  });

  it("creates new profiles disabled so a half-filled endpoint is never reachable", () => {
    const id = useLlmStore.getState().addProfile();
    const profile = useLlmStore.getState().profiles.find((p) => p.id === id);
    expect(profile?.enabled).toBe(false);
    expect(isProfileUsable(profile)).toBe(false);
  });

  it("treats an enabled profile with no model as unusable", () => {
    const id = useLlmStore.getState().addProfile({ enabled: true, model: "   " });
    useLlmStore.getState().bindSeat(0, id);
    expect(profileForSeat(useLlmStore.getState(), 0)).toBeUndefined();
  });

  it("binds a seat to a usable profile and unbinds it again", () => {
    const id = addUsable("Claude");
    useLlmStore.getState().bindSeat(1, id);
    expect(profileForSeat(useLlmStore.getState(), 1)?.id).toBe(id);
    expect(profileForSeat(useLlmStore.getState(), 0)).toBeUndefined();

    useLlmStore.getState().bindSeat(1, null);
    expect(profileForSeat(useLlmStore.getState(), 1)).toBeUndefined();
  });

  it("drops every binding to a removed profile in the same commit", () => {
    const id = addUsable("Claude");
    useLlmStore.getState().bindSeat(0, id);
    useLlmStore.getState().bindSeat(2, id);
    useLlmStore.getState().setDraftProfileId(id);

    useLlmStore.getState().removeProfile(id);

    const state = useLlmStore.getState();
    expect(state.seatBindings).toEqual({});
    expect(state.draftProfileId).toBeNull();
    expect(profileForSeat(state, 0)).toBeUndefined();
  });

  it("keeps drafting heuristic until it is explicitly enabled", () => {
    const id = addUsable("Claude");
    expect(draftProfile(useLlmStore.getState())).toBeUndefined();

    useLlmStore.getState().setDraftEnabled(true);
    expect(draftProfile(useLlmStore.getState())?.id).toBe(id);
  });

  it("falls back to the first usable profile when the chosen draft profile is gone", () => {
    const first = addUsable("First");
    useLlmStore.getState().setDraftEnabled(true);
    useLlmStore.getState().setDraftProfileId("no-such-profile");

    expect(draftProfile(useLlmStore.getState())?.id).toBe(first);
  });

  it("does not resolve a draft profile when every profile is disabled", () => {
    const id = addUsable("Claude");
    useLlmStore.getState().setDraftEnabled(true);
    useLlmStore.getState().updateProfile(id, { enabled: false });

    expect(draftProfile(useLlmStore.getState())).toBeUndefined();
  });

  it("carries the API key only in the profile record", () => {
    const id = useLlmStore.getState().addProfile({ apiKey: "sk-test", model: "m", enabled: true });
    const profile = useLlmStore.getState().profiles.find((p) => p.id === id) as LlmProfile;
    expect(profile.apiKey).toBe("sk-test");
  });

  it("never writes the API key to persistent storage", () => {
    useLlmStore
      .getState()
      .addProfile({ name: "Claude", model: "m", apiKey: "sk-super-secret", enabled: true });

    const raw = localStorage.getItem(LLM_ENDPOINTS_KEY) ?? "";
    // The profile itself persists — only the credential is withheld.
    expect(raw).toContain("Claude");
    expect(raw).not.toContain("sk-super-secret");
    expect(raw).not.toContain("apiKey");
  });

  it("drops the credential when the provider changes", () => {
    const id = useLlmStore
      .getState()
      .addProfile({ model: "gpt-5", apiKey: "sk-openai", provider: "OpenAi", enabled: true });

    useLlmStore.getState().updateProfile(id, { provider: "Anthropic" });

    const profile = useLlmStore.getState().profiles.find((p) => p.id === id);
    // An OpenAI key must never be sent to Anthropic.
    expect(profile?.apiKey).toBe("");
    // And the profile is no longer usable until a new key is supplied.
    expect(isProfileUsable(profile)).toBe(false);
  });

  it("keeps the credential when the provider is unchanged", () => {
    const id = useLlmStore
      .getState()
      .addProfile({ model: "gpt-5", apiKey: "sk-openai", provider: "OpenAi", enabled: true });

    useLlmStore.getState().updateProfile(id, { name: "renamed" });

    expect(useLlmStore.getState().profiles.find((p) => p.id === id)?.apiKey).toBe("sk-openai");
  });

  it("accepts a new credential supplied in the same patch as the provider change", () => {
    const id = useLlmStore
      .getState()
      .addProfile({ model: "gpt-5", apiKey: "sk-openai", provider: "OpenAi", enabled: true });

    useLlmStore
      .getState()
      .updateProfile(id, { provider: "Anthropic", apiKey: "sk-anthropic" });

    expect(useLlmStore.getState().profiles.find((p) => p.id === id)?.apiKey).toBe("sk-anthropic");
  });

  it("removes a credential that a previous build had already written to disk", async () => {
    // The pre-v1 shape: a persisted record that carries the key.
    localStorage.setItem(
      LLM_ENDPOINTS_KEY,
      JSON.stringify({
        state: {
          profiles: [
            {
              id: "legacy",
              name: "Legacy",
              provider: "OpenAi",
              baseUrl: null,
              apiKey: "sk-leaked-from-an-older-build",
              model: "gpt-5",
              maxOutputTokens: null,
              temperature: null,
              enabled: true,
            },
          ],
          seatBindings: {},
          draftEnabled: false,
          draftProfileId: null,
        },
      }),
    );

    await useLlmStore.persist.rehydrate();

    // Scrubbed from disk, not merely ignored in memory.
    expect(localStorage.getItem(LLM_ENDPOINTS_KEY)).not.toContain("sk-leaked-from-an-older-build");
    const profile = useLlmStore.getState().profiles.find((p) => p.id === "legacy");
    expect(profile?.apiKey).toBe("");
    // The profile survives; only the credential is gone.
    expect(profile?.name).toBe("Legacy");
  });

  // ── Credential scope: the endpoint, not just the vendor ──────────────────

  it("drops the credential when the endpoint changes", () => {
    const id = useLlmStore.getState().addProfile({
      model: "gpt-5",
      apiKey: "sk-openai",
      provider: "OpenAi",
      baseUrl: "https://api.openai.com/v1",
      enabled: true,
    });

    // Retargeting at a host the player may not control must not carry the key.
    useLlmStore.getState().updateProfile(id, { baseUrl: "https://someone-elses.example/v1" });

    const profile = useLlmStore.getState().profiles.find((p) => p.id === id);
    expect(profile?.apiKey).toBe("");
    expect(isProfileUsable(profile)).toBe(false);
  });

  it("drops the credential when the endpoint is cleared back to the default", () => {
    const id = useLlmStore.getState().addProfile({
      model: "gpt-5",
      apiKey: "sk-openai",
      baseUrl: "https://proxy.internal/v1",
      enabled: true,
    });

    useLlmStore.getState().updateProfile(id, { baseUrl: null });

    expect(useLlmStore.getState().profiles.find((p) => p.id === id)?.apiKey).toBe("");
  });

  it("keeps the credential when a patch restates the same endpoint", () => {
    const id = useLlmStore.getState().addProfile({
      model: "gpt-5",
      apiKey: "sk-openai",
      baseUrl: "https://api.openai.com/v1",
      enabled: true,
    });

    // The settings form re-sends the current value routinely; that is not a
    // change and must not wipe a working key.
    useLlmStore.getState().updateProfile(id, { baseUrl: "https://api.openai.com/v1" });
    useLlmStore.getState().updateProfile(id, { baseUrl: "  https://api.openai.com/v1  " });
    useLlmStore.getState().updateProfile(id, { name: "renamed" });

    expect(useLlmStore.getState().profiles.find((p) => p.id === id)?.apiKey).toBe("sk-openai");
  });

  it("treats an absent and an empty endpoint as the same default", () => {
    const id = useLlmStore
      .getState()
      .addProfile({ model: "gpt-5", apiKey: "sk-openai", baseUrl: null, enabled: true });

    useLlmStore.getState().updateProfile(id, { baseUrl: "   " });

    expect(useLlmStore.getState().profiles.find((p) => p.id === id)?.apiKey).toBe("sk-openai");
  });

  it("accepts a replacement credential supplied with the endpoint change", () => {
    const id = useLlmStore.getState().addProfile({
      model: "gpt-5",
      apiKey: "sk-old",
      baseUrl: "https://api.openai.com/v1",
      enabled: true,
    });

    useLlmStore
      .getState()
      .updateProfile(id, { baseUrl: "https://proxy.internal/v1", apiKey: "sk-new" });

    expect(useLlmStore.getState().profiles.find((p) => p.id === id)?.apiKey).toBe("sk-new");
  });
});
