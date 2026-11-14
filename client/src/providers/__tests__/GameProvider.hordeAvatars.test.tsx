import { act } from "react";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../services/serverDetection.ts", () => ({
  detectServerUrl: vi.fn(() => new Promise<string>(() => {})),
}));

// Boss art resolves through the token-image sidecar, a static asset the
// test environment does not serve. Stub the resolution keyed by the
// requested printing — an unkeyed stub would stay green while the HUD
// showed another Boss's portrait.
const { fetchTokenImageAssetByRef } = vi.hoisted(() => ({
  fetchTokenImageAssetByRef: vi.fn(),
}));
vi.mock("../../services/scryfall.ts", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../services/scryfall.ts")>()),
  fetchTokenImageAssetByRef,
}));

// The rendered witness below mounts a real seat header; survivor card art
// is irrelevant there, so the card-image hook stays stubbed like the
// header's own visual-avatar suite stubs it.
vi.mock("../../hooks/useCardImage.ts", () => ({
  useCardImage: vi.fn(() => ({
    src: null,
    isLoading: false,
    source: null,
    advanceFailedSource: vi.fn(),
  })),
}));

import type { HordeRuleset, TokenImageRef } from "../../adapter/types.ts";
import { OpponentSeatHeader } from "../../components/board/OpponentSeatHeader.tsx";
import { assignRandomAvatars } from "../../services/playerAvatars.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore.ts";
import { useMultiplayerStore } from "../../stores/multiplayerStore.ts";
import {
  buildCommanderFormatConfig,
  buildFormatConfig,
  buildGameState,
  buildPlayers,
} from "../../test/factories/gameStateFactory.ts";
import { GameProvider } from "../GameProvider.tsx";

const TURTLE_TEAMUP_RULESET: HordeRuleset = {
  challenge_deck: "TurtleTeamUpHorde",
  wave: { type: "FixedCount", data: 2 },
  survivor_setup_turns: 0,
  combined_base_life: 10,
  per_extra_survivor_life_delta: 10,
  horde_creatures_forced_attackers: true,
  horde_creatures_haste: false,
};

// Real pinned refs from `known-tokens.overlay.toml` — every Boss fixture
// carries its OWN printing, so a portrait mixup reddens instead of passing
// under a shared id.
const SHREDDER_REF: TokenImageRef = {
  preset_id: "f164b86f-196f-5372-8c7e-279b7d1281de",
  scryfall_id: "19f04282-ef44-4474-bb0f-bcc938bdba7a",
  scryfall_oracle_id: "8fbaded5-2847-4cee-869f-6bee45294ba4",
};
const KRANG_REF: TokenImageRef = {
  preset_id: "df03a49b-89dd-536c-8d5a-819cb0318e4b",
  scryfall_id: "8742baf6-2bf9-48ac-af68-6427704b3593",
  scryfall_oracle_id: "d63579be-52d9-469d-8976-60c16b73bc93",
};
const SLASH_REF: TokenImageRef = {
  preset_id: "8a66162e-10cd-5f12-ad48-dcad19b41de2",
  scryfall_id: "6d84f8fc-0da6-4acc-9da2-9993daabd9f4",
  scryfall_oracle_id: "59404596-24c2-4c7e-8514-969d9f6e8e6d",
};
const CHROME_DOME_REF: TokenImageRef = {
  preset_id: "aa8dcb67-bffa-5bb5-8ec4-6cfa6e7f87c0",
  scryfall_id: "9a2d23c4-d381-4a92-b4b1-0cf71948f941",
  scryfall_oracle_id: "00fd090a-2021-41ac-be80-07c7c23027e3",
};

// Sidecar-resolved art lives on the image CDN host the desktop CSP allows
// as an image source; the Scryfall API host is fetch-only there.
const cdnArtCrop = (printingId: string) =>
  `https://cards.scryfall.io/art_crop/front/${printingId[0]}/${printingId[1]}/${printingId}.jpg`;
const BOSS_ART_URLS: Record<string, string> = {
  [SHREDDER_REF.scryfall_id]: cdnArtCrop(SHREDDER_REF.scryfall_id),
  [KRANG_REF.scryfall_id]: cdnArtCrop(KRANG_REF.scryfall_id),
  [SLASH_REF.scryfall_id]: cdnArtCrop(SLASH_REF.scryfall_id),
  [CHROME_DOME_REF.scryfall_id]: cdnArtCrop(CHROME_DOME_REF.scryfall_id),
};

