import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useCardImage } from "../../hooks/useCardImage.ts";
import { useEngineCardData } from "../../hooks/useEngineCardData.ts";
import type { TokenSearchFilters } from "../../services/scryfall.ts";
import type { FaceDownCause, TokenImageRef } from "../../adapter/types.ts";
import { CARD_BACK_URL } from "../../services/scryfall.ts";
import { faceDownMarkerName, faceDownMarkerRef } from "./faceDownMarker.ts";
import { getBevelBorderStyle } from "./cardFrame.ts";
import { getCardImageSrcSetProps } from "./cardImageSrcSet.ts";
import { CardArtFallback } from "./CardArtFallback.tsx";
import { UnimplementedMechanicsBadge } from "./UnimplementedMechanicsBadge.tsx";
import { ManaSymbol } from "../mana/ManaSymbol.tsx";

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
   * Which keyword action turned the permanent face down. Selects the marker
   * token paper play uses (Morph / Manifest / A Mysterious Creature); without
   * it — or for a cause with no printed marker — the generic card back stays.
   */
  faceDownCause?: FaceDownCause | null;
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
  faceDownCause,
  tapIndicator = false,
  oracleId,
  faceName,
  oracleText,
}: CardImageProps) {
  const { t } = useTranslation("game");
  // A face-down permanent shows the marker token for the ability that turned it
  // face down, the way paper play does. With no marker (unknown cause, or the
  // Ixidron class, which has no printing) the lookup is skipped entirely and the
  // generic card back is rendered exactly as before.
  const faceDownMarker = faceDownMarkerRef(faceDown, faceDownCause);
  const { src, isLoading } = useCardImage(faceDown ? "" : cardName, {
    size,
    faceIndex,
    isToken: faceDown ? faceDownMarker !== null : isToken,
    tokenFilters: faceDown ? undefined : tokenFilters,
    tokenImageRef: faceDown ? (faceDownMarker ?? undefined) : tokenImageRef,
    oracleId: faceDown ? undefined : oracleId,
    faceName: faceDown ? undefined : faceName,
  });
  const [imageError, setImageError] = useState(false);
  // Reset whenever the art source changes so a component instance that once saw
  // a 404 re-tries the new image: the same instance survives a permanent turning
  // face up or a DFC transforming, and would otherwise stay latched on the text
  // tile forever. Mirrors `CardPreview.tsx`'s `useEffect(… , [src])`.
  useEffect(() => setImageError(false), [src]);
  // Only resolve rules text when the art lookup has definitively failed. On the
  // first render `src` is null for every card while useCardImage is loading; an
  // eager fallback lookup here used to make all seven mulligan cards initialize
  // card-data queries even though their artwork resolved a moment later.
  const showArtFallback = !faceDown && !isLoading && (imageError || !src);
  const fallbackData = useEngineCardData(
    showArtFallback && oracleText == null ? cardName : null,
  );
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
  // The card back is the fallback in BOTH directions: a marker that never
  // resolves (`!src`) and a marker URL whose `<img>` fails to load
  // (`imageError` — offline, CDN gap, stale printing) both fall back to it. A
  // face-down permanent must never render a broken image, and it must never
  // fall through to the artless text tile either: `showArtFallback` stays gated
  // on `!faceDown`, so this is the only fallback the face-down path has.
  const renderedSrc = faceDown
    ? (imageError ? CARD_BACK_URL : (src ?? CARD_BACK_URL))
    : (src ?? "");
  const renderedAlt = faceDown
    ? (faceDownMarkerName(true, faceDownCause) ?? t("card.faceDownName"))
    : cardName;

  return (
    <div className="relative inline-block w-fit select-none">
      {showArtFallback ? (
        // Swapped in place of the `<img>` rather than early-returned, so the
        // overlay badges below stay on screen: an artless card must not also
        // lose its unimplemented-mechanics warning.
        <CardArtFallback
          name={cardName}
          oracleText={resolvedOracleText}
          className={baseClasses}
          style={borderStyle ?? { border: "1px solid #4b5563" }}
        />
      ) : (
        <img
          src={renderedSrc}
          {...getCardImageSrcSetProps(renderedSrc)}
          alt={renderedAlt}
          draggable={false}
          onError={() => setImageError(true)}
          className={`${baseClasses} shadow-lg object-cover`}
          style={borderStyle ?? { border: "1px solid #4b5563" }}
        />
      )}
      <UnimplementedMechanicsBadge mechanics={unimplementedMechanics} variant="overlay" />
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
