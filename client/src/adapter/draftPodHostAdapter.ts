/**
 * Draft Pod Host Adapter.
 *
 * Lifecycle wrapper that creates a PeerJS peer, optionally registers with
 * the lobby broker, instantiates a `P2PDraftHost`, and exposes a clean
 * event-driven interface for the Zustand `multiplayerDraftStore`.
 *
 * Mirrors the pattern of `P2PHostAdapter` (game host), but the underlying
 * coordinator speaks the `DraftP2PMessage` protocol and manages an 8-seat
 * draft pod instead of a 2-4 player game.
 */

import { DraftAdapter } from "./draft-adapter";
import type { DraftKind, DraftPlayerView, PairingView, PodPolicy, PoolInput, SeatPublicView, TournamentFormat } from "./draft-adapter";
import type { MatchScore } from "./types";
import { P2PDraftHost, type DraftHostEvent } from "./p2p-draft-host";
import { hostRoom, type HostResult } from "../network/connection";
import type { CommanderSeatDecks, DraftCommanderLaunch, DraftMatchDeckPayload, DraftMatchLaunch, DraftMatchSettlement, DraftPauseReason } from "../network/draftProtocol";
import type { BrokerClient, RegisterHostRequest } from "../services/brokerClient";
import { loadDraftHostSession } from "../services/draftPersistence";
import type { DraftIntergameCommand, DraftIntergameCommandAck } from "../services/intergameCommandLedger";
import type { DraftWorkspaceState } from "../components/draft/workspace/types";

// ── Types ──────────────────────────────────────────────────────────────

export type DraftPodHostStatus =
  | "idle"
  | "connecting"
  | "lobby"
  | "drafting"
  | "deckbuilding"
  | "pairing"
  | "matchInProgress"
  | "roundComplete"
  | "complete"
  | "error";

export type DraftPodHostEvent =
  | { type: "statusChanged"; status: DraftPodHostStatus }
  | { type: "roomCreated"; roomCode: string }
  | { type: "workspaceRestored"; workspaceState: DraftWorkspaceState | null }
  | { type: "viewUpdated"; view: DraftPlayerView }
  | { type: "lobbyUpdate"; seats: SeatPublicView[]; joined: number; total: number }
  | { type: "lobbyFull" }
  | { type: "draftStarted"; view: DraftPlayerView }
  /** `cardInstanceIds` = the cards this seat drafted in this step, on both the normal and the draft-effect path. */
  | { type: "pickReceived"; seatIndex: number; cardInstanceIds: string[] }
  | { type: "roundComplete" }
  | { type: "draftComplete" }
  | { type: "deckSubmitted"; seatIndex: number }
  | { type: "allDecksSubmitted" }
  | { type: "draftPaused"; reason: DraftPauseReason }
  | { type: "draftResumed" }
  | { type: "seatJoined"; seatIndex: number; displayName: string }
  | { type: "seatReconnected"; seatIndex: number }
  | { type: "seatDisconnected"; seatIndex: number }
  | { type: "seatKicked"; seatIndex: number; reason: DraftPauseReason | string }
  | { type: "pairingsGenerated"; round: number; pairings: PairingView[] }
  | { type: "matchStart"; launch: DraftMatchLaunch }
  /**
   * CR 903.13a: the completed Commander pod's launch into ONE shared N-seat
   * game. `handleHostEvent` in the store is typed on THIS union, not on
   * `DraftHostEvent`, so the member has to exist here for the host's own launch
   * to reach the store at all.
   */
  | { type: "commanderLaunch"; launch: DraftCommanderLaunch }
  | { type: "matchResultReceived"; matchId: string; winnerSeat: number | null }
  | { type: "roundAdvanced" }
  | { type: "timerExpired" }
  | {
      type: "bo3SideboardPrompt";
      matchId: string;
      gameNumber: number;
      score: MatchScore;
      loserSeat: number | null;
      timerMs: number;
    }
  | {
      type: "bo3ChoosePlayDraw";
      matchId: string;
      gameNumber: number;
      score: MatchScore;
      timerMs: number;
    }
  | { type: "bo3GameStart"; matchId: string; gameNumber: number; firstPlayerSeat: number }
  | { type: "bo3SideboardPromptSent"; matchId: string }
  | { type: "bo3BothSideboardsSubmitted"; matchId: string }
  | { type: "bo3GameStarted"; matchId: string; gameNumber: number }
  | { type: "bo3AuthorizedCommand"; command: DraftIntergameCommand; acknowledgement: DraftIntergameCommandAck }
  | { type: "error"; message: string };

