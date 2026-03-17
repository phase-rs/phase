import { useMemo } from "react";

import type { GameObject, PlayerId } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";

interface CommandZoneProps {
  playerId: PlayerId;
}

interface GroupedEmblem {
  description: string;
  count: number;
  representative: GameObject;
}

function descriptionOf(emblem: GameObject): string {
  return (
    (emblem.static_definitions as Array<{ description?: string }>)
      ?.map((sd) => sd.description)
      .filter(Boolean)
      .join("; ") || "Emblem"
  );
}

/**
 * Renders emblems in the command zone as a compact horizontal strip.
 * Identical emblems are stacked with a count badge (CR 114).
 */
export function CommandZone({ playerId }: CommandZoneProps) {
  const gameState = useGameStore((s) => s.gameState);

  const groups = useMemo(() => {
    if (!gameState) return [];

    const commandZoneIds = gameState.command_zone ?? [];
    const emblems: GameObject[] = commandZoneIds
      .map((id) => gameState.objects[id])
      .filter(
        (obj): obj is GameObject =>
          obj != null && obj.is_emblem === true && obj.controller === playerId,
      );

    // Group identical emblems by description
    const byDesc = new Map<string, GroupedEmblem>();
    for (const emblem of emblems) {
      const desc = descriptionOf(emblem);
      const existing = byDesc.get(desc);
      if (existing) {
        existing.count++;
      } else {
        byDesc.set(desc, { description: desc, count: 1, representative: emblem });
      }
    }

    return [...byDesc.values()];
  }, [gameState, playerId]);

  if (groups.length === 0) return null;

  return (
    <div className="flex items-center gap-1.5">
      {groups.map((group) => (
        <EmblemCard key={group.representative.id} group={group} />
      ))}
    </div>
  );
}

function EmblemCard({ group }: { group: GroupedEmblem }) {
  return (
    <div
      className="relative flex items-center gap-1.5 rounded-md border border-amber-700/50 bg-amber-950/70 px-2 py-1 text-[10px] leading-tight text-amber-200 shadow-sm"
      title={group.description}
    >
      <span className="font-semibold text-amber-400">Emblem</span>
      <span className="max-w-[140px] truncate opacity-80">{group.description}</span>
      {group.count > 1 && (
        <span className="ml-0.5 inline-flex h-4 min-w-4 items-center justify-center rounded-full bg-amber-600 px-1 text-[9px] font-bold text-black">
          ×{group.count}
        </span>
      )}
    </div>
  );
}
