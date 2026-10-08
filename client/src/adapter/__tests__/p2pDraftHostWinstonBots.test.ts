import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { clearDraftHostSession, saveDraftHostSession } = vi.hoisted(() => ({
  clearDraftHostSession: vi.fn(async () => {}),
  saveDraftHostSession: vi.fn<(id: string, session: unknown) => Promise<void>>(async () => {}),
}));

vi.mock("../../services/draftPersistence", () => ({
  clearDraftHostSession,
  saveDraftHostSession,
}));

import { P2PDraftHost } from "../p2p-draft-host";
import type { DraftPlayerView, MultiplayerSeatDescriptor } from "../draft-adapter";
import type { PersistedDraftHostSession } from "../../services/draftPersistence";
import { draftProcedureFixture } from "./draftProcedureFixture";

/**
 * A Winston pod's BOT seat, from the host's side: the card database it needs to
 * exist at all, and the engine turn-loop the host must run after every human
 * decision.
 *
 * Both halves were unreachable before this change, for the same reason — the
 * reducer refused a bot seat under `PackDistribution::SharedStackPiles`, so the
 * host suppressed bot fill and nothing downstream of it ever ran. Neither half
 * is a re-spelling of the engine's own tests: `draft-wasm` owns whether the
 * loop terminates and what a bot decides, and nothing here asks either
 * question.
 */
