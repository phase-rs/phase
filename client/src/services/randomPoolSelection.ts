import type { DeckCatalogSource } from "./deckCatalog";
import {
  listFolders,
  loadDeckMetaMap,
  type DeckFolder,
  type DeckMeta,
} from "../constants/storage";

/**
 * Which random draw a pool selection governs. The human pool feeds the
 * player's own Random deck (the MyDecks tile's immediate pick and the deferred
 * game-start pick); the AI pool feeds every AI Random seat (single-player
 * setup, Host Game, and the live room tile).
 */
export type RandomPoolKind = "human" | "ai";

/**
 * Persisted inclusion state for one random pool. All-empty means "include
 * everything" — the pre-feature behavior — so fresh and migrated stores need
 * no backfill and unknown ids/folders are harmless no-ops.
 *
 * - `excludedGroups`: group keys toggled off wholesale (a folder, Unfiled,
 *   one feed, or the precon set).
 * - `excludedDecks`: catalog ids toggled off individually.
 * - `includedDecks`: catalog ids explicitly toggled back on inside an
 *   excluded group. An explicit deck state always wins over its group, in
 *   both directions, so a folder toggle followed by per-deck flips reads as
 *   "all off except these".
 */
export interface RandomPoolSelection {
  excludedGroups: string[];
  excludedDecks: string[];
  includedDecks: string[];
}

export function emptyPoolSelection(): RandomPoolSelection {
  return { excludedGroups: [], excludedDecks: [], includedDecks: [] };
}

export function isPoolSelectionEmpty(selection: RandomPoolSelection): boolean {
  return (
    selection.excludedGroups.length === 0 &&
    selection.excludedDecks.length === 0 &&
    selection.includedDecks.length === 0
  );
}

/** Normalize a persisted selection: malformed blobs reset to include-all. */
export function normalizePoolSelection(value: unknown): RandomPoolSelection {
  if (!value || typeof value !== "object") return emptyPoolSelection();
  const record = value as Partial<RandomPoolSelection>;
  const strings = (input: unknown): string[] =>
    Array.isArray(input) ? input.filter((entry): entry is string => typeof entry === "string") : [];
  return {
    excludedGroups: strings(record.excludedGroups),
    excludedDecks: strings(record.excludedDecks),
    includedDecks: strings(record.includedDecks),
  };
}

/** Group key for saved decks that belong to no folder. */
export const UNFILED_POOL_GROUP = "unfiled";
/** Group key for every precon-shaped candidate (MTGJSON precons + bundled cEDH). */
export const PRECON_POOL_GROUP = "precon";

export function folderPoolGroup(folderId: string): string {
  return `folder:${folderId}`;
}

export function feedPoolGroup(feedId: string): string {
  return `feed:${feedId}`;
}

/**
 * Minimal candidate shape for pool grouping: a catalog id, its source, a
 * display name, and — for saved decks — the library name that resolves the
 * folder. Each caller maps to this shape at the boundary (`name` already is
 * the library name for AI catalog candidates; MyDecks candidates map
 * `deckName`).
 */
export interface PoolCandidate {
  id: string;
  name: string;
  source: DeckCatalogSource;
  savedName?: string;
}

/**
 * One-shot folder index over the saved-deck library: a single read of the
 * folder registry + metadata map that answers every lookup in a filtering
 * pass without re-parsing localStorage per deck.
 */
export interface FolderIndex {
  /**
   * The live folder id for a saved deck, or undefined for Unfiled. A
   * `folderId` naming a deleted folder self-heals to Unfiled — the same rule
   * `groupSavedDecks` applies — so a removed folder never strands its decks
   * in an unreachable group.
   */
  folderIdOf(savedName: string): string | undefined;
}

export function buildFolderIndex(folders: DeckFolder[], meta: Record<string, DeckMeta>): FolderIndex {
  const live = new Set(folders.map((folder) => folder.id));
  return {
    folderIdOf: (savedName: string) => {
      const id = meta[savedName]?.folderId;
      return id && live.has(id) ? id : undefined;
    },
  };
}

export function loadFolderIndex(): FolderIndex {
  return buildFolderIndex(listFolders(), loadDeckMetaMap());
}

/**
 * Map catalog-shaped candidates to pool candidates. For saved decks the
 * display `name` is the library name, which is what resolves the folder
 * (this holds for both the AI catalog and MyDecks' candidates).
 */
export function withCatalogSavedNames<T extends { name: string; source: DeckCatalogSource }>(
  candidates: readonly T[],
): (T & { savedName?: string })[] {
  return candidates.map((candidate) => ({
    ...candidate,
    savedName: candidate.source.type === "saved" ? candidate.name : undefined,
  }));
}

