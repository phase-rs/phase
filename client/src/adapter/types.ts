// ── Identifiers ──────────────────────────────────────────────────────────

export type ObjectId = number;
export type CardId = number;
export type PlayerId = number;

// ── Game Format ─────────────────────────────────────────────────────────

export type GameFormat = "Standard" | "Commander" | "Pioneer" | "Historic" | "Pauper" | "Brawl" | "HistoricBrawl" | "FreeForAll" | "TwoHeadedGiant";

export interface FormatConfig {
  format: GameFormat;
  starting_life: number;
  min_players: number;
  max_players: number;
  deck_size: number;
  singleton: boolean;
  command_zone: boolean;
  commander_damage_threshold: number | null;
  range_of_influence: number | null;
  team_based: boolean;
}

// ── Match / Series ───────────────────────────────────────────────────────

export type MatchType = "Bo1" | "Bo3";
export type MatchPhase = "InGame" | "BetweenGames" | "Completed";

export interface MatchConfig {
  match_type: MatchType;
}

export interface MatchScore {
  p0_wins: number;
  p1_wins: number;
  draws: number;
}

export interface DeckCardCount {
  name: string;
  count: number;
}

export interface DeckPoolEntry {
  card: {
    name: string;
  };
  count: number;
}

// ── Attack Target ───────────────────────────────────────────────────────

export type AttackTarget =
  | { type: "Player"; data: PlayerId }
  | { type: "Planeswalker"; data: ObjectId };

// ── Commander Damage ────────────────────────────────────────────────────

export interface CommanderDamageEntry {
  player: PlayerId;
  commander: ObjectId;
  damage: number;
}

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
  | { type: "Cost"; shards: string[]; generic: number }
  | { type: "SelfManaCost" };

export type UnlessCost =
  | { type: "Fixed"; cost: ManaCost }
  | { type: "DynamicGeneric"; quantity: unknown }
  | { type: "PayLife"; amount: number }
  | { type: "DiscardCard" }
  | { type: "SacrificeAPermanent" };

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
  | "Lore"
  | "Stun"
  | { Generic: string };

// ── Keywords ─────────────────────────────────────────────────────────────

/**
 * Keyword type matching the Rust Keyword enum's serde output.
 * Simple keywords serialize as strings (e.g. "Flying").
 * Parameterized keywords serialize as objects (e.g. { Equip: { Cost: ... } }).
 */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type Keyword = string | Record<string, any>;

// ── Casting Permission ───────────────────────────────────────────────────

export type CastingPermission =
  | { type: "AdventureCreature" }
  | { type: "ExileWithAltCost"; cost: ManaCost }
  | { type: "PlayFromExile"; duration: string }
  | { type: "ExileWithEnergyCost" }
  | { type: "WarpExile"; castable_after_turn: number };

// ── Game Restriction ────────────────────────────────────────────────────

export type RestrictionExpiry = { type: "EndOfTurn" } | { type: "EndOfCombat" };

export type RestrictionScope =
  | { type: "SourcesControlledBy"; data: PlayerId }
  | { type: "SpecificSource"; data: ObjectId }
  | { type: "DamageToTarget"; data: ObjectId };

export type GameRestriction = {
  type: "DamagePreventionDisabled";
  source: ObjectId;
  expiry: RestrictionExpiry;
  scope?: RestrictionScope | null;
};

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
  keywords: Keyword[];
  abilities: unknown[];
  trigger_definitions: unknown[];
  replacement_definitions: unknown[];
  static_definitions: unknown[];
  color: ManaColor[];
  base_power: number | null;
  base_toughness: number | null;
  base_keywords: Keyword[];
  base_color: ManaColor[];
  timestamp: number;
  entered_battlefield_turn: number | null;
  unimplemented_mechanics?: string[];
  has_summoning_sickness?: boolean;
  has_mana_ability?: boolean;
  mana_ability_index?: number;
  is_suspected?: boolean;
  case_state?: { is_solved: boolean; solve_condition: unknown } | null;
  class_level?: number;
  devotion?: number;
  available_mana_colors?: ManaColor[];
  casting_permissions?: CastingPermission[];
  is_emblem?: boolean;
  is_commander?: boolean;
  commander_tax?: number;
  back_face?: {
    name: string;
    power: number | null;
    toughness: number | null;
    card_types: CardType;
    mana_cost: ManaCost;
    keywords: Keyword[];
    abilities: unknown[];
    color: ManaColor[];
  } | null;
}

// ── Companion ────────────────────────────────────────────────────────────

/** Partial typing of engine CardFace — only fields the frontend currently reads. */
export interface CardFacePartial {
  name: string;
}

