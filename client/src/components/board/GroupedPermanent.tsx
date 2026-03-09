import { useState } from "react";

import type { GroupedPermanent as GroupedPermanentType } from "../../viewmodel/battlefieldProps";
import { PermanentCard } from "./PermanentCard.tsx";

interface GroupedPermanentProps {
  group: GroupedPermanentType;
}

export function GroupedPermanentDisplay({ group }: GroupedPermanentProps) {
  const [expanded, setExpanded] = useState(false);

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

  return (
    <div
      className="relative cursor-pointer"
      onClick={() => setExpanded(true)}
    >
      {/* Stacked shadow layers behind */}
      {group.count >= 3 && (
        <div className="absolute left-[4px] top-[4px] z-0 h-full w-full rounded-lg bg-gray-700/40" />
      )}
      {group.count >= 2 && (
        <div className="absolute left-[2px] top-[2px] z-[1] h-full w-full rounded-lg bg-gray-600/50" />
      )}

      {/* Representative card */}
      <div className="relative z-[2]">
        <PermanentCard objectId={group.ids[0]} />
      </div>

      {/* Count badge */}
      <div className="absolute left-1 top-1 z-30 flex h-5 w-5 items-center justify-center rounded-full bg-black/80 text-[10px] font-bold text-white ring-1 ring-gray-500">
        {group.count}
      </div>
    </div>
  );
}
