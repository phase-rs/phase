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
  initialize_multiplayer_host_game,
  submit_action,
  submit_interaction_js,
  get_game_state,
  get_filtered_game_state,
  get_ai_action_proposal,
  get_ai_action_proposal_with_diagnostics,
  get_ai_tactical_action_proposal,
  get_ai_tactical_action_proposal_with_diagnostics,
  get_ai_action_proposal_from_scores,
  get_ai_action_proposal_from_scores_with_diagnostics,
  get_ai_scored_candidates,
  submit_ai_action_proposal,
  get_legal_actions_js,
  get_legal_actions_for_viewer_js,
  get_viewer_snapshot_js,
  restore_game_state,
  resume_restored_game_state,
  resume_multiplayer_host_state,
  load_card_database,
  build_ai_card_subset,
  evaluate_deck_compatibility_js,
  evaluateDeckFormatGate,
  customFormatFromLobbyConfig,
  formatConfigForCustomRules,
  apply_seat_mutation,
  project_seat_view,
  export_game_state_json,
  clear_game_state,
  set_multiplayer_mode,
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

import { isActionOutcome, type ActionRejection, type AiActionProposal, type GameAction } from "./types";
import type { InteractionSubmission } from "./generated/interaction";
import type { BracketDeckRequest } from "../types/bracketEstimate";
import { classifyInitFailure, type InitFailure } from "./init-envelope";

// ── Message Protocol ─────────────────────────────────────────────────────

type EngineRequest =
  | { type: "init"; id: number }
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
  | {
      type: "initializeMultiplayerHostGame";
      id: number;
      deckData: unknown | null;
      seed: number;
      formatConfig: unknown | null;
      matchConfig: unknown | null;
      playerCount?: number;
      firstPlayer?: number;
    }
  | { type: "submitAction"; id: number; actor: number; action: GameAction }
  | { type: "submitInteraction"; id: number; actor: number; submission: InteractionSubmission }
  | { type: "previewManaPayment"; id: number; actor: number; action: GameAction }
  | { type: "getState"; id: number }
  | { type: "getFilteredState"; id: number; viewerId: number }
  | { type: "getLegalActions"; id: number }
  | { type: "getSnapshot"; id: number }
  | { type: "getLegalActionsForViewer"; id: number; viewerId: number }
  | { type: "getViewerSnapshot"; id: number; viewerId: number }
  | { type: "getAiActionProposal"; id: number; difficulty: string; playerId: number }
  | { type: "getAiActionProposalWithDiagnostics"; id: number; difficulty: string; playerId: number }
  | { type: "getAiTacticalActionProposal"; id: number; difficulty: string; playerId: number }
  | { type: "getAiTacticalActionProposalWithDiagnostics"; id: number; difficulty: string; playerId: number }
  | { type: "getAiScoredCandidates"; id: number; difficulty: string; playerId: number; seed: number }
  | { type: "getAiActionProposalFromScores"; id: number; scoresJson: string; difficulty: string; playerId: number; seed: number }
  | { type: "getAiActionProposalFromScoresWithDiagnostics"; id: number; scoresJson: string; difficulty: string; playerId: number; seed: number }
  | { type: "submitAiActionProposal"; id: number; proposal: AiActionProposal }
  | { type: "restoreState"; id: number; stateJson: string }
  | { type: "resumeRestoredGameState"; id: number }
  | { type: "resumeMultiplayerHostState"; id: number; stateJson: string }
  | { type: "exportState"; id: number }
  | { type: "loadCardDbFromUrl"; id: number }
  | { type: "buildAiCardSubset"; id: number }
  | { type: "evaluateDeckCompatibility"; id: number; request: unknown }
  | { type: "evaluateDeckFormatGate"; id: number; request: unknown }
  | { type: "customFormatFromLobbyConfig"; id: number; name: string; formatConfig: unknown }
  | { type: "formatConfigForCustomRules"; id: number; customRules: unknown }
  | { type: "getCardFaceData"; id: number; cardName: string }
  | { type: "getCardParseDetails"; id: number; cardName: string }
  | { type: "getCardRulings"; id: number; cardName: string }
  | { type: "resetGame"; id: number }
  | { type: "setMultiplayerMode"; id: number; enabled: boolean }
  | { type: "ping"; id: number }
  | { type: "takeLastPanic"; id: number }
  | { type: "applySeatMutation"; id: number; stateJson: string; mutationJson: string }
  | { type: "projectSeatView"; id: number; stateJson: string }
  | { type: "estimateBracketForDeck"; id: number; deck: BracketDeckRequest }
  | { type: "hasReplayRecording"; id: number }
  | { type: "exportReplayLog"; id: number }
  | { type: "loadReplayForPlayback"; id: number; replayJson: string }
  | { type: "replayLength"; id: number }
  | { type: "replayHeader"; id: number }
  | { type: "replaySeek"; id: number; target: number }
  | { type: "clearReplayPlayback"; id: number };

