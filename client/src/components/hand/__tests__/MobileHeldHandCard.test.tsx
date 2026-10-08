import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import type { MobileHandGesture } from "../../../stores/uiStore.ts";
import { buildGameObject } from "../../../test/factories/gameObjectFactory.ts";
import { MobileHeldHandCard } from "../MobileHeldHandCard.tsx";

vi.mock("../../card/CardImage.tsx", () => ({
  CardImage: ({ cardName }: { cardName: string }) => <div>{cardName}</div>,
}));

afterEach(() => {
  cleanup();
  usePreferencesStore.setState({ animationSpeedMultiplier: 1 });
});

describe("MobileHeldHandCard", () => {
  const object = buildGameObject({ id: 11, zone: "Hand", name: "Lightning Bolt" });
  const gesture: MobileHandGesture = {
    objectId: object.id,
    phase: "drag",
    sourceOrigin: {
      bottom: 748,
      centerX: 280,
      height: 168,
      rotation: 0,
      top: 580,
      width: 120,
    },
    offsetX: 12,
    offsetY: -90,
    playable: true,
    castReady: true,
  };

  it("portals a hand-sized gold card at the finger's direct displacement", () => {
    render(<MobileHeldHandCard gesture={gesture} object={object} />);

    const heldCard = document.querySelector<HTMLElement>("[data-mobile-held-card]");
    expect(heldCard).not.toBeNull();
    expect(heldCard).toHaveAttribute("data-mobile-held-card-state", "cast-ready");
    expect(heldCard).toHaveAttribute("data-mobile-held-card-motion", "velocity");
    expect(heldCard).toHaveClass("ring-amber-300");
    expect(heldCard?.style.left).toBe("232px");
    expect(heldCard?.style.top).toBe("490px");
    expect(heldCard?.style.width).toBe("120px");
    expect(heldCard?.style.height).toBe("168px");
  });

  it("does not render during the stationary large-preview phase", () => {
    render(
      <MobileHeldHandCard
        gesture={{ ...gesture, phase: "preview", castReady: false }}
        object={object}
      />,
    );

    expect(document.querySelector("[data-mobile-held-card]")).toBeNull();
  });

  it("disables velocity tilt when animations are disabled", () => {
    usePreferencesStore.setState({ animationSpeedMultiplier: 0 });

    render(<MobileHeldHandCard gesture={gesture} object={object} />);

    expect(document.querySelector("[data-mobile-held-card]")).not.toHaveAttribute(
      "data-mobile-held-card-motion",
    );
  });
});
