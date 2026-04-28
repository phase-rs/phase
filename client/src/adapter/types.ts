// ── Identifiers ──────────────────────────────────────────────────────────

export type ObjectId = number;
export type CardId = number;
export type PlayerId = number;

// ── Dungeon ─────────────────────────────────────────────────────────────

export type DungeonId =
  | "LostMineOfPhandelver"
  | "DungeonOfTheMadMage"
  | "TombOfAnnihilation"
  | "Undercity"
  | "BaldursGateWilderness";

// ── Game Format ─────────────────────────────────────────────────────────

export type GameFormat =
  | "Standard"
  | "Commander"
  | "Pioneer"
  | "Modern"
  | "Legacy"
  | "Vintage"
  | "Historic"
  | "Timeless"
  | "Pauper"
  | "PauperCommander"
  | "DuelCommander"
  | "Brawl"
  | "HistoricBrawl"
  | "FreeForAll"
  | "TwoHeadedGiant";

export type FormatGroup = "Constructed" | "Commander" | "Multiplayer";

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

/**
 * Authoritative per-format metadata produced by the engine's
 * `get_format_registry` WASM export. Adding a format is a single engine-side
 * edit; frontend components consume this list rather than maintaining parallel
 * format tables.
 */
export interface FormatMetadata {
  format: GameFormat;
  label: string;
  short_label: string;
  description: string;
  group: FormatGroup;
  default_config: FormatConfig;
}

// ── Lobby ────────────────────────────────────────────────────────────────

/**
 * Wire-level lobby row as broadcast by `phase-server`. Field names are
 * snake_case to match the Rust `LobbyGame` struct exactly — see
 * `crates/server-core/src/protocol.rs`.
 */
export interface LobbyGame {
  game_code: string;
  host_name: string;
  created_at: number;
  has_password: boolean;
  format?: GameFormat;
  current_players?: number;
  max_players?: number;
  /** Display-only version string (e.g. "0.1.11"). */
  host_version?: string;
  /**
   * Git short-hash of the host's build. Used as a hard compatibility gate:
   * when the lobby list renders, rows whose commit doesn't match the
   * client's own build are disabled because the host and guest would run
   * diverged engine rules otherwise.
   */
  host_build_commit?: string;
  /** Optional host-provided label for this room, distinct from their player
   * name. When present, the lobby row shows it as the primary title with
   * the host's player name as secondary metadata. */
  room_name?: string | null;
  /**
   * `true` when the row represents a P2P-brokered room (host runs the
   * engine; guests dial the host). `false`/absent for server-run rooms.
   * Always compare with `=== true` — an older `phase-server` build omits
   * the field entirely, so treating `undefined` as falsy is what we want.
   */
  is_p2p?: boolean;
}

/**
 * Broker response to `JoinGameWithPassword` on a `LobbyOnly` server. Gives
 * the guest everything they need to dial the host over PeerJS plus the
 * format and match config so the pre-flight can refuse to dial a room
 * with an incompatible format.
 */
export interface PeerInfo {
  game_code: string;
  host_peer_id: string;
  format_config?: FormatConfig | null;
  match_config: MatchConfig;
  player_count: number;
  filled_seats: number;
}

/**
 * Read-only join-target lookup returned before deck selection. Lets the
 * client discover format and whether the code targets a brokered P2P room
 * without consuming a seat.
 */
