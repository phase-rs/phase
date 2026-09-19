import { create } from "zustand";
import { persist } from "zustand/middleware";

import { LLM_ENDPOINTS_KEY } from "../constants/storage";
import type { LlmProfile, LlmProviderId } from "../services/llm/types";

/**
 * How a profile is persisted: everything except the credential.
 *
 * `apiKey` is deliberately absent. A key written to `localStorage` is readable
 * by anything with script access to the origin and outlives the session that
 * needed it, so keys live in memory for the lifetime of the tab and are
 * re-entered afterwards. {@link STORED_PROFILE_KEYS} is the allowlist the
 * persister projects through, so a field added to `LlmProfile` is NOT persisted
 * until it is named here.
 */
type StoredProfile = Omit<LlmProfile, "apiKey">;

const STORED_PROFILE_KEYS = [
  "id",
  "name",
  "provider",
  "baseUrl",
  "model",
  "maxOutputTokens",
  "temperature",
  "enabled",
] as const satisfies readonly (keyof StoredProfile)[];

function withoutCredential(profile: LlmProfile): StoredProfile {
  return Object.fromEntries(
    STORED_PROFILE_KEYS.map((key) => [key, profile[key]]),
  ) as StoredProfile;
}

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

/**
 * Every provider id the app understands, mirroring `ACCEPTED_PROVIDER_LABELS`
 * in `crates/phase-llm/src/provider.rs`. Kept as a literal list so a persisted
 * string can be checked against it without waiting for the async catalog.
 */
const KNOWN_PROVIDERS: readonly LlmProviderId[] = [
  "OpenAi",
  "Anthropic",
  "Gemini",
  "DeepSeek",
  "OpenAiCompatible",
] as const;

/**
 * Coerce a persisted provider value to a known id.
 *
 * Applies the same normalization `LlmProvider::from_label` does — case- and
 * punctuation-insensitive — and falls back to `OpenAiCompatible` for anything
 * unrecognised, which is exactly what the engine does with an unknown label.
 */
function normalizeProvider(value: unknown): LlmProviderId {
  if (typeof value !== "string") return DEFAULT_PROVIDER;
  const normalized = value.replace(/[^a-zA-Z0-9]/g, "").toLowerCase();
  const match = KNOWN_PROVIDERS.find(
    (provider) => provider.toLowerCase() === normalized,
  );
  return match ?? "OpenAiCompatible";
}

function normalizeString(value: unknown): string {
  return typeof value === "string" ? value : "";
}

