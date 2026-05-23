import {
  PROTOCOL_VERSION,
  type LobbyGame,
  type RoomSecret,
  type SocketState,
} from "./protocol";

// ⚠️ STUB. Throwaway TS reimplementation of the LobbyOnly broker happy-path
// (handshake + lobby list + P2P host/join), enough to test the Cloudflare
// path end-to-end against a real client. It deliberately OMITS the security
// hardening the Rust server has (rate limiting, MAX_LOBBY_ENTRIES cap, seat
// reservations, the expiry reaper) — all of which arrive for free when the
// compiled Rust `lobby-broker` crate replaces this body
// (.planning/lobby-failover-federation-plan.md §4a, §6c, §6f).

const SERVER_VERSION = "lobby-stub";
// build_commit is cosmetic for a LobbyOnly broker — the gameplay-relevant gate
// is each room's host_build_commit (the host client's hash), not the broker's.
const SERVER_BUILD_COMMIT = "lobby-stub-dev";

const CODE_ALPHABET = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

export class LobbyDO {
  private ctx: DurableObjectState;
  /** In-memory cache of the persisted room map; rebuilt from DO storage on
   *  first use after a cold start / hibernation wake. */
  private rooms: Map<string, LobbyGame> | null = null;

  constructor(ctx: DurableObjectState, _env: unknown) {
    this.ctx = ctx;
  }

  // ── HTTP / WS entry ────────────────────────────────────────────────────

