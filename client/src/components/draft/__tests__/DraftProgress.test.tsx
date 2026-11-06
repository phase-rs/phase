import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type {
  DraftProgressFields,
  FifoPlayerView,
  FifoPublicView,
  SeatPublicView,
  SpectatorDraftView,
} from "../../../adapter/draft-adapter";

vi.mock("../../../stores/draftStore", () => ({
  useDraftStore: (selector: (state: Record<string, unknown>) => unknown) =>
    selector({ view: null }),
}));

import { DraftProgress } from "../DraftProgress";

afterEach(cleanup);

/** The pips `PackSegment` renders, per pack, in pack order. */
function pipCountsPerPack(container: HTMLElement): number[] {
  return [...container.querySelectorAll("div.gap-px")].map(
    (segment) => segment.children.length,
  );
}

/**
 * Total pips across every pack. They carry no accessible role — the bar is
 * purely visual — so the DOM is the only available instrument, and the pip
 * COUNT is precisely the user-visible claim under test.
 */
function pipCount(container: HTMLElement): number {
  return container.querySelectorAll("div.h-2").length;
}

/**
 * CR 903.13b. `pick_number` counts pick STEPS, not cards, so the bar's
 * denominator is the engine's published step count. A 14-card Commander pack
 * drains in SEVEN steps, and 14 is a denominator the session can never reach.
 *
 * REVERT-PROBE: point `DraftProgress` back at `cards_per_pack` and the first
 * case renders 14 pips and `4/14` against assertions naming 7 and `4/7`.
 */
describe("DraftProgress — CR 903.13b pick steps", () => {
  /** One pack, so the rendered pip count IS the step count. */
  function progressView(
    overrides: Partial<DraftProgressFields> = {},
  ): DraftProgressFields {
    return {
      current_pack_number: 0,
      pick_number: 3,
      cards_per_pack: 14,
      pack_sizes: [14],
      pack_set_codes: ["TST"],
      pack_pick_steps: [7],
      pick_steps_per_pack: 7,
      pack_count: 1,
      pass_direction: "Left",
      ...overrides,
    };
  }

  it("renders one pip per pick STEP, not one per card", () => {
    const { container } = render(<DraftProgress view={progressView()} />);

    // CR 903.13b: "drafts two cards" — 14 cards is 7 steps.
    expect(pipCount(container)).toBe(7);
    expect(screen.getByText("/7")).toBeInTheDocument();
    // The reach-guard for the negative: the bar really did render, and it
    // rendered the seat's position within the STEP count.
    expect(screen.getByText("4")).toBeInTheDocument();
  });

  /**
   * Hostile sibling — the four CR 905.1a kinds. Their `cards_per_pick` is 1, so
   * the published count EQUALS `cards_per_pack` and the bar is byte-identical
   * to its pre-fix output. This is what proves the fix is the published count
   * rather than a hard-coded halving: a `cards_per_pack / 2` implementation
   * passes the case above and reds here.
   */
  it("still renders one pip per card when a step takes one card", () => {
    const { container } = render(
      <DraftProgress
        view={progressView({
          cards_per_pack: 14,
          pack_pick_steps: [14],
          pick_steps_per_pack: 14,
        })}
      />,
    );

    expect(pipCount(container)).toBe(14);
    expect(screen.getByText("/14")).toBeInTheDocument();
  });

  /**
   * Second hostile — the OTHER view shape. `DraftSpectatorDashboard` passes a
   * `SpectatorDraftView` through the same `DraftProgressFields` prop contract,
   * so the field has to exist on both views or this is unsatisfiable. The typed
   * literal is the compile-time half of the assertion; the render is the
   * runtime half.
   */
  it("renders a spectator view identically", () => {
    const spectator: SpectatorDraftView = {
      status: "Drafting",
      kind: "CommanderDraft",
      current_pack_number: 0,
      pick_number: 3,
      pass_direction: "Left",
      seats: [],
      cards_per_pack: 14,
      pack_sizes: [14],
      pack_set_codes: ["TST"],
      pack_pick_steps: [7],
      pick_steps_per_pack: 7,
      pack_count: 1,
      min_deck_size: 60,
      addable_cards: [],
      standings: [],
      current_round: 0,
      tournament_format: "Swiss",
      pod_policy: "Competitive",
      pairings: [],
      match_config: { match_type: "Bo1" },
    };

    const { container } = render(<DraftProgress view={spectator} />);

    expect(pipCount(container)).toBe(7);
    expect(screen.getByText("/7")).toBeInTheDocument();
  });
});

