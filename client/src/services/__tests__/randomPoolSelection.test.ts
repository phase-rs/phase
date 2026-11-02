import { describe, expect, it } from "vitest";

import type { DeckCatalogSource } from "../deckCatalog";
import {
  DECK_FOLDERS_KEY,
  DECK_METADATA_KEY,
  type DeckFolder,
} from "../../constants/storage";
import {
  applyPoolSelection,
  buildPoolSections,
  candidateInPool,
  emptyPoolSelection,
  feedPoolGroup,
  folderPoolGroup,
  isPoolSelectionEmpty,
  loadFolderIndex,
  normalizePoolSelection,
  poolGroupForCandidate,
  poolGroupState,
  PRECON_POOL_GROUP,
  setPoolDeckIncluded,
  setPoolGroupIncluded,
  summarizePoolSelection,
  UNFILED_POOL_GROUP,
  type FolderIndex,
  type PoolCandidate,
} from "../randomPoolSelection";

const saved = (feedId?: string): DeckCatalogSource =>
  feedId ? { type: "saved", feedId } : { type: "saved" };

function candidate(
  id: string,
  source: DeckCatalogSource,
  savedName?: string,
): PoolCandidate {
  return { id, name: id, source, savedName };
}

function folderIndex(folders: Record<string, string | undefined>): FolderIndex {
  return {
    folderIdOf: (savedName: string) => folders[savedName],
  };
}

const FOLDERS: DeckFolder[] = [
  { id: "f2", name: "Zeta", order: 1 },
  { id: "f1", name: "Alpha", order: 0 },
];

describe("loadFolderIndex", () => {
  it("self-heals a folderId naming a deleted folder to Unfiled", () => {
    localStorage.setItem(
      DECK_FOLDERS_KEY,
      JSON.stringify([{ id: "f1", name: "Aggro", order: 0 }]),
    );
    localStorage.setItem(
      DECK_METADATA_KEY,
      JSON.stringify({
        aggro: { addedAt: 1, folderId: "f1" },
        rogue: { addedAt: 2, folderId: "deleted" },
      }),
    );
    try {
      const index = loadFolderIndex();
      expect(index.folderIdOf("aggro")).toBe("f1");
      expect(index.folderIdOf("rogue")).toBeUndefined();
      expect(poolGroupForCandidate(candidate("saved:rogue", saved(), "rogue"), index)).toBe(
        UNFILED_POOL_GROUP,
      );
    } finally {
      localStorage.clear();
    }
  });
});

describe("poolGroupForCandidate", () => {
  const index = folderIndex({ aggro: "f1", draft: "f2", loose: undefined });

  it("groups saved decks by live folder id", () => {
    expect(poolGroupForCandidate(candidate("saved:aggro", saved(), "aggro"), index)).toBe(
      folderPoolGroup("f1"),
    );
  });

  it("sends folderless and nameless saved decks to Unfiled", () => {
    expect(poolGroupForCandidate(candidate("saved:loose", saved(), "loose"), index)).toBe(
      UNFILED_POOL_GROUP,
    );
    expect(poolGroupForCandidate(candidate("saved:anon", saved()), index)).toBe(
      UNFILED_POOL_GROUP,
    );
  });

  it("groups feed decks per feed and every precon together", () => {
    expect(
      poolGroupForCandidate(
        candidate("feed:meta:burn", { type: "feed", feedId: "meta" }),
        index,
      ),
    ).toBe(feedPoolGroup("meta"));
    expect(
      poolGroupForCandidate(
        candidate("precon:x", { type: "precon", deckId: "x", code: "TST" }),
        index,
      ),
    ).toBe(PRECON_POOL_GROUP);
  });

  it("groups saved feed mirrors by their feed, not their folder", () => {
    // The mirror lives in folder f1, but the feed toggle owns it.
    expect(
      poolGroupForCandidate(candidate("saved:aggro", saved("meta"), "aggro"), index),
    ).toBe(feedPoolGroup("meta"));
  });

  it("sends an adopted deck (provenance removed) back to user organization", () => {
    expect(
      poolGroupForCandidate(candidate("saved:aggro", saved(), "aggro"), index),
    ).toBe(folderPoolGroup("f1"));
  });
});

