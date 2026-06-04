import { usePreviewDismiss } from "../../hooks/usePreviewDismiss.ts";
import { cardImageLookup } from "../../services/cardImageLookup.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
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
  // Lives here (not in GamePageContent) so its inspectedObjectId/previewSticky
  // subscriptions don't re-render the whole page on every hover. This component
  // is always mounted, so the dismiss listeners run for the game's full life.
  usePreviewDismiss();

  const inspectedObjectId = useUiStore((s) => s.inspectedObjectId);
  const inspectedFaceIndex = useUiStore((s) => s.inspectedFaceIndex);
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
  const inspectedCardName = inspectedObj && !inspectedObj.face_down
    ? inspectedFaceIndex === 1 && inspectedObj.back_face
      ? inspectedObj.back_face.name
      : inspectedLookup?.name ?? inspectedObj.name
    : null;
  // The "other" face: when viewing front, this is back_face; when viewing back, this is the front.
  const inspectedOtherFaceName = inspectedObj?.back_face && !inspectedObj.face_down
    ? inspectedFaceIndex === 1 ? inspectedObj.name : inspectedObj.back_face.name
    : null;

  const previewSuppressed = cardPreviewMode === "shift" && !shiftHeld;

  return (
    <CardPreview
      cardName={previewSuppressed ? null : inspectedCardName}
      backFaceName={previewSuppressed ? null : inspectedOtherFaceName}
      dockSide={cardPreviewMode === "side"}
    />
  );
}
