import { useState } from "react";
import { useNavigate } from "react-router";

import { CardCoverageDashboard } from "../components/controls/CardCoverageDashboard";

const DIFFICULTIES = [
  { id: "VeryEasy", label: "Very Easy" },
  { id: "Easy", label: "Easy" },
  { id: "Medium", label: "Medium" },
  { id: "Hard", label: "Hard" },
  { id: "VeryHard", label: "Very Hard" },
] as const;

export function MenuPage() {
  const navigate = useNavigate();
  const [showCoverage, setShowCoverage] = useState(false);
  const [showDifficulty, setShowDifficulty] = useState(false);

  return (
    <div className="flex min-h-screen flex-col items-center justify-center gap-8">
      <h1 className="text-5xl font-bold tracking-tight">Forge.ts</h1>
      <p className="text-gray-400">Magic: The Gathering Engine</p>

      <div className="flex flex-col gap-4">
        {!showDifficulty ? (
          <>
            <button
              onClick={() => setShowDifficulty(true)}
              className="rounded-lg bg-indigo-600 px-8 py-3 text-lg font-semibold transition-colors hover:bg-indigo-500"
            >
              Play vs AI
            </button>
            <button
              disabled
              className="cursor-not-allowed rounded-lg border border-gray-700 px-8 py-3 text-lg font-semibold text-gray-500"
            >
              Play Online
              <span className="ml-2 text-xs text-gray-600">Coming Soon</span>
            </button>
          </>
        ) : (
          <div className="flex flex-col items-center gap-3">
            <p className="text-sm font-medium text-gray-300">
              Select Difficulty
            </p>
            <div className="flex flex-col gap-2">
              {DIFFICULTIES.map((d) => (
                <button
                  key={d.id}
                  onClick={() =>
                    navigate(`/game?mode=ai&difficulty=${d.id}`)
                  }
                  className="rounded-lg bg-indigo-600 px-8 py-2 text-base font-semibold transition-colors hover:bg-indigo-500"
                >
                  {d.label}
                </button>
              ))}
            </div>
            <button
              onClick={() => setShowDifficulty(false)}
              className="mt-1 text-sm text-gray-400 hover:text-gray-200"
            >
              Back
            </button>
          </div>
        )}

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
