import { useRef, useState } from "react";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameAction, GameObject } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import {
  buildGameObjectWithCoreTypes,
  buildObjectMap,
} from "../../../test/factories/gameObjectFactory.ts";
import {
  buildGameState,
  buildPlayers,
  buildPriorityWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";
import { ZoneViewer } from "../ZoneViewer.tsx";

vi.mock("../../card/CardImage.tsx", () => ({
  CardImage: ({
    cardName,
    oracleId,
    faceDown,
  }: {
    cardName: string;
    oracleId?: string;
    faceDown?: boolean;
  }) => (
    <div
      aria-label={faceDown ? "Face-down card" : cardName}
      data-testid="card-image"
      data-oracle-id={faceDown ? "" : oracleId ?? ""}
      data-face-down={String(!!faceDown)}
    />
  ),
}));

const targetDispatch = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => targetDispatch,
}));

function makeObject(overrides: Partial<GameObject> = {}): GameObject {
  return buildGameObjectWithCoreTypes(["Sorcery"], {
    id: 7,
    card_id: 700,
    zone: "Graveyard",
    name: "Flame Jab",
    mana_cost: { type: "Cost", shards: ["Red"], generic: 0 },
    keywords: ["Retrace"],
    color: ["Red"],
    base_keywords: ["Retrace"],
    base_color: ["Red"],
    entered_battlefield_turn: null,
    ...overrides,
  });
}

function makeCastAction(objectId: number): GameAction {
  return {
    type: "CastSpell",
    data: { object_id: objectId, card_id: 700, targets: [] },
  };
}

function makeState(object: GameObject) {
  return buildGameState({
    active_player: 0,
    priority_player: 0,
    players: buildPlayers([
      { id: 0, graveyard: [object.id] },
      { id: 1 },
    ]),
    objects: buildObjectMap(object),
    battlefield: [],
    exile: [],
    stack: [],
    waiting_for: buildPriorityWaitingFor(),
  });
}

