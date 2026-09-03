/**
 * PF2 / U21 rows 4, 5 and row 3b's store-level companion — the pod reaches its
 * launch screen through the REAL production chain.
 *
 * WHY THIS FILE EXISTS AND WHY IT IS NOT `DraftPodPage.commanderLaunch.test.tsx`.
 * That suite mocks `../../adapter/draftPodHostAdapter` wholesale, which is
 * exactly the class these rows must run for real; `vi.mock` is hoisted and
 * FILE-scoped, so it cannot be un-mocked inside a `describe`. Hence a sibling
 * file. `commanderLaunch.test.tsx` keeps its mocks and its landed tests.
 *
 * THE VACUITY THESE ROWS EXIST TO AVOID. Hand-feeding the store
 * `allDecksSubmitted` then `viewUpdated({status:"Complete"})` CANNOT FAIL: the
 * store's `viewUpdated` arm already maps `Complete` -> `"complete"` correctly at
 * base, so (1) = (2) = `"complete"`. The defect U21 fixes is not the store's
 * mapping — it is that NO `viewUpdated` IS EMITTED AT ALL, because
 * `generatePairings()` throws before `broadcastViews()` runs. The discriminator
 * is the PRESENCE of the event, and only production can supply that. So the
 * event sequence here is produced by the real `P2PDraftHost` funnel and is
 * never hand-written.
 *
 * THE CHAIN, real end to end:
 *   hostDraft -> new DraftPodHostAdapter() -> adapter.initialize()
 *     -> new P2PDraftHost() -> host.initialize()
 *   submitDeck -> adapter.submitDeck -> host.submitHostDeck
 *     -> handleDeckSubmission -> THE GATE -> broadcastViews -> viewUpdated
 *     -> adapter.handleHostEvent -> setStatus + emit
 *     -> store handleHostEvent -> phase -> DraftPodPage's phase switch
 *
 * FOUR STUBBED SEAMS, AND NO OTHERS, on the host-assembly chain. "Only the wasm
 * boundary is stubbed" would be FALSE — `DraftPodHostAdapter.initialize()` also
 * opens a peer room, fetches the full card database over the network, and
 * touches IndexedDB:
 *   1. `../../adapter/draft-adapter` (importOriginal) — the wasm boundary. ONE
 *      module mock covers BOTH on-chain `new DraftAdapter()` sites, since
 *      `draftPodHostAdapter.ts` and `p2p-draft-host.ts` import from the same
 *      module and `vi.mock` is keyed on the resolved module. `importOriginal`
 *      is required, not stylistic: `p2p-draft-host.ts` also imports
 *      `EMPTY_DRAFT_POOL_GROUPS` from it and a bare object mock drops it.
 *   2. `../../network/connection` — `hostRoom`, a peer/room setup.
 *   3. `globalThis.fetch` — `__CARD_DATA_URL__` is a Vite `define`, so the
 *      card-data fetch at the CommanderDraft gate is a REAL network call.
 *   4. `../../services/draftPersistence` (importOriginal) — ONLY the two
 *      IndexedDB functions. `saveActiveDraftPod` / `loadActiveDraftPod` /
 *      `clearActiveDraftPod` stay REAL: they are plain `localStorage`, they
 *      work under the test DOM, and ROW 5'S ENTIRE SUBJECT IS WHAT THEY STORE.
 *      Mocking this module wholesale would delete row 5.
 *
 * Everything else runs for real: both `initialize()`s, `handleDeckSubmission`,
 * `broadcastViews`, both layers' `handleHostEvent`, and every status mapper.
 *
 * PROHIBITED IN THIS FILE, and neither prohibition is discharged by "a sibling
 * suite does it" — the siblings mock these BECAUSE they hand-feed the store on
 * purpose:
 *   - `../../stores/multiplayerDraftStore` (4 of the 5 sibling suites mock it).
 *     This store IS rows 4/5's subject.
 *   - `../../adapter/draftPodHostAdapter` (`commanderLaunch` mocks it). It
 *     replaces the class these rows must run for real.
 *
 * `MenuShell`'s stub body is LOAD-BEARING: it is the sole ANCESTOR of the phase
 * content (`DraftPodPage.tsx:956`, inside `<MenuShell>` at `:933-958`), so
 * `() => null` would render neither `PairingPhaseView` nor `CompleteView` and
 * would COLLAPSE row 4's (1) onto its (2). `ScreenChrome` and `HostControls`
 * are SIBLINGS of `MenuShell`, outside the asserted subtree, so `() => null` is
 * correct for those two. `<MemoryRouter>` is likewise a discriminator-preserver,
 * not a convenience: the page calls `useNavigate`/`useSearchParams` and
 * `CompleteView` calls `useNavigate()`, all of which throw outside a router —
 * on BOTH trees, which would collapse (1) onto (2) the same way.
 */