function mockKeyedSidecar() {
  fetchTokenImageAssetByRef.mockImplementation(async (ref: TokenImageRef) => {
    const src = BOSS_ART_URLS[ref.scryfall_id];
    return src ? { src, faceIndex: 0 } : null;
  });
}

interface BossFixture {
  id: number;
  name: string;
  owner: number;
  ref: TokenImageRef;
}

function bossObject(boss: BossFixture) {
  return {
    name: boss.name,
    owner: boss.owner,
    is_horde_deck_token: true,
    token_image_ref: boss.ref,
  };
}

/** Turtle Team-Up state shaped like a delivered viewer snapshot: seat and Boss
 *  identity arrive via `derived.horde`, the engine-authored projection. */
function tmntState(options: {
  players: { id: number }[];
  hordeSeats: number[];
  heroes: number[];
  bosses: BossFixture[];
  deployedBossIds: number[];
  challengeDeck?: HordeRuleset["challenge_deck"];
  commandZone?: number[];
  extraObjects?: Record<number, object>;
}) {
  const objects: Record<number, object> = {};
  for (const boss of options.bosses) {
    objects[boss.id] = bossObject(boss);
  }
  Object.assign(objects, options.extraObjects ?? {});
  return {
    ...buildGameState({
      players: buildPlayers(options.players),
    }),
    format_config: buildFormatConfig({
      format: "Horde",
      horde_ruleset: {
        ...TURTLE_TEAMUP_RULESET,
        challenge_deck: options.challengeDeck ?? "TurtleTeamUpHorde",
      },
    }),
    horde_seats: options.hordeSeats,
    command_zone: options.commandZone ?? options.deployedBossIds,
    objects,
    derived: {
      horde: {
        horde_seat_ids: options.hordeSeats,
        hero_player_ids: options.heroes,
        deployed_boss_ids: options.deployedBossIds,
      },
    },
  } as never;
}

function expectedRandom(playerCount: number, seed: string) {
  return new Map(
    assignRandomAvatars(playerCount, seed).map((avatar, playerId) => [
      playerId,
      { kind: "card" as const, cardName: avatar.cardName },
    ]),
  );
}

function expectedRandomNames(playerCount: number, seed: string) {
  const names = new Map<number, string>([[0, "You"]]);
  for (const [playerId, avatar] of assignRandomAvatars(playerCount, seed).entries()) {
    if (playerId > 0) names.set(playerId, avatar.name);
  }
  return names;
}

/** Two-seat Commander game: the commander branch derives both seats. */
function commanderState() {
  return {
    ...buildGameState({ players: buildPlayers([{ id: 0 }, { id: 1 }]) }),
    format_config: buildCommanderFormatConfig(),
    objects: {
      1: { name: "Atraxa, Praetors' Voice", owner: 0, is_commander: true },
      2: { name: "Muldrotha, the Gravetide", owner: 1, is_commander: true },
    },
  } as never;
}

