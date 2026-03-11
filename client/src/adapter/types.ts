// ── Identifiers ──────────────────────────────────────────────────────────

export type ObjectId = number;
export type CardId = number;
export type PlayerId = number;

// ── Enums (string literal unions matching Rust serde output) ─────────────

export type Phase =
  | "Untap"
  | "Upkeep"
  | "Draw"
  | "PreCombatMain"
  | "BeginCombat"
  | "DeclareAttackers"
  | "DeclareBlockers"
  | "CombatDamage"
  | "EndCombat"
  | "PostCombatMain"
  | "End"
  | "Cleanup";

export type Zone =
  | "Library"
  | "Hand"
  | "Battlefield"
  | "Graveyard"
  | "Stack"
  | "Exile"
  | "Command";

export type ManaColor = "White" | "Blue" | "Black" | "Red" | "Green";

export type ManaType = "White" | "Blue" | "Black" | "Red" | "Green" | "Colorless";

// ── Mana ─────────────────────────────────────────────────────────────────

export interface ManaRestriction {
  OnlyForSpellType: string;
}

export interface ManaUnit {
  color: ManaType;
  source_id: ObjectId;
  snow: boolean;
  restrictions: ManaRestriction[];
}

export interface ManaPool {
  mana: ManaUnit[];
}

export type ManaCost =
  | { type: "NoCost" }
  | { type: "Cost"; shards: string[]; generic: number };

// ── Card Types ───────────────────────────────────────────────────────────

export interface CardType {
  supertypes: string[];
  core_types: string[];
  subtypes: string[];
}

// ── Counter Types ────────────────────────────────────────────────────────

export type CounterType =
  | "Plus1Plus1"
  | "Minus1Minus1"
  | "Loyalty"
  | { Generic: string };

// ── Game Object ──────────────────────────────────────────────────────────

export interface GameObject {
  id: ObjectId;
  card_id: CardId;
  owner: PlayerId;
  controller: PlayerId;
  zone: Zone;
  tapped: boolean;
  face_down: boolean;
  flipped: boolean;
  transformed: boolean;
  damage_marked: number;
  dealt_deathtouch_damage: boolean;
  attached_to: ObjectId | null;
  attachments: ObjectId[];
  counters: Record<string, number>;
  name: string;
  power: number | null;
  toughness: number | null;
  loyalty: number | null;
  card_types: CardType;
  mana_cost: ManaCost;
  keywords: string[];
  abilities: string[];
  trigger_definitions: unknown[];
  replacement_definitions: unknown[];
  static_definitions: unknown[];
  svars: Record<string, string>;
  color: ManaColor[];
  base_power: number | null;
  base_toughness: number | null;
  base_keywords: string[];
  base_color: ManaColor[];
  timestamp: number;
  entered_battlefield_turn: number | null;
  has_unimplemented_mechanics?: boolean;
  has_summoning_sickness?: boolean;
  devotion?: number;
  back_face?: {
    name: string;
    power: number | null;
    toughness: number | null;
    card_types: CardType;
    keywords: string[];
    abilities: string[];
    color: ManaColor[];
  } | null;
}

// ── Player ───────────────────────────────────────────────────────────────

export interface Player {
  id: PlayerId;
  life: number;
  mana_pool: ManaPool;
  library: ObjectId[];
  hand: ObjectId[];
  graveyard: ObjectId[];
  has_drawn_this_turn: boolean;
  lands_played_this_turn: number;
  can_look_at_top_of_library?: boolean;
}

// ── Target Ref ───────────────────────────────────────────────────────────

export type TargetRef =
  | { Object: ObjectId }
  | { Player: PlayerId };

// ── Combat ───────────────────────────────────────────────────────────────

export interface AttackerInfo {
  object_id: ObjectId;
  defending_player: PlayerId;
}

export type DamageTarget =
  | { Object: ObjectId }
  | { Player: PlayerId };

export interface DamageAssignment {
  target: DamageTarget;
  amount: number;
}

export interface CombatState {
  attackers: AttackerInfo[];
  blocker_assignments: Record<string, ObjectId[]>;
  blocker_to_attacker: Record<string, ObjectId>;
  damage_assignments: Record<string, DamageAssignment[]>;
  first_strike_done: boolean;
}

// ── Stack ────────────────────────────────────────────────────────────────

