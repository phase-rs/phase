import type { MouseEvent } from "react";
import { memo } from "react";

import type { ManaCost, ObjectId } from "../../adapter/types.ts";
import { useCardHover } from "../../hooks/useCardHover.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { formatCounterType } from "../../viewmodel/cardProps.ts";
import { getCardDisplayColors } from "../card/cardFrame.ts";

interface AttachmentChipProps {
  id: ObjectId;
  /** When true, hide the label/cost suffix and render just the glyph. Used by AttachmentChipRow's overflow mode. */
  glyphOnly?: boolean;
}

interface ChipStyle {
  glyph: string;
  className: string;
}

// Glyph palette is deliberately disjoint from CardPreview.tsx's CATEGORY_STYLES
// (◆/✦/⚡/🛡/↺/$) and from the under-attack badge (⚔ on PermanentCard) so a
// chip can never be confused with a parsed-ability pill or a combat indicator.
const STYLE_EQUIPMENT: ChipStyle = {
  glyph: "⚒",
  className: "bg-zinc-400/15 text-zinc-200",
};
const STYLE_AURA: ChipStyle = {
  glyph: "✧",
  className: "bg-fuchsia-400/15 text-fuchsia-300",
};
const STYLE_FORTIFICATION: ChipStyle = {
  glyph: "▣",
  className: "bg-stone-400/15 text-stone-300",
};
const STYLE_OTHER: ChipStyle = {
  glyph: "◇",
  className: "bg-slate-400/15 text-slate-300",
};
const STYLE_FACE_DOWN: ChipStyle = {
  glyph: "?",
  className: "bg-slate-700/30 text-slate-400",
};

function chipStyle(subtypes: string[], faceDown: boolean): ChipStyle {
  if (faceDown) return STYLE_FACE_DOWN;
  if (subtypes.includes("Equipment")) return STYLE_EQUIPMENT;
  if (subtypes.includes("Aura")) return STYLE_AURA;
  if (subtypes.includes("Fortification")) return STYLE_FORTIFICATION;
  return STYLE_OTHER;
}

// Color-identity left-border accent — mirrors CardPreview.tsx:385's
// `border-l-{color}-400/60` convention so a blue Aura and a red Aura render
// distinguishably even though both share the Aura subtype tint.
const COLOR_BORDER: Record<string, string> = {
  White: "border-l-amber-200",
  Blue: "border-l-sky-400",
  Black: "border-l-zinc-700",
  Red: "border-l-red-500",
  Green: "border-l-emerald-500",
  Colorless: "border-l-slate-400",
};

function colorBorderClass(colors: string[]): string {
  if (colors.length === 0) return "border-l-slate-500";
  if (colors.length > 1) return "border-l-yellow-400"; // multicolor → gold accent
  return COLOR_BORDER[colors[0]] ?? "border-l-slate-500";
}

/**
 * Compact mana-cost serialization. Returns "" for free/non-Cost shapes so the
 * chip falls back to name. Shards arrive in mixed formats:
 *   - single letter: "W", "U", "B", "R", "G"
 *   - full color name: "White", "Blue", …
 *   - hybrid pip: "B/W" (engine reality TBD; not seen in current test fixtures)
 * Strategy: pass single-character and slash-bearing shards through untouched;
 * truncate longer full-name shards to their first letter.
 */
function shortManaCost(cost: ManaCost | undefined): string {
  if (!cost || cost.type !== "Cost") return "";
  const generic = cost.generic > 0 ? String(cost.generic) : "";
  const colored = cost.shards.map((s) => {
    if (s.length <= 1) return s;
    if (s.includes("/")) return s; // preserve hybrid/Phyrexian "B/W", "B/P"
    return s.charAt(0); // "Green" → "G"
  }).join("");
  return generic + colored;
}

function shortName(name: string): string {
  return name.length > 8 ? `${name.slice(0, 7)}…` : name;
}

interface CounterSummary {
  type: string;
  count: number;
}

/**
 * Pick the largest-count entry. The plan called this "predominant counter
 * type" — for Cranial Plating + Charge counters this is the Charge total,
 * for a +1/+1-buffed creature it's the Plus1Plus1 total. Returning the type
 * preserves semantic distinctness (a +3 P1P1 chip is different from a +3
 * Charge chip) and lets the tooltip name it explicitly.
 */
function predominantCounter(counters: Record<string, number | undefined>): CounterSummary | null {
  let best: CounterSummary | null = null;
  for (const [type, value] of Object.entries(counters)) {
    if (typeof value !== "number" || value <= 0) continue;
    if (best === null || value > best.count) best = { type, count: value };
  }
  return best;
}

/**
 * Single-attachment chip rendered on the host PermanentCard. Each chip is its
 * own component instance so useCardHover can be called legally — wrapping it
 * in .map() inline would violate the rules of hooks.
 */
export const AttachmentChip = memo(function AttachmentChip({ id, glyphOnly = false }: AttachmentChipProps) {
  const obj = useGameStore((s) => s.gameState?.objects[id]);
  const selectObject = useUiStore((s) => s.selectObject);
  const { handlers, firedRef } = useCardHover(id);

  // Defensive: attachments[] may briefly reference an ID not yet in objects
  // mid-WASM-tick. Mirror PermanentCard.tsx:112's early return.
  if (!obj) return null;

  const style = chipStyle(obj.card_types.subtypes, obj.face_down);
  const isLand = obj.card_types.core_types.includes("Land");
  const displayColors = getCardDisplayColors(
    obj.color,
    isLand,
    obj.card_types.subtypes,
    obj.available_mana_colors,
  );
  const borderClass = colorBorderClass(displayColors);
  const costSuffix = shortManaCost(obj.mana_cost);
  const label = costSuffix || shortName(obj.name);
  const counter = predominantCounter(obj.counters);
  const counterLabel = counter ? `${formatCounterType(counter.type)} ×${counter.count}` : null;
  const tooltip = counterLabel ? `${obj.name} (${counterLabel})` : obj.name;

  const handleClick = (event: MouseEvent<HTMLButtonElement>) => {
    // Stop propagation so clicking a chip does not also trigger the host
    // creature's onClick (which would attempt to select/target/attack it).
    event.stopPropagation();
    // Long-press already fired the preview; consume the synthetic click and
    // reset the flag for the next interaction. Pattern mirrors PermanentCard.tsx:191.
    if (firedRef.current) {
      firedRef.current = false;
      return;
    }
    selectObject(id);
  };

  return (
    <button
      type="button"
      onClick={handleClick}
      title={tooltip}
      aria-label={tooltip}
      className={`flex h-4 max-w-full items-center gap-0.5 overflow-hidden rounded border border-l-2 px-1 text-[10px] font-semibold leading-none shadow-sm pointer-events-auto ${style.className} ${borderClass}`}
      {...handlers}
    >
      <span aria-hidden>{style.glyph}</span>
      {!glyphOnly && label && <span className="truncate">{label}</span>}
      {!glyphOnly && obj.tapped && (
        <span aria-label="tapped" className="ml-0.5 inline-block h-1 w-1 rounded-full bg-current opacity-80" />
      )}
      {!glyphOnly && counter && (
        <span aria-label={`${formatCounterType(counter.type)} counter${counter.count === 1 ? "" : "s"}: ${counter.count}`}>
          +{counter.count}
        </span>
      )}
    </button>
  );
});
