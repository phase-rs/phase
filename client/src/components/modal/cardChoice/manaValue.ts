import type { ManaCost } from "../../../adapter/types.ts";

// Pure presentation derivation (CMC for sort/group). Mirrors the engine's mana
// value rule (CR 202.3): generic + one per colored/hybrid pip, hybrid "two or a
// color" pips count as 2, {X} counts as 0 outside the stack. This is display-only
// ordering, never game logic.
export function manaValueOfShard(shard: string): number {
  switch (shard) {
    case "TwoWhite":
    case "TwoBlue":
    case "TwoBlack":
    case "TwoRed":
    case "TwoGreen":
      return 2;
    case "X":
      return 0;
    default:
      return 1;
  }
}

export function manaValueOfCost(cost: ManaCost): number {
  switch (cost.type) {
    case "NoCost":
    case "SelfManaCost":
    case "SelfManaValue":
      return 0;
    case "Cost":
      return (
        cost.generic +
        cost.shards.reduce((sum, shard) => sum + manaValueOfShard(shard), 0)
      );
  }
}

export function manaValueOfObject(obj: { mana_cost: ManaCost }): number {
  return manaValueOfCost(obj.mana_cost);
}
