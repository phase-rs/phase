/**
 * Engine Web Worker — owns a dedicated WASM instance and handles all engine operations.
 *
 * The main thread communicates via postMessage with typed request/response messages.
 * This worker owns the authoritative game state — the main thread never loads WASM directly.
 */
import init, {
  ping,
  take_last_panic_message,
  initialize_game,
  submit_action,
  get_game_state,
  get_filtered_game_state,
  get_ai_action,
  get_ai_scored_candidates,
  select_action_from_scores,
  get_legal_actions_js,
  get_legal_actions_for_viewer_js,
  get_viewer_snapshot_js,
  restore_game_state,
  resume_multiplayer_host_state,
  load_card_database,
  build_ai_card_subset,
  evaluate_deck_compatibility_js,
  apply_seat_mutation,
  project_seat_view,
  export_game_state_json,
  clear_game_state,
  set_multiplayer_mode,
  resolve_all,
  estimate_bracket_for_deck,
  has_replay_recording,
  export_replay_log,
  load_replay_for_playback,
  replay_length_js,
  replay_header_js,
  replay_seek_js,
  clear_replay_playback,
  preview_mana_payment_js,
  get_card_face_data,
  get_card_parse_details,
  get_card_rulings,
} from "@wasm/engine";

import type { GameAction } from "./types";
import type { BracketDeckRequest } from "../types/bracketEstimate";

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
      firstPlayer?: number;
    }
  | { type: "submitAction"; id: number; actor: number; action: GameAction }
  | { type: "previewManaPayment"; id: number; actor: number; action: GameAction }
  | { type: "getState"; id: number }
  | { type: "getFilteredState"; id: number; viewerId: number }
  | { type: "getLegalActions"; id: number }
  | { type: "getSnapshot"; id: number }
  | { type: "getLegalActionsForViewer"; id: number; viewerId: number }
  | { type: "getViewerSnapshot"; id: number; viewerId: number }
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
  | { type: "resumeMultiplayerHostState"; id: number; stateJson: string }
  | { type: "exportState"; id: number }
  | { type: "loadCardDbFromUrl"; id: number }
  | { type: "buildAiCardSubset"; id: number }
  | { type: "evaluateDeckCompatibility"; id: number; request: unknown }
  | { type: "getCardFaceData"; id: number; cardName: string }
  | { type: "getCardParseDetails"; id: number; cardName: string }
  | { type: "getCardRulings"; id: number; cardName: string }
  | { type: "resetGame"; id: number }
  | { type: "setMultiplayerMode"; id: number; enabled: boolean }
  | { type: "ping"; id: number }
  | { type: "takeLastPanic"; id: number }
  | { type: "applySeatMutation"; id: number; stateJson: string; mutationJson: string }
  | { type: "projectSeatView"; id: number; stateJson: string }
  | { type: "resolveAll"; id: number; requester: number; aiSeatsJson: string; maxResolutions: number }
  | { type: "estimateBracketForDeck"; id: number; deck: BracketDeckRequest }
  | { type: "hasReplayRecording"; id: number }
  | { type: "exportReplayLog"; id: number }
  | { type: "loadReplayForPlayback"; id: number; replayJson: string }
  | { type: "replayLength"; id: number }
  | { type: "replayHeader"; id: number }
  | { type: "replaySeek"; id: number; target: number }
  | { type: "clearReplayPlayback"; id: number };

type EngineResponse =
  | { type: "ready" }
  | { type: "result"; id: number; data: unknown }
  | { type: "error"; id: number; message: string; bracketViolation?: true };

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

function bracketViolationError(id: number, message: string): void {
  respond({ type: "error", id, message, bracketViolation: true });
}

// ── Message Handler ──────────────────────────────────────────────────────

