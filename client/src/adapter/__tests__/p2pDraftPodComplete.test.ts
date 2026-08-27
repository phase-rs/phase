import { describe, expect, it, vi } from "vitest";

import { P2PDraftHost } from "../p2p-draft-host";

/**
 * U21 — the post-submission funnel reads the engine's published status instead
 * of assuming a tournament, and publishes it to the whole pod.
 *
 * The defect: `handleDeckSubmission`'s `:826` called `generatePairings()`
 * unguarded. For a `PostDraftPlay::CompleteImmediately` pod the reducer has
 * already assigned `Complete` (`draft-core/src/session.rs:902`, assigned at
 * `:905`), and `apply_generate_pairings` REFUSES a `Complete` session (its
 * guard at `:214-217` admits only `Deckbuilding | Pairing | RoundComplete`).
 * So the call threw, `generatePairings()`'s catch emitted `error`, and
 * `broadcastViews()` at `:1124` was never reached — the pod never learned it
 * was complete.
 *
 * THE STUB IS TIME-KEYED, NOT CONSTANT, AND IT HAS TWO CLOCKS. A constant
 * adapter carries neither reducer fact, and under one every row below is
 * vacuous:
 *
 *   Clock (a) — the accumulating submitted-seat set. `submitDeckForSeat(seat)`
 *   adds `seat`; `getViewForSeat` projects the set into
 *   `seats[].has_submitted_deck` and derives the status: `Deckbuilding` while
 *   any HUMAN seat is outstanding, then `Complete` (`CompleteImmediately`) or
 *   `Pairing` (`TournamentPairings`) once the last human is in. This mirrors
 *   `session.rs:895` (the outstanding-human gate) and `:901-905`.
 *
 *   Clock (b) — the `pairingsGenerated` boolean. `getViewForSeat` returns
 *   `Pairing` before `generatePairings()` and `MatchInProgress` after
 *   (`session.rs:254`); and `generatePairings()` REJECTS any status outside the
 *   reducer's admit set, which is what makes a `Complete` pod's unfixed tree
 *   throw.
 *
 * One boolean provably cannot do this: `pairingsGenerated` is `false` both when
 * row 1 needs `Complete` and when row 2 needs `Deckbuilding`.
 *
 * The premise those two clocks encode is not invented here — it is pinned
 * against the real reducer by
 * `crates/draft-core/src/session.rs`'s
 * `commander_pod_reaches_complete_and_generate_pairings_is_refused`.
 *
 * `getViewForSeat`'s return carries `pairings` and `pool`, and
 * `generatePairings()`'s carries `current_round` and `pairings`, because
 * `generatePairings()` dereferences all of them inside a `try` — a missing key
 * is a `TypeError` surfaced as an `error` event, which reds a row through its
 * reach-guard for a reason unrelated to its claim.
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

/** Seat 0 is the host seat: it has no `guestSessions` entry, by construction. */
type SeatSpec = { seat: number; isBot: boolean };

type DraftStatusName =
  | "Lobby"
  | "Drafting"
  | "Paused"
  | "Deckbuilding"
  | "Pairing"
  | "MatchInProgress"
  | "RoundComplete"
  | "Complete"
  | "Abandoned";

function twoClockStub(opts: {
  postDraftPlay: "CompleteImmediately" | "TournamentPairings";
  seats: SeatSpec[];
  poolSize?: number;
}) {
  const submitted = new Set<number>();
  let pairingsGenerated = false;
  const humanSeats = opts.seats.filter((s) => !s.isBot).map((s) => s.seat);

  /** The reducer's own transition rule, not a constant. */
  function statusNow(): DraftStatusName {
    // session.rs:895 — no transition while a human seat is outstanding.
    if (humanSeats.some((seat) => !submitted.has(seat))) return "Deckbuilding";
    // session.rs:901-905 — the last-deck arm's two-way split.
    if (opts.postDraftPlay === "CompleteImmediately") return "Complete";
    // session.rs:903 then :254.
    return pairingsGenerated ? "MatchInProgress" : "Pairing";
  }

  function viewFor(seat: number) {
    return {
      status: statusNow(),
      kind: "CommanderDraft",
      seat_index: seat,
      current_round: 1,
      pairings: [],
      pool: Array.from({ length: opts.poolSize ?? 0 }, (_, i) => ({
        card_instance_id: `c${i}`,
      })),
      seats: opts.seats.map((s) => ({
        seat_index: s.seat,
        is_bot: s.isBot,
        has_submitted_deck: submitted.has(s.seat),
      })),
    };
  }

  return {
    submitDeckForSeat: vi.fn(async (seat: number) => {
      submitted.add(seat);
      return viewFor(seat);
    }),
    getViewForSeat: vi.fn(async (seat: number) => viewFor(seat)),
    generatePairings: vi.fn(async () => {
      // apply_generate_pairings' guard, session.rs:214-217.
      const status = statusNow();
      if (
        status !== "Deckbuilding" &&
        status !== "Pairing" &&
        status !== "RoundComplete"
      ) {
        throw new Error(
          `InvalidTransition { from: ${status}, action: "GeneratePairings" }`,
        );
      }
      pairingsGenerated = true;
      return { current_round: 1, pairings: [] };
    }),
  } as unknown as Record<string, ReturnType<typeof vi.fn>>;
}

