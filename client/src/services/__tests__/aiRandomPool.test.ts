import { afterEach, describe, expect, it, vi } from "vitest";

import type { CommanderBracket } from "../../types/bracket";
import { AI_DECK_RANDOM } from "../../stores/preferencesStore";
import type { AiDeckCandidate } from "../aiDeckCatalog";
import {
  candidatePassesFilters,
  cedhSeatProblem,
  pickAiSeatDecks,
  resolveBracketDrawBase,
  resolveEffectiveAiRandomPool,
  resolveRandomDrawPool,
  restrictAiPoolByBracket,
  type BracketTaggedCandidate,
  type CedhSeatProblem,
  type EffectiveAiRandomPoolOptions,
  type PickAiSeatDecksOptions,
  type RandomDrawPoolOptions,
} from "../aiRandomPool";
import {
  emptyPoolSelection,
  feedPoolGroup,
  folderPoolGroup,
  PRECON_POOL_GROUP,
} from "../randomPoolSelection";
import { DECK_FOLDERS_KEY, DECK_METADATA_KEY } from "../../constants/storage";

interface FakeCandidate extends BracketTaggedCandidate {
  id: string;
}

function candidate(id: string, bracket: CommanderBracket | null): FakeCandidate {
  return { id, bracket };
}

function ids(pool: readonly FakeCandidate[]): string[] {
  return pool.map((deck) => deck.id);
}

const MIXED_POOL: FakeCandidate[] = [
  candidate("b1", 1),
  candidate("b2", 2),
  candidate("b3", 3),
  candidate("user-b4", 4),
  candidate("cedh", 5),
  candidate("untagged", null),
];

describe("restrictAiPoolByBracket", () => {
  it("returns the pool unchanged when the filter is off", () => {
    expect(
      ids(restrictAiPoolByBracket(MIXED_POOL, {
        bracketFilter: [],
        cedhMode: false,
        selectedFormat: "Commander",
      })),
    ).toEqual(["b1", "b2", "b3", "user-b4", "cedh", "untagged"]);
  });

  it("keeps only selected tiers and excludes untagged decks", () => {
    // The reported scenario: brackets 1–3 selected, the user's bracket-4
    // deck must not be in the draw.
    expect(
      ids(restrictAiPoolByBracket(MIXED_POOL, {
        bracketFilter: [1, 2, 3],
        cedhMode: false,
        selectedFormat: "Commander",
      })),
    ).toEqual(["b1", "b2", "b3"]);
  });

  it("cEDH mode restricts to bracket 5 even over a manual selection", () => {
    expect(
      ids(restrictAiPoolByBracket(MIXED_POOL, {
        bracketFilter: [1, 2, 3],
        cedhMode: true,
        selectedFormat: "Commander",
      })),
    ).toEqual(["cedh"]);
  });

  it("applies no constraint outside the Commander family", () => {
    const all = ["b1", "b2", "b3", "user-b4", "cedh", "untagged"];
    expect(
      ids(restrictAiPoolByBracket(MIXED_POOL, {
        bracketFilter: [2],
        cedhMode: false,
        selectedFormat: "Standard",
      })),
    ).toEqual(all);
    expect(
      ids(restrictAiPoolByBracket(MIXED_POOL, {
        bracketFilter: [],
        cedhMode: true,
        selectedFormat: "Standard",
      })),
    ).toEqual(all);
    expect(
      ids(restrictAiPoolByBracket(MIXED_POOL, {
        bracketFilter: [2],
        cedhMode: false,
        selectedFormat: null,
      })),
    ).toEqual(all);
    expect(
      ids(restrictAiPoolByBracket(MIXED_POOL, {
        bracketFilter: [2],
        cedhMode: false,
      })),
    ).toEqual(all);
  });
});

function deck(id: string, overrides: Partial<AiDeckCandidate> = {}): AiDeckCandidate {
  return {
    id,
    name: id,
    source: { type: "precon", deckId: id, code: "TST" },
    deck: { main: [], sideboard: [] },
    coveragePct: 100,
    archetype: null,
    bracket: null,
    ...overrides,
  };
}

