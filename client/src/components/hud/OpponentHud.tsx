import { LifeTotal } from "../controls/LifeTotal.tsx";
import { ManaPoolSummary } from "./ManaPoolSummary.tsx";

export function OpponentHud() {
  return (
    <div className="flex items-center justify-center py-1">
      <div className="flex items-center gap-2 rounded-full bg-black/50 px-3 py-1">
        <LifeTotal playerId={1} size="lg" />
        <ManaPoolSummary playerId={1} />
      </div>
    </div>
  );
}
