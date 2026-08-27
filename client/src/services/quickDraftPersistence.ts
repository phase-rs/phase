import { del, get, set } from "idb-keyval";

import { ACTIVE_QUICK_DRAFT_KEY, DRAFT_RUN_KEY_PREFIX, QUICK_DRAFT_KEY_PREFIX } from "../constants/storage";
import {
  validateWorkspaceState,
  type DraftWorkspaceState,
} from "../components/draft/workspace/types";
import type { DraftPhase, LocalDraftKind, PoolSortMode } from "../stores/draftStore";
import { getDraftStore } from "./draftPersistence";

export type DraftRunFormat = "single" | "bo3" | "run";

export interface ActiveQuickDraftMeta {
  id: string;
  setCode: string;
  setName?: string;
  difficulty: number;
  kind?: LocalDraftKind;
  phase: "drafting" | "opening" | "deckbuilding" | "launching" | "playing" | "complete";
  pickCount: number;
  updatedAt: number;
  runFormat?: DraftRunFormat;
  runWins?: number;
  runLosses?: number;
  runDraws?: number;
  currentGameId?: string;
}

export type DraftMatchResult = "win" | "loss" | "draw";

export interface DraftRunActiveMatch {
  draftId: string;
  gameId: string;
  format: DraftRunFormat;
  resultCountAtLaunch: number;
  botSeat: number;
  opponentDeck: string[];
}

export interface DraftRunState {
  format: DraftRunFormat;
  results: Array<{ gameId: string; result: DraftMatchResult }>;
  playerDeck: string[];
  opponentDeck: string[];
  usedBotSeats: number[];
  activeMatch?: DraftRunActiveMatch;
}

interface PersistedQuickDraftSession {
  compressedSessionJson: ArrayBuffer;
  compressed: boolean;
  mainDeck: string[];
  landCounts: Record<string, number>;
  poolSortMode: PoolSortMode;
  poolPanelOpen: boolean;
  workspace?: unknown;
}

export interface QuickDraftSnapshot {
  sessionJson: string;
  mainDeck: string[];
  landCounts: Record<string, number>;
  poolSortMode: PoolSortMode;
  poolPanelOpen: boolean;
  workspace: DraftWorkspaceState | null;
}

export interface QuickDraftSnapshotInput {
  phase: DraftPhase;
  mainDeck: string[];
  landCounts: Record<string, number>;
  poolSortMode: PoolSortMode;
  poolPanelOpen: boolean;
  workspace?: DraftWorkspaceState;
}

export interface DraftMatchPayload {
  player: { main_deck: string[]; sideboard: string[]; commander: string[] };
  opponent: { main_deck: string[]; sideboard: string[]; commander: string[] };
  ai_decks: never[];
}

const SESSION_TTL_MS = 24 * 60 * 60 * 1000;
const DRAFT_DECK_SESSION_KEY = "phase:draft-deck";
const HAS_COMPRESSION = typeof CompressionStream !== "undefined";

let persistenceTail: Promise<void> = Promise.resolve();

function enqueuePersistence<T>(work: () => Promise<T> | T): Promise<T> {
  const operation = persistenceTail.then(work);
  persistenceTail = operation.then(
    () => undefined,
    () => undefined,
  );
  return operation;
}

export async function drainQuickDraftPersistence(): Promise<void> {
  await persistenceTail;
}

async function compressString(input: string): Promise<ArrayBuffer> {
  const encoded = new TextEncoder().encode(input);
  if (!HAS_COMPRESSION) return encoded.buffer as ArrayBuffer;
  const stream = new Blob([encoded]).stream().pipeThrough(new CompressionStream("gzip"));
  return new Response(stream).arrayBuffer();
}

