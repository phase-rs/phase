import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("idb-keyval", () => ({
  createStore: vi.fn(() => ({})),
  del: vi.fn().mockResolvedValue(undefined),
  get: vi.fn().mockResolvedValue(undefined),
  set: vi.fn().mockResolvedValue(undefined),
}));

import { get as idbGet, set as idbSet } from "idb-keyval";
import {
  commitFullTerminalDelivery,
  isValidFullTerminalDelivery,
  loadFullTerminalDelivery,
  replaceFullTerminalDelivery,
  type FullTerminalDelivery,
} from "../fullTerminalResult";

const delivery: FullTerminalDelivery = {
  key: { game_code: "TERM01", generation: 3 },
  terminal_revision: 8,
  delivery_id: "delivery-0",
  credential: "credential-0",
  display: { winner: 1, reason: "Match conceded" },
};

describe("full terminal result persistence", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("commits a first delivery and loads it from the isolated namespace", async () => {
    expect(await commitFullTerminalDelivery(delivery)).toBe(true);
    expect(idbSet).toHaveBeenCalledWith(
      "phase-full-terminal:TERM01:3",
      delivery,
      expect.anything(),
    );

    vi.mocked(idbGet).mockResolvedValueOnce(delivery);
    await expect(loadFullTerminalDelivery(delivery.key)).resolves.toEqual(delivery);
  });

  it("requires an explicit replacement for a changed delivery tuple", async () => {
    vi.mocked(idbGet).mockResolvedValueOnce(delivery);
    expect(
      await commitFullTerminalDelivery({ ...delivery, delivery_id: "delivery-1" }),
    ).toBe(false);
    expect(idbSet).not.toHaveBeenCalled();

    expect(
      await replaceFullTerminalDelivery({ ...delivery, delivery_id: "delivery-1" }),
    ).toBe(true);
    expect(idbSet).toHaveBeenCalledWith(
      "phase-full-terminal:TERM01:3",
      expect.objectContaining({ delivery_id: "delivery-1" }),
      expect.anything(),
    );
  });

  it("does not revive a legacy snapshot as a terminal delivery", async () => {
    const legacySnapshot = { waiting_for: { type: "GameOver" }, players: [] };
    vi.mocked(idbGet).mockResolvedValueOnce(legacySnapshot);

    expect(isValidFullTerminalDelivery(legacySnapshot)).toBe(false);
    await expect(loadFullTerminalDelivery(delivery.key)).resolves.toBeNull();
    await expect(commitFullTerminalDelivery(legacySnapshot as never)).resolves.toBe(false);
  });
});
