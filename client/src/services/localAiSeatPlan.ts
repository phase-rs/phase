import { AI_DECK_RANDOM, usePreferencesStore, type AiDeckSelection } from "../stores/preferencesStore";
import { effectiveAiDifficulty } from "./cedhLock";
import { loadActiveGame } from "./gamePersistence";

/** One AI seat's engine difficulty (already cEDH-mapped) and requested deck. */
export interface LocalAiSeatPlan {
  difficulty: string;
  deckId: AiDeckSelection;
}

/**
 * Per-seat difficulty + requested deck for a fresh local AI game. The seat
 * snapshot in `ActiveGameMeta` (written by the setup page or by Host Game's
 * all-AI start) wins; seats it does not cover fall back to single-player
 * preferences. Unconfigured seats default to Random — NOT to seat 0, which
 * would clone one pinned deck across every missing seat on a resume fallback.
 */
export function resolveLocalAiSeatPlan(gameId: string, playerCount: number): LocalAiSeatPlan[] {
  const opponentCount = Math.max(1, playerCount - 1);
  const meta = loadActiveGame();
  const snapshot = meta?.id === gameId ? meta.aiSeats : undefined;
  const { aiSeats, cedhMode } = usePreferencesStore.getState();
  return Array.from({ length: opponentCount }, (_, i) => {
    const snapshotSeat = snapshot?.[i];
    const difficulty =
      snapshotSeat?.difficulty
      ?? effectiveAiDifficulty(aiSeats[i]?.difficulty ?? "Medium", cedhMode);
    const deckId: AiDeckSelection =
      snapshotSeat && snapshotSeat.deckId !== undefined
        ? (snapshotSeat.deckId ?? AI_DECK_RANDOM)
        : (aiSeats[i]?.deckId ?? AI_DECK_RANDOM);
    return { difficulty, deckId };
  });
}