describe("DraftProgress", () => {
  it("labels_each_pack_and_renders_text_free_pick_bars", () => {
    const { container } = render(
      <DraftProgress
        view={{
          current_pack_number: 1,
          pick_number: 2,
          cards_per_pack: 4,
          pick_steps_per_pack: 4,
          pack_count: 3,
          pass_direction: "Left",
        }}
      />,
    );

    expect(screen.getByText("P1")).toBeInTheDocument();
    expect(screen.getByText("P2")).toBeInTheDocument();
    expect(screen.getByText("P3")).toBeInTheDocument();
    expect(container.querySelector("[data-draft-progress]")).toHaveClass(
      "border-hairline",
      "bg-white/[0.035]",
      "py-1.5",
      "shadow-[inset_0_-1px_0_rgba(0,0,0,0.28)]",
    );
    for (const label of ["P1", "P2", "P3"]) {
      expect(screen.getByText(label)).toHaveClass(
        "font-display",
        "text-xs",
        "font-semibold",
        "tracking-[-0.02em]",
        "text-fg",
      );
    }
    const pickCells = container.querySelectorAll<HTMLElement>("[data-pick-number]");
    expect(pickCells).toHaveLength(12);
    expect([...pickCells].every((cell) => cell.textContent === "")).toBe(true);
    for (const cell of pickCells) {
      expect(cell).toHaveClass("relative", "h-2");
      expect(cell).not.toHaveClass("flex", "h-6");
    }
    expect(container.querySelector("[data-draft-progress] > div"))
      .toHaveClass("flex-col", "sm:flex-row");
    const packOne = container.querySelectorAll<HTMLElement>('[data-pick-number][data-pack-number="1"]');
    const packTwo = container.querySelectorAll<HTMLElement>('[data-pick-number][data-pack-number="2"]');
    const packThree = container.querySelectorAll<HTMLElement>('[data-pick-number][data-pack-number="3"]');
    for (const cell of packOne) expect(cell).toHaveClass("bg-amber-400/50");
    expect(packTwo[0]).toHaveClass("bg-amber-400/50");
    expect(packTwo[1]).toHaveClass("bg-amber-400/90");
    expect(packTwo[2]).toHaveClass("bg-white/8");
    expect(packTwo[3]).toHaveClass("bg-white/8");
    for (const cell of packThree) expect(cell).toHaveClass("bg-white/4");
    expect(screen.getByText((_content, element) => element?.textContent === "3/4")).toBeInTheDocument();
  });
});

/**
 * Multi-set drafts open a different set each round, and those boosters differ
 * in size. The bar reads the engine's per-pack step counts rather than
 * describing every pack with the current one's.
 */
