import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { BetweenGamesSideboardModal } from "../BetweenGamesSideboardModal";

afterEach(cleanup);

function entry(name: string, count: number) {
  return { card: { name }, count };
}

const basePool = {
  registered_main: [
    entry("Lightning Bolt", 4),
    entry("Counterspell", 3),
    entry("Mountain", 10),
  ],
  registered_sideboard: [entry("Pyroblast", 2), entry("Chalice", 1)],
  current_main: [
    entry("Lightning Bolt", 4),
    entry("Counterspell", 3),
    entry("Mountain", 10),
  ],
  current_sideboard: [entry("Pyroblast", 2), entry("Chalice", 1)],
};

const score = { p0_wins: 1, p1_wins: 0, draws: 0 };

/** Engine-supplied bounds for a 17-card registered main / 15-card cap. */
const bounds = { minMainDeckSize: 17, maxSideboardSize: 15 };

describe("BetweenGamesSideboardModal", () => {
  it("seeds drafts from pool.current_*", () => {
    render(
      <BetweenGamesSideboardModal
        pool={basePool}
        gameNumber={2}
        score={score}
        {...bounds}
        onSubmit={vi.fn()}
      />,
    );
    expect(screen.getByText("Lightning Bolt")).toBeInTheDocument();
    expect(screen.getByText("Pyroblast")).toBeInTheDocument();
    // Main total: 4 + 3 + 10 = 17, shown against the engine's minimum.
    expect(screen.getByText(/Main \(17, min 17\)/)).toBeInTheDocument();
    // Sideboard total: 2 + 1 = 3, against the format's 15-card cap.
    expect(screen.getByText(/Sideboard \(3\/15\)/)).toBeInTheDocument();
  });

  it("preserves total pool size when moving cards (partition invariant)", () => {
    render(
      <BetweenGamesSideboardModal
        pool={basePool}
        gameNumber={2}
        score={score}
        {...bounds}
        onSubmit={vi.fn()}
      />,
    );
    // Move one Lightning Bolt from main → sideboard.
    fireEvent.click(
      screen.getByRole("button", { name: /move one lightning bolt to sideboard/i }),
    );
    // Main total drops to 16, sideboard rises to 4 — combined still 20.
    expect(screen.getByText(/Main \(16, min 17\)/)).toBeInTheDocument();
    expect(screen.getByText(/Sideboard \(4\/15\)/)).toBeInTheDocument();
  });

  it("disables submit when the main deck falls below the minimum deck size", () => {
    render(
      <BetweenGamesSideboardModal
        pool={basePool}
        gameNumber={2}
        score={score}
        {...bounds}
        onSubmit={vi.fn()}
      />,
    );
    fireEvent.click(
      screen.getByRole("button", { name: /move one lightning bolt to sideboard/i }),
    );
    expect(screen.getByRole("button", { name: /submit deck for next game/i })).toBeDisabled();
  });

  // CR 100.5: there is no maximum deck size. A player who registered 60/15 may
  // side a card in without siding one out; the old gate required an exact
  // match and blocked this entirely.
  it("allows submitting a main deck larger than the minimum", () => {
    const onSubmit = vi.fn();
    render(
      <BetweenGamesSideboardModal
        pool={basePool}
        gameNumber={2}
        score={score}
        {...bounds}
        onSubmit={onSubmit}
      />,
    );
    fireEvent.click(
      screen.getByRole("button", { name: /move one pyroblast to main/i }),
    );
    expect(screen.getByText(/Main \(18, min 17\)/)).toBeInTheDocument();

    const submit = screen.getByRole("button", { name: /submit deck for next game/i });
    expect(submit).not.toBeDisabled();
    fireEvent.click(submit);

    const [main, side] = onSubmit.mock.calls[0];
    expect(main).toEqual(
      expect.arrayContaining([{ name: "Pyroblast", count: 1 }]),
    );
    expect(side).toEqual(expect.arrayContaining([{ name: "Pyroblast", count: 1 }]));
  });

  // CR 100.4a: the sideboard cap is enforced independently of the minimum.
  it("disables submit when the sideboard exceeds the format cap", () => {
    render(
      <BetweenGamesSideboardModal
        pool={basePool}
        gameNumber={2}
        score={score}
        minMainDeckSize={0}
        maxSideboardSize={3}
        onSubmit={vi.fn()}
      />,
    );
    fireEvent.click(
      screen.getByRole("button", { name: /move one lightning bolt to sideboard/i }),
    );
    expect(screen.getByRole("button", { name: /submit deck for next game/i })).toBeDisabled();
    expect(screen.getByRole("status")).toHaveTextContent(/maximum 3/i);
  });

  it("omits the sideboard cap when the format imposes none", () => {
    render(
      <BetweenGamesSideboardModal
        pool={basePool}
        gameNumber={2}
        score={score}
        minMainDeckSize={17}
        maxSideboardSize={null}
        onSubmit={vi.fn()}
      />,
    );
    expect(screen.getByText(/Sideboard \(3\)/)).toBeInTheDocument();
  });

  it("enables submit at the minimum and dispatches SubmitSideboard", () => {
    const onSubmit = vi.fn();
    render(
      <BetweenGamesSideboardModal
        pool={basePool}
        gameNumber={2}
        score={score}
        {...bounds}
        onSubmit={onSubmit}
      />,
    );
    const submit = screen.getByRole("button", { name: /submit deck for next game/i });
    expect(submit).not.toBeDisabled();
    fireEvent.click(submit);
    expect(onSubmit).toHaveBeenCalledTimes(1);
    const [main, side] = onSubmit.mock.calls[0];
    expect(main).toEqual(
      expect.arrayContaining([
        { name: "Lightning Bolt", count: 4 },
        { name: "Counterspell", count: 3 },
        { name: "Mountain", count: 10 },
      ]),
    );
    expect(side).toEqual(
      expect.arrayContaining([
        { name: "Pyroblast", count: 2 },
        { name: "Chalice", count: 1 },
      ]),
    );
  });

  it("reset restores the registered partition (not current_*)", () => {
    const divergedPool = {
      ...basePool,
      // current_* reflects last game's post-sideboarding state.
      current_main: [
        entry("Lightning Bolt", 2), // 2 cards moved to sideboard last match
        entry("Counterspell", 3),
        entry("Mountain", 10),
        entry("Pyroblast", 2),
      ],
      current_sideboard: [entry("Lightning Bolt", 2), entry("Chalice", 1)],
    };
    render(
      <BetweenGamesSideboardModal
        pool={divergedPool}
        gameNumber={3}
        score={{ p0_wins: 1, p1_wins: 1, draws: 0 }}
        {...bounds}
        onSubmit={vi.fn()}
      />,
    );
    // Seeded from current_*: Pyroblast is currently in main.
    expect(screen.getByText(/Main \(17, min 17\)/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /reset to registered deck/i }));

    // After reset: main = registered_main (4 Bolt + 3 CS + 10 Mountain = 17),
    // sideboard = registered_sideboard (2 Pyroblast + 1 Chalice = 3).
    // Verify by checking Pyroblast is now only on the sideboard side.
    const sideRegion = screen.getByText(/Sideboard \(3\/15\)/).closest("div");
    expect(sideRegion).not.toBeNull();
    if (sideRegion) {
      expect(within(sideRegion.parentElement!).getByText("Pyroblast")).toBeInTheDocument();
    }
  });

  it("does not render a remove button (partition-only UI)", () => {
    render(
      <BetweenGamesSideboardModal
        pool={basePool}
        gameNumber={2}
        score={score}
        {...bounds}
        onSubmit={vi.fn()}
      />,
    );
    expect(
      screen.queryByRole("button", { name: /remove one lightning bolt/i }),
    ).not.toBeInTheDocument();
  });

  it("announces integrity status to screen readers", () => {
    render(
      <BetweenGamesSideboardModal
        pool={basePool}
        gameNumber={2}
        score={score}
        {...bounds}
        onSubmit={vi.fn()}
      />,
    );
    const status = screen.getByRole("status");
    expect(status).toHaveTextContent(/ready to submit/i);

    fireEvent.click(
      screen.getByRole("button", { name: /move one lightning bolt to sideboard/i }),
    );
    expect(screen.getByRole("status")).toHaveTextContent(/at least 17/i);
  });
});
