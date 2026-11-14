import { dispatchAction } from "./dispatch.ts";

/**
 * Carried by a permanent while the local player's pointer entering it would be
 * reported to the engine as an Un-card touch (`UnAction::TouchObject`). The
 * engine decides which objects are touchable (`un_actions.touchable`); the card
 * only marks itself so the hit-test below can find it.
 */
export const UN_TOUCH_ATTRIBUTE = "data-un-touch-object-id";

/**
 * Carried by an attachment control (e.g. collapsed-count or fan button) rendered
 * within a permanent's DOM tree but outside its physical card face.
 */
export const ATTACHMENT_CONTROL_ATTRIBUTE = "data-attachment-control";

/**
 * Carried by a keyword badge (e.g. trample, flying) rendered in the keyword
 * strip that straddles the card's top-left edge. The overhanging portion is
 * outside the card face and must not register as a touch on the host.
 */
export const KEYWORD_BADGE_ATTRIBUTE = "data-keyword-badge";

/** Carried by every rendered permanent, touchable or not. */
const PERMANENT_CARD_ATTRIBUTE = "data-permanent-card";

/** The touchable object each pointer is currently over, if any. */
const overByPointer = new Map<number, number | null>();

/**
 * Pointer `pointerId` is now over touchable `objectId` (or over none). Entering
 * a touchable object is one touch; staying on it is not another.
 */
function pointerOverTouchable(pointerId: number, objectId: number | null): void {
  if ((overByPointer.get(pointerId) ?? null) === objectId) return;
  overByPointer.set(pointerId, objectId);
  if (objectId === null) return;
  // A touch the engine refuses (over its bound, or the object just left) is
  // already reported by dispatch; the pointer has nothing left to do with it.
  dispatchAction({
    type: "UnAction",
    data: { action: { type: "TouchObject", data: { object_id: objectId } } },
  }).catch(() => {});
}

/**
 * The touchable object at `element`: the innermost permanent containing it, if
 * that permanent is marked touchable. An attachment is rendered inside its
 * host's element but is its own card, so a pointer on an attachment's exposed
 * edge touches the attachment, never the host beneath it. An attachment
 * control (such as the collapsed `+N` button or the attachment fan button) is
 * outside the host's touch region. A keyword badge in the keyword strip
 * straddles the card's top-left edge; its overhanging portion is outside the
 * card face and must not register as a touch on the host.
 */
function touchableAt(element: Element | null | undefined): number | null {
  if (element?.closest(`[${ATTACHMENT_CONTROL_ATTRIBUTE}]`)) return null;
  if (element?.closest(`[${KEYWORD_BADGE_ATTRIBUTE}]`)) return null;
  const card = element?.closest(`[${PERMANENT_CARD_ATTRIBUTE}]`);
  const marked = card?.getAttribute(UN_TOUCH_ATTRIBUTE);
  if (marked == null) return null;
  const objectId = Number(marked);
  return Number.isInteger(objectId) ? objectId : null;
}

/**
 * Every pointer crossing into an element — a mouse moving, or a finger landing
 * — reports the card it is now on. Both routes below resolve that card through
 * `touchableAt`, so one entry is never counted twice.
 */
function onPointerOver(event: PointerEvent): void {
  pointerOverTouchable(event.pointerId, touchableAt(event.target as Element | null));
}

/**
 * A pressed pointer may be captured — by `useLongPress` on the card it went
 * down on, or implicitly by the browser for a finger — and a captured pointer
 * reports its crossings only against the capturing element. Hit-test its moves
 * instead.
 */
function onPointerMove(event: PointerEvent): void {
  if (event.buttons === 0) return;
  pointerOverTouchable(event.pointerId, touchableAt(document.elementFromPoint?.(event.clientX, event.clientY)));
}

/** A pointer that left the window is over nothing; coming back is a new entry. */
function onPointerOut(event: PointerEvent): void {
  if (event.relatedTarget === null) pointerOverTouchable(event.pointerId, null);
}

/** A lifted finger stops touching; its next contact is a new touch. */
function onPointerEnd(event: PointerEvent): void {
  if (event.pointerType === "touch") overByPointer.delete(event.pointerId);
}

const listeners: [string, (event: PointerEvent) => void][] = [
  ["pointerover", onPointerOver],
  ["pointermove", onPointerMove],
  ["pointerout", onPointerOut],
  ["pointerup", onPointerEnd],
  ["pointercancel", onPointerEnd],
];

let listenerHolders = 0;

/**
 * Track touches while at least one touchable card is mounted. Returns the
 * release function for a `useEffect` cleanup.
 */
export function retainTouchTracking(): () => void {
  if (listenerHolders === 0) {
    for (const [type, listener] of listeners) {
      document.addEventListener(type, listener as EventListener, { capture: true, passive: true });
    }
  }
  listenerHolders += 1;
  let released = false;
  return () => {
    if (released) return;
    released = true;
    listenerHolders -= 1;
    if (listenerHolders > 0) return;
    for (const [type, listener] of listeners) {
      document.removeEventListener(type, listener as EventListener, { capture: true });
    }
    overByPointer.clear();
  };
}
