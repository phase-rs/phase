// Durable Object shell for the official phase.rs lobby broker.
//
// This is a THIN imperative shell around the compiled Rust `lobby-broker` core
// (lobby-worker/broker-wasm -> broker-wasm-pkg). All protocol parsing, dispatch,
// reservations, capacity caps, build-commit gating and staleness reaping live in
// Rust — the SAME code the native phase-server runs (extracted in Phase A), so
// the two deployments behave identically by construction. The shell only:
//   - owns the WebSocket Hibernation lifecycle + DO storage,
//   - forwards raw frames into `WasmBroker.handle`,
//   - interprets the returned `Outbound` side effects over its transport,
//   - snapshots the broker to storage after mutations (a hibernated DO loses
//     in-memory state), and
//   - drives the reaper from a DO alarm (no tokio interval in a Worker).
// Public-lobby name moderation is intentionally applied here, not in the shared
// Rust broker, so self-hosted native servers keep their own policy surface.
//
// Mirrors the engine -> engine-wasm -> React-adapter pattern: the WASM owns the
// logic, the host language is a serialization boundary with zero game logic.

import wasmModule from "./broker-wasm-pkg/broker_bg.wasm";
import { initSync, WasmBroker } from "./broker-wasm-pkg/broker.js";
import {
  classifyHelloGate,
  helloGateErrorMessage,
  type ConnAttachment,
} from "./hello-gate";
import { moderationErrorForLobbyFrame } from "./name-filter";
import { PROTOCOL_VERSION } from "./protocol";

// Instantiate the broker WASM once per isolate, at top level (CF imports `.wasm`
// as a WebAssembly.Module; `initSync` wires the wasm-bindgen imports
// synchronously). Doing this here — not per request — avoids re-instantiation.
initSync({ module: wasmModule });

const SERVER_VERSION = "lobby-rs";
// build_commit is cosmetic for a LobbyOnly broker — the gameplay-relevant gate
// is each room's host_build_commit (enforced inside the Rust core), not the
// broker's own build.
const SERVER_BUILD_COMMIT = "lobby-rs";

// Staleness reaper. `REAP_TIMEOUT_SECONDS` mirrors the native phase-server
// (`broker.reap_expired(300, …)`). The DO alarm interval is coarser than the
// native 10s tokio tick because each alarm wakes the (otherwise hibernating) DO:
// 60s reaps a stale entry within a minute of the 300s threshold while still
// letting a fully idle lobby hibernate (the alarm stops rescheduling when empty).
const REAP_TIMEOUT_SECONDS = 300;
const REAP_INTERVAL_MS = 60_000;

/// Per-socket state, mirroring `lobby_broker::ConnState::default()`. Stored in
/// the WebSocket attachment as a structured object; stringified across the WASM
/// boundary and written back from each call's result.
const DEFAULT_CONN = {
  client_hello: null,
  subscribed: false,
  host_game: null,
  reservations: [],
};

/** Boundary mirror of `lobby_broker_wasm::OutboundDto`. */
interface OutboundDto {
  kind: "ToSelf" | "ToSubscribers" | "AddSubscriber" | "RemoveSubscriber" | "SendPlayerCountToSelf";
  msg?: unknown;
}

/** Boundary mirror of `lobby_broker_wasm::CallResult`. */
interface CallResult {
  conn: unknown;
  outbounds: OutboundDto[];
  dirty: boolean;
  reject?: string;
}

const SNAPSHOT_KEY = "broker_snapshot";

export class LobbyDO {
  private ctx: DurableObjectState;
  /** In-memory broker, restored from the DO-storage snapshot on first use after
   *  a cold start / hibernation wake. */
  private broker: WasmBroker | null = null;

  constructor(ctx: DurableObjectState, _env: unknown) {
    this.ctx = ctx;
  }

  private async loadBroker(): Promise<WasmBroker> {
    if (!this.broker) {
      const snap = await this.ctx.storage.get<string>(SNAPSHOT_KEY);
      this.broker = snap ? WasmBroker.from_snapshot(snap) : new WasmBroker();
    }
    return this.broker;
  }