export interface JoinTargetInfo {
  game_code: string;
  is_p2p: boolean;
  format_config?: FormatConfig | null;
  match_config: MatchConfig;
  player_count: number;
  filled_seats: number;
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
  | { type: "Planeswalker"; data: ObjectId }
  | { type: "Battle"; data: ObjectId };

// CR 702.19: Which trample variant applies to combat damage assignment.
export type TrampleKind = "Standard" | "OverPlaneswalkers";

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

// Narrow source-zone type for `WaitingFor::ExileForCost` — only `Hand` (pitch
// spells) and `Graveyard` (escape) are valid (mirrors the engine's
// `ExileCostSourceZone`).
export type ExileCostSourceZone = "Hand" | "Graveyard";

export type ManaColor = "White" | "Blue" | "Black" | "Red" | "Green";

export type ManaType = "White" | "Blue" | "Black" | "Red" | "Green" | "Colorless";

/**
 * Display-layer projection of the engine's `ManaProduction` enum. One variant
 * per producer shape so colorless and commander-identity producers reach the
 * frontend with full fidelity (the previous `ManaColor[]` shape silently
 * dropped both classes). Engine-derived; the frontend renders pips verbatim.
 *
 * - `Color` — a specific WUBRG color (CR 106.1a).
 * - `Colorless` — colorless `{C}` (CR 106.1b). War Room, Wastes.
 * - `OneOfColors` — controller picks one color from the listed set per
 *   activation (CR 106.4). City of Brass, Mana Confluence.
 * - `CombinationOfColors` — controller assigns each unit independently across
 *   the listed set (CR 106.4). Cascading Cataracts.
 * - `AnyInCommandersIdentity` — Command Tower / Path of Ancestry. Resolve the
 *   pip set against the controller's `commander_color_identity` (CR 903.4).
 */
export type ManaPip =
  | { type: "Color"; data: ManaColor }
  | { type: "Colorless" }
  | { type: "OneOfColors"; data: ManaColor[] }
  | { type: "CombinationOfColors"; data: ManaColor[] }
  | { type: "AnyInCommandersIdentity" };

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
  | { type: "Sacrifice"; count: number; filter: TargetFilter };

// ── Card Types ───────────────────────────────────────────────────────────

export interface CardType {
  supertypes: string[];
  core_types: string[];
  subtypes: string[];
}

// ── Counter Types ────────────────────────────────────────────────────────

/**
 * Counter type keys matching the Rust CounterType serde output.
 * These are the exact strings used as keys in `obj.counters`.
 */
export type CounterType =
  | "P1P1"
  | "M1M1"
  | "loyalty"
  | "lore"
  | "stun"
  | (string & {});

// ── Keywords ─────────────────────────────────────────────────────────────

/**
 * Keyword type matching the Rust Keyword enum's serde output.
 * Simple keywords serialize as strings (e.g. "Flying").
 * Parameterized keywords serialize as objects (e.g. { Equip: { Cost: ... } }).
 */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type Keyword = string | Record<string, any>;

// ── CR 701.57a + CR 702.85a: Cast/decline choice for Discover and Cascade ──

export type CastChoice = { type: "Cast" } | { type: "Decline" };

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

export interface SerializedManaProduction {
  type: string;
  colors?: string[];
  [key: string]: unknown;
}

export interface SerializedAbilityEffect {
  type?: string;
  produced?: SerializedManaProduction;
  [key: string]: unknown;
}

export interface SerializedAbility {
  cost?: SerializedAbilityCost;
  effect?: SerializedAbilityEffect;
  description?: string;
  [key: string]: unknown;
}

export type ChooseFromZoneConstraint =
  | { type: "DistinctCardTypes"; categories: string[] };

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
  counters: Partial<Record<CounterType, number>>;
  name: string;
  power: number | null;
  toughness: number | null;
  loyalty: number | null;
  card_types: CardType;
  mana_cost: ManaCost;
  keywords: Keyword[];
  abilities: SerializedAbility[];
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
  available_mana_pips?: ManaPip[];
  casting_permissions?: CastingPermission[];
  is_emblem?: boolean;
  /**
   * Image-lookup routing hint from the engine. "Card" → look up the image
   * in the real-card database (default; also covers token-copies of real
   * cards like Twinflame/Helm of the Host). "Token" → look up the image
   * in Scryfall's generic-token database (Treasure, Spirit 1/1, etc.).
   * Independent of `is_token` (which is the CR 111.1 game-rules concept).
   */
  display_source?: "Card" | "Token";
  is_commander?: boolean;
  commander_tax?: number;
  /**
   * Stable identity of the printed card this object was instantiated from.
   * `oracle_id` is Scryfall's per-card identifier (shared across both faces
   * of a DFC/MDFC); `face_name` distinguishes which face the engine is
   * currently presenting. The frontend uses this pair as the canonical key
   * for image lookup — it sidesteps engine-vs-Scryfall front/back-face
   * naming asymmetry that would otherwise hide MDFCs played as their
   * Scryfall-back face. Optional because synthesized objects (emblems,
   * generic tokens) may not carry a printed identity.
   */
  printed_ref?: PrintedRef | null;
  back_face?: {
    name: string;
    power: number | null;
    toughness: number | null;
    card_types: CardType;
    mana_cost: ManaCost;
    keywords: Keyword[];
    abilities: SerializedAbility[];
    color: ManaColor[];
    printed_ref?: PrintedRef | null;
  } | null;
}

