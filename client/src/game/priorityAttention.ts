import type { GameState, PlayerId, WaitingFor } from "../adapter/types";
import { getPlayerId } from "../hooks/usePlayerId";
import { shouldAutoPass } from "./autoPass";

/**
 * Presentation-only timing for the priority-attention cues (glow, chime, tab
 * title). No game rule lives here: *which* windows are meaningful is the
 * engine's call (`autoPassRecommended`, via `shouldAutoPass`) and *who* may
 * submit is the engine-published `priority_player`; this module only decides
 * when a meaningful window is worth announcing to a player who may not be
 * watching.
 */

/** Minimum time away (waiting on someone else, or on auto-passed windows)
 *  before a meaningful window announces itself. Shorter waits mean the player
 *  was watching anyway — an instant AI reply or an opponent's quick pass. */
export const MIN_WAIT_BEFORE_ALERT_MS = 4_000;

/** How long a reactive window can sit unanswered before the glow strengthens.
 *  Planning windows never escalate: sitting on priority while planning a main
 *  phase is thinking, not forgetting. */
export const ESCALATE_AFTER_MS = 15_000;

/** What the local player is doing, as far as attention is concerned. */
export type AttentionObservation =
  /** A priority window worth stopping for, reacting to what the active player
   *  does (an opponent's turn, or any seat other than the active one). Left
   *  unanswered it earns a stronger reminder. `key` tells distinct windows apart
   *  across commits (see `windowKey`). */
  | { kind: "reactive"; key: string }
  /** A priority window worth stopping for on behalf of the active player (your
   *  own turn, or a turn you control): the open "what do I do" window. Sitting
   *  on it is planning, not forgetting, so it never earns a reminder. */
  | { kind: "planning"; key: string }
  /** A decision of their own is pending (attackers, targets, payment, discard…).
   *  It never counts as time away. Announcing such prompts is out of scope: this
   *  feature announces priority windows only. */
  | { kind: "engaged" }
  /** Waiting on someone else, on an auto-passed window, on a prompt with no
   *  single owner (a mulligan), or on nothing. */
  | { kind: "away" };

/**
 * Identifies one priority window among the commits that carry it. An opaque
 * equality token built only from engine-published fields: the turn, the step
 * and which occurrence of it this is (the engine counts the second combat
 * damage step, CR 510.4, and repeated cleanup steps separately), the seat
 * holding semantic priority (it advances even when the authorized submitter
 * does not, as when one player controls two seats) and the identity of every
 * stack entry (a changed stack at the same depth is a new window). None of them
 * change when the store re-commits the same snapshot, and they change when the
 * player acts and the engine hands priority back in the situations the engine
 * distinguishes, so a window that follows an answered one is a new window.
 */
export function windowKey(gameState: GameState, seat: PlayerId): string {
  const stack = gameState.stack.map((entry) => entry.id).join(",");
  const occurrence = gameState.steps_started_this_turn?.[gameState.phase] ?? 0;
  return `${gameState.turn_number}:${gameState.phase}#${occurrence}:${seat}:${stack}`;
}

/**
 * Classify the current engine snapshot for the local player.
 *
 * The submitter authority is `state.priority_player` (CR 723.5: under a turn
 * control effect the semantic player and the authorized submitter diverge;
 * `shouldAutoPass` and the engine's `public_state` both key off it). The
 * controlled player — whose own decision only their controller can submit — is
 * therefore `away`, and so is a spectator. A meaningful priority window is one
 * the same predicate the game loop uses to auto-pass does not swallow, so Full
 * Control and phase stops count and empty pass-only windows do not.
 */
export function observeLocalDecision(
  gameState: GameState | null,
  waitingFor: WaitingFor | null,
  canActNow: boolean,
  fullControl: boolean,
  autoPassRecommended: boolean,
): AttentionObservation {
  if (!gameState || !waitingFor || !canActNow) return { kind: "away" };
  if (gameState.priority_player !== getPlayerId()) return { kind: "away" };
  if (waitingFor.type !== "Priority") return { kind: "engaged" };
  if (shouldAutoPass(gameState, waitingFor, fullControl, autoPassRecommended)) {
    return { kind: "away" };
  }
  // Engine-published seats only: the pending priority seat against the active
  // seat. Stack contents say nothing about urgency, so they play no part here.
  // CR 805.4a + CR 805.5: in a team format the whole team takes the turn and
  // holds priority on it, so a teammate's window is the team's own planning
  // window. The client is told the format but not who is on which team, and must
  // not guess, so every window there is treated as planning: it may miss a
  // reminder for an opposing team's window, but never nags during the player's
  // own team's turn.
  const teamPlay = gameState.format_config?.team_based === true;
  return {
    kind: teamPlay || waitingFor.data.player === gameState.active_player ? "planning" : "reactive",
    key: windowKey(gameState, waitingFor.data.player),
  };
}

export type WindowKind = Extract<AttentionObservation, { key: string }>["kind"];

/** Where a held window stands. `quiet` windows did not announce (the player was
 *  not away long enough) but may still earn the reminder. */
export type WindowStatus = "quiet" | "announced" | "reminded" | "answered";

/** A meaningful window currently held. */
export interface HeldWindow {
  key: string;
  kind: WindowKind;
  /** When this window (or its current reactive stretch) began. */
  openedAt: number;
  status: WindowStatus;
}

export type AttentionPhase =
  /** Not at a decision of their own and not holding a window. */
  | { kind: "away"; since: number }
  | { kind: "engaged" }
  | { kind: "holding"; window: HeldWindow };

