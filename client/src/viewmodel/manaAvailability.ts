import type { GameObject, ManaColor, ManaPip } from "../adapter/types.ts";
import { SHARD_ABBREVIATION } from "./costLabel.ts";

// Renders a player's land mana as a "per-source pip rail": one tight pip-group
// per source, never per-color totals. This preserves the engine's flexible-
// source modeling — a dual that taps for "{W} or {U}" is ONE pip
// (`ManaPip::OneOfColors`), not one white + one blue — so the rail can't
// overcount available mana (the matching problem stays in the engine).
//
// Tapped vs untapped is the user-facing signal: `available_mana_pips` is the
// engine's color projection (NOT tapped-aware — see `display_land_mana_pips`),
// so the `tapped` flag drives whether a source renders bright or dimmed.

/** One source rendered as a single tight pip-group in the rail. */
export interface LandManaSource {
  /**
   * Scryfall shard strings for `ManaSymbol` (e.g. `"W"`, `"C"`, `"W/U"`).
   * A source producing two colors simultaneously contributes two shards
   * (still one group); a two-color *choice* contributes one hybrid shard.
   * Empty when the source's output can't map to mana symbols — see `rainbow`
   * and the neutral fallback in the renderer.
   */
  shards: string[];
  /**
   * True when the source produces flexible mana with no single Scryfall
   * symbol (3+ color choice, any-combination, or commander-identity); the
   * rail shows a rainbow swatch instead of a lettered pip.
   */
  rainbow: boolean;
  /** CR 106.1: a tapped source can't produce mana now → rendered dimmed. */
  tapped: boolean;
}

export interface LandManaAvailability {
  /** One entry per land, in battlefield order. */
  sources: LandManaSource[];
  /** Total lands (matches the prior bare count). */
  total: number;
  /** Lands that are currently untapped (their mana is available now). */
  untapped: number;
}

/** Map one `ManaPip` to render shards, flagging flexible output as rainbow. */
function pipToShards(pip: ManaPip): { shards: string[]; rainbow: boolean } {
  switch (pip.type) {
    case "Color":
      return { shards: [colorShard(pip.data)], rainbow: false };
    case "Colorless":
      return { shards: ["C"], rainbow: false };
    case "OneOfColors":
      // CR 106.4: a two-color choice has a Scryfall hybrid symbol ("W/U");
      // a 3+-color choice does not, so fall back to a rainbow swatch.
      return pip.data.length === 2
        ? { shards: [pip.data.map(colorShard).join("/")], rainbow: false }
        : { shards: [], rainbow: true };
    case "CombinationOfColors":
    case "AnyInCommandersIdentity":
      return { shards: [], rainbow: true };
  }
}

function colorShard(color: ManaColor): string {
  // SHARD_ABBREVIATION is keyed by shard-variant name; the five single-color
  // names overlap ManaColor exactly ("White" → "W", …).
  return SHARD_ABBREVIATION[color] ?? "C";
}

/**
 * Build the per-source rail descriptor for a player's lands. `lands` should be
 * the player's battlefield lands (e.g. `partitionByType(...).lands`); each
 * carries the engine-derived `available_mana_pips` + `tapped` already on the
 * wire, so this is pure presentation aggregation — no game logic.
 */
export function landManaAvailability(lands: GameObject[]): LandManaAvailability {
  const sources: LandManaSource[] = [];
  let untapped = 0;

  for (const land of lands) {
    const tapped = land.tapped ?? false;
    if (!tapped) untapped += 1;

    const shards: string[] = [];
    let rainbow = false;
    for (const pip of land.available_mana_pips ?? []) {
      const mapped = pipToShards(pip);
      shards.push(...mapped.shards);
      rainbow = rainbow || mapped.rainbow;
    }
    // A land with no pip projection (e.g. Maze of Ith) makes no mana — it is
    // neither lettered nor rainbow; the renderer shows a neutral dot so the
    // land count stays honest without implying it taps for mana.
    sources.push({ shards, rainbow, tapped });
  }

  return { sources, total: lands.length, untapped };
}