export type StackEntryKind =
  | { type: "Spell"; data: { card_id: CardId; ability: unknown } }
  | { type: "ActivatedAbility"; data: { source_id: ObjectId; ability: unknown } }
  | { type: "TriggeredAbility"; data: { source_id: ObjectId; ability: unknown } };

export interface StackEntry {
  id: ObjectId;
  source_id: ObjectId;
  controller: PlayerId;
  kind: StackEntryKind;
}

// ── Pending Cast (for target selection) ──────────────────────────────────

export interface PendingCast {
  object_id: ObjectId;
  card_id: CardId;
  ability: unknown;
  cost: ManaCost;
}

// ── WaitingFor (discriminated union with tag="type", content="data") ─────

export type WaitingFor =
  | { type: "Priority"; data: { player: PlayerId } }
  | { type: "MulliganDecision"; data: { player: PlayerId; mulligan_count: number } }
  | { type: "MulliganBottomCards"; data: { player: PlayerId; count: number } }
  | { type: "ManaPayment"; data: { player: PlayerId } }
  | { type: "TargetSelection"; data: { player: PlayerId; pending_cast: PendingCast; legal_targets: TargetRef[] } }
  | { type: "DeclareAttackers"; data: { player: PlayerId; valid_attacker_ids: ObjectId[] } }
  | { type: "DeclareBlockers"; data: { player: PlayerId; valid_blocker_ids: ObjectId[]; valid_block_targets: Record<string, ObjectId[]> } }
  | { type: "GameOver"; data: { winner: PlayerId | null } }
  | { type: "ReplacementChoice"; data: { player: PlayerId; candidate_count: number } }
  | { type: "EquipTarget"; data: { player: PlayerId; equipment_id: ObjectId; valid_targets: ObjectId[] } }
  | { type: "ScryChoice"; data: { player: PlayerId; cards: ObjectId[] } }
  | { type: "DigChoice"; data: { player: PlayerId; cards: ObjectId[]; keep_count: number } }
  | { type: "SurveilChoice"; data: { player: PlayerId; cards: ObjectId[] } };

// ── Action Result ────────────────────────────────────────────────────────

export interface ActionResult {
  events: GameEvent[];
  waiting_for: WaitingFor;
}

// ── Game Actions (discriminated union, tag="type", content="data") ───────

export type GameAction =
  | { type: "PassPriority" }
  | { type: "PlayLand"; data: { card_id: CardId } }
  | { type: "CastSpell"; data: { card_id: CardId; targets: ObjectId[] } }
  | { type: "ActivateAbility"; data: { source_id: ObjectId; ability_index: number } }
  | { type: "DeclareAttackers"; data: { attacker_ids: ObjectId[] } }
  | { type: "DeclareBlockers"; data: { assignments: [ObjectId, ObjectId][] } }
  | { type: "MulliganDecision"; data: { keep: boolean } }
  | { type: "TapLandForMana"; data: { object_id: ObjectId } }
  | { type: "SelectCards"; data: { cards: ObjectId[] } }
  | { type: "SelectTargets"; data: { targets: TargetRef[] } }
  | { type: "ChooseReplacement"; data: { index: number } }
  | { type: "CancelCast" }
  | { type: "Equip"; data: { equipment_id: ObjectId; target_id: ObjectId } }
  | { type: "Transform"; data: { object_id: ObjectId } }
  | { type: "PlayFaceDown"; data: { card_id: CardId } }
  | { type: "TurnFaceUp"; data: { object_id: ObjectId } };

// ── Game Events (discriminated union, tag="type", content="data") ────────

