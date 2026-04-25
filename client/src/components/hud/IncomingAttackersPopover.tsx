import { CardImage } from "../card/CardImage.tsx";
import { useGameStore } from "../../stores/gameStore.ts";
import { getKeywordName } from "../../viewmodel/keywordProps.ts";
import type { ObjectId } from "../../adapter/types.ts";

// Keywords on the *attacker* that materially change how a defender plans a
// block or removes the threat. These MUST match `getKeywordName`'s display-form
// output — `splitPascalCase` turns `FirstStrike` into `"First Strike"`, and
// parameterized keywords (Protection, Ward, Hexproof) fall through to the bare
// key — so a PascalCase set would silently drop the most important matchups.
//
// Excluded by design:
//   - "Reach": a defender's keyword (lets *blockers* hit fliers); irrelevant
//     on an attacker.
//   - "Defender": creatures with defender can't attack, so it never appears
//     in this popover.
const BLOCKING_RELEVANT_KEYWORDS = new Set([
  // Combat-shape modifiers
  "Flying",
  "Trample",
  "Menace",
  "Deathtouch",
  "Lifelink",
  "First Strike",
  "Double Strike",
  // Block-restriction keywords
  "Unblockable",
  "Intimidate",
  "Fear",
  "Shadow",
  "Skulk",
  "Horsemanship",
  "Protection",
  // Removal-restriction keywords (defender's removal plans)
  "Hexproof",
  "Shroud",
  "Indestructible",
  "Ward",
]);

interface IncomingAttackersPopoverProps {
  attackerIds: readonly ObjectId[];
  opponentName: string;
}

/** Hover popover surfaced from an `OpponentTab` when one or more of that
 *  opponent's creatures is attacking the local player (or their permanents)
 *  but the opponent's board is not currently focused. Shows mini card
 *  images with P/T + a one-line keyword hint so the defender can plan a
 *  block without switching focus first.
 *
 *  Rendered via absolute positioning below the tab. `pointer-events-none`
 *  prevents events from fighting the tab's hover state (the wrapping
 *  `OpponentTab` owns the show/hide lifecycle via a short close delay).
 *  `aria-hidden` keeps screen readers from concatenating attacker detail
 *  into the button's accessible name — the tab's own `aria-label` already
 *  carries a summary of the incoming count. */
export function IncomingAttackersPopover({
  attackerIds,
  opponentName,
}: IncomingAttackersPopoverProps) {
  const objects = useGameStore((s) => s.gameState?.objects);
  if (!objects || attackerIds.length === 0) return null;

  return (
    <div
      aria-hidden
      className="pointer-events-none absolute left-1/2 top-full z-50 mt-2 max-w-[calc(100vw-1rem)] -translate-x-1/2 rounded-lg border border-red-400/50 bg-slate-950/95 px-2.5 py-2 shadow-xl backdrop-blur-xl"
    >
      <div className="mb-1.5 whitespace-nowrap text-[10px] font-semibold uppercase tracking-[0.16em] text-red-300">
        ⚔×{attackerIds.length} incoming from {opponentName}
      </div>
      <div className="flex flex-wrap gap-1.5">
        {attackerIds.map((id) => {
          const obj = objects[id];
          if (!obj) return null;
          const pt = obj.power != null && obj.toughness != null
            ? `${obj.power}/${obj.toughness}`
            : null;
          const relevantKeywords = obj.keywords
            .map(getKeywordName)
            .filter((name) => BLOCKING_RELEVANT_KEYWORDS.has(name));

          return (
            <div key={id} className="flex w-16 flex-col items-center gap-0.5">
              <div
                className="overflow-hidden rounded ring-1 ring-red-400/40"
                style={{ width: 64, height: 90 }}
              >
                <CardImage
                  cardName={obj.name}
                  size="small"
                  isToken={obj.card_id === 0}
                />
              </div>
              {pt && (
                <div className="rounded bg-black/80 px-1 text-[9px] font-bold text-white">
                  {pt}
                </div>
              )}
              {relevantKeywords.length > 0 && (
                <div className="text-center text-[8px] font-medium leading-tight text-amber-200">
                  {relevantKeywords.slice(0, 2).join(", ")}
                </div>
              )}
            </div>
          );
        })}
      </div>
      <div className="mt-1.5 text-[9px] italic text-slate-400">
        click to focus
      </div>
    </div>
  );
}
