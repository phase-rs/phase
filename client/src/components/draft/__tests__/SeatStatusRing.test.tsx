import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, within } from "@testing-library/react";

// A MUTABLE fixture, because the claims below differ only in what the engine
// published: the same two seats read as packs-in-hand in a pick-and-pass draft
// and as cards-drafted under a shared stack, and nothing but `shared_stack`
// distinguishes the two.
const store = vi.hoisted(() => ({
  state: {
    seatIndex: 0,
    view: {
      pass_direction: "Left",
      distribution: "PickAndPass" as unknown,
      seats: [] as Record<string, unknown>[],
    },
  },
}));

vi.mock("../../../stores/multiplayerDraftStore", () => ({
  useMultiplayerDraftStore: (selector: (state: typeof store.state) => unknown) =>
    selector(store.state),
}));

const SEATS: Record<string, unknown>[] = [
  {
    seat_index: 0,
    display_name: "Drafter",
    is_bot: false,
    connected: true,
    has_submitted_deck: false,
    pick_status: "Pending",
    active_pack_count: 1,
    drafted_card_count: 4,
    face_up_draft_cards: [],
  },
  {
    seat_index: 1,
    display_name: "Opponent",
    is_bot: false,
    connected: true,
    has_submitted_deck: false,
    pick_status: "Picked",
    active_pack_count: 0,
    drafted_card_count: 9,
    face_up_draft_cards: [
      {
        instance_id: "cogwork-1",
        name: "Cogwork Librarian",
        set_code: "CNS",
        collector_number: "58",
        rarity: "common",
        colors: [],
        cmc: 4,
        type_line: "Artifact Creature - Construct",
        draft_effect: "additional_pick",
      },
    ],
  },
];

import { SeatStatusRing, SeatStatusRingLayout } from "../SeatStatusRing";

describe("SeatStatusRing", () => {
  afterEach(cleanup);

  beforeEach(() => {
    store.state.view.distribution = "PickAndPass";
    store.state.view.seats = SEATS.map((seat) => ({ ...seat }));
  });

  it("tallies drafted cards, not packs, once the engine publishes a shared stack", () => {
    // Keyed on the PROCEDURE, not on the live `shared_stack`, which the engine
    // status-gates to `Drafting` — this ring also renders in the pod status
    // dialog, which opens long after the last card is taken.
    // A shared-stack seat NEVER holds a pack — the engine publishes 0 for every
    // seat by construction — so the pack tally sits at zero for the whole draft
    // while the number a Winston player actually tracks goes unshown. The two
    // fixture seats hold different numbers of packs AND different numbers of
    // cards, so neither reading can pass for the other here.
    store.state.view.distribution = { SharedStackPiles: { pile_count: 3 } };

    render(<SeatStatusRing />);

    expect(screen.getByText("4 cards drafted by Drafter")).toBeInTheDocument();
    expect(screen.getByText("9 cards drafted by Opponent")).toBeInTheDocument();
    expect(screen.queryByText("1 pack at Drafter")).toBeNull();
    for (const tally of document.querySelectorAll("[data-seat-tally]")) {
      expect(tally).toHaveAttribute("data-seat-tally", "drafted");
    }
    // Nothing is passed at a shared stack: every booster was shuffled together
    // before the draft began, so an arrow would describe a rule the format does
    // not have.
    expect(document.querySelector("[data-pass-arrow]")).toBeNull();
  });

  it("shows other drafters' face-up draft cards", () => {
    const { container } = render(<SeatStatusRing />);

    expect(screen.getByText("Face-up: Cogwork Librarian")).toBeInTheDocument();
    expect(screen.getByText("1")).toBeInTheDocument();
    expect(screen.getByText("0")).toBeInTheDocument();
    expect(screen.getByText("1 pack at Drafter")).toBeInTheDocument();
    expect(screen.getByText("0 packs at Opponent")).toBeInTheDocument();
    expect(container.querySelector("[data-seat-status-ring]")).toHaveClass(
      "grid-cols-[repeat(auto-fit,minmax(calc(15ch+3.5rem),1fr))]",
      "mb-2",
      "gap-1",
      "text-xs",
    );
    const units = container.querySelectorAll<HTMLElement>("[data-seat-pass-unit]");
    expect(units).toHaveLength(2);
    for (const unit of units) {
      expect(unit.firstElementChild).toHaveAttribute("data-seat-badge");
      expect(unit.lastElementChild).toHaveAttribute("data-pass-arrow");
      const badge = unit.querySelector<HTMLElement>("[data-seat-badge]")!;
      // `pr-14` is the width the tally needs beside the name, and it is the
      // ring's own `calc(15ch+3.5rem)` column floor: the two have to agree or
      // a long name runs under the count.
      expect(badge).toHaveClass("min-w-[15ch]", "min-h-[40px]", "py-0.5", "pr-14");
      expect(badge).not.toHaveClass("gap-0.5", "pr-9");
      expect(unit.querySelector("[data-pass-arrow]")).toHaveTextContent("→");
    }
    const tally = document.querySelector<HTMLElement>("[data-seat-tally]")!;
    // The count sits NEXT TO the artwork, not on top of it: a digit centred on
    // the icon needed an outline to stay readable, and a three-digit pool count
    // covered it outright.
    expect(tally).toHaveClass("flex", "items-center", "gap-1", "right-1");
    const digits = within(tally).getByText("1");
    expect(digits).toHaveClass("text-xs", "text-jade", "tabular-nums");
    expect(digits).not.toHaveClass(
      "absolute",
      "inset-0",
      "[-webkit-text-stroke:1px_rgb(2_6_23_/_0.95)]",
      "[paint-order:stroke_fill]",
    );
    expect(within(tally).getByRole("presentation", { hidden: true })).toHaveClass("h-5", "w-5");
  });

  it("places right-pass arrows before their equal-width seat badges", () => {
    const seats = [{
      seat_index: 0,
      display_name: "Drafter",
      is_bot: false,
      connected: true,
      has_submitted_deck: false,
      pick_status: "Pending" as const,
      active_pack_count: 1,
      drafted_card_count: 0,
      face_up_draft_cards: [],
    }];

    const { container } = render(
      <SeatStatusRingLayout
        seats={seats}
        passDirection="Right"
        localSeat={0}
        passDirectionLabel="Passing Right"
        tally="packs"
        passesPacks
      />,
    );

    const unit = container.querySelector<HTMLElement>("[data-seat-pass-unit]")!;
    expect(unit.firstElementChild).toHaveAttribute("data-pass-arrow");
    expect(unit.lastElementChild).toHaveAttribute("data-seat-badge");
    expect(unit.querySelector("[data-pass-arrow]")).toHaveTextContent("←");
  });
});
