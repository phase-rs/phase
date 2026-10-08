import { describe, expect, it } from "vitest";

import {
  CARD_CANDIDATE_PROJECTION_VERSION,
  decodeCandidateKey,
  encodeCandidateKey,
  semanticCardCandidateGroups,
} from "../candidateKeys.ts";

const ORACLE = "11111111-abcd-4111-8111-111111111111";

function semantic(overrides: Partial<Parameters<typeof semanticCardCandidateGroups>[0]> = {}) {
  return semanticCardCandidateGroups({
    oracleId: ORACLE,
    sourceSetCode: "M21",
    sourceCollectorNumber: "123a",
    cardName: "Éowyn",
    faceName: "Éowyn, Shieldmaiden",
    variant: "full_card",
    rung: "large",
    ...overrides,
  });
}

describe("semanticCardCandidateGroups", () => {
  it("declares v2 of the descriptor projection while retaining candidate:v1 wire keys", () => {
    expect(CARD_CANDIDATE_PROJECTION_VERSION).toBe(2);
  });

  it("encodes source, oracle, and name identities in canonical order", () => {
    const groups = semantic();

    expect(groups.map((group) => group.rung)).toEqual(["normal", "normal", "normal"]);
    expect(groups.flatMap((group) => group.keys).map((key) => decodeCandidateKey(key))).toEqual([
      ["source_printing", ["m21", "123a", "éowyn, shieldmaiden", "full_card", "normal"]],
      ["oracle_face", [ORACLE, "éowyn, shieldmaiden", "full_card", "normal"]],
      ["name_face", ["éowyn", "éowyn, shieldmaiden", "full_card", "normal"]],
    ]);
  });

  it("normalizes semantic names to lowercase NFC and omits an absent source identity", () => {
    const groups = semantic({
      sourceSetCode: undefined,
      sourceCollectorNumber: undefined,
      cardName: "CAFE\u0301",
      faceName: "CAFE\u0301 FRONT",
      rung: "small",
    });

    expect(groups.flatMap((group) => group.keys).map((key) => decodeCandidateKey(key))).toEqual([
      ["oracle_face", [ORACLE, "café front", "full_card", "small"]],
      ["name_face", ["café", "café front", "full_card", "small"]],
    ]);
  });

  it("keeps source keys unique even when card and face names coincide", () => {
    const keys = semantic({ cardName: "Same", faceName: "Same" }).flatMap((group) => group.keys);

    expect(new Set(keys)).toHaveLength(keys.length);
    expect(keys).toHaveLength(3);
  });

  it.each([
    ["upper-case oracle", () => semantic({ oracleId: ORACLE.toUpperCase() })],
    ["empty oracle", () => semantic({ oracleId: "" })],
    ["invalid source set", () => semantic({ sourceSetCode: "M-21" })],
    ["partial source set", () => semantic({ sourceSetCode: "m21", sourceCollectorNumber: undefined })],
    ["partial source collector", () => semantic({ sourceSetCode: undefined, sourceCollectorNumber: "123" })],
    ["empty card name", () => semantic({ cardName: "" })],
    ["empty face name", () => semantic({ faceName: "" })],
    ["malformed card name", () => semantic({ cardName: "\ud800" })],
    ["empty collector", () => semantic({ sourceCollectorNumber: "" })],
    ["malformed collector", () => semantic({ sourceCollectorNumber: "\ud800" })],
  ])("rejects %s at the builder boundary", (_name, build) => {
    expect(build).toThrow();
  });

  it.each([
    ["upper-case source tuple", "source_printing", ["M21", "123", "face", "full_card", "small"]],
    ["non-NFC card name", "name_face", ["cafe\u0301", "face", "full_card", "small"]],
    ["non-NFC collector", "source_printing", ["m21", "cafe\u0301", "face", "full_card", "small"]],
    ["wrong oracle-face arity", "oracle_face", [ORACLE, "face", 0, "full_card", "small"]],
    ["wrong source-printing arity", "source_printing", ["m21", "123", "face", 0, "full_card", "small"]],
    ["wrong name-face arity", "name_face", ["name", "face", 0, "full_card", "small"]],
    ["full-card art-crop rung", "name_face", ["name", "face", "full_card", "art_crop"]],
    ["art-crop normal rung", "name_face", ["name", "face", "art_crop", "normal"]],
  ] as const)("rejects %s through the public candidate validator", (_name, kind, tuple) => {
    expect(() => encodeCandidateKey(kind, tuple)).toThrow();
  });
});
