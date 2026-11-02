import { dispatchAction } from "./dispatch.ts";

/**
 * Carried by a permanent while the local player's pointer entering it would be
 * reported to the engine as an Un-card touch (`UnAction::TouchObject`). The
 * engine decides which objects are touchable (`un_actions.touchable`); the card
 * only marks itself so the hit-test below can find it.
 */
export const UN_TOUCH_ATTRIBUTE = "data-un-touch-object-id";

/** The touchable object each pointer is currently over, if any. */
const overByPointer = new Map<number, number | null>();

/**
 * Pointer `pointerId` is now over touchable `objectId` (or over none). Entering
 * a touchable object is one touch; staying on it is not another. Both the
 * card's own `pointerenter` and the captured-move hit-test report through here,
 * so one entry is never counted twice.
 */
export function pointerOverTouchable(pointerId: number, objectId: number | null): void {
  if ((overByPointer.get(pointerId) ?? null) === objectId) return;
  overByPointer.set(pointerId, objectId);
  if (objectId === null) return;
  void dispatchAction({
    type: "UnAction",
    data: { action: { type: "TouchObject", data: { object_id: objectId } } },
  });
}

/** Pointer `pointerId` left touchable `objectId`; a later entry is a new touch. */
export function pointerLeftTouchable(pointerId: number, objectId: number): void {
  if (overByPointer.get(pointerId) === objectId) overByPointer.set(pointerId, null);
}

function touchableUnder(x: number, y: number): number | null {
  const host = document
    .elementFromPoint?.(x, y)
    ?.closest<HTMLElement>(`[${UN_TOUCH_ATTRIBUTE}]`);
  if (!host) return null;
  const objectId = Number(host.getAttribute(UN_TOUCH_ATTRIBUTE));
  return Number.isInteger(objectId) ? objectId : null;
}

/**
 * A pressed pointer may be captured — by `useLongPress` on the card it went
 * down on, or implicitly by the browser for a finger — and a captured pointer
 * never delivers `pointerenter` to the cards it slides onto. Hit-test its moves
 * instead. An unpressed pointer is never captured; its card's own enter and
 * leave handlers report it.
 */
function onPointerMove(event: PointerEvent): void {
  if (event.buttons === 0) return;
  pointerOverTouchable(event.pointerId, touchableUnder(event.clientX, event.clientY));
}

/** A lifted finger stops touching; its next contact is a new touch. */
function onPointerEnd(event: PointerEvent): void {
  if (event.pointerType === "touch") overByPointer.delete(event.pointerId);
}

let listenerHolders = 0;

/**
 * Install the document-level hit-test while at least one touchable card is
 * mounted. Returns the release function for a `useEffect` cleanup.
 */
export function retainCapturedTouchTracking(): () => void {
  if (listenerHolders === 0) {
    document.addEventListener("pointermove", onPointerMove, { capture: true, passive: true });
    document.addEventListener("pointerup", onPointerEnd, { capture: true, passive: true });
    document.addEventListener("pointercancel", onPointerEnd, { capture: true, passive: true });
  }
  listenerHolders += 1;
  let released = false;
  return () => {
    if (released) return;
    released = true;
    listenerHolders -= 1;
    if (listenerHolders > 0) return;
    document.removeEventListener("pointermove", onPointerMove, { capture: true });
    document.removeEventListener("pointerup", onPointerEnd, { capture: true });
    document.removeEventListener("pointercancel", onPointerEnd, { capture: true });
    overByPointer.clear();
  };
}
