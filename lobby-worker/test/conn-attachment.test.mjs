import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

// Structural guardrail over the per-connection state mirrors.
//
// `ConnAttachment` (hello-gate.ts) and `DEFAULT_CONN` (lobby-do.ts) are both
// hand-maintained mirrors of `lobby_broker::ConnState`, whose JSON crosses the
// WASM boundary verbatim. Nothing forces them to stay in step, and they DID
// silently drift once already: `ConnState` gained `organized_tournaments` /
// `joined_tournaments` while both mirrors kept their four pre-tournament
// fields, undetected because `#[serde(default)]` makes the Rust side tolerate
// the missing keys.
//
// These are read as SOURCE TEXT rather than imported because `lobby-do.ts`
// cannot be imported here at all: it imports `../broker-wasm-pkg/broker_bg.wasm`
// and calls `initSync` at module scope, and that package is a gitignored build
// artifact absent from a plain checkout (CI runs `npm ci && pnpm test`, never a
// wasm build). Parsing the declarations is what makes this assertion possible
// without a Miniflare/DO harness. Mirrors the same source-parsing guardrail
// pattern the client adapter tests use against Rust enums.

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(HERE, "../..");

const read = (relative) => readFileSync(resolve(REPO_ROOT, relative), "utf8");

/** The block between `header` and the first line that closes it at column 0+2. */
function declarationBody(source, header, closer) {
  const start = source.indexOf(header);
  assert.notEqual(start, -1, `expected to find \`${header}\``);
  const end = source.indexOf(closer, start);
  assert.notEqual(end, -1, `expected \`${header}\` to be closed by \`${closer}\``);
  return source.slice(start + header.length, end);
}

/** Strip `//` and `/* *\/` comments so field regexes can't match prose. */
function stripComments(body) {
  return body.replace(/\/\*[\s\S]*?\*\//g, "").replace(/\/\/[^\n]*/g, "");
}

/** Top-level `name:` keys of a TS interface / object-literal body. */
function declaredFields(body) {
  return Array.from(stripComments(body).matchAll(/^\s{2}(\w+):/gm), (m) => m[1]);
}

/** Field names of a Rust struct body, in declaration order. */
function rustStructFields(body) {
  return Array.from(stripComments(body).matchAll(/^\s{4}pub (\w+):/gm), (m) => m[1]);
}

/** `DEFAULT_CONN`'s literal, re-read as real data (keys quoted, commas trimmed). */
function defaultConnValue() {
  const body = declarationBody(read("lobby-worker/src/lobby-do.ts"), "const DEFAULT_CONN = {", "};");
  const json = stripComments(body)
    .replace(/^(\s*)(\w+):/gm, '$1"$2":')
    .replace(/,(\s*)$/, "$1");
  return JSON.parse(`{${json}}`);
}

// The single expected shape. Hand-maintained, like the mirrors themselves —
// but asserted against BOTH mirrors and against the Rust original below, so no
// single-file revert can keep this green.
const EXPECTED_CONN_FIELDS = [
  "client_hello",
  "subscribed",
  "host_game",
  "reservations",
  "organized_tournaments",
  "joined_tournaments",
];

test("DEFAULT_CONN carries every ConnState field, tournament lists included", () => {
  const conn = defaultConnValue();
  assert.deepEqual(Object.keys(conn).sort(), [...EXPECTED_CONN_FIELDS].sort());
  // Values, not just presence: a fresh connection organizes and has joined
  // nothing, and that must be a real empty list rather than a missing key that
  // only survives because the Rust side defaults it.
  assert.deepEqual(conn, {
    client_hello: null,
    subscribed: false,
    host_game: null,
    reservations: [],
    organized_tournaments: [],
    joined_tournaments: [],
  });
});

test("ConnAttachment types every field DEFAULT_CONN sets", () => {
  const attachment = declaredFields(
    declarationBody(read("lobby-worker/src/hello-gate.ts"), "export interface ConnAttachment {", "\n}"),
  );
  assert.deepEqual(attachment.sort(), [...EXPECTED_CONN_FIELDS].sort());
  // The two shells' mirrors must agree with each other, not merely each with
  // the expectation — casting a DEFAULT_CONN-shaped attachment to
  // ConnAttachment is exactly what lobby-do.ts does on every frame.
  assert.deepEqual(attachment.sort(), Object.keys(defaultConnValue()).sort());
});

test("both mirrors match lobby_broker::ConnState's actual field set", () => {
  // The authority the mirrors exist to mirror. A seventh ConnState field fails
  // here the moment it lands, instead of drifting unnoticed as the tournament
  // pair did.
  const conn = rustStructFields(
    declarationBody(read("crates/lobby-broker/src/broker.rs"), "pub struct ConnState {", "\n}"),
  );
  assert.deepEqual(conn.sort(), [...EXPECTED_CONN_FIELDS].sort());
});
