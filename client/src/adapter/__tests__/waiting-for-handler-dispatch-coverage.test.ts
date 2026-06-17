import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import type { WaitingFor } from "../types";
import { HANDLED_WAITING_FOR_TYPES } from "../../game/waitingForRegistry";

/**
 * Complementary gate to `waiting-for-handler-parity.test.ts`.
 *
 * The parity test asserts every ENGINE `WaitingFor` variant is a member of
 * `HANDLED_WAITING_FOR_TYPES` (catches the "missing from set" class — e.g. the
 * Assist gap). This test catches the OPPOSITE failure: a variant that IS listed
 * in `HANDLED_WAITING_FOR_TYPES` but has NO real UI dispatch/render site (the
 * "registry lies" / silent-hang class — e.g. LearnChoice before its modal
 * shipped). A handled variant must be referenced by some real dispatch or
 * render site, not merely listed in the registry.
 *
 * Heuristic: for every variant name in `HANDLED_WAITING_FOR_TYPES`, its exact
 * string literal (e.g. `"LearnChoice"`) must occur in at least one source file
 * under `client/src/{components,pages,viewmodel}`. The registry file itself and
 * the `WaitingFor` TS union definition (`adapter/types.ts`) are excluded —
 * those are declarations, not dispatch sites — but they live in `adapter/`
 * which is outside the scanned dirs anyway. Test files are excluded so the
 * registry's own membership list (re-exported into tests) cannot satisfy the
 * gate.
 */

function clientSrc(): string {
  return resolve(dirname(fileURLToPath(import.meta.url)), "../..");
}

const SCAN_DIRS = ["components", "pages", "viewmodel"] as const;

const SCANNABLE_EXTENSIONS = [".ts", ".tsx"];

/**
 * Variants whose real UI dispatch genuinely exists but is NOT expressed as a
 * bare `"VariantName"` string literal in a scanned file (e.g. dispatched via an
 * enum switch on a mapped value, or a constructed string). Each entry MUST cite
 * the concrete dispatch site (file:line) proving the UI is real.
 *
 * Intentionally EMPTY: re-verified against current main, every handled variant
 * has a string-literal dispatch site in the scanned dirs.
 */
const STRING_LITERAL_HEURISTIC_BLIND_SPOTS: ReadonlySet<WaitingFor["type"]> =
  new Set<WaitingFor["type"]>([]);

function collectSourceFiles(dir: string, acc: string[]): void {
  for (const entry of readdirSync(dir)) {
    const full = resolve(dir, entry);
    const stats = statSync(full);
    if (stats.isDirectory()) {
      // Skip test directories — the registry's membership list is re-exported
      // into tests and would otherwise satisfy the gate for free.
      if (entry === "__tests__") continue;
      collectSourceFiles(full, acc);
      continue;
    }
    if (SCANNABLE_EXTENSIONS.some((ext) => entry.endsWith(ext))) {
      acc.push(full);
    }
  }
}

describe("WaitingFor handler dispatch coverage", () => {
  it("every handled WaitingFor variant has a real dispatch/render site", () => {
    const root = clientSrc();
    const files: string[] = [];
    for (const dir of SCAN_DIRS) {
      collectSourceFiles(resolve(root, dir), files);
    }

    const corpus = files.map((file) => readFileSync(file, "utf8"));

    const missing = [...HANDLED_WAITING_FOR_TYPES].filter((variant) => {
      if (STRING_LITERAL_HEURISTIC_BLIND_SPOTS.has(variant)) return false;
      const needle = `"${variant}"`;
      return !corpus.some((contents) => contents.includes(needle));
    });

    expect(
      missing,
      `HANDLED_WAITING_FOR_TYPES variant(s) [${missing.join(", ")}] are registered ` +
        "as handled but have no dispatch/render site (no `\"VariantName\"` string literal) " +
        "in client/src/{components,pages,viewmodel}. The registry lies: the engine will " +
        "emit these states and the UI will silently hang. Wire a modal/overlay/affordance " +
        "for each, or — only if the dispatch genuinely exists via a non-literal indirection " +
        "(enum switch / constructed string) — add the variant to " +
        "STRING_LITERAL_HEURISTIC_BLIND_SPOTS with a cited file:line reference.",
    ).toEqual([]);
  });

  it("has no stale STRING_LITERAL_HEURISTIC_BLIND_SPOTS allowlist entries", () => {
    const stale = [...STRING_LITERAL_HEURISTIC_BLIND_SPOTS].filter(
      (variant) => !HANDLED_WAITING_FOR_TYPES.has(variant),
    );

    expect(
      stale,
      `STRING_LITERAL_HEURISTIC_BLIND_SPOTS contains entries [${stale.join(", ")}] that are ` +
        "no longer in HANDLED_WAITING_FOR_TYPES. Remove the stale allowlist entries.",
    ).toEqual([]);
  });
});