describe("GameProvider Horde seat identities", () => {
  beforeEach(() => {
    fetchTokenImageAssetByRef.mockReset();
    mockKeyedSidecar();
    localStorage.clear();
    useMultiplayerStore.setState({
      playerNames: new Map(),
      playerAvatars: new Map(),
      activePlayerId: 0,
    });
    useMultiplayerDraftStore.setState({ matchPairing: null });
    useGameStore.setState({
      adapter: null,
      gameId: null,
      gameState: null,
      gameMode: null,
      waitingFor: null,
    });
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it("installs Turtle and Boss identities for a Turtle Team-Up Horde game", async () => {
    const gameId = "horde-avatars-tmnt";
    render(
      <GameProvider gameId={gameId} mode="ai" playerCount={2}>
        <div />
      </GameProvider>,
    );

    act(() => {
      useGameStore.setState({
        gameId,
        gameState: tmntState({
          players: [{ id: 0 }, { id: 1 }],
          hordeSeats: [1],
          heroes: [0],
          bosses: [{ id: 100, name: "Shredder, Foot Clan Overlord", owner: 1, ref: SHREDDER_REF }],
          deployedBossIds: [100],
        }),
      });
    });

    expect(useMultiplayerStore.getState().playerNames).toEqual(
      new Map([[0, "Leonardo"], [1, "Shredder"]]),
    );
    // Names land synchronously; the Boss portrait resolves async through
    // the sidecar and merges into the stored map when it lands.
    expect(useMultiplayerStore.getState().playerAvatars).toEqual(new Map([
      [0, { kind: "card", cardName: "Leonardo, Sewer Samurai" }],
    ]));
    expect(fetchTokenImageAssetByRef).toHaveBeenCalledWith(
      expect.objectContaining({ scryfall_id: SHREDDER_REF.scryfall_id }),
      "art_crop",
    );
    await waitFor(() => {
      expect(useMultiplayerStore.getState().playerAvatars).toEqual(new Map([
        [0, { kind: "card", cardName: "Leonardo, Sewer Samurai" }],
        [1, { kind: "external", url: BOSS_ART_URLS[SHREDDER_REF.scryfall_id] }],
      ]));
    });
  });

  it("assigns Turtles in seat order across allied survivors", async () => {
    const gameId = "horde-avatars-allies";
    render(
      <GameProvider gameId={gameId} mode="ai" playerCount={3}>
        <div />
      </GameProvider>,
    );

    act(() => {
      useGameStore.setState({
        gameId,
        gameState: tmntState({
          players: [{ id: 0 }, { id: 1 }, { id: 2 }],
          hordeSeats: [1],
          heroes: [0, 2],
          bosses: [{ id: 100, name: "Krang, Dimension X Overlord", owner: 1, ref: KRANG_REF }],
          deployedBossIds: [100],
        }),
      });
    });

    expect(useMultiplayerStore.getState().playerNames).toEqual(
      new Map([[0, "Leonardo"], [1, "Krang"], [2, "Raphael"]]),
    );
    expect(useMultiplayerStore.getState().playerAvatars.get(2)).toEqual({
      kind: "card",
      cardName: "Raphael, Ninja Destroyer",
    });
    // Flush the async Boss merge so no late write escapes this test.
    await waitFor(() => {
      expect(useMultiplayerStore.getState().playerAvatars.get(1)).toEqual({
        kind: "external",
        url: BOSS_ART_URLS[KRANG_REF.scryfall_id],
      });
    });
  });

  it("keeps the seeded identities for other challenge decks", () => {
    const gameId = "horde-avatars-other-deck";
    render(
      <GameProvider gameId={gameId} mode="ai" playerCount={2}>
        <div />
      </GameProvider>,
    );

    act(() => {
      // Synthetic: classic decks never deploy Bosses, but the deployed Boss
      // here proves the deck gate — not Boss presence — selects theming.
      useGameStore.setState({
        gameId,
        gameState: tmntState({
          players: [{ id: 0 }, { id: 1 }],
          hordeSeats: [1],
          heroes: [0],
          bosses: [{ id: 100, name: "Krang, Dimension X Overlord", owner: 1, ref: KRANG_REF }],
          deployedBossIds: [100],
          challengeDeck: "CybermanHorde",
        }),
      });
    });

    // No themed seats outside Turtle Team-Up: the random fallback survives.
    expect(useMultiplayerStore.getState().playerAvatars).toEqual(expectedRandom(2, gameId));
    expect(useMultiplayerStore.getState().playerNames.get(0)).toBe("You");
  });

  it("waits for the Boss when none is deployed yet, then applies", async () => {
    const gameId = "horde-avatars-late-boss";
    render(
      <GameProvider gameId={gameId} mode="ai" playerCount={2}>
        <div />
      </GameProvider>,
    );

    const slash = { id: 100, name: "Slash, the Dark Mirror", owner: 1, ref: SLASH_REF };
    act(() => {
      useGameStore.setState({
        gameId,
        gameState: tmntState({
          players: [{ id: 0 }, { id: 1 }],
          hordeSeats: [1],
          heroes: [0],
          bosses: [slash],
          deployedBossIds: [],
        }),
      });
    });
    expect(useMultiplayerStore.getState().playerAvatars).toEqual(expectedRandom(2, gameId));

    act(() => {
      useGameStore.setState({
        gameState: tmntState({
          players: [{ id: 0 }, { id: 1 }],
          hordeSeats: [1],
          heroes: [0],
          bosses: [slash],
          deployedBossIds: [100],
        }),
      });
    });
    expect(useMultiplayerStore.getState().playerNames).toEqual(
      new Map([[0, "Leonardo"], [1, "Slash"]]),
    );
    await waitFor(() => {
      expect(useMultiplayerStore.getState().playerAvatars.get(1)).toEqual({
        kind: "external",
        url: BOSS_ART_URLS[SLASH_REF.scryfall_id],
      });
    });
  });

  it("follows the engine projection, not the command-zone scan or the raw seat vector", async () => {
    const gameId = "horde-avatars-decoy";
    render(
      <GameProvider gameId={gameId} mode="ai" playerCount={2}>
        <div />
      </GameProvider>,
    );

    // The command zone leads with a decoy Horde token the old scan would
    // select, and the raw seat vector is omitted the way a compatible older
    // snapshot omits it — while `derived.horde` names the true Boss/seats.
    const state = tmntState({
      players: [{ id: 0 }, { id: 1 }],
      hordeSeats: [1],
      heroes: [0],
      bosses: [{ id: 100, name: "Shredder, Foot Clan Overlord", owner: 1, ref: SHREDDER_REF }],
      deployedBossIds: [100],
      commandZone: [7, 100],
      extraObjects: {
        7: {
          name: "Decoy Doom",
          owner: 1,
          is_horde_deck_token: true,
          token_image_ref: CHROME_DOME_REF,
        },
      },
    }) as Record<string, unknown>;
    delete state.horde_seats;
    act(() => {
      useGameStore.setState({ gameId, gameState: state as never });
    });

    expect(useMultiplayerStore.getState().playerNames).toEqual(
      new Map([[0, "Leonardo"], [1, "Shredder"]]),
    );
    expect(fetchTokenImageAssetByRef).toHaveBeenCalledWith(
      expect.objectContaining({ scryfall_id: SHREDDER_REF.scryfall_id }),
      "art_crop",
    );
    await waitFor(() => {
      expect(useMultiplayerStore.getState().playerAvatars.get(1)).toEqual({
        kind: "external",
        url: BOSS_ART_URLS[SHREDDER_REF.scryfall_id],
      });
    });
  });

  it("re-derives the seat when a stage clear replaces the deployed Boss", async () => {
    const gameId = "horde-avatars-stage-clear";
    render(
      <GameProvider gameId={gameId} mode="ai" playerCount={2}>
        <div />
      </GameProvider>,
    );

    const stage1 = () =>
      tmntState({
        players: [{ id: 0 }, { id: 1 }],
        hordeSeats: [1],
        heroes: [0],
        bosses: [{ id: 100, name: "Shredder, Foot Clan Overlord", owner: 1, ref: SHREDDER_REF }],
        deployedBossIds: [100],
      });
    const stage2 = () =>
      tmntState({
        players: [{ id: 0 }, { id: 1 }],
        hordeSeats: [1],
        heroes: [0],
        bosses: [
          { id: 200, name: "Krang, Dimension X Overlord", owner: 1, ref: KRANG_REF },
          { id: 201, name: "Slash, the Dark Mirror", owner: 1, ref: SLASH_REF },
        ],
        deployedBossIds: [200, 201],
      });

    act(() => {
      useGameStore.setState({ gameId, gameState: stage1() });
    });
    await waitFor(() => {
      expect(useMultiplayerStore.getState().playerAvatars.get(1)).toEqual({
        kind: "external",
        url: BOSS_ART_URLS[SHREDDER_REF.scryfall_id],
      });
    });

    // Same mount, next snapshot: continuous play must follow the new Boss,
    // not keep the cleared stage's seat.
    act(() => {
      useGameStore.setState({ gameState: stage2() });
    });
    expect(useMultiplayerStore.getState().playerNames).toEqual(
      new Map([[0, "Leonardo"], [1, "Krang"]]),
    );
    await waitFor(() => {
      expect(useMultiplayerStore.getState().playerAvatars.get(1)).toEqual({
        kind: "external",
        url: BOSS_ART_URLS[KRANG_REF.scryfall_id],
      });
    });

    // A remount on the same position agrees with continuous play: the
    // seat effect derives after setup re-seeds the random fallback, so the
    // mount run itself restores the theme with no new snapshot required.
    cleanup();
    render(
      <GameProvider gameId={gameId} mode="ai" playerCount={2}>
        <div />
      </GameProvider>,
    );
    expect(useMultiplayerStore.getState().playerNames).toEqual(
      new Map([[0, "Leonardo"], [1, "Krang"]]),
    );
    await waitFor(() => {
      expect(useMultiplayerStore.getState().playerAvatars.get(1)).toEqual({
        kind: "external",
        url: BOSS_ART_URLS[KRANG_REF.scryfall_id],
      });
    });
  });

  it("drops a pending portrait when a session replace lands under the same game id", async () => {
    const gameId = "horde-avatars-session-replace";
    render(
      <GameProvider gameId={gameId} mode="ai" playerCount={2}>
        <div />
      </GameProvider>,
    );

    const pending: Array<{ ref: TokenImageRef; resolve: (value: unknown) => void }> = [];
    fetchTokenImageAssetByRef.mockImplementation(
      (ref: TokenImageRef) =>
        new Promise((resolve) => {
          pending.push({ ref, resolve });
        }),
    );

    const shredder = { id: 100, name: "Shredder, Foot Clan Overlord", owner: 1, ref: SHREDDER_REF };
    act(() => {
      useGameStore.setState({
        gameId,
        gameState: tmntState({
          players: [{ id: 0 }, { id: 1 }],
          hordeSeats: [1],
          heroes: [0],
          bosses: [shredder],
          deployedBossIds: [100],
        }),
      });
    });
    expect(pending).toHaveLength(1);

    // A debug import replaces the session under the same route id: only the
    // generation advances. The replacement game deploys Krang instead.
    const generation = useGameStore.getState().gameSessionGeneration;
    act(() => {
      useGameStore.setState({
        gameSessionGeneration: generation + 1,
        gameState: tmntState({
          players: [{ id: 0 }, { id: 1 }],
          hordeSeats: [1],
          heroes: [0],
          bosses: [{ id: 100, name: "Krang, Dimension X Overlord", owner: 1, ref: KRANG_REF }],
          deployedBossIds: [100],
        }),
      });
    });
    expect(pending).toHaveLength(2);

    // The stale resolve lands after the replace: it must not paint.
    await act(async () => {
      pending[0].resolve({ src: BOSS_ART_URLS[SHREDDER_REF.scryfall_id], faceIndex: 0 });
    });
    expect(useMultiplayerStore.getState().playerAvatars.get(1)).toBeUndefined();

    // The replacement game's own portrait still applies.
    await act(async () => {
      pending[1].resolve({ src: BOSS_ART_URLS[KRANG_REF.scryfall_id], faceIndex: 0 });
    });
    expect(useMultiplayerStore.getState().playerAvatars.get(1)).toEqual({
      kind: "external",
      url: BOSS_ART_URLS[KRANG_REF.scryfall_id],
    });
  });

  it("drops a pending portrait when a stage clear replaces the Boss mid-flight", async () => {
    const gameId = "horde-avatars-boss-replace";
    render(
      <GameProvider gameId={gameId} mode="ai" playerCount={2}>
        <div />
      </GameProvider>,
    );

    const pending: Array<{ ref: TokenImageRef; resolve: (value: unknown) => void }> = [];
    fetchTokenImageAssetByRef.mockImplementation(
      (ref: TokenImageRef) =>
        new Promise((resolve) => {
          pending.push({ ref, resolve });
        }),
    );

    act(() => {
      useGameStore.setState({
        gameId,
        gameState: tmntState({
          players: [{ id: 0 }, { id: 1 }],
          hordeSeats: [1],
          heroes: [0],
          bosses: [{ id: 100, name: "Shredder, Foot Clan Overlord", owner: 1, ref: SHREDDER_REF }],
          deployedBossIds: [100],
        }),
      });
    });
    expect(pending).toHaveLength(1);

    // Same session, next stage: Krang deploys and kicks his own fetch.
    act(() => {
      useGameStore.setState({
        gameState: tmntState({
          players: [{ id: 0 }, { id: 1 }],
          hordeSeats: [1],
          heroes: [0],
          bosses: [{ id: 200, name: "Krang, Dimension X Overlord", owner: 1, ref: KRANG_REF }],
          deployedBossIds: [200],
        }),
      });
    });
    expect(pending).toHaveLength(2);

    await act(async () => {
      pending[0].resolve({ src: BOSS_ART_URLS[SHREDDER_REF.scryfall_id], faceIndex: 0 });
    });
    expect(useMultiplayerStore.getState().playerAvatars.get(1)).toBeUndefined();

    await act(async () => {
      pending[1].resolve({ src: BOSS_ART_URLS[KRANG_REF.scryfall_id], faceIndex: 0 });
    });
    expect(useMultiplayerStore.getState().playerAvatars.get(1)).toEqual({
      kind: "external",
      url: BOSS_ART_URLS[KRANG_REF.scryfall_id],
    });
  });

  it("renders the Horde portrait from a desktop-allowed host, never the Scryfall API", async () => {
    const gameId = "horde-avatars-witness";
    render(
      <GameProvider gameId={gameId} mode="ai" playerCount={2}>
        <div />
      </GameProvider>,
    );

    act(() => {
      useGameStore.setState({
        gameId,
        gameState: tmntState({
          players: [{ id: 0 }, { id: 1 }],
          hordeSeats: [1],
          heroes: [0],
          bosses: [{ id: 100, name: "Shredder, Foot Clan Overlord", owner: 1, ref: SHREDDER_REF }],
          deployedBossIds: [100],
        }),
      });
    });
    await waitFor(() => {
      expect(useMultiplayerStore.getState().playerAvatars.get(1)).toEqual({
        kind: "external",
        url: BOSS_ART_URLS[SHREDDER_REF.scryfall_id],
      });
    });

    // The desktop CSP allows api.scryfall.com for fetches only — an <img>
    // pointed there renders as initials. The stored portrait must already
    // be the sidecar-resolved CDN URL when the HUD renders it.
    render(<OpponentSeatHeader playerId={1} />);
    const portrait = screen.getByRole("img", { name: "Shredder" });
    expect(portrait).toHaveAttribute("src", BOSS_ART_URLS[SHREDDER_REF.scryfall_id]);
    expect(portrait.getAttribute("src")).not.toContain("api.scryfall.com");
  });

  it("renders the requested printing's art, not the named Boss's", async () => {
    const gameId = "horde-avatars-wrong-ref";
    render(
      <GameProvider gameId={gameId} mode="ai" playerCount={2}>
        <div />
      </GameProvider>,
    );

    // Shredder's name carrying Chrome Dome's ref: the portrait must follow
    // the ref. An unkeyed resolver stub renders the wrong art here.
    act(() => {
      useGameStore.setState({
        gameId,
        gameState: tmntState({
          players: [{ id: 0 }, { id: 1 }],
          hordeSeats: [1],
          heroes: [0],
          bosses: [{ id: 100, name: "Shredder, Foot Clan Overlord", owner: 1, ref: CHROME_DOME_REF }],
          deployedBossIds: [100],
        }),
      });
    });

    render(<OpponentSeatHeader playerId={1} />);
    const portrait = await screen.findByRole("img", { name: "Shredder" });
    await waitFor(() => {
      expect(portrait).toHaveAttribute("src", BOSS_ART_URLS[CHROME_DOME_REF.scryfall_id]);
    });
  });

  it("leaves the Horde seat on the initial tile when the sidecar misses", async () => {
    fetchTokenImageAssetByRef.mockResolvedValue(null);
    const gameId = "horde-avatars-sidecar-miss";
    render(
      <GameProvider gameId={gameId} mode="ai" playerCount={2}>
        <div />
      </GameProvider>,
    );

    act(() => {
      useGameStore.setState({
        gameId,
        gameState: tmntState({
          players: [{ id: 0 }, { id: 1 }],
          hordeSeats: [1],
          heroes: [0],
          bosses: [{ id: 100, name: "Shredder, Foot Clan Overlord", owner: 1, ref: SHREDDER_REF }],
          deployedBossIds: [100],
        }),
      });
    });

    expect(useMultiplayerStore.getState().playerNames).toEqual(
      new Map([[0, "Leonardo"], [1, "Shredder"]]),
    );
    // waitFor polls on real timers, so once it observes the kickoff the
    // whole microtask merge chain has drained: a phantom write would have
    // landed by the time the absence below is read.
    await waitFor(() => {
      expect(fetchTokenImageAssetByRef).toHaveBeenCalledWith(
        expect.objectContaining({ scryfall_id: SHREDDER_REF.scryfall_id }),
        "art_crop",
      );
    });
    expect(useMultiplayerStore.getState().playerAvatars).toEqual(new Map([
      [0, { kind: "card", cardName: "Leonardo, Sewer Samurai" }],
    ]));
  });

  it("sheds Turtle theming when a replacement session deploys no themed Boss", async () => {
    const gameId = "horde-avatars-turtle-to-cyberman";
    render(
      <GameProvider gameId={gameId} mode="ai" playerCount={2}>
        <div />
      </GameProvider>,
    );

    act(() => {
      useGameStore.setState({
        gameId,
        gameState: tmntState({
          players: [{ id: 0 }, { id: 1 }],
          hordeSeats: [1],
          heroes: [0],
          bosses: [{ id: 100, name: "Shredder, Foot Clan Overlord", owner: 1, ref: SHREDDER_REF }],
          deployedBossIds: [100],
        }),
      });
    });
    // Let the Boss portrait merge first: the reseed must clear even the
    // async entry, not just the synchronous names.
    await waitFor(() => {
      expect(useMultiplayerStore.getState().playerAvatars.get(1)).toEqual({
        kind: "external",
        url: BOSS_ART_URLS[SHREDDER_REF.scryfall_id],
      });
    });

    // A debug import swaps the deck under the same route id: only the
    // generation advances, and neither branch claims the replacement.
    // Bumped from the store's own value like the other replacement tests:
    // the module counter can lag the store after earlier `+ 1` bumps, so
    // `nextGameSessionGeneration()` could return the value already stored.
    const cyberGeneration = useGameStore.getState().gameSessionGeneration;
    act(() => {
      useGameStore.setState({
        gameSessionGeneration: cyberGeneration + 1,
        gameState: tmntState({
          players: [{ id: 0 }, { id: 1 }],
          hordeSeats: [1],
          heroes: [0],
          bosses: [],
          deployedBossIds: [],
          challengeDeck: "CybermanHorde",
        }),
      });
    });

    expect(useMultiplayerStore.getState().playerNames).toEqual(
      expectedRandomNames(2, gameId),
    );
    expect(useMultiplayerStore.getState().playerAvatars).toEqual(
      expectedRandom(2, gameId),
    );
    expect(fetchTokenImageAssetByRef).toHaveBeenCalledTimes(1);
  });

  it("selects Turtle theming when a replacement session follows a Commander game", async () => {
    const gameId = "horde-avatars-commander-to-turtle";
    render(
      <GameProvider gameId={gameId} mode="ai" playerCount={2}>
        <div />
      </GameProvider>,
    );

    act(() => {
      useGameStore.setState({ gameId, gameState: commanderState() });
    });
    // The commander one-shot fires — and must not deafen the provider to
    // the replacement that follows.
    expect(useMultiplayerStore.getState().playerNames).toEqual(
      new Map([[0, "Atraxa"], [1, "Muldrotha"]]),
    );

    const turtleGeneration = useGameStore.getState().gameSessionGeneration;
    act(() => {
      useGameStore.setState({
        gameSessionGeneration: turtleGeneration + 1,
        gameState: tmntState({
          players: [{ id: 0 }, { id: 1 }],
          hordeSeats: [1],
          heroes: [0],
          bosses: [{ id: 100, name: "Shredder, Foot Clan Overlord", owner: 1, ref: SHREDDER_REF }],
          deployedBossIds: [100],
        }),
      });
    });

    expect(useMultiplayerStore.getState().playerNames).toEqual(
      new Map([[0, "Leonardo"], [1, "Shredder"]]),
    );
    expect(useMultiplayerStore.getState().playerAvatars).toEqual(new Map([
      [0, { kind: "card", cardName: "Leonardo, Sewer Samurai" }],
    ]));
    await waitFor(() => {
      expect(useMultiplayerStore.getState().playerAvatars.get(1)).toEqual({
        kind: "external",
        url: BOSS_ART_URLS[SHREDDER_REF.scryfall_id],
      });
    });
  });
});
