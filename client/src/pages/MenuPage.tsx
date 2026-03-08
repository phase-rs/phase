import { useState } from "react";
import { useNavigate } from "react-router";

import { CardCoverageDashboard } from "../components/controls/CardCoverageDashboard";

export function MenuPage() {
  const navigate = useNavigate();
  const [showCoverage, setShowCoverage] = useState(false);

  return (
    <div className="flex min-h-screen flex-col items-center justify-center gap-8">
      <h1 className="text-5xl font-bold tracking-tight">Forge.ts</h1>
      <p className="text-gray-400">Magic: The Gathering Engine</p>
      <div className="flex flex-col gap-4">
        <button
          onClick={() => navigate("/game")}
          className="rounded-lg bg-indigo-600 px-8 py-3 text-lg font-semibold transition-colors hover:bg-indigo-500"
        >
          New Game
        </button>
        <button
          onClick={() => navigate("/deck-builder")}
          className="rounded-lg border border-gray-600 px-8 py-3 text-lg font-semibold transition-colors hover:border-gray-400"
        >
          Deck Builder
        </button>
        <button
          onClick={() => setShowCoverage(true)}
          className="rounded-lg border border-gray-600 px-8 py-3 text-lg font-semibold transition-colors hover:border-gray-400"
        >
          Card Coverage
        </button>
      </div>

      {showCoverage && (
        <CardCoverageDashboard onClose={() => setShowCoverage(false)} />
      )}
    </div>
  );
}
