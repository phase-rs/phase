import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import initEngine, {
  apply_seat_mutation,
  clear_game_state,
  get_game_state,
  initialize_game,
  project_seat_view,
} from "@wasm/engine";
import type { FormatConfig, GameFormat } from "../types";
import type { SeatMutation, SeatMutationResult, SeatState, SeatView } from "../../multiplayer/seatTypes";
import { formatMetadata } from "../../data/formatRegistry";

const currentConfig = (format: GameFormat): FormatConfig => formatMetadata(format)!.default_config;

/** A config an engine before CR 903.12h's exclusion wrote: Brawl carried CR 704.6c's 21. */
const legacyConfig = (format: GameFormat) => ({ ...currentConfig(format), commander_damage_threshold: 21 });

const legacySeatState = (format: unknown) => JSON.stringify({
  seats: [{ type: "HostHuman" }, { type: "WaitingHuman" }],
  tokens: ["host", ""],
  format,
  gameStarted: false,
});

beforeAll(async () => {
  const bytes = await readFile(resolve(__dirname, "../../wasm/engine_wasm_bg.wasm"));
  await initEngine({ module_or_path: await WebAssembly.compile(bytes) });
});

afterEach(() => {
  clear_game_state();
});

describe("legacy FormatConfig through the public WASM exports", () => {
  it.each(["Brawl", "HistoricBrawl"] as const)(
    "initialize_game installs a pre-change %s config under CR 903.12h",
    (format) => {
      const result = initialize_game(null, 42, legacyConfig(format), null, 2, 0) as { error?: boolean; reasons?: string[] };
      expect(result.error, result.reasons?.join("; ")).toBeUndefined();
      const installed = (get_game_state() as { state: { format_config: FormatConfig } } | null)?.state.format_config;
      expect(installed?.format).toBe(format);
      expect(installed?.commander_damage_threshold).toBeNull();
    },
  );

  it("initialize_game keeps a Commander config's threshold", () => {
    const result = initialize_game(null, 42, currentConfig("Commander"), null, 2, 0) as { error?: boolean };
    expect(result.error).toBeUndefined();
    const installed = (get_game_state() as { state: { format_config: FormatConfig } } | null)?.state.format_config;
    expect(installed?.commander_damage_threshold).toBe(21);
  });

  it("project_seat_view accepts a pre-change Brawl seat state and projects the current format", () => {
    const view = project_seat_view(legacySeatState(legacyConfig("Brawl"))) as SeatView;
    expect(view.seats).toEqual([{ type: "HostHuman" }, { type: "WaitingHuman" }]);
    expect(view.format).toEqual(currentConfig("Brawl"));
  });

  it("apply_seat_mutation accepts a pre-change Brawl seat state and seats an AI", () => {
    const mutation: SeatMutation = {
      type: "SetKind",
      data: {
        seatIndex: 1,
        kind: {
          type: "Ai",
          data: {
            difficulty: "Medium",
            deck: { type: "DeckList", data: { main_deck: ["Forest"], sideboard: [], commander: [] } },
          },
        },
        expectedKind: { type: "WaitingHuman" },
      },
    };
    const result = apply_seat_mutation(
      legacySeatState(legacyConfig("Brawl")),
      JSON.stringify(mutation),
    ) as SeatMutationResult;
    const state: SeatState = result.state;
    expect(state.seats[0]).toEqual({ type: "HostHuman" });
    expect(state.tokens[0]).toBe("host");
    expect(state.seats[1].type).toBe("Ai");
    expect(result.delta.newAi).toHaveLength(1);
    expect(state.format).toEqual(currentConfig("Brawl"));
  });

  it("project_seat_view still refuses a threshold no engine wrote", () => {
    expect(() => project_seat_view(legacySeatState({ ...currentConfig("Brawl"), commander_damage_threshold: 0 })))
      .toThrow("Invalid SeatState");
  });
});
