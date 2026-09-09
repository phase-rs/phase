import { describe, expect, it, vi } from "vitest";

import { P2PDraftHost } from "../p2p-draft-host";

/**
 * U17 — the commander designation's submission channel at the P2P host seam.
 *
 * CR 903.3: "Each deck has a legendary card designated as its commander." This
 * suite asserts the host carries that designation from the wire to the adapter;
 * CR 903.1 scopes the designation to the Commander variant, which is why the
 * empty-designation row is a legal payload and not a hostile one.
 *
 * Every test drives the REAL host seam (`handleGuestMessage` / `submitHostDeck`
 * / the private `handleDeckSubmission` funnel) against a stubbed adapter,
 * rather than asserting a message shape.
 *
 * WHAT THIS HARNESS DOES NOT RUN: `handleGuestMessage` is invoked directly with
 * a plain object literal. `validateDraftMessage` runs UPSTREAM of it, in
 * `decodeDraftWireMessage` -> `draftPeerSession.ts`'s `conn.on("data")`, and
 * this harness constructs no peer session. So no row here may name
 * `validateSubmitDeck`, its bound or its floor as the branch it reaches —
 * every validator claim belongs to `draftProtocol.test.ts`, which is the only
 * suite in `client/src` that runs the validator at all.
 *
 * Modelled on `p2pDraftPick.test.ts`, including its `REVERT-PROBE:`
 * convention: every test names the exact line whose reversion it catches.
 */

function newHost(kind: "CommanderDraft" | "Premier" = "CommanderDraft") {
  return new P2PDraftHost(
    { id: "host" } as never,
    () => () => {},
    { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } } as never,
    kind,
    4,
    "Host",
    "Swiss",
    "Competitive",
  );
}

type PrivateHost = {
  draftStarted: boolean;
  paused: boolean;
  guestSessions: Map<number, { send: ReturnType<typeof vi.fn> }>;
  adapter: Record<string, ReturnType<typeof vi.fn>>;
  handleGuestMessage: (seat: number, message: unknown) => Promise<void>;
};

function asPrivate(host: P2PDraftHost): PrivateHost {
  return host as unknown as PrivateHost;
}

/**
 * The two adapter methods `handleDeckSubmission` reaches.
 *
 * `getViewForSeat` is NOT optional here: the funnel awaits it AFTER the send,
 * inside the same `try`, so a stub missing it throws, the `catch` sends
 * `draft_error` and rethrows — which is exactly what the rejection row asserts,
 * reached for the wrong reason.
 *
 * Its `seats` array must hold at least one seat that is neither
 * `has_submitted_deck` nor `is_bot`: `seats.every(...)` on an EMPTY array is
 * `true`, which enters `allDecksSubmitted` -> `generatePairings()`, absent from
 * this stub.
 */
function stubAdapter(overrides: Record<string, unknown> = {}) {
  return {
    submitDeckForSeat: vi.fn(async () => ({ status: "Deckbuilding", seat_view: "submitting" })),
    getViewForSeat: vi.fn(async () => ({
      status: "Deckbuilding",
      seats: [{ has_submitted_deck: false, is_bot: false }],
    })),
    ...overrides,
  } as unknown as Record<string, ReturnType<typeof vi.fn>>;
}

/** Seed a guest session for `seat` — without it, every send is a silent no-op. */
function seatGuestSession(privateHost: PrivateHost, seat: number) {
  const send = vi.fn();
  privateHost.guestSessions.set(seat, { send });
  return send;
}

