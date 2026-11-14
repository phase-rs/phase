/**
 * Shared instrument for the mirror gates: read the lobby Worker's SOURCE TEXT
 * and extract its declarations, so a drift in `lobby-worker/src/` reds a client
 * test.
 *
 * `client/tsconfig.app.json` is `"include": ["src"]` and `lobby-worker/` is a
 * separate package with its own ambient Cloudflare types, so there is no import
 * path from the client to the Worker — reading the text is the only way to
 * compare the two declarations. Two suites now need the same extractors
 * (`serverDirectory.test.ts` for the directory contract,
 * `serverMetrics.test.ts` for the metrics contract), so they live here instead
 * of being declared twice.
 *
 * NOT A SUITE: `vitest.config.ts` collects `src/**\/*.test.{ts,tsx}` only, and
 * this filename matches neither half, so it adds no test file to the run.
 */
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../../../..");

/** `lobby-worker/src/directory.ts` — the contract declarations. */
export const workerDirectory = readFileSync(
  resolve(repoRoot, "lobby-worker/src/directory.ts"),
  "utf8",
);

/** `lobby-worker/src/lobby-do.ts` — the DDL and the request handlers. */
export const workerLobbyDo = readFileSync(
  resolve(repoRoot, "lobby-worker/src/lobby-do.ts"),
  "utf8",
);

/** Field names declared directly in one interface body. Stops at the first
 * column-0 `}`, so it reads exactly one declaration. */
export function interfaceFields(source: string, name: string): string[] {
  const start = source.match(new RegExp(`export interface ${name}[^{]*\\{`));
  if (!start?.index) return [];
  const bodyStart = start.index + start[0].length;
  const end = source.indexOf("\n}", bodyStart);
  if (end === -1) return [];
  return [...source.slice(bodyStart, end).matchAll(/^ {2}(\w+)\??:/gm)].map((m) => m[1]);
}

/** Column names of one `CREATE TABLE IF NOT EXISTS` statement. */
export function ddlColumns(source: string, table: string): string[] {
  const start = source.indexOf(`CREATE TABLE IF NOT EXISTS ${table} (`);
  if (start === -1) return [];
  const end = source.indexOf(")", start);
  if (end === -1) return [];
  return [
    ...source.slice(start, end).matchAll(/^\s+(\w+)\s+(?:TEXT|INTEGER|REAL|BLOB)/gm),
  ].map((m) => m[1]);
}

/** The string members of a top-level `const <name>: readonly ... = [ ... ]`
 * array literal. Used for the outcome-union mirror, where the Worker's runtime
 * list — not just its type — is what the sanitiser filters on. */
export function stringArrayLiteral(source: string, name: string): string[] {
  // Anchored on the ASSIGNMENT's bracket, never merely the first `[` after the
  // name: a `readonly ProbeOutcome[]` type annotation puts a bracket pair
  // between the two, and reading that one yields an empty list — a silently
  // vacuous comparison rather than a failure.
  const match = new RegExp(`const ${name}[^=]*=\\s*\\[`).exec(source);
  if (!match) return [];
  const open = match.index + match[0].length - 1;
  const close = source.indexOf("]", open);
  if (close === -1) return [];
  return [...source.slice(open, close).matchAll(/"([^"]+)"/g)].map((m) => m[1]);
}

/** The integer a top-level `export const <name> = <expr>;` evaluates to.
 * `NaN` when the declaration is absent or is not a simple arithmetic literal —
 * which the guard-the-guard row asserts against, so a missing constant cannot
 * make a comparison pass over nothing. */
export function numericConstant(source: string, name: string): number {
  const match = new RegExp(`export const ${name} = ([^;]+);`).exec(source);
  if (!match) return NaN;
  const expr = match[1].replace(/_/g, "").trim();
  // Digits and the multiplication of two of them are the only forms the
  // Worker uses (`60_000`, `32 * 1024`); anything else is a drift this must
  // not silently accept.
  if (!/^\d+(\s*\*\s*\d+)?$/.test(expr)) return NaN;
  return expr
    .split("*")
    .map((part) => Number(part.trim()))
    .reduce((a, b) => a * b);
}
