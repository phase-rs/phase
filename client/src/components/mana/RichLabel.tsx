import { Fragment } from "react";
import { ManaSymbol } from "./ManaSymbol.tsx";

interface RichLabelProps {
  text: string;
  size?: "xs" | "sm" | "md" | "lg";
  className?: string;
}

// Matches Oracle-style symbol tokens: {T}, {Q}, {C}, {W}, {U}, {2}, {X},
// {W/U}, {2/W}, {W/P}, {B/G/P}, {CHAOS}, etc. The capture group keeps the
// inner code so we can dispatch to ManaSymbol (which normalizes "W/U" → "WU").
const SYMBOL_PATTERN = /\{([^{}]+)\}/g;

/**
 * Render a string that may contain MTG symbol tokens ("{T}", "{U}", "Add {C}.")
 * as a mix of text and inline ManaSymbol SVGs. Tokens the symbol catalog does
 * not recognize still render — ManaSymbol falls back through its own error
 * handler — so callers can safely pass any label from costLabel.ts.
 */
export function RichLabel({ text, size = "sm", className }: RichLabelProps) {
  const parts: Array<{ kind: "text"; value: string } | { kind: "symbol"; code: string }> = [];
  let cursor = 0;
  for (const match of text.matchAll(SYMBOL_PATTERN)) {
    const start = match.index ?? 0;
    if (start > cursor) {
      parts.push({ kind: "text", value: text.slice(cursor, start) });
    }
    parts.push({ kind: "symbol", code: match[1] });
    cursor = start + match[0].length;
  }
  if (cursor < text.length) {
    parts.push({ kind: "text", value: text.slice(cursor) });
  }

  return (
    <span className={className}>
      {parts.map((part, i) =>
        part.kind === "text" ? (
          <Fragment key={i}>{part.value}</Fragment>
        ) : (
          <ManaSymbol
            key={i}
            shard={part.code}
            size={size}
            className="align-[-0.125em]"
          />
        ),
      )}
    </span>
  );
}
