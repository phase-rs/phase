import type { ReactElement } from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState, PlayerId, TargetRef, WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import {
  buildGameState,
  buildLoopShortcutWaitingFor,
  buildPlayers,
  buildPriorityWaitingFor,
  buildTargetSelectionProgress,
  buildTargetSelectionSlot,
  targetSelectionWaitingForFactory,
} from "../../../test/factories/gameStateFactory.ts";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";
import { OpponentSeatHeader } from "../../board/OpponentSeatHeader.tsx";
import { DeclareShortcutModal } from "../../modal/LoopShortcutModal.tsx";
import type { TooltipTouch } from "../../ui/GameplayTooltip.tsx";
import {
  CityBlessingBadge,
  ConditionBadge,
  CounterBadge,
  EnduringStoryBadge,
  InitiativeBadge,
  MonarchBadge,
  PendingSpellBadge,
  PoisonCounterBadge,
  UnboundedBadge,
} from "../HudBadges.tsx";
import { OpponentHud } from "../OpponentHud.tsx";
import { PlayerHud } from "../PlayerHud.tsx";

// Issue #11118: a tap no longer opens a GameplayTooltip unless its trigger opts
// in with `touch="toggle"`. Every HUD badge that explains itself passes its
// host's choice to its tooltip. A HUD plate is a button that targets its player
// while that player is a valid target, so its badges open on a tap only while
// it is not. A multiplayer opponent tab is a button whose tap focuses or
// targets that opponent, and does nothing only while it is focused and not a
// target, so only then do its badges open on a tap. The split opponent seat
// header's target button is a full-header overlay behind its badge row, which
// stops taking pointer events while the button shows, so its badges always
// open on a tap. The loop shortcut offer's badges sit in the modal's body.

vi.mock("../../../game/dispatch.ts", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../../game/dispatch.ts")>()),
  dispatchInteraction: vi.fn(),
  previewInteractionResponse: vi.fn(),
}));

// A subset, in order, of the events a browser sends for a finger tapping an
// element. Badges take no focus, so the tap gives none.
function tap(element: Element) {
  fireEvent.pointerEnter(element, { pointerType: "touch" });
  fireEvent.pointerDown(element, { pointerType: "touch" });
  fireEvent.pointerUp(element, { pointerType: "touch" });
  fireEvent.pointerLeave(element, { pointerType: "touch" });
  fireEvent.click(element);
}

// Only a visible tooltip is in the accessibility tree. GameplayTooltip portals
// to document.body; the opponent rail's own toggles carry inline
// role="tooltip" spans that only a Tailwind class hides, which tests do not
// load, so they are left out.
function visibleTooltips() {
  return screen.queryAllByRole("tooltip").filter((tooltip) => tooltip.parentElement === document.body);
}

function badgeTrigger(name: string | RegExp) {
  const trigger = screen.getByRole("img", { name }).closest(".group");
  if (!trigger) throw new Error(`the ${String(name)} badge has no tooltip trigger`);
  return trigger;
}

function expectTapToToggle(name: string | RegExp) {
  tap(badgeTrigger(name));
  expect(visibleTooltips()).toHaveLength(1);

  tap(badgeTrigger(name));
  expect(visibleTooltips()).toHaveLength(0);
}

/** Every badge that shows its explanation in a BadgeTip, by its accessible name. */
const EXPLAINING_BADGES: Array<[string, string | RegExp]> = [
  ["Monarch", "Monarch"],
  ["Initiative", "Initiative"],
  ["City's Blessing", "City's Blessing"],
  ["Enduring Story", "Enduring Story"],
  ["poison", "3 poison counters"],
  ["energy", "2 energy counters"],
  ["rad", "3 rad counters"],
  ["experience", "4 experience counters"],
  ["speed", "Speed 2"],
  ["the next spell", "Next spell"],
  ["a condition", "Can't gain life"],
  ["an unbounded resource", /Unbounded mana/],
];
/** Only an opponent's ring shows a BadgeTip; the player's own ring opens a popover. */
const OPPONENT_RING: [string, RegExp] = ["the Ring", /The Ring tempts you/];

