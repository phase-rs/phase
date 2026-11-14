import { ManaGlyphPresentation } from "./ManaGlyphPresentation.tsx";
import { isManaSymbolShard } from "../../services/scryfall.ts";

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
