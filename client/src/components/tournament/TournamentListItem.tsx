import { useTranslation } from "react-i18next";

import type { TournamentSummary } from "../../adapter/types";
import { arityLabel } from "../../pages/tournamentPageState";

interface TournamentListItemProps {
  /**
   * The list row's own wire shape — deliberately NOT a `TournamentView`.
   *
   * `TournamentSummary.player_count` is the count of **active** entrants
   * (`TournamentMeta::active_player_count`), while `TournamentView.players` is
   * the full history including drops. Rendering the latter as an entrant count
   * is wrong by exactly the number of drops. Taking the narrower type makes
   * that mistake unrepresentable here rather than merely discouraged.
   */
  summary: TournamentSummary;
  onOpen: (code: string) => void;
}

export function TournamentListItem({ summary, onOpen }: TournamentListItemProps) {
  const { t, i18n } = useTranslation("tournament");
  const arity = arityLabel(summary.arity);

  return (
    <button
      type="button"
      onClick={() => onOpen(summary.code)}
      className="flex w-full items-center gap-3 rounded-[10px] border border-white/10 bg-[linear-gradient(180deg,rgba(255,255,255,0.055),rgba(0,0,0,0.18))] px-4 py-3 text-left shadow-[0_10px_26px_rgba(0,0,0,0.22)] backdrop-blur-sm transition-colors hover:border-white/20 hover:bg-[linear-gradient(180deg,rgba(255,255,255,0.075),rgba(0,0,0,0.16))]"
    >
      {/* Bracket and status badges index the catalog directly off the wire
          PascalCase values — `bracket.Swiss`, `status.InProgress` — which is
          how phase 3 authored those two groups. This is NOT a general licence
          to build keys from wire tags: `outcome.*` is lowercase and is not
          1:1 with its union, which is why it goes through the page-state
          module instead. */}
      <span className="flex-shrink-0 rounded-[5px] border border-indigo-300/20 bg-indigo-500/15 px-1.5 py-0.5 text-xs font-semibold text-indigo-200">
        {t(`bracket.${summary.bracket}`)}
      </span>
      <span className="flex-shrink-0 rounded-[5px] border border-cyan-300/20 bg-cyan-500/15 px-1.5 py-0.5 text-xs font-semibold text-cyan-200">
        {t(`status.${summary.status}`)}
      </span>

      <div className="min-w-0 flex-1">
        <p className="truncate text-sm font-medium text-gray-200">{summary.name}</p>
        <p className="flex flex-wrap gap-x-3 text-xs text-gray-500">
          <span>{t("list.entrants", { count: summary.player_count })}</span>
          <span>
            {t("labels.roundOf", {
              current: summary.current_round,
              total: summary.total_rounds,
            })}
          </span>
          <span>
            {"seats" in arity ? t(arity.key, { seats: arity.seats }) : t(arity.key)}
          </span>
          <span>
            {/* `created_at` is unix SECONDS (`tournament.rs:1527` computes
                `env.now_ms() / 1000`), so it is scaled to milliseconds here.
                The locale is the app's language, not the browser's. */}
            {t("labels.created", {
              date: new Date(summary.created_at * 1000).toLocaleDateString(i18n.language),
            })}
          </span>
        </p>
      </div>

      <span className="flex-shrink-0 rounded-[6px] bg-emerald-600 px-3 py-1 text-xs font-medium text-white">
        {t("list.view")}
      </span>
      <span className="flex-shrink-0 rounded-[6px] border border-white/10 bg-black/25 px-2 py-0.5 font-mono text-xs tracking-wider text-emerald-300">
        {t("labels.code", { code: summary.code })}
      </span>
    </button>
  );
}
