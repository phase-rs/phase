import { useGameStore } from "../stores/gameStore.ts";
import type { PlayerId } from "../adapter/types.ts";

const NEUTRAL = "#9CA3AF";

/** Stable per-seat palette. Indexed by seat_order position, not raw PlayerId,
 *  so "your color" depends on seat rather than an arbitrary server-assigned id. */
export const SEAT_COLORS = [
  "#22D3EE",
  "#F43F5E",
  "#A78BFA",
  "#F59E0B",
  "#34D399",
  "#60A5FA",
] as const;

/** Pure resolver — safe to call from non-hook contexts (log renderers, etc.). */
export function getSeatColor(
  playerId: PlayerId | null | undefined,
  seatOrder: PlayerId[] | undefined,
): string {
  if (playerId == null) return NEUTRAL;
  const idx = seatOrder?.indexOf(playerId) ?? -1;
  if (idx < 0) return NEUTRAL;
  return SEAT_COLORS[idx % SEAT_COLORS.length];
}

export function useSeatColor(playerId: PlayerId | null | undefined): string {
  const seatOrder = useGameStore((s) => s.gameState?.seat_order);
  return getSeatColor(playerId, seatOrder);
}
