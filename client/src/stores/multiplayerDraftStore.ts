/**
 * Zustand store for multiplayer P2P draft state.
 *
 * Separate from `draftStore` (Quick Draft, single-player) because the
 * multiplayer draft lifecycle is fundamentally different:
 * - Host/guest asymmetry (host runs WASM, guest is stateless receiver)
 * - Lobby phase with seat management before draft starts
 * - Network events (disconnect, reconnect, kick, pause/resume)
 * - Pairing and match handoff after deckbuilding
 *
 * The store wraps `DraftPodHostAdapter` or `DraftPodGuestAdapter` and
 * projects their events into reactive Zustand state for the React UI.
 */

import { create } from "zustand";

import type {
  DraftCardInstance,
  DraftPlayerView,
  PairingView,
  SeatPublicView,
  StandingEntry,
} from "../adapter/draft-adapter";
import type { EngineAdapter, GameEvent, GameLogEntry, MatchScore, SubmitResult } from "../adapter/types";
import type { DraftMatchLaunch, DraftPauseReason } from "../network/draftProtocol";
import type { AISeatBinding } from "../game/controllers/aiController";
import { createGameLoopController, type GameLoopController } from "../game/controllers/gameLoopController";
import { processRemoteUpdate } from "../game/dispatch";
import { legalResultState, useGameStore } from "./gameStore";
import {
  DraftPodHostAdapter,
  type DraftPodHostConfig,
  type DraftPodHostEvent,
  type DraftPodHostStatus,
} from "../adapter/draftPodHostAdapter";
import {
  DraftPodGuestAdapter,
  type DraftPodGuestConfig,
  type DraftPodGuestEvent,
  type DraftPodGuestStatus,
} from "../adapter/draftPodGuestAdapter";
import {
  clearActiveDraftPod,
  loadActiveDraftPod,
  saveActiveDraftPod,
  type ActiveDraftPodMeta,
  type ActiveDraftPodPhase,
} from "../services/draftPersistence";
import { FORMAT_DEFAULTS } from "./multiplayerStore";

// ── Types ──────────────────────────────────────────────────────────────

export type DraftRole = "host" | "guest";

export type MultiplayerDraftPhase =
  | "idle"
  | "connecting"
  | "lobby"
  | "drafting"
  | "deckbuilding"
  | "pairing"
  | "matchInProgress"
  | "betweenGames"
  | "roundComplete"
  | "complete"
  | "error"
  | "kicked"
  | "hostLeft";

export interface PairingInfo {
  round: number;
  table: number;
  opponentName: string;
  matchHostPeerId: string;
  matchId: string;
}

interface MultiplayerDraftState {
  role: DraftRole | null;
  phase: MultiplayerDraftPhase;
  roomCode: string | null;
  draftCode: string | null;
  seatIndex: number | null;
  view: DraftPlayerView | null;
  seats: SeatPublicView[];
  joined: number;
  total: number;
  paused: boolean;
  pauseReason: DraftPauseReason | null;
  pairing: PairingInfo | null;
  error: string | null;
  selectedCard: string | null;
  mainDeck: string[];
  landCounts: Record<string, number>;
  timerRemainingMs: number | null;
  standings: StandingEntry[];
  currentRound: number;
  pairings: PairingView[];
  /** Full deck submitted during deckbuilding (mainDeck + lands). */
  submittedDeck: string[];
  matchPairing: DraftMatchLaunch | null;
  matchAdapter: unknown | null;
  /** Bo3: sideboard prompt state between games. */
  sideboardPrompt: {
    matchId: string;
    gameNumber: number;
    score: MatchScore;
    loserSeat: number | null;
    timerMs: number;
  } | null;
  /** Bo3: play/draw choice prompt. */
  playDrawPrompt: {
    matchId: string;
    gameNumber: number;
    score: MatchScore;
    timerMs: number;
  } | null;
  /** Bo3: whether this player has submitted their sideboard. */
  sideboardSubmitted: boolean;
}

