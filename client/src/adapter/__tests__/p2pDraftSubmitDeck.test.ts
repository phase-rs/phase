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
    { type: "Set", data: { set_pool_json: "{}" } } as never,
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

/**
 * U15/U21 — the CR 903.3 designation's LAUNCH channel at the same host seam.
 *
 * The submission suite above carries a designation INTO the session; this one
 * carries it back OUT, through `podCommanderDeckPayload` -> `botDeckForSeat` /
 * `submittedDeckForSeat` -> `deckPayload`. It drives the REAL assembler against
 * a stubbed adapter, so `deckPayload`'s widening is exercised rather than
 * mocked over — `DraftPodPage.commanderLaunch.test.tsx` mocks the host adapter
 * and therefore cannot reach any of these three functions.
 *
 * REVERT-PROBE: `deckPayload`'s hardcoded `commander: []`. Restore it and every
 * designation assertion below fails while the ordering assertions still pass,
 * so the two axes are independently guarded.
 */
describe("P2PDraftHost.podCommanderDeckPayload", () => {
  /** Seat 0 is the human host; seats 1..3 are bots. */
  function commanderPodView(seatCount: number, draftSetCode?: string) {
    return {
      kind: "CommanderDraft",
      status: "Complete",
      draft_set_code: draftSetCode ?? null,
      seats: Array.from({ length: seatCount }, (_, i) => ({
        seat_index: i,
        is_bot: i !== 0,
      })),
    } as never;
  }

  /**
   * `exportSession` returns the JSON `exportDraftSession` parses. Seat 0's
   * submission carries its OWN designation, and its pool holds one card the
   * main deck does not, so `sideboardFromPool` has something to produce and an
   * empty sideboard cannot pass as "the pool was read".
   */
  function launchAdapter(botCommander = "Bot Legend") {
    return {
      exportSession: vi.fn(async () =>
        JSON.stringify({
          pools: [[{ name: "Human Legend" }, { name: "Spare Card" }]],
          submitted_decks: {
            "0": {
              seat: 0,
              main_deck: ["Human Legend", "Plains"],
              commanders: ["Human Legend"],
            },
          },
        }),
      ),
      getBotDeck: vi.fn(async () => ({
        main_deck: [botCommander, "Bot Spell"],
        lands: { Plains: 2 },
        commander: [botCommander],
      })),
    } as unknown as Record<string, ReturnType<typeof vi.fn>>;
  }

  it("carries each seat's OWN commander, human and bot", async () => {
    const host = newHost();
    asPrivate(host).adapter = launchAdapter();

    const payload = await host.podCommanderDeckPayload(commanderPodView(4), 0);

    // Reach guard before any designation claim: the assembler really built a
    // deck, so an empty designation would be a real absence.
    expect(payload.player.main_deck.length).toBeGreaterThan(0);
    // REVERT-FAILING: `deckPayload` hardcodes `commander: []` at base, so this
    // is `[]` and the inequality below cannot hold either.
    expect(payload.player.commander).toEqual(["Human Legend"]);
    expect(payload.opponent.commander).toEqual(["Bot Legend"]);
    // Two seats with DIFFERENT designations, so "they differ" cannot pass on
    // two empties.
    expect(payload.player.commander).not.toEqual(payload.opponent.commander);
    // The bot's designation is a member of its main deck (CR 903.5a), and
    // `botDeckForSeat` flattens `lands` into it, so no name is added or lost.
    expect(payload.opponent.main_deck).toContain("Bot Legend");
    expect(payload.opponent.main_deck).toEqual([
      "Bot Legend",
      "Bot Spell",
      "Plains",
      "Plains",
    ]);
    // The human's sideboard is pool-minus-maindeck, proving the session's pools
    // were read rather than an empty default returned.
    expect(payload.player.sideboard).toEqual(["Spare Card"]);
  });

  it("maps the local seat to game player 0 and the rest in ascending seat order", async () => {
    const host = newHost();
    const adapter = launchAdapter();
    asPrivate(host).adapter = adapter;

    const payload = await host.podCommanderDeckPayload(commanderPodView(4), 0);

    expect(payload.player.main_deck.length).toBeGreaterThan(0);
    // N-1 non-local seats: one becomes `opponent`, the rest `ai_decks`.
    expect(payload.ai_decks).toHaveLength(2);
    expect(adapter.getBotDeck.mock.calls.map((c) => c[0])).toEqual([1, 2, 3]);
    // `exportDraftSession` is called ONCE for the whole payload, not per seat.
    expect(adapter.exportSession).toHaveBeenCalledTimes(1);
  });

  it("reads the pod's own seat count rather than a fixed four", async () => {
    const host = newHost();
    const adapter = launchAdapter();
    asPrivate(host).adapter = adapter;

    const payload = await host.podCommanderDeckPayload(commanderPodView(5), 0);

    // A 5-seat pod: 1 player + 1 opponent + 3 ai_decks. A hardcoded four would
    // give 2 here, so this is the row a fixed pod size reddens.
    expect(payload.ai_decks).toHaveLength(3);
    expect(adapter.getBotDeck.mock.calls.map((c) => c[0])).toEqual([1, 2, 3, 4]);
  });

  /**
   * U22. The draft's set code reaches the launch payload.
   *
   * CR 903.13f(3): a draft that contained Commander Masters boosters grants the
   * partner ability, for deckbuilding purposes, to any card that can be a
   * commander by itself whose color identity is one or fewer colors. The engine
   * decides that from `DeckList.draft_set_code`, and this is the client hop
   * that supplies it — read off the SAME view the assembler already builds the
   * decks from (`draft-core/src/view.rs:302`, populated at `:569`).
   *
   * REVERT-PROBE: drop `draft_set_code: view.draft_set_code` from
   * `podCommanderDeckPayload`'s return and `payload.draft_set_code` is
   * `undefined` here.
   */
  it("carries the view's draft set code into the launch payload", async () => {
    const host = newHost();
    asPrivate(host).adapter = launchAdapter();

    const payload = await host.podCommanderDeckPayload(
      commanderPodView(4, "CMM"),
      0,
    );

    // Reach guard (the same precedent as the `carries each seat's OWN commander`
    // row): the assembler ran, so an absent set code below would be a real
    // absence rather than a dead harness.
    expect(payload.player.commander).toEqual(["Human Legend"]);
    // REVERT-FAILING: `undefined` at base.
    expect(payload.draft_set_code).toBe("CMM");
  });

  it("leaves the launch payload's set code absent when the view carries none", async () => {
    const host = newHost();
    asPrivate(host).adapter = launchAdapter();

    const payload = await host.podCommanderDeckPayload(commanderPodView(4), 0);

    expect(payload.player.commander).toEqual(["Human Legend"]);
    // Paired negative: the assembler forwards the view's value verbatim rather
    // than manufacturing a set code, so constructed-shaped views stay grantless.
    expect(payload.draft_set_code ?? null).toBeNull();
  });

  it("propagates a draft-wasm refusal rather than shipping an unjudged deck", async () => {
    const host = newHost();
    asPrivate(host).adapter = {
      ...launchAdapter(),
      getBotDeck: vi.fn(async () => {
        throw new Error(
          "Card database must be loaded before a Commander Draft bot deck",
        );
      }),
    } as unknown as Record<string, ReturnType<typeof vi.fn>>;

    await expect(
      host.podCommanderDeckPayload(commanderPodView(4), 0),
    ).rejects.toThrow("Card database");
  });

  it("throws when the local seat has no submitted deck", async () => {
    const host = newHost();
    asPrivate(host).adapter = {
      ...launchAdapter(),
      exportSession: vi.fn(async () =>
        JSON.stringify({ pools: [[]], submitted_decks: {} }),
      ),
    } as unknown as Record<string, ReturnType<typeof vi.fn>>;

    await expect(
      host.podCommanderDeckPayload(commanderPodView(4), 0),
    ).rejects.toThrow("Seat 0 has no submitted deck");
  });
});
