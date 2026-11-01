import { beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

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
import { p2pFinalStateCommitment } from "../p2pTerminalResult";
import type { GameState } from "../../adapter/types";

const finalViewJsonTemplate = readFileSync(
  resolve(process.cwd(), "../fixtures/full-terminal-final-view.template.json"),
  "utf8",
).trim();
const finalView = JSON.parse(finalViewJsonTemplate) as GameState;

const delivery: FullTerminalDelivery = {
  key: { game_code: "TERM01", generation: 3 },
  terminalRevision: 8,
  deliveryId: "delivery-0",
  credential: "credential-0",
  display: { winner: 1, reason: "Match conceded" },
  finalView,
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
      await commitFullTerminalDelivery({ ...delivery, deliveryId: "delivery-1" }),
    ).toBe(false);
    expect(idbSet).not.toHaveBeenCalled();

    expect(
      await replaceFullTerminalDelivery({ ...delivery, deliveryId: "delivery-1" }),
    ).toBe(true);
    expect(idbSet).toHaveBeenCalledWith(
      "phase-full-terminal:TERM01:3",
      expect.objectContaining({ deliveryId: "delivery-1" }),
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

  it("retains the local final view when an acknowledged server read omits its payload", async () => {
    vi.mocked(idbGet).mockResolvedValueOnce(delivery);
    const afterAck: FullTerminalDelivery = { ...delivery };
    delete afterAck.finalView;

    expect(await replaceFullTerminalDelivery(afterAck)).toBe(true);
    expect(idbSet).toHaveBeenCalledWith(
      "phase-full-terminal:TERM01:3",
      expect.objectContaining({ finalView, deliveryId: delivery.deliveryId }),
      expect.anything(),
    );
  });

  it("preserves the P2P final-state commitment through terminal JSON recovery", async () => {
    const recoveredFinalView = JSON.parse(finalViewJsonTemplate) as GameState;
    expect(recoveredFinalView).toEqual(finalView);
    await expect(p2pFinalStateCommitment(recoveredFinalView)).resolves.toBe(
      await p2pFinalStateCommitment(finalView),
    );
  });
});
