import { create } from "zustand";
import { persist } from "zustand/middleware";

import { LLM_ENDPOINTS_KEY } from "../constants/storage";
import type { LlmProfile, LlmProviderId } from "../services/llm/types";

/**
 * Configured LLM opponent endpoints.
 *
 * LLM opponents are strictly opt-in. An empty profile list — the default, and
 * what every existing installation has — means no LLM code path ever runs and
 * every AI seat is driven by the engine's heuristic AI exactly as before.
 * Binding a seat to a profile is a second, separate opt-in
 * ({@link seatBindings}).
 *
 * Persisted under its own storage key so the credentials never ride along with
 * the portable profile that backup export and cloud sync move off-device.
 */
export interface LlmState {
  profiles: LlmProfile[];
  /**
   * AI seat index (0 = first AI opponent, matching `preferencesStore.aiSeats`)
   * -> profile id. A seat with no entry uses the heuristic AI.
   */
  seatBindings: Record<number, string>;
  /** Whether bot seats in a draft pod use the bound profile. Off by default. */
  draftEnabled: boolean;
  /** Profile id used by LLM drafters. `null` = the first enabled profile. */
  draftProfileId: string | null;

  addProfile(partial?: Partial<LlmProfile>): string;
  updateProfile(id: string, patch: Partial<LlmProfile>): void;
  removeProfile(id: string): void;
  bindSeat(seatIndex: number, profileId: string | null): void;
  setDraftEnabled(enabled: boolean): void;
  setDraftProfileId(id: string | null): void;
}

const DEFAULT_PROVIDER: LlmProviderId = "OpenAi";

function newProfileId(): string {
  return `llm-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
}

/** A profile is usable only when the player explicitly enabled it AND it names
 *  a model. The key check is the engine's (`LlmEndpointConfig::validate`), which
 *  runs before any request is built; this is the cheap UI-side gate. */
export function isProfileUsable(profile: LlmProfile | undefined): profile is LlmProfile {
  return Boolean(profile?.enabled && profile.model.trim());
}

export const useLlmStore = create<LlmState>()(
  persist(
    (set, get) => ({
      profiles: [],
      seatBindings: {},
      draftEnabled: false,
      draftProfileId: null,

      addProfile(partial) {
        const id = newProfileId();
        const profile: LlmProfile = {
          id,
          name: "",
          provider: DEFAULT_PROVIDER,
          baseUrl: null,
          apiKey: "",
          model: "",
          maxOutputTokens: null,
          temperature: null,
          // New profiles start disabled: a half-filled endpoint must never be
          // reachable from a seat picker.
          enabled: false,
          ...partial,
        };
        set((state) => ({ profiles: [...state.profiles, profile] }));
        return id;
      },

      updateProfile(id, patch) {
        set((state) => ({
          profiles: state.profiles.map((profile) =>
            profile.id === id ? { ...profile, ...patch } : profile,
          ),
        }));
      },

      removeProfile(id) {
        const { seatBindings, draftProfileId } = get();
        // Drop every binding to the removed profile in the same commit, so no
        // seat is left pointing at a profile that no longer exists.
        const remainingBindings = Object.fromEntries(
          Object.entries(seatBindings).filter(([, profileId]) => profileId !== id),
        );
        set((state) => ({
          profiles: state.profiles.filter((profile) => profile.id !== id),
          seatBindings: remainingBindings,
          draftProfileId: draftProfileId === id ? null : draftProfileId,
        }));
      },

      bindSeat(seatIndex, profileId) {
        set((state) => {
          const next = { ...state.seatBindings };
          if (profileId == null) delete next[seatIndex];
          else next[seatIndex] = profileId;
          return { seatBindings: next };
        });
      },

      setDraftEnabled(draftEnabled) {
        set({ draftEnabled });
      },

      setDraftProfileId(draftProfileId) {
        set({ draftProfileId });
      },
    }),
    { name: LLM_ENDPOINTS_KEY },
  ),
);

/** The profile bound to an AI seat, or `undefined` when the seat is heuristic. */
export function profileForSeat(state: LlmState, seatIndex: number): LlmProfile | undefined {
  const id = state.seatBindings[seatIndex];
  if (!id) return undefined;
  const profile = state.profiles.find((candidate) => candidate.id === id);
  return isProfileUsable(profile) ? profile : undefined;
}

/** The profile LLM drafters use, or `undefined` when drafting stays heuristic. */
export function draftProfile(state: LlmState): LlmProfile | undefined {
  if (!state.draftEnabled) return undefined;
  const explicit = state.profiles.find((profile) => profile.id === state.draftProfileId);
  if (isProfileUsable(explicit)) return explicit;
  return state.profiles.find(isProfileUsable);
}
