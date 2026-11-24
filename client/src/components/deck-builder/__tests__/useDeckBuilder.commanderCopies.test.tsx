import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, renderHook, screen, waitFor } from "@testing-library/react";

import type { CommanderCandidatesRequest } from "../../../services/engineRuntime";
import { CommanderPanel } from "../CommanderPanel";
import type { DeckEntry } from "../../../services/deckParser";
import type { ScryfallCard } from "../../../services/scryfall";
import type { GameFormat } from "../../../adapter/types";
import { loadPreconDeckMap, type DeckMap } from "../../../hooks/useDecks";
import {
  evaluateDeckCompatibility,
  type DeckCompatibilityResult,
} from "../../../services/deckCompatibility";
import { resolveCommander } from "../../../services/commanderResolution";
import { DECKS_CHANGED_EVENT, SAVED_DECK_REWRITTEN_EVENT, STORAGE_KEY_PREFIX, loadSavedDeck } from "../../../constants/storage";

import {
  installFifoWebLocks,
  resetSavedDeckLibraryForTests,
  uninstallWebLocks,
} from "../../../test/helpers/webLocks";

const inference = vi.fn<(rows: readonly Pick<DeckEntry, "name" | "count">[]) => Promise<string | null>>();
const canonicalCardNames = vi.fn(async (names: string[]) => names);

const eligible = new Set<string>();
const partnerCandidates = vi.fn(async () => [] as string[]);
const cardDataCache = new Map<string, { name: string; cmc: number; color_identity: string[] }>();

const defaultOffers = async (request: CommanderCandidatesRequest) =>
  [...new Set(request.main_deck)].filter((name) => eligible.has(name) && !request.commander.includes(name));
const candidateQuery = vi.fn((request: CommanderCandidatesRequest, _composition: string) => defaultOffers(request));
const perName = vi.fn(async (name: string) => eligible.has(name));
const companionQuery = vi.fn(async () => [] as string[]);
const signatureQuery = vi.fn(async (): Promise<import("../../../services/engineRuntime").SignatureSpellSelectionPolicy> => ({ type: "None" }));

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

vi.mock("../../../services/engineRuntime", () => ({
  isCardCommanderEligibleForFormat: (name: string) => perName(name),
  commanderCandidates: (request: CommanderCandidatesRequest, composition: string) => candidateQuery(request, composition),
  commanderPartnerCandidates: (...args: unknown[]) =>
    (partnerCandidates as unknown as (...a: unknown[]) => Promise<string[]>)(...args),
  companionCandidates: () => companionQuery(),
  inferCommander: (rows: Parameters<typeof inference>[0]) => inference(rows),
  maxDeckCopies: vi.fn(async (name: string) => name === "Forest"
    ? { type: "Unlimited" }
    : { type: "Limited", data: 1 }),
  signatureSpellSelectionPolicy: () => signatureQuery(),
}));

vi.mock("../../../hooks/useDeckCardData", () => ({
  useDeckCardData: () => ({ cardDataCache, cacheCards: vi.fn() }),
}));

vi.mock("../../../hooks/useBracketEstimate", () => ({
  useBracketEstimate: () => ({ estimate: null, unsupported: true }),
}));

vi.mock("../../../hooks/useDecks", () => ({
  loadPreconDeckMap: vi.fn(async () => ({})),
}));

vi.mock("../../../services/deckCompatibility", () => ({
  evaluateDeckCompatibility: vi.fn(async () => null),
}));

vi.mock("../../../adapter/wasm-adapter", () => ({
  getSharedAdapter: () => ({ canonicalCardNames }),
}));

vi.mock("../../../services/commanderResolution", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../services/commanderResolution")>();
  return { ...actual, resolveCommander: vi.fn(actual.resolveCommander) };
});

import { useDeckBuilder } from "../useDeckBuilder";

/**
 * VM row 7 — CR 903.3. The designation "is not a characteristic of the object
 * ... it is an attribute of the card itself": a LABEL on one card, never a
 * removal of every copy. Designating one copy of a card the deck holds three of
 * must leave two.
 *
 * REVERT-PROBE: restore `prev.main.filter((e) => e.name !== cardName)` in
 * `handleSetCommander` and the first case finds NO `Legend` row in main at all.
 */
