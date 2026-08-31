import { beforeEach, describe, expect, it, vi } from "vitest";

import { VisualPackBackendError } from "../backend.ts";
import { invalidateDeckLibraryPack, planDeckLibraryPack } from "../deckLibraryPack.ts";
import { packId } from "../types.ts";
import type { DeckCatalogCandidate } from "../../deckCatalog.ts";
import type { ParsedDeck } from "../../deckParser.ts";
import type { CuratedMembershipInput } from "../curatedMembership.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import type { DeckMap } from "../../../hooks/useDecks.ts";

const state = vi.hoisted(() => ({
  cards: {} as unknown,
  printings: {} as unknown,
  catalog: [] as DeckCatalogCandidate[],
  catalogError: null as Error | null,
  plannerError: null as Error | null,
  resolveError: null as Error | null,
  oracleIds: new Map<string, string>(),
  precons: {} as DeckMap | null,
  subscriptions: [] as Array<{ sourceId: string }>,
  feeds: new Map<string, unknown>(),
}));

vi.mock("../../scryfall.ts", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../scryfall.ts")>(),
  loadScryfallData: vi.fn(async () => state.cards),
  loadPrintingsData: vi.fn(async () => state.printings),
  resolveOracleIdSync: vi.fn((name: string) => {
    if (state.resolveError) throw state.resolveError;
    return state.oracleIds.get(name) ?? null;
  }),
}));

vi.mock("../../deckCatalog.ts", () => ({
  buildDeckCatalog: vi.fn(async () => {
    if (state.catalogError) throw state.catalogError;
    return state.catalog;
  }),
}));

vi.mock("../../../hooks/useDecks.ts", () => ({
  loadPreconDeckMap: vi.fn(async () => state.precons),
}));

vi.mock("../../feedService.ts", () => ({
  listSubscriptions: vi.fn(() => state.subscriptions),
  getCachedFeed: vi.fn((feedId: string) => state.feeds.get(feedId) ?? null),
}));

vi.mock("../curatedMembership.ts", () => ({
  planCuratedMembership: vi.fn(async (input: CuratedMembershipInput) => {
    if (state.plannerError) throw state.plannerError;
    return {
      descriptors: [...(input.includedOracleIds ?? [])].sort().map((oracleId) => ({ oracleId })),
      membershipDigest: "0000000000000000000000000000000000000000000000000000000000000000",
    };
  }),
}));

import { buildDeckCatalog } from "../../deckCatalog.ts";
import { getCachedFeed, listSubscriptions } from "../../feedService.ts";
import { planCuratedMembership } from "../curatedMembership.ts";
import { loadPreconDeckMap } from "../../../hooks/useDecks.ts";

const PACK = packId("complete");
const BOLT = "11111111-abcd-4111-8111-111111111111";
const GIANT = "22222222-abcd-4222-8222-822222222222";
const SOLO = "33333333-abcd-4333-8333-333333333333";
const OUTSIDE = "44444444-abcd-4444-8444-444444444444";

function deck(overrides: Partial<ParsedDeck> = {}): ParsedDeck {
  return { main: [], sideboard: [], ...overrides };
}

function candidate(id: string, source: DeckCatalogCandidate["source"], contents: ParsedDeck): DeckCatalogCandidate {
  return { id, name: id, source, deck: contents };
}

function plannedInput(): CuratedMembershipInput {
  const calls = vi.mocked(planCuratedMembership).mock.calls;
  return calls[calls.length - 1][0];
}

