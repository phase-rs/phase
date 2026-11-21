import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AttackTarget, GameAction, GameObject, WaitingFor } from "../../../adapter/types.ts";
import { dispatchAction } from "../../../game/dispatch.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import {
  buildGameObject,
  buildGameObjectWithCoreTypes,
  buildObjectMap,
} from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayers, buildPriorityWaitingFor } from "../../../test/factories/gameStateFactory.ts";
import { AttackTargetPicker } from "../../controls/AttackTargetPicker.tsx";
import { DialogAttachmentCard } from "../../hud/DialogAttachmentCard.tsx";
import { ZoneViewer } from "../../zone/ZoneViewer.tsx";

// Issue #11118: a tap no longer opens a GameplayTooltip unless its trigger opts
// in with `touch="toggle"`. These counters sit on hosts whose tap acts only some
// of the time, so each opts in only while its host does not act: a card in the
// attachments dialog or the zone viewer while it is not interactive, and the
// attack picker's counters in its table cell but not inside its phone layout's
// expand button.

vi.mock("../../../game/dispatch.ts", () => ({
  dispatchAction: vi.fn(),
  dispatchInteraction: vi.fn(),
}));

vi.mock("../../card/CardImage.tsx", () => ({
  CardImage: ({ cardName }: { cardName: string }) => <div aria-label={cardName} />,
}));

vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  useEngineCardData: () => null,
  useLocalizedCardName: (name: string | null) => name,
  useCardParseDetails: () => null,
  useCardRulings: () => [],
}));

// A subset, in order, of the events a browser sends for a finger tapping an
// element. These counters take no focus, so the tap gives none.
function tap(element: Element) {
  fireEvent.pointerEnter(element, { pointerType: "touch" });
  fireEvent.pointerDown(element, { pointerType: "touch" });
  fireEvent.pointerUp(element, { pointerType: "touch" });
  fireEvent.pointerLeave(element, { pointerType: "touch" });
  fireEvent.click(element);
}

// Only a visible tooltip is in the accessibility tree. GameplayTooltip portals
// to document.body, so counting only those leaves out any inline
// role="tooltip" span that only a Tailwind class hides.
function visibleTooltips() {
  return screen.queryAllByRole("tooltip").filter((tooltip) => tooltip.parentElement === document.body);
}

function counterTrigger(counter: Element) {
  const trigger = counter.closest(".group");
  if (!trigger) throw new Error("the counter has no tooltip trigger");
  return trigger;
}

const CURSE_ID = 408;
const CURSE_TAP: GameAction = {
  type: "ActivateAbility",
  data: { source_id: CURSE_ID, ability_index: 0 },
};

// A Curse with a charge counter, as DialogAttachmentCard.test.tsx builds it.
function renderAttachedCurse(waitingFor: WaitingFor, legalActionsByObject: Record<string, GameAction[]>) {
  const curse = buildGameObject({
    id: CURSE_ID,
    card_id: 4080,
    zone: "Battlefield",
    owner: 0,
    controller: 0,
    name: "Cruel Reality",
    attached_to: { type: "Player", data: 1 },
    entered_battlefield_turn: null,
    card_types: { supertypes: [], core_types: ["Enchantment"], subtypes: ["Aura", "Curse"] },
    abilities: [{ effect: { type: "Tap" } }] as unknown as GameObject["abilities"],
  });
  useGameStore.setState({
    gameMode: "local",
    gameState: {
      ...buildGameState({
        players: buildPlayers([0, 1]),
        objects: buildObjectMap(curse),
        battlefield: [CURSE_ID],
        exile: [],
        stack: [],
        waiting_for: waitingFor,
      }),
      derived: { counter_display: { [String(CURSE_ID)]: { pills: [{ counter: "charge", count: 2 }] } } },
    },
    waitingFor,
    legalActions: [],
    legalActionsByObject,
    viewerInteraction: null,
  });
  render(<DialogAttachmentCard objectId={CURSE_ID} widthPx={200} onDismiss={vi.fn()} />);
  return counterTrigger(screen.getByText("charge x2"));
}

const EXILED_ID = 7;

// An exiled card with three time counters, as ZoneViewer.test.tsx builds it.
function renderExiledCard(legalActionsByObject: Record<string, GameAction[]>) {
  const exiled = buildGameObjectWithCoreTypes(["Sorcery"], {
    id: EXILED_ID,
    card_id: 700,
    zone: "Exile",
    name: "Rift Bolt",
    entered_battlefield_turn: null,
  });
  const gameState = {
    ...buildGameState({
      active_player: 0,
      priority_player: 0,
      players: buildPlayers([0, 1]),
      objects: buildObjectMap(exiled),
      battlefield: [],
      exile: [EXILED_ID],
      stack: [],
      waiting_for: buildPriorityWaitingFor(),
    }),
    derived: { counter_display: { [String(EXILED_ID)]: { pills: [{ counter: "time", count: 3 }] } } },
  };
  const dispatch = vi.fn(async () => []);
  useGameStore.setState({
    gameState,
    waitingFor: gameState.waiting_for,
    legalActions: [],
    legalActionsByObject,
    spellCosts: {},
    dispatch,
    gameMode: "ai",
  });
  render(<ZoneViewer zone="exile" playerId={0} onClose={vi.fn()} />);
  return { dispatch, counter: counterTrigger(screen.getByLabelText("3")) };
}