/** `null` means "the provider default"; any non-string is treated the same. */
function normalizeNullableString(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

/** A finite positive number, or `null`. Rejects NaN, Infinity and strings. */
function normalizePositiveNumber(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : null;
}

function normalizeFiniteNumber(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

/**
 * Profiles recovered from a persisted payload, NORMALIZED to complete records.
 *
 * Storage is not a trusted input: the record can be hand-edited, truncated by a
 * quota failure, or written by a different build. Two failure modes follow, and
 * filtering alone only addresses the first:
 *
 *  - a shape that THROWS during migrate/merge (`profiles` as `null`, an object,
 *    a string, or an array holding `null`) stops `partialize` from ever running,
 *    leaving a pre-v1 credential on disk and the store unconstructable;
 *  - a shape that SURVIVES but is incomplete — `{ id: "p", enabled: true }` is
 *    valid JSON with a string id — reaches the settings UI, which calls
 *    `.trim()` on `model` and `name` and crashes on `undefined`.
 *
 * Every field is therefore coerced to its declared type here, so anything this
 * function returns satisfies `LlmProfile` structurally and no consumer needs a
 * defensive check of its own. Only `id` is load-bearing enough to reject on:
 * without a stable identity a profile cannot be bound to a seat, edited, or
 * removed, so a record lacking one is dropped rather than given a fabricated id.
 */
function readPersistedProfiles(value: unknown): LlmProfile[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((entry): LlmProfile[] => {
    if (typeof entry !== "object" || entry === null || Array.isArray(entry)) return [];
    const record = entry as Record<string, unknown>;
    const id = typeof record.id === "string" ? record.id.trim() : "";
    if (!id) return [];
    return [
      {
        id,
        name: normalizeString(record.name),
        provider: normalizeProvider(record.provider),
        baseUrl: normalizeNullableString(record.baseUrl),
        // Never restored from storage: credentials are not persisted, and a
        // hand-edited record must not smuggle one back in.
        apiKey: "",
        model: normalizeString(record.model),
        maxOutputTokens: normalizePositiveNumber(record.maxOutputTokens),
        temperature: normalizeFiniteNumber(record.temperature),
        // A profile whose model did not survive cannot be usable, so it must
        // not present itself as enabled.
        enabled: record.enabled === true && normalizeString(record.model).trim() !== "",
      },
    ];
  });
}

/**
 * Seat bindings recovered from a persisted payload.
 *
 * `profileForSeat` indexes this and `removeProfile` calls `Object.entries` on
 * it, so a `null` or non-object value crashes at the first read rather than at
 * hydration — which is why it is coerced here rather than guarded at each use.
 * A key must look like a seat index and a value must be a profile id; anything
 * else is dropped, since a binding that names neither is usable either way.
 */
function readPersistedSeatBindings(value: unknown): Record<number, string> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return {};
  const bindings: Record<number, string> = {};
  for (const [key, profileId] of Object.entries(value as Record<string, unknown>)) {
    const seatIndex = Number(key);
    if (!Number.isInteger(seatIndex) || seatIndex < 0) continue;
    if (typeof profileId !== "string" || !profileId) continue;
    bindings[seatIndex] = profileId;
  }
  return bindings;
}

/**
 * The complete persisted slice, normalized.
 *
 * Validating `profiles` alone left the rest of the record trusted, and every
 * field here is read without a guard somewhere downstream. Normalizing the
 * whole slice in one place keeps "what the store may contain" a single
 * statement rather than a set of assumptions spread across consumers.
 */
function readPersistedState(
  value: unknown,
): Pick<LlmState, "profiles" | "seatBindings" | "draftEnabled" | "draftProfileId"> {
  const record =
    typeof value === "object" && value !== null && !Array.isArray(value)
      ? (value as Record<string, unknown>)
      : {};
  return {
    profiles: readPersistedProfiles(record.profiles),
    seatBindings: readPersistedSeatBindings(record.seatBindings),
    // Strict: only a real `true` enables drafting, so a truthy string cannot
    // silently switch an LLM into a pod.
    draftEnabled: record.draftEnabled === true,
    draftProfileId: typeof record.draftProfileId === "string" ? record.draftProfileId : null,
  };
}

/** Absent, empty and whitespace-only base URLs all mean "the provider default",
 *  so they must compare equal — otherwise clearing a field the player never set
 *  would count as retargeting and wipe a working key. */
function normalizedBaseUrl(url: string | null | undefined): string {
  return (url ?? "").trim();
}

/**
 * Whether a patch points this profile's credential at a different endpoint.
 *
 * True when the patch changes the provider or the base URL to a different
 * value. A patch that merely restates the current value — which the settings
 * form does on every render — is not a change and must not clear anything.
 */
function retargetsCredential(profile: LlmProfile, patch: Partial<LlmProfile>): boolean {
  const providerMoved = patch.provider != null && patch.provider !== profile.provider;
  const endpointMoved =
    patch.baseUrl !== undefined
    && normalizedBaseUrl(patch.baseUrl) !== normalizedBaseUrl(profile.baseUrl);
  return providerMoved || endpointMoved;
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
          profiles: state.profiles.map((profile) => {
            if (profile.id !== id) return profile;
            const next = { ...profile, ...patch };
            // A credential is scoped to the ENDPOINT it was issued for, which is
            // the provider and the base URL together. Either one changing points
            // the key at a different server: switching provider would send an
            // OpenAI key to Anthropic, and editing the base URL would send it to
            // whatever host was typed — including one the player does not
            // control. The key is therefore dropped on either change, unless
            // this very patch supplies its replacement.
            if (retargetsCredential(profile, patch)) {
              next.apiKey = patch.apiKey ?? "";
              next.enabled = patch.enabled ?? false;
            }
            return next;
          }),
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
    {
      name: LLM_ENDPOINTS_KEY,
      version: 1,
      // The credential never reaches storage. A profile rehydrates with an
      // empty `apiKey`, which `isProfileUsable` treats as unconfigured for
      // every provider that requires one.
      partialize: (state) => ({
        profiles: state.profiles.map(withoutCredential),
        seatBindings: state.seatBindings,
        draftEnabled: state.draftEnabled,
        draftProfileId: state.draftProfileId,
      }),
      // Scrub credentials written by the pre-v1 shape, which persisted them.
      // Runs on every load of a v0 record, so an existing key is removed from
      // disk the first time this build reads it rather than lingering.
      migrate: (persisted, version) => {
        if (typeof persisted !== "object" || persisted === null || Array.isArray(persisted)) {
          // Not a state record at all. Returning it unchanged lets `merge`
          // fall back to the store's defaults rather than throwing here.
          return persisted as LlmState;
        }
        const state = persisted as Partial<LlmState>;
        if (version >= 1) return state as LlmState;
        // `merge` normalizes the whole slice regardless; doing it here too
        // keeps a v0 record's credential from surviving the migration step.
        return { ...state, ...readPersistedState(state) } as LlmState;
      },
      // `partialize` stops FUTURE writes from carrying a credential, but a key
      // already on disk would linger there until the next state change. Forcing
      // one write immediately after rehydration re-persists through
      // `partialize` and removes it now, which is the difference between "we
      // stopped storing keys" and "your stored key is gone".
      onRehydrateStorage: () => (state) => {
        if (!state) return;
        // Deferred, not immediate. Persist can invoke this callback
        // SYNCHRONOUSLY during `create(...)` when the storage is synchronous, at
        // which point `useLlmStore` is still in its temporal dead zone and
        // touching it throws a ReferenceError that would take the whole module
        // down at import. A microtask runs after construction has completed, and
        // nothing depends on the scrub landing sooner: `merge` has already
        // blanked the credential in memory, so this write only rewrites the
        // stored record through `partialize` to remove it from disk.
        queueMicrotask(() => {
          useLlmStore.setState({
            profiles: readPersistedProfiles(state.profiles).map((profile) => ({ ...profile })),
          });
        });
      },
      merge: (persisted, current) => ({
        // The persisted slice goes through `readPersistedState` rather than
        // being spread raw: spreading `incoming` let any field the record
        // happened to carry through untyped, so `{seatBindings: null}` survived
        // hydration and then crashed at the first read.
        ...current,
        ...readPersistedState(persisted),
      }),
    },
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
