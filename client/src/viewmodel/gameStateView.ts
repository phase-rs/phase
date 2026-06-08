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

// The game's seat count, stable across eliminations — the engine never
// removes from `seat_order`. Single source of truth for layout decisions
// like "is this 1v1?". Keep all callers (GameBoard, OpponentHud,
// BlockAssignmentLines, AttackTargetLines) routed through here so they
// cannot drift apart — the bug this helper exists to prevent is exactly
// that drift.
export function getSeatCount(gameState: GameState | null): number {
  if (!gameState) return 0;
  return gameState.seat_order?.length ?? gameState.players.length;
}

export function isOneOnOne(gameState: GameState | null): boolean {
  return getSeatCount(gameState) === 2;
}

export function getPlayerZoneIds(
  gameState: GameState | null,
  zone: "graveyard" | "exile" | "library",
  playerId: PlayerId,
): ObjectId[] {
  if (!gameState) return [];
  if (zone === "graveyard") {
    return gameState.players[playerId]?.graveyard ?? [];
  }
  if (zone === "library") {
    // library[0] = top of library (engine convention from zones.rs). Returns
    // the full ordered library; the library viewer filters to the cards the
    // engine has revealed to the viewer (isLibraryCardRevealedToViewer) so
    // unrevealed cards are never shown.
    return gameState.players[playerId]?.library ?? [];
  }
  return gameState.exile.filter((id) => gameState.objects[id]?.owner === playerId);
}

/**
 * Whether the engine has revealed a given library card's identity to `viewerId`.
 *
 * Mirrors the engine's library visibility (`crates/engine/src/game/visibility.rs`)
 * using the explicit reveal sets — NEVER the card name. In single-player the
 * client renders the raw, unredacted state (the `showAiHand` debug toggle depends
 * on it), so `name !== "Hidden Card"` is always true and cannot be used to infer
 * visibility; doing so leaks every opponent library card. This is the same
 * pattern `OpponentHand` uses for opponent hand cards.
 *
 * Deliberately excludes `public_revealed_cards`: the engine does not un-redact
 * library cards by that persistent memory set (a card revealed once and put back
 * must not leak its new position).
 */
export function isLibraryCardRevealedToViewer(
  gameState: GameState | null,
  objectId: ObjectId,
  viewerId: PlayerId,
): boolean {
  if (!gameState) return false;
  // CR 701.20b: publicly revealed top cards (RevealTop, "play with the top card
  // revealed") are visible to every player.
  if (gameState.revealed_cards?.includes(objectId)) return true;
  // CR 701.20e: a private "look at the top card" (Mishra's Bauble at an
  // opponent's library; your own scry look) surfaces the peeked ids only to the
  // looking player.
  return (
    gameState.private_look_player === viewerId &&
    (gameState.private_look_ids?.includes(objectId) ?? false)
  );
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
    case "CopyRetarget": {
      const slot = waitingFor.data.target_slots[waitingFor.data.current_slot ?? 0];
      return (slot?.legal_alternatives ?? []).flatMap((t) => "Object" in t ? [t.Object] : []);
    }
    case "RetargetChoice":
      // CR 115.7: Single-target retargets (Bolt Bend, Redirect) are resolved by
      // a board click; multi-target (`All`-scope) retargets keep the dialog.
      if (waitingFor.data.scope.type !== "Single") return [];
      return waitingFor.data.legal_new_targets.flatMap((target) =>
        "Object" in target ? [target.Object] : [],
      );
    case "ExploreChoice":
      return waitingFor.data.choosable;
    case "PopulateChoice":
      return waitingFor.data.valid_tokens;
    case "ReturnAsAuraTarget":
      // CR 303.4 / CR 115.1: `legal_targets` is a TargetRef[] of object hosts
      // *and* players (Curse / enchant-player Auras). Only object hosts glow on
      // the board; player hosts are handled by PlayerHud/OpponentHud glow.
      return waitingFor.data.legal_targets.flatMap((target) =>
        "Object" in target ? [target.Object] : [],
      );
    case "PairChoice":
      return waitingFor.data.choices;
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
