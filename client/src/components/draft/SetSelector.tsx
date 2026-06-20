import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { BotDifficultySelector } from "./BotDifficultySelector";

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
  onStartDraft: (setCode: string, setName: string) => void;
}

// ── Component ───────────────────────────────────────────────────────────

export function SetSelector({ onStartDraft }: SetSelectorProps) {
  const { t } = useTranslation("draft");

  const [sets, setSets] = useState<Array<{ code: string; name: string; icon?: string; releasedAt: string }>>([]);
  const [loading, setLoading] = useState(true);
  // `null` = no error. `{ detail }` carries a technical message; `detail`
  // undefined means "use the generic translated fallback". Translation happens
  // at render so the load effect never closes over `t` (avoids re-fetch on
  // language change).
  const [error, setError] = useState<{ detail?: string } | null>(null);

  useEffect(() => {
    let cancelled = false;

    async function loadSets() {
      try {
        const [poolsResp, setsResp] = await Promise.all([
          fetch(__DRAFT_POOLS_URL__),
          fetch(__SCRYFALL_SETS_URL__),
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
          setError({ detail: err instanceof Error ? err.message : undefined });
        }
      } finally {
        if (!cancelled) setLoading(false);
      }
    }

    loadSets();
    return () => { cancelled = true; };
  }, []);

  const handleSetClick = useCallback(
    (code: string, name: string) => { onStartDraft(code, name); },
    [onStartDraft],
  );

  return (
    <div className="flex flex-col gap-6">
      <BotDifficultySelector />

      {/* Set grid */}
      <div className="flex flex-col gap-2">
        <h3 className="text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
          {t("setSelector.chooseSet")}
        </h3>

        {error && (
          <div className="py-4 text-center text-sm text-red-300">
            {error.detail ?? t("setSelector.loadFailed")}
          </div>
        )}

        {!loading && !error && sets.length === 0 && (
          <div className="py-8 text-center text-sm text-white/40">
            {t("setSelector.noPools")}
          </div>
        )}

        <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5">
          {loading
            ? Array.from({ length: 10 }, (_, i) => (
                <div
                  key={i}
                  className="flex animate-pulse flex-col items-center gap-2 rounded-[16px] border border-white/8 bg-black/18 p-4"
                >
                  <div className="h-10 w-10 rounded-full bg-white/10" />
                  <div className="h-2.5 w-3/4 rounded bg-white/8" />
                </div>
              ))
            : sets.map(({ code, name, icon }) => (
                <button
                  key={code}
                  onClick={() => handleSetClick(code, name)}
                  className="flex cursor-pointer flex-col items-center gap-2 rounded-[16px] border border-white/10 bg-black/18 p-4 backdrop-blur-md transition-colors hover:border-white/20 hover:bg-white/8"
                >
                  {icon ? (
                    <img
                      src={icon}
                      alt={t("setSelector.setIconAlt", { name })}
                      className="h-10 w-10 invert opacity-80"
                    />
                  ) : (
                    <span className="text-2xl font-bold tracking-wider text-white">
                      {code}
                    </span>
                  )}
                  <span className="text-center text-xs leading-tight text-white/55">
                    {name}
                  </span>
                </button>
              ))}
        </div>
      </div>
    </div>
  );
}