export interface CompanionInfo {
  card: { card: CardFacePartial; count: number };
  used: boolean;
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
  is_eliminated?: boolean;
  companion?: CompanionInfo;
  /** CR 122.1: Player's energy counter total. */
  energy?: number;
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

// ── Resolved Ability (structural type for stack/pending cast abilities) ──

export interface ResolvedAbility {
  targets: TargetRef[];
  sub_ability?: ResolvedAbility;
}

// ── Stack ────────────────────────────────────────────────────────────────

export type StackEntryKind =
  | { type: "Spell"; data: { card_id: CardId; ability: ResolvedAbility } }
  | { type: "ActivatedAbility"; data: { source_id: ObjectId; ability: ResolvedAbility } }
  | { type: "TriggeredAbility"; data: { source_id: ObjectId; ability: ResolvedAbility; description?: string } };

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
  ability: ResolvedAbility;
  cost: ManaCost;
  activation_cost?: SerializedAbilityCost;
  activation_ability_index?: number;
  target_constraints?: Array<{ type: string }>;
}

export interface TargetSelectionSlot {
  legal_targets: TargetRef[];
  optional?: boolean;
}

export interface TargetSelectionProgress {
  current_slot: number;
  selected_slots?: Array<TargetRef | null>;
  current_legal_targets: TargetRef[];
}

export type TargetSelectionConstraint =
  | { type: "DifferentTargetPlayers" };

// ── Additional Costs (kicker, blight, "or pay") ─────────────────────────

export type AdditionalCost =
  | { type: "Optional"; data: SerializedAbilityCost }
  | { type: "Choice"; data: [SerializedAbilityCost, SerializedAbilityCost] };

/** Mirrors Rust AbilityCost serialization (serde tag = "type"). */
export type SerializedAbilityCost = { type: string; [key: string]: unknown };

// ── Modal Choice metadata ─────────────────────────────────────────────

export interface ModalChoice {
  min_choices: number;
  max_choices: number;
  mode_count: number;
  mode_descriptions: string[];
  allow_repeat_modes: boolean;
  /** Per-mode additional mana costs (Spree). Empty/absent for standard modal spells. */
  mode_costs?: ManaCost[];
  constraints?: Array<{ type: string }>;
}

// ── WaitingFor (discriminated union with tag="type", content="data") ─────

