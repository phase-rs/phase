import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type {
  DraftProgressFields,
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
