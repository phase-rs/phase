import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameAction, GameObject, GameState } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { ZoneViewer } from "../ZoneViewer.tsx";

vi.mock("../../card/CardImage.tsx", () => ({
  CardImage: ({ cardName }: { cardName: string }) => (
    <div aria-label={cardName} data-testid="card-image" />
  ),
}));

const targetDispatch = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => targetDispatch,
}));

function makeObject(overrides: Partial<GameObject> = {}): GameObject {
  return {
    id: 7,
    card_id: 700,
    owner: 0,
    controller: 0,
    zone: "Graveyard",
    tapped: false,
    face_down: false,
    flipped: false,
    transformed: false,
    damage_marked: 0,
    dealt_deathtouch_damage: false,
    attached_to: null,
    attachments: [],
    counters: {},
    name: "Flame Jab",
    power: null,
    toughness: null,
    loyalty: null,
    card_types: { supertypes: [], core_types: ["Sorcery"], subtypes: [] },
    mana_cost: { type: "Cost", shards: ["Red"], generic: 0 },
    keywords: ["Retrace"],
    abilities: [],
    trigger_definitions: [],
    replacement_definitions: [],
    static_definitions: [],
    color: ["Red"],
    base_power: null,
    base_toughness: null,
    base_keywords: ["Retrace"],
    base_color: ["Red"],
    timestamp: 1,
    entered_battlefield_turn: null,
    ...overrides,
  };
}

function makeCastAction(objectId: number): GameAction {
  return {
    type: "CastSpell",
    data: { object_id: objectId, card_id: 700, targets: [] },
  };
}

function makeState(object: GameObject): GameState {
  return {
    active_player: 0,
    priority_player: 0,
    players: [
      {
        id: 0,
        life: 20,
        poison_counters: 0,
        mana_pool: { mana: [] },
        library: [],
        hand: [],
        graveyard: [object.id],
        has_drawn_this_turn: false,
        lands_played_this_turn: 0,
        turns_taken: 0,
      },
      {
        id: 1,
        life: 20,
        poison_counters: 0,
        mana_pool: { mana: [] },
        library: [],
        hand: [],
        graveyard: [],
        has_drawn_this_turn: false,
        lands_played_this_turn: 0,
        turns_taken: 0,
      },
    ],
    objects: { [object.id]: object },
    battlefield: [],
    exile: [],
    stack: [],
    combat: null,
    waiting_for: { type: "Priority", data: { player: 0 } },
  } as unknown as GameState;
}

