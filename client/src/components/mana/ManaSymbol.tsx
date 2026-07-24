interface ManaSymbolProps {
  shard: string;
  size?: "xs" | "sm" | "md" | "lg";
  className?: string;
}

const SIZE_CLASSES = {
  xs: "w-3.5 h-3.5",
  sm: "w-5 h-5",
  md: "w-6 h-6",
  lg: "w-8 h-8",
} as const;

const SCRYFALL_SVG_BASE = "https://svgs.scryfall.io/card-symbols";
const SINGLE_SYMBOL_CODES = new Set([
  "W", "U", "B", "R", "G", "C", "S", "T", "Q", "E", "P", "X", "Y", "Z", "A", "∞", "½", "CHAOS",
]);

/** True when `shard` has a corresponding Scryfall card-symbol SVG. */
export function isManaSymbolShard(shard: string): boolean {
  if (/^\d+$/.test(shard) || SINGLE_SYMBOL_CODES.has(shard)) return true;
  const parts = shard.split("/");
  return parts.length > 1 && parts.every((part) => SINGLE_SYMBOL_CODES.has(part) || /^\d+$/.test(part));
}

/** Map our internal shard notation to the Scryfall SVG filename (without .svg). */
function shardToScryfallCode(shard: string): string {
  // Generic numbers: "3" → "3"
  if (/^\d+$/.test(shard)) return shard;
  // Hybrid/phyrexian: "W/U" → "WU", "W/P" → "WP", "B/G/P" → "BGP", "2/W" → "2W", "C/W" → "CW"
  return shard.replace(/\//g, "");
}

export function ManaSymbol({
  shard,
  size = "md",
  className = "",
}: ManaSymbolProps) {
  const code = shardToScryfallCode(shard);

  return (
    <img
      src={`${SCRYFALL_SVG_BASE}/${code}.svg`}
      alt={shard}
      className={`inline-block ${SIZE_CLASSES[size]} ${className}`}
      draggable={false}
    />
  );
}
