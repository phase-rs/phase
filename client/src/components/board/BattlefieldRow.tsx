import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import type { GroupedPermanent } from "../../viewmodel/battlefieldProps";
import { GroupedPermanentDisplay } from "./GroupedPermanent.tsx";

interface BattlefieldRowProps {
  groups: GroupedPermanent[];
  rowType: "creatures" | "lands" | "support" | "other";
  className?: string;
}

const ROW_JUSTIFY: Record<string, string> = {
  creatures: "justify-center",
  lands: "justify-start",
  support: "justify-end",
  other: "justify-end",
};

function getCreatureScale(groupCount: number, display: "art_crop" | "full_card"): number {
  if (display === "art_crop") {
    if (groupCount <= 1) return 1.25;
    if (groupCount === 2) return 1.18;
    if (groupCount === 3) return 1.12;
    if (groupCount === 4) return 1.06;
    if (groupCount <= 6) return 1.03;
    return 1;
  }

  if (groupCount <= 1) return 1.12;
  if (groupCount === 2) return 1.08;
  if (groupCount === 3) return 1.05;
  if (groupCount === 4) return 1.02;
  return 1;
}

export function BattlefieldRow({ groups, rowType, className }: BattlefieldRowProps) {
  const battlefieldCardDisplay = usePreferencesStore((s) => s.battlefieldCardDisplay);

  if (groups.length === 0) return null;

  // Art-crop adds ~16px for name label above card
  const minH = battlefieldCardDisplay === "art_crop"
    ? "min-h-[calc(var(--art-crop-h)+24px)]"
    : "min-h-[calc(var(--card-h)+8px)]";
  const creatureScale = rowType === "creatures"
    ? getCreatureScale(groups.length, battlefieldCardDisplay)
    : 1;

  const rowStyle = rowType === "creatures"
    ? ({
        "--art-crop-w": `calc(var(--art-crop-base) * var(--card-size-scale) * var(--art-crop-viewport-scale) * ${creatureScale})`,
        "--art-crop-h": `calc(var(--art-crop-base) * var(--card-size-scale) * var(--art-crop-viewport-scale) * ${creatureScale} * 0.75)`,
        "--card-w": `calc(var(--card-base) * var(--card-size-scale) * var(--card-viewport-scale) * ${creatureScale})`,
        "--card-h": `calc(var(--card-base) * var(--card-size-scale) * var(--card-viewport-scale) * ${creatureScale} * 1.4)`,
      } as React.CSSProperties)
    : undefined;

  return (
    <div
      className={`flex ${minH} flex-wrap items-center gap-2 px-2 ${ROW_JUSTIFY[rowType]} ${className ?? ""}`}
      style={rowStyle}
    >
      {groups.map((group) => (
        <GroupedPermanentDisplay key={group.ids[0]} group={group} />
      ))}
    </div>
  );
}
