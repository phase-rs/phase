import { afterEach, describe, expect, it, vi } from "vitest";

import { P2PDraftHost } from "../p2p-draft-host";
import type { DraftPlayerView, SharedStackPileView, SharedStackView } from "../draft-adapter";
import { draftProcedureFixture } from "./draftProcedureFixture";

/**
 * The P2P pick clock must DRIVE a stalled shared-stack seat, and must keep
 * running after it does.
 *
 * Both halves were dead on the path Winston actually ships on. `startPickTimer`
 * fired at the end of `startDraft` (the pod policy defaults to `"Competitive"`),
 * but `autoPickAllPending` gated its whole body on
 * `view.current_pack && view.current_pack.length > 0`, which is null for every
 * shared-stack seat; and the only restart site lived in `applyPick`, which a
 * shared-stack decision never reaches. So the clock counted down once, expired
 * into a no-op, and never re-armed — leaving a stalled Winston seat with no
 * recovery at all, while `ReplaceSeatWithBot` is (correctly) refused for this
 * distribution.
 *
 * `server-core` built this arm already (`draft_seats_needing_auto_pick` plus
 * `pick_random_for_seat`'s `forced_decision` path). These tests pin the P2P
 * half against the same contract.
 */
describe("P2PDraftHost shared-stack pick timer", () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  const WINSTON_PROCEDURE = draftProcedureFixture({
    pod_size: 2,
    human_seats: 2,
    min_pod_size: 2,
    max_pod_size: 4,
    allowed_pod_sizes: [2, 3, 4],
    distribution: { SharedStackPiles: { pile_count: 3 } },
  });

  function pile(
    index: number,
    take: SharedStackPileView["legality"][number]["refusal"],
    decline: SharedStackPileView["legality"][number]["refusal"],
  ): SharedStackPileView {
    return {
      index,
      total: 3,
      revealed: [],
      // Built in `SharedStackPileDecision::ALL` order, exactly as
      // `shared_stack_view` builds it, because the order is what
      // `forced_decision` folds and therefore what the host must reproduce.
      legality: [
        { decision: "Take", refusal: take },
        { decision: "Decline", refusal: decline },
      ],
    };
  }

  /** `active_seat` 1 throughout: seat 0 is the HOST, so a fixture whose active
   *  seat were 0 could not tell "drove the active seat" from "drove seat 0". */
  function winstonView(sharedStack: SharedStackView): DraftPlayerView {
    return {
      status: "Drafting",
      kind: "Winston",
      launch_capability: "None",
      commanders_required: 0,
      pool: [],
      // Null for EVERY seat under a shared stack. This is the field the old
      // sweep gated on, which is why the sweep could never act here.
      current_pack: null,
      required_pick_count: 0,
      draft_effects: [],
      pool_groups: {
        color_groups: [], type_groups: [], cmc_groups: [], rarity_groups: [],
        type_filter_options: [], color_filter_options: [],
        color_counts: { white: 0, blue: 0, black: 0, red: 0, green: 0 },
        workspace_capabilities: { rarity_group_order: null },
        workspace_row_classification: {
          creature_instance_ids: [], noncreature_instance_ids: [],
        },
      },
      seats: [
        { seat_index: 0, display_name: "Host", is_bot: false, connected: true, has_submitted_deck: false, pick_status: "Waiting", active_pack_count: 0, drafted_card_count: 0, face_up_draft_cards: [] },
        { seat_index: 1, display_name: "Guest", is_bot: false, connected: true, has_submitted_deck: false, pick_status: "Pending", active_pack_count: 0, drafted_card_count: 0, face_up_draft_cards: [] },
      ],
      current_pack_number: 0,
      pick_number: 0,
      pass_direction: "Left",
      cards_per_pack: 15,
      pack_count: 3,
      min_deck_size: 40,
      addable_cards: [],
      timer_remaining_ms: null,
      standings: [],
      current_round: 0,
      tournament_format: "Swiss",
      pod_policy: "Competitive",
      pairings: [],
      match_config: { match_type: "Bo1" },
      shared_stack: sharedStack,
      play_first_chooser: null,
    } as unknown as DraftPlayerView;
  }

  async function startedWinstonHost(sharedStack: SharedStackView,
    // WHAT THE ENGINE ANSWERS. The host no longer scans `legality` for the
    // first unrefused entry -- `shared_stack::forced_decision` chooses and the
    // host only dispatches -- so the fixture supplies the engine's answer and
    // the assertions pin that the host relays exactly it.
    forcedDecision: "Take" | "Decline" | null = "Take") {
    vi.useFakeTimers();
    const host = new P2PDraftHost(
      { id: "host" } as never,
      () => () => {},
      { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } } as never,
      "Winston",
      2,
      "Host",
      "Swiss",
      // The pod policy `startPickTimer` requires. It is also the DEFAULT the
      // pod page ships, which is why this path is live rather than hypothetical.
      "Competitive",
    );
    const submitSharedStackDecisionForSeat = vi.fn(
      async (_seat: number, _pile: number, _decision: string) => winstonView(sharedStack),
    );
    const privateHost = host as unknown as {
      timerInterval: ReturnType<typeof setInterval> | null;
      timerContext: string | null;
      mutationQueue: Promise<void>;
    };
    const sharedStackForcedDecision = vi.fn(async (_seat: number) => forcedDecision);
    (host as unknown as { adapter: unknown }).adapter = {
      draftProcedure: vi.fn(async () => WINSTON_PROCEDURE),
      createMultiplayerDraft: vi.fn(async () => {}),
      getViewForSeat: vi.fn(async () => winstonView(sharedStack)),
      allPicksSubmitted: vi.fn(async () => false),
      submitSharedStackDecisionForSeat,
      sharedStackForcedDecision,
    };

    await host.initialize();
    await host.startDraft(false);
    return { host, privateHost, submitSharedStackDecisionForSeat, sharedStackForcedDecision };
  }

  /** Runs the clock to zero and lets the detached expiry mutation settle. */
  async function expireTheClock(privateHost: { mutationQueue: Promise<void> }) {
    await vi.advanceTimersByTimeAsync(80_000);
    await privateHost.mutationQueue;
    await vi.advanceTimersByTimeAsync(0);
  }

  /**
   * REVERT-PROBE: delete the `isSharedStackDistribution` arm at the head of
   * `autoPickAllPending` and this reds with zero calls — the old sweep falls
   * through to the `current_pack` loop, which is null for every seat here.
   */
  /**
   * THE HOST'S OWN CLOCK.
   *
   * `draft-core` publishes `timer_remaining_ms: None` on every view, and the
   * countdown reaches guests only over the `draft_timer_sync` broadcast — which
   * the host, holding no guest session of its own, never receives. So without
   * this event the one player who owns the clock is the one who cannot see it.
   * Under a shared stack expiry TAKES THE PILE, and the host is half of a
   * two-seat pod and all of one played against bots.
   *
   * REVERT-FAILING: delete the `timerTick` emit from `onPickTimerTick` and no
   * tick is ever observed.
   */
  it("emits its own clock reading, which no broadcast could reach it", async () => {
    const sharedStack: SharedStackView = {
      main_stack_remaining: 11,
      total_cards: 20,
      active_seat: 0,
      active_pile: 0,
      piles: [
        pile(0, null, null),
        pile(1, "PileNotActive", "PileNotActive"),
        pile(2, "PileNotActive", "PileNotActive"),
      ],
      decisions: 1,
      history: [],
      forced_draw: null,
    };
    const { host } = await startedWinstonHost(sharedStack);
    const ticks: number[] = [];
    host.onEvent((event) => {
      if (event.type === "timerTick") ticks.push(event.remainingMs);
    });

    await vi.advanceTimersByTimeAsync(3_000);

    // A live countdown, not a single reading: the host sees it move.
    expect(ticks.length).toBeGreaterThanOrEqual(2);
    expect(ticks[0]).toBeGreaterThan(0);
    expect(ticks[ticks.length - 1]).toBeLessThan(ticks[0]);
  });

  it("drives the active seat's forced decision when the clock expires", async () => {
    const sharedStack: SharedStackView = {
      main_stack_remaining: 11,
      total_cards: 20,
      active_seat: 1,
      active_pile: 2,
      // The cursor is pile 2, and BOTH decisions are legal there, so the forced
      // move is `Take` — the first entry of `SharedStackPileDecision::ALL` whose
      // refusal is null, which is exactly what `forced_decision` folds.
      piles: [
        pile(0, "PileNotActive", "PileNotActive"),
        pile(1, "PileNotActive", "PileNotActive"),
        pile(2, null, null),
      ],
      decisions: 5,
      // Empty: this fixture exercises a display/transport path, and no client
      // consumer reads the history yet. Its fidelity to the reducer is pinned
      // in `draft-core` (`history_records_sizes_and_decisions_and_never_cards`).
      history: [],
      forced_draw: null,
    };
    const { privateHost, submitSharedStackDecisionForSeat } =
      await startedWinstonHost(sharedStack);

    // Reach-guard: the clock really was armed for this pod, so a decision below
    // is the expiry acting and not something the start path did.
    expect(privateHost.timerContext).toBe("pick");
    expect(privateHost.timerInterval).not.toBeNull();
    expect(submitSharedStackDecisionForSeat).not.toHaveBeenCalled();

    await expireTheClock(privateHost);

    // The ACTIVE seat (1), the ENGINE's cursor (2), and the engine's own first
    // legal decision. Not seat 0, and not a pile chosen here.
    expect(submitSharedStackDecisionForSeat).toHaveBeenCalledWith(1, 2, "Take");
  });

  /**
   * The discriminator for "reads the published verdict rather than defaulting
   * to Take": the SAME fixture shape with `Take` refused must submit `Decline`.
   * A hardcoded `"Take"` passes the test above and reds here.
   */
  it("submits the engine's legal decision, not a fixed one", async () => {
    const sharedStack: SharedStackView = {
      main_stack_remaining: 11,
      total_cards: 20,
      active_seat: 1,
      active_pile: 0,
      piles: [
        pile(0, "PileEmpty", null),
        pile(1, "PileNotActive", "PileNotActive"),
        pile(2, "PileNotActive", "PileNotActive"),
      ],
      decisions: 5,
      // Empty: this fixture exercises a display/transport path, and no client
      // consumer reads the history yet. Its fidelity to the reducer is pinned
      // in `draft-core` (`history_records_sizes_and_decisions_and_never_cards`).
      history: [],
      forced_draw: null,
    };
    const { privateHost, submitSharedStackDecisionForSeat, sharedStackForcedDecision } =
      await startedWinstonHost(sharedStack, "Decline");

    await expireTheClock(privateHost);

    // Asked the ENGINE, about the active seat.
    expect(sharedStackForcedDecision).toHaveBeenCalledWith(1);
    // And relayed its answer verbatim. A host that hardcoded "Take", or that
    // went back to scanning `legality` in its own order, reds here.
    expect(submitSharedStackDecisionForSeat).toHaveBeenCalledWith(1, 0, "Decline");
  });

  /**
   * REVERT-PROBE: delete the `startPickTimer` re-arm in
   * `handleSharedStackDecision` and this reds — which is the half that turned a
   * stalled seat into an unrecoverable one, because the clock then ran exactly
   * once per draft.
   *
   * Pinned on the DEADLINE MOVING, not on `timerInterval` being non-null: the
   * opening interval outlives a decision either way, so "a timer exists" passes
   * under the mutation and proves nothing. What only the re-arm produces is a
   * window that restarts from the decision rather than from `startDraft`.
   */
  it("restarts the decision window on every decision instead of letting the opening deadline stand", async () => {
    const sharedStack: SharedStackView = {
      main_stack_remaining: 11,
      total_cards: 20,
      active_seat: 1,
      active_pile: 2,
      piles: [
        pile(0, "PileNotActive", "PileNotActive"),
        pile(1, "PileNotActive", "PileNotActive"),
        pile(2, null, null),
      ],
      decisions: 5,
      // Empty: this fixture exercises a display/transport path, and no client
      // consumer reads the history yet. Its fidelity to the reducer is pinned
      // in `draft-core` (`history_records_sizes_and_decisions_and_never_cards`).
      history: [],
      forced_draw: null,
    };
    const { host, privateHost, submitSharedStackDecisionForSeat } =
      await startedWinstonHost(sharedStack);

    // 70s of the 75s opening window elapse with nothing due yet.
    await vi.advanceTimersByTimeAsync(70_000);
    expect(submitSharedStackDecisionForSeat).not.toHaveBeenCalled();

    // A PLAYER-driven decision at t=70s, not a timeout: the re-arm has to hold
    // for the ordinary path too, which is the path every non-stalled turn takes.
    await host.submitHostSharedStackDecision(2, "Take");
    expect(submitSharedStackDecisionForSeat).toHaveBeenCalledTimes(1);
    submitSharedStackDecisionForSeat.mockClear();

    // t=80s — PAST the opening deadline of t=75s. The re-arm restarted the
    // window at t=70s, so nothing is due. WITHOUT the re-arm the opening
    // interval is still counting to t=75s and fires a forced decision here,
    // against a seat that just decided.
    await vi.advanceTimersByTimeAsync(10_000);
    await privateHost.mutationQueue;
    expect(submitSharedStackDecisionForSeat).not.toHaveBeenCalled();
    expect(privateHost.timerContext).toBe("pick");
    expect(privateHost.timerInterval).not.toBeNull();

    // And the re-armed window does expire in its own right, so the clock still
    // drives a genuine stall a SECOND time — what the one-shot could not do.
    await expireTheClock(privateHost);
    expect(submitSharedStackDecisionForSeat).toHaveBeenCalledWith(1, 2, "Take");
  });
});
