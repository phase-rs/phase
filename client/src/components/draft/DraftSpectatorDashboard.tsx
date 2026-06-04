import { useTranslation } from "react-i18next";

import type { SpectatorDraftView } from "../../adapter/draft-adapter";
import { DraftProgress } from "./DraftProgress";

interface DraftSpectatorDashboardProps {
  view: SpectatorDraftView;
}

export function DraftSpectatorDashboard({ view }: DraftSpectatorDashboardProps) {
  const { t } = useTranslation("draft");

  const omniscient = view.pools != null;

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto">
      <div className="rounded-xl border border-amber-400/25 bg-amber-950/40 px-4 py-3 text-sm text-amber-100">
        {t("spectator.banner")}
        {omniscient && (
          <span className="ml-2 rounded-full bg-amber-500/25 px-2 py-0.5 text-[10px] uppercase tracking-wide">
            {t("spectator.omniscient")}
          </span>
        )}
      </div>

      <DraftProgress view={view} />

      <section className="rounded-xl border border-white/10 bg-black/20 p-4">
        <h3 className="mb-3 text-xs font-semibold uppercase tracking-wider text-slate-400">
          {t("spectator.seats")}
        </h3>
        <ul className="space-y-2">
          {view.seats.map((seat) => (
            <li
              key={seat.seat_index}
              className="flex items-center justify-between rounded-lg bg-white/5 px-3 py-2 text-sm"
            >
              <span className="font-medium text-white">{seat.display_name}</span>
              <span className="text-[10px] uppercase tracking-wide text-slate-400">
                {seat.pick_status}
                {seat.has_submitted_deck ? ` · ${t("spectator.deckSubmitted")}` : ""}
              </span>
            </li>
          ))}
        </ul>
      </section>

      {view.standings.length > 0 && (
        <section className="rounded-xl border border-white/10 bg-black/20 p-4">
          <h3 className="mb-3 text-xs font-semibold uppercase tracking-wider text-slate-400">
            {t("spectator.standings")}
          </h3>
          <ul className="space-y-1 text-sm text-slate-200">
            {view.standings.map((row) => (
              <li key={row.seat_index} className="flex justify-between gap-2">
                <span>{row.display_name}</span>
                <span className="tabular-nums text-slate-400">
                  {row.match_wins}-{row.match_losses}
                </span>
              </li>
            ))}
          </ul>
        </section>
      )}

      {view.pairings.length > 0 && (
        <section className="rounded-xl border border-white/10 bg-black/20 p-4">
          <h3 className="mb-3 text-xs font-semibold uppercase tracking-wider text-slate-400">
            {t("spectator.pairings")}
          </h3>
          <ul className="space-y-2 text-sm">
            {view.pairings.map((pairing) => (
              <li key={pairing.match_id} className="rounded-lg bg-white/5 px-3 py-2 text-slate-200">
                {pairing.name_a} vs {pairing.name_b}
                <span className="ml-2 text-[10px] uppercase text-slate-500">{pairing.status}</span>
              </li>
            ))}
          </ul>
        </section>
      )}

      {omniscient && view.pools && (
        <section className="rounded-xl border border-white/10 bg-black/20 p-4">
          <h3 className="mb-3 text-xs font-semibold uppercase tracking-wider text-slate-400">
            {t("spectator.pools")}
          </h3>
          <div className="grid gap-3 sm:grid-cols-2">
            {view.pools.map((pool, seatIndex) => (
              <div key={seatIndex} className="rounded-lg bg-white/5 p-3">
                <p className="mb-2 text-xs font-semibold text-slate-300">
                  {view.seats[seatIndex]?.display_name ?? t("spectator.seat", { index: seatIndex })}
                </p>
                <p className="text-[11px] text-slate-400">
                  {t("spectator.poolSize", { count: pool.length })}
                </p>
              </div>
            ))}
          </div>
        </section>
      )}
    </div>
  );
}
