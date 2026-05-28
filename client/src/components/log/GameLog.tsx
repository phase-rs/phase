import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";

import type { GameEvent } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getPlayerDisplayName } from "../../stores/multiplayerStore.ts";

function formatEvent(event: GameEvent, t: TFunction<"game">): string {
  switch (event.type) {
    case "GameStarted":
      return t("log.gameStarted");
    case "TurnStarted":
      return t("log.turnStarted", { turn: event.data.turn_number, player: getPlayerDisplayName(event.data.player_id) });
    case "PhaseChanged":
      return t("log.phaseChanged", { phase: event.data.phase });
    case "PriorityPassed":
      return t("log.priorityPassed", { player: getPlayerDisplayName(event.data.player_id) });
    case "SpellCast":
      return t("log.spellCast", { player: getPlayerDisplayName(event.data.controller) });
    case "AbilityActivated":
      return t("log.abilityActivated", { sourceId: event.data.source_id });
    case "ZoneChanged":
      // `from` is null for token creation (CR 111.1 + CR 603.6a — tokens are
      // created in the battlefield zone with no prior zone).
      return event.data.from
        ? t("log.zoneChangedMoved", { objectId: event.data.object_id, from: event.data.from, to: event.data.to })
        : t("log.zoneChangedEnters", { objectId: event.data.object_id, to: event.data.to });
    case "LifeChanged":
      return t("log.lifeChanged", {
        player: getPlayerDisplayName(event.data.player_id),
        sign: event.data.amount >= 0 ? "+" : "",
        amount: event.data.amount,
      });
    case "ManaAdded":
      return t("log.manaAdded", { player: getPlayerDisplayName(event.data.player_id), manaType: event.data.mana_type });
    case "PermanentTapped":
      return t("log.permanentTapped", { objectId: event.data.object_id });
    case "PermanentUntapped":
      return t("log.permanentUntapped", { objectId: event.data.object_id });
    case "PlayerLost":
      return t("log.playerLost", { player: getPlayerDisplayName(event.data.player_id) });
    case "MulliganStarted":
      return t("log.mulliganStarted");
    case "CardsDrawn":
      return t("log.cardsDrawn", { player: getPlayerDisplayName(event.data.player_id), count: event.data.count });
    case "CardDrawn":
      return t("log.cardDrawn", { player: getPlayerDisplayName(event.data.player_id) });
    case "LandPlayed":
      return t("log.landPlayed", { player: getPlayerDisplayName(event.data.player_id) });
    case "StackPushed":
      return t("log.stackPushed", { objectId: event.data.object_id });
    case "StackResolved":
      return t("log.stackResolved", { objectId: event.data.object_id });
    case "Discarded":
      return t("log.discarded", { player: getPlayerDisplayName(event.data.player_id) });
    case "DamageCleared":
      return t("log.damageCleared", { objectId: event.data.object_id });
    case "GameOver":
      return event.data.winner != null
        ? t("log.gameOverWinner", { player: getPlayerDisplayName(event.data.winner) })
        : t("log.gameOverDraw");
    case "DamageDealt":
      return "Player" in event.data.target
        ? t("log.damageDealtPlayer", {
            sourceId: event.data.source_id,
            amount: event.data.amount,
            player: getPlayerDisplayName(event.data.target.Player),
          })
        : t("log.damageDealtObject", {
            sourceId: event.data.source_id,
            amount: event.data.amount,
            objectId: event.data.target.Object,
          });
    case "SpellCountered":
      return t("log.spellCountered", { objectId: event.data.object_id, counteredBy: event.data.countered_by });
    case "CounterAdded":
      return t("log.counterAdded", { counterType: event.data.counter_type, count: event.data.count, objectId: event.data.object_id });
    case "CounterRemoved":
      return t("log.counterRemoved", { counterType: event.data.counter_type, count: event.data.count, objectId: event.data.object_id });
    case "TokenCreated":
      return t("log.tokenCreated", { name: event.data.name });
    case "CreatureDestroyed":
      return t("log.creatureDestroyed", { objectId: event.data.object_id });
    case "PermanentSacrificed":
      return t("log.permanentSacrificed", { player: getPlayerDisplayName(event.data.player_id), objectId: event.data.object_id });
    case "EffectResolved":
      return t("log.effectResolved", { kind: event.data.kind });
    case "AttackersDeclared":
      return t("log.attackersDeclared", { count: event.data.attacker_ids.length });
    case "BlockersDeclared":
      return t("log.blockersDeclared", { count: event.data.assignments.length });
    case "BecomesTarget":
      return "Player" in event.data.target
        ? t("log.becomesTargetPlayer", {
            player: getPlayerDisplayName(event.data.target.Player),
            sourceId: event.data.source_id,
          })
        : t("log.becomesTarget", { objectId: event.data.target.Object, sourceId: event.data.source_id });
    case "ReplacementApplied":
      return t("log.replacementApplied", { eventType: event.data.event_type });
    case "CompanionRevealed":
      return t("log.companionRevealed", { player: getPlayerDisplayName(event.data.player), cardName: event.data.card_name });
    case "CompanionMovedToHand":
      return t("log.companionMovedToHand", { player: getPlayerDisplayName(event.data.player), cardName: event.data.card_name });
    case "PowerToughnessChanged": {
      const d = event.data;
      const sign = (n: number) => (n >= 0 ? `+${n}` : `${n}`);
      return t("log.powerToughnessChanged", {
        objectId: d.object_id,
        power: d.power,
        toughness: d.toughness,
        powerDelta: sign(d.power_delta),
        toughnessDelta: sign(d.toughness_delta),
      });
    }
    default:
      return t("log.genericEvent", { type: (event as GameEvent).type });
  }
}

export function GameLog() {
  const { t } = useTranslation("game");
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
        {t("log.title")}
      </h3>
      <div
        ref={scrollRef}
        className="flex-1 overflow-y-auto rounded bg-gray-900 p-1.5 font-mono text-[10px] leading-relaxed text-gray-300"
      >
        {events.length === 0 ? (
          <p className="italic text-gray-600">{t("log.noEvents")}</p>
        ) : (
          events.map((event, i) => (
            <div key={i} className="border-b border-gray-800 py-0.5">
              {formatEvent(event, t)}
            </div>
          ))
        )}
      </div>
    </div>
  );
}
