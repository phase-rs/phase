import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { DiceRollOverlay } from "../DiceRollOverlay";
import { useMultiplayerStore } from "../../../stores/multiplayerStore";
import { useUiStore } from "../../../stores/uiStore";
import type { DiceRollPayload } from "../../../stores/uiStore";
import { stubBannerTitleLayout, type TitleWidths } from "../../../test/helpers/bannerTitleLayout";

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
// centre and bound each caption, and set the widths a browser would measure
// (see `stubBannerTitleLayout`) to check the caption is the one fitted to its
// row. Without WebGL the overlay shows static faces, which land at once, so
// every caption shows.
describe("DiceRollOverlay with a long player name", () => {
  let widths: TitleWidths;
  let layout: ReturnType<typeof stubBannerTitleLayout>;

  beforeEach(() => {
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
    useMultiplayerStore.setState({ playerNames: new Map([[1, LONG_NAME]]) });
    // A phone's 388px row; the caption's widths at its size class.
    widths = { line: 388, oneLine: 300, longestRun: 160 };
    layout = stubBannerTitleLayout(widths);
  });

  afterEach(() => {
    cleanup();
    layout.restore();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    useUiStore.getState().resetDiceRoll();
    useMultiplayerStore.setState({ playerNames: new Map() });
  });

  it("gives the overlay the side gutters its captions' rows sit inside", async () => {
    render(<DiceRollOverlay />);
    await showRoll(contestWonBy(1));

    expect(screen.getByRole("status")).toHaveClass(SIDE_GUTTERS);
  });

  it("centres and balances the roll winner's caption, at the floor size where it has to wrap", async () => {
    // Too wide for one line above the 24px floor of the 36px caption.
    widths.oneLine = 1200;
    render(<DiceRollOverlay />);
    await showRoll(contestWonBy(1));

    const caption = await screen.findByText(`${LONG_NAME} won the roll`);
    expect(caption).toHaveClass("text-center", "text-balance");
    expect(caption.style.fontSize).toBe("24px");

    // The row is the caption's line, as wide as the overlay inside its gutters.
    // It reserves one line's height, so the dice hold still for a short
    // caption, and grows for a wrapped one instead of letting it spill out.
    const row = caption.parentElement!;
    expect(row).toHaveClass("w-full", "min-h-10");
    expect(row).not.toHaveClass("h-10");
  });

  it("keeps the caption's row at the overlay's width when four seats' dice run wider than a phone", async () => {
    // Four 132px dice and their gaps are 624px, wider than a portrait phone.
    // The contest's column and the caption's row stay at the overlay's width
    // inside its gutters (`w-full`), so the caption wraps there.
    const seats = [0, 1, 2, 3].map((playerId) => ({ playerId, value: playerId === 1 ? 18 : playerId + 2 }));
    render(<DiceRollOverlay />);
    await showRoll({ kind: "die", context: "startingPlayer", sides: 20, winner: 1, rounds: [seats], rolls: seats });

    const caption = await screen.findByText(`${LONG_NAME} won the roll`);
    // Reach guard: all four seats' dice are in the row.
    expect(screen.getByText("Opp 4")).toBeInTheDocument();
    const row = caption.parentElement!;
    expect(row).toHaveClass("w-full");
    expect(row.parentElement).toHaveClass("w-full");
  });

  it("centres and balances the long name over its die", async () => {
    render(<DiceRollOverlay />);
    await showRoll(contestWonBy(1));

    expect(screen.getByText(LONG_NAME)).toHaveClass("text-center", "text-balance");
  });

  it("renders a short caption as before: its size class, in its one-line row", async () => {
    render(<DiceRollOverlay />);
    await showRoll(contestWonBy(0));

    const caption = await screen.findByText("You won the roll");
    expect(caption).toHaveClass("text-4xl", "font-extrabold", "uppercase", "tracking-wider");
    // It fits its row, so it keeps the size class: no inline size.
    expect(caption.style.fontSize).toBe("");
    expect(caption.parentElement).toHaveClass("min-h-10");
  });

  it("fits a coin flip's outcome to its row, in a row that grows when it wraps", async () => {
    // 450px at the 24px size class fits the 386px it may fill at 24 × 386 / 450,
    // above the 18px floor. happy-dom has no Tailwind stylesheet, so the pill's
    // `px-5 border` measure as nothing here; the hook's own test covers them.
    widths.oneLine = 450;
    render(<DiceRollOverlay />);
    await showRoll({ kind: "coin", playerId: 1, won: false, context: "ability" });

    const outcome = await screen.findByText(`${LONG_NAME} lost the flip`);
    expect(outcome).toHaveClass("text-center", "text-balance");
    expect(parseFloat(outcome.style.fontSize)).toBeCloseTo((24 * 386) / 450);

    const row = outcome.parentElement!;
    expect(row).toHaveClass("w-full", "min-h-14");
    expect(row).not.toHaveClass("h-14");
  });
});