describe("useDeckBuilder — CR 903.3 designation accounting", () => {
  const LEGEND = "Legend";
  const OTHER = "Other Legend";

  function setup(format: GameFormat = "Commander") {
    return renderHook(() =>
      useDeckBuilder({
        format,
        onFormatChange: vi.fn(),
        searchFilters: {} as never,
      }),
    );
  }

  /** Install a main deck and wait for the panel and row answers. */
  async function withMain(
    result: { current: ReturnType<typeof useDeckBuilder> },
    main: DeckEntry[],
  ) {
    act(() => {
      result.current.handleImport({ main, sideboard: [] });
    });
    await waitFor(() => {
      // The tests click through both entrances, each with its own engine answer.
      expect(result.current.isCommanderEligible(LEGEND)).toBe(true);
      expect(result.current.commanderCandidateNames).toContain(LEGEND);
    });
  }

  function mainCount(
    result: { current: ReturnType<typeof useDeckBuilder> },
    name: string,
  ): number | undefined {
    return result.current.deck.main.find((e) => e.name === name)?.count;
  }

  beforeEach(() => {
    candidateQuery.mockReset().mockImplementation(defaultOffers);
    perName.mockReset().mockImplementation(async (name) => eligible.has(name));
    companionQuery.mockReset().mockResolvedValue([]);
    signatureQuery.mockReset().mockResolvedValue({ type: "None" });
    eligible.clear();
    eligible.add(LEGEND);
    eligible.add(OTHER);
    partnerCandidates.mockReset();
    partnerCandidates.mockResolvedValue([]);
    cardDataCache.clear();
    localStorage.clear();
  });

  afterEach(() => {
    cleanup();
  });

  it("edits only the selected printing and adds by name to the first row", async () => {
    const { result } = setup();
    const one = { count: 4, name: "Forest", sourcePrinting: { setCode: "one", collectorNumber: "276" } };
    const dmu = { count: 3, name: "Forest", sourcePrinting: { setCode: "dmu", collectorNumber: "277" } };
    const sideOne = { ...one, count: 2 };
    await act(async () => result.current.handleImport({ main: [one, dmu], sideboard: [sideOne] }));
    expect(result.current.deck.main).toEqual([one, dmu]);
    expect(result.current.cardCounts.get("Forest")).toBe(7);

    act(() => result.current.handleIncrementCard(dmu, "main"));
    expect(result.current.deck.main).toEqual([one, { ...dmu, count: 4 }]);
    act(() => result.current.handleRemoveCard(dmu, "main"));
    expect(result.current.deck.main).toEqual([one, dmu]);

    act(() => result.current.handleMoveCard(dmu, "main"));
    expect(result.current.deck.main).toEqual([one, { ...dmu, count: 2 }]);
    expect(result.current.deck.sideboard).toEqual([sideOne, { ...dmu, count: 1 }]);
    act(() => result.current.handleMoveCard(dmu, "sideboard"));
    expect(result.current.deck.main).toEqual([one, dmu]);
    expect(result.current.deck.sideboard).toEqual([sideOne]);

    act(() => result.current.handleAddCard({ name: "Forest" } as ScryfallCard));
    expect(result.current.deck.main).toEqual([{ ...one, count: 5 }, dmu]);
  });

  it("projects both printings and uses outside composition even for Commander Draft", async () => {
    const { result } = setup("CommanderDraft");
    const name = "Isamaru, Hound of Konda";
    const control = "Yargle, Glutton of Urborg";
    const first = { count: 1, name, sourcePrinting: { setCode: "chk", collectorNumber: "19" } };
    const second = { count: 1, name, sourcePrinting: { setCode: "j22", collectorNumber: "201" } };
    const other = { count: 1, name: control };
    eligible.add(name);
    candidateQuery.mockImplementation(async ({ commander }) => commander.length === 0 ? [name, control] : [control]);
    act(() => result.current.handleImport({ main: [first, second, other], sideboard: [] }));
    await waitFor(() => expect(result.current.commanderCandidateNames).toEqual([name, control]));
    expect(result.current.deck.main).toEqual([first, second, other]);
    expect(candidateQuery).toHaveBeenLastCalledWith(expect.objectContaining({
      main_deck: [name, name, control], commander: [], selected_format: "CommanderDraft",
    }), "commanders-outside");
    act(() => result.current.handleSetCommander(name));
    await waitFor(() => expect(result.current.commanderCandidateNames).toEqual([control]));
    expect(candidateQuery).toHaveBeenLastCalledWith(expect.objectContaining({
      main_deck: [name, control], commander: [name], selected_format: "CommanderDraft",
    }), "commanders-outside");

    const main = result.current.deck.main;
    expect(main).toEqual([second, other]);
    expect(result.current.isCommanderEligible(name)).toBe(true);
    await act(async () => result.current.handleSetCommander(name));
    expect(result.current.commanders).toEqual([name]);
    expect(result.current.deck.main).toEqual(main);
    expect(partnerCandidates).not.toHaveBeenCalled();
    await act(async () => result.current.handleSetCommander(second));
    expect(partnerCandidates).toHaveBeenCalled();
    expect(result.current.commanders).toEqual([name]);
    expect(result.current.deck.main).toEqual([other, { count: 1, name }]);
  });

  it("publishes only the current candidate answer across A to B to A", async () => {
    const { result } = setup();
    const lateSuccess = deferred<string[]>();
    const lateFailure = deferred<string[]>();
    const latest = deferred<string[]>();
    const importName = (name: string) =>
      act(() => result.current.handleImport({ main: [{ name, count: 1 }], sideboard: [] }));
    importName(LEGEND);
    await waitFor(() => expect(result.current.commanderCandidateNames).toEqual([LEGEND]));
    candidateQuery.mockReturnValueOnce(lateSuccess.promise)
      .mockReturnValueOnce(lateFailure.promise).mockReturnValueOnce(latest.promise);
    importName(OTHER);
    expect(result.current.commanderCandidateNames).toBeNull();
    importName(LEGEND);
    expect(result.current.commanderCandidateNames).toBeNull();
    importName(OTHER);
    expect(result.current.commanderCandidateNames).toBeNull();
    await act(async () => latest.resolve([OTHER]));
    await act(async () => lateSuccess.resolve([LEGEND]));
    await act(async () => lateFailure.reject(new Error("stale")));
    expect(result.current.commanderCandidateNames).toEqual([OTHER]);
  });

  it("sets the offered commander from the panel while per-name eligibility is pending", async () => {
    const pending = deferred<boolean>();
    perName.mockReturnValue(pending.promise);
    const name = "Isamaru, Hound of Konda";
    const offer = deferred<string[]>();
    candidateQuery.mockImplementation(({ main_deck }) => main_deck.includes(name) ? offer.promise : Promise.resolve([]));
    function Consumer() {
      const builder = useDeckBuilder({ format: "Commander", onFormatChange: vi.fn(), searchFilters: {} as never });
      return <>
        <button onClick={() => builder.handleImport({ main: [{ count: 1, name }], sideboard: [] })}>Load fixture</button>
        <CommanderPanel
          commanders={builder.commanders}
          deck={builder.deck.main}
          deckComposition="commanders-outside"
          cardDataCache={new Map()}
          deckSizeRule={builder.deckSizeRule}
          commanderCandidates={builder.commanderCandidateNames}
          onSetCommander={builder.handleSetCommander}
          onRemoveCommander={builder.handleRemoveCommander}
        />
        <output>{builder.deck.main.length}</output>
      </>;
    }
    render(<Consumer />);
    fireEvent.click(screen.getByRole("button", { name: "Load fixture" }));
    expect(perName).toHaveBeenCalledWith(name);
    await act(async () => offer.resolve([name]));
    fireEvent.click(screen.getByRole("button", { name }));
    expect(await screen.findByRole("button", { name: "Remove" })).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("0");
    await act(async () => pending.resolve(true));
  });

  it("moves the exact offered composite spelling", async () => {
    const { result } = setup();
    const composite = "Sheoldred // The True Scriptures";
    candidateQuery.mockResolvedValueOnce([composite]);
    act(() => result.current.handleImport({
      main: [{ count: 1, name: "Sheoldred" }, { count: 1, name: composite }],
      sideboard: [],
    }));
    await waitFor(() => expect(result.current.commanderCandidateNames).toEqual([composite]));
    await act(async () => result.current.handleSetCommander(composite));
    expect(result.current.commanders).toEqual([composite]);
    expect(result.current.deck.main).toEqual([{ count: 1, name: "Sheoldred" }]);
  });

  it("keeps companion and signature-spell answers when the candidate query rejects", async () => {
    const { result } = setup();
    await waitFor(() => expect(result.current.commanderCandidateNames).toEqual([]));
    const answer = deferred<string[]>();
    candidateQuery.mockReturnValueOnce(answer.promise);
    companionQuery.mockResolvedValueOnce(["Lurrus of the Dream-Den"]);
    signatureQuery.mockResolvedValueOnce({ type: "Required", data: { candidates: ["Lightning Bolt"] } });
    act(() => result.current.handleImport({ main: [{ count: 1, name: LEGEND }], sideboard: [] }));
    await act(async () => answer.reject(new Error("candidate failure")));
    expect(result.current.commanderCandidateNames).toBeNull();
    expect(result.current.signatureSpellCandidates).toEqual(["Lightning Bolt"]);
    expect(result.current.companionCandidateNames).toEqual(["Lurrus of the Dream-Den"]);
  });

  it("decrements the main-deck copy count rather than removing every copy", async () => {
    const { result } = setup();
    await withMain(result, [{ count: 3, name: LEGEND }]);

    await act(async () => {
      result.current.handleSetCommander(LEGEND);
    });

    await waitFor(() => {
      expect(result.current.commanders).toEqual([LEGEND]);
    });
    // REVERT-FAILING: the base `filter` leaves `undefined` here.
    expect(mainCount(result, LEGEND)).toBe(2);
  });

  it("decrements only the selected commander printing", async () => {
    const { result } = setup();
    const first = { count: 3, name: LEGEND, sourcePrinting: { setCode: "one", collectorNumber: "12" } };
    const second = { count: 2, name: LEGEND, sourcePrinting: { setCode: "dmu", collectorNumber: "34" } };
    await withMain(result, [first, second]);

    await act(async () => {
      result.current.handleSetCommander(result.current.deck.main[1]);
    });

    expect(result.current.commanders).toEqual([LEGEND]);
    expect(result.current.deck.main).toEqual([first, { ...second, count: 1 }]);
  });

  it("takes a copy by name when the clicked printing row moves during the partner check", async () => {
    const { result } = setup();
    const first = { count: 3, name: LEGEND, sourcePrinting: { setCode: "one", collectorNumber: "12" } };
    const second = { count: 1, name: LEGEND, sourcePrinting: { setCode: "dmu", collectorNumber: "34" } };
    await withMain(result, [first, second, { count: 1, name: OTHER }]);
    await act(async () => {
      result.current.handleSetCommander(OTHER);
    });
    await waitFor(() => {
      expect(result.current.commanders).toEqual([OTHER]);
    });

    // CR 702.124h: the pairing answer is held open while the clicked row leaves main.
    let confirmPair: (names: string[]) => void = () => {};
    partnerCandidates.mockReturnValue(new Promise<string[]>((resolve) => { confirmPair = resolve; }));
    act(() => {
      result.current.handleSetCommander(second);
    });
    act(() => {
      result.current.handleMoveCard(second, "main");
    });
    expect(result.current.commanders).toEqual([OTHER]);
    await act(async () => {
      confirmPair([LEGEND]);
    });

    await waitFor(() => {
      expect(result.current.commanders).toEqual([OTHER, LEGEND]);
    });
    expect(result.current.deck.main).toEqual([{ ...first, count: 2 }]);
    expect(result.current.deck.sideboard).toEqual([second]);
  });

  it("removes the row entirely when the last copy is designated", async () => {
    const { result } = setup();
    await withMain(result, [{ count: 1, name: LEGEND }]);

    await act(async () => {
      result.current.handleSetCommander(LEGEND);
    });

    await waitFor(() => {
      expect(result.current.commanders).toEqual([LEGEND]);
    });
    // Hostile sibling: `{count: 1}` must FILTER, not decrement to zero — a
    // naive `count - 1` everywhere leaves a `{count: 0}` ghost row.
    expect(mainCount(result, LEGEND)).toBeUndefined();
    expect(result.current.deck.main.some((e) => e.name === LEGEND)).toBe(false);
  });

  it("adding a partner does not disturb the first commander's accounting", async () => {
    const { result } = setup();
    await withMain(result, [
      { count: 3, name: LEGEND },
      { count: 2, name: OTHER },
    ]);

    await act(async () => {
      result.current.handleSetCommander(LEGEND);
    });
    await waitFor(() => {
      expect(result.current.commanders).toEqual([LEGEND]);
    });

    // CR 702.124h: the engine confirms the pair, so this is an ADD, not a swap.
    partnerCandidates.mockResolvedValue([OTHER]);
    await waitFor(() => expect(result.current.commanderCandidateNames).toContain(OTHER));
    await act(async () => {
      result.current.handleSetCommander(OTHER);
    });
    await waitFor(() => {
      expect(result.current.commanders).toEqual([LEGEND, OTHER]);
    });

    // Multi-authority: the second designation touches only its own card.
    expect(mainCount(result, LEGEND)).toBe(2);
    expect(mainCount(result, OTHER)).toBe(1);
  });

  it("designate-then-remove restores the exact prior multiset", async () => {
    const { result } = setup();
    await withMain(result, [{ count: 3, name: LEGEND }]);

    await act(async () => {
      result.current.handleSetCommander(LEGEND);
    });
    await waitFor(() => {
      expect(mainCount(result, LEGEND)).toBe(2);
    });

    act(() => {
      result.current.handleRemoveCommander(LEGEND);
    });

    // REVERT-FAILING on `handleRemoveCommander`: the base appends a bare
    // `{count: 1}` row, so main holds TWO `Legend` rows (2 and 1) rather than
    // one row of 3 — a duplicate the deduplicating inverse prevents.
    expect(result.current.deck.main.filter((e) => e.name === LEGEND)).toHaveLength(1);
    expect(mainCount(result, LEGEND)).toBe(3);
    expect(result.current.commanders).toEqual([]);
  });

  it("uses the engine distribution even when cached card data is absent or stale", async () => {
    cardDataCache.set("Azorius Pair", {
      name: "Azorius Pair",
      cmc: 2,
      color_identity: ["G"],
    });
    const distribution = [
      { color: "White" as const, count: 2, percentage: 40, display_percentage: 40 },
      { color: "Blue" as const, count: 2, percentage: 40, display_percentage: 40 },
      { color: "Red" as const, count: 1, percentage: 20, display_percentage: 20 },
    ];
    vi.mocked(evaluateDeckCompatibility).mockResolvedValue({
      standard: { compatible: true, reasons: [] },
      commander: { compatible: true, reasons: [] },
      bo3_ready: false,
      unknown_cards: [],
      selected_format_reasons: [],
      color_identity: ["W", "U", "R"],
      color_distribution: distribution,
    } satisfies DeckCompatibilityResult);
    const { result } = setup();

    act(() => {
      result.current.handleImport({
        main: [
          { name: "Azorius Pair", count: 2 },
          { name: "Red Card", count: 1 },
          { name: "Wastes", count: 1 },
          { name: "Uncached Card", count: 1 },
        ],
        sideboard: [],
      });
    });

    await waitFor(() => {
      expect(result.current.colorDistribution).toEqual(distribution);
    });
  });
});