function pick(
  catalog: AiDeckCandidate[],
  requestedDeckIds: string[],
  overrides: Partial<PickAiSeatDecksOptions> = {},
): string[] {
  return pickAiSeatDecks({
    catalog,
    requestedDeckIds,
    archetypeFilter: "Any",
    coverageFloor: 50,
    bracketFilter: [],
    cedhMode: false,
    selectedFormat: "Standard",
    ...overrides,
  }).map((candidate) => candidate.id);
}

describe("candidatePassesFilters", () => {
  it("excludes decks below the coverage floor but keeps unknown coverage", () => {
    expect(candidatePassesFilters(deck("low", { coveragePct: 60 }), "Any", 70)).toBe(false);
    expect(candidatePassesFilters(deck("ok", { coveragePct: 70 }), "Any", 70)).toBe(true);
    expect(candidatePassesFilters(deck("unknown", { coveragePct: null }), "Any", 100)).toBe(true);
  });

  it("excludes a deck of another archetype but keeps untagged decks", () => {
    expect(candidatePassesFilters(deck("a", { archetype: "Aggro" }), "Control", 50)).toBe(false);
    expect(candidatePassesFilters(deck("c", { archetype: "Control" }), "Control", 50)).toBe(true);
    expect(candidatePassesFilters(deck("n", { archetype: null }), "Control", 50)).toBe(true);
  });
});

