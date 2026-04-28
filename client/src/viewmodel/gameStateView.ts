import type {
  GameObject,
  GameState,
  ObjectId,
  PlayerId,
  WaitingFor,
} from "../adapter/types";
import {
  groupByName,
  partitionByType,
  type GroupedPermanent,
} from "./battlefieldProps";

export interface PlayerBattlefieldView {
  creatures: GroupedPermanent[];
  lands: GroupedPermanent[];
  support: GroupedPermanent[];
  planeswalkers: GroupedPermanent[];
  other: GroupedPermanent[];
}

export function getOpponentIds(
  gameState: GameState | null,
  playerId: PlayerId,
): PlayerId[] {
  if (!gameState) return [];
  const seatOrder = gameState.seat_order ?? gameState.players.map((player) => player.id);
  const eliminated = new Set(gameState.eliminated_players ?? []);
  return seatOrder.filter((id) => id !== playerId && !eliminated.has(id));
}

export function getPlayerZoneIds(
  gameState: GameState | null,
  zone: "graveyard" | "exile",
  playerId: PlayerId,
): ObjectId[] {
  if (!gameState) return [];
  if (zone === "graveyard") {
    return gameState.players[playerId]?.graveyard ?? [];
  }
  return gameState.exile.filter((id) => gameState.objects[id]?.owner === playerId);
}

export function getWaitingForObjectChoiceIds(
  waitingFor: WaitingFor | null | undefined,
): ObjectId[] {
  switch (waitingFor?.type) {
    case "TargetSelection":
    case "TriggerTargetSelection":
      return waitingFor.data.selection.current_legal_targets.flatMap((target) =>
        "Object" in target ? [target.Object] : [],
      );
    case "CopyTargetChoice":
      return waitingFor.data.valid_targets;
    case "ExploreChoice":
      return waitingFor.data.choosable;
    default:
      return [];
  }
}

export function buildPlayerBattlefieldView(
  gameState: GameState | null,
  playerId: PlayerId,
): PlayerBattlefieldView {
  if (!gameState) {
    return emptyBattlefieldView();
  }

  const battlefieldObjects = gameState.battlefield
    .map((id) => gameState.objects[id])
    .filter(Boolean) as GameObject[];
  const playerObjects = battlefieldObjects.filter(
    (object) => object.controller === playerId,
  );
  return buildPlayerBattlefieldViewFromObjects(playerObjects);
}

export function buildPlayerBattlefieldViewFromObjects(
  playerObjects: GameObject[],
): PlayerBattlefieldView {
  const partition = partitionByType(playerObjects);
  const objectMap = new Map(playerObjects.map((object) => [object.id, object]));
  const resolveObjects = (ids: ObjectId[]) =>
    ids
      .map((id) => objectMap.get(id))
      .filter(Boolean) as GameObject[];

  return {
    creatures: groupByName(resolveObjects(partition.creatures)),
    lands: groupByName(resolveObjects(partition.lands)),
    support: groupByName(resolveObjects(partition.support)),
    planeswalkers: groupByName(resolveObjects(partition.planeswalkers)),
    other: groupByName(resolveObjects(partition.other)),
  };
}

function emptyBattlefieldView(): PlayerBattlefieldView {
  return {
    creatures: [],
    lands: [],
    support: [],
    planeswalkers: [],
    other: [],
  };
}