  async fetch(request: Request): Promise<Response> {
    if (request.headers.get("Upgrade") !== "websocket") {
      // Plain GET → version/health endpoint. Handy for the deploy smoke check
      // (`curl https://<worker>/` → asserts protocol_version).
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
    this.setState(server, { subscribed: false, buildCommit: "" });
    // Unprompted ServerHello — the client waits for this and validates
    // protocol_version before sending anything.
    this.sendHello(server);
    return new Response(null, { status: 101, webSocket: client });
  }

  // ── WebSocket Hibernation handlers ─────────────────────────────────────

  async webSocketMessage(ws: WebSocket, raw: string | ArrayBuffer): Promise<void> {
    let msg: { type: string; data?: Record<string, unknown> };
    try {
      const text = typeof raw === "string" ? raw : new TextDecoder().decode(raw);
      msg = JSON.parse(text);
    } catch {
      return; // drop malformed frames silently
    }

    const state = this.getState(ws);
    const data = msg.data ?? {};

    switch (msg.type) {
      case "ClientHello": {
        state.buildCommit = (data.build_commit as string) ?? "";
        this.setState(ws, state);
        return;
      }

      case "SubscribeLobby": {
        state.subscribed = true;
        this.setState(ws, state);
        const rooms = await this.loadRooms();
        this.send(ws, "LobbyUpdate", { games: [...rooms.values()] });
        this.send(ws, "PlayerCount", { count: this.playerCount() });
        return;
      }

      case "UnsubscribeLobby": {
        state.subscribed = false;
        this.setState(ws, state);
        return;
      }

      case "Ping": {
        this.send(ws, "Pong", { timestamp: data.timestamp ?? 0 });
        return;
      }

      case "CreateGameWithSettings": {
        await this.handleCreate(ws, state, data);
        return;
      }

      case "UpdateLobbyMetadata": {
        await this.handleUpdateMetadata(state, data);
        return;
      }

      case "UnregisterLobby": {
        await this.handleUnregister(ws, state, data.game_code as string);
        return;
      }

      case "LookupJoinTarget": {
        await this.handleJoin(ws, data, /* lookupOnly */ true);
        return;
      }

      case "JoinGameWithPassword": {
        await this.handleJoin(ws, data, /* lookupOnly */ false);
        return;
      }

      default:
        // Stub ignores the rest of the protocol (game-session messages, drafts,
        // seat mutation, reservations). The Rust broker implements the full set.
        return;
    }
  }

  async webSocketClose(ws: WebSocket): Promise<void> {
    const state = this.getState(ws);
    if (state.ownedGameCode) {
      await this.removeRoom(state.ownedGameCode);
    }
    // PlayerCount changed; the closing socket is already excluded from
    // getWebSockets() by the time this fires.
    this.broadcastPlayerCount();
  }

  async webSocketError(ws: WebSocket): Promise<void> {
    await this.webSocketClose(ws);
  }

  // ── Handlers ───────────────────────────────────────────────────────────

  private async handleCreate(
    ws: WebSocket,
    state: SocketState,
    data: Record<string, unknown>,
  ): Promise<void> {
    const rooms = await this.loadRooms();
    const code = this.genCode(rooms);
    const hostPeerId = (data.host_peer_id as string) ?? "";
    const formatConfig = (data.format_config as Record<string, unknown>) ?? null;

    const game: LobbyGame = {
      game_code: code,
      host_name: (data.display_name as string) ?? "Anonymous",
      created_at: Date.now(),
      has_password: !!data.password,
      host_version: "",
      host_build_commit: state.buildCommit,
      current_players: 1,
      max_players: (data.player_count as number) ?? 2,
      format: (formatConfig?.format as string) ?? null,
      room_name: (data.room_name as string) ?? null,
      // is_p2p is derived from host_peer_id presence (matches the Rust server).
      is_p2p: hostPeerId !== "",
      is_sandbox: !!formatConfig?.allow_debug_actions,
    };

    rooms.set(code, game);
    await this.saveRooms();
    const secret: RoomSecret = {
      hostPeerId,
      password: (data.password as string) ?? null,
      formatConfig,
      matchConfig: data.match_config ?? {},
    };
    await this.ctx.storage.put(`secret:${code}`, secret);

    state.ownedGameCode = code;
    this.setState(ws, state);

    this.send(ws, "GameCreated", { game_code: code, player_token: crypto.randomUUID() });
    this.broadcastToSubscribers("LobbyGameAdded", { game });
    this.broadcastPlayerCount();
  }

  private async handleUpdateMetadata(
    state: SocketState,
    data: Record<string, unknown>,
  ): Promise<void> {
    const code = data.game_code as string;
    // Ownership check: only the socket that registered the room may mutate it.
    if (state.ownedGameCode !== code) return;
    const rooms = await this.loadRooms();
    const game = rooms.get(code);
    if (!game) return;
    game.current_players = (data.current_players as number) ?? game.current_players;
    game.max_players = (data.max_players as number) ?? game.max_players;
    rooms.set(code, game);
    await this.saveRooms();
    this.broadcastToSubscribers("LobbyGameUpdated", { game });
  }

  private async handleUnregister(
    ws: WebSocket,
    state: SocketState,
    code: string,
  ): Promise<void> {
    if (state.ownedGameCode !== code) return;
    await this.removeRoom(code);
    state.ownedGameCode = undefined;
    this.setState(ws, state);
  }

  private async handleJoin(
    ws: WebSocket,
    data: Record<string, unknown>,
    lookupOnly: boolean,
  ): Promise<void> {
    const code = data.game_code as string;
    const rooms = await this.loadRooms();
    const game = rooms.get(code);
    if (!game) {
      this.send(ws, "Error", { message: "Game not found" });
      return;
    }
    const secret = await this.ctx.storage.get<RoomSecret>(`secret:${code}`);
    if (secret?.password && secret.password !== (data.password as string | undefined)) {
      this.send(ws, "PasswordRequired", { game_code: code });
      return;
    }

    const common = {
      game_code: code,
      is_p2p: game.is_p2p,
      format_config: secret?.formatConfig ?? null,
      match_config: secret?.matchConfig ?? {},
      player_count: game.max_players,
      filled_seats: game.current_players,
    };

    if (lookupOnly) {
      // JoinTargetInfo has no host_peer_id (read-only routing metadata).
      this.send(ws, "JoinTargetInfo", common);
    } else {
      // PeerInfo hands the guest the host's peer id so it can dial P2P.
      this.send(ws, "PeerInfo", { ...common, host_peer_id: secret?.hostPeerId ?? "" });
    }
  }

  // ── Storage / state helpers ────────────────────────────────────────────

  private async loadRooms(): Promise<Map<string, LobbyGame>> {
    if (!this.rooms) {
      const stored = await this.ctx.storage.get<Record<string, LobbyGame>>("rooms");
      this.rooms = new Map(Object.entries(stored ?? {}));
    }
    return this.rooms;
  }

  private async saveRooms(): Promise<void> {
    if (this.rooms) {
      await this.ctx.storage.put("rooms", Object.fromEntries(this.rooms));
    }
  }

  private async removeRoom(code: string): Promise<void> {
    const rooms = await this.loadRooms();
    if (rooms.delete(code)) {
      await this.saveRooms();
      await this.ctx.storage.delete(`secret:${code}`);
      this.broadcastToSubscribers("LobbyGameRemoved", { game_code: code });
    }
  }

  private setState(ws: WebSocket, state: SocketState): void {
    ws.serializeAttachment(state);
  }

  private getState(ws: WebSocket): SocketState {
    return (
      (ws.deserializeAttachment() as SocketState | null) ?? {
        subscribed: false,
        buildCommit: "",
      }
    );
  }

  // ── Messaging helpers ──────────────────────────────────────────────────

  private send(ws: WebSocket, type: string, data?: unknown): void {
    // Adjacently-tagged enum: unit variants omit `data`, the rest carry it.
    ws.send(JSON.stringify(data === undefined ? { type } : { type, data }));
  }

  private sendHello(ws: WebSocket): void {
    this.send(ws, "ServerHello", {
      server_version: SERVER_VERSION,
      build_commit: SERVER_BUILD_COMMIT,
      protocol_version: PROTOCOL_VERSION,
      mode: "LobbyOnly",
    });
  }

  private playerCount(): number {
    return this.ctx.getWebSockets().length;
  }

  private broadcastToSubscribers(type: string, data: unknown): void {
    for (const ws of this.ctx.getWebSockets()) {
      if (this.getState(ws).subscribed) this.send(ws, type, data);
    }
  }

  private broadcastPlayerCount(): void {
    this.broadcastToSubscribers("PlayerCount", { count: this.playerCount() });
  }

  private genCode(rooms: Map<string, LobbyGame>): string {
    let code: string;
    do {
      code = Array.from(
        { length: 5 },
        () => CODE_ALPHABET[Math.floor(Math.random() * CODE_ALPHABET.length)],
      ).join("");
    } while (rooms.has(code));
    return code;
  }
}
