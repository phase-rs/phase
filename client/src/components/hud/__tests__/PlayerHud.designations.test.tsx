import { act } from "react";
import { cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { GameState } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { buildGameState, buildPlayers } from "../../../test/factories/gameStateFactory.ts";
import { PlayerHud } from "../PlayerHud.tsx";

describe("PlayerHud designations", () => {
  beforeEach(() => {
    useMultiplayerStore.setState({ activePlayerId: 0 });
    useGameStore.setState({ gameState: buildGameState() });
  });

  afterEach(() => {
    cleanup();
  });

  describe("Monarch", () => {
    it("renders the crown when the local player is the monarch", () => {
      act(() => {
        useGameStore.setState({ gameState: buildGameState({ monarch: 0 }) });
      });
      render(<PlayerHud />);
      expect(screen.getByLabelText("Monarch")).toBeInTheDocument();
    });

    it("does not render the crown when an opponent is the monarch", () => {
      act(() => {
        useGameStore.setState({ gameState: buildGameState({ monarch: 1 }) });
      });
      render(<PlayerHud />);
      expect(screen.queryByLabelText("Monarch")).toBeNull();
    });

    it("does not render the crown when no one is the monarch", () => {
      render(<PlayerHud />);
      expect(screen.queryByLabelText("Monarch")).toBeNull();
    });
  });

  describe("Initiative", () => {
    it("renders when the local player has the initiative", () => {
      act(() => {
        useGameStore.setState({ gameState: buildGameState({ initiative: 0 }) });
      });
      render(<PlayerHud />);
      expect(screen.getByLabelText("Initiative")).toBeInTheDocument();
    });

    it("does not render when an opponent has the initiative", () => {
      act(() => {
        useGameStore.setState({ gameState: buildGameState({ initiative: 1 }) });
      });
      render(<PlayerHud />);
      expect(screen.queryByLabelText("Initiative")).toBeNull();
    });

    it("does not render when no one has the initiative", () => {
      render(<PlayerHud />);
      expect(screen.queryByLabelText("Initiative")).toBeNull();
    });
  });

  describe("City's Blessing", () => {
    it("renders when the local player has the blessing", () => {
      act(() => {
        useGameStore.setState({ gameState: buildGameState({ city_blessing: [0] }) });
      });
      render(<PlayerHud />);
      expect(screen.getByLabelText("City's Blessing")).toBeInTheDocument();
    });

    it("does not render when only an opponent has the blessing", () => {
      act(() => {
        useGameStore.setState({ gameState: buildGameState({ city_blessing: [1] }) });
      });
      render(<PlayerHud />);
      expect(screen.queryByLabelText("City's Blessing")).toBeNull();
    });

    it("does not render when no one has the blessing", () => {
      render(<PlayerHud />);
      expect(screen.queryByLabelText("City's Blessing")).toBeNull();
    });
  });

  describe("Ring level", () => {
    it("renders the ring counter at level 3 for the local player", () => {
      act(() => {
        useGameStore.setState({ gameState: buildGameState({ ring_level: { "0": 3 } }) });
      });
      render(<PlayerHud />);
      expect(screen.getByLabelText(/the ring tempts you \(level 3\)/i)).toBeInTheDocument();
    });

    it("does not render at level 0", () => {
      act(() => {
        useGameStore.setState({ gameState: buildGameState({ ring_level: { "0": 0 } }) });
      });
      render(<PlayerHud />);
      expect(screen.queryByLabelText(/the ring tempts you/i)).toBeNull();
    });

    it("does not render when only an opponent is tempted", () => {
      act(() => {
        useGameStore.setState({ gameState: buildGameState({ ring_level: { "1": 2 } }) });
      });
      render(<PlayerHud />);
      expect(screen.queryByLabelText(/the ring tempts you/i)).toBeNull();
    });
  });

  describe("Energy", () => {
    it("renders the energy counter when the local player has energy", () => {
      const gameState = buildGameState();
      gameState.players[0].energy = 5;
      act(() => {
        useGameStore.setState({ gameState });
      });
      render(<PlayerHud />);
      expect(screen.getByLabelText("5 energy counters")).toBeInTheDocument();
    });

    it("uses singular form for one energy", () => {
      const gameState = buildGameState();
      gameState.players[0].energy = 1;
      act(() => {
        useGameStore.setState({ gameState });
      });
      render(<PlayerHud />);
      expect(screen.getByLabelText("1 energy counter")).toBeInTheDocument();
    });

    it("does not render at zero energy", () => {
      render(<PlayerHud />);
      expect(screen.queryByLabelText(/energy counter/)).toBeNull();
    });
  });

  describe("Dungeon", () => {
    it("renders the dungeon badge when the local player is venturing", () => {
      act(() => {
        useGameStore.setState({
          gameState: buildGameState({
            dungeon_progress: {
              "0": { current_dungeon: "LostMineOfPhandelver", current_room: 1, completed: [] },
            },
          }),
        });
      });
      render(<PlayerHud />);
      expect(screen.getByLabelText("Venturing in Lost Mine, room 2")).toBeInTheDocument();
    });

    it("does not render when the player has progress but no active dungeon", () => {
      act(() => {
        useGameStore.setState({
          gameState: buildGameState({
            dungeon_progress: {
              "0": { current_dungeon: null, current_room: 0, completed: ["TombOfAnnihilation"] },
            },
          }),
        });
      });
      render(<PlayerHud />);
      expect(screen.queryByLabelText(/venturing in/i)).toBeNull();
    });

    it("does not render when only an opponent is venturing", () => {
      act(() => {
        useGameStore.setState({
          gameState: buildGameState({
            dungeon_progress: {
              "1": { current_dungeon: "Undercity", current_room: 0, completed: [] },
            },
          }),
        });
      });
      render(<PlayerHud />);
      expect(screen.queryByLabelText(/venturing in/i)).toBeNull();
    });
  });

  // CR 732.2a: the `∞` HUD badge is driven ONLY by the engine projection
  // `derived.unbounded_families` — the FE derives neither which axes are unbounded, nor the
  // family they group into, nor whether a collapse is coming.
  describe("Unbounded resources (∞)", () => {
    it("renders an ∞ badge for the local player's engine-attributed family", () => {
      act(() => {
        useGameStore.setState({
          gameState: buildGameState({
            derived: {
              unbounded_families: [
                { player: 0, family: "tokens", state: { type: "Unscheduled" } },
              ],
            },
          }),
        });
      });
      render(<PlayerHud />);
      // REVERT-PROBE: stop reading `derived.unbounded_families` (or remove the
      // PlayerHud map) → the badge is absent → this assertion fails.
      expect(screen.getByLabelText("Unbounded tokens (∞)")).toBeInTheDocument();
    });

    it("does not render when there are no unbounded resources", () => {
      act(() => {
        useGameStore.setState({
          gameState: buildGameState({ derived: { unbounded_families: [] } }),
        });
      });
      render(<PlayerHud />);
      expect(screen.queryByLabelText(/Unbounded/)).toBeNull();
    });

    it("does not render when only an opponent has an unbounded family", () => {
      act(() => {
        useGameStore.setState({
          gameState: buildGameState({
            derived: {
              unbounded_families: [
                { player: 1, family: "tokens", state: { type: "Unscheduled" } },
              ],
            },
          }),
        });
      });
      render(<PlayerHud />);
      expect(screen.queryByLabelText(/Unbounded/)).toBeNull();
    });

    // The "two mana axes collapse to one badge" case MOVED TO THE ENGINE as
    // `derived_views::tests::two_mana_axes_fold_to_one_family_row`; migrating it IS the evidence
    // that the fold left the display layer. What remains here is the render-level consequence:
    // the engine hands down one row per family, so the HUD renders one badge per row.
  });

  // The mana-pool `∞` marker is a SECOND ∞ surface — rendered by `ManaPoolSummary` beside the pool
  // pills, not in the badge strip. It used to answer "is mana unbounded?" by running the client's
  // `familyOf` mirror over `derived.unbounded_resources`: a family derivation in the display layer,
  // which is exactly what `ResourceAxis`'s own doc claimed the frontend never does.
  //
  // These two cases DELIBERATELY put the channels in conflict. On the wire they never disagree
  // (`derive_views` emits both from one loop over `unbounded_resources`), so an AGREEING fixture
  // cannot tell which channel is the authority — it goes green either way. Disagreement is the only
  // shape that discriminates, and it discriminates in both directions.
  describe("Unbounded mana pool marker (∞)", () => {
    const withPool = (derived: GameState["derived"]) =>
      buildGameState({
        players: buildPlayers([
          {
            id: 0,
            mana_pool: {
              mana: [{ color: "Blue", source_id: 1, pip_id: 1, snow: false, restrictions: [] }],
            },
          },
          { id: 1 },
        ]),
        derived,
      });

    // The badge-strip badge for the `mana` family carries the SAME aria-label as this marker, so
    // an unscoped query matches both (measured: `getByLabelText` found two elements). Every
    // assertion below is scoped to the pool row, and the throw is the reach-guard — a null query
    // inside a row that never rendered would be vacuous, and `ManaPoolSummary` returns `null`
    // outright on an empty pool.
    const manaPoolRow = (): HTMLElement => {
      const row = document.querySelector<HTMLElement>("[data-mana-pool-summary]");
      if (!row) throw new Error("the mana pool row did not render; the assertion would be vacuous");
      return row;
    };

    it("follows the engine's family channel, not the axis list", () => {
      act(() => {
        // Engine says: no mana family unbounded. The axis list says the opposite.
        useGameStore.setState({
          gameState: withPool({
            unbounded_resources: [{ player: 0, axis: { Mana: "Blue" } }],
            unbounded_families: [],
          }),
        });
      });
      render(<PlayerHud />);
      // REVERT-PROBE (negative direction): restore
      // `unboundedResources.some((u) => familyOf(u.axis) === "mana")` ⇒ the marker appears ⇒ this
      // fails. NOT vacuous: the sibling below renders the marker from this same pool, so absence
      // here is the channel choice, not an unreachable render.
      expect(within(manaPoolRow()).queryByLabelText("Unbounded mana (∞)")).toBeNull();
    });

    it("renders from the family channel alone, with no unbounded axis row", () => {
      act(() => {
        // Engine says: mana family unbounded. The axis list is empty.
        useGameStore.setState({
          gameState: withPool({
            unbounded_resources: [],
            unbounded_families: [{ player: 0, family: "mana", state: { type: "Unscheduled" } }],
          }),
        });
      });
      render(<PlayerHud />);
      // REVERT-PROBE (positive direction): the old `familyOf` derivation over an EMPTY
      // `unbounded_resources` yields `false` ⇒ the marker is absent ⇒ this fails.
      expect(within(manaPoolRow()).getByLabelText("Unbounded mana (∞)")).toBeInTheDocument();
    });
  });
});
