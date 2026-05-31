import { beforeEach, describe, expect, it, vi } from "vitest";

// Mock the draft-adapter module — vitest cannot resolve the lazy
// `@wasm/draft` import that DraftAdapter's `ensureDraftWasm` performs
// (the vitest config only stubs `@wasm/engine`). The seat-gate tests below
// drive the real P2PDraftHost but overwrite its `adapter` field per-test, so
// a no-op constructor mock is sufficient.
vi.mock("../draft-adapter", () => ({
  DraftAdapter: vi.fn().mockImplementation(() => ({})),
}));

import { P2PDraftHost } from "../p2p-draft-host";
import type { DraftP2PMessage } from "../../network/draftProtocol";
import type { DraftPlayerView, PairingView } from "../draft-adapter";

describe("P2PDraftHost Bo3", () => {
  describe("BO3-02: sideboard timer auto-submit", () => {
    it.todo(
      "auto-submits current deck when 60s sideboard timer expires in Competitive mode",
    );
  });

  describe("BO3-03: no timer in Casual", () => {
    it.todo("does not start sideboard timer when podPolicy is Casual");
    it.todo("sends timerMs: 0 in sideboard prompt for Casual mode");
  });

  // Security regression guard for PR #1454: the draft_match_result handler must
  // only accept results from a guest seated in the named pairing. The host is
  // the authoritative relay and the only layer that maps a DataConnection to a
  // seat, so the participant check belongs here (the draft-core session has no
  // concept of the sending peer). Mirrors the seat checks in
  // `handleSideboardSubmit` (T-58-01) and `handlePlayDrawChosen` (T-58-04).
  describe("draft_match_result seat gate", () => {
    function pairing(
      matchId: string,
      round: number,
      seatA: number,
      seatB: number,
    ): PairingView {
      return {
        round,
        table: 1,
        seat_a: seatA,
        name_a: `A${seatA}`,
        seat_b: seatB,
        name_b: `B${seatB}`,
        match_id: matchId,
        status: "InProgress",
        winner_seat: null,
        score_a: null,
        score_b: null,
      };
    }

    let host: P2PDraftHost;
    let reportSpy: ReturnType<typeof vi.fn>;
    const sent = new Map<number, DraftP2PMessage[]>();

    function fakeSession(seat: number) {
      sent.set(seat, []);
      return { send: (m: DraftP2PMessage) => sent.get(seat)!.push(m) };
    }

    function setHostView(view: Partial<DraftPlayerView>): void {
      const adapter = (host as unknown as { adapter: Record<string, unknown> })
        .adapter;
      adapter.getViewForSeat = vi.fn(async () => view as DraftPlayerView);
    }

    async function deliver(
      seat: number,
      matchId: string,
      winnerSeat: number | null,
    ): Promise<void> {
      await (
        host as unknown as {
          handleGuestMessage: (s: number, m: DraftP2PMessage) => Promise<void>;
        }
      ).handleGuestMessage(seat, {
        type: "draft_match_result",
        matchId,
        winnerSeat,
      });
    }

    beforeEach(() => {
      sent.clear();
      host = new P2PDraftHost(
        { id: "host" } as never,
        () => () => {},
        { type: "Set", data: { set_pool_json: "{}" } } as never,
        "Premier",
        8,
        "Host",
        "Swiss",
        "Competitive",
      );

      // Current round 2, table 1 pairs seats 1 & 2; table 2 pairs seats 3 & 4.
      setHostView({
        current_round: 2,
        pairings: [pairing("m-12", 2, 1, 2), pairing("m-34", 2, 3, 4)],
      });

      // Spy on reportMatchResult to detect acceptance without touching WASM.
      reportSpy = vi.fn(async () => {});
      (host as unknown as { reportMatchResult: unknown }).reportMatchResult =
        reportSpy;

      // Seat the participants (1, 2) plus a bystander guest (seat 5).
      const guestSessions = (
        host as unknown as { guestSessions: Map<number, unknown> }
      ).guestSessions;
      guestSessions.set(1, fakeSession(1));
      guestSessions.set(2, fakeSession(2));
      guestSessions.set(5, fakeSession(5));
    });

    it("accepts a result reported by a seated participant", async () => {
      await deliver(1, "m-12", 1);
      expect(reportSpy).toHaveBeenCalledWith("m-12", 1);
      expect(sent.get(1)).toEqual([]);
    });

    it("rejects a result from a non-participant guest", async () => {
      await deliver(5, "m-12", 1);
      expect(reportSpy).not.toHaveBeenCalled();
      expect(sent.get(5)).toEqual([
        { type: "draft_error", reason: "Not a participant in this match" },
      ]);
    });

    it("rejects a result for a match not in the current round", async () => {
      setHostView({
        current_round: 2,
        pairings: [pairing("m-12", 1, 1, 2)],
      });
      await deliver(1, "m-12", 1);
      expect(reportSpy).not.toHaveBeenCalled();
      expect(sent.get(1)).toEqual([
        { type: "draft_error", reason: "Unknown match" },
      ]);
    });
  });
});