describe("DraftProgress — per-pack booster shape", () => {
  function progressView(
    overrides: Partial<DraftProgressFields> = {},
  ): DraftProgressFields {
    return {
      current_pack_number: 0,
      pick_number: 0,
      cards_per_pack: 15,
      pack_sizes: [15, 15, 15],
      pack_set_codes: ["ISD", "ISD", "ISD"],
      pack_pick_steps: [15, 15, 15],
      pick_steps_per_pack: 15,
      pack_count: 3,
      pass_direction: "Left",
      ...overrides,
    };
  }

  it("draws each booster at the step count the engine reported for it", () => {
    const { container } = render(
      <DraftProgress
        view={progressView({
          pack_sizes: [15, 14, 20],
          pack_set_codes: ["ISD", "BLB", "CMR"],
          pack_pick_steps: [15, 14, 20],
        })}
      />,
    );

    expect(pipCountsPerPack(container)).toEqual([15, 14, 20]);
  });

  it("counts picks against the booster in play, not the first one", () => {
    render(
      <DraftProgress
        view={progressView({
          current_pack_number: 1,
          pick_number: 3,
          cards_per_pack: 14,
          pack_sizes: [15, 14, 15],
          pack_set_codes: ["ISD", "BLB", "ISD"],
          pack_pick_steps: [15, 14, 15],
        })}
      />,
    );

    expect(screen.getByText("4")).toBeInTheDocument();
    expect(screen.getByText("/14")).toBeInTheDocument();
  });

  /**
   * The composition of both axes, and the discriminating case for this merge:
   * a Commander draft whose boosters differ in size takes TWO cards per step,
   * so neither `pack_sizes` (the card counts) nor the scalar
   * `pick_steps_per_pack` (the current pack's) renders this bar correctly.
   * Only the per-pack step array does.
   *
   * REVERT-PROBE: read `pack_sizes` and the pips become [20, 14, 16]; read the
   * scalar `pick_steps_per_pack` and they become [7, 7, 7].
   */
  it("reads per-pack step counts when packs differ in size AND a step takes two cards", () => {
    const { container } = render(
      <DraftProgress
        view={progressView({
          current_pack_number: 1,
          pick_number: 2,
          cards_per_pack: 14,
          pack_sizes: [20, 14, 16],
          pack_set_codes: ["CMR", "CLB", "LCC"],
          pack_pick_steps: [10, 7, 8],
          pick_steps_per_pack: 7,
        })}
      />,
    );

    expect(pipCountsPerPack(container)).toEqual([10, 7, 8]);
    expect(screen.getByText("3")).toBeInTheDocument();
    expect(screen.getByText("/7")).toBeInTheDocument();
  });

  it("names each booster's set when the draft mixes sets", () => {
    render(
      <DraftProgress
        view={progressView({
          current_pack_number: 1,
          pack_set_codes: ["ISD", "DKA", "AVR"],
        })}
      />,
    );

    expect(screen.getByText("DKA")).toBeInTheDocument();
    expect(screen.getByText("AVR")).toBeInTheDocument();
  });

  it("stays unlabelled when every booster comes from the same set", () => {
    render(<DraftProgress view={progressView()} />);

    expect(screen.queryByText("ISD")).not.toBeInTheDocument();
  });

  it("labels only the current Chaos booster from the redacted source view", () => {
    render(
      <DraftProgress
        view={progressView({
          current_pack_number: 1,
          pack_set_codes: [],
          source: {
            type: "Set",
            data: {
              layout: {
                Chaos: {
                  candidate_codes: ["ISD", "DKA", "AVR"],
                  current_pack_code: "DKA",
                  completed_own_pack_codes: null,
                  actual_set_codes: null,
                },
              },
            },
          },
        })}
      />,
    );

    expect(screen.getByText("DKA")).toBeInTheDocument();
    expect(screen.queryByText("ISD")).not.toBeInTheDocument();
    expect(screen.queryByText("AVR")).not.toBeInTheDocument();
  });
});

/**
 * CR 905.2a: during a draft there is no active player or system of priority,
 * so on a FIFO draft each seat moves at its own pace and the engine publishes
 * each seat's own completed steps this round. `pick_number` stays 0 for the
 * whole round there, so a bar that reads it shows no progress at all.
 */
