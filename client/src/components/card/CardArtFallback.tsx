import type { CSSProperties } from "react";
import { useTranslation } from "react-i18next";

import { RichLabel } from "../mana/RichLabel.tsx";

/**
 * Render scale for the fallback tile. `artCrop` and `fullCard` mirror the
 * variant vocabulary `SummoningSicknessOverlay` already uses, so the two
 * battlefield render modes name themselves the same way everywhere. `preview`
 * is the mobile preview's card-sized tile, whose text must read at a glance,
 * and `peek` the compact peek's, which closes on any scroll or touch move.
 */
export type CardArtFallbackVariant = "artCrop" | "fullCard" | "preview" | "peek";

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
   * Offers a "Tap to reload art" button that loads the art again. Only the
   * `preview` tile takes it: it owns its taps and has room for a 44px button,
   * where a board tile's tap belongs to the game and the peek closes on a tap.
   */
  onReload?: () => void;
}

/** Classes per scale. `body` lays out the name and rules text; `rules` is null
 *  where the tile shows the name alone. */
const tileClassesByVariant = {
  artCrop: {
    tile: "p-1",
    body: "justify-center",
    name: "text-[9px] font-semibold text-gray-100 leading-tight text-center break-words",
    rules: null,
    rulesSymbolSize: "xs",
  },
  fullCard: {
    tile: "p-2",
    body: "",
    name: "text-xs font-semibold text-gray-100 mb-1 truncate",
    rules: "text-[10px] text-gray-300 whitespace-pre-wrap leading-tight overflow-hidden",
    rulesSymbolSize: "xs",
  },
  preview: {
    tile: "p-4",
    body: "",
    name: "text-base font-semibold text-gray-100 mb-2",
    // The preview's tile can be short on a landscape phone, so long rules
    // text scrolls rather than being cut off.
    rules: "min-h-0 flex-1 overflow-y-auto overscroll-contain text-sm text-gray-300 whitespace-pre-wrap leading-snug",
    rulesSymbolSize: "sm",
  },
  // A scroll would close the peek, so its rules text clips instead.
  peek: {
    tile: "p-4",
    body: "",
    name: "text-base font-semibold text-gray-100 mb-2",
    rules: "text-sm text-gray-300 whitespace-pre-wrap leading-snug overflow-hidden",
    rulesSymbolSize: "sm",
  },
} as const satisfies Record<CardArtFallbackVariant, {
  tile: string;
  body: string;
  name: string;
  rules: string | null;
  rulesSymbolSize: "xs" | "sm";
}>;

/**
 * Deliberate text tile shown when a card/token has no renderable art — either
 * because art resolution produced no src (issue #6156: tokens with no official
 * paper printing, e.g. Kibo, Uktabi Prince's Banana) or because the resolved
 * image failed to load. A failed load is remembered, so the mobile preview's
 * tile offers a one-tap reload (#11008); it never retries on its own.
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
  const classes = tileClassesByVariant[variant];

  return (
    <div
      className={`${className} bg-gray-800 shadow-md overflow-hidden flex flex-col ${classes.tile}`}
      style={style}
      data-testid="card-art-fallback"
    >
      <div className={`flex min-h-0 flex-1 flex-col overflow-hidden ${classes.body}`}>
        {/* The image role sits on the name alone: an `img` hides whatever it
            contains from assistive tech, so the rules text and the reload
            button stay outside it, readable as text and reachable. */}
        <div role="img" aria-label={name} className={classes.name}>
          {name}
        </div>
        {classes.rules && oracleText && (
          <div className={classes.rules}>
            <RichLabel text={oracleText} size={classes.rulesSymbolSize} />
          </div>
        )}
      </div>
      {onReload && (
        // `min-h-11` keeps the touch target at least 44px tall. The preview
        // around the tile dismisses on a tap, so the button stops both its
        // pointerdown and its click.
        <button
          type="button"
          onPointerDown={(event) => event.stopPropagation()}
          onClick={(event) => {
            event.stopPropagation();
            onReload();
          }}
          className={[
            "mt-2 min-h-11 w-full shrink-0 rounded-md bg-black/40 px-3 text-sm font-semibold",
            "leading-tight text-gray-100 ring-1 ring-white/20 hover:bg-black/60",
          ].join(" ")}
        >
          {t("card.reloadArt")}
        </button>
      )}
    </div>
  );
}
