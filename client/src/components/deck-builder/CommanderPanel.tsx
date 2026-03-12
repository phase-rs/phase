import type { ScryfallCard } from "../../services/scryfall";
import type { DeckEntry } from "../../services/deckParser";
import {
  getCombinedColorIdentity,
  getColorIdentityViolations,
  getSingletonViolations,
  canBeCommander,
  canAddPartner,
} from "./commanderUtils";

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
  onSetCommander: (cardName: string) => void;
  onRemoveCommander: (cardName: string) => void;
}


export function CommanderPanel({
  commanders,
  deck,
  cardDataCache,
  onSetCommander,
  onRemoveCommander,
}: CommanderPanelProps) {
  const identity = getCombinedColorIdentity(commanders, cardDataCache);
  const colorViolations = getColorIdentityViolations(deck, commanders, cardDataCache);
  const singletonViolations = getSingletonViolations(deck);
  const totalCards = deck.reduce((sum, e) => sum + e.count, 0) + commanders.length;

  // Find cards in deck that are eligible to be set as commander
  const eligibleCommanders = deck
    .filter((entry) => {
      const card = cardDataCache.get(entry.name);
      if (!card || !canBeCommander(card)) return false;
      if (commanders.includes(entry.name)) return false;
      return canAddPartner(commanders, card, cardDataCache);
    })
    .map((e) => e.name);

  return (
    <div className="space-y-3">
      <h4 className="text-xs font-semibold uppercase text-gray-500">
        Commander
      </h4>

      {/* Commander slots */}
      <div className="space-y-2">
        {commanders.length === 0 && (
          <div className="rounded border border-dashed border-gray-700 p-3 text-center text-xs text-gray-500">
            No commander selected
          </div>
        )}
        {commanders.map((name) => {
          return (
            <div
              key={name}
              className="flex items-center justify-between rounded bg-purple-900/30 px-2 py-1.5"
            >
              <span className="text-sm font-medium text-purple-300">
                {name}
              </span>
              <button
                onClick={() => onRemoveCommander(name)}
                className="text-xs text-red-400 hover:text-red-300"
              >
                Remove
              </button>
            </div>
          );
        })}
      </div>

      {/* Color identity display */}
      {commanders.length > 0 && (
        <div className="flex items-center gap-1">
          <span className="text-[10px] text-gray-500">Identity:</span>
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
          <span className="text-[10px] text-gray-500">Set as commander:</span>
          {eligibleCommanders.slice(0, 5).map((name) => (
            <button
              key={name}
              onClick={() => onSetCommander(name)}
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
          className={`text-xs ${totalCards === 100 ? "text-green-400" : "text-yellow-400"}`}
        >
          {totalCards}/100 cards
        </div>
        {singletonViolations.length > 0 && (
          <div className="text-xs text-red-400">
            Singleton violations: {singletonViolations.join(", ")}
          </div>
        )}
        {colorViolations.length > 0 && (
          <div className="text-xs text-red-400">
            Color identity violations: {colorViolations.join(", ")}
          </div>
        )}
      </div>
    </div>
  );
}
