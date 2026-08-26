import type {
  GameAction,
  GameObject,
  GameState,
  ObjectCounterDisplay,
  ObjectId,
  PlayerId,
  TapCreaturesAggregate,
  TargetRef,
  WaitingFor,
} from "../adapter/types";
import type {
  MultiplayerBoardLayout,
  ResolvedMultiplayerBoardLayout,
} from "../stores/preferencesStore";
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

/**
 * Resolves the display layout without mutating the persisted raw preference.
 * Two-player games always use the focused presentation. For multiplayer,
 * `auto` selects focused on mobile and split elsewhere; explicit choices are
 * honored on either viewport.
 */
export function resolveMultiplayerBoardLayout(
  layout: MultiplayerBoardLayout,
  seatCount: number,
  isMobile: boolean,
): ResolvedMultiplayerBoardLayout {
  if (seatCount <= 2) return "focused";
  if (layout === "auto") return isMobile ? "focused" : "split";
  return layout;
}

export function isSplitBoardActive(
  layout: MultiplayerBoardLayout,
  seatCount: number,
): boolean {
  return layout === "split" && seatCount > 2;
}

export function shouldRenderFocusedOpponentTopRow(
  layout: MultiplayerBoardLayout,
  seatCount: number,
): boolean {
  return !isSplitBoardActive(layout, seatCount);
}

export function getVisibleBoardPlayerIds(
  gameState: GameState | null,
  viewerId: PlayerId,
  focusedOpponent: PlayerId | null,
  layout: MultiplayerBoardLayout,
): PlayerId[] {
  if (!gameState) return [];

  const opponents = getOpponentIds(gameState, viewerId);
  if (isOneOnOne(gameState)) {
    return opponents[0] == null ? [viewerId] : [viewerId, opponents[0]];
  }

  if (isSplitBoardActive(layout, getSeatCount(gameState))) {
    return [viewerId, ...opponents];
  }

  const focusedId = resolveFocusedOpponent(focusedOpponent, opponents);
  return focusedId == null ? [viewerId] : [viewerId, focusedId];
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
    // the full ordered library; the library viewer consumes each object's
    // engine-projected display visibility before rendering it.
    return gameState.players[playerId]?.library ?? [];
  }
  return gameState.exile.filter((id) => gameState.objects[id]?.owner === playerId);
}

/**
 * Whether the engine has projected a library card's identity for this viewer.
 * The display layer reads the per-object projection; it does not reconstruct
 * visibility from reveal, look, or product-knowledge state.
 */
export function isLibraryCardRevealedToViewer(
  gameState: GameState | null,
  objectId: ObjectId,
  _viewerId: PlayerId,
): boolean {
  return gameState?.objects[objectId]?.display_visible_to_viewer ?? false;
}

/**
 * Whether a face-down card sitting in the shared Exile zone is visible to
 * `viewerId`.
 *
 * The rules permissions (Foretell, Hideaway, and face-down look effects) are
 * resolved by Rust before this value reaches the client.
 */
export function isFaceDownExileCardVisibleToViewer(
  _gameState: GameState | null,
  obj: GameObject,
  _viewerId: PlayerId,
): boolean {
  return obj.face_down && (obj.display_visible_to_viewer ?? false);
}

/**
 * Whether `viewerId` may see the identity of `obj` for the card-report picker.
 *
 * Rust resolves every zone and reveal rule into `display_visible_to_viewer`.
 * Conservative: hides on an omitted projection, never leaks.
 */
export function isObjectReportableToViewer(
  gameState: GameState | null,
  obj: GameObject,
  _viewerId: PlayerId,
): boolean {
  return gameState != null && (obj.display_visible_to_viewer ?? false);
}

