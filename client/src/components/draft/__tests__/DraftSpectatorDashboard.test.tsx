import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";

import type { SeatPublicView, SpectatorDraftView } from "../../../adapter/draft-adapter";
import { DraftSpectatorDashboard } from "../DraftSpectatorDashboard";

function seat(seat_index: number, display_name: string, pick_status: SeatPublicView["pick_status"]): SeatPublicView {
  return {
    seat_index,
    display_name,
    is_bot: false,
    connected: true,
    has_submitted_deck: false,
    pick_status,
    active_pack_count: 0,
    drafted_card_count: 0,
    face_up_draft_cards: [],
  };
}

function view(seats: SeatPublicView[]): SpectatorDraftView {
  return {
    status: "Drafting",
    kind: "Winston",
    current_pack_number: 0,
    pick_number: 0,
    pass_direction: "Left",
    seats,
    cards_per_pack: 15,
    pick_steps_per_pack: 15,
    pack_count: 3,
    min_deck_size: 40,
    addable_cards: [],
    standings: [],
    current_round: 0,
    tournament_format: "Swiss",
    pod_policy: "Casual",
    pairings: [],
    match_config: { match_type: "Bo1" },
  };
}

describe("DraftSpectatorDashboard seat status", () => {
  afterEach(cleanup);

  it("translates every PickStatus instead of printing the wire enum", () => {
    render(
      <DraftSpectatorDashboard
        view={view([
          seat(0, "Alice", "Pending"),
          seat(1, "Bob", "Waiting"),
          seat(2, "Cara", "TimedOut"),
          seat(3, "Dan", "NotDrafting"),
          seat(4, "Erin", "Picked"),
        ])}
      />,
    );

    // Reach guard: the seat list rendered at all.
    expect(screen.getByText("Alice")).toBeInTheDocument();

    // REVERT-FAILING: rendering `{seat.pick_status}` raw produces the enum
    // spellings asserted absent below, and none of these three copy strings.
    expect(screen.getByText("Picking")).toBeInTheDocument();
    expect(screen.getByText("Timed out")).toBeInTheDocument();
    expect(screen.getByText("Not drafting")).toBeInTheDocument();
    // `Waiting` — the variant a shared-stack pod introduced, and the reason this
    // lookup exists — resolves through the same namespace as the four that
    // predate it.
    expect(screen.getByText("Waiting")).toBeInTheDocument();
    expect(screen.getByText("Picked")).toBeInTheDocument();

    expect(screen.queryByText("TimedOut")).toBeNull();
    expect(screen.queryByText("NotDrafting")).toBeNull();
    expect(screen.queryByText("Pending")).toBeNull();
  });
});