interface MultiplayerDraftActions {
  /** Host: create a new draft pod and start accepting guests. */
  hostDraft: (config: DraftPodHostConfig) => Promise<void>;
  /** Guest: join an existing draft pod by room code. */
  joinDraft: (config: DraftPodGuestConfig) => Promise<void>;
  /** Host: start the draft once the pod is ready. */
  startDraft: (botFillEmptySeats?: boolean) => Promise<void>;
  /** Both: submit a pick. */
  submitPick: (cardInstanceId: string) => Promise<void>;
  /** Both: select a card (UI highlight before confirming pick). */
  selectCard: (cardInstanceId: string | null) => void;
  /** Both: confirm the currently selected card as pick. */
  confirmPick: () => Promise<void>;
  /** Both: pick a card from the current pack using a deterministic draft heuristic. */
  autoPickCard: () => Promise<void>;
  /** Both: add a card to the deck during deckbuilding. */
  addToDeck: (cardName: string) => void;
  /** Both: remove a card from the deck during deckbuilding. */
  removeFromDeck: (cardName: string) => void;
  /** Both: set land count for a specific basic land. */
  setLandCount: (landName: string, count: number) => void;
  /** Both: submit the built deck. */
  submitDeck: () => Promise<void>;
  /** Host: kick a player from the pod. */
  kickPlayer: (seat: number, reason?: string) => void;
  /** Host: pause the draft. */
  requestPause: () => void;
  /** Host: resume the draft. */
  requestResume: () => void;
  /** Both: tear down the connection and reset state. */
  leave: (preserveSession?: boolean) => Promise<void>;
  /** Reset store to initial state (without network cleanup). */
  reset: () => void;
  /** Both: start the match for the current pairing. */
  startMatch: () => Promise<string | null>;
  /** Both: report a match result back to the pod host. */
  reportMatchResult: (matchId: string, winnerSeat: number | null) => Promise<void>;
  /** Both: report the active game result using the current draft match pairing. */
  reportActiveMatchGameResult: (gameWinner: number | null) => Promise<void>;
  /** Both: concede the active draft match and report the opponent as winner. */
  reportActiveMatchConcession: () => Promise<void>;
  /** Host: advance to the next round (Casual mode). */
  advanceRound: () => void;
  /** Host: override a match result (Casual mode). */
  overrideMatchResult: (matchId: string, winnerSeat: number | null) => void;
  /** Host: replace a disconnected player with a bot (Casual mode). */
  replaceSeatWithBot: (seat: number) => void;
  /** Both: submit sideboard between Bo3 games. */
  submitSideboard: (matchId: string, mainDeck: string[], sideboard: Array<{ name: string; count: number }>) => void;
  /** Both: choose play or draw (loser of previous game). */
  choosePlayDraw: (matchId: string, playFirst: boolean) => void;
  /** Both: handle between-games prompt from match adapter. */
  handleBetweenGamesPrompt: (prompt: { matchId: string; gameNumber: number; score: MatchScore; loserSeat: number | null; timerMs: number }) => void;
}

// ── Module-level adapter refs ──────────────────────────────────────────

let activeHostAdapter: DraftPodHostAdapter | null = null;
let activeGuestAdapter: DraftPodGuestAdapter | null = null;
let activeMatchController: GameLoopController | null = null;
const reportedMatchResultIds = new Set<string>();
const DRAFT_MATCH_FORMAT_CONFIG = FORMAT_DEFAULTS.Limited;

const RARITY_SCORE: Record<string, number> = {
  mythic: 4,
  rare: 3,
  uncommon: 2,
  common: 1,
};

function preferredColors(pool: DraftCardInstance[]): Set<string> {
  const counts = new Map<string, number>();
  for (const card of pool) {
    for (const color of card.colors) {
      counts.set(color, (counts.get(color) ?? 0) + 1);
    }
  }

  return new Set(
    [...counts.entries()]
      .sort(([, a], [, b]) => b - a)
      .slice(0, 2)
      .map(([color]) => color),
  );
}

function curveScore(cmc: number, poolSize: number): number {
  if (poolSize < 5) {
    if (cmc <= 2) return 1;
    if (cmc >= 6) return -1;
    return 0;
  }

  if (cmc >= 2 && cmc <= 4) return 2;
  if (cmc >= 6) return -1;
  return 0;
}

function scoreDraftCard(card: DraftCardInstance, colors: Set<string>, poolSize: number): number {
  const rarityScore = (RARITY_SCORE[card.rarity.toLowerCase()] ?? 0) * 2;
  let colorScore = 0;
  if (card.colors.length === 0) {
    colorScore = 1;
  } else if (colors.size > 0) {
    colorScore = card.colors.some((color) => colors.has(color)) ? 3 : -1;
  }

  return rarityScore + colorScore + curveScore(card.cmc, poolSize);
}

function chooseAutoPickCard(view: DraftPlayerView | null): string | null {
  const pack = view?.current_pack;
  if (!pack || pack.length === 0) return null;

  const colors = preferredColors(view.pool);
  let bestCard = pack[0];
  let bestScore = scoreDraftCard(bestCard, colors, view.pool.length);

  for (const card of pack.slice(1)) {
    const score = scoreDraftCard(card, colors, view.pool.length);
    if (score > bestScore) {
      bestCard = card;
      bestScore = score;
    }
  }

  return bestCard.instance_id;
}

