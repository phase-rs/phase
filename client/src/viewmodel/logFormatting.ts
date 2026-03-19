import type { GameEvent } from "../adapter/types";

export type LogVerbosity = "full" | "compact" | "minimal";

export function formatEvent(event: GameEvent): string {
  switch (event.type) {
    case "GameStarted":
      return "Game started";
    case "TurnStarted":
      return `Turn ${event.data.turn_number} -- Player ${event.data.player_id + 1}`;
    case "PhaseChanged":
      return `Phase: ${event.data.phase}`;
    case "PriorityPassed":
      return `Player ${event.data.player_id + 1} passed priority`;
    case "SpellCast":
      return `Spell cast by Player ${event.data.controller + 1}`;
    case "AbilityActivated":
      return `Ability activated (source ${event.data.source_id})`;
    case "ZoneChanged":
      return `Object ${event.data.object_id} moved ${event.data.from} -> ${event.data.to}`;
    case "LifeChanged": {
      const prefix = event.data.amount >= 0 ? "+" : "";
      return `Player ${event.data.player_id + 1} life: ${prefix}${event.data.amount}`;
    }
    case "ManaAdded":
      return `Player ${event.data.player_id + 1} added ${event.data.mana_type} mana`;
    case "PermanentTapped":
      return `Permanent ${event.data.object_id} tapped`;
    case "PermanentUntapped":
      return `Permanent ${event.data.object_id} untapped`;
    case "PlayerLost":
      return `Player ${event.data.player_id + 1} lost the game`;
    case "MulliganStarted":
      return "Mulligan phase";
    case "CardsDrawn":
      return `Player ${event.data.player_id + 1} drew ${event.data.count} card(s)`;
    case "CardDrawn":
      return `Player ${event.data.player_id + 1} drew a card`;
    case "LandPlayed":
      return `Player ${event.data.player_id + 1} played a land`;
    case "StackPushed":
      return `Object ${event.data.object_id} pushed to stack`;
    case "StackResolved":
      return `Stack entry ${event.data.object_id} resolved`;
    case "Discarded":
      return `Player ${event.data.player_id + 1} discarded`;
    case "DamageCleared":
      return `Damage cleared from ${event.data.object_id}`;
    case "GameOver":
      return event.data.winner != null
        ? `Game over -- Player ${event.data.winner + 1} wins!`
        : "Game over -- Draw";
    case "DamageDealt": {
      const target =
        "Player" in event.data.target
          ? `Player ${event.data.target.Player + 1}`
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
    case "Regenerated":
      return `Creature ${event.data.object_id} regenerates`;
    case "PermanentSacrificed":
      return `Player ${event.data.player_id + 1} sacrificed ${event.data.object_id}`;
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
    case "CardsRevealed":
      return `Revealed: ${event.data.card_names.join(", ")}`;
    case "CreatureSuspected":
      return `Creature ${event.data.object_id} suspected`;
    case "CaseSolved":
      return `Case ${event.data.object_id} solved`;
    case "ClassLevelGained":
      return `Class reached level ${event.data.level}`;
    default:
      return `Event: ${(event as GameEvent).type}`;
  }
}

const COMBAT_EVENTS = new Set(["AttackersDeclared", "BlockersDeclared", "DamageDealt"]);
const SPELL_EVENTS = new Set(["SpellCast", "StackPushed", "StackResolved", "SpellCountered"]);
const ZONE_EVENTS = new Set(["ZoneChanged", "Discarded"]);

export function classifyEventColor(event: GameEvent): string {
  if (COMBAT_EVENTS.has(event.type)) return "red";
  if (SPELL_EVENTS.has(event.type)) return "blue";
  if (ZONE_EVENTS.has(event.type)) return "gray";

  if (event.type === "LifeChanged") {
    return event.data.amount >= 0 ? "green" : "red";
  }

  return "gray";
}

const COMPACT_EXCLUDE = new Set([
  "PriorityPassed",
  "ManaAdded",
  "PermanentTapped",
  "PermanentUntapped",
  "DamageCleared",
]);

const MINIMAL_INCLUDE = new Set([
  "GameStarted",
  "TurnStarted",
  "SpellCast",
  "DamageDealt",
  "LifeChanged",
  "GameOver",
  "AttackersDeclared",
  "BlockersDeclared",
  "CreatureDestroyed",
  "TokenCreated",
]);

export function filterByVerbosity(events: GameEvent[], level: LogVerbosity): GameEvent[] {
  switch (level) {
    case "full":
      return events;
    case "compact":
      return events.filter((e) => !COMPACT_EXCLUDE.has(e.type));
    case "minimal":
      return events.filter((e) => MINIMAL_INCLUDE.has(e.type));
  }
}
