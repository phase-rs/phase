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

  function stateUpdateEnvelope(
    viewerState: string,
    revision: number,
    extra: Record<string, unknown> = {},
  ) {
    return {
      type: "StateUpdate" as const,
      data: {
        state_revision: revision,
        state: JSON.parse(viewerState) as GameState,
        events: [{ type: "TurnStarted" }],
        legal_actions: [{ type: "PassPriority" }],
        full_key: FULL_KEY,
        ...extra,
      },
    };
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

  function deltaFrame(text: string, revision: bigint, baseText: string, baseRevision: bigint): Uint8Array {
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

  it("reconstructs complete server envelopes exactly through the runtime helper", () => {
    const firstJson = corpus.checkpoints[0].audiences[0].json;
    const secondJson = corpus.checkpoints[1].audiences[0].json;
    const startedText = JSON.stringify(gameStartedEnvelope(firstJson, 1));
    const updateText = JSON.stringify(stateUpdateEnvelope(secondJson, 2));

    const full = fullFrame(startedText, 1n);
    expect(frameType(full)).toBe("Full");
    const delta = deltaFrame(updateText, 2n, startedText, 1n);
    expect(frameType(delta)).toBe("Delta");

    const runtime = new StateCodecRuntime(wasm);
    const session = runtime.open();
    try {
      const decodedFull = runtime.decode(session, full);
      expect(decodedFull).toEqual({ kind: "decoded", json: startedText });
      // The transport observes text only: revisions stay inside WASM.
      expect(Object.keys(decodedFull).sort()).toEqual(["json", "kind"]);

      const decodedDelta = runtime.decode(session, delta);
      expect(decodedDelta).toEqual({ kind: "decoded", json: updateText });
      expect(JSON.parse((decodedDelta as { json: string }).json))
        .toEqual(JSON.parse(updateText));
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
    const delta = deltaFrame(currentText, BigInt(currentRow.revision), baseText, BigInt(baseRow.revision));
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

  it("reports resyncRequired for a missing baseline and a corrupt frame, then recovers on a Full", () => {
    const firstJson = corpus.checkpoints[0].audiences[0].json;
    const secondJson = corpus.checkpoints[1].audiences[0].json;
    const startedText = JSON.stringify(gameStartedEnvelope(firstJson, 1));
    const updateText = JSON.stringify(stateUpdateEnvelope(secondJson, 2));
    const full = fullFrame(startedText, 1n);
    const delta = deltaFrame(updateText, 2n, startedText, 1n);

    const runtime = new StateCodecRuntime(wasm);
    const session = runtime.open();
    try {
      // A delta cannot establish a baseline.
      expect(runtime.decode(session, delta)).toEqual({ kind: "resyncRequired" });
      // A frame whose result hash was altered is rejected.
      const tampered = JSON.parse(frameText(delta)) as {
        data: { result_hash: string };
      };
      tampered.data.result_hash =
        `${tampered.data.result_hash[0] === "0" ? "1" : "0"}${tampered.data.result_hash.slice(1)}`;
      expect(runtime.decode(session, utf8.encode(JSON.stringify(tampered))))
        .toEqual({ kind: "resyncRequired" });
      // A Full establishes the baseline; the same delta then applies.
      expect(runtime.decode(session, full)).toEqual({ kind: "decoded", json: startedText });
      expect(runtime.decode(session, delta)).toEqual({ kind: "decoded", json: updateText });
      // A closed session is a lifecycle failure, not a resync request.
      runtime.close(session);
      expect(() => runtime.decode(session, full)).toThrow();
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

  /** A provider over this file's real module, i.e. real Rust reconstruction. */
  function realSessionProvider(runtime: StateCodecRuntime) {
    return () => {
      const sessionId = runtime.open();
      return Promise.resolve({
        decode: async (frame: Uint8Array) => runtime.decode(sessionId, frame),
        close: () => runtime.close(sessionId),
      });
    };
  }

  function sentTypes(socket: IntegrationSocket): string[] {
    return socket.sent.map((raw) => (JSON.parse(raw) as { type?: string }).type ?? "");
  }

  it("normal and draft adapters emit the same state from one real codec stream", async () => {
    vi.stubGlobal("WebSocket", IntegrationSocket);
    const firstJson = corpus.checkpoints[0].audiences[0].json;
    const secondJson = corpus.checkpoints[2].audiences[0].json;
    const started = gameStartedEnvelope(firstJson, 1);
    const update = stateUpdateEnvelope(secondJson, 2);
    const startedText = JSON.stringify(started);
    const updateText = JSON.stringify(update);
    const full = frameText(fullFrame(startedText, 1n));
    const delta = frameText(deltaFrame(updateText, 2n, startedText, 1n));

    // ── Normal adapter ──
    const normalRuntime = new StateCodecRuntime(wasm);
    const normal = new WebSocketAdapter(
      "ws://localhost:9374/ws",
      "join",
      { main_deck: [], sideboard: [] },
      "GAME01",
      undefined,
      undefined,
      undefined,
      { stateCodecSessionProvider: realSessionProvider(normalRuntime) },
    );
    const normalStates: GameState[] = [];
    normal.onEvent((event) => {
      if (event.type === "stateChanged") normalStates.push(event.snapshot.state);
    });
    const normalInit = normal.initialize();
    await vi.waitFor(() => expect(IntegrationSocket.last).not.toBeNull());
    const normalSocket = IntegrationSocket.last!;
    normalSocket.deliver(SERVER_HELLO);
    await vi.waitFor(() =>
      expect((normal as unknown as { ws: unknown }).ws).not.toBeNull(),
    );
    normalSocket.deliver(CONFIRMATION);
    normalSocket.deliver(full);
    await normalInit;
    normalSocket.deliver(delta);
    await vi.waitFor(() => expect(normalStates).toHaveLength(2));
    expect(normalStates[0]).toEqual(started.data.state);
    expect(normalStates[1]).toEqual(update.data.state);

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
    await vi.waitFor(() => expect(IntegrationSocket.last).not.toBeNull());
    const draftSocket = IntegrationSocket.last!;
    draftSocket.deliver(SERVER_HELLO);
    await vi.waitFor(() =>
      expect((draft as unknown as { ws: unknown }).ws).not.toBeNull(),
    );
    draftSocket.deliver(CONFIRMATION);
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
    draftSocket.deliver(full);
    draftSocket.deliver(delta);
    await vi.waitFor(() => expect(draftStates).toHaveLength(2));
    expect(draftStates[0]).toEqual(started.data.state);
    expect(draftStates[1]).toEqual(update.data.state);

    expect(normalStates).toEqual(draftStates);
    normal.dispose();
    draft.dispose();
    normalRuntime.dispose();
    draftRuntime.dispose();
  });

  it("recovers a corrupted real delta through one ResyncState and a real Full", async () => {
    vi.stubGlobal("WebSocket", IntegrationSocket);
    const firstJson = corpus.checkpoints[0].audiences[0].json;
    const secondJson = corpus.checkpoints[1].audiences[0].json;
    const thirdJson = corpus.checkpoints[2].audiences[0].json;
    const started = gameStartedEnvelope(firstJson, 1);
    const update = stateUpdateEnvelope(secondJson, 2);
    const startedText = JSON.stringify(started);
    const updateText = JSON.stringify(update);
    const delta = deltaFrame(updateText, 2n, startedText, 1n);
    const tampered = JSON.parse(frameText(delta)) as { data: { result_hash: string } };
    tampered.data.result_hash =
      `${tampered.data.result_hash[0] === "0" ? "1" : "0"}${tampered.data.result_hash.slice(1)}`;

    const runtime = new StateCodecRuntime(wasm);
    const adapter = new WebSocketAdapter(
      "ws://localhost:9374/ws",
      "join",
      { main_deck: [], sideboard: [] },
      "GAME01",
      undefined,
      undefined,
      undefined,
      { stateCodecSessionProvider: realSessionProvider(runtime) },
    );
    const init = adapter.initialize();
    await vi.waitFor(() => expect(IntegrationSocket.last).not.toBeNull());
    const socket = IntegrationSocket.last!;
    socket.deliver(SERVER_HELLO);
    await vi.waitFor(() => expect((adapter as unknown as { ws: unknown }).ws).not.toBeNull());
    socket.deliver(CONFIRMATION);
    socket.deliver(frameText(fullFrame(startedText, 1n)));
    await init;

    const pending = adapter.submitAction({ type: "PassPriority" } as never, 0);
    const outcome = pending.then(() => "resolved", (error: unknown) => error);
    socket.deliver(JSON.stringify(tampered));
    await expect(outcome).resolves.toMatchObject({ code: "WS_CLOSED", recoverable: true });
    expect(sentTypes(socket).filter((type) => type === "ResyncState")).toHaveLength(1);
    await expect(adapter.submitAction({ type: "PassPriority" } as never, 0))
      .rejects.toMatchObject({ code: "ACTION_NOT_SENT" });

    // The server answers with a fresh FULL at the current revision.
    const recovery = stateUpdateEnvelope(thirdJson, 3);
    const recoveryText = JSON.stringify(recovery);
    socket.deliver(frameText(fullFrame(recoveryText, 3n)));
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
    const completion = stateUpdateEnvelope(thirdJson, 4, { command_id: commandId });
    const completionText = JSON.stringify(completion);
    socket.deliver(frameText(deltaFrame(completionText, 4n, recoveryText, 3n)));
    await expect(resumed).resolves.toBeDefined();

    adapter.dispose();
    runtime.dispose();
  });
});
