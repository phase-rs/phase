import type { PlayerId } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { PermanentCard } from "../board/PermanentCard.tsx";

interface Props {
  playerId: PlayerId;
}

/**
 * Renders the Auras enchanting `playerId` as a horizontal strip alongside the
 * player's HUD plate. Player-host attachments (Curse cycle, Faith's Fetters)
 * have no other rendering surface — `partitionByType` (battlefieldProps.ts)
 * filters them out of the main battlefield rows because the convention is
 * "attached permanents render through their host", and players have no
 * `attachments` back-link of their own.
 *
 * The list comes from `gameState.derived.auras_attached_to_player`, an
 * engine-authored projection (see `crates/engine/src/game/derived_views.rs`).
 * Per CLAUDE.md, the FE does NOT scan the battlefield for `attached_to.type
 * === "Player"` itself — that's game logic, owned by the engine. This
 * component is a pure renderer of the engine-provided list.
 *
 * Each Aura renders through the standard `<PermanentCard>` so all interaction
 * (hover, inspect, debug-highlight, target selection, ability activation)
 * behaves identically to any other permanent — no parallel render path.
 */
export function PlayerAttachedAuras({ playerId }: Props) {
  const auraIds = useGameStore(
    (s) => s.gameState?.derived?.auras_attached_to_player?.[String(playerId)] ?? EMPTY,
  );

  if (auraIds.length === 0) return null;

  return (
    <div
      className="flex shrink-0 items-center gap-1"
      data-testid={`player-attached-auras-${playerId}`}
      aria-label={`Auras enchanting player ${playerId}`}
    >
      {auraIds.map((id) => (
        <PermanentCard key={id} objectId={id} />
      ))}
    </div>
  );
}

// Stable empty reference so unrelated state changes don't churn the selector.
const EMPTY: readonly never[] = [];