type DraftPodHostEventListener = (event: DraftPodHostEvent) => void;

function hostStatusForView(view: DraftPlayerView): DraftPodHostStatus {
  switch (view.status) {
    case "Lobby":
      return "lobby";
    case "Drafting":
    case "Paused":
      return "drafting";
    case "Deckbuilding":
      return "deckbuilding";
    case "Pairing":
      return "pairing";
    case "MatchInProgress":
      return "matchInProgress";
    case "RoundComplete":
      return "roundComplete";
    case "Complete":
      return "complete";
    case "Abandoned":
      return "error";
  }
}

export interface DraftPodHostConfig {
  poolInput: PoolInput;
  kind: Exclude<DraftKind, "Quick">;
  podSize: number;
  hostDisplayName: string;
  /** Swiss (3 rounds) or Single Elimination bracket. */
  tournamentFormat: TournamentFormat;
  /** Competitive (timed) or Casual (untimed, host-controlled). */
  podPolicy: PodPolicy;
  /** Broker client for lobby registration. Optional: P2P works without broker. */
  broker?: BrokerClient;
  /** Broker request for lobby registration. Required if broker is set. */
  brokerRequest?: RegisterHostRequest;
  /** Persistence ID for host crash recovery. */
  persistenceId?: string;
  /** Resume from a specific room code (re-hosts on the same PeerJS ID). */
  preferredRoomCode?: string;
  /** HTTP origin of the selected phase-server for best-effort P2P backups. */
  backupEndpoint?: string;
  /** Abort signal for cancellation during setup. */
  signal?: AbortSignal;
}

// ── DraftPodHostAdapter ────────────────────────────────────────────────

export class DraftPodHostAdapter {
  private listeners: DraftPodHostEventListener[] = [];
  private host: P2PDraftHost | null = null;
  private hostResult: HostResult | null = null;
  private hostEventUnsub: (() => void) | null = null;
  /** Closes an in-flight local host before a replacement may be created. */
  private pendingDispose: (() => Promise<void>) | null = null;
  /** Settles only after a canceled initializer has released every local resource. */
  private pendingInitialization: Promise<void> | null = null;
  private _status: DraftPodHostStatus = "idle";
  private _roomCode: string | null = null;
  private disposed = false;

  onEvent(listener: DraftPodHostEventListener): () => void {
    this.listeners.push(listener);
    return () => {
      this.listeners = this.listeners.filter((l) => l !== listener);
    };
  }

  private emit(event: DraftPodHostEvent): void {
    for (const listener of this.listeners) {
      listener(event);
    }
  }

  private setStatus(status: DraftPodHostStatus): void {
    this._status = status;
    this.emit({ type: "statusChanged", status });
  }

  get status(): DraftPodHostStatus {
    return this._status;
  }

  get roomCode(): string | null {
    return this._roomCode;
  }

  // ── Initialization ─────────────────────────────────────────────────