  // ── HTTP / WS entry ────────────────────────────────────────────────────

  async fetch(request: Request): Promise<Response> {
    if (request.headers.get("Upgrade") !== "websocket") {
      // Plain GET → version/health endpoint (deploy smoke check asserts
      // protocol_version == released client's).
      return Response.json({
        mode: "LobbyOnly",
        protocol_version: PROTOCOL_VERSION,
        server_version: SERVER_VERSION,
      });
    }

    const { 0: client, 1: server } = new WebSocketPair();
    // Hibernation API: the runtime owns the socket and wakes the DO via the
    // webSocket* handlers, so an idle lobby incurs no duration charge.
    this.ctx.acceptWebSocket(server);
    server.serializeAttachment(DEFAULT_CONN);
    this.sendHello(server);
    // A new connection changes the live player count for existing subscribers.
    this.broadcastPlayerCount();
    // Lobby occupancy heartbeat — once per connection (not per frame). Lets you
    // see usage and spot connection leaks (count that never returns to 0).
    console.log({ event: "lobby_connect", players: this.ctx.getWebSockets().length });
    return new Response(null, { status: 101, webSocket: client });
  }

  // ── WebSocket Hibernation handlers ─────────────────────────────────────

  async webSocketMessage(ws: WebSocket, raw: string | ArrayBuffer): Promise<void> {
    const broker = await this.loadBroker();
    const conn = ws.deserializeAttachment() ?? DEFAULT_CONN;
    const text = typeof raw === "string" ? raw : new TextDecoder().decode(raw);
    const moderationError = moderationErrorForLobbyFrame(text);
    if (moderationError) {
      ws.send(JSON.stringify({ type: "Error", data: { message: moderationError } }));
      console.warn({ event: "lobby_name_rejected" });
      return;
    }

    let frame: { type?: string; data?: Record<string, unknown> };
    try {
      frame = JSON.parse(text) as { type?: string; data?: Record<string, unknown> };
    } catch {
      console.warn({ event: "lobby_frame_rejected", reason: "invalid_json" });
      return;
    }

    const attachment = conn as ConnAttachment;
    const gate = classifyHelloGate(attachment.client_hello != null, frame);
    const gateError = helloGateErrorMessage(gate);
    if (gateError) {
      ws.send(JSON.stringify({ type: "Error", data: { message: gateError } }));
      console.warn({ event: "lobby_hello_gate_rejected", reason: gate.kind });
      return;
    }
    if (gate.kind === "ignore") {
      return;
    }

    const result = JSON.parse(broker.handle(JSON.stringify(conn), text, Date.now())) as CallResult;

    if (result.reject) {
      // Unknown tag / malformed frame — the Rust parser rejected it. Drop it
      // (no state change, attachment untouched), but log so it surfaces in
      // Workers Logs.
      console.warn({ event: "lobby_frame_rejected", reason: result.reject });
      return;
    }

    ws.serializeAttachment(result.conn);
    this.interpret(ws, result.outbounds);

    if (result.dirty) {
      await this.ctx.storage.put(SNAPSHOT_KEY, broker.snapshot());
      await this.ensureAlarm();
    }
  }

  async webSocketClose(ws: WebSocket): Promise<void> {
    const broker = await this.loadBroker();
    const conn = ws.deserializeAttachment() ?? DEFAULT_CONN;
    // Releases the connection's reservations + removes any hosted entry; emits
    // LobbyGameUpdated/Removed to the remaining subscribers (the closing socket
    // is already excluded from getWebSockets()).
    const result = JSON.parse(broker.on_disconnect(JSON.stringify(conn))) as CallResult;
    this.interpret(ws, result.outbounds);
    await this.ctx.storage.put(SNAPSHOT_KEY, broker.snapshot());
    // Player-count decrement+broadcast is shell-owned on close (the broker
    // cannot know the socket set). getWebSockets() already excludes the closing
    // socket, so this count reflects who remains.
    this.broadcastPlayerCount();
    console.log({ event: "lobby_disconnect", players: this.ctx.getWebSockets().length });
  }

