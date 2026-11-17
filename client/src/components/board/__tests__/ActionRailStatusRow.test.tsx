/**
 * Regression for #11010: on a landscape phone the action rail's status chips
 * grew left over Undo, Manual and the life total. On short landscape screens
 * the row now goes compact, wraps upward, and is bounded to the room right of
 * the player HUD cluster, wherever the rail and the plate have been dragged.
 *
 * Happy DOM does no layout, so the HUD cluster and the row get stubbed screen
 * rects. The numbers model an 844×390 phone: the plate ends at x=480, the
 * Manual / Undo column hanging off it ends at x=530, and the row's right edge
 * sits at x=838, level with both.
 */
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import {
  buildGameState,
  buildPlayers,
  buildPriorityWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";
import { ActionRailStatusRow } from "../ActionRailStatusRow.tsx";

const initialViewport = { width: window.innerWidth, height: window.innerHeight };
const initialFlexLayout = usePreferencesStore.getState().flexLayout;

function setViewport(width: number, height: number) {
  Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: width });
  Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: height });
}

/** A stand-in for the mounted PlayerHud: its Flex Layout widget, the HUD root,
 *  and the Manual / Undo column hanging off the plate's right edge. */
function mountPlayerHud() {
  const widget = document.createElement("div");
  widget.setAttribute("data-flex-zone", "playerHud");
  const playerHud = document.createElement("div");
  const sideControls = document.createElement("div");
  sideControls.setAttribute("data-player-hud-side-controls", "");
  playerHud.appendChild(sideControls);
  widget.appendChild(playerHud);
  document.body.appendChild(widget);

  playerHud.getBoundingClientRect = () => new DOMRect(330, 300, 150, 46);
  sideControls.getBoundingClientRect = () => new DOMRect(484, 296, 46, 54);
  return { widget, playerHud, sideControls };
}

/** Renders the row inside a stand-in rail widget and gives the row its docked
 *  screen rect. The resize event stands in for the first real layout. */
function renderRow(playerHud: HTMLElement) {
  render(
    <div data-flex-zone="actionRail">
      <ActionRailStatusRow playerHud={playerHud} />
    </div>,
  );
  const row = screen.getByTestId("action-rail-status-row");
  row.getBoundingClientRect = () => new DOMRect(520, 318, 318, 26);
  act(() => {
    window.dispatchEvent(new Event("resize"));
  });
  return row;
}

