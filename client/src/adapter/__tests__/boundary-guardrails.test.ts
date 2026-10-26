import { readdirSync, readFileSync } from "node:fs";
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

  it("keeps one engine instance per tab", () => {
    // Each runtime load of `@wasm/engine` outside the worker is a second engine
    // in the tab: another 40 MB module compile and, once it loads the card
    // database, ~540 MB more WASM memory. That is past iOS Safari's ceiling, so
    // the tab is killed (#9696 did this from the Play setup screen). UI code asks
    // the engine through `getSharedAdapter()`, or `services/engineRuntime`,
    // which routes there.
    const RUNTIME_ENGINE_IMPORT = new RegExp(
      [
        /^\s*(?:import|export)\s+(?!type\b)[^;]*?\bfrom\s*["']@wasm\/engine["']/.source,
        /(?<!typeof\s*)\bimport\(\s*["']@wasm\/engine["']\s*\)/.source,
      ].join("|"),
      "m",
    );
    // The fallback's loaders allocate that second instance too; only the
    // adapter may call them, and only when no worker could start.
    const FALLBACK_LOADER_IMPORT =
      /import\s*{[^}]*\b(?:ensureWasmInit|ensureCardDatabase|loadEngineModule)\b[^}]*}\s*from\s*["'][^"']*main-thread-engine(?:\.ts)?["']/;
    const ENGINE_OWNERS = new Set(["adapter/engine-worker.ts", "adapter/main-thread-engine.ts"]);

    const srcDir = resolve(repoRoot(), "client/src");
    const sources = readdirSync(srcDir, { recursive: true, encoding: "utf8" }).filter(
      (file) => /\.tsx?$/.test(file) && !/(^|\/)__tests__\//.test(file) && !file.startsWith("wasm/"),
    );
    expect(sources.length).toBeGreaterThan(100);

    const offenders = sources.filter((file) => {
      const source = readFileSync(resolve(srcDir, file), "utf8");
      return (!ENGINE_OWNERS.has(file) && RUNTIME_ENGINE_IMPORT.test(source))
        || (file !== "adapter/wasm-adapter.ts" && FALLBACK_LOADER_IMPORT.test(source));
    });
    expect(offenders).toEqual([]);

    // The scan must still see the owners, or a rename would pass it vacuously.
    for (const owner of ENGINE_OWNERS) {
      expect(RUNTIME_ENGINE_IMPORT.test(readFileSync(resolve(srcDir, owner), "utf8")), owner).toBe(true);
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

  it("handles the token-host waiting payload", () => {
    const waitingFor: WaitingFor = {
      type: "ChooseTokenHost",
      data: { player: 0, source_id: 1, legal_targets: [{ Object: 7 }, { Object: 8 }], pending_ability: null },
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
