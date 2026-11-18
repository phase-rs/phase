import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameAction, GameObject } from "../../../adapter/types.ts";
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
import type { GroupedPermanent } from "../../../viewmodel/battlefieldProps.ts";
import { BattlefieldZoneOverflow } from "../../board/BattlefieldZoneOverflow.tsx";
import { BoardInteractionContext } from "../../board/BoardInteractionContext.tsx";
import { DayNightBadge } from "../../hud/HudBadges.tsx";
import { TurnStatusLine } from "../../hud/TurnStatusLine.tsx";
import { CommandZone } from "../../zone/CommandZone.tsx";

// Issue #11118: a tap no longer opens a GameplayTooltip unless its trigger only
// explains something and opts in with `touch="toggle"`. These are the triggers
// that opt in besides the card preview's keywords (tested beside CardPreview):
// none sits inside a control whose tap acts. The emblem chip is one only while
// it is inert; an activatable chip's tap activates it and shows nothing.

vi.mock("../../../game/dispatch.ts", () => ({
  dispatchAction: vi.fn(),
  dispatchInteraction: vi.fn(),
}));

vi.mock("../../card/CardImage.tsx", () => ({
  CardImage: ({ cardName }: { cardName: string }) => <div aria-label={cardName} />,
}));

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: () => ({ src: "card.png", isLoading: false, isRotated: false, isFlip: false }),
}));

vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  useEngineCardData: () => null,
  useLocalizedCardName: (name: string | null) => name,
  useCardParseDetails: () => null,
  useCardRulings: () => [],
}));

// A subset, in order, of the events a browser sends for a finger tapping an
// element. These triggers take no focus, so the tap gives none.
function tap(element: Element) {
  fireEvent.pointerEnter(element, { pointerType: "touch" });
  fireEvent.pointerDown(element, { pointerType: "touch" });
  fireEvent.pointerUp(element, { pointerType: "touch" });
  fireEvent.pointerLeave(element, { pointerType: "touch" });
  fireEvent.click(element);
}

function expectTapToToggle(trigger: Element, tooltip: HTMLElement) {
  expect(tooltip).not.toBeVisible();

  tap(trigger);
  expect(tooltip).toBeVisible();

  tap(trigger);
  expect(tooltip).toBeInTheDocument();
  expect(tooltip).not.toBeVisible();
}

const initialInnerWidth = window.innerWidth;
const initialInnerHeight = window.innerHeight;

const EMBLEM_ID = 700;
const EMBLEM_ABILITIES: unknown[] = [
  { description: "{X}, Discard a card: Create a token.", effect: { type: "CreateToken" } },
];
const EMBLEM_ABILITY: GameAction = {
  type: "ActivateAbility",
  data: { source_id: EMBLEM_ID, ability_index: 0 },
};

function renderEmblemChip(legalActionsByObject: Record<string, GameAction[]>) {
  const emblem = buildGameObject({
    id: EMBLEM_ID,
    card_id: 7000,
    zone: "Command",
    owner: 0,
    controller: 0,
    name: "Emblem",
    is_emblem: true,
    entered_battlefield_turn: null,
    card_types: { supertypes: [], core_types: [], subtypes: [] },
    abilities: EMBLEM_ABILITIES as GameObject["abilities"],
  });
  const waitingFor = buildPriorityWaitingFor();
  useGameStore.setState({
    gameMode: "local",
    gameState: buildGameState({
      players: buildPlayers([0, 1]),
      objects: buildObjectMap(emblem),
      battlefield: [],
      exile: [],
      stack: [],
      command_zone: [EMBLEM_ID],
      waiting_for: waitingFor,
    }),
    waitingFor,
    legalActions: [],
    legalActionsByObject,
    viewerInteraction: null,
  });
  render(<CommandZone playerId={0} />);
  return {
    chip: screen.getByTestId("emblem-card"),
    tooltip: screen.getByRole("tooltip", { hidden: true }),
  };
}

