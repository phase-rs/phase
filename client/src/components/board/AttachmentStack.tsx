import { memo } from "react";
import type { MouseEvent } from "react";

import type { ObjectId } from "../../adapter/types.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import { useCardHover } from "../../hooks/useCardHover.ts";
import { useIsValidObjectTarget } from "../../hooks/useIsValidTarget.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { formatCounterType } from "../../viewmodel/cardProps.ts";
import { ArtCropCard } from "../card/ArtCropCard.tsx";
import { CardImage } from "../card/CardImage.tsx";
import { cardImageLookup } from "../../services/cardImageLookup.ts";

interface AttachmentStackProps {
  objectIds: ObjectId[];
}

// Horizontal reveal: each attachment peeks to the right of the host by this
// fraction of its own width. 0.45 = the right ~45% of the attached card is
// visible past the host's right edge — enough for the title bar plus part of
// the art. Each subsequent attachment in a stack reveals a further fraction
// beyond the previous one and shifts down slightly so Auras under Auras don't
// fully occlude each other.
const PEEK_REVEAL = 0.45;
const PEEK_STAGGER = 0.22;
const PEEK_VERTICAL_DROP = 28; // px between stacked attachments

// Subtype-tinted frame ring so the player can identify Aura vs Equipment vs
// Fortification at a glance — colors mirror the MTG card-frame conventions
// (gold for enchantments, silver for artifacts) so the highlight reads as
// "this card is THAT type" without any text label.
function frameTint(subtypes: string[], faceDown: boolean): string {
  if (faceDown) return "ring-2 ring-slate-500/80 shadow-[0_0_8px_1px_rgba(100,116,139,0.6)]";
  if (subtypes.includes("Aura")) return "ring-2 ring-amber-400/90 shadow-[0_0_10px_2px_rgba(251,191,36,0.55)]";
  if (subtypes.includes("Equipment")) return "ring-2 ring-zinc-300/90 shadow-[0_0_10px_2px_rgba(228,228,231,0.55)]";
  if (subtypes.includes("Fortification")) return "ring-2 ring-stone-400/90 shadow-[0_0_10px_2px_rgba(168,162,158,0.55)]";
  return "ring-2 ring-slate-400/80 shadow-[0_0_8px_1px_rgba(148,163,184,0.5)]";
}

/**
 * Staggered card stack rendered above the host PermanentCard. Each attached
 * Equipment/Aura/Fortification appears as a real card image (matching the
 * host's display preference — full-card or art-crop), tucked partially behind
 * the host so the host frame reads as "wearing" the attachments. Replaces the
 * earlier chip-pill design which proved too abstract — players want to see
 * the card.
 *
 * Click / hover / targeting behavior mirrors what the chips supported:
 * - Click a peek-card: dispatch ChooseTarget if the local player is being
 *   prompted to pick targets and this object is legal, else select it.
 * - Hover / long-press: surface the card preview (data-card-hover invariant
 *   preserved via useCardHover so usePreviewDismiss continues to work).
 * - Targeting glow: amber ring on legal-target peek-cards, same shape as
 *   StackEntry uses, so every targetable surface in the UI reads identically.
 */
export const AttachmentStack = memo(function AttachmentStack({ objectIds }: AttachmentStackProps) {
  if (objectIds.length === 0) return null;

  return (
    <>
      {objectIds.map((id, index) => (
        <AttachmentPeek
          key={id}
          id={id}
          // Index 0 sits closest to the host (least revealed); the last index
          // sits highest in the fan. Stagger each by PEEK_STAGGER so two
          // attachments don't fully overlap.
          revealRatio={PEEK_REVEAL + index * PEEK_STAGGER}
          // z-index: closer-to-host attachments sit BEHIND further ones so the
          // top card in the fan stays fully visible. PermanentCard's main
          // image is z-10; we use 1..N so all peeks sit behind the host face.
          zIndex={1 + index}
        />
      ))}
    </>
  );
});

interface AttachmentPeekProps {
  id: ObjectId;
  revealRatio: number;
  zIndex: number;
}