export interface PrintedRef {
  oracle_id: string;
  face_name: string;
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

/**
 * Player-level phasing status (mirrors Rust `PlayerStatus`).
 * Serde output: `{ "type": "Active" }` / `{ "type": "PhasedOut" }`.
 * While `PhasedOut`, the player is excluded from targeting/attack/damage/
 * SBA-loss filter choke points in the engine.
 */
export type PlayerStatus =
  | { type: "Active" }
  | { type: "PhasedOut" };

export interface Player {
  id: PlayerId;
  life: number;
  poison_counters: number;
  speed?: number | null;
  mana_pool: ManaPool;
  library: ObjectId[];
  hand: ObjectId[];
  graveyard: ObjectId[];
  has_drawn_this_turn: boolean;
  lands_played_this_turn: number;
  /** CR 500: per-player turn count, excluding skipped turns. */
  turns_taken: number;
  can_look_at_top_of_library?: boolean;
  is_eliminated?: boolean;
  companion?: CompanionInfo;
  /** CR 122.1: Player's energy counter total. */
  energy?: number;
  /**
   * Player phasing status (serde-default `Active` for replay compat).
   * When `PhasedOut`, the engine treats the player as excluded from
   * targeting, attacking, damage, and SBA-loss checks.
   */
  status?: PlayerStatus;
  /**
   * CR 903.4: Combined color identity of this player's commander(s).
   * Engine-derived; the frontend reads to render
   * `ManaPip.AnyInCommandersIdentity` pips. Empty when the player has no
   * commander or has only a colorless commander (CR 903.4f).
   */
  commander_color_identity?: ManaColor[];
}

// ── Target Filter ───────────────────────────────────────────────────────

/** Engine-side target filter (opaque — frontend only checks presence, never inspects). */
export type TargetFilter = Record<string, unknown>;

// ── Target Ref ───────────────────────────────────────────────────────────

export type TargetRef =
  | { Object: ObjectId }
  | { Player: PlayerId };

// ── Combat ───────────────────────────────────────────────────────────────

export interface AttackerInfo {
  object_id: ObjectId;
  defending_player: PlayerId;
  attack_target: AttackTarget;
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
  blocker_to_attacker: Record<string, ObjectId[]>;
  blockers_declared_by: PlayerId[];
  pending_blocker_declaration_events: GameEvent[];
  damage_assignments: Record<string, DamageAssignment[]>;
  first_strike_done: boolean;
  damage_step_index: number | null;
  pending_damage: [ObjectId, DamageAssignment][];
  regular_damage_done: boolean;
}

// ── Resolved Ability (structural type for stack/pending cast abilities) ──

export interface ResolvedAbility {
  targets: TargetRef[];
  sub_ability?: ResolvedAbility;
}

// ── Stack ────────────────────────────────────────────────────────────────

export type StackEntryKind =
  | { type: "Spell"; data: { card_id: CardId; ability?: ResolvedAbility; actual_mana_spent?: number } }
  | { type: "ActivatedAbility"; data: { source_id: ObjectId; ability: ResolvedAbility } }
  | { type: "TriggeredAbility"; data: { source_id: ObjectId; ability: ResolvedAbility; description?: string } };

export interface StackEntry {
  id: ObjectId;
  source_id: ObjectId;
  controller: PlayerId;
  kind: StackEntryKind;
}

/**
 * Engine-authored coalesced view of the stack. Adjacent entries with the
 * same source + kind + description + target signature collapse into one
 * group with a `×count` badge. Authoritative derivation lives in
 * `crates/engine/src/game/stack.rs::stack_display_groups`; the frontend
 * never re-implements the grouping rule.
 */
export interface StackDisplayGroup {
  representative: ObjectId;
  count: number;
  member_ids: ObjectId[];
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

// ── Combat Tax (CR 508.1d + 508.1h + 509.1c + 509.1d) ────────────────────

/** Which combat step a `WaitingFor::CombatTaxPayment` belongs to.
 * Serde output: `{ "type": "Attacking" }` / `{ "type": "Blocking" }`. */
export type CombatTaxContext =
  | { type: "Attacking" }
  | { type: "Blocking" };

/** The declaration paused awaiting a combat-tax decision. Serde
 * `tag = "type", content = "data"`. Rust tuples (ObjectId, AttackTarget)
 * and (ObjectId, ObjectId) serialize as JSON arrays. */
export type CombatTaxPending =
  | { type: "Attack"; data: { attacks: [ObjectId, AttackTarget][] } }
  | { type: "Block"; data: { assignments: [ObjectId, ObjectId][] } };

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
  | {
      type: "MulliganDecision";
      data: { player: PlayerId; mulligan_count: number; free_first_mulligan: boolean };
    }
  | { type: "MulliganBottomCards"; data: { player: PlayerId; count: number } }
  | { type: "ManaPayment"; data: { player: PlayerId } }
  | { type: "ChooseXValue"; data: { player: PlayerId; max: number; pending_cast: PendingCast } }
  | { type: "TargetSelection"; data: { player: PlayerId; pending_cast: PendingCast; target_slots: TargetSelectionSlot[]; selection: TargetSelectionProgress } }
  | { type: "DeclareAttackers"; data: { player: PlayerId; valid_attacker_ids: ObjectId[]; valid_attack_targets?: AttackTarget[] } }
  | { type: "DeclareBlockers"; data: { player: PlayerId; valid_blocker_ids: ObjectId[]; valid_block_targets: Record<string, ObjectId[]> } }
  | { type: "GameOver"; data: { winner: PlayerId | null } }
  | { type: "ReplacementChoice"; data: { player: PlayerId; candidate_count: number; candidate_descriptions?: string[] } }
  | { type: "CopyTargetChoice"; data: { player: PlayerId; source_id: ObjectId; valid_targets: ObjectId[]; max_mana_value?: number | null } }
  | { type: "ExploreChoice"; data: { player: PlayerId; source_id: ObjectId; choosable: ObjectId[]; remaining: ObjectId[]; pending_effect: unknown } }
  | { type: "EquipTarget"; data: { player: PlayerId; equipment_id: ObjectId; valid_targets: ObjectId[] } }
  | { type: "CrewVehicle"; data: { player: PlayerId; vehicle_id: ObjectId; crew_power: number; eligible_creatures: ObjectId[] } }
  | { type: "StationTarget"; data: { player: PlayerId; spacecraft_id: ObjectId; eligible_creatures: ObjectId[] } }
  | { type: "SaddleMount"; data: { player: PlayerId; mount_id: ObjectId; saddle_power: number; eligible_creatures: ObjectId[] } }
  | { type: "ScryChoice"; data: { player: PlayerId; cards: ObjectId[] } }
  | { type: "DigChoice"; data: { player: PlayerId; cards: ObjectId[]; keep_count: number; up_to?: boolean; selectable_cards?: ObjectId[]; kept_destination?: Zone | null; rest_destination?: Zone | null } }
  | { type: "SurveilChoice"; data: { player: PlayerId; cards: ObjectId[] } }
  | { type: "RevealChoice"; data: { player: PlayerId; cards: ObjectId[]; filter: unknown; optional?: boolean } }
  | { type: "SearchChoice"; data: { player: PlayerId; cards: ObjectId[]; count: number; reveal?: boolean } }
  | { type: "TriggerTargetSelection"; data: { player: PlayerId; target_slots: TargetSelectionSlot[]; target_constraints?: TargetSelectionConstraint[]; selection: TargetSelectionProgress; source_id?: ObjectId; description?: string } }
  | { type: "BetweenGamesSideboard"; data: { player: PlayerId; game_number: number; score: MatchScore } }
  | { type: "BetweenGamesChoosePlayDraw"; data: { player: PlayerId; game_number: number; score: MatchScore } }
  | { type: "NamedChoice"; data: { player: PlayerId; choice_type: string | Record<string, unknown>; options: string[]; source_id?: ObjectId } }
  | { type: "ModeChoice"; data: { player: PlayerId; modal: ModalChoice; pending_cast: PendingCast } }
  | { type: "AbilityModeChoice"; data: { player: PlayerId; modal: ModalChoice; source_id: ObjectId; mode_abilities: unknown[]; is_activated: boolean; ability_index?: number; ability_cost?: unknown; unavailable_modes?: number[] } }
  | { type: "DiscardToHandSize"; data: { player: PlayerId; count: number; cards: ObjectId[] } }
  | { type: "OptionalCostChoice"; data: { player: PlayerId; cost: AdditionalCost; pending_cast: PendingCast } }
  | { type: "DefilerPayment"; data: { player: PlayerId; life_cost: number; mana_reduction: ManaCost; pending_cast: PendingCast } }
  | { type: "AdventureCastChoice"; data: { player: PlayerId; object_id: ObjectId; card_id: CardId } }
  | { type: "ModalFaceChoice"; data: { player: PlayerId; object_id: ObjectId; card_id: CardId } }
  | { type: "WarpCostChoice"; data: { player: PlayerId; object_id: ObjectId; card_id: CardId; normal_cost: ManaCost; warp_cost: ManaCost } }
  | { type: "MiracleReveal"; data: { player: PlayerId; object_id: ObjectId; cost: ManaCost } }
  | { type: "MiracleCastOffer"; data: { player: PlayerId; object_id: ObjectId; cost: ManaCost } }
  | { type: "MadnessCastOffer"; data: { player: PlayerId; object_id: ObjectId; cost: ManaCost } }
  | { type: "DiscardForCost"; data: { player: PlayerId; count: number; cards: ObjectId[]; pending_cast: PendingCast } }
  | { type: "SacrificeForCost"; data: { player: PlayerId; count: number; permanents: ObjectId[]; pending_cast: PendingCast } }
  | { type: "ReturnToHandForCost"; data: { player: PlayerId; count: number; permanents: ObjectId[]; pending_cast: PendingCast } }
  | { type: "BlightChoice"; data: { player: PlayerId; count: number; creatures: ObjectId[]; pending_cast: PendingCast } }
  | { type: "TapCreaturesForManaAbility"; data: { player: PlayerId; count: number; creatures: ObjectId[]; pending_mana_ability: unknown } }
  | { type: "ChooseManaColor"; data: { player: PlayerId; choice: ManaChoicePrompt; pending_mana_ability: unknown } }
  | { type: "TapCreaturesForSpellCost"; data: { player: PlayerId; count: number; creatures: ObjectId[]; pending_cast: PendingCast } }
  | { type: "ExileForCost"; data: { player: PlayerId; zone: ExileCostSourceZone; count: number; cards: ObjectId[]; pending_cast: PendingCast } }
  | { type: "CollectEvidenceChoice"; data: { player: PlayerId; minimum_mana_value: number; cards: ObjectId[]; resume: unknown } }
  | { type: "HarmonizeTapChoice"; data: { player: PlayerId; eligible_creatures: ObjectId[]; pending_cast: PendingCast } }
  | { type: "OptionalEffectChoice"; data: { player: PlayerId; source_id: ObjectId; description?: string } }
  | { type: "OpponentMayChoice"; data: { player: PlayerId; source_id: ObjectId; description?: string; remaining: PlayerId[] } }
  | { type: "UnlessPayment"; data: { player: PlayerId; cost: UnlessCost; pending_effect: unknown; effect_description?: string } }
  | { type: "WardDiscardChoice"; data: { player: PlayerId; cards: ObjectId[]; pending_effect: unknown } }
  | { type: "WardSacrificeChoice"; data: { player: PlayerId; permanents: ObjectId[]; pending_effect: unknown; remaining: number } }
  | { type: "ChooseRingBearer"; data: { player: PlayerId; candidates: ObjectId[] } }
  | { type: "DiscoverChoice"; data: { player: PlayerId; hit_card: ObjectId; exiled_misses: ObjectId[] } }
  | { type: "CascadeChoice"; data: { player: PlayerId; hit_card: ObjectId; exiled_misses: ObjectId[]; source_mv: number } }
  | { type: "TopOrBottomChoice"; data: { player: PlayerId; object_id: ObjectId } }
  | { type: "CompanionReveal"; data: { player: PlayerId; eligible_companions: [string, number][] } }
  | { type: "ChooseLegend"; data: { player: PlayerId; legend_name: string; candidates: ObjectId[] } }
  | { type: "BattleProtectorChoice"; data: { player: PlayerId; battle_id: ObjectId; candidates: PlayerId[] } }
  | { type: "TributeChoice"; data: { player: PlayerId; source_id: ObjectId; count: number } }
  | { type: "CombatTaxPayment"; data: { player: PlayerId; context: CombatTaxContext; total_cost: ManaCost; per_creature: [ObjectId, ManaCost][]; pending: CombatTaxPending } }
  | { type: "PhyrexianPayment"; data: { player: PlayerId; spell_object: ObjectId; shards: PhyrexianShard[] } }
  | { type: "AssignCombatDamage"; data: { player: PlayerId; attacker_id: ObjectId; total_damage: number; blockers: { blocker_id: ObjectId; lethal_minimum: number }[]; trample: TrampleKind | null; defending_player: PlayerId; attack_target: AttackTarget; pw_loyalty?: number; pw_controller?: PlayerId } }
  | { type: "DistributeAmong"; data: { player: PlayerId; total: number; targets: TargetRef[]; unit: DistributionUnit } }
  | { type: "ChooseFromZoneChoice"; data: { player: PlayerId; cards: ObjectId[]; count: number; up_to?: boolean; constraint?: ChooseFromZoneConstraint | null; source_id: ObjectId } }
  | { type: "EffectZoneChoice"; data: {
      player: PlayerId;
      cards: ObjectId[];
      count: number;
      up_to?: boolean;
      source_id: ObjectId;
      effect_kind: string;
      zone: Zone;
      destination?: Zone | null;
      enter_tapped?: boolean;
      enter_transformed?: boolean;
      under_your_control?: boolean;
      enters_attacking?: boolean;
      owner_library?: boolean;
    } }
  | { type: "RetargetChoice"; data: { player: PlayerId; stack_entry_index: number; scope: RetargetScope; current_targets: TargetRef[]; legal_new_targets: TargetRef[] } }
  | { type: "ProliferateChoice"; data: { player: PlayerId; eligible: TargetRef[] } }
  | { type: "ConniveDiscard"; data: { player: PlayerId; conniver_id: ObjectId; source_id: ObjectId; cards: ObjectId[]; count: number } }
  | { type: "DiscardChoice"; data: { player: PlayerId; count: number; cards: ObjectId[]; source_id: ObjectId; effect_kind: string; up_to?: boolean; unless_filter?: TargetFilter } }
  | { type: "ManifestDreadChoice"; data: { player: PlayerId; cards: ObjectId[] } }
  | { type: "LearnChoice"; data: { player: PlayerId; hand_cards: ObjectId[] } }
  | { type: "ClashCardPlacement"; data: { player: PlayerId; card: ObjectId; remaining: [PlayerId, ObjectId][] } }
  | { type: "VoteChoice"; data: {
      player: PlayerId;
      remaining_votes: number;
      options: string[];
      option_labels: string[];
      remaining_voters: [PlayerId, number][];
      tallies: number[];
      controller: PlayerId;
      source_id: ObjectId;
    } }
  | { type: "ChooseDungeon"; data: { player: PlayerId; options: DungeonId[] } }
  | { type: "ChooseDungeonRoom"; data: { player: PlayerId; dungeon: DungeonId; options: number[]; option_names: string[] } }
  | { type: "CategoryChoice"; data: {
      player: PlayerId;
      target_player: PlayerId;
      categories: string[];
      eligible_per_category: ObjectId[][];
      source_id: ObjectId;
      remaining_players: PlayerId[];
      all_kept: ObjectId[];
    } };

// ── Learn ────────────────────────────────────────────────────────────────

export type LearnOption =
  | { type: "Rummage"; data: { card_id: ObjectId } }
  | { type: "Skip" };

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
  | "Mana" | "State" | "Token" | "Trigger" | "Special" | "Destroy"
  | "Debug";

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
  | { type: "CrewVehicle"; data: { vehicle_id: ObjectId; creature_ids: ObjectId[] } }
  | { type: "ActivateStation"; data: { spacecraft_id: ObjectId; creature_id?: ObjectId | null } }
  | { type: "SaddleMount"; data: { mount_id: ObjectId; creature_ids: ObjectId[] } }
  | { type: "Transform"; data: { object_id: ObjectId } }
  | { type: "PlayFaceDown"; data: { object_id: ObjectId; card_id: CardId } }
  | { type: "TurnFaceUp"; data: { object_id: ObjectId } }
  | { type: "SubmitSideboard"; data: { main: DeckCardCount[]; sideboard: DeckCardCount[] } }
  | { type: "ChoosePlayDraw"; data: { play_first: boolean } }
  | { type: "ChooseOption"; data: { choice: string } }
  | { type: "SelectModes"; data: { indices: number[] } }
  | { type: "DecideOptionalCost"; data: { pay: boolean } }
  | { type: "ChooseAdventureFace"; data: { creature: boolean } }
  | { type: "ChooseModalFace"; data: { back_face: boolean } }
  | { type: "ChooseWarpCost"; data: { use_warp: boolean } }
  | { type: "CastSpellAsMiracle"; data: { object_id: ObjectId; card_id: CardId } }
  | { type: "CastSpellAsMadness"; data: { object_id: ObjectId; card_id: CardId } }
  // CR 702.190a: Cast a spell from hand via the Sneak alternative cost during
  // the declare-blockers step, returning an unblocked attacker you control.
  // Applies to any card type; CR 702.190b enter-attacking-alongside is
  // handled engine-side for permanent spells only.
  | { type: "CastSpellAsSneak"; data: { hand_object: ObjectId; card_id: CardId; creature_to_return: ObjectId } }
  | { type: "ActivateNinjutsu"; data: { ninjutsu_card_id: CardId; creature_to_return: ObjectId } }
  | { type: "DecideOptionalEffect"; data: { accept: boolean } }
  | { type: "PayUnlessCost"; data: { pay: boolean } }
  | { type: "ChooseRingBearer"; data: { target: ObjectId } }
  | { type: "ChooseLegend"; data: { keep: ObjectId } }
  | { type: "ChooseBattleProtector"; data: { protector: PlayerId } }
  | { type: "PayCombatTax"; data: { accept: boolean } }
  | { type: "HarmonizeTap"; data: { creature_id: ObjectId | null } }
  | { type: "DeclareCompanion"; data: { card_index: number | null } }
  | { type: "CompanionToHand" }
  | { type: "DiscoverChoice"; data: { choice: CastChoice } }
  | { type: "CascadeChoice"; data: { choice: CastChoice } }
  | { type: "ChooseTopOrBottom"; data: { top: boolean } }
  | { type: "SetAutoPass"; data: { mode: { type: "UntilStackEmpty" } | { type: "UntilEndOfTurn" } } }
  | { type: "CancelAutoPass" }
  | { type: "SetPhaseStops"; data: { stops: Phase[] } }
  | { type: "AssignCombatDamage"; data: { assignments: [ObjectId, number][]; trample_damage: number; controller_damage: number } }
  | { type: "DistributeAmong"; data: { distribution: [TargetRef, number][] } }
  | { type: "RetargetSpell"; data: { new_targets: TargetRef[] } }
  | { type: "LearnDecision"; data: { choice: LearnOption } }
  | { type: "ChooseDungeon"; data: { dungeon: DungeonId } }
  | { type: "ChooseDungeonRoom"; data: { room_index: number } }
  | { type: "SelectCategoryPermanents"; data: { choices: (ObjectId | null)[] } }
  | { type: "ChooseX"; data: { value: number } }
  | { type: "SubmitPhyrexianChoices"; data: { choices: ShardChoice[] } }
  | { type: "ChooseManaColor"; data: { choice: ManaChoice } };

// CR 605.3b + CR 106.1a: Shape of the prompt surfaced by WaitingFor::ChooseManaColor.
export type ManaChoicePrompt =
  | { type: "SingleColor"; data: { options: ManaType[] } }
  | { type: "Combination"; data: { options: ManaType[][] } };

// CR 605.3b: Player's answer to a ManaChoicePrompt. Shape mirrors the prompt.
export type ManaChoice =
  | { type: "SingleColor"; data: ManaType }
  | { type: "Combination"; data: ManaType[] };

// CR 107.4f + CR 601.2f: Per-shard Phyrexian payment choice.
export type ShardChoice =
  | { type: "PayMana" }
  | { type: "PayLife" };

export type ShardOptions =
  | { type: "ManaOrLife" }
  | { type: "ManaOnly" }
  | { type: "LifeOnly" };

export interface PhyrexianShard {
  shard_index: number;
  color: ManaColor;
  options: ShardOptions;
}

// ── Game Events (discriminated union, tag="type", content="data") ────────

export type GameEvent =
  | { type: "GameStarted" }
  | { type: "TurnStarted"; data: { player_id: PlayerId; turn_number: number } }
  | { type: "PhaseChanged"; data: { phase: Phase } }
  | { type: "PriorityPassed"; data: { player_id: PlayerId } }
  | { type: "SpellCast"; data: { card_id: CardId; controller: PlayerId; object_id: ObjectId } }
  | { type: "XValueChosen"; data: { player: PlayerId; object_id: ObjectId; value: number } }
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
  | { type: "DamageDealt"; data: { source_id: ObjectId; target: TargetRef; amount: number; is_combat: boolean; excess?: number } }
  | { type: "SpellCountered"; data: { object_id: ObjectId; countered_by: ObjectId } }
  | { type: "CounterAdded"; data: { object_id: ObjectId; counter_type: string; count: number } }
  | { type: "CounterRemoved"; data: { object_id: ObjectId; counter_type: string; count: number } }
  | { type: "TokenCreated"; data: { object_id: ObjectId; name: string } }
  | { type: "CreatureDestroyed"; data: { object_id: ObjectId } }
  | { type: "PermanentSacrificed"; data: { object_id: ObjectId; player_id: PlayerId } }
  | { type: "EffectResolved"; data: { kind: string; source_id: ObjectId } }
  | { type: "AttackersDeclared"; data: { attacker_ids: ObjectId[]; defending_player: PlayerId; attacks?: [ObjectId, AttackTarget][] } }
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
  | { type: "SpeedChanged"; data: { player: PlayerId; old_speed: number | null; new_speed: number | null } }
  | { type: "CreatureExploited"; data: { exploiter: ObjectId; sacrificed: ObjectId } }
  | { type: "PowerToughnessChanged"; data: { object_id: ObjectId; power: number; toughness: number; power_delta: number; toughness_delta: number } }
  | { type: "RoomEntered"; data: { player_id: PlayerId; dungeon: DungeonId; room_index: number; room_name: string } }
  | { type: "DungeonCompleted"; data: { player_id: PlayerId; dungeon: DungeonId } }
  | { type: "InitiativeTaken"; data: { player_id: PlayerId } };

// ── Game State ───────────────────────────────────────────────────────────

/**
 * Engine-authored presentation projections — a single commander-damage
 * badge entry. Mirrors `engine::game::derived_views::CommanderDamageView`.
 */
export interface CommanderDamageView {
  victim: PlayerId;
  commander: ObjectId;
  damage: number;
}

/**
 * Engine-authored projections computed at each state snapshot. Rides
 * alongside GameState through every adapter path. Frontend components
 * consume this shape directly and never compute grouping/filtering
 * themselves (CLAUDE.md: engine owns all logic). Mirrors
 * `engine::game::derived_views::DerivedViews`.
 */
export interface DerivedViews {
  /** Keyed by attacking commander's current controller (PlayerId as string). */
  commander_damage_by_attacker?: Record<string, CommanderDamageView[]>;
  /**
   * Engine-authored coalesced view of the stack. Empty (and omitted from
   * the wire payload) when the stack is empty. StackDisplay consumes this
   * directly — never re-compute the grouping client-side. Mirrors
   * `engine::game::derived_views::DerivedViews::stack_display_groups`.
   */
  stack_display_groups?: StackDisplayGroup[];
}

export interface GameState {
  turn_number: number;
  active_player: PlayerId;
  phase: Phase;
  players: Player[];
  priority_player: PlayerId;
  turn_decision_controller?: PlayerId | null;
  objects: Record<string, GameObject>;
  next_object_id: number;
  battlefield: ObjectId[];
  stack: StackEntry[];
  exile: ObjectId[];
  rng_seed: number;
  combat: CombatState | null;
  waiting_for: WaitingFor;
  has_pending_cast: boolean;
  lands_played_this_turn: number;
  max_lands_per_turn: number;
  priority_pass_count: number;
  /**
   * Engine-authored derived projections, attached by adapters from the
   * wire-format `ClientGameState.derived` sibling field. Optional because
   * some wire paths (legacy cached state, older server builds) may not
   * carry it. Consumers MUST treat absence as "no data" and MUST NOT
   * synthesize grouped values client-side — that's a CLAUDE.md violation.
   */
  derived?: DerivedViews;
  pending_replacement: unknown | null;
  layers_dirty: boolean;
  next_timestamp: number;
  seat_order?: PlayerId[];
  format_config?: FormatConfig;
  eliminated_players?: PlayerId[];
  dungeon_progress?: Record<string, { current_dungeon: DungeonId | null; current_room: number; completed: DungeonId[] }>;
  initiative?: PlayerId | null;
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
  phase_stops?: Record<number, Phase[]>;
  lands_tapped_for_mana?: Record<number, number[]>;
  scheduled_turn_controls?: Array<{
    target_player: PlayerId;
    controller: PlayerId;
    grant_extra_turn_after?: boolean;
  }>;
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
  /**
   * Optional Rust panic message captured by `take_last_panic_message` after
   * a WASM trap. Only set when `code === ENGINE_PANIC`. Carrying the panic
   * here (rather than only via the message) lets the modal render the full
   * diagnostic without the recovery layer needing to thread it back.
   */
  readonly panic?: string;