describe("P2PDraftHost Winston bot seats", () => {
  const WINSTON_PROCEDURE = draftProcedureFixture({
    pod_size: 2,
    human_seats: 2,
    min_pod_size: 2,
    max_pod_size: 4,
    allowed_pod_sizes: [2, 3, 4],
    distribution: { SharedStackPiles: { pile_count: 3 } },
  });

  const PREMIER_PROCEDURE = draftProcedureFixture({
    pod_size: 8,
    human_seats: 8,
    allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
    distribution: "PickAndPass",
  });

  const originalFetch = globalThis.fetch;
  let fetchMock: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    vi.clearAllMocks();
    fetchMock = vi.fn(async () => new Response("CARD-DATA", { status: 200 }));
    globalThis.fetch = fetchMock as unknown as typeof fetch;
  });

  afterEach(() => {
    globalThis.fetch = originalFetch;
    vi.clearAllTimers();
    vi.useRealTimers();
  });

  // ── The card database ────────────────────────────────────────────────

  function lobbyHost(kind: "Winston" | "Premier", podSize: number) {
    const host = new P2PDraftHost(
      { id: "host" } as never,
      () => () => {},
      { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } } as never,
      kind,
      podSize,
      "Host",
      "Swiss",
      "Competitive",
    );
    const createMultiplayerDraft = vi.fn(
      async (_pool: unknown, _seats: MultiplayerSeatDescriptor[]) => {},
    );
    const loadCardDatabase = vi.fn(async (_json: string) => 0);
    (host as unknown as { adapter: unknown }).adapter = {
      draftProcedure: vi.fn(async () => (kind === "Winston" ? WINSTON_PROCEDURE : PREMIER_PROCEDURE)),
      createMultiplayerDraft,
      loadCardDatabase,
      // `Lobby`, so no bot resolution and no pick timer run: these three cases
      // are about what `startDraftInner` fetches before the draft exists.
      getViewForSeat: vi.fn(async () => ({ status: "Lobby" })),
    };
    return { host, createMultiplayerDraft, loadCardDatabase };
  }

  /**
   * The database is what makes two of the five drafting principles live: the
   * valuation prices mana fixing off `produced_color_count` and cheap
   * interaction off the parsed effect profile, both of which read a `CardFace`.
   * A Set-pool Winston pod loads nothing through `draftPodHostAdapter`'s gate
   * (`poolInput.type === "Cube" || kind === "CommanderDraft"`), so without this
   * the bot would play on rarity priors alone and never say so.
   *
   * REVERT-FAILING: delete the load in `startDraftInner` and both assertions go
   * to zero calls.
   */
  it("loads the card database for a shared-stack pod that seats a bot", async () => {
    const { host, createMultiplayerDraft, loadCardDatabase } = lobbyHost("Winston", 2);
    await host.initialize();
    await host.startDraft(true);

    // Reach-guard: the pod really did seat a bot, so the fetch below is that
    // seat's doing rather than an unconditional download.
    const seats = createMultiplayerDraft.mock.calls[0]![1];
    expect(seats.filter((seat) => seat.type === "Bot")).toHaveLength(1);

    expect(fetchMock).toHaveBeenCalledOnce();
    expect(loadCardDatabase).toHaveBeenCalledWith("CARD-DATA");
    // BEFORE the session exists: `create_multiplayer_draft` deals the piles,
    // and the first bot turn can follow immediately.
    expect(loadCardDatabase.mock.invocationCallOrder[0])
      .toBeLessThan(createMultiplayerDraft.mock.invocationCallOrder[0]);
  });

  /**
   * The paired negative that keeps the fix from becoming a download regression:
   * a human-vs-human Winston pod is the common shape, and it must not pay for a
   * multi-megabyte fetch it would never read.
   *
   * REVERT-FAILING: drop the `seats.some(seat => seat.type === "Bot")` conjunct
   * and this fetches.
   */
  it("fetches nothing for a shared-stack pod with no bot seat", async () => {
    const { host, createMultiplayerDraft, loadCardDatabase } = lobbyHost("Winston", 2);
    await host.initialize();
    await host.startDraft(false);

    // Reach-guard: the start path ran to completion, with the host seat alone.
    expect(createMultiplayerDraft.mock.calls[0]![1]).toEqual([
      { type: "Human", player_id: 0, display_name: "Host" },
    ]);
    expect(fetchMock).not.toHaveBeenCalled();
    expect(loadCardDatabase).not.toHaveBeenCalled();
  });

  /**
   * The distribution half of the same conjunct. A pick-and-pass pod full of
   * bots reads its pool from JSON and needs no database — `draftPodHostAdapter`
   * says so in place, and its landed "skips the CARD_DB fetch for Set pods" row
   * would red if this load were written as a blanket one.
   *
   * REVERT-FAILING: drop the `isSharedStackDistribution(procedure.distribution)`
   * conjunct and this fetches.
   */
  it("fetches nothing for a pick-and-pass pod full of bots", async () => {
    const { host, createMultiplayerDraft, loadCardDatabase } = lobbyHost("Premier", 8);
    await host.initialize();
    await host.startDraft(true);

    // Reach-guard: seven bot seats, and still no fetch.
    expect(createMultiplayerDraft.mock.calls[0]![1]
      .filter((seat) => seat.type === "Bot")).toHaveLength(7);
    expect(fetchMock).not.toHaveBeenCalled();
    expect(loadCardDatabase).not.toHaveBeenCalled();
  });

  // ── The turn loop after a human decision ─────────────────────────────

  /**
   * `active_seat` says whose turn it is. Seat 1 is the BOT when the pod seated
   * one; the human-only variant is the same pod with that flag false, so the
   * two cases differ in exactly the field the host dispatches on.
   */
  function winstonView(activeSeat: number, status: string, seatABot: boolean): DraftPlayerView {
    return {
      status,
      kind: "Winston",
      pool: [],
      current_pack: null,
      required_pick_count: 0,
      draft_effects: [],
      seats: [
        { seat_index: 0, display_name: "Host", is_bot: false, connected: true,
          has_submitted_deck: false, pick_status: "Pending", active_pack_count: 0, drafted_card_count: 0,
          face_up_draft_cards: [] },
        { seat_index: 1, display_name: "Guest", is_bot: seatABot, connected: true,
          has_submitted_deck: false, pick_status: "Pending", active_pack_count: 0, drafted_card_count: 0,
          face_up_draft_cards: [] },
      ],
      pick_number: 0,
      shared_stack: {
        main_stack_remaining: 11,
        total_cards: 20,
        active_seat: activeSeat,
        active_pile: 0,
        piles: [],
        decisions: 5,
        history: [],
        forced_draw: null,
      },
    } as unknown as DraftPlayerView;
  }

  /**
   * Starts a Winston pod with one bot seat and hands back the mocks the two
   * cases below assert on. `resolveSharedStackBotTurns` flips the view the host
   * reads, exactly as the engine would: the turn comes back to the human.
   */
  async function startedPodWithABot(
    finalStatus: "Drafting" | "Deckbuilding",
    { seatABot = true }: { seatABot?: boolean } = {},
  ) {
    vi.useFakeTimers();
    const host = new P2PDraftHost(
      { id: "host" } as never,
      () => () => {},
      { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } } as never,
      "Winston",
      2,
      "Host",
      "Swiss",
      "Competitive",
      undefined,
      "winston-bot-pod",
      "ABCDE",
    );
    let botsHaveRun = false;
    const resolveSharedStackBotTurns = vi.fn(async () => {
      botsHaveRun = true;
      return [{ SharedStackDecisionApplied: { seat: 1 } }];
    });
    const getViewForSeat = vi.fn(async () =>
      (botsHaveRun ? winstonView(0, finalStatus, seatABot) : winstonView(1, "Drafting", seatABot)));
    const submitSharedStackDecisionForSeat = vi.fn(
      async () => winstonView(1, "Drafting", seatABot));
    const adapter: Record<string, ReturnType<typeof vi.fn>> = {
      draftProcedure: vi.fn(async () => WINSTON_PROCEDURE),
      createMultiplayerDraft: vi.fn(async () => {}),
      loadCardDatabase: vi.fn(async () => 0),
      exportSession: vi.fn(async () => "{}"),
      allPicksSubmitted: vi.fn(async () => false),
      replaceSeatWithBot: vi.fn(async () => winstonView(0, "Drafting", true)),
      getViewForSeat,
      submitSharedStackDecisionForSeat,
      resolveSharedStackBotTurns,
    };
    (host as unknown as { adapter: unknown }).adapter = adapter;
    await host.initialize();
    await host.startDraft(seatABot);
    // The start path resolves the first bot turns too; reset so the assertions
    // below read the DECISION path alone.
    botsHaveRun = false;
    resolveSharedStackBotTurns.mockClear();
    saveDraftHostSession.mockClear();
    return {
      host,
      resolveSharedStackBotTurns,
      submitSharedStackDecisionForSeat,
      privateAdapter: adapter,
      privateHost: host as unknown as {
        timerContext: string | null;
        timerInterval: ReturnType<typeof setInterval> | null;
      },
    };
  }

  /**
   * The seam this phase opens: a human decision passes the turn to a bot, and
   * the host must hand that turn to the engine's own loop and make the result
   * durable before anyone sees it.
   *
   * REVERT-FAILING: delete the `resolveBotPicks` call from
   * `handleSharedStackDecision` and the bot never moves (0 calls), leaving the
   * pod stalled on a seat with no player in it.
   */
  it("runs the engine's bot turn loop after an applied decision, and fences it", async () => {
    const { host, resolveSharedStackBotTurns, submitSharedStackDecisionForSeat, privateHost } =
      await startedPodWithABot("Drafting");

    await host.submitHostSharedStackDecision(0, "Take");

    // Reach-guard: the human's decision really reached the reducer, so the bot
    // work below follows an applied decision rather than an empty call.
    expect(submitSharedStackDecisionForSeat).toHaveBeenCalledWith(0, 0, "Take");
    expect(resolveSharedStackBotTurns).toHaveBeenCalledOnce();
    // Two fences: the human's decision, then the bot turns' own.
    expect(saveDraftHostSession).toHaveBeenCalledTimes(2);
    // The clock is re-armed for the seat the bot chain stopped on.
    expect(privateHost.timerContext).toBe("pick");
    expect(privateHost.timerInterval).not.toBeNull();
  });

  /**
   * The paired negative for the loop, and the guard on the `is_bot` pre-check:
   * a human-vs-human Winston pod — the common shape — makes NO engine
   * round-trip on any decision. The export would answer honestly (an empty
   * list when the active seat is human), so this is an economy rather than a
   * legality test, and it is the reason `p2pDraftHostWinstonTimer.test.ts`
   * still passes with its adapter mock untouched.
   *
   * REVERT-FAILING: delete the `hostView.seats.some(seat => seat.is_bot)`
   * pre-check in `resolveSharedStackBotTurns` and this reds with one call.
   */
  it("makes no bot round-trip for a shared-stack pod of humans", async () => {
    const { host, resolveSharedStackBotTurns, submitSharedStackDecisionForSeat } =
      await startedPodWithABot("Drafting", { seatABot: false });

    await host.submitHostSharedStackDecision(0, "Take");

    // Reach-guard: the decision path really ran, so "not called" is a decision
    // rather than a path this fixture never entered.
    expect(submitSharedStackDecisionForSeat).toHaveBeenCalledWith(0, 0, "Take");
    expect(resolveSharedStackBotTurns).not.toHaveBeenCalled();
  });

  /**
   * The ORDERING discriminator, and the reason the host view is re-read after
   * the bot turns rather than before: a bot chain can end the draft. Read too
   * early and the host arms a pick clock on a finished draft and never reports
   * it complete.
   *
   * REVERT-FAILING: move the `getViewForSeat(0)` re-read above the
   * `resolveBotPicks` call and this reds — the early view still says
   * `Drafting`.
   */
  it("reports a draft the bot turns finished, instead of arming a dead clock", async () => {
    const { host, privateHost } = await startedPodWithABot("Deckbuilding");
    const events: string[] = [];
    host.onEvent((event) => events.push(event.type));

    await host.submitHostSharedStackDecision(0, "Decline");

    expect(events).toContain("draftComplete");
    expect(privateHost.timerInterval).toBeNull();
  });

  // ── The failure boundary around the bot loop ─────────────────────────

  /**
   * A loud bot-loop failure must not be reported as a refused player decision,
   * and must not take the pod's broadcast and clock down with it.
   *
   * By the time `resolveBotPicks` runs, the deciding seat's decision is already
   * applied by the reducer, persisted, and acknowledged. Letting an `Err` out of
   * the loop fall into `handleSharedStackDecision`'s outer `catch` sent that
   * seat `draft_error` — "your decision was refused" — about a decision the
   * engine accepted, and skipped both the broadcast and the clock re-arm, which
   * left the pod with a stale view and no timer and therefore no recovery at
   * all. `resolve_shared_stack_bot_turns` fails loudly BY DESIGN
   * (`the_loop_fails_loudly_rather_than_spinning`), so this is a reachable
   * state rather than a hypothetical.
   *
   * REVERT-FAILING: remove the inner `try`/`catch` around `resolveBotPicks` and
   * every assertion below reds — the call rejects, `draft_error` is sent, and
   * `timerInterval` stays null.
   */
  it("keeps the pod broadcasting and timing when the bot loop fails loudly", async () => {
    const { host, resolveSharedStackBotTurns, privateHost } =
      await startedPodWithABot("Drafting");
    resolveSharedStackBotTurns.mockRejectedValue(
      new Error("shared-stack bot loop exceeded its bound"),
    );
    const guestSend = vi.fn();
    (host as unknown as { guestSessions: Map<number, unknown> })
      .guestSessions.set(1, { send: guestSend });
    const events: { type: string; message?: string }[] = [];
    host.onEvent((event) => events.push(event as { type: string; message?: string }));

    // The deciding seat's own call RESOLVES: its decision was accepted.
    await expect(host.submitHostSharedStackDecision(0, "Take")).resolves.toBeDefined();

    // Reach-guard: the loop really was entered and really did throw.
    expect(resolveSharedStackBotTurns).toHaveBeenCalledOnce();

    // Reported as a host error, once, naming the engine's own message.
    const errors = events.filter((event) => event.type === "error");
    expect(errors).toHaveLength(1);
    expect(errors[0]!.message).toContain("shared-stack bot loop exceeded its bound");

    // NOT reported to the deciding seat as a refusal of its own decision.
    expect(guestSend.mock.calls.map(([msg]) => (msg as { type: string }).type))
      .not.toContain("draft_error");
    // The pod still got its broadcast and its clock.
    expect(guestSend.mock.calls.map(([msg]) => (msg as { type: string }).type))
      .toContain("draft_state_update");
    expect(privateHost.timerContext).toBe("pick");
    expect(privateHost.timerInterval).not.toBeNull();
  });

  /**
   * THE FAILURE PATH OWES THE SAME FENCE AS THE SUCCESS PATH.
   * `drive_shared_stack_bot_turns` mutates the session in place and returns
   * `Err` from wherever it reached, so a failure on a later bot turn leaves the
   * earlier ones APPLIED in the reducer. The caller then emits and broadcasts --
   * publishing a reducer result no snapshot holds. A host reload would rewind
   * every client past decisions they had already seen.
   *
   * REVERT-FAILING: delete the `await this.persistSessionStrict()` from the
   * `catch` in `driveSharedStackBots` and this drops to one persist and reds.
   * The count is the discrimination: the first persist is the human's own
   * applied decision, which happens before the loop is ever entered, so
   * "persisted at all" would pass with the fence gone.
   */
  it("persists before rethrowing when the bot loop fails mid-way", async () => {
    const { host, resolveSharedStackBotTurns } = await startedPodWithABot("Drafting");
    // Non-empty deltas would be the success path; the point here is that the
    // loop got far enough to apply decisions and THEN threw.
    resolveSharedStackBotTurns.mockRejectedValue(
      new Error("shared-stack bot loop exceeded its bound"),
    );
    const events: { type: string }[] = [];
    host.onEvent((event) => events.push(event as { type: string }));

    await expect(host.submitHostSharedStackDecision(0, "Take")).resolves.toBeDefined();

    // Reach guard: the loop really was entered and really did throw.
    expect(resolveSharedStackBotTurns).toHaveBeenCalledOnce();
    expect(events.filter((event) => event.type === "error")).toHaveLength(1);

    // Two persists: the human's applied decision, then the fence on the way out.
    // The success path asserts this same count, so the failure path is held to
    // the same durability bar rather than a weaker one.
    expect(saveDraftHostSession).toHaveBeenCalledTimes(2);
  });

  /**
   * The paired positive that keeps the boundary from swallowing a REAL refusal:
   * an `Err` out of the reducer itself still reaches the deciding seat as
   * `draft_error` and still rejects.
   *
   * REVERT-FAILING: widen the new inner `catch` to cover
   * `submitSharedStackDecisionForSeat` and this reds.
   */
  it("still reports a refused decision to the seat that made it", async () => {
    const { host, submitSharedStackDecisionForSeat, resolveSharedStackBotTurns } =
      await startedPodWithABot("Drafting");
    submitSharedStackDecisionForSeat.mockRejectedValue(new Error("PileNotActive"));
    const guestSend = vi.fn();
    (host as unknown as { guestSessions: Map<number, unknown> })
      .guestSessions.set(1, { send: guestSend });

    await expect(host.submitHostSharedStackDecision(0, "Take")).rejects.toThrow("PileNotActive");

    expect(resolveSharedStackBotTurns).not.toHaveBeenCalled();
    expect(guestSend).not.toHaveBeenCalled();
  });

  // ── A seat that becomes a bot mid-draft ──────────────────────────────

  /**
   * `ReplaceSeatWithBot` is the third way a bot comes to own a shared-stack
   * turn, and it used to persist and broadcast without ever driving it.
   *
   * The reducer accepts this action under `SharedStackPiles` now (the
   * distribution dispatch that refused it was removed with this feature), and
   * the seat it converts may be the ACTIVE one. Nothing else would move it: the
   * reducer refuses a decision from a non-active seat, so no human can move for
   * it, and the pick clock exists only under `Competitive`. It is unreachable
   * from the UI today only because `HostControls.tsx` gates the button on
   * `matchInProgress || roundComplete` — a gate in a different file, in a
   * different layer, which is not where this invariant should live.
   *
   * The database load is the same obligation: an all-human pod never took
   * `startDraftInner`'s bot-seat branch, so this seat is the pod's FIRST bot and
   * would otherwise decide with no card faces at all.
   *
   * REVERT-FAILING: delete the `resolveBotPicks` call (or the
   * `loadCardDatabaseForSharedStackBots` call) from `replaceSeatWithBotInner`
   * and the matching assertion goes to zero calls.
   */
  it("drives and equips a seat that becomes a bot mid-Winston-draft", async () => {
    const { host, resolveSharedStackBotTurns, privateAdapter } =
      await startedPodWithABot("Drafting", { seatABot: false });
    // The reducer's own return: seat 1 is a bot now, and the draft is live.
    privateAdapter.replaceSeatWithBot = vi.fn(async () => winstonView(1, "Drafting", true));
    privateAdapter.getViewForSeat = vi.fn(async () => winstonView(1, "Drafting", true));
    fetchMock.mockClear();

    await host.replaceSeatWithBot(1);

    expect(privateAdapter.replaceSeatWithBot).toHaveBeenCalledOnce();
    expect(fetchMock).toHaveBeenCalledOnce();
    expect(privateAdapter.loadCardDatabase).toHaveBeenCalledWith("CARD-DATA");
    expect(resolveSharedStackBotTurns).toHaveBeenCalledOnce();
  });

  /**
   * The paired negative, and the reachable case: the host control is gated on
   * `matchInProgress || roundComplete`, so a replacement outside `Drafting`
   * owes no turn to anybody and must not pay for a card-data fetch.
   *
   * REVERT-FAILING: drop the `stillDrafting` gate and both assertions red.
   */
  it("drives nothing when a seat becomes a bot outside the draft", async () => {
    const { host, resolveSharedStackBotTurns, privateAdapter } =
      await startedPodWithABot("Drafting", { seatABot: false });
    privateAdapter.replaceSeatWithBot = vi.fn(async () => winstonView(0, "MatchInProgress", true));
    privateAdapter.getViewForSeat = vi.fn(async () => winstonView(0, "MatchInProgress", true));
    fetchMock.mockClear();

    await host.replaceSeatWithBot(1);

    expect(privateAdapter.replaceSeatWithBot).toHaveBeenCalledOnce();
    expect(fetchMock).not.toHaveBeenCalled();
    expect(resolveSharedStackBotTurns).not.toHaveBeenCalled();
  });
});

