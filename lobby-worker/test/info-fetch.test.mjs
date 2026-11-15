import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import http from "node:http";
import { dirname, resolve } from "node:path";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";

import { Miniflare } from "miniflare";

// `fetchInfoDocument` (lobby-do.ts) is the announce handler's verifier: it
// fetches the announced host's own `/info` and every non-`ok` outcome becomes
// a 422 `info_unreachable`. Its contract is entirely about how the WORKERS
// runtime answers a `fetch` — a redirect option Node accepts and workerd
// rejects reads as "every server on earth is unreachable" — so this runs the
// real method body inside workerd rather than under Node's fetch.
//
// The body is read as SOURCE TEXT and re-hosted in a Miniflare worker for the
// same reason `conn-attachment.test.mjs` parses its declarations: `lobby-do.ts`
// imports `../broker-wasm-pkg/broker_bg.wasm` and calls `initSync` at module
// scope, and that package is a gitignored build artifact absent from the plain
// checkout CI tests (`npm ci && pnpm test`, never a wasm build).

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(HERE, "../..");
const SOURCE = readFileSync(resolve(REPO_ROOT, "lobby-worker/src/lobby-do.ts"), "utf8");

// Deliberately unlike production's 3 s / 4096: a body that reached for a
// literal instead of these would hang the timeout row for three seconds and
// stop discriminating the over-cap row.
const TIMEOUT_MS = 250;
const MAX_BYTES = 512;

/** The body between `header`'s opening `{` and its brace-depth-matched close. */
function methodBody(header) {
  const start = SOURCE.indexOf(header);
  assert.notEqual(start, -1, `expected to find \`${header}\``);
  const bodyStart = SOURCE.indexOf("{", start);
  let depth = 0;
  for (let index = bodyStart; index < SOURCE.length; index += 1) {
    if (SOURCE[index] === "{") depth += 1;
    if (SOURCE[index] === "}") {
      depth -= 1;
      if (depth === 0) return SOURCE.slice(bodyStart + 1, index);
    }
  }
  throw new Error(`expected \`${header}\` to be closed by a matching brace`);
}

const WORKER = `
const INFO_FETCH_TIMEOUT_MS = ${TIMEOUT_MS};
const MAX_INFO_BYTES = ${MAX_BYTES};

// Stand-in for directory.ts's reader, whose own bound behaviour is pinned by
// directory.test.mjs. Same contract: the text, or null once the cap is passed.
async function readBoundedText(body, max) {
  const text = await new Response(body).text();
  return text.length > max ? null : text;
}

async function fetchInfoDocument(infoUrl) {${methodBody("private async fetchInfoDocument(infoUrl: string): Promise<InfoFetch> {")}}

export default {
  async fetch(request) {
    const target = new URL(request.url).searchParams.get("u");
    return Response.json(await fetchInfoDocument(target));
  },
};
`;

const INFO_BODY = JSON.stringify({ mode: "Full", server_version: "0.74.0" });

let mf;
let origin;
let base;
/** Paths the origin actually served, so "unreachable" can be told apart from
 *  "never asked". */
const hits = [];

before(async () => {
  origin = http.createServer((request, response) => {
    const path = request.url;
    hits.push(path);
    if (path === "/info") {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(INFO_BODY);
    } else if (path === "/moved") {
      response.writeHead(302, { location: "/info" });
      response.end();
    } else if (path === "/huge") {
      response.writeHead(200, { "content-type": "application/json" });
      response.end("x".repeat(MAX_BYTES + 1));
    } else if (path === "/slow") {
      // Held open with no bytes and no close: only the fetch's own timeout
      // ends this one.
    } else {
      response.writeHead(404);
      response.end("no such document");
    }
  });
  await new Promise((ready) => origin.listen(0, "127.0.0.1", ready));
  base = `http://127.0.0.1:${origin.address().port}`;
  mf = new Miniflare({ compatibilityDate: "2026-05-01", modules: true, script: WORKER });
  await mf.ready;
});

after(async () => {
  await mf?.dispose();
  origin?.closeAllConnections();
  await new Promise((done) => origin.close(done));
});

const probe = async (path) => {
  const response = await mf.dispatchFetch(`http://verifier/?u=${encodeURIComponent(base + path)}`);
  return response.json();
};

test("a 200 info document is read back verbatim", async () => {
  assert.deepEqual(await probe("/info"), { kind: "ok", text: INFO_BODY });
});

test("a redirect is unreachable and is not followed", async () => {
  const before = hits.length;
  assert.deepEqual(await probe("/moved"), { kind: "unreachable" });
  // The redirect target is the same origin, so a follower would have returned
  // `ok` above; this pins that the second request never happened either.
  assert.deepEqual(hits.slice(before), ["/moved"]);
});

test("a 404 is unreachable", async () => {
  assert.deepEqual(await probe("/gone"), { kind: "unreachable" });
});

test("a host that never answers is unreachable, not a hang", async () => {
  const before = hits.length;
  const started = Date.now();
  assert.deepEqual(await probe("/slow"), { kind: "unreachable" });
  // Reached the origin and was ended by the fetch's own timeout — otherwise
  // this row would pass for a host that simply refused the connection.
  assert.deepEqual(hits.slice(before), ["/slow"]);
  assert.ok(Date.now() - started >= TIMEOUT_MS, "returned before the timeout could fire");
});

test("an over-cap body is too_large, not unreachable", async () => {
  assert.deepEqual(await probe("/huge"), { kind: "too_large" });
});