describe("DraftProgress — FIFO progress", () => {
  /** A pip is filled when `PackSegment` gives it one of its two filled classes. */
  function filledPipCount(pips: Iterable<Element>): number {
    return [...pips].filter(
      (pip) => pip.classList.contains("bg-amber-400/50") || pip.classList.contains("bg-amber-400/90"),
    ).length;
  }

  /** One booster's pips, by its 1-based number. */
  function packPips(container: HTMLElement, packNumber: number): NodeListOf<Element> {
    return container.querySelectorAll(`[data-pack-number="${packNumber}"][data-pick-number]`);
  }

  /** One seat's pip row on a spectator view. */
  function seatPips(container: HTMLElement, seatIndex: number): NodeListOf<Element> {
    return container.querySelectorAll(`[data-seat-progress="${seatIndex}"] [data-pick-number]`);
  }

  function fifoState(): FifoPublicView {
    return { session_id: "local-fifo-session", revision: 7, paused: null, review_clock: null };
  }

  /** Seat 0's FIFO record in the second round, after round one's 15 steps. */
  function fifoPlayerData({
    roundCompletedSteps,
    holdsHead,
  }: {
    roundCompletedSteps: number;
    holdsHead: boolean;
  }): FifoPlayerView {
    return {
      state: fifoState(),
      progress: { completed_steps: 15 + roundCompletedSteps, round_completed_steps: roundCompletedSteps },
      // The head is the front of the seat's queue, so a seat holding one has
      // at least one queued pack and a seat holding none has none.
      queue_count: holdsHead ? 2 : 0,
      head: holdsHead ? { pack_id: 4, opening_seat: 2, round: 1, original_size: 15 } : null,
      prompt_id: holdsHead ? { session_id: "local-fifo-session", seat: 0, generation: 7 } : null,
      requirements: { picks: holdsHead ? 1 : 0, burns: 0 },
      effects: null,
      clock: null,
    };
  }

  /** First-round seats, each publishing its own completed steps. */
  function fifoSeats(roundCompletedSteps: readonly number[]): SeatPublicView[] {
    return roundCompletedSteps.map((steps, seat_index): SeatPublicView => ({
      seat_index,
      display_name: `Seat ${seat_index + 1}`,
      is_bot: seat_index > 0,
      connected: true,
      has_submitted_deck: false,
      pick_status: "Pending",
      active_pack_count: 1,
      drafted_card_count: steps,
      face_up_draft_cards: [],
      fifo_progress: { completed_steps: steps, round_completed_steps: steps },
    }));
  }

  /**
   * A FIFO view's progress fields over three 15-step boosters, with
   * `pick_number` as the engine publishes it all round unless a case says
   * otherwise. The player view is in the second round; the spectator view, in
   * the first.
   */
  function fifoProgressFields(
    shape:
      | { readonly view: "player"; readonly fifo: FifoPlayerView }
      | { readonly view: "spectator"; readonly fifo: FifoPublicView; readonly seats: readonly SeatPublicView[] },
    pickNumber = 0,
  ): DraftProgressFields {
    const fields = {
      pick_number: pickNumber,
      cards_per_pack: 15,
      pack_sizes: [15, 15, 15],
      pack_set_codes: ["TST", "TST", "TST"],
      pack_pick_steps: [15, 15, 15],
      pick_steps_per_pack: 15,
      pack_count: 3,
      flow: "FifoAsync" as const,
    };
    switch (shape.view) {
      case "player":
        // CR 905.1b: the second draft round passes to the right.
        return { ...fields, current_pack_number: 1, pass_direction: "Right", fifo: shape.fifo };
      case "spectator":
        return { ...fields, current_pack_number: 0, pass_direction: "Left", fifo: shape.fifo, seats: shape.seats };
    }
  }

  it("fills_the_current_round_to_the_seats_own_steps", () => {
    // `completed_steps` is 20 (15 + 5): a bar that read it would fill all 15
    // pips and show 21, and one that read `pick_number` would fill none and
    // show 1.
    const { container } = render(
      <DraftProgress
        view={fifoProgressFields({ view: "player", fifo: fifoPlayerData({ roundCompletedSteps: 5, holdsHead: true }) })}
      />,
    );

    // Reach guard: the bar rendered, and the finished first round is full.
    expect(filledPipCount(packPips(container, 1))).toBe(15);
    expect(filledPipCount(packPips(container, 2))).toBe(5);
    expect(container.querySelector("[data-draft-progress-counter]")?.textContent).toBe("6/15");
  });

  it("omits_the_counter_while_the_fifo_seat_holds_no_head", () => {
    // The state an LLM seat holding the round open leaves this seat in: it has
    // drafted from every pack that reached it and holds none.
    const { container } = render(
      <DraftProgress
        view={fifoProgressFields({ view: "player", fifo: fifoPlayerData({ roundCompletedSteps: 15, holdsHead: false }) })}
      />,
    );

    // Reach guard: the seat's own steps fill the current round, so its arm was taken.
    expect(filledPipCount(packPips(container, 2))).toBe(15);
    expect(container.querySelector("[data-draft-progress-counter]")).toBeNull();
  });

  it("fills_one_row_per_seat_on_a_fifo_spectator_view", () => {
    const { container } = render(
      <DraftProgress view={fifoProgressFields({ view: "spectator", fifo: fifoState(), seats: fifoSeats([3, 7, 0]) })} />,
    );

    expect(container.querySelectorAll("[data-seat-progress]")).toHaveLength(3);
    expect([0, 1, 2].map((seatIndex) => filledPipCount(seatPips(container, seatIndex)))).toEqual([3, 7, 0]);
    expect(container.querySelector("[data-draft-progress-counter]")).toBeNull();
  });

  it.each([
    {
      leg: "a player view whose FIFO record has no progress",
      currentPack: 2,
      view: (damaged: boolean) => {
        const fifo = fifoPlayerData({ roundCompletedSteps: 5, holdsHead: true });
        if (damaged) Reflect.deleteProperty(fifo, "progress");
        return fifoProgressFields({ view: "player", fifo }, 4);
      },
      // The seat holds a head, so a reading without the wire check shows a counter.
      reachGuard: (container: HTMLElement) =>
        expect(container.querySelector("[data-draft-progress-counter]")).not.toBeNull(),
    },
    {
      leg: "a spectator view with one seat missing its progress",
      currentPack: 1,
      view: (damaged: boolean) => {
        const seats = fifoSeats([3, 7, 0]);
        if (damaged) delete seats[1].fifo_progress;
        return fifoProgressFields({ view: "spectator", fifo: fifoState(), seats }, 4);
      },
      reachGuard: (container: HTMLElement) =>
        expect(container.querySelectorAll("[data-seat-progress]")).toHaveLength(3),
    },
    {
      leg: "a spectator view whose FIFO state has no session id",
      currentPack: 1,
      view: (damaged: boolean) => {
        const fifo = fifoState();
        if (damaged) Reflect.deleteProperty(fifo, "session_id");
        return fifoProgressFields({ view: "spectator", fifo, seats: fifoSeats([3, 7, 0]) }, 4);
      },
      reachGuard: (container: HTMLElement) =>
        expect(container.querySelectorAll("[data-seat-progress]")).toHaveLength(3),
    },
  ])("shows_no_position_for_a_fifo_view_it_cannot_read ($leg)", ({ currentPack, view, reachGuard }) => {
    // Reach guard: the intact view renders its arm, so the deleted field, and
    // not another defect of the fixture, is what the view is refused for.
    const intact = render(<DraftProgress view={view(false)} />);
    reachGuard(intact.container);
    intact.unmount();

    // `pick_number` is 4: a fallback to the legacy counter would fill 4 pips
    // of the current booster and show 5.
    const { container } = render(<DraftProgress view={view(true)} />);
    expect(packPips(container, currentPack)).toHaveLength(15);
    expect(filledPipCount(packPips(container, currentPack))).toBe(0);
    expect(container.querySelector("[data-draft-progress-counter]")).toBeNull();
    expect(container.querySelector("[data-seat-progress]")).toBeNull();
  });
});
