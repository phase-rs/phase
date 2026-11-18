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
 *
 * Happy DOM's ResizeObserver never fires either, so a controlled one stands in.
 * It records what each observer watches and, like a browser, notifies only the
 * observers watching the element a test reports as resized.
 */
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import {
  buildGameState,
  buildPlayers,
  buildPriorityWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";
import { ActionRailStatusRow } from "../ActionRailStatusRow.tsx";

const initialViewport = { width: window.innerWidth, height: window.innerHeight };
const initialFlexLayout = usePreferencesStore.getState().flexLayout;
const initialFullControl = useUiStore.getState().fullControl;

function setViewport(width: number, height: number) {
  Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: width });
  Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: height });
}

const resizeObservers = new Set<ControlledResizeObserver>();

class ControlledResizeObserver implements ResizeObserver {
  readonly observed = new Set<Element>();
  readonly callback: ResizeObserverCallback;

  constructor(callback: ResizeObserverCallback) {
    this.callback = callback;
    resizeObservers.add(this);
  }

  observe(target: Element) {
    this.observed.add(target);
  }

  unobserve(target: Element) {
    this.observed.delete(target);
  }

  disconnect() {
    this.observed.clear();
  }
}

/** Notifies every observer watching `target`, as a browser does when the
 *  element's size changes. An element nobody observes notifies no one. */
function reportResize(target: Element) {
  act(() => {
    for (const observer of resizeObservers) {
      if (observer.observed.has(target)) observer.callback([], observer);
    }
  });
}

/** A stand-in for the mounted PlayerHud, in PlayerArea's structure: a board
 *  column holding the creature row, the middle-row wrapper with the HUD anchor
 *  inside it, and the other-permanent row. The anchor holds the HUD's Flex
 *  Layout widget, the HUD root, and the Manual / Undo column hanging off the
 *  plate's right edge. */
function mountPlayerHud() {
  const boardColumn = document.createElement("div");
  const creatureRow = document.createElement("div");
  const middleRowWrapper = document.createElement("div");
  const anchor = document.createElement("div");
  anchor.setAttribute("data-player-hud-anchor", "");
  const widget = document.createElement("div");
  widget.setAttribute("data-flex-zone", "playerHud");
  const playerHud = document.createElement("div");
  const sideControls = document.createElement("div");
  sideControls.setAttribute("data-player-hud-side-controls", "");
  const otherRow = document.createElement("div");

  playerHud.appendChild(sideControls);
  widget.appendChild(playerHud);
  anchor.appendChild(widget);
  middleRowWrapper.appendChild(anchor);
  boardColumn.append(creatureRow, middleRowWrapper, otherRow);
  document.body.appendChild(boardColumn);

  playerHud.getBoundingClientRect = () => new DOMRect(330, 300, 150, 46);
  sideControls.getBoundingClientRect = () => new DOMRect(484, 296, 46, 54);
  return { widget, playerHud, sideControls, creatureRow, middleRowWrapper, otherRow };
}

type HudFixture = ReturnType<typeof mountPlayerHud>;

/** Renders the row inside a stand-in rail widget and gives the row its screen
 *  rect, docked by default. The resize event stands in for the first real
 *  layout. */
function renderRow(playerHud: HTMLElement, rowRect = new DOMRect(520, 318, 318, 26)) {
  render(
    <div data-flex-zone="actionRail">
      <ActionRailStatusRow playerHud={playerHud} />
    </div>,
  );
  const row = screen.getByTestId("action-rail-status-row");
  row.getBoundingClientRect = () => rowRect;
  act(() => {
    window.dispatchEvent(new Event("resize"));
  });
  return row;
}

interface ResizeCase {
  what: string;
  /** Moves rects as that resize would, and returns the element that resized. */
  resize: (fixture: HudFixture & { row: HTMLElement; rail: HTMLElement }) => Element;
  maxWidth: string;
}

/** One reported resize for each subscription on the HUD, the board rows, the
 *  rail and the chips present at mount, each from the docked 302px bound. Every
 *  outcome differs from 302px, so a subscription that went missing leaves the
 *  stale bound and fails its case. The row's own subscription and a chip that
 *  mounts later need a different start, so their tests follow the table. */
