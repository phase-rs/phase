import { memo } from "react";

import type { ObjectId } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { AttachmentChip } from "./AttachmentChip.tsx";

interface AttachmentChipRowProps {
  objectIds: ObjectId[];
}

const VISIBLE_LIMIT = 3;

/**
 * Compact summary of all permanents attached to a host. Replaces the previous
 * 15px tucked-card peek that showed nothing actionable. Anchored as a sibling
 * of PTBox inside the host card's motion.div so it inherits tap rotation.
 */
export const AttachmentChipRow = memo(function AttachmentChipRow({ objectIds }: AttachmentChipRowProps) {
  if (objectIds.length === 0) return null;

  const visible = objectIds.length > VISIBLE_LIMIT
    ? objectIds.slice(0, VISIBLE_LIMIT)
    : objectIds;
  const overflow = objectIds.slice(VISIBLE_LIMIT);
  const collapsed = overflow.length > 0;

  return (
    <div className="absolute -top-2 left-1 right-1 z-30 flex max-w-full flex-wrap justify-center gap-0.5 overflow-visible">
      {visible.map((id) => (
        <AttachmentChip key={id} id={id} glyphOnly={collapsed} />
      ))}
      {collapsed && <OverflowChip ids={overflow} />}
    </div>
  );
});

interface OverflowChipProps {
  ids: ObjectId[];
}

/**
 * `+N` indicator listing remaining attachment names via the native `title`
 * attribute — matches the existing convention at PermanentCard.tsx (counter
 * tooltips, under-attack badge). No popover primitive needed.
 */
const OverflowChip = memo(function OverflowChip({ ids }: OverflowChipProps) {
  const names = useGameStore((s) => {
    const objects = s.gameState?.objects;
    if (!objects) return "";
    // Use a bullet separator instead of a comma — many MTG card names contain
    // commas (e.g., "Sram, Senior Edificer"), which would render an ambiguous
    // tooltip if comma-joined.
    return ids
      .map((id) => objects[id]?.name)
      .filter((n): n is string => typeof n === "string")
      .join(" · ");
  });

  return (
    <span
      title={names}
      className="flex h-4 items-center rounded border border-slate-400/40 bg-slate-500/20 px-1 text-[10px] font-semibold leading-none text-slate-200"
    >
      +{ids.length}
    </span>
  );
});
