import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";

vi.mock("../../../stores/multiplayerDraftStore", () => ({
  useMultiplayerDraftStore: (selector: (state: Record<string, unknown>) => unknown) =>
    selector({
      seatIndex: 0,
      view: {
        pass_direction: "Left",
        seats: [
          {
            seat_index: 0,
            display_name: "Drafter",
            is_bot: false,
            connected: true,
            has_submitted_deck: false,
            pick_status: "Pending",
            face_up_draft_cards: [],
          },
          {
            seat_index: 1,
            display_name: "Opponent",
            is_bot: false,
            connected: true,
            has_submitted_deck: false,
            pick_status: "Picked",
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
        ],
      },
    }),
}));

import { SeatStatusRing, SeatStatusRingLayout } from "../SeatStatusRing";

describe("SeatStatusRing", () => {
  afterEach(cleanup);

  it("shows other drafters' face-up draft cards", () => {
    const { container } = render(<SeatStatusRing />);

    expect(screen.getByText("Face-up: Cogwork Librarian")).toBeInTheDocument();
    expect(container.querySelector("[data-seat-status-ring]")).toHaveClass(
      "grid-cols-[repeat(auto-fit,minmax(calc(15ch+1.5rem),1fr))]",
      "text-xs",
    );
    const units = container.querySelectorAll<HTMLElement>("[data-seat-pass-unit]");
    expect(units).toHaveLength(2);
    for (const unit of units) {
      expect(unit.firstElementChild).toHaveAttribute("data-seat-badge");
      expect(unit.lastElementChild).toHaveAttribute("data-pass-arrow");
      expect(unit.querySelector("[data-seat-badge]")).toHaveClass("min-w-[15ch]");
      expect(unit.querySelector("[data-pass-arrow]")).toHaveTextContent("→");
    }
  });

  it("places right-pass arrows before their equal-width seat badges", () => {
    const seats = [{
      seat_index: 0,
      display_name: "Drafter",
      is_bot: false,
      connected: true,
      has_submitted_deck: false,
      pick_status: "Pending" as const,
      face_up_draft_cards: [],
    }];

    const { container } = render(
      <SeatStatusRingLayout
        seats={seats}
        passDirection="Right"
        localSeat={0}
        passDirectionLabel="Passing Right"
      />,
    );

    const unit = container.querySelector<HTMLElement>("[data-seat-pass-unit]")!;
    expect(unit.firstElementChild).toHaveAttribute("data-pass-arrow");
    expect(unit.lastElementChild).toHaveAttribute("data-seat-badge");
    expect(unit.querySelector("[data-pass-arrow]")).toHaveTextContent("←");
  });
});