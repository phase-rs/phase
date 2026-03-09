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

export function BattlefieldRow({ groups }: BattlefieldRowProps) {
  if (groups.length === 0) return null;

  return (
    <div className="flex min-h-[calc(var(--card-h)+8px)] flex-wrap items-center justify-center gap-2 px-2">
      {groups.map((group) => (
        <GroupedPermanentDisplay key={group.ids[0]} group={group} />
      ))}
    </div>
  );
}
