import type { GameObject, GameState, PlayerId } from "../adapter/types.ts";

/**
 * Single source of truth for what the commander column (CommanderCardZone +
 * CommanderDamage) renders, and therefore whether PlayerArea should render the
 * column wrapper + its divider at all.
 *
 * These selectors exist because the visibility predicate was previously
 * duplicated across PlayerArea (the wrapper gate), CommanderCardZone, and
 * CommanderDamage — and the copies drifted: an earlier wrapper gate required
 * the Commander-format flag, while CommanderDamage renders on damage alone
 * (using a fallback threshold for non-Commander formats that produced
 * commander damage). The wrapper then suppressed a render path its child
 * supported. Routing all three through these helpers keeps them in lockstep.
 */

/**
 * Commanders this player owns that are currently in the command zone — the
 * exact set CommanderCardZone renders. Used both there and by PlayerArea's
 * column-visibility gate so the wrapper appears iff the card zone will.
 */
export function commandersInZone(gameState: GameState, playerId: PlayerId): GameObject[] {
  return (gameState.command_zone ?? [])
    .map((id) => gameState.objects[id])
    .filter(
      (obj): obj is GameObject =>
        obj != null &&
        obj.is_commander === true &&
        obj.owner === playerId &&
        obj.zone === "Command",
    );
}

/** One attacker's commander-damage badges inflicted on a given victim. */
export interface CommanderDamageEntry {
  /** Attacking commander's controller (PlayerId as string key). */
  attacker: string;
  /** Per-commander damage badges, already filtered to this victim. */
  views: { commander: number; damage: number }[];
}

/**
 * Commander-damage entries inflicted on this player, grouped by attacker — the
 * exact set CommanderDamage renders. Gated on `damage > 0` alone, with no
 * format-flag check: the child uses a fallback threshold so non-Commander
 * formats that somehow produced commander damage still display. Used both
 * there and by PlayerArea's column-visibility gate (`.length > 0`).
 */
export function commanderDamageEntriesFor(
  gameState: GameState,
  playerId: PlayerId,
): CommanderDamageEntry[] {
  const byAttacker = gameState.derived?.commander_damage_by_attacker ?? {};
  const entries: CommanderDamageEntry[] = [];
  for (const [attacker, views] of Object.entries(byAttacker)) {
    const forVictim = views.filter((view) => view.victim === playerId && view.damage > 0);
    if (forVictim.length > 0) entries.push({ attacker, views: forVictim });
  }
  return entries;
}
