import { useTranslation } from "react-i18next";

import type { ChallengeDeck } from "../../adapter/types";
import { CHALLENGE_DECK_REGISTRY } from "../../data/challengeDeckRegistry";

interface HordeDeckSelectorProps {
  /** Currently selected challenge deck. */
  value: ChallengeDeck;
  onChange: (deck: ChallengeDeck) => void;
}

/**
 * Picker for the self-piloting Horde deck the survivors face.
 *
 * The list comes from `CHALLENGE_DECK_REGISTRY`, a mirror of the engine's
 * `ChallengeDeck::registry()` — labels, descriptions, and per-deck rules are all
 * engine-owned, so adding a Horde deck is an engine-side change and this picker
 * grows automatically. Nothing about which decks exist is hardcoded here.
 */
export function HordeDeckSelector({ value, onChange }: HordeDeckSelectorProps) {
  const { t } = useTranslation("game");

  return (
    <div className="space-y-2">
      <div>
        <h3 className="text-sm font-medium text-white/90">
          {t("gameSetup.hordeDeck.title")}
        </h3>
        <p className="mt-0.5 text-xs text-white/50">
          {t("gameSetup.hordeDeck.subtitle")}
        </p>
      </div>

      <div role="radiogroup" aria-label={t("gameSetup.hordeDeck.title")} className="space-y-1.5">
        {CHALLENGE_DECK_REGISTRY.map((meta) => {
          const selected = meta.deck === value;
          return (
            <button
              key={meta.deck}
              type="button"
              role="radio"
              aria-checked={selected}
              onClick={() => onChange(meta.deck)}
              className={[
                "flex w-full items-start gap-3 rounded-lg border px-3 py-2 text-left transition-colors",
                selected
                  ? "border-amber-400/40 bg-amber-400/10"
                  : "border-white/8 bg-white/2 hover:border-white/20 hover:bg-white/5",
              ].join(" ")}
            >
              <span
                className={[
                  "mt-0.5 shrink-0 rounded px-1.5 py-0.5 text-[10px] font-semibold tracking-wide",
                  selected ? "bg-amber-400/20 text-amber-200" : "bg-white/10 text-white/60",
                ].join(" ")}
              >
                {meta.short_label}
              </span>
              <span className="min-w-0 flex-1">
                <span className="block text-sm text-white/90">{meta.label}</span>
                <span className="mt-0.5 block text-xs text-white/50">
                  {meta.description}
                </span>
                {/*
                 * How this deck plays and how it differs from the others. Every
                 * line is engine-authored (`ChallengeDeckMetadata.rules`, rendered
                 * by `HordeRuleset::summary()`); the picker only styles them, so
                 * the rules text stays owned by the engine. Shown for the selected
                 * deck to keep the collapsed list scannable.
                 */}
                {selected && (
                  <span className="mt-2 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1">
                    {meta.rules.map((rule) => (
                      <span key={rule.label} className="contents">
                        <span className="text-[10px] font-semibold uppercase tracking-wide text-amber-200/70">
                          {rule.label}
                        </span>
                        <span className="text-xs text-white/70">{rule.detail}</span>
                      </span>
                    ))}
                  </span>
                )}
              </span>
              {selected && (
                <span className="sr-only">{t("gameSetup.hordeDeck.selected")}</span>
              )}
            </button>
          );
        })}
      </div>
    </div>
  );
}