  /**
   * Create PeerJS peer, optionally register with broker, and start
   * accepting guest connections.
   */
  async initialize(config: DraftPodHostConfig): Promise<void> {
    this.disposed = false;
    this.setStatus("connecting");
    let finishInitialization!: () => void;
    const initializationSettled = new Promise<void>((resolve) => {
      finishInitialization = resolve;
    });
    this.pendingInitialization = initializationSettled;
    let pendingHost: P2PDraftHost | null = null;
    let pendingHostDisposed = false;
    let pendingHostResult: HostResult | null = null;
    let pendingHostResultDestroyed = false;

    const abortIfRequested = () => {
      if (config.signal?.aborted || this.disposed) {
        throw new Error("Draft pod host initialization aborted");
      }
    };
    const disposePending = async () => {
      if (this.hostEventUnsub) {
        this.hostEventUnsub();
        this.hostEventUnsub = null;
      }
      if (pendingHost && !pendingHostDisposed) {
        pendingHostDisposed = true;
        await pendingHost.dispose();
      }
      if (pendingHostResult && !pendingHostResultDestroyed) {
        pendingHostResultDestroyed = true;
        pendingHostResult.destroy();
      }
      if (pendingHostResult) {
        if (this.hostResult === pendingHostResult) this.hostResult = null;
        pendingHostResult = null;
      }
      this._roomCode = null;
    };
    this.pendingDispose = disposePending;

    try {
      // 1. Create PeerJS host peer
      const hostResult = await hostRoom(config.signal, {
        preferredRoomCode: config.preferredRoomCode,
      });
      pendingHostResult = hostResult;
      // `hostRoom` is only cancellation-aware while it is pending. Once it
      // resolves, every following async boundary must re-check before making
      // this peer discoverable or starting its local draft host.
      abortIfRequested();
      this.hostResult = hostResult;
      this._roomCode = hostResult.roomCode;
      this.emit({ type: "roomCreated", roomCode: hostResult.roomCode });

      // 2. Register with lobby broker if provided.
      //
      // Note: no in-tree caller currently builds a brokerRequest for draft
      // pods. When a future caller does, it should populate
      // `draftMetadata.cubeName` from `config.poolInput.data.cube_name` for
      // Cube pods and leave it `undefined` for Set pods. The lobby protocol
      // schema is already forward-ready (see DraftLobbyMetadata, #1253).
      if (config.broker && config.brokerRequest) {
        try {
          await config.broker.registerHost({
            ...config.brokerRequest,
            hostPeerId: hostResult.peerId,
          });
        } catch (err) {
          if (config.signal?.aborted || this.disposed) throw err;
          console.warn("[DraftPodHostAdapter] broker registration failed:", err);
          // Non-fatal: direct room code still works
        }
        abortIfRequested();
      }

      // 3. Two pod shapes need the WASM CARD_DB, for different reasons.
      //    A CUBE pod needs it before create_multiplayer_draft is invoked,
      //    which resolves cube cards against the database. A COMMANDERDRAFT
      //    pod needs it before get_bot_deck, which designates each bot seat's
      //    commander (CR 903.3) and constrains that seat's deck to the
      //    designation's colour identity (CR 903.5c) — both read off a
      //    `CardFace`, so with no database draft-wasm refuses rather than
      //    shipping an unjudged deck. A Set pool for any of the four
      //    CR 905.1a kinds still reads its pool from JSON and needs no
      //    database. A Chaos pool is set-backed too: draft-wasm resolves its
      //    private assignments from supplied pool JSON and needs no card DB.
      //
      //    The kind gate is required, not stylistic: widening this to all
      //    set-backed pools would turn the landed "skips the CARD_DB fetch for Set pods"
      //    row (fixture `kind: "Premier"`) red.
      if (config.poolInput.type === "Cube" || config.kind === "CommanderDraft") {
        const resp = await fetch(__CARD_DATA_URL__);
        abortIfRequested();
        if (!resp.ok) {
          throw new Error(`Failed to load card data: ${resp.status}`);
        }
        const cardData = await resp.text();
        abortIfRequested();
        await new DraftAdapter().loadCardDatabase(cardData);
        abortIfRequested();
      }

      // 4. Create P2PDraftHost
      abortIfRequested();
      const host = new P2PDraftHost(
        hostResult.peer,
        hostResult.onGuestConnected,
        config.poolInput,
        config.kind,
        config.podSize,
        config.hostDisplayName,
        config.tournamentFormat,
        config.podPolicy,
        undefined, // default grace period
        config.persistenceId,
        hostResult.roomCode,
        config.backupEndpoint,
      );
      pendingHost = host;

      // 4. Wire host events
      this.hostEventUnsub = host.onEvent((event) => {
        this.handleHostEvent(event);
      });

      // 5. Check for persisted session to restore
      if (config.persistenceId) {
        const persisted = await loadDraftHostSession(config.persistenceId);
        abortIfRequested();
        if (persisted) {
          const view = await host.restoreFromPersisted(persisted);
          abortIfRequested();
          if (view) {
            this.setStatus(hostStatusForView(view));
            this.emit({ type: "workspaceRestored", workspaceState: host.getHostWorkspaceState() });
            this.emit({ type: "viewUpdated", view });
          }
        }
      }

      // 6. Start accepting connections
      await host.initialize();
      abortIfRequested();
      this.host = host;
      pendingHost = null;
      pendingHostResult = null;
      if (this.pendingDispose === disposePending) this.pendingDispose = null;

      if (this._status === "connecting") {
        this.setStatus("lobby");
      }
    } catch (err) {
      await disposePending();
      if (config.signal?.aborted || this.disposed) {
        this.setStatus("idle");
        throw err;
      }
      this.setStatus("error");
      const message = err instanceof Error ? err.message : String(err);
      this.emit({ type: "error", message });
      throw err;
    } finally {
      finishInitialization();
      if (this.pendingDispose === disposePending) this.pendingDispose = null;
      if (this.pendingInitialization === initializationSettled) {
        this.pendingInitialization = null;
      }
    }
  }

