import type { GameEvent, PlayerId } from "../adapter/types";
import { useUiStore } from "../stores/uiStore";

type DieRolledEvent = Extract<GameEvent, { type: "DieRolled" }>;
type CoinFlippedEvent = Extract<GameEvent, { type: "CoinFlipped" }>;
type StartingPlayerContestEvent = Extract<GameEvent, { type: "StartingPlayerContest" }>;

/**
 * Fire the starting-player contest overlay from a game-start event batch.
 *
 * The engine emits one `StartingPlayerContest` event carrying the full roll-off
 * by round (round 0 = every seat; each later round = the previous round's
 * tied-max group that rerolled) plus the authoritative winner (CR 103.1). The
 * overlay renders it round-by-round so the winner is always the high roller of
 * the round shown — fixing the prior last-roll-per-player collapse, which could
 * surface an eliminated seat's higher earlier die as beating the winner's lower
 * reroll. `startingPlayer` is the engine's pick (never recomputed here); it
 * equals the event's `winner` by construction. No-ops when no contest ran
 * (explicit play/draw choice).
 */
export function flashStartingPlayerContest(events: GameEvent[], startingPlayer: PlayerId): void {
  const contest = events.find(
    (e): e is StartingPlayerContestEvent => e.type === "StartingPlayerContest",
  );
  if (!contest) return;
  const rounds = contest.data.rounds.map((round) =>
    round.rolls.map(([playerId, value]) => ({ playerId, value })),
  );
  useUiStore.getState().flashDiceRoll({
    kind: "die",
    // CR 103.1: the first-player roll-off is always a d20.
    sides: 20,
    // The decisive (final) round, kept for the no-rounds fallback and keying.
    rolls: rounds[rounds.length - 1] ?? [],
    rounds,
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
  // CR 901.9d / CR 706.7: the symbolic planar die emits DieRolled with a null
  // result (no numeric face to animate); drop those before building the overlay.
  const numericDice = dice.filter(
    (e): e is DieRolledEvent & { data: { result: number } } => e.data.result !== null,
  );
  if (numericDice.length > 0) {
    flash({
      kind: "die",
      sides: numericDice[0].data.sides,
      rolls: numericDice.map((e) => ({ playerId: e.data.player_id, value: e.data.result })),
      context: "ability",
    });
  }
  if (coin) {
    flash({ kind: "coin", playerId: coin.data.player_id, won: coin.data.won, context: "ability" });
  }
}