type EngineResponse =
  | { type: "result"; id: number; data: unknown }
  | {
      type: "error";
      id: number;
      message: string;
      bracketViolation?: true;
      engineOccupied?: true;
      actionRejection?: ActionRejection;
    };

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

function rejectionError(id: number, rejection: ActionRejection): void {
  respond({ type: "error", id, message: rejection.message, actionRejection: rejection });
}

function malformedOutcomeError(id: number): void {
  respond({
    type: "error",
    id,
    message: "The engine rejected that action.",
    actionRejection: undefined,
  });
}

/**
 * Raise an initialize-envelope failure, preserving its typed discriminator so
 * `EngineWorkerClient` can rebuild a typed `AdapterError` on the main thread
 * rather than matching on the message text.
 */
function initFailureError(id: number, failure: InitFailure): void {
  switch (failure.kind) {
    case "bracketViolation":
      respond({ type: "error", id, message: failure.message, bracketViolation: true });
      break;
    case "engineOccupied":
      respond({ type: "error", id, message: failure.message, engineOccupied: true });
      break;
    case "deckValidation":
      error(id, failure.message);
      break;
  }
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
        result(msg.id, null);
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
          error(msg.id, "Card database not loaded. Call loadCardDb or loadCardDbFromUrl first.");
          break;
        }
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

      // The ENFORCING sibling of `evaluateDeckCompatibility`: always returns a
      // definite `{ compatible, reasons }`, never a tri-state. Used by the P2P
      // host's per-guest deck-kick gate, which must not inherit the UI-hint
      // path's "no opinion" answer for Custom formats.
      case "evaluateDeckFormatGate": {
        if (!cardDbLoaded) {
          error(
            msg.id,
            "Card database not loaded. Call loadCardDb or loadCardDbFromUrl first.",
          );
          break;
        }
        result(msg.id, evaluateDeckFormatGate(msg.request));
        break;
      }

      // Custom-format save/select. Neither call touches the card database —
      // they are pure format-schema conversions — so neither gates on it.
      case "customFormatFromLobbyConfig": {
        result(msg.id, customFormatFromLobbyConfig(msg.name, msg.formatConfig));
        break;
      }

      case "formatConfigForCustomRules": {
        result(msg.id, formatConfigForCustomRules(msg.customRules));
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
        const failure = classifyInitFailure(gameResult);
        if (failure) {
          initFailureError(msg.id, failure);
          break;
        }
        result(msg.id, {
          events: gameResult.events ?? [],
          log_entries: gameResult.log_entries ?? [],
        });
        break;
      }

      case "initializeMultiplayerHostGame": {
        if (!cardDbLoaded && msg.deckData) {
          error(
            msg.id,
            "Card database not loaded. Call loadCardDb or loadCardDbFromUrl first.",
          );
          break;
        }
        // The host entry point refuses an engine that already holds a game and
        // claims the multiplayer flag alongside the install — both inside this
        // one synchronous handler, so no other posted message can interleave.
        const gameResult = initialize_multiplayer_host_game(
          msg.deckData ?? null,
          msg.seed,
          msg.formatConfig ?? null,
          msg.matchConfig ?? null,
          msg.playerCount ?? undefined,
          msg.firstPlayer ?? undefined,
        );
        const failure = classifyInitFailure(gameResult);
        if (failure) {
          initFailureError(msg.id, failure);
          break;
        }
        result(msg.id, {
          events: gameResult.events ?? [],
          log_entries: gameResult.log_entries ?? [],
        });
        break;
      }

      case "submitAction": {
        const outcome = submit_action(msg.actor, msg.action);
        if (typeof outcome === "string") {
          // Rust's submit_action error contract: returns the error string
          // on failure. `NOT_INITIALIZED:` prefix signals state-loss —
          // forward verbatim so the adapter can classify it as STATE_LOST.
          error(msg.id, outcome);
          break;
        }
        if (!isActionOutcome(outcome)) {
          malformedOutcomeError(msg.id);
          break;
        }
        if (outcome.status === "rejected") {
          rejectionError(msg.id, outcome.rejection);
          break;
        }
        const actionResult = outcome.result as { events?: unknown[]; log_entries?: unknown[] };
        result(msg.id, {
          events: actionResult.events ?? [],
          log_entries: actionResult.log_entries ?? [],
        });
        break;
      }

      case "submitInteraction": {
        const outcome = submit_interaction_js(msg.actor, msg.submission);
        if (typeof outcome === "string") {
          error(msg.id, outcome);
          break;
        }
        if (!isActionOutcome(outcome)) {
          malformedOutcomeError(msg.id);
          break;
        }
        if (outcome.status === "rejected") {
          rejectionError(msg.id, outcome.rejection);
          break;
        }
        const actionResult = outcome.result as { events?: unknown[]; log_entries?: unknown[] };
        result(msg.id, {
          events: actionResult.events ?? [],
          log_entries: actionResult.log_entries ?? [],
        });
        break;
      }

      case "previewManaPayment": {
        const outcome = preview_mana_payment_js(msg.actor, msg.action);
        if (typeof outcome === "string") {
          error(msg.id, outcome);
          break;
        }
        if (!isActionOutcome(outcome)) {
          malformedOutcomeError(msg.id);
          break;
        }
        if (outcome.status === "rejected") {
          rejectionError(msg.id, outcome.rejection);
          break;
        }
        result(msg.id, outcome.result);
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

      case "getAiActionProposal": {
        const proposal = get_ai_action_proposal(msg.difficulty, msg.playerId);
        result(msg.id, proposal ?? null);
        break;
      }

      case "getAiActionProposalWithDiagnostics": {
        result(msg.id, get_ai_action_proposal_with_diagnostics(msg.difficulty, msg.playerId) ?? null);
        break;
      }

      case "getAiTacticalActionProposal": {
        result(msg.id, get_ai_tactical_action_proposal(msg.difficulty, msg.playerId) ?? null);
        break;
      }

      case "getAiTacticalActionProposalWithDiagnostics": {
        result(msg.id, get_ai_tactical_action_proposal_with_diagnostics(msg.difficulty, msg.playerId) ?? null);
        break;
      }

      case "getAiScoredCandidates": {
        result(msg.id, get_ai_scored_candidates(msg.difficulty, msg.playerId, BigInt(msg.seed)) ?? []);
        break;
      }

      case "getAiActionProposalFromScores": {
        result(
          msg.id,
          get_ai_action_proposal_from_scores(
            msg.scoresJson,
            msg.difficulty,
            msg.playerId,
            BigInt(msg.seed),
          ) ?? null,
        );
        break;
      }

      case "getAiActionProposalFromScoresWithDiagnostics": {
        result(msg.id, get_ai_action_proposal_from_scores_with_diagnostics(msg.scoresJson, msg.difficulty, msg.playerId, BigInt(msg.seed)) ?? null);
        break;
      }

      case "submitAiActionProposal": {
        const outcome = submit_ai_action_proposal(
          msg.proposal.token,
          msg.proposal.actor,
          msg.proposal.action,
        );
        result(msg.id, outcome);
        break;
      }

      case "restoreState": {
        restore_game_state(msg.stateJson);
        result(msg.id, null);
        break;
      }

      case "resumeRestoredGameState": {
        const presentation = resume_restored_game_state();
        result(msg.id, {
          presentation,
          snapshot: {
            state: get_game_state(),
            legalResult: get_legal_actions_js(),
          },
        });
        break;
      }

      case "resumeMultiplayerHostState": {
        const presentation = resume_multiplayer_host_state(msg.stateJson);
        result(msg.id, {
          presentation,
          snapshot: {
            state: get_game_state(),
            legalResult: get_legal_actions_js(),
          },
        });
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
    error(msg.id, err instanceof Error ? err.message : String(err));
  }
};