function opponentSeatForLaunch(launch: DraftMatchLaunch): number {
  return launch.type === "Bot" ? launch.botSeat : launch.opponentSeat;
}

function winnerSeatForLaunch(launch: DraftMatchLaunch, gameWinner: number | null): number | null {
  if (gameWinner === null) return null;
  return gameWinner === 0 ? launch.localSeat : opponentSeatForLaunch(launch);
}

function guestWinnerSeatForLaunch(launch: DraftMatchLaunch, gameWinner: number | null): number | null {
  if (gameWinner === null) return null;
  return gameWinner === 0 ? opponentSeatForLaunch(launch) : launch.localSeat;
}

function winnerSeatForGameResult(launch: DraftMatchLaunch, gameWinner: number | null): number | null {
  if (gameWinner === null) return null;
  const localGamePlayerId = launch.type === "HumanGuest" ? 1 : 0;
  return gameWinner === localGamePlayerId ? launch.localSeat : opponentSeatForLaunch(launch);
}

function disposeMatchController(): void {
  activeMatchController?.dispose();
  activeMatchController = null;
}

/** The engine-side AI seat for a Bot draft match (the local player is game
 *  player 0, the bot is 1). Single authority shared by the live game-loop
 *  controller and the Resolve All batch drain, so the drain can never play
 *  the bot at a different difficulty than the controller driving it. */
export const DRAFT_BOT_AI_SEAT: AISeatBinding = { playerId: 1, difficulty: "Medium" };

async function installMatchRuntime(
  gameId: string,
  adapter: EngineAdapter,
  initResult: SubmitResult,
  controllerMode: "ai" | "online",
): Promise<void> {
  const state = await adapter.getState();
  const legalResult = await adapter.getLegalActions();
  const initLogEntries: GameLogEntry[] = (initResult.log_entries ?? []).map((entry, i) => ({
    ...entry,
    seq: i,
  }));

  useGameStore.setState({
    gameId,
    gameMode: "draft-match",
    adapter,
    gameState: state,
    waitingFor: state.waiting_for,
    ...legalResultState(legalResult),
    events: [] as GameEvent[],
    eventHistory: [] as GameEvent[],
    logHistory: initLogEntries,
    nextLogSeq: initLogEntries.length,
    stateHistory: [],
    turnCheckpoints: [],
    lobbyProgress: null,
  });

  disposeMatchController();
  activeMatchController = createGameLoopController({
    mode: controllerMode,
    difficulty: DRAFT_BOT_AI_SEAT.difficulty,
    aiSeats: controllerMode === "ai" ? [DRAFT_BOT_AI_SEAT] : undefined,
    playerCount: 2,
  });
  activeMatchController.start();
}

function saveDraftPodProgress(phase: ActiveDraftPodPhase, view?: DraftPlayerView | null): void {
  const meta = loadActiveDraftPod();
  if (!meta) return;
  saveActiveDraftPod({
    ...meta,
    phase,
    pickCount: view?.pool.length ?? meta.pickCount,
    updatedAt: Date.now(),
  });
}

function updateActiveDraftPod(patch: Partial<ActiveDraftPodMeta>): void {
  const meta = loadActiveDraftPod();
  if (!meta) return;
  saveActiveDraftPod({ ...meta, ...patch, updatedAt: Date.now() });
}

function activePhaseForHostStatus(status: DraftPodHostStatus): ActiveDraftPodPhase | null {
  switch (status) {
    case "lobby":
    case "drafting":
    case "deckbuilding":
    case "pairing":
    case "matchInProgress":
    case "complete":
      return status;
    case "roundComplete":
      return "pairing";
    case "idle":
    case "connecting":
    case "error":
      return null;
  }
}

