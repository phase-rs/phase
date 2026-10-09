import { describe, expect, it } from "vitest";

import type { CommanderBracket } from "../../types/bracket";
import {
  restrictAiPoolByBracket,
  type BracketTaggedCandidate,
} from "../aiRandomPool";

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
