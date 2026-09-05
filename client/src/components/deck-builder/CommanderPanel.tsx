import { useTranslation } from "react-i18next";

import type { ScryfallCard } from "../../services/scryfall";
import type { DeckEntry } from "../../services/deckParser";
import type { DeckSizeRule } from "../../adapter/types";
import {
  getCombinedColorIdentity,
} from "./commanderUtils";
import { mouseHoverPreview } from "./hoverPreview";
import type { CardHoverHandler } from "./hoverPreview";

const WUBRG_COLORS = ["W", "U", "B", "R", "G"] as const;

const COLOR_PIP_STYLES: Record<string, string> = {
  W: "bg-amber-100 text-amber-900",
  U: "bg-blue-500 text-white",
  B: "bg-gray-800 text-gray-100 ring-1 ring-gray-600",
  R: "bg-red-600 text-white",
  G: "bg-green-600 text-white",
};

/**
 * CR 903.5a: does the `deck` this panel was handed already CONTAIN the
 * designated commander cards, or are they held beside it?
 *
 * The two callers genuinely differ and neither is wrong. Constructed Commander
 * is commanders-OUTSIDE — `useDeckBuilder.handleSetCommander` filters the
 * chosen card out of `main`, so the 99 plus the commander make 100. A drafted
 * Commander pool is commanders-INSIDE — `validate_limited_deck` step 5 requires
 * a copy of every designated name IN `main_deck`, so the designation is a LABEL
 * on a deck card, never an extra card beside the deck.
 *
 * Declared rather than inferred: a panel that guesses gets one caller's count
 * wrong by exactly `commanders.length`, which is silent, plausible, and off by
 * a legal-vs-illegal margin.
 */
export type CommanderDeckComposition = "commanders-inside" | "commanders-outside";

/**
 * How many of `commanders` are NOT already counted by `deck`. Exhaustive with
 * no `default`, so a third composition is a compile error here rather than a
 * silently-wrong total.
 */
function commandersNotInDeck(
  composition: CommanderDeckComposition,
  commanders: string[],
): number {
  switch (composition) {
    case "commanders-inside":
      return 0;
    case "commanders-outside":
      return commanders.length;
  }
}

function hasCommanderCopyAvailable(
  composition: CommanderDeckComposition,
  entry: DeckEntry,
  selectedCount: number,
): boolean {
  switch (composition) {
    case "commanders-inside":
      return selectedCount < entry.count;
    case "commanders-outside":
      return selectedCount === 0;
  }
}

/**
 * CR 903.13f(1) / CR 903.5: is `totalCards` a legal size under this rule?
 * Exhaustive with no `default`, so a third `DeckSizeRule` variant is a compile
 * error here rather than a silently-wrong indicator colour.
 */
function deckSizeSatisfied(rule: DeckSizeRule, totalCards: number): boolean {
  switch (rule.type) {
    case "Minimum":
      return totalCards >= rule.data;
    case "Exactly":
      return totalCards === rule.data;
  }
}

interface CommanderPanelProps {
  commanders: string[];
  deck: DeckEntry[];
  /** CR 903.5a: whether `deck` already contains the designated commanders. */
  deckComposition: CommanderDeckComposition;
  cardDataCache: Map<string, ScryfallCard>;
  /**
   * CR 903.13f(1): the format's typed deck-size rule. Read exhaustively — a
   * `Minimum` rule (Commander Draft: at least 60, NO maximum) is satisfied by
   * any larger deck, so an exact-equality indicator would paint a legal
   * 61-card deck as not-yet-valid.
   */
  deckSizeRule: DeckSizeRule;
  isCommanderEligible: (name: string) => boolean;
  onSetCommander: (cardName: string) => void;
  onRemoveCommander: (cardName: string) => void;
  signatureSpell?: string;
  /** `null` means this format has no signature-spell slot. */
  signatureSpellCandidates?: string[] | null;
  onSetSignatureSpell?: (cardName: string) => void;
  onRemoveSignatureSpell?: () => void;
  companion?: string;
  /** `null` means candidates have not loaded yet. */
  companionCandidates?: string[] | null;
  onSetCompanion?: (cardName: string) => void;
  onRemoveCompanion?: () => void;
  onCardHover?: CardHoverHandler;
  /** Engine evaluateDeckCompatibility reasons for the active format. */
  formatValidationReasons?: string[];
}