describe("pickAiSeatDecks", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("gives a pinned seat its deck even when the filters exclude it", () => {
    const catalog = [
      deck("pinned", { coveragePct: 10, archetype: "Aggro" }),
      deck("other", { coveragePct: 100, archetype: "Control" }),
    ];
    expect(
      pick(catalog, ["pinned"], { archetypeFilter: "Control", coverageFloor: 90 }),
    ).toEqual(["pinned"]);
  });

  it("returns one deck per requested seat, in seat order", () => {
    const catalog = [deck("a"), deck("b"), deck("c")];
    expect(pick(catalog, ["c", "a", "b"])).toEqual(["c", "a", "b"]);
  });

  it("does not repeat a deck across Random seats or an excluded id when alternatives exist", () => {
    const catalog = [deck("a"), deck("b"), deck("c")];
    for (const roll of [0, 0.34, 0.67, 0.99]) {
      vi.spyOn(Math, "random").mockReturnValue(roll);
      const picks = pick(catalog, [AI_DECK_RANDOM, AI_DECK_RANDOM], { excludeIds: ["a"] });
      expect(picks).not.toContain("a");
      expect(new Set(picks).size).toBe(2);
    }
  });

  it("keeps a Random seat off the deck a pinned seat already took", () => {
    const catalog = [deck("a"), deck("b")];
    vi.spyOn(Math, "random").mockReturnValue(0);
    expect(pick(catalog, ["a", AI_DECK_RANDOM])).toEqual(["a", "b"]);
  });

  it("draws Random seats from the archetype and coverage filtered pool", () => {
    const catalog = [
      deck("fit", { archetype: "Control", coveragePct: 95 }),
      deck("wrong-archetype", { archetype: "Aggro", coveragePct: 95 }),
      deck("low-coverage", { archetype: "Control", coveragePct: 55 }),
    ];
    for (const roll of [0, 0.5, 0.99]) {
      vi.spyOn(Math, "random").mockReturnValue(roll);
      expect(
        pick(catalog, [AI_DECK_RANDOM], { archetypeFilter: "Control", coverageFloor: 90 }),
      ).toEqual(["fit"]);
    }
  });

  it("falls back to the whole pool when the filters empty it", () => {
    const catalog = [deck("only", { archetype: "Aggro", coveragePct: 55 })];
    expect(
      pick(catalog, [AI_DECK_RANDOM], { archetypeFilter: "Control", coverageFloor: 90 }),
    ).toEqual(["only"]);
  });

  it("restricts Random seats to the bracket pool in Commander and falls back when it is empty", () => {
    const catalog = [deck("b2", { bracket: 2 }), deck("b4", { bracket: 4 })];
    for (const roll of [0, 0.99]) {
      vi.spyOn(Math, "random").mockReturnValue(roll);
      expect(
        pick(catalog, [AI_DECK_RANDOM], { bracketFilter: [2], selectedFormat: "Commander" }),
      ).toEqual(["b2"]);
    }
    // No deck matches bracket 5: the full legal catalog is the pool.
    vi.spyOn(Math, "random").mockReturnValue(0);
    expect(
      pick(catalog, [AI_DECK_RANDOM], { bracketFilter: [5], selectedFormat: "Commander" }),
    ).toEqual(["b2"]);
  });

  it("restricts Random seats to bracket 5 in cEDH mode but not a pinned seat", () => {
    const catalog = [deck("b2", { bracket: 2 }), deck("cedh", { bracket: 5 })];
    vi.spyOn(Math, "random").mockReturnValue(0);
    expect(
      pick(catalog, [AI_DECK_RANDOM, "b2"], { cedhMode: true, selectedFormat: "Commander" }),
    ).toEqual(["cedh", "b2"]);
  });

  it("treats an unknown pinned id as Random", () => {
    const catalog = [deck("a", { coveragePct: 95 }), deck("b", { coveragePct: 55 })];
    expect(pick(catalog, ["gone"], { coverageFloor: 90 })).toEqual(["a"]);
  });

  it("never draws a Random seat from an excluded deck id", () => {
    const catalog = [deck("a"), deck("b")];
    for (const roll of [0, 0.5, 0.99]) {
      vi.spyOn(Math, "random").mockReturnValue(roll);
      expect(
        pick(catalog, [AI_DECK_RANDOM], {
          poolSelection: { ...emptyPoolSelection(), excludedDecks: ["b"] },
        }),
      ).toEqual(["a"]);
    }
  });

  it("excludes a whole feed group from Random seats", () => {
    const catalog = [
      deck("keep", { source: { type: "feed", feedId: "alpha" } }),
      deck("drop", { source: { type: "feed", feedId: "beta" } }),
    ];
    vi.spyOn(Math, "random").mockReturnValue(0.99);
    expect(
      pick(catalog, [AI_DECK_RANDOM], {
        poolSelection: { ...emptyPoolSelection(), excludedGroups: [feedPoolGroup("beta")] },
      }),
    ).toEqual(["keep"]);
  });

  it("falls back to the bracket pool when the selection matches nothing", () => {
    const catalog = [deck("a"), deck("b")];
    vi.spyOn(Math, "random").mockReturnValue(0);
    expect(
      pick(catalog, [AI_DECK_RANDOM], {
        poolSelection: { ...emptyPoolSelection(), excludedGroups: [PRECON_POOL_GROUP] },
      }),
    ).toEqual(["a"]);
  });

  it("still serves a pinned seat its deck when the selection excludes it", () => {
    const catalog = [deck("a"), deck("b")];
    expect(
      pick(catalog, ["b"], {
        poolSelection: { ...emptyPoolSelection(), excludedDecks: ["b"] },
      }),
    ).toEqual(["b"]);
  });

  it("resolves a saved deck's folder through the stored library", () => {
    localStorage.setItem(
      DECK_FOLDERS_KEY,
      JSON.stringify([{ id: "f1", name: "Aggro", order: 0 }]),
    );
    localStorage.setItem(
      DECK_METADATA_KEY,
      JSON.stringify({ Aggro: { addedAt: 1, folderId: "f1" } }),
    );
    const catalog = [
      deck("saved:Aggro", { name: "Aggro", source: { type: "saved" } }),
      deck("saved:Loose", { name: "Loose", source: { type: "saved" } }),
    ];
    try {
      // Roll 0, not 0.99: with the folder exclusion applied the pool holds
      // only Loose (index 0 either way), but a bypassed curation would serve
      // pool [Aggro, Loose] where roll 0 picks the excluded Aggro. A 0.99
      // roll selects the last entry on both paths and cannot discriminate.
      vi.spyOn(Math, "random").mockReturnValue(0);
      expect(
        pick(catalog, [AI_DECK_RANDOM], {
          poolSelection: { ...emptyPoolSelection(), excludedGroups: [folderPoolGroup("f1")] },
        }),
      ).toEqual(["saved:Loose"]);
    } finally {
      localStorage.clear();
    }
  });
});