function renderCreatureOverview() {
  const creatures = Array.from({ length: 13 }, (_, index) => {
    const id = index + 1;
    return {
      ...buildGameObjectWithCoreTypes(["Creature"], {
        id,
        card_id: id,
        zone: "Battlefield",
        name: `Creature ${id}`,
        timestamp: id,
        entered_battlefield_turn: null,
      }),
      power: 2,
      toughness: 3,
    };
  });
  const groups: GroupedPermanent[] = creatures.map((creature) => ({
    name: creature.name,
    ids: [creature.id],
    count: 1,
    representative: {} as GroupedPermanent["representative"],
    isUnboundedPile: false,
  }));
  useGameStore.setState({
    gameState: buildGameState({
      players: buildPlayers([0]),
      objects: buildObjectMap(...creatures),
      battlefield: creatures.map((creature) => creature.id),
      exile: [],
      stack: [],
      waiting_for: buildPriorityWaitingFor(),
    }),
  });
  render(
    <BoardInteractionContext.Provider
      value={{
        activatableObjectIds: new Set(),
        blockableAttackerIds: new Set(),
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
      <BattlefieldZoneOverflow groups={groups} zone="creatures" side="left" />
    </BoardInteractionContext.Provider>,
  );
}

describe("issue #11118: informational tooltips open on a tap and close on the next", () => {
  beforeEach(() => {
    useMultiplayerStore.setState({ activePlayerId: 0, isSpectator: false });
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
    Object.defineProperty(window, "innerWidth", { configurable: true, value: initialInnerWidth });
    Object.defineProperty(window, "innerHeight", { configurable: true, value: initialInnerHeight });
    useGameStore.setState(useGameStore.getInitialState(), true);
    useMultiplayerStore.setState(useMultiplayerStore.getInitialState(), true);
    useUiStore.setState({ pendingAbilityChoice: null });
  });

  it("the turn status pill shows why the player is waiting", () => {
    useGameStore.setState({
      gameMode: "online",
      gameState: buildGameState({
        active_player: 0,
        phase: "PreCombatMain",
        priority_player: 0,
        turn_decision_controller: 0,
      }),
      waitingFor: { type: "Priority", data: { player: 0 } },
    });
    render(<TurnStatusLine />);

    const tooltip = screen.getByRole("tooltip", { hidden: true });
    expect(tooltip).toHaveTextContent(/^Your priority\W+main phase$/);
    expectTapToToggle(screen.getByRole("status"), tooltip);
  });

  it("the day/night badge explains day", () => {
    useGameStore.setState({ gameState: buildGameState({ day_night: "Day" }) });
    render(<DayNightBadge />);

    const badge = screen.getByRole("img", { name: "Day" }).closest(".group");
    if (!badge) throw new Error("the day badge has no tooltip trigger");
    expectTapToToggle(badge, screen.getByRole("tooltip", { hidden: true }));
  });

  it("the creature overview's power summary shows its totals", () => {
    // A desktop-sized window, as BattlefieldZoneOverflow.test.tsx lays it out.
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 1200 });
    Object.defineProperty(window, "innerHeight", { configurable: true, value: 800 });
    renderCreatureOverview();

    const summary = screen.getByText("26/39").closest(".group");
    if (!summary) throw new Error("the power summary has no tooltip trigger");
    expectTapToToggle(summary, screen.getByText("26/39 total power and toughness"));
  });

  it("an inert emblem chip shows what the emblem does", () => {
    const { chip, tooltip } = renderEmblemChip({});

    expect(chip).not.toHaveAttribute("data-activatable");
    expectTapToToggle(chip, tooltip);
  });

  it("an activatable emblem chip activates on a tap and shows nothing", () => {
    const { chip, tooltip } = renderEmblemChip({ [String(EMBLEM_ID)]: [EMBLEM_ABILITY] });

    tap(chip);

    expect(vi.mocked(dispatchAction)).toHaveBeenCalledWith(EMBLEM_ABILITY);
    expect(tooltip).toBeInTheDocument();
    expect(tooltip).not.toBeVisible();
  });
});
