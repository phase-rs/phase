import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameAction, GameObject, GameState, WaitingFor } from "../../../adapter/types.ts";
import { dispatchAction } from "../../../game/dispatch.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import {
  buildGameState,
  buildPlayers,
  buildPriorityWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";
import { DialogAttachmentCard } from "../DialogAttachmentCard.tsx";

/**
 * `DialogAttachmentCard` adoption of the shared activation authority
 * (plan §6.4). Rows V16 (D2 — the gate the dialog never had) and V16b (the
 * LIVE #506 bypass this site was shipping).
 *
 * Before this change the dialog decided activation with
 * `objectActions.length > 0` — no `WaitingFor` gate and no seat gate — and then
 * re-implemented the lone-action dispatch decision inline, so a lone
 * card-consuming ability auto-fired here while the battlefield correctly asked
 * for confirmation.
 *
 * ---------------------------------------------------------------------------
 * EVIDENCE-LABEL CONVENTION (plan §7.2, binding on this module). Every comment
 * carrying a mutant/coverage claim is tagged, because an in-file label is
 * otherwise ambiguous between an INSTRUCTION and a PAST-TENSE REPORT:
 *
 *   MEASURED   past-tense report: this PR ran that arm; the quoted text is the
 *              assertion that flipped.
 *   QUOTED     past-tense report copied verbatim from a named harness log.
 *   POINTER    names a row/mutant whose measurement lives in the plan's
 *              evidence logs, not here.
 *   PAIR-ONLY / DROP-ONLY   the row carries ONE side by construction (§7.1).
 *
 * An untagged comment is prose and is evidence for nothing.
 *
 * NAMED UNMEASURED ANCHOR IN THIS FILE: none.
 * ---------------------------------------------------------------------------
 */

vi.mock("../../../game/dispatch.ts", () => ({
  dispatchAction: vi.fn(),
  dispatchInteraction: vi.fn(),
}));

vi.mock("../../card/CardImage.tsx", () => ({
  CardImage: ({ cardName }: { cardName: string }) => <div data-card-image={cardName} />,
}));

const CURSE_ID = 408;

/** A Curse-cycle Aura: attached to a PLAYER, so it renders only in this dialog. */
const CURSE_TAP: GameAction = {
  type: "ActivateAbility",
  data: { source_id: CURSE_ID, ability_index: 0 },
};

function makeCurse(abilities: unknown[]): GameObject {
  return buildGameObject({
    id: CURSE_ID,
    card_id: 4080,
    zone: "Battlefield",
    owner: 0,
    controller: 0,
    name: "Cruel Reality",
    attached_to: { type: "Player", data: 1 },
    entered_battlefield_turn: null,
    card_types: { supertypes: [], core_types: ["Enchantment"], subtypes: ["Aura", "Curse"] },
    abilities: abilities as GameObject["abilities"],
  });
}

function makeState(curse: GameObject, waitingFor: WaitingFor): GameState {
  return buildGameState({
    players: buildPlayers([0, 1]),
    objects: buildObjectMap(curse),
    battlefield: [CURSE_ID],
    exile: [],
    stack: [],
    waiting_for: waitingFor,
  });
}

const DECLARE_BLOCKERS: WaitingFor = {
  type: "DeclareBlockers",
  data: { player: 0, valid_blocker_ids: [], valid_block_targets: {} },
};

function seed(options: {
  abilities?: unknown[];
  legalActionsByObject?: Record<string, GameAction[]>;
  waitingFor?: WaitingFor;
  counters?: GameObject["counters"];
  derived?: GameState["derived"];
}) {
  const base = makeCurse(options.abilities ?? [{ effect: { type: "Tap" } }]);
  const curse = options.counters ? { ...base, counters: options.counters } : base;
  const waitingFor = options.waitingFor ?? buildPriorityWaitingFor();
  useGameStore.setState({
    gameMode: "local",
    gameState: { ...makeState(curse, waitingFor), derived: options.derived },
    waitingFor,
    legalActions: [],
    legalActionsByObject: options.legalActionsByObject ?? {},
    viewerInteraction: null,
  });
  useUiStore.setState({ pendingAbilityChoice: null });
  const onDismiss = vi.fn();
  render(<DialogAttachmentCard objectId={CURSE_ID} widthPx={200} onDismiss={onDismiss} />);
  return { onDismiss };
}

/** `role="button"` is the component's rendering of `interactive`. */
function interactiveCard(): HTMLElement | null {
  return screen.queryByRole("button");
}

