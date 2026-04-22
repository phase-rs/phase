import type { ManaCost } from "../../adapter/types.ts";
import { manaCostToShards } from "../../viewmodel/costLabel.ts";
import { ManaSymbol } from "./ManaSymbol.tsx";

interface ManaCostSymbolsProps {
  cost: ManaCost;
  size?: "xs" | "sm" | "md" | "lg";
  className?: string;
  freeClassName?: string;
}

export function ManaCostSymbols({
  cost,
  size = "sm",
  className = "inline-flex items-center gap-0.5",
  freeClassName = "text-slate-500",
}: ManaCostSymbolsProps) {
  if (cost.type === "NoCost" || cost.type === "SelfManaCost") {
    return <span className={freeClassName}>Free</span>;
  }

  const shards = manaCostToShards(cost);
  if (shards.length === 0) shards.push("0");

  return (
    <span className={className}>
      {shards.map((shard, index) => (
        <ManaSymbol key={index} shard={shard} size={size} />
      ))}
    </span>
  );
}