/** A game in which `seat` carries every designation and counter a HUD badges. */
function everyBadgeState(seat: PlayerId, waitingFor: WaitingFor = buildPriorityWaitingFor()): GameState {
  const gameState = buildGameState({
    monarch: seat,
    initiative: seat,
    city_blessing: [seat],
    enduring_story: [seat],
    ring_level: { [String(seat)]: 2 },
    pending_next_spell_cost_reductions: [{ player: seat, amount: 1 }],
    derived: {
      player_status: [{ player: seat, kind: { type: "CantGainLife" } }],
      unbounded_families: [{ player: seat, family: "mana", state: { type: "Unscheduled" } }],
    },
    waiting_for: waitingFor,
  });
  const player = gameState.players[seat];
  player.poison_counters = 3;
  player.energy = 2;
  player.speed = 2;
  player.player_counters = { Rad: 3, Experience: 4 };
  return gameState;
}

/** A target prompt for player 0 in which the engine names `players` as legal. */
function targetPlayers(players: PlayerId[]): WaitingFor {
  const legal: TargetRef[] = players.map((player) => ({ Player: player }));
  return targetSelectionWaitingForFactory
    .withData({
      selection: buildTargetSelectionProgress({ current_legal_targets: legal }),
      target_slots: [buildTargetSelectionSlot({ legal_targets: legal })],
    })
    .forPlayer(0)
    .build();
}

function seatGame(gameState: GameState) {
  const dispatch = vi.fn().mockResolvedValue([]);
  setGameStoreForTest({ gameState, dispatch });
  return { dispatch };
}