describe("ZoneViewer", () => {
  const dispatch = vi.fn(async () => []);

  beforeEach(() => {
    const object = makeObject();
    const action = makeCastAction(object.id);
    const gameState = makeState(object);
    targetDispatch.mockClear();
    dispatch.mockClear();
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [action],
      legalActionsByObject: { [String(object.id)]: [action] },
      spellCosts: {},
      dispatch,
      gameMode: "ai",
    });
    useUiStore.setState({
      inspectedObjectId: null,
      previewSticky: false,
      pendingAbilityChoice: null,
      debugInteractionMode: false,
    });
  });

  afterEach(() => {
    cleanup();
  });

  it("dispatches an engine-provided graveyard CastSpell action", () => {
    render(<ZoneViewer zone="graveyard" playerId={0} onClose={vi.fn()} />);

    // The castable card carries the purple "playable" affordance instead of a
    // labeled button; clicking the card itself routes through handleCast and
    // auto-dispatches the lone CastSpell action.
    fireEvent.click(screen.getByTestId("card-image"));

    expect(dispatch).toHaveBeenCalledTimes(1);
    expect(dispatch).toHaveBeenCalledWith(
      expect.objectContaining({ type: "CastSpell" }),
    );
  });

  it("renders revealed library tops face-up and unrevealed cards as backs", () => {
    // CR 701.20b: a RevealTop / Oracle of Mul Daya look surfaces the top card's
    // identity; the rest of the library arrives redacted (`Hidden Card`). The
    // library viewer shows the revealed card face-up and the remainder as
    // card-backs, top-first.
    const revealed = makeObject({
      id: 20,
      zone: "Library",
      name: "Llanowar Elves",
      keywords: [],
      base_keywords: [],
    });
    const hiddenA = makeObject({ id: 21, zone: "Library", name: "Hidden Card", face_down: true });
    const hiddenB = makeObject({ id: 22, zone: "Library", name: "Hidden Card", face_down: true });
    const base = makeState(revealed);
    const gameState = {
      ...base,
      objects: { [revealed.id]: revealed, [hiddenA.id]: hiddenA, [hiddenB.id]: hiddenB },
      players: [
        { ...base.players[0], graveyard: [], library: [revealed.id, hiddenA.id, hiddenB.id] },
        base.players[1],
      ],
    } as unknown as GameState;

    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [],
      legalActionsByObject: {},
      spellCosts: {},
      dispatch,
      gameMode: "ai",
    });

    render(<ZoneViewer zone="library" playerId={0} onClose={vi.fn()} />);

    // All three cards render; only the revealed one carries a real name. The two
    // hidden cards render via the hook-free FaceDownCard (mocked CardImage with
    // an empty name).
    expect(screen.getAllByTestId("card-image")).toHaveLength(3);
    expect(screen.getByLabelText("Llanowar Elves")).toBeInTheDocument();
    expect(screen.queryByLabelText("Hidden Card")).not.toBeInTheDocument();
  });

  it("dispatches the engine-surfaced play-from-top action for a revealed library top", () => {
    // CR 401.5 + CR 118.9: with a TopOfLibraryCastPermission active (Future
    // Sight, Bolas's Citadel, Mystic Forge, …) the engine surfaces a play/cast
    // action on the revealed top. The viewer dispatches it just like a
    // graveyard/exile cast — no library-specific permission inspection.
    const revealed = makeObject({
      id: 30,
      zone: "Library",
      name: "Mystic Sanctuary",
      keywords: [],
      base_keywords: [],
    });
    const hidden = makeObject({ id: 31, zone: "Library", name: "Hidden Card", face_down: true });
    const action = makeCastAction(revealed.id);
    const base = makeState(revealed);
    const gameState = {
      ...base,
      objects: { [revealed.id]: revealed, [hidden.id]: hidden },
      players: [
        { ...base.players[0], graveyard: [], library: [revealed.id, hidden.id] },
        base.players[1],
      ],
    } as unknown as GameState;

    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [action],
      legalActionsByObject: { [String(revealed.id)]: [action] },
      spellCosts: {},
      dispatch,
      gameMode: "ai",
    });

    render(<ZoneViewer zone="library" playerId={0} onClose={vi.fn()} />);
    fireEvent.click(screen.getByLabelText("Mystic Sanctuary"));

    expect(dispatch).toHaveBeenCalledTimes(1);
    expect(dispatch).toHaveBeenCalledWith(
      expect.objectContaining({ type: "CastSpell" }),
    );
  });

  it("shows an opponent's revealed library top face-up with no castable affordance", () => {
    // CR 701.20b: an opponent's library top revealed to all players (e.g. an
    // Oracle of Mul Daya the opponent controls, or a public RevealTop) is
    // visible to this viewer, but the viewer has NO play permission on the
    // opponent's library — so legalActionsByObject is empty and clicking the
    // revealed card must not dispatch. The rest of the opponent's library stays
    // redacted and renders as backs.
    const revealed = makeObject({
      id: 40,
      owner: 1,
      controller: 1,
      zone: "Library",
      name: "Courser of Kruphix",
      keywords: [],
      base_keywords: [],
    });
    const hidden = makeObject({
      id: 41,
      owner: 1,
      controller: 1,
      zone: "Library",
      name: "Hidden Card",
      face_down: true,
    });
    const base = makeState(revealed);
    const gameState = {
      ...base,
      objects: { [revealed.id]: revealed, [hidden.id]: hidden },
      players: [
        { ...base.players[0], graveyard: [] },
        { ...base.players[1], graveyard: [], library: [revealed.id, hidden.id] },
      ],
    } as unknown as GameState;

    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [],
      legalActionsByObject: {},
      spellCosts: {},
      dispatch,
      gameMode: "ai",
    });

    render(<ZoneViewer zone="library" playerId={1} onClose={vi.fn()} />);

    expect(screen.getAllByTestId("card-image")).toHaveLength(2);
    expect(screen.getByLabelText("Courser of Kruphix")).toBeInTheDocument();
    expect(screen.queryByLabelText("Hidden Card")).not.toBeInTheDocument();

    // No play permission → clicking the revealed opponent card is inert.
    fireEvent.click(screen.getByLabelText("Courser of Kruphix"));
    expect(dispatch).not.toHaveBeenCalled();
  });

  it("dispatches a CastSpell for an opponent-owned exiled card the viewer may play", () => {
    // Hostage Taker / Gonti / Thief of Sanity: the card is owned by the
    // opponent (player 1) and sits in their exile pile, but the engine granted
    // the viewer (player 0) permission to play it — surfaced as a CastSpell in
    // legalActionsByObject. The viewer must honor the engine's authority even
    // though the pile is not the viewer's own. Regression guard for the removed
    // client-side `isMyZone` ownership gate.
    const object = makeObject({
      id: 9,
      owner: 1,
      controller: 1,
      zone: "Exile",
      name: "Gonti, Lord of Luxury",
      keywords: [],
      base_keywords: [],
    });
    const action = makeCastAction(object.id);
    const base = makeState(object);
    const gameState = {
      ...base,
      objects: { [object.id]: object },
      exile: [object.id],
      players: [
        { ...base.players[0], graveyard: [] },
        { ...base.players[1], graveyard: [] },
      ],
    } as unknown as GameState;

    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [action],
      legalActionsByObject: { [String(object.id)]: [action] },
      spellCosts: {},
      dispatch,
      gameMode: "ai",
    });

    render(<ZoneViewer zone="exile" playerId={1} onClose={vi.fn()} />);
    fireEvent.click(screen.getByTestId("card-image"));

    expect(dispatch).toHaveBeenCalledTimes(1);
    expect(dispatch).toHaveBeenCalledWith(
      expect.objectContaining({ type: "CastSpell" }),
    );
  });
});
