import { StrictMode } from "react";
import type { ManaColor } from "../../../adapter/types.ts";
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useGameStore } from "../../../stores/gameStore";
import { usePreferencesStore } from "../../../stores/preferencesStore";
import type { BoardBackground } from "../../../stores/preferencesStore";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory";
import { buildGameState, buildPlayer } from "../../../test/factories/gameStateFactory";
import { BattlefieldBackground, resolveBackground } from "../BattlefieldBackground";

const scene = vi.hoisted(() => ({ create: vi.fn(() => vi.fn()) }));
vi.mock("../arenaBackgroundScene.ts", () => ({ createArenaBackgroundScene: scene.create }));

const initialPreferences = usePreferencesStore.getInitialState();

describe("resolveBackground", () => {
  afterEach(() => vi.restoreAllMocks());

  it.each([
    ["air_angelic_sky", "White"],
    ["water_moonlit_ocean_temple", "Blue"],
    ["shadow_moon_coven_sanctum", "Black"],
    ["fire_molten", "Red"],
    ["earth_snowy_forest", "Green"],
  ])("resolves %s to its animated arena", (id, color) => {
    expect(resolveBackground(id, "", undefined, { current: null })).toEqual({ kind: "arena", color });
  });

  it("selects a random playmat for colorless decks in auto-wubrg mode", () => {
    vi.spyOn(Math, "random").mockReturnValue(0);
    const lock = { current: null };

    const background = resolveBackground("auto-wubrg" as BoardBackground, "", null, lock);

    expect(background).toEqual({ kind: "arena", color: "White" });
  });

  it("waits for deck data before locking a colored playmat", () => {
    vi.spyOn(Math, "random").mockReturnValue(0);
    const lock = { current: null };

    expect(resolveBackground("auto-wubrg" as BoardBackground, "", undefined, lock)).toBeNull();

    expect(resolveBackground("auto-wubrg" as BoardBackground, "", "Blue", lock)).toEqual({
      kind: "arena",
      color: "Blue",
    });
  });

  it("keeps the locked playmat when deck data is withheld on later renders", () => {
    // Once the lock is set, the component stops re-running the deck scan
    // (memo guard) and passes `undefined` on every later render. Dropping the
    // lock here renders a transparent layer — black board — on the next
    // render after any gameState change, the exact regression the component
    // test below reproduces.
    const lock = { current: "Blue" as ManaColor };

    expect(resolveBackground("auto-wubrg" as BoardBackground, "", undefined, lock)).toEqual({
      kind: "arena",
      color: "Blue",
    });
  });
});

describe("BattlefieldBackground", () => {
  afterEach(() => {
    cleanup();
    useGameStore.setState({ gameMode: null, gameState: null });
    usePreferencesStore.setState(initialPreferences);
    vi.clearAllMocks();
    vi.restoreAllMocks();
  });

  it("honors the VFX quality preference and keeps static art at minimal quality", async () => {
    usePreferencesStore.setState({ boardBackground: "fire_molten", vfxQuality: "full" });
    const { container } = render(<BattlefieldBackground />);
    await waitFor(() => expect(scene.create).toHaveBeenCalledTimes(1));
    expect(scene.create.mock.calls[0].slice(1)).toEqual(["Red", "full", 1, 1]);

    act(() => usePreferencesStore.getState().setVfxQuality("reduced"));
    await waitFor(() => expect(scene.create).toHaveBeenCalledTimes(2));
    expect(scene.create.mock.calls[1].slice(1)).toEqual(["Red", "reduced", 1, 1]);

    act(() => usePreferencesStore.getState().setVfxQuality("minimal"));
    expect(scene.create).toHaveBeenCalledTimes(2);
    expect(container.querySelector("img")?.getAttribute("src")).toContain("molten-topdown.webp");
  });

  it("keeps the locked playmat across later game-state renders under StrictMode", () => {
    vi.spyOn(Math, "random").mockReturnValue(0);
    const library = buildGameObject({
      id: 1,
      owner: 0,
      card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] },
      mana_cost: { type: "Cost", shards: ["Blue"], generic: 0 },
    });
    useGameStore.setState({
      gameMode: "ai",
      gameState: buildGameState({
        players: [buildPlayer({ id: 0, library: [1] })],
        objects: buildObjectMap(library),
        battlefield: [],
      }),
    });
    usePreferencesStore.setState({ boardBackground: "auto-wubrg", customBackgroundUrl: "" });

    const { container } = render(
      <StrictMode>
        <BattlefieldBackground />
      </StrictMode>,
    );

    expect(container.querySelector("img")?.getAttribute("src")).toContain(
      "/battlefield/arenas/ocean-temple-topdown.webp",
    );

    // A later game-state change (any action: tap, phase tick) re-runs the
    // deck scan, which short-circuits to undefined once the lock exists. The
    // locked playmat must survive that render — regression: the background
    // dropped to a transparent layer (black board) after the first action.
    act(() => {
      useGameStore.setState({
        gameState: buildGameState({
          players: [buildPlayer({ id: 0, library: [1], turns_taken: 1 })],
          objects: buildObjectMap(library),
          battlefield: [],
        }),
      });
    });

    expect(container.querySelector("img")?.getAttribute("src")).toContain(
      "/battlefield/arenas/ocean-temple-topdown.webp",
    );
  });
});
