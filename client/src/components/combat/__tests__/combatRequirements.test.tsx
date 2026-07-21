import { render, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { GameState, WaitingFor } from "../../../adapter/types";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { AttackRequirementBadges } from "../AttackRequirementBadges.tsx";
import { BlockerConstraintBadges } from "../BlockerConstraintBadges.tsx";
import { useAttackRequirements } from "../useAttackRequirements.ts";
import { useBlockerConstraints } from "../useBlockerConstraints.ts";

function setWaitingFor(waitingFor: WaitingFor | undefined) {
  useGameStore.setState({ waitingFor });
}

/** Stub a battlefield card element so `useObjectAnchors` finds a live anchor. */
function anchorEl(id: number) {
  const el = document.createElement("div");
  el.setAttribute("data-object-id", String(id));
  document.body.appendChild(el);
}

/** Set the engine objects map (only `name` is read by the badge components). */
function setObjects(objects: Record<string, { name: string }>) {
  useGameStore.setState({ gameState: { objects } as unknown as GameState });
}

describe("useAttackRequirements", () => {
  beforeEach(() => {
    useUiStore.setState({ selectedAttackers: [], blockerAssignments: new Map() });
  });
  afterEach(() => setWaitingFor(undefined));

  it("does not throw on an empty or undefined constraint map", () => {
    setWaitingFor(undefined);
    expect(renderHook(() => useAttackRequirements()).result.current.byObject.size).toBe(0);

    setWaitingFor({
      type: "DeclareAttackers",
      data: { player: 0, valid_attacker_ids: [], attacker_constraints: {} },
    });
    expect(renderHook(() => useAttackRequirements()).result.current.byObject.size).toBe(0);
  });

  it("flips a must-attack badge pending -> satisfied on selection (display only)", () => {
    setWaitingFor({
      type: "DeclareAttackers",
      data: {
        player: 0,
        valid_attacker_ids: [100],
        attacker_constraints: { "100": { kind: "MustAttack", players: [] } },
      },
    });

    // Unselected: the must-attack creature shows a pending badge. Crucially the
    // hook no longer exposes any confirm-gate counter — badges are display only.
    useUiStore.setState({ selectedAttackers: [] });
    let r = renderHook(() => useAttackRequirements());
    expect(r.result.current.byObject.get(100)?.status).toBe("pending");
    expect("unsatisfiedMustAttackCount" in r.result.current).toBe(false);

    // Selected: badge satisfied.
    useUiStore.setState({ selectedAttackers: [100] });
    r = renderHook(() => useAttackRequirements());
    expect(r.result.current.byObject.get(100)?.status).toBe("satisfied");
  });

  it("surfaces can't-attack as an info badge", () => {
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
  });

  it("passes engine-provided sources through unchanged (display-only)", () => {
    setWaitingFor({
      type: "DeclareAttackers",
      data: {
        player: 0,
        valid_attacker_ids: [100],
        attacker_constraints: { "100": { kind: "MustAttack", players: [], sources: [200] } },
      },
    });
    const r = renderHook(() => useAttackRequirements());
    expect(r.result.current.byObject.get(100)?.sources).toEqual([200]);
  });

  it("defaults sources to an empty array when the engine omits the field", () => {
    setWaitingFor({
      type: "DeclareAttackers",
      data: {
        player: 0,
        valid_attacker_ids: [],
        attacker_constraints: { "200": { kind: "CantAttack" } },
      },
    });
    const r = renderHook(() => useAttackRequirements());
    expect(r.result.current.byObject.get(200)?.sources).toEqual([]);
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

  it("passes engine-provided sources through unchanged (display-only)", () => {
    setWaitingFor({
      type: "DeclareBlockers",
      data: {
        player: 0,
        valid_blocker_ids: [100],
        valid_block_targets: { "100": [200] },
        blocker_constraints: { "100": { kind: "CantBlock", sources: [300] } },
      },
    });
    const r = renderHook(() => useBlockerConstraints());
    expect(r.result.current.byObject.get(100)?.sources).toEqual([300]);
  });
});

describe("AttackRequirementBadges source attribution (display-only)", () => {
  beforeEach(() => {
    useUiStore.setState({ selectedAttackers: [], blockerAssignments: new Map() });
  });
  afterEach(() => {
    setWaitingFor(undefined);
    useGameStore.setState({ gameState: null });
    document.querySelectorAll("[data-object-id]").forEach((el) => el.remove());
  });

  function renderBadge(
    objectId: number,
    sources: number[],
    objects: Record<string, { name: string }>,
  ) {
    anchorEl(objectId);
    setObjects(objects);
    setWaitingFor({
      type: "DeclareAttackers",
      data: {
        player: 0,
        valid_attacker_ids: [objectId],
        attacker_constraints: {
          [String(objectId)]: { kind: "MustAttack", players: [], sources },
        },
      },
    });
    render(<AttackRequirementBadges />);
  }

  it("appends (from <name>) for a resolvable remote source", async () => {
    renderBadge(100, [200], { "200": { name: "Grand Melee" } });
    await waitFor(() => {
      const badge = document.querySelector("span[title]");
      expect(badge?.getAttribute("title")).toContain("(from Grand Melee)");
    });
  });

  it("joins multiple resolvable sources", async () => {
    renderBadge(100, [200, 201], {
      "200": { name: "Grand Melee" },
      "201": { name: "Disinformation Campaign" },
    });
    await waitFor(() => {
      const badge = document.querySelector("span[title]");
      expect(badge?.getAttribute("title")).toContain(
        "(from Grand Melee, Disinformation Campaign)",
      );
    });
  });

  it("suppresses a self-source (intrinsic requirement shows a bare badge)", async () => {
    renderBadge(100, [100], { "100": { name: "Juggernaut" } });
    await waitFor(() => expect(document.querySelector("span[title]")).not.toBeNull());
    expect(document.querySelector("span[title]")?.getAttribute("title")).not.toContain("(from");
  });

  it("omits a departed source that no longer resolves (departed-source guard)", async () => {
    renderBadge(100, [200], {}); // object 200 absent from the objects map
    await waitFor(() => expect(document.querySelector("span[title]")).not.toBeNull());
    expect(document.querySelector("span[title]")?.getAttribute("title")).not.toContain("(from");
  });
});

describe("BlockerConstraintBadges source attribution (display-only)", () => {
  beforeEach(() => {
    useUiStore.setState({ selectedAttackers: [], blockerAssignments: new Map() });
  });
  afterEach(() => {
    setWaitingFor(undefined);
    useGameStore.setState({ gameState: null });
    document.querySelectorAll("[data-object-id]").forEach((el) => el.remove());
  });

  it("appends (from <name>) for a resolvable remote CantBlock source", async () => {
    anchorEl(100);
    setObjects({ "300": { name: "Meekstone" } });
    setWaitingFor({
      type: "DeclareBlockers",
      data: {
        player: 0,
        valid_blocker_ids: [],
        valid_block_targets: {},
        blocker_constraints: { "100": { kind: "CantBlock", sources: [300] } },
      },
    });
    render(<BlockerConstraintBadges />);
    await waitFor(() => {
      const badge = document.querySelector("span[title]");
      expect(badge?.getAttribute("title")).toContain("(from Meekstone)");
    });
  });
});