export type WaitingFor =
  | { type: "Priority"; data: { player: PlayerId } }
  | { type: "MulliganDecision"; data: { player: PlayerId; mulligan_count: number } }
  | { type: "MulliganBottomCards"; data: { player: PlayerId; count: number } }
  | { type: "ManaPayment"; data: { player: PlayerId } }
  | { type: "TargetSelection"; data: { player: PlayerId; pending_cast: PendingCast; target_slots: TargetSelectionSlot[]; selection: TargetSelectionProgress } }
  | { type: "DeclareAttackers"; data: { player: PlayerId; valid_attacker_ids: ObjectId[]; valid_attack_targets?: AttackTarget[] } }
  | { type: "DeclareBlockers"; data: { player: PlayerId; valid_blocker_ids: ObjectId[]; valid_block_targets: Record<string, ObjectId[]> } }
  | { type: "GameOver"; data: { winner: PlayerId | null } }
  | { type: "ReplacementChoice"; data: { player: PlayerId; candidate_count: number; candidate_descriptions?: string[] } }
  | { type: "CopyTargetChoice"; data: { player: PlayerId; source_id: ObjectId; valid_targets: ObjectId[] } }
  | { type: "EquipTarget"; data: { player: PlayerId; equipment_id: ObjectId; valid_targets: ObjectId[] } }
  | { type: "ScryChoice"; data: { player: PlayerId; cards: ObjectId[] } }
  | { type: "DigChoice"; data: { player: PlayerId; cards: ObjectId[]; keep_count: number; up_to?: boolean; selectable_cards?: ObjectId[]; kept_destination?: Zone | null; rest_destination?: Zone | null } }
  | { type: "SurveilChoice"; data: { player: PlayerId; cards: ObjectId[] } }
  | { type: "RevealChoice"; data: { player: PlayerId; cards: ObjectId[]; filter: unknown } }
  | { type: "SearchChoice"; data: { player: PlayerId; cards: ObjectId[]; count: number } }
  | { type: "TriggerTargetSelection"; data: { player: PlayerId; target_slots: TargetSelectionSlot[]; target_constraints?: TargetSelectionConstraint[]; selection: TargetSelectionProgress; source_id?: ObjectId; description?: string } }
  | { type: "BetweenGamesSideboard"; data: { player: PlayerId; game_number: number; score: MatchScore } }
  | { type: "BetweenGamesChoosePlayDraw"; data: { player: PlayerId; game_number: number; score: MatchScore } }
  | { type: "NamedChoice"; data: { player: PlayerId; choice_type: string | Record<string, unknown>; options: string[]; source_id?: ObjectId } }
  | { type: "ModeChoice"; data: { player: PlayerId; modal: ModalChoice; pending_cast: PendingCast } }
  | { type: "AbilityModeChoice"; data: { player: PlayerId; modal: ModalChoice; source_id: ObjectId; mode_abilities: unknown[]; is_activated: boolean; ability_index?: number; ability_cost?: unknown; unavailable_modes?: number[] } }
  | { type: "DiscardToHandSize"; data: { player: PlayerId; count: number; cards: ObjectId[] } }
  | { type: "OptionalCostChoice"; data: { player: PlayerId; cost: AdditionalCost; pending_cast: PendingCast } }
  | { type: "AdventureCastChoice"; data: { player: PlayerId; object_id: ObjectId; card_id: CardId } }
  | { type: "WarpCostChoice"; data: { player: PlayerId; object_id: ObjectId; card_id: CardId; normal_cost: ManaCost; warp_cost: ManaCost } }
  | { type: "DiscardForCost"; data: { player: PlayerId; count: number; cards: ObjectId[]; pending_cast: PendingCast } }
  | { type: "SacrificeForCost"; data: { player: PlayerId; count: number; permanents: ObjectId[]; pending_cast: PendingCast } }
  | { type: "ExileFromGraveyardForCost"; data: { player: PlayerId; count: number; cards: ObjectId[]; pending_cast: PendingCast } }
  | { type: "HarmonizeTapChoice"; data: { player: PlayerId; eligible_creatures: ObjectId[]; pending_cast: PendingCast } }
  | { type: "OptionalEffectChoice"; data: { player: PlayerId; source_id: ObjectId; description?: string } }
  | { type: "OpponentMayChoice"; data: { player: PlayerId; source_id: ObjectId; description?: string; remaining: PlayerId[] } }
  | { type: "UnlessPayment"; data: { player: PlayerId; cost: UnlessCost; pending_effect: unknown; effect_description?: string } }
  | { type: "WardDiscardChoice"; data: { player: PlayerId; cards: ObjectId[]; pending_effect: unknown } }
  | { type: "WardSacrificeChoice"; data: { player: PlayerId; permanents: ObjectId[]; pending_effect: unknown } }
  | { type: "ChooseRingBearer"; data: { player: PlayerId; candidates: ObjectId[] } }
  | { type: "DiscoverChoice"; data: { player: PlayerId; hit_card: ObjectId; exiled_misses: ObjectId[] } }
  | { type: "TopOrBottomChoice"; data: { player: PlayerId; object_id: ObjectId } }
  | { type: "CompanionReveal"; data: { player: PlayerId; eligible_companions: [string, number][] } }
  | { type: "ChooseLegend"; data: { player: PlayerId; legend_name: string; candidates: ObjectId[] } }
  | { type: "DistributeAmong"; data: { player: PlayerId; total: number; targets: TargetRef[]; unit: DistributionUnit } }
  | { type: "ChooseFromZoneChoice"; data: { player: PlayerId; cards: ObjectId[]; count: number; source_id: ObjectId } }
  | { type: "RetargetChoice"; data: { player: PlayerId; stack_entry_index: number; scope: RetargetScope; current_targets: TargetRef[]; legal_new_targets: TargetRef[] } }
  | { type: "ConniveDiscard"; data: { player: PlayerId; conniver_id: ObjectId; source_id: ObjectId; cards: ObjectId[]; count: number } }
  | { type: "ManifestDreadChoice"; data: { player: PlayerId; cards: ObjectId[] } };

// ── Distribution ─────────────────────────────────────────────────────────

export type DistributionUnit =
  | { type: "Damage" }
  | { type: "EvenSplitDamage" }
  | { type: "Counters"; data: string }
  | { type: "Life" };

// ── Retarget Scope ───────────────────────────────────────────────────────

export type RetargetScope =
  | { type: "Single" }
  | { type: "All" }
  | { type: "ForcedTo"; data: TargetRef };

// ── Log Types ────────────────────────────────────────────────────────────

export type LogCategory =
  | "Game" | "Turn" | "Stack" | "Combat" | "Zone" | "Life"
  | "Mana" | "State" | "Token" | "Trigger" | "Special" | "Destroy";

