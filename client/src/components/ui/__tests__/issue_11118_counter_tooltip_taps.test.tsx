import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AttackTarget, GameAction, GameObject, WaitingFor } from "../../../adapter/types.ts";
import { dispatchAction } from "../../../game/dispatch.ts";
import { LONG_PRESS_DELAY_MS } from "../../../hooks/useLongPress.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import {
  buildGameObject,
  buildGameObjectWithCoreTypes,
  buildObjectMap,
} from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayers, buildPriorityWaitingFor } from "../../../test/factories/gameStateFactory.ts";
import { touchHold, touchTap } from "../../../test/helpers/touchGestures.ts";
import { AttackTargetPicker } from "../../controls/AttackTargetPicker.tsx";
import { DialogAttachmentCard } from "../../hud/DialogAttachmentCard.tsx";
import { ZoneViewer } from "../../zone/ZoneViewer.tsx";

// Issue #11118: a tap no longer opens a GameplayTooltip unless its trigger opts
// in with `touch="toggle"`. These counters sit on hosts whose tap acts only some
// of the time, so each opts in only while its host does not act: a card in the
// attachments dialog or the zone viewer while it is not interactive, and the
// attack picker's counters in its table cell but not inside its phone layout's
// expand button.
//
// Each of these hosts opens the card preview on a long press, through a hook
// that captures the pointer on the host unless the press starts on a trigger
// that owns its taps. A capture there would send the tap's `pointerup` and
// `click` to the host instead of the counter. The taps below are routed by
// pointer capture as a browser routes them (`touchTap`), so a counter that
// loses its tap to the host fails here.

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

// Only a visible tooltip is in the accessibility tree. GameplayTooltip portals
// to document.body, so counting only those leaves out any inline
// role="tooltip" span that only a Tailwind class hides.
function visibleTooltips() {
  return screen.queryAllByRole("tooltip").filter((tooltip) => tooltip.parentElement === document.body);
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
  return screen.getByText("charge x2");
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
  return { dispatch, counter: screen.getByLabelText("3") };
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
    useUiStore.setState({
      pendingAbilityChoice: null,
      inspectedObjectId: null,
      previewSticky: false,
      debugInteractionMode: false,
    });
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    vi.clearAllMocks();
    useGameStore.setState(useGameStore.getInitialState(), true);
    useMultiplayerStore.setState(useMultiplayerStore.getInitialState(), true);
    useUiStore.setState({ pendingAbilityChoice: null, inspectedObjectId: null, previewSticky: false });
  });

  const declareBlockers: WaitingFor = {
    type: "DeclareBlockers",
    data: { player: 0, valid_blocker_ids: [], valid_block_targets: {} },
  };

  it("a counter on an inert attached card opens on a tap and closes on the next", () => {
    const counter = renderAttachedCurse(declareBlockers, { [String(CURSE_ID)]: [CURSE_TAP] });
    expect(screen.queryByRole("button")).toBeNull();

    touchTap(counter);
    expect(visibleTooltips()).toHaveLength(1);

    touchTap(counter);
    expect(visibleTooltips()).toHaveLength(0);
  });

  it("a counter on an activatable attached card activates it on a tap and shows nothing", () => {
    const counter = renderAttachedCurse(buildPriorityWaitingFor(), { [String(CURSE_ID)]: [CURSE_TAP] });

    touchTap(counter);

    expect(vi.mocked(dispatchAction)).toHaveBeenCalledWith(CURSE_TAP);
    expect(visibleTooltips()).toHaveLength(0);
  });

  it("the time counter on an inert exiled card opens on a tap and closes on the next", () => {
    const { counter } = renderExiledCard({});
    expect(counter.closest("button")).toBeNull();

    touchTap(counter);
    expect(visibleTooltips()).toHaveLength(1);

    touchTap(counter);
    expect(visibleTooltips()).toHaveLength(0);
  });

  it("the time counter on a castable exiled card casts it on a tap and shows nothing", () => {
    const cast: GameAction = { type: "CastSpell", data: { object_id: EXILED_ID, card_id: 700, targets: [] } };
    const { counter, dispatch } = renderExiledCard({ [String(EXILED_ID)]: [cast] });

    touchTap(counter);

    expect(dispatch).toHaveBeenCalledWith(expect.objectContaining({ type: "CastSpell" }));
    expect(visibleTooltips()).toHaveLength(0);
  });

  it("an attacker's counter in the picker's table opens on a tap and closes on the next", () => {
    renderAttackPicker();
    const inTable = screen.getAllByText("charge x4").find((counter) => counter.closest("td"));
    if (!inTable) throw new Error("the table shows no counter");

    touchTap(inTable);
    expect(visibleTooltips()).toHaveLength(1);

    touchTap(inTable);
    expect(visibleTooltips()).toHaveLength(0);
  });

  it("an attacker's counter inside the phone layout's expand button expands the stack and shows nothing", () => {
    renderAttackPicker();
    const inButton = screen.getAllByText("charge x4").find((counter) => counter.closest("button"));
    if (!inButton) throw new Error("the expand button shows no counter");
    const expand = inButton.closest("button");
    expect(expand).toHaveAttribute("aria-expanded", "false");

    touchTap(inButton);

    expect(expand).toHaveAttribute("aria-expanded", "true");
    expect(visibleTooltips()).toHaveLength(0);
  });

  describe("the long press on the host", () => {
    beforeEach(() => {
      vi.useFakeTimers();
    });

    it("still opens the card preview from an attached card's counter, without its explanation", () => {
      const counter = renderAttachedCurse(declareBlockers, {});

      touchHold(counter, LONG_PRESS_DELAY_MS);

      expect(useUiStore.getState().inspectedObjectId).toBe(CURSE_ID);
      expect(useUiStore.getState().previewSticky).toBe(true);
      expect(visibleTooltips()).toHaveLength(0);
    });

    it("still opens the card preview from an exiled card's time counter, without its explanation", () => {
      const { counter } = renderExiledCard({});

      touchHold(counter, LONG_PRESS_DELAY_MS);

      expect(useUiStore.getState().inspectedObjectId).toBe(EXILED_ID);
      expect(useUiStore.getState().previewSticky).toBe(true);
      expect(visibleTooltips()).toHaveLength(0);
    });

    it("leaves a press released before the long press fires to the counter, even when its click comes late", () => {
      const counter = renderAttachedCurse(declareBlockers, {});
      const finger = { pointerType: "touch", pointerId: 1, isPrimary: true, button: 0 } as const;

      fireEvent.pointerDown(counter, finger);
      act(() => {
        vi.advanceTimersByTime(LONG_PRESS_DELAY_MS - 1);
      });
      fireEvent.pointerUp(counter, finger);
      act(() => {
        vi.advanceTimersByTime(2);
      });
      fireEvent.click(counter);

      expect(useUiStore.getState().previewSticky).toBe(false);
      expect(visibleTooltips()).toHaveLength(1);
    });

    it("is still cancelled when the finger moves off the counter", () => {
      const { counter } = renderExiledCard({});
      const finger = { pointerType: "touch", pointerId: 1, isPrimary: true, button: 0 } as const;

      fireEvent.pointerDown(counter, { ...finger, clientX: 0, clientY: 0 });
      fireEvent.pointerMove(counter, { ...finger, clientX: 40, clientY: 0 });
      act(() => {
        vi.advanceTimersByTime(LONG_PRESS_DELAY_MS * 2);
      });

      expect(useUiStore.getState().previewSticky).toBe(false);
      expect(useUiStore.getState().inspectedObjectId).toBeNull();
    });
  });
});
