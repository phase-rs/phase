import { renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { WaitingFor } from "../../../adapter/types";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { useAttackRequirements } from "../useAttackRequirements.ts";
import { useBlockerConstraints } from "../useBlockerConstraints.ts";

function setWaitingFor(waitingFor: WaitingFor | undefined) {
  useGameStore.setState({ waitingFor });
}

describe("useAttackRequirements", () => {
  beforeEach(() => {
    useUiStore.setState({ selectedAttackers: [], blockerAssignments: new Map() });
  });
  afterEach(() => setWaitingFor(undefined));

  it("does not throw on an empty or undefined constraint map", () => {
    setWaitingFor(undefined);
    expect(renderHook(() => useAttackRequirements()).result.current.unsatisfiedMustAttackCount).toBe(0);

    setWaitingFor({
      type: "DeclareAttackers",
      data: { player: 0, valid_attacker_ids: [], attacker_constraints: {} },
    });
    expect(renderHook(() => useAttackRequirements()).result.current.byObject.size).toBe(0);
  });

  it("satisfies the must-attack gate when the required creature is selected (1 -> 0)", () => {
    setWaitingFor({
      type: "DeclareAttackers",
      data: {
        player: 0,
        valid_attacker_ids: [100],
        attacker_constraints: { "100": { kind: "MustAttack", players: [] } },
      },
    });

    // Unselected: one unsatisfied must-attack, status pending.
    useUiStore.setState({ selectedAttackers: [] });
    let r = renderHook(() => useAttackRequirements());
    expect(r.result.current.unsatisfiedMustAttackCount).toBe(1);
    expect(r.result.current.byObject.get(100)?.status).toBe("pending");

    // Selected: gate satisfied.
    useUiStore.setState({ selectedAttackers: [100] });
    r = renderHook(() => useAttackRequirements());
    expect(r.result.current.unsatisfiedMustAttackCount).toBe(0);
    expect(r.result.current.byObject.get(100)?.status).toBe("satisfied");
  });

  it("surfaces can't-attack as info without counting against the gate", () => {
    setWaitingFor({
      type: "DeclareAttackers",
      data: {
        player: 0,
        valid_attacker_ids: [],
        attacker_constraints: { "200": { kind: "CantAttack" } },
      },
    });
    const r = renderHook(() => useAttackRequirements());
    expect(r.result.current.byObject.get(200)?.status).toBe("info");
    expect(r.result.current.unsatisfiedMustAttackCount).toBe(0);
  });
});

describe("useBlockerConstraints", () => {
  beforeEach(() => {
    useUiStore.setState({ selectedAttackers: [], blockerAssignments: new Map() });
  });
  afterEach(() => setWaitingFor(undefined));

  it("does not throw on an empty or undefined constraint map", () => {
    setWaitingFor(undefined);
    expect(renderHook(() => useBlockerConstraints()).result.current.unsatisfiedMustBlockCount).toBe(0);
  });

  it("satisfies the must-block gate when the creature is assigned (1 -> 0)", () => {
    setWaitingFor({
      type: "DeclareBlockers",
      data: {
        player: 0,
        valid_blocker_ids: [100],
        valid_block_targets: { "100": [200] },
        blocker_constraints: { "100": { kind: "MustBlock" } },
      },
    });

    useUiStore.setState({ blockerAssignments: new Map() });
    let r = renderHook(() => useBlockerConstraints());
    expect(r.result.current.unsatisfiedMustBlockCount).toBe(1);
    expect(r.result.current.byObject.get(100)?.status).toBe("pending");

    useUiStore.setState({ blockerAssignments: new Map([[100, 200]]) });
    r = renderHook(() => useBlockerConstraints());
    expect(r.result.current.unsatisfiedMustBlockCount).toBe(0);
    expect(r.result.current.byObject.get(100)?.status).toBe("satisfied");
  });
});