/**
 * The engine-authored legal set for a prompt the player answers with ONE
 * `GameAction::ChooseTarget` carrying a single `TargetRef` — CR 115.1: "the
 * targets are object(s) and/or player(s) the spell or ability will affect."
 *
 * Returns `null` when `waitingFor` is not such a prompt, so a caller can tell
 * "no click-targeting is in progress" from "click-targeting is in progress and
 * nothing is legal yet". Same applicable-or-not convention as
 * `getBoardChoiceView`, which `DialogHost` consumes as `!= null`.
 *
 * Membership needs BOTH halves; do not widen on either alone:
 *   1. the engine has a `GameAction::ChooseTarget` apply arm for the variant, and
 *   2. its legal-set field is typed `TargetRef[]`, so it can name a player.
 *
 * The other `ChooseTarget` variants carry `ObjectId[]` and therefore cannot name
 * a player — `CopyTargetChoice.valid_targets`, `ExploreChoice.choosable`,
 * `PopulateChoice.valid_tokens` (the engine's populate arm matches
 * `TargetRef::Object` outright). They stay in `getWaitingForObjectChoiceIds`.
 *
 * Prompts answered by a DIFFERENT action are dialog-only by design and stay out.
 * Each already renders its player rows in its own modal, so nothing is
 * unreachable — only the click path differs:
 *   - `DistributeAmong`   -> `DistributeAmong { distribution }`; a click carries
 *                            no amount (DistributeAmongModal).
 *   - `ProliferateChoice` -> `SelectTargets`; CR 701.34a chooses any-size subset
 *     `TimeTravelChoice`     of permanents and/or players (ProliferateModal).
 *     `ChooseObjectsSelection`
 *   - `EachPlayerCopyChosenSelection` -> `SelectTargets`, ORDERED (first pick is
 *                            copied, second scales); a click carries no order.
 *   - `ChooseOneOfBranch` -> `ChooseBranch { index }`; `parent_targets` is
 *                            continuation context the modal never reads.
 * All of the above are host-wrapped (absent from
 * `DialogHost.CLICK_THROUGH_WAITING_FOR_TYPES`), so a HUD glow beneath their
 * `fixed inset-0` host could not be clicked even if it were offered.
 *
 * NOTE: `getWaitingForObjectChoiceIds` below keeps its own switch over a
 * superset of these arms. The two must agree on every shared arm, and neither
 * may silently gain an arm the other lacks. `PARTITION_FIXTURES` in
 * `gameStateView.test.ts` is a `Record<WaitingFor["type"], …>`, so adding a
 * variant to `WaitingFor` fails `pnpm run type-check` until that map records
 * what the player axis does with it. Apply the two-criteria test above before
 * answering that compile error.
 */
export function getWaitingForClickTargetRefs(
  waitingFor: WaitingFor | null | undefined,
): TargetRef[] | null {
  switch (waitingFor?.type) {
    case "TargetSelection":
    case "TriggerTargetSelection":
      return waitingFor.data.selection?.current_legal_targets ?? [];
    case "CopyRetarget": {
      // CR 707.10c: the copy's controller may choose new targets, one slot at a
      // time — read the slot the engine is currently asking about.
      const slot = waitingFor.data.target_slots[waitingFor.data.current_slot ?? 0];
      return slot?.legal_alternatives ?? [];
    }
    case "RetargetChoice":
      // CR 115.7: a single-target retarget (Bolt Bend, Misdirection) is answered by a
      // board/HUD click. An `All`-scope retarget keeps RetargetChoiceModal, whose
      // confirm button needs pointer events, and the engine has no `ChooseTarget`
      // arm for it — so it is not a click prompt at all.
      return waitingFor.data.scope.type === "Single"
        ? waitingFor.data.legal_new_targets
        : null;
    case "ReturnAsAuraTarget":
      // CR 303.4: an Aura enters attached to an object OR a player, so this list
      // mixes both. The engine owns which hosts are legal; the client only routes
      // each ref to the surface that can render it.
      return waitingFor.data.legal_targets;
    default:
      return null;
  }
}

/**
 * The players the engine is currently offering as click targets — the
 * player-axis sibling of `getWaitingForObjectChoiceIds`, and the single
 * authority for every surface that renders a seat (PlayerHud, OpponentHud's 1v1
 * pill and multiplayer tabs, OpponentSeatHeader).
 *
 * Pair it with `useCanActForWaitingState()` at the call site: this function says
 * WHICH players are legal, the hook says WHETHER this client may answer. The two
 * resolve different seats on purpose — the hook against the real seat, the
 * membership test against the rendered seat — and under a CR 723 turn-control
 * effect those differ. See `usePerspectivePlayerId`.
 */
