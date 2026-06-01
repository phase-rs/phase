import type { GameEvent, PlayerId } from "../adapter/types";
import { useUiStore } from "../stores/uiStore";

type DieRolledEvent = Extract<GameEvent, { type: "DieRolled" }>;
type CoinFlippedEvent = Extract<GameEvent, { type: "CoinFlipped" }>;

/**
 * The leading run of consecutive `DieRolled` events — the starting-player
 * contest the engine emits before `GameStarted` (CR 103.1). Empty when the
 * starter was chosen explicitly (play/draw in setup), so callers no-op.
 */
function leadingDieRolls(events: GameEvent[]): DieRolledEvent[] {
  const out: DieRolledEvent[] = [];
  for (const e of events) {
    if (e.type !== "DieRolled") break;
    out.push(e);
  }
  return out;
}

/**
 * The last roll per player — the decisive round after any tie rerolls — in
 * first-seen seat order. The engine emits every reroll round; for display we
 * show each player's final value.
 */
function finalRollPerPlayer(rolls: DieRolledEvent[]): { playerId: PlayerId; value: number }[] {
  const byPlayer = new Map<PlayerId, number>();
  for (const r of rolls) byPlayer.set(r.data.player_id, r.data.result);
  return [...byPlayer.entries()].map(([playerId, value]) => ({ playerId, value }));
}

/**
 * Fire the starting-player contest overlay from a game-start event batch.
 *
 * `startingPlayer` is the engine's authoritative choice (the player taking turn
 * 1) — never recomputed on the frontend, so the highlighted winner always
 * matches engine state even in the all-tied-at-cap → lowest-seat fallback. The
 * `context` is supplied here (this code path IS the contest), not inferred from
 * event ordering. No-ops on an empty contest (explicit play/draw choice).
 */
export function flashStartingPlayerContest(events: GameEvent[], startingPlayer: PlayerId): void {
  const rolls = leadingDieRolls(events);
  if (rolls.length === 0) return;
  useUiStore.getState().flashDiceRoll({
    kind: "die",
    sides: rolls[0].data.sides,
    rolls: finalRollPerPlayer(rolls),
    context: "startingPlayer",
    winner: startingPlayer,
  });
}

/**
 * Fire the in-game roll overlay for an action's event batch. Groups all
 * `DieRolled` into one die overlay (e.g. a Krark's Thumb double) and otherwise
 * shows the first `CoinFlipped`. Always `context: "ability"`. No-ops when the
 * batch contains neither.
 */
export function flashInGameRolls(events: GameEvent[]): void {
  const dice = events.filter((e): e is DieRolledEvent => e.type === "DieRolled");
  const coin = events.find((e): e is CoinFlippedEvent => e.type === "CoinFlipped");
  const flash = useUiStore.getState().flashDiceRoll;
  // All dice in the batch group into one overlay (e.g. a Krark's Thumb double);
  // a co-occurring coin queues behind them and plays after (the overlay FIFO
  // serializes both rather than dropping either).
  if (dice.length > 0) {
    flash({
      kind: "die",
      sides: dice[0].data.sides,
      rolls: dice.map((e) => ({ playerId: e.data.player_id, value: e.data.result })),
      context: "ability",
    });
  }
  if (coin) {
    flash({ kind: "coin", playerId: coin.data.player_id, won: coin.data.won, context: "ability" });
  }
}