export interface AttentionMachine {
  phase: AttentionPhase;
  /** Increments once per announcement (not per reminder); consumers react to changes. */
  fireCount: number;
}

/** `fireCount` carries over so a restarted session keeps counting upward. */
export function initialAttentionMachine(now: number, fireCount = 0): AttentionMachine {
  return { phase: { kind: "away", since: now }, fireCount };
}

function heldWindow(machine: AttentionMachine): HeldWindow | null {
  return machine.phase.kind === "holding" ? machine.phase.window : null;
}

/** What the player should currently be shown, as published to the cues. */
export type AttentionCue = "none" | "announced" | "reminded";

/** `announced` windows glow and chime; a `reminded` one glows stronger. */
export function attentionCue(machine: AttentionMachine): AttentionCue {
  const status = heldWindow(machine)?.status;
  return status === "announced" || status === "reminded" ? status : "none";
}

/** When the reminder is due, or null when there is none to wait for. */
export function escalationDeadline(machine: AttentionMachine): number | null {
  const held = heldWindow(machine);
  if (held === null || held.kind !== "reactive") return null;
  if (held.status !== "quiet" && held.status !== "announced") return null;
  return held.openedAt + ESCALATE_AFTER_MS;
}

/** A new window that replaces one the player had not yet answered keeps its
 *  announcement, a reminder starts over, and an answered one starts quiet. */
function carriedStatus(previous: WindowStatus): WindowStatus {
  if (previous === "answered") return "quiet";
  if (previous === "reminded") return "announced";
  return previous;
}

/**
 * Advance the machine for one observation of the store.
 *
 * - `engaged`: the player is deciding; any held window is closed.
 * - Engagement ending, or a held window closing, starts the idle clock.
 * - A window opening after `MIN_WAIT_BEFORE_ALERT_MS` of idleness, or while the
 *   tab is hidden, announces itself (chime, glow, title). A reactive window that
 *   opened sooner stays quiet but is still held, so it can earn the reminder.
 * - The same window re-committed stays as it is. A different window (its key
 *   or kind changed: the player acted and priority returned) is a fresh one
 *   that carries over an unanswered announcement but never an answer, with its
 *   own reminder (reactive) or none (planning).
 */
export function stepAttention(
  prev: AttentionMachine,
  observation: AttentionObservation,
  now: number,
  visibility: DocumentVisibilityState,
): AttentionMachine {
  switch (observation.kind) {
    case "engaged":
      return prev.phase.kind === "engaged" ? prev : { ...prev, phase: { kind: "engaged" } };
    case "away":
      return prev.phase.kind === "away"
        ? prev
        : { ...prev, phase: { kind: "away", since: now } };
    case "reactive":
    case "planning":
      return stepWindow(prev, observation, now, visibility);
  }
}

function stepWindow(
  prev: AttentionMachine,
  observation: Extract<AttentionObservation, { key: string }>,
  now: number,
  visibility: DocumentVisibilityState,
): AttentionMachine {
  const { kind, key } = observation;
  const hidden = visibility === "hidden";
  const phase = prev.phase;

  if (phase.kind === "holding") {
    const held = phase.window;
    if (held.key === key && held.kind === kind) return prev;
    // A new window the player did not answer carries its announcement; an
    // answered one starts quiet, unless the tab is hidden: then nobody is watching.
    const carried = carriedStatus(held.status);
    const announceNow = hidden && carried === "quiet";
    return {
      fireCount: announceNow ? prev.fireCount + 1 : prev.fireCount,
      phase: {
        kind: "holding",
        window: { key, kind, openedAt: now, status: announceNow ? "announced" : carried },
      },
    };
  }

  // Idleness is measured from the moment the player stopped being engaged.
  const awaySince = phase.kind === "away" ? phase.since : now;
  // A hidden tab proves the player is not watching, however short the wait was.
  const announce = hidden || now - awaySince >= MIN_WAIT_BEFORE_ALERT_MS;
  return {
    fireCount: announce ? prev.fireCount + 1 : prev.fireCount,
    phase: {
      kind: "holding",
      window: { key, kind, openedAt: now, status: announce ? "announced" : "quiet" },
    },
  };
}

/**
 * The tab was hidden while a window sat quiet (opened too soon to announce): the
 * player is not watching it, so it announces now.
 */
export function announceHiddenWindow(prev: AttentionMachine): AttentionMachine {
  const held = heldWindow(prev);
  if (held === null || held.status !== "quiet") return prev;
  return {
    fireCount: prev.fireCount + 1,
    phase: { kind: "holding", window: { ...held, status: "announced" } },
  };
}

/** The reminder deadline passed with the window still unanswered. */
export function escalateAttention(prev: AttentionMachine): AttentionMachine {
  const held = heldWindow(prev);
  if (held === null || escalationDeadline(prev) === null) return prev;
  return { ...prev, phase: { kind: "holding", window: { ...held, status: "reminded" } } };
}

/**
 * The player gave real input (pointer or key): they are here. A held window is
 * answered; otherwise the idle clock restarts, which covers prompts with no
 * single owner (a mulligan) that read as `away`.
 */
export function acknowledgeInput(prev: AttentionMachine, now: number): AttentionMachine {
  const phase = prev.phase;
  switch (phase.kind) {
    case "away":
      return { ...prev, phase: { kind: "away", since: now } };
    case "engaged":
      return prev;
    case "holding":
      if (phase.window.status === "answered") return prev;
      return {
        ...prev,
        phase: { kind: "holding", window: { ...phase.window, status: "answered" } },
      };
  }
}
