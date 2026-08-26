import { useTranslation } from "react-i18next";

import { usePreviewDismiss } from "../../hooks/usePreviewDismiss.ts";
import { cardImageLookup } from "../../services/cardImageLookup.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { shouldRenderCardBack } from "../../viewmodel/cardProps.ts";
import { faceDownMarkerName } from "./faceDownMarker.ts";
import { CardPreview } from "./CardPreview.tsx";

/**
 * In-game wrapper around <CardPreview> that owns the hover-frequency uiStore
 * subscriptions (inspectedObjectId / inspectedFaceIndex / isDragging / shiftHeld)
 * and resolves the inspected object's display names from game state.
 *
 * Isolating these subscriptions in a leaf keeps GamePageContent from
 * re-rendering — and cascading into the entire battlefield + Framer Motion
 * layout machinery — on every card hover. The deck-builder/draft call sites
 * pass `cardName` to <CardPreview> directly; only the in-game preview derives
 * it from the inspected game object, which is what this component does.
 */
export function GameCardPreview() {
  const { t } = useTranslation("game");
  // Lives here (not in GamePageContent) so its inspectedObjectId/previewSticky
  // subscriptions don't re-render the whole page on every hover. This component
  // is always mounted, so the dismiss listeners run for the game's full life.
  usePreviewDismiss();

  const inspectedObjectId = useUiStore((s) => s.inspectedObjectId);
  const inspectedFaceIndex = useUiStore((s) => s.inspectedFaceIndex);
  const previewPlacement = useUiStore((s) => s.previewPlacement);
  const isDragging = useUiStore((s) => s.isDragging);
  const shiftHeld = useUiStore((s) => s.shiftHeld);
  // Card-preview behavior preference. In "shift" mode the preview only renders
  // while Shift is held; in "side" mode it docks to the screen edge.
  const cardPreviewMode = usePreferencesStore((s) => s.cardPreviewMode);
  const obj = useGameStore((s) =>
    inspectedObjectId != null ? s.gameState?.objects[inspectedObjectId] ?? null : null,
  );

  // Suppress the preview while a card is being dragged — the drag ghost is the
  // visual feedback, and the inspected object would otherwise flash behind it.
  const inspectedObj = !isDragging ? obj : null;

  // Scryfall lookups must use the front-face name (scryfall-data.json indexes
  // only front faces). When a permanent has transformed, the engine swaps
  // obj.name to the back-face name — cardImageLookup recovers the front name
  // from obj.back_face. See services/cardImageLookup.ts (issue #90).
  const inspectedLookup = inspectedObj ? cardImageLookup(inspectedObj) : null;
  // A face-down permanent the viewer may look at (their own morph/manifest —
  // CR 708.5): the live face is blanked per CR 708.2a, so the PREVIEW is the
  // peek — it always shows the stored real face, no matter which face index
  // the hover carries (#7547). The battlefield tile keeps the cause marker.
  const inspectedPeekedFace =
    inspectedObj && !shouldRenderCardBack(inspectedObj) && inspectedObj.face_down
      ? (inspectedObj.back_face ?? null)
      : null;
  const inspectedCardName = inspectedObj && !shouldRenderCardBack(inspectedObj)
    ? inspectedPeekedFace
      ? inspectedPeekedFace.name
      : inspectedFaceIndex === 1 && inspectedObj.back_face
        ? inspectedObj.back_face.name
        : inspectedLookup?.name ?? inspectedObj.name
    : // An OPPONENT's face-down permanent previews as its cause MARKER (full
      // size, reminder text included) — the identity stays hidden; the image
      // itself resolves inside `CardPreview` from the object's cause (#7547).
      // With no marker printing (unknown cause from an older save, or the
      // Ixidron class — an effect turned it face down, CR 708.2a) the hover
      // still answers: the generic label routes `CardPreview` onto the plain
      // card back, which reveals nothing. BATTLEFIELD only — a face-down card
      // in a hidden zone (hideaway exile, issue #2889) keeps rendering no
      // preview at all: it has no public characteristics the back could stand
      // in for, and that row pins exactly this.
      (inspectedObj
        ? faceDownMarkerName(true, inspectedObj.face_down_cause)
          ?? (inspectedObj.zone === "Battlefield" ? t("card.faceDownName") : null)
        : null);
  // The "other" face: when viewing front, this is back_face; when viewing back,
  // this is the front. A face-down permanent has no OTHER printed face — its
  // `back_face` is the stored real face already shown by the peek.
  const inspectedOtherFaceName =
    inspectedObj?.back_face && !shouldRenderCardBack(inspectedObj) && !inspectedPeekedFace
      ? inspectedFaceIndex === 1 ? inspectedObj.name : inspectedObj.back_face.name
      : null;

  const previewSuppressed = cardPreviewMode === "shift" && !shiftHeld;

  return (
    <CardPreview
      cardName={previewSuppressed ? null : inspectedCardName}
      objectId={inspectedObj?.id ?? null}
      backFaceName={previewSuppressed ? null : inspectedOtherFaceName}
      dockSide={cardPreviewMode === "side" || previewPlacement === "side"}
      handSourceObjectId={inspectedObj?.zone === "Hand" ? inspectedObj.id : null}
    />
  );
}
