import { useEffect, useRef } from "react";

import type { GameEvent } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getPlayerDisplayName } from "../../stores/multiplayerStore.ts";

function formatEvent(event: GameEvent): string {
  switch (event.type) {
    case "GameStarted":
      return "Game started";
    case "TurnStarted":
      return `Turn ${event.data.turn_number} -- ${getPlayerDisplayName(event.data.player_id)}`;
    case "PhaseChanged":
      return `Phase: ${event.data.phase}`;
    case "PriorityPassed":
      return `${getPlayerDisplayName(event.data.player_id)} passed priority`;
    case "SpellCast":
      return `Spell cast by ${getPlayerDisplayName(event.data.controller)}`;
    case "AbilityActivated":
      return `Ability activated (source ${event.data.source_id})`;
    case "ZoneChanged":
      // `from` is null for token creation (CR 111.1 + CR 603.6a — tokens are
      // created in the battlefield zone with no prior zone).
      return event.data.from
        ? `Object ${event.data.object_id} moved ${event.data.from} -> ${event.data.to}`
        : `Object ${event.data.object_id} enters ${event.data.to}`;
    case "LifeChanged": {
      const prefix = event.data.amount >= 0 ? "+" : "";
      return `${getPlayerDisplayName(event.data.player_id)} life: ${prefix}${event.data.amount}`;
    }
    case "ManaAdded":
      return `${getPlayerDisplayName(event.data.player_id)} added ${event.data.mana_type} mana`;
    case "PermanentTapped":
      return `Permanent ${event.data.object_id} tapped`;
    case "PermanentUntapped":
      return `Permanent ${event.data.object_id} untapped`;
    case "PlayerLost":
      return `${getPlayerDisplayName(event.data.player_id)} lost the game`;
    case "MulliganStarted":
      return "Mulligan phase";
    case "CardsDrawn":
      return `${getPlayerDisplayName(event.data.player_id)} drew ${event.data.count} card(s)`;
    case "CardDrawn":
      return `${getPlayerDisplayName(event.data.player_id)} drew a card`;
    case "LandPlayed":
      return `${getPlayerDisplayName(event.data.player_id)} played a land`;
    case "StackPushed":
      return `Object ${event.data.object_id} pushed to stack`;
    case "StackResolved":
      return `Stack entry ${event.data.object_id} resolved`;
    case "Discarded":
      return `${getPlayerDisplayName(event.data.player_id)} discarded`;
    case "DamageCleared":
      return `Damage cleared from ${event.data.object_id}`;
    case "GameOver":
      return event.data.winner != null
        ? `Game over -- ${getPlayerDisplayName(event.data.winner)} wins!`
        : "Game over -- Draw";
    case "DamageDealt": {
      const target =
        "Player" in event.data.target
          ? getPlayerDisplayName(event.data.target.Player)
          : `object ${event.data.target.Object}`;
      return `Source ${event.data.source_id} deals ${event.data.amount} damage to ${target}`;
    }
    case "SpellCountered":
      return `Object ${event.data.object_id} countered by ${event.data.countered_by}`;
    case "CounterAdded":
      return `${event.data.counter_type} x${event.data.count} added to ${event.data.object_id}`;
    case "CounterRemoved":
      return `${event.data.counter_type} x${event.data.count} removed from ${event.data.object_id}`;
    case "TokenCreated":
      return `Token "${event.data.name}" created`;
    case "CreatureDestroyed":
      return `Creature ${event.data.object_id} destroyed`;
    case "PermanentSacrificed":
      return `${getPlayerDisplayName(event.data.player_id)} sacrificed ${event.data.object_id}`;
    case "EffectResolved":
      return `Effect ${event.data.kind} resolved`;
    case "AttackersDeclared":
      return `${event.data.attacker_ids.length} attacker(s) declared`;
    case "BlockersDeclared":
      return `${event.data.assignments.length} blocker(s) assigned`;
    case "BecomesTarget":
      return `Object ${event.data.object_id} targeted by ${event.data.source_id}`;
    case "ReplacementApplied":
      return `Replacement applied: ${event.data.event_type}`;
    case "CompanionRevealed":
      return `${getPlayerDisplayName(event.data.player)} revealed companion: ${event.data.card_name}`;
    case "CompanionMovedToHand":
      return `${getPlayerDisplayName(event.data.player)} put companion ${event.data.card_name} into hand`;
    case "PowerToughnessChanged": {
      const d = event.data;
      const sign = (n: number) => (n >= 0 ? `+${n}` : `${n}`);
      return `Object ${d.object_id} is now ${d.power}/${d.toughness} (${sign(d.power_delta)}/${sign(d.toughness_delta)})`;
    }
    default:
      return `Event: ${(event as GameEvent).type}`;
  }
}

export function GameLog() {
  const events = useGameStore((s) => s.events);
  const scrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = scrollRef.current;
    if (el) {
      el.scrollTop = el.scrollHeight;
    }
  }, [events]);

  return (
    <div className="flex flex-1 flex-col gap-1 overflow-hidden">
      <h3 className="text-xs font-semibold uppercase tracking-wider text-gray-400">
        Game Log
      </h3>
      <div
        ref={scrollRef}
        className="flex-1 overflow-y-auto rounded bg-gray-900 p-1.5 font-mono text-[10px] leading-relaxed text-gray-300"
      >
        {events.length === 0 ? (
          <p className="italic text-gray-600">No events yet</p>
        ) : (
          events.map((event, i) => (
            <div key={i} className="border-b border-gray-800 py-0.5">
              {formatEvent(event)}
            </div>
          ))
        )}
      </div>
    </div>
  );
}
