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

import { SeatStatusRing } from "../SeatStatusRing";

describe("SeatStatusRing", () => {
  afterEach(cleanup);

  it("shows other drafters' face-up draft cards", () => {
    render(<SeatStatusRing />);

    expect(screen.getByText("Face-up: Cogwork Librarian")).toBeInTheDocument();
  });
});