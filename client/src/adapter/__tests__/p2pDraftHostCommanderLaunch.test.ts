import { describe, expect, it, vi } from "vitest";

import { P2PDraftHost, type DraftHostEvent } from "../p2p-draft-host";
import type { DraftCommanderLaunch, DraftP2PMessage } from "../../network/draftProtocol";

/**
 * The host side of a completed Commander pod's launch into ONE shared N-seat
 * game (CR 903.13a: "a draft ... followed by a multiplayer game").
 *
 * Three units live here:
 *   - `commanderSeatPlan`, the SINGLE authority for which pod seats a human
 *     will pilot (`!is_bot && connected`) and which the engine must pilot;
 *   - `commanderSeatDecks`, the SINGLE authority for every deck the launch
 *     needs, which sends nothing; and
 *   - `sendCommanderLaunches`, which puts one `draft_commander_launch` on each
 *     live human seat, from those decks alone.
 *
 * A test that asserts only on the returned decks drives `commanderSeatDecks`
 * alone. The moment an assertion reads a SENT launch or an EMITTED
 * `commanderLaunch`, that test drives the pure function and THEN the sender.
 *
 * Every test drives the REAL assembler against a STUBBED draft adapter — never
 * a mocked host — because a mocked host would make each assertion a restatement
 * of the mock.
 *
 * This file also HOLDS the four coverage axes ported from
 * `p2pDraftSubmitDeck.test.ts`'s `podCommanderDeckPayload` suite, which step
 * 3a-ii deletes: CR 903.3 designation carry-through (with its REVERT-PROBE),
 * `draftSetCodes` populated and absent, draft-wasm refusal propagation, and the
 * throw when the local seat has no submitted deck.
 *
 * Modelled on `p2pDraftSubmitDeck.test.ts`, including its `asPrivate(host)`
 * cast precedent and its `REVERT-PROBE:` convention: a row names the exact line
 * whose reversion it catches.
 */

function newHost() {
  return new P2PDraftHost(
    { id: "host" } as never,
    () => () => {},
    { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } } as never,
    "CommanderDraft",
    4,
    "Host",
    "Swiss",
    "Competitive",
  );
}

type PrivateHost = {
  guestSessions: Map<number, { send: ReturnType<typeof vi.fn> }>;
  adapter: Record<string, ReturnType<typeof vi.fn>>;
  commanderSeatPlan: (view: unknown) => { liveHumanSeats: number[]; engineSeats: number[] };
};

function asPrivate(host: P2PDraftHost): PrivateHost {
  return host as unknown as PrivateHost;
}

/** Seed a guest session for `seat` — without it, every send is a silent no-op. */
function seatGuestSession(privateHost: PrivateHost, seat: number) {
  const send = vi.fn();
  privateHost.guestSessions.set(seat, { send });
  return send;
}

/**
 * The ONE fixture for this file. `connected` is set EXPLICITLY on every seat.
 *
 * That is load bearing rather than tidy: the source fixture this file ports
 * from omitted the key entirely, and under `!is_bot && connected` an
 * `undefined` is falsy — so EVERY seat, host included, would classify
 * engine-piloted, `liveHumanSeats` would be `[]`, and the ported axes would go
 * green while exercising a state `commanderSeatPlan` itself says cannot occur
 * in production (the host is always live).
 *
 * Default shape: seat 0 live human host, seat 1 bot, seat 2 live human guest,
 * seat 3 a human who dropped before the launch.
 */
function commanderPodView(
  overrides: {
    seats?: Array<{ is_bot: boolean; connected: boolean }>;
    draftSetCodes?: string[];
  } = {},
) {
  const seats = overrides.seats ?? [
    { is_bot: false, connected: true },
    { is_bot: true, connected: true },
    { is_bot: false, connected: true },
    { is_bot: false, connected: false },
  ];
  return {
    kind: "CommanderDraft",
    status: "Complete",
    draft_set_codes: overrides.draftSetCodes,
    seats: seats.map((seat, i) => ({ seat_index: i, ...seat })),
  } as never;
}

