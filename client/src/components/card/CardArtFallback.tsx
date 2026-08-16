import type { CSSProperties } from "react";

import { RichLabel } from "../mana/RichLabel.tsx";

/**
 * Render scale for the fallback tile. Mirrors the `artCrop` / `fullCard`
 * variant vocabulary `SummoningSicknessOverlay` already uses, so the two
 * battlefield render modes name themselves the same way everywhere.
 */
export type CardArtFallbackVariant = "artCrop" | "fullCard";

export interface CardArtFallbackProps {
  /** Card/token name, shown as the tile heading and `aria-label`. */
  name: string;
  /** Oracle text rendered below the name when the engine knows it. */
  oracleText?: string;
  /** Sizing/border/tap-rotation classes shared with the loaded `<img>`. */
  className: string;
  /** Color-derived bevel border (or the neutral default). */
  style?: CSSProperties;
  /**
   * `artCrop` tiles are a fraction of a full card's height, so they show the
   * name alone — Oracle text at that scale is unreadable and would push the
   * name out of the box. Defaults to `fullCard`.
   */
  variant?: CardArtFallbackVariant;
}

/**
 * Deliberate text tile shown when a card/token has no renderable art — either
 * because art resolution produced no src (issue #6156: tokens with no official
 * paper printing, e.g. Kibo, Uktabi Prince's Banana) or because the resolved
 * image failed to load.
 *
 * Shared by the two board renderers: `CardImage` (full-card board and modals)
 * and `ArtCropCard` (the *default* battlefield renderer) route both their "no
 * art" and "broken art" cases here, so an artless permanent stays identifiable
 * by name wherever it appears rather than rendering as a blank/black square,
 * and the two renders cannot drift apart.
 *
 * `CardPreview` deliberately keeps its own placeholder: the hover preview is
 * an order of magnitude larger and centres a single name at `text-sm`, which
 * this tile's top-aligned heading would not reproduce. It mirrors the `role`
 * and `aria-label` here so assistive tech announces an artless card the same
 * way in both places.
 */
export function CardArtFallback({
  name,
  oracleText,
  className,
  style,
  variant = "fullCard",
}: CardArtFallbackProps) {
  const isArtCrop = variant === "artCrop";
  return (
    <div
      className={`${className} bg-gray-800 shadow-md overflow-hidden flex flex-col ${isArtCrop ? "justify-center p-1" : "p-2"}`}
      style={style}
      role="img"
      aria-label={name}
    >
      <div
        className={
          isArtCrop
            ? "text-[9px] font-semibold text-gray-100 leading-tight text-center break-words"
            : "text-xs font-semibold text-gray-100 mb-1 truncate"
        }
      >
        {name}
      </div>
      {!isArtCrop && oracleText && (
        <div className="text-[10px] text-gray-300 whitespace-pre-wrap leading-tight overflow-hidden">
          <RichLabel text={oracleText} size="xs" />
        </div>
      )}
    </div>
  );
}
