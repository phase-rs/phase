import type {
  GameAction,
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
import { playOrCastActionsForObject } from "./cardActionChoice.ts";

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

/** Resolve the opponent tab/board focus, ignoring eliminated seats. */
export function resolveFocusedOpponent(
  focusedOpponent: PlayerId | null,
  liveOpponents: PlayerId[],
): PlayerId | null {
  if (liveOpponents.length === 0) return null;
  if (focusedOpponent != null && liveOpponents.includes(focusedOpponent)) {
    return focusedOpponent;
  }
  return liveOpponents[0] ?? null;
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

/**
 * Whether a face-down card sitting in the shared Exile zone is visible to
 * `viewerId`.
 *
 * Mirrors the engine's `hidden_facedown_exile_ids` gate
 * (`crates/engine/src/game/visibility.rs`, CR 406.3 + CR 702.75a +
 * CR 702.143e): a foretold card's owner may look at it, and the controller of
 * the permanent that Hideaway-exiled a card may look at it. Every other
 * face-down exile — including a plain `TrackedBySource` link that grants no
 * look-permission (Bomat Courier, Necropotence, Asmodeus) — stays hidden.
 *
 * Like `isLibraryCardRevealedToViewer` above, this exists because single-player
 * renders the raw, unredacted state: `obj.face_down` alone can't distinguish
 * "hidden from this viewer" from "visible to this viewer", and the object's
 * `name`/`printed_ref` carry the real identity regardless of viewer. Used by
 * the exile `ZoneViewer` to keep an opponent's Hideaway-exiled card (or a
 * non-owner's foretold exile) from leaking its name or image.
 */
export function isFaceDownExileCardVisibleToViewer(
  gameState: GameState | null,
  obj: GameObject,
  viewerId: PlayerId,
): boolean {
  if (!gameState || !obj.face_down) return false;
  if (obj.foretold && obj.owner === viewerId) return true;
  return (gameState.exile_links ?? []).some(
    (link) =>
      link.exiled_id === obj.id &&
      link.kind === "HideawayLookable" &&
      gameState.objects[link.source_id]?.controller === viewerId,
  );
}

export function getWaitingForObjectChoiceIds(
  waitingFor: WaitingFor | null | undefined,
): ObjectId[] {
  switch (waitingFor?.type) {
    case "TargetSelection":
    case "TriggerTargetSelection":
      return (waitingFor.data.selection?.current_legal_targets ?? []).flatMap((target) =>
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
    default:
      return [];
  }
}

export interface BattlefieldSacrificeChoiceView {
  objectIds: ObjectId[];
  count: number;
  minCount: number;
  upTo: boolean;
}

export type BoardChoiceIntent =
  | "sacrifice"
  | "return"
  | "exile"
  | "tap"
  | "crew"
  | "saddle"
  | "station"
  | "blight"
  | "ringBearer";

export type BoardChoiceSelection =
  | { type: "single"; immediate: true }
  | { type: "exactCount"; count: number; immediate?: boolean }
  | { type: "rangeCount"; min: number; max: number }
  | { type: "totalPowerAtLeast"; power: number };

export type BoardChoiceResponse =
  | { type: "SelectCards" }
  | { type: "CrewVehicle"; vehicleId: ObjectId }
  | { type: "ActivateStation"; spacecraftId: ObjectId }
  | { type: "SaddleMount"; mountId: ObjectId }
  | { type: "ChooseRingBearer" }
  | { type: "HarmonizeTap" };

export interface BoardChoiceView {
  player: PlayerId;
  objectIds: ObjectId[];
  intent: BoardChoiceIntent;
  selection: BoardChoiceSelection;
  response: BoardChoiceResponse;
  sourceId?: ObjectId;
  skipAction?: GameAction;
  cancelAction?: GameAction;
}

function payCostSourceId(data: Extract<WaitingFor, { type: "PayCost" }>["data"]): ObjectId | undefined {
  if (data.resume.type === "ManaAbility") {
    return (data.resume.ManaAbility as { source_id?: ObjectId } | undefined)?.source_id;
  }
  return (data.resume.Spell as { object_id?: ObjectId } | undefined)?.object_id;
}

function countSelection(count: number, minCount: number): BoardChoiceSelection {
  if (count === 1 && minCount === 1) {
    return { type: "exactCount", count, immediate: true };
  }
  if (minCount === count) {
    return { type: "exactCount", count };
  }
  return { type: "rangeCount", min: minCount, max: count };
}

function confirmedCountSelection(count: number, minCount: number): BoardChoiceSelection {
  if (minCount === count) {
    return { type: "exactCount", count };
  }
  return { type: "rangeCount", min: minCount, max: count };
}

export function getBoardChoiceView(
  waitingFor: WaitingFor | null | undefined,
  objects?: Record<ObjectId, GameObject | undefined>,
): BoardChoiceView | null {
  switch (waitingFor?.type) {
    case "EffectZoneChoice": {
      if (waitingFor.data.zone !== "Battlefield") return null;
      let intent: BoardChoiceIntent | null = null;
      if (
        waitingFor.data.effect_kind === "Sacrifice" &&
        waitingFor.data.destination == null
      ) {
        intent = "sacrifice";
      } else if (waitingFor.data.destination === "Hand") {
        intent = "return";
      } else if (waitingFor.data.destination === "Exile") {
        intent = "exile";
      }
      if (!intent) return null;
      const minCount = waitingFor.data.up_to === true ? waitingFor.data.min_count ?? 0 : waitingFor.data.count;
      return {
        player: waitingFor.data.player,
        objectIds: waitingFor.data.cards,
        intent,
        selection: countSelection(waitingFor.data.count, minCount),
        response: { type: "SelectCards" },
        sourceId: waitingFor.data.source_id,
      };
    }
    case "PayCost": {
      if (!isBattlefieldCostChoice(waitingFor, objects)) return null;
      switch (waitingFor.data.kind.type) {
        case "Sacrifice":
          return {
            player: waitingFor.data.player,
            objectIds: waitingFor.data.choices,
            intent: "sacrifice",
            selection: confirmedCountSelection(waitingFor.data.count, waitingFor.data.min_count),
            response: { type: "SelectCards" },
            sourceId: payCostSourceId(waitingFor.data),
            cancelAction: waitingFor.data.resume.type === "Spell" ? { type: "CancelCast" } : undefined,
          };
        case "ReturnToHand":
          return {
            player: waitingFor.data.player,
            objectIds: waitingFor.data.choices,
            intent: "return",
            selection: confirmedCountSelection(waitingFor.data.count, waitingFor.data.min_count),
            response: { type: "SelectCards" },
            sourceId: payCostSourceId(waitingFor.data),
            cancelAction: waitingFor.data.resume.type === "Spell" ? { type: "CancelCast" } : undefined,
          };
        case "ExilePermanent":
          return {
            player: waitingFor.data.player,
            objectIds: waitingFor.data.choices,
            intent: "exile",
            selection: confirmedCountSelection(waitingFor.data.count, waitingFor.data.min_count),
            response: { type: "SelectCards" },
            sourceId: payCostSourceId(waitingFor.data),
            cancelAction: waitingFor.data.resume.type === "Spell" ? { type: "CancelCast" } : undefined,
          };
        case "TapCreatures":
          return {
            player: waitingFor.data.player,
            objectIds: waitingFor.data.choices,
            intent: "tap",
            selection: confirmedCountSelection(waitingFor.data.count, waitingFor.data.count),
            response: { type: "SelectCards" },
            sourceId: payCostSourceId(waitingFor.data),
            cancelAction: waitingFor.data.resume.type === "Spell" ? { type: "CancelCast" } : undefined,
          };
        default:
          return null;
      }
    }
    case "WardSacrificeChoice":
      return {
        player: waitingFor.data.player,
        objectIds: waitingFor.data.permanents,
        intent: "sacrifice",
        selection:
          waitingFor.data.min_total_power != null
            ? { type: "totalPowerAtLeast", power: waitingFor.data.min_total_power }
            : { type: "single", immediate: true },
        response: { type: "SelectCards" },
      };
    case "CrewVehicle":
      return {
        player: waitingFor.data.player,
        objectIds: waitingFor.data.eligible_creatures,
        intent: "crew",
        selection: { type: "totalPowerAtLeast", power: waitingFor.data.crew_power },
        response: { type: "CrewVehicle", vehicleId: waitingFor.data.vehicle_id },
        sourceId: waitingFor.data.vehicle_id,
      };
    case "SaddleMount":
      return {
        player: waitingFor.data.player,
        objectIds: waitingFor.data.eligible_creatures,
        intent: "saddle",
        selection: { type: "totalPowerAtLeast", power: waitingFor.data.saddle_power },
        response: { type: "SaddleMount", mountId: waitingFor.data.mount_id },
        sourceId: waitingFor.data.mount_id,
      };
    case "StationTarget":
      return {
        player: waitingFor.data.player,
        objectIds: waitingFor.data.eligible_creatures,
        intent: "station",
        selection: { type: "single", immediate: true },
        response: { type: "ActivateStation", spacecraftId: waitingFor.data.spacecraft_id },
        sourceId: waitingFor.data.spacecraft_id,
      };
    case "BlightChoice":
      return {
        player: waitingFor.data.player,
        objectIds: waitingFor.data.creatures,
        intent: "blight",
        selection: confirmedCountSelection(waitingFor.data.count, waitingFor.data.count),
        response: { type: "SelectCards" },
        sourceId: waitingFor.data.pending_cast.object_id,
        cancelAction: { type: "CancelCast" },
      };
    case "UnlessBounceChoice":
      return {
        player: waitingFor.data.player,
        objectIds: waitingFor.data.permanents,
        intent: "return",
        selection: { type: "single", immediate: true },
        response: { type: "SelectCards" },
      };
    case "HarmonizeTapChoice":
      return {
        player: waitingFor.data.player,
        objectIds: waitingFor.data.eligible_creatures,
        intent: "tap",
        selection: { type: "single", immediate: true },
        response: { type: "HarmonizeTap" },
        sourceId: waitingFor.data.pending_cast.object_id,
        skipAction: { type: "HarmonizeTap", data: { creature_id: null } },
        cancelAction: { type: "CancelCast" },
      };
    case "ChooseRingBearer":
      return {
        player: waitingFor.data.player,
        objectIds: waitingFor.data.candidates,
        intent: "ringBearer",
        selection: { type: "single", immediate: true },
        response: { type: "ChooseRingBearer" },
      };
    default:
      return null;
  }
}

function isBattlefieldCostChoice(
  waitingFor: Extract<WaitingFor, { type: "PayCost" }>,
  objects?: Record<ObjectId, GameObject | undefined>,
): boolean {
  switch (waitingFor.data.kind.type) {
    case "Sacrifice":
    case "ReturnToHand":
    case "ExilePermanent":
    case "TapCreatures":
      return (
        objects != null &&
        waitingFor.data.choices.length > 0 &&
        waitingFor.data.choices.every((id) => objects[id]?.zone === "Battlefield")
      );
    default:
      return false;
  }
}

export function buildBoardChoiceAction(
  choice: BoardChoiceView,
  selectedIds: ObjectId[],
): GameAction {
  switch (choice.response.type) {
    case "SelectCards":
      return { type: "SelectCards", data: { cards: selectedIds } };
    case "CrewVehicle":
      return {
        type: "CrewVehicle",
        data: { vehicle_id: choice.response.vehicleId, creature_ids: selectedIds },
      };
    case "ActivateStation":
      return {
        type: "ActivateStation",
        data: { spacecraft_id: choice.response.spacecraftId, creature_id: selectedIds[0] },
      };
    case "SaddleMount":
      return {
        type: "SaddleMount",
        data: { mount_id: choice.response.mountId, creature_ids: selectedIds },
      };
    case "ChooseRingBearer":
      return { type: "ChooseRingBearer", data: { target: selectedIds[0] } };
    case "HarmonizeTap":
      return { type: "HarmonizeTap", data: { creature_id: selectedIds[0] } };
  }
}

export function boardChoiceSelectedPower(
  choice: BoardChoiceView,
  selectedIds: ObjectId[],
  objects: Record<ObjectId, GameObject> | undefined,
): number {
  if (choice.selection.type !== "totalPowerAtLeast") return 0;
  return selectedIds.reduce((sum, id) => {
    const obj = objects?.[id];
    return sum + Math.max(obj?.power ?? 0, 0);
  }, 0);
}

export function canConfirmBoardChoice(
  choice: BoardChoiceView,
  selectedIds: ObjectId[],
  objects: Record<ObjectId, GameObject> | undefined,
): boolean {
  switch (choice.selection.type) {
    case "single":
      return selectedIds.length === 1;
    case "exactCount":
      return selectedIds.length === choice.selection.count;
    case "rangeCount":
      return selectedIds.length >= choice.selection.min && selectedIds.length <= choice.selection.max;
    case "totalPowerAtLeast":
      return boardChoiceSelectedPower(choice, selectedIds, objects) >= choice.selection.power;
  }
}

export function boardChoiceMaxSelection(choice: BoardChoiceView): number | null {
  switch (choice.selection.type) {
    case "single":
      return 1;
    case "exactCount":
      return choice.selection.count;
    case "rangeCount":
      return choice.selection.max;
    case "totalPowerAtLeast":
      return null;
  }
}

export function isBoardChoiceImmediate(choice: BoardChoiceView): boolean {
  switch (choice.selection.type) {
    case "single":
      return true;
    case "exactCount":
      return choice.selection.immediate === true;
    case "rangeCount":
    case "totalPowerAtLeast":
      return false;
  }
}

export function getBattlefieldSacrificeChoice(
  waitingFor: WaitingFor | null | undefined,
): BattlefieldSacrificeChoiceView | null {
  const choice = getBoardChoiceView(waitingFor);
  if (!choice || choice.intent !== "sacrifice") return null;
  if (choice.selection.type === "totalPowerAtLeast") {
    return {
      objectIds: choice.objectIds,
      count: choice.objectIds.length,
      minCount: 1,
      upTo: false,
    };
  }
  if (choice.selection.type === "single") {
    return {
      objectIds: choice.objectIds,
      count: 1,
      minCount: 1,
      upTo: false,
    };
  }
  if (choice.selection.type === "exactCount") {
    return {
      objectIds: choice.objectIds,
      count: choice.selection.count,
      minCount: choice.selection.count,
      upTo: false,
    };
  }
  return {
    objectIds: choice.objectIds,
    count: choice.selection.max,
    minCount: choice.selection.min,
    upTo: true,
  };
}

export type ZoneViewerTarget = {
  zone: "graveyard" | "exile";
  playerId: PlayerId;
  objectIds: ObjectId[];
};

/**
 * When the player has Priority and the engine surfaces play/cast actions on
 * graveyard or exile cards (Retrace, Flashback, Adventure, etc.), return the
 * sole zone pile to auto-open in `ZoneViewer`. Mirrors the object-choice
 * auto-open grouping: only auto-open when every castable card lives in one
 * zone+owner pile so we don't trap the player in the wrong graveyard.
 */
export function getCastableZoneViewerTarget(
  waitingFor: WaitingFor | null | undefined,
  objects: Record<ObjectId, GameObject> | undefined,
  legalActionsByObject: Record<string, GameAction[]> | undefined,
): ZoneViewerTarget | null {
  if (waitingFor?.type !== "Priority" || !objects || !legalActionsByObject) {
    return null;
  }

  const groups = new Set<string>();
  let firstHit: ZoneViewerTarget | null = null;
  const objectIds: ObjectId[] = [];

  for (const key of Object.keys(legalActionsByObject)) {
    const objectId = Number(key) as ObjectId;
    if (playOrCastActionsForObject(legalActionsByObject, objectId).length === 0) {
      continue;
    }
    const obj = objects[objectId];
    if (!obj) continue;
    if (obj.zone !== "Graveyard" && obj.zone !== "Exile") continue;

    const zone: ZoneViewerTarget["zone"] =
      obj.zone === "Graveyard" ? "graveyard" : "exile";
    groups.add(`${zone}:${obj.owner}`);
    objectIds.push(objectId);
    if (!firstHit) firstHit = { zone, playerId: obj.owner, objectIds };
  }

  objectIds.sort((a, b) => a - b);
  return groups.size === 1 ? firstHit : null;
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
  // CR 701.54: the Ring-bearer must render as its own card even when a
  // same-named, identically-statted permanent (e.g. another Army token)
  // would otherwise collapse it into a shared group — otherwise the
  // ring-bearer badge can land on the wrong representative or disappear
  // entirely behind a stack badge.
  const ringBearerIds = new Set(
    Object.values(gameState.ring_bearer ?? {}).filter(
      (id): id is ObjectId => id != null,
    ),
  );
  return buildPlayerBattlefieldViewFromObjects(playerObjects, ringBearerIds);
}

export function buildPlayerBattlefieldViewFromObjects(
  playerObjects: GameObject[],
  ringBearerIds: ReadonlySet<ObjectId> = new Set(),
): PlayerBattlefieldView {
  const partition = partitionByType(playerObjects);
  const objectMap = new Map(playerObjects.map((object) => [object.id, object]));
  const resolveObjects = (ids: ObjectId[]) =>
    ids
      .map((id) => objectMap.get(id))
      .filter(Boolean) as GameObject[];

  return {
    creatures: groupByName(resolveObjects(partition.creatures), ringBearerIds),
    lands: groupByName(resolveObjects(partition.lands), ringBearerIds),
    support: groupByName(resolveObjects(partition.support), ringBearerIds),
    planeswalkers: groupByName(resolveObjects(partition.planeswalkers), ringBearerIds),
    other: groupByName(resolveObjects(partition.other), ringBearerIds),
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