const RESIZE_CASES: ResizeCase[] = [
  {
    what: "the Manual / Undo column growing (Undo appears)",
    resize: ({ sideControls }) => {
      sideControls.getBoundingClientRect = () => new DOMRect(484, 296, 86, 54);
      return sideControls;
    },
    // 838 - 570 - 6.
    maxWidth: "262px",
  },
  {
    what: "the plate growing (a badge appears), which pushes the column right",
    resize: ({ playerHud, sideControls }) => {
      playerHud.getBoundingClientRect = () => new DOMRect(330, 300, 170, 46);
      sideControls.getBoundingClientRect = () => new DOMRect(504, 296, 46, 54);
      return playerHud;
    },
    // 838 - 550 - 6.
    maxWidth: "282px",
  },
  {
    what: "the other-permanent row growing, which lifts the HUD above the row",
    resize: ({ playerHud, sideControls, otherRow }) => {
      playerHud.getBoundingClientRect = () => new DOMRect(330, 200, 150, 46);
      sideControls.getBoundingClientRect = () => new DOMRect(484, 196, 46, 54);
      return otherRow;
    },
    maxWidth: "",
  },
  {
    what: "the creature row growing (the other-permanent row emptied), which drops the HUD below the row",
    resize: ({ playerHud, sideControls, creatureRow }) => {
      playerHud.getBoundingClientRect = () => new DOMRect(330, 352, 150, 46);
      sideControls.getBoundingClientRect = () => new DOMRect(484, 348, 46, 54);
      return creatureRow;
    },
    maxWidth: "",
  },
  {
    what: "the middle row growing, which drops the HUD below the row",
    resize: ({ playerHud, sideControls, middleRowWrapper }) => {
      playerHud.getBoundingClientRect = () => new DOMRect(330, 352, 150, 46);
      sideControls.getBoundingClientRect = () => new DOMRect(484, 348, 46, 54);
      return middleRowWrapper;
    },
    maxWidth: "",
  },
  {
    what: "the rail growing (more action buttons), which lifts the row above the HUD",
    resize: ({ row, rail }) => {
      row.getBoundingClientRect = () => new DOMRect(520, 240, 318, 26);
      return rail;
    },
    maxWidth: "",
  },
  {
    what: "a chip growing wider than the room right of the HUD",
    resize: () => {
      const statusChip = screen.getByRole("status");
      statusChip.getBoundingClientRect = () => new DOMRect(528, 320, 310, 22);
      return statusChip;
    },
    maxWidth: "",
  },
];

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
    resizeObservers.clear();
    vi.stubGlobal("ResizeObserver", ControlledResizeObserver);
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    document.body.replaceChildren();
    setViewport(initialViewport.width, initialViewport.height);
    useGameStore.getState().reset();
    useMultiplayerStore.setState({ activePlayerId: null, isSpectator: false, playerNames: new Map() });
    usePreferencesStore.setState({ flexLayout: initialFlexLayout });
    useUiStore.setState({ fullControl: initialFullControl });
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

  it("remeasures when a chip mounts, even when nothing it observes resizes", async () => {
    const { playerHud, sideControls } = mountPlayerHud();
    const row = renderRow(playerHud);
    expect(row.style.maxWidth).toBe("302px");

    // The plate has moved, but no resize or drag reports it (the controlled
    // ResizeObserver fires only when a test reports a resize). Turning Full
    // Control on mounts its chip in the row, and that alone has to bring the
    // bound up to date.
    playerHud.getBoundingClientRect = () => new DOMRect(400, 300, 150, 46);
    sideControls.getBoundingClientRect = () => new DOMRect(554, 296, 46, 54);
    await act(async () => {
      useUiStore.setState({ fullControl: true });
    });
    expect(screen.getByRole("button", { name: "Full Control On" })).toBeInTheDocument();
    await waitFor(() => expect(row.style.maxWidth).toBe("232px"));
  });

  // Each case is driven by one reported resize alone: no window resize, no
  // style mutation and no chip mounting.
  it.each(RESIZE_CASES)("keeps the bound current after $what", ({ resize, maxWidth }) => {
    const fixture = mountPlayerHud();
    const row = renderRow(fixture.playerHud);
    expect(row.style.maxWidth).toBe("302px");
    const rail = row.parentElement;
    if (!rail) throw new Error("the row renders inside the rail widget");

    reportResize(resize({ ...fixture, row, rail }));
    expect(row.style.maxWidth).toBe(maxWidth);
  });

  it("keeps the bound current when the row itself drops from two lines to one", () => {
    const fixture = mountPlayerHud();
    // A HUD level only with the row's wrapped upper line: 258-306 against a
    // two-line row at 290-344.
    fixture.playerHud.getBoundingClientRect = () => new DOMRect(330, 262, 150, 40);
    fixture.sideControls.getBoundingClientRect = () => new DOMRect(484, 258, 46, 48);
    const row = renderRow(fixture.playerHud, new DOMRect(520, 290, 318, 54));
    expect(row.style.maxWidth).toBe("302px");

    // Back to one line at 318-344: nothing level with the HUD any more.
    row.getBoundingClientRect = () => new DOMRect(520, 318, 318, 26);
    reportResize(row);
    expect(row.style.maxWidth).toBe("");
  });

  it("keeps the bound current when a chip that mounted later resizes", async () => {
    const { playerHud, sideControls } = mountPlayerHud();
    const row = renderRow(playerHud);
    expect(row.style.maxWidth).toBe("302px");

    // The plate moves, then Full Control's chip mounts: the mount's own
    // remeasure picks up the 232px of room the moved plate leaves.
    playerHud.getBoundingClientRect = () => new DOMRect(400, 300, 150, 46);
    sideControls.getBoundingClientRect = () => new DOMRect(554, 296, 46, 54);
    await act(async () => {
      useUiStore.setState({ fullControl: true });
    });
    await waitFor(() => expect(row.style.maxWidth).toBe("232px"));

    // Then the new chip grows wider than that room, reported as a resize alone.
    const fullControlChip = screen.getByRole("button", { name: "Full Control On" });
    fullControlChip.getBoundingClientRect = () => new DOMRect(598, 320, 240, 22);
    reportResize(fullControlChip);
    expect(row.style.maxWidth).toBe("");
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
