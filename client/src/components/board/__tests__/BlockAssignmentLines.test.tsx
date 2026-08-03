import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import {
  buildGameObject,
  buildObjectMap,
} from "../../../test/factories/gameObjectFactory.ts";
import {
  buildGameState,
  buildPlayers,
} from "../../../test/factories/gameStateFactory.ts";
import { BlockAssignmentLines } from "../BlockAssignmentLines.tsx";

describe("BlockAssignmentLines", () => {
  let rafCallbacks: FrameRequestCallback[];

  beforeEach(() => {
    rafCallbacks = [];
    vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
      rafCallbacks.push(callback);
      return rafCallbacks.length;
    });
    useGameStore.setState({ gameMode: "local" });
    usePreferencesStore.setState({ multiplayerBoardLayout: "focused", vfxQuality: "minimal" });
    useUiStore.setState({
      blockerAssignments: new Map(),
      combatMode: null,
      focusedOpponent: 1,
    });
  });

  afterEach(() => {
    cleanup();
    document.querySelectorAll("[data-object-id], [data-player-hud]").forEach((element) => {
      element.remove();
    });
    vi.restoreAllMocks();
  });

  it("renders the engine-derived multi-block pairs for an offscreen controller once each", () => {
    const blocker = buildGameObject({ id: 20, controller: 2 });
    const firstAttacker = buildGameObject({ id: 100, controller: 0 });
    const secondAttacker = buildGameObject({ id: 101, controller: 0 });
    const gameState = buildGameState({
      players: buildPlayers([0, 1, 2, 3]),
      seat_order: [0, 1, 2, 3],
      objects: buildObjectMap(blocker, firstAttacker, secondAttacker),
      combat: {
        attackers: [],
        blocker_assignments: {},
        blocker_to_attacker: {},
        blockers_declared_by: [],
        pending_blocker_declaration_events: [],
        damage_assignments: {},
        first_strike_done: false,
        damage_step_index: null,
        pending_damage: [],
        regular_damage_done: false,
      },
      derived: { blocker_assignment_pairs: [[20, 100], [20, 101]] },
    });
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
    useUiStore.setState({ blockerAssignments: new Map([[20, new Set([100])]]) });

    for (const id of [20, 100, 101]) {
      const anchor = document.createElement("div");
      anchor.dataset.objectId = String(id);
      document.body.append(anchor);
    }

    render(<BlockAssignmentLines effectiveMultiplayerBoardLayout="focused" />);
    act(() => {
      rafCallbacks.shift()?.(0);
    });

    expect(document.querySelectorAll('path[marker-end="url(#block-arrow-head)"]')).toHaveLength(4);
  });

  it("keeps the raw-combat HUD indicator branch for an offscreen blocker", () => {
    const blocker = buildGameObject({ id: 20, controller: 2 });
    const attacker = buildGameObject({ id: 100, controller: 0 });
    const gameState = buildGameState({
      players: buildPlayers([0, 1, 2, 3]),
      seat_order: [0, 1, 2, 3],
      objects: buildObjectMap(blocker, attacker),
      combat: {
        attackers: [],
        blocker_assignments: { 100: [20] },
        blocker_to_attacker: {},
        blockers_declared_by: [],
        pending_blocker_declaration_events: [],
        damage_assignments: {},
        first_strike_done: false,
        damage_step_index: null,
        pending_damage: [],
        regular_damage_done: false,
      },
    });
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    const hud = document.createElement("div");
    hud.dataset.playerHud = "2";
    const attackerAnchor = document.createElement("div");
    attackerAnchor.dataset.objectId = "100";
    document.body.append(hud, attackerAnchor);

    render(<BlockAssignmentLines effectiveMultiplayerBoardLayout="focused" />);
    act(() => {
      rafCallbacks.shift()?.(0);
    });

    expect(document.querySelectorAll('path[marker-end="url(#block-arrow-head)"]')).toHaveLength(2);
  });
});
