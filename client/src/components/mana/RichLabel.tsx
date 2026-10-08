import { isManaSymbolShard } from "../../services/scryfall.ts";
import { ManaSymbol } from "./ManaSymbol.tsx";

interface RichLabelProps {
  text: string;
  size?: "xs" | "sm" | "md" | "lg";
  className?: string;
}

const SYMBOL_PATTERN = /\{([^{}]+)\}/g;

/**
 * Flatten brace notation to the text `RichLabel` renders visually.
 *
 * `RichLabel` swaps every recognized shard for a `ManaSymbol` image whose alt
 * text is the bare shard, and leaves an unrecognized one in braces. An
 * `aria-label` cannot hold that markup, so it has to carry the same flattening
 * or the announced name says "brace C brace" where the visible line says "C".
 * It lives beside the component so the two cannot drift apart.
 */
export function flattenRichLabel(text: string): string {
  return text.replace(SYMBOL_PATTERN, (braced, shard: string) =>
    isManaSymbolShard(shard) ? shard : braced,
  );
}

export function RichLabel({ text, size = "sm", className }: RichLabelProps) {
  return (
    <span className={className}>
      {/* ChoiceModal uses brace-delimited mana/tap notation like {W} and {T}. */}
      {text.split(SYMBOL_PATTERN).map((part, i) => {
        if (i % 2 === 0) return part;
        if (!isManaSymbolShard(part)) return `{${part}}`;
        return <ManaSymbol key={i} shard={part} size={size} className="align-[-0.125em]" />;
      })}
    </span>
  );
}