/**
 * `exportSession` returns the JSON `exportDraftSession` parses.
 *
 * Seats 0, 2 and 3 submit — seat 0 and seat 2 because they are live humans that
 * must each receive their OWN deck, seat 3 because a dropped human resolves
 * through `submittedDeckForSeat` rather than `botDeckForSeat`. Each submission
 * carries its own designation, and each pool holds one card its main deck does
 * not, so `sideboardFromPool` has something to produce and an empty sideboard
 * cannot pass as "the pool was read".
 */
function launchAdapter(botCommander = "Bot Legend") {
  return {
    exportSession: vi.fn(async () =>
      JSON.stringify({
        pools: [
          [{ name: "Human Legend" }, { name: "Spare Card" }],
          [{ name: "Bot Legend" }],
          [{ name: "Guest Legend" }, { name: "Guest Spare" }],
          [{ name: "Dropped Legend" }, { name: "Dropped Spare" }],
        ],
        submitted_decks: {
          "0": { seat: 0, main_deck: ["Human Legend", "Plains"], commanders: ["Human Legend"] },
          "2": { seat: 2, main_deck: ["Guest Legend", "Island"], commanders: ["Guest Legend"] },
          "3": {
            seat: 3,
            main_deck: ["Dropped Legend", "Swamp"],
            commanders: ["Dropped Legend"],
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

/** Every `draft_commander_launch` a mocked guest session received. */
function launchesOn(send: ReturnType<typeof vi.fn>): DraftCommanderLaunch[] {
  return send.mock.calls
    .map(([msg]) => msg as DraftP2PMessage)
    .filter((msg) => msg.type === "draft_commander_launch")
    .map((msg) => (msg as Extract<DraftP2PMessage, { type: "draft_commander_launch" }>).launch);
}

describe("P2PDraftHost.commanderSeatPlan", () => {
  /**
   * The classification's whole discriminating power lives in `is_bot`:
   * draft-core reports `connected` as `true` UNCONDITIONALLY for a bot seat
   * (`DraftSeat::Bot => true`), so a `connected`-only test would place seat 1
   * in `liveHumanSeats` alongside the real humans.
   *
   * REVERT-PROBE: drop the `!this.isBotSeatFromView(...)` conjunct from
   * `commanderSeatPlan` and seat 1 moves to `liveHumanSeats`, reddening both
   * lists at once.
   */
  it("partitions a bot, a dropped human and a live human into the right lists", () => {
    const host = newHost();

    const plan = asPrivate(host).commanderSeatPlan(commanderPodView());

    expect(plan.liveHumanSeats).toEqual([0, 2]);
    expect(plan.engineSeats).toEqual([1, 3]);
  });

  /**
   * Paired counter-fixture. The `is_bot: true, connected: false` seat is a
   * SYNTHETIC shape the engine never emits — `view.rs` reports `Bot => true`
   * for `connected` unconditionally. It is used deliberately: pairing it with a
   * live human at a higher index swaps which seats land in which list relative
   * to the row above, so neither list can be passing on seat INDEX or ordering,
   * and a hardcoded "seat 0 is always live" fails here.
   */
  it("classifies on the seat's own flags rather than its index", () => {
    const host = newHost();

    const plan = asPrivate(host).commanderSeatPlan(
      commanderPodView({
        seats: [
          { is_bot: true, connected: false },
          { is_bot: false, connected: true },
        ],
      }),
    );

    expect(plan.liveHumanSeats).toEqual([1]);
    expect(plan.engineSeats).toEqual([0]);
  });
});

describe("P2PDraftHost.commanderSeatDecks + sendCommanderLaunches", () => {
  /**
   * The production path: the host is ALWAYS pod seat 0, so its own launch has
   * to come back through `sendToSeat`'s seat-0 arm as a local event.
   *
   * REVERT-PROBE: remove the `case "draft_commander_launch"` arm from
   * `sendToSeat`'s seat-0 `switch`. That switch ends in `default: break`, so
   * seat 0's launch is silently dropped — neither emitted nor sent — and the
   * `commanderLaunch` expectation below reds while everything else stays green.
   *
   * The second half is a CONSERVATION check, not "no wire send happened for
   * seat 0": seat 0 never has a `guestSessions` entry, so a not-called
   * assertion there would be vacuous. Instead the TOTAL send count across every
   * seeded session must equal `liveHumanSeats.length - 1`, and each session must
   * have received its OWN seat's deck. Together those leave seat 0's launch no
   * wire to have travelled on.
   */
  it("emits the host's own launch locally and sends one per other live human seat", async () => {
    const host = newHost();
    const privateHost = asPrivate(host);
    const adapter = launchAdapter();
    privateHost.adapter = adapter;
    // Seat 3 dropped, but it still has a seeded session: the conservation count
    // is only meaningful if an engine-piloted seat COULD have been sent to.
    const seat2Send = seatGuestSession(privateHost, 2);
    const seat3Send = seatGuestSession(privateHost, 3);
    const events: DraftHostEvent[] = [];
    host.onEvent((event) => events.push(event));

    const view = commanderPodView();
    const decks = await host.commanderSeatDecks(view, 0);
    host.sendCommanderLaunches(view, "game-1", "room-1", decks);
    const hostDeck = decks.hostDeck;

    // The DISCRIMINATING exactly-once assertion. Its twin on the pure test
    // drives only `commanderSeatDecks`, so a sender that re-exported the
    // session would still pass there; here both calls have run.
    expect(adapter.exportSession).toHaveBeenCalledTimes(1);

    const emitted = events.filter((e) => e.type === "commanderLaunch");
    expect(emitted).toHaveLength(1);
    const hostLaunch = (emitted[0] as Extract<DraftHostEvent, { type: "commanderLaunch" }>).launch;
    expect(hostLaunch.gameId).toBe("game-1");
    expect(hostLaunch.roomCode).toBe("room-1");
    // Every pod seat is a game player, humans and engine-piloted alike.
    expect(hostLaunch.playerCount).toBe(4);
    // The host's own message carries the SAME deck object the caller is handed,
    // which is what "synthesized exactly once" means for the local seat.
    expect(hostLaunch.localDeck).toBe(hostDeck);

    // Conservation: two live human seats, so exactly ONE wire send in total
    // across every seeded session. Seat 0 has no session of its own, so a
    // not-called assertion there would be vacuous; this leaves seat 0's launch
    // no wire it could have travelled on instead.
    const seat2Launches = launchesOn(seat2Send);
    const seat3Launches = launchesOn(seat3Send);
    expect(seat2Launches.length + seat3Launches.length).toBe(1);
    // ...and it went to seat 2, carrying seat 2's OWN deck.
    expect(seat2Launches).toHaveLength(1);
    expect(seat2Launches[0].localDeck.commander).toEqual(["Guest Legend"]);
    // The dropped human is engine-piloted: it is not a recipient.
    expect(seat3Launches).toHaveLength(0);
  });

  /**
   * `localSeat` is a PARAMETER, not a hardcoded 0.
   *
   * The recipient set does not vary with `localSeat` — it is exactly
   * `liveHumanSeats`. What `localSeat` selects is which seat's deck becomes
   * `hostDeck`. At a non-zero `localSeat` that seat is reached over the wire
   * like any other live seat, so this row asserts the wire delivery rather than
   * a local emit; the seat-0 row above covers the emit.
   *
   * REVERT-PROBE: replace `localSeat` with a literal `0` in
   * `commanderSeatDecks` and `hostDeck` becomes seat 0's deck here.
   */
  it("takes hostDeck from the passed localSeat and still sends that seat its launch", async () => {
    const host = newHost();
    const privateHost = asPrivate(host);
    privateHost.adapter = launchAdapter();
    const seat2Send = seatGuestSession(privateHost, 2);
    const events: DraftHostEvent[] = [];
    host.onEvent((event) => events.push(event));

    const view = commanderPodView();
    const decks = await host.commanderSeatDecks(view, 2);
    host.sendCommanderLaunches(view, "game-2", "room-2", decks);
    const hostDeck = decks.hostDeck;

    // The pure half of the identity mandate: `localSeat`'s entry in
    // `liveSeatDecks` is the SAME OBJECT as `hostDeck`, never a re-synthesis.
    // Its wire-side twin is the `toBe(hostDeck)` on seat 2's sent launch below.
    expect(decks.liveSeatDecks.find((entry) => entry.seat === 2)?.deck).toBe(hostDeck);

    // Reach guard before any designation claim.
    expect(hostDeck.main_deck.length).toBeGreaterThan(0);
    expect(hostDeck.commander).toEqual(["Guest Legend"]);
    // Seat 2's own deck OBJECT, not a second synthesis of it. `getBotDeck` call
    // counts cannot catch a double-synthesis here, because
    // `submittedDeckForSeat` is synchronous and un-mocked.
    const seat2Launches = launchesOn(seat2Send);
    expect(seat2Launches).toHaveLength(1);
    expect(seat2Launches[0].localDeck).toBe(hostDeck);
    // Seat 0 is still a live human, so it still receives its own launch — the
    // recipient set is `liveHumanSeats`, independent of `localSeat`.
    const emitted = events.filter((e) => e.type === "commanderLaunch");
    expect(emitted).toHaveLength(1);
    expect(
      (emitted[0] as Extract<DraftHostEvent, { type: "commanderLaunch" }>).launch.localDeck
        .commander,
    ).toEqual(["Human Legend"]);
  });

  /**
   * The engine-piloted half of the plan, and the deck-synthesis budget.
   *
   * A bot seat resolves through `botDeckForSeat`; a human who dropped before
   * the launch resolves through `submittedDeckForSeat` — its drafted deck is
   * real and already submitted, so botting it would discard what it built.
   * The return is SEAT-KEYED so the caller can map each deck to a game player
   * deterministically.
   */
  it("resolves bots through botDeckForSeat and dropped humans through their submission", async () => {
    const host = newHost();
    const privateHost = asPrivate(host);
    const adapter = launchAdapter();
    privateHost.adapter = adapter;

    const { engineSeatDecks } = await host.commanderSeatDecks(commanderPodView(), 0);

    expect(engineSeatDecks.map((entry) => entry.seat)).toEqual([1, 3]);
    // The bot's designation is a member of its main deck (CR 903.5a), and
    // `botDeckForSeat` flattens `lands` into it, so no name is added or lost.
    expect(engineSeatDecks[0].deck.commander).toEqual(["Bot Legend"]);
    expect(engineSeatDecks[0].deck.main_deck).toEqual([
      "Bot Legend",
      "Bot Spell",
      "Plains",
      "Plains",
    ]);
    // The dropped human keeps its OWN drafted deck, designation included.
    expect(engineSeatDecks[1].deck.commander).toEqual(["Dropped Legend"]);
    expect(engineSeatDecks[1].deck.sideboard).toEqual(["Dropped Spare"]);
    // Exactly one bot seat, so exactly one bot deck synthesis; and the session
    // is exported ONCE for the whole computation, not per seat. This row drives
    // ONLY the pure function, so it cannot see a sender that re-exported —
    // its twin in the seat-0 send row above is the one that discriminates that.
    expect(adapter.getBotDeck.mock.calls.map((c) => c[0])).toEqual([1]);
    expect(adapter.exportSession).toHaveBeenCalledTimes(1);
  });

  /**
   * The seat count comes from the VIEW, not from a constant.
   *
   * Every other row here takes `commanderPodView`'s default 4-seat pod, so an
   * assembler that read a hardcoded four would satisfy all of them. This pod
   * seats FIVE and its engine half is `[1, 3, 4]` — a length and a membership a
   * 4-seat assembler cannot produce, since it would stop at `[1, 3]`. Both
   * assertions are load bearing: `toEqual` catches a truncated membership,
   * `toHaveLength` catches a count that ignores the view.
   *
   * Seats 0 and 2 stay the live humans because `submittedDeckForSeat` throws
   * for a seat with no submission and the fixture submits only 0, 2 and 3; the
   * pod therefore grows by a BOT, whose seat-4 pool is absent so
   * `sideboardFromPool` returns `[]` through its own `?? []`.
   *
   * `commanderSeatDecks` delegates its partition to `commanderSeatPlan`, so a
   * hardcoded count is structurally impossible today. This row is what keeps
   * that true if the delegation is ever inlined.
   */
  it("reads the pod's own seat count rather than a fixed four", async () => {
    const host = newHost();
    const privateHost = asPrivate(host);
    const adapter = launchAdapter();
    privateHost.adapter = adapter;

    const { engineSeatDecks } = await host.commanderSeatDecks(
      commanderPodView({
        seats: [
          { is_bot: false, connected: true },
          { is_bot: true, connected: true },
          { is_bot: false, connected: true },
          { is_bot: false, connected: false },
          { is_bot: true, connected: true },
        ],
      }),
      0,
    );

    expect(engineSeatDecks.map((entry) => entry.seat)).toEqual([1, 3, 4]);
    expect(engineSeatDecks).toHaveLength(3);
    // The fifth seat is REACHED, not merely listed: it is bot-synthesized, and
    // its absent pool leaves an empty sideboard rather than borrowing seat 3's.
    expect(adapter.getBotDeck.mock.calls.map((c) => c[0])).toEqual([1, 4]);
    expect(engineSeatDecks[2].deck.sideboard).toEqual([]);
  });

  // ── PORTED AXIS 1 — CR 903.3 designation carry-through ────────────────
  /**
   * CR 903.3: "Each deck has a legendary card designated as its commander."
   * Every seat's designation must survive the assembly, human and bot alike.
   *
   * REVERT-PROBE: `deckPayload`'s `commander` parameter — restore the hardcoded
   * `commander: []` it replaced and every designation assertion below fails
   * while the seat-partition assertions elsewhere still pass, so the two axes
   * are independently guarded.
   */
  it("carries each seat's OWN commander, human and bot", async () => {
    const host = newHost();
    const privateHost = asPrivate(host);
    privateHost.adapter = launchAdapter();
    const seat2Send = seatGuestSession(privateHost, 2);

    const view = commanderPodView();
    const decks = await host.commanderSeatDecks(view, 0);
    // Seat 2's designation is observable only on its SENT launch, so this row
    // spans the seam and must drive the sender too.
    host.sendCommanderLaunches(view, "game-4", "room-4", decks);
    const { hostDeck, engineSeatDecks } = decks;

    // Reach guard before any designation claim: the assembler really built a
    // deck, so an empty designation below would be a real absence.
    expect(hostDeck.main_deck.length).toBeGreaterThan(0);
    // REVERT-FAILING under a hardcoded `commander: []`.
    expect(hostDeck.commander).toEqual(["Human Legend"]);
    expect(engineSeatDecks[0].deck.commander).toEqual(["Bot Legend"]);
    expect(launchesOn(seat2Send)[0].localDeck.commander).toEqual(["Guest Legend"]);
    // Three seats with DIFFERENT designations, so "they differ" cannot pass on
    // three empties.
    expect(hostDeck.commander).not.toEqual(engineSeatDecks[0].deck.commander);
    // The human's sideboard is pool-minus-maindeck, proving the session's pools
    // were read rather than an empty default returned.
    expect(hostDeck.sideboard).toEqual(["Spare Card"]);
  });

  // ── PORTED AXIS 2 — draftSetCodes, populated and absent ───────────────
  /**
   * CR 903.13f(3): a draft that contained Commander Masters boosters grants the
   * partner ability, for deckbuilding purposes, to any card that can be a
   * commander by itself whose color identity is one or fewer colors. The engine
   * decides that from the deck's draft set codes, and this is the client hop
   * that supplies them — read off the SAME view the assembler builds decks from.
   *
   * The MIXED half is what makes the plural load-bearing: a CMM+CLB draft
   * contained Commander Masters, so the grant is in force, and a host that
   * forwarded one representative code would drop whichever set it did not pick.
   *
   * REVERT-PROBE: drop `draftSetCodes: view.draft_set_codes ?? null` from the
   * launch and the message no longer validates on arrival at all
   * (`undefined` is dropped by `JSON.stringify`, and step 1's validator rejects
   * a missing `draftSetCodes`); here it reads `undefined`.
   */
  it("carries every one of the view's draft set codes onto the launch", async () => {
    const host = newHost();
    asPrivate(host).adapter = launchAdapter();
    const events: DraftHostEvent[] = [];
    host.onEvent((event) => events.push(event));

    // `draftSetCodes` lives ONLY on the sent launch — `CommanderSeatDecks` has
    // no such field and must not grow one — so this axis spans the seam.
    const view = commanderPodView({ draftSetCodes: ["CMM", "CLB"] });
    host.sendCommanderLaunches(
      view,
      "game-5",
      "room-5",
      await host.commanderSeatDecks(view, 0),
    );

    const launch = (
      events.find((e) => e.type === "commanderLaunch") as Extract<
        DraftHostEvent,
        { type: "commanderLaunch" }
      >
    ).launch;
    // Reach guard: the assembler ran, so an absent set code would be a real
    // absence rather than a dead harness.
    expect(launch.localDeck.commander).toEqual(["Human Legend"]);
    expect(launch.draftSetCodes).toEqual(["CMM", "CLB"]);
  });

  /**
   * Paired negative: the host forwards the view's value rather than
   * manufacturing a set code, so a constructed-shaped view stays grantless.
   *
   * `null`, not `[]`: the wire field is required-nullable while the view's is
   * optional, so `null` is this contract's declared "no sets" value. That is a
   * vocabulary choice, NOT a rules one — the engine reads `null`, `undefined`
   * and `[]` identically as the empty array.
   */
  it("spells an absent set list as null rather than an empty array", async () => {
    const host = newHost();
    asPrivate(host).adapter = launchAdapter();
    const events: DraftHostEvent[] = [];
    host.onEvent((event) => events.push(event));

    const view = commanderPodView();
    host.sendCommanderLaunches(view, "game-6", "room-6", await host.commanderSeatDecks(view, 0));

    const launch = (
      events.find((e) => e.type === "commanderLaunch") as Extract<
        DraftHostEvent,
        { type: "commanderLaunch" }
      >
    ).launch;
    expect(launch.localDeck.commander).toEqual(["Human Legend"]);
    expect(launch.draftSetCodes).toBeNull();
  });

  // ── PORTED AXIS 3 — draft-wasm refusal propagation ────────────────────
  it("propagates a draft-wasm refusal rather than shipping an unjudged deck", async () => {
    const host = newHost();
    asPrivate(host).adapter = {
      ...launchAdapter(),
      getBotDeck: vi.fn(async () => {
        throw new Error("Card database must be loaded before a Commander Draft bot deck");
      }),
    } as unknown as Record<string, ReturnType<typeof vi.fn>>;

    await expect(
      host.commanderSeatDecks(commanderPodView(), 0),
    ).rejects.toThrow("Card database");
  });

  // ── PORTED AXIS 4 — the local seat has no submitted deck ──────────────
  /**
   * The existing `submittedDeckForSeat` throw, not a new error path. The local
   * seat's deck is computed FIRST, before any other seat's, so this is the
   * error that surfaces even though live seat 2 also submits nothing here.
   */
  it("throws when the local seat has no submitted deck", async () => {
    const host = newHost();
    asPrivate(host).adapter = {
      ...launchAdapter(),
      exportSession: vi.fn(async () => JSON.stringify({ pools: [[]], submitted_decks: {} })),
    } as unknown as Record<string, ReturnType<typeof vi.fn>>;

    await expect(
      host.commanderSeatDecks(commanderPodView(), 0),
    ).rejects.toThrow("Seat 0 has no submitted deck");
  });
});
