import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import type { DraftPlayerView } from "../../../adapter/draft-adapter";

vi.mock("../../../stores/draftStore", () => ({
  useDraftStore: (selector: (state: Record<string, unknown>) => unknown) =>
    selector({
      view: null,
      selectedCard: null,
      selectCard: vi.fn(),
      confirmPick: vi.fn(),
      pickCardWithDraftEffect: vi.fn(),
      autoPickCard: vi.fn(),
    }),
}));

vi.mock("../../../hooks/useCardImage", () => ({
  useCardImage: () => ({ src: null, isLoading: false }),
}));

import { PackDisplay } from "../PackDisplay";

const view: DraftPlayerView = {
  status: "Drafting",
  kind: "Premier",
  current_pack_number: 0,
  pick_number: 0,
  pass_direction: "Left",
  // Premier (CR 905.1a): one card per pick step.
  required_pick_count: 1,
  current_pack: [
    {
      instance_id: "card-1",
      name: "Lightning Bolt",
      set_code: "tst",
      collector_number: "1",
      rarity: "common",
      colors: ["R"],
      cmc: 1,
      type_line: "Instant",
    },
  ],
  pool: [],
  draft_effects: [],
  pool_groups: {
    color_groups: [],
    type_groups: [],
    cmc_groups: [],
    rarity_groups: [],
    type_filter_options: [],
    color_filter_options: [],
    color_counts: { white: 0, blue: 0, black: 0, red: 0, green: 0 },
  },
  seats: [],
  cards_per_pack: 14,
  pick_steps_per_pack: 14,
  pack_count: 3,
  min_deck_size: 40,
  addable_cards: ["Plains", "Island", "Swamp", "Mountain", "Forest"],
  timer_remaining_ms: null,
  standings: [],
  current_round: 0,
  next_pairing_round: 1,
  tournament_format: "Swiss",
  pod_policy: "Competitive",
  pairings: [],
  match_config: { match_type: "Bo1" },
};

// A two-card pack, so the multi-select branch is reachable: the one-card base
// pack triggers the auto-select effect before any click can land.
const TWO_CARD_PACK = [
  view.current_pack![0],
  { ...view.current_pack![0], instance_id: "card-2", name: "Island" },
];