describe("useDeckBuilder — Load resolver rejection", () => {
  afterEach(() => {
    cleanup();
  });

  it("keeps the loaded deck and only the editor rewrite subscription when the resolver rejects", async () => {
    localStorage.clear();
    localStorage.setItem(`${STORAGE_KEY_PREFIX}D`, JSON.stringify({ main: [{ name: "Forest", count: 60 }], sideboard: [] }));
    vi.mocked(resolveCommander).mockRejectedValueOnce(new Error("boom"));

    const addSpy = vi.spyOn(window, "addEventListener");
    const removeSpy = vi.spyOn(window, "removeEventListener");

    const { result, unmount } = renderHook(() =>
      useDeckBuilder({
        format: "Commander" as GameFormat,
        onFormatChange: vi.fn(),
        searchFilters: {} as never,
      }),
    );

    await act(async () => {
      expect(await result.current.handleLoad("D")).toBe(true);
    });
    expect(result.current.deckName).toBe("D");
    expect(result.current.deck.main).toEqual([{ name: "Forest", count: 60 }]);
    expect(result.current.commanders).toEqual([]);
    expect(result.current.dirty).toBe(false);
    unmount();

    const rewriteAdds = addSpy.mock.calls.filter(([type]) => type === SAVED_DECK_REWRITTEN_EVENT).length;
    const rewriteRemoves = removeSpy.mock.calls.filter(
      ([type]) => type === SAVED_DECK_REWRITTEN_EVENT,
    ).length;

    addSpy.mockRestore();
    removeSpy.mockRestore();

    expect(rewriteAdds).toBe(1);
    expect(rewriteRemoves).toBe(rewriteAdds);
  });
});

