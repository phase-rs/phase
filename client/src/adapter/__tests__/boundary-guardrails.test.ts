import { readdirSync, readFileSync } from "node:fs";
import { dirname, posix, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import ts from "typescript";
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

/** The `paths` aliases from `tsconfig.app.json` that point into `client/src`. */
const WASM_ALIASES = new Map([
  ["@wasm/engine", "wasm/engine_wasm"],
  ["@wasm/draft", "wasm/draft_wasm"],
]);

/**
 * The `client/src`-relative modules a source file loads at runtime, without
 * extensions and with `@wasm/*` aliases resolved. Read from the syntax tree, so
 * comments, `import type`, `export type` and `typeof import()` (all erased at
 * compile time) are skipped, while side-effect imports, re-exports and literal
 * `import()` calls are kept. Under `verbatimModuleSyntax`, `import { type X }`
 * still emits an import, so it counts.
 */
function runtimeImports(file: string, sourceText: string): string[] {
  const resolveSpecifier = (specifier: string): string => {
    const aliased = WASM_ALIASES.get(specifier);
    if (aliased) return aliased;
    if (!specifier.startsWith(".")) return specifier;
    return posix.join(posix.dirname(file), specifier).replace(/\.(?:tsx?|js)$/, "");
  };
  const source = ts.createSourceFile(file, sourceText, ts.ScriptTarget.Latest, false, ts.ScriptKind.TSX);
  const imports: string[] = [];
  const visit = (node: ts.Node): void => {
    const specifier =
      ts.isImportDeclaration(node) && !node.importClause?.isTypeOnly ? node.moduleSpecifier
      : ts.isExportDeclaration(node) && !node.isTypeOnly ? node.moduleSpecifier
      : ts.isCallExpression(node) && node.expression.kind === ts.SyntaxKind.ImportKeyword ? node.arguments[0]
      : undefined;
    if (specifier && ts.isStringLiteralLike(specifier)) imports.push(resolveSpecifier(specifier.text));
    ts.forEachChild(node, visit);
  };
  visit(source);
  return imports;
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

  it("finds every runtime import form and skips type-only ones", () => {
    const source = `
      import { a } from "@wasm/engine";
      import { type B } from "../wasm/engine_wasm.js";
      import "./side-effect";
      export * from "./reexported";
      const lazy = import(/* @vite-ignore */ "./lazy");
      const templated = import(\`./templated\`);
      import type { C } from "./type-only";
      export type { D } from "./type-only-export";
      type E = typeof import("./import-type");
      // import { f } from "./commented";
    `;
    expect(runtimeImports("adapter/probe.ts", source)).toEqual([
      "wasm/engine_wasm",
      "wasm/engine_wasm",
      "adapter/side-effect",
      "adapter/reexported",
      "adapter/lazy",
      "adapter/templated",
    ]);
  });

  it("keeps one engine instance per tab", () => {
    // Each runtime load of `@wasm/engine` outside the worker is a second engine
    // in the tab: another 40 MB module compile and, once it loads the card
    // database, ~540 MB more WASM memory. That is past iOS Safari's ceiling, so
    // the tab is killed (#9696 did this from the Play setup screen). UI code asks
    // the engine through `getSharedAdapter()`, or `services/engineRuntime`,
    // which routes there. The main-thread fallback allocates that second
    // instance too, so only the adapter may load it, and only when no worker
    // could start.
    const allowedImporters = new Map([
      ["wasm/engine_wasm", ["adapter/engine-worker.ts", "adapter/main-thread-engine.ts"]],
      ["adapter/main-thread-engine", ["adapter/wasm-adapter.ts"]],
    ]);

    const srcDir = resolve(repoRoot(), "client/src");
    const sources = readdirSync(srcDir, { recursive: true, encoding: "utf8" }).filter(
      (file) => /\.tsx?$/.test(file) && !/(^|\/)__tests__\//.test(file) && !file.startsWith("wasm/"),
    );
    expect(sources.length).toBeGreaterThan(100);

    const importers = new Map([...allowedImporters.keys()].map((target) => [target, [] as string[]]));
    for (const file of sources) {
      const text = readFileSync(resolve(srcDir, file), "utf8");
      // Any import of a guarded module spells its file name, through the alias
      // or a relative path, so only those files need a syntax tree.
      if (!/engine_wasm|@wasm\/engine|main-thread-engine/.test(text)) continue;
      for (const target of new Set(runtimeImports(file, text))) {
        importers.get(target)?.push(file);
      }
    }
    // Exact equality also fails a rename of an allowed importer, which would
    // otherwise leave this passing without scanning it.
    expect(Object.fromEntries(importers)).toEqual(Object.fromEntries(allowedImporters));
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
