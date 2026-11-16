/**
 * Manual native -> gzip -> transport -> real WASM -> spectator adapter proof.
 * No decoder/adapter mocks or generated multi-megabyte fixtures are committed.
 * From the repo root, produce the fixed gitignored fixture directory:
 * cargo test -p phase-server \
 *   --bin phase-server socket_io::large_full_tests::produce_native_large_spectator_full \
 *   -- --ignored --exact --nocapture
 * Build same-source WASM: bash scripts/build-wasm.sh wasm-dev
 * cd client && pnpm exec vitest run \
 *   --config vitest.integration.config.ts --coverage.enabled=false \
 *   src/adapter/__tests__/largeSpectatorFull.integration.test.ts
 * The generated fixture is a native scenario-constructed board of real fixture
 * Llanowar Elves, not an action replay or evidence of a reachable 9,000-card deck.
 * Its GameStarted builder, projection, state codec, worker, guards and gzip are
 * genuine phase-server code. The transport is deliberately an in-process socket.
 */
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { readFile, stat } from "node:fs/promises";
import { resolve } from "node:path";
import { gunzipSync } from "node:zlib";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import type * as WasmEngine from "@wasm/engine";
import { StateCodecRuntime } from "../state-codec-runtime";
import { PROTOCOL_VERSION, WebSocketAdapter } from "../ws-adapter";
import { decodeJsonEnvelope } from "../../network/wireEnvelope";

const DIRECTORY = resolve(__dirname, "../../../../target/state-codec-fixtures/large-spectator-full");
const ENABLED = existsSync(DIRECTORY);
const MAX_BYTES = 256 * 1024 * 1024;
const WASM_BINARY = resolve(__dirname, "../../wasm/engine_wasm_bg.wasm");
type FrameStats = { wire_bytes: number; frame_utf16_units: number; decoded_bytes: number; revision: number; decoded_sha256: string };
type Manifest = {
  schema_version: number; producer: string; card: string; objects: number; game_code: string;
  small: FrameStats; large: FrameStats; after: FrameStats;
};

class ProbeSocket extends EventTarget {
  static OPEN = 1;
  static CLOSED = 3;
  static last: ProbeSocket | null = null;
  readyState = ProbeSocket.OPEN;
  binaryType: BinaryType = "blob";
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;
  readonly sentTypes: string[] = [];
  close = vi.fn(() => {
    this.readyState = ProbeSocket.CLOSED;
    this.onclose?.();
    this.dispatchEvent(new Event("close"));
  });
  send = vi.fn((data: string | ArrayBuffer | ArrayBufferView) => {
    const text = typeof data === "string"
      ? Promise.resolve(data)
      : decodeJsonEnvelope(data instanceof ArrayBuffer
        ? new Uint8Array(data)
        : new Uint8Array(data.buffer, data.byteOffset, data.byteLength));
    void text.then((json) => this.sentTypes.push(JSON.parse(json).type));
  });
  constructor(public url: string) { super(); ProbeSocket.last = this; }
  deliver(data: unknown): void { this.onmessage?.({ data }); }
}

if (!ENABLED) {
  process.stdout.write("large spectator Full proof skipped: run the native producer for target/state-codec-fixtures/large-spectator-full\n");
}