export type GameEvent =
  | { type: "GameStarted" }
  | { type: "TurnStarted"; data: { player_id: PlayerId; turn_number: number } }
  | { type: "PhaseChanged"; data: { phase: Phase } }
  | { type: "PriorityPassed"; data: { player_id: PlayerId } }
  | { type: "SpellCast"; data: { card_id: CardId; controller: PlayerId } }
  | { type: "AbilityActivated"; data: { source_id: ObjectId } }
  | { type: "ZoneChanged"; data: { object_id: ObjectId; from: Zone; to: Zone } }
  | { type: "LifeChanged"; data: { player_id: PlayerId; amount: number } }
  | { type: "ManaAdded"; data: { player_id: PlayerId; mana_type: ManaType; source_id: ObjectId } }
  | { type: "PermanentTapped"; data: { object_id: ObjectId } }
  | { type: "PlayerLost"; data: { player_id: PlayerId } }
  | { type: "MulliganStarted" }
  | { type: "CardsDrawn"; data: { player_id: PlayerId; count: number } }
  | { type: "CardDrawn"; data: { player_id: PlayerId; object_id: ObjectId } }
  | { type: "PermanentUntapped"; data: { object_id: ObjectId } }
  | { type: "LandPlayed"; data: { object_id: ObjectId; player_id: PlayerId } }
  | { type: "StackPushed"; data: { object_id: ObjectId } }
  | { type: "StackResolved"; data: { object_id: ObjectId } }
  | { type: "Discarded"; data: { player_id: PlayerId; object_id: ObjectId } }
  | { type: "DamageCleared"; data: { object_id: ObjectId } }
  | { type: "GameOver"; data: { winner: PlayerId | null } }
  | { type: "DamageDealt"; data: { source_id: ObjectId; target: TargetRef; amount: number } }
  | { type: "SpellCountered"; data: { object_id: ObjectId; countered_by: ObjectId } }
  | { type: "CounterAdded"; data: { object_id: ObjectId; counter_type: string; count: number } }
  | { type: "CounterRemoved"; data: { object_id: ObjectId; counter_type: string; count: number } }
  | { type: "TokenCreated"; data: { object_id: ObjectId; name: string } }
  | { type: "CreatureDestroyed"; data: { object_id: ObjectId } }
  | { type: "PermanentSacrificed"; data: { object_id: ObjectId; player_id: PlayerId } }
  | { type: "EffectResolved"; data: { api_type: string; source_id: ObjectId } }
  | { type: "AttackersDeclared"; data: { attacker_ids: ObjectId[]; defending_player: PlayerId } }
  | { type: "BlockersDeclared"; data: { assignments: [ObjectId, ObjectId][] } }
  | { type: "BecomesTarget"; data: { object_id: ObjectId; source_id: ObjectId } }
  | { type: "ReplacementApplied"; data: { source_id: ObjectId; event_type: string } }
  | { type: "Transformed"; data: { object_id: ObjectId } }
  | { type: "DayNightChanged"; data: { new_state: string } }
  | { type: "TurnedFaceUp"; data: { object_id: ObjectId } };

// ── Game State ───────────────────────────────────────────────────────────

export interface GameState {
  turn_number: number;
  active_player: PlayerId;
  phase: Phase;
  players: Player[];
  priority_player: PlayerId;
  objects: Record<string, GameObject>;
  next_object_id: number;
  battlefield: ObjectId[];
  stack: StackEntry[];
  exile: ObjectId[];
  rng_seed: number;
  combat: CombatState | null;
  waiting_for: WaitingFor;
  lands_played_this_turn: number;
  max_lands_per_turn: number;
  priority_pass_count: number;
  pending_replacement: unknown | null;
  layers_dirty: boolean;
  next_timestamp: number;
}

// ── Adapter Interface ────────────────────────────────────────────────────

/**
 * Error type for adapter operations. Wraps WASM/transport errors
 * with structured metadata for error handling in the UI layer.
 */
export class AdapterError extends Error {
  readonly code: string;
  readonly recoverable: boolean;

  constructor(code: string, message: string, recoverable: boolean) {
    super(message);
    this.name = "AdapterError";
    this.code = code;
    this.recoverable = recoverable;
  }
}

/** Error codes for AdapterError */
export const AdapterErrorCode = {
  NOT_INITIALIZED: "NOT_INITIALIZED",
  WASM_ERROR: "WASM_ERROR",
  INVALID_ACTION: "INVALID_ACTION",
} as const;

/**
 * Transport-agnostic interface for communicating with the game engine.
 * Phase 1: WasmAdapter (direct WASM calls)
 * Phase 7: TauriAdapter (IPC to native Rust process)
 */
export interface EngineAdapter {
  initialize(): Promise<void>;
  initializeGame(deckData?: unknown): Promise<GameEvent[]> | GameEvent[];
  submitAction(action: GameAction): Promise<GameEvent[]>;
  getState(): Promise<GameState>;
  getLegalActions(): Promise<GameAction[]>;
  getAiAction(difficulty: string): Promise<GameAction | null> | GameAction | null;
  restoreState(state: GameState): void;
  dispose(): void;
}
