import type { GroupedPermanent } from "../../viewmodel/battlefieldProps";
import { GroupedPermanentDisplay } from "./GroupedPermanent.tsx";

interface BattlefieldRowProps {
  groups: GroupedPermanent[];
  rowType: "creatures" | "lands" | "other";
}

const ROW_LABELS: Record<string, string> = {
  creatures: "Creatures",
  lands: "Lands",
  other: "Other",
};

export function BattlefieldRow({ groups, rowType }: BattlefieldRowProps) {
  if (groups.length === 0) return null;

  return (
    <div className="flex min-h-[calc(var(--card-h)+8px)] flex-wrap items-center gap-2 px-2">
      <span className="text-[10px] font-medium uppercase tracking-wider text-gray-600 [writing-mode:vertical-lr]">
        {ROW_LABELS[rowType]}
      </span>
      {groups.map((group) => (
        <GroupedPermanentDisplay key={group.ids[0]} group={group} />
      ))}
    </div>
  );
}