function phaseForDraftViewStatus(status: DraftPlayerView["status"]): MultiplayerDraftPhase {
  switch (status) {
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

function activePhaseForDraftViewStatus(status: DraftPlayerView["status"]): ActiveDraftPodPhase | null {
  switch (status) {
    case "Lobby":
      return "lobby";
    case "Drafting":
    case "Paused":
      return "drafting";
    case "Deckbuilding":
      return "deckbuilding";
    case "Pairing":
    case "RoundComplete":
      return "pairing";
    case "MatchInProgress":
      return "matchInProgress";
    case "Complete":
      return "complete";
    case "Abandoned":
      return null;
  }
}

/** Dispose the active match adapter (P2PHostAdapter or P2PGuestAdapter). */
function disposeMatchAdapter(set: SetFn): void {
  const state = useMultiplayerDraftStore.getState();
  disposeMatchController();
  if (state.matchAdapter) {
    const adapter = state.matchAdapter as { dispose?: () => void };
    adapter.dispose?.();
    if (useGameStore.getState().adapter === state.matchAdapter) {
      useGameStore.setState({
        gameId: null,
        gameState: null,
        events: [],
        eventHistory: [],
        logHistory: [],
        nextLogSeq: 0,
        adapter: null,
        waitingFor: null,
        legalActions: [],
        autoPassRecommended: false,
        spellCosts: {},
        legalActionsByObject: {},
        stateHistory: [],
        turnCheckpoints: [],
      });
    }
    set({ matchAdapter: null, matchPairing: null, sideboardPrompt: null, playDrawPrompt: null, sideboardSubmitted: false });
  }
}

// ── Initial state ──────────────────────────────────────────────────────

const initialState: MultiplayerDraftState = {
  role: null,
  phase: "idle",
  roomCode: null,
  draftCode: null,
  seatIndex: null,
  view: null,
  seats: [],
  joined: 0,
  total: 0,
  paused: false,
  pauseReason: null,
  pairing: null,
  error: null,
  selectedCard: null,
  mainDeck: [],
  landCounts: {},
  timerRemainingMs: null,
  standings: [],
  currentRound: 0,
  pairings: [],
  submittedDeck: [],
  matchPairing: null,
  matchAdapter: null,
  sideboardPrompt: null,
  playDrawPrompt: null,
  sideboardSubmitted: false,
};

// ── Store ──────────────────────────────────────────────────────────────

export const useMultiplayerDraftStore = create<
  MultiplayerDraftState & MultiplayerDraftActions
>()((set, get) => ({
  ...initialState,

  hostDraft: async (config) => {
    const adapter = new DraftPodHostAdapter();
    activeHostAdapter = adapter;

    adapter.onEvent((event) => handleHostEvent(event, set));

    set({
      ...initialState,
      role: "host",
      phase: "connecting",
      seatIndex: 0,
    });

    try {
      await adapter.initialize(config);
      if (config.persistenceId) {
        const view = get().view;
        const phase = view ? activePhaseForDraftViewStatus(view.status) ?? "lobby" : "lobby";
        saveActiveDraftPod({
          id: config.persistenceId,
          roomCode: adapter.roomCode ?? config.preferredRoomCode ?? "",
          kind: config.kind,
          podSize: config.podSize,
          hostDisplayName: config.hostDisplayName,
          tournamentFormat: config.tournamentFormat,
          podPolicy: config.podPolicy,
          phase,
          pickCount: view?.pool.length ?? 0,
          updatedAt: Date.now(),
        });
      }
    } catch {
      // Error already emitted via adapter event
    }
  },

  joinDraft: async (config) => {
    const adapter = new DraftPodGuestAdapter();
    activeGuestAdapter = adapter;

    adapter.onEvent((event) => handleGuestEvent(event, set));

    set({
      ...initialState,
      role: "guest",
      phase: "connecting",
    });

    try {
      await adapter.initialize(config);
    } catch {
      // Error already emitted via adapter event
    }
  },

  startDraft: async (botFillEmptySeats = true) => {
    if (!activeHostAdapter) return;
    await activeHostAdapter.startDraft(botFillEmptySeats);
  },

  submitPick: async (cardInstanceId) => {
    const { role } = get();
    if (role === "host" && activeHostAdapter) {
      const view = await activeHostAdapter.submitPick(cardInstanceId);
      set({ view, selectedCard: null });
    } else if (role === "guest" && activeGuestAdapter) {
      await activeGuestAdapter.submitPick(cardInstanceId);
      set({ selectedCard: null });
    }
  },

  selectCard: (cardInstanceId) => {
    set({ selectedCard: cardInstanceId });
  },

  confirmPick: async () => {
    const { selectedCard, submitPick } = get();
    if (!selectedCard) return;
    await submitPick(selectedCard);
  },

  autoPickCard: async () => {
    const { view, submitPick } = get();
    const cardInstanceId = chooseAutoPickCard(view);
    if (!cardInstanceId) return;
    await submitPick(cardInstanceId);
  },

  addToDeck: (cardName) => {
    set((prev) => ({ mainDeck: [...prev.mainDeck, cardName] }));
  },

  removeFromDeck: (cardName) => {
    set((prev) => {
      const idx = prev.mainDeck.indexOf(cardName);
      if (idx === -1) return prev;
      const next = [...prev.mainDeck];
      next.splice(idx, 1);
      return { mainDeck: next };
    });
  },

  setLandCount: (landName, count) => {
    set((prev) => ({
      landCounts: { ...prev.landCounts, [landName]: Math.max(0, count) },
    }));
  },

  submitDeck: async () => {
    const { role, mainDeck, landCounts } = get();
    const landCards: string[] = [];
    for (const [name, count] of Object.entries(landCounts)) {
      for (let i = 0; i < count; i++) {
        landCards.push(name);
      }
    }
    const fullDeck = [...mainDeck, ...landCards];

    set({ submittedDeck: fullDeck });

    if (role === "host" && activeHostAdapter) {
      const view = await activeHostAdapter.submitDeck(fullDeck);
      set({ view });
    } else if (role === "guest" && activeGuestAdapter) {
      await activeGuestAdapter.submitDeck(fullDeck);
    }
  },

  kickPlayer: (seat, reason) => {
    if (!activeHostAdapter) return;
    activeHostAdapter.kickPlayer(seat, reason);
  },

  requestPause: () => {
    if (!activeHostAdapter) return;
    activeHostAdapter.requestPause();
  },

  requestResume: () => {
    if (!activeHostAdapter) return;
    activeHostAdapter.requestResume();
  },

  startMatch: async () => {
    const { matchPairing, matchAdapter } = get();
    if (!matchPairing) return null;
    const gameId = `draft-match-${matchPairing.matchId}`;
    if (matchAdapter) return gameId;

    try {
      if (matchPairing.type === "HumanHost") {
        // Lower seat# hosts the match (D-09).
        const [{ hostRoom }, { P2PHostAdapter }] = await Promise.all([
          import("../network/connection"),
          import("../adapter/p2p-adapter"),
        ]);

        const host = await hostRoom(undefined, {
          preferredRoomCode: matchPairing.matchRoomCode,
        });

        const matchAdapter = new P2PHostAdapter(
          matchPairing.deckPayload,
          host.peer,
          host.onGuestConnected,
          2, // 1v1 match
          DRAFT_MATCH_FORMAT_CONFIG,
          matchPairing.matchConfig,
        );

        let resolveRoomFull!: () => void;
        const roomFull = new Promise<void>((resolve) => {
          resolveRoomFull = resolve;
        });
        matchAdapter.onEvent((event) => {
          if (event.type === "roomFull") {
            resolveRoomFull();
          }
          if (event.type === "stateChanged") {
            void processRemoteUpdate(event.state, event.events, event.legalResult, event.logEntries);
          }
          if (event.type === "stateChanged") {
            const wf = event.state?.waiting_for;
            if (!wf) return;

            if (wf.type === "GameOver") {
              // Match is complete — report result to pod host
              const winnerSeat = winnerSeatForLaunch(matchPairing, wf.data.winner);
              void get().reportMatchResult(matchPairing.matchId, winnerSeat);
            } else if (wf.type === "BetweenGamesSideboard") {
              // Between games in Bo3 — bridge to draft pod host for sideboard orchestration.
              const score = wf.data.score;
              const gameNumber = wf.data.game_number;
              // Determine loser: the player whose wins are fewer
              const loserSeat = score.p0_wins > score.p1_wins
                ? matchPairing.opponentSeat
                : score.p1_wins > score.p0_wins
                  ? matchPairing.localSeat
                  : null; // draw
              if (activeHostAdapter) {
                activeHostAdapter.handleMatchBetweenGames(
                  matchPairing.matchId,
                  gameNumber,
                  score,
                  loserSeat,
                  matchPairing.localSeat,
                  matchPairing.opponentSeat,
                );
              }
              // Also transition the host's own UI to betweenGames
              get().handleBetweenGamesPrompt({
                matchId: matchPairing.matchId,
                gameNumber,
                score,
                loserSeat,
                timerMs: 0, // Host determines timer internally via podPolicy
              });
            }
          }
          if (event.type === "gameOver") {
            // Connection-level failure — report as match loss
            const winnerSeat = winnerSeatForLaunch(matchPairing, event.winner);
            void get().reportMatchResult(matchPairing.matchId, winnerSeat);
          }
        });

        await matchAdapter.initialize();
        await roomFull;
        const initResult = await matchAdapter.startPregameGame();
        await installMatchRuntime(gameId, matchAdapter, initResult, "online");
        set({ matchAdapter, phase: "matchInProgress" });
        return gameId;
      } else if (matchPairing.type === "HumanGuest") {
        // Higher seat# joins as guest.
        const [{ joinRoom }, { P2PGuestAdapter }] = await Promise.all([
          import("../network/connection"),
          import("../adapter/p2p-adapter"),
        ]);

        const { conn, peer } = await joinRoom(matchPairing.matchRoomCode);

        const matchAdapter = new P2PGuestAdapter(
          {
            player: matchPairing.localDeck,
          },
          peer,
          conn.peer,
          conn,
        );

        matchAdapter.onEvent((event) => {
          if (event.type === "stateChanged") {
            void processRemoteUpdate(event.state, event.events, event.legalResult, event.logEntries);
          }
          if (event.type === "stateChanged") {
            const wf = event.state?.waiting_for;
            if (!wf) return;

            if (wf.type === "GameOver") {
              // Guest reports as backup (host's report is authoritative)
              const winnerSeat = guestWinnerSeatForLaunch(matchPairing, wf.data.winner);
              void get().reportMatchResult(matchPairing.matchId, winnerSeat);
            }
            // BetweenGamesSideboard: guest receives sideboard prompt via draft pod channel
            // (handled by bo3SideboardPrompt event from P2PDraftGuest), not here.
          }
          if (event.type === "gameOver") {
            // Connection failure — report as match loss
            const winnerSeat = guestWinnerSeatForLaunch(matchPairing, event.winner);
            void get().reportMatchResult(matchPairing.matchId, winnerSeat);
          }
        });

        await matchAdapter.initialize();
        const initResult = await matchAdapter.initializeGame();
        await installMatchRuntime(gameId, matchAdapter, initResult, "online");
        set({ matchAdapter, phase: "matchInProgress" });
        return gameId;
      } else {
        const { WasmAdapter } = await import("../adapter/wasm-adapter");
        const matchAdapter = new WasmAdapter();
        await matchAdapter.initialize();
        const initResult = await matchAdapter.initializeGame(
          matchPairing.deckPayload,
          DRAFT_MATCH_FORMAT_CONFIG,
          2,
          matchPairing.matchConfig,
        );
        await installMatchRuntime(gameId, matchAdapter, initResult, "ai");
        set({ matchAdapter, phase: "matchInProgress" });
        return gameId;
      }
    } catch (err) {
      console.error("[multiplayerDraftStore] startMatch failed:", err);
      set({ error: err instanceof Error ? err.message : String(err) });
      return null;
    }
  },

  reportMatchResult: (matchId, winnerSeat) => {
    const { role } = get();
    if (reportedMatchResultIds.has(matchId)) return Promise.resolve();
    if (role === "host" && activeHostAdapter) {
      reportedMatchResultIds.add(matchId);
      return activeHostAdapter.overrideMatchResult(matchId, winnerSeat).catch((err) => {
        reportedMatchResultIds.delete(matchId);
        throw err;
      });
    } else if (role === "guest" && activeGuestAdapter) {
      reportedMatchResultIds.add(matchId);
      activeGuestAdapter.sendMatchResult(matchId, winnerSeat);
    }
    return Promise.resolve();
  },

  reportActiveMatchGameResult: async (gameWinner) => {
    const { matchPairing, reportMatchResult } = get();
    if (!matchPairing) return;
    await reportMatchResult(
      matchPairing.matchId,
      winnerSeatForGameResult(matchPairing, gameWinner),
    );
  },

  reportActiveMatchConcession: async () => {
    const { matchPairing, reportMatchResult } = get();
    if (!matchPairing) return;
    await reportMatchResult(matchPairing.matchId, opponentSeatForLaunch(matchPairing));
  },

  advanceRound: () => {
    if (!activeHostAdapter) return;
    void activeHostAdapter.advanceRound();
  },

  overrideMatchResult: (matchId, winnerSeat) => {
    if (!activeHostAdapter) return;
    void activeHostAdapter.overrideMatchResult(matchId, winnerSeat);
  },

  replaceSeatWithBot: (seat) => {
    if (!activeHostAdapter) return;
    void activeHostAdapter.replaceSeatWithBot(seat);
  },

  submitSideboard: (matchId, mainDeck, sideboard) => {
    const { role } = get();
    if (role === "host" && activeHostAdapter) {
      // Host submits to own P2PDraftHost via DraftPodHostAdapter forwarder (seat 0).
      activeHostAdapter.handleSideboardSubmit(0, matchId, mainDeck, sideboard);
    } else if (role === "guest" && activeGuestAdapter) {
      activeGuestAdapter.sendSideboardSubmit(matchId, mainDeck, sideboard);
    }
    set({ sideboardSubmitted: true });
  },

  choosePlayDraw: (matchId, playFirst) => {
    const { role } = get();
    if (role === "host" && activeHostAdapter) {
      // Host as loser chooses play/draw via DraftPodHostAdapter forwarder (seat 0).
      activeHostAdapter.handlePlayDrawChosen(0, matchId, playFirst);
    } else if (role === "guest" && activeGuestAdapter) {
      activeGuestAdapter.sendPlayDrawChoice(matchId, playFirst);
    }
  },

  handleBetweenGamesPrompt: (prompt) => {
    set({
      phase: "betweenGames",
      sideboardPrompt: {
        matchId: prompt.matchId,
        gameNumber: prompt.gameNumber,
        score: prompt.score,
        loserSeat: prompt.loserSeat,
        timerMs: prompt.timerMs,
      },
      sideboardSubmitted: false,
      playDrawPrompt: null,
      timerRemainingMs: prompt.timerMs > 0 ? prompt.timerMs : null,
    });
  },

  leave: async (preserveSession = false) => {
    // Dispose match adapter first (game P2P connection)
    disposeMatchAdapter(set);
    reportedMatchResultIds.clear();

    if (activeHostAdapter) {
      await activeHostAdapter.dispose({ preserveSession });
      activeHostAdapter = null;
      if (!preserveSession) {
        clearActiveDraftPod();
      }
    }
    if (activeGuestAdapter) {
      await activeGuestAdapter.dispose();
      activeGuestAdapter = null;
    }
    set(initialState);
  },

  reset: () => {
    disposeMatchAdapter(set);
    reportedMatchResultIds.clear();
    set(initialState);
  },
}));

// ── Event handlers ─────────────────────────────────────────────────────

function hostStatusToPhase(status: DraftPodHostStatus): MultiplayerDraftPhase {
  switch (status) {
    case "idle":
      return "idle";
    case "connecting":
      return "connecting";
    case "lobby":
      return "lobby";
    case "drafting":
      return "drafting";
    case "deckbuilding":
      return "deckbuilding";
    case "pairing":
      return "pairing";
    case "matchInProgress":
      return "matchInProgress";
    case "roundComplete":
      return "roundComplete";
    case "complete":
      return "complete";
    case "error":
      return "error";
  }
}

function guestStatusToPhase(status: DraftPodGuestStatus): MultiplayerDraftPhase {
  switch (status) {
    case "idle":
      return "idle";
    case "connecting":
      return "connecting";
    case "lobby":
      return "lobby";
    case "drafting":
      return "drafting";
    case "deckbuilding":
      return "deckbuilding";
    case "matchInProgress":
      return "matchInProgress";
    case "complete":
      return "complete";
    case "kicked":
      return "kicked";
    case "hostLeft":
      return "hostLeft";
    case "error":
      return "error";
  }
}

type SetFn = (
  partial:
    | Partial<MultiplayerDraftState>
    | ((state: MultiplayerDraftState) => Partial<MultiplayerDraftState>),
) => void;

function handleHostEvent(event: DraftPodHostEvent, set: SetFn): void {
  switch (event.type) {
    case "statusChanged":
      set({ phase: hostStatusToPhase(event.status) });
      {
        const activePhase = activePhaseForHostStatus(event.status);
        if (activePhase) saveDraftPodProgress(activePhase);
      }
      break;
    case "roomCreated":
      set({ roomCode: event.roomCode });
      updateActiveDraftPod({ roomCode: event.roomCode });
      break;
    case "viewUpdated":
      set({
        phase: phaseForDraftViewStatus(event.view.status),
        view: event.view,
        timerRemainingMs: event.view.timer_remaining_ms ?? null,
        standings: event.view.standings ?? [],
        currentRound: event.view.current_round ?? 0,
        pairings: event.view.pairings ?? [],
      });
      {
        const activePhase = activePhaseForDraftViewStatus(event.view.status);
        if (activePhase) saveDraftPodProgress(activePhase, event.view);
      }
      break;
    case "lobbyUpdate":
      set({ joined: event.joined, total: event.total, seats: event.seats });
      break;
    case "lobbyFull":
      break;
    case "draftStarted":
      set({ view: event.view, phase: "drafting" });
      saveDraftPodProgress("drafting", event.view);
      break;
    case "draftComplete":
      set({ phase: "deckbuilding" });
      saveDraftPodProgress("deckbuilding");
      break;
    case "allDecksSubmitted":
      set({ phase: "pairing" });
      saveDraftPodProgress("pairing");
      break;
    case "draftPaused":
      set({ paused: true, pauseReason: event.reason });
      break;
    case "draftResumed":
      set({ paused: false, pauseReason: null });
      break;
    case "pairingsGenerated":
      set({ phase: "matchInProgress", currentRound: event.round, pairings: event.pairings });
      saveDraftPodProgress("matchInProgress");
      break;
    case "matchStart":
      set({ matchPairing: event.launch, phase: "matchInProgress" });
      break;
    case "roundAdvanced":
      disposeMatchAdapter(set);
      set({ phase: "pairing", currentRound: event.newRound });
      saveDraftPodProgress("pairing");
      break;
    case "roundComplete":
      disposeMatchAdapter(set);
      break;
    case "matchResultReceived":
      // Informational — standings update comes via viewUpdated
      break;
    case "timerExpired":
      break;
    case "error":
      set({ error: event.message });
      break;
    // Seat events are informational — the lobby update carries the authoritative seat list
    case "seatJoined":
    case "seatReconnected":
    case "seatDisconnected":
    case "seatKicked":
    case "pickReceived":
    case "deckSubmitted":
      break;
    case "bo3SideboardPromptSent":
      // Host UI transition handled by the stateChanged bridge in startMatch.
      break;
    case "bo3BothSideboardsSubmitted":
      // Informational — play/draw prompt or game start follows automatically.
      break;
    case "bo3GameStarted":
      set({ phase: "matchInProgress", sideboardPrompt: null, playDrawPrompt: null, sideboardSubmitted: false });
      saveDraftPodProgress("matchInProgress");
      break;
    case "bo3SideboardPrompt":
      set({
        phase: "betweenGames",
        sideboardPrompt: {
          matchId: event.matchId,
          gameNumber: event.gameNumber,
          score: event.score,
          loserSeat: event.loserSeat,
          timerMs: event.timerMs,
        },
        sideboardSubmitted: false,
        playDrawPrompt: null,
        timerRemainingMs: event.timerMs > 0 ? event.timerMs : null,
      });
      break;
    case "bo3ChoosePlayDraw":
      set({
        playDrawPrompt: {
          matchId: event.matchId,
          gameNumber: event.gameNumber,
          score: event.score,
          timerMs: event.timerMs,
        },
        timerRemainingMs: event.timerMs > 0 ? event.timerMs : null,
      });
      break;
    case "bo3GameStart":
      set({
        phase: "matchInProgress",
        sideboardPrompt: null,
        playDrawPrompt: null,
        sideboardSubmitted: false,
      });
      break;
  }
}

function handleGuestEvent(event: DraftPodGuestEvent, set: SetFn): void {
  switch (event.type) {
    case "statusChanged":
      set({ phase: guestStatusToPhase(event.status) });
      break;
    case "joined":
      set({
        seatIndex: event.seatIndex,
        draftCode: event.draftCode,
        phase: "lobby",
      });
      break;
    case "reconnected":
      set({ seatIndex: event.seatIndex });
      break;
    case "viewUpdated":
      set({
        phase: phaseForDraftViewStatus(event.view.status),
        view: event.view,
        timerRemainingMs: event.view.timer_remaining_ms ?? null,
        standings: event.view.standings ?? [],
        currentRound: event.view.current_round ?? 0,
        pairings: event.view.pairings ?? [],
      });
      break;
    case "pickAcknowledged":
      set({ view: event.view });
      break;
    case "lobbyUpdate":
      set({ seats: event.seats, joined: event.joined, total: event.total });
      break;
    case "draftPaused":
      set({ paused: true, pauseReason: event.reason });
      break;
    case "draftResumed":
      set({ paused: false, pauseReason: null });
      break;
    case "pairing":
      set({
        pairing: {
          round: event.round,
          table: event.table,
          opponentName: event.opponentName,
          matchHostPeerId: event.matchHostPeerId,
          matchId: event.matchId,
        },
      });
      break;
    case "matchResult":
      break;
    case "timerSync":
      set({ timerRemainingMs: event.remainingMs });
      break;
    case "matchStart":
      set({
        matchPairing: event.launch,
        phase: "matchInProgress",
      });
      break;
    case "kicked":
      set({ phase: "kicked", error: event.reason });
      break;
    case "hostLeft":
      set({ phase: "hostLeft", error: event.reason });
      break;
    case "error":
      set({ error: event.message });
      break;
    case "reconnecting":
    case "reconnectFailed":
      break;
    case "bo3SideboardPrompt":
      set({
        phase: "betweenGames",
        sideboardPrompt: {
          matchId: event.matchId,
          gameNumber: event.gameNumber,
          score: event.score,
          loserSeat: event.loserSeat,
          timerMs: event.timerMs,
        },
        sideboardSubmitted: false,
        playDrawPrompt: null,
        timerRemainingMs: event.timerMs > 0 ? event.timerMs : null,
      });
      break;
    case "bo3ChoosePlayDraw":
      set({
        playDrawPrompt: {
          matchId: event.matchId,
          gameNumber: event.gameNumber,
          score: event.score,
          timerMs: event.timerMs,
        },
        timerRemainingMs: event.timerMs > 0 ? event.timerMs : null,
      });
      break;
    case "bo3GameStart":
      set({
        phase: "matchInProgress",
        sideboardPrompt: null,
        playDrawPrompt: null,
        sideboardSubmitted: false,
      });
      break;
    case "bo3ScoreUpdate":
      // Informational — standings update comes via viewUpdated
      break;
  }
}