export function CommanderPanel({
  commanders,
  deck,
  deckComposition,
  cardDataCache,
  deckSizeRule,
  isCommanderEligible,
  onSetCommander,
  onRemoveCommander,
  signatureSpell,
  signatureSpellCandidates = null,
  onSetSignatureSpell = () => {},
  onRemoveSignatureSpell = () => {},
  companion,
  companionCandidates = null,
  onSetCompanion = () => {},
  onRemoveCompanion = () => {},
  onCardHover,
  formatValidationReasons = [],
}: CommanderPanelProps) {
  const { t } = useTranslation("deck-builder");
  const identity = getCombinedColorIdentity(commanders, cardDataCache);
  const hoverInfo = (name: string) => ({
    name,
    scryfallId: cardDataCache.get(name)?.id,
  });
  const totalCards = deck.reduce((sum, e) => sum + e.count, 0)
    + commandersNotInDeck(deckComposition, commanders)
    + (signatureSpell ? 1 : 0);

  // Cards in deck that could become a commander. The handler decides whether
  // clicking adds (free slot or partner pair) or swaps (replaces existing).
  const eligibleCommanders = deck
    .filter((entry) => {
      if (!isCommanderEligible(entry.name)) return false;
      // CR 903.13f(2): Commander Draft may use any number of same-name cards
      // from the pool, so inside-deck designations consume copies, not names.
      const selectedCount = commanders.filter((name) => name === entry.name).length;
      return hasCommanderCopyAvailable(deckComposition, entry, selectedCount);
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
        {commanders.map((name, index) => {
          const occurrence = commanders.slice(0, index + 1)
            .filter((commander) => commander === name).length;
          return (
            <div
              key={`${name}-${occurrence}`}
              {...mouseHoverPreview(onCardHover, hoverInfo(name))}
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
              {...mouseHoverPreview(onCardHover, hoverInfo(name))}
              className="block w-full truncate rounded bg-purple-800/40 px-2 py-1 text-left text-xs text-purple-300 hover:bg-purple-700/40"
            >
              {name}
            </button>
          ))}
        </div>
      )}

      {signatureSpellCandidates !== null && (
        <div className="space-y-2 border-t border-white/10 pt-3">
          <h5 className="text-xs font-semibold uppercase text-gray-500">
            {t("commanderPanel.signatureSpell.heading")}
          </h5>
          {signatureSpell ? (
            <div
              {...mouseHoverPreview(onCardHover, hoverInfo(signatureSpell))}
              className="flex items-center justify-between rounded bg-purple-900/30 px-2 py-1.5"
            >
              <span className="text-sm font-medium text-purple-300">{signatureSpell}</span>
              <button
                onClick={onRemoveSignatureSpell}
                className="min-h-11 px-2 text-xs text-red-400 hover:text-red-300 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-purple-300"
              >
                {t("commanderPanel.signatureSpell.remove")}
              </button>
            </div>
          ) : (
            <p className="text-xs text-gray-500">
              {t("commanderPanel.signatureSpell.noSignatureSpell")}
            </p>
          )}
          {!signatureSpell && signatureSpellCandidates.map((name) => (
            <button
              key={name}
              onClick={() => onSetSignatureSpell(name)}
              {...mouseHoverPreview(onCardHover, hoverInfo(name))}
              className="block min-h-11 w-full truncate rounded bg-purple-800/40 px-2 py-1 text-left text-xs text-purple-300 hover:bg-purple-700/40 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-purple-300"
            >
              {name}
            </button>
          ))}
        </div>
      )}

      {companionCandidates !== null && (
        <div className="space-y-2 border-t border-white/10 pt-3">
          <h5 className="text-xs font-semibold uppercase text-gray-500">
            {t("commanderPanel.companion.heading")}
          </h5>
          {companion ? (
            <div
              {...mouseHoverPreview(onCardHover, hoverInfo(companion))}
              className="flex items-center justify-between rounded bg-blue-900/30 px-2 py-1.5"
            >
              <span className="text-sm font-medium text-blue-300">{companion}</span>
              <button
                onClick={onRemoveCompanion}
                className="min-h-11 px-2 text-xs text-red-400 hover:text-red-300 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-blue-300"
              >
                {t("commanderPanel.companion.remove")}
              </button>
            </div>
          ) : (
            <p className="text-xs text-gray-500">{t("commanderPanel.companion.noCompanion")}</p>
          )}
          {!companion && companionCandidates.map((name) => (
            <button
              key={name}
              onClick={() => onSetCompanion(name)}
              {...mouseHoverPreview(onCardHover, hoverInfo(name))}
              className="block min-h-11 w-full truncate rounded bg-blue-800/40 px-2 py-1 text-left text-xs text-blue-300 hover:bg-blue-700/40 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-blue-300"
            >
              {name}
            </button>
          ))}
        </div>
      )}

      {/* Validation summary */}
      <div className="space-y-1">
        <div
          className={`text-xs ${deckSizeSatisfied(deckSizeRule, totalCards) ? "text-green-400" : "text-yellow-400"}`}
        >
          {t("commanderPanel.cardCount", { count: totalCards, expected: deckSizeRule.data })}
        </div>
        {formatValidationReasons.map((reason) => (
          <div key={reason} className="text-xs text-red-400">
            {reason}
          </div>
        ))}
      </div>
    </div>
  );
}
