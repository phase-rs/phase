import { beforeEach, describe, expect, it, vi } from "vitest";

const idb = vi.hoisted(() => {
  const records = new Map<string, unknown>();
  return {
    records,
    del: vi.fn(async (key: string) => { records.delete(key); }),
    get: vi.fn(async (key: string) => records.get(key)),
    set: vi.fn(async (key: string, value: unknown) => { records.set(key, value); }),
  };
});

vi.mock("idb-keyval", () => ({ del: idb.del, get: idb.get, set: idb.set }));
vi.mock("../draftPersistence", () => ({ getDraftStore: () => ({}) }));

import {
  drainQuickDraftPersistence,
  inspectActiveQuickDraftLifecycle,
  loadActiveQuickDraft,
  loadQuickDraftSession,
  recordDraftMatchResult,
  saveDraftRun,
  saveQuickDraftSession,
} from "../quickDraftPersistence";
import { ACTIVE_QUICK_DRAFT_KEY, DRAFT_RUN_KEY_PREFIX, QUICK_DRAFT_KEY_PREFIX } from "../../constants/storage";
import { createDraftWorkspaceState } from "../../components/draft/workspace/workspacePlacement";

const uiState = {
  phase: "deckbuilding" as const,
  mainDeck: ["Spell"],
  landCounts: { Island: 2 },
  poolSortMode: "color" as const,
  poolPanelOpen: true,
};

describe("quick draft persistence coordinator", () => {
  beforeEach(async () => {
    await drainQuickDraftPersistence();
    idb.records.clear();
    vi.clearAllMocks();
    localStorage.clear();
    sessionStorage.clear();
  });

  it("recovers the queue after a strict save rejection", async () => {
    idb.set.mockRejectedValueOnce(new Error("write failed"));
    await expect(saveQuickDraftSession("failed", "{}", uiState)).rejects.toThrow("write failed");

    await expect(saveDraftRun("next", {
      format: "single",
      results: [],
      playerDeck: [],
      opponentDeck: [],
      usedBotSeats: [],
    })).resolves.toBeUndefined();
    expect(idb.records.has(`${DRAFT_RUN_KEY_PREFIX}next`)).toBe(true);
  });

  it("keeps expired records during the synchronous compatibility read", async () => {
    const meta = {
      id: "expired",
      setCode: "TST",
      difficulty: 2,
      phase: "drafting" as const,
      pickCount: 3,
      updatedAt: Date.now() - 25 * 60 * 60 * 1000,
    };
    localStorage.setItem(ACTIVE_QUICK_DRAFT_KEY, JSON.stringify(meta));
    idb.records.set(`${QUICK_DRAFT_KEY_PREFIX}expired`, { session: true });
    idb.records.set(`${DRAFT_RUN_KEY_PREFIX}expired`, { run: true });

    expect(loadActiveQuickDraft()).toBeNull();
    expect(localStorage.getItem(ACTIVE_QUICK_DRAFT_KEY)).toBe(JSON.stringify(meta));
    expect(idb.records.has(`${QUICK_DRAFT_KEY_PREFIX}expired`)).toBe(true);
    expect(idb.records.has(`${DRAFT_RUN_KEY_PREFIX}expired`)).toBe(true);
    expect(idb.del).not.toHaveBeenCalled();

    await expect(inspectActiveQuickDraftLifecycle("inspect")).resolves.toBeNull();
    expect(idb.del.mock.calls.map(([key]) => key)).toEqual([
      `${QUICK_DRAFT_KEY_PREFIX}expired`,
      `${DRAFT_RUN_KEY_PREFIX}expired`,
    ]);
    expect(localStorage.getItem(ACTIVE_QUICK_DRAFT_KEY)).toBeNull();
  });

  it("does not let cleanup delete newer metadata", async () => {
    const expired = {
      id: "expired",
      setCode: "OLD",
      difficulty: 2,
      phase: "drafting" as const,
      pickCount: 0,
      updatedAt: Date.now() - 25 * 60 * 60 * 1000,
    };
    const replacement = { ...expired, id: "new", setCode: "NEW", updatedAt: Date.now() };
    localStorage.setItem(ACTIVE_QUICK_DRAFT_KEY, JSON.stringify(expired));
    idb.del.mockImplementationOnce(async () => {
      localStorage.setItem(ACTIVE_QUICK_DRAFT_KEY, JSON.stringify(replacement));
    });

    await inspectActiveQuickDraftLifecycle("inspect");
    expect(JSON.parse(localStorage.getItem(ACTIVE_QUICK_DRAFT_KEY) ?? "null")).toEqual(replacement);
  });

  it("round_trips_workspace_and_reads_legacy_main_deck_snapshots", async () => {
    const workspace = {
      ...createDraftWorkspaceState(),
      placements: {
        card: { zone: "sideboard" as const, row: 1, column: 2, order: 3 },
      },
      virtualBasics: [{ instanceId: "basic", name: "Island" }],
    };
    await saveQuickDraftSession("workspace", "session", { ...uiState, workspace });
    await expect(loadQuickDraftSession("workspace")).resolves.toMatchObject({
      sessionJson: "session",
      mainDeck: ["Spell"],
      workspace,
    });

    await saveQuickDraftSession("legacy", "legacy-session", uiState);
    await expect(loadQuickDraftSession("legacy")).resolves.toMatchObject({
      sessionJson: "legacy-session",
      mainDeck: ["Spell"],
      workspace: null,
    });
  });

  it("uses the canonical workspace validator when loading persisted state", async () => {
    idb.records.set(`${QUICK_DRAFT_KEY_PREFIX}invalid-workspace`, {
      compressedSessionJson: new TextEncoder().encode("session").buffer,
      compressed: false,
      mainDeck: ["Spell"],
      landCounts: {},
      poolSortMode: "color",
      poolPanelOpen: true,
      workspace: {
        schemaVersion: 1,
        placements: { card: { zone: "deck", row: 2, column: 0, order: 0 } },
        virtualBasics: [],
      },
    });

    await expect(loadQuickDraftSession("invalid-workspace")).resolves.toMatchObject({
      sessionJson: "session",
      workspace: null,
    });
  });

  it("repairs metadata and clears a matching stage on duplicate result", async () => {
    idb.records.set(`${DRAFT_RUN_KEY_PREFIX}draft`, {
      format: "single",
      results: [{ gameId: "game", result: "win" }],
      playerDeck: ["Player"],
      opponentDeck: ["Opponent"],
      usedBotSeats: [1],
      activeMatch: {
        draftId: "draft",
        gameId: "game",
        format: "single",
        resultCountAtLaunch: 0,
        botSeat: 1,
        opponentDeck: ["Opponent"],
      },
    });

    const result = await recordDraftMatchResult({
      draftId: "draft",
      gameId: "game",
      result: "win",
      makeMeta: () => ({
        id: "draft", setCode: "TST", difficulty: 2, phase: "complete",
        pickCount: 1, updatedAt: Date.now(),
      }),
    });

    expect(result?.run.activeMatch).toBeUndefined();
    expect(idb.records.get(`${DRAFT_RUN_KEY_PREFIX}draft`)).not.toHaveProperty("activeMatch");
    expect(loadActiveQuickDraft()?.phase).toBe("complete");
  });
});