  async webSocketError(ws: WebSocket): Promise<void> {
    // Distinguish abnormal closes (protocol/transport error) from clean ones —
    // teardown is identical, but a spike here points at a client/network fault.
    console.warn({ event: "lobby_ws_error" });
    await this.webSocketClose(ws);
  }

  // ── Staleness reaper (DO alarm) ────────────────────────────────────────

  async alarm(): Promise<void> {
    const broker = await this.loadBroker();
    const outbounds = JSON.parse(
      broker.reap_expired(REAP_TIMEOUT_SECONDS, Date.now()),
    ) as OutboundDto[];
    // Reaper emits only ToSubscribers(LobbyGameRemoved) — no connection scope.
    for (const o of outbounds) this.dispatchOutbound(null, o);
    // One log per non-empty sweep (≤ once/REAP_INTERVAL_MS); count == entries
    // reaped, since each removal emits exactly one LobbyGameRemoved.
    if (outbounds.length > 0) {
      console.log({ event: "lobby_reaped", count: outbounds.length });
    }
    await this.ctx.storage.put(SNAPSHOT_KEY, broker.snapshot());
    // Keep reaping while entries remain; an empty lobby stops rescheduling so
    // the DO can hibernate fully.
    if (!broker.is_empty()) {
      await this.ctx.storage.setAlarm(Date.now() + REAP_INTERVAL_MS);
    }
  }

  // ── Outbound side-effect interpretation ────────────────────────────────

  private interpret(ws: WebSocket, outbounds: OutboundDto[]): void {
    for (const o of outbounds) this.dispatchOutbound(ws, o);
  }

  private dispatchOutbound(ws: WebSocket | null, o: OutboundDto): void {
    switch (o.kind) {
      case "ToSelf":
        if (ws) ws.send(JSON.stringify(o.msg));
        return;
      case "ToSubscribers":
        this.broadcastToSubscribers(JSON.stringify(o.msg));
        return;
      case "SendPlayerCountToSelf":
        if (ws) ws.send(this.playerCountFrame());
        return;
      case "AddSubscriber":
      case "RemoveSubscriber":
        // No-op: the subscriber registry IS each socket's persisted
        // ConnState.subscribed (set by the broker, read in
        // broadcastToSubscribers). A separate in-memory set would be lost on
        // hibernation, so the attachment is the single source of truth.
        return;
    }
  }

  // ── Messaging helpers ──────────────────────────────────────────────────

  private broadcastToSubscribers(frame: string): void {
    for (const sock of this.ctx.getWebSockets()) {
      if (this.isSubscribed(sock)) sock.send(frame);
    }
  }

  private broadcastPlayerCount(): void {
    const frame = this.playerCountFrame();
    for (const sock of this.ctx.getWebSockets()) {
      if (this.isSubscribed(sock)) sock.send(frame);
    }
  }

  private isSubscribed(sock: WebSocket): boolean {
    const conn = sock.deserializeAttachment() as { subscribed?: boolean } | null;
    return conn?.subscribed === true;
  }

  private playerCountFrame(): string {
    // PlayerCount is shell-owned: the broker emits SendPlayerCountToSelf and the
    // shell fills the count from the live socket set.
    return JSON.stringify({
      type: "PlayerCount",
      data: { count: this.ctx.getWebSockets().length },
    });
  }

  private async ensureAlarm(): Promise<void> {
    if ((await this.ctx.storage.getAlarm()) === null) {
      await this.ctx.storage.setAlarm(Date.now() + REAP_INTERVAL_MS);
    }
  }

  private sendHello(ws: WebSocket): void {
    ws.send(
      JSON.stringify({
        type: "ServerHello",
        data: {
          server_version: SERVER_VERSION,
          build_commit: SERVER_BUILD_COMMIT,
          protocol_version: PROTOCOL_VERSION,
          mode: "LobbyOnly",
        },
      }),
    );
  }
}
