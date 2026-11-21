import type { TFunction } from "i18next";

import type { GameState, PlayerId } from "../adapter/types.ts";
import { useAnimationStore } from "../stores/animationStore.ts";
import { useGameStore } from "../stores/gameStore.ts";
import { formatHalfPointValue, halfPointValue } from "../viewmodel/halfPoints.ts";

/**
 * What a life readout shows: the engine-marked `∞`, or a finite total. `life` is the
 * exact total, Unhinged half point included (17½ is `17.5`).
 */
export type LifeReadout =
  | { magnitude: "Unbounded" }
  | { magnitude: "Finite"; life: number };

/**
 * CR 119.3 + CR 732.2a: the committed snapshot's readout for a seat — the single client authority
 * for whether a life total is `∞`, read from the engine's `derived.life_magnitude`.
 */
export function lifeReadout(
  gameState: GameState | null | undefined,
  playerId: PlayerId,
): LifeReadout {
  if (gameState?.derived?.life_magnitude?.[String(playerId)] === "Unbounded") {
    return { magnitude: "Unbounded" };
  }
  const player = gameState?.players.find((candidate) => candidate.id === playerId);
  return {
    magnitude: "Finite",
    life: player ? halfPointValue(player.life, player.life_fraction) : 0,
  };
}

export function formatLifeReadout(readout: LifeReadout): string {
  switch (readout.magnitude) {
    case "Unbounded":
      return "∞";
    case "Finite":
      return formatHalfPointValue(readout.life);
  }
}

export function lifeReadoutTitle(
  t: TFunction<"game">,
  readout: LifeReadout,
): string | undefined {
  return readout.magnitude === "Unbounded"
    ? t("badges.unboundedTooltip", { resource: t("badges.unboundedLife") })
    : undefined;
}

export function lifeTierClass(readout: LifeReadout): string {
  if (readout.magnitude === "Unbounded" || readout.life >= 10) return "text-green-400";
  return readout.life >= 5 ? "text-yellow-400" : "text-red-400";
}

/**
 * The life readout to render for a player right now.
 *
 * The committed snapshot remains the authority: a finite readout only substitutes an
 * engine-reported total from a hit that has already landed on screen but whose snapshot has not
 * been committed yet, which is the whole animation window of a multi-hit combat. Every life
 * readout goes through here so they all tick on the same beat.
 */
export function useDisplayedLife(playerId: PlayerId): LifeReadout {
  const gameState = useGameStore((state) => state.gameState);
  const displayedLife = useAnimationStore((state) => state.displayedLife);
  const engineCommitEpoch = useGameStore((state) => state.engineCommitEpoch);

  const readout = lifeReadout(gameState, playerId);
  // A landed total is finite, so it never replaces the engine's `∞`.
  if (readout.magnitude === "Unbounded") return readout;

  // Totals recorded before the current snapshot describe an older one; the
  // snapshot itself is then both newer and authoritative.
  if (!displayedLife || displayedLife.epoch !== engineCommitEpoch) return readout;

  const landed = displayedLife.totals.get(playerId);
  return landed === undefined ? readout : { magnitude: "Finite", life: landed };
}
