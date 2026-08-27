import { ManaGlyphPresentation } from "./ManaGlyphPresentation.tsx";
import type { ManaSymbolShard } from "../../hooks/useFixedVisualImage.ts";

interface ManaSymbolProps {
  shard: string;
  size?: "xs" | "sm" | "md" | "lg";
  className?: string;
}

export const MANA_SYMBOL_SIZE_CLASSES = {
  xs: "w-3.5 h-3.5",
  sm: "w-5 h-5",
  md: "w-6 h-6",
  lg: "w-8 h-8",
} as const;

const SINGLE_SYMBOL_CODES = new Set([
  "W", "U", "B", "R", "G", "C", "S", "T", "Q", "E", "P", "X", "Y", "Z", "A", "∞", "½", "CHAOS",
]);
const COMPOSITE_SYMBOL_CODES = new Set([
  "W/U", "W/B", "U/B", "U/R", "B/R", "B/G", "R/W", "R/G", "G/W", "G/U",
  "2/W", "2/U", "2/B", "2/R", "2/G",
  "W/P", "U/P", "B/P", "R/P", "G/P",
  "W/U/P", "W/B/P", "U/B/P", "U/R/P", "B/R/P", "B/G/P", "R/W/P", "R/G/P", "G/W/P", "G/U/P",
  "C/W", "C/U", "C/B", "C/R", "C/G",
]);

const FINITE_NUMERIC_SYMBOL_CODES = new Set([
  ...Array.from({ length: 21 }, (_, value) => String(value)),
  "100",
  "1000000",
]);

/** True when `shard` has a corresponding Scryfall card-symbol SVG. */
export function isManaSymbolShard(shard: string): shard is ManaSymbolShard {
  return FINITE_NUMERIC_SYMBOL_CODES.has(shard)
    || SINGLE_SYMBOL_CODES.has(shard)
    || COMPOSITE_SYMBOL_CODES.has(shard);
}

export function ManaSymbol({
  shard,
  size = "md",
  className = "",
}: ManaSymbolProps) {
  const admittedShard = isManaSymbolShard(shard) ? shard : null;
  return <ManaGlyphPresentation
    shard={admittedShard}
    notation={shard}
    className={`inline-block ${MANA_SYMBOL_SIZE_CLASSES[size]} ${className}`}
  />;
}