  // ── Host event mapping ─────────────────────────────────────────────

  private handleHostEvent(event: DraftHostEvent): void {
    switch (event.type) {
      case "seatJoined":
        this.emit({
          type: "seatJoined",
          seatIndex: event.seatIndex,
          displayName: event.displayName,
        });
        break;
      case "seatReconnected":
        this.emit({ type: "seatReconnected", seatIndex: event.seatIndex });
        break;
      case "seatDisconnected":
        this.emit({ type: "seatDisconnected", seatIndex: event.seatIndex });
        break;
      case "seatKicked":
        this.emit({
          type: "seatKicked",
          seatIndex: event.seatIndex,
          reason: event.reason,
        });
        break;
      case "lobbyUpdate":
        this.emit({
          type: "lobbyUpdate",
          seats: event.seats,
          joined: event.joined,
          total: event.total,
        });
        break;
      case "lobbyFull":
        this.emit({ type: "lobbyFull" });
        break;
      case "draftStarted":
        this.setStatus(hostStatusForView(event.view));
        this.emit({ type: "draftStarted", view: event.view });
        break;
      case "pickReceived":
        this.emit({
          type: "pickReceived",
          seatIndex: event.seatIndex,
          cardInstanceIds: event.cardInstanceIds,
        });
        break;
      case "roundComplete":
        this.emit({ type: "roundComplete" });
        break;
      case "draftComplete":
        this.setStatus("deckbuilding");
        this.emit({ type: "draftComplete" });
        break;
      case "deckSubmitted":
        this.emit({ type: "deckSubmitted", seatIndex: event.seatIndex });
        break;
      case "allDecksSubmitted":
        // Shape B: no status is written here. `allDecksSubmitted` fires for
        // EVERY pod kind, and this arm cannot know which one — a
        // `PostDraftPlay::CompleteImmediately` pod is already `Complete`
        // (draft-core session.rs:902), so writing "pairing" here overwrote the
        // reducer's own answer. The `viewUpdated` the host broadcasts on the
        // next line of its funnel carries the engine-published status, and the
        // `viewUpdated` case below maps it through `hostStatusForView`.
        this.emit({ type: "allDecksSubmitted" });
        break;
      case "draftPaused":
        this.emit({ type: "draftPaused", reason: event.reason });
        break;
      case "draftResumed":
        this.emit({ type: "draftResumed" });
        break;
      case "error":
        this.emit({ type: "error", message: event.message });
        break;
      case "viewUpdated":
        // The engine-published view is the single status authority, matching
        // the restore path (:249-250) and `draftStarted` (:306-307).
        // `setStatus` goes BEFORE the emit, so the store sees `statusChanged`
        // first (writing `phase` and a no-view `saveDraftPodProgress`) and
        // `viewUpdated` second (writing `phase` again and the VIEW-CARRYING
        // `saveDraftPodProgress`). That order is a readability choice, not a
        // correctness one: `saveDraftPodProgress` re-reads meta and writes
        // `view?.pool.length ?? meta.pickCount`, so the no-view form echoes
        // back whatever the view-carrying form persisted rather than clearing
        // it, and either order leaves the same record.
        this.setStatus(hostStatusForView(event.view));
        this.emit({ type: "viewUpdated", view: event.view });
        break;
      case "pairingsGenerated":
        this.setStatus("matchInProgress");
        this.emit({ type: "pairingsGenerated", round: event.round, pairings: event.pairings });
        break;
      case "matchStart":
        this.setStatus("matchInProgress");
        this.emit({ type: "matchStart", launch: event.launch });
        break;
      case "commanderLaunch":
        // Shape B, deliberately UNLIKE `matchStart` above: no status is written
        // here. A Commander launch does not change pod phase — the pod stays
        // `complete`, and the host must stay on `CompleteView` so its
        // launch-in-flight state and Cancel control can render. Writing
        // "matchInProgress" here would be the same overwrite of the reducer's
        // own answer that `allDecksSubmitted` documents above.
        this.emit({ type: "commanderLaunch", launch: event.launch });
        break;
      case "matchResultReceived":
        this.emit({ type: "matchResultReceived", matchId: event.matchId, winnerSeat: event.winnerSeat });
        break;
      case "roundAdvanced":
        this.setStatus("pairing");
        this.emit({ type: "roundAdvanced" });
        break;
      case "timerExpired":
        this.emit({ type: "timerExpired" });
        break;
      case "bo3SideboardPromptSent":
        this.emit({ type: "bo3SideboardPromptSent", matchId: event.matchId });
        break;
      case "bo3BothSideboardsSubmitted":
        this.emit({ type: "bo3BothSideboardsSubmitted", matchId: event.matchId });
        break;
      case "bo3GameStarted":
        this.emit({ type: "bo3GameStarted", matchId: event.matchId, gameNumber: event.gameNumber });
        break;
      case "bo3SideboardPrompt":
        this.emit({
          type: "bo3SideboardPrompt",
          matchId: event.matchId,
          gameNumber: event.gameNumber,
          score: event.score,
          loserSeat: event.loserSeat,
          timerMs: event.timerMs,
        });
        break;
      case "bo3ChoosePlayDraw":
        this.emit({
          type: "bo3ChoosePlayDraw",
          matchId: event.matchId,
          gameNumber: event.gameNumber,
          score: event.score,
          timerMs: event.timerMs,
        });
        break;
      case "bo3GameStart":
        this.emit({
          type: "bo3GameStart",
          matchId: event.matchId,
          gameNumber: event.gameNumber,
          firstPlayerSeat: event.firstPlayerSeat,
        });
        break;
      case "bo3AuthorizedCommand":
        this.emit({ type: "bo3AuthorizedCommand", command: event.command, acknowledgement: event.acknowledgement });
        break;
    }
  }