describe("issue #11118: HUD badges open their explanation on a tap where their host does not act", () => {
  beforeEach(() => {
    localStorage.clear();
    useMultiplayerStore.setState({ activePlayerId: 0, isSpectator: false, playerNames: new Map() });
    useGameStore.setState({ gameMode: null });
    usePreferencesStore.setState({ followActiveOpponent: false });
    useUiStore.setState({ focusedOpponent: 1 });
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
    useGameStore.setState(useGameStore.getInitialState(), true);
    useMultiplayerStore.setState(useMultiplayerStore.getInitialState(), true);
  });

  describe("each badge passes its host's choice to its tooltip", () => {
    const badges: Array<[string, (touch?: TooltipTouch) => ReactElement]> = [
      ["Monarch", (touch) => <MonarchBadge touch={touch} />],
      ["Initiative", (touch) => <InitiativeBadge touch={touch} />],
      ["City's Blessing", (touch) => <CityBlessingBadge touch={touch} />],
      ["Enduring Story", (touch) => <EnduringStoryBadge touch={touch} />],
      ["poison", (touch) => <PoisonCounterBadge playerId={0} value={3} touch={touch} />],
      ["energy", (touch) => <CounterBadge kind="energy" value={2} touch={touch} />],
      ["rad", (touch) => <CounterBadge kind="rad" value={3} touch={touch} />],
      ["experience", (touch) => <CounterBadge kind="experience" value={4} touch={touch} />],
      ["speed", (touch) => <CounterBadge kind="speed" value={2} touch={touch} />],
      ["the Ring", (touch) => <CounterBadge kind="ring" value={2} ringBearerName={null} touch={touch} />],
      [
        "the next spell",
        (touch) => <PendingSpellBadge modifiers={[]} reductions={[{ player: 0, amount: 1 }]} touch={touch} />,
      ],
      [
        "a condition",
        (touch) => <ConditionBadge condition={{ player: 0, kind: { type: "CantGainLife" } }} touch={touch} />,
      ],
      [
        "an unbounded resource",
        (touch) => <UnboundedBadge family="mana" state={{ type: "Unscheduled" }} touch={touch} />,
      ],
    ];

    it.each(badges)("%s opens on a tap with touch=\"toggle\" and shows nothing by default", (_label, badge) => {
      seatGame(buildGameState());

      const { unmount } = render(badge());
      tap(screen.getByRole("img").closest(".group")!);
      expect(visibleTooltips()).toHaveLength(0);
      unmount();

      render(badge("toggle"));
      const trigger = screen.getByRole("img").closest(".group")!;
      tap(trigger);
      expect(visibleTooltips()).toHaveLength(1);
      tap(trigger);
      expect(visibleTooltips()).toHaveLength(0);
    });
  });

  describe("the player's HUD plate", () => {
    it.each(EXPLAINING_BADGES)("opens %s on a tap while the player is not a target", (_label, name) => {
      seatGame(everyBadgeState(0));
      render(<PlayerHud />);

      expect(document.querySelector("[data-hud-plate]")?.tagName).toBe("DIV");
      expectTapToToggle(name);
    });

    it.each(EXPLAINING_BADGES)("targets the player when %s is tapped while the player is a target", (_label, name) => {
      const { dispatch } = seatGame(everyBadgeState(0, targetPlayers([0])));
      render(<PlayerHud />);

      tap(badgeTrigger(name));

      expect(dispatch).toHaveBeenCalledWith({ type: "ChooseTarget", data: { target: { Player: 0 } } });
      expect(visibleTooltips()).toHaveLength(0);
    });
  });

  describe("the opponent's HUD plate (two-player)", () => {
    // This plate shows no next-spell or condition badge for the opponent.
    const opponentBadges = [
      ...EXPLAINING_BADGES.filter(([label]) => label !== "the next spell" && label !== "a condition"),
      OPPONENT_RING,
    ];

    it.each(opponentBadges)("opens %s on a tap while the opponent is not a target", (_label, name) => {
      seatGame(everyBadgeState(1));
      render(<OpponentHud />);

      expect(document.querySelector("[data-hud-plate]")?.tagName).toBe("DIV");
      expectTapToToggle(name);
    });

    it.each(opponentBadges)("targets the opponent when %s is tapped while it is a target", (_label, name) => {
      const { dispatch } = seatGame(everyBadgeState(1, targetPlayers([1])));
      render(<OpponentHud />);

      tap(badgeTrigger(name));

      expect(dispatch).toHaveBeenCalledWith({ type: "ChooseTarget", data: { target: { Player: 1 } } });
      expect(visibleTooltips()).toHaveLength(0);
    });
  });

  describe("the multiplayer opponent tabs", () => {
    // Opponents 1 and 2, with opponent 1 focused, each with a poison badge.
    function threePlayerGame(waitingFor: WaitingFor = buildPriorityWaitingFor()) {
      const gameState = buildGameState({
        players: buildPlayers([0, 1, 2]),
        seat_order: [0, 1, 2],
        waiting_for: waitingFor,
      });
      gameState.players[1].poison_counters = 3;
      gameState.players[2].poison_counters = 4;
      return seatGame(gameState);
    }

    it("opens a badge on the focused tab on a tap while that opponent is not a target", () => {
      threePlayerGame();
      render(<OpponentHud />);

      expectTapToToggle("3 poison counters");
    });

    it("focuses an unfocused tab when its badge is tapped, and shows nothing", () => {
      threePlayerGame();
      render(<OpponentHud />);

      tap(badgeTrigger("4 poison counters"));

      expect(useUiStore.getState().focusedOpponent).toBe(2);
      expect(visibleTooltips()).toHaveLength(0);
    });

    it("targets the focused opponent when its badge is tapped while it is a target", () => {
      const { dispatch } = threePlayerGame(targetPlayers([1]));
      render(<OpponentHud />);

      tap(badgeTrigger("3 poison counters"));

      expect(dispatch).toHaveBeenCalledWith({ type: "ChooseTarget", data: { target: { Player: 1 } } });
      expect(visibleTooltips()).toHaveLength(0);
    });
  });

  describe("the split opponent seat header", () => {
    it.each([...EXPLAINING_BADGES, OPPONENT_RING])("opens %s on a tap", (_label, name) => {
      seatGame(everyBadgeState(1));
      render(<OpponentSeatHeader playerId={1} />);

      expectTapToToggle(name);
    });
  });

  describe("the loop shortcut offer", () => {
    it("opens an unbounded family's explanation on a tap", () => {
      const waitingFor = buildLoopShortcutWaitingFor({ certificate: { unbounded: [{ DamageDealt: 1 }] } });
      setGameStoreForTest({
        gameState: buildGameState({ objects: {}, priority_player: 0, waiting_for: waitingFor }),
        waitingFor,
      });
      render(<DeclareShortcutModal />);

      expectTapToToggle(/damage/i);
    });
  });
});