describe("useDeckBuilder — counted Commander inference", () => {
  const plains = [{ name: "Plains", count: 60 }];
  const unmarked = { main: plains, sideboard: [] };

  beforeEach(async () => {
    await resetSavedDeckLibraryForTests();
    installFifoWebLocks();
    inference.mockReset().mockResolvedValue(null);
    canonicalCardNames.mockReset().mockImplementation(async (names) => names);
    vi.mocked(loadPreconDeckMap).mockReset().mockResolvedValue({});
  });

  afterEach(() => {
    cleanup();
    uninstallWebLocks();
  });

  function openBuilder() {
    return renderHook(() => useDeckBuilder({
      format: "Commander",
      onFormatChange: vi.fn(),
      searchFilters: {} as never,
    }));
  }

  it("loads an unmarked 60-card deck when inference rejects", async () => {
    const name = "Loaded Plains";
    localStorage.setItem(STORAGE_KEY_PREFIX + name, JSON.stringify(unmarked));
    const { result } = openBuilder();
    await act(async () => result.current.handleImport({ main: [{ name: "Forest", count: 2 }], sideboard: [] }));
    inference.mockRejectedValueOnce(new Error("engine initialization failed"));

    await act(async () => expect(await result.current.handleLoad(name)).toBe(true));
    expect(inference.mock.calls).toEqual([[plains]]);
    expect(result.current.deckName).toBe(name);
    expect(result.current.deck.main).toEqual(plains);
    expect(result.current.commanders).toEqual([]);
  });

  it("saves an unmarked 60-card deck when inference rejects", async () => {
    const name = "Saved Plains";
    const { result } = openBuilder();
    await act(async () => {
      result.current.handleImport(unmarked);
      result.current.handleDeckNameChange(name);
    });
    inference.mockRejectedValueOnce(new Error("engine initialization failed"));

    await act(async () => expect(await result.current.handleSave()).toBe("saved"));
    expect(inference.mock.calls).toEqual([[plains]]);
    const saved = loadSavedDeck(name);
    expect(saved?.main).toEqual(plains);
    expect(saved?.commander).toBeUndefined();
  });

  const commander = "Isamaru, Hound of Konda";
  const main = [{ name: commander, count: 1 }, { name: "Plains", count: 99 }];
  const commanderDeck = { main, sideboard: [] };

  function openStandardBuilder() {
    return renderHook(() => {
      const [format, onFormatChange] = useState<GameFormat>("Standard");
      const builder = useDeckBuilder({ format, onFormatChange, searchFilters: {} as never });
      return { ...builder, format };
    });
  }

  it.each(["Save", "Clone"] as const)("keeps the promoted editor separate from an earlier %s snapshot", async (write) => {
    const name = "Unmarked Commander";
    const clone = `${name} copy`;
    localStorage.setItem(STORAGE_KEY_PREFIX + name, JSON.stringify(commanderDeck));
    const { result } = openStandardBuilder();
    const answer = deferred<string | null>();
    const canonical = deferred<string[]>();
    inference.mockReturnValueOnce(answer.promise);
    canonicalCardNames.mockReturnValueOnce(canonical.promise);

    await act(async () => expect(await result.current.handleLoad(name)).toBe(true));
    const pending = write === "Save" ? result.current.handleSave() : result.current.handleClone();
    expect(canonicalCardNames).toHaveBeenCalledWith([commander, "Plains"]);
    expect(inference.mock.calls).toEqual([[main]]);

    await act(async () => answer.resolve(commander));
    expect(result.current.format).toBe("Commander");
    expect(result.current.commanders).toEqual([commander]);
    expect(result.current.deck.main).toEqual(main.slice(1));
    expect(result.current.dirty).toBe(false);

    await act(async () => {
      canonical.resolve([commander, "Plains"]);
      expect(await pending).toBe(write === "Save" ? "saved-then-changed" : undefined);
    });
    const writtenName = write === "Save" ? name : clone;
    const stored = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + writtenName)!);
    expect(stored.format).toBe("Standard");
    expect(stored.commander).toBeUndefined();
    expect(stored.main).toEqual(main);
    expect(result.current.deckName).toBe(name);
    expect(result.current.format).toBe("Commander");
    expect(result.current.commanders).toEqual([commander]);
    expect(result.current.deck.main).toEqual(main.slice(1));
    expect(result.current.justSaved).toBe(false);
    expect(result.current.dirty).toBe(write === "Save");

    // Saving the promoted editor writes back to its source and leaves any copy intact.
    await act(async () => expect(await result.current.handleSave()).toBe("saved"));
    expect(JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + name)!)).toMatchObject({
      format: result.current.format,
      commander: result.current.commanders,
      main: result.current.deck.main,
    });
    expect(result.current.dirty).toBe(false);
    if (write === "Clone") {
      expect(JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + clone)!)).toEqual(stored);
    }
  });

  it("keeps the editor aligned with a Save that completes before Load inference", async () => {
    const name = "Unmarked Commander";
    localStorage.setItem(STORAGE_KEY_PREFIX + name, JSON.stringify(commanderDeck));
    const { result } = openStandardBuilder();
    const answer = deferred<string | null>();
    inference.mockReturnValueOnce(answer.promise);

    await act(async () => expect(await result.current.handleLoad(name)).toBe(true));
    expect(inference.mock.calls).toEqual([[main]]);
    await act(async () => expect(await result.current.handleSave()).toBe("saved"));
    await act(async () => answer.resolve(commander));

    const stored = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + name)!);
    expect(result.current.format).toBe(stored.format);
    expect(result.current.commanders).toEqual(stored.commander ?? []);
    expect(result.current.deck.main).toEqual(stored.main);
    expect(result.current.dirty).toBe(false);
  });

  it("keeps a pending Clone from adopting a snapshot superseded by Save inference", async () => {
    const name = "Unmarked Commander";
    const clone = `${name} copy`;
    localStorage.setItem(STORAGE_KEY_PREFIX + name, JSON.stringify(commanderDeck));
    const { result } = openBuilder();
    const loadAnswer = deferred<string | null>();
    const canonical = deferred<string[]>();
    inference.mockReturnValueOnce(loadAnswer.promise).mockResolvedValueOnce(commander);
    canonicalCardNames.mockReturnValueOnce(canonical.promise);

    await act(async () => expect(await result.current.handleLoad(name)).toBe(true));
    const pending = result.current.handleClone();
    expect(canonicalCardNames).toHaveBeenCalledWith([commander, "Plains"]);
    await act(async () => expect(await result.current.handleSave()).toBe("saved"));
    expect(inference.mock.calls).toEqual([[main], [main]]);
    expect(result.current.commanders).toEqual([commander]);

    await act(async () => {
      canonical.resolve([commander, "Plains"]);
      await pending;
      loadAnswer.resolve(commander);
    });
    expect(result.current.deckName).toBe(name);
    expect(loadSavedDeck(clone)?.main).toEqual(main);
    expect(loadSavedDeck(clone)?.commander).toBeUndefined();
    expect(loadSavedDeck(name)?.main).toEqual(result.current.deck.main);
    expect(loadSavedDeck(name)?.commander).toEqual(result.current.commanders);
    expect(result.current.dirty).toBe(false);
  });

  it("opens the copy when Load inference settles as the Clone transaction writes it", async () => {
    const name = "Unmarked Commander";
    const clone = `${name} copy`;
    localStorage.setItem(STORAGE_KEY_PREFIX + name, JSON.stringify(commanderDeck));
    const { result } = openStandardBuilder();
    const answer = deferred<Awaited<ReturnType<typeof resolveCommander>>>();
    vi.mocked(resolveCommander).mockReturnValueOnce(answer.promise);

    await act(async () => expect(await result.current.handleLoad(name)).toBe(true));
    const respond = vi.fn(() => {
      expect(loadSavedDeck(clone)?.main).toEqual(main);
      answer.resolve({ ...commanderDeck, main: main.slice(1), commander: [commander] });
    });
    window.addEventListener(DECKS_CHANGED_EVENT, respond, { once: true });
    try {
      await act(async () => result.current.handleClone());
      expect(respond).toHaveBeenCalledOnce();
    } finally {
      window.removeEventListener(DECKS_CHANGED_EVENT, respond);
    }
    const stored = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + clone)!);
    expect(result.current.deckName).toBe(clone);
    expect(result.current.format).toBe(stored.format);
    expect(result.current.commanders).toEqual(stored.commander ?? []);
    expect(result.current.deck.main).toEqual(stored.main);
    expect(result.current.dirty).toBe(false);
  });

  it("projects a pending import using the format after Load inference", async () => {
    const name = "Unmarked Commander";
    localStorage.setItem(STORAGE_KEY_PREFIX + name, JSON.stringify(commanderDeck));
    const { result } = openStandardBuilder();
    const answer = deferred<Awaited<ReturnType<typeof resolveCommander>>>();
    vi.mocked(resolveCommander).mockReturnValueOnce(answer.promise);

    await act(async () => expect(await result.current.handleLoad(name)).toBe(true));
    const finishImport = result.current.handleImport;
    const imported = {
      main: main.slice(1),
      sideboard: [],
      commander: ["Elas il-Kor, Sadistic Pilgrim"],
      companion: "Lurrus of the Dream-Den",
    };
    await act(async () => {
      const pending = answer.promise.then(() => finishImport(imported));
      answer.resolve({ ...commanderDeck, main: main.slice(1), commander: [commander] });
      await pending;
    });
    expect(result.current.format).toBe("Commander");
    expect(result.current.currentDeck).toEqual(expect.objectContaining(imported));
    expect(result.current.dirty).toBe(true);
  });

  it("keeps a pending precon Load eligible after an earlier saved Load promotes", async () => {
    const name = "Unmarked Commander";
    localStorage.setItem(STORAGE_KEY_PREFIX + name, JSON.stringify(commanderDeck));
    const { result } = openStandardBuilder();
    const answer = deferred<string | null>();
    const catalog = deferred<DeckMap>();
    inference.mockReturnValueOnce(answer.promise);
    vi.mocked(loadPreconDeckMap).mockReturnValueOnce(catalog.promise);

    await act(async () => expect(await result.current.handleLoad(name)).toBe(true));
    const preconLoad = result.current.handleLoad("[Pre-built] Precon Plains (TST)");
    expect(loadPreconDeckMap).toHaveBeenCalledTimes(1);
    await act(async () => answer.resolve(commander));
    expect(result.current.commanders).toEqual([commander]);

    await act(async () => {
      catalog.resolve({
        plains: { name: "Precon Plains", code: "TST", type: "Commander Deck", coveragePct: 100, mainBoard: plains },
      });
      expect(await preconLoad).toBe(true);
    });
    expect(result.current.deckName).toBe("Precon Plains (TST)");
    expect(result.current.deck.main).toEqual(plains);
    expect(result.current.commanders).toEqual([]);
  });

  it("opens the latest saved Load and ignores an older positive inference answer", async () => {
    const olderName = "Unmarked Commander";
    const newerName = "Loaded Plains";
    localStorage.setItem(STORAGE_KEY_PREFIX + olderName, JSON.stringify(commanderDeck));
    localStorage.setItem(STORAGE_KEY_PREFIX + newerName, JSON.stringify(unmarked));
    const { result } = openBuilder();
    const olderAnswer = deferred<string | null>();
    const newerAnswer = deferred<string | null>();
    inference.mockReturnValueOnce(olderAnswer.promise).mockReturnValueOnce(newerAnswer.promise);

    await act(async () => expect(await result.current.handleLoad(olderName)).toBe(true));
    expect(result.current.deck.main).toEqual(main);
    await act(async () => expect(await result.current.handleLoad(newerName)).toBe(true));
    expect(inference.mock.calls).toEqual([[main], [plains]]);
    expect(result.current.deckName).toBe(newerName);
    expect(result.current.deck.main).toEqual(plains);

    await act(async () => olderAnswer.resolve(commander));
    expect(result.current.deck.main).toEqual(plains);
    expect(result.current.commanders).toEqual([]);
    await act(async () => newerAnswer.resolve(null));
    expect(result.current.deckName).toBe(newerName);
    expect(result.current.deck.main).toEqual(plains);
    expect(result.current.commanders).toEqual([]);
    expect(result.current.dirty).toBe(false);
  });

  it.each(["Deleted Deck", "[Pre-built] My Test Deck"])(
    "keeps the open deck and its pending inference when selecting missing target %s",
    async (missing) => {
      const name = "Unmarked Commander";
      localStorage.setItem(STORAGE_KEY_PREFIX + name, JSON.stringify(commanderDeck));
      const { result } = openBuilder();
      const answer = deferred<string | null>();
      inference.mockReturnValueOnce(answer.promise);

      await act(async () => expect(await result.current.handleLoad(name)).toBe(true));
      expect(await result.current.handleLoad(missing)).toBe(false);
      expect(loadPreconDeckMap).toHaveBeenCalledTimes(missing === "Deleted Deck" ? 0 : 1);
      expect(inference.mock.calls).toEqual([[main]]);
      expect(result.current.deckName).toBe(name);
      expect(result.current.deck.main).toEqual(main);

      await act(async () => answer.resolve(commander));
      expect(result.current.deckName).toBe(name);
      expect(result.current.deck.main).toEqual(main.slice(1));
      expect(result.current.commanders).toEqual([commander]);
    },
  );

  it("keeps the newer saved Load when an older precon is found late", async () => {
    const name = "Loaded Plains";
    localStorage.setItem(STORAGE_KEY_PREFIX + name, JSON.stringify(unmarked));
    const { result } = openBuilder();
    const catalog = deferred<DeckMap>();
    const forest = [{ name: "Forest", count: 60 }];
    vi.mocked(loadPreconDeckMap).mockReturnValueOnce(catalog.promise);

    const preconLoad = result.current.handleLoad("[Pre-built] Precon Forest (TST)");
    expect(loadPreconDeckMap).toHaveBeenCalledTimes(1);
    await act(async () => expect(await result.current.handleLoad(name)).toBe(true));
    expect(result.current.deckName).toBe(name);

    await act(async () => {
      catalog.resolve({
        forest: { name: "Precon Forest", code: "TST", type: "Commander Deck", coveragePct: 100, mainBoard: forest },
      });
      expect(await preconLoad).toBe(false);
    });
    expect(inference.mock.calls).toEqual([[plains]]);
    expect(result.current.deck.main).toEqual(plains);
    expect(result.current.deckName).toBe(name);
  });

  it("opens a newer explicit Commander precon in Commander after a Standard Load", async () => {
    const name = "Loaded Plains";
    const preconName = "Precon Forest (TST)";
    const forest = [{ name: "Forest", count: 99 }];
    const commander = "Isamaru, Hound of Konda";
    localStorage.setItem(STORAGE_KEY_PREFIX + name, JSON.stringify({ ...unmarked, format: "Standard" }));
    const { result } = renderHook(() => {
      const [format, onFormatChange] = useState<GameFormat>("Commander");
      const builder = useDeckBuilder({ format, onFormatChange, searchFilters: {} as never });
      return { ...builder, format };
    });
    const plainsAnswer = deferred<string | null>();
    const catalog = deferred<DeckMap>();
    inference.mockReturnValueOnce(plainsAnswer.promise);
    vi.mocked(loadPreconDeckMap).mockReturnValueOnce(catalog.promise);

    let preconLoad!: Promise<boolean>;
    await act(async () => {
      const plainsLoad = result.current.handleLoad(name);
      preconLoad = result.current.handleLoad(`[Pre-built] ${preconName}`);
      expect(inference.mock.calls).toEqual([[plains]]);
      expect(loadPreconDeckMap).toHaveBeenCalledTimes(1);
      plainsAnswer.resolve(null);
      expect(await plainsLoad).toBe(true);
    });
    expect(result.current.deckName).toBe(name);
    expect(result.current.deck.main).toEqual(plains);
    expect(result.current.format).toBe("Standard");

    await act(async () => {
      catalog.resolve({
        forest: {
          name: "Precon Forest", code: "TST", type: "Commander Deck", coveragePct: 100,
          mainBoard: forest, commander: [{ name: commander, count: 1 }],
        },
      });
      expect(await preconLoad).toBe(true);
    });
    expect(result.current.deckName).toBe(preconName);
    expect(result.current.deck.main).toEqual(forest);
    expect(result.current.commanders).toEqual([commander]);
    expect(result.current.format).toBe("Commander");
  });

  it("ignores a positive inference answer after Clone opens a copy", async () => {
    const name = "Unmarked Commander";
    const clone = `${name} copy`;
    localStorage.setItem(STORAGE_KEY_PREFIX + name, JSON.stringify(commanderDeck));
    const { result } = openBuilder();
    const answer = deferred<string | null>();
    inference.mockReturnValueOnce(answer.promise);

    await act(async () => expect(await result.current.handleLoad(name)).toBe(true));
    expect(inference.mock.calls).toEqual([[main]]);
    await act(async () => result.current.handleClone());
    expect(result.current.deckName).toBe(clone);
    expect(loadSavedDeck(clone)?.main).toEqual(main);

    await act(async () => answer.resolve(commander));
    expect(result.current.deckName).toBe(clone);
    expect(result.current.deck.main).toEqual(main);
    expect(result.current.commanders).toEqual([]);
  });

  it("ignores a positive inference answer after an edit", async () => {
    const name = "Unmarked Commander";
    localStorage.setItem(STORAGE_KEY_PREFIX + name, JSON.stringify(commanderDeck));
    const { result } = openBuilder();
    const answer = deferred<string | null>();
    inference.mockReturnValueOnce(answer.promise);

    await act(async () => expect(await result.current.handleLoad(name)).toBe(true));
    expect(inference.mock.calls).toEqual([[main]]);
    expect(result.current.deck.main).toEqual(main);
    const edit = { main: [{ name: "Forest", count: 2 }], sideboard: [] };
    await act(async () => result.current.handleImport(edit));
    await act(async () => answer.resolve(commander));
    expect(result.current.deckName).toBe(name);
    expect(result.current.deck.main).toEqual(edit.main);
    expect(result.current.commanders).toEqual([]);
    expect(result.current.dirty).toBe(true);
  });

  it("opens an unmarked 100-card deck immediately and later applies its inferred commander", async () => {
    const name = "Unmarked Commander";
    localStorage.setItem(STORAGE_KEY_PREFIX + name, JSON.stringify(commanderDeck));
    const { result } = renderHook(() => {
      const [format, onFormatChange] = useState<GameFormat>("Standard");
      const builder = useDeckBuilder({ format, onFormatChange, searchFilters: {} as never });
      return { ...builder, format };
    });
    const answer = deferred<string | null>();
    inference.mockReturnValueOnce(answer.promise);

    await act(async () => expect(await result.current.handleLoad(name)).toBe(true));
    expect(inference.mock.calls).toEqual([[main]]);
    expect(result.current.deckName).toBe(name);
    expect(result.current.deck.main).toEqual(main);
    expect(result.current.commanders).toEqual([]);
    expect(result.current.format).toBe("Standard");
    expect(result.current.dirty).toBe(false);

    await act(async () => answer.resolve(commander));
    expect(result.current.deck.main).toEqual(main.slice(1));
    expect(result.current.commanders).toEqual([commander]);
    expect(result.current.format).toBe("Commander");
    expect(result.current.dirty).toBe(false);
  });
});
