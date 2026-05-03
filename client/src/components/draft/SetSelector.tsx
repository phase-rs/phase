import { useCallback, useEffect, useState } from "react";

import { useDraftStore } from "../../stores/draftStore";

// ── Types ───────────────────────────────────────────────────────────────

interface SetPoolEntry {
  name?: string;
  code?: string;
  [key: string]: unknown;
}

interface ScryfallSetEntry {
  name: string;
  icon_svg_uri: string;
  released_at: string;
}

interface SetSelectorProps {
  onStartDraft: (setCode: string) => void;
}

// ── Constants ───────────────────────────────────────────────────────────

const DIFFICULTY_LABELS = [
  "Very Easy",
  "Easy",
  "Medium",
  "Hard",
  "Very Hard",
] as const;

const DIFFICULTY_COLORS = [
  "bg-green-600 hover:bg-green-500",
  "bg-blue-600 hover:bg-blue-500",
  "bg-yellow-600 hover:bg-yellow-500",
  "bg-orange-600 hover:bg-orange-500",
  "bg-red-600 hover:bg-red-500",
] as const;

// ── Component ───────────────────────────────────────────────────────────

export function SetSelector({ onStartDraft }: SetSelectorProps) {
  const difficulty = useDraftStore((s) => s.difficulty);
  const setDifficulty = useDraftStore((s) => s.setDifficulty);

  const [sets, setSets] = useState<Array<{ code: string; name: string; icon?: string; releasedAt: string }>>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;

    async function loadSets() {
      try {
        const [poolsResp, setsResp] = await Promise.all([
          fetch("/draft-pools.json"),
          fetch("/scryfall-sets.json"),
        ]);
        if (!poolsResp.ok) throw new Error(`Failed to load draft pools: ${poolsResp.status}`);

        const pools: Record<string, SetPoolEntry> = await poolsResp.json();
        const scryfallSets: Record<string, ScryfallSetEntry> = setsResp.ok
          ? await setsResp.json()
          : {};

        if (cancelled) return;

        const entries = Object.entries(pools).map(([code, entry]) => ({
          code: code.toUpperCase(),
          name: (entry.name as string) ?? code.toUpperCase(),
          icon: scryfallSets[code]?.icon_svg_uri,
          releasedAt: scryfallSets[code]?.released_at ?? "",
        }));

        entries.sort((a, b) => b.releasedAt.localeCompare(a.releasedAt));
        setSets(entries);
      } catch (err) {
        if (!cancelled) {
          setError(err instanceof Error ? err.message : "Failed to load sets");
        }
      } finally {
        if (!cancelled) setLoading(false);
      }
    }

    loadSets();
    return () => { cancelled = true; };
  }, []);

  const handleSetClick = useCallback(
    (code: string) => { onStartDraft(code); },
    [onStartDraft],
  );

  return (
    <div className="flex flex-col gap-6">
      {/* Difficulty selector */}
      <div className="flex flex-col gap-2">
        <h3 className="text-sm font-medium text-gray-400 uppercase tracking-wider">
          Bot Difficulty
        </h3>
        <div className="flex gap-2 flex-wrap">
          {DIFFICULTY_LABELS.map((label, idx) => (
            <button
              key={label}
              onClick={() => setDifficulty(idx)}
              className={`px-3 py-1.5 rounded text-sm font-medium transition-colors ${
                difficulty === idx
                  ? `${DIFFICULTY_COLORS[idx]} text-white ring-2 ring-white/30`
                  : "bg-gray-700 text-gray-300 hover:bg-gray-600"
              }`}
            >
              {label}
            </button>
          ))}
        </div>
      </div>

      {/* Set grid */}
      <div className="flex flex-col gap-2">
        <h3 className="text-sm font-medium text-gray-400 uppercase tracking-wider">
          Choose a Set
        </h3>

        {loading && (
          <div className="text-gray-400 text-sm py-8 text-center">
            Loading available sets...
          </div>
        )}

        {error && (
          <div className="text-red-400 text-sm py-4 text-center">{error}</div>
        )}

        {!loading && !error && sets.length === 0 && (
          <div className="text-gray-400 text-sm py-8 text-center">
            No draft pools available. Run the draft data pipeline first.
          </div>
        )}

        <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 gap-3">
          {sets.map(({ code, name, icon }) => (
            <button
              key={code}
              onClick={() => handleSetClick(code)}
              className="flex flex-col items-center gap-2 p-4 rounded-lg bg-gray-800 hover:bg-gray-700 border border-gray-700 hover:border-gray-500 transition-colors cursor-pointer"
            >
              {icon ? (
                <img
                  src={icon}
                  alt={`${name} set icon`}
                  className="h-10 w-10 invert opacity-80"
                />
              ) : (
                <span className="text-2xl font-bold text-white tracking-wider">
                  {code}
                </span>
              )}
              <span className="text-xs text-gray-400 text-center leading-tight">
                {name}
              </span>
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}
