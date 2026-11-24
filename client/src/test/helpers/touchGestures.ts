import { act, fireEvent } from "@testing-library/react";
import { vi } from "vitest";

const FINGER = { pointerType: "touch", pointerId: 1, isPrimary: true, button: 0 } as const;

const FOCUSABLE = 'button:not([disabled]), a[href], input, select, textarea, [tabindex]:not([tabindex="-1"])';

/**
 * The element a browser sends the rest of a finger's press to once its
 * `pointerdown` has run: the nearest element, from the pressed one up, that
 * captured the pointer (`setPointerCapture`), else the pressed element, which a
 * touch pointer is captured to implicitly. While captured, the press's
 * `pointerup` and the `click` that follows target the capturing element, as
 * the Pointer Events spec routes them. happy-dom records `setPointerCapture`
 * but does not route events by it, so these gestures route them themselves.
 */
function pressTarget(pressed: Element): Element {
  for (let element: Element | null = pressed; element; element = element.parentElement) {
    if (element.hasPointerCapture(FINGER.pointerId)) return element;
  }
  return pressed;
}

/** Ends a press the way a browser does after `pointerdown`: up and click at the press target. */
function lift(pressed: Element) {
  const target = pressTarget(pressed);
  fireEvent.pointerUp(target, FINGER);
  fireEvent.pointerLeave(pressed, FINGER);
  // The tap focuses the focusable element under the finger, then clicks.
  const focusable = pressed.closest<HTMLElement>(FOCUSABLE);
  if (focusable) act(() => focusable.focus());
  fireEvent.click(target);
}

/** A finger tapping `element`: a primary touch press, routed as a browser routes it. */
export function touchTap(element: Element) {
  fireEvent.pointerEnter(element, FINGER);
  fireEvent.pointerDown(element, FINGER);
  lift(element);
}

/**
 * A finger pressing `element` and holding it for `holdMs` before lifting,
 * routed as a browser routes it. Needs `vi.useFakeTimers()`.
 */
export function touchHold(element: Element, holdMs: number) {
  fireEvent.pointerEnter(element, FINGER);
  fireEvent.pointerDown(element, FINGER);
  act(() => {
    vi.advanceTimersByTime(holdMs);
  });
  lift(element);
}
