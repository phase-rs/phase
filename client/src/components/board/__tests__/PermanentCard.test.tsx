import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameAction, GameObject, GameState } from "../../../adapter/types.ts";
import type {
  InteractionChoiceId,
  InteractionId,
  ViewerInteraction,
} from "../../../adapter/generated/interaction";
import { dispatchAction, dispatchInteraction } from "../../../game/dispatch.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import {
  buildGameState,
  buildPendingCast,
  buildPlayers,
  buildPriorityWaitingFor,
  buildTargetSelectionProgress,
  buildTargetSelectionWaitingFor,
  buildTriggerTargetSelectionWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";
import { AttachmentFan } from "../AttachmentFan.tsx";
import { BoardInteractionContext } from "../BoardInteractionContext.tsx";
import { PermanentCard } from "../PermanentCard.tsx";

vi.mock("../../../game/dispatch.ts", () => ({
  dispatchAction: vi.fn(),
  dispatchInteraction: vi.fn(),
}));

vi.mock("../../card/CardImage.tsx", () => ({
  CardImage: ({
    cardName,
    faceDown,
    oracleText,
    tokenFilters,
  }: {
    cardName: string;
    faceDown?: boolean;
    oracleText?: string;
    tokenFilters?: { subtypes?: string[] };
  }) => (
    <div
      aria-label={faceDown ? "Face-down card" : cardName}
      data-face-down={faceDown ? "true" : "false"}
      data-oracle-text={oracleText ?? ""}
      data-token-subtypes={tokenFilters?.subtypes?.join(",") ?? ""}
      style={{ height: "var(--card-h)", width: "var(--card-w)" }}
    />
  ),
}));

vi.mock("../KeywordStrip.tsx", () => ({
  KeywordStrip: ({ keywords }: { keywords: unknown }) => (
    <output data-testid="keyword-strip">{JSON.stringify(keywords)}</output>
  ),
}));

function makeObject(overrides: Partial<GameObject> = {}): GameObject {
  return buildGameObject({
    id: 1,
    card_id: 100,
    zone: "Battlefield",
    name: "Test Creature",
    power: 2,
    toughness: 2,
    card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] },
    mana_cost: { type: "Cost", shards: ["Green"], generic: 1 },
    color: ["Green"],
    base_power: 2,
    base_toughness: 2,
    base_color: ["Green"],
    entered_battlefield_turn: null,
    ...overrides,
  });
}

function makeState(): GameState {
  const host = makeObject({ id: 1, attachments: [2] });
  const equipment = makeObject({
    id: 2,
    card_id: 200,
    attached_to: { type: "Object", data: 1 },
    attachments: [3],
    name: "Test Equipment",
    power: null,
    toughness: null,
    base_power: null,
    base_toughness: null,
    card_types: { supertypes: [], core_types: ["Artifact"], subtypes: ["Equipment"] },
    color: [],
    base_color: [],
  });
  const aura = makeObject({
    id: 3,
    card_id: 300,
    attached_to: { type: "Object", data: 2 },
    attachments: [],
    name: "Test Aura",
    power: null,
    toughness: null,
    base_power: null,
    base_toughness: null,
    card_types: { supertypes: [], core_types: ["Enchantment"], subtypes: ["Aura"] },
    color: ["Blue"],
    base_color: ["Blue"],
  });

  return buildGameState({
    players: buildPlayers([0, 1]),
    objects: buildObjectMap(host, equipment, aura),
    battlefield: [1, 2, 3],
    exile: [],
    stack: [],
    waiting_for: buildPriorityWaitingFor(),
  });
}

function renderPermanent(
  validTargetObjectIds = new Set<number>(),
  selectableSacrificeObjectIds = new Set<number>(),
  boardChoiceObjectIds = new Set<number>(),
  activatableObjectIds = new Set<number>(),
  undoableTapObjectIds = new Set<number>(),
  manaTappableObjectIds = new Set<number>(),
) {
  return render(
    <BoardInteractionContext.Provider
      value={{
        activatableObjectIds,
        boardChoiceObjectIds,
        committedAttackerIds: new Set(),
        incomingAttackerCounts: new Map(),
        manaTappableObjectIds,
        selectableSacrificeObjectIds,
        selectableManaCostCreatureIds: new Set(),
        undoableTapObjectIds,
        validAttackerIds: new Set(),
        validTargetObjectIds,
      }}
    >
      <PermanentCard objectId={1} />
    </BoardInteractionContext.Provider>,
  );
}

/**
 * The engine's projection for host 1: `members` is everything it publishes as
 * attached to that host — its whole subtree — and `objectIds` the subset it also
 * published a one-step pick for. A member without a pick is the ordinary case:
 * the engine publishes picks per direct host and only for a child with exactly
 * one legal choice.
 */
function interactionForAttachedObjects(
  objectIds: number[],
  members: number[] = objectIds,
): ViewerInteraction {
  const interactionId = "attachment-interaction" as InteractionId;
  const choiceId = (objectId: number) => `attachment-${objectId}` as InteractionChoiceId;
  const submission = (objectId: number) => ({
    interactionId,
    response: { type: "choose" as const, data: { choiceId: choiceId(objectId) } },
  });
  return {
    waitingForKind: { simultaneous: null, terminal: false, code: "choose" },
    authorizedSubmitters: [0],
    canSubmit: true,
    autoPassRecommended: false,
    opportunities: [{
      interactionId,
      response: {
        type: "exactChoices",
        data: {
          choices: objectIds.map((objectId) => ({
            id: choiceId(objectId),
            status: { type: "available" },
            surfaces: [],
          })),
        },
      },
      surfaces: [],
      progress: { selected: 0, minimum: 1, maximum: 1, aggregate: null, confirmable: false },
    }],
    attachmentFans: {
      1: {
        hostId: 1,
        children: objectIds.map((objectId) => ({ objectId, submission: submission(objectId) })),
      },
    },
    attachmentViews: members.length === 0 ? {} : {
      1: {
        hostId: 1,
        cards: members.map((objectId) => ({
          objectId,
          submission: objectIds.includes(objectId) ? submission(objectId) : null,
        })),
      },
    },
    availability: { type: "inputRequired" },
  } as ViewerInteraction;
}

function interactionForAttachedObject(objectId: number): ViewerInteraction {
  return interactionForAttachedObjects([objectId]);
}

/**
 * A projection that publishes membership and no pick at all — what the engine
 * sends whenever nothing about these cards is currently choosable, which is most
 * of the time. The badge and the fan read it either way.
 */
function membershipOnly(members: number[]): ViewerInteraction {
  return interactionForAttachedObjects([], members);
}

/**
 * The reported board: Slumbering Keepguard (`{2}{W}`: pump) enchanted by Cooped
 * Up (`{2}{W}`: Exile enchanted creature). Cooped Up is deliberate — its
 * ability is legitimately activatable FROM THE BATTLEFIELD, so the Aura being a
 * live choice is CORRECT and the host's unreachability needs no engine defect
 * to reproduce.
 */
function keepguardUnderCoopedUp(): GameState {
  const keepguard = makeObject({
    id: 1,
    name: "Slumbering Keepguard",
    attachments: [2],
    power: 3,
    toughness: 3,
    base_power: 3,
    base_toughness: 3,
  });
  const coopedUp = makeObject({
    id: 2,
    card_id: 200,
    attached_to: { type: "Object", data: 1 },
    attachments: [],
    name: "Cooped Up",
    power: null,
    toughness: null,
    base_power: null,
    base_toughness: null,
    card_types: { supertypes: [], core_types: ["Enchantment"], subtypes: ["Aura"] },
    color: ["White"],
    base_color: ["White"],
  });

  return buildGameState({
    players: buildPlayers([0, 1]),
    objects: buildObjectMap(keepguard, coopedUp),
    battlefield: [1, 2],
    exile: [],
    stack: [],
    waiting_for: buildPriorityWaitingFor(),
  });
}

