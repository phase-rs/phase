import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import type { WaitingFor } from "../types";
import { isWaitingForHandled } from "../../game/waitingForRegistry";
import { repoRoot, rustEnumVariants } from "./rustEnumVariants";

const ADAPTER_FILES = [
  "ws-adapter.ts",
  "p2p-adapter.ts",
  "wasm-adapter.ts",
  "engine-worker-client.ts",
  "engine-worker.ts",
  "index.ts",
];

function tsUnionVariantTypes(source: string, typeName: string, followingHeader: string): string[] {
  const unionStart = source.indexOf(`export type ${typeName} =`);
  expect(unionStart, `${typeName} union should exist`).toBeGreaterThanOrEqual(0);

  const unionEnd = source.indexOf(followingHeader, unionStart);
  expect(unionEnd, `${typeName} union should end before ${followingHeader}`).toBeGreaterThan(
    unionStart,
  );

  return Array.from(
    source
      .slice(unionStart, unionEnd)
      .matchAll(/^ {2}\| \{(?: type:|\n {6}type:) "([A-Z][A-Za-z0-9]+)"/gm),
    (match) => match[1],
  );
}

function alternativeCastKeywordTypes(source: string): string[] {
  const marker = '{ type: "AlternativeCastChoice"; data:';
  const alternativeCastChoiceStart = source.indexOf(marker);
  expect(
    alternativeCastChoiceStart,
    "AlternativeCastChoice waiting payload should exist",
  ).toBeGreaterThanOrEqual(0);

  const keywordStart = source.indexOf("keyword:", alternativeCastChoiceStart);
  expect(keywordStart, "AlternativeCastChoice keyword field should exist").toBeGreaterThan(
    alternativeCastChoiceStart,
  );

  const keywordEnd = source.indexOf("; normal_cost:", keywordStart);
  expect(
    keywordEnd,
    "AlternativeCastChoice keyword field should end before normal_cost",
  ).toBeGreaterThan(keywordStart);

  return Array.from(
    source
      .slice(keywordStart, keywordEnd)
      .matchAll(/\{ type: "([A-Z][A-Za-z0-9]+)" \}/g),
    (match) => match[1],
  );
}

describe("adapter boundary guardrails", () => {
  it("adapter modules do not import stores or use localStorage directly", () => {
    const adapterDir = dirname(fileURLToPath(import.meta.url));
    for (const file of ADAPTER_FILES) {
      const source = readFileSync(resolve(adapterDir, "..", file), "utf8");
      expect(source).not.toMatch(/from "\.\.\/stores\//);
      expect(source).not.toContain("localStorage");
    }
  });

  it("keeps the frontend WaitingFor union in lockstep with the engine enum", () => {
    const root = repoRoot();
    const rustSource = readFileSync(
      resolve(root, "crates/engine/src/types/game_state.rs"),
      "utf8",
    );
    const tsSource = readFileSync(resolve(root, "client/src/adapter/types.ts"), "utf8");

    const rustVariants = rustEnumVariants(rustSource, "WaitingFor");
    const tsVariants = tsUnionVariantTypes(tsSource, "WaitingFor", "// ── Learn");

    expect(new Set(tsVariants)).toEqual(new Set(rustVariants));
  });

  it("keeps the alternative-cast keyword payload in lockstep with the engine enum", () => {
    const root = repoRoot();
    const rustSource = readFileSync(
      resolve(root, "crates/engine/src/types/game_state.rs"),
      "utf8",
    );
    const tsSource = readFileSync(resolve(root, "client/src/adapter/types.ts"), "utf8");

    const rustVariants = rustEnumVariants(rustSource, "AlternativeCastKeyword");
    const tsVariants = alternativeCastKeywordTypes(tsSource);

    expect(new Set(tsVariants)).toEqual(new Set(rustVariants));
  });

  it("handles the discard-for-mana-ability pay-cost waiting payload", () => {
    const waitingFor: WaitingFor = {
      type: "PayCost",
      data: {
        player: 0,
        kind: { type: "Discard" },
        choices: [42],
        count: 1,
        min_count: 0,
        resume: { type: "ManaAbility", ManaAbility: {} },
      },
    };

    expect(isWaitingForHandled(waitingFor)).toBe(true);
  });

  it("handles the populate creature-token choice waiting payload", () => {
    const waitingFor: WaitingFor = {
      type: "PopulateChoice",
      data: { player: 0, source_id: 1, valid_tokens: [10, 11] },
    };

    expect(isWaitingForHandled(waitingFor)).toBe(true);
  });

  it("handles the copy-retarget waiting payload", () => {
    const waitingFor: WaitingFor = {
      type: "CopyRetarget",
      data: {
        player: 0,
        copy_id: 7,
        target_slots: [
          {
            current: { Object: 42 },
            legal_alternatives: [{ Object: 43 }, { Player: 1 }],
          },
        ],
      },
    };

    expect(isWaitingForHandled(waitingFor)).toBe(true);
  });

  it("handles the splice offer waiting payload", () => {
    const waitingFor: WaitingFor = {
      type: "SpliceOffer",
      data: {
        player: 0,
        pending_cast: {} as Extract<
          WaitingFor,
          { type: "SpliceOffer" }
        >["data"]["pending_cast"],
        eligible: [2],
      },
    };

    expect(isWaitingForHandled(waitingFor)).toBe(true);
  });

  it("keeps the frontend GameAction union in lockstep with the engine enum", () => {
    const root = repoRoot();
    const rustSource = readFileSync(resolve(root, "crates/engine/src/types/actions.rs"), "utf8");
    const tsSource = readFileSync(resolve(root, "client/src/adapter/types.ts"), "utf8");

    const rustVariants = rustEnumVariants(rustSource, "GameAction");
    const tsVariants = tsUnionVariantTypes(tsSource, "GameAction", "// CR 605.3b");

    expect(new Set(tsVariants)).toEqual(new Set(rustVariants));
  });
});
