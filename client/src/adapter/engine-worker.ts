/**
 * Engine Web Worker — owns a dedicated WASM instance and handles all engine operations.
 *
 * The main thread communicates via postMessage with typed request/response messages.
 * This worker owns the authoritative game state — the main thread never loads WASM directly.
 */
import init, {
  ping,
  create_initial_state,
  initialize_game,
  submit_action,
  get_game_state,
  get_filtered_game_state,
  get_ai_action,
  get_ai_scored_candidates,
  select_action_from_scores,
  get_legal_actions_js,
  restore_game_state,
  load_card_database,
  export_game_state_json,
  clear_game_state,
} from "@wasm/engine";

import type { GameAction } from "./types";

// ── Message Protocol ─────────────────────────────────────────────────────

type EngineRequest =
  | { type: "init" }
  | { type: "loadCardDb"; id: number; cardDataText: string }
  | {
      type: "initializeGame";
      id: number;
      deckData: unknown | null;
      seed: number;
      formatConfig: unknown | null;
      matchConfig: unknown | null;
      playerCount?: number;
    }
  | { type: "submitAction"; id: number; action: GameAction }
  | { type: "getState"; id: number }
  | { type: "getFilteredState"; id: number; viewerId: number }
  | { type: "getLegalActions"; id: number }
  | { type: "getAiAction"; id: number; difficulty: string; playerId: number }
  | {
      type: "getAiScoredCandidates";
      id: number;
      difficulty: string;
      playerId: number;
      seed: number;
    }
  | {
      type: "selectActionFromScores";
      id: number;
      scoresJson: string;
      difficulty: string;
      seed: number;
    }
  | { type: "restoreState"; id: number; stateJson: string }
  | { type: "exportState"; id: number }
  | { type: "loadCardDbFromUrl"; id: number }
  | { type: "resetGame"; id: number }
  | { type: "ping"; id: number };

type EngineResponse =
  | { type: "ready" }
  | { type: "result"; id: number; data: unknown }
  | { type: "error"; id: number; message: string };

// ── State ────────────────────────────────────────────────────────────────

let cardDbLoaded = false;

function respond(msg: EngineResponse): void {
  self.postMessage(msg);
}

function result(id: number, data: unknown): void {
  respond({ type: "result", id, data });
}

function error(id: number, message: string): void {
  respond({ type: "error", id, message });
}

// ── Message Handler ──────────────────────────────────────────────────────

self.onmessage = async (e: MessageEvent<EngineRequest>) => {
  const msg = e.data;

  try {
    switch (msg.type) {
      case "init": {
        await init();
        respond({ type: "ready" });
        break;
      }

      case "loadCardDb": {
        const count = load_card_database(msg.cardDataText);
        cardDbLoaded = true;
        result(msg.id, count);
        break;
      }

      case "loadCardDbFromUrl": {
        const resp = await fetch(__CARD_DATA_URL__);
        if (!resp.ok)
          throw new Error(
            `Failed to load card-data.json (${resp.status})`,
          );
        const text = await resp.text();
        const count = load_card_database(text);
        cardDbLoaded = true;
        result(msg.id, count);
        break;
      }

      case "initializeGame": {
        if (!cardDbLoaded && msg.deckData) {
          error(
            msg.id,
            "Card database not loaded. Call loadCardDb or loadCardDbFromUrl first.",
          );
          break;
        }
        const gameResult = initialize_game(
          msg.deckData ?? null,
          msg.seed,
          msg.formatConfig ?? null,
          msg.matchConfig ?? null,
          msg.playerCount ?? undefined,
        );
        // Engine returns { error: true, reasons: [...] } when deck validation fails
        if (
          gameResult &&
          typeof gameResult === "object" &&
          "error" in gameResult &&
          gameResult.error
        ) {
          const reasons =
            (gameResult as { reasons?: string[] }).reasons ?? [];
          error(
            msg.id,
            `Deck validation failed: ${reasons.join("; ")}`,
          );
          break;
        }
        result(msg.id, {
          events: gameResult.events ?? [],
          log_entries: gameResult.log_entries ?? [],
        });
        break;
      }

      case "submitAction": {
        const actionResult = submit_action(msg.action);
        if (typeof actionResult === "string") {
          error(msg.id, actionResult);
          break;
        }
        result(msg.id, {
          events: actionResult.events ?? [],
          log_entries: actionResult.log_entries ?? [],
        });
        break;
      }

      case "getState": {
        const state = get_game_state();
        result(msg.id, state === null ? create_initial_state() : state);
        break;
      }

      case "getFilteredState": {
        const state = get_filtered_game_state(msg.viewerId);
        result(msg.id, state === null ? create_initial_state() : state);
        break;
      }

      case "getLegalActions": {
        const r = get_legal_actions_js();
        result(msg.id, r === null ? { actions: [], autoPassRecommended: false } : r);
        break;
      }

      case "getAiAction": {
        const aiResult = get_ai_action(msg.difficulty, msg.playerId);
        result(msg.id, aiResult ?? null);
        break;
      }

      case "getAiScoredCandidates": {
        const scored = get_ai_scored_candidates(
          msg.difficulty,
          msg.playerId,
          BigInt(msg.seed),
        );
        result(msg.id, scored ?? []);
        break;
      }

      case "selectActionFromScores": {
        const selected = select_action_from_scores(
          msg.scoresJson,
          msg.difficulty,
          BigInt(msg.seed),
        );
        result(msg.id, selected ?? null);
        break;
      }

      case "restoreState": {
        restore_game_state(msg.stateJson);
        result(msg.id, null);
        break;
      }

      case "exportState": {
        const json = export_game_state_json();
        result(msg.id, json);
        break;
      }

      case "resetGame": {
        clear_game_state();
        result(msg.id, null);
        break;
      }

      case "ping": {
        result(msg.id, ping());
        break;
      }
    }
  } catch (err) {
    const id = "id" in msg ? (msg as { id: number }).id : -1;
    error(id, err instanceof Error ? err.message : String(err));
  }
};
