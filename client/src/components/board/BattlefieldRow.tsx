import { usePreferencesStore } from "../../stores/preferencesStore.ts";
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
  const battlefieldCardDisplay = usePreferencesStore((s) => s.battlefieldCardDisplay);

  if (groups.length === 0) return null;

  const minH = battlefieldCardDisplay === "art_crop"
    ? "min-h-[calc(var(--art-crop-h)+8px)]"
    : "min-h-[calc(var(--card-h)+8px)]";

  return (
    <div className={`flex ${minH} flex-wrap items-center gap-2 px-2 ${ROW_JUSTIFY[rowType]}`}>
      {groups.map((group) => (
        <GroupedPermanentDisplay key={group.ids[0]} group={group} />
      ))}
    </div>
  );
}
