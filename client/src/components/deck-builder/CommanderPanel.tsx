import { useTranslation } from "react-i18next";

import type { ScryfallCard } from "../../services/scryfall";
import type { DeckEntry } from "../../services/deckParser";
import {
  getCombinedColorIdentity,
  getColorIdentityViolations,
  getSingletonViolations,
} from "./commanderUtils";
import { mouseHoverPreview } from "./hoverPreview";

const WUBRG_COLORS = ["W", "U", "B", "R", "G"] as const;

const COLOR_PIP_STYLES: Record<string, string> = {
  W: "bg-amber-100 text-amber-900",
  U: "bg-blue-500 text-white",
  B: "bg-gray-800 text-gray-100 ring-1 ring-gray-600",
  R: "bg-red-600 text-white",
  G: "bg-green-600 text-white",
};

interface CommanderPanelProps {
  commanders: string[];
  deck: DeckEntry[];
  cardDataCache: Map<string, ScryfallCard>;
  expectedDeckSize: number;
  isCommanderEligible: (name: string) => boolean;
  onSetCommander: (cardName: string) => void;
  onRemoveCommander: (cardName: string) => void;
  onCardHover?: (cardName: string | null) => void;
  /** CR 100.2a / CR 903.5b: engine-backed per-card copy cap resolver. */
  getEffectiveCap: (name: string) => number;
}


export function CommanderPanel({
  commanders,
  deck,
  cardDataCache,
  expectedDeckSize,
  isCommanderEligible,
  onSetCommander,
  onRemoveCommander,
  onCardHover,
  getEffectiveCap,
}: CommanderPanelProps) {
  const { t } = useTranslation("deck-builder");
  const identity = getCombinedColorIdentity(commanders, cardDataCache);
  const colorViolations = getColorIdentityViolations(deck, commanders, cardDataCache);
  const singletonViolations = getSingletonViolations(deck, cardDataCache, getEffectiveCap);
  const totalCards = deck.reduce((sum, e) => sum + e.count, 0) + commanders.length;

  // Cards in deck that could become a commander. The handler decides whether
  // clicking adds (free slot or partner pair) or swaps (replaces existing).
  const eligibleCommanders = deck
    .filter((entry) => {
      if (!isCommanderEligible(entry.name)) return false;
      return !commanders.includes(entry.name);
    })
    .map((e) => e.name);

  return (
    <div className="space-y-3">
      <h4 className="text-xs font-semibold uppercase text-gray-500">
        {t("commanderPanel.heading")}
      </h4>

      {/* Commander slots */}
      <div className="space-y-2">
        {commanders.length === 0 && (
          <div className="rounded border border-dashed border-gray-700 p-3 text-center text-xs text-gray-500">
            {t("commanderPanel.noCommander")}
          </div>
        )}
        {commanders.map((name) => {
          return (
            <div
              key={name}
              {...mouseHoverPreview(onCardHover, name)}
              className="flex items-center justify-between rounded bg-purple-900/30 px-2 py-1.5"
            >
              <span className="text-sm font-medium text-purple-300">
                {name}
              </span>
              <button
                onClick={() => onRemoveCommander(name)}
                className="text-xs text-red-400 hover:text-red-300"
              >
                {t("commanderPanel.remove")}
              </button>
            </div>
          );
        })}
      </div>

      {/* Color identity display */}
      {commanders.length > 0 && (
        <div className="flex items-center gap-1">
          <span className="text-[10px] text-gray-500">{t("commanderPanel.identity")}</span>
          {WUBRG_COLORS.map((c) => (
            <span
              key={c}
              className={`flex h-5 w-5 items-center justify-center rounded-full text-[9px] font-bold ${
                identity.includes(c)
                  ? COLOR_PIP_STYLES[c]
                  : "bg-gray-800 text-gray-600"
              }`}
            >
              {c}
            </span>
          ))}
        </div>
      )}

      {/* Set as commander buttons */}
      {eligibleCommanders.length > 0 && (
        <div className="space-y-1">
          <span className="text-[10px] text-gray-500">{t("commanderPanel.setAsCommander")}</span>
          {eligibleCommanders.map((name) => (
            <button
              key={name}
              onClick={() => onSetCommander(name)}
              {...mouseHoverPreview(onCardHover, name)}
              className="block w-full truncate rounded bg-purple-800/40 px-2 py-1 text-left text-xs text-purple-300 hover:bg-purple-700/40"
            >
              {name}
            </button>
          ))}
        </div>
      )}

      {/* Validation summary */}
      <div className="space-y-1">
        <div
          className={`text-xs ${totalCards === expectedDeckSize ? "text-green-400" : "text-yellow-400"}`}
        >
          {t("commanderPanel.cardCount", { count: totalCards, expected: expectedDeckSize })}
        </div>
        {singletonViolations.length > 0 && (
          <div className="text-xs text-red-400">
            {t("commanderPanel.singletonViolations", { cards: singletonViolations.join(", ") })}
          </div>
        )}
        {colorViolations.length > 0 && (
          <div className="text-xs text-red-400">
            {t("commanderPanel.colorViolations", { cards: colorViolations.join(", ") })}
          </div>
        )}
      </div>
    </div>
  );
}
