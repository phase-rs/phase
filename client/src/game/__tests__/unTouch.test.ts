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

vi.mock("../../components/animation/cardVfx/cardAnchors.ts", () => ({
  measureSurface: vi.fn((el: HTMLElement) => ({
    pose: {
      x: el.offsetLeft + el.offsetWidth / 2,
      y: el.offsetTop + el.offsetHeight / 2,
      w: el.offsetWidth,
      h: el.offsetHeight,
      angleDeg: 0,
    },
    opacity: 1,
  })),
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

  it("hit-tests zero-button hover moves across element coordinates", async () => {
    const badge = document.createElement("span");
    card.append(badge);

    const hostRect = { left: 100, top: 100, right: 300, bottom: 400, width: 200, height: 300, x: 100, y: 100, toJSON: () => {} };
    vi.spyOn(card, "getBoundingClientRect").mockReturnValue(hostRect as DOMRect);

    // Mock measureSurface to return 0° upright pose at (200, 250) with 200x300 dimensions.
    const { measureSurface } = await import("../../components/animation/cardVfx/cardAnchors.ts");
    vi.mocked(measureSurface).mockReturnValue({
      pose: {
        x: 200,
        y: 250,
        w: 200,
        h: 300,
        angleDeg: 0,
      },
      opacity: 1,
    });

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

  it("hit-tests pointer motion against a rotated card face", async () => {
    const badge = document.createElement("span");
    card.append(badge);

    // Card tapped with 17° rotation (MTGA default)
    card.setAttribute("data-tap-angle", "17");

    // Untransformed card is 200x300 centered at (200, 250).
    // Rotated by 17°, the axis-aligned bounding box spans [60.51, 339.49] x [77.31, 422.69].
    // A point at (110, 150) lies within this bounding box, but outside the rotated card face.
    const hostRect = {
      left: 60.51,
      top: 77.31,
      right: 339.49,
      bottom: 422.69,
      width: 278.98,
      height: 345.38,
      x: 60.51,
      y: 77.31,
      toJSON: () => {},
    };
    vi.spyOn(card, "getBoundingClientRect").mockReturnValue(hostRect as DOMRect);

    // Mock measureSurface to return the 17° rotated pose.
    const { measureSurface } = await import("../../components/animation/cardVfx/cardAnchors.ts");
    vi.mocked(measureSurface).mockReturnValue({
      pose: {
        x: 200,
        y: 250,
        w: 200,
        h: 300,
        angleDeg: 17,
      },
      opacity: 1,
    });

    let under: Element = badge;
    Object.defineProperty(document, "elementFromPoint", { value: () => under, configurable: true });

    // Route 1 (hovering uncaptured pointer):
    // Pointer enters the badge overhang: clientX = 110, clientY = 150 (outside rotated face)
    badge.dispatchEvent(new PointerEvent("pointerover", { bubbles: true, pointerId: 1, pointerType: "mouse", clientX: 110, clientY: 150 }));
    expect(dispatched).toEqual([]);

    // Zero-button move still outboard sends zero touches
    document.dispatchEvent(new PointerEvent("pointermove", { bubbles: true, pointerId: 1, buttons: 0, pointerType: "mouse", clientX: 115, clientY: 150 }));
    expect(dispatched).toEqual([]);

    // Zero-button move crosses onto the rotated card face: clientX = 135, clientY = 150 (one touch)
    document.dispatchEvent(new PointerEvent("pointermove", { bubbles: true, pointerId: 1, buttons: 0, pointerType: "mouse", clientX: 135, clientY: 150 }));
    expect(dispatched).toEqual([touchAction]);

    // Zero-button move to host face center (200, 250) preserves touch
    under = card;
    card.dispatchEvent(new PointerEvent("pointerover", { bubbles: true, pointerId: 1, pointerType: "mouse", clientX: 200, clientY: 250 }));
    document.dispatchEvent(new PointerEvent("pointermove", { bubbles: true, pointerId: 1, buttons: 0, pointerType: "mouse", clientX: 200, clientY: 250 }));
    expect(dispatched).toHaveLength(1);

    // Moving back to badge inboard preserves touch
    under = badge;
    badge.dispatchEvent(new PointerEvent("pointerover", { bubbles: true, pointerId: 1, pointerType: "mouse", clientX: 135, clientY: 150 }));
    document.dispatchEvent(new PointerEvent("pointermove", { bubbles: true, pointerId: 1, buttons: 0, pointerType: "mouse", clientX: 135, clientY: 150 }));
    expect(dispatched).toHaveLength(1);

    // Zero-button move outboard to overhang clears touch
    document.dispatchEvent(new PointerEvent("pointermove", { bubbles: true, pointerId: 1, buttons: 0, pointerType: "mouse", clientX: 110, clientY: 150 }));
    expect(dispatched).toHaveLength(1);

    // Zero-button move back inboard sends a new touch
    document.dispatchEvent(new PointerEvent("pointermove", { bubbles: true, pointerId: 1, buttons: 0, pointerType: "mouse", clientX: 135, clientY: 150 }));
    expect(dispatched).toEqual([touchAction, touchAction]);

    // Moving away to elsewhere ends touch
    under = elsewhere;
    elsewhere.dispatchEvent(new PointerEvent("pointerover", { bubbles: true, pointerId: 1, pointerType: "mouse", clientX: 500, clientY: 500 }));
    document.dispatchEvent(new PointerEvent("pointermove", { bubbles: true, pointerId: 1, buttons: 0, pointerType: "mouse", clientX: 500, clientY: 500 }));
    expect(dispatched).toHaveLength(2);

    // Re-entering host face sends a third touch
    under = card;
    card.dispatchEvent(new PointerEvent("pointerover", { bubbles: true, pointerId: 1, pointerType: "mouse", clientX: 200, clientY: 250 }));
    expect(dispatched).toEqual([touchAction, touchAction, touchAction]);

    // Route 2 (captured touch pointer):
    // Moving to badge overhang sends no touch
    under = badge;
    capturedMove(5);
    document.dispatchEvent(new PointerEvent("pointermove", { bubbles: true, pointerId: 5, buttons: 1, pointerType: "touch", clientX: 110, clientY: 150 }));
    expect(dispatched).toHaveLength(3);

    // Crossing inboard sends 4th touch
    document.dispatchEvent(new PointerEvent("pointermove", { bubbles: true, pointerId: 5, buttons: 1, pointerType: "touch", clientX: 135, clientY: 150 }));
    expect(dispatched).toEqual([touchAction, touchAction, touchAction, touchAction]);

    // Moving to host face preserves touch
    under = card;
    document.dispatchEvent(new PointerEvent("pointermove", { bubbles: true, pointerId: 5, buttons: 1, pointerType: "touch", clientX: 200, clientY: 250 }));
    expect(dispatched).toHaveLength(4);

    // Moving back to badge inboard preserves touch
    under = badge;
    document.dispatchEvent(new PointerEvent("pointermove", { bubbles: true, pointerId: 5, buttons: 1, pointerType: "touch", clientX: 135, clientY: 150 }));
    expect(dispatched).toHaveLength(4);

    // Moving back to host face preserves touch
    under = card;
    document.dispatchEvent(new PointerEvent("pointermove", { bubbles: true, pointerId: 5, buttons: 1, pointerType: "touch", clientX: 200, clientY: 250 }));
    expect(dispatched).toHaveLength(4);
  });
});
