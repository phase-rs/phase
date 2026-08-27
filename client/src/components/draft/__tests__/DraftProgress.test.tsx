import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";

import type {
  DraftProgressFields,
  SpectatorDraftView,
} from "../../../adapter/draft-adapter";

vi.mock("../../../stores/draftStore", () => ({
  useDraftStore: (selector: (state: Record<string, unknown>) => unknown) =>
    selector({ view: null }),
}));

import { DraftProgress } from "../DraftProgress";

/**
 * VM row 8 — CR 903.13b. `pick_number` counts pick STEPS, not cards, so the
 * bar's denominator is the engine's published step count. A 14-card Commander
 * pack drains in SEVEN steps, and 14 was a denominator the session could never
 * reach.
 *
 * REVERT-PROBE: point `DraftProgress` back at `cards_per_pack` and the first
 * case renders 14 pips and `4/14` against assertions naming 7 and `4/7`.
 */
describe("DraftProgress — CR 903.13b pick steps", () => {
  afterEach(() => {
    cleanup();
  });

  /** One pack, so the rendered pip count IS the step count. */
  function progressView(
    overrides: Partial<DraftProgressFields> = {},
  ): DraftProgressFields {
    return {
      current_pack_number: 0,
      pick_number: 3,
      cards_per_pack: 14,
      pick_steps_per_pack: 7,
      pack_count: 1,
      pass_direction: "Left",
      ...overrides,
    };
  }

  /**
   * The pips `PackSegment` renders. They carry no accessible role — the bar is
   * purely visual — so the DOM is the only available instrument, and the pip
   * COUNT is precisely the user-visible claim under test.
   */
  function pipCount(container: HTMLElement): number {
    return container.querySelectorAll("div.h-2").length;
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
        view={progressView({ cards_per_pack: 14, pick_steps_per_pack: 14 })}
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