describe("deck-library membership planner", () => {
  beforeEach(() => {
    invalidateDeckLibraryPack();
    state.cards = {};
    state.printings = {};
    state.catalog = [];
    state.catalogError = null;
    state.plannerError = null;
    state.resolveError = null;
    state.oracleIds = new Map();
    state.precons = {};
    state.subscriptions = [];
    state.feeds = new Map();
    vi.mocked(buildDeckCatalog).mockClear();
    vi.mocked(loadPreconDeckMap).mockClear();
    vi.mocked(listSubscriptions).mockClear();
    vi.mocked(getCachedFeed).mockClear();
    vi.mocked(planCuratedMembership).mockClear();
    usePreferencesStore.setState({ artChain: [], artOverrides: {} });
  });

  it("collects every expanded deck slot from saved, feed, precon, and bundled cEDH candidates", async () => {
    state.oracleIds = new Map([
      ["Main", BOLT], ["Side", GIANT], ["Commander", SOLO], ["Planar", OUTSIDE],
      ["Scheme", BOLT], ["Sticker", GIANT], ["Signature", SOLO], ["Companion", OUTSIDE],
    ]);
    state.catalog = [
      candidate("saved", { type: "saved" }, deck({ main: [{ count: 2, name: "Main" }] })),
      candidate("feed", { type: "feed", feedId: "weekly" }, deck({ sideboard: [{ count: 1, name: "Side" }] })),
      candidate("precon", { type: "precon", deckId: "precon", code: "abc" }, deck({ commander: ["Commander"], planar_deck: ["Planar"] })),
      candidate("cedh", { type: "precon", deckId: "BundledCedh", code: "cedh" }, deck({
        scheme_deck: ["Scheme"], sticker_sheets: ["Sticker"], signature_spell: ["Signature"], companion: "Companion",
      })),
    ];

    await planDeckLibraryPack(PACK);

    expect(buildDeckCatalog).toHaveBeenCalledWith();
    expect(plannedInput().includedOracleIds).toEqual(new Set([BOLT, GIANT, SOLO, OUTSIDE]));
  });

  it("is deterministic across candidate ordering, skips unresolved names, and excludes cards outside the catalog", async () => {
    state.oracleIds = new Map([["Bolt", BOLT], ["Giant", GIANT]]);
    const first = candidate("first", { type: "saved" }, deck({ main: [{ count: 4, name: "Bolt" }, { count: 1, name: "Missing" }] }));
    const second = candidate("second", { type: "feed", feedId: "weekly" }, deck({ sideboard: [{ count: 2, name: "Giant" }, { count: 1, name: "Bolt" }] }));
    state.catalog = [first, second];
    await planDeckLibraryPack(PACK);
    const forward = plannedInput();

    invalidateDeckLibraryPack();
    state.catalog = [second, first];
    await planDeckLibraryPack(PACK);
    const reversed = plannedInput();

    expect(forward.includedOracleIds).toEqual(new Set([BOLT, GIANT]));
    expect(reversed.includedOracleIds).toEqual(forward.includedOracleIds);
    expect([...(reversed.includedOracleIds ?? [])]).not.toContain(OUTSIDE);
  });

  it("forwards structured source printings and the current art preferences", async () => {
    const artChain = [{ type: "source_printing" as const }];
    const artOverrides = { [BOLT]: { scryfallId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", setCode: "lea", collectorNumber: "161" } };
    usePreferencesStore.setState({ artChain, artOverrides });
    state.oracleIds = new Map([["Bolt", BOLT], ["Giant", GIANT]]);
    state.catalog = [candidate("saved", { type: "saved" }, deck({
      main: [{ count: 4, name: "Bolt", sourcePrinting: { setCode: "LEA", collectorNumber: "161" } }],
      sideboard: [{ count: 1, name: "Giant", sourcePrinting: { setCode: "M20", collectorNumber: "1" } }],
    }))];

    await planDeckLibraryPack(PACK);

    expect(plannedInput()).toMatchObject({
      packId: PACK,
      artChain,
      artOverrides,
      deckPrintings: [
        { oracleId: BOLT, source: { setCode: "LEA", collectorNumber: "161" } },
        { oracleId: GIANT, source: { setCode: "M20", collectorNumber: "1" } },
      ],
    });
  });

  it("treats an empty catalog as a valid empty membership", async () => {
    await planDeckLibraryPack(PACK);

    expect(plannedInput().includedOracleIds).toEqual(new Set());
    expect(plannedInput().deckPrintings).toEqual([]);
  });

  it("rejects unavailable precons before accepting a partial catalog, then retries when the map is available", async () => {
    state.precons = null;
    state.catalog = [candidate("saved", { type: "saved" }, deck({ main: [{ count: 1, name: "Bolt" }] }))];

    await expect(planDeckLibraryPack(PACK)).rejects.toMatchObject({ kind: "network", detail: null });
    expect(buildDeckCatalog).not.toHaveBeenCalled();
    expect(planCuratedMembership).not.toHaveBeenCalled();

    state.precons = {};
    await planDeckLibraryPack(PACK);
    expect(buildDeckCatalog).toHaveBeenCalledTimes(1);
  });

  it("requires every subscribed feed cache, while actual unsubscribe permits normal pruning", async () => {
    state.subscriptions = [{ sourceId: "weekly" }];
    state.catalog = [candidate("saved", { type: "saved" }, deck({ main: [{ count: 1, name: "Bolt" }] }))];

    await expect(planDeckLibraryPack(PACK)).rejects.toMatchObject({ kind: "network", detail: null });
    expect(buildDeckCatalog).not.toHaveBeenCalled();

    state.feeds.set("weekly", {});
    await planDeckLibraryPack(PACK);
    expect(buildDeckCatalog).toHaveBeenCalledTimes(1);

    invalidateDeckLibraryPack();
    state.subscriptions = [];
    state.feeds.clear();
    state.catalog = [];
    await planDeckLibraryPack(PACK);
    expect(plannedInput().includedOracleIds).toEqual(new Set());
  });

  it("shares an overlapping plan and keys it by pack, preference identities, and catalog generation", async () => {
    state.oracleIds = new Map([["Bolt", BOLT]]);
    state.catalog = [candidate("saved", { type: "saved" }, deck({ main: [{ count: 1, name: "Bolt" }] }))];

    const [first, second] = await Promise.all([planDeckLibraryPack(PACK), planDeckLibraryPack(PACK)]);
    expect(planCuratedMembership).toHaveBeenCalledTimes(1);
    expect(second).toBe(first);

    await planDeckLibraryPack(packId("core"));
    usePreferencesStore.setState({ artChain: [], artOverrides: {} });
    await planDeckLibraryPack(PACK);
    expect(planCuratedMembership).toHaveBeenCalledTimes(3);
  });

  it("keeps the newer membership cached when an invalidated in-flight plan finishes later", async () => {
    const oldCatalog = [candidate("old", { type: "saved" }, deck({ main: [{ count: 1, name: "Old" }] }))];
    const currentCatalog = [candidate("current", { type: "saved" }, deck({ main: [{ count: 1, name: "Current" }] }))];
    state.oracleIds = new Map([["Old", BOLT], ["Current", GIANT]]);

    let startOld = (): void => undefined;
    const oldStarted = new Promise<void>((resolve) => { startOld = resolve; });
    let releaseOld = (): void => undefined;
    const oldGate = new Promise<void>((resolve) => { releaseOld = resolve; });
    let startCurrent = (): void => undefined;
    const currentStarted = new Promise<void>((resolve) => { startCurrent = resolve; });
    let releaseCurrent = (): void => undefined;
    const currentGate = new Promise<void>((resolve) => { releaseCurrent = resolve; });
    vi.mocked(buildDeckCatalog)
      .mockImplementationOnce(async () => {
        startOld();
        await oldGate;
        return oldCatalog;
      })
      .mockImplementationOnce(async () => {
        startCurrent();
        await currentGate;
        return currentCatalog;
      });

    const stale = planDeckLibraryPack(PACK);
    await oldStarted;
    invalidateDeckLibraryPack();
    const current = planDeckLibraryPack(PACK);
    await currentStarted;

    releaseCurrent();
    const currentMembership = await current;
    releaseOld();
    const staleMembership = await stale;

    const plannerCalls = vi.mocked(planCuratedMembership).mock.calls;
    expect(plannerCalls).toHaveLength(2);
    expect(plannerCalls[0][0].includedOracleIds).toEqual(new Set([GIANT]));
    expect(plannerCalls[1][0].includedOracleIds).toEqual(new Set([BOLT]));
    expect(staleMembership).not.toBe(currentMembership);

    const cached = await planDeckLibraryPack(PACK);
    expect(cached).toBe(currentMembership);
    expect(planCuratedMembership).toHaveBeenCalledTimes(2);
  });

  it("classifies null datasets as network without untranslated detail", async () => {
    state.cards = null;

    await expect(planDeckLibraryPack(PACK)).rejects.toMatchObject({ kind: "network", detail: null });
    invalidateDeckLibraryPack();
    state.cards = {};
    state.printings = null;
    await expect(planDeckLibraryPack(PACK)).rejects.toMatchObject({ kind: "network", detail: null });
    expect(planCuratedMembership).not.toHaveBeenCalled();
  });

  it("classifies unexpected catalog, resolution, and planner errors as internal", async () => {
    state.catalogError = new Error("catalog failure");
    await expect(planDeckLibraryPack(PACK)).rejects.toMatchObject({ kind: "internal", detail: "catalog failure" });

    invalidateDeckLibraryPack();
    state.catalogError = null;
    state.resolveError = new Error("resolution failure");
    state.catalog = [candidate("saved", { type: "saved" }, deck({ main: [{ count: 1, name: "Bolt" }] }))];
    await expect(planDeckLibraryPack(PACK)).rejects.toMatchObject({ kind: "internal", detail: "resolution failure" });

    invalidateDeckLibraryPack();
    state.resolveError = null;
    state.plannerError = new Error("planning failure");
    await expect(planDeckLibraryPack(PACK)).rejects.toMatchObject({ kind: "internal", detail: "planning failure" });
  });

  it("preserves existing backend errors and evicts rejected cache entries", async () => {
    const backendError = new VisualPackBackendError("conflict");
    state.plannerError = backendError;
    await expect(planDeckLibraryPack(PACK)).rejects.toBe(backendError);

    state.plannerError = null;
    await planDeckLibraryPack(PACK);
    expect(planCuratedMembership).toHaveBeenCalledTimes(2);
  });
});
