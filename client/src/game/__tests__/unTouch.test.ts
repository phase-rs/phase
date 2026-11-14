import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameAction } from "../../adapter/types.ts";

// A plain function, not `vi.fn`: a `vi.fn` mock attaches its own handlers to
// every promise it returns (to record settled results), which would hide a
// rejection the pointer boundary leaves unhandled.
let dispatchImpl: (action: GameAction) => Promise<void> = () => Promise.resolve();
const dispatched: GameAction[] = [];
vi.mock("../dispatch.ts", () => ({
  dispatchAction: (action: GameAction) => {
    dispatched.push(action);
    return dispatchImpl(action);
  },
}));

const { ATTACHMENT_CONTROL_ATTRIBUTE, retainTouchTracking, UN_TOUCH_ATTRIBUTE } = await import("../unTouch.ts");

/** A refused dispatch that records whether anyone handles its rejection. */
class Refusal extends Promise<void> {
  handled = false;
  override then<A = void, B = never>(
    onFulfilled?: ((value: void) => A | PromiseLike<A>) | null,
    onRejected?: ((reason: unknown) => B | PromiseLike<B>) | null,
  ): Promise<A | B> {
    if (onRejected) this.handled = true;
    return super.then(onFulfilled, onRejected);
  }
}

const touchAction: GameAction = {
  type: "UnAction",
  data: { action: { type: "TouchObject", data: { object_id: 1 } } },
};

describe("Un-card touch pointer boundary", () => {
  let card: HTMLElement;
  let elsewhere: HTMLElement;
  let release: () => void;

  beforeEach(() => {
    dispatched.length = 0;
    dispatchImpl = () => Promise.resolve();
    card = document.createElement("div");
    card.setAttribute("data-permanent-card", "1");
    card.setAttribute(UN_TOUCH_ATTRIBUTE, "1");
    elsewhere = document.createElement("div");
    document.body.append(card, elsewhere);
    release = retainTouchTracking();
  });

  afterEach(() => {
    release();
    card.remove();
    elsewhere.remove();
    delete (document as { elementFromPoint?: unknown }).elementFromPoint;
  });

  function over(target: Element, pointerId: number) {
    target.dispatchEvent(new PointerEvent("pointerover", { bubbles: true, pointerId, pointerType: "mouse" }));
  }

  function capturedMove(pointerId: number) {
    document.dispatchEvent(new PointerEvent("pointermove", { bubbles: true, pointerId, buttons: 1, pointerType: "touch" }));
  }

  it("consumes a refused touch on both pointer routes, and a later entry still touches", () => {
    const refusals: Refusal[] = [];
    dispatchImpl = () => {
      const refusal = new Refusal((_, reject) => reject(new Error("Too many of your touches are still waiting to resolve")));
      refusals.push(refusal);
      return refusal;
    };

    // Uncaptured route, then captured route: both refused by the engine.
    over(card, 1);
    let under: Element = card;
    Object.defineProperty(document, "elementFromPoint", { value: () => under, configurable: true });
    capturedMove(5);
    expect(dispatched).toEqual([touchAction, touchAction]);
    expect(refusals.map((refusal) => refusal.handled)).toEqual([true, true]);

    // A later valid entry on each route still sends exactly one touch.
    dispatchImpl = () => Promise.resolve();
    over(elsewhere, 1);
    over(card, 1);
    under = elsewhere;
    capturedMove(5);
    under = card;
    capturedMove(5);
    expect(dispatched).toEqual([touchAction, touchAction, touchAction, touchAction]);
  });

  it("treats an attachment control inside a touchable card as outside the touch region", () => {
    const control = document.createElement("button");
    control.setAttribute(ATTACHMENT_CONTROL_ATTRIBUTE, "true");
    card.append(control);

    // Uncaptured pointer route: over control, then over card face
    over(control, 1);
    expect(dispatched).toEqual([]);
    over(card, 1);
    expect(dispatched).toEqual([touchAction]);

    // Captured pointer route: move onto control, then card face
    let under: Element = control;
    Object.defineProperty(document, "elementFromPoint", { value: () => under, configurable: true });
    capturedMove(5);
    expect(dispatched).toEqual([touchAction]);
    under = card;
    capturedMove(5);
    expect(dispatched).toEqual([touchAction, touchAction]);
  });

  it("hit-tests zero-button hover moves across element coordinates", () => {
    const badge = document.createElement("span");
    card.append(badge);

    const hostRect = { left: 100, top: 100, right: 300, bottom: 400, width: 200, height: 300, x: 100, y: 100, toJSON: () => {} };
    vi.spyOn(card, "getBoundingClientRect").mockReturnValue(hostRect as DOMRect);

    const under: Element = badge;
    Object.defineProperty(document, "elementFromPoint", { value: () => under, configurable: true });

    // Pointer enters the badge outboard: clientX = 90 (left of card at 100)
    badge.dispatchEvent(new PointerEvent("pointerover", { bubbles: true, pointerId: 1, pointerType: "mouse", clientX: 90, clientY: 200 }));
    expect(dispatched).toEqual([]);

    // Zero-button move still outboard
    document.dispatchEvent(new PointerEvent("pointermove", { bubbles: true, pointerId: 1, buttons: 0, pointerType: "mouse", clientX: 95, clientY: 200 }));
    expect(dispatched).toEqual([]);

    // Zero-button move crosses inboard: clientX = 110 (one touch)
    document.dispatchEvent(new PointerEvent("pointermove", { bubbles: true, pointerId: 1, buttons: 0, pointerType: "mouse", clientX: 110, clientY: 200 }));
    expect(dispatched).toEqual([touchAction]);

    // Zero-button move within inboard area preserves touch
    document.dispatchEvent(new PointerEvent("pointermove", { bubbles: true, pointerId: 1, buttons: 0, pointerType: "mouse", clientX: 150, clientY: 200 }));
    expect(dispatched).toHaveLength(1);

    // Zero-button move outboard clears touch
    document.dispatchEvent(new PointerEvent("pointermove", { bubbles: true, pointerId: 1, buttons: 0, pointerType: "mouse", clientX: 90, clientY: 200 }));
    expect(dispatched).toHaveLength(1);

    // Zero-button move back inboard sends a new touch
    document.dispatchEvent(new PointerEvent("pointermove", { bubbles: true, pointerId: 1, buttons: 0, pointerType: "mouse", clientX: 110, clientY: 200 }));
    expect(dispatched).toEqual([touchAction, touchAction]);
  });
});
