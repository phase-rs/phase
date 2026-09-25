import type { CSSProperties, ReactNode } from "react";

import { MELDED_CARD_SCALE } from "./boardSizing.ts";

const CARD_SIZE_VARS = ["card-w", "card-h", "art-crop-w", "art-crop-h"] as const;

/** Scaled copies of the row's card-size variables, computed from the inherited values. */
const SCALED_SIZE_STYLE = Object.fromEntries(
  CARD_SIZE_VARS.map((name) => [`--melded-${name}`, `calc(var(--${name}) * ${MELDED_CARD_SCALE})`]),
) as CSSProperties;

/** The row's card-size variables, rebound to their scaled copies. */
const REBOUND_SIZE_STYLE = Object.fromEntries(
  CARD_SIZE_VARS.map((name) => [`--${name}`, `var(--melded-${name})`]),
) as CSSProperties;

/**
 * Renders its card at the oversized melded size. Every card surface sizes from
 * `--card-w`/`--card-h` (or the art-crop pair), and a custom property cannot
 * reference itself, so the scaled sizes are computed one element up from the
 * inherited row sizes and rebound on the element below.
 */
export function MeldedCardFrame({ children }: { children: ReactNode }) {
  return (
    <div data-melded-card="" style={SCALED_SIZE_STYLE}>
      <div style={REBOUND_SIZE_STYLE}>{children}</div>
    </div>
  );
}
