import { describe, expect, it } from "vitest";

import {
  TABLETOP_ART_ONLY_DEPTH_RATIO,
  TABLETOP_BOTTOM_FRAME_DEPTH_RATIO,
  TABLETOP_CARD_COLLAPSE_DURATION_SECONDS,
  TABLETOP_CARD_COLLAPSE_HOLD_FRACTION,
  TABLETOP_COLLAPSED_PERMANENT_DEPTH_RATIO,
  TABLETOP_COLLAPSED_TEXTURE_RATIO,
  tabletopCardCollapseDuration,
  tabletopCardCollapseProgress,
  tabletopCardCollapseTransform,
  tabletopCardRestPose,
  tabletopCardSettleResponse,
  tabletopCardStatUv,
  collapsedTabletopCardV,
} from "../tabletopCardCollapse.ts";

describe("tabletopCardCollapseTransform", () => {
  it("finishes top-anchored with the art and bottom frame retained", () => {
    expect(tabletopCardCollapseTransform(0)).toEqual({
      easedProgress: 0,
      depthScale: 1,
      centerOffsetInCardDepths: 0,
      visibleTextureRatio: 1,
    });

    expect(tabletopCardCollapseTransform(1)).toEqual({
      easedProgress: 1,
      depthScale: TABLETOP_COLLAPSED_PERMANENT_DEPTH_RATIO,
      centerOffsetInCardDepths:
        (TABLETOP_COLLAPSED_PERMANENT_DEPTH_RATIO - 1) / 2,
      visibleTextureRatio: TABLETOP_COLLAPSED_TEXTURE_RATIO,
    });
  });

  it("retains exactly the art and one bottom-frame rail from the source", () => {
    const transform = tabletopCardCollapseTransform(1);

    expect(
      transform.visibleTextureRatio - TABLETOP_BOTTOM_FRAME_DEPTH_RATIO,
    ).toBeCloseTo(TABLETOP_ART_ONLY_DEPTH_RATIO);
    expect(transform.depthScale).toBeGreaterThan(
      transform.visibleTextureRatio,
    );
  });

  it("crops the main texture without moving its top edge", () => {
    expect(collapsedTabletopCardV(1, TABLETOP_COLLAPSED_TEXTURE_RATIO))
      .toBe(1);
    expect(collapsedTabletopCardV(0, TABLETOP_COLLAPSED_TEXTURE_RATIO))
      .toBeCloseTo(1 - TABLETOP_COLLAPSED_TEXTURE_RATIO);
  });

  it("maps a dedicated overlay onto the original live stat box", () => {
    const bottomLeft = tabletopCardStatUv(0, 0);
    const topRight = tabletopCardStatUv(1, 1);

    expect(bottomLeft.u).toBeCloseTo(0.762);
    expect(bottomLeft.v).toBeCloseTo(0.04);
    expect(topRight.u).toBeCloseTo(0.96);
    expect(topRight.v).toBeCloseTo(0.112);
  });

  it("settles the frame quickly after a short arrival beat", () => {
    expect(tabletopCardCollapseProgress(0)).toBe(0);
    expect(
      tabletopCardCollapseProgress(
        TABLETOP_CARD_COLLAPSE_DURATION_SECONDS
          * TABLETOP_CARD_COLLAPSE_HOLD_FRACTION,
      ),
    ).toBe(0);
    expect(
      tabletopCardCollapseProgress(TABLETOP_CARD_COLLAPSE_DURATION_SECONDS),
    ).toBe(1);
    expect(TABLETOP_CARD_COLLAPSE_DURATION_SECONDS).toBeLessThan(0.5);
  });

  it("honors the persisted animation-speed setting, including instant mode", () => {
    expect(tabletopCardCollapseDuration(1)).toBe(
      TABLETOP_CARD_COLLAPSE_DURATION_SECONDS,
    );
    expect(tabletopCardCollapseDuration(0.5)).toBeCloseTo(
      TABLETOP_CARD_COLLAPSE_DURATION_SECONDS / 2,
    );
    expect(tabletopCardCollapseProgress(0, tabletopCardCollapseDuration(0))).toBe(1);

    expect(tabletopCardSettleResponse(1 / 60, 0)).toBe(1);
    expect(tabletopCardSettleResponse(1 / 60, 0.5)).toBeGreaterThan(
      tabletopCardSettleResponse(1 / 60, 1),
    );
    expect(tabletopCardSettleResponse(1 / 60, 2)).toBeLessThan(
      tabletopCardSettleResponse(1 / 60, 1),
    );
  });
});

describe("tabletopCardRestPose", () => {
  it("shows attack selection as the normal tapped pose without lunging", () => {
    const selectedAttacker = tabletopCardRestPose(
      [1.2, 0.16, -2.4],
      0.3,
      false,
      true,
      false,
    );
    const tappedPermanent = tabletopCardRestPose(
      [1.2, 0.16, -2.4],
      0.3,
      true,
      false,
      false,
    );

    expect(selectedAttacker).toEqual(tappedPermanent);
    expect(selectedAttacker.x).toBe(1.2);
    expect(selectedAttacker.y).toBe(0.16);
    expect(selectedAttacker.z).toBe(-2.4);
    expect(selectedAttacker.rotationY).toBeCloseTo(0.3 + Math.PI / 4);
  });
});
