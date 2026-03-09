import { useState } from "react";

import type { GroupedPermanent as GroupedPermanentType } from "../../viewmodel/battlefieldProps";
import { useGameStore } from "../../stores/gameStore.ts";
import { PermanentCard } from "./PermanentCard.tsx";

interface GroupedPermanentProps {
  group: GroupedPermanentType;
}

export function GroupedPermanentDisplay({ group }: GroupedPermanentProps) {
  const [expanded, setExpanded] = useState(false);
  const representativeObj = useGameStore(
    (s) => s.gameState?.objects[group.ids[0]],
  );

  if (group.count === 1) {
    return <PermanentCard objectId={group.ids[0]} />;
  }

  if (expanded) {
    return (
      <div className="flex flex-wrap items-end gap-1">
        {group.ids.map((id) => (
          <PermanentCard key={id} objectId={id} />
        ))}
        <button
          className="rounded bg-gray-800 px-1.5 py-0.5 text-[10px] text-gray-400 hover:bg-gray-700 hover:text-gray-200"
          onClick={() => setExpanded(false)}
        >
          collapse
        </button>
      </div>
    );
  }

  // Staggered render: show each card slightly offset to the right
  const STAGGER_PX = 20;

  return (
    <div
      className="relative cursor-pointer"
      onClick={() => setExpanded(true)}
      style={{
        // Reserve width for staggered cards beyond the first
        paddingRight: `${(group.count - 1) * STAGGER_PX}px`,
      }}
    >
      {/* Each card staggered to the right, last card on top */}
      {group.ids.map((id, i) => (
        <div
          key={id}
          className="absolute top-0"
          style={{
            left: `${i * STAGGER_PX}px`,
            zIndex: i,
          }}
        >
          <PermanentCard objectId={id} />
        </div>
      ))}

      {/* Invisible spacer sized to first card for layout */}
      <div className="invisible">
        <PermanentCard objectId={group.ids[0]} />
      </div>

      {/* Count badge */}
      <div className="absolute left-1 top-1 z-30 flex h-5 w-5 items-center justify-center rounded-full bg-black/80 text-[10px] font-bold text-white ring-1 ring-gray-500">
        {group.count}
      </div>
    </div>
  );
}
