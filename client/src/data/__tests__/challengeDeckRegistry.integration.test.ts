import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { describe, it, expect, beforeAll } from "vitest";

import init, { getChallengeDeckRegistry } from "@wasm/engine";
import { CHALLENGE_DECK_REGISTRY, withChallengeDeck } from "../challengeDeckRegistry";
import { formatMetadata } from "../formatRegistry";
import type { ChallengeDeckMetadata } from "../../adapter/types";

/**
 * Drift-detection test: the TS `CHALLENGE_DECK_REGISTRY` is a hand-authored
 * mirror of the Rust `ChallengeDeck::registry()`. This test loads the real WASM
 * binary, calls the engine's `getChallengeDeckRegistry` export, and verifies the
 * shapes match exactly. If this fails, either the TS mirror or the Rust registry
 * was updated without the other — which would make a newly-added Horde deck
 * unselectable, or offer a deck the engine can't load.
 *
 * Requires: ./scripts/build-wasm.sh to have been run.
 */

async function initWasm() {
  const wasmPath = resolve(__dirname, "../../wasm/engine_wasm_bg.wasm");
  const bytes = await readFile(wasmPath);
  const module = await WebAssembly.compile(bytes);
  await init({ module_or_path: module });
}

describe("CHALLENGE_DECK_REGISTRY (engine drift check)", () => {
  beforeAll(async () => {
    await initWasm();
  });

  it("TS mirror matches the Rust registry exactly", () => {
    const fromEngine = getChallengeDeckRegistry() as ChallengeDeckMetadata[];

    // Same length: catches a deck added on one side only.
    expect(fromEngine.length).toBe(CHALLENGE_DECK_REGISTRY.length);

    // Same order: the deck picker iterates this list, so reordering shuffles UI.
    for (let i = 0; i < CHALLENGE_DECK_REGISTRY.length; i++) {
      expect(fromEngine[i]).toEqual(CHALLENGE_DECK_REGISTRY[i]);
    }
  });

  it("every deck's bundled ruleset describes that deck", () => {
    // Guards the picker's core assumption: applying an entry yields a config for
    // the deck the user actually clicked.
    for (const meta of CHALLENGE_DECK_REGISTRY) {
      expect(meta.default_ruleset.challenge_deck).toBe(meta.deck);
    }
  });

  it("applying a deck swaps in that deck's whole ruleset", () => {
    const hordeConfig = formatMetadata("Horde")?.default_config;
    expect(hordeConfig).toBeDefined();

    const dnd = withChallengeDeck(hordeConfig!, "DndHorde");
    expect(dnd.horde_ruleset?.challenge_deck).toBe("DndHorde");
    // The wave rule must travel with the deck — picking a deck must not leave
    // the previous deck's wave policy attached.
    expect(dnd.horde_ruleset?.wave).toEqual({
      type: "UntilRarityAtLeast",
      data: "uncommon",
    });

    const cyber = withChallengeDeck(dnd, "CybermanHorde");
    expect(cyber.horde_ruleset?.challenge_deck).toBe("CybermanHorde");
    expect(cyber.horde_ruleset?.wave).toEqual({
      type: "UntilNonToken",
      data: { count: { type: "Fixed", data: 1 } },
    });

    // The Zombies deck carries the snaking count that defines its difficulty
    // ramp — the whole point of parameterizing the nontoken count.
    const zombies = withChallengeDeck(cyber, "ZombiesHorde");
    expect(zombies.horde_ruleset?.challenge_deck).toBe("ZombiesHorde");
    expect(zombies.horde_ruleset?.wave).toEqual({
      type: "UntilNonToken",
      data: { count: { type: "Snaking", data: { min: 1, max: 3 } } },
    });
  });

  it("leaves non-Horde formats untouched", () => {
    const standard = formatMetadata("Standard")?.default_config;
    expect(standard).toBeDefined();
    expect(withChallengeDeck(standard!, "DndHorde")).toBe(standard);
  });
});
