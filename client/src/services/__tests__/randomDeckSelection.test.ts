import { describe, expect, it } from "vitest";

import {
  pickRandomDeckCandidate,
  randomDeckPriority,
  topRandomDeckPool,
  type RandomDeckCandidate,
} from "../randomDeckSelection";

function candidate(
  id: string,
  knownFormat?: RandomDeckCandidate["knownFormat"],
  selected_format_compatible?: boolean | null,
): RandomDeckCandidate {
  return {
    id,
    name: id,
    source: { type: "saved" },
    knownFormat,
    compatibility: selected_format_compatible == null
      ? null
      : { selected_format_compatible },
  };
}

describe("random deck selection", () => {
  it("ranks exact known-format decks above compatible unknowns and incompatible decks", () => {
    expect(randomDeckPriority(candidate("standard", "Standard"), "Standard")).toBe(0);
    expect(randomDeckPriority(candidate("user-compatible", undefined, true), "Standard")).toBe(1);
    expect(randomDeckPriority(candidate("unknown"), "Standard")).toBe(2);
    expect(randomDeckPriority(candidate("pauper", "Pauper"), "Standard")).toBe(4);
    expect(randomDeckPriority(candidate("illegal", undefined, false), "Standard")).toBe(4);
  });

  it("picks only from the best available tier", () => {
    const pool = [
      candidate("pauper", "Pauper"),
      candidate("user-compatible", undefined, true),
      candidate("standard-a", "Standard"),
      candidate("standard-b", "Standard"),
    ];

    expect(topRandomDeckPool(pool, { selectedFormat: "Standard" }).map((deck) => deck.id)).toEqual([
      "standard-a",
      "standard-b",
    ]);
    expect(
      pickRandomDeckCandidate(pool, { selectedFormat: "Standard", random: () => 0.99 })?.id,
    ).toBe("standard-b");
  });

  it("uses lower-ranked fresh decks before duplicating excluded top-tier decks", () => {
    const pool = [
      candidate("standard", "Standard"),
      candidate("user-compatible", undefined, true),
    ];

    expect(
      pickRandomDeckCandidate(pool, {
        selectedFormat: "Standard",
        excludeIds: new Set(["standard"]),
        random: () => 0,
      })?.id,
    ).toBe("user-compatible");
  });
});
