import { beforeEach, describe, expect, it, vi } from "vitest";

// Mock the draft-adapter module — vitest cannot resolve the lazy
// `@wasm/draft` import that DraftAdapter's `ensureDraftWasm` performs
// (the vitest config only stubs `@wasm/engine`). The seat-gate tests below
// drive the real P2PDraftHost but overwrite its `adapter` field per-test, so
// a no-op constructor mock is sufficient.
vi.mock("../draft-adapter", () => ({
  DraftAdapter: vi.fn().mockImplementation(function () {
    return {};
  }),
}));

import { P2PDraftHost } from "../p2p-draft-host";
import type { DraftMatchBinding, DraftMatchLaunch, DraftP2PMessage } from "../../network/draftProtocol";
import type { DraftPlayerView, PairingView } from "../draft-adapter";
import { draftIntergameDigest, type DraftIntergameCommand } from "../../services/intergameCommandLedger";

describe("P2PDraftHost Bo3", () => {
  describe("durable intergame ledger", () => {
    function command(seat: number, launchDigest: string): DraftIntergameCommand {
      return {
        commandId: `sideboard-${seat}`,
        matchId: "bo3-1",
        gameNumber: 2,
        seat,
        payload: { type: "SubmitSideboard", main: [], sideboard: [] },
        launchPayload: { matchId: "bo3-1", seat },
        launchDigest,
        payloadDigest: draftIntergameDigest({ type: "SubmitSideboard", main: [], sideboard: [] }),
        status: "Pending",
      };
    }

    it("authorizes both Traditional sideboards only after both held commands arrive", () => {
      const host = new P2PDraftHost(
        { id: "host" } as never, () => () => {},
        { type: "Set", data: { set_pool_json: "{}" } } as never,
        "Traditional", 8, "Host", "Swiss", "Casual",
      );
      const sent = new Map<number, DraftP2PMessage[]>([[1, []], [2, []]]);
      const privateHost = host as unknown as {
        bo3State: Map<string, unknown>;
        launchDigests: Map<string, Map<number, string>>;
        guestSessions: Map<number, { send: (message: DraftP2PMessage) => void }>;
        intergameCommands: { snapshot: () => DraftIntergameCommand[] };
      };
      privateHost.bo3State.set("bo3-1", {
        seatA: 1, seatB: 2, submittedA: false, submittedB: false,
        loserSeat: 1, gameNumber: 2, score: { p0_wins: 0, p1_wins: 1, draws: 0 },
      });
      const launch1 = draftIntergameDigest({ matchId: "bo3-1", seat: 1 });
      const launch2 = draftIntergameDigest({ matchId: "bo3-1", seat: 2 });
      privateHost.launchDigests.set("bo3-1", new Map([[1, launch1], [2, launch2]]));
      for (const seat of [1, 2]) {
        privateHost.guestSessions.set(seat, { send: (message) => sent.get(seat)!.push(message) });
      }

      host.submitAuthorized(1, command(1, launch1));
      expect(sent.get(1)).toEqual([]);
      host.submitAuthorized(2, command(2, launch2));
      expect(sent.get(1)?.[0]?.type).toBe("draft_bo3_intergame_authorized");
      expect(sent.get(2)?.[0]?.type).toBe("draft_bo3_intergame_authorized");
      expect(privateHost.intergameCommands.snapshot().every((entry) => entry.status === "Executing")).toBe(true);
    });

    it("rejects forged and stale held commands before authorization", () => {
      const host = new P2PDraftHost(
        { id: "host" } as never, () => () => {},
        { type: "Set", data: { set_pool_json: "{}" } } as never,
        "Traditional", 8, "Host", "Swiss", "Casual",
      );
      const privateHost = host as unknown as {
        bo3State: Map<string, unknown>;
        launchDigests: Map<string, Map<number, string>>;
        intergameCommands: { snapshot: () => DraftIntergameCommand[] };
      };
      privateHost.bo3State.set("bo3-1", {
        seatA: 1, seatB: 2, submittedA: false, submittedB: false,
        loserSeat: 1, gameNumber: 2, score: { p0_wins: 0, p1_wins: 1, draws: 0 },
      });
      const launch1 = draftIntergameDigest({ matchId: "bo3-1", seat: 1 });
      privateHost.launchDigests.set("bo3-1", new Map([[1, launch1]]));
      host.submitAuthorized(1, { ...command(1, launch1), payloadDigest: "forged" });
      host.submitAuthorized(1, command(1, "stale"));
      expect(privateHost.intergameCommands.snapshot()).toEqual([]);
    });
  });

  describe("BO3-02: sideboard timer auto-submit", () => {
    it("issues unchanged deck defaults through the authorized intergame ledger", () => {
      vi.useFakeTimers();
      try {
        const host = new P2PDraftHost(
          { id: "host" } as never, () => () => {},
          { type: "Set", data: { set_pool_json: "{}" } } as never,
          "Traditional", 8, "Host", "Swiss", "Competitive",
        );
        const sent = new Map<number, DraftP2PMessage[]>([[1, []], [2, []]]);
        const binding: DraftMatchBinding = {
          podId: "draft-1", matchId: "bo3-1", round: 1, sessionKey: "session", lease: "lease", nonce: "nonce", revision: 0, matchAuthoritySeat: 1,
        };
        const launch = (seat: number): DraftMatchLaunch => ({
          type: "HumanGuest",
          matchId: "bo3-1",
          matchRoomCode: "room",
          round: 1,
          localSeat: seat,
          opponentSeat: seat === 1 ? 2 : 1,
          opponentName: "Opponent",
          matchHostPeerId: "room",
          localDeck: { main_deck: [seat === 1 ? "Plains" : "Island"], sideboard: ["Negate"], commander: [] },
          matchConfig: { match_type: "Bo3" },
          binding,
        });
        const launch1 = launch(1);
        const launch2 = launch(2);
        const privateHost = host as unknown as {
          bo3State: Map<string, unknown>;
          launchDigests: Map<string, Map<number, string>>;
          matchLaunches: Map<string, Map<number, DraftMatchLaunch>>;
          guestSessions: Map<number, { send: (message: DraftP2PMessage) => void; close: () => void }>;
          intergameCommands: { snapshot: () => DraftIntergameCommand[] };
          startSideboardTimer: (matchId: string) => void;
        };
        privateHost.bo3State.set("bo3-1", {
          seatA: 1, seatB: 2, submittedA: false, submittedB: false,
          loserSeat: 1, gameNumber: 2, score: { p0_wins: 0, p1_wins: 1, draws: 0 },
          decks: [
            { seat: 1, main: [{ name: "Plains", count: 1 }], sideboard: [{ name: "Negate", count: 1 }] },
            { seat: 2, main: [{ name: "Island", count: 1 }], sideboard: [{ name: "Negate", count: 1 }] },
          ],
        });
        privateHost.launchDigests.set("bo3-1", new Map([
          [1, draftIntergameDigest(launch1)],
          [2, draftIntergameDigest(launch2)],
        ]));
        privateHost.matchLaunches.set("bo3-1", new Map([[1, launch1], [2, launch2]]));
        for (const seat of [1, 2]) {
          privateHost.guestSessions.set(seat, { send: (message) => sent.get(seat)!.push(message), close: () => {} });
        }

        privateHost.startSideboardTimer("bo3-1");
        vi.advanceTimersByTime(60_000);

        for (const seat of [1, 2]) {
          expect(sent.get(seat)).toContainEqual(expect.objectContaining({
            type: "draft_bo3_intergame_authorized",
            command: expect.objectContaining({
              seat,
              payload: {
                type: "SubmitSideboard",
                main: [{ name: seat === 1 ? "Plains" : "Island", count: 1 }],
                sideboard: [{ name: "Negate", count: 1 }],
              },
            }),
          }));
        }
        expect(privateHost.intergameCommands.snapshot().every((command) => command.status === "Executing")).toBe(true);
        host.dispose();
      } finally {
        vi.useRealTimers();
      }
    });

    it("issues the play-first default through the same ledger when the choice timer expires", () => {
      vi.useFakeTimers();
      try {
        const host = new P2PDraftHost(
          { id: "host" } as never, () => () => {},
          { type: "Set", data: { set_pool_json: "{}" } } as never,
          "Traditional", 8, "Host", "Swiss", "Competitive",
        );
        const launch = {
          type: "HumanGuest" as const,
          matchId: "bo3-1",
          matchRoomCode: "room",
          round: 1,
          localSeat: 1,
          opponentSeat: 2,
          opponentName: "Opponent",
          matchHostPeerId: "room",
          localDeck: { main_deck: ["Plains"], sideboard: [], commander: [] },
          matchConfig: { match_type: "Bo3" as const },
          binding: { podId: "draft-1", matchId: "bo3-1", round: 1, sessionKey: "session", lease: "lease", nonce: "nonce", revision: 0, matchAuthoritySeat: 1 },
        };
        const sent: DraftP2PMessage[] = [];
        const privateHost = host as unknown as {
          bo3State: Map<string, unknown>;
          launchDigests: Map<string, Map<number, string>>;
          matchLaunches: Map<string, Map<number, DraftMatchLaunch>>;
          guestSessions: Map<number, { send: (message: DraftP2PMessage) => void; close: () => void }>;
          startPlayDrawTimer: (matchId: string) => void;
        };
        privateHost.bo3State.set("bo3-1", {
          seatA: 1, seatB: 2, submittedA: true, submittedB: true,
          loserSeat: 1, gameNumber: 2, score: { p0_wins: 0, p1_wins: 1, draws: 0 }, decks: [],
        });
        privateHost.launchDigests.set("bo3-1", new Map([[1, draftIntergameDigest(launch)]]));
        privateHost.matchLaunches.set("bo3-1", new Map([[1, launch]]));
        privateHost.guestSessions.set(1, { send: (message) => sent.push(message), close: () => {} });

        privateHost.startPlayDrawTimer("bo3-1");
        vi.advanceTimersByTime(10_000);

        expect(sent).toContainEqual(expect.objectContaining({
          type: "draft_bo3_intergame_authorized",
          command: expect.objectContaining({ payload: { type: "ChoosePlayDraw", playFirst: true } }),
        }));
        host.dispose();
      } finally {
        vi.useRealTimers();
      }
    });
  });

  describe("BO3-03: no timer in Casual", () => {
    it("publishes an untimed sideboard prompt without arming the production timer", () => {
      const host = new P2PDraftHost(
        { id: "host" } as never, () => () => {},
        { type: "Set", data: { set_pool_json: "{}" } } as never,
        "Traditional", 8, "Host", "Swiss", "Casual",
      );
      const events: unknown[] = [];
      host.onEvent((event) => events.push(event));

      host.handleMatchBetweenGames(
        "bo3-1", 2, { p0_wins: 1, p1_wins: 0, draws: 0 }, 2, 0, 2,
      );

      expect(host.activeTimerContext).toBeNull();
      expect(events).toContainEqual(expect.objectContaining({
        type: "bo3SideboardPrompt",
        timerMs: 0,
      }));
      host.dispose();
    });
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

    const binding: DraftMatchBinding = {
      podId: "draft-1",
      matchId: "m-12",
      round: 2,
      sessionKey: "session-1",
      lease: "lease-1",
      nonce: "nonce-1",
      revision: 0,
      matchAuthoritySeat: 1,
    };

    async function deliverRaw(
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

    async function deliverSettlement(
      seat: number,
      submittedBinding: DraftMatchBinding = binding,
    ): Promise<void> {
      await (
        host as unknown as {
          handleGuestMessage: (s: number, m: DraftP2PMessage) => Promise<void>;
        }
      ).handleGuestMessage(seat, {
        type: "draft_match_settlement",
        settlement: { binding: submittedBinding, receiptId: "receipt-1", winnerSeat: 1 },
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
      (host as unknown as { matchBindings: Map<string, DraftMatchBinding> })
        .matchBindings.set(binding.matchId, binding);
    });

    it("accepts only the bound match-authority settlement and acks its receipt", async () => {
      await deliverSettlement(1);
      expect(reportSpy).toHaveBeenCalledWith("m-12", 1);
      expect(sent.get(1)).toEqual([
        { type: "draft_match_settlement_ack", matchId: "m-12", receiptId: "receipt-1", revision: 0 },
      ]);
    });

    it("rejects the raw result shape", async () => {
      await deliverRaw(1, "m-12", 1);
      expect(reportSpy).not.toHaveBeenCalled();
      expect(sent.get(1)).toEqual([
        { type: "draft_error", reason: "Unbound match result" },
      ]);
    });

    it("rejects a forged binding", async () => {
      await deliverSettlement(1, { ...binding, nonce: "forged" });
      expect(reportSpy).not.toHaveBeenCalled();
      expect(sent.get(1)).toEqual([
        { type: "draft_error", reason: "Invalid match binding" },
      ]);
    });

    it("rejects a bound settlement when its round is no longer current", async () => {
      setHostView({
        current_round: 2,
        pairings: [pairing("m-12", 1, 1, 2)],
      });
      await deliverSettlement(1);
      expect(reportSpy).not.toHaveBeenCalled();
      expect(sent.get(1)).toEqual([
        { type: "draft_error", reason: "Unauthorized match settlement" },
      ]);
    });
  });
});