describe("candidateInPool", () => {
  const index = folderIndex({ aggro: "f1", control: "f1" });
  const aggro = candidate("saved:aggro", saved(), "aggro");
  const control = candidate("saved:control", saved(), "control");

  it("includes everything under an empty selection", () => {
    expect(candidateInPool(aggro, emptyPoolSelection(), index)).toBe(true);
  });

  it("lets an excluded group remove its members", () => {
    const selection = { ...emptyPoolSelection(), excludedGroups: [folderPoolGroup("f1")] };
    expect(candidateInPool(aggro, selection, index)).toBe(false);
  });

  it("lets an explicit deck state win over its group either way", () => {
    const folderOff = { ...emptyPoolSelection(), excludedGroups: [folderPoolGroup("f1")] };
    const rescued = { ...folderOff, includedDecks: ["saved:aggro"] };
    expect(candidateInPool(aggro, rescued, index)).toBe(true);
    expect(candidateInPool(control, rescued, index)).toBe(false);

    const singledOut = { ...emptyPoolSelection(), excludedDecks: ["saved:aggro"] };
    expect(candidateInPool(aggro, singledOut, index)).toBe(false);
    expect(candidateInPool(control, singledOut, index)).toBe(true);
  });

  it("excludes a saved feed mirror with its feed, ignoring folder state", () => {
    const mirror = candidate("saved:aggro", saved("meta"), "aggro");
    const feedOff = { ...emptyPoolSelection(), excludedGroups: [feedPoolGroup("meta")] };
    expect(candidateInPool(mirror, feedOff, index)).toBe(false);

    // The mirror's folder membership is irrelevant while it has provenance:
    // excluding f1 leaves it in, and an explicit include still rescues it
    // from the feed exclusion.
    const folderOff = { ...emptyPoolSelection(), excludedGroups: [folderPoolGroup("f1")] };
    expect(candidateInPool(mirror, folderOff, index)).toBe(true);
    const rescued = { ...feedOff, includedDecks: ["saved:aggro"] };
    expect(candidateInPool(mirror, rescued, index)).toBe(true);
  });

  it("excludes raw feed candidates with their feed", () => {
    const raw = candidate("feed:meta:burn", { type: "feed", feedId: "meta" });
    const feedOff = { ...emptyPoolSelection(), excludedGroups: [feedPoolGroup("meta")] };
    expect(candidateInPool(raw, feedOff, index)).toBe(false);
    expect(candidateInPool(raw, emptyPoolSelection(), index)).toBe(true);
  });
});

describe("applyPoolSelection", () => {
  const index = folderIndex({ a: "f1", b: "f1", c: undefined });
  const all = [
    candidate("saved:a", saved(), "a"),
    candidate("saved:b", saved(), "b"),
    candidate("saved:c", saved(), "c"),
  ];

  it("passes an empty selection through without touching storage", () => {
    // No index is passed: this would throw outside a DOM env if it loaded one.
    expect(applyPoolSelection(all, emptyPoolSelection())).toEqual({ pool: all, fellBack: false });
    expect(isPoolSelectionEmpty(emptyPoolSelection())).toBe(true);
  });

  it("keeps only selected decks and reports no fallback", () => {
    const selection = { ...emptyPoolSelection(), excludedDecks: ["saved:b"] };
    expect(applyPoolSelection(all, selection, index)).toEqual({
      pool: [all[0], all[2]],
      fellBack: false,
    });
  });

  it("falls back to the full input when the selection matches nothing", () => {
    const selection = {
      ...emptyPoolSelection(),
      excludedGroups: [folderPoolGroup("f1"), UNFILED_POOL_GROUP],
    };
    expect(applyPoolSelection(all, selection, index)).toEqual({ pool: all, fellBack: true });
  });

  it("does not report a fallback for an empty input", () => {
    const selection = { ...emptyPoolSelection(), excludedGroups: [UNFILED_POOL_GROUP] };
    expect(applyPoolSelection([], selection, index)).toEqual({ pool: [], fellBack: false });
  });
});

describe("setPoolGroupIncluded", () => {
  const members = ["saved:a", "saved:b"];

  it("unselecting a folder clears per-deck overrides inside it", () => {
    // A deck rescued while the folder was off must not linger: toggling the
    // folder off again is "unselect all", full stop.
    const selection = {
      excludedGroups: [] as string[],
      excludedDecks: ["saved:b"],
      includedDecks: ["saved:a"],
    };
    expect(setPoolGroupIncluded(selection, folderPoolGroup("f1"), members, false)).toEqual({
      excludedGroups: [folderPoolGroup("f1")],
      excludedDecks: [],
      includedDecks: [],
    });
  });

  it("selecting a folder clears per-deck overrides inside it", () => {
    const selection = {
      excludedGroups: [folderPoolGroup("f1")],
      excludedDecks: ["saved:b", "saved:elsewhere"],
      includedDecks: ["saved:a"],
    };
    expect(setPoolGroupIncluded(selection, folderPoolGroup("f1"), members, true)).toEqual({
      excludedGroups: [],
      excludedDecks: ["saved:elsewhere"],
      includedDecks: [],
    });
  });

  it("is idempotent for repeated toggles", () => {
    const off = setPoolGroupIncluded(emptyPoolSelection(), folderPoolGroup("f1"), members, false);
    expect(setPoolGroupIncluded(off, folderPoolGroup("f1"), members, false)).toEqual(off);
  });
});

