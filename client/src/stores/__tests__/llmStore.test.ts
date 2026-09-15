// A persisted zustand store captures its storage when this file's imports are
// evaluated, so the working-localStorage install has to precede them.
import "../../test/helpers/persistedStorage";

import { beforeEach, describe, expect, it } from "vitest";

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
});
