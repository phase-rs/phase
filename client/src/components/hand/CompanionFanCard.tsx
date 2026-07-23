import { memo, useRef } from "react";
import { motion } from "framer-motion";
import type { PanInfo } from "framer-motion";
import { useTranslation } from "react-i18next";

import type { CompanionInfo } from "../../adapter/types.ts";
import { useCardImage } from "../../hooks/useCardImage.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import { DRAG_PLAY_THRESHOLD } from "../../hooks/useDragToCast.ts";
import type { ZoneTheme } from "../../viewmodel/zoneAffordance.ts";

interface CompanionFanCardProps {
  companion: CompanionInfo;
  /** Whether the global `CompanionToHand` special action is currently legal.
   *  A display affordance derived by the parent — never a mode flag. */
  canActivate: boolean;
  theme: ZoneTheme;
  rotation: number;
  arcOffset: number;
  restingY: number;
  hoverY: number;
  marginLeft: string | number;
  zIndex: number;
}

// The perspective player's companion, rendered as the trailing (far-right) card
// of the hand fan. It mirrors ZoneFanCard's resting animation (arc + tilt +
// hover lift + flick-up gesture) for visual continuity with the castable
// graveyard/exile wings, but is standalone because a companion is not a battle-
// field/zone object: it is a name-only `CompanionInfo` with no `ObjectId` and no
// printed mana cost, so it images BY NAME, renders no mana pips, and carries no
// `data-object-id`/`data-card-hover` (keeping it out of the reorder DOM sweep).
// Both its click and flick-up dispatch the GLOBAL `CompanionToHand` special
// action — its {3} activation cost is not the card's printed cost — and only
// when that action is legal (`canActivate`); otherwise the gesture is a no-op.
const CompanionFanCard = memo(function CompanionFanCard({
  companion,
  canActivate,
  theme,
  rotation,
  arcOffset,
  restingY,
  hoverY,
  marginLeft,
  zIndex,
}: CompanionFanCardProps) {
  const { t } = useTranslation("game");
  const setDragging = useUiStore((s) => s.setDragging);
  const cardName = companion.card.card.name;
  const { src } = useCardImage(cardName, { size: "normal" });
  // Suppress dragSnapToOrigin only when the flick actually activated, so a
  // short/sideways drag springs back into the fan instead of flying off.
  const playedRef = useRef(false);

  const activate = () => dispatchAction({ type: "CompanionToHand" });

  return (
    <motion.div
      layout
      initial={{ opacity: 0, y: restingY + 10 }}
      animate={{ opacity: 1, y: restingY + arcOffset, rotate: rotation }}
      exit={{ opacity: 0, scale: 0.8 }}
      whileHover={{ y: hoverY + arcOffset, scale: 1.08, zIndex: 30 }}
      whileDrag={{ scale: 1.05, zIndex: 9999 }}
      transition={{ duration: 0.25, layout: { duration: 0.15, delay: 0 } }}
      drag={canActivate}
      dragConstraints={false}
      dragElastic={0}
      dragSnapToOrigin={!playedRef.current}
      onDragStart={() => {
        playedRef.current = false;
        setDragging(true);
      }}
      onDragEnd={(_event, info: PanInfo) => {
        setDragging(false);
        // Cast-only: flick up past the threshold. Legality is already gated by
        // `canActivate` (drag is disabled otherwise), so no extra check here.
        if (info.offset.y < DRAG_PLAY_THRESHOLD) {
          playedRef.current = true;
          activate();
        }
      }}
      onClick={(e) => {
        e.stopPropagation();
        if (canActivate) activate();
      }}
      className={`relative cursor-pointer leading-[0] select-none ${
        canActivate ? "" : "cursor-default"
      }`}
      style={{ marginLeft, zIndex }}
      title={
        canActivate
          ? t("zone.companionActivate", { name: cardName })
          : t("zone.companionTitle", { name: cardName })
      }
    >
      <div
        className={`relative overflow-hidden rounded-lg border ${theme.cardBorder}`}
      >
        {src ? (
          <img
            src={src}
            alt={cardName}
            draggable={false}
            className="!h-[var(--hand-card-h)] !w-[var(--hand-card-w)] object-cover"
          />
        ) : (
          <div className="h-[var(--hand-card-h)] w-[var(--hand-card-w)] bg-gray-700" />
        )}
        {/* Translucent wash marking the companion affordance. */}
        <div className={`pointer-events-none absolute inset-0 transition-colors ${theme.overlayCard}`} />
      </div>
      {/* Identity badge — required so the purple companion card never reads as
          just another purple exile wing. */}
      <div className="absolute -top-1 left-1/2 z-10 -translate-x-1/2 rounded-sm bg-purple-700 px-1.5 py-px text-[8px] font-bold text-purple-100 shadow">
        {t("zone.companion")}
      </div>
      {/* Activatable glow ring (sibling of the clipped image so it isn't cropped). */}
      {canActivate && (
        <div className={`pointer-events-none absolute inset-0 rounded-lg ${theme.ring}`} />
      )}
    </motion.div>
  );
});

export { CompanionFanCard };