export function poolGroupForCandidate(candidate: PoolCandidate, index: FolderIndex): string {
  switch (candidate.source.type) {
    case "feed":
      return feedPoolGroup(candidate.source.feedId);
    case "precon":
      return PRECON_POOL_GROUP;
    case "saved": {
      // A saved feed mirror keeps its origin: the `feed:<id>` toggle must
      // catch it even though it lives in saved storage (possibly inside a
      // user folder). An adopted deck, whose provenance was removed, falls
      // through to user organization below. Explicit per-deck overrides live
      // outside grouping, so they keep winning either way.
      if (candidate.source.feedId) return feedPoolGroup(candidate.source.feedId);
      if (!candidate.savedName) return UNFILED_POOL_GROUP;
      const folderId = index.folderIdOf(candidate.savedName);
      return folderId ? folderPoolGroup(folderId) : UNFILED_POOL_GROUP;
    }
  }
}

type DeckOverride = "included" | "excluded" | "inherit";

interface SelectionMembership {
  deckState(id: string): DeckOverride;
  groupExcluded(key: string): boolean;
}

/** Single authority for "explicit deck state wins over its group, either way". */
function membershipOf(selection: RandomPoolSelection): SelectionMembership {
  const excludedGroups = new Set(selection.excludedGroups);
  const excludedDecks = new Set(selection.excludedDecks);
  const includedDecks = new Set(selection.includedDecks);
  return {
    deckState: (id: string) => {
      if (includedDecks.has(id)) return "included";
      if (excludedDecks.has(id)) return "excluded";
      return "inherit";
    },
    groupExcluded: (key: string) => excludedGroups.has(key),
  };
}

export function candidateInPool(
  candidate: PoolCandidate,
  selection: RandomPoolSelection,
  index: FolderIndex,
): boolean {
  const membership = membershipOf(selection);
  const state = membership.deckState(candidate.id);
  if (state !== "inherit") return state === "included";
  return !membership.groupExcluded(poolGroupForCandidate(candidate, index));
}

export interface AppliedPoolSelection<T> {
  pool: T[];
  /**
   * True when the selection matched nothing and the full input was kept, so
   * callers can warn instead of silently serving excluded decks.
   */
  fellBack: boolean;
}

/**
 * Restrict candidates to the selection, falling back to the full input when
 * the selection matches nothing (warn-plus-fallback: an over-tight selection
 * never blocks a game, it just warns at setup). Pinned seats bypass this
 * entirely — callers resolve explicit ids against the unfiltered catalog.
 */
export function applyPoolSelection<T extends PoolCandidate>(
  candidates: readonly T[],
  selection: RandomPoolSelection,
  index?: FolderIndex,
): AppliedPoolSelection<T> {
  if (isPoolSelectionEmpty(selection)) return { pool: [...candidates], fellBack: false };
  const resolved = index ?? loadFolderIndex();
  const membership = membershipOf(selection);
  const pool = candidates.filter((candidate) => {
    const state = membership.deckState(candidate.id);
    if (state !== "inherit") return state === "included";
    return !membership.groupExcluded(poolGroupForCandidate(candidate, resolved));
  });
  if (pool.length === 0 && candidates.length > 0) return { pool: [...candidates], fellBack: true };
  return { pool, fellBack: false };
}

function without(values: readonly string[], remove: ReadonlySet<string>): string[] {
  return values.filter((value) => !remove.has(value));
}

function withId(values: readonly string[], id: string): string[] {
  return values.includes(id) ? [...values] : [...values, id];
}

/**
 * Set every deck in `groupKey` to `included`, clearing per-deck overrides for
 * `memberIds`. A group toggle is "select all / unselect all": decks flipped
 * afterwards record fresh overrides via `setPoolDeckIncluded`, so toggling a
 * folder off and re-checking two decks reads as "all off except these two"
 * rather than reviving stale state.
 */
export function setPoolGroupIncluded(
  selection: RandomPoolSelection,
  groupKey: string,
  memberIds: readonly string[],
  included: boolean,
): RandomPoolSelection {
  const members = new Set(memberIds);
  return {
    excludedGroups: included
      ? selection.excludedGroups.filter((group) => group !== groupKey)
      : withId(selection.excludedGroups, groupKey),
    excludedDecks: without(selection.excludedDecks, members),
    includedDecks: without(selection.includedDecks, members),
  };
}

/** Record one deck's explicit state; it wins over its group either way. */
export function setPoolDeckIncluded(
  selection: RandomPoolSelection,
  deckId: string,
  included: boolean,
): RandomPoolSelection {
  return {
    excludedGroups: [...selection.excludedGroups],
    excludedDecks: included
      ? selection.excludedDecks.filter((id) => id !== deckId)
      : withId(selection.excludedDecks, deckId),
    includedDecks: included
      ? withId(selection.includedDecks, deckId)
      : selection.includedDecks.filter((id) => id !== deckId),
  };
}

