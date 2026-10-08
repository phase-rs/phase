import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";

import { DraftIntro } from "../DraftIntro";

afterEach(cleanup);

describe("DraftIntro", () => {
  afterEach(cleanup);

  it("shows the draft's configured pack count and pack size", () => {
    render(
      <DraftIntro
        mode="quick"
        podSize={8}
        packCount={4}
        cardsPerPack={12}
        minDeckSize={35}
        onContinue={vi.fn()}
      />,
    );

    expect(screen.getByText("You'll open 4 packs; each pack contains 12 cards")).toBeInTheDocument();
    expect(
      screen.getByText("After all picks, build a deck of at least 35 cards and play a match"),
    ).toBeInTheDocument();
    expect(screen.getByText("The passing direction alternates each round")).toBeInTheDocument();
    expect(screen.getByText("Quick Draft")).toBeInTheDocument();
  });

  it("describes the Commander procedure in commander mode", () => {
    render(
      <DraftIntro
        mode="commander"
        podSize={4}
        packCount={4}
        cardsPerPack={18}
        packSizes={[18, 18, 18, 18]}
        minDeckSize={63}
        onContinue={vi.fn()}
      />,
    );

    // CR 903.13b requires two picks per pack, while CR 903.13f sets a 60-card
    // floor. REVERT-FAILING: 63 proves the copy reads the engine's stricter
    // published minimum instead of duplicating that floor here.
    expect(
      screen.getByText("Open 4 packs; each pack contains 18 cards — pick two cards, pass the rest"),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        "After drafting, build a Commander deck of at least 63 cards and play one multiplayer game",
      ),
    ).toBeInTheDocument();
    expect(screen.getByText("Commander Draft")).toBeInTheDocument();
    // Reach guard for the REUSE decision: step 1 resolves through the shared
    // `intro.pod.step1` key, with the passed `podSize` interpolated.
    expect(screen.getByText("You're drafting with 4 players in a pod")).toBeInTheDocument();
    expect(
      screen.getByText("The passing direction alternates each round"),
    ).toBeInTheDocument();
  });

  it("lists every pack size for a mixed-size Commander draft", () => {
    render(
      <DraftIntro
        mode="commander"
        podSize={6}
        packCount={3}
        cardsPerPack={20}
        packSizes={[20, 18, 20]}
        minDeckSize={60}
        onContinue={vi.fn()}
      />,
    );

    expect(
      screen.getByText(
        "Open 3 packs of mixed sizes, in this order: 20 cards, 18 cards, and 20 cards — pick two cards, pass the rest",
      ),
    ).toBeInTheDocument();
  });

  it("shows the pod's configured procedure", () => {
    render(
      <DraftIntro
        mode="pod"
        podSize={8}
        packCount={4}
        cardsPerPack={12}
        packSizes={[12, 12, 12, 12]}
        minDeckSize={45}
        onContinue={vi.fn()}
      />,
    );

    expect(screen.getByText("Pod Draft")).toBeInTheDocument();
    expect(
      screen.getByText("Open 4 packs; each pack contains 12 cards — pick one, pass the rest"),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        "After drafting, build a deck of at least 45 cards and play tournament matches",
      ),
    ).toBeInTheDocument();
    // Non-vacuous: the positive assertions above prove this render mounted, so
    // the absent Commander step is a real absence.
    expect(
      screen.queryByText(
        "Open 4 packs; each pack contains 12 cards — pick two cards, pass the rest",
      ),
    ).toBeNull();
  });

  it("lists every booster's size when a multi-set draft mixes them", () => {
    render(
      <DraftIntro
        mode="quick"
        podSize={8}
        packCount={3}
        cardsPerPack={15}
        packSizes={[15, 14, 15]}
        minDeckSize={40}
        onContinue={vi.fn()}
      />,
    );

    expect(
      screen.getByText(
        "You'll open 3 packs of mixed sizes, in this order: 15 cards, 14 cards, and 15 cards",
      ),
    ).toBeInTheDocument();
  });

  it("keeps the single-size line when every booster agrees", () => {
    render(
      <DraftIntro
        mode="quick"
        podSize={8}
        packCount={3}
        cardsPerPack={15}
        packSizes={[15, 15, 15]}
        minDeckSize={40}
        onContinue={vi.fn()}
      />,
    );

    expect(screen.getByText("You'll open 3 packs; each pack contains 15 cards")).toBeInTheDocument();
  });

  it("lists every pack size for a mixed-size pod draft", () => {
    render(
      <DraftIntro
        mode="pod"
        podSize={6}
        packCount={3}
        cardsPerPack={15}
        packSizes={[15, 14, 15]}
        minDeckSize={40}
        onContinue={vi.fn()}
      />,
    );

    expect(
      screen.getByText(
        "Open 3 packs of mixed sizes, in this order: 15 cards, 14 cards, and 15 cards — pick one, pass the rest",
      ),
    ).toBeInTheDocument();
  });

  it("uses singular quantities and direction copy for a one-pack cube", () => {
    render(
      <DraftIntro
        mode="quick"
        podSize={2}
        packCount={1}
        cardsPerPack={1}
        packSizes={[1]}
        minDeckSize={1}
        onContinue={vi.fn()}
      />,
    );

    expect(screen.getByText("You'll open 1 pack; each pack contains 1 card")).toBeInTheDocument();
    expect(
      screen.getByText(
        "This draft has one pack round, so the passing direction does not alternate",
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByText("After all picks, build a deck of at least 1 card and play a match"),
    ).toBeInTheDocument();
  });
});
