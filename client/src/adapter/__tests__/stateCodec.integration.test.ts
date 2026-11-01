/**
 * Manual real-WASM proof; no workflow runs this suite. From the repo root:
 * STATE_CODEC_CORPUS=/tmp/loop186-corpus.json cargo test -p state-codec --test integration \
 *   portability::produce_native_portability_corpus -- --ignored --exact --nocapture
 * Build ./scripts/build-wasm.sh release (or use fresh Tilt wasm output) from the
 * SAME HEAD as the native producer, then run:
 * cd client && STATE_CODEC_CORPUS=/tmp/loop186-corpus.json pnpm exec vitest run \
 *   --config vitest.integration.config.ts --coverage.enabled=false \
 *   src/adapter/__tests__/stateCodec.integration.test.ts
 * STATE_CODEC_CORPUS must be absolute. Unset visibly skips; configured missing
 * inputs fail. Requires generated engine_wasm.js/engine_wasm_bg.wasm and the
 * committed integration_cards.json.gz. No generated card data or mocks.
 * Limits: 64 MiB corpus, 64 accepted actions, five checkpoints/four audiences.
 */
import { existsSync, statSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { isAbsolute, resolve } from "node:path";
import { gunzipSync } from "node:zlib";

import { beforeAll, describe, expect, it } from "vitest";

import type { StateCodecHandle } from "@wasm/engine";
import type * as WasmEngine from "@wasm/engine";

type ExactRow = { json: string; hash: string; revision?: string };
type Checkpoint = {
  milestone: string;
  after_actions: number;
  audiences: (ExactRow & { viewer: number | null; revision: string })[];
};
type Corpus = {
  schema_version: number;
  header: {
    deck_data: unknown;
    seed: number;
    format_config: unknown;
    match_config: unknown;
    player_count: number;
    first_player: number;
  };
  actions: { actor: number; action: unknown }[];
  checkpoints: Checkpoint[];
  lexical_rows: (ExactRow & { name: string })[];
  ownership_rows: (ExactRow & { name: string; revision: string })[];
};

const CORPUS_PATH = process.env.STATE_CODEC_CORPUS;
const ENABLED = CORPUS_PATH !== undefined;
const MAX_BYTES = 64 * 1024 * 1024;
const WASM_JS = resolve(__dirname, "../../wasm/engine_wasm.js");
const WASM_BINARY = resolve(__dirname, "../../wasm/engine_wasm_bg.wasm");
const FIXTURE = resolve(
  __dirname,
  "../../../../crates/engine/tests/fixtures/integration_cards.json.gz",
);
const MILESTONES = [
  "initial",
  "post_mulligan",
  "land_seat_0",
  "land_seat_1",
  "final",
];
const LEXICAL_ROWS = [
  ["nested_ordering", '{"z":{"y":2,"x":1},"a":0}'],
  ["u64", '{"u64":18446744073709551615}'],
  ["u128", '{"u128":340282366920938463463374607431768211455}'],
  ["unicode", '{"unicode":"Éowyn 雪 🧙 \\u0061"}'],
  ["whitespace", '{ "z" : [ 1, 2 ], "a" : { "y":0, "x":1 } }'],
] as const;
const utf8 = new TextEncoder();

function assertExact(handle: StateCodecHandle, row: ExactRow, context: string) {
  expect(handle.json_bytes(), `${context}: UTF-8 bytes`).toEqual(utf8.encode(row.json));
  expect(handle.json(), `${context}: text`).toBe(row.json);
  expect(handle.hash(), `${context}: native SHA-256`).toBe(row.hash);
  if (row.revision !== undefined) {
    expect(handle.revision(), `${context}: revision`).toBe(BigInt(row.revision));
  }
}

/**
 * Proof lines go straight to stdout, not through `console`. In agent harnesses
 * Vitest auto-selects its "agent" reporter (`silent: "passed-only"`), which
 * drops intercepted console output for every non-failed test — including a
 * wholly skipped file, so an unset gate would print nothing at all.
 */
function report(line: string): void {
  process.stdout.write(`${line}\n`);
}

if (!ENABLED) {
  report(
    "stateCodec portability skipped: STATE_CODEC_CORPUS is unset. Produce an absolute-path corpus, build same-HEAD WASM, and run the manual command in this file's header.",
  );
}

describe.skipIf(!ENABLED)("stateCodec — native / real WASM portability", () => {
  let wasm: typeof WasmEngine;
  let corpus: Corpus;

  beforeAll(async () => {
    if (!CORPUS_PATH || !isAbsolute(CORPUS_PATH)) {
      throw new Error("STATE_CODEC_CORPUS must be a nonempty absolute path");
    }
    for (const path of [CORPUS_PATH, WASM_JS, WASM_BINARY, FIXTURE]) {
      if (!existsSync(path)) throw new Error(`Required portability input missing: ${path}`);
      if (!statSync(path).isFile()) throw new Error(`Required portability input is not a file: ${path}`);
    }
    expect(statSync(CORPUS_PATH).size, "corpus byte cap").toBeLessThanOrEqual(MAX_BYTES);
    corpus = JSON.parse(await readFile(CORPUS_PATH, "utf8")) as Corpus;
    expect(corpus.schema_version).toBe(1);
    expect(corpus.actions.length).toBeGreaterThanOrEqual(4);
    expect(corpus.actions.length).toBeLessThanOrEqual(64);
    expect(corpus.checkpoints.map((row) => row.milestone)).toEqual(MILESTONES);
    expect(corpus.checkpoints[0].after_actions).toBe(0);
    const lastCheckpoint = corpus.checkpoints[corpus.checkpoints.length - 1];
    expect(lastCheckpoint?.after_actions).toBe(corpus.actions.length);
    for (const [index, checkpoint] of corpus.checkpoints.entries()) {
      expect(Number.isInteger(checkpoint.after_actions)).toBe(true);
      if (index > 0) {
        expect(checkpoint.after_actions).toBeGreaterThan(corpus.checkpoints[index - 1].after_actions);
      }
      expect(checkpoint.audiences.map((row) => row.viewer)).toEqual([0, 1, 255, null]);
      expect(checkpoint.audiences[0].json, "private seat projections differ").not.toBe(checkpoint.audiences[1].json);
    }
    expect(corpus.lexical_rows.map((row) => [row.name, row.json])).toEqual(LEXICAL_ROWS);
    expect(corpus.ownership_rows.map((row) => row.name)).toEqual(["base", "current"]);
    expect(corpus.ownership_rows.map((row) => row.revision)).toEqual([
      "9007199254740993", "9007199254740994",
    ]);
    // Static loading would break the unset gate when generated WASM is absent.
    wasm = await import("@wasm/engine");
    const module = await WebAssembly.compile(await readFile(WASM_BINARY));
    await wasm.default({ module_or_path: module });
    expect(wasm.load_card_database(gunzipSync(await readFile(FIXTURE)).toString("utf8"))).toBeGreaterThan(0);
  }, 300_000);

  it("independently replays accepted actions across every milestone and audience", () => {
    const header = corpus.header;
    const initial = wasm.initialize_game(
      header.deck_data, header.seed, header.format_config, header.match_config,
      header.player_count, header.first_player,
    ) as { events?: unknown[]; waiting_for?: { type: string }; disposition?: string };
    expect(initial.events).toBeInstanceOf(Array);
    expect(initial.waiting_for?.type).toBe("MulliganDecision");
    expect(initial.disposition ?? "Applied").toBe("Applied");
    let checkpointIndex = 0;
    let comparisons = 0;
    const compareCheckpoint = (afterActions: number) => {
      const checkpoint = corpus.checkpoints[checkpointIndex];
      if (checkpoint?.after_actions !== afterActions) return;
      for (const row of checkpoint.audiences) {
        const context = `${checkpoint.milestone} action=${afterActions} viewer=${row.viewer ?? "unseated"}`;
        const handle = wasm.state_codec_for_viewer(row.viewer ?? undefined);
        try {
          assertExact(handle, row, context);
          comparisons++;
        } finally {
          handle.free();
        }
      }
      checkpointIndex++;
    };
    compareCheckpoint(0);
    for (const [index, record] of corpus.actions.entries()) {
      const outcome = wasm.submit_action(record.actor, record.action) as {
        status?: string;
        result?: { disposition?: string; waiting_for?: { type: string } };
      };
      expect(outcome.status, `action=${index + 1} actor=${record.actor}`).toBe("applied");
      expect(outcome.result, `action=${index + 1} applied result`).toBeDefined();
      expect(outcome.result?.disposition ?? "Applied").toBe("Applied");
      if (corpus.checkpoints.some((row) => row.after_actions === index + 1 && row.milestone !== "initial")) {
        expect(outcome.result?.waiting_for?.type, `checkpoint action=${index + 1}`).toBe("Priority");
      }
      compareCheckpoint(index + 1);
    }
    expect(checkpointIndex).toBe(5);
    expect(comparisons).toBe(20);
    report(`portability replay: accepted_actions=${corpus.actions.length} checkpoints=${checkpointIndex} audience_comparisons=${comparisons}`);
  }, 300_000);

  it("preserves all five lexical literals through FULL encode and decode", () => {
    for (const row of corpus.lexical_rows) {
      const handle = wasm.state_codec_from_json(utf8.encode(row.json));
      try {
        assertExact(handle, row, row.name);
        const frame = wasm.state_codec_encode(handle);
        const decoded = wasm.state_codec_decode(frame);
        try {
          assertExact(decoded, row, `${row.name} FULL round trip`);
        } finally {
          decoded.free();
        }
      } finally {
        handle.free();
      }
    }
    report("portability lexical: five exact literal/FULL rows passed");
  });

  it("detects an altered byte with the same exact-row comparator", () => {
    const original = corpus.lexical_rows[0];
    const altered = original.json.replace('"y":2', '"y":3');
    const control = wasm.state_codec_from_json(utf8.encode(original.json));
    const candidate = wasm.state_codec_from_json(utf8.encode(altered));
    try {
      assertExact(control, original, "altered-byte positive control");
      expect(candidate.json()).toBe(altered);
      expect(candidate.json_bytes()).toEqual(utf8.encode(altered));
      expect(() => assertExact(candidate, original, "altered byte")).toThrow();
      expect(candidate.hash()).not.toBe(original.hash);
    } finally {
      control.free();
      candidate.free();
    }
    report("portability mutation: altered byte detected");
  });

  it("detects semantically equal reordered keys without normalizing the comparison", () => {
    const original = corpus.lexical_rows[0];
    const reordered = '{"a":0,"z":{"x":1,"y":2}}';
    expect(JSON.parse(reordered)).toEqual(JSON.parse(original.json));
    const control = wasm.state_codec_from_json(utf8.encode(original.json));
    const candidate = wasm.state_codec_from_json(utf8.encode(reordered));
    try {
      assertExact(control, original, "key-order positive control");
      expect(candidate.json()).toBe(reordered);
      expect(candidate.json_bytes()).toEqual(utf8.encode(reordered));
      expect(() => assertExact(candidate, original, "reordered keys")).toThrow();
      expect(candidate.hash()).not.toBe(original.hash);
    } finally {
      control.free();
      candidate.free();
    }
    report("portability mutation: normalized ordering detected");
  });

  it("consumes real baseline handles on DELTA success and decode error, retaining clones", () => {
    const [baseRow, currentRow] = corpus.ownership_rows;
    const revisioned = (row: typeof baseRow) => {
      const raw = wasm.state_codec_from_json(utf8.encode(row.json));
      try {
        return raw.with_revision(BigInt(row.revision));
      } finally {
        raw.free();
      }
    };
    const base = revisioned(baseRow);
    const current = revisioned(currentRow);
    try {
      assertExact(base, baseRow, "retained base before transfer");
      assertExact(current, currentRow, "borrowed current before transfer");
      const encodeTransfer = base.clone_handle();
      assertExact(encodeTransfer, baseRow, "encode transfer readable before call");
      const delta = wasm.state_codec_encode(current, encodeTransfer);
      const full = wasm.state_codec_encode(current);
      const metadata: { type: string } = JSON.parse(new TextDecoder().decode(delta));
      expect(metadata.type).toBe("Delta");
      expect(delta.byteLength).toBeLessThan(full.byteLength);
      expect(() => encodeTransfer.json_bytes()).toThrow();
      assertExact(base, baseRow, "retained base after encode");
      assertExact(current, currentRow, "borrowed current after encode");

      const decodeTransfer = base.clone_handle();
      assertExact(decodeTransfer, baseRow, "decode transfer readable before call");
      const decoded = wasm.state_codec_decode(delta, decodeTransfer);
      try {
        assertExact(decoded, currentRow, "DELTA decode");
        expect(() => decodeTransfer.json_bytes()).toThrow();
      } finally {
        decoded.free();
      }
      assertExact(base, baseRow, "retained base after decode");
      assertExact(current, currentRow, "borrowed current after decode");

      const errorTransfer = base.clone_handle();
      assertExact(errorTransfer, baseRow, "error transfer readable before call");
      expect(() => wasm.state_codec_decode(delta.subarray(0, delta.length - 1), errorTransfer)).toThrow();
      expect(() => errorTransfer.json_bytes()).toThrow();
      assertExact(base, baseRow, "retained base after decode error");
      assertExact(current, currentRow, "borrowed current after decode error");
    } finally {
      base.free();
      current.free();
    }
    report("portability ownership: real DELTA encode/decode consumption and decode-error consumption passed");
  });
});
