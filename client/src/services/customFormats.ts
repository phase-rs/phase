/**
 * Client-persisted saved custom formats (Axis A).
 *
 * A "save as custom format" action captures the lobby's live, fully-resolved
 * `FormatConfig` as a `CustomFormatDef` — produced by the ENGINE
 * (`customFormatFromLobbyConfig`), never assembled here — and stores it in
 * `localStorage`. There is no server-side registry write path in this phase.
 *
 * Why definitions need a client-generated id of their own: every Axis-A save
 * carries the engine's reserved sentinel `LOBBY_SAVE_CUSTOM_FORMAT_ID`
 * (`CustomFormatId(0)`), deliberately, so an ad-hoc save can never impersonate
 * a registry-stable preset. That makes `rules.id` useless for telling two saved
 * formats apart — it is 0 for all of them. `SavedCustomFormat.id` is the only
 * value that can, which is why the remembered host config persists it rather
 * than the engine id.
 *
 * `localStorage` is a durable boundary shared with older and newer builds, so
 * nothing read back from it is trusted by shape. Every load runs the stored
 * blob through a real structural guard and drops entries that fail, exactly as
 * `normalizeRememberedHostConfig` does for the host-setup snapshot.
 */

import type { CustomFormatDef, CustomFormatRules } from "../adapter/types";
import { isCustomFormatRulesShape } from "../adapter/format-config-shape";

const STORAGE_KEY = "phase-custom-formats";

/** A saved definition plus the client-side identity the lobby refers to it by. */
export interface SavedCustomFormat {
  /** Client-generated, stable for the life of the save. See the module note on
   *  why `def.rules.id` cannot serve this purpose. */
  id: string;
  /** The player-supplied name. Mirrors `def.label`; kept as its own field so a
   *  rename flow (a later phase) need not rewrite the engine-produced blob. */
  name: string;
  def: CustomFormatDef;
  /** Epoch ms, for stable newest-last ordering in the picker. */
  savedAt: number;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isCustomFormatDefShape(value: unknown): value is CustomFormatDef {
  if (!isRecord(value)) return false;
  if (
    typeof value.label !== "string"
    || typeof value.short_label !== "string"
    || typeof value.description !== "string"
  ) {
    return false;
  }
  const reprint = value.reprint_policy;
  if (
    reprint !== null
    && reprint !== undefined
    && reprint !== "OriginalPrintingsOnly"
    && reprint !== "AllowSpecialReprintSets"
    && reprint !== "AllowAnyPrinting"
  ) {
    return false;
  }
  if (
    value.printing_fidelity !== "NotApplicable"
    && value.printing_fidelity !== "SetCodeApproximation"
  ) {
    return false;
  }
  return isCustomFormatRulesShape(value.rules);
}

function isSavedCustomFormat(value: unknown): value is SavedCustomFormat {
  if (!isRecord(value)) return false;
  return (
    typeof value.id === "string"
    && value.id.length > 0
    && typeof value.name === "string"
    && typeof value.savedAt === "number"
    && Number.isFinite(value.savedAt)
    && isCustomFormatDefShape(value.def)
  );
}

/**
 * Every well-formed saved format, oldest first. Synchronous by contract:
 * `normalizeRememberedHostConfig` runs inside a Zustand `set()` and cannot
 * await, so rehydration must be able to resolve a saved definition without a
 * WASM round-trip.
 *
 * Malformed entries are dropped individually rather than failing the whole
 * read — one unreadable save from an older build must not erase the rest.
 */
export function loadSavedCustomFormats(): SavedCustomFormat[] {
  let raw: string | null;
  try {
    raw = localStorage.getItem(STORAGE_KEY);
  } catch {
    // Private-mode / disabled storage. No saves is the honest answer.
    return [];
  }
  if (!raw) return [];

  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(parsed)) return [];
  return parsed
    .filter(isSavedCustomFormat)
    .sort((a, b) => a.savedAt - b.savedAt);
}

/** The saved format with this client-side id, or `null`. Synchronous, for the
 *  same reason as {@link loadSavedCustomFormats}. */
export function findSavedCustomFormat(id: string | null): SavedCustomFormat | null {
  if (!id) return null;
  return loadSavedCustomFormats().find((saved) => saved.id === id) ?? null;
}

function persist(formats: SavedCustomFormat[]): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(formats));
  } catch {
    // Quota exceeded or storage disabled. The caller's in-memory selection is
    // still valid for this session; only persistence is lost.
  }
}

/**
 * Store an engine-produced definition under a new client-side id and return the
 * stored record.
 *
 * `def` MUST come from `customFormatFromLobbyConfig` — this function does not
 * and cannot validate that the rules are ones the engine would accept.
 */
export function saveCustomFormat(name: string, def: CustomFormatDef): SavedCustomFormat {
  const saved: SavedCustomFormat = {
    id: crypto.randomUUID(),
    name,
    def,
    savedAt: Date.now(),
  };
  persist([...loadSavedCustomFormats(), saved]);
  return saved;
}

/** Remove a saved format. A remembered host config pointing at it degrades to
 *  defaults on the next rehydration, the same as any other unresolvable case. */
export function deleteSavedCustomFormat(id: string): void {
  persist(loadSavedCustomFormats().filter((saved) => saved.id !== id));
}

/** The rules a selected saved format resolves from. Convenience for the
 *  select path, which hands these to the engine's resolver. */
export function customFormatRulesOf(saved: SavedCustomFormat): CustomFormatRules {
  return saved.def.rules;
}