/**
 * A HOST RELOAD of a Winston pod whose restored active seat is a bot.
 *
 * This is the state the decision path itself creates:
 * `handleSharedStackDecision` persists the human's applied decision BEFORE it
 * runs the bot turns, so the durable snapshot strictly between those two awaits
 * describes a pod waiting on a bot. `restoreFromPersisted` used to branch only
 * on `MatchInProgress` and `Pairing` and hand a restored `Drafting` view back
 * untouched — and nothing else could recover it: the reducer refuses a decision
 * from a non-active seat, `frozenTimer` is in-memory only and is never
 * persisted, and `startPickTimer` returns immediately for any pod that is not
 * `Competitive`. The pod sat there with no error, no clock and no legal move
 * available to anyone.
 */
describe("P2PDraftHost Winston restore", () => {
  const WINSTON_PROCEDURE = draftProcedureFixture({
    pod_size: 2,
    human_seats: 2,
    min_pod_size: 2,
    max_pod_size: 4,
    allowed_pod_sizes: [2, 3, 4],
    distribution: { SharedStackPiles: { pile_count: 3 } },
  });
  const PREMIER_PROCEDURE = draftProcedureFixture({
    pod_size: 8,
    human_seats: 8,
    allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
    distribution: "PickAndPass",
  });

  const originalFetch = globalThis.fetch;
  let fetchMock: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    vi.clearAllMocks();
    fetchMock = vi.fn(async () => new Response("CARD-DATA", { status: 200 }));
    globalThis.fetch = fetchMock as unknown as typeof fetch;
  });

  afterEach(() => {
    globalThis.fetch = originalFetch;
  });

  /**
   * `seatTokens: {}` is load-bearing, for the reason `p2pDraftHostResume`
   * states: a non-empty map arms a per-seat grace timer and pauses the host.
   */
  function persistedWinstonSession(): PersistedDraftHostSession {
    return {
      persistenceId: "winston-restore",
      roomCode: "ABCDE",
      kind: "Winston",
      podSize: 2,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Casual",
      seatTokens: {},
      seatNames: { 0: "Host" },
      kickedTokens: [],
      draftStarted: true,
      draftCode: "draft-12345678",
      draftSessionJson: '{"status":"Drafting"}',
      poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
    } as unknown as PersistedDraftHostSession;
  }

  /**
   * A host that has NOT been initialized, which is the real ordering:
   * `draftPodHostAdapter` restores at step 5 and calls `initialize()` at step 6.
   * That is why `this.procedure` is still null inside `restoreFromPersisted`,
   * and why a dispatch written against it silently falls through.
   */
  function restoringHost(
    restored: DraftPlayerView,
    kind: "Winston" | "Premier" = "Winston",
  ) {
    const host = new P2PDraftHost(
      { id: "host" } as never,
      () => () => {},
      { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } } as never,
      kind,
      2,
      "Host",
      "Swiss",
      "Casual",
      undefined,
      "winston-restore",
      "ABCDE",
    );
    let botsHaveRun = false;
    const resolveSharedStackBotTurns = vi.fn(async () => {
      botsHaveRun = true;
      return [{ SharedStackDecisionApplied: { seat: 1 } }];
    });
    const draftProcedure = vi.fn(async () =>
      (kind === "Winston" ? WINSTON_PROCEDURE : PREMIER_PROCEDURE));
    const loadCardDatabase = vi.fn(async (_json: string) => 0);
    const adapter = {
      draftProcedure,
      loadCardDatabase,
      importSession: vi.fn(async () => restored),
      exportSession: vi.fn(async () => "{}"),
      setSeatConnected: vi.fn(async () => {}),
      resolveSharedStackBotTurns,
      // After the loop the turn is back on the human seat; before it, the
      // restored snapshot is the bot-active state itself.
      getViewForSeat: vi.fn(async () =>
        (botsHaveRun ? winstonRestoreView(0, "Drafting", true) : restored)),
    };
    (host as unknown as { adapter: unknown }).adapter = adapter;
    return { host, adapter, resolveSharedStackBotTurns, loadCardDatabase, draftProcedure };
  }

  /**
   * The restored pod, and the seam it turns on. The bot-active leg is
   * `activeSeat: 1` with `is_bot: true` on that seat; every paired negative
   * below moves exactly one of those two published fields.
   */
  function winstonRestoreView(
    activeSeat: number,
    status: string,
    seatABot: boolean,
    { sharedStack = true }: { sharedStack?: boolean } = {},
  ): DraftPlayerView {
    return {
      status,
      kind: "Winston",
      pool: [],
      current_pack: null,
      required_pick_count: 0,
      draft_effects: [],
      seats: [
        { seat_index: 0, display_name: "Host", is_bot: false, connected: true,
          has_submitted_deck: false, pick_status: "Pending", active_pack_count: 0, drafted_card_count: 0,
          face_up_draft_cards: [] },
        { seat_index: 1, display_name: "Guest", is_bot: seatABot, connected: true,
          has_submitted_deck: false, pick_status: "Pending", active_pack_count: 0, drafted_card_count: 0,
          face_up_draft_cards: [] },
      ],
      pick_number: 0,
      shared_stack: sharedStack
        ? {
          main_stack_remaining: 11,
          total_cards: 20,
          active_seat: activeSeat,
          active_pile: 0,
          piles: [],
          decisions: 5,
          history: [],
          forced_draw: null,
        }
        : null,
    } as unknown as DraftPlayerView;
  }

  /**
   * THE defect. A restored pod waiting on a bot is driven, fenced, and handed
   * back in the state the drive left it — not the bot-active state it was
   * restored into.
   *
   * REVERT-FAILING: delete the `view.status === "Drafting"` branch (or the
   * `resolveBotPicks` call inside `resumeDraftingAfterRestore`) and the loop is
   * never called, the snapshot is never re-fenced, and the returned view still
   * names the bot as the active seat.
   */
  it("drives a restored pod whose active seat is a bot", async () => {
    const restored = winstonRestoreView(1, "Drafting", true);
    const { host, resolveSharedStackBotTurns } = restoringHost(restored);

    const view = await host.restoreFromPersisted(persistedWinstonSession());

    // Reach-guard: the restored snapshot really was waiting on the bot seat.
    expect(restored.shared_stack!.active_seat).toBe(1);
    expect(restored.seats[1]!.is_bot).toBe(true);

    expect(resolveSharedStackBotTurns).toHaveBeenCalledOnce();
    // Fenced: the bot turns are durable before anybody is served.
    expect(saveDraftHostSession).toHaveBeenCalled();
    // And the caller is handed where the chain STOPPED, not where it started.
    expect(view!.shared_stack!.active_seat).toBe(0);
  });

  /**
   * The second half of the same reload, and a defect of its own: a restored
   * Set-pool Winston pod never loaded a card database. The only load for that
   * case lives in `startDraftInner`, which restore never calls, and
   * `draftPodHostAdapter`'s own fetch is gated on `Cube || CommanderDraft`. The
   * bot kept playing and never said so —
   * `winston_decision_degrades_without_a_card_database` pins exactly that
   * degraded behaviour, with principles 1 and 5 dead.
   *
   * REVERT-FAILING: delete the `loadCardDatabaseForSharedStackBots` call from
   * `resumeDraftingAfterRestore` and both assertions go to zero calls.
   */
  it("loads the card database for a restored pod that seats a bot", async () => {
    const { host, loadCardDatabase } = restoringHost(winstonRestoreView(1, "Drafting", true));

    await host.restoreFromPersisted(persistedWinstonSession());

    expect(fetchMock).toHaveBeenCalledOnce();
    expect(loadCardDatabase).toHaveBeenCalledWith("CARD-DATA");
  });

  /**
   * A CASUAL POD HAS A RECOVERY TOO.
   *
   * When a bot turn fails loudly the shared-stack cursor is left on a seat only
   * the host can move: `refusal_for` refuses a decision from any other seat, and
   * `ReplaceSeatWithBot` on a seat that is already a bot changes nothing. The
   * pick clock is the documented recovery — but `startPickTimer` returns
   * immediately unless the pod is Competitive, and `restoringHost` builds a
   * CASUAL pod, so without this the pod is simply stranded.
   *
   * REVERT-FAILING: delete the bot re-drive from `requestResume` and the second
   * `resolveSharedStackBotTurns` call never happens.
   */
  it("re-drives a stuck bot when the host resumes, whatever the pod policy", async () => {
    const { host, resolveSharedStackBotTurns } = restoringHost(
      winstonRestoreView(1, "Drafting", true),
    );
    await host.restoreFromPersisted(persistedWinstonSession());
    const drivenByRestore = resolveSharedStackBotTurns.mock.calls.length;

    host.requestPause();
    host.requestResume();
    await vi.waitFor(() =>
      expect(resolveSharedStackBotTurns.mock.calls.length).toBeGreaterThan(drivenByRestore));
  });

  /**
   * A FAILING CARD-DATA FETCH MUST NOT COST THE POD.
   *
   * `loadCardDatabaseForSharedStackBots` pulls multiple megabytes over the
   * network, so an offline reload or a CDN blip throws inside the restore. If
   * that throw escaped, it would propagate through `hostDraft`'s catch, which
   * disposes the pending host and rethrows — leaving the IndexedDB snapshot
   * untouched and still describing a bot-active pod, so every later attempt to
   * re-host would run the same failing path. One transient fetch failure would
   * make an in-progress draft permanently unhostable.
   *
   * Failing open costs only the bot's head start. The engine scores without a
   * card database (degraded but functional), and the pick clock or a host
   * control recovers the turn, so there is nothing to fail closed for.
   *
   * REVERT-FAILING: remove the `try`/`catch` from `resumeDraftingAfterRestore`
   * and `restoreFromPersisted` rejects instead of resolving.
   */
  it("survives a card-data fetch that fails during a restore", async () => {
    const { host } = restoringHost(winstonRestoreView(1, "Drafting", true));
    fetchMock.mockRejectedValueOnce(new Error("offline"));
    const events = vi.fn();
    host.onEvent(events);

    const view = await host.restoreFromPersisted(persistedWinstonSession());

    // The pod came back rather than throwing, which is the whole claim.
    expect(view!.status).toBe("Drafting");
    // And the failure is surfaced rather than swallowed, so the host is not
    // left wondering why its bot has not moved.
    expect(events).toHaveBeenCalledWith(expect.objectContaining({
      type: "error",
      message: expect.stringContaining("offline"),
    }));
  });

  /**
   * The `is_bot` half of the gate: a restored human-vs-human Winston pod — the
   * common shape — makes no engine round-trip at all and pays for no fetch.
   *
   * The PROCEDURE assertion is the discriminating one, and it was chosen by
   * measurement rather than by taste. Dropping the `is_bot` conjunct leaves the
   * bot-loop and fetch assertions GREEN — `resolveSharedStackBotTurns` has its
   * own published `is_bot` pre-check, and
   * `loadCardDatabaseForSharedStackBots` short-circuits on a zero bot count —
   * so the only thing the conjunct actually buys is not paying for the
   * procedure round-trip on the common shape. That is what this pins.
   *
   * REVERT-FAILING: drop the `view.seats.some(seat => seat.is_bot)` conjunct and
   * the `draftProcedure` assertion reds.
   */
  it("drives nothing for a restored pod of humans", async () => {
    const { host, resolveSharedStackBotTurns, loadCardDatabase, draftProcedure } =
      restoringHost(winstonRestoreView(1, "Drafting", false));

    const view = await host.restoreFromPersisted(persistedWinstonSession());

    // Reach-guard: the restore really did run and really did return the pod.
    expect(view!.status).toBe("Drafting");
    expect(draftProcedure).not.toHaveBeenCalled();
    expect(resolveSharedStackBotTurns).not.toHaveBeenCalled();
    expect(fetchMock).not.toHaveBeenCalled();
    expect(loadCardDatabase).not.toHaveBeenCalled();
  });

  /**
   * The distribution half. `shared_stack` is the engine's own published
   * discriminator — `None` for every non-Winston frame and outside `Drafting` —
   * so a pick-and-pass pod full of bots falls straight through, fetches nothing,
   * and does not even resolve its procedure.
   *
   * REVERT-FAILING: drop the `!view.shared_stack` conjunct and this reds on the
   * fetch: the pick-and-pass branch of `resolveBotPicks` would run instead.
   */
  it("drives nothing for a restored pick-and-pass pod", async () => {
    const { host, resolveSharedStackBotTurns, loadCardDatabase, draftProcedure } =
      restoringHost(winstonRestoreView(1, "Drafting", true, { sharedStack: false }), "Premier");

    const view = await host.restoreFromPersisted(persistedWinstonSession());

    expect(view!.status).toBe("Drafting");
    expect(resolveSharedStackBotTurns).not.toHaveBeenCalled();
    expect(loadCardDatabase).not.toHaveBeenCalled();
    expect(fetchMock).not.toHaveBeenCalled();
    expect(draftProcedure).not.toHaveBeenCalled();
  });

  /**
   * The ordering the fix depends on, pinned so it cannot regress into a silent
   * no-op: `restoreFromPersisted` runs BEFORE `initialize()`, so `this.procedure`
   * is null and `resolveBotPicks`'s dispatch — which reads it — would fall
   * through to a `current_pack` loop that is null for every seat under this
   * distribution.
   *
   * REVERT-FAILING: remove the `ensureProcedure()` call from
   * `resumeDraftingAfterRestore` and `resolveSharedStackBotTurns` is never
   * called, because the shared-stack arm is never taken.
   */
  it("resolves the procedure itself, because restore precedes initialize", async () => {
    const { host, draftProcedure, resolveSharedStackBotTurns } =
      restoringHost(winstonRestoreView(1, "Drafting", true));
    // The host really has not been initialized: the field the dispatch reads is
    // still null when the restore begins.
    expect((host as unknown as { procedure: unknown }).procedure).toBeNull();

    await host.restoreFromPersisted(persistedWinstonSession());

    expect(draftProcedure).toHaveBeenCalledWith("Winston", "Swiss");
    expect(resolveSharedStackBotTurns).toHaveBeenCalledOnce();

    // And `initialize()` does not fetch it a second time: the answer is a pure
    // function of the kind and format, both immutable for this host.
    await host.initialize();
    expect(draftProcedure).toHaveBeenCalledOnce();
  });
});