describe("resolveBracketDrawBase", () => {
  it("returns the bracket pool when non-empty", () => {
    const catalog = [deck("a", { bracket: 2 }), deck("b", { bracket: 5 })];
    expect(
      resolveBracketDrawBase(catalog, {
        bracketFilter: [2],
        cedhMode: false,
        selectedFormat: "Commander",
      }).map((d) => d.id),
    ).toEqual(["a"]);
  });

  it("falls back to the catalog when the bracket pool is empty", () => {
    const catalog = [deck("a", { bracket: 2 })];
    expect(
      resolveBracketDrawBase(catalog, {
        bracketFilter: [5],
        cedhMode: false,
        selectedFormat: "Commander",
      }).map((d) => d.id),
    ).toEqual(["a"]);
  });

  it("restricts to bracket 5 in cEDH mode", () => {
    const catalog = [deck("a", { bracket: 2 }), deck("b", { bracket: 5 })];
    expect(
      resolveBracketDrawBase(catalog, {
        bracketFilter: [],
        cedhMode: true,
        selectedFormat: "Commander",
      }).map((d) => d.id),
    ).toEqual(["b"]);
  });
});

describe("resolveRandomDrawPool", () => {
  const catalog = [deck("a", { bracket: 2 }), deck("b", { bracket: 2 }), deck("c", { bracket: 5 })];
  const options: RandomDrawPoolOptions = {
    bracketFilter: [2],
    cedhMode: false,
    selectedFormat: "Commander",
  };

  it("layers curation after the bracket restriction", () => {
    const { pool, fellBack } = resolveRandomDrawPool(catalog, {
      ...options,
      poolSelection: { ...emptyPoolSelection(), excludedDecks: ["a"] },
    });
    expect(pool.map((d) => d.id)).toEqual(["b"]);
    expect(fellBack).toBe(false);
  });

  it("falls back to the bracket pool when the selection matches nothing", () => {
    const { pool, fellBack } = resolveRandomDrawPool(catalog, {
      ...options,
      poolSelection: { ...emptyPoolSelection(), excludedDecks: ["a", "b"] },
    });
    expect(pool.map((d) => d.id)).toEqual(["a", "b"]);
    expect(fellBack).toBe(true);
  });

  it("returns the bracket base untouched without a selection", () => {
    const { pool, fellBack } = resolveRandomDrawPool(catalog, options);
    expect(pool.map((d) => d.id)).toEqual(["a", "b"]);
    expect(fellBack).toBe(false);
  });
});

describe("resolveEffectiveAiRandomPool", () => {
  const base: EffectiveAiRandomPoolOptions = {
    bracketFilter: [],
    cedhMode: false,
    selectedFormat: "Commander",
    archetypeFilter: "Any",
    coverageFloor: 90,
  };

  it("falls back to the curated pool when soft preferences empty it", () => {
    // The reported case: excluding only the coverage-95 deck must preview
    // Random (1) — the coverage-55 deck production would draw — not Random (0).
    const catalog = [deck("hi", { coveragePct: 95 }), deck("lo", { coveragePct: 55 })];
    expect(
      resolveEffectiveAiRandomPool(catalog, {
        ...base,
        poolSelection: { ...emptyPoolSelection(), excludedDecks: ["hi"] },
      }).map((d) => d.id),
    ).toEqual(["lo"]);
  });

  it("falls back to the catalog when the bracket restriction matches nothing", () => {
    const catalog = [deck("b2", { bracket: 2 }), deck("b4", { bracket: 4 })];
    expect(
      resolveEffectiveAiRandomPool(catalog, {
        ...base,
        bracketFilter: [5],
        coverageFloor: 0,
      }).map((d) => d.id),
    ).toEqual(["b2", "b4"]);
  });

  it("narrows to the soft-filtered pool when preferences match", () => {
    const catalog = [deck("hi", { coveragePct: 95 }), deck("lo", { coveragePct: 55 })];
    expect(
      resolveEffectiveAiRandomPool(catalog, base).map((d) => d.id),
    ).toEqual(["hi"]);
  });
});

