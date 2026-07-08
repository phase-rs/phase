import { describe, expect, test } from "bun:test";

import {
  buildCardNameIndex,
  extractCardReferences,
  normalizeCardName,
  resolveCardReference,
} from "../cardNames.ts";
import { extractSummary } from "../extract.ts";

// A small synthetic card-data key set covering every tricky case. These are the
// lowercased canonical keys the way card-data.json stores them.
const KEYS = [
  "welcome to . . .", // REX Saga front face (spaced dots)
  "jurassic park", // its back face
  "welcome to australia",
  "welcome to sweettooth",
  "welcome to the fold",
  "welcome to valley",
  "welcome to mini-apolis",
  "sp//dr, piloted by peni",
  "lightning bolt",
  "lightning blast",
  "fire",
  "ice",
  "lava axe",
  "lava, axe",
  "_____",
  "______",
  "urza's saga",
];

const index = buildCardNameIndex(KEYS);

describe("normalizeCardName", () => {
  test("collapses spaced dots and ellipses to the same form", () => {
    expect(normalizeCardName("welcome to . . .")).toBe("welcome to");
    expect(normalizeCardName("welcome to...")).toBe("welcome to");
    expect(normalizeCardName("Welcome To…")).toBe("welcome to");
  });

  test("collapses // and commas to spaces", () => {
    expect(normalizeCardName("SP//dr, Piloted by Peni")).toBe("sp dr piloted by peni");
  });

  test("maps a URL slug onto the card name", () => {
    expect(normalizeCardName("welcome-to-jurassic-park")).toBe("welcome to jurassic park");
  });

  test("empty for punctuation-only names", () => {
    expect(normalizeCardName("_____")).toBe("");
  });
});

describe("resolveCardReference", () => {
  test("exact normalized hit for the spaced-dot Saga (the #4389 case)", () => {
    expect(resolveCardReference("welcome to...", index)).toEqual(["welcome to . . ."]);
  });

  test("// name resolves through normalization", () => {
    expect(resolveCardReference("sp dr piloted by peni", index)).toEqual([
      "sp//dr, piloted by peni",
    ]);
  });

  test("DFC combined name splits into both face keys", () => {
    expect(resolveCardReference("welcome to jurassic park", index)).toEqual([
      "welcome to . . .",
      "jurassic park",
    ]);
  });

  test("unique word-boundary prefix resolves a truncated reference", () => {
    expect(resolveCardReference("lightning bo", index)).toEqual(["lightning bolt"]);
  });

  test("ambiguous prefix resolves to nothing", () => {
    expect(resolveCardReference("lightning b", index)).toEqual([]);
  });

  test("ambiguous prefix is not silently forced (welcome ...)", () => {
    expect(resolveCardReference("welcome", index)).toEqual([]);
  });

  test("raw-key match wins over a normalized collision", () => {
    expect(resolveCardReference("lava axe", index)).toEqual(["lava axe"]);
    expect(resolveCardReference("lava, axe", index)).toEqual(["lava, axe"]);
  });

  test("a normalized collision that is not a raw key resolves to nothing", () => {
    // "lava.axe" is not a card-data key; its normalized form "lava axe" is
    // ambiguous (two keys), so we refuse to guess.
    expect(resolveCardReference("lava.axe", index)).toEqual([]);
  });

  test("punctuation-only reference never resolves to a blank-name card", () => {
    expect(resolveCardReference("...", index)).toEqual([]);
  });

  test("apostrophe-elided input matches via the no-space index", () => {
    expect(resolveCardReference("urzas saga", index)).toEqual(["urza's saga"]);
  });
});

describe("extractCardReferences", () => {
  test("[[...]] bracket with a truncated ellipsis name", () => {
    expect(extractCardReferences("[[welcome to...]] is broken", index)).toEqual([
      "welcome to . . .",
    ]);
  });

  test("[[sp//dr, piloted by peni]] with slashes and comma", () => {
    expect(extractCardReferences("cant add [[sp//dr, piloted by peni]]", index)).toEqual([
      "sp//dr, piloted by peni",
    ]);
  });

  test("Scryfall URL slug resolves a DFC to both faces", () => {
    const text = "see https://scryfall.com/card/rex/7/welcome-to-jurassic-park thanks";
    expect(extractCardReferences(text, index)).toEqual(["welcome to . . .", "jurassic park"]);
  });

  test("no references → empty", () => {
    expect(extractCardReferences("the game just froze", index)).toEqual([]);
  });
});

describe("extractSummary", () => {
  test("does not truncate at the dot inside a [[...]] ellipsis (the #4389 title bug)", () => {
    expect(extractSummary("[[welcome to...]] doesn't work")).toBe("[[welcome to...]] doesn't work");
  });

  test("stops at the first real sentence end", () => {
    expect(extractSummary("Card is broken. More detail here.")).toBe("Card is broken.");
  });

  test("does not treat a dot inside a URL as a sentence end", () => {
    expect(extractSummary("Repro at https://phase.gg/g/a.b.c then it crashes")).toBe(
      "Repro at https://phase.gg/g/a.b.c then it crashes",
    );
  });
});