/** Seed a guest session for `seat` — without it every send is a silent no-op. */
function seatGuestSession(privateHost: PrivateHost, seat: number) {
  const send = vi.fn();
  privateHost.guestSessions.set(seat, { send });
  return send;
}

type RecordedEvent = { type: string; view?: { status?: string } };

function recordEvents(host: P2PDraftHost): RecordedEvent[] {
  const events: RecordedEvent[] = [];
  host.onEvent((event) => {
    events.push(event as RecordedEvent);
  });
  return events;
}

describe("P2PDraftHost — the pod goes where the reducer says it went", () => {
  /**
   * ROW 1. Host sequence, `CompleteImmediately` pod.
   *
   * REVERT-PROBE: restore `p2p-draft-host.ts:826` to the bare
   * `await this.generatePairings();`. Then `adapter.generatePairings()` throws
   * (the reducer refuses a `Complete` session), `broadcastViews()` at `:1124`
   * is never reached, NO `viewUpdated` is emitted at all, and an `error` is.
   * Both asserted values flip.
   */
  it("emits a viewUpdated carrying Complete, and no error, for a CompleteImmediately pod", async () => {
    const host = newHost("CommanderDraft");
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    privateHost.paused = false;
    privateHost.adapter = twoClockStub({
      postDraftPlay: "CompleteImmediately",
      seats: [
        { seat: 0, isBot: false },
        { seat: 1, isBot: true },
        { seat: 2, isBot: true },
        { seat: 3, isBot: true },
      ],
    });
    const events = recordEvents(host);

    await host.submitHostDeck(["Plains"], ["Human Legend"]);

    // Paired positive reach-guard: the funnel RAN. `handleDeckSubmission` emits
    // one `deckSubmitted` per submission, so a harness that never submits
    // records zero and "no error" below would be vacuously true.
    expect(events.filter((e) => e.type === "deckSubmitted")).toHaveLength(1);
    expect(events.some((e) => e.type === "allDecksSubmitted")).toBe(true);

    // PRIMARY (REVERT-FAILING). `broadcastViews` swallows its own throws — both
    // its per-seat `catch` and its outer best-effort `catch` — so a
    // broadcast-internal failure yields NEITHER a `viewUpdated` NOR an `error`,
    // which is why the positive assertion is the primary one and "no error"
    // cannot stand alone.
    expect(
      events.filter((e) => e.type === "viewUpdated" && e.view?.status === "Complete"),
    ).not.toHaveLength(0);
    // SECONDARY (also revert-failing).
    expect(events.filter((e) => e.type === "error")).toHaveLength(0);
  });

  /**
   * ROW 1's hostile sibling — the paired negative for clock (a). It proves the
   * accumulating set actually gates, rather than the stub returning `Complete`
   * unconditionally.
   *
   * First production branch it reaches: `handleDeckSubmission`'s all-submitted
   * gate, `hostView.seats.every(...)`.
   */
  it("does not fire the funnel while a human seat is still outstanding", async () => {
    const host = newHost("CommanderDraft");
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    privateHost.paused = false;
    privateHost.adapter = twoClockStub({
      postDraftPlay: "CompleteImmediately",
      seats: [
        { seat: 0, isBot: false },
        { seat: 1, isBot: false },
        { seat: 2, isBot: true },
      ],
    });
    const events = recordEvents(host);

    await host.submitHostDeck(["Plains"], ["Human Legend"]);

    // Reach guard: seat 0's submission really happened.
    expect(events.filter((e) => e.type === "deckSubmitted")).toHaveLength(1);
    expect(events.some((e) => e.type === "allDecksSubmitted")).toBe(false);
    expect(events.some((e) => e.type === "viewUpdated")).toBe(false);
  });

  /**
   * ROW 2. Guest fan-out — >=3 seats of which >=2 are human.
   *
   * Seat 0 is bot-projected deliberately: it is the host seat, so it has no
   * `guestSessions` entry and cannot be driven through `handleGuestMessage`.
   * Projected human and never submitting, the all-submitted gate's `every(...)`
   * would never be true and this row would red through its reach-guard rather
   * than through its discriminator. With seat 0 bot-projected the human set is
   * {1, 2}.
   *
   * The assertion names seat 1 — a NON-last submitter. The last submitter
   * receives its own `draft_deck_submit_ack` regardless, already carrying
   * `Complete`, so asserting on it would PASS UNDER THE DEFECT.
   *
   * REVERT-PROBE: drop `broadcastViews()` from `publishAcceptedDeckSubmission`'s
   * `case "Complete":` arm. It never runs, so seat 1's only view-carrying
   * message is its own receipt ack — carrying `Deckbuilding`.
   */
  it("sends the Complete view to a human seat that was not the last submitter", async () => {
    const host = newHost("CommanderDraft");
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    privateHost.paused = false;
    privateHost.adapter = twoClockStub({
      postDraftPlay: "CompleteImmediately",
      seats: [
        { seat: 0, isBot: true },
        { seat: 1, isBot: false },
        { seat: 2, isBot: false },
      ],
    });
    const seat1Send = seatGuestSession(privateHost, 1);
    seatGuestSession(privateHost, 2);

    await privateHost.handleGuestMessage(1, {
      type: "draft_submit_deck",
      submissionId: "submission-seat-1",
      mainDeck: ["Plains"],
      commanders: ["Legend One"],
    });
    await privateHost.handleGuestMessage(2, {
      type: "draft_submit_deck",
      submissionId: "submission-seat-2",
      mainDeck: ["Island"],
      commanders: ["Legend Two"],
    });

    const seat1Statuses = seat1Send.mock.calls
      .map((call) => call[0] as { type: string; view?: { status?: string } })
      .filter((msg) => msg.type === "draft_state_update" || msg.type === "draft_deck_submit_ack")
      .map((msg) => msg.view?.status);

    // Paired positive reach-guard #1: seat 1's session was reached at all.
    // First production branch it reaches: `handleDeckSubmission`'s
    // `if (session)` guard around the `draft_state_update` send.
    expect(seat1Statuses.length).toBeGreaterThan(0);
    // Reach-guard #2, specific to clock (a) — TRUE ON BOTH TREES. This is what
    // catches an all-pre-submitted `seats` fixture, under which seat 1's own
    // receipt ack would already carry `Complete` and the row would red for the
    // wrong reason.
    expect(seat1Statuses[0]).toBe("Deckbuilding");

    // REVERT-FAILING: `"Deckbuilding"` on the unfixed tree.
    expect(seat1Statuses[seat1Statuses.length - 1]).toBe("Complete");
  });

  /**
   * ROW 3b. Premier hostile sibling, driven through the production route.
   *
   * The word that reaches the defect is FIRST. A bare ordering assertion
   * ("viewUpdated precedes pairingsGenerated") is SATISFIED AT BASE — `:1124`'s
   * `broadcastViews()` already emits at `:856` before `:1126` — and cannot
   * fail. "The first `viewUpdated` after `allDecksSubmitted`" is well defined
   * because at base no `viewUpdated` is emitted between `:825` and the
   * `generatePairings()` call: the funnel's next statement after the emit IS
   * the call.
   *
   * REVERT-PROBE 1: restore `:826` to the bare `await this.generatePairings();`
   * REVERT-PROBE 2: drop `broadcastViews()` from the `case "Pairing":` arm.
   * Either way `adapter.generatePairings()` runs first, flipping clock (b), and
   * the first `viewUpdated` carries `MatchInProgress`. Nothing else republishes
   * `Pairing` — `apply_generate_pairings` overwrites it (`session.rs:254`), and
   * its only two production producers are `:599` and `:903`.
   */
  it("republishes Pairing before generating, for a TournamentPairings pod", async () => {
    const host = newHost("Premier");
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    privateHost.paused = false;
    privateHost.adapter = twoClockStub({
      postDraftPlay: "TournamentPairings",
      seats: [
        { seat: 0, isBot: false },
        { seat: 1, isBot: true },
        { seat: 2, isBot: true },
        { seat: 3, isBot: true },
      ],
    });
    const events = recordEvents(host);

    await host.submitHostDeck(["Plains"], ["Human Legend"]);

    const types = events.map((e) => e.type);
    const allDecksAt = types.indexOf("allDecksSubmitted");
    // Paired positive reach-guard: the funnel ran to completion rather than
    // throwing, and there is at least one `viewUpdated` after the marker, so
    // "the first one" is not an assertion over an empty list.
    expect(allDecksAt).toBeGreaterThanOrEqual(0);
    expect(types.indexOf("pairingsGenerated")).toBeGreaterThan(allDecksAt);
    expect(events.filter((e) => e.type === "error")).toHaveLength(0);

    const after = events.slice(allDecksAt + 1);
    const firstViewUpdated = after.find((e) => e.type === "viewUpdated");
    expect(firstViewUpdated).toBeDefined();

    // REVERT-FAILING: `"MatchInProgress"` on the unfixed tree. Because the
    // host's `viewUpdated` is the TAIL of `broadcastViews()` and not an
    // independent channel, this also witnesses that the guest fan-out ran on
    // the tournament branch.
    expect(firstViewUpdated?.view?.status).toBe("Pairing");
  });
});
