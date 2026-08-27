import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";

import { DraftIntro } from "../DraftIntro";

describe("DraftIntro", () => {
  afterEach(cleanup);

  it("shows the draft's configured pack count and pack size", () => {
    render(
      <DraftIntro
        mode="quick"
        packCount={4}
        cardsPerPack={12}
        onContinue={vi.fn()}
      />,
    );

    expect(screen.getByText("You'll open 4 packs of 12 cards each")).toBeInTheDocument();
    expect(screen.getByText("Quick Draft")).toBeInTheDocument();
  });

  it("describes the Commander procedure in commander mode", () => {
    render(<DraftIntro mode="commander" podSize={4} onContinue={vi.fn()} />);

    // REVERT-FAILING: BASE has no "commander" mode, so these two steps have no
    // renderer and no i18n key. CR 903.13b (two cards per pack) and
    // CR 903.13f (60-card minimum) are what this copy asserts.
    expect(screen.getByText("Pick two cards from each pack, then pass the rest")).toBeInTheDocument();
    expect(screen.getByText(/60-card Commander deck/)).toBeInTheDocument();
    expect(screen.getByText("Commander Draft")).toBeInTheDocument();
    // Reach guard for the REUSE decision: steps 1 and 3 resolve through the
    // shared `intro.pod.*` keys, with the passed `podSize` interpolated.
    expect(screen.getByText("You're drafting with 4 players in a pod")).toBeInTheDocument();
    expect(
      screen.getByText("Packs alternate direction each round — left, right, left"),
    ).toBeInTheDocument();
  });

  it("leaves the pod variant unchanged", () => {
    render(<DraftIntro mode="pod" podSize={8} onContinue={vi.fn()} />);

    expect(screen.getByText("Pod Draft")).toBeInTheDocument();
    expect(screen.getByText("Open 3 packs of 14 cards — pick one, pass the rest")).toBeInTheDocument();
    // Non-vacuous: the positive assertions above prove this render mounted, so
    // the absent Commander step is a real absence.
    expect(screen.queryByText("Pick two cards from each pack, then pass the rest")).toBeNull();
  });
});