describe.skipIf(!ENABLED)("native large spectator GameStarted Full", () => {
  let wasm: typeof WasmEngine;
  let manifest: Manifest;
  const frames: Record<string, Uint8Array<ArrayBuffer>> = {};
  const adapters: WebSocketAdapter[] = [];

  beforeAll(async () => {
    manifest = JSON.parse(await readFile(resolve(DIRECTORY, "manifest.json"), "utf8"));
    expect(manifest.schema_version).toBe(1);
    expect(manifest.card).toBe("Llanowar Elves");
    expect(manifest.objects).toBe(9_000);
    expect(manifest.large.frame_utf16_units).toBeGreaterThan(8_388_608);
    for (const name of ["small", "large", "after"] as const) {
      const path = resolve(DIRECTORY, `${name}.wire`);
      expect((await stat(path)).size).toBeLessThanOrEqual(MAX_BYTES);
      frames[name] = new Uint8Array(await readFile(path));
      expect(frames[name][0]).toBe(0x01);
      expect(frames[name].byteLength).toBe(manifest[name].wire_bytes);
      // Independent Node gzip inspection of producer bytes, not test encoding.
      const text = gunzipSync(frames[name].subarray(1)).toString("utf8");
      expect(text.length).toBe(manifest[name].frame_utf16_units);
      const full = JSON.parse(text);
      expect(full.type).toBe("Full");
      expect(full.data.state.type).toBe(name === "after" ? "StateUpdate" : "GameStarted");
    }
    // Generated WASM does not exist in every checkout; keep the missing-fixture
    // gate usable without resolving this platform/build-specific artifact.
    wasm = await import("@wasm/engine");
    await wasm.default({ module_or_path: await WebAssembly.compile(await readFile(WASM_BINARY)) });
  }, 300_000);

  afterEach(() => {
    for (const adapter of adapters.splice(0)) adapter.dispose();
    vi.unstubAllGlobals();
  });

  it.each([false, true])("commits a >8Mi-code-unit Full with warm spectator=%s and then accepts an update", async (warm) => {
    vi.stubGlobal("WebSocket", ProbeSocket);
    ProbeSocket.last = null;
    const runtime = new StateCodecRuntime(wasm);
    const stats = { decodes: 0, closes: 0 };
    const expectedFrames = warm ? [manifest.small, manifest.large, manifest.after] : [manifest.large, manifest.after];
    const adapter = new WebSocketAdapter(
      "ws://localhost:9374/ws", "spectate", { main_deck: [], sideboard: [] }, manifest.game_code,
      undefined, undefined, undefined,
      { stateCodecSessionProvider: async () => {
        const session = runtime.open();
        return {
          decode: async (frame: Uint8Array) => {
            const expected = expectedFrames[stats.decodes++];
            const result = runtime.decode(session, frame);
            expect(result.kind).toBe("decoded");
            if (result.kind === "decoded") {
              expect(Buffer.byteLength(result.json, "utf8")).toBe(expected.decoded_bytes);
              expect(createHash("sha256").update(result.json).digest("hex")).toBe(expected.decoded_sha256);
            }
            return result;
          },
          close: () => { stats.closes++; runtime.close(session); },
        };
      } },
    );
    adapters.push(adapter);
    const events: string[] = [];
    adapter.onEvent((event) => events.push(event.type));
    const initialized = adapter.initialize();
    await vi.waitFor(() => expect(ProbeSocket.last).not.toBeNull());
    const socket = ProbeSocket.last!;
    socket.deliver(JSON.stringify({ type: "ServerHello", data: {
      server_version: "probe", build_commit: "same-source-native-producer", protocol_version: PROTOCOL_VERSION,
      mode: "Full", state_codecs: ["StateCodecV1"], wire_formats: ["GzipEnvelopeV1"],
    } }));
    await vi.waitFor(() => expect(socket.sentTypes).toContain("SpectatorJoin"));
    socket.deliver(JSON.stringify({ type: "ServerHello", data: {
      server_version: "probe", selected_state_codec: "StateCodecV1",
    } }));
    // Public adapter Pong delivery fences both confirmation and gzip-wrapper
    // setup before the empty-queue oversized bootstrap is delivered.
    socket.deliver(JSON.stringify({ type: "Pong", data: { timestamp: Date.now() } }));
    await vi.waitFor(() => expect(events).toContain("latencyChanged"));
    if (warm) {
      socket.deliver(frames.small.buffer);
      await initialized;
      expect(Object.keys((await adapter.getSnapshot()).state.objects)).toHaveLength(0);
    }
    socket.deliver(frames.large.buffer);
    if (!warm) await initialized;
    await vi.waitFor(async () => {
      expect(Object.keys((await adapter.getSnapshot()).state.objects)).toHaveLength(9_000);
    }, { timeout: 30_000 });
    const state = (await adapter.getSnapshot()).state;
    const objects = Object.values(state.objects);
    expect(objects.every((object) => object.name === "Llanowar Elves")).toBe(true);
    expect(objects.every((object) => object.abilities.length > 0)).toBe(true);
    expect(adapter.playerId).toBe(255);
    expect((await adapter.getLegalActions()).actions).toEqual([]);
    expect(socket.close).not.toHaveBeenCalled();
    expect(stats.closes).toBe(0);
    expect(socket.sentTypes).not.toContain("ResyncState");
    expect(events).not.toContain("error");
    expect(events).not.toContain("disconnected");
    socket.deliver(frames.after.buffer);
    await vi.waitFor(async () => {
      expect(Object.keys((await adapter.getSnapshot()).state.objects)).toHaveLength(1);
    }, { timeout: 30_000 });
    expect(events).toContain("stateChanged");
    expect(stats.decodes).toBe(warm ? 3 : 2);
    expect(socket.close).not.toHaveBeenCalled();
    expect(stats.closes).toBe(0);
    adapter.dispose();
    expect(stats.closes).toBe(1);
    expect(socket.close).toHaveBeenCalledOnce();
    process.stdout.write(`native spectator Full: warm=${warm} objects=9000 utf16=${manifest.large.frame_utf16_units} wire=${manifest.large.wire_bytes} real_decodes=${stats.decodes} closes=${stats.closes}\n`);
  }, 120_000);
});