describe("PermanentCard", () => {
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
    const gameState = makeState();
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [],
      legalActionsByObject: {},
      spellCosts: {},
      // Reset alongside the rest of the engine-published state: without this the
      // rows that publish an attachment fan leak it into every later row, so the
      // suite only passed because they happened to be declared last. Membership
      // itself is always published — object 2 hangs on the host and object 3 on
      // object 2 — because the fan and the badge now read it from here.
      viewerInteraction: membershipOnly([2, 3]),
    });
    useUiStore.setState({
      selectedObjectId: null,
      hoveredObjectId: null,
      inspectedObjectId: null,
      combatMode: null,
      selectedAttackers: [],
      blockerAssignments: new Map(),
      combatClickHandler: null,
      selectedCardIds: [],
      pendingAbilityChoice: null,
    });
    usePreferencesStore.setState({
      battlefieldCardDisplay: "full_card",
      showKeywordStrip: false,
      tapRotation: "classic",
    });
    vi.mocked(dispatchAction).mockClear();
    vi.mocked(dispatchInteraction).mockResolvedValue();
  });

  afterEach(() => {
    cleanup();
  });

  // Issue #5932: a Phantasmal Image copying a Reveillark rendered identically to
  // the real one. The board's copy badge was gated on a TOKEN-copy heuristic
  // (`is_token`), so a real card under a copy effect never qualified. The engine
  // now classifies it (CR 613.2a Layer 1a + CR 707.2) and this reads that.
  it("badges a real card that a copy effect turned into a copy", () => {
    const gameState = makeState();
    gameState.derived = { copied_permanents: [1] };
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    renderPermanent();

    expect(screen.getByText("Copy")).toBeInTheDocument();
  });

  it("shows no copy badge on an ordinary permanent", () => {
    // Discriminating guard: without it the test above would still pass if the
    // badge rendered unconditionally.
    const gameState = makeState();
    gameState.derived = { copied_permanents: [] };
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    renderPermanent();

    expect(screen.queryByText("Copy")).not.toBeInTheDocument();
  });

  it("renders the engine-authored temporary can't-be-blocked badge with its public source", () => {
    const gameState = makeState();
    gameState.derived = { cant_be_blocked: [1], temporary_cant_be_blocked: { 1: 2 } };
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    renderPermanent();

    const badge = screen.getByLabelText("Can't be blocked");
    fireEvent.pointerEnter(badge.closest(".group")!);

    expect(screen.getByRole("tooltip")).toBeVisible();
    expect(screen.getByRole("tooltip")).toHaveTextContent(
      "Can't be blocked (from Test Equipment)",
    );
  });

  it("renders the engine-authored permanent can't-be-blocked badge without temporary attribution", () => {
    const gameState = makeState();
    gameState.derived = { cant_be_blocked: [1] };
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    renderPermanent();

    expect(screen.getByLabelText("Can't be blocked")).toBeInTheDocument();
  });

  it("renders the engine-authored temporary can't-be-blocked badge on a face-down recipient without source attribution", () => {
    const gameState = makeState();
    gameState.objects[1].face_down = true;
    gameState.derived = { cant_be_blocked: [1], temporary_cant_be_blocked: { 1: null } };
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    renderPermanent();

    expect(screen.getByLabelText("Can't be blocked")).toBeInTheDocument();
    expect(screen.queryByText(/\(from/)).not.toBeInTheDocument();
  });

  it("does not render a temporary can't-be-blocked badge without an engine marker", () => {
    const gameState = makeState();
    gameState.derived = {};
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    renderPermanent();

    expect(screen.getByLabelText("Test Creature")).toBeInTheDocument();
    expect(screen.queryByLabelText("Can't be blocked")).not.toBeInTheDocument();
  });

  it("never badges a face-down permanent as a copy (CR 708.2)", () => {
    // A face-down permanent has only the characteristics its face-down rules
    // grant, so surfacing "Copy" would leak what it really is. The engine omits
    // it from the projection; the client keeps its own guard so neither side
    // alone can leak it.
    const gameState = makeState();
    gameState.objects[1].face_down = true;
    gameState.derived = { copied_permanents: [1] };
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    renderPermanent();

    expect(screen.queryByText("Copy")).not.toBeInTheDocument();
  });

  it("renders only the engine-classified battlefield keyword badges", () => {
    const gameState = makeState();
    gameState.objects[1].keywords = ["Flying", "Ravenous", "Evoke"];
    gameState.derived = {
      battlefield_keyword_badges: { 1: ["Flying"] },
    };
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
    usePreferencesStore.setState({ showKeywordStrip: true });

    renderPermanent();

    expect(screen.getByTestId("keyword-strip")).toHaveTextContent("Flying");
    expect(screen.getByTestId("keyword-strip")).not.toHaveTextContent("Ravenous");
    expect(screen.getByTestId("keyword-strip")).not.toHaveTextContent("Evoke");
  });

  // CR 732.2a / CR 701.34a: an accepted counter-growth ∞ loop (Kilo proliferate → Pentad
  // charge) annotates the pumped row in `derived.counter_display`; the pill renders ∞
  // instead of the (still-finite) real count. Matched pair — the ONLY difference between the
  // two cases is the row's `magnitude`, so it is the discriminator.
  it("renders ∞ on a counter the engine marks as unbounded", () => {
    const gameState = makeState();
    gameState.objects[1].counters = { charge: 4 };
    gameState.derived = {
      counter_display: { 1: { pills: [{ counter: "charge", count: 4, magnitude: "Unbounded" }] } },
    };
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    const { container } = renderPermanent();

    expect(container.textContent).toContain("∞");
    expect(container.textContent).not.toContain("x4");
  });

  it("renders the finite ×N count when the counter is not marked unbounded", () => {
    const gameState = makeState();
    gameState.objects[1].counters = { charge: 4 };
    // `magnitude` omitted exactly as the engine omits the serde default.
    gameState.derived = { counter_display: { 1: { pills: [{ counter: "charge", count: 4 }] } } };
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    const { container } = renderPermanent();

    expect(container.textContent).toContain("x4");
    expect(container.textContent).not.toContain("∞");
  });

  // THE NO-FALLBACK MATCHED PAIR. `counter_display` is the SINGLE authority: an object carrying
  // real counters with no projection entry renders NO pill. This is the only test that catches a
  // render site re-introducing `Object.entries(obj.counters)`, and it is worthless without its
  // positive twin — alone it would also pass on a component that rendered nothing at all.
  it("renders no pill for an object with counters but no projection entry", () => {
    const gameState = makeState();
    gameState.objects[1].counters = { charge: 4 };
    gameState.derived = {}; // a frame that arrived without `derived.counter_display`
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    const { container } = renderPermanent();

    expect(container.textContent).not.toContain("x4");
    expect(container.textContent).not.toContain("∞");
  });

  it("renders the pill for that SAME object once the projection carries it", () => {
    const gameState = makeState();
    gameState.objects[1].counters = { charge: 4 };
    gameState.derived = { counter_display: { 1: { pills: [{ counter: "charge", count: 4 }] } } };
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    const { container } = renderPermanent();

    expect(container.textContent).toContain("x4");
  });

  // THE `0 -> 1` ROW, at the render layer. The engine registers a pumped pair while the
  // object carries NONE of that counter, so the row's `count` is 0 and there is no entry in
  // `obj.counters` to join back to. Before the channel published rows, this pill could not be
  // drawn at all — the display had nothing to hang `∞` on.
  //
  // DISCRIMINATOR: the finite `burden` pill in the SAME frame proves the component did not
  // simply start rendering `∞` for everything, and it is a positive reach-guard for the
  // negative assertion below — without it, "no x0" would pass on a card that rendered no
  // pills whatsoever.
  it("renders ∞ for a marked counter the object does not yet carry (count 0)", () => {
    const gameState = makeState();
    gameState.objects[1].counters = { burden: 2 };
    gameState.derived = {
      counter_display: {
        1: {
          pills: [
            { counter: "charge", count: 0, magnitude: "Unbounded" },
            { counter: "burden", count: 2 },
          ],
        },
      },
    };
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    const { container } = renderPermanent();

    expect(container.textContent).toContain("∞");
    expect(container.textContent).toContain("x2");
    expect(container.textContent).not.toContain("x0");
  });

  // CR 306.5c: a planeswalker's loyalty IS its loyalty-counter count, so the engine routes that
  // row to `loyalty` rather than to `pills` and an `Unbounded` one means the TOTAL is unbounded.
  // The partition is engine-side now, so there is no ∞ pill beside a stale numeric badge.
  it("renders ∞ on the loyalty TOTAL badge when the engine marks a loyalty row", () => {
    const gameState = makeState();
    gameState.objects[1].loyalty = 4;
    gameState.objects[1].counters = { loyalty: 4 };
    gameState.derived = {
      counter_display: { 1: { loyalty: { counter: "loyalty", count: 4, magnitude: "Unbounded" } } },
    };
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    const { container } = renderPermanent();
    const badge = container.querySelector('[data-loyalty-badge="total"]') as HTMLElement;

    expect(badge).toBeInTheDocument();
    expect(badge.textContent).toContain("∞");
    expect(badge.textContent).not.toContain("4");
    // The DOM attribute stays truthful — selectors keep working.
    expect(badge.getAttribute("data-loyalty-value")).toBe("4");
  });

  it("renders the finite loyalty total when no loyalty row is marked", () => {
    const gameState = makeState();
    gameState.objects[1].loyalty = 4;
    gameState.objects[1].counters = { loyalty: 4 };
    gameState.derived = {
      counter_display: { 1: { loyalty: { counter: "loyalty", count: 4 } } },
    };
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    const { container } = renderPermanent();
    const badge = container.querySelector('[data-loyalty-badge="total"]') as HTMLElement;

    expect(badge).toBeInTheDocument();
    expect(badge.textContent).toContain("4");
    expect(badge.textContent).not.toContain("∞");
  });

  it("lifts the permanent tree above siblings while keeping attachments behind the host", () => {
    const { container } = renderPermanent();
    const host = container.querySelector('[data-object-id="1"]') as HTMLElement;
    const attachment = container.querySelector('[data-object-id="2"]') as HTMLElement;
    const attachmentLayer = attachment.parentElement as HTMLElement;
    const nestedAttachment = container.querySelector('[data-object-id="3"]') as HTMLElement;
    const nestedAttachmentLayer = nestedAttachment.parentElement as HTMLElement;

    expect(host.style.zIndex).toBe("");
    expect(attachmentLayer.style.zIndex).toBe("5");
    expect(nestedAttachmentLayer.style.zIndex).toBe("5");

    fireEvent.pointerEnter(host, { pointerType: "mouse" });

    expect(host.style.zIndex).toBe("80");
    expect(attachmentLayer.style.zIndex).toBe("5");
    expect(nestedAttachmentLayer.style.zIndex).toBe("5");
  });

  it("keeps the attachment tree lifted while a nested attachment is hovered", () => {
    const { container } = renderPermanent();
    const host = container.querySelector('[data-object-id="1"]') as HTMLElement;
    const nestedAttachment = container.querySelector('[data-object-id="3"]') as HTMLElement;

    fireEvent.pointerEnter(nestedAttachment, { pointerType: "mouse" });

    expect(host.style.zIndex).toBe("80");
  });

  it("does not recursively render cyclic attachment graphs", () => {
    const gameState = makeState();
    gameState.objects[1].attached_to = { type: "Object", data: 2 };
    gameState.objects[2].attachments = [1];
    gameState.objects[3].attachments = [];
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    const { container } = renderPermanent();

    expect(container.querySelectorAll('[data-object-id="1"]')).toHaveLength(1);
    expect(container.querySelectorAll('[data-object-id="2"]')).toHaveLength(1);
  });

  it("keeps multiple direct attachments collapsed through hover and inspection, but expands when selected", () => {
    const secondEquipment = makeObject({
      id: 4,
      card_id: 400,
      attached_to: { type: "Object", data: 1 },
      name: "Second Equipment",
      power: null,
      toughness: null,
      base_power: null,
      base_toughness: null,
      card_types: { supertypes: [], core_types: ["Artifact"], subtypes: ["Equipment"] },
      color: [],
      base_color: [],
    });
    const gameState = makeState();
    gameState.objects[1].attachments = [2, 4];
    gameState.objects[4] = secondEquipment;
    gameState.battlefield = [1, 2, 3, 4];
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    const { container } = renderPermanent();

    expect(container.querySelector('[data-object-id="2"]')).not.toBeNull();
    expect(container.querySelector('[data-object-id="4"]')).toBeNull();
    expect(container.textContent).toContain("+1");

    act(() => {
      useUiStore.setState({ inspectedObjectId: 1 });
    });
    expect(container.querySelector('[data-object-id="4"]')).toBeNull();

    fireEvent.pointerEnter(container.querySelector('[data-object-id="1"]') as HTMLElement, { pointerType: "mouse" });
    expect(container.querySelector('[data-object-id="4"]')).toBeNull();

    act(() => {
      useUiStore.setState({ selectedObjectId: 1 });
    });
    expect(container.querySelector('[data-object-id="4"]')).not.toBeNull();
  });

  it("opens the attachment fan from the collapsed-count button without selecting the host", () => {
    const gameState = makeState();
    gameState.objects[1].attachments = [2, 4];
    gameState.objects[4] = makeObject({
      id: 4,
      card_id: 400,
      attached_to: { type: "Object", data: 1 },
      attachments: [],
      name: "Second Equipment",
      power: null,
      toughness: null,
      base_power: null,
      base_toughness: null,
      card_types: { supertypes: [], core_types: ["Artifact"], subtypes: ["Equipment"] },
      color: [],
      base_color: [],
    });
    gameState.battlefield = [1, 2, 3, 4];
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
    act(() => {
      useUiStore.setState({ attachmentFanHostId: null, inspectedObjectId: null });
    });

    renderPermanent();

    const button = screen.getByRole("button", { name: "Show 1 hidden attached card" });

    // pointerdown must be stopped so the host motion.div never captures the
    // pointer (useLongPress.setPointerCapture) and retargets the click to the
    // host — which would fire card selection instead of opening the fan.
    fireEvent.pointerDown(button);
    fireEvent.click(button);

    // Routes to the fan-host state (uiStore), clears any covering preview, and
    // never selects the host because the control stops propagation.
    expect(useUiStore.getState().attachmentFanHostId).toBe(1);
    expect(useUiStore.getState().selectedObjectId).toBeNull();
    expect(useUiStore.getState().inspectedObjectId).toBeNull();
  });

  it("opens the attachment fan from the single-attachment button without selecting the host", () => {
    act(() => {
      useUiStore.setState({ attachmentFanHostId: null, inspectedObjectId: null });
    });

    renderPermanent();

    const button = screen.getByRole("button", {
      name: "View Test Creature's 2 attached cards",
    });

    fireEvent.pointerDown(button);
    fireEvent.click(button);

    expect(useUiStore.getState().attachmentFanHostId).toBe(1);
    expect(useUiStore.getState().selectedObjectId).toBeNull();
    expect(useUiStore.getState().inspectedObjectId).toBeNull();
  });

  it("labels the fan control from the projected card count rather than raw attachments", () => {
    const gameState = makeState();
    gameState.objects[1].attachments = [2, 4];
    gameState.objects[4] = makeObject({
      id: 4,
      card_id: 400,
      attached_to: { type: "Object", data: 1 },
      attachments: [],
      name: "Second Equipment",
      power: null,
      toughness: null,
      base_power: null,
      base_toughness: null,
      card_types: { supertypes: [], core_types: ["Artifact"], subtypes: ["Equipment"] },
      color: [],
      base_color: [],
    });
    gameState.battlefield = [1, 2, 3, 4];
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      viewerInteraction: membershipOnly([2]),
    });
    act(() => {
      useUiStore.setState({ selectedObjectId: 1 });
    });

    renderPermanent();

    expect(screen.getByRole("button", {
      name: "View Test Creature's attached card",
    })).toBeInTheDocument();
  });

  it("keeps the single-attachment control readable at compact card sizes", () => {
    renderPermanent();

    const button = screen.getByRole("button", {
      name: "View Test Creature's 2 attached cards",
    });

    expect(button).toHaveStyle({
      width: "clamp(20px, calc(var(--card-w) * 0.22), 28px)",
      height: "clamp(20px, calc(var(--card-w) * 0.22), 28px)",
      fontSize: "clamp(12px, calc(var(--card-w) * 0.12), 15px)",
    });
  });

  /**
   * Plan rows V10–V13 + V10c — hunk B: a host whose OWN click offers nothing,
   * over an attachment the engine published affordances for, falls through to
   * the full-card chooser instead of demanding a pixel-accurate click on a
   * ~22px peek rendered BELOW the host (CR 301.5 / CR 303.4 — an attachment is
   * its own object).
   *
   * EVIDENCE LABELS (plan §7.2): MEASURED = this PR ran that mutant arm and the
   * quoted line is what flipped. QUOTED = verbatim from a named plan log.
   * An untagged comment is prose and is evidence for nothing.
   */
  function clickHost(container: HTMLElement) {
    fireEvent.click(container.querySelector('[data-object-id="1"]') as HTMLElement);
  }

  // V10 — the positive control for hunk B firing at all: an inert host over an
  // actionable attachment opens the fan AND selects the host.
  // QUOTED (plan §6.10, `.review3r4-v13.log`):
  //   `PV13d[inert host + actionable attachment] dispatch=none fanHostId=1 selectedObjectId=1`
  // MEASURED (drop side — hunk B deleted): `expected null to be 1`.
  // MEASURED (always side — the `.some(...)` predicate forced constant-true):
  //   V11's `expected 1 to be null` flips instead; on THIS fixture an
  //   always-true predicate agrees, which is exactly why V11 exists.
  it("opens the fan from an inert host when an attachment is actionable", () => {
    act(() => {
      useUiStore.setState({ attachmentFanHostId: null, selectedObjectId: null });
    });

    const { container } = renderPermanent(new Set(), new Set(), new Set(), new Set([2]));
    clickHost(container);

    expect(useUiStore.getState().attachmentFanHostId).toBe(1);
    expect(useUiStore.getState().selectedObjectId).toBe(1);
    expect(dispatchAction).not.toHaveBeenCalled();
  });

  // V10 (mana arm) — the predicate's SECOND disjunct. `activatableObjectIds` and
  // `manaTappableObjectIds` are two independent affordance sets and hunk B reads
  // both; without this arm the `|| manaTappableObjectIds.has(attachId)` half
  // could be deleted and every other row here would stay green.
  // MEASURED (drop side — the mana disjunct deleted): `expected null to be 1`.
  it("opens the fan when the attachment is mana-tappable rather than activatable", () => {
    act(() => {
      useUiStore.setState({ attachmentFanHostId: null, selectedObjectId: null });
    });

    const { container } = renderPermanent(
      new Set(), new Set(), new Set(), new Set(), new Set(), new Set([2]),
    );
    clickHost(container);

    expect(useUiStore.getState().attachmentFanHostId).toBe(1);
  });

  // V10c — hunk B's selection is UNCONDITIONAL, deliberately unlike the plain
  // click fallback which TOGGLES. Only a fixture whose host is ALREADY selected
  // can see the difference; V10 above passes under both implementations.
  // QUOTED (plan §6.10):
  //   `PC1[before=null]  unconditional=401 toggle=401 DISCRIMINATES=false`
  //   `PC1[before=401] unconditional=401 toggle=null  DISCRIMINATES=true`
  // MEASURED (drop side — `selectObject(objectId)` replaced by the toggle form
  //   `selectObject(isSelected ? null : objectId)`): `expected null to be 1`.
  it("keeps the host selected when the fan is re-opened from an already-selected host", () => {
    act(() => {
      useUiStore.setState({ attachmentFanHostId: null, selectedObjectId: 1 });
    });

    const { container } = renderPermanent(new Set(), new Set(), new Set(), new Set([2]));
    clickHost(container);

    // A toggle would strand the fan open over a host that just lost its ring,
    // its attachment expansion and its exile-link expansion.
    expect(useUiStore.getState().selectedObjectId).toBe(1);
    expect(useUiStore.getState().attachmentFanHostId).toBe(1);
  });

  // V11 — the negative side: no actionable attachment ⇒ the pre-existing
  // fallback still owns the click. PAIR-ONLY (§7.1) — the row claims the change
  // did NOTHING on this fixture, so no drop-the-fix mutant can be visible on it
  // by construction; it is counted as always-side coverage only.
  // QUOTED (plan §6.10): `PV13e[inert host + inert attachment] fanHostId=null selectedObjectId=1`
  // MEASURED (always side — predicate forced constant-true): `expected 1 to be null`.
  it("leaves the plain-click fallback alone when no attachment is actionable", () => {
    act(() => {
      useUiStore.setState({ attachmentFanHostId: null, selectedObjectId: null });
    });

    const { container } = renderPermanent();
    clickHost(container);

    expect(useUiStore.getState().attachmentFanHostId).toBeNull();
    expect(useUiStore.getState().selectedObjectId).toBe(1);
  });

  // V12 — the predicate's SOURCE. Hunk B reads the shared affordance sets, never
  // the raw `legalActionsByObject` bucket, so it can never offer what the board
  // itself would not (the sets already carry the timing/seat gates). The bucket
  // here is populated exactly as in V10; only the affordance set is empty.
  // MEASURED (drop side — `activatableObjectIds.has(attachId)` swapped for a raw
  //   `collectObjectActions(legalActionsByObject, attachId).length > 0` check):
  //   `expected 1 to be null` — the fan opens at a state the board gates off.
  it("ignores a populated action bucket that the affordance sets exclude", () => {
    act(() => {
      useUiStore.setState({ attachmentFanHostId: null, selectedObjectId: null });
    });
    useGameStore.setState({
      legalActionsByObject: {
        "2": [{ type: "ActivateAbility", data: { source_id: 2, ability_index: 0 } }],
      },
    });

    const { container } = renderPermanent();
    clickHost(container);

    expect(useUiStore.getState().attachmentFanHostId).toBeNull();
    // REACH GUARD, paired with the negative above: a bare "no fan" assertion
    // would also pass if the click never landed at all. Selection proves the
    // handler ran and fell through to the plain-click fallback.
    expect(useUiStore.getState().selectedObjectId).toBe(1);
  });

  // V13 (mobile arm) — hunk B sits ABOVE the `isMobile` branch, so on a narrow
  // viewport a host whose attachment is actionable opens the fan instead of the
  // sticky preview. That is a deliberate trade (long-press still inspects, and
  // the fan renders large legible cards), and it is pinned here so a future
  // reorder cannot revert it silently.
  // MEASURED (drop side — hunk B deleted): `expected null to be 1`, and the
  //   preview arm takes the click instead.
  it("prefers the fan over the mobile sticky preview when an attachment is actionable", () => {
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 800 });
    act(() => {
      useUiStore.setState({
        attachmentFanHostId: null,
        selectedObjectId: null,
        inspectedObjectId: null,
      });
    });

    try {
      const { container } = renderPermanent(new Set(), new Set(), new Set(), new Set([2]));
      clickHost(container);

      expect(useUiStore.getState().attachmentFanHostId).toBe(1);
      // The arm hunk B pre-empts, asserted as the pair: no sticky preview.
      expect(useUiStore.getState().inspectedObjectId).toBeNull();
    } finally {
      Object.defineProperty(window, "innerWidth", { configurable: true, value: 1024 });
    }
  });

  // V13 — hunk B is placed LAST, so it can never pre-empt the host's own
  // target / activation / undo intent. Every arm below holds the actionable
  // attachment FIXED and varies only the host's own affordance, so a passing
  // arm is a statement about branch ORDER and nothing else.
  // QUOTED (plan §6.10, `.review3r4-v13.log`):
  //   `PV13a[valid target + actionable attachment]      dispatch=ChooseTarget      fanHostId=null`
  //   `PV13b[activatable host + actionable attachment]  dispatch=ActivateAbility   fanHostId=null`
  //   `PV13c[undoable tap + actionable attachment]      dispatch=UntapLandForMana  fanHostId=null`
  // MEASURED (drop side — hunk B moved ABOVE `isValidTarget`): the PV13a arm
  //   flips to `expected "spy" to be called with ... ChooseTarget`, and
  //   `fanHostId` reads 1 instead of null.
  // V10 is the positive control proving these arms are not uniformly inert.
  it("never pre-empts the host's own target, activation or undo intent", () => {
    const actionableAttachment = new Set([2]);

    act(() => {
      useUiStore.setState({ attachmentFanHostId: null, selectedObjectId: null });
    });
    const targetArm = renderPermanent(new Set([1]), new Set(), new Set(), actionableAttachment);
    clickHost(targetArm.container);
    expect(dispatchAction).toHaveBeenCalledWith({
      type: "ChooseTarget",
      data: { target: { Object: 1 } },
    });
    expect(useUiStore.getState().attachmentFanHostId).toBeNull();
    cleanup();
    vi.mocked(dispatchAction).mockClear();

    const hostAbility = { type: "ActivateAbility", data: { source_id: 1, ability_index: 0 } } as const;
    useGameStore.setState({ legalActionsByObject: { "1": [hostAbility] } });
    const activateArm = renderPermanent(
      new Set(), new Set(), new Set(), new Set([1, 2]),
    );
    clickHost(activateArm.container);
    expect(dispatchAction).toHaveBeenCalledWith(hostAbility);
    expect(useUiStore.getState().attachmentFanHostId).toBeNull();
    cleanup();
    vi.mocked(dispatchAction).mockClear();
    useGameStore.setState({ legalActionsByObject: {} });

    const undoArm = renderPermanent(
      new Set(), new Set(), new Set(), actionableAttachment, new Set([1]),
    );
    clickHost(undoArm.container);
    expect(dispatchAction).toHaveBeenCalledWith({
      type: "UntapLandForMana",
      data: { object_id: 1 },
    });
    expect(useUiStore.getState().attachmentFanHostId).toBeNull();
  });

  /**
   * Reported from a real game (Priority during Declare Blockers): Slumbering
   * Keepguard's `{2}{W}` was unreachable while Cooped Up sat on it. Every click
   * on the host — centre, edge, corner — opened the attachment chooser, and the
   * chooser cannot pick the host by design (`AttachmentFan.tsx:241`,
   * `id !== host.id`), so the host's own ability had no path at all.
   *
   * This is the SAME branch-order invariant the V13 arm above pins, on the OTHER
   * source. Hunk B reads the affordance sets and sits LAST; the
   * `attachmentsActionable` branch reads `viewerInteraction.attachmentFans` and
   * USED TO sit above the activation branch, on the premise stated in its own
   * comment — "the host is not a legal choice" — which is unchecked and false
   * whenever Priority publishes the host as activatable too. It now sits below
   * the host's own intent (`PermanentCard.tsx:694` activation, `:727` the fan
   * branch, `:747` hunk B), which is what this row pins.
   *
   * COVERAGE GAP this closed: before this row, every interaction-fan test in
   * this file called `renderPermanent()` with no arguments, so
   * `activatableObjectIds` was empty in all of them and no arm held a fan AND an
   * activatable host at once — which is precisely what
   * `HumanResponseModel::ExactCandidates` publishes during Priority.
   */
  const keepguardPump = {
    type: "ActivateAbility",
    data: { source_id: 1, ability_index: 0 },
  } as const;
  const coopedUpExile = {
    type: "ActivateAbility",
    data: { source_id: 2, ability_index: 0 },
  } as const;

  it("activates the host's own ability while an attachment is a live interaction choice", () => {
    const gameState = keepguardUnderCoopedUp();
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      viewerInteraction: interactionForAttachedObject(2),
      legalActionsByObject: { "1": [keepguardPump], "2": [coopedUpExile] },
    });
    act(() => {
      useUiStore.setState({ attachmentFanHostId: null, selectedObjectId: null });
    });

    const { container } = renderPermanent(new Set(), new Set(), new Set(), new Set([1, 2]));
    clickHost(container);

    expect(dispatchAction).toHaveBeenCalledWith(keepguardPump);
    expect(useUiStore.getState().attachmentFanHostId).toBeNull();
    // BRANCH DISCRIMINATOR: hunk B (`:747`) selects the host unconditionally, so
    // a failure caused by THAT branch would leave `selectedObjectId === 1`.
    // MEASURED on the DROP side (the fan branch restored above activation):
    //   `dispatched: [] fanHostId: 1 selectedObjectId: null`
    // — no selection, which only the `:727` branch produces, so the red state is
    // attributable to it and not to hunk B. It also matches the reported
    // screenshot: the host rendered dimmed and WITHOUT a selection ring. Left as
    // evidence rather than an assertion: a fix must reach the ability, and
    // whether it also selects the host is not this row's business.
  });

  // REACHABILITY GUARD for the reorder above. Handing the click back to the host
  // is only correct if the attachments keep a route of their own. Two attachments
  // that are both actionable auto-expand the stack, so `hiddenAttachmentCount`
  // is 0 and the `+N` control is absent; the `⧉` control used to require exactly
  // ONE attachment, leaving this state with no entry point at all and each Aura
  // reachable only through a ~22px peek behind the host face.
  //
  // The reported board was Slumbering Keepguard under TWO Bestial Bloodline, but
  // the fixture deliberately uses Cage of Hands (`{1}{W}: Return this Aura to its
  // owner's hand`) as the second Aura: Bestial Bloodline's only activated ability
  // works from the graveyard, so its appearing as a live choice on the
  // battlefield is a separate engine defect (CR 113.6b). Pinning this row on two
  // Auras that are legitimately activatable keeps it green once that is fixed.
  it("keeps an explicit fan route on a host whose several attachments are all expanded", () => {
    const gameState = keepguardUnderCoopedUp();
    const secondAura = buildGameObject({
      ...gameState.objects[2],
      id: 3,
      card_id: 300,
      name: "Cage of Hands",
    });
    gameState.objects[1] = { ...gameState.objects[1], attachments: [2, 3] };
    gameState.objects[3] = secondAura;
    gameState.battlefield = [1, 2, 3];
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      viewerInteraction: interactionForAttachedObjects([2, 3]),
      legalActionsByObject: { "1": [keepguardPump], "2": [coopedUpExile] },
    });
    act(() => {
      useUiStore.setState({ attachmentFanHostId: null, selectedObjectId: null });
    });

    const { container } = renderPermanent(new Set(), new Set(), new Set(), new Set([1, 2, 3]));

    // Both attachments are on the board, so nothing is hidden and no `+N` exists.
    expect(container.querySelector('[data-object-id="2"]')).not.toBeNull();
    expect(container.querySelector('[data-object-id="3"]')).not.toBeNull();

    // Stated as its own claim so the red state reads "no explicit route exists"
    // rather than as a selector that stopped matching.
    // MEASURED (drop side — the control gated back on `attachments.length === 1`):
    //   `Unable to find a label with the text of: /Slumbering Keepguard/`.
    const fanControl = screen.queryByLabelText(/Slumbering Keepguard/, { selector: "button" });
    expect(fanControl).not.toBeNull();

    fireEvent.click(fanControl as HTMLElement);
    expect(useUiStore.getState().attachmentFanHostId).toBe(1);
  });

  // The pair, and the guard on the reorder: when the host itself offers nothing
  // the fan MUST still take the click, because the Aura's ability is only
  // reachable through it.
  //
  // `selectedObjectId` is what makes this arm discriminating, and it is not
  // decoration. `attachmentFanHostId === 1` alone is NOT enough: the
  // affordance-set branch below (hunk B) produces the very same fan from the
  // very same fixture, so deleting the branch under test left this row green.
  // Only hunk B calls `selectObject`, so requiring NO selection is what proves
  // the fan came from the interaction branch.
  // MEASURED (drop side — the `attachmentsActionable` branch deleted):
  //   `expected 1 to be null` on the selection assertion; hunk B answered the
  //   click instead. Before that assertion existed the mutant was invisible here.
  it("still opens the fan when only the attachment is a live interaction choice", () => {
    const gameState = keepguardUnderCoopedUp();
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      viewerInteraction: interactionForAttachedObject(2),
      legalActionsByObject: { "2": [coopedUpExile] },
    });
    act(() => {
      useUiStore.setState({ attachmentFanHostId: null, selectedObjectId: null });
    });

    const { container } = renderPermanent(new Set(), new Set(), new Set(), new Set([2]));
    clickHost(container);

    expect(useUiStore.getState().attachmentFanHostId).toBe(1);
    expect(dispatchAction).not.toHaveBeenCalled();
    expect(useUiStore.getState().selectedObjectId).toBeNull();
  });

  // The two halves of the `⧉` gate, each pinned on its own. Without these the
  // gate could be weakened to either disjunct alone and the whole board suite
  // stayed green: dropping `attachmentsExpanded` puts a `⧉` on every permanent
  // that has attachments including collapsed ones (two competing controls), and
  // dropping `obj.attachments.length > 0` puts one on every permanent on the
  // board, attachments or not.
  it("hands the collapsed state to the +N control and renders no second route", () => {
    const gameState = keepguardUnderCoopedUp();
    const secondAura = buildGameObject({
      ...gameState.objects[2],
      id: 3,
      card_id: 300,
      name: "Cage of Hands",
    });
    gameState.objects[1] = { ...gameState.objects[1], attachments: [2, 3] };
    gameState.objects[3] = secondAura;
    gameState.battlefield = [1, 2, 3];
    // No interaction fan and no selection, so nothing expands the stack.
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
    act(() => {
      useUiStore.setState({ attachmentFanHostId: null, selectedObjectId: null });
    });

    renderPermanent();

    expect(screen.queryByLabelText(/Slumbering Keepguard/, { selector: "button" })).toBeNull();
    // Reach guard: proves the fixture really is collapsed rather than simply
    // missing both controls for some unrelated reason.
    expect(screen.getByText("+1")).toBeInTheDocument();
  });

  it("renders no fan route on a permanent that has no attachments", () => {
    const gameState = keepguardUnderCoopedUp();
    gameState.objects[1] = { ...gameState.objects[1], attachments: [] };
    gameState.battlefield = [1];
    // Nothing is attached, so the engine publishes no membership for this host —
    // and the badge is gated on that, not on a scan of the snapshot.
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      viewerInteraction: membershipOnly([]),
    });

    const { container } = renderPermanent();

    expect(screen.queryByLabelText(/Slumbering Keepguard/, { selector: "button" })).toBeNull();
    expect(screen.queryByText(/^\+\d+$/)).toBeNull();
    // Reach guard, matching the row above: both assertions are negative, so
    // without this an early return (`PermanentCard.tsx:491`, `if (!obj)`) or any
    // fixture drift would satisfy them for the wrong reason.
    expect(container.querySelector('[data-object-id="1"]')).not.toBeNull();
  });

  it("refreshes the attachment fan when the engine clears host attachments", () => {
    const gameState = makeState();
    const host = gameState.objects[1];
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
    });
    useUiStore.setState({ attachmentFanHostId: 1 });

    const { queryAllByLabelText } = render(<AttachmentFan />);

    expect(queryAllByLabelText("Test Creature").length).toBeGreaterThan(0);
    expect(queryAllByLabelText("Test Equipment").length).toBeGreaterThan(0);

    act(() => {
      host.attachments = [];
      gameState.objects[2] = {
        ...gameState.objects[2],
        zone: "Graveyard",
        attached_to: null,
      };
      const nextState = {
        ...gameState,
        objects: { ...gameState.objects },
        battlefield: [1],
        graveyard: [2],
      };
      useGameStore.setState({
        gameState: nextState,
        waitingFor: nextState.waiting_for,
        // The engine republishes membership with the state it belongs to; the
        // fan follows that, not the raw `attachments` array.
        viewerInteraction: membershipOnly([]),
      });
    });

    // The last member left the battlefield, so the fan has nothing to spread and
    // tears itself down rather than lingering as a lone host card over the board.
    expect(queryAllByLabelText("Test Equipment")).toHaveLength(0);
    expect(queryAllByLabelText("Test Creature")).toHaveLength(0);
  });

  it("auto-expands collapsed attachments when one is a valid target", () => {
    // Regression: Moira Brown's "put a quest counter on target nonland
    // permanent you control" offers the host's attached Equipment/Auras as
    // targets. Collapsed behind the host they are unclickable, so the counter
    // lands on the host creature instead of the chosen attachment. A host with
    // an actionable attachment must open WITHOUT requiring a hover.
    const secondEquipment = makeObject({
      id: 4,
      card_id: 400,
      attached_to: { type: "Object", data: 1 },
      name: "Second Equipment",
      power: null,
      toughness: null,
      base_power: null,
      base_toughness: null,
      card_types: { supertypes: [], core_types: ["Artifact"], subtypes: ["Equipment"] },
      color: [],
      base_color: [],
    });
    const gameState = makeState();
    gameState.objects[1].attachments = [2, 4];
    gameState.objects[4] = secondEquipment;
    gameState.battlefield = [1, 2, 3, 4];
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      viewerInteraction: interactionForAttachedObject(4),
    });

    // Attachment 4 is a valid target — both attachments must render even though
    // the host is neither hovered nor inspected.
    const { container } = renderPermanent();

    expect(container.querySelector('[data-object-id="2"]')).not.toBeNull();
    expect(container.querySelector('[data-object-id="4"]')).not.toBeNull();
  });

  it("opens the full attachment chooser when a host has a targetable attachment", () => {
    // Regression: Rampaging Yao Guai can target Darksteel Plate while it is
    // attached beside Skullclamp on Bastion Protector. The targetable Plate
    // expands into only a narrow overlapping board peek, so clicking the host
    // must expose the fan's full-size cards and let the engine-authorized
    // target dispatch unambiguously.
    const darksteelPlate = makeObject({
      id: 4,
      card_id: 400,
      attached_to: { type: "Object", data: 1 },
      attachments: [],
      name: "Darksteel Plate",
      power: null,
      toughness: null,
      base_power: null,
      base_toughness: null,
      card_types: { supertypes: [], core_types: ["Artifact"], subtypes: ["Equipment"] },
      color: [],
      base_color: [],
    });
    const gameState = makeState();
    gameState.objects[1] = { ...gameState.objects[1], name: "Bastion Protector", attachments: [2, 4] };
    gameState.objects[2] = { ...gameState.objects[2], name: "Skullclamp" };
    gameState.objects[4] = darksteelPlate;
    gameState.battlefield = [1, 2, 3, 4];
    const waitingFor = buildTriggerTargetSelectionWaitingFor({
      data: {
        player: 0,
        target_slots: [],
        selection: buildTargetSelectionProgress({ current_legal_targets: [{ Object: 4 }] }),
      },
    });
    gameState.waiting_for = waitingFor;
    useGameStore.setState({
      gameState,
      waitingFor,
      viewerInteraction: interactionForAttachedObject(4),
    });
    useUiStore.setState({ attachmentFanHostId: null });

    const { container } = renderPermanent();
    render(<AttachmentFan />);

    fireEvent.click(container.querySelector('[data-object-id="1"]') as HTMLElement);

    const fan = document.querySelector("[data-attachment-fan]");
    expect(fan).not.toBeNull();
    const darksteelCard = fan?.querySelector('[aria-label="Darksteel Plate"]') as HTMLElement;
    expect(darksteelCard).not.toBeNull();

    fireEvent.click(darksteelCard);

    expect(dispatchInteraction).toHaveBeenCalledWith({
      interactionId: "attachment-interaction",
      response: {
        type: "choose",
        data: { choiceId: "attachment-4" },
      },
    });
  });

  it("submits each attachment's engine-authored response independently", () => {
    const secondEquipment = makeObject({
      id: 4,
      card_id: 400,
      attached_to: { type: "Object", data: 1 },
      attachments: [],
      name: "Second Equipment",
      power: null,
      toughness: null,
      base_power: null,
      base_toughness: null,
      card_types: { supertypes: [], core_types: ["Artifact"], subtypes: ["Equipment"] },
      color: [],
      base_color: [],
    });
    const gameState = makeState();
    gameState.objects[1].attachments = [2, 4];
    gameState.objects[4] = secondEquipment;
    gameState.battlefield = [1, 2, 3, 4];
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      viewerInteraction: interactionForAttachedObjects([2, 4]),
    });
    useUiStore.setState({ attachmentFanHostId: 1 });
    render(<AttachmentFan />);

    const fan = document.querySelector("[data-attachment-fan]") as HTMLElement;
    fireEvent.click(fan.querySelector('[aria-label="Test Equipment"]') as HTMLElement);
    expect(dispatchInteraction).toHaveBeenCalledWith({
      interactionId: "attachment-interaction",
      response: { type: "choose", data: { choiceId: "attachment-2" } },
    });

    fireEvent.click(fan.querySelector('[aria-label="Second Equipment"]') as HTMLElement);
    expect(dispatchInteraction).toHaveBeenCalledWith({
      interactionId: "attachment-interaction",
      response: { type: "choose", data: { choiceId: "attachment-4" } },
    });
  });

  it("auto-expands collapsed attachments when one is activatable (re-equip)", () => {
    // Regression: an attached Equipment whose Equip ability is activatable must
    // be reachable so it can be moved to another creature. Collapsed behind the
    // host it cannot be clicked, so equip appears stuck once attached.
    const secondEquipment = makeObject({
      id: 4,
      card_id: 400,
      attached_to: { type: "Object", data: 1 },
      name: "Second Equipment",
      power: null,
      toughness: null,
      base_power: null,
      base_toughness: null,
      card_types: { supertypes: [], core_types: ["Artifact"], subtypes: ["Equipment"] },
      color: [],
      base_color: [],
    });
    const gameState = makeState();
    gameState.objects[1].attachments = [2, 4];
    gameState.objects[4] = secondEquipment;
    gameState.battlefield = [1, 2, 3, 4];
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      viewerInteraction: interactionForAttachedObject(4),
    });

    const { container } = renderPermanent();

    expect(container.querySelector('[data-object-id="2"]')).not.toBeNull();
    expect(container.querySelector('[data-object-id="4"]')).not.toBeNull();
  });

  it("auto-expands collapsed attachments when one has an undoable mana tap", () => {
    // Regression: an attachment tapped for mana that can still be untapped
    // (undo) is actionable. Collapsed behind its host the undo affordance is
    // unclickable, stranding the tapped mana source.
    const secondEquipment = makeObject({
      id: 4,
      card_id: 400,
      attached_to: { type: "Object", data: 1 },
      name: "Second Equipment",
      power: null,
      toughness: null,
      base_power: null,
      base_toughness: null,
      card_types: { supertypes: [], core_types: ["Artifact"], subtypes: ["Equipment"] },
      color: [],
      base_color: [],
    });
    const gameState = makeState();
    gameState.objects[1].attachments = [2, 4];
    gameState.objects[4] = secondEquipment;
    gameState.battlefield = [1, 2, 3, 4];
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      viewerInteraction: interactionForAttachedObject(4),
    });

    const { container } = renderPermanent();

    expect(container.querySelector('[data-object-id="2"]')).not.toBeNull();
    expect(container.querySelector('[data-object-id="4"]')).not.toBeNull();
  });

  it("collapses multiple exiled cards hosted by one permanent until hover", () => {
    const exiledOne = makeObject({
      id: 10,
      card_id: 1000,
      zone: "Exile",
      name: "Exiled One",
      power: null,
      toughness: null,
      base_power: null,
      base_toughness: null,
    });
    const exiledTwo = makeObject({
      id: 11,
      card_id: 1001,
      zone: "Exile",
      name: "Exiled Two",
      power: null,
      toughness: null,
      base_power: null,
      base_toughness: null,
    });
    const gameState: GameState = {
      ...makeState(),
      objects: {
        ...makeState().objects,
        10: exiledOne,
        11: exiledTwo,
      },
      exile: [10, 11],
      exile_links: [
        { exiled_id: 10, source_id: 1, kind: "TrackedBySource" },
        { exiled_id: 11, source_id: 1, kind: "TrackedBySource" },
      ],
    };
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    const { container, queryByLabelText } = renderPermanent();

    expect(queryByLabelText("Exiled One")).not.toBeNull();
    expect(queryByLabelText("Exiled Two")).toBeNull();
    expect(container.textContent).toContain("+1");

    fireEvent.pointerEnter(container.querySelector('[data-object-id="1"]') as HTMLElement, { pointerType: "mouse" });

    expect(queryByLabelText("Exiled Two")).not.toBeNull();
  });

  it("restores host preview when moving from an attachment back to its host", () => {
    const { container } = renderPermanent();
    const host = container.querySelector('[data-object-id="1"]') as HTMLElement;
    const attachment = container.querySelector('[data-object-id="2"]') as HTMLElement;

    fireEvent.pointerEnter(host, { pointerType: "mouse" });
    expect(useUiStore.getState().inspectedObjectId).toBe(1);

    fireEvent.pointerEnter(attachment, { pointerType: "mouse" });
    expect(useUiStore.getState().inspectedObjectId).toBe(2);

    fireEvent.pointerLeave(attachment, { pointerType: "mouse", relatedTarget: host });
    expect(useUiStore.getState().inspectedObjectId).toBe(1);
    expect(useUiStore.getState().hoveredObjectId).toBe(1);
  });

  // A remote-desktop session does not enumerate the local mouse as a HID, so the
  // guest browser reports no hover-capable input — while still delivering honest
  // `pointerenter` events with `pointerType: "mouse"`. Gating on the capability
  // query killed battlefield hover outright on those hosts; gating on the event's
  // own pointerType is what makes this environment work.
  it("previews on a host whose hover capability queries all report false", () => {
    window.matchMedia = ((query: string) => ({
      matches: false,
      media: query,
      onchange: null,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(),
    })) as unknown as typeof window.matchMedia;
    Object.defineProperty(navigator, "maxTouchPoints", { configurable: true, value: 0 });
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 1920 });

    const { container } = renderPermanent();
    const host = container.querySelector('[data-object-id="1"]') as HTMLElement;

    fireEvent.pointerEnter(host, { pointerType: "mouse" });

    expect(useUiStore.getState().inspectedObjectId).toBe(1);
    // The card-lift/z-index hover state is a second, independent symptom: it
    // rides on hoverObject, which only this inline gate calls.
    expect(useUiStore.getState().hoveredObjectId).toBe(1);
    expect(host.style.zIndex).toBe("80");

    Object.defineProperty(window, "innerWidth", { configurable: true, value: 1024 });
  });

  it("clears the hover lift when the pointer leaves for a non-card element", () => {
    const { container } = renderPermanent();
    const host = container.querySelector('[data-object-id="1"]') as HTMLElement;

    fireEvent.pointerEnter(host, { pointerType: "mouse" });
    expect(useUiStore.getState().hoveredObjectId).toBe(1);

    fireEvent.pointerLeave(host, { pointerType: "mouse", relatedTarget: document.body });

    expect(useUiStore.getState().hoveredObjectId).toBeNull();
  });

  it("ignores a touch-synthesized pointer enter", () => {
    const { container } = renderPermanent();
    const host = container.querySelector('[data-object-id="1"]') as HTMLElement;

    fireEvent.pointerEnter(host, { pointerType: "touch" });

    expect(useUiStore.getState().inspectedObjectId).toBeNull();
    expect(useUiStore.getState().hoveredObjectId).toBeNull();
  });

  it("still cancels the long-press timer through the merged pointer-leave handler", () => {
    // useLongPress owns an onPointerLeave of its own and the hover handler wins
    // the key collision, so it must delegate — otherwise the timer survives the
    // leave and opens a sticky preview over whatever the pointer moved on to.
    vi.useFakeTimers();
    const { container } = renderPermanent();
    const host = container.querySelector('[data-object-id="1"]') as HTMLElement;

    fireEvent.pointerDown(host, {
      button: 0,
      clientX: 10,
      clientY: 10,
      isPrimary: true,
      pointerId: 1,
      pointerType: "touch",
    });
    fireEvent.pointerLeave(host, { pointerId: 1, pointerType: "touch", relatedTarget: document.body });
    act(() => vi.advanceTimersByTime(500));

    expect(useUiStore.getState().inspectedObjectId).toBeNull();
    expect(useUiStore.getState().previewSticky).toBe(false);
    vi.useRealTimers();
  });

  it("targets the attached permanent itself when the attachment is clicked", () => {
    const { container } = renderPermanent(new Set([2]));
    const attachment = container.querySelector('[data-object-id="2"]') as HTMLElement;

    fireEvent.click(attachment);

    expect(dispatchAction).toHaveBeenCalledWith({
      type: "ChooseTarget",
      data: { target: { Object: 2 } },
    });
  });

  it("dispatches a target click even when a stale combat mode lingers during target selection", () => {
    // Regression: a spell's TargetSelection must win over a leftover
    // `combatMode` UI flag. PermanentCard routed combat clicks on `combatMode`
    // alone — unlike GroupedPermanent, which also requires the matching combat
    // WaitingFor (`waitingFor.type === "DeclareBlockers"`). So a stale
    // `combatMode` from a just-finished combat step swallowed bounce/target
    // clicks: targets glowed (validTargetObjectIds) but the click hit the dead
    // blocker branch and `ChooseTarget` never fired. Reported on Chain of Vapor
    // cast during combat.
    const gameState: GameState = {
      ...makeState(),
      waiting_for: buildTargetSelectionWaitingFor({
        data: {
          player: 0,
          pending_cast: buildPendingCast({ object_id: 99 }),
          target_slots: [],
          selection: buildTargetSelectionProgress({
            current_legal_targets: [{ Object: 1 }],
          }),
        },
      }),
    };
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
    const staleBlockerHandler = vi.fn();
    useUiStore.setState({
      combatMode: "blockers",
      combatClickHandler: staleBlockerHandler,
    });

    const { container } = renderPermanent(new Set([1]));
    const permanent = container.querySelector('[data-object-id="1"]') as HTMLElement;

    fireEvent.click(permanent);

    expect(staleBlockerHandler).not.toHaveBeenCalled();
    expect(dispatchAction).toHaveBeenCalledWith({
      type: "ChooseTarget",
      data: { target: { Object: 1 } },
    });
  });

  it("directly targets the host (not the fan) when host and attachment are both legal targets", () => {
    act(() => {
      useUiStore.setState({ attachmentFanHostId: null });
    });
    // Both the host (1) and its attached Equipment (2) are legal targets. A
    // click on the host targets the host DIRECTLY — the fan is never forced.
    // (The attachment stays independently reachable via its peek, and the fan
    // is available on demand from the "⧉" badge — covered by the badge test.)
    const { container } = renderPermanent(new Set([1, 2]));
    const host = container.querySelector('[data-object-id="1"]') as HTMLElement;

    fireEvent.click(host);

    expect(useUiStore.getState().attachmentFanHostId).toBeNull();
    expect(dispatchAction).toHaveBeenCalledWith({
      type: "ChooseTarget",
      data: { target: { Object: 1 } },
    });
  });

  it("submits a single battlefield sacrifice choice from the board", () => {
    const gameState: GameState = {
      ...makeState(),
      waiting_for: {
        type: "EffectZoneChoice",
        data: {
          player: 0,
          cards: [1],
          count: 1,
          source_id: 99,
          effect_kind: "Sacrifice",
          zone: "Battlefield",
          destination: null,
        },
      },
    };
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
    });
    const { container } = renderPermanent(new Set(), new Set(), new Set([1]));
    const permanent = container.querySelector('[data-object-id="1"]') as HTMLElement;

    fireEvent.click(permanent);

    expect(dispatchAction).toHaveBeenCalledWith({
      type: "SelectCards",
      data: { cards: [1] },
    });
  });

  it("submits immediate board choices from the board", () => {
    const gameState: GameState = {
      ...makeState(),
      waiting_for: {
        type: "StationTarget",
        data: {
          player: 0,
          spacecraft_id: 9,
          eligible_creatures: [1],
        },
      },
    };
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
    });
    const { container } = renderPermanent(new Set(), new Set(), new Set([1]));
    const permanent = container.querySelector('[data-object-id="1"]') as HTMLElement;

    fireEvent.click(permanent);

    expect(dispatchAction).toHaveBeenCalledWith({
      type: "ActivateStation",
      data: { spacecraft_id: 9, creature_id: 1 },
    });
  });

  it("submits an authorized untap decision from the board", () => {
    const gameState: GameState = {
      ...makeState(),
      turn_decision_controller: 0,
      active_player: 1,
      waiting_for: { type: "UntapChoice", data: { player: 1, candidates: [1] } },
    };
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
    const { container } = renderPermanent(new Set(), new Set(), new Set([1]));

    fireEvent.click(container.querySelector('[data-object-id="1"]') as HTMLElement);

    expect(dispatchAction).toHaveBeenCalledWith({
      type: "ChooseUntap",
      data: { object_id: 1, untap: true },
    });
  });

  it("counts only active board-choice selections when enforcing count limits", () => {
    const gameState: GameState = {
      ...makeState(),
      waiting_for: {
        type: "PayCost",
        data: {
          player: 0,
          kind: { type: "ReturnToHand" },
          choices: [1],
          count: 1,
          min_count: 1,
          resume: {
            type: "Spell",
            Spell: {
              object_id: 9,
              card_id: 90,
              ability: { targets: [] },
              cost: { type: "NoCost" },
            },
          },
        },
      },
    };
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
    });
    useUiStore.setState({ selectedCardIds: [99] });
    const { container } = renderPermanent(new Set(), new Set(), new Set([1]));
    const permanent = container.querySelector('[data-object-id="1"]') as HTMLElement;

    fireEvent.click(permanent);

    expect(useUiStore.getState().selectedCardIds).toEqual([99, 1]);
  });

  it("renders action affordance highlights above the card face", () => {
    const { container } = renderPermanent(new Set([1]));
    const highlight = container.querySelector(
      '[data-card-affordance-highlight="true"]',
    );

    expect(highlight).toBeTruthy();
    expect(highlight?.className).toContain("absolute");
    expect(highlight?.className).toContain("z-30");
    expect(highlight?.className).toContain("pointer-events-none");
  });

  it("renders the summoning sickness art overlay when marked by the engine", () => {
    const gameState = makeState();
    gameState.objects[1] = {
      ...gameState.objects[1],
      has_summoning_sickness: true,
    };
    useGameStore.setState({ gameState });

    const { container } = renderPermanent();

    expect(container.querySelector('[data-summoning-sickness-underwater="true"]')).toBeTruthy();
  });

  it("does not render a selected attacker as tapped until the engine marks it tapped", () => {
    useUiStore.setState({
      combatMode: "attackers",
      selectedAttackers: [1],
    });

    const { container } = renderPermanent();

    expect(container.querySelector(".ms-tap")).toBeNull();

    act(() => {
      const gameState = useGameStore.getState().gameState!;
      useGameStore.setState({
        gameState: {
          ...gameState,
          objects: {
            ...gameState.objects,
            1: { ...gameState.objects[1], tapped: true },
          },
        },
      });
    });

    expect(container.querySelector(".ms-tap")).not.toBeNull();
  });

  it("opens the ability picker when a land has mana actions plus a non-mana activated ability", () => {
    const kessig = makeObject({
      id: 39,
      name: "Kessig Wolf Run",
      power: null,
      toughness: null,
      base_power: null,
      base_toughness: null,
      card_types: {
        supertypes: [],
        core_types: ["Land"],
        subtypes: ["Plains", "Island", "Swamp", "Mountain", "Forest"],
      },
      mana_cost: { type: "NoCost" },
      color: [],
      base_color: [],
      abilities: [
        {
          kind: "Activated",
          cost: { type: "Tap" },
          description: "{T}: Add {C}.",
          effect: {
            type: "Mana",
            produced: { type: "Colorless" },
          },
        },
        {
          kind: "Activated",
          cost: {
            type: "Composite",
            costs: [
              {
                type: "Mana",
                cost: { type: "Cost", shards: ["X", "Red", "Green"], generic: 0 },
              },
              { type: "Tap" },
            ],
          },
          description: "{X}{R}{G}, {T}: Target creature gets +X/+0 and gains trample until end of turn.",
          effect: { type: "GenericEffect" },
        },
      ] satisfies GameObject["abilities"],
    });

    const gameState: GameState = {
      ...makeState(),
      objects: { 39: kessig },
      battlefield: [39],
    };
    const manaAction: GameAction = {
      type: "TapLandForMana",
      data: {
        selection: {
          source: { object_id: 39, incarnation: 1 },
          ability_index: null,
          mana_type: "Green",
          output: { type: "Concrete", data: "Green" },
          atomic_combination: null,
          restrictions: [],
          penalty: "None",
          taps_for_mana: [],
        },
      },
    };
    const abilityAction = {
      type: "ActivateAbility",
      data: { source_id: 39, ability_index: 1 },
    } as const;

    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [manaAction, abilityAction],
      legalActionsByObject: { 39: [manaAction, abilityAction] },
      spellCosts: {},
    });

    const { container } = render(
      <BoardInteractionContext.Provider
        value={{
          activatableObjectIds: new Set([39]),
          boardChoiceObjectIds: new Set(),
          committedAttackerIds: new Set(),
          incomingAttackerCounts: new Map(),
          manaTappableObjectIds: new Set([39]),
          selectableSacrificeObjectIds: new Set(),
          selectableManaCostCreatureIds: new Set(),
          undoableTapObjectIds: new Set(),
          validAttackerIds: new Set(),
          validTargetObjectIds: new Set(),
        }}
      >
        <PermanentCard objectId={39} />
      </BoardInteractionContext.Provider>,
    );

    fireEvent.click(container.querySelector('[data-object-id="39"]') as HTMLElement);

    expect(dispatchAction).not.toHaveBeenCalled();
    expect(useUiStore.getState().pendingAbilityChoice).toEqual({
      objectId: 39,
      actions: [abilityAction, manaAction],
    });
  });

  it("opens the ability picker when a land has multiple mana abilities", () => {
    const holdout = makeObject({
      id: 40,
      name: "Holdout Settlement",
      power: null,
      toughness: null,
      base_power: null,
      base_toughness: null,
      card_types: {
        supertypes: [],
        core_types: ["Land"],
        subtypes: [],
      },
      mana_cost: { type: "NoCost" },
      color: [],
      base_color: [],
      // `is_mana_ability` is the engine-derived key (CR 605.1a) that BOTH
      // `deriveActivationAffordances` and `resolveObjectActivation` classify
      // with, so in production the affordance sets and the resolver's partition
      // can never disagree. This fixture asserts the mana-only affordance pair
      // below, so the abilities must carry the flag that produces it; without it
      // the fixture describes a state the engine cannot emit.
      abilities: [
        {
          kind: "Activated",
          is_mana_ability: true,
          cost: { type: "Tap" },
          description: "{T}: Add {C}.",
          effect: {
            type: "Mana",
            produced: { type: "Colorless" },
          },
        },
        {
          kind: "Activated",
          is_mana_ability: true,
          cost: {
            type: "Composite",
            costs: [
              { type: "Tap" },
              {
                type: "TapCreatures",
                count: 1,
              },
            ],
          },
          description: "{T}, Tap an untapped creature you control: Add one mana of any color.",
          effect: {
            type: "Mana",
            produced: {
              type: "AnyOneColor",
              count: { type: "Fixed", value: 1 },
              color_options: ["White", "Blue", "Black", "Red", "Green"],
            },
          },
        },
      ] satisfies GameObject["abilities"],
    });

    const gameState: GameState = {
      ...makeState(),
      objects: { 40: holdout },
      battlefield: [40],
    };
    const colorlessAction = {
      type: "ActivateAbility",
      data: { source_id: 40, ability_index: 0 },
    } as const;
    const anyColorAction = {
      type: "ActivateAbility",
      data: { source_id: 40, ability_index: 1 },
    } as const;

    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [colorlessAction, anyColorAction],
      legalActionsByObject: { 40: [colorlessAction, anyColorAction] },
      spellCosts: {},
    });

    const { container } = render(
      <BoardInteractionContext.Provider
        value={{
          activatableObjectIds: new Set(),
          boardChoiceObjectIds: new Set(),
          committedAttackerIds: new Set(),
          incomingAttackerCounts: new Map(),
          manaTappableObjectIds: new Set([40]),
          selectableSacrificeObjectIds: new Set(),
          selectableManaCostCreatureIds: new Set(),
          undoableTapObjectIds: new Set(),
          validAttackerIds: new Set(),
          validTargetObjectIds: new Set(),
        }}
      >
        <PermanentCard objectId={40} />
      </BoardInteractionContext.Provider>,
    );

    fireEvent.click(container.querySelector('[data-object-id="40"]') as HTMLElement);

    expect(dispatchAction).not.toHaveBeenCalled();
    expect(useUiStore.getState().pendingAbilityChoice).toEqual({
      objectId: 40,
      actions: [colorlessAction, anyColorAction],
    });
  });

  it("opens the ability picker when a convoke creature can pay colored or generic mana", () => {
    const helper = makeObject({
      id: 41,
      name: "Conclave Helper",
      color: ["Green"],
      base_color: ["Green"],
    });

    const gameState: GameState = {
      ...makeState(),
      objects: { 41: helper },
      battlefield: [41],
    };
    const genericAction = {
      type: "TapForConvoke",
      data: { object_id: 41, mana_type: "Colorless" },
    } as const;
    const greenAction = {
      type: "TapForConvoke",
      data: { object_id: 41, mana_type: "Green" },
    } as const;

    useGameStore.setState({
      gameState,
      waitingFor: {
        type: "ManaPayment",
        data: { player: 0, convoke_mode: "Convoke" },
      },
      legalActions: [genericAction, greenAction],
      legalActionsByObject: { 41: [genericAction, greenAction] },
      spellCosts: {},
    });

    const { container } = render(
      <BoardInteractionContext.Provider
        value={{
          activatableObjectIds: new Set(),
          boardChoiceObjectIds: new Set(),
          committedAttackerIds: new Set(),
          incomingAttackerCounts: new Map(),
          manaTappableObjectIds: new Set([41]),
          selectableSacrificeObjectIds: new Set(),
          selectableManaCostCreatureIds: new Set(),
          undoableTapObjectIds: new Set(),
          validAttackerIds: new Set(),
          validTargetObjectIds: new Set(),
        }}
      >
        <PermanentCard objectId={41} />
      </BoardInteractionContext.Provider>,
    );

    fireEvent.click(container.querySelector('[data-object-id="41"]') as HTMLElement);

    expect(dispatchAction).not.toHaveBeenCalled();
    expect(useUiStore.getState().pendingAbilityChoice).toEqual({
      objectId: 41,
      actions: [genericAction, greenAction],
    });
  });

  it("renders face-down permanents with the card back in full-card mode", () => {
    const faceDownPermanent = makeObject({
      id: 54,
      name: "Shredder's Technique",
      face_down: true,
      color: [],
      base_color: [],
    });

    const gameState: GameState = {
      ...makeState(),
      objects: { 54: faceDownPermanent },
      battlefield: [54],
    };

    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [],
      legalActionsByObject: {},
      spellCosts: {},
    });

    const { getByLabelText } = render(
      <BoardInteractionContext.Provider
        value={{
          activatableObjectIds: new Set(),
          boardChoiceObjectIds: new Set(),
          committedAttackerIds: new Set(),
          incomingAttackerCounts: new Map(),
          manaTappableObjectIds: new Set(),
          selectableSacrificeObjectIds: new Set(),
          selectableManaCostCreatureIds: new Set(),
          undoableTapObjectIds: new Set(),
          validAttackerIds: new Set(),
          validTargetObjectIds: new Set(),
        }}
      >
        <PermanentCard objectId={54} />
      </BoardInteractionContext.Provider>,
    );

    expect(getByLabelText("Face-down card")).toHaveAttribute("data-face-down", "true");
  });

  it("keeps the tile backed even when the engine projects the identity to this viewer (#7547)", () => {
    // The controller's peek lives in the hover preview; the battlefield tile
    // shows the cause marker exactly as the physical card lies face down.
    const gameState = makeState();
    gameState.objects[1].face_down = true;
    gameState.objects[1].display_visible_to_viewer = true;
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    renderPermanent();

    expect(screen.getByLabelText("Face-down card")).toHaveAttribute("data-face-down", "true");
  });

  it("dispatches the engine-provided turn-face-up action", () => {
    const gameState = makeState();
    gameState.objects[1].face_down = true;
    gameState.objects[1].display_visible_to_viewer = true;
    gameState.objects[1].attachments = [];
    const turnFaceUpAction = { type: "TurnFaceUp", data: { object_id: 1 } } as const;
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [turnFaceUpAction],
      legalActionsByObject: { 1: [turnFaceUpAction] },
      viewerInteraction: null,
    });

    const { container } = renderPermanent(new Set(), new Set(), new Set(), new Set([1]));

    fireEvent.click(container.querySelector('[data-object-id="1"]') as HTMLElement);

    expect(dispatchAction).toHaveBeenCalledWith(turnFaceUpAction);
  });

  it("forwards engine-provided token rules text and subtypes to the card image", () => {
    const lander = makeObject({
      id: 70,
      name: "Lander",
      display_source: "Token",
      power: null,
      toughness: null,
      base_power: null,
      base_toughness: null,
      card_types: { supertypes: [], core_types: ["Artifact"], subtypes: ["Lander"] },
      color: [],
      base_color: [],
      token_rules_text:
        "{2}, {T}, Sacrifice this token: Search your library for a basic land card, put it onto the battlefield tapped, then shuffle.",
    } as Partial<GameObject>);

    const gameState: GameState = {
      ...makeState(),
      objects: { 70: lander },
      battlefield: [70],
    };

    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [],
      legalActionsByObject: {},
      spellCosts: {},
    });

    const { container } = render(
      <BoardInteractionContext.Provider
        value={{
          activatableObjectIds: new Set(),
          boardChoiceObjectIds: new Set(),
          committedAttackerIds: new Set(),
          incomingAttackerCounts: new Map(),
          manaTappableObjectIds: new Set(),
          selectableSacrificeObjectIds: new Set(),
          selectableManaCostCreatureIds: new Set(),
          undoableTapObjectIds: new Set(),
          validAttackerIds: new Set(),
          validTargetObjectIds: new Set(),
        }}
      >
        <PermanentCard objectId={70} />
      </BoardInteractionContext.Provider>,
    );

    const image = container.querySelector("[data-oracle-text]") as HTMLElement;
    expect(image.getAttribute("data-oracle-text")).toContain("basic land");
    expect(image.getAttribute("data-token-subtypes")).toBe("Lander");
  });

  // #506: a lone card-consuming ActivateAbility (consumes_source true) must
  // surface the choice modal instead of auto-firing on a single click. With
  // the resolveSingleActionDispatch gate reverted this test fails — the
  // action auto-dispatches.
  it("opens the choice modal for a lone card-consuming activated ability", () => {
    const sacker = makeObject({
      id: 80,
      name: "Self-Sacrifice Permanent",
      abilities: [
        {
          kind: "Activated",
          cost: { type: "Tap" },
          description: "Sacrifice this permanent: Draw a card.",
          effect: { type: "Draw" },
          consumes_source: true,
        },
      ] satisfies GameObject["abilities"],
    });

    const gameState: GameState = {
      ...makeState(),
      objects: { 80: sacker },
      battlefield: [80],
    };
    const abilityAction = {
      type: "ActivateAbility",
      data: { source_id: 80, ability_index: 0 },
    } as const;

    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [abilityAction],
      legalActionsByObject: { 80: [abilityAction] },
      spellCosts: {},
    });

    const { container } = render(
      <BoardInteractionContext.Provider
        value={{
          activatableObjectIds: new Set([80]),
          boardChoiceObjectIds: new Set(),
          committedAttackerIds: new Set(),
          incomingAttackerCounts: new Map(),
          manaTappableObjectIds: new Set(),
          selectableSacrificeObjectIds: new Set(),
          selectableManaCostCreatureIds: new Set(),
          undoableTapObjectIds: new Set(),
          validAttackerIds: new Set(),
          validTargetObjectIds: new Set(),
        }}
      >
        <PermanentCard objectId={80} />
      </BoardInteractionContext.Provider>,
    );

    fireEvent.click(container.querySelector('[data-object-id="80"]') as HTMLElement);

    expect(dispatchAction).not.toHaveBeenCalled();
    expect(useUiStore.getState().pendingAbilityChoice).toEqual({
      objectId: 80,
      actions: [abilityAction],
    });
  });

  // #506 guard: a lone benign activated ability (consumes_source false) must
  // still auto-dispatch — the fix does not regress repeatable tap abilities.
  it("auto-dispatches a lone benign activated ability", () => {
    const scryer = makeObject({
      id: 81,
      name: "Benign Scry Permanent",
      abilities: [
        {
          kind: "Activated",
          cost: { type: "Tap" },
          description: "{T}: Scry 1.",
          effect: { type: "Scry" },
          consumes_source: false,
        },
      ] satisfies GameObject["abilities"],
    });

    const gameState: GameState = {
      ...makeState(),
      objects: { 81: scryer },
      battlefield: [81],
    };
    const abilityAction = {
      type: "ActivateAbility",
      data: { source_id: 81, ability_index: 0 },
    } as const;

    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [abilityAction],
      legalActionsByObject: { 81: [abilityAction] },
      spellCosts: {},
    });

    const { container } = render(
      <BoardInteractionContext.Provider
        value={{
          activatableObjectIds: new Set([81]),
          boardChoiceObjectIds: new Set(),
          committedAttackerIds: new Set(),
          incomingAttackerCounts: new Map(),
          manaTappableObjectIds: new Set(),
          selectableSacrificeObjectIds: new Set(),
          selectableManaCostCreatureIds: new Set(),
          undoableTapObjectIds: new Set(),
          validAttackerIds: new Set(),
          validTargetObjectIds: new Set(),
        }}
      >
        <PermanentCard objectId={81} />
      </BoardInteractionContext.Provider>,
    );

    fireEvent.click(container.querySelector('[data-object-id="81"]') as HTMLElement);

    expect(dispatchAction).toHaveBeenCalledWith(abilityAction);
    expect(useUiStore.getState().pendingAbilityChoice).toBeNull();
  });

  // Issue #6092: the engine-derived `blocked_abilities` read-out renders as a
  // badge with a localized reason. The frontend performs no game logic — it
  // reads the entries verbatim.
  it("renders the blocked-ability badge and localized reason from blocked_abilities", () => {
    const gameState = makeState();
    gameState.objects[1] = {
      ...gameState.objects[1],
      abilities: [
        {
          kind: "Activated",
          cost: { type: "Tap" },
          description: "Tap ability",
          effect: { type: "Draw" },
        },
      ] satisfies GameObject["abilities"],
      blocked_abilities: [
        { ability_index: 0, sources: [1], type: "CantBeActivated" },
      ],
    };
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    renderPermanent();

    // Badge label (t("abilityBlock.badge")) and the localized CantBeActivated
    // reason both render.
    expect(screen.getAllByText("Blocked").length).toBeGreaterThan(0);
    expect(
      screen.getByText(/This ability can't be activated/),
    ).toBeInTheDocument();
    // Single-source name renders via preview.fromSource.
    expect(screen.getByText(/\(from Test Creature\)/)).toBeInTheDocument();
  });

  it("renders every prohibiting source when two sources block one ability", () => {
    const gameState = makeState();
    gameState.objects[10] = makeObject({ id: 10, name: "Needle A" });
    gameState.objects[11] = makeObject({ id: 11, name: "Needle B" });
    gameState.objects[1] = {
      ...gameState.objects[1],
      abilities: [
        {
          kind: "Activated",
          cost: { type: "Tap" },
          description: "Tap ability",
          effect: { type: "Draw" },
        },
      ] satisfies GameObject["abilities"],
      blocked_abilities: [
        { ability_index: 0, sources: [10, 11], type: "CantBeActivated" },
      ],
    };
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    renderPermanent();

    // Both prohibiting source names render in the joined fromSource string.
    expect(screen.getByText(/\(from Needle A, Needle B\)/)).toBeInTheDocument();
  });

  it("renders a blocked-ability reason without throwing when the source is departed", () => {
    const gameState = makeState();
    gameState.objects[1] = {
      ...gameState.objects[1],
      abilities: [],
      // source 999 is not present in objects — the departed-source guard must
      // render the reason alone and never dereference a missing object.
      blocked_abilities: [
        { ability_index: 5, sources: [999], type: "Prohibited" },
      ],
    };
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    expect(() => renderPermanent()).not.toThrow();
    expect(
      screen.getByText(/Activating this ability is prohibited/),
    ).toBeInTheDocument();
    // Departed source is dropped — no fromSource span renders.
    expect(screen.queryByText(/\(from/)).not.toBeInTheDocument();
  });

  // CR 201.5: the engine ships `~` as the self-reference token, so the blocked-ability
  // tooltip must bind it to the host object's name (mirrors CardPreview, the other
  // `blocked_abilities` consumer). Description is abridged from the reported Kilo board dump
  // (object 110) and asserted against this fixture's own name: the engine text continues "Its
  // controller may search their library for a basic land card, put it onto the battlefield,
  // then shuffle." — elided because that tail carries no `~` and so moves neither assertion.
  it("substitutes ~ with the source name in the blocked-ability tooltip", () => {
    const gameState = makeState();
    gameState.objects[1] = {
      ...gameState.objects[1],
      abilities: [
        {
          kind: "Activated",
          cost: { type: "Tap" },
          description: "{T}, Sacrifice ~: Destroy target land.",
          effect: { type: "Destroy" },
        },
      ] satisfies GameObject["abilities"],
      blocked_abilities: [
        { ability_index: 0, sources: [1], type: "CantBeActivated" },
      ],
    };
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    renderPermanent();

    // Reach-guard: the row rendered, so the negative below is not vacuous. `GameplayTooltip`
    // portals to document.body, hence the body-scoped negative.
    expect(
      screen.getByText(/\{T\}, Sacrifice Test Creature: Destroy target land\./),
    ).toBeInTheDocument();
    expect(document.body.textContent).not.toContain("~");
  });
});