export interface PoolSelectionSummary {
  /** Decks included by the selection. */
  selected: number;
  /** Selected decks that also survive the draw-base restriction. Without a
   *  base this equals `selected`. */
  playable: number;
  /** Draw-base members present in `items` (or `items.length` without a base). */
  baseSize: number;
}

/**
 * Compare a selection against a draw base (e.g. the AI bracket pool): how
 * many decks are selected, and how many of those the base leaves playable.
 * Single authority behind the tree header counts and the setup pages'
 * bracket-conflict banners, so all three always agree with each other and
 * with the draws.
 */
export function summarizePoolSelection(
  items: readonly PoolCandidate[],
  selection: RandomPoolSelection,
  baseIds: ReadonlySet<string> | undefined,
  index: FolderIndex,
): PoolSelectionSummary {
  let selected = 0;
  let playable = 0;
  let baseSize = 0;
  const membership = membershipOf(selection);
  for (const item of items) {
    const inBase = !baseIds || baseIds.has(item.id);
    if (inBase) baseSize += 1;
    const state = membership.deckState(item.id);
    const inPool = state === "included"
      || (state === "inherit" && !membership.groupExcluded(poolGroupForCandidate(item, index)));
    if (!inPool) continue;
    selected += 1;
    if (inBase) playable += 1;
  }
  return { selected, playable, baseSize };
}

export type PoolGroupState = "all" | "none" | "mixed";

/** Tri-state for a group checkbox over the selection-effective membership. */
export function poolGroupState(
  memberIds: readonly string[],
  groupKey: string,
  selection: RandomPoolSelection,
): PoolGroupState {
  const membership = membershipOf(selection);
  const groupOff = membership.groupExcluded(groupKey);
  let inCount = 0;
  for (const id of memberIds) {
    const state = membership.deckState(id);
    if (state === "included" || (state === "inherit" && !groupOff)) inCount += 1;
  }
  if (inCount === 0) return "none";
  if (inCount === memberIds.length) return "all";
  return "mixed";
}

export type PoolSectionKind = "folder" | "unfiled" | "feed" | "precon";

export interface PoolSection<T extends PoolCandidate> {
  key: string;
  kind: PoolSectionKind;
  /** Folder or feed display name; null for Unfiled/Precons (the tree localizes those). */
  name: string | null;
  members: T[];
}

export interface PoolSectionSource {
  folders: DeckFolder[];
  feedNameOf(feedId: string): string;
}

function sectionKindForKey(key: string): PoolSectionKind {
  if (key === UNFILED_POOL_GROUP) return "unfiled";
  if (key === PRECON_POOL_GROUP) return "precon";
  if (key.startsWith("folder:")) return "folder";
  if (key.startsWith("feed:")) return "feed";
  return "unfiled";
}

/**
 * Group candidates into toggle-tree sections: user folders in registry order,
 * then Unfiled, then feeds alphabetically, then Precons. Only groups with at
 * least one candidate are emitted — the tree shows the current pool, not the
 * whole library — and members render alphabetically for a stable layout.
 */
export function buildPoolSections<T extends PoolCandidate>(
  candidates: readonly T[],
  source: PoolSectionSource,
  index: FolderIndex,
): PoolSection<T>[] {
  const sections = new Map<string, PoolSection<T>>();
  const folderById = new Map(source.folders.map((folder) => [folder.id, folder]));
  for (const candidate of candidates) {
    const key = poolGroupForCandidate(candidate, index);
    let section = sections.get(key);
    if (!section) {
      const kind = sectionKindForKey(key);
      section = { key, kind, name: null, members: [] };
      if (kind === "folder") {
        section.name = folderById.get(key.slice("folder:".length))?.name ?? null;
      } else if (kind === "feed") {
        section.name = source.feedNameOf(key.slice("feed:".length));
      }
      sections.set(key, section);
    }
    section.members.push(candidate);
  }
  for (const section of sections.values()) {
    section.members.sort((a, b) => a.name.localeCompare(b.name));
  }
  const kindRank = (kind: PoolSectionKind): number => {
    switch (kind) {
      case "folder":
        return 0;
      case "unfiled":
        return 1;
      case "feed":
        return 2;
      case "precon":
        return 3;
    }
  };
  const folderRank = new Map(
    source.folders.map((folder) => [folderPoolGroup(folder.id), folder.order]),
  );
  return [...sections.values()].sort(
    (a, b) =>
      kindRank(a.kind) - kindRank(b.kind) ||
      (folderRank.get(a.key) ?? 0) - (folderRank.get(b.key) ?? 0) ||
      (a.name ?? "").localeCompare(b.name ?? ""),
  );
}
