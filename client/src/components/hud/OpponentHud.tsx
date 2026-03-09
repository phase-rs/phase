import { LifeTotal } from "../controls/LifeTotal.tsx";
import { ManaPoolSummary } from "./ManaPoolSummary.tsx";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";

export function OpponentHud() {
  const hudLayout = usePreferencesStore((s) => s.hudLayout);

  const content = (
    <div className="flex items-center gap-3">
      <LifeTotal playerId={1} />
      <ManaPoolSummary playerId={1} />
    </div>
  );

  if (hudLayout === "floating") {
    return (
      <div className="fixed left-4 top-4 z-30 rounded-lg bg-gray-900/90 px-3 py-2 shadow-lg ring-1 ring-gray-700">
        {content}
      </div>
    );
  }

  return content;
}
