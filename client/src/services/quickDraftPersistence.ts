import { del, get, set } from "idb-keyval";

import { ACTIVE_QUICK_DRAFT_KEY, QUICK_DRAFT_KEY_PREFIX } from "../constants/storage";
import type { DraftPhase, PoolSortMode } from "../stores/draftStore";
import { getDraftStore } from "./draftPersistence";

// ── Types ──────────────────────────────────────────────────────────────

export interface ActiveQuickDraftMeta {
  id: string;
  setCode: string;
  difficulty: number;
  phase: "drafting" | "deckbuilding";
  updatedAt: number;
}

interface PersistedQuickDraftSession {
  compressedSessionJson: ArrayBuffer;
  compressed: boolean;
  mainDeck: string[];
  landCounts: Record<string, number>;
  poolSortMode: PoolSortMode;
  poolPanelOpen: boolean;
}

export interface QuickDraftSnapshot {
  sessionJson: string;
  mainDeck: string[];
  landCounts: Record<string, number>;
  poolSortMode: PoolSortMode;
  poolPanelOpen: boolean;
}

// ── Compression ────────────────────────────────────────────────────────

const SESSION_TTL_MS = 24 * 60 * 60 * 1000;
const HAS_COMPRESSION = typeof CompressionStream !== "undefined";

async function compressString(input: string): Promise<ArrayBuffer> {
  const encoded = new TextEncoder().encode(input);
  if (!HAS_COMPRESSION) return encoded.buffer as ArrayBuffer;
  const stream = new Blob([encoded])
    .stream()
    .pipeThrough(new CompressionStream("gzip"));
  return new Response(stream).arrayBuffer();
}

async function decompressToString(buf: ArrayBuffer, wasCompressed: boolean): Promise<string> {
  if (!wasCompressed) return new TextDecoder().decode(buf);
  const stream = new Blob([buf])
    .stream()
    .pipeThrough(new DecompressionStream("gzip"));
  return new Response(stream).text();
}

// ── Meta (localStorage — synchronous) ──────────────────────────────────

export function saveActiveQuickDraft(meta: ActiveQuickDraftMeta): void {
  localStorage.setItem(ACTIVE_QUICK_DRAFT_KEY, JSON.stringify(meta));
}

export function loadActiveQuickDraft(): ActiveQuickDraftMeta | null {
  try {
    const raw = localStorage.getItem(ACTIVE_QUICK_DRAFT_KEY);
    if (!raw) return null;
    const meta = JSON.parse(raw) as ActiveQuickDraftMeta;
    if (Date.now() - meta.updatedAt > SESSION_TTL_MS) {
      void clearQuickDraftSession(meta.id);
      return null;
    }
    return meta;
  } catch {
    return null;
  }
}

export function clearActiveQuickDraft(): void {
  localStorage.removeItem(ACTIVE_QUICK_DRAFT_KEY);
}

// ── Session (IndexedDB — async, compressed) ────────────────────────────

export async function saveQuickDraftSession(
  id: string,
  sessionJson: string,
  uiState: {
    phase: DraftPhase;
    mainDeck: string[];
    landCounts: Record<string, number>;
    poolSortMode: PoolSortMode;
    poolPanelOpen: boolean;
  },
): Promise<void> {
  try {
    const blob = await compressString(sessionJson);
    const data: PersistedQuickDraftSession = {
      compressedSessionJson: blob,
      compressed: HAS_COMPRESSION,
      mainDeck: uiState.mainDeck,
      landCounts: uiState.landCounts,
      poolSortMode: uiState.poolSortMode,
      poolPanelOpen: uiState.poolPanelOpen,
    };
    await set(QUICK_DRAFT_KEY_PREFIX + id, data, getDraftStore());
  } catch (err) {
    console.warn("[saveQuickDraftSession] IDB write failed:", err);
  }
}

export async function loadQuickDraftSession(
  id: string,
): Promise<QuickDraftSnapshot | null> {
  try {
    const data = await get<PersistedQuickDraftSession>(
      QUICK_DRAFT_KEY_PREFIX + id,
      getDraftStore(),
    );
    if (!data) return null;
    const sessionJson = await decompressToString(data.compressedSessionJson, data.compressed ?? true);
    return {
      sessionJson,
      mainDeck: data.mainDeck,
      landCounts: data.landCounts,
      poolSortMode: data.poolSortMode,
      poolPanelOpen: data.poolPanelOpen,
    };
  } catch {
    return null;
  }
}

export async function clearQuickDraftSession(id: string): Promise<void> {
  try {
    await del(QUICK_DRAFT_KEY_PREFIX + id, getDraftStore());
  } catch { /* best-effort */ }
  clearActiveQuickDraft();
}