function ActionCloseFocusHarness() {
  const [open, setOpen] = useState(false);
  const launcherRef = useRef<HTMLButtonElement | null>(null);
  const fallbackRef = useRef<HTMLButtonElement | null>(null);
  const returnFocusRef = useRef<HTMLElement | SVGElement | null>(null);
  const launcherExists = useGameStore(
    (state) => (state.gameState?.players[0]?.graveyard.length ?? 0) > 0,
  );

  return (
    <>
      {launcherExists && (
        <button
          ref={launcherRef}
          type="button"
          onClick={() => {
            returnFocusRef.current = launcherRef.current;
            setOpen(true);
          }}
        >
          Graveyard pile
        </button>
      )}
      <button ref={fallbackRef} type="button">
        Game menu
      </button>
      {open && (
        <ZoneViewer
          zone="graveyard"
          playerId={0}
          returnFocusRef={returnFocusRef}
          onPrepareActionClose={() => {
            returnFocusRef.current = fallbackRef.current;
          }}
          onClose={() => setOpen(false)}
        />
      )}
    </>
  );
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
    const prepareActionClose = vi.fn();
    render(
      <ZoneViewer
        zone="graveyard"
        playerId={0}
        onClose={vi.fn()}
        onPrepareActionClose={prepareActionClose}
      />,
    );

    // The castable card carries the purple "playable" affordance instead of a
    // labeled button; clicking the card itself routes through handleCast and
    // auto-dispatches the lone CastSpell action.
    fireEvent.click(screen.getByTestId("card-image"));

    expect(dispatch).toHaveBeenCalledTimes(1);
    expect(dispatch).toHaveBeenCalledWith(
      expect.objectContaining({ type: "CastSpell" }),
    );
    expect(prepareActionClose).toHaveBeenCalledTimes(1);
    expect(prepareActionClose.mock.invocationCallOrder[0]).toBeLessThan(
      dispatch.mock.invocationCallOrder[0],
    );
  });

  it("hands action-close focus to a durable target before an async final-card snapshot", async () => {
    let resolveDispatch!: (events: never[]) => void;
    dispatch.mockImplementationOnce(
      () =>
        new Promise<never[]>((resolve) => {
          resolveDispatch = resolve;
        }),
    );
    render(<ActionCloseFocusHarness />);
    fireEvent.click(screen.getByRole("button", { name: "Graveyard pile" }));
    const dialog = screen.getByRole("dialog", { name: /Graveyard/ });
    await waitFor(() => expect(dialog).toHaveFocus());

    fireEvent.click(screen.getByTestId("card-image"));
    await waitFor(() => expect(dialog).not.toBeInTheDocument());
    const gameMenu = screen.getByRole("button", { name: "Game menu" });
    expect(gameMenu).toHaveFocus();

    const current = useGameStore.getState().gameState!;
    act(() => {
      useGameStore.setState({
        gameState: {
          ...current,
          objects: {},
          players: current.players.map((player) =>
            player.id === 0 ? { ...player, graveyard: [] } : player,
          ),
        },
      });
      resolveDispatch([]);
    });

    expect(
      screen.queryByRole("button", { name: "Graveyard pile" }),
    ).not.toBeInTheDocument();
    expect(gameMenu).toHaveFocus();
    expect(document.body).not.toHaveFocus();
  });

  it("identifies visible zone cards for contextual errors", () => {
    render(<ZoneViewer zone="graveyard" playerId={0} onClose={vi.fn()} />);

    expect(screen.getByTestId("card-image").parentElement).toHaveAttribute("data-object-id", "7");
  });

  it("keeps debug card actions inside the zone viewer focus authority", async () => {
    const onClose = vi.fn();
    useUiStore.setState({ debugInteractionMode: true });
    render(<ZoneViewer zone="graveyard" playerId={0} onClose={onClose} />);

    const dialog = screen.getByRole("dialog", { name: /Graveyard/ });
    await waitFor(() => expect(dialog).toHaveFocus());
    const card = within(dialog).getByRole("button", { name: "Flame Jab" });
    vi.spyOn(card, "getBoundingClientRect").mockReturnValue({
      bottom: 340,
      height: 140,
      left: 120,
      right: 220,
      top: 200,
      width: 100,
      x: 120,
      y: 200,
      toJSON: () => ({}),
    });
    fireEvent.click(card);

    const menu = screen.getByRole("menu");
    expect(dialog).not.toContainElement(menu);
    expect(menu).toHaveStyle({ left: "170px", top: "270px" });
    const firstMenuItem = within(menu).getAllByRole("menuitem")[0];
    expect(firstMenuItem).toHaveFocus();

    fireEvent.keyDown(firstMenuItem, { key: "Escape" });

    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(onClose).not.toHaveBeenCalled();
    await waitFor(() => expect(card).toHaveFocus());
  });

  it("does not expose an inert visible card as a button", () => {
    useGameStore.setState({ legalActions: [], legalActionsByObject: {} });
    render(<ZoneViewer zone="graveyard" playerId={0} onClose={vi.fn()} />);

    expect(screen.queryByRole("button", { name: "Flame Jab" })).not.toBeInTheDocument();
    expect(screen.getByLabelText("Flame Jab")).toBeInTheDocument();
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
    // Rust projects identity visibility per object. Real names alone never make
    // a library card render in the viewer.
    const revealed = makeObject({
      id: 20,
      zone: "Library",
      name: "Llanowar Elves",
      display_visible_to_viewer: true,
      keywords: [],
      base_keywords: [],
    });
    // Real names, but no engine display projection → must be filtered out.
    const unrevealedA = makeObject({ id: 21, zone: "Library", name: "Black Lotus" });
    const unrevealedB = makeObject({ id: 22, zone: "Library", name: "Mox Sapphire" });
    const base = makeState(revealed);
    const gameState = {
      ...base,
      objects: buildObjectMap(revealed, unrevealedA, unrevealedB),
      players: [
        { ...base.players[0], graveyard: [], library: [revealed.id, unrevealedA.id, unrevealedB.id] },
        base.players[1],
      ],
    };

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
      display_visible_to_viewer: true,
      keywords: [],
      base_keywords: [],
    });
    const unrevealed = makeObject({ id: 31, zone: "Library", name: "Sol Ring" });
    const action = makeCastAction(revealed.id);
    const base = makeState(revealed);
    const gameState = {
      ...base,
      objects: buildObjectMap(revealed, unrevealed),
      players: [
        { ...base.players[0], graveyard: [], library: [revealed.id, unrevealed.id] },
        base.players[1],
      ],
    };

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
      display_visible_to_viewer: true,
      keywords: [],
      base_keywords: [],
    });
    const buried = makeObject({ id: 51, zone: "Library", name: "Buried Secret" });
    const action = makeCastAction(top.id);
    const base = makeState(top);
    const gameState = {
      ...base,
      objects: buildObjectMap(top, buried),
      players: [
        {
          ...base.players[0],
          graveyard: [],
          library: [top.id, buried.id],
          can_look_at_top_of_library: true,
        },
        base.players[1],
      ],
    };

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
      display_visible_to_viewer: true,
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
      objects: buildObjectMap(revealed, unrevealed),
      players: [
        { ...base.players[0], graveyard: [] },
        { ...base.players[1], graveyard: [], library: [revealed.id, unrevealed.id] },
      ],
    };

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
      objects: buildObjectMap(object),
      exile: [object.id],
      players: [
        { ...base.players[0], graveyard: [] },
        { ...base.players[1], graveyard: [] },
      ],
    };

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

  it("shows an engine-provided time-counter count on a visible exiled card", () => {
    const exiled = makeObject({ zone: "Exile", name: "Rift Bolt" });
    const base = makeState(exiled);
    const gameState = {
      ...base,
      objects: buildObjectMap(exiled),
      players: base.players.map((player) => ({ ...player, graveyard: [] })),
      exile: [exiled.id],
      derived: {
        counter_display: {
          [String(exiled.id)]: { pills: [{ counter: "time", count: 3 }] },
        },
      },
    };
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [],
      legalActionsByObject: {},
      spellCosts: {},
      dispatch,
      gameMode: "ai",
    });

    render(<ZoneViewer zone="exile" playerId={0} onClose={vi.fn()} />);

    expect(screen.getByLabelText("3")).toBeInTheDocument();
  });

  it("does not expose a time-counter badge for a face-down exiled card", () => {
    const hidden = makeObject({
      zone: "Exile",
      name: "Secret Suspend Card",
      face_down: true,
    });
    const base = makeState(hidden);
    const gameState = {
      ...base,
      objects: buildObjectMap(hidden),
      players: base.players.map((player) => ({ ...player, graveyard: [] })),
      exile: [hidden.id],
      derived: {
        counter_display: {
          [String(hidden.id)]: { pills: [{ counter: "time", count: 3 }] },
        },
      },
    };
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [],
      legalActionsByObject: {},
      spellCosts: {},
      dispatch,
      gameMode: "ai",
    });

    render(<ZoneViewer zone="exile" playerId={0} onClose={vi.fn()} />);

    expect(screen.getByLabelText("Face-down card")).toBeInTheDocument();
    expect(screen.queryByLabelText("Secret Suspend Card")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("3")).not.toBeInTheDocument();
  });

  it("renders a time counter without labeling a non-Suspend card as suspended", () => {
    const exiled = makeObject({
      zone: "Exile",
      name: "Timebug",
      keywords: [],
      base_keywords: [],
    });
    const base = makeState(exiled);
    const gameState = {
      ...base,
      objects: buildObjectMap(exiled),
      players: base.players.map((player) => ({ ...player, graveyard: [] })),
      exile: [exiled.id],
      derived: {
        counter_display: {
          [String(exiled.id)]: { pills: [{ counter: "time", count: 2 }] },
        },
      },
    };
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [],
      legalActionsByObject: {},
      spellCosts: {},
      dispatch,
      gameMode: "ai",
    });

    render(<ZoneViewer zone="exile" playerId={0} onClose={vi.fn()} />);

    expect(screen.getByLabelText("2")).toBeInTheDocument();
    expect(screen.queryByText(/suspend/i)).not.toBeInTheDocument();
  });
});
