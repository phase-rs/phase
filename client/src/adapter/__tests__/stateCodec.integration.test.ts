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

import { beforeAll, describe, expect, it, vi } from "vitest";

import type { StateCodecHandle } from "@wasm/engine";
import type * as WasmEngine from "@wasm/engine";
import { StateCodecRuntime } from "../state-codec-runtime";
import { readGameStartedFullCredentials } from "../../services/stateCodecSocket";
import { PROTOCOL_VERSION, WebSocketAdapter } from "../ws-adapter";
import { ServerDraftAdapter } from "../server-draft-adapter";
import type { GameState } from "../types";

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

  // ── Negotiated StateCodecV1 transport over the real codec ─────────────
  //
  // The corpus supplies exact filtered viewer-state JSON. These rows wrap it
  // in the complete server envelopes the transport really delivers and drive
  // the production runtime helper and both production adapters, so emitted
  // application state is compared with the decoded envelope itself.

  const FULL_KEY = { game_code: "GAME01", generation: 1 };

  function gameStartedEnvelope(viewerState: string, revision: number) {
    return {
      type: "GameStarted" as const,
      data: {
        state_revision: revision,
        state: JSON.parse(viewerState) as GameState,
        your_player: 0,
        player_token: "integration-token",
        full_key: FULL_KEY,
        legal_actions: [{ type: "PassPriority" }],
        auto_pass_recommended: false,
        events: [],
        rewind_targets: [],
      },
    };
  }

  /**
   * `command_id` is a top-level envelope key (the adapters match
   * `msg.command_id`), so it goes in `topLevel`; `dataExtra` extends `data`.
   */
  function stateUpdateEnvelope(
    viewerState: string,
    revision: number,
    topLevel: Record<string, unknown> = {},
    dataExtra: Record<string, unknown> = {},
  ) {
    return {
      type: "StateUpdate" as const,
      ...topLevel,
      data: {
        state_revision: revision,
        state: JSON.parse(viewerState) as GameState,
        events: [{ type: "TurnStarted" }],
        legal_actions: [{ type: "PassPriority" }],
        full_key: FULL_KEY,
        ...dataExtra,
      },
    };
  }

  /** The viewer state with only its turn number advanced: a small, real diff. */
  function viewerVariant(viewerJson: string, step: number): string {
    const state = JSON.parse(viewerJson) as GameState;
    return JSON.stringify({ ...state, turn_number: (state.turn_number ?? 0) + step });
  }

  function revisionedHandle(text: string, revision: bigint) {
    const raw = wasm.state_codec_from_json(utf8.encode(text));
    try {
      return raw.with_revision(revision);
    } finally {
      raw.free();
    }
  }

  function fullFrame(text: string, revision: bigint): Uint8Array {
    const handle = revisionedHandle(text, revision);
    try {
      return wasm.state_codec_encode(handle);
    } finally {
      handle.free();
    }
  }

  /**
   * What the real server encoder emits for `text` after `baseText` was
   * delivered: a Delta only when the baseline is a StateUpdate and the diff
   * is smaller than a Full. A GameStarted baseline is FullOnly.
   */
  function encodeAfter(text: string, revision: bigint, baseText: string, baseRevision: bigint): Uint8Array {
    const handle = revisionedHandle(text, revision);
    const base = revisionedHandle(baseText, baseRevision);
    try {
      return wasm.state_codec_encode(handle, base);
    } finally {
      handle.free();
    }
  }

  const frameText = (bytes: Uint8Array) => new TextDecoder().decode(bytes);
  const frameType = (bytes: Uint8Array) => (JSON.parse(frameText(bytes)) as { type: string }).type;

  /**
   * The real stream order: GameStarted Full -> StateUpdate Full -> StateUpdate
   * Delta. Every kind is asserted, so a codec that stops producing a Delta
   * fails here instead of silently weakening the rows that depend on one.
   */
  function realStream(viewerJson: string) {
    const startedText = JSON.stringify(gameStartedEnvelope(viewerVariant(viewerJson, 0), 1));
    const updateText = JSON.stringify(stateUpdateEnvelope(viewerVariant(viewerJson, 1), 2));
    const nextText = JSON.stringify(stateUpdateEnvelope(viewerVariant(viewerJson, 2), 3));
    const started = fullFrame(startedText, 1n);
    const update = encodeAfter(updateText, 2n, startedText, 1n);
    const delta = encodeAfter(nextText, 3n, updateText, 2n);
    expect(frameType(started)).toBe("Full");
    expect(frameType(update), "a GameStarted baseline is FullOnly").toBe("Full");
    expect(frameType(delta), "StateUpdate after StateUpdate").toBe("Delta");
    return { startedText, updateText, nextText, started, update, delta };
  }

  /** A Delta with one `data` field changed; the original must be a real Delta. */
  function tamperDelta(frame: Uint8Array, mutate: (data: Record<string, unknown>) => void): Uint8Array {
    const parsed = JSON.parse(frameText(frame)) as { type: string; data: Record<string, unknown> };
    expect(parsed.type).toBe("Delta");
    mutate(parsed.data);
    return utf8.encode(JSON.stringify(parsed));
  }

  const flipFirst = (hash: unknown) => {
    const text = String(hash);
    return `${text[0] === "0" ? "1" : "0"}${text.slice(1)}`;
  };

  const baselineJson = () => corpus.checkpoints[0].audiences[0].json;

  it("reconstructs complete server envelopes exactly through the runtime helper", () => {
    const stream = realStream(baselineJson());
    const runtime = new StateCodecRuntime(wasm);
    const session = runtime.open();
    try {
      const decodedStarted = runtime.decode(session, stream.started);
      expect(decodedStarted).toEqual({ kind: "decoded", json: stream.startedText });
      // The transport observes text only: revisions stay inside WASM.
      expect(Object.keys(decodedStarted).sort()).toEqual(["json", "kind"]);
      expect(runtime.decode(session, stream.update)).toEqual({ kind: "decoded", json: stream.updateText });
      const decodedDelta = runtime.decode(session, stream.delta);
      expect(decodedDelta).toEqual({ kind: "decoded", json: stream.nextText });
      expect(JSON.parse((decodedDelta as { json: string }).json)).toEqual(JSON.parse(stream.nextText));
    } finally {
      runtime.dispose();
    }
  });

  it("reconstructs a deleted top-level command_id exactly", () => {
    const viewer = baselineJson();
    const startedText = JSON.stringify(gameStartedEnvelope(viewerVariant(viewer, 0), 1));
    const withId = JSON.stringify(stateUpdateEnvelope(viewerVariant(viewer, 1), 2, { command_id: 7 }));
    const withoutId = JSON.stringify(stateUpdateEnvelope(viewerVariant(viewer, 2), 3));
    expect(JSON.parse(withId)).toHaveProperty("command_id", 7);
    expect(JSON.parse(withoutId)).not.toHaveProperty("command_id");
    const delta = encodeAfter(withoutId, 3n, withId, 2n);
    expect(frameType(delta)).toBe("Delta");
    // Reach guard: the encoder expressed the deletion as a removal patch.
    expect((JSON.parse(frameText(delta)) as {
      data: { patch: { message: { remove: string[] } } };
    }).data.patch.message.remove).toEqual(["command_id"]);

    const runtime = new StateCodecRuntime(wasm);
    const session = runtime.open();
    try {
      runtime.decode(session, fullFrame(startedText, 1n));
      expect(runtime.decode(session, encodeAfter(withId, 2n, startedText, 1n)))
        .toEqual({ kind: "decoded", json: withId });
      const decoded = runtime.decode(session, delta);
      expect(decoded).toEqual({ kind: "decoded", json: withoutId });
      expect(JSON.parse((decoded as { json: string }).json)).not.toHaveProperty("command_id");
    } finally {
      runtime.dispose();
    }
  });

  it("decodes a delta whose codec revisions exceed 2^53 without JS numbers", () => {
    const [baseRow, currentRow] = corpus.ownership_rows;
    const baseEnvelope = stateUpdateEnvelope(baseRow.json, 1);
    const currentEnvelope = stateUpdateEnvelope(currentRow.json, 2);
    const baseText = JSON.stringify(baseEnvelope);
    const currentText = JSON.stringify(currentEnvelope);
    const delta = encodeAfter(currentText, BigInt(currentRow.revision), baseText, BigInt(baseRow.revision));
    expect(frameType(delta)).toBe("Delta");

    const runtime = new StateCodecRuntime(wasm);
    const session = runtime.open();
    try {
      expect(runtime.decode(session, fullFrame(baseText, BigInt(baseRow.revision))))
        .toEqual({ kind: "decoded", json: baseText });
      expect(runtime.decode(session, delta)).toEqual({ kind: "decoded", json: currentText });
    } finally {
      runtime.dispose();
    }
  });

  it("reports resyncRequired for a missing baseline and each corrupted integrity field", () => {
    const stream = realStream(baselineJson());
    const runtime = new StateCodecRuntime(wasm);
    const establish = () => {
      const session = runtime.open();
      expect(runtime.decode(session, stream.started)).toMatchObject({ kind: "decoded" });
      expect(runtime.decode(session, stream.update)).toMatchObject({ kind: "decoded" });
      return session;
    };
    try {
      // Positive control: the untampered Delta applies on its own baseline.
      const control = establish();
      expect(runtime.decode(control, stream.delta)).toEqual({ kind: "decoded", json: stream.nextText });

      // A Delta cannot establish a baseline.
      expect(runtime.decode(runtime.open(), stream.delta)).toEqual({ kind: "resyncRequired" });

      const tampers: [string, (data: Record<string, unknown>) => void][] = [
        ["result_hash", (data) => { data.result_hash = flipFirst(data.result_hash); }],
        ["base_hash", (data) => { data.base_hash = flipFirst(data.base_hash); }],
        ["base_revision", (data) => { data.base_revision = Number(data.base_revision) + 3; }],
      ];
      for (const [label, mutate] of tampers) {
        const session = establish();
        expect(runtime.decode(session, tamperDelta(stream.delta, mutate)), label)
          .toEqual({ kind: "resyncRequired" });
        // The rejected frame consumed the baseline: only a Full recovers it.
        expect(runtime.decode(session, stream.delta), `${label}: baseline dropped`)
          .toEqual({ kind: "resyncRequired" });
        expect(runtime.decode(session, stream.update), `${label}: Full recovers`)
          .toEqual({ kind: "decoded", json: stream.updateText });
        expect(runtime.decode(session, stream.delta), `${label}: Delta applies again`)
          .toEqual({ kind: "decoded", json: stream.nextText });
      }
    } finally {
      runtime.dispose();
    }
  });

  it("isolates baselines between sessions and never recreates a closed one", () => {
    const stream = realStream(baselineJson());
    const runtime = new StateCodecRuntime(wasm);
    try {
      const a = runtime.open();
      const b = runtime.open();
      for (const frame of [stream.started, stream.update]) {
        expect(runtime.decode(a, frame)).toMatchObject({ kind: "decoded" });
      }
      // B has no baseline: the same real Delta is rejected for B only.
      expect(runtime.decode(b, stream.delta)).toEqual({ kind: "resyncRequired" });
      expect(runtime.decode(a, stream.delta)).toEqual({ kind: "decoded", json: stream.nextText });

      // Closing A leaves B usable; A's id is never implicitly reopened.
      runtime.close(a);
      for (const frame of [stream.started, stream.update]) {
        expect(runtime.decode(b, frame)).toMatchObject({ kind: "decoded" });
      }
      expect(runtime.decode(b, stream.delta)).toEqual({ kind: "decoded", json: stream.nextText });
      expect(() => runtime.decode(a, stream.started)).toThrow();
    } finally {
      runtime.dispose();
    }
  });

  // ── Production adapter parity over the same frames ────────────────────

  class IntegrationSocket extends EventTarget {
    static OPEN = 1;
    static last: IntegrationSocket | null = null;
    readyState = IntegrationSocket.OPEN;
    binaryType: BinaryType = "blob";
    onopen: (() => void) | null = null;
    onmessage: ((event: { data: string }) => void) | null = null;
    onerror: (() => void) | null = null;
    onclose: (() => void) | null = null;
    readonly sent: string[] = [];
    send = vi.fn((data: string) => {
      this.sent.push(data);
    });
    close = vi.fn(() => {
      this.onclose?.();
      this.dispatchEvent(new Event("close"));
    });
    constructor(public url: string) {
      super();
      IntegrationSocket.last = this;
    }
    deliver(text: string): void {
      this.onmessage?.({ data: text });
    }
  }

  const SERVER_HELLO = JSON.stringify({
    type: "ServerHello",
    data: {
      server_version: "0.0.0-test",
      build_commit: "integration",
      protocol_version: PROTOCOL_VERSION,
      mode: "Full",
      command_correlation: "CommandIdV1",
      state_codecs: ["StateCodecV1"],
    },
  });
  const CONFIRMATION = JSON.stringify({
    type: "ServerHello",
    data: { server_version: "0.0.0-test", selected_state_codec: "StateCodecV1" },
  });

  interface SessionStats {
    decodes: number;
    closes: number;
  }

  /** A provider over this file's real module, i.e. real Rust reconstruction. */
  function realSessionProvider(runtime: StateCodecRuntime, sessions: SessionStats[] = []) {
    return () => {
      const sessionId = runtime.open();
      const stats: SessionStats = { decodes: 0, closes: 0 };
      sessions.push(stats);
      return Promise.resolve({
        decode: async (frame: Uint8Array) => {
          stats.decodes++;
          return runtime.decode(sessionId, frame);
        },
        close: () => {
          stats.closes++;
          runtime.close(sessionId);
        },
      });
    };
  }

  function sentTypes(socket: IntegrationSocket): string[] {
    return socket.sent.map((raw) => (JSON.parse(raw) as { type?: string }).type ?? "");
  }

  const resyncCount = (socket: IntegrationSocket) =>
    sentTypes(socket).filter((type) => type === "ResyncState").length;

  /** Hello, setup-frame wait, then the codec confirmation, on the next socket. */
  async function handshakeSocket(
    setupType: string,
    previous: IntegrationSocket | null = null,
  ): Promise<IntegrationSocket> {
    await vi.waitFor(() => expect(IntegrationSocket.last).not.toBe(previous));
    const socket = IntegrationSocket.last!;
    socket.deliver(SERVER_HELLO);
    await vi.waitFor(() => expect(sentTypes(socket)).toContain(setupType));
    socket.deliver(CONFIRMATION);
    return socket;
  }

  type Observed = { type: string; snapshot?: { state: GameState }; state?: GameState };

  function newNormalAdapter(runtime: StateCodecRuntime, sessions: SessionStats[] = []) {
    vi.stubGlobal("WebSocket", IntegrationSocket);
    IntegrationSocket.last = null;
    const adapter = new WebSocketAdapter(
      "ws://localhost:9374/ws",
      "join",
      { main_deck: [], sideboard: [] },
      "GAME01",
      undefined,
      undefined,
      undefined,
      { stateCodecSessionProvider: realSessionProvider(runtime, sessions) },
    );
    const events: Observed[] = [];
    adapter.onEvent((event) => events.push(event as unknown as Observed));
    return { adapter, events };
  }

  const stateChanges = (events: Observed[]) =>
    events.filter((event) => event.type === "stateChanged").map((event) => event.snapshot!.state);

  /** A joined normal stream: GameStarted Full accepted, then the first StateUpdate Full. */
  async function establishedNormal(runtime: StateCodecRuntime, stream: ReturnType<typeof realStream>) {
    const sessions: SessionStats[] = [];
    const { adapter, events } = newNormalAdapter(runtime, sessions);
    const init = adapter.initialize();
    const socket = await handshakeSocket("JoinGameWithPassword");
    socket.deliver(frameText(stream.started));
    await init;
    // The initializing GameStarted is read through getSnapshot, not stateChanged.
    expect(stateChanges(events)).toEqual([]);
    expect((await adapter.getSnapshot()).state).toEqual(JSON.parse(stream.startedText).data.state);
    socket.deliver(frameText(stream.update));
    await vi.waitFor(() => expect(stateChanges(events)).toHaveLength(1));
    return { adapter, events, socket, sessions };
  }

  it("normal and draft adapters emit the same state from one real codec stream", async () => {
    vi.stubGlobal("WebSocket", IntegrationSocket);
    const stream = realStream(baselineJson());
    const started = JSON.parse(stream.startedText) as ReturnType<typeof gameStartedEnvelope>;
    const update = JSON.parse(stream.updateText) as ReturnType<typeof stateUpdateEnvelope>;
    const next = JSON.parse(stream.nextText) as ReturnType<typeof stateUpdateEnvelope>;

    // ── Normal adapter ──
    const normalRuntime = new StateCodecRuntime(wasm);
    const { adapter: normal, events: normalEvents } = newNormalAdapter(normalRuntime);
    const normalInit = normal.initialize();
    const normalSocket = await handshakeSocket("JoinGameWithPassword");
    normalSocket.deliver(frameText(stream.started));
    await normalInit;
    expect((await normal.getSnapshot()).state).toEqual(started.data.state);
    normalSocket.deliver(frameText(stream.update));
    normalSocket.deliver(frameText(stream.delta));
    await vi.waitFor(() => expect(stateChanges(normalEvents)).toHaveLength(2));
    const normalStates = stateChanges(normalEvents);
    expect(normalStates[0]).toEqual(update.data.state);
    expect(normalStates[1]).toEqual(next.data.state);
    expect(resyncCount(normalSocket)).toBe(0);

    // ── Draft adapter ──
    IntegrationSocket.last = null;
    const draftRuntime = new StateCodecRuntime(wasm);
    const draft = new ServerDraftAdapter("ws://localhost:9374/ws", {
      stateCodecSessionProvider: realSessionProvider(draftRuntime),
    });
    const draftStates: GameState[] = [];
    draft.onEvent((event) => {
      if (event.type === "gameStateUpdated") draftStates.push(event.state);
    });
    const created = draft.createDraft({
      displayName: "Integration",
      setCodes: ["MKM"],
      kind: "Premier",
      public: true,
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
      podSize: 8,
    });
    const draftSocket = await handshakeSocket("CreateDraftWithSettings");
    draftSocket.deliver(JSON.stringify({
      type: "DraftCreated",
      data: { draft_code: "INTEG1", player_token: "integration-draft", seat_index: 0 },
    }));
    await created;
    draftSocket.deliver(JSON.stringify({
      type: "DraftMatchStart",
      data: {
        match_id: "r1-t0",
        round: 1,
        game_code: FULL_KEY.game_code,
        full_key: FULL_KEY,
        player_token: "integration-draft-match",
        your_player: 0,
        opponent_name: "Opponent",
      },
    }));
    draftSocket.deliver(frameText(stream.started));
    draftSocket.deliver(frameText(stream.update));
    draftSocket.deliver(frameText(stream.delta));
    // The draft adapter also publishes the match's GameStarted state.
    await vi.waitFor(() => expect(draftStates).toHaveLength(3));
    expect(draftStates[0]).toEqual(started.data.state);
    expect(draftStates[1]).toEqual(update.data.state);
    expect(draftStates[2]).toEqual(next.data.state);
    expect(draftStates.slice(1)).toEqual(normalStates);
    expect(resyncCount(draftSocket)).toBe(0);

    normal.dispose();
    draft.dispose();
    normalRuntime.dispose();
    draftRuntime.dispose();
  });

  it("recovers a corrupted real delta through one ResyncState and a real Full", async () => {
    const stream = realStream(baselineJson());
    const runtime = new StateCodecRuntime(wasm);
    const { adapter, socket } = await establishedNormal(runtime, stream);
    const viewer = baselineJson();

    const pending = adapter.submitAction({ type: "PassPriority" } as never, 0);
    const outcome = pending.then(() => "resolved", (error: unknown) => error);
    // A real Delta with a corrupted result hash: Rust rejects it.
    socket.deliver(frameText(tamperDelta(stream.delta, (data) => {
      data.result_hash = flipFirst(data.result_hash);
    })));
    await expect(outcome).resolves.toMatchObject({ code: "WS_CLOSED", recoverable: true });
    expect(resyncCount(socket)).toBe(1);
    await expect(adapter.submitAction({ type: "PassPriority" } as never, 0))
      .rejects.toMatchObject({ code: "ACTION_NOT_SENT" });
    expect(socket.close).not.toHaveBeenCalled();

    // The server answers with a fresh Full at the current revision.
    const recovery = stateUpdateEnvelope(viewerVariant(viewer, 3), 4);
    const recoveryText = JSON.stringify(recovery);
    socket.deliver(frameText(fullFrame(recoveryText, 4n)));
    await vi.waitFor(async () => expect(await adapter.getState()).toEqual(recovery.data.state));

    // Command correlation survives: a new stamped command resolves normally.
    const resumed = adapter.submitAction({ type: "PassPriority" } as never, 0);
    await vi.waitFor(() =>
      expect(sentTypes(socket).filter((type) => type === "Action")).toHaveLength(2),
    );
    const actionFrame = socket.sent
      .filter((raw) => (JSON.parse(raw) as { type?: string }).type === "Action")
      .slice(-1)[0];
    const commandId = (JSON.parse(actionFrame) as { command_id: number }).command_id;
    const completionText = JSON.stringify(
      stateUpdateEnvelope(viewerVariant(viewer, 4), 5, { command_id: commandId }),
    );
    const completion = encodeAfter(completionText, 5n, recoveryText, 4n);
    expect(frameType(completion)).toBe("Delta");
    socket.deliver(frameText(completion));
    await expect(resumed).resolves.toBeDefined();
    expect(resyncCount(socket)).toBe(1);

    adapter.dispose();
    runtime.dispose();
  });

  it("retains the real wire baseline across a strictly stale normal update", async () => {
    const viewer = baselineJson();
    const startedText = JSON.stringify(gameStartedEnvelope(viewerVariant(viewer, 0), 10));
    const staleText = JSON.stringify(stateUpdateEnvelope(viewerVariant(viewer, 1), 9));
    const newerText = JSON.stringify(stateUpdateEnvelope(viewerVariant(viewer, 2), 11));
    const stale = encodeAfter(staleText, 9n, startedText, 10n);
    const newer = encodeAfter(newerText, 11n, staleText, 9n);
    expect(frameType(stale)).toBe("Full");
    expect(frameType(newer)).toBe("Delta");

    const runtime = new StateCodecRuntime(wasm);
    const sessions: SessionStats[] = [];
    const { adapter, events } = newNormalAdapter(runtime, sessions);
    const init = adapter.initialize();
    const socket = await handshakeSocket("JoinGameWithPassword");
    socket.deliver(frameText(fullFrame(startedText, 10n)));
    await init;

    // The stale Full decodes (reach) but is not applied; its Delta then needs it.
    socket.deliver(frameText(stale));
    socket.deliver(frameText(newer));
    await vi.waitFor(() => expect(stateChanges(events)).toHaveLength(1));
    expect(stateChanges(events)[0]).toEqual(JSON.parse(newerText).data.state);
    expect(sessions[0].decodes).toBe(3);
    expect(resyncCount(socket)).toBe(0);
    expect(socket.close).not.toHaveBeenCalled();
    expect(sessions[0].closes).toBe(0);
    adapter.dispose();
    runtime.dispose();
  });

  it("retires the real session on a wrong-key Full and fences its queued Delta", async () => {
    const stream = realStream(baselineJson());
    const viewer = baselineJson();
    const runtime = new StateCodecRuntime(wasm);
    const { adapter, events, socket, sessions } = await establishedNormal(runtime, stream);
    const shown = stateChanges(events).length;

    const wrong = { game_code: "GAME01", generation: 2 };
    const wrongText = JSON.stringify(stateUpdateEnvelope(viewerVariant(viewer, 5), 6, {}, { full_key: wrong }));
    const wrongNextText = JSON.stringify(stateUpdateEnvelope(viewerVariant(viewer, 6), 7, {}, { full_key: wrong }));
    const wrongNext = encodeAfter(wrongNextText, 7n, wrongText, 6n);
    expect(frameType(wrongNext)).toBe("Delta");
    socket.deliver(frameText(fullFrame(wrongText, 6n)));
    socket.deliver(frameText(wrongNext));

    await vi.waitFor(() => expect(sessions[0].closes).toBe(1));
    // started + update + the rejecting Full; the queued Delta is never decoded.
    expect(sessions[0].decodes).toBe(3);
    expect(events).toContainEqual(expect.objectContaining({ type: "error" }));
    expect(stateChanges(events)).toHaveLength(shown);
    expect(socket.close).toHaveBeenCalledOnce();
    expect(resyncCount(socket)).toBe(0);
    adapter.dispose();
    runtime.dispose();
  });

  it("keeps a draft's ignored wrong-key Full and dependent Delta on the real wire baseline", async () => {
    vi.stubGlobal("WebSocket", IntegrationSocket);
    IntegrationSocket.last = null;
    const stream = realStream(baselineJson());
    const viewer = baselineJson();
    const runtime = new StateCodecRuntime(wasm);
    const sessions: SessionStats[] = [];
    const draft = new ServerDraftAdapter("ws://localhost:9374/ws", {
      stateCodecSessionProvider: realSessionProvider(runtime, sessions),
    });
    const labels: GameState[] = [];
    draft.onEvent((event) => {
      if (event.type === "gameStateUpdated") labels.push(event.state);
    });
    const created = draft.createDraft({
      displayName: "Integration", setCodes: ["MKM"], kind: "Premier", public: true,
      tournamentFormat: "Swiss", podPolicy: "Competitive", podSize: 8,
    });
    const socket = await handshakeSocket("CreateDraftWithSettings");
    socket.deliver(JSON.stringify({
      type: "DraftCreated",
      data: { draft_code: "INTEG2", player_token: "integration-draft", seat_index: 0 },
    }));
    await created;
    socket.deliver(JSON.stringify({
      type: "DraftMatchStart",
      data: {
        match_id: "r1-t0", round: 1, game_code: FULL_KEY.game_code, full_key: FULL_KEY,
        player_token: "integration-draft-match", your_player: 0, opponent_name: "Opponent",
      },
    }));
    socket.deliver(frameText(stream.started));
    socket.deliver(frameText(stream.update));
    await vi.waitFor(() => expect(labels).toHaveLength(2));

    // A wrong-key Full, then a Delta that depends on it (same rejected key):
    // both decode, neither is emitted, and no resync or close results.
    const wrong = { game_code: "GAME01", generation: 2 };
    const wrongText = JSON.stringify(stateUpdateEnvelope(viewerVariant(viewer, 5), 6, {}, { full_key: wrong }));
    const wrongNextText = JSON.stringify(stateUpdateEnvelope(viewerVariant(viewer, 6), 7, {}, { full_key: wrong }));
    const wrongNext = encodeAfter(wrongNextText, 7n, wrongText, 6n);
    expect(frameType(wrongNext)).toBe("Delta");
    socket.deliver(frameText(fullFrame(wrongText, 6n)));
    socket.deliver(frameText(wrongNext));

    // Restoration uses a matching-key Full (the server forces Full across keys),
    // then a matching-key Delta: the emitted states prove both decoded.
    const goodText = JSON.stringify(stateUpdateEnvelope(viewerVariant(viewer, 7), 8));
    const goodNextText = JSON.stringify(stateUpdateEnvelope(viewerVariant(viewer, 8), 9));
    const goodNext = encodeAfter(goodNextText, 9n, goodText, 8n);
    expect(frameType(goodNext)).toBe("Delta");
    socket.deliver(frameText(fullFrame(goodText, 8n)));
    socket.deliver(frameText(goodNext));
    await vi.waitFor(() => expect(labels).toHaveLength(4));
    expect(labels[2]).toEqual(JSON.parse(goodText).data.state);
    expect(labels[3]).toEqual(JSON.parse(goodNextText).data.state);
    expect(sessions[0].decodes).toBe(6);
    expect(sessions[0].closes).toBe(0);
    expect(resyncCount(socket)).toBe(0);
    expect(socket.close).not.toHaveBeenCalled();
    draft.dispose();
    runtime.dispose();
  });

  it("restarts a reconnect on an empty real session: initial Delta fails closed, then snapshot-only play is restored from plain frames", async () => {
    vi.useFakeTimers();
    const stream = realStream(baselineJson());
    const viewer = baselineJson();
    const runtime = new StateCodecRuntime(wasm);
    try {
      const { adapter, events, socket: first, sessions } = await establishedNormal(runtime, stream);
      expect(sessions).toHaveLength(1);
      // Lose the connection: the adapter reconnects with a brand-new session.
      first.onclose?.();
      await vi.advanceTimersByTimeAsync(1000);
      const second = await handshakeSocket("Reconnect", first);
      expect(sessions).toHaveLength(2);
      expect(sessions[0].closes).toBe(1);

      // A Delta with no baseline in the new session: the real codec rejects it
      // and, with no bootstrap yet, the attachment fails closed after one resync.
      second.deliver(frameText(stream.delta));
      await vi.waitFor(() => expect(sessions[1].closes).toBe(1));
      expect(resyncCount(second)).toBe(1);
      expect(second.close).toHaveBeenCalled();

      // The real bootstrap rejection is a terminal codec reason: this adapter's
      // next attempt is snapshot-only. It does not open a third decoder session,
      // advertises no codec, and restores play from plain GameStarted and
      // StateUpdate frames, which are exactly the text the codec would decode to.
      await vi.advanceTimersByTimeAsync(5000);
      await vi.waitFor(() => expect(IntegrationSocket.last).not.toBe(second));
      const third = IntegrationSocket.last!;
      // The server still offers the codec: a snapshot-only client must decline it.
      third.deliver(SERVER_HELLO);
      await vi.waitFor(() => expect(sentTypes(third)).toContain("Reconnect"));
      // Reach guard: the attempt really sent its ClientHello, without a codec.
      expect(sentTypes(third)).toContain("ClientHello");
      expect(third.sent.some((raw) => raw.includes("state_codecs"))).toBe(false);
      expect(sessions).toHaveLength(2);
      const shown = stateChanges(events).length;
      third.deliver(stream.startedText);
      await vi.waitFor(() => expect(events).toContainEqual({ type: "reconnected", inGame: true }));
      expect(resyncCount(third)).toBe(0);
      third.deliver(stream.updateText);
      third.deliver(stream.nextText);
      await vi.waitFor(() => expect(stateChanges(events)).toHaveLength(shown + 3));
      expect((await adapter.getState()).turn_number).toBe(
        (JSON.parse(viewer) as GameState).turn_number + 2,
      );
      expect(resyncCount(third)).toBe(0);
      expect(sessions).toHaveLength(2);
      adapter.dispose();
    } finally {
      runtime.dispose();
      vi.useRealTimers();
    }
  });

  // ── Initialization-owned fallback over the real codec ─────────────────
  //
  // The envelope below is built in TypeScript in the shape the server's
  // serializer is traced to produce (see `readGameStartedFullCredentials`); only
  // its ENCODING is real WASM. These rows therefore prove that the reader
  // extracts the exact credentials from a real encoded Full, that the real
  // decoder accepts it healthy and rejects a wire-revision mutation with
  // credentials intact, and that the production adapter then finishes the same
  // join from plain frames. They are not independent evidence of the server's
  // layout.

  const REAL_CREDENTIALS = { playerToken: "integration-token", yourPlayer: 0, fullKey: FULL_KEY };

  /** The real Full frame with ONLY its wire revision replaced by an invalid type. */
  function withInvalidWireRevision(frame: Uint8Array): string {
    const text = frameText(frame);
    const valid = '"revision":1}}';
    expect(text.endsWith(valid), "the wire revision closes the Full frame").toBe(true);
    return `${text.slice(0, -valid.length)}"revision":"not-a-revision"}}`;
  }

  /** The plain reply a Reconnect receives: the same GameStarted without the token. */
  function plainReconnectReply(stream: ReturnType<typeof realStream>): string {
    const started = JSON.parse(stream.startedText) as ReturnType<typeof gameStartedEnvelope>;
    const { player_token: _token, ...withoutToken } = started.data;
    return JSON.stringify({ ...started, data: withoutToken });
  }

  it("extracts the exact join credentials from a real encoded Full, which a healthy real decode then accepts", () => {
    const stream = realStream(baselineJson());
    expect(readGameStartedFullCredentials(frameText(stream.started))).toEqual(REAL_CREDENTIALS);
    // Only a GameStarted Full carries credentials, whatever the real encoder emits.
    expect(readGameStartedFullCredentials(frameText(stream.update))).toBeNull();
    expect(readGameStartedFullCredentials(frameText(stream.delta))).toBeNull();
    const runtime = new StateCodecRuntime(wasm);
    try {
      expect(runtime.decode(runtime.open(), stream.started))
        .toEqual({ kind: "decoded", json: stream.startedText });
    } finally {
      runtime.dispose();
    }
  });

  it("makes the real codec reject a wire-revision mutation with the credentials intact, and the adapter finishes the same join from plain frames", async () => {
    const stream = realStream(baselineJson());
    const mutated = withInvalidWireRevision(stream.started);
    // Reach guard: the mutation touched nothing but the revision, so the reader
    // still extracts the same credentials, and the real decoder rejects it.
    expect(readGameStartedFullCredentials(mutated)).toEqual(REAL_CREDENTIALS);
    const probe = new StateCodecRuntime(wasm);
    try {
      expect(probe.decode(probe.open(), utf8.encode(mutated))).toEqual({ kind: "resyncRequired" });
    } finally {
      probe.dispose();
    }

    const runtime = new StateCodecRuntime(wasm);
    const sessions: SessionStats[] = [];
    const { adapter, events } = newNormalAdapter(runtime, sessions);
    let settled: unknown = "never settled";
    const init = adapter.initialize().then(
      (value) => { settled = { resolvedWith: value }; },
      (error: unknown) => { settled = error; },
    );
    try {
      const socket = await handshakeSocket("JoinGameWithPassword");
      socket.deliver(mutated);
      // The real rejection is a bootstrap rejection: one resync, then the close.
      await vi.waitFor(() => expect(sessions[0].closes).toBe(1));
      expect(resyncCount(socket)).toBe(1);
      expect(socket.close).toHaveBeenCalled();

      // The same join continues on one snapshot-only Reconnect carrying the
      // credentials read from the rejected frame.
      await vi.waitFor(() => expect(IntegrationSocket.last).not.toBe(socket));
      const fallback = IntegrationSocket.last!;
      fallback.deliver(SERVER_HELLO);
      await vi.waitFor(() => expect(sentTypes(fallback)).toContain("Reconnect"));
      expect(sentTypes(fallback)).toContain("ClientHello");
      expect(fallback.sent.some((raw) => raw.includes("state_codecs"))).toBe(false);
      expect(fallback.sent.map((raw) => JSON.parse(raw) as { type?: string; data?: unknown })
        .find((frame) => frame.type === "Reconnect")).toEqual({
        type: "Reconnect",
        data: { game_code: "GAME01", player_token: "integration-token", full_key: FULL_KEY },
      });
      expect(sessions).toHaveLength(1);
      expect(settled).toBe("never settled");

      fallback.deliver(plainReconnectReply(stream));
      await init;
      expect(settled).toEqual({ resolvedWith: undefined });
      expect((await adapter.getSnapshot()).state).toEqual(JSON.parse(stream.startedText).data.state);
      expect(events.map((event) => event.type)).not.toContain("reconnecting");
      expect(sentTypes(socket).filter((type) => type === "JoinGameWithPassword")).toHaveLength(1);
      expect(sentTypes(fallback)).not.toContain("JoinGameWithPassword");
    } finally {
      adapter.dispose();
      runtime.dispose();
    }
  });

  // Orchestration only: the lease below rejects by construction over the same
  // real bytes. It is NOT a real wasm trap (that is the runtime-poison rows in
  // ws-adapter.test.ts); it shows the adapter's reaction to a decode rejection.
  it("finishes the same join from plain frames when an injected lease rejects the real Full (orchestration, not a real trap)", async () => {
    const stream = realStream(baselineJson());
    vi.stubGlobal("WebSocket", IntegrationSocket);
    IntegrationSocket.last = null;
    const lease = { decode: vi.fn(() => Promise.reject(new Error("decoder trapped"))), close: vi.fn() };
    const adapter = new WebSocketAdapter(
      "ws://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01",
      undefined, undefined, undefined,
      { stateCodecSessionProvider: () => Promise.resolve(lease) },
    );
    try {
      const init = adapter.initialize();
      const socket = await handshakeSocket("JoinGameWithPassword");
      socket.deliver(frameText(stream.started));
      await vi.waitFor(() => expect(lease.close).toHaveBeenCalledOnce());
      expect(lease.decode).toHaveBeenCalledOnce();
      await vi.waitFor(() => expect(IntegrationSocket.last).not.toBe(socket));
      const fallback = IntegrationSocket.last!;
      fallback.deliver(SERVER_HELLO);
      await vi.waitFor(() => expect(sentTypes(fallback)).toContain("Reconnect"));
      fallback.deliver(plainReconnectReply(stream));
      await init;
      expect((await adapter.getSnapshot()).state).toEqual(JSON.parse(stream.startedText).data.state);
    } finally {
      adapter.dispose();
    }
  });

});