describe("PackDisplay pod state", () => {
  afterEach(() => {
    cleanup();
  });

  it("renders an explicit pod pack and dispatches pod pick actions", () => {
    const onSelectCard = vi.fn();
    const onConfirmPick = vi.fn();
    const { rerender } = render(
      <PackDisplay
        view={view}
        selectedCard={null}
        onSelectCard={onSelectCard}
        onConfirmPick={onConfirmPick}
        onCardHover={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Lightning Bolt" }));

    expect(onSelectCard).toHaveBeenCalledWith("card-1");

    rerender(
      <PackDisplay
        view={view}
        selectedCard="card-1"
        onSelectCard={onSelectCard}
        onConfirmPick={onConfirmPick}
        onCardHover={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Confirm Pick" }));

    expect(onConfirmPick).toHaveBeenCalledTimes(1);
    expect(onConfirmPick).toHaveBeenCalledWith(["card-1"]);
  });

  it("renders pod auto-pick and dispatches the pod auto-pick action", () => {
    const onAutoPick = vi.fn();

    render(
      <PackDisplay
        view={view}
        selectedCard={null}
        onSelectCard={vi.fn()}
        onConfirmPick={vi.fn()}
        showAutoPick
        onAutoPick={onAutoPick}
        onCardHover={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Auto-pick" }));

    expect(onAutoPick).toHaveBeenCalledTimes(1);
  });

  it("shows draft effects only when the engine provides drafted effect cards", () => {
    const effectView: DraftPlayerView = {
      ...view,
      current_pack: [
        view.current_pack![0],
        { ...view.current_pack![0], instance_id: "card-2", name: "Island" },
      ],
      draft_effects: [
        {
          instance_id: "cogwork-1",
          name: "Cogwork Librarian",
          set_code: "cns",
          collector_number: "58",
          rarity: "common",
          colors: [],
          cmc: 4,
          type_line: "Artifact Creature — Construct",
          draft_effect: "additional_pick",
        },
      ],
    };

    const { rerender } = render(
      <PackDisplay
        view={effectView}
        enableDraftEffects
        onCardHover={vi.fn()}
      />,
    );

    expect(screen.getByText("Draft effects:")).toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: "Cogwork Librarian" })).toBeInTheDocument();

    rerender(
      <PackDisplay
        view={{ ...effectView, draft_effects: [] }}
        enableDraftEffects
        onCardHover={vi.fn()}
      />,
    );

    expect(screen.queryByText("Draft effects:")).not.toBeInTheDocument();
  });

  it("dispatches a pod draft-effect pick through its injected callback", () => {
    const onPickWithDraftEffect = vi.fn();
    const effectView: DraftPlayerView = {
      ...view,
      current_pack: [
        view.current_pack![0],
        { ...view.current_pack![0], instance_id: "card-2", name: "Island" },
      ],
      draft_effects: [
        {
          instance_id: "cogwork-1",
          name: "Cogwork Librarian",
          set_code: "cns",
          collector_number: "58",
          rarity: "common",
          colors: [],
          cmc: 4,
          type_line: "Artifact Creature — Construct",
          draft_effect: "additional_pick",
        },
      ],
    };
    const { rerender } = render(
      <PackDisplay
        view={effectView}
        selectedCard={null}
        enableDraftEffects
        onPickWithDraftEffect={onPickWithDraftEffect}
        onCardHover={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("checkbox", { name: "Cogwork Librarian" }));
    rerender(
      <PackDisplay
        view={effectView}
        selectedCard="card-1"
        enableDraftEffects
        onPickWithDraftEffect={onPickWithDraftEffect}
        onCardHover={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Island" }));
    fireEvent.click(screen.getAllByRole("button", { name: "Confirm Pick" })[0]);

    expect(onPickWithDraftEffect).toHaveBeenCalledWith("cogwork-1", ["card-1", "card-2"]);
  });

  // CR 903.13b: a Commander pod drafts two cards per pick step.
  const commanderView: DraftPlayerView = {
    ...view,
    kind: "CommanderDraft",
    required_pick_count: 2,
    current_pack: TWO_CARD_PACK,
  };

  it("requires the engine's pick-step count before confirming a Commander step", () => {
    const onSelectCard = vi.fn();
    const onConfirmPick = vi.fn();
    const { rerender } = render(
      <PackDisplay
        view={commanderView}
        selectedCard={null}
        onSelectCard={onSelectCard}
        onConfirmPick={onConfirmPick}
        onCardHover={vi.fn()}
      />,
    );

    expect(screen.getByText("0 of 2 selected")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Lightning Bolt" }));
    expect(onSelectCard).toHaveBeenCalledWith("card-1");

    rerender(
      <PackDisplay
        view={commanderView}
        selectedCard="card-1"
        onSelectCard={onSelectCard}
        onConfirmPick={onConfirmPick}
        onCardHover={vi.fn()}
      />,
    );

    // Positive reach-guard for the negative below: the component rendered the
    // half-made step, so a crash or an early bail cannot satisfy the `.not`.
    expect(screen.getByText("1 of 2 selected")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Confirm Pick" })).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "Confirm Pick" }));
    expect(onConfirmPick).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Island" }));

    expect(screen.getByText("2 of 2 selected")).toBeInTheDocument();
    expect(
      screen.getAllByRole("button", { name: "Confirm Pick" })[0],
    ).not.toBeDisabled();

    fireEvent.click(screen.getAllByRole("button", { name: "Confirm Pick" })[0]);

    expect(onConfirmPick).toHaveBeenCalledTimes(1);
    expect(onConfirmPick).toHaveBeenCalledWith(["card-1", "card-2"]);
  });

  it("reads the pick-step count from the view, not from the kind", () => {
    // CR 903.13b's odd-leftover step: the kind says two, the engine says one.
    // No per-kind lookup can express this, which is the point of the row.
    const oddLeftoverView: DraftPlayerView = {
      ...view,
      kind: "CommanderDraft",
      required_pick_count: 1,
    };
    const onSelectCard = vi.fn();
    const onConfirmPick = vi.fn();
    const { rerender } = render(
      <PackDisplay
        view={oddLeftoverView}
        selectedCard={null}
        onSelectCard={onSelectCard}
        onConfirmPick={onConfirmPick}
        onCardHover={vi.fn()}
      />,
    );

    expect(onSelectCard).toHaveBeenCalledWith("card-1");

    rerender(
      <PackDisplay
        view={oddLeftoverView}
        selectedCard="card-1"
        onSelectCard={onSelectCard}
        onConfirmPick={onConfirmPick}
        onCardHover={vi.fn()}
      />,
    );

    expect(screen.queryByText(/selected/)).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Confirm Pick" })).not.toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "Confirm Pick" }));

    expect(onConfirmPick).toHaveBeenCalledWith(["card-1"]);
  });

  it("re-selects the already-selected card on a multi-card pack", () => {
    // Pins single-card CLICK semantics for the four CR 905.1a kinds — re-select,
    // never deselect — once the branch predicate is count-driven. The click half
    // is unchanged from base; the row as a whole is NOT green at base, because
    // the final assertion is `toHaveBeenCalledWith(["card-1"])` and base's
    // `handleConfirmPick` called `confirmPick()` with no arguments at all.
    const onSelectCard = vi.fn();
    const onConfirmPick = vi.fn();
    render(
      <PackDisplay
        view={{ ...view, current_pack: TWO_CARD_PACK }}
        selectedCard="card-1"
        onSelectCard={onSelectCard}
        onConfirmPick={onConfirmPick}
        onCardHover={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Lightning Bolt" }));

    expect(onSelectCard).toHaveBeenCalledWith("card-1");
    expect(onSelectCard).not.toHaveBeenCalledWith(null);

    fireEvent.click(screen.getByRole("button", { name: "Confirm Pick" }));

    expect(onConfirmPick).toHaveBeenCalledWith(["card-1"]);
  });

  // REVERT-PROBE: reds if `PackDisplay.tsx` handleSelectCard's
  // `if (requiredCount <= 1)` is reverted to `=== 1` — a published 0 would then
  // fall into the multi-select branch and deselect instead of re-selecting.
  it("a zero pick-step count still takes the single-card path", () => {
    const onSelectCard = vi.fn();
    render(
      <PackDisplay
        view={{ ...view, required_pick_count: 0, current_pack: TWO_CARD_PACK }}
        selectedCard="card-1"
        onSelectCard={onSelectCard}
        onConfirmPick={vi.fn()}
        onCardHover={vi.fn()}
      />,
    );

    expect(screen.queryByText(/selected/)).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Lightning Bolt" }));

    expect(onSelectCard).toHaveBeenCalledWith("card-1");
    expect(onSelectCard).not.toHaveBeenCalledWith(null);
  });

  // REVERT-PROBE: reds if the step-identity `useEffect`'s dependency array is
  // reverted to `[view?.current_pack]` — a guest gets a fresh array object on
  // every `draft_state_update` for the same step, which would wipe the partial.
  it("keeps a half-made selection across a same-step view refresh", () => {
    const { rerender } = render(
      <PackDisplay
        view={commanderView}
        selectedCard="card-1"
        onSelectCard={vi.fn()}
        onConfirmPick={vi.fn()}
        onCardHover={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Island" }));
    expect(screen.getByText("2 of 2 selected")).toBeInTheDocument();

    rerender(
      <PackDisplay
        view={{
          ...commanderView,
          // A new array of new objects for the SAME engine step.
          current_pack: TWO_CARD_PACK.map((card) => ({ ...card })),
        }}
        // The primary is deliberately retained: under `null` the derived
        // `selectedIds` is `[]` whatever the additionals hold, and the row
        // stops discriminating.
        selectedCard="card-1"
        onSelectCard={vi.fn()}
        onConfirmPick={vi.fn()}
        onCardHover={vi.fn()}
      />,
    );

    expect(screen.getByText("2 of 2 selected")).toBeInTheDocument();
  });

  // REVERT-PROBE: reds if the step-identity `useEffect` is deleted outright —
  // nothing would then clear the additionals when the engine's step advances.
  it("clears the partial selection when the engine's step advances", () => {
    const { rerender } = render(
      <PackDisplay
        view={commanderView}
        selectedCard="card-1"
        onSelectCard={vi.fn()}
        onConfirmPick={vi.fn()}
        onCardHover={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Island" }));
    expect(screen.getByText("2 of 2 selected")).toBeInTheDocument();

    rerender(
      <PackDisplay
        view={{ ...commanderView, pick_number: 1 }}
        // Held, not nulled: cleared reads "1 of 2", not cleared reads "2 of 2".
        // With `null` both readings collapse to "0 of 2" and the row is vacuous.
        selectedCard="card-1"
        onSelectCard={vi.fn()}
        onConfirmPick={vi.fn()}
        onCardHover={vi.fn()}
      />,
    );

    expect(screen.getByText("1 of 2 selected")).toBeInTheDocument();
  });

  // REVERT-PROBE: reds if `primaryCard` is reverted to the raw `selectedCard`.
  // The store nulls the primary ONLY when this client submits, so a step the
  // seat did not advance itself — a server auto-pick on timeout, the default
  // under `PodPolicy::Competitive` — delivers a new pack while the store still
  // names a card from the old one. No row above reaches this: they retain
  // `selectedCard="card-1"` across the rerender AND hold the pack fixed, so the
  // retained id stays a pack member there. Here it does not.
  it("starts a fresh Commander pick after an outside party advances the step", () => {
    // The store's primary is whatever `selectCard` last set — nothing else
    // writes it on this path — so the rerenders below are driven by the spy
    // rather than by the value the fix is supposed to produce. Read raw, the
    // spy is never called, "card-1" survives into the payload, and the engine
    // answers `CardNotInPack`; a rerender that just asserted "card-3" would
    // hand the sliding window the right answer and stop discriminating.
    let storePrimary: string | null = "card-1";
    const onSelectCard = vi.fn((instanceId: string | null) => {
      storePrimary = instanceId;
    });
    const onConfirmPick = vi.fn();
    const nextStepView: DraftPlayerView = {
      ...commanderView,
      pick_number: 1,
      current_pack: [
        { ...TWO_CARD_PACK[0], instance_id: "card-3", name: "Forest" },
        { ...TWO_CARD_PACK[0], instance_id: "card-4", name: "Mountain" },
      ],
    };
    const { rerender } = render(
      <PackDisplay
        view={commanderView}
        selectedCard="card-1"
        onSelectCard={onSelectCard}
        onConfirmPick={onConfirmPick}
        onCardHover={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Island" }));
    expect(screen.getByText("2 of 2 selected")).toBeInTheDocument();

    // The pick was made FOR this seat: a new step and a new pack arrive and
    // nothing nulled the primary, because no submit path ran on this client.
    rerender(
      <PackDisplay
        view={nextStepView}
        selectedCard="card-1"
        onSelectCard={onSelectCard}
        onConfirmPick={onConfirmPick}
        onCardHover={vi.fn()}
      />,
    );

    // "card-1" is not in this pack, so it counts for nothing. Read raw, this
    // says "1 of 2 selected" while no card is highlighted and no Confirm exists.
    expect(screen.getByText("0 of 2 selected")).toBeInTheDocument();

    // ...and the click must take the PRIMARY, not an additional slot: read raw,
    // the deselect arm is unsatisfiable here and every click falls through to
    // the sliding window, so the seat can never name a primary again.
    fireEvent.click(screen.getByRole("button", { name: "Forest" }));
    expect(onSelectCard).toHaveBeenCalledWith("card-3");

    rerender(
      <PackDisplay
        view={nextStepView}
        selectedCard={storePrimary}
        onSelectCard={onSelectCard}
        onConfirmPick={onConfirmPick}
        onCardHover={vi.fn()}
      />,
    );

    expect(screen.getByText("1 of 2 selected")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Mountain" }));
    expect(screen.getByText("2 of 2 selected")).toBeInTheDocument();

    fireEvent.click(screen.getAllByRole("button", { name: "Confirm Pick" })[0]);

    // Both ids come from the pack on screen; nothing from the old step survives.
    expect(onConfirmPick).toHaveBeenCalledWith(["card-3", "card-4"]);
  });

  // REVERT-PROBE: reds if handleSelectCard's final `else` is changed from the
  // sliding window to ignore-when-full — that rule dispatches
  // ["card-1", "card-2"] and changes shipped draft-effect behaviour.
  it("slides the additional slots when a further card is clicked", () => {
    const onPickWithDraftEffect = vi.fn();
    const effectView: DraftPlayerView = {
      ...view,
      current_pack: [
        ...TWO_CARD_PACK,
        { ...TWO_CARD_PACK[0], instance_id: "card-3", name: "Forest" },
      ],
      draft_effects: [
        {
          instance_id: "cogwork-1",
          name: "Cogwork Librarian",
          set_code: "cns",
          collector_number: "58",
          rarity: "common",
          colors: [],
          cmc: 4,
          type_line: "Artifact Creature — Construct",
          draft_effect: "additional_pick",
        },
      ],
    };

    const { rerender } = render(
      <PackDisplay
        view={effectView}
        selectedCard={null}
        enableDraftEffects
        onPickWithDraftEffect={onPickWithDraftEffect}
        onCardHover={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("checkbox", { name: "Cogwork Librarian" }));
    rerender(
      <PackDisplay
        view={effectView}
        selectedCard="card-1"
        enableDraftEffects
        onPickWithDraftEffect={onPickWithDraftEffect}
        onCardHover={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Island" }));
    fireEvent.click(screen.getByRole("button", { name: "Forest" }));

    expect(
      screen.getAllByRole("button", { name: "Confirm Pick" })[0],
    ).not.toBeDisabled();
    fireEvent.click(screen.getAllByRole("button", { name: "Confirm Pick" })[0]);

    expect(onPickWithDraftEffect).toHaveBeenCalledWith("cogwork-1", ["card-1", "card-3"]);
  });
});
