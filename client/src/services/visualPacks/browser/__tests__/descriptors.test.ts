import { describe, expect, it } from "vitest";

import { decodeCandidateKey } from "../../candidateKeys.ts";
import { packId } from "../../types.ts";
import { canonicalDescriptors, englishDescriptors } from "../descriptors.ts";

const PACK = packId("deck_library");
const ORACLE = "11111111-abcd-4111-8111-111111111111";
const PRINTING = "22222222-abcd-4222-8222-222222222222";

function decodedKinds(keys: readonly string[]) {
  return keys.map((key) => decodeCandidateKey(key)[0]);
}

describe("Scryfall descriptor candidate projection", () => {
  it("appends semantic identities after the existing exact-printing identities", () => {
    const [descriptor] = englishDescriptors(PACK, {
      id: PRINTING,
      oracleId: ORACLE,
      set: "M21",
      collector: "123a",
      name: "Front Card",
      faces: [{ name: "Front Face", images: { normal: "https://example.test/front.jpg" } }],
    });

    expect(descriptor?.candidateKeys.map((key) => decodeCandidateKey(key))).toEqual([
      ["english_printing", [PRINTING, 0, "full_card", "normal"]],
      ["english_alias", ["front card", 0, "full_card", "normal"]],
      ["english_alias", ["front face", 0, "full_card", "normal"]],
      ["oracle", [ORACLE, 0, "full_card", "normal"]],
      ["oracle_alias", ["front card", 0, "full_card", "normal"]],
      ["oracle_alias", ["front face", 0, "full_card", "normal"]],
      ["source_printing", ["m21", "123a", "front face", "full_card", "normal"]],
      ["oracle_face", [ORACLE, "front face", "full_card", "normal"]],
      ["name_face", ["front card", "front face", "full_card", "normal"]],
    ]);
  });

  it("adds only oracle and name semantic groups to canonical descriptors", () => {
    const [descriptor] = canonicalDescriptors(PACK, {
      oracleId: ORACLE,
      name: "Canonical Card",
      faces: [{ name: "Canonical Face", images: { normal: "https://example.test/canonical.jpg" } }],
    });

    expect(decodedKinds(descriptor?.candidateKeys ?? [])).toEqual([
      "oracle", "oracle_alias", "oracle_alias", "oracle_face", "name_face",
    ]);
    expect(descriptor?.candidateKeys.map((key) => decodeCandidateKey(key)).slice(-2)).toEqual([
      ["oracle_face", [ORACLE, "canonical face", "full_card", "normal"]],
      ["name_face", ["canonical card", "canonical face", "full_card", "normal"]],
    ]);
  });

  it("keeps modal double-faced card semantic identities distinct per face", () => {
    const descriptors = canonicalDescriptors(PACK, {
      oracleId: ORACLE,
      name: "Modal Card",
      faces: [
        { name: "Front Face", images: { normal: "https://example.test/front.jpg" } },
        { name: "Back Face", images: { normal: "https://example.test/back.jpg" } },
      ],
    });

    expect(descriptors.map((descriptor) => decodeCandidateKey(descriptor.candidateKeys[descriptor.candidateKeys.length - 1]!)[1][1]))
      .toEqual(["front face", "back face"]);
  });

  it("does not emit descriptors or semantic keys for absent image URLs", () => {
    expect(englishDescriptors(PACK, {
      id: PRINTING,
      oracleId: ORACLE,
      set: "m21",
      collector: "123",
      name: "No Image",
      faces: [{ name: "No Image", images: {} }],
    })).toEqual([]);
  });
});