const TARGETS: AttackTarget[] = [
  { type: "Player", data: 1 },
  { type: "Player", data: 2 },
];

// One attacker with four charge counters, split between two defenders.
function renderAttackPicker() {
  const goblin = buildGameObjectWithCoreTypes(["Creature"], {
    id: 101,
    name: "Goblin",
    color: ["Red"],
    base_color: ["Red"],
    power: 1,
    toughness: 1,
    base_power: 1,
    base_toughness: 1,
  });
  useGameStore.setState({
    gameState: buildGameState({
      players: buildPlayers([0, 1, 2]),
      seat_order: [0, 1, 2],
      objects: buildObjectMap(goblin),
      derived: { counter_display: { "101": { pills: [{ counter: "charge", count: 4 }] } } },
    }),
  });
  render(
    <AttackTargetPicker validTargets={TARGETS} selectedAttackers={[101]} onConfirm={vi.fn()} onCancel={vi.fn()} />,
  );
  fireEvent.click(screen.getByRole("button", { name: "Distribute" }));
}

describe("issue #11118: counters open their explanation on a tap only while their host does not act", () => {
  beforeEach(() => {
    useMultiplayerStore.setState({ activePlayerId: 0, isSpectator: false, playerNames: new Map() });
    useUiStore.setState({ pendingAbilityChoice: null, inspectedObjectId: null, debugInteractionMode: false });
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
    useGameStore.setState(useGameStore.getInitialState(), true);
    useMultiplayerStore.setState(useMultiplayerStore.getInitialState(), true);
    useUiStore.setState({ pendingAbilityChoice: null, inspectedObjectId: null });
  });

  it("a counter on an inert attached card opens on a tap and closes on the next", () => {
    const declareBlockers: WaitingFor = {
      type: "DeclareBlockers",
      data: { player: 0, valid_blocker_ids: [], valid_block_targets: {} },
    };
    const counter = renderAttachedCurse(declareBlockers, { [String(CURSE_ID)]: [CURSE_TAP] });
    expect(screen.queryByRole("button")).toBeNull();

    tap(counter);
    expect(visibleTooltips()).toHaveLength(1);

    tap(counter);
    expect(visibleTooltips()).toHaveLength(0);
  });

  it("a counter on an activatable attached card activates it on a tap and shows nothing", () => {
    const counter = renderAttachedCurse(buildPriorityWaitingFor(), { [String(CURSE_ID)]: [CURSE_TAP] });

    tap(counter);

    expect(vi.mocked(dispatchAction)).toHaveBeenCalledWith(CURSE_TAP);
    expect(visibleTooltips()).toHaveLength(0);
  });

  it("the time counter on an inert exiled card opens on a tap and closes on the next", () => {
    const { counter } = renderExiledCard({});
    expect(counter.closest("button")).toBeNull();

    tap(counter);
    expect(visibleTooltips()).toHaveLength(1);

    tap(counter);
    expect(visibleTooltips()).toHaveLength(0);
  });

  it("the time counter on a castable exiled card casts it on a tap and shows nothing", () => {
    const cast: GameAction = { type: "CastSpell", data: { object_id: EXILED_ID, card_id: 700, targets: [] } };
    const { counter, dispatch } = renderExiledCard({ [String(EXILED_ID)]: [cast] });

    tap(counter);

    expect(dispatch).toHaveBeenCalledWith(expect.objectContaining({ type: "CastSpell" }));
    expect(visibleTooltips()).toHaveLength(0);
  });

  it("an attacker's counter in the picker's table opens on a tap and closes on the next", () => {
    renderAttackPicker();
    const inTable = screen.getAllByText("charge x4").find((counter) => counter.closest("td"));
    if (!inTable) throw new Error("the table shows no counter");

    tap(counterTrigger(inTable));
    expect(visibleTooltips()).toHaveLength(1);

    tap(counterTrigger(inTable));
    expect(visibleTooltips()).toHaveLength(0);
  });

  it("an attacker's counter inside the phone layout's expand button expands the stack and shows nothing", () => {
    renderAttackPicker();
    const inButton = screen.getAllByText("charge x4").find((counter) => counter.closest("button"));
    if (!inButton) throw new Error("the expand button shows no counter");
    const expand = inButton.closest("button");
    expect(expand).toHaveAttribute("aria-expanded", "false");

    tap(counterTrigger(inButton));

    expect(expand).toHaveAttribute("aria-expanded", "true");
    expect(visibleTooltips()).toHaveLength(0);
  });
});
