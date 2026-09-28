import { useTranslation } from "react-i18next";

import { DECK_SIGNAL_KINDS, type DeckSignals } from "../../types/deckSignals";

interface Props {
  signals: DeckSignals | null;
  emptyReason?: "not-commander" | "no-commander" | "card-data-unavailable";
}

export function DeckSignalsPanel({ signals, emptyReason }: Props) {
  const { t } = useTranslation("deck-builder");

  if (emptyReason === "not-commander") return null;
  if (emptyReason === "card-data-unavailable") {
    return (
      <div className="rounded-md border border-white/10 bg-black/20 px-3 py-2 text-xs text-slate-400">
        {t("signals.unavailable")}
      </div>
    );
  }
  if (emptyReason === "no-commander" || !signals) {
    return (
      <div className="rounded-md border border-white/10 bg-black/20 px-3 py-2 text-xs text-slate-400">
        {t("signals.noCommander")}
      </div>
    );
  }

  const average = signals.average_mana_value_centi === null
    ? t("signals.notMeasurable")
    : (signals.average_mana_value_centi / 100).toFixed(2);

  return (
    <section className="rounded-md border border-white/10 bg-black/20 px-3 py-2">
      <h3 className="text-xs font-medium text-slate-200">{t("signals.title")}</h3>
      <p className="mt-1 text-xs text-slate-400">{t("signals.disclaimer")}</p>
      <dl className="mt-3 space-y-2 text-xs">
        {DECK_SIGNAL_KINDS.map((kind) => {
          const reading = signals.readings[kind];
          return (
            <div
              key={kind}
              className="grid grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)] items-start gap-2"
            >
              <dt className="text-slate-300">{t(`signals.kind.${kind}`)}</dt>
              <dd className="text-right tabular-nums text-slate-200">{reading.count}</dd>
              <dd className="text-slate-400">{reading.contributing.join(", ")}</dd>
            </div>
          );
        })}
      </dl>
      <div className="mt-3 border-t border-white/5 pt-2 text-xs text-slate-400">
        {t("signals.avgManaValue", { value: average })}
      </div>
      {signals.unresolved_cards > 0 && (
        <div className="mt-1 text-xs text-slate-400">
          {t("signals.unresolved", { n: signals.unresolved_cards })}
        </div>
      )}
    </section>
  );
}