  // ── Draft actions ──────────────────────────────────────────────────

  async startDraft(botFillEmptySeats = true): Promise<void> {
    if (!this.host) throw new Error("Host not initialized");
    await this.host.startDraft(botFillEmptySeats);
  }

  async submitPick(cardInstanceIds: string[]): Promise<DraftPlayerView> {
    if (!this.host) throw new Error("Host not initialized");
    return this.host.submitHostPick(cardInstanceIds);
  }

  async submitPickWithDraftEffect(
    effectCardInstanceId: string,
    cardInstanceIds: string[],
  ): Promise<DraftPlayerView> {
    if (!this.host) throw new Error("Host not initialized");
    return this.host.submitHostPickWithDraftEffect(effectCardInstanceId, cardInstanceIds);
  }

  async submitDeck(mainDeck: string[], commanders: string[]): Promise<DraftPlayerView> {
    if (!this.host) throw new Error("Host not initialized");
    return this.host.submitHostDeck(mainDeck, commanders);
  }

  async updateWorkspace(state: DraftWorkspaceState): Promise<void> {
    if (!this.host) throw new Error("Host not initialized");
    await this.host.updateHostWorkspace(state);
  }

  async getHostView(): Promise<DraftPlayerView> {
    if (!this.host) throw new Error("Host not initialized");
    return this.host.getHostView();
  }

  // ── Match coordination ──────────────────────────────────────────────

  async generatePairings(): Promise<void> {
    if (!this.host) throw new Error("Host not initialized");
    await this.host.generatePairings();
  }

  async advanceRound(): Promise<void> {
    if (!this.host) throw new Error("Host not initialized");
    await this.host.advanceRound();
  }

