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

type MenuView = "main" | "difficulty" | "online" | "join";

export function MenuPage() {
  const navigate = useNavigate();
  const [showCoverage, setShowCoverage] = useState(false);
  const [menuView, setMenuView] = useState<MenuView>("main");
  const [joinCode, setJoinCode] = useState("");

  const handleJoinSubmit = () => {
    const code = joinCode.trim().toUpperCase();
    if (code) {
      navigate(`/game?mode=join&code=${code}`);
    }
  };

  return (
    <div className="flex min-h-screen flex-col items-center justify-center gap-8">
      <h1 className="text-5xl font-bold tracking-tight">Forge.ts</h1>
      <p className="text-gray-400">Magic: The Gathering Engine</p>

      <div className="flex flex-col gap-4">
        {menuView === "main" && (
          <>
            <button
              onClick={() => setMenuView("difficulty")}
              className="rounded-lg bg-indigo-600 px-8 py-3 text-lg font-semibold transition-colors hover:bg-indigo-500"
            >
              Play vs AI
            </button>
            <button
              onClick={() => setMenuView("online")}
              className="rounded-lg bg-emerald-600 px-8 py-3 text-lg font-semibold transition-colors hover:bg-emerald-500"
            >
              Play Online
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
          </>
        )}

        {menuView === "difficulty" && (
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
              onClick={() => setMenuView("main")}
              className="mt-1 text-sm text-gray-400 hover:text-gray-200"
            >
              Back
            </button>
          </div>
        )}

        {menuView === "online" && (
          <div className="flex flex-col items-center gap-3">
            <p className="text-sm font-medium text-gray-300">
              Multiplayer
            </p>
            <button
              onClick={() => navigate("/game?mode=host")}
              className="rounded-lg bg-emerald-600 px-8 py-3 text-base font-semibold transition-colors hover:bg-emerald-500"
            >
              Host Game
            </button>
            <button
              onClick={() => setMenuView("join")}
              className="rounded-lg bg-cyan-600 px-8 py-3 text-base font-semibold transition-colors hover:bg-cyan-500"
            >
              Join Game
            </button>
            <button
              onClick={() => setMenuView("main")}
              className="mt-1 text-sm text-gray-400 hover:text-gray-200"
            >
              Back
            </button>
          </div>
        )}

        {menuView === "join" && (
          <div className="flex flex-col items-center gap-3">
            <p className="text-sm font-medium text-gray-300">
              Enter Game Code
            </p>
            <input
              type="text"
              value={joinCode}
              onChange={(e) => setJoinCode(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && handleJoinSubmit()}
              placeholder="e.g. ABC123"
              maxLength={10}
              className="w-48 rounded-lg bg-gray-800 px-4 py-2 text-center text-lg font-mono tracking-widest text-white placeholder-gray-500 outline-none ring-1 ring-gray-700 focus:ring-cyan-500"
              autoFocus
            />
            <button
              onClick={handleJoinSubmit}
              disabled={!joinCode.trim()}
              className={`rounded-lg px-8 py-2 text-base font-semibold transition-colors ${
                joinCode.trim()
                  ? "bg-cyan-600 text-white hover:bg-cyan-500"
                  : "cursor-not-allowed bg-gray-700 text-gray-500"
              }`}
            >
              Join
            </button>
            <button
              onClick={() => setMenuView("online")}
              className="mt-1 text-sm text-gray-400 hover:text-gray-200"
            >
              Back
            </button>
          </div>
        )}
      </div>

      {showCoverage && (
        <CardCoverageDashboard onClose={() => setShowCoverage(false)} />
      )}
    </div>
  );
}