import { cleanup, render, screen } from "@testing-library/react";
import type { ReactNode } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { DraftPodPage } from "../DraftPodPage";
import { useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore";
import {
  clearActiveDraftPod,
  loadActiveDraftPod,
  saveActiveDraftPod,
} from "../../services/draftPersistence";

// ── The module-scope stub config ───────────────────────────────────────
//
// Production constructs the adapter with NO arguments — nothing on this chain
// hands it a seat roster (`createMultiplayerDraft` is inside `startDraft`, not
// on the hostDraft -> initialize -> submitDeck path). So the stub reads this
// object at CALL time, and a `beforeEach` assigns EVERY field of it: three
// tests share it across two draft kinds and two `postDraftPlay` values, and a
// test that assigned only `kind` would silently inherit the previous test's
// `postDraftPlay`.
//
// CLOCK STATE IS PER-INSTANCE, only the config is module-scope.
// `draftPodHostAdapter.ts:220`'s `new DraftAdapter()` is transient (its return
// is discarded); `p2p-draft-host.ts:255`'s is the one every stubbed method
// actually goes through, so a shared submitted-set would let the discarded
// instance perturb clock (a).

type SeatSpec = { seat: number; isBot: boolean };

/**
 * The reducer's nine `DraftStatus` members. Declared in full, and used as
 * `statusNow`'s RETURN TYPE, so the stub's `generatePairings` guard can state
 * the reducer's whole admit set (`Deckbuilding | Pairing | RoundComplete`)
 * rather than only the members this file's two pod shapes happen to produce.
 * Without it TypeScript narrows the return to the produced subset and the
 * `RoundComplete` arm becomes a "no overlap" error — i.e. the stub would be
 * forced to misdescribe the guard it stands for.
 */
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

const stubConfig: {
  postDraftPlay: "CompleteImmediately" | "TournamentPairings";
  seats: SeatSpec[];
  poolSize: number;
} = {
  postDraftPlay: "CompleteImmediately",
  seats: [],
  poolSize: 0,
};

// Must be CANONICAL: `draftPersistence` validates every stored `roomCode` with
// `isCanonicalRoomCode`, and `connection.ts`'s CODE_ALPHABET omits I/L/O/0/1.
// A non-canonical code makes `inspectActiveDraftPod` report `invalid`, so row
// 5's persisted record would read back as `null`.
const ROOM_CODE = "PDABC";

vi.mock("../../adapter/draft-adapter", async (importOriginal) => {
  const original = await importOriginal<typeof import("../../adapter/draft-adapter")>();
  return {
    ...original,
    // `function`, not an arrow: production calls `new DraftAdapter()`.
    DraftAdapter: vi.fn().mockImplementation(function () {
      // Per-instance clocks. See the header: clock (a) is the accumulating
      // submitted-seat set (draft-core session.rs:895), clock (b) is the
      // pairingsGenerated boolean (session.rs:903 then :254).
      const submitted = new Set<number>();
      let pairingsGenerated = false;

      const statusNow = (): DraftStatusName => {
        const humans = stubConfig.seats.filter((s) => !s.isBot).map((s) => s.seat);
        if (humans.some((seat) => !submitted.has(seat))) return "Deckbuilding";
        if (stubConfig.postDraftPlay === "CompleteImmediately") return "Complete";
        return pairingsGenerated ? "MatchInProgress" : "Pairing";
      };

      const viewFor = (seat: number) => ({
        status: statusNow(),
        kind: stubConfig.postDraftPlay === "CompleteImmediately" ? "CommanderDraft" : "Premier",
        launch_capability: stubConfig.postDraftPlay === "CompleteImmediately"
          ? "CommanderMultiplayer"
          : "None",
        seat_index: seat,
        current_round: 1,
        pairings: [],
        pool_groups: {
          color_groups: [], type_groups: [], cmc_groups: [], rarity_groups: [],
          type_filter_options: [], color_filter_options: [],
          color_counts: { white: 0, blue: 0, black: 0, red: 0, green: 0 },
          workspace_capabilities: { rarity_group_order: null },
          workspace_row_classification: { creature_instance_ids: [], noncreature_instance_ids: [] },
        },
        // `saveDraftPodProgress` dereferences `view?.pool.length` — the `?.`
        // guards `view`, NOT `pool`. A missing `pool` throws inside the store,
        // synchronously under `broadcastViews`' host emit, whose surrounding
        // `catch` is bare — so it produces NO error event at all and the rows
        // would red through their reach-guards with nothing naming the cause.
        pool: Array.from({ length: stubConfig.poolSize }, (_, i) => ({
          card_instance_id: `c${i}`,
        })),
        standings: [],
        next_pairing_round: 1,
        timer_remaining_ms: null,
        seats: stubConfig.seats.map((s) => ({
          seat_index: s.seat,
          is_bot: s.isBot,
          connected: true,
          display_name: s.isBot ? `Bot ${s.seat}` : `Player ${s.seat}`,
          has_submitted_deck: submitted.has(s.seat),
          pick_status: "NotDrafting",
          active_pack_count: 0,
          face_up_draft_cards: [],
        })),
        match_config: { match_type: "Bo1" },
      });

      return {
        loadCardDatabase: vi.fn(async () => 0),
        draftProcedure: vi.fn(async () => ({
          post_draft_play: stubConfig.postDraftPlay,
          launch_capability: stubConfig.postDraftPlay === "CompleteImmediately"
            ? "CommanderMultiplayer"
            : "None",
        })),
        // Seam 1 again: `submitHostDeck` refuses a submission before the draft
        // has started, so the chain now runs `startDraft` first and this is the
        // wasm call it makes. `statusNow()` is `Deckbuilding` at this point
        // (no seat has submitted), so `startDraftInner`'s bot-pick branch —
        // gated on `Drafting` — is not entered and neither clock moves.
        createMultiplayerDraft: vi.fn(async () => {}),
        submitDeckForSeat: vi.fn(async (seat: number) => {
          submitted.add(seat);
          return viewFor(seat);
        }),
        getViewForSeat: vi.fn(async (seat: number) => viewFor(seat)),
        generatePairings: vi.fn(async () => {
          // apply_generate_pairings' guard, draft-core session.rs:214-217.
          const status = statusNow();
          if (status !== "Deckbuilding" && status !== "Pairing" && status !== "RoundComplete") {
            throw new Error(
              `InvalidTransition { from: ${status}, action: "GeneratePairings" }`,
            );
          }
          pairingsGenerated = true;
          return { current_round: 1, pairings: [] };
        }),
      };
    }),
  };
});

vi.mock("../../network/connection", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../network/connection")>()),
  hostRoom: vi.fn(async () => ({
    roomCode: ROOM_CODE,
    peerId: `phase2-${ROOM_CODE}`,
    peer: { destroy: vi.fn(), on: vi.fn() },
    onGuestConnected: vi.fn(() => vi.fn()),
    destroy: vi.fn(),
  })),
  joinRoom: vi.fn(),
}));

