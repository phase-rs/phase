import { act } from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState, TargetRef, WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import {
  buildCopyTargetSlot,
  buildGameState,
  buildTargetSelectionProgress,
  buildTargetSelectionSlot,
  copyRetargetWaitingForFactory,
  retargetChoiceWaitingForFactory,
  returnAsAuraTargetWaitingForFactory,
  targetSelectionWaitingForFactory,
} from "../../../test/factories/gameStateFactory.ts";
import { PlayerHud } from "../PlayerHud.tsx";

describe("PlayerHud", () => {
  beforeEach(() => {
    // `useCanActForWaitingState` short-circuits on EITHER `gameMode === "spectate"`
    // OR `isSpectator`, and the two live in different module-singleton stores that
    // persist across tests in this file. Both are reset here, not only in
    // `afterEach`, so one spectator row cannot make every later seated row inert.
    useMultiplayerStore.setState({
      activePlayerId: 0,
      isSpectator: false,
      playerAvatars: new Map([
        [0, { kind: "external" as const, url: "/player-avatar.jpg" }],
      ]),
    });
    useUiStore.setState({ fullControl: false, manualManaOverride: false });
    useGameStore.setState({
      gameState: buildGameState(),
      gameMode: null,
      waitingFor: null,
      stateHistory: [],
    });
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  it("renders local poison and speed outside the unified edge life pill", () => {
    const gameState = buildGameState();
    gameState.players[0].poison_counters = 8;
    gameState.players[0].speed = 3;

    act(() => {
      useGameStore.setState({ gameState });
    });

    render(<PlayerHud />);

    const plate = document.querySelector('[data-hud-plate]');
    const statuses = document.querySelector('[data-player-hud-edge-statuses]');
    expect(statuses).toContainElement(screen.getByLabelText("8 poison counters"));
    expect(statuses).toContainElement(screen.getByLabelText("Speed 3"));
    expect(statuses).toHaveClass(
      "right-0",
      "top-0",
      "translate-x-1/2",
      "-translate-y-1/2",
    );
    expect(statuses).not.toHaveClass("left-1/2", "bottom-full");
    expect(plate).not.toContainElement(screen.getByLabelText("8 poison counters"));
    expect(plate).not.toContainElement(screen.getByLabelText("Speed 3"));
  });

  it("hides local zero poison counters", () => {
    render(<PlayerHud />);

    expect(screen.queryByText(/Poison counters:/)).toBeNull();
  });

  it("renders temporary turn badges outside the unified edge life pill", () => {
    act(() => {
      useGameStore.setState({
        gameState: buildGameState({
          derived: {
            turn_order: [
              { player: 0, slot_index: 1, turns_from_now: 1, turn_number: 2 },
              { player: 0, slot_index: 2, turns_from_now: 2, turn_number: 3 },
            ],
          },
        }),
      });
    });

    render(<PlayerHud />);

    const nextUp = screen.getByTitle("This player's turn is next.");
    expect(document.querySelector('[data-player-hud-edge-statuses]')).toContainElement(nextUp);
    expect(document.querySelector('[data-hud-plate]')).not.toContainElement(nextUp);
  });

  // ── The player-target affordance: one authority + one actor gate ──────────
  //
  // `isValidTarget` is now
  // `useCanActForWaitingState() && getWaitingForPlayerChoiceIds(waitingFor).includes(playerId)`.
  // `HudPlate` renders a `<button>` when it has an `onClick` and a `<div>`
  // otherwise (HudPlate.tsx), so `[data-hud-plate]`'s tagName reads
  // `isValidTarget` directly.
  describe("player-target affordance", () => {
    const plateTag = () => document.querySelector("[data-hud-plate]")?.tagName;

    function mount(waitingFor: WaitingFor, gameState: GameState = buildGameState()) {
      const dispatch = vi.fn().mockResolvedValue([]);
      act(() => {
        useGameStore.setState({ dispatch, gameState, waitingFor });
      });
      render(<PlayerHud />);
      return { dispatch };
    }

    const legal = (players: number[]): TargetRef[] => players.map((p) => ({ Player: p }));

    function targetSelection(players: number[], forPlayer = 0) {
      return targetSelectionWaitingForFactory
        .withData({
          selection: buildTargetSelectionProgress({ current_legal_targets: legal(players) }),
          target_slots: [buildTargetSelectionSlot({ legal_targets: legal(players) })],
        })
        .forPlayer(forPlayer)
        .build();
    }

    // V6 — one row per distinct `case` body in the authority. The
    // `TargetSelection` / `TriggerTargetSelection` body is one shared arm and is
    // covered by the seated reach guard in the spectator block below.

    // CR 707.10c: the copy's controller retargets the slot the engine is asking
    // about, so the offer must follow `current_slot`.
    it("offers the seat for CopyRetarget's current slot", () => {
      const { dispatch } = mount(
        copyRetargetWaitingForFactory
          .withData({
            current_slot: 1,
            target_slots: [
              buildCopyTargetSlot({ legal_alternatives: legal([1]) }),
              buildCopyTargetSlot({ legal_alternatives: legal([0]) }),
            ],
          })
          .forPlayer(0)
          .build(),
      );

      expect(plateTag()).toBe("BUTTON");
      fireEvent.click(document.querySelector("[data-hud-plate]")!);
      expect(dispatch).toHaveBeenCalledWith({
        type: "ChooseTarget",
        data: { target: { Player: 0 } },
      });
    });

    // CR 115.7: a single-target retarget (Bolt Bend, Misdirection) is answered by a
    // HUD click.
    it("offers the seat for RetargetChoice(Single)", () => {
      const { dispatch } = mount(
        retargetChoiceWaitingForFactory
          .withData({ scope: { type: "Single" }, legal_new_targets: legal([0]) })
          .forPlayer(0)
          .build(),
      );

      expect(plateTag()).toBe("BUTTON");
      fireEvent.click(document.querySelector("[data-hud-plate]")!);
      expect(dispatch).toHaveBeenCalledWith({
        type: "ChooseTarget",
        data: { target: { Player: 0 } },
      });
    });

    // CR 303.4: an Aura enters attached to an object OR a player, so a Curse can
    // name this seat as its host.
    it("offers the seat for ReturnAsAuraTarget", () => {
      const { dispatch } = mount(
        returnAsAuraTargetWaitingForFactory
          .withData({ legal_targets: legal([0]) })
          .forPlayer(0)
          .build(),
      );

      expect(plateTag()).toBe("BUTTON");
      fireEvent.click(document.querySelector("[data-hud-plate]")!);
      expect(dispatch).toHaveBeenCalledWith({
        type: "ChooseTarget",
        data: { target: { Player: 0 } },
      });
    });

    // The negative sibling of the shared TargetSelection arm: the engine offers a
    // DIFFERENT seat, so this HUD stays inert.
    it("does not offer the seat when the engine names a different player", () => {
      const { dispatch } = mount(targetSelection([1]));

      expect(plateTag()).toBe("DIV");
      fireEvent.click(document.querySelector("[data-hud-plate]")!);
      expect(dispatch).not.toHaveBeenCalled();
    });

    // V7 — the fix. `dispatch.ts` silently refuses a spectator's action, so
    // offering the affordance at all is a false live-looking control.
    it("gives a spectating client no affordance, and the seated client the same fixture as a reach guard", () => {
      const seated = mount(targetSelection([0]));
      expect(plateTag()).toBe("BUTTON");
      fireEvent.click(document.querySelector("[data-hud-plate]")!);
      expect(seated.dispatch).toHaveBeenCalledWith({
        type: "ChooseTarget",
        data: { target: { Player: 0 } },
      });
      cleanup();

      const dispatch = vi.fn().mockResolvedValue([]);
      act(() => {
        useMultiplayerStore.setState({ isSpectator: true });
        useGameStore.setState({
          dispatch,
          gameMode: "spectate",
          gameState: buildGameState(),
          waitingFor: targetSelection([0]),
        });
      });
      render(<PlayerHud />);

      expect(plateTag()).toBe("DIV");
      fireEvent.click(document.querySelector("[data-hud-plate]")!);
      expect(dispatch).not.toHaveBeenCalled();
    });

    // The multiplayer-spectator shape the old inline gate also missed: a
    // spectator whose `gameMode` is still null but whose seat is spectating.
    it("gives an isSpectator client no affordance even when gameMode is null", () => {
      act(() => {
        useMultiplayerStore.setState({ isSpectator: true });
      });
      const { dispatch } = mount(targetSelection([0]));

      expect(plateTag()).toBe("DIV");
      expect(dispatch).not.toHaveBeenCalled();
    });

    // V12 — CR 723.1 turn control (a player controls another player for that
    // player's whole turn), and CR 723.3: only control of the player changes —
    // a controlled player is still the active player. So
    // `useCanActForWaitingState` resolves the REAL
    // seat (`usePlayerId`), the membership test resolves the RENDERED seat
    // (`usePerspectivePlayerId`), and under `turn_decision_controller` these are
    // two different players. Both rows assert the rendered seat so a future
    // change to `usePerspectivePlayerId` cannot make them pass for the wrong
    // reason.
    describe("under a turn-control effect (CR 723.1 / CR 723.3)", () => {
      const turnControlState = () =>
        buildGameState({ turn_decision_controller: 0, active_player: 1 });

      it("offers the piloted seat when the engine names it", () => {
        // Real seat 0 pilots seat 1's turn, so this HUD renders seat 1. The
        // prompt is addressed to seat 0 (`data.player`) and offers seat 1.
        const { dispatch } = mount(targetSelection([1], 0), turnControlState());

        expect(document.querySelector("[data-player-hud]")?.getAttribute("data-player-hud"))
          .toBe("1");
        expect(plateTag()).toBe("BUTTON");
        fireEvent.click(document.querySelector("[data-hud-plate]")!);
        expect(dispatch).toHaveBeenCalledWith({
          type: "ChooseTarget",
          data: { target: { Player: 1 } },
        });
      });

      it("stays inert when the engine names the real seat this HUD does not render", () => {
        const { dispatch } = mount(targetSelection([0], 0), turnControlState());

        expect(document.querySelector("[data-player-hud]")?.getAttribute("data-player-hud"))
          .toBe("1");
        expect(plateTag()).toBe("DIV");
        fireEvent.click(document.querySelector("[data-hud-plate]")!);
        expect(dispatch).not.toHaveBeenCalled();
      });
    });
  });

  it("centers the rendered life pill without a desktop-only offset", () => {
    const { container } = render(<PlayerHud alignNameplateToAnchor />);
    const hud = container.querySelector<HTMLElement>("[data-local-player-hud]");

    expect(hud).toHaveAttribute("data-edge-pill-layout", "true");
    expect(hud).not.toHaveAttribute("data-nameplate-anchor-aligned");
    expect(hud?.style.transform).toBe("");
  });

  it("uses a portrait-filled life pill with independent glass controls at every resolution", () => {
    act(() => {
      useGameStore.setState({ gameMode: "ai", stateHistory: [buildGameState()] });
    });

    const { container } = render(<PlayerHud alignNameplateToAnchor />);

    expect(container.querySelector('[data-edge-pill-layout="true"]')).toHaveAttribute(
      "data-player-life-shape",
      "pill",
    );
    const plate = container.querySelector('[data-hud-plate]');
    const cornerControls = container.querySelector('[data-player-hud-corner-controls]');
    const undoControl = container.querySelector('[data-player-hud-undo-control]');
    expect(cornerControls).toContainElement(
      screen.getByRole("button", { name: "Full Control Off" }),
    );
    expect(cornerControls).toContainElement(screen.getByRole("button", { name: "Manual" }));
    expect(plate).not.toContainElement(screen.getByRole("button", { name: "Manual" }));
    expect(plate).not.toContainElement(screen.getByRole("button", { name: "Full Control Off" }));
    expect(plate).not.toContainElement(screen.getByRole("button", { name: "Undo" }));
    expect(undoControl).toContainElement(screen.getByRole("button", { name: "Undo" }));
    expect(plate).toHaveTextContent("20");
    expect(plate?.querySelector("[data-hud-plate-label]")).not.toBeInTheDocument();
    expect(plate?.querySelector("svg")).toBeNull();
    expect(plate?.querySelector('[data-hud-plate-art] img')).toHaveAttribute(
      "src",
      "/player-avatar.jpg",
    );
    expect(container.querySelectorAll('[data-major-phase-stop-rail]')).toHaveLength(1);
    expect(container.querySelector('[data-player-hud-phase-stop-rail]'))
      .toContainElement(container.querySelector('[data-major-phase-stop-rail="all"]'));
    expect(container.querySelector('[data-phase-stop-rail-center-gap]')).toBeInTheDocument();
    expect(container.querySelector('[data-hud-plate-corner]')).not.toBeInTheDocument();
    expect(container.querySelector('[data-hud-plate-trailing]')).not.toBeInTheDocument();
    const manualMana = screen.getByRole("button", { name: "Manual" });
    expect(manualMana).toHaveAttribute("data-icon-only", "true");
    expect(manualMana).toHaveClass("tabletop-liquid-glass-control");
    expect(manualMana.querySelector("[data-control-label]")).toBeNull();
    const fullControl = screen.getByRole("button", { name: "Full Control Off" });
    expect(fullControl).toHaveAttribute("data-icon-only", "true");
    expect(fullControl).toHaveClass("tabletop-liquid-glass-control");
    const undo = screen.getByRole("button", { name: "Undo" });
    expect(undo).toHaveClass("tabletop-liquid-glass-control");
    expect(undo.querySelector("svg")).toHaveClass("h-5", "w-5");
    expect(undoControl).toHaveClass("fixed");

  });

  it("uses the same edge pill for the desktop HUD", () => {
    act(() => {
      useGameStore.setState({ gameMode: "ai", stateHistory: [buildGameState()] });
    });

    const { container } = render(<PlayerHud />);

    expect(container.querySelector('[data-edge-pill-layout="true"]')).toBeInTheDocument();
    expect(container.querySelector('[data-edge-pill-layout="true"]')).toHaveAttribute(
      "data-player-life-shape",
      "pill",
    );
    expect(container.querySelector('[data-player-hud-corner-controls]')).toContainElement(
      screen.getByRole("button", { name: "Full Control Off" }),
    );
    expect(container.querySelector('[data-player-hud-corner-controls]')).toContainElement(
      screen.getByRole("button", { name: "Manual" }),
    );
    expect(container.querySelector('[data-player-hud-undo-control]')).toContainElement(
      screen.getByRole("button", { name: "Undo" }),
    );
    expect(container.querySelectorAll('[data-major-phase-stop-rail]')).toHaveLength(1);
    expect(container.querySelector('[data-hud-plate-corner]')).not.toBeInTheDocument();
    expect(container.querySelector('[data-hud-plate-trailing]')).not.toBeInTheDocument();

  });
});
