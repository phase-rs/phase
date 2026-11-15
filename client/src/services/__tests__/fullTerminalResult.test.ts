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
  isValidFinalView,
  isValidFullTerminalDelivery,
  loadFullTerminalDelivery,
  replaceFullTerminalDelivery,
  type FullTerminalDelivery,
} from "../fullTerminalResult";
import { p2pFinalStateCommitment } from "../p2pTerminalResult";
import type { GameState, StackEntry } from "../../adapter/types";

const finalViewJsonTemplate = readFileSync(
  resolve(process.cwd(), "../fixtures/full-terminal-final-view.template.json"),
  "utf8",
).trim();
const finalView = JSON.parse(finalViewJsonTemplate) as GameState;

/** A spell entry shaped exactly as the stack renderer consumes it. */
const terminalStackEntry: StackEntry = {
  id: 10,
  source_id: 10,
  controller: 0,
  kind: { type: "Spell", data: { card_id: 101 } },
};

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
    expect(isValidFullTerminalDelivery({ ...delivery, finalView: {} as never })).toBe(false);
    expect(isValidFinalView({})).toBe(false);
    // The result screen labels players; a view without typed player records
    // cannot be retained, acknowledged, or installed.
    expect(isValidFinalView({ players: [], objects: {} })).toBe(false);
    expect(isValidFinalView({
      players: [{ id: 0, life: "20" }],
      objects: {},
      battlefield: [],
      stack: [],
      exile: [],
      turn_number: 1,
      active_player: 0,
      priority_player: 0,
      phase: "PreCombatMain",
      waiting_for: { type: "GameOver" },
    })).toBe(false);
    expect(isValidFinalView({
      players: [{ id: 0, life: 20, is_eliminated: false }, { id: 1, life: 0, is_eliminated: true }],
      objects: {},
      battlefield: [],
      stack: [],
      exile: [],
      turn_number: 12,
      active_player: 0,
      priority_player: 0,
      phase: "PreCombatMain",
      waiting_for: { type: "GameOver", data: { winner: 0 } },
    })).toBe(true);
    expect(isValidFinalView({
      players: [{ id: 0, life: 20, is_eliminated: false }, { id: 1, life: 0, is_eliminated: true }],
      objects: {},
      battlefield: [1],
      stack: [terminalStackEntry],
      exile: [2],
      turn_number: 12,
      active_player: 0,
      priority_player: 0,
      phase: "PreCombatMain",
      waiting_for: { type: "GameOver", data: { winner: 0 } },
    })).toBe(true);
    // Malformed nested entries across stack, battlefield, and exile are rejected.
    expect(isValidFinalView({
      ...finalView,
      stack: [null],
    })).toBe(false);
    expect(isValidFinalView({
      ...finalView,
      stack: [{}],
    })).toBe(false);
    expect(isValidFinalView({
      ...finalView,
      stack: [{ id: "not-a-number" }],
    })).toBe(false);
    expect(isValidFinalView({
      ...finalView,
      battlefield: [null],
    })).toBe(false);
    expect(isValidFinalView({
      ...finalView,
      exile: ["bad-id"],
    })).toBe(false);
    // The board switches on `kind` and dereferences its payload, so an entry
    // carrying only an identity cannot be retained, acknowledged, or rendered.
    expect(isValidFinalView({
      ...finalView,
      stack: [{ id: 10 }],
    })).toBe(false);
    expect(isValidFinalView({
      ...finalView,
      stack: [{ id: 10, source_id: 10, controller: 0 }],
    })).toBe(false);
    expect(isValidFinalView({
      ...finalView,
      stack: [{ ...terminalStackEntry, kind: { type: "Spell", data: {} } }],
    })).toBe(false);
    expect(isValidFinalView({
      ...finalView,
      stack: [{
        ...terminalStackEntry,
        kind: { type: "TriggeredAbility", data: { source_id: 10 } },
      }],
    })).toBe(false);
    expect(isValidFinalView({
      ...finalView,
      stack: [{
        ...terminalStackEntry,
        kind: { type: "TriggeredAbility", data: { source_id: 10, ability: { targets: [] } } },
      }],
    })).toBe(true);
    // Regression for GH #9086: Rust serializes leaf abilities with `sub_ability: null`
    // (since `ResolvedAbility.sub_ability` has `#[serde(default)]` without
    // `skip_serializing_if`). The admission guard must accept `null` as well as
    // `undefined` for `sub_ability` and `else_ability`.
    expect(isValidFinalView({
      ...finalView,
      stack: [{
        ...terminalStackEntry,
        kind: {
          type: "TriggeredAbility",
          data: { source_id: 10, ability: { targets: [], sub_ability: null } },
        },
      }],
    })).toBe(true);
    // Nested chain ending in null should also be accepted.
    expect(isValidFinalView({
      ...finalView,
      stack: [{
        ...terminalStackEntry,
        kind: {
          type: "TriggeredAbility",
          data: {
            source_id: 10,
            ability: {
              targets: [],
              sub_ability: { targets: [], sub_ability: null },
            },
          },
        },
      }],
    })).toBe(true);
    // `else_ability: null` should also be accepted for consistency.
    expect(isValidFinalView({
      ...finalView,
      stack: [{
        ...terminalStackEntry,
        kind: {
          type: "TriggeredAbility",
          data: {
            source_id: 10,
            ability: { targets: [], else_ability: null },
          },
        },
      }],
    })).toBe(true);
    expect(isValidFinalView({
      ...finalView,
      stack: [{ ...terminalStackEntry, kind: { type: "UnknownKind", data: {} } }],
    })).toBe(false);
    // A mid-resolution entry is the same renderable type and gets the same guard.
    expect(isValidFinalView({
      ...finalView,
      resolving_stack_entry: { id: 10 },
    })).toBe(false);
    expect(isValidFinalView({
      ...finalView,
      resolving_stack_entry: terminalStackEntry,
    })).toBe(true);
    expect(isValidFinalView(finalView)).toBe(true);
    expect(isValidFullTerminalDelivery({
      ...delivery,
      finalView: { ...finalView, players: [{ id: 0, life: "12" }] } as never,
    })).toBe(false);
    expect(isValidFullTerminalDelivery({
      ...delivery,
      finalView: { ...finalView, stack: [null] } as never,
    })).toBe(false);
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