// Partial, deliberately: only the two IndexedDB functions. See seam 4 above.
vi.mock("../../services/draftPersistence", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../services/draftPersistence")>()),
  loadDraftHostSession: vi.fn(async () => null),
  saveDraftHostSession: vi.fn(async () => {}),
  clearDraftHostSession: vi.fn(async () => {}),
}));

// Only the hook is overridden; every other export stays real, so the `?kind=`
// slug the page's entry effect reads keeps its single authority.
//
// The selector is run against the REAL store state with only the three
// side-effecting fns the page's entry effects call replaced. Feeding it a
// three-key literal instead — the nearest precedent's shape — is not enough
// here: this file renders the page while the pod is still in `lobby`, and
// `DraftPodLobby` selects `config`, `poolMode`, `cubeForm`, `botFillEnabled`,
// `toggleBotFill` and `startDraft` from the same store. Passing real state
// through is also strictly closer to production than a literal.
vi.mock("../../stores/draftPodStore", async (importOriginal) => {
  const original = await importOriginal<typeof import("../../stores/draftPodStore")>();
  return {
    ...original,
    useDraftPodStore: (selector: (state: unknown) => unknown) =>
      selector({
        ...original.useDraftPodStore.getState(),
        reset: vi.fn(),
        resumeHostedPod: vi.fn(),
        enterKind: vi.fn(),
      }),
  };
});

