import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, GameState } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { buildGameObjectWithCoreTypes, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayers, buildPriorityWaitingFor } from "../../../test/factories/gameStateFactory.ts";
import { GameCardPreview } from "../../card/GameCardPreview.tsx";
import { ZoneViewer } from "../ZoneViewer.tsx";

// The hover/long-press preview (GameCardPreview -> CardPreview) reads the raw
// object from the store independently of ZoneViewer's redacted strip — it has
// its own `useCardImage`/engine-data fetches, mocked here the same way
// GameCardPreview.test.tsx does, so the integration test below can render it
// alongside ZoneViewer and simulate a real hover.
vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: () => ({ src: "card.png", isLoading: false, isRotated: false, isFlip: false }),
}));
vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  useEngineCardData: () => null,
  useCardParseDetails: () => null,
  useCardRulings: () => [],
}));

// Issue #2889: a Hideaway permanent exiles a card face down (CR 702.75a). The
// engine's per-viewer `filter_state_for_viewer` already redacts this card on
// the network path, but single-player renders the raw, unredacted state
// directly — so the real `name`/`printed_ref` sit on the object regardless of
// who's looking. Before this fix, `ZoneViewer`'s exile pile rendered that raw
// object straight through `objectImageProps`, leaking an opponent's hidden
// card's name and art the moment the Exile zone was opened. The mock below
// surfaces the `faceDown` prop CardImage receives so these tests can assert
// the placeholder path is taken instead of a real card-art lookup.
vi.mock("../../card/CardImage.tsx", () => ({
  // Mirrors the real CardImage.tsx: when `faceDown` is set, the card's own
  // `cardName`/`oracleId` are ignored entirely in favor of the shared
  // card-back placeholder — that's the behavior under test, so the mock must
  // reproduce it rather than simply echoing whatever props it received.
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

function makeObject(overrides: Partial<GameObject> = {}): GameObject {
  return buildGameObjectWithCoreTypes(["Creature"], {
    id: 7,
    card_id: 700,
    zone: "Exile",
    name: "Placeholder",
    mana_cost: { type: "Cost", shards: [], generic: 0 },
    timestamp: 1,
    entered_battlefield_turn: null,
    ...overrides,
  });
}

function makeState(
  objects: GameObject[],
  exileLinks: NonNullable<GameState["exile_links"]> = [],
): GameState {
  return buildGameState({
    priority_player: 0,
    players: buildPlayers([0, 1]),
    objects: buildObjectMap(...objects),
    battlefield: objects.filter((o) => o.zone === "Battlefield").map((o) => o.id),
    exile: objects.filter((o) => o.zone === "Exile").map((o) => o.id),
    stack: [],
    exile_links: exileLinks,
    waiting_for: buildPriorityWaitingFor(),
  });
}

describe("ZoneViewer exile face-down visibility (issue #2889)", () => {
  const dispatch = vi.fn(async () => []);

  beforeEach(() => {
    dispatch.mockClear();
    useUiStore.setState({
      inspectedObjectId: null,
      previewSticky: false,
      pendingAbilityChoice: null,
      debugInteractionMode: false,
    });
    // Desktop hover path: `useInspectHoverProps` gates onMouseEnter on
    // `useCanHover` (any-hover media query) and `useIsMobile` (jsdom's default
    // 1024px innerWidth already reads as non-mobile).
    window.matchMedia = ((query: string) => ({
      matches: query === "(any-hover: hover)",
      media: query,
      onchange: null,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(),
    })) as unknown as typeof window.matchMedia;
  });

  afterEach(() => {
    cleanup();
  });

  it("hides an opponent's Hideaway-exiled card from a viewer with no look-permission", () => {
    // CR 702.75a: the permanent's controller (player 1) may look; the viewer
    // (player 0, the default single-player viewer) is the opponent and must
    // not see the real card.
    const source = makeObject({
      id: 1,
      owner: 1,
      controller: 1,
      zone: "Battlefield",
      name: "Windbrisk Heights",
    });
    const hidden = makeObject({
      id: 2,
      owner: 1,
      controller: 1,
      zone: "Exile",
      name: "Ghalta, Primal Hunter",
      face_down: true,
      printed_ref: { oracle_id: "ghalta-oracle", face_name: "Ghalta, Primal Hunter" },
    });
    const gameState = makeState(
      [source, hidden],
      [{ exiled_id: 2, source_id: 1, kind: "HideawayLookable" }],
    );

    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [],
      legalActionsByObject: {},
      spellCosts: {},
      dispatch,
      gameMode: "ai",
    });

    render(<ZoneViewer zone="exile" playerId={1} onClose={vi.fn()} />);

    const image = screen.getByTestId("card-image");
    expect(image.getAttribute("data-face-down")).toBe("true");
    expect(image.getAttribute("data-oracle-id")).toBe("");
    expect(screen.queryByLabelText("Ghalta, Primal Hunter")).not.toBeInTheDocument();
  });

  it("reveals the real card to the controller of the Hideaway permanent that exiled it", () => {
    const source = makeObject({
      id: 1,
      owner: 0,
      controller: 0,
      zone: "Battlefield",
      name: "Windbrisk Heights",
    });
    const hidden = makeObject({
      id: 2,
      owner: 0,
      controller: 0,
      zone: "Exile",
      name: "Ghalta, Primal Hunter",
      face_down: true,
      printed_ref: { oracle_id: "ghalta-oracle", face_name: "Ghalta, Primal Hunter" },
    });
    const gameState = makeState(
      [source, hidden],
      [{ exiled_id: 2, source_id: 1, kind: "HideawayLookable" }],
    );

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

    const image = screen.getByTestId("card-image");
    expect(image.getAttribute("data-face-down")).toBe("false");
    expect(image.getAttribute("data-oracle-id")).toBe("ghalta-oracle");
    expect(screen.getByLabelText("Ghalta, Primal Hunter")).toBeInTheDocument();
  });

  it("keeps a plain TrackedBySource face-down exile hidden even from the exiling permanent's controller", () => {
    // Regression guard mirroring crates/engine/src/database/hideaway_tests.rs's
    // `tracked_by_source_face_down_exile_stays_hidden_from_controller`: a
    // Bomat-Courier-style link grants no look-permission, so even the
    // exiling permanent's own controller must see a face-down placeholder.
    const source = makeObject({
      id: 1,
      owner: 0,
      controller: 0,
      zone: "Battlefield",
      name: "Bomat Courier",
    });
    const hidden = makeObject({
      id: 2,
      owner: 0,
      controller: 0,
      zone: "Exile",
      name: "Secret Card",
      face_down: true,
    });
    const gameState = makeState(
      [source, hidden],
      [{ exiled_id: 2, source_id: 1, kind: "TrackedBySource" }],
    );

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

    const image = screen.getByTestId("card-image");
    expect(image.getAttribute("data-face-down")).toBe("true");
    expect(screen.queryByLabelText("Secret Card")).not.toBeInTheDocument();
  });

  it("reveals a foretold card to its owner but hides it from an opponent", () => {
    // CR 702.143e: a foretold card's owner may look at it.
    const ownForetold = makeObject({
      id: 3,
      owner: 0,
      controller: 0,
      zone: "Exile",
      name: "Alrund's Epiphany",
      face_down: true,
      foretold: true,
      printed_ref: { oracle_id: "epiphany-oracle", face_name: "Alrund's Epiphany" },
    });
    const gameState = makeState([ownForetold]);

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

    expect(screen.getByTestId("card-image").getAttribute("data-face-down")).toBe("false");
    expect(screen.getByLabelText("Alrund's Epiphany")).toBeInTheDocument();
  });

  it("does not leak the real card via hover/long-press preview either", () => {
    // The strip thumbnail is only half the surface: ZoneCard still wires
    // `hoverProps(obj.id)` (mouseenter on desktop, long-press on touch) to
    // `inspectObject`, and GameCardPreview resolves that id straight from the
    // RAW, unredacted `gameState.objects` — independent of the redacted strip
    // image above. It stays safe because `GameCardPreview` nulls the derived
    // `cardName` whenever `obj.face_down` is true (regardless of viewer, the
    // same fail-closed rule CR 708.5 already applies to a face-down battlefield
    // permanent — see GameCardPreview.test.tsx's "never previews a face-down
    // permanent" case), and `CardPreview` renders nothing at all when
    // `cardName` is null. This test exercises that path end-to-end through a
    // real hover instead of relying on the unit test alone.
    const source = makeObject({
      id: 1,
      owner: 1,
      controller: 1,
      zone: "Battlefield",
      name: "Windbrisk Heights",
    });
    const hidden = makeObject({
      id: 2,
      owner: 1,
      controller: 1,
      zone: "Exile",
      name: "Ghalta, Primal Hunter",
      face_down: true,
      printed_ref: { oracle_id: "ghalta-oracle", face_name: "Ghalta, Primal Hunter" },
    });
    const gameState = makeState(
      [source, hidden],
      [{ exiled_id: 2, source_id: 1, kind: "HideawayLookable" }],
    );

    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [],
      legalActionsByObject: {},
      spellCosts: {},
      dispatch,
      gameMode: "ai",
    });
    usePreferencesStore.setState({ cardPreviewMode: "follow" });

    render(
      <>
        <ZoneViewer zone="exile" playerId={1} onClose={vi.fn()} />
        <GameCardPreview />
      </>,
    );

    const cardImage = screen.getByTestId("card-image");
    const hoverTarget = cardImage.parentElement as HTMLElement;
    fireEvent.mouseEnter(hoverTarget);

    // The hover did wire up — `inspectObject` ran — but the preview must
    // render nothing for a face-down card with no look-permission.
    expect(useUiStore.getState().inspectedObjectId).toBe(hidden.id);
    expect(screen.queryByAltText("Ghalta, Primal Hunter")).not.toBeInTheDocument();
    expect(document.querySelector("[data-card-preview]")).toBeNull();

    fireEvent.mouseLeave(hoverTarget);
  });
});
