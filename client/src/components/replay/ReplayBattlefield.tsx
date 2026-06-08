import type { GameState, GameObject, PlayerId } from "../../adapter/types.ts";

interface ReplayBattlefieldProps {
  state: GameState;
  playerId: PlayerId;
  label: string;
}

function CardChip({ obj }: { obj: GameObject }) {
  const isCreature = obj.card_types?.core_types?.includes("Creature");
  const hasCounters = Object.keys(obj.counters ?? {}).length > 0;
  const counterText = hasCounters
    ? ` [${Object.entries(obj.counters)
        .map(([k, v]) => `${v}${k.charAt(0)}`)
        .join(",")}]`
    : "";

  return (
    <div
      className={`inline-flex items-center gap-1 px-2 py-0.5 rounded text-xs border ${
        obj.tapped
          ? "border-[#555] bg-[#1a1a1a] text-[#888] italic"
          : "border-[#444] bg-[#222] text-[#ddd]"
      }`}
      title={obj.name}
    >
      {obj.tapped && <span className="text-[#666]">↻</span>}
      <span className="truncate max-w-[120px]">{obj.name}</span>
      {isCreature && obj.power !== null && obj.toughness !== null && (
        <span className="text-[#aaa] shrink-0">
          {obj.power}/{obj.toughness}
        </span>
      )}
      {hasCounters && (
        <span className="text-yellow-400 shrink-0">{counterText}</span>
      )}
    </div>
  );
}

export function ReplayBattlefield({ state, playerId, label }: ReplayBattlefieldProps) {
  const objects = state.battlefield
    .map((id) => state.objects[id])
    .filter((obj) => obj && obj.controller === playerId);

  const creatures = objects.filter((obj) =>
    obj.card_types?.core_types?.includes("Creature"),
  );
  const lands = objects.filter((obj) =>
    obj.card_types?.core_types?.includes("Land"),
  );
  const others = objects.filter(
    (obj) =>
      !obj.card_types?.core_types?.includes("Creature") &&
      !obj.card_types?.core_types?.includes("Land"),
  );

  if (objects.length === 0) {
    return (
      <div className="px-2 py-1 text-xs text-[#555] italic">
        {label}: empty battlefield
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-1 px-2">
      <span className="text-[10px] text-[#666] uppercase tracking-wide">{label}</span>
      {creatures.length > 0 && (
        <div className="flex flex-wrap gap-1">
          {creatures.map((obj) => (
            <CardChip key={obj.id} obj={obj} />
          ))}
        </div>
      )}
      {others.length > 0 && (
        <div className="flex flex-wrap gap-1">
          {others.map((obj) => (
            <CardChip key={obj.id} obj={obj} />
          ))}
        </div>
      )}
      {lands.length > 0 && (
        <div className="flex flex-wrap gap-1">
          {lands.map((obj) => (
            <CardChip key={obj.id} obj={obj} />
          ))}
        </div>
      )}
    </div>
  );
}
