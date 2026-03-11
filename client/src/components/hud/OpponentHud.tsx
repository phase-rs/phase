import { LifeTotal } from "../controls/LifeTotal.tsx";
import { ManaPoolSummary } from "./ManaPoolSummary.tsx";

interface OpponentHudProps {
  opponentName?: string | null;
}

export function OpponentHud({ opponentName }: OpponentHudProps) {
  return (
    <div className="flex items-center justify-center py-1">
      <div className="flex items-center gap-2 rounded-full bg-black/50 px-3 py-1">
        {opponentName && (
          <span className="text-xs font-medium text-gray-400">{opponentName}</span>
        )}
        <LifeTotal playerId={1} size="lg" />
        <ManaPoolSummary playerId={1} />
      </div>
    </div>
  );
}