vi.mock("../../components/chrome/ScreenChrome", () => ({ ScreenChrome: () => null }));
vi.mock("../../components/menu/MenuShell", () => ({
  // PASSTHROUGH, never `() => null` — see the header.
  MenuShell: ({ children }: { children: ReactNode }) => <>{children}</>,
}));
vi.mock("../../components/draft/HostControls", () => {
  const emptyTopActions: readonly [] = [];
  return {
    HostControls: () => null,
    useHostDraftTopActions: (_options: { enabled: boolean }) => emptyTopActions,
  };
});

// ── Harness ────────────────────────────────────────────────────────────

const POD_ID = "pod-under-test";

function commanderPodConfig(seatCount: number) {
  return {
    poolInput: { type: "Set" as const, data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
    kind: "CommanderDraft" as const,
    podSize: seatCount,
    hostDisplayName: "Host",
    tournamentFormat: "Swiss" as const,
    podPolicy: "Competitive" as const,
  };
}

/** Seat 0 human (the seat `submitHostDeck` submits), every other seat a bot. */
function hostPlusBots(seatCount: number): SeatSpec[] {
  return Array.from({ length: seatCount }, (_, i) => ({ seat: i, isBot: i !== 0 }));
}

function renderPage() {
  return render(
    <MemoryRouter>
      <DraftPodPage />
    </MemoryRouter>,
  );
}

/**
 * `saveDraftPodProgress` opens with `const meta = loadActiveDraftPod(); if
 * (!meta) return;` — with no active-pod meta in `localStorage` EVERY
 * `saveDraftPodProgress` call on BOTH trees is a no-op, so (1) = (2) = "no
 * record" and row 5 would be vacuous. `hostDraft` writes the meta only when
 * `config.persistenceId` is set, and this fixture deliberately does not set one.
 *
 * `updatedAt: Date.now()` is load-bearing: `loadActiveDraftPod` returns `null`
 * and CLEARS the record past `HOST_SESSION_TTL_MS`, which would silently
 * re-open the vacuity.
 *
 * The seeded phase is `"deckbuilding"` — NEITHER of row 5's two asserted
 * values, so the seed cannot satisfy either assertion.
 */
function seedActivePodMeta(seatCount: number) {
  saveActiveDraftPod({
    id: POD_ID,
    roomCode: ROOM_CODE,
    kind: "CommanderDraft",
    podSize: seatCount,
    hostDisplayName: "Host",
    tournamentFormat: "Swiss",
    podPolicy: "Competitive",
    phase: "deckbuilding",
    pickCount: 7,
    updatedAt: Date.now(),
  });
}

describe("DraftPodPage — a completed pod reaches its launch screen", () => {
  beforeEach(() => {
    // EVERY field, every test — see the module-scope config's note.
    stubConfig.postDraftPlay = "CompleteImmediately";
    stubConfig.seats = hostPlusBots(4);
    stubConfig.poolSize = 0;
    localStorage.clear();
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => ({ ok: true, text: async () => "{}" })),
    );
  });

  afterEach(() => {
    cleanup();
    clearActiveDraftPod();
    localStorage.clear();
    vi.unstubAllGlobals();
  });

  /**
   * ROW 4 — `DraftPodPage` reaches `CompleteView` by submitting the last deck
   * through the store, superseding the `setState({phase:"complete"})`
   * manufacture in `commanderLaunch.test.tsx` as row 4's route.
   *
   * REVERT-PROBE: restore `p2p-draft-host.ts`'s gate to the bare
   * `await this.generatePairings();`. `generatePairings()` then throws, NO
   * `viewUpdated` is ever emitted, and the store's phase is whatever the base
   * writers left — `PairingPhaseView`, with no launch button.
   */
  it("renders CompleteView with a resolved launch label after the last deck lands", async () => {
    await useMultiplayerDraftStore.getState().hostDraft(commanderPodConfig(4));

    // REACH-GUARD #2 — the four-seam assembly ACTUALLY RAN. `roomCode` is
    // written by the store's `roomCreated` arm from the adapter, which is AFTER
    // `hostRoom` and BEFORE the card-data fetch, so this proves `initialize()`
    // was entered and got past the peer seam. Without it, a silently-swallowed
    // assembly failure (`hostDraft`'s catch is EMPTY) would render the initial
    // phase and every "is not pairing" assertion would be vacuous.
    expect(useMultiplayerDraftStore.getState().roomCode).toBe(ROOM_CODE);

    renderPage();
    // Production order: a deck submission is only legal after the draft has
    // started, and `submitHostDeck` enforces it.
    await useMultiplayerDraftStore.getState().startDraft(true);
    await useMultiplayerDraftStore.getState().submitDeck(["Human Legend"]);

    // REVERT-FAILING. REACH-GUARD #1 is built in: the button is asserted by its
    // RENDERED TEXT, so a missing-i18n render (which would produce the raw key
    // `podComplete.launchCommanderGame`) cannot pass.
    expect(
      await screen.findByRole("button", { name: "Start Commander Game" }),
    ).toBeTruthy();
    expect(useMultiplayerDraftStore.getState().phase).toBe("complete");
  });

  /**
   * ROW 5 — multi-authority: the persisted record.
   *
   * The persisted `ActiveDraftPodMeta.phase` is written by the store on BOTH
   * `statusChanged` AND `viewUpdated`, so a route that writes only the status
   * still shows up here. Like row 4's rendered phase, it self-corrects on any
   * `viewUpdated` that follows; what this row pins is the case where none
   * follows.
   *
   * REVERT-PROBE: the same host-gate revert. With no `viewUpdated` ever
   * emitted, nothing arrives to correct what the base writers persisted.
   */
  it("persists complete and never pairing, and records that a view-carrying write landed", async () => {
    stubConfig.poolSize = 3;
    seedActivePodMeta(4);
    await useMultiplayerDraftStore.getState().hostDraft(commanderPodConfig(4));
    expect(useMultiplayerDraftStore.getState().roomCode).toBe(ROOM_CODE);

    renderPage();
    // Production order: a deck submission is only legal after the draft has
    // started, and `submitHostDeck` enforces it.
    await useMultiplayerDraftStore.getState().startDraft(true);
    await useMultiplayerDraftStore.getState().submitDeck(["Human Legend"]);

    const meta = loadActiveDraftPod();
    // Reach guard: an unseeded or TTL-expired record would satisfy "never
    // pairing" vacuously.
    expect(meta).not.toBeNull();
    // REVERT-FAILING, and asserted as EXACT equality: a bare
    // `not.toBe("pairing")` is satisfied by the seeded `"deckbuilding"` and
    // would pass on a tree where nothing ran at all.
    expect(meta?.phase).toBe("complete");

    // Hostile sibling to the phase assertion above: it reads the OTHER field
    // these writes touch. `saveDraftPodProgress` re-reads meta on every call
    // and writes `view?.pool.length ?? meta.pickCount`, so a no-view write
    // echoes back whatever a view-carrying write already persisted. A final 7
    // (the seeded value) therefore means exactly one thing: no view-carrying
    // write ever landed — which is what the host-gate revert produces. It does
    // NOT discriminate the order of `setStatus` and the `viewUpdated` emit;
    // both orders terminate at 3.
    expect(meta?.pickCount).toBe(3);
  });

  /**
   * ROW 3b's CHARTERED STORE-LEVEL COMPANION — fed the ADAPTER's stream, not
   * the host's.
   *
   * It lives here because three of the four seams are `vi.mock` calls and
   * `vi.mock` is file-scoped, so this file is the one place they exist. It
   * drives the REAL store through `hostDraft` rather than calling
   * `handleHostEvent` directly — `handleHostEvent` is NOT exported, so
   * `hostDraft` is the only way to reach it, and going through it is what makes
   * the consumed stream the adapter's BY CONSTRUCTION rather than by
   * transcription. The two streams genuinely differ: the adapter injects
   * `statusChanged` events the host never emits.
   *
   * What it proves, precisely: Shape B's deletions did NOT delete the Premier
   * pod's `pairing` phase. Its revert-failing ground is the HOST's both-branch
   * `broadcastViews()`, not the store's own deletion — re-adding that store
   * write still yields `"pairing"` first, so it is not credited with
   * discriminating power it does not have. The store edit is covered by rows 3a
   * and 5.
   *
   * REVERT-PROBE: restore the bare `await this.generatePairings();`. Then
   * `adapter.generatePairings()` runs FIRST, flipping clock (b), and the
   * trajectory goes `deckbuilding -> matchInProgress` with no `"pairing"`.
   */
  it("keeps the Premier pod's pairing phase on the trajectory", async () => {
    stubConfig.postDraftPlay = "TournamentPairings";
    stubConfig.seats = hostPlusBots(4);

    const trajectory: string[] = [];
    const unsub = useMultiplayerDraftStore.subscribe((state, prev) => {
      if (state.phase !== prev.phase) trajectory.push(state.phase);
    });
    try {
      await useMultiplayerDraftStore.getState().hostDraft({
        ...commanderPodConfig(4),
        kind: "Premier" as const,
      });
      expect(useMultiplayerDraftStore.getState().roomCode).toBe(ROOM_CODE);

      // Production order: a deck submission is only legal after the draft has
      // started, and `submitHostDeck` enforces it. Started BEFORE the
      // trajectory is cleared, so its own phase writes are not measured.
      await useMultiplayerDraftStore.getState().startDraft(true);

      // The store's own `draftComplete` handling is not on this path; put the
      // pod in deckbuilding the way the funnel's precondition requires, then
      // let PRODUCTION compute every phase after it.
      useMultiplayerDraftStore.setState({ phase: "deckbuilding" });
      trajectory.length = 0;

      await useMultiplayerDraftStore.getState().submitDeck(["Human Legend"]);

      // Reach guard, TRUE ON BOTH TREES: a harness whose `hostDraft` threw
      // (its catch SWALLOWS) yields an empty trajectory, and "does not contain
      // pairing" is vacuously true of [].
      expect(trajectory.length).toBeGreaterThan(0);
      expect(trajectory[trajectory.length - 1]).toBe("matchInProgress");
      // REVERT-FAILING: absent on the unfixed tree.
      expect(trajectory).toContain("pairing");
    } finally {
      unsub();
    }
  });
});
