import type { GroupedPermanent } from "../../viewmodel/battlefieldProps";
import { GroupedPermanentDisplay } from "./GroupedPermanent.tsx";

interface BattlefieldRowProps {
  groups: GroupedPermanent[];
  rowType: "creatures" | "lands" | "other";
}

const ROW_JUSTIFY: Record<string, string> = {
  creatures: "justify-center",
  lands: "justify-start",
  other: "justify-end",
};

export function BattlefieldRow({ groups, rowType }: BattlefieldRowProps) {
  if (groups.length === 0) return null;

  return (
    <div className={`flex min-h-[calc(var(--card-h)+8px)] flex-wrap items-center gap-2 px-2 ${ROW_JUSTIFY[rowType]}`}>
      {groups.map((group) => (
        <GroupedPermanentDisplay key={group.ids[0]} group={group} />
      ))}
    </div>
  );
}
