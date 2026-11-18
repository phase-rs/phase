import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { DiceRollOverlay } from "../DiceRollOverlay";
import { useMultiplayerStore } from "../../../stores/multiplayerStore";
import { useUiStore } from "../../../stores/uiStore";
import type { DiceRollPayload } from "../../../stores/uiStore";

/** The opponent's name from #11117's screenshots. */
const LONG_NAME = "Alphinaud Leveilleur";
/** Equal side gutters outside the wider safe-area inset. */
const SIDE_GUTTERS = "px-[calc(max(env(safe-area-inset-left),env(safe-area-inset-right))+var(--game-edge-right,0.75rem))]";

/** The starting-player contest, won by `winner`. The viewer is seat 0. */
function contestWonBy(winner: number): DiceRollPayload {
  return {
    kind: "die",
    context: "startingPlayer",
    sides: 20,
    winner,
    rounds: [[{ playerId: 0, value: 3 }, { playerId: 1, value: 18 }]],
    rolls: [{ playerId: 0, value: 3 }, { playerId: 1, value: 18 }],
  };
}

async function showRoll(payload: DiceRollPayload): Promise<void> {
  await act(async () => {
    useUiStore.setState({ diceRoll: payload });
  });
}

// happy-dom does no layout, so these tests pin the classes and structure that
// centre, bound and size each caption, not rendered positions. Without WebGL
// the overlay shows static faces, which land at once, so every caption shows.
describe("DiceRollOverlay with a long player name", () => {
  beforeEach(() => {
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
    useMultiplayerStore.setState({ playerNames: new Map([[1, LONG_NAME]]) });
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    useUiStore.getState().resetDiceRoll();
    useMultiplayerStore.setState({ playerNames: new Map() });
  });

  it("gives the overlay the side gutters and the container its captions fit", async () => {
    render(<DiceRollOverlay />);
    await showRoll(contestWonBy(1));

    expect(screen.getByRole("status")).toHaveClass("@container", SIDE_GUTTERS);
  });

  it("centres and balances the roll winner's caption, in a row that grows when it wraps", async () => {
    render(<DiceRollOverlay />);
    await showRoll(contestWonBy(1));

    const caption = await screen.findByText(`${LONG_NAME} won the roll`);
    expect(caption).toHaveClass("text-center", "text-balance");
    // 33 characters, and the 10 of "Leveilleur", at (0.68 + 0.05)em each.
    expect(caption.style.getPropertyValue("--title-width-em")).toBe("24.09");
    expect(caption.style.getPropertyValue("--title-run-width-em")).toBe("7.3");

    // The row reserves one line's height, so the dice hold still for a short
    // caption, and grows for a wrapped one instead of letting it spill out.
    const row = caption.parentElement!;
    expect(row).toHaveClass("min-h-10");
    expect(row).not.toHaveClass("h-10");
  });

  it("caps the caption's row at the overlay's width (100cqi) in a four-seat contest", async () => {
    // Four 132px dice and their gaps make the contest column 624px wide, wider
    // than a portrait phone; the caption's row is capped at the overlay's
    // width inside its gutters (`100cqi`) instead of taking the column's.
    const seats = [0, 1, 2, 3].map((playerId) => ({ playerId, value: playerId === 1 ? 18 : playerId + 2 }));
    render(<DiceRollOverlay />);
    await showRoll({ kind: "die", context: "startingPlayer", sides: 20, winner: 1, rounds: [seats], rolls: seats });

    const caption = await screen.findByText(`${LONG_NAME} won the roll`);
    // Reach guard: all four seats' dice are in the row.
    expect(screen.getByText("Opp 4")).toBeInTheDocument();
    expect(caption.parentElement).toHaveClass("max-w-[100cqi]");
  });

  it("centres and balances the long name over its die", async () => {
    render(<DiceRollOverlay />);
    await showRoll(contestWonBy(1));

    expect(screen.getByText(LONG_NAME)).toHaveClass("text-center", "text-balance");
  });

  it("keeps a short caption's size class and its one-line row", async () => {
    render(<DiceRollOverlay />);
    await showRoll(contestWonBy(0));

    const caption = await screen.findByText("You won the roll");
    expect(caption).toHaveClass("text-4xl", "font-extrabold", "uppercase", "tracking-wider");
    expect(caption.style.getPropertyValue("--title-width-em")).toBe("11.68");
    expect(caption.parentElement).toHaveClass("min-h-10");
  });

  it("centres and balances a coin flip's outcome, in a row that grows when it wraps", async () => {
    render(<DiceRollOverlay />);
    await showRoll({ kind: "coin", playerId: 1, won: false, context: "ability" });

    const outcome = await screen.findByText(`${LONG_NAME} lost the flip`);
    expect(outcome).toHaveClass("text-center", "text-balance");
    // 34 characters at (0.68 + 0.025)em.
    expect(outcome.style.getPropertyValue("--title-width-em")).toBe("23.97");

    const row = outcome.parentElement!;
    expect(row).toHaveClass("min-h-14");
    expect(row).not.toHaveClass("h-14");
  });
});
