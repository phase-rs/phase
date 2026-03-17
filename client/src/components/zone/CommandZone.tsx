import type { GameObject, PlayerId } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";

interface CommandZoneProps {
  playerId: PlayerId;
}

/**
 * Renders emblems in the command zone as a compact horizontal strip.
 * Emblems are persistent game objects created by planeswalker abilities (CR 114).
 */
export function CommandZone({ playerId }: CommandZoneProps) {
  const gameState = useGameStore((s) => s.gameState);
  if (!gameState) return null;

  const commandZoneIds = gameState.command_zone ?? [];
  const emblems: GameObject[] = commandZoneIds
    .map((id) => gameState.objects[id])
    .filter(
      (obj): obj is GameObject =>
        obj != null && obj.is_emblem === true && obj.controller === playerId,
    );

  if (emblems.length === 0) return null;

  return (
    <div className="flex items-center gap-1.5">
      {emblems.map((emblem) => (
        <EmblemCard key={emblem.id} emblem={emblem} />
      ))}
    </div>
  );
}

function EmblemCard({ emblem }: { emblem: GameObject }) {
  // Extract a brief description from the emblem's static definitions
  const description =
    (emblem.static_definitions as Array<{ description?: string }>)
      ?.map((sd) => sd.description)
      .filter(Boolean)
      .join("; ") || "Emblem";

  return (
    <div
      className="flex items-center gap-1.5 rounded-md border border-amber-700/50 bg-amber-950/70 px-2 py-1 text-[10px] leading-tight text-amber-200 shadow-sm"
      title={description}
    >
      <span className="font-semibold text-amber-400">Emblem</span>
      <span className="max-w-[140px] truncate opacity-80">{description}</span>
    </div>
  );
}
