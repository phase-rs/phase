import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { describe, expect, it } from "vitest";

import type { WaitingFor } from "../types";
import { HANDLED_WAITING_FOR_TYPES } from "../../game/waitingForRegistry";
import { repoRoot, rustEnumVariants } from "./rustEnumVariants";

/**
 * Engine `WaitingFor` variants that are never surfaced to a human and so
 * legitimately have no frontend UI handler.
 *
 * `ResolveAllReady` is deliberately inert: `WaitingFor::acting_player()` in
 * `crates/engine/src/types/game_state.rs` returns `None` for it, and restore
 * automation repairs legacy saved frames before the client receives a
 * snapshot. `GameOver` likewise returns `None` but remains in
 * `HANDLED_WAITING_FOR_TYPES` because it has a rendered terminal screen.
 *
 * If a future variant is genuinely never player-facing, add it here WITH a
 * cited `acting_player()` reference proving it returns `None`.
 */
const INTERNAL_NEVER_PLAYER_FACING: ReadonlySet<WaitingFor["type"]> =
  new Set<WaitingFor["type"]>(["ResolveAllReady"]);

describe("WaitingFor handler parity", () => {
  it("registers both interactive meld waiting states", () => {
    expect(HANDLED_WAITING_FOR_TYPES.has("MeldPairChoice")).toBe(true);
    expect(HANDLED_WAITING_FOR_TYPES.has("MeldAttackTargetChoice")).toBe(true);
    expect(HANDLED_WAITING_FOR_TYPES.has("EntryAttackTargetChoice")).toBe(true);
  });

  it("every engine WaitingFor variant has a frontend UI handler", () => {
    const rustSource = readFileSync(
      resolve(repoRoot(), "crates/engine/src/types/game_state.rs"),
      "utf8",
    );
    const engineVariants = rustEnumVariants(rustSource, "WaitingFor");

    const unhandled = engineVariants.filter(
      (variant) =>
        !HANDLED_WAITING_FOR_TYPES.has(variant as WaitingFor["type"]) &&
        !INTERNAL_NEVER_PLAYER_FACING.has(variant as WaitingFor["type"]),
    );

    expect(
      unhandled,
      `Engine WaitingFor variant(s) [${unhandled.join(", ")}] have no frontend UI handler. ` +
        "Add a handler to HANDLED_WAITING_FOR_TYPES (client/src/game/waitingForRegistry.ts) " +
        "and wire a corresponding modal/overlay in GamePage. Only if the variant is truly " +
        "internal and never surfaced to a player, add it to INTERNAL_NEVER_PLAYER_FACING with " +
        "a cited acting_player() reference proving it returns None.",
    ).toEqual([]);
  });

  it("has no stale INTERNAL_NEVER_PLAYER_FACING allowlist entries", () => {
    const rustSource = readFileSync(
      resolve(repoRoot(), "crates/engine/src/types/game_state.rs"),
      "utf8",
    );
    const engineVariants = new Set(rustEnumVariants(rustSource, "WaitingFor"));

    const stale = [...INTERNAL_NEVER_PLAYER_FACING].filter(
      (variant) => !engineVariants.has(variant),
    );

    expect(
      stale,
      `INTERNAL_NEVER_PLAYER_FACING contains entries [${stale.join(", ")}] that no longer ` +
        "exist on the engine WaitingFor enum. Remove the stale allowlist entries.",
    ).toEqual([]);
  });
});
