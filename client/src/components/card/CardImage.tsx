import { useState } from "react";
import type { CSSProperties } from "react";
import { useTranslation } from "react-i18next";
import { useCardImage } from "../../hooks/useCardImage.ts";
import { useEngineCardData } from "../../hooks/useEngineCardData.ts";
import type { TokenSearchFilters } from "../../services/scryfall.ts";
import type { TokenImageRef } from "../../adapter/types.ts";
import { CARD_BACK_URL } from "../../services/scryfall.ts";
import { getBevelBorderStyle } from "./cardFrame.ts";
import { ManaSymbol } from "../mana/ManaSymbol.tsx";
import { RichLabel } from "../mana/RichLabel.tsx";

interface CardImageProps {
  cardName: string;
  size?: "small" | "normal" | "large";
  faceIndex?: number;
  className?: string;
  tapped?: boolean;
  unimplementedMechanics?: string[];
  colors?: string[];
  isToken?: boolean;
  tokenFilters?: TokenSearchFilters;
  tokenImageRef?: TokenImageRef | null;
  faceDown?: boolean;
  /**
   * Renders a {T} symbol overlay in the corner to mark a tapped battlefield
   * permanent. Used by selection modals — which display cards upright rather
   * than rotated — so the player can still tell tapped permanents apart.
   * Distinct from `tapped`, which rotates the card 90° (board rendering).
   */
  tapIndicator?: boolean;
  /**
   * Canonical lookup id from `printed_ref.oracle_id` (battlefield call sites).
   * When provided, the image is resolved by oracle id + `faceName`, which is
   * the only correct path for MDFCs played as Scryfall's back face.
   */
  oracleId?: string;
  faceName?: string;
  /**
   * Oracle text rendered inside the broken-image fallback when the image fails
   * to load. If omitted, the component looks it up via the engine card database.
   */
  oracleText?: string;
}

interface CardArtFallbackProps {
  /** Card/token name, shown as the tile heading and `aria-label`. */
  name: string;
  /** Oracle text rendered below the name when the engine knows it. */
  oracleText?: string;
  /** Sizing/border/tap-rotation classes shared with the loaded `<img>`. */
  className: string;
  /** Color-derived bevel border (or the neutral default). */
  style?: CSSProperties;
}

/**
 * Deliberate text tile shown when a card/token has no renderable art — either
 * because art resolution produced no src (issue #6156: tokens with no official
 * paper printing) or because the resolved image failed to load. Naming both
 * failure modes in one place keeps the "no art" and "broken art" renders
 * identical, and guarantees an artless token is still identifiable by name
 * rather than showing as a blank/black square.
 */
function CardArtFallback({ name, oracleText, className, style }: CardArtFallbackProps) {
  return (
    <div
      className={`${className} bg-gray-800 shadow-md overflow-hidden flex flex-col p-2`}
      style={style}
      role="img"
      aria-label={name}
    >
      <div className="text-xs font-semibold text-gray-100 mb-1 truncate">{name}</div>
      {oracleText && (
        <div className="text-[10px] text-gray-300 whitespace-pre-wrap leading-tight overflow-hidden">
          <RichLabel text={oracleText} size="xs" />
        </div>
      )}
    </div>
  );
}

export function CardImage({
  cardName,
  size = "normal",
  faceIndex,
  className = "",
  tapped = false,
  unimplementedMechanics,
  colors,
  isToken = false,
  tokenFilters,
  tokenImageRef,
  faceDown = false,
  tapIndicator = false,
  oracleId,
  faceName,
  oracleText,
}: CardImageProps) {
  const { t } = useTranslation("game");
  const { src, isLoading } = useCardImage(faceDown ? "" : cardName, {
    size,
    faceIndex,
    isToken: faceDown ? false : isToken,
    tokenFilters: faceDown ? undefined : tokenFilters,
    tokenImageRef: faceDown ? undefined : tokenImageRef,
    oracleId: faceDown ? undefined : oracleId,
    faceName: faceDown ? undefined : faceName,
  });
  const [imageError, setImageError] = useState(false);
  const fallbackData = useEngineCardData(!faceDown && oracleText === undefined ? cardName : null);
  const resolvedOracleText = oracleText ?? fallbackData?.oracle_text ?? undefined;

  const tappedStyle = tapped ? "rotate-[90deg] origin-center" : "";
  const baseClasses = `w-[var(--card-w)] h-[var(--card-h)] rounded-lg transition-transform duration-200 ${tappedStyle} ${className}`;

  const borderStyle = colors
    ? getBevelBorderStyle(colors)
    : undefined;

  // Genuinely still resolving art — pulse until the async lookup settles.
  if (!faceDown && isLoading) {
    return (
      <div
        className={`${baseClasses} bg-gray-700 shadow-md animate-pulse`}
        style={borderStyle ?? { border: "1px solid #4b5563" }}
        aria-label={t("card.loading", { name: cardName })}
      />
    );
  }

  // Two distinct art failures collapse to the same deliberate text tile:
  //   - `!src`: art resolution finished with no image (issue #6156 — tokens with
  //     no official paper printing, e.g. Kibo, Uktabi Prince's Banana, resolve to
  //     a null token-image src). Previously these fell into the pulse branch and
  //     animated forever as a featureless dark square.
  //   - `imageError`: the resolved `<img>` failed to load.
  // Both render the card/token name (and Oracle text when known) so every artless
  // card or token — not just one hard-coded name — stays identifiable.
  if (!faceDown && (imageError || !src)) {
    return (
      <CardArtFallback
        name={cardName}
        oracleText={resolvedOracleText}
        className={baseClasses}
        style={borderStyle ?? { border: "1px solid #4b5563" }}
      />
    );
  }

  const renderedSrc = faceDown ? CARD_BACK_URL : (src ?? "");
  const renderedAlt = faceDown ? t("card.faceDownName") : cardName;

  return (
    <div className="relative inline-block w-fit select-none">
      <img
        src={renderedSrc}
        alt={renderedAlt}
        draggable={false}
        onError={() => setImageError(true)}
        className={`${baseClasses} shadow-lg object-cover`}
        style={borderStyle ?? { border: "1px solid #4b5563" }}
      />
      {unimplementedMechanics && unimplementedMechanics.length > 0 && (
        <span
          className="absolute top-0.5 left-0.5 bg-amber-500 text-black text-[8px] font-bold rounded-sm px-0.5 leading-tight"
          title={t("card.unimplemented", { mechanics: unimplementedMechanics.join(", ") })}
        >
          !
        </span>
      )}
      {tapIndicator && (
        <span
          className="absolute top-1 right-1 flex items-center justify-center rounded-full bg-black/70 p-1 shadow-md ring-1 ring-white/20"
          title={t("card.tapped")}
          aria-label={t("card.tapped")}
        >
          <ManaSymbol shard="T" size="sm" />
        </span>
      )}
    </div>
  );
}