export type LogSegment =
  | { type: "Text"; value: string }
  | { type: "CardName"; value: { name: string; object_id: ObjectId } }
  | { type: "PlayerName"; value: { name: string; player_id: PlayerId } }
  | { type: "Number"; value: number }
  | { type: "Mana"; value: string }
  | { type: "Zone"; value: Zone }
  | { type: "Keyword"; value: string };

export interface GameLogEntry {
  seq: number;
  turn: number;
  phase: Phase;
  category: LogCategory;
  segments: LogSegment[];
}

// ── Action Result ────────────────────────────────────────────────────────

export interface ActionResult {
  events: GameEvent[];
  waiting_for: WaitingFor;
  log_entries?: GameLogEntry[];
}

// ── Game Actions (discriminated union, tag="type", content="data") ───────

export type GameAction =
  | { type: "PassPriority" }
  | { type: "PlayLand"; data: { object_id: ObjectId; card_id: CardId } }
  | { type: "CastSpell"; data: { object_id: ObjectId; card_id: CardId; targets: ObjectId[] } }
  | { type: "ActivateAbility"; data: { source_id: ObjectId; ability_index: number } }
  | { type: "DeclareAttackers"; data: { attacks: [ObjectId, AttackTarget][] } }
  | { type: "DeclareBlockers"; data: { assignments: [ObjectId, ObjectId][] } }
  | { type: "MulliganDecision"; data: { keep: boolean } }
  | { type: "TapLandForMana"; data: { object_id: ObjectId } }
  | { type: "UntapLandForMana"; data: { object_id: ObjectId } }
  | { type: "SelectCards"; data: { cards: ObjectId[] } }
  | { type: "SelectTargets"; data: { targets: TargetRef[] } }
  | { type: "ChooseTarget"; data: { target: TargetRef | null } }
  | { type: "ChooseReplacement"; data: { index: number } }
  | { type: "CancelCast" }
  | { type: "Equip"; data: { equipment_id: ObjectId; target_id: ObjectId } }
  | { type: "Transform"; data: { object_id: ObjectId } }
  | { type: "PlayFaceDown"; data: { object_id: ObjectId; card_id: CardId } }
  | { type: "TurnFaceUp"; data: { object_id: ObjectId } }
  | { type: "SubmitSideboard"; data: { main: DeckCardCount[]; sideboard: DeckCardCount[] } }
  | { type: "ChoosePlayDraw"; data: { play_first: boolean } }
  | { type: "ChooseOption"; data: { choice: string } }
  | { type: "SelectModes"; data: { indices: number[] } }
  | { type: "DecideOptionalCost"; data: { pay: boolean } }
  | { type: "ChooseAdventureFace"; data: { creature: boolean } }
  | { type: "ChooseWarpCost"; data: { use_warp: boolean } }
  | { type: "ActivateNinjutsu"; data: { ninjutsu_card_id: CardId; creature_to_return: ObjectId } }
  | { type: "DecideOptionalEffect"; data: { accept: boolean } }
  | { type: "PayUnlessCost"; data: { pay: boolean } }
  | { type: "ChooseRingBearer"; data: { target: ObjectId } }
  | { type: "ChooseLegend"; data: { keep: ObjectId } }
  | { type: "HarmonizeTap"; data: { creature_id: ObjectId | null } }
  | { type: "DeclareCompanion"; data: { card_index: number | null } }
  | { type: "CompanionToHand" }
  | { type: "DiscoverChoice"; data: { cast: boolean } }
  | { type: "ChooseTopOrBottom"; data: { top: boolean } }
  | { type: "SetAutoPass"; data: { mode: { type: "UntilStackEmpty" } | { type: "UntilEndOfTurn" } } }
  | { type: "CancelAutoPass" }
  | { type: "DistributeAmong"; data: { distribution: [TargetRef, number][] } }
  | { type: "RetargetSpell"; data: { new_targets: TargetRef[] } };

// ── Game Events (discriminated union, tag="type", content="data") ────────

