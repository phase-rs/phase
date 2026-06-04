import { useCallback, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";

import type { GameFormat } from "../../adapter/types";
import { useCardImage } from "../../hooks/useCardImage";
import { stashPlaytestDeck } from "../../services/playtestDeck";
import type { DeckEntry, ParsedDeck } from "../../services/deckParser";
import { FORMAT_DEFAULTS } from "../../stores/multiplayerStore";
import { sampleOpeningHand } from "../../viewmodel/deckSample";
import { menuButtonClass } from "../menu/buttonStyles";

const DIFFICULTY_OPTIONS = ["Easy", "Medium", "Hard"] as const;
type PlaytestDifficulty = (typeof DIFFICULTY_OPTIONS)[number];

interface PlaytestPanelProps {
  deck: ParsedDeck;
  format: GameFormat;
  mainCount: number;
}

export function PlaytestPanel({ deck, format, mainCount }: PlaytestPanelProps) {
  const { t } = useTranslation("deck-builder");
  const navigate = useNavigate();
  const [sampleHand, setSampleHand] = useState<string[]>(() => sampleOpeningHand(deck.main));
  const [difficulty, setDifficulty] = useState<PlaytestDifficulty>("Medium");

  const handEntries = useMemo(
    () =>
      sampleHand.map(
        (name): DeckEntry => ({ name, count: 1 }),
      ),
    [sampleHand],
  );

  const reshuffle = useCallback(() => {
    setSampleHand(sampleOpeningHand(deck.main));
  }, [deck.main]);

  const launchGoldfish = () => {
    stashPlaytestDeck({
      deck,
      format,
      formatConfig: FORMAT_DEFAULTS[format],
    });
    const gameId = crypto.randomUUID();
    navigate(
      `/game/${gameId}?mode=playtest&format=${format.toLowerCase()}&difficulty=${difficulty}&players=2`,
    );
  };

  return (
    <div className="flex flex-col gap-6 p-4 md:p-6">
      <div>
        <h2 className="text-lg font-semibold text-white">{t("playtest.title")}</h2>
        <p className="mt-1 max-w-2xl text-sm text-slate-400">{t("playtest.subtitle")}</p>
      </div>

      <section className="rounded-xl border border-white/10 bg-black/20 p-4">
        <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
          <h3 className="text-xs font-semibold uppercase tracking-wider text-slate-400">
            {t("playtest.sampleHand")}
          </h3>
          <button type="button" className={menuButtonClass({ tone: "neutral", size: "sm" })} onClick={reshuffle}>
            {t("playtest.reshuffle")}
          </button>
        </div>
        <p className="mb-3 text-[11px] text-slate-500">{t("playtest.sampleHandHint")}</p>
        <ul className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
          {handEntries.map((entry, index) => (
            <PlaytestCardRow key={`${entry.name}-${index}`} name={entry.name} />
          ))}
        </ul>
      </section>

      <section className="rounded-xl border border-cyan-500/20 bg-cyan-950/20 p-4">
        <h3 className="text-xs font-semibold uppercase tracking-wider text-cyan-200/80">
          {t("playtest.goldfish")}
        </h3>
        <p className="mt-2 text-sm text-slate-300">{t("playtest.goldfishHint", { count: mainCount })}</p>
        <div className="mt-4 flex flex-wrap items-center gap-3">
          <label className="flex items-center gap-2 text-sm text-slate-300">
            <span>{t("playtest.difficulty")}</span>
            <select
              value={difficulty}
              onChange={(e) => setDifficulty(e.target.value as typeof difficulty)}
              className="rounded-lg border border-white/10 bg-black/30 px-2 py-1 text-sm"
            >
              {DIFFICULTY_OPTIONS.map((option) => (
                <option key={option} value={option}>
                  {t(`playtest.difficulty${option}`)}
                </option>
              ))}
            </select>
          </label>
          <button type="button" className={menuButtonClass({ tone: "cyan", size: "sm" })} onClick={launchGoldfish}>
            {t("playtest.launch")}
          </button>
        </div>
      </section>
    </div>
  );
}

function PlaytestCardRow({ name }: { name: string }) {
  const { src, isLoading } = useCardImage(name);
  return (
    <li className="flex items-center gap-2 rounded-lg bg-white/5 px-2 py-1.5 text-sm text-slate-200">
      {src ? (
        <img src={src} alt="" className="h-10 w-7 rounded object-cover" />
      ) : (
        <div className="flex h-10 w-7 items-center justify-center rounded bg-slate-800 text-[9px] text-slate-500">
          {isLoading ? "…" : "?"}
        </div>
      )}
      <span className="truncate">{name}</span>
    </li>
  );
}
