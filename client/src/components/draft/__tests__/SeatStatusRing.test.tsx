import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, within } from "@testing-library/react";

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
            active_pack_count: 1,
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
      expect(badge).toHaveClass("min-w-[15ch]", "min-h-[40px]", "py-0.5", "pr-7");
      expect(badge).not.toHaveClass("gap-0.5", "pr-9");
      expect(unit.querySelector("[data-pass-arrow]")).toHaveTextContent("→");
    }
    const packIcon = screen.getByText("1", { selector: "[aria-hidden='true'] span" }).parentElement!;
    expect(packIcon).toHaveClass("h-7", "w-7", "right-0.5");
    expect(within(packIcon).getByText("1")).toHaveClass(
      "inset-0",
      "text-xs",
      "text-jade",
      "[-webkit-text-stroke:1px_rgb(2_6_23_/_0.95)]",
      "[paint-order:stroke_fill]",
    );
    expect(within(packIcon).getByText("1")).not.toHaveClass(
      "text-white",
      "rounded-full",
      "border",
      "bg-slate-950/80",
    );
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
