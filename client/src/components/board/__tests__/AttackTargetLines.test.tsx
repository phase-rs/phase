import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { GAME_Z_LAYER } from "../../../constants/ui.ts";
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
import { AttackTargetLines } from "../AttackTargetLines.tsx";

describe("AttackTargetLines", () => {
  let rafCallbacks: FrameRequestCallback[];

  beforeEach(() => {
    rafCallbacks = [];
    vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
      rafCallbacks.push(callback);
      return rafCallbacks.length;
    });
    useGameStore.setState({ gameMode: "local" });
    usePreferencesStore.setState({ multiplayerBoardLayout: "focused", vfxQuality: "minimal" });
    useUiStore.setState({ focusedOpponent: 1 });
  });

  afterEach(() => {
    cleanup();
    document.querySelectorAll("[data-object-id], [data-player-hud]").forEach((element) => {
      element.remove();
    });
    useGameStore.setState({ gameState: null, waitingFor: null });
    vi.restoreAllMocks();
  });

  it("portals attack arrows below the dialog host", () => {
    const attacker = buildGameObject({ id: 100, controller: 1 });
    const gameState = buildGameState({
      players: buildPlayers([0, 1, 2]),
      seat_order: [0, 1, 2],
      objects: buildObjectMap(attacker),
      combat: {
        attackers: [
          {
            object_id: 100,
            defending_player: 0,
            attack_target: { type: "Player", data: 0 },
          },
        ],
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
    });
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    const attackerAnchor = document.createElement("div");
    attackerAnchor.dataset.objectId = "100";
    const defenderHud = document.createElement("div");
    defenderHud.dataset.playerHud = "0";
    document.body.append(attackerAnchor, defenderHud);

    render(<AttackTargetLines effectiveMultiplayerBoardLayout="focused" />);
    act(() => {
      rafCallbacks.shift()?.(0);
    });

    const portal = document.getElementById("attack-arrow-head")?.closest("svg");
    expect(portal).toHaveClass(GAME_Z_LAYER.combatArrow);
    expect(portal).not.toHaveClass(GAME_Z_LAYER.dialogHost);
  });
});
