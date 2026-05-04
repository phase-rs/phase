import { useCallback, useEffect, useState } from "react";

import { useDraftStore } from "../../stores/draftStore";
import { menuButtonClass } from "../menu/buttonStyles";
import type { MenuButtonTone } from "../menu/buttonStyles";

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

const DIFFICULTY_TONES: MenuButtonTone[] = [
  "emerald",
  "blue",
  "amber",
  "red",
  "purple",
];

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
          fetch(__DRAFT_POOLS_URL__),
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
        <h3 className="text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
          Bot Difficulty
        </h3>
        <div className="flex flex-wrap gap-2">
          {DIFFICULTY_LABELS.map((label, idx) => (
            <button
              key={label}
              onClick={() => setDifficulty(idx)}
              className={menuButtonClass({
                tone: difficulty === idx ? DIFFICULTY_TONES[idx] : "neutral",
                size: "sm",
              })}
            >
              {label}
            </button>
          ))}
        </div>
      </div>

      {/* Set grid */}
      <div className="flex flex-col gap-2">
        <h3 className="text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
          Choose a Set
        </h3>

        {loading && (
          <div className="py-8 text-center text-sm text-white/40">
            Loading available sets...
          </div>
        )}

        {error && (
          <div className="py-4 text-center text-sm text-red-300">{error}</div>
        )}

        {!loading && !error && sets.length === 0 && (
          <div className="py-8 text-center text-sm text-white/40">
            No draft pools available. Run the draft data pipeline first.
          </div>
        )}

        <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5">
          {sets.map(({ code, name, icon }) => (
            <button
              key={code}
              onClick={() => handleSetClick(code)}
              className="flex cursor-pointer flex-col items-center gap-2 rounded-[16px] border border-white/10 bg-black/18 p-4 backdrop-blur-md transition-colors hover:border-white/20 hover:bg-white/8"
            >
              {icon ? (
                <img
                  src={icon}
                  alt={`${name} set icon`}
                  className="h-10 w-10 invert opacity-80"
                />
              ) : (
                <span className="text-2xl font-bold tracking-wider text-white">
                  {code}
                </span>
              )}
              <span className="text-center text-xs leading-tight text-white/50">
                {name}
              </span>
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}
