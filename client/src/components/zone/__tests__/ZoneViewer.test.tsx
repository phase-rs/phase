import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameAction, GameObject, GameState } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { ZoneViewer } from "../ZoneViewer.tsx";

vi.mock("../../card/CardImage.tsx", () => ({
  CardImage: ({ cardName, oracleId }: { cardName: string; oracleId?: string }) => (
    <div aria-label={cardName} data-testid="card-image" data-oracle-id={oracleId ?? ""} />
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

  it("resolves graveyard card art via printed_ref oracle_id, not name", () => {
    // Regression: a transformed / DFC / back-face card (e.g. a transformed
    // planeswalker) has a back-face name that isn't a scryfall-data key, so the
    // legacy name-only lookup failed and rendered the oversized broken-image
    // fallback ("huge, no art"). The viewer must use the engine's printed_ref
    // (oracle_id) like every other object-rendering surface.
    const pw = makeObject({
      id: 42,
      name: "Ral, Leyline Prodigy",
      card_types: { supertypes: [], core_types: ["Planeswalker"], subtypes: ["Ral"] },
      transformed: true,
      printed_ref: { oracle_id: "ral-oracle-id", face_name: "Ral, Leyline Prodigy" },
    });
    useGameStore.setState({ gameState: makeState(pw) });

    render(<ZoneViewer zone="graveyard" playerId={0} onClose={vi.fn()} />);

    expect(screen.getByTestId("card-image").getAttribute("data-oracle-id")).toBe(
      "ral-oracle-id",
    );
  });

  it("shows only the engine-revealed library cards, omitting unrevealed ones", () => {
    // CR 701.20b: a RevealTop / "play with top revealed" surfaces specific top
    // cards via `revealed_cards`. Visibility is gated on that engine set, NOT on
    // the card name — single-player renders the raw, unredacted state, so the
    // unrevealed cards below carry real names yet must NOT appear in the viewer.
    const revealed = makeObject({
      id: 20,
      zone: "Library",
      name: "Llanowar Elves",
      keywords: [],
      base_keywords: [],
    });
    // Real names, but absent from revealed_cards → must be filtered out.
    const unrevealedA = makeObject({ id: 21, zone: "Library", name: "Black Lotus" });
    const unrevealedB = makeObject({ id: 22, zone: "Library", name: "Mox Sapphire" });
    const base = makeState(revealed);
    const gameState = {
      ...base,
      objects: { [revealed.id]: revealed, [unrevealedA.id]: unrevealedA, [unrevealedB.id]: unrevealedB },
      revealed_cards: [revealed.id],
      players: [
        { ...base.players[0], graveyard: [], library: [revealed.id, unrevealedA.id, unrevealedB.id] },
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

    // Only the one revealed card renders; the unrevealed real-named cards are
    // omitted (no card-backs) — the leak-safe "just the revealed" behavior.
    expect(screen.getAllByTestId("card-image")).toHaveLength(1);
    expect(screen.getByLabelText("Llanowar Elves")).toBeInTheDocument();
    expect(screen.queryByLabelText("Black Lotus")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Mox Sapphire")).not.toBeInTheDocument();
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
    const unrevealed = makeObject({ id: 31, zone: "Library", name: "Sol Ring" });
    const action = makeCastAction(revealed.id);
    const base = makeState(revealed);
    const gameState = {
      ...base,
      objects: { [revealed.id]: revealed, [unrevealed.id]: unrevealed },
      revealed_cards: [revealed.id],
      players: [
        { ...base.players[0], graveyard: [], library: [revealed.id, unrevealed.id] },
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

  it("shows the owner's own top under a continuous look (Future Sight) and keeps it castable", () => {
    // Future Sight / Bolas's Citadel / Oracle of Mul Daya grant
    // `can_look_at_top_of_library` — a continuous static that exposes the OWNER's
    // top WITHOUT adding it to revealed_cards/private_look. The modal must still
    // show that top (and the engine-surfaced play action), mirroring the pile.
    const top = makeObject({
      id: 50,
      zone: "Library",
      name: "Future Sight Top",
      keywords: [],
      base_keywords: [],
    });
    const buried = makeObject({ id: 51, zone: "Library", name: "Buried Secret" });
    const action = makeCastAction(top.id);
    const base = makeState(top);
    const gameState = {
      ...base,
      objects: { [top.id]: top, [buried.id]: buried },
      revealed_cards: [],
      players: [
        {
          ...base.players[0],
          graveyard: [],
          library: [top.id, buried.id],
          can_look_at_top_of_library: true,
        },
        base.players[1],
      ],
    } as unknown as GameState;

    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [action],
      legalActionsByObject: { [String(top.id)]: [action] },
      spellCosts: {},
      dispatch,
      gameMode: "ai",
    });

    render(<ZoneViewer zone="library" playerId={0} onClose={vi.fn()} />);

    // Only the looked-at top renders; the buried card (real name, not visible to
    // the viewer) is omitted.
    expect(screen.getAllByTestId("card-image")).toHaveLength(1);
    expect(screen.getByLabelText("Future Sight Top")).toBeInTheDocument();
    expect(screen.queryByLabelText("Buried Secret")).not.toBeInTheDocument();

    // The engine-surfaced play-from-top action stays reachable through the modal.
    fireEvent.click(screen.getByLabelText("Future Sight Top"));
    expect(dispatch).toHaveBeenCalledTimes(1);
    expect(dispatch).toHaveBeenCalledWith(
      expect.objectContaining({ type: "CastSpell" }),
    );
  });

  it("shows an opponent's publicly-revealed library top with no castable affordance, hiding the rest", () => {
    // CR 701.20b: an opponent's library top revealed to all players (their own
    // Oracle of Mul Daya / a public RevealTop) is visible to this viewer via
    // `revealed_cards`, but the viewer has NO play permission — legalActionsByObject
    // is empty and clicking is inert. The rest of the opponent's library is NOT
    // in revealed_cards, so it must not leak even though raw state carries names.
    const revealed = makeObject({
      id: 40,
      owner: 1,
      controller: 1,
      zone: "Library",
      name: "Courser of Kruphix",
      keywords: [],
      base_keywords: [],
    });
    const unrevealed = makeObject({
      id: 41,
      owner: 1,
      controller: 1,
      zone: "Library",
      name: "Lightning Bolt",
    });
    const base = makeState(revealed);
    const gameState = {
      ...base,
      objects: { [revealed.id]: revealed, [unrevealed.id]: unrevealed },
      revealed_cards: [revealed.id],
      players: [
        { ...base.players[0], graveyard: [] },
        { ...base.players[1], graveyard: [], library: [revealed.id, unrevealed.id] },
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

    // Only the publicly-revealed card shows; the unrevealed opponent card (real
    // name in raw state) is filtered out — no leak.
    expect(screen.getAllByTestId("card-image")).toHaveLength(1);
    expect(screen.getByLabelText("Courser of Kruphix")).toBeInTheDocument();
    expect(screen.queryByLabelText("Lightning Bolt")).not.toBeInTheDocument();

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