self.onmessage = async (e: MessageEvent<EngineRequest>) => {
  const msg = e.data;

  try {
    switch (msg.type) {
      case "init": {
        if (__ENGINE_WASM_URL__) {
          await init({ module_or_path: __ENGINE_WASM_URL__ });
        } else {
          await init();
        }
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

      case "buildAiCardSubset": {
        if (!cardDbLoaded) {
          error(
            msg.id,
            "Card database not loaded. Call loadCardDb or loadCardDbFromUrl first.",
          );
          break;
        }
        // Returns the serialized AiCardSubsetResult tagged union as a string.
        result(msg.id, build_ai_card_subset());
        break;
      }

      case "evaluateDeckCompatibility": {
        if (!cardDbLoaded) {
          error(
            msg.id,
            "Card database not loaded. Call loadCardDb or loadCardDbFromUrl first.",
          );
          break;
        }
        const data = evaluate_deck_compatibility_js(msg.request);
        result(msg.id, data);
        break;
      }

      case "getCardFaceData": {
        result(msg.id, get_card_face_data(msg.cardName));
        break;
      }

      case "getCardParseDetails": {
        result(msg.id, get_card_parse_details(msg.cardName));
        break;
      }

      case "getCardRulings": {
        result(msg.id, get_card_rulings(msg.cardName));
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
          msg.firstPlayer ?? undefined,
        );
        // Engine returns { error: true, cedh_bracket_violation: true, reasons: [...] }
        // when the cEDH bracket lock fires. Preserve the violation flag so the
        // client can throw a typed BracketViolationError rather than matching
        // on a raw string substring.
        if (
          gameResult &&
          typeof gameResult === "object" &&
          "error" in gameResult &&
          gameResult.error
        ) {
          const envelope = gameResult as { reasons?: string[]; cedh_bracket_violation?: boolean };
          const reasons = envelope.reasons ?? [];
          const message = `Deck validation failed: ${reasons.join("; ")}`;
          if (envelope.cedh_bracket_violation) {
            bracketViolationError(msg.id, message);
          } else {
            error(msg.id, message);
          }
          break;
        }
        result(msg.id, {
          events: gameResult.events ?? [],
          log_entries: gameResult.log_entries ?? [],
        });
        break;
      }

      case "submitAction": {
        if (
          !cardDbLoaded &&
          msg.action?.type === "Debug" &&
          msg.action?.data?.type === "CreateCard"
        ) {
          const resp = await fetch(__CARD_DATA_URL__);
          if (resp.ok) {
            const text = await resp.text();
            load_card_database(text);
            cardDbLoaded = true;
          }
        }
        const actionResult = submit_action(msg.actor, msg.action);
        if (typeof actionResult === "string") {
          // Rust's submit_action error contract: returns the error string
          // on failure. `NOT_INITIALIZED:` prefix signals state-loss —
          // forward verbatim so the adapter can classify it as STATE_LOST.
          error(msg.id, actionResult);
          break;
        }
        result(msg.id, {
          events: actionResult.events ?? [],
          log_entries: actionResult.log_entries ?? [],
        });
        break;
      }

      case "previewManaPayment": {
        const sources = preview_mana_payment_js(msg.actor, msg.action);
        if (typeof sources === "string") {
          error(msg.id, sources);
          break;
        }
        result(msg.id, sources);
        break;
      }

      case "getState": {
        const state = get_game_state();
        // null means the WASM thread-local `GAME_STATE` is None. Previously
        // we substituted a fresh default state here, which would poison
        // IndexedDB via the dispatch.ts saveGame call. Surface as a real
        // error so the adapter classifies it STATE_LOST and the recovery
        // layer can rehydrate from the last-known-good state.
        if (state === null) {
          error(msg.id, "NOT_INITIALIZED: get_game_state returned null");
          break;
        }
        result(msg.id, state);
        break;
      }

      case "getFilteredState": {
        const state = get_filtered_game_state(msg.viewerId);
        if (state === null) {
          error(msg.id, "NOT_INITIALIZED: get_filtered_game_state returned null");
          break;
        }
        result(msg.id, state);
        break;
      }

      case "getLegalActions": {
        const r = get_legal_actions_js();
        if (r === null) {
          error(msg.id, "NOT_INITIALIZED: get_legal_actions_js returned null");
          break;
        }
        result(msg.id, r);
        break;
      }

      case "getSnapshot": {
        // Atomicity guarantee: these two reads form ONE synchronous block with
        // no yield point between them, and the only engine mutation
        // (`submit_action`) is itself a single synchronous call. This handler
        // is `async` and handlers CAN interleave at await points (e.g.
        // submitAction's Debug/CreateCard card-DB fetch), so the absence of an
        // `await` between the two calls below is exactly what makes the pair
        // atomic: a snapshot can never observe a half-applied action, nor
        // straddle two engine versions.
        const state = get_game_state();
        const legalResult = get_legal_actions_js();
        if (state === null || legalResult === null) {
          error(msg.id, "NOT_INITIALIZED: get_game_state/get_legal_actions_js returned null");
          break;
        }
        result(msg.id, { state, legalResult });
        break;
      }

      case "getLegalActionsForViewer": {
        const r = get_legal_actions_for_viewer_js(msg.viewerId);
        if (r === null) {
          error(msg.id, "NOT_INITIALIZED: get_legal_actions_for_viewer_js returned null");
          break;
        }
        result(msg.id, r);
        break;
      }

      case "getViewerSnapshot": {
        const r = get_viewer_snapshot_js(msg.viewerId);
        if (r === null) {
          error(msg.id, "NOT_INITIALIZED: get_viewer_snapshot_js returned null");
          break;
        }
        result(msg.id, r);
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

      case "resumeMultiplayerHostState": {
        resume_multiplayer_host_state(msg.stateJson);
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

      case "setMultiplayerMode": {
        set_multiplayer_mode(msg.enabled);
        result(msg.id, null);
        break;
      }

      case "ping": {
        result(msg.id, ping());
        break;
      }

      case "takeLastPanic": {
        // Pulls + clears the panic captured by the Rust panic hook in
        // engine-wasm/src/lib.rs. Called by the adapter after a STATE_LOST
        // sentinel so we can distinguish a transient state-loss (no panic)
        // from a real engine crash (panic captured) — the latter must NOT
        // be retried because the same input will re-panic.
        result(msg.id, take_last_panic_message() ?? null);
        break;
      }

      case "applySeatMutation": {
        const delta = apply_seat_mutation(msg.stateJson, msg.mutationJson);
        result(msg.id, delta ?? null);
        break;
      }

      case "projectSeatView": {
        const view = project_seat_view(msg.stateJson);
        result(msg.id, view ?? null);
        break;
      }

      case "resolveAll": {
        const r = resolve_all(msg.requester, msg.aiSeatsJson, msg.maxResolutions);
        if (typeof r === "string") {
          error(msg.id, r);
          break;
        }
        result(msg.id, r);
        break;
      }

      case "estimateBracketForDeck": {
        // Pure, stateless — does not require an active game state. Returns
        // null when the deck has no commander or the card database is not
        // loaded yet (engine returns Option::None in those cases).
        const estimate = estimate_bracket_for_deck(msg.deck);
        result(msg.id, estimate ?? null);
        break;
      }

      // ── Replay system ────────────────────────────────────────────────
      // Recording lives alongside GAME_STATE in WASM (see initializeGame /
      // submitAction above) — these calls just surface it. Playback
      // (loadReplayForPlayback / replaySeek / replayLength / replayHeader /
      // clearReplayPlayback) is independent of GAME_STATE entirely.

      case "hasReplayRecording": {
        result(msg.id, has_replay_recording());
        break;
      }

      case "exportReplayLog": {
        // export_replay_log / load_replay_for_playback return Result<T, JsValue>
        // on the Rust side — wasm-bindgen throws on Err, which the outer
        // try/catch around this switch already converts to an error response.
        result(msg.id, export_replay_log());
        break;
      }

      case "loadReplayForPlayback": {
        result(msg.id, load_replay_for_playback(msg.replayJson));
        break;
      }

      case "replayLength": {
        result(msg.id, replay_length_js());
        break;
      }

      case "replayHeader": {
        result(msg.id, replay_header_js() ?? null);
        break;
      }

      case "replaySeek": {
        // replay_seek_js returns Result<JsValue, JsValue> on the Rust side —
        // `null` only for "no replay loaded"; a reconstruction desync throws,
        // which the outer try/catch around this switch converts to an error
        // response instead of silently returning null for both cases.
        result(msg.id, replay_seek_js(msg.target));
        break;
      }

      case "clearReplayPlayback": {
        clear_replay_playback();
        result(msg.id, null);
        break;
      }
    }
  } catch (err) {
    const id = "id" in msg ? (msg as { id: number }).id : -1;
    error(id, err instanceof Error ? err.message : String(err));
  }
};
