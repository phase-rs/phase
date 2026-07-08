import { describe, expect, it } from "vitest";

import { GAME_Z, GAME_Z_LAYER } from "../ui.ts";

describe("GAME_Z", () => {
  it("keeps board-choice controls below prompt overlays but above HUD rails", () => {
    expect(GAME_Z.boardChoiceGrid).toBeGreaterThan(GAME_Z.hudRail);
    expect(GAME_Z.boardChoiceGrid).toBeLessThan(GAME_Z.dialogHost);
  });

  it("keeps Tailwind layer classes aligned with numeric layer values", () => {
    expect(GAME_Z_LAYER.board).toBe(`z-${GAME_Z.board}`);
    expect(GAME_Z_LAYER.hudRail).toBe(`z-${GAME_Z.hudRail}`);
    expect(GAME_Z_LAYER.boardChoiceGrid).toBe(`z-[${GAME_Z.boardChoiceGrid}]`);
    expect(GAME_Z_LAYER.dialogHost).toBe(`z-${GAME_Z.dialogHost}`);
    expect(GAME_Z_LAYER.modal).toBe(`z-${GAME_Z.modal}`);
    expect(GAME_Z_LAYER.floatingOverlay).toBe(`z-[${GAME_Z.floatingOverlay}]`);
    expect(GAME_Z_LAYER.nestedDialog).toBe(`z-[${GAME_Z.nestedDialog}]`);
    expect(GAME_Z_LAYER.debugPanel).toBe(`z-[${GAME_Z.debugPanel}]`);
  });
});
