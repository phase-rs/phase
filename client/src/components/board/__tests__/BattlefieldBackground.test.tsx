import { StrictMode } from "react";
import { act, cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useGameStore } from "../../../stores/gameStore";
import { usePreferencesStore } from "../../../stores/preferencesStore";
import type { BoardBackground } from "../../../stores/preferencesStore";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory";
import { buildGameState, buildPlayer } from "../../../test/factories/gameStateFactory";
import { BattlefieldBackground, resolveBackground } from "../BattlefieldBackground";

describe("resolveBackground", () => {
  afterEach(() => vi.restoreAllMocks());

  it("selects a random playmat for colorless decks in auto-wubrg mode", () => {
    vi.spyOn(Math, "random").mockReturnValue(0);
    const lock = { current: null };

    const background = resolveBackground("auto-wubrg" as BoardBackground, "", null, lock);

    expect(background).toEqual({ kind: "image", src: "/battlefield/air_angelic_sky.webp" });
  });

  it("waits for deck data before locking a colored playmat", () => {
    vi.spyOn(Math, "random").mockReturnValue(0);
    const lock = { current: null };

    expect(resolveBackground("auto-wubrg" as BoardBackground, "", undefined, lock)).toBeNull();

    expect(resolveBackground("auto-wubrg" as BoardBackground, "", "Blue", lock)).toEqual({
      kind: "image",
      src: "/battlefield/water_moonlit_ocean_temple.webp",
    });
  });

  it("keeps the locked playmat when deck data is withheld on later renders", () => {
    // Once the lock is set, the component stops re-running the deck scan
    // (memo guard) and passes `undefined` on every later render. Dropping the
    // lock here renders a transparent layer — black board — on the next
    // render after any gameState change, the exact regression the component
    // test below reproduces.
    const lock = { current: "/battlefield/water_moonlit_ocean_temple.webp" };

    expect(resolveBackground("auto-wubrg" as BoardBackground, "", undefined, lock)).toEqual({
      kind: "image",
      src: "/battlefield/water_moonlit_ocean_temple.webp",
    });
  });
});

describe("BattlefieldBackground", () => {
  afterEach(() => {
    cleanup();
    useGameStore.setState({ gameMode: null, gameState: null });
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

    const layer = container.firstChild as HTMLElement;
    expect(layer.style.backgroundImage).toContain(
      "/battlefield/water_moonlit_ocean_temple.webp",
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

    expect(layer.style.backgroundImage).toContain(
      "/battlefield/water_moonlit_ocean_temple.webp",
    );
  });
});