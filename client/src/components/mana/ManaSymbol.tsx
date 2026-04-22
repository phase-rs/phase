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

// Symbols are mirrored into client/public/mana-symbols/ by
// scripts/gen-symbol-images.sh. We serve from there first and fall back to
// Scryfall's CDN if a symbol is missing locally (e.g. a brand-new symbol that
// hasn't been re-downloaded yet).
const LOCAL_SVG_BASE = "/mana-symbols";
const SCRYFALL_SVG_BASE = "https://svgs.scryfall.io/card-symbols";

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
      src={`${LOCAL_SVG_BASE}/${code}.svg`}
      alt={shard}
      className={`inline-block ${SIZE_CLASSES[size]} ${className}`}
      draggable={false}
      onError={(e) => {
        const img = e.currentTarget;
        const fallback = `${SCRYFALL_SVG_BASE}/${code}.svg`;
        if (img.src !== fallback) img.src = fallback;
      }}
    />
  );
}