  constructor(code: string, message: string, recoverable: boolean, panic?: string) {
    super(message);
    this.name = "AdapterError";
    this.code = code;
    this.recoverable = recoverable;
    this.panic = panic;
  }
}

/** Error codes for AdapterError */
export const AdapterErrorCode = {
  NOT_INITIALIZED: "NOT_INITIALIZED",
  /**
   * The engine had a game, then lost it. Distinct from NOT_INITIALIZED
   * (never had one). Triggered by the Rust sentinel `NOT_INITIALIZED: ...`
   * prefix — indicates the thread-local `GAME_STATE` is `None` mid-session
   * (worker restart, PWA update desync). Recoverable via
   * `adapter.restoreState(lastKnownGoodState)` only when no panic preceded
   * the loss; if a panic did precede it, classify as ENGINE_PANIC instead
   * because retrying the same input will re-panic.
   */
  STATE_LOST: "STATE_LOST",
  /**
   * The engine panicked. State loss followed (the take/set thread-local
   * pattern can't return state on a WASM trap), but unlike STATE_LOST this
   * is NOT a transient situation — the same action against the same state
   * will panic again. The adapter pulls `take_last_panic_message()` from
   * the worker before classifying so the modal can show the real cause and
   * offer a pre-filled bug report.
   */
  ENGINE_PANIC: "ENGINE_PANIC",
  WASM_ERROR: "WASM_ERROR",
  INVALID_ACTION: "INVALID_ACTION",
} as const;

/**
 * Detect the Rust-side sentinel used by `with_state`/`with_state_mut` in
 * `engine-wasm/src/lib.rs` when `GAME_STATE` is `None`. Match against the
 * exact prefix — never the full message, which may evolve.
 */
export function isStateLostMessage(message: string): boolean {
  return message.startsWith("NOT_INITIALIZED:");
}

/**
 * Transport-agnostic interface for communicating with the game engine.
 * Phase 1: WasmAdapter (direct WASM calls)
 * Phase 7: TauriAdapter (IPC to native Rust process)
 */
export interface SubmitResult {
  events: GameEvent[];
  log_entries?: GameLogEntry[];
}

/** Bundles legal actions with the engine's auto-pass recommendation. */
export interface LegalActionsResult {
  actions: GameAction[];
  autoPassRecommended: boolean;
  /** Effective mana costs for castable spells, keyed by object_id string. */
  spellCosts?: Record<string, ManaCost>;
  /**
   * Engine-grouped subset of `actions` keyed by `GameAction::source_object()`.
   * Frontend uses this for "what can I do with this card?" lookups so it
   * doesn't have to introspect `GameAction` variants client-side.
   */
  legalActionsByObject?: Record<string, GameAction[]>;
}

/**
 * Combined filtered-state + viewer-scoped legal-actions snapshot returned by
 * the engine in one WASM round-trip. Used by the P2P host broadcast loop to
 * collapse `getFilteredState(pid) + getLegalActionsForViewer(pid)` into a
 * single call. Fields deliberately mirror `LegalActionsResult`'s field names
 * so the existing `legalActionsToWire` helper accepts a `ViewerSnapshot`
 * directly via structural typing.
 */
export interface ViewerSnapshot {
  state: GameState;
  actions: GameAction[];
  autoPassRecommended: boolean;
  spellCosts?: Record<string, ManaCost>;
  legalActionsByObject?: Record<string, GameAction[]>;
}

export interface EngineAdapter {
  initialize(): Promise<void>;
  initializeGame(
    deckData?: unknown,
    formatConfig?: FormatConfig,
    playerCount?: number,
    matchConfig?: MatchConfig,
    firstPlayer?: number,
  ): Promise<SubmitResult> | SubmitResult;
  /**
   * Submit a game action on behalf of `actor`. The engine enforces that
   * `actor === authorized_submitter(state)` (with the `Concede` exception),
   * so a mismatched actor is rejected by the engine. Callers must pass the
   * locally-authenticated PlayerId — never a value copied out of the
   * action payload or the UI state.
   */
  submitAction(action: GameAction, actor: PlayerId): Promise<SubmitResult>;
  getState(): Promise<GameState>;
  getLegalActions(): Promise<LegalActionsResult>;
  getAiAction(difficulty: string, playerId: number): Promise<GameAction | null> | GameAction | null;
  restoreState(state: GameState): void;
  dispose(): void;
}