export function getWaitingForPlayerChoiceIds(
  waitingFor: WaitingFor | null | undefined,
): PlayerId[] {
  return (getWaitingForClickTargetRefs(waitingFor) ?? []).flatMap((target) =>
    "Player" in target ? [target.Player] : [],
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
      // CR 115.7: Single-target retargets (Bolt Bend, Misdirection) are resolved by
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
      // the board; player hosts are projected by
      // `getWaitingForPlayerChoiceIds` above, which every seat-rendering
      // surface reads.
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
  | "untap"
  | "crew"
  | "saddle"
  | "station"
  | "blight"
  | "ringBearer"
  | "keep";

export type BoardChoiceSelection =
  | { type: "single"; immediate: true }
  | { type: "exactCount"; count: number; immediate?: boolean }
  | { type: "rangeCount"; min: number; max: number }
  // `contributions` maps an eligible object id to the power it contributes
  // toward the threshold. Supplied by the engine for Crew (CR 702.122a), where
  // "as though its power were N greater" / "using its toughness" make the
  // contribution differ from printed power; when absent (Ward-sacrifice total
  // power), the summation falls back to raw power.
  | { type: "totalPowerAtLeast"; power: number; contributions?: Record<ObjectId, number> }
  // CR 107.1c + CR 701.21a (Slaughter the Strong): keep any subset whose
  // combined power is at most `power`; selecting beyond it blocks confirm.
  | { type: "totalPowerAtMost"; power: number };

export type BoardChoiceResponse =
  | { type: "SelectCards" }
  | { type: "ChooseUntap"; objectId: ObjectId }
  | { type: "CrewVehicle"; vehicleId: ObjectId }
  | { type: "ActivateStation"; spacecraftId: ObjectId }
  | { type: "SaddleMount"; mountId: ObjectId }
  | { type: "ChooseRingBearer" }
  | { type: "HarmonizeTap" }
  | { type: "ChooseKeptCreatures" }
  | { type: "ChooseKeptPermanents" };

export interface BoardChoiceView {
  player: PlayerId;
  objectIds: ObjectId[];
  intent: BoardChoiceIntent;
  selection: BoardChoiceSelection;
  response: BoardChoiceResponse;
  sourceId?: ObjectId;
  skipAction?: GameAction;
  skipLabel?: "keepTapped";
  cancelAction?: GameAction;
}

/**
 * Zip the engine's parallel `eligible_creatures` / `contributions` arrays into a
 * lookup from object id to its adjusted crew/saddle contribution. The engine
 * emits them index-aligned (CR 702.122a / 702.171a); a length mismatch falls
 * back to an empty map so the caller degrades to raw power rather than misreads.
 */
function zipContributions(
  eligibleCreatures: ObjectId[],
  contributions?: number[],
): Record<ObjectId, number> {
  const map: Record<ObjectId, number> = {};
  if (!contributions || eligibleCreatures.length !== contributions.length) {
    return map;
  }
  eligibleCreatures.forEach((id, index) => {
    map[id] = contributions[index];
  });
  return map;
}

function payCostSourceId(data: Extract<WaitingFor, { type: "PayCost" }>["data"]): ObjectId | undefined {
  if (data.resume.type === "ManaAbility") {
    return (data.resume.ManaAbility as { source_id?: ObjectId } | undefined)?.source_id;
  }
  if (data.resume.type === "Resolution") {
    return undefined;
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

/**
 * CR 208.1 + CR 601.2f (Crew CR 702.122a / Saddle CR 702.171a / Teamwork
 * CR 702.194a): map a TapCreatures Aggregate cost's advertised comparator to
 * the board-choice selection that gates confirmation on summed power — the
 * same `totalPowerAtLeast` mechanism CrewVehicle/SaddleMount already use.
 * Only `GE` ("total power N or greater") is constructible by any current
 * engine registration site (`TapCreaturesRequirement::total_power_at_least`
 * is the sole non-test constructor of the `Aggregate` requirement shape, and
 * it hardcodes `GE`). `totalPowerAtMost` is deliberately NOT reused for a
 * hypothetical `LE`: its Slaughter-the-Strong keep-set (CR 107.1c's "any
 * number") sums SIGNED power, so a negative-power creature genuinely lowers
 * the total, whereas the engine's tap-cost total over the same CR 208.1 power
 * axis sums POSITIVE power only (`tap_creature_power_contribution` clamps to
 * `max(power, 0)`) — reusing it would silently diverge from the engine the
 * instant a non-GE aggregate cost existed. Any comparator other than `GE`
 * therefore has no correct client representation yet; fail loud and block
 * confirmation rather than guess.
 */
function tapCreaturesAggregateSelection(aggregate: TapCreaturesAggregate): BoardChoiceSelection {
  switch (aggregate.comparator) {
    case "GE":
      return { type: "totalPowerAtLeast", power: aggregate.value };
    case "GT":
    case "LT":
    case "LE":
    case "EQ":
    case "NE":
      console.error(
        `Unsupported TapCreatures aggregate comparator "${aggregate.comparator}"; blocking confirmation.`,
      );
      return { type: "rangeCount", min: 0, max: 0 };
  }
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
      } else if (waitingFor.data.effect_kind === "Untap") {
        intent = "untap";
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
    // CR 107.1c + CR 701.21a (Slaughter the Strong): pick the creatures to keep
    // directly on the battlefield, capped by combined power; the rest are
    // sacrificed. Engine sends `eligible` + `cap`; dispatch is ChooseKeptCreatures.
    case "KeepWithinTotalPowerChoice":
      return {
        player: waitingFor.data.player,
        objectIds: waitingFor.data.eligible,
        intent: "keep",
        selection: { type: "totalPowerAtMost", power: waitingFor.data.cap },
        response: { type: "ChooseKeptCreatures" },
        sourceId: waitingFor.data.source_id,
      };
    // CR 101.4 + CR 701.21a: exact keeper-cardinality selection. The engine
    // supplies the legal pool and authoritative count; the board is display
    // only and submits the typed choice action below.
    case "KeepExactPermanentsChoice":
      return {
        player: waitingFor.data.player,
        objectIds: waitingFor.data.eligible,
        intent: "keep",
        selection: { type: "exactCount", count: waitingFor.data.required_count },
        response: { type: "ChooseKeptPermanents" },
        sourceId: waitingFor.data.source_id,
      };
    case "ChooseUntapSubset":
      return {
        player: waitingFor.data.player,
        objectIds: waitingFor.data.group,
        intent: "untap",
        selection: { type: "rangeCount", min: 0, max: waitingFor.data.max },
        response: { type: "SelectCards" },
      };
    case "UntapChoice": {
      const objectId = waitingFor.data.candidates[0];
      if (objectId == null) return null;
      return {
        player: waitingFor.data.player,
        objectIds: [objectId],
        intent: "untap",
        selection: { type: "single", immediate: true },
        response: { type: "ChooseUntap", objectId },
        skipAction: { type: "ChooseUntap", data: { object_id: objectId, untap: false } },
        skipLabel: "keepTapped",
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
        case "TapCreatures": {
          const { mode } = waitingFor.data.kind;
          return {
            player: waitingFor.data.player,
            objectIds: waitingFor.data.choices,
            intent: "tap",
            selection:
              typeof mode === "object"
                ? tapCreaturesAggregateSelection(mode.Aggregate)
                : confirmedCountSelection(waitingFor.data.count, waitingFor.data.min_count),
            response: { type: "SelectCards" },
            sourceId: payCostSourceId(waitingFor.data),
            cancelAction: waitingFor.data.resume.type === "Spell" ? { type: "CancelCast" } : undefined,
          };
        }
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
        selection: {
          type: "totalPowerAtLeast",
          power: waitingFor.data.crew_power,
          contributions: zipContributions(
            waitingFor.data.eligible_creatures,
            waitingFor.data.contributions,
          ),
        },
        response: { type: "CrewVehicle", vehicleId: waitingFor.data.vehicle_id },
        sourceId: waitingFor.data.vehicle_id,
        cancelAction: { type: "CancelCast" },
      };
    case "SaddleMount":
      return {
        player: waitingFor.data.player,
        objectIds: waitingFor.data.eligible_creatures,
        intent: "saddle",
        selection: {
          type: "totalPowerAtLeast",
          power: waitingFor.data.saddle_power,
          contributions: zipContributions(
            waitingFor.data.eligible_creatures,
            waitingFor.data.contributions,
          ),
        },
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
        selection: confirmedCountSelection(1, 1),
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
    case "ChooseUntap":
      return { type: "ChooseUntap", data: { object_id: choice.response.objectId, untap: true } };
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
    case "ChooseKeptCreatures":
      return { type: "ChooseKeptCreatures", data: { kept: selectedIds } };
    case "ChooseKeptPermanents":
      return { type: "ChooseKeptPermanents", data: { kept: selectedIds } };
  }
}

export function boardChoiceSelectedPower(
  choice: BoardChoiceView,
  selectedIds: ObjectId[],
  objects: Record<ObjectId, GameObject> | undefined,
): number {
  if (
    choice.selection.type !== "totalPowerAtLeast" &&
    choice.selection.type !== "totalPowerAtMost"
  ) {
    return 0;
  }
  // `totalPowerAtMost` (Slaughter the Strong's keep set) mirrors the engine's
  // CR 208.3 total, which sums raw power — a -1-power creature genuinely lowers
  // the total, so a 5/-1 pair fits a cap of 4. Crew/Saddle-style
  // `totalPowerAtLeast` contributes positive power only.
  const clampNegative = choice.selection.type === "totalPowerAtLeast";
  // CR 702.122a / 702.171a: for Crew/Saddle the engine supplies each creature's
  // adjusted contribution (Pilot tokens' "+2 greater", Giant Ox's toughness).
  // Prefer it over printed power so the UI gates on the same value the engine
  // validates; fall back to raw power when the engine sent no contributions.
  const contributions =
    choice.selection.type === "totalPowerAtLeast" ? choice.selection.contributions : undefined;
  return selectedIds.reduce((sum, id) => {
    const power = contributions?.[id] ?? objects?.[id]?.power ?? 0;
    return sum + (clampNegative ? Math.max(power, 0) : power);
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
    case "totalPowerAtMost":
      return boardChoiceSelectedPower(choice, selectedIds, objects) <= choice.selection.power;
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
    case "totalPowerAtMost":
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
    case "totalPowerAtMost":
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
  // `totalPowerAtMost` is only produced for the "keep" intent, which is filtered
  // out above; narrow it off so the rangeCount fallback stays well-typed.
  if (choice.selection.type === "totalPowerAtMost") return null;
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
  // CR 732.2a: engine-authored ∞-pile membership (accepted object-growth loop).
  // Read exactly like ring_bearer — the adapter attaches `derived` onto gameState.
  const unboundedPileIds = new Set(gameState.derived?.unbounded_pile ?? []);
  // CR 122.1: the engine's complete counter-display projection is part of the group IDENTITY —
  // two permanents that render different counter rows must not share a representative. Read
  // exactly like `unbounded_pile` above.
  return buildPlayerBattlefieldViewFromObjects(
    playerObjects,
    ringBearerIds,
    unboundedPileIds,
    gameState.derived?.counter_display,
  );
}

export function buildPlayerBattlefieldViewFromObjects(
  playerObjects: GameObject[],
  ringBearerIds: ReadonlySet<ObjectId> = new Set(),
  unboundedPileIds: ReadonlySet<ObjectId> = new Set(),
  counterDisplay: Record<string, ObjectCounterDisplay> | undefined,
): PlayerBattlefieldView {
  const partition = partitionByType(playerObjects);
  const objectMap = new Map(playerObjects.map((object) => [object.id, object]));
  const resolveObjects = (ids: ObjectId[]) =>
    ids
      .map((id) => objectMap.get(id))
      .filter(Boolean) as GameObject[];

  return {
    creatures: groupByName(resolveObjects(partition.creatures), ringBearerIds, unboundedPileIds, counterDisplay),
    lands: groupByName(resolveObjects(partition.lands), ringBearerIds, unboundedPileIds, counterDisplay),
    support: groupByName(resolveObjects(partition.support), ringBearerIds, unboundedPileIds, counterDisplay),
    planeswalkers: groupByName(resolveObjects(partition.planeswalkers), ringBearerIds, unboundedPileIds, counterDisplay),
    other: groupByName(resolveObjects(partition.other), ringBearerIds, unboundedPileIds, counterDisplay),
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