describe("P2P deck-submission channel", () => {
  /**
   * V-TS-1. The wire message's designation reaches the adapter, in order.
   *
   * REVERT-PROBE: change `p2p-draft-host.ts`'s `case "draft_submit_deck"` to
   * `handleDeckSubmission(seat, msg.mainDeck)` — dropping the third argument —
   * and this reds.
   */
  it("routes a guest's designation to the adapter, in order", async () => {
    const host = newHost();
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    privateHost.paused = false;
    privateHost.adapter = stubAdapter();
    const send = seatGuestSession(privateHost, 2);

    await privateHost.handleGuestMessage(2, {
      type: "draft_submit_deck",
      submissionId: "submission-1",
      mainDeck: ["Plains", "Island"],
      commanders: ["Kenrith, the Returned King"],
    });

    expect(privateHost.adapter.submitDeckForSeat).toHaveBeenCalledWith(
      2,
      ["Plains", "Island"],
      ["Kenrith, the Returned King"],
    );
    // Paired positive reach-guard: the call RESOLVED and the seat received its
    // durable receipt (`draft_deck_submit_ack`, draft protocol 14).
    // A thrown guard cannot satisfy a not-called negative elsewhere in this
    // suite if this row cannot reach the adapter at all.
    expect(send).toHaveBeenCalledWith(
      expect.objectContaining({ type: "draft_deck_submit_ack", submissionId: "submission-1" }),
    );
  });

  /**
   * V-TS-2. Multi-authority: the funnel has TWO callers, and each must carry
   * its OWN designation. A guest's `draft_submit_deck` arrives on the wire at
   * seat n; the host's `submitHostDeck` never touches the wire and is seat 0.
   *
   * REVERT-PROBE: change `submitHostDeck` to
   * `handleDeckSubmission(0, mainDeck)` and this reds.
   */
  it("carries the host's own designation through submitHostDeck", async () => {
    const host = newHost();
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    privateHost.paused = false;
    privateHost.adapter = stubAdapter();

    const view = await host.submitHostDeck(
      ["Swamp", "Mountain"],
      ["Gyruda, Doom of Depths"],
    );

    expect(privateHost.adapter.submitDeckForSeat).toHaveBeenCalledWith(
      0,
      ["Swamp", "Mountain"],
      ["Gyruda, Doom of Depths"],
    );
    // Reach-guard: seat 0 returns the SUBMITTING seat's own view, not the host
    // view the other branch returns — so the funnel ran to completion.
    expect(view).toMatchObject({ seat_view: "submitting" });
  });

  /**
   * V-TS-3. An empty designation is FORWARDED — neither dropped nor defaulted
   * into a synthesised name.
   *
   * This proves the HOST forwards `[]`. It proves NOTHING about the validator's
   * floor: `validateDraftMessage` does not run in this harness. The floor of 0
   * is `draftProtocol.test.ts`'s zero-entry accept row, and only that row.
   *
   * REVERT-PROBE: the same dropped-argument reversion as V-TS-1 — with two
   * arguments, `toHaveBeenCalledWith(seat, mainDeck, [])` reds.
   */
  it("forwards an empty designation rather than dropping it", async () => {
    const host = newHost("Premier");
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    privateHost.paused = false;
    privateHost.adapter = stubAdapter();
    const send = seatGuestSession(privateHost, 1);

    await privateHost.handleGuestMessage(1, {
      type: "draft_submit_deck",
      submissionId: "submission-1",
      mainDeck: ["Forest"],
      commanders: [],
    });

    expect(privateHost.adapter.submitDeckForSeat).toHaveBeenCalledWith(1, ["Forest"], []);
    expect(send).toHaveBeenCalledWith(
      expect.objectContaining({ type: "draft_deck_submit_ack", submissionId: "submission-1" }),
    );
  });

  /**
   * V-TS-4. Hostile: a submission BEFORE the draft starts is refused and never
   * reaches the adapter. The `!this.draftStarted` guard is unchanged by this
   * phase; the row exists so a new argument threaded through the case body
   * cannot have moved the refusal.
   *
   * The negative is not vacuous: V-TS-1 proves this same harness CAN reach the
   * adapter.
   */
  it("refuses a submission before the draft has started", async () => {
    const host = newHost();
    const privateHost = asPrivate(host);
    privateHost.draftStarted = false;
    privateHost.adapter = stubAdapter();
    const send = seatGuestSession(privateHost, 3);

    await privateHost.handleGuestMessage(3, {
      type: "draft_submit_deck",
      submissionId: "submission-1",
      mainDeck: ["Plains"],
      commanders: ["Kenrith, the Returned King"],
    });

    expect(send).toHaveBeenCalledWith({
      type: "draft_error",
      reason: "Draft not started",
      submissionId: "submission-1",
      submissionDisposition: "Rejected",
    });
    expect(privateHost.adapter.submitDeckForSeat).not.toHaveBeenCalled();
  });

  /**
   * V-TS-5. The loud-refusal surface: an adapter rejection reaches the
   * SUBMITTING guest as `draft_error`, and rethrows.
   *
   * The `reason`-TEXT assertion is the second belt. `handleDeckSubmission`'s
   * `catch` is reachable from `getViewForSeat` as well as from
   * `submitDeckForSeat`, so asserting merely that a `draft_error` was sent
   * would pass for a mis-stubbed fixture. Asserting the rejection's OWN
   * distinctive message is what attributes the error to its cause.
   *
   * REVERT-PROBE: weaken `handleDeckSubmission`'s `try`/`catch` — swallow the
   * error, or stop sending `draft_error` — and this reds. §9 Step 6 forbids it.
   */
  it("returns an adapter rejection to the submitting guest as draft_error", async () => {
    const host = newHost();
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    privateHost.paused = false;
    privateHost.adapter = stubAdapter({
      submitDeckForSeat: vi.fn(async () => {
        throw new Error("card 'Kenrith, the Returned King' is designated as commander 1 time(s) but the deck contains 0 copy(ies)");
      }),
    });
    const send = seatGuestSession(privateHost, 2);

    await expect(
      privateHost.handleGuestMessage(2, {
        type: "draft_submit_deck",
        submissionId: "submission-1",
        mainDeck: ["Plains"],
        commanders: ["Kenrith, the Returned King"],
      }),
    ).rejects.toThrow("is designated as commander");

    expect(send).toHaveBeenCalledWith({
      type: "draft_error",
      reason:
        "card 'Kenrith, the Returned King' is designated as commander 1 time(s) but the deck contains 0 copy(ies)",
      submissionId: "submission-1",
      submissionDisposition: "Rejected",
    });
  });
});