describe("DialogAttachmentCard activation gate", () => {
  beforeEach(() => {
    window.matchMedia = ((query: string) => ({
      matches: query === "(hover: hover)" || query === "(any-hover: hover)",
      media: query,
      onchange: null,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(),
    })) as unknown as typeof window.matchMedia;
    vi.mocked(dispatchAction).mockReset();
    vi.mocked(dispatchAction).mockResolvedValue(undefined);
  });

  afterEach(() => {
    cleanup();
    useUiStore.setState({ pendingAbilityChoice: null });
  });

  // V16 (D2) — the timing gate the dialog never had. IDENTICAL bucket on both
  // arms; only the engine's `WaitingFor` differs. Assertion order: the
  // DeclareBlockers arm (which both the drop and the always mutant fail), then
  // the Priority arm, which is the control proving this fixture can be
  // interactive at all — without it "never interactive" would pass.
  //
  // QUOTED (`.plan-item3-r4.md:651`, the pre/post pair for this fixture):
  //   `D-2 interactive at DeclareBlockers(opponent): true`
  //   → `D2 interactive at DeclareBlockers: false`
  // MEASURED (drop side): restoring `isActivatable = objectActions.length > 0`
  //   flips the first assert to `expected <div /> to be null`.
  it("is inert at DeclareBlockers and interactive at Priority with the same bucket", () => {
    const bucket = { [String(CURSE_ID)]: [CURSE_TAP] };

    seed({ legalActionsByObject: bucket, waitingFor: DECLARE_BLOCKERS });
    expect(interactiveCard()).toBeNull();
    cleanup();

    seed({ legalActionsByObject: bucket });
    expect(interactiveCard()).not.toBeNull();
  });

  // The seat axis of the same gate: at a Priority prompt this viewer cannot act
  // on, the dialog must not offer the activation either. `legalActionsByObject`
  // in local/AI mode is computed for the STATE's priority player, not the
  // viewer, so a populated bucket alone is not authorization.
  it("is inert at an opponent's Priority even with a populated bucket", () => {
    seed({
      legalActionsByObject: { [String(CURSE_ID)]: [CURSE_TAP] },
      waitingFor: { type: "Priority", data: { player: 1 } },
    });

    expect(interactiveCard()).toBeNull();
  });

  // V16b — the LIVE #506 bypass. Same lone action on both arms; only
  // `consumes_source` differs, so neither arm can pass by "always modal" or
  // "always dispatch".
  //
  // QUOTED (`.plan-item3-r4.md:652`, pre-fix behaviour of this exact fixture):
  //   `D-1 consumes_source=true dispatchAction calls: 1 pendingAbilityChoice: null`
  // MEASURED (drop side): restoring `objectActions.length === 1 ⇒ dispatchAction`
  //   flips the consuming arm to
  //   `expected "dispatchAction" to not be called at all, but it was called 1 times`.
  it("sends a lone card-consuming ability to the modal, and a benign one straight through", () => {
    const consuming = seed({
      abilities: [{ effect: { type: "Tap" }, consumes_source: true }],
      legalActionsByObject: { [String(CURSE_ID)]: [CURSE_TAP] },
    });

    fireEvent.click(interactiveCard() as HTMLElement);
    expect(dispatchAction).not.toHaveBeenCalled();
    expect(useUiStore.getState().pendingAbilityChoice).toEqual({
      objectId: CURSE_ID,
      actions: [CURSE_TAP],
    });
    // `onDismiss` fires on BOTH outcomes, exactly as before this change: the
    // picker floats independently above this dialog.
    expect(consuming.onDismiss).toHaveBeenCalledTimes(1);
    cleanup();

    const benign = seed({
      abilities: [{ effect: { type: "Tap" }, consumes_source: false }],
      legalActionsByObject: { [String(CURSE_ID)]: [CURSE_TAP] },
    });

    fireEvent.click(interactiveCard() as HTMLElement);
    expect(dispatchAction).toHaveBeenCalledTimes(1);
    expect(dispatchAction).toHaveBeenCalledWith(CURSE_TAP);
    expect(useUiStore.getState().pendingAbilityChoice).toBeNull();
    expect(benign.onDismiss).toHaveBeenCalledTimes(1);
  });

  // FU-B: this site now consumes `derived.counter_display` with NO raw-map fallback and no
  // filter. The raw map and the projection DISAGREE on every axis, so a reintroduced
  // `Object.entries(obj.counters)` read (or a join back to it) fails at least one assertion.
  //
  // MEASURED (drop side): restoring the raw-map read fails with
  //   `Unable to find an element with the text: charge x4` (the projection count is gone,
  //   `charge x99` renders instead).
  it("renders finite pills from the engine projection, never the raw counters map (CR 122.1)", () => {
    seed({
      counters: { charge: 99, stun: 2 },
      derived: { counter_display: { [String(CURSE_ID)]: { pills: [{ counter: "charge", count: 4 }] } } },
    });

    // Projection count wins over the raw map's disagreeing count.
    expect(screen.getByText("charge x4")).toBeTruthy();
    expect(screen.queryByText("charge x99")).toBeNull();
    // Raw-map-only entry the projection dropped must NOT render.
    expect(screen.queryByText("stun x2")).toBeNull();
  });

  // CR 732.2a / CR 701.34a: the pill the superseded raw-map renderer could not express at all.
  // `count: 0` is deliberate — the UNBOUNDED pass of `counter_display_views` has no zero filter,
  // so a 0 -> 1 growth loop legitimately publishes an Unbounded row whose live count is still 0.
  //
  // MEASURED (drop side): restoring the raw-map read fails with
  //   `Unable to find an element with the text: charge ∞` — the map has no `charge` key at all,
  //   and even seeded it could only ever render a finite `x0`, which the old `> 0` filter dropped.
  it("renders an Unbounded pill as ∞ even at count 0 (CR 732.2a)", () => {
    seed({
      counters: {},
      derived: {
        counter_display: {
          [String(CURSE_ID)]: {
            pills: [{ counter: "charge", count: 0, magnitude: "Unbounded" }],
          },
        },
      },
    });

    expect(screen.getByText("charge ∞")).toBeTruthy();
    expect(screen.queryByText("charge x0")).toBeNull();
  });
});
