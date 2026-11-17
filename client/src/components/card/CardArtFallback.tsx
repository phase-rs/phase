import type { CSSProperties } from "react";
import { useTranslation } from "react-i18next";

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
  /**
   * Offers a "Tap to reload art" button that loads the art again. Pass it only
   * where the tile owns its taps: the button can't sit inside another button,
   * and on a phone's battlefield tile it would take the tap meant for the game.
   */
  onReload?: () => void;
}

/**
 * The reload button needs a 44px row, which a small tile doesn't have, so a
 * container query on the tile's width shows it only where it fits; a smaller
 * tile keeps its name and shows no button. Width stands in for height because
 * each variant keeps its shape: a battlefield art box is 4:3, so at 6rem wide
 * it is tall enough for the button alone (the frame header above it shows the
 * name), and a 5:7 card at 4rem wide fits the button under the name.
 */
const reloadButtonClassByVariant = {
  artCrop: "hidden @min-[6rem]:block text-[10px]",
  fullCard: "mt-1 hidden @min-[4rem]:block text-xs",
} as const satisfies Record<CardArtFallbackVariant, string>;

/**
 * Deliberate text tile shown when a card/token has no renderable art — either
 * because art resolution produced no src (issue #6156: tokens with no official
 * paper printing, e.g. Kibo, Uktabi Prince's Banana) or because the resolved
 * image failed to load. A failed load is remembered, so the tile can offer a
 * one-tap reload (#11008); it never retries on its own.
 *
 * Shared by the two board renderers: `CardImage` (full-card board and modals)
 * and `ArtCropCard` (the *default* battlefield renderer) route both their "no
 * art" and "broken art" cases here, so an artless permanent stays identifiable
 * by name wherever it appears rather than rendering as a blank/black square,
 * and the two renders cannot drift apart. The mobile preview uses it too.
 *
 * The desktop `CardPreview` deliberately keeps its own placeholder: the hover
 * preview is an order of magnitude larger and centres a single name at
 * `text-sm`, which this tile's top-aligned heading would not reproduce. It
 * mirrors the `role` and `aria-label` here so assistive tech announces an
 * artless card the same way in both places.
 */
export function CardArtFallback({
  name,
  oracleText,
  className,
  style,
  variant = "fullCard",
  onReload,
}: CardArtFallbackProps) {
  const { t } = useTranslation("game");
  const isArtCrop = variant === "artCrop";
  const offersReload = onReload != null;
  // Where the button shows, an art crop has room for it or the name, not both;
  // the name then stays only as the label for assistive tech.
  const nameGivesWayClass = isArtCrop && offersReload ? "@min-[6rem]:sr-only" : "";

  return (
    <div
      className={[
        className,
        "bg-gray-800 shadow-md overflow-hidden flex flex-col",
        isArtCrop ? "justify-center p-1" : "p-2",
        offersReload ? "@container" : "",
      ].join(" ")}
      style={style}
    >
      {/* The image role sits on the text alone: an `img` hides whatever it
          contains from assistive tech, and the reload button must stay
          reachable. */}
      <div
        role="img"
        aria-label={name}
        className={`flex min-h-0 flex-1 flex-col overflow-hidden ${isArtCrop ? "justify-center" : ""} ${nameGivesWayClass}`}
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
      {onReload && (
        // `min-h-11` keeps the touch target at least 44px tall. The surface
        // around the tile (a permanent, a preview) acts on the same tap, so the
        // button stops both its pointerdown and its click.
        <button
          type="button"
          onPointerDown={(event) => event.stopPropagation()}
          onClick={(event) => {
            event.stopPropagation();
            onReload();
          }}
          className={[
            "min-h-11 w-full shrink-0 rounded-md bg-black/40 px-2 font-semibold leading-tight",
            "text-gray-100 ring-1 ring-white/20 hover:bg-black/60",
            reloadButtonClassByVariant[variant],
          ].join(" ")}
        >
          {t("card.reloadArt")}
        </button>
      )}
    </div>
  );
}