  async overrideMatchResult(matchId: string, winnerSeat: number | null): Promise<void> {
    if (!this.host) throw new Error("Host not initialized");
    await this.host.overrideMatchResult(matchId, winnerSeat);
  }

  async submitMatchSettlement(settlement: DraftMatchSettlement): Promise<void> {
    if (!this.host) throw new Error("Host not initialized");
    await this.host.submitHostMatchSettlement(settlement);
  }

  /**
   * CR 903.13a: the N-seat deck payload a completed Commander pod launches a
   * LOCAL multiplayer game from, used above the P2P seat ceiling. The store
   * holds this wrapper, not the underlying `P2PDraftHost`, so every host call
   * goes through a delegate like this one.
   */
  async podCommanderDeckPayload(
    view: DraftPlayerView,
    localSeat: number,
  ): Promise<DraftMatchDeckPayload> {
    if (!this.host) throw new Error("Host not initialized");
    return this.host.podCommanderDeckPayload(view, localSeat);
  }

  /**
   * CR 903.13a: every deck the completed Commander pod's launch needs. Sends
   * nothing — pair it with `sendCommanderLaunches` once the game is up. `view`
   * must be read at call time — see `P2PDraftHost.commanderSeatDecks`.
   */
  async commanderSeatDecks(view: DraftPlayerView, localSeat: number): Promise<CommanderSeatDecks> {
    if (!this.host) throw new Error("Host not initialized");
    return this.host.commanderSeatDecks(view, localSeat);
  }

  /**
   * CR 903.13a: put the pod's launch on every live human seat, the host's own
   * included, from decks `commanderSeatDecks` already computed.
   */
  sendCommanderLaunches(
    view: DraftPlayerView,
    gameId: string,
    roomCode: string,
    decks: CommanderSeatDecks,
  ): void {
    if (!this.host) throw new Error("Host not initialized");
    this.host.sendCommanderLaunches(view, gameId, roomCode, decks);
  }

  async replaceSeatWithBot(seat: number): Promise<void> {
    if (!this.host) throw new Error("Host not initialized");
    await this.host.replaceSeatWithBot(seat);
  }

  // ── Bo3 Between-Games forwarding ───────────────────────────────────

  handleMatchBetweenGames(
    matchId: string,
    gameNumber: number,
    score: MatchScore,
    loserSeat: number | null,
    seatA: number,
    seatB: number,
  ): void {
    if (!this.host) throw new Error("Host not initialized");
    this.host.handleMatchBetweenGames(matchId, gameNumber, score, loserSeat, seatA, seatB);
  }

  submitAuthorized(seat: number, command: DraftIntergameCommand): void {
    if (!this.host) throw new Error("Host not initialized");
    this.host.submitAuthorized(seat, command);
  }

  // ── Host controls ──────────────────────────────────────────────────

  kickPlayer(seat: number, reason?: string): void {
    if (!this.host) throw new Error("Host not initialized");
    this.host.kickPlayer(seat, reason);
  }

  requestPause(): void {
    if (!this.host) throw new Error("Host not initialized");
    this.host.requestPause();
  }

  requestResume(): void {
    if (!this.host) throw new Error("Host not initialized");
    this.host.requestResume();
  }

  get isFull(): boolean {
    return this.host?.isFull ?? false;
  }

  get isStarted(): boolean {
    return this.host?.isStarted ?? false;
  }

  get isPaused(): boolean {
    return this.host?.isPaused ?? false;
  }

  // ── Cleanup ────────────────────────────────────────────────────────

  async dispose(options: { preserveSession?: boolean } = {}): Promise<void> {
    this.disposed = true;
    const pendingDispose = this.pendingDispose;
    const pendingInitialization = this.pendingInitialization;
    if (pendingDispose) await pendingDispose();
    if (pendingInitialization) await pendingInitialization;
    if (this.hostEventUnsub) {
      this.hostEventUnsub();
      this.hostEventUnsub = null;
    }
    if (this.host) {
      if (options.preserveSession) {
        await this.host.dispose();
      } else {
        await this.host.terminateDraft();
      }
      this.host = null;
    }
    if (this.hostResult) {
      this.hostResult.destroy();
      this.hostResult = null;
    }
    this.listeners = [];
    this._roomCode = null;
    this.setStatus("idle");
  }
}