describe("ActionRailStatusRow", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
    const waitingForGiada = buildPriorityWaitingFor({ data: { player: 1 } });
    useGameStore.setState({
      gameMode: "online",
      gameState: buildGameState({
        players: buildPlayers([{ id: 0, hand: [11, 12, 13, 14, 15] }, 1]),
        priority_player: 1,
        waiting_for: waitingForGiada,
      }),
      waitingFor: waitingForGiada,
    });
    useMultiplayerStore.setState({
      activePlayerId: 0,
      isSpectator: false,
      playerNames: new Map([[1, "Giada"]]),
    });
    setViewport(844, 390);
  });

  afterEach(() => {
    cleanup();
    document.body.replaceChildren();
    setViewport(initialViewport.width, initialViewport.height);
    useGameStore.getState().reset();
    useMultiplayerStore.setState({ activePlayerId: null, isSpectator: false, playerNames: new Map() });
    usePreferencesStore.setState({ flexLayout: initialFlexLayout });
  });

  it("shows compact chips on a short landscape screen and wraps them upward", () => {
    const { playerHud } = mountPlayerHud();
    const row = renderRow(playerHud);

    expect(row).toHaveClass("flex-wrap-reverse", "justify-end");
    const phaseChip = screen.getByRole("button", { name: /Current phase: Main 1\./ });
    expect(phaseChip).toHaveTextContent("Main 1");
    expect(phaseChip).not.toHaveTextContent("Main Phase 1");
    expect(screen.getByRole("button", { name: "View full hand (5 cards)" })).toHaveTextContent(/^5$/);
    expect(screen.getByRole("status")).toHaveTextContent("Waiting for Giada");
  });

  it("bounds the row to the room right of the docked plate, Manual and Undo", () => {
    const { playerHud } = mountPlayerHud();
    const row = renderRow(playerHud);

    // 838 (row right) - 530 (Manual / Undo right) - 6 (chip gap).
    expect(row.style.maxWidth).toBe("302px");
  });

  it("follows the plate when it is dragged, and lets go once it is clear of the row", async () => {
    const { widget, playerHud, sideControls } = mountPlayerHud();
    const row = renderRow(playerHud);
    expect(row.style.maxWidth).toBe("302px");

    // Dragged 70px right: DraggableWidget writes the offset to its inline style.
    playerHud.getBoundingClientRect = () => new DOMRect(400, 300, 150, 46);
    sideControls.getBoundingClientRect = () => new DOMRect(554, 296, 46, 54);
    widget.setAttribute("style", "transform: translateX(70px)");
    await waitFor(() => expect(row.style.maxWidth).toBe("232px"));

    // Dragged up above the rail: nothing level with the row to keep clear of.
    playerHud.getBoundingClientRect = () => new DOMRect(400, 100, 150, 46);
    sideControls.getBoundingClientRect = () => new DOMRect(554, 96, 46, 54);
    widget.setAttribute("style", "transform: translate(70px, -200px)");
    await waitFor(() => expect(row.style.maxWidth).toBe(""));
  });

  it("keeps its natural width when the plate leaves less room than a chip needs", async () => {
    const { widget, playerHud, sideControls } = mountPlayerHud();
    const row = renderRow(playerHud);
    expect(row.style.maxWidth).toBe("302px");

    // A 90px chip, then the plate dragged up against the rail: 838 - 760 - 6
    // leaves 72px, so a bound could only stack chips that still overflow.
    screen.getByRole("status").getBoundingClientRect = () => new DOMRect(600, 320, 90, 22);
    playerHud.getBoundingClientRect = () => new DOMRect(560, 300, 150, 46);
    sideControls.getBoundingClientRect = () => new DOMRect(714, 296, 46, 54);
    widget.setAttribute("style", "transform: translateX(230px)");
    await waitFor(() => expect(row.style.maxWidth).toBe(""));
  });

  it("follows the rail when it is dragged", async () => {
    const { playerHud } = mountPlayerHud();
    const row = renderRow(playerHud);
    expect(row.style.maxWidth).toBe("302px");

    const rail = row.parentElement;
    if (!rail) throw new Error("the row renders inside the rail widget");
    row.getBoundingClientRect = () => new DOMRect(442, 318, 318, 26);
    rail.setAttribute("style", "transform: translateX(-78px)");
    // 760 (dragged row right) - 530 - 6.
    await waitFor(() => expect(row.style.maxWidth).toBe("224px"));
  });

  it("converts the bound into the row's own pixels when the rail is box-scaled", () => {
    usePreferencesStore.setState((state) => ({
      flexLayout: { ...state.flexLayout, scales: { ...state.flexLayout.scales, actionRail: 1.5 } },
    }));
    const { playerHud } = mountPlayerHud();
    const row = renderRow(playerHud);

    // 302 screen pixels at 1.5x scale, floored to whole pixels.
    expect(row.style.maxWidth).toBe("201px");
  });

  it("keeps the full row on desktop, even in a short window", () => {
    // Short enough for useIsCompactHeight, but `lg` wide: still desktop.
    setViewport(1280, 450);
    const { playerHud } = mountPlayerHud();
    const row = renderRow(playerHud);

    expect(row).not.toHaveClass("flex-wrap-reverse");
    expect(row.style.maxWidth).toBe("");
    expect(screen.getByRole("button", { name: /Current phase/ })).toHaveTextContent("Main Phase 1");
    expect(screen.getByRole("button", { name: "View full hand (5 cards)" })).toHaveTextContent("Hand 5");
  });
});