describe("cedhSeatProblem", () => {
  const noBracketFive = [deck("b2", { bracket: 2 }), deck("b4", { bracket: 4 })];

  function problem(
    catalog: AiDeckCandidate[],
    requestedDeckIds: string[],
    overrides: Partial<Parameters<typeof cedhSeatProblem>[0]> = {},
  ): CedhSeatProblem | null {
    return cedhSeatProblem({
      catalog,
      requestedDeckIds,
      bracketFilter: [],
      cedhMode: true,
      selectedFormat: "Commander",
      ...overrides,
    });
  }

  it("reports NoCedhDeckForRandom for a Random seat at a cEDH table with no bracket-5 deck", () => {
    expect(problem(noBracketFive, [AI_DECK_RANDOM])).toBe("NoCedhDeckForRandom");
    expect(problem([], ["gone", AI_DECK_RANDOM])).toBe("NoCedhDeckForRandom");
  });

  it("treats a pinned id missing from the catalog like Random", () => {
    expect(problem(noBracketFive, ["gone"])).toBe("NoCedhDeckForRandom");
  });

  it("reports PinnedDeckNotCedh for a seat pinned to a non-bracket-5 catalog deck", () => {
    expect(problem(noBracketFive, ["b2", "b4"])).toBe("PinnedDeckNotCedh");
    const withCedh = [...noBracketFive, deck("cedh", { bracket: 5 })];
    expect(problem(withCedh, ["cedh", "b4"])).toBe("PinnedDeckNotCedh");
    expect(problem(withCedh, [AI_DECK_RANDOM, "b4"])).toBe("PinnedDeckNotCedh");
  });

  it("treats an untagged pinned deck as not bracket 5", () => {
    expect(problem([deck("untagged", { bracket: null })], ["untagged"])).toBe("PinnedDeckNotCedh");
  });

  it("is null when every pinned deck is bracket 5", () => {
    const catalog = [...noBracketFive, deck("cedh", { bracket: 5 })];
    expect(problem(catalog, ["cedh"])).toBeNull();
    expect(problem(catalog, ["cedh", AI_DECK_RANDOM])).toBeNull();
  });

  it("is null when the bracket-5 pool is non-empty", () => {
    const catalog = [...noBracketFive, deck("cedh", { bracket: 5 })];
    expect(problem(catalog, [AI_DECK_RANDOM])).toBeNull();
  });

  it("is null outside cEDH mode and outside the Commander family", () => {
    expect(problem(noBracketFive, [AI_DECK_RANDOM], { cedhMode: false })).toBeNull();
    expect(problem(noBracketFive, [AI_DECK_RANDOM], { selectedFormat: "Standard" })).toBeNull();
    expect(problem(noBracketFive, [AI_DECK_RANDOM], { selectedFormat: null })).toBeNull();
  });

  it("does not flag a pinned non-bracket-5 deck at a non-cEDH table", () => {
    expect(problem(noBracketFive, ["b4"], { cedhMode: false })).toBeNull();
    expect(problem(noBracketFive, ["b4"], { selectedFormat: "Standard" })).toBeNull();
  });

  it("ignores the manual bracket selection, which cEDH mode overrides", () => {
    expect(problem(noBracketFive, [AI_DECK_RANDOM], { bracketFilter: [2] })).toBe("NoCedhDeckForRandom");
  });

  it("is unaffected by archetype and coverage, which the predicate does not take", () => {
    // A bracket-5 deck that the soft archetype/coverage filters would exclude
    // still keeps the bracket pool non-empty.
    const catalog = [deck("cedh", { bracket: 5, coveragePct: 1, archetype: "Aggro" })];
    expect(problem(catalog, [AI_DECK_RANDOM])).toBeNull();
  });
});