describe("setPoolDeckIncluded", () => {
  it("records an explicit state that survives its group either way", () => {
    const folderOff = setPoolGroupIncluded(emptyPoolSelection(), folderPoolGroup("f1"), ["saved:a"], false);
    const rescued = setPoolDeckIncluded(folderOff, "saved:a", true);
    expect(rescued).toEqual({
      excludedGroups: [folderPoolGroup("f1")],
      excludedDecks: [],
      includedDecks: ["saved:a"],
    });
    const reshut = setPoolDeckIncluded(rescued, "saved:a", false);
    expect(reshut).toEqual({
      excludedGroups: [folderPoolGroup("f1")],
      excludedDecks: ["saved:a"],
      includedDecks: [],
    });
  });

  it("never duplicates an id", () => {
    const once = setPoolDeckIncluded(emptyPoolSelection(), "saved:a", false);
    expect(setPoolDeckIncluded(once, "saved:a", false).excludedDecks).toEqual(["saved:a"]);
  });
});

describe("poolGroupState", () => {
  const group = folderPoolGroup("f1");
  const members = ["saved:a", "saved:b"];

  it("reads all/none/mixed off the selection-effective membership", () => {
    expect(poolGroupState(members, group, emptyPoolSelection())).toBe("all");
    expect(
      poolGroupState(members, group, { ...emptyPoolSelection(), excludedGroups: [group] }),
    ).toBe("none");
    expect(
      poolGroupState(members, group, { ...emptyPoolSelection(), excludedDecks: ["saved:a"] }),
    ).toBe("mixed");
    expect(
      poolGroupState(
        members,
        group,
        {
          ...emptyPoolSelection(),
          excludedGroups: [group],
          includedDecks: ["saved:a"],
        },
      ),
    ).toBe("mixed");
  });
});

describe("buildPoolSections", () => {
  const index = folderIndex({ a: "f1", z: "f2", loose: undefined });

  it("orders folders by registry order, then Unfiled, feeds, and precons", () => {
    const sections = buildPoolSections(
      [
        candidate("precon:x", { type: "precon", deckId: "x", code: "TST" }),
        candidate("feed:beta:d", { type: "feed", feedId: "beta" }),
        candidate("saved:loose", saved(), "loose"),
        candidate("feed:alpha:d", { type: "feed", feedId: "alpha" }),
        candidate("saved:z", saved(), "z"),
        candidate("saved:a", saved(), "a"),
      ],
      { folders: FOLDERS, feedNameOf: (feedId) => `Feed ${feedId}` },
      index,
    );
    expect(sections.map((section) => section.key)).toEqual([
      folderPoolGroup("f1"),
      folderPoolGroup("f2"),
      UNFILED_POOL_GROUP,
      feedPoolGroup("alpha"),
      feedPoolGroup("beta"),
      PRECON_POOL_GROUP,
    ]);
    expect(sections[0]).toMatchObject({ kind: "folder", name: "Alpha" });
    expect(sections[3]).toMatchObject({ kind: "feed", name: "Feed alpha" });
  });

  it("sorts members alphabetically and skips empty groups", () => {
    const sections = buildPoolSections(
      [
        { ...candidate("saved:b", saved(), "loose-b"), name: "bravo" },
        { ...candidate("saved:a", saved(), "loose-a"), name: "alpha" },
      ],
      { folders: FOLDERS, feedNameOf: (feedId) => feedId },
      folderIndex({}),
    );
    expect(sections.map((section) => section.key)).toEqual([UNFILED_POOL_GROUP]);
    expect(sections[0]?.members.map((member) => member.name)).toEqual(["alpha", "bravo"]);
  });
});

describe("summarizePoolSelection", () => {
  const index = folderIndex({ a: "f1", b: "f1", c: undefined });
  const items = [
    candidate("saved:a", saved(), "a"),
    candidate("saved:b", saved(), "b"),
    candidate("saved:c", saved(), "c"),
  ];

  it("reports playable equal to selected without a base", () => {
    const selection = { ...emptyPoolSelection(), excludedDecks: ["saved:b"] };
    expect(summarizePoolSelection(items, selection, undefined, index)).toEqual({
      selected: 2,
      playable: 2,
      baseSize: 3,
    });
  });

  it("intersects the selection with the base", () => {
    // Everything selected, but the base holds only b and c.
    expect(
      summarizePoolSelection(items, emptyPoolSelection(), new Set(["saved:b", "saved:c"]), index),
    ).toEqual({ selected: 3, playable: 2, baseSize: 2 });
  });

  it("reports zero playable when the selection misses the base entirely", () => {
    // Only a is selected; the base holds only b and c: draws would silently
    // fall back to the full base.
    const selection = { ...emptyPoolSelection(), excludedDecks: ["saved:b", "saved:c"] };
    expect(
      summarizePoolSelection(items, selection, new Set(["saved:b", "saved:c"]), index),
    ).toEqual({ selected: 1, playable: 0, baseSize: 2 });
  });
});

describe("normalizePoolSelection", () => {
  it("resets malformed blobs to include-all and drops non-string entries", () => {
    expect(normalizePoolSelection(null)).toEqual(emptyPoolSelection());
    expect(normalizePoolSelection("nope")).toEqual(emptyPoolSelection());
    expect(
      normalizePoolSelection({
        excludedGroups: ["a", 7],
        excludedDecks: "saved:a",
        includedDecks: ["saved:b", null],
      }),
    ).toEqual({ excludedGroups: ["a"], excludedDecks: [], includedDecks: ["saved:b"] });
  });
});