const AttachmentPeek = memo(function AttachmentPeek({ id, revealRatio, zIndex }: AttachmentPeekProps) {
  const obj = useGameStore((s) => s.gameState?.objects[id]);
  const selectObject = useUiStore((s) => s.selectObject);
  const battlefieldCardDisplay = usePreferencesStore((s) => s.battlefieldCardDisplay);
  const { handlers, firedRef } = useCardHover(id);
  const isValidTarget = useIsValidObjectTarget(id);

  if (!obj) return null;

  const useArtCrop = battlefieldCardDisplay === "art_crop";
  const { name: imgName, faceIndex: imgFace } = cardImageLookup(obj);

  const handleClick = (event: MouseEvent<HTMLDivElement>) => {
    event.stopPropagation();
    if (firedRef.current) {
      firedRef.current = false;
      return;
    }
    if (isValidTarget) {
      dispatchAction({ type: "ChooseTarget", data: { target: { Object: id } } });
      return;
    }
    selectObject(id);
  };

  // Targeting wins over subtype tint — when a peek-card is a legal target
  // the player needs to see "click here for ChooseTarget" first, ahead of
  // the subtype hint. Otherwise apply the subtype-tinted frame.
  const ringClass = isValidTarget
    ? "ring-2 ring-amber-300 shadow-[0_0_12px_3px_rgba(251,191,36,0.85)]"
    : frameTint(obj.card_types.subtypes, obj.face_down);

  const counter = predominantCounter(obj.counters);
  const counterLabel = counter ? `${formatCounterType(counter.type)} ×${counter.count}` : null;
  const tooltip = counterLabel ? `${obj.name} (${counterLabel})` : obj.name;

  // Tuck the peek-card to the right of the host. The card's left edge sits
  // at the host's right edge (`left: 100%`), pulled back leftward by
  // `(1 - revealRatio) * 100%` of its own width so `revealRatio` percent
  // of the card sticks out to the right. Each subsequent attachment in the
  // stack drops PEEK_VERTICAL_DROP px so a bearer with two Auras shows both.
  const tuckPullX = `${Math.round((1 - revealRatio) * 100)}%`;

  return (
    <div
      // useCardHover's `handlers` already supplies the data-card-hover
      // invariant that usePreviewDismiss relies on; do not also set it
      // explicitly (would generate a TS "specified more than once" error).
      onClick={handleClick}
      title={tooltip}
      aria-label={tooltip}
      style={{
        position: "absolute",
        top: `${zIndex * PEEK_VERTICAL_DROP - PEEK_VERTICAL_DROP}px`,
        left: "100%",
        transform: `translateX(-${tuckPullX}) scale(0.62)`,
        transformOrigin: "top left",
        zIndex,
        cursor: "pointer",
      }}
      className={`rounded-lg ${ringClass}`}
      {...handlers}
    >
      {useArtCrop ? (
        <ArtCropCard objectId={id} />
      ) : (
        <CardImage
          cardName={imgName}
          faceIndex={imgFace}
          size="small"
          unimplementedMechanics={obj.unimplemented_mechanics}
          colors={obj.color}
          isToken={obj.display_source === "Token"}
        />
      )}
      {counterLabel && (
        <span
          aria-label={counterLabel}
          className="absolute right-1 top-1 z-10 rounded bg-emerald-600/90 px-1 text-[10px] font-bold text-white shadow"
        >
          +{counter?.count}
        </span>
      )}
      {obj.tapped && (
        <span
          aria-label="tapped"
          className="absolute left-1 top-1 z-10 inline-block h-2 w-2 rounded-full bg-amber-400 shadow"
        />
      )}
    </div>
  );
});

interface CounterSummary {
  type: string;
  count: number;
}

function predominantCounter(counters: Record<string, number | undefined>): CounterSummary | null {
  let best: CounterSummary | null = null;
  for (const [type, value] of Object.entries(counters)) {
    if (typeof value !== "number" || value <= 0) continue;
    if (best === null || value > best.count) best = { type, count: value };
  }
  return best;
}