async function decompressToString(buf: ArrayBuffer, wasCompressed: boolean): Promise<string> {
  if (!wasCompressed) return new TextDecoder().decode(buf);
  const stream = new Blob([buf]).stream().pipeThrough(new DecompressionStream("gzip"));
  return new Response(stream).text();
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function parseActiveMeta(raw: string | null): ActiveQuickDraftMeta | null {
  if (!raw) return null;
  try {
    const value: unknown = JSON.parse(raw);
    if (!isRecord(value)
      || typeof value.id !== "string" || value.id.length === 0
      || typeof value.setCode !== "string"
      || typeof value.difficulty !== "number"
      || typeof value.pickCount !== "number"
      || typeof value.updatedAt !== "number") return null;
    return value as unknown as ActiveQuickDraftMeta;
  } catch {
    return null;
  }
}

function isExpired(meta: ActiveQuickDraftMeta): boolean {
  return !Number.isFinite(meta.updatedAt) || Date.now() - meta.updatedAt > SESSION_TTL_MS;
}

function parseWorkspace(value: unknown): DraftWorkspaceState | null {
  const validated = validateWorkspaceState(value);
  return "error" in validated ? null : validated;
}

function rawReadMeta(): { raw: string | null; meta: ActiveQuickDraftMeta | null } {
  const raw = localStorage.getItem(ACTIVE_QUICK_DRAFT_KEY);
  return { raw, meta: parseActiveMeta(raw) };
}

function rawWriteMeta(meta: ActiveQuickDraftMeta): void {
  localStorage.setItem(ACTIVE_QUICK_DRAFT_KEY, JSON.stringify(meta));
}

function rawDeleteMetaIfMatching(raw: string | null, draftId?: string): void {
  const currentRaw = localStorage.getItem(ACTIVE_QUICK_DRAFT_KEY);
  if (raw !== null && currentRaw === raw) {
    localStorage.removeItem(ACTIVE_QUICK_DRAFT_KEY);
    return;
  }
  if (draftId && parseActiveMeta(currentRaw)?.id === draftId) {
    localStorage.removeItem(ACTIVE_QUICK_DRAFT_KEY);
  }
}

async function rawSaveQuickDraftSession(
  id: string,
  sessionJson: string,
  uiState: QuickDraftSnapshotInput,
): Promise<void> {
  const data: PersistedQuickDraftSession = {
    compressedSessionJson: await compressString(sessionJson),
    compressed: HAS_COMPRESSION,
    mainDeck: uiState.mainDeck,
    landCounts: uiState.landCounts,
    poolSortMode: uiState.poolSortMode,
    poolPanelOpen: uiState.poolPanelOpen,
    workspace: uiState.workspace,
  };
  await set(QUICK_DRAFT_KEY_PREFIX + id, data, getDraftStore());
}

async function rawLoadQuickDraftSession(id: string): Promise<QuickDraftSnapshot | null> {
  const data = await get<PersistedQuickDraftSession>(QUICK_DRAFT_KEY_PREFIX + id, getDraftStore());
  if (!data) return null;
  return {
    sessionJson: await decompressToString(data.compressedSessionJson, data.compressed ?? true),
    mainDeck: data.mainDeck,
    landCounts: data.landCounts,
    poolSortMode: data.poolSortMode,
    poolPanelOpen: data.poolPanelOpen,
    workspace: parseWorkspace(data.workspace),
  };
}

async function rawDeleteQuickDraftSession(id: string): Promise<void> {
  await del(QUICK_DRAFT_KEY_PREFIX + id, getDraftStore());
}

async function rawSaveDraftRun(id: string, state: DraftRunState): Promise<void> {
  await set(DRAFT_RUN_KEY_PREFIX + id, state, getDraftStore());
}

async function rawLoadDraftRun(id: string): Promise<DraftRunState | null> {
  return await get<DraftRunState>(DRAFT_RUN_KEY_PREFIX + id, getDraftStore()) ?? null;
}

async function rawDeleteDraftRun(id: string): Promise<void> {
  await del(DRAFT_RUN_KEY_PREFIX + id, getDraftStore());
}

function payloadKey(gameId: string): string {
  return `${DRAFT_DECK_SESSION_KEY}:${gameId}`;
}

function rawPublishPayloadIfMissing(gameId: string, payload: DraftMatchPayload): void {
  const key = payloadKey(gameId);
  const expected = JSON.stringify(payload);
  const current = sessionStorage.getItem(key);
  if (current === null) sessionStorage.setItem(key, expected);
  else if (current !== expected) throw new Error(`Conflicting draft match payload for ${gameId}`);
}

export function saveActiveQuickDraft(meta: ActiveQuickDraftMeta): void {
  rawWriteMeta(meta);
}

export function loadActiveQuickDraft(): ActiveQuickDraftMeta | null {
  const meta = rawReadMeta().meta;
  return meta && !isExpired(meta) ? meta : null;
}

export function clearActiveQuickDraft(): void {
  localStorage.removeItem(ACTIVE_QUICK_DRAFT_KEY);
}

export function saveQuickDraftSession(
  id: string,
  sessionJson: string,
  uiState: QuickDraftSnapshotInput,
): Promise<void> {
  return enqueuePersistence(() => rawSaveQuickDraftSession(id, sessionJson, uiState));
}

export function loadQuickDraftSession(id: string): Promise<QuickDraftSnapshot | null> {
  return enqueuePersistence(() => rawLoadQuickDraftSession(id));
}

export function clearQuickDraftSession(id: string): Promise<void> {
  return enqueuePersistence(async () => {
    const { raw, meta } = rawReadMeta();
    await rawDeleteQuickDraftSession(id);
    if (meta?.id === id) rawDeleteMetaIfMatching(raw, id);
  });
}

export function saveDraftRun(id: string, state: DraftRunState): Promise<void> {
  return enqueuePersistence(() => rawSaveDraftRun(id, state));
}

export function loadDraftRun(id: string): Promise<DraftRunState | null> {
  return enqueuePersistence(() => rawLoadDraftRun(id));
}

export function clearDraftRun(id: string): Promise<void> {
  return enqueuePersistence(() => rawDeleteDraftRun(id));
}

export function inspectActiveQuickDraftLifecycle(
  mode: "inspect" | "consume",
): Promise<ActiveQuickDraftMeta | null> {
  return enqueuePersistence(async () => {
    const { raw, meta } = rawReadMeta();
    if (!raw) return null;
    if (!meta) {
      rawDeleteMetaIfMatching(raw);
      return null;
    }
    if (isExpired(meta) || mode === "consume") {
      await rawDeleteQuickDraftSession(meta.id);
      await rawDeleteDraftRun(meta.id);
      rawDeleteMetaIfMatching(raw, meta.id);
      return mode === "consume" && !isExpired(meta) ? meta : null;
    }
    return meta;
  });
}

export function cleanupQuickDraftLifecycle(id: string): Promise<void> {
  return enqueuePersistence(async () => {
    const { raw, meta } = rawReadMeta();
    await rawDeleteQuickDraftSession(id);
    await rawDeleteDraftRun(id);
    if (meta?.id === id) rawDeleteMetaIfMatching(raw, id);
  });
}

export function persistQuickDraftSnapshot(
  id: string,
  sessionJson: string,
  uiState: QuickDraftSnapshotInput,
  meta: ActiveQuickDraftMeta,
): Promise<void> {
  return enqueuePersistence(async () => {
    await rawSaveQuickDraftSession(id, sessionJson, uiState);
    rawWriteMeta(meta);
  });
}

export function publishInitialDraftMatch(input: {
  draftId: string;
  sessionJson: string;
  snapshot: QuickDraftSnapshotInput;
  run: DraftRunState;
  gameId: string;
  payload: DraftMatchPayload;
  meta: ActiveQuickDraftMeta;
}): Promise<void> {
  return enqueuePersistence(async () => {
    await rawSaveQuickDraftSession(input.draftId, input.sessionJson, input.snapshot);
    await rawSaveDraftRun(input.draftId, input.run);
    rawPublishPayloadIfMissing(input.gameId, input.payload);
    const current = rawReadMeta().meta;
    if (JSON.stringify(current) !== JSON.stringify(input.meta)) rawWriteMeta(input.meta);
  });
}

export function publishStagedDraftMatch(input: {
  draftId: string;
  run?: DraftRunState;
  gameId: string;
  payload: DraftMatchPayload;
  meta: ActiveQuickDraftMeta;
}): Promise<void> {
  return enqueuePersistence(async () => {
    if (input.run) await rawSaveDraftRun(input.draftId, input.run);
    rawPublishPayloadIfMissing(input.gameId, input.payload);
    const current = rawReadMeta().meta;
    if (JSON.stringify(current) !== JSON.stringify(input.meta)) rawWriteMeta(input.meta);
  });
}

export function recordDraftMatchResult(input: {
  draftId: string;
  gameId: string;
  result: DraftMatchResult;
  makeMeta: (run: DraftRunState) => ActiveQuickDraftMeta;
}): Promise<{ run: DraftRunState; meta: ActiveQuickDraftMeta } | null> {
  return enqueuePersistence(async () => {
    const run = await rawLoadDraftRun(input.draftId);
    if (!run) return null;
    if (run.activeMatch && run.activeMatch.gameId !== input.gameId) {
      throw new Error("Match result does not match the active draft game");
    }
    const alreadyRecorded = run.results.some((entry) => entry.gameId === input.gameId);
    const { activeMatch: _resolvedMatch, ...resolvedRun } = run;
    const nextRun: DraftRunState = {
      ...resolvedRun,
      results: alreadyRecorded
        ? run.results
        : [...run.results, { gameId: input.gameId, result: input.result }],
    };
    if (!alreadyRecorded || run.activeMatch !== undefined) {
      await rawSaveDraftRun(input.draftId, nextRun);
    }
    const meta = input.makeMeta(nextRun);
    rawWriteMeta(meta);
    return { run: nextRun, meta };
  });
}

export function runLimits(format: DraftRunFormat): { maxWins: number; maxLosses: number } {
  switch (format) {
    case "single": return { maxWins: 1, maxLosses: 1 };
    case "bo3": return { maxWins: 1, maxLosses: 1 };
    case "run": return { maxWins: 7, maxLosses: 3 };
  }
}