export type GameEvent =
  | { type: "GameStarted" }
  | { type: "TurnStarted"; data: { player_id: PlayerId; turn_number: number } }
  | { type: "PhaseChanged"; data: { phase: Phase } }
  | { type: "PriorityPassed"; data: { player_id: PlayerId } }
  | { type: "SpellCast"; data: { card_id: CardId; controller: PlayerId; object_id: ObjectId } }
  | { type: "AbilityActivated"; data: { source_id: ObjectId } }
  | { type: "ZoneChanged"; data: { object_id: ObjectId; from: Zone; to: Zone } }
  | { type: "LifeChanged"; data: { player_id: PlayerId; amount: number } }
  | { type: "ManaAdded"; data: { player_id: PlayerId; mana_type: ManaType; source_id: ObjectId; tapped_for_mana?: boolean } }
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
  | { type: "DamageDealt"; data: { source_id: ObjectId; target: TargetRef; amount: number; is_combat: boolean } }
  | { type: "SpellCountered"; data: { object_id: ObjectId; countered_by: ObjectId } }
  | { type: "CounterAdded"; data: { object_id: ObjectId; counter_type: string; count: number } }
  | { type: "CounterRemoved"; data: { object_id: ObjectId; counter_type: string; count: number } }
  | { type: "TokenCreated"; data: { object_id: ObjectId; name: string } }
  | { type: "CreatureDestroyed"; data: { object_id: ObjectId } }
  | { type: "PermanentSacrificed"; data: { object_id: ObjectId; player_id: PlayerId } }
  | { type: "EffectResolved"; data: { kind: string; source_id: ObjectId } }
  | { type: "AttackersDeclared"; data: { attacker_ids: ObjectId[]; defending_player: PlayerId } }
  | { type: "BlockersDeclared"; data: { assignments: [ObjectId, ObjectId][] } }
  | { type: "BecomesTarget"; data: { object_id: ObjectId; source_id: ObjectId } }
  | { type: "ReplacementApplied"; data: { source_id: ObjectId; event_type: string } }
  | { type: "Transformed"; data: { object_id: ObjectId } }
  | { type: "DayNightChanged"; data: { new_state: string } }
  | { type: "TurnedFaceUp"; data: { object_id: ObjectId } }
  | { type: "CardsRevealed"; data: { player: PlayerId; card_ids?: ObjectId[]; card_names: string[] } }
  | { type: "Regenerated"; data: { object_id: ObjectId } }
  | { type: "CreatureSuspected"; data: { object_id: ObjectId } }
  | { type: "CaseSolved"; data: { object_id: ObjectId } }
  | { type: "ClassLevelGained"; data: { object_id: ObjectId; level: number } }
  | { type: "RingTemptsYou"; data: { player_id: PlayerId } }
  | { type: "CompanionRevealed"; data: { player: PlayerId; card_name: string } }
  | { type: "CompanionMovedToHand"; data: { player: PlayerId; card_name: string } }
  | { type: "EnergyChanged"; data: { player: PlayerId; delta: number } }
  | { type: "CreatureExploited"; data: { exploiter: ObjectId; sacrificed: ObjectId } }
  | { type: "PowerToughnessChanged"; data: { object_id: ObjectId; power: number; toughness: number; power_delta: number; toughness_delta: number } };

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
  seat_order?: PlayerId[];
  format_config?: FormatConfig;
  eliminated_players?: PlayerId[];
  commander_damage?: CommanderDamageEntry[];
  exile_links?: Array<{ exiled_id: ObjectId; source_id: ObjectId }>;
  match_config?: MatchConfig;
  match_phase?: MatchPhase;
  match_score?: MatchScore;
  game_number?: number;
  current_starting_player?: PlayerId;
  next_game_chooser?: PlayerId | null;
  deck_pools?: Array<{
    player: PlayerId;
    registered_main: DeckPoolEntry[];
    registered_sideboard: DeckPoolEntry[];
    current_main: DeckPoolEntry[];
    current_sideboard: DeckPoolEntry[];
  }>;
  sideboard_submitted?: PlayerId[];
  revealed_cards?: ObjectId[];
  restrictions?: GameRestriction[];
  command_zone?: ObjectId[];
  auto_pass?: Record<number, AutoPassMode>;
  lands_tapped_for_mana?: Record<number, number[]>;
}

export type AutoPassMode =
  | { type: "UntilStackEmpty"; initial_stack_len: number }
  | { type: "UntilEndOfTurn" };

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
export interface SubmitResult {
  events: GameEvent[];
  log_entries?: GameLogEntry[];
}

export interface EngineAdapter {
  initialize(): Promise<void>;
  initializeGame(
    deckData?: unknown,
    formatConfig?: FormatConfig,
    playerCount?: number,
    matchConfig?: MatchConfig,
  ): Promise<SubmitResult> | SubmitResult;
  submitAction(action: GameAction): Promise<SubmitResult>;
  getState(): Promise<GameState>;
  getLegalActions(): Promise<GameAction[]>;
  getAiAction(difficulty: string, playerId?: number): Promise<GameAction | null> | GameAction | null;
  restoreState(state: GameState): void;
  dispose(): void;
}
