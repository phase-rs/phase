import type { BracketDeckRequest, BracketEstimate } from "../types/bracketEstimate";

// ── Identifiers ──────────────────────────────────────────────────────────

export type ObjectId = number;
export type CardId = number;
export type PlayerId = number;

// ── Attachment Target ────────────────────────────────────────────────────
// Mirrors `engine::game::game_object::AttachTarget`. Auras may attach to a
// permanent (`Object`) or to a player (`Player`, e.g. Curse cycle); Equipment
// is `Object`-only by CR 301.5. Serde tag/content format matches the engine.
export type AttachTarget =
  | { type: "Object"; data: ObjectId }
  | { type: "Player"; data: PlayerId };

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
  | "Premodern"
  | "Legacy"
  | "Vintage"
  | "Historic"
  | "Timeless"
  | "Pauper"
  | "PauperCommander"
  | "DuelCommander"
  | "TinyLeaders"
  | "Brawl"
  | "HistoricBrawl"
  | "FreeForAll"
  | "TwoHeadedGiant"
  | "Limited";

export type FormatGroup = "Constructed" | "Commander" | "Multiplayer" | "Limited";

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
  /**
   * Engine-derived predicate: true when the format uses a commander card
   * and the commander-damage state-based action (CR 903.10a / CR 704.5u).
   * The frontend must consume this directly rather than re-listing
   * commander-style format strings client-side.
   */
  uses_commander: boolean;
  /**
   * Sandbox capability flag: when true the server permits `GameAction.Debug(_)`
   * from any player in the `debug_permitted` set. Off by default. Orthogonal
   * to format — applies on top of any `GameFormat`. Immutable for the life
   * of a session.
   */
  allow_debug_actions: boolean;
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
  /**
   * `true` when the host enabled Sandbox mode for this game (debug actions
   * permitted under host control). Browsers render a SANDBOX badge and prompt
   * joiners to confirm before entering.
   */
  is_sandbox?: boolean;
  /** Draft-specific metadata. Present when the room is a draft pod. */
  draft_metadata?: DraftLobbyMetadata | null;
}

/** Metadata for draft pod lobby entries. */
export interface DraftLobbyMetadata {
  /** Three-letter set code (e.g. "MKM", "OTJ"). For cube drafts, "custom-cube". */
  setCode: string;
  /** Draft kind: "Quick", "Premier", or "Traditional". */
  draftKind: string;
  /** Human-readable cube name when the pod is a cube draft. Absent for set drafts. */
  cubeName?: string;
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
  reservation_token?: string | null;
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
  reservation_token?: string | null;
  reservation_expires_at_ms?: number | null;
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

/**
 * Discriminated source for a single outside-game candidate. Sideboard entries
 * carry their full `CardFace` so the UI can render them without a sideboard
 * lookup; face-up exile candidates are addressed by their in-game `ObjectId`.
 * Mirrors Rust `OutsideGameChoiceSource` (engine `types/game_state.rs`).
 */
export type OutsideGameChoiceSource =
  | { type: "Sideboard"; data: { sideboard_index: number; card: CardFacePartial } }
  | { type: "FaceUpExile"; data: { object_id: ObjectId } };

export interface OutsideGameChoiceEntry {
  source: OutsideGameChoiceSource;
  count: number;
  name: string;
}

/**
 * One committed selection on `GameAction::ChooseOutsideGameCards`. Mirrors
 * Rust `OutsideGameSelection` (engine `types/actions.rs`).
 */
export type OutsideGameSelection =
  | { type: "Sideboard"; data: { sideboard_index: number } }
  | { type: "FaceUpExile"; data: { object_id: ObjectId } };

export interface OutsideGameCardUse {
  player: PlayerId;
  sideboard_index: number;
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

export type LibraryPosition =
  | { type: "Top" }
  | { type: "Bottom" }
  | { type: "NthFromTop"; n: number };

// Narrow source-zone type for `WaitingFor::ExileForCost` — only `Hand` (pitch
// spells) and `Graveyard` (escape) are valid (mirrors the engine's
// `ExileCostSourceZone`).
export type ExileCostSourceZone = "Hand" | "Graveyard";

export type ManaColor = "White" | "Blue" | "Black" | "Red" | "Green";

export type CoreType =
  | "Artifact"
  | "Creature"
  | "Enchantment"
  | "Instant"
  | "Land"
  | "Planeswalker"
  | "Sorcery"
  | "Tribal"
  | "Battle"
  | "Kindred"
  | "Dungeon";

export type ManaType = "White" | "Blue" | "Black" | "Red" | "Green" | "Colorless";
export type ConvokeMode = "Convoke" | "Waterbend" | "Improvise";
export type RoomDoor = "Left" | "Right";

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

// Mirrors `crate::types::mana::ManaRestriction` (externally-tagged serde:
// unit variants serialize as bare strings, data variants as
// `{ VariantName: payload }`). `KeywordKind` is the engine's large unit-only
// keyword enum — serialized as a bare keyword string (e.g. "Flashback").
export type KeywordKind = string;

export type ManaRestriction =
  // "Spend this mana only to cast creature/artifact spells."
  | { OnlyForSpellType: string }
  // "Spend this mana only to cast a creature spell of the chosen type."
  | { OnlyForCreatureType: string }
  // "Spend this mana only to cast creature spells or activate creature abilities."
  | { OnlyForTypeSpellsOrAbilities: string }
  // "Spend this mana only to cast spells with flashback."
  | { OnlyForSpellWithKeywordKind: KeywordKind }
  // "Spend this mana only to cast spells with flashback from a graveyard."
  | { OnlyForSpellWithKeywordKindFromZone: [KeywordKind, Zone] }
  // "Spend this mana only to activate abilities."
  | "OnlyForActivation"
  // "Spend this mana only on costs that include {X}."
  | "OnlyForXCosts"
  // Internal convoke-tap marker — never surfaced to the player.
  | "ConvokePayment";

// Mirrors `crate::types::mana::ManaSpellGrant` (CR 106.6) — properties this
// mana grants to the spell it is spent on. Externally-tagged serde.
export type ManaSpellGrant =
  | "CantBeCountered"
  | {
      AddKeywordUntilEndOfTurn: {
        keyword: Keyword;
        restriction?: ManaRestriction | null;
      };
    };

export interface ManaUnit {
  color: ManaType;
  source_id: ObjectId;
  snow: boolean;
  restrictions: ManaRestriction[];
  // `#[serde(default, skip_serializing_if = "Vec::is_empty")]` — absent when empty.
  grants?: ManaSpellGrant[];
}

export interface ManaPool {
  mana: ManaUnit[];
}

export type ManaCost =
  | { type: "NoCost" }
  | { type: "Cost"; shards: string[]; generic: number }
  | { type: "SelfManaCost" };

export type CastFrequency =
  | "Unlimited"
  | "OncePerTurn"
  | "OncePerTurnPerPermanentType";

export type CastingVariant =
  | { type: "Normal" }
  | { type: "Adventure" }
  | { type: "Omen" }
  | { type: "Warp" }
  | { type: "Escape" }
  | { type: "Retrace" }
  | { type: "Harmonize" }
  | { type: "Flashback" }
  | { type: "Aftermath" }
  | {
      type: "GraveyardPermission";
      data: {
        source: ObjectId;
        frequency: CastFrequency;
        slot_type?: CoreType | null;
        graveyard_destination_replacement?: Zone | null;
      };
    }
  | { type: "HandPermission"; data: { source: ObjectId; frequency: CastFrequency } }
  | { type: "Sneak"; data: { returned_creature: ObjectId; placement?: unknown | null } }
  | { type: "WebSlinging"; data: { returned_creature: ObjectId } }
  | { type: "Miracle" }
  | { type: "Madness" }
  | { type: "Evoke" }
  | { type: "Suspend" }
  | { type: "Plot" }
  | { type: "Foretell" }
  | { type: "Overload" }
  | { type: "Bestow" }
  | { type: "Cleave" };

export interface CastingVariantChoiceOption {
  variant: CastingVariant;
  mana_cost: ManaCost;
}

export type CastPaymentMode = { type: "Auto" } | { type: "Manual" };

export type UnlessCost =
  | { type: "Fixed"; cost: ManaCost }
  | { type: "DynamicGeneric"; quantity: unknown }
  | { type: "PayLife"; amount: number }
  | { type: "DiscardCard" }
  | { type: "Sacrifice"; count: number; filter: TargetFilter }
  | { type: "ReturnToHand"; count: number; filter: TargetFilter };

// CR 118.12a: Player decision at an `UnlessPaymentChooseCost` prompt. Mirrors
// the Rust `UnlessCostBranch` enum (`crates/engine/src/types/actions.rs`).
// `Decline` falls through to the effect happening; `Pay { index }` selects
// the sub-cost by its position in `UnlessPaymentChooseCost.costs`.
export type UnlessCostBranch =
  | { type: "Decline" }
  | { type: "Pay"; data: { index: number } };

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

export type CounterMatch =
  | { type: "Any" }
  | { type: "OfType"; data: CounterType };

export type CounterMoveChoice = {
  destination_id: ObjectId;
  counter_type: CounterType;
  count: number;
};

export type PlayerCounterKind =
  | "Poison"
  | "Experience"
  | "Rad"
  | "Ticket";

// ── Keywords ─────────────────────────────────────────────────────────────

/**
 * Keyword type matching the Rust Keyword enum's serde output.
 * Simple keywords serialize as strings (e.g. "Flying").
 * Parameterized keywords serialize as objects (e.g. { Equip: { Cost: ... } }).
 */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type Keyword = string | Record<string, any>;

// ── Token body characteristics ──────────────────────────────────────────
// Shared by TokenSpec (runtime), TokenPreset (catalog), and
// DebugAction::CreateToken (debug payload). Single source of truth on
// the Rust side; this mirrors `engine::types::proposed_event::TokenCharacteristics`.

export type Supertype = "Legendary" | "Basic" | "Snow" | "World" | "Ongoing";

export interface TokenCharacteristics {
  display_name: string;
  power: number | null;
  toughness: number | null;
  core_types: CoreType[];
  subtypes: string[];
  supertypes: Supertype[];
  colors: ManaColor[];
  keywords: Keyword[];
}

export interface TokenImageRef {
  scryfall_id: string;
  scryfall_oracle_id?: string | null;
  face_name?: string | null;
  preset_id: string;
}

// ── CR 701.57a + CR 702.85a: Cast/decline choice for Discover and Cascade ──

export type CastChoice = { type: "Cast" } | { type: "Decline" };

export type AutoMayChoice = { type: "Accept" } | { type: "Decline" };

export type MayTriggerOrigin =
  | { type: "Printed"; trigger_index: number }
  | { type: "Keyword"; keyword: string };

export interface MayTriggerAutoChoiceKey {
  player: PlayerId;
  source_id: ObjectId;
  origin: MayTriggerOrigin;
}

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
  /** Derived by the engine (AbilityDefinition::consumes_source): true when
   *  paying this ability's cost discards the source card itself (cycling,
   *  Channel). Absent / false otherwise. The UI uses this to require a
   *  confirmation modal for a lone card-consuming action — see
   *  requiresConfirmation in viewmodel/cardActionChoice.ts. */
  consumes_source?: boolean;
  [key: string]: unknown;
}

export type ChooseFromZoneConstraint =
  | { type: "DistinctCardTypes"; categories: string[] };

export type SearchSelectionConstraint =
  | { type: "None" }
  | { type: "DistinctQualities"; qualities: string[] }
  | { type: "TotalManaValue"; comparator: string; value: number }
  | { type: "MatchEachFilter"; filters: TargetFilter[] };

// CR 701.23a + CR 608.2c: Cultivate-class split destination metadata mirrored
// from Rust `SearchDestinationSplit`.
export type SearchDestinationSplit = {
  primary_destination: Zone;
  primary_count: number;
  primary_enter_tapped: boolean;
  rest_destination: Zone;
};

// ── Game Object ──────────────────────────────────────────────────────────

/**
 * Per-permanent phasing status (mirrors Rust `PhaseStatus`).
 * Serde output: `{ "status": "PhasedIn" }` / `{ "status": "PhasedOut", "cause": "Directly" | "Indirectly" }`.
 * CR 702.26: phased-out permanents stay on the battlefield but are treated
 * as though they don't exist for almost all rules queries (CR 702.26d).
 */
export type PhaseStatus =
  | { status: "PhasedIn" }
  | { status: "PhasedOut"; cause: "Directly" | "Indirectly" };

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
  /** Mirrors engine `Option<AttachTarget>`: null when unattached, otherwise
   *  a tagged-union pointing at either an Object host (Equipment/most Auras)
   *  or a Player host (Curse cycle, Faith's Fetters-class). FE consumers must
   *  inspect `.type` before reading `.data`; do not treat as a bare ObjectId. */
  attached_to: AttachTarget | null;
  attachments: ObjectId[];
  paired_with?: ObjectId | null;
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
  /** CR 111.10: engine-provided printed rules text for predefined tokens
   *  (Lander, etc.). Used as alt-text / aria-label when the Scryfall token
   *  image is unavailable. Absent for non-predefined objects. */
  token_rules_text?: string;
  token_image_ref?: TokenImageRef | null;
  source_related_token_ids?: string[];
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
   * CR 111.1: Whether this object is a token (not a card). Independent of
   * `display_source`: a token-copy of a real card (Twinflame, Helm of the
   * Host) carries `is_token = true` AND `display_source = "Card"`, so it
   * renders visually identical to the printed card. Combine the two to flag
   * such copies (`is_token && display_source !== "Token"`).
   */
  is_token?: boolean;
  /**
   * Image-lookup routing hint from the engine. "Card" → look up the image
   * in the real-card database (default; also covers token-copies of real
   * cards like Twinflame/Helm of the Host). "Token" → look up the image
   * in Scryfall's generic-token database (Treasure, Spirit 1/1, etc.).
   * Independent of `is_token` (which is the CR 111.1 game-rules concept).
   */
  display_source?: "Card" | "Token";
  /**
   * CR 702.26: Phasing status of this permanent. Absent for objects in zones
   * where phasing doesn't apply (engine-side default is `PhasedIn`, which may
   * be elided on the wire if the field defaults). The FE renders a sky-blue
   * "ethereal plane" tint over phased-out permanents.
   */
  phase_status?: PhaseStatus;
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
  player_counters?: Record<string, number>;
}

// ── Target Filter ───────────────────────────────────────────────────────

/** Engine-side target filter (opaque — frontend only checks presence, never inspects). */
export type TargetFilter = Record<string, unknown>;

// ── Target Ref ───────────────────────────────────────────────────────────

export type TargetRef =
  | { Object: ObjectId }
  | { Player: PlayerId };

export type CopyTargetSlot = { current?: TargetRef | null; legal_alternatives: TargetRef[] };

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
  else_ability?: ResolvedAbility;
  description?: string;
}

// ── Stack ────────────────────────────────────────────────────────────────

export type KeywordAction =
  | { Equip: { equipment_id: ObjectId; target_creature_id: ObjectId } }
  | { Crew: { vehicle_id: ObjectId; paid_creature_ids: ObjectId[] } }
  | { Saddle: { mount_id: ObjectId; paid_creature_ids: ObjectId[] } }
  | { Station: { spacecraft_id: ObjectId; paid_creature_id: ObjectId; snapshot_power: number } };

export type StackEntryKind =
  | { type: "Spell"; data: { card_id: CardId; ability?: ResolvedAbility; actual_mana_spent?: number } }
  | { type: "ActivatedAbility"; data: { source_id: ObjectId; ability: ResolvedAbility } }
  | { type: "TriggeredAbility"; data: { source_id: ObjectId; ability: ResolvedAbility; description?: string; source_name?: string } }
  | { type: "KeywordAction"; data: { action: KeywordAction } };

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

export interface StackTargetDisplay {
  target: TargetRef;
  label: string;
}

export type StackPaidFactView =
  | { type: "XValue"; data: { value: number } }
  | { type: "ManaSpent"; data: { amount: number } }
  | { type: "ColorsSpent"; data: { distinct: number } }
  | { type: "Kicked"; data: { count: number } }
  | { type: "AdditionalCostPaid" }
  | { type: "CastVariant"; data: { variant: string } }
  | { type: "Convoked"; data: { count: number } };

export interface TriggerContextDisplay {
  label: string;
  object_id?: ObjectId;
  player?: PlayerId;
}

export interface StackEntryDisplay {
  source_name: string;
  kind_label: string;
  ability_description?: string;
  targets?: StackTargetDisplay[];
  paid?: StackPaidFactView[];
  trigger_context?: TriggerContextDisplay[];
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
  | { type: "Optional"; data: { cost: SerializedAbilityCost; repeatable?: boolean } }
  | { type: "Kicker"; data: { costs: SerializedAbilityCost[]; repeatable?: boolean } }
  | { type: "Required"; data: SerializedAbilityCost }
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

// CR 603.3b: Display payload for one collected-but-not-yet-stacked trigger
// awaiting its controller's ordering choice. Engine-derived; the overlay
// must NOT re-derive name/description from state.objects.
export interface PendingTriggerSummary {
  source_id: ObjectId;
  source_name: string;
  description: string;
}

// ── WaitingFor (discriminated union with tag="type", content="data") ─────

export type OpeningHandBottomReason = { type: "TinyLeadersMultiCommander" };

export type WaitingFor =
  | { type: "Priority"; data: { player: PlayerId } }
  | { type: "ActivationCostOneOfChoice"; data: { player: PlayerId; costs: SerializedAbilityCost[]; pending_cast: PendingCast } }
  | {
      type: "MulliganDecision";
      data: {
        pending: { player: PlayerId; mulligan_count: number }[];
        free_first_mulligan: boolean;
      };
    }
  | {
      type: "MulliganBottomCards";
      data: { pending: { player: PlayerId; count: number }[] };
    }
  | {
      type: "OpeningHandBottomCards";
      data: {
        pending: { player: PlayerId; count: number }[];
        reason: OpeningHandBottomReason;
      };
    }
  | { type: "ManaPayment"; data: { player: PlayerId; convoke_mode?: ConvokeMode } }
  | {
      type: "ChooseXValue";
      data: { player: PlayerId; min?: number; max: number; pending_cast: PendingCast };
    }
  | { type: "PayAmountChoice"; data: { player: PlayerId; resource: PayableResource; min: number; max: number; accumulated?: number; source_id: ObjectId } }
  | { type: "TargetSelection"; data: { player: PlayerId; pending_cast: PendingCast; target_slots: TargetSelectionSlot[]; selection: TargetSelectionProgress } }
  | { type: "DeclareAttackers"; data: { player: PlayerId; valid_attacker_ids: ObjectId[]; valid_attack_targets?: AttackTarget[] } }
  | { type: "DeclareBlockers"; data: { player: PlayerId; valid_blocker_ids: ObjectId[]; valid_block_targets: Record<string, ObjectId[]>; block_requirements?: Record<string, number> } }
  | { type: "GameOver"; data: { winner: PlayerId | null } }
  | { type: "ReplacementChoice"; data: { player: PlayerId; candidate_count: number; candidate_descriptions?: string[] } }
  | { type: "OrderTriggers"; data: { player: PlayerId; triggers: PendingTriggerSummary[] } }
  | { type: "CopyTargetChoice"; data: { player: PlayerId; source_id: ObjectId; valid_targets: ObjectId[]; max_mana_value?: number | null } }
  | { type: "ExploreChoice"; data: { player: PlayerId; source_id: ObjectId; choosable: ObjectId[]; remaining: ObjectId[]; pending_effect: unknown } }
  | { type: "ReturnAsAuraTarget"; data: { player: PlayerId; source_id: ObjectId; returned_id: ObjectId; legal_targets: ObjectId[]; pending_effect: unknown } }
  | { type: "EquipTarget"; data: { player: PlayerId; equipment_id: ObjectId; valid_targets: ObjectId[] } }
  | { type: "CrewVehicle"; data: { player: PlayerId; vehicle_id: ObjectId; crew_power: number; eligible_creatures: ObjectId[] } }
  | { type: "StationTarget"; data: { player: PlayerId; spacecraft_id: ObjectId; eligible_creatures: ObjectId[] } }
  | { type: "SaddleMount"; data: { player: PlayerId; mount_id: ObjectId; saddle_power: number; eligible_creatures: ObjectId[] } }
  | { type: "ScryChoice"; data: { player: PlayerId; cards: ObjectId[] } }
  | { type: "DigChoice"; data: { player: PlayerId; cards: ObjectId[]; keep_count: number; up_to?: boolean; selectable_cards?: ObjectId[]; kept_destination?: Zone | null; rest_destination?: Zone | null } }
  | { type: "SurveilChoice"; data: { player: PlayerId; cards: ObjectId[] } }
  | { type: "RevealChoice"; data: { player: PlayerId; cards: ObjectId[]; filter: unknown; optional?: boolean } }
  | { type: "SearchChoice"; data: { player: PlayerId; cards: ObjectId[]; count: number; reveal?: boolean; up_to?: boolean; constraint?: SearchSelectionConstraint; split?: SearchDestinationSplit | null } }
  | { type: "SearchPartitionChoice"; data: { player: PlayerId; cards: ObjectId[]; primary_destination: Zone; primary_count: number; primary_enter_tapped: boolean; rest_destination: Zone; source_id: ObjectId } }
  | { type: "OutsideGameChoice"; data: { player: PlayerId; source_id: ObjectId; choices: OutsideGameChoiceEntry[]; count: number; reveal?: boolean; up_to?: boolean; destination: Zone } }
  | { type: "ChooseOneOfBranch"; data: { player: PlayerId; controller: PlayerId; source_id: ObjectId; branches: unknown[]; branch_descriptions?: string[]; parent_targets?: TargetRef[]; context?: unknown; remaining_players?: PlayerId[] } }
  | { type: "TriggerTargetSelection"; data: { player: PlayerId; target_slots: TargetSelectionSlot[]; target_constraints?: TargetSelectionConstraint[]; selection: TargetSelectionProgress; source_id?: ObjectId; description?: string } }
  | { type: "BetweenGamesSideboard"; data: { player: PlayerId; game_number: number; score: MatchScore } }
  | { type: "BetweenGamesChoosePlayDraw"; data: { player: PlayerId; game_number: number; score: MatchScore } }
  | { type: "NamedChoice"; data: { player: PlayerId; choice_type: string | Record<string, unknown>; options: string[]; source_id?: ObjectId } }
  | { type: "DamageSourceChoice"; data: { player: PlayerId; source_filter: TargetFilter; options: ObjectId[] } }
  | { type: "ModeChoice"; data: { player: PlayerId; modal: ModalChoice; pending_cast: PendingCast } }
  | { type: "AbilityModeChoice"; data: { player: PlayerId; modal: ModalChoice; source_id: ObjectId; mode_abilities: unknown[]; is_activated: boolean; ability_index?: number; ability_cost?: unknown; unavailable_modes?: number[] } }
  | { type: "DiscardToHandSize"; data: { player: PlayerId; count: number; cards: ObjectId[] } }
  | { type: "OptionalCostChoice"; data: { player: PlayerId; cost: AdditionalCost; times_kicked: number; pending_cast: PendingCast } }
  | { type: "DefilerPayment"; data: { player: PlayerId; life_cost: number; mana_reduction: ManaCost; pending_cast: PendingCast } }
  | { type: "AdventureCastChoice"; data: { player: PlayerId; object_id: ObjectId; card_id: CardId; payment_mode?: CastPaymentMode } }
  | { type: "ModalFaceChoice"; data: { player: PlayerId; object_id: ObjectId; card_id: CardId } }
  | { type: "AlternativeCastChoice"; data: { player: PlayerId; object_id: ObjectId; card_id: CardId; payment_mode?: CastPaymentMode; keyword: { type: "Warp" } | { type: "Evoke" } | { type: "Overload" } | { type: "Bestow" } | { type: "Cleave" }; normal_cost: ManaCost; alternative_cost: ManaCost | null; alternative_additional_cost: SerializedAbilityCost | null } }
  | { type: "CastingVariantChoice"; data: { player: PlayerId; object_id: ObjectId; card_id: CardId; payment_mode?: CastPaymentMode; options: CastingVariantChoiceOption[] } }
  | { type: "ChoosePermanentTypeSlot"; data: { player: PlayerId; object_id: ObjectId; card_id: CardId; source: ObjectId; payment_mode?: CastPaymentMode; available_slots: CoreType[] } }
  | { type: "MultiTargetSelection"; data: { player: PlayerId; legal_targets: ObjectId[]; min_targets: number; max_targets: number; pending_ability: unknown } }
  | { type: "MiracleReveal"; data: { player: PlayerId; object_id: ObjectId; cost: ManaCost } }
  | { type: "MiracleCastOffer"; data: { player: PlayerId; object_id: ObjectId; cost: ManaCost } }
  | { type: "MadnessCastOffer"; data: { player: PlayerId; object_id: ObjectId; cost: ManaCost } }
  | { type: "DiscardForCost"; data: { player: PlayerId; count: number; cards: ObjectId[]; pending_cast: PendingCast } }
  | { type: "SacrificeForCost"; data: { player: PlayerId; count: number; permanents: ObjectId[]; pending_cast: PendingCast } }
  | { type: "ReturnToHandForCost"; data: { player: PlayerId; count: number; permanents: ObjectId[]; pending_cast: PendingCast } }
  | { type: "RemoveCounterForCost"; data: { player: PlayerId; count: number; counter_type: CounterMatch; permanents: ObjectId[]; pending_cast: PendingCast } }
  | { type: "BlightChoice"; data: { player: PlayerId; count: number; creatures: ObjectId[]; pending_cast: PendingCast } }
  | { type: "BeholdForCost"; data: { player: PlayerId; count: number; choices: ObjectId[]; action: "ChooseOrReveal" | "ExileChosen"; pending_cast: PendingCast } }
  | { type: "TapCreaturesForManaAbility"; data: { player: PlayerId; count: number; creatures: ObjectId[]; pending_mana_ability: unknown } }
  | { type: "DiscardForManaAbility"; data: { player: PlayerId; count: number; cards: ObjectId[]; pending_mana_ability: unknown } }
  | { type: "ExileForManaAbility"; data: { player: PlayerId; count: number; zone: Zone; cards: ObjectId[]; pending_mana_ability: unknown } }
  | { type: "SacrificeForManaAbility"; data: { player: PlayerId; count: number; permanents: ObjectId[]; pending_mana_ability: unknown } }
  | { type: "PayManaAbilityMana"; data: { player: PlayerId; options: ManaType[][]; pending_mana_ability: unknown } }
  | {
      type: "ChooseManaColor";
      data: {
        player: PlayerId;
        choice: ManaChoicePrompt;
        // CR 605.3a: Only the ManaAbility context carries the bulk-activation
        // siblings the UI reads (omitted from the wire when empty). The heavy
        // PendingManaAbility / ResolvedAbility payloads stay opaque here.
        context:
          | { type: "ManaAbility"; data: { batch_siblings?: ObjectId[] } }
          | { type: "ResolvingEffect"; data: unknown };
      };
    }
  | { type: "TapCreaturesForSpellCost"; data: { player: PlayerId; count: number; creatures: ObjectId[]; pending_cast: PendingCast } }
  | { type: "ExileForCost"; data: { player: PlayerId; zone: ExileCostSourceZone; count: number; cards: ObjectId[]; pending_cast: PendingCast } }
  | { type: "CollectEvidenceChoice"; data: { player: PlayerId; minimum_mana_value: number; cards: ObjectId[]; resume: unknown } }
  | { type: "HarmonizeTapChoice"; data: { player: PlayerId; eligible_creatures: ObjectId[]; pending_cast: PendingCast } }
  | { type: "OptionalEffectChoice"; data: { player: PlayerId; source_id: ObjectId; description?: string; may_trigger_key?: MayTriggerAutoChoiceKey } }
  | { type: "PairChoice"; data: { player: PlayerId; source_id: ObjectId; choices: ObjectId[] } }
  | { type: "OpponentMayChoice"; data: { player: PlayerId; source_id: ObjectId; description?: string; remaining: PlayerId[] } }
  | { type: "UnlessPayment"; data: { player: PlayerId; cost: UnlessCost; pending_effect: unknown; trigger_event?: unknown; effect_description?: string; remaining?: PlayerId[] } }
  // CR 118.12a: Disjunctive unless-cost — player picks **which** sub-cost
  // to pay (or declines all). Drives Tergrid's Lantern and the broader
  // "unless they X or Y" punisher class.
  | { type: "UnlessPaymentChooseCost"; data: { player: PlayerId; costs: UnlessCost[]; pending_effect: unknown; trigger_event?: unknown; effect_description?: string } }
  | { type: "WardDiscardChoice"; data: { player: PlayerId; cards: ObjectId[]; pending_effect: unknown } }
  | { type: "WardSacrificeChoice"; data: { player: PlayerId; permanents: ObjectId[]; pending_effect: unknown; remaining: number } }
  | { type: "UnlessBounceChoice"; data: { player: PlayerId; permanents: ObjectId[]; pending_effect: unknown; remaining: number } }
  | { type: "ChooseRingBearer"; data: { player: PlayerId; candidates: ObjectId[] } }
  | { type: "DiscoverChoice"; data: { player: PlayerId; hit_card: ObjectId; exiled_misses: ObjectId[] } }
  | { type: "RevealUntilKeptChoice"; data: { player: PlayerId; hit_card: ObjectId; source_id: ObjectId; accept_zone: string; decline_zone: string; enter_tapped: boolean; enters_attacking: boolean; revealed_misses: ObjectId[]; rest_destination: string } }
  | { type: "RepeatDecision"; data: { player: PlayerId; ability: unknown } }
  | { type: "CascadeChoice"; data: { player: PlayerId; hit_card: ObjectId; exiled_misses: ObjectId[]; source_mv: number } }
  | { type: "TopOrBottomChoice"; data: { player: PlayerId; object_id: ObjectId } }
  | { type: "ParadigmCastOffer"; data: { player: PlayerId; offers: ObjectId[] } }
  | { type: "PopulateChoice"; data: { player: PlayerId; source_id: ObjectId; valid_tokens: ObjectId[] } }
  | { type: "CompanionReveal"; data: { player: PlayerId; eligible_companions: [string, number][] } }
  | { type: "ChooseLegend"; data: { player: PlayerId; legend_name: string; candidates: ObjectId[] } }
  | { type: "CommanderZoneChoice"; data: { player: PlayerId; commander_id: ObjectId; current_zone: string } }
  | { type: "BattleProtectorChoice"; data: { player: PlayerId; battle_id: ObjectId; candidates: PlayerId[] } }
  | { type: "TributeChoice"; data: { player: PlayerId; source_id: ObjectId; count: number } }
  | { type: "CombatTaxPayment"; data: { player: PlayerId; context: CombatTaxContext; total_cost: ManaCost; per_creature: [ObjectId, ManaCost][]; pending: CombatTaxPending } }
  | { type: "UntapChoice"; data: { player: PlayerId; candidates: ObjectId[]; chosen_not_to_untap?: ObjectId[] } }
  | { type: "ExertChoice"; data: { player: PlayerId; attacker: ObjectId; remaining?: ObjectId[] } }
  | { type: "PhyrexianPayment"; data: { player: PlayerId; spell_object: ObjectId; shards: PhyrexianShard[] } }
  | { type: "AssignCombatDamage"; data: { player: PlayerId; attacker_id: ObjectId; total_damage: number; blockers: { blocker_id: ObjectId; lethal_minimum: number }[]; trample: TrampleKind | null; defending_player: PlayerId; attack_target: AttackTarget; pw_loyalty?: number; pw_controller?: PlayerId } }
  | { type: "DistributeAmong"; data: { player: PlayerId; total: number; targets: TargetRef[]; unit: DistributionUnit } }
  | { type: "MoveCountersDistribution"; data: { player: PlayerId; source_id: ObjectId; counter_type?: CounterType | null; available: [CounterType, number][]; destinations: ObjectId[]; pending_effect: unknown } }
  | { type: "ChooseFromZoneChoice"; data: { player: PlayerId; cards: ObjectId[]; count: number; up_to?: boolean; constraint?: ChooseFromZoneConstraint | null; source_id: ObjectId } }
  | { type: "EffectZoneChoice"; data: {
      player: PlayerId;
      cards: ObjectId[];
      count: number;
      min_count?: number;
      up_to?: boolean;
      source_id: ObjectId;
      effect_kind: string;
      zone: Zone;
      destination?: Zone | null;
      enter_tapped?: boolean;
      enter_transformed?: boolean;
      // CR 110.2a: pre-resolved controller override carried through the
      // EffectZoneChoice round-trip. `null`/omitted = no override (object
      // enters under its owner's control).
      enters_under_player?: PlayerId | null;
      enters_attacking?: boolean;
      owner_library?: boolean;
      track_exiled_by_source?: boolean;
    } }
  | { type: "DrawnThisTurnTopdeckChoice"; data: { player: PlayerId; cards: ObjectId[]; count: number; min_count: number; life_payment: number; source_id: ObjectId } }
  | { type: "RetargetChoice"; data: { player: PlayerId; stack_entry_index: number; scope: RetargetScope; current_targets: TargetRef[]; legal_new_targets: TargetRef[] } }
  | { type: "ProliferateChoice"; data: { player: PlayerId; eligible: TargetRef[] } }
  | { type: "ChooseObjectsSelection"; data: { player: PlayerId; eligible: TargetRef[]; trigger_event?: GameEvent } }
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
      // The "who acts" descriptor for this step. `player` above is the
      // SUBJECT being voted-for/labeled.
      //   * `{ type: "SubjectActs" }` — classic Council's-dilemma; the
      //     subject votes for themselves.
      //   * `{ type: "Delegated", data: PlayerId }` — Battlebond friend-
      //     or-foe; a fixed player (the spell controller) casts every
      //     vote while `player` cycles through subjects.
      // Resolve via `data.actor.type === "Delegated" ? data.actor.data
      // : data.player` to get the authorized submitter.
      actor:
        | { type: "SubjectActs" }
        | { type: "Delegated"; data: PlayerId };
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
    } }
  | { type: "CopyRetarget"; data: { player: PlayerId; copy_id: ObjectId; target_slots: CopyTargetSlot[]; current_slot?: number } }
  // CR 700.3 + CR 700.3a: Subject is partitioning their own eligible objects
  // into two piles for an `Effect::SeparateIntoPiles`. `player` is the
  // partitioner (subject); pile B is derived engine-side as
  // `eligible \ pile_a`. `chosen_pile_effect` is opaque to the frontend.
  | { type: "SeparatePilesPartition"; data: {
      player: PlayerId;
      eligible: ObjectId[];
      remaining_subjects: [PlayerId, ObjectId[]][];
      completed: PileResult[];
      chooser: PlayerId;
      source_id: ObjectId;
    } }
  // CR 700.3 + CR 101.4c: Chooser picks pile A or pile B per completed
  // subject partition.
  | { type: "SeparatePilesChoice"; data: {
      player: PlayerId;
      pending: PileResult[];
      current: PileResult;
      source_id: ObjectId;
    } };

// CR 700.3 + CR 700.3a + CR 700.3d: One subject's completed pile partition.
export interface PileResult {
  subject: PlayerId;
  pile_a: ObjectId[];
  pile_b: ObjectId[];
}

// CR 700.3: Identifies one of the two piles produced by a
// `SeparateIntoPiles` partition. Typed enum (no bool) shared by the engine
// handler and the `GameAction::ChoosePile` payload.
export type PileSide =
  | { type: "A" }
  | { type: "B" };

// ── Learn ────────────────────────────────────────────────────────────────

export type LearnOption =
  | { type: "Rummage"; data: { card_id: ObjectId } }
  | { type: "Skip" };

// ── Mulligan ─────────────────────────────────────────────────────────────

// CR 103.5 + 103.5b: Player decision at a MulliganDecision prompt.
//   Keep            — lock in the opening hand (CR 103.5).
//   Mulligan        — shuffle hand back, redraw the starting hand size (CR 103.5).
//   UseSerumPowder  — exile every card from hand including the Powder, redraw
//                     the same number; mulligan counter unchanged (CR 103.5b
//                     + Serum Powder Oracle text). `object_id` must reference
//                     a card named "Serum Powder" in the actor's hand.
export type MulliganChoice =
  | { type: "Keep" }
  | { type: "Mulligan" }
  | { type: "UseSerumPowder"; data: { object_id: ObjectId } };

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

export type DebugTokenRequest =
  | {
      type: "Preset";
      data: {
        preset_id: string;
        owner: PlayerId;
        enter_with_counters?: [CounterType, number][];
      };
    }
  | {
      type: "Custom";
      data: {
        owner: PlayerId;
        characteristics: TokenCharacteristics;
        enter_with_counters?: [CounterType, number][];
      };
    };

export type DebugAction =
  | {
      type: "MoveToZone";
      data: {
        object_id: ObjectId;
        to_zone: Zone;
        library_position?: LibraryPosition;
        simulate?: boolean;
      };
    }
  | {
      type: "CreateCard";
      data: {
        card_name: string;
        owner: PlayerId;
        zone: Zone;
        attach_to?: AttachTarget;
      };
    }
  | { type: "RemoveObject"; data: { object_id: ObjectId } }
  | { type: "DrawCards"; data: { player_id: PlayerId; count: number } }
  | { type: "Mill"; data: { player_id: PlayerId; count: number } }
  | { type: "ShuffleLibrary"; data: { player_id: PlayerId } }
  | { type: "Proliferate"; data: { player_id: PlayerId } }
  | { type: "SetBasePowerToughness"; data: { object_id: ObjectId; power: number | null; toughness: number | null } }
  | { type: "ModifyCounters"; data: { object_id: ObjectId; counter_type: CounterType; delta: number } }
  | { type: "SetTapped"; data: { object_id: ObjectId; tapped: boolean } }
  | { type: "SetController"; data: { object_id: ObjectId; controller: PlayerId } }
  | { type: "SetSummoningSickness"; data: { object_id: ObjectId; sick: boolean } }
  | { type: "SetFaceState"; data: { object_id: ObjectId; face_down?: boolean; transformed?: boolean; flipped?: boolean } }
  | { type: "Attach"; data: { object_id: ObjectId; target: AttachTarget } }
  | { type: "Detach"; data: { object_id: ObjectId } }
  | { type: "GrantKeyword"; data: { object_id: ObjectId; keyword: Keyword } }
  | { type: "RemoveKeyword"; data: { object_id: ObjectId; keyword: Keyword } }
  | { type: "SetLife"; data: { player_id: PlayerId; life: number } }
  | { type: "ModifyPlayerCounters"; data: { player_id: PlayerId; counter_kind: PlayerCounterKind; delta: number } }
  | { type: "ModifyEnergy"; data: { player_id: PlayerId; delta: number } }
  | { type: "AddMana"; data: { player_id: PlayerId; mana: ManaType[] } }
  | { type: "SetPhase"; data: { phase: Phase; active_player: PlayerId } }
  | { type: "RunStateBasedActions" }
  | {
      type: "CreateToken";
      data: {
        request: DebugTokenRequest;
      };
    }
  | { type: "CreateTokenCopy"; data: { source_id: ObjectId; owner: PlayerId } };

export type GameAction =
  | { type: "PassPriority" }
  | { type: "ChooseActivationCostBranch"; data: { index: number } }
  | { type: "PlayLand"; data: { object_id: ObjectId; card_id: CardId } }
  | { type: "CastSpell"; data: { object_id: ObjectId; card_id: CardId; targets: ObjectId[] } }
  | { type: "CastSpellWithPaymentMode"; data: { object_id: ObjectId; card_id: CardId; targets: ObjectId[]; payment_mode: CastPaymentMode } }
  | { type: "Foretell"; data: { object_id: ObjectId; card_id: CardId } }
  | { type: "ActivateAbility"; data: { source_id: ObjectId; ability_index: number } }
  | { type: "DeclareAttackers"; data: { attacks: [ObjectId, AttackTarget][] } }
  | { type: "DeclareBlockers"; data: { assignments: [ObjectId, ObjectId][] } }
  | { type: "MulliganDecision"; data: { choice: MulliganChoice } }
  | { type: "ReorderHand"; data: { order: ObjectId[] } }
  | { type: "TapLandForMana"; data: { object_id: ObjectId } }
  | { type: "UntapLandForMana"; data: { object_id: ObjectId } }
  | { type: "TapForConvoke"; data: { object_id: ObjectId; mana_type: ManaType } }
  | { type: "SelectCards"; data: { cards: ObjectId[] } }
  | { type: "ChooseOutsideGameCards"; data: { selections: OutsideGameSelection[] } }
  | { type: "SelectTargets"; data: { targets: TargetRef[] } }
  | { type: "ChooseTarget"; data: { target: TargetRef | null } }
  | { type: "ChoosePair"; data: { partner: ObjectId | null } }
  | { type: "ChooseReplacement"; data: { index: number } }
  | { type: "OrderTriggers"; data: { order: number[] } }
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
  | { type: "SubmitPilePartition"; data: { pile_a: ObjectId[] } }
  | { type: "ChoosePile"; data: { pile: PileSide } }
  | { type: "ChooseBranch"; data: { index: number } }
  | { type: "ChooseDamageSource"; data: { source: ObjectId } }
  | { type: "SelectModes"; data: { indices: number[] } }
  | { type: "DecideOptionalCost"; data: { pay: boolean } }
  | { type: "ChooseAdventureFace"; data: { creature: boolean } }
  | { type: "ChooseModalFace"; data: { back_face: boolean } }
  | { type: "ChooseAlternativeCast"; data: { choice: { type: "Normal" } | { type: "Alternative" } } }
  | { type: "ChooseCastingVariant"; data: { index: number } }
  | { type: "KeepAllCopyTargets" }
  | { type: "ChoosePermanentTypeSlot"; data: { slot: CoreType } }
  | { type: "CastSpellForFree"; data: { object_id: ObjectId; card_id: CardId; source_id: ObjectId } }
  | { type: "CastSpellForFreeWithPaymentMode"; data: { object_id: ObjectId; card_id: CardId; source_id: ObjectId; payment_mode: CastPaymentMode } }
  | { type: "CastSpellAsMiracle"; data: { object_id: ObjectId; card_id: CardId } }
  | { type: "CastSpellAsMiracleWithPaymentMode"; data: { object_id: ObjectId; card_id: CardId; payment_mode: CastPaymentMode } }
  | { type: "CastSpellAsMadness"; data: { object_id: ObjectId; card_id: CardId } }
  | { type: "CastSpellAsMadnessWithPaymentMode"; data: { object_id: ObjectId; card_id: CardId; payment_mode: CastPaymentMode } }
  // CR 702.190a: Cast a spell from hand via the Sneak alternative cost during
  // the declare-blockers step, returning an unblocked attacker you control.
  // Applies to any card type; CR 702.190b enter-attacking-alongside is
  // handled engine-side for permanent spells only.
  | { type: "CastSpellAsSneak"; data: { hand_object: ObjectId; card_id: CardId; creature_to_return: ObjectId } }
  | { type: "CastSpellAsSneakWithPaymentMode"; data: { hand_object: ObjectId; card_id: CardId; creature_to_return: ObjectId; payment_mode: CastPaymentMode } }
  | { type: "CastSpellAsWebSlinging"; data: { hand_object: ObjectId; card_id: CardId; creature_to_return: ObjectId } }
  | { type: "CastSpellAsWebSlingingWithPaymentMode"; data: { hand_object: ObjectId; card_id: CardId; creature_to_return: ObjectId; payment_mode: CastPaymentMode } }
  | { type: "ActivateNinjutsu"; data: { ninjutsu_object_id: ObjectId; creature_to_return: ObjectId } }
  | { type: "DecideOptionalEffect"; data: { accept: boolean } }
  | { type: "DecideOptionalEffectAndRemember"; data: { choice: AutoMayChoice } }
  | { type: "PayUnlessCost"; data: { pay: boolean } }
  // CR 118.12a: Choose a branch of a disjunctive unless-cost. The
  // discriminant is `Decline` (effect happens) or `Pay { index }` (the
  // selected sub-cost re-enters the standard unless-payment flow).
  | { type: "ChooseUnlessCostBranch"; data: { choice: UnlessCostBranch } }
  | { type: "ChooseRingBearer"; data: { target: ObjectId } }
  | { type: "ChooseLegend"; data: { keep: ObjectId } }
  | { type: "ChooseBattleProtector"; data: { protector: PlayerId } }
  | { type: "PayCombatTax"; data: { accept: boolean } }
  | { type: "ChooseUntap"; data: { object_id: ObjectId; untap: boolean } }
  | { type: "ChooseExert"; data: { exert: boolean } }
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
  | { type: "ChooseCounterMoveDistribution"; data: { selections: CounterMoveChoice[] } }
  | { type: "RetargetSpell"; data: { new_targets: TargetRef[] } }
  | { type: "LearnDecision"; data: { choice: LearnOption } }
  | { type: "ChooseDungeon"; data: { dungeon: DungeonId } }
  | { type: "ChooseDungeonRoom"; data: { room_index: number } }
  | { type: "UnlockRoomDoor"; data: { object_id: ObjectId; door: RoomDoor } }
  | { type: "TapForConvoke"; data: { object_id: ObjectId; mana_type: ManaType } }
  | { type: "SelectCategoryPermanents"; data: { choices: (ObjectId | null)[] } }
  | { type: "ChooseX"; data: { value: number } }
  | { type: "SubmitPayAmount"; data: { amount: number } }
  | { type: "SubmitPhyrexianChoices"; data: { choices: ShardChoice[] } }
  | { type: "ChooseManaColor"; data: { choice: ManaChoice; count?: number } }
  | { type: "PayManaAbilityMana"; data: { payment: ManaType[] } }
  | { type: "CastPreparedCopy"; data: { source: ObjectId } }
  | { type: "CastParadigmCopy"; data: { source: ObjectId } }
  | { type: "PassParadigmOffer" }
  | { type: "Debug"; data: DebugAction }
  | { type: "GrantDebugPermission"; data: { player_id: PlayerId } }
  | { type: "RevokeDebugPermission"; data: { player_id: PlayerId } }
  | { type: "Concede"; data: { player_id: PlayerId } };

// CR 605.3b + CR 106.1a: Shape of the prompt surfaced by WaitingFor::ChooseManaColor.
export type ManaChoicePrompt =
  | { type: "SingleColor"; data: { options: ManaType[] } }
  | { type: "Combination"; data: { options: ManaType[][] } }
  | { type: "AnyCombination"; data: { count: number; options: ManaType[] } };

// CR 605.3b: Player's answer to a ManaChoicePrompt. Shape mirrors the prompt.
export type ManaChoice =
  | { type: "SingleColor"; data: ManaType }
  | { type: "Combination"; data: ManaType[] };

// CR 107.4f + CR 601.2f: Per-shard Phyrexian payment choice.
export type ShardChoice =
  | { type: "PayMana" }
  | { type: "PayLife" };

export type PayableResource =
  | { type: "Energy" }
  | { type: "ManaGeneric"; data: { per_x: number } };

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
  | { type: "AbilityActivated"; data: { player_id: PlayerId; source_id: ObjectId } }
  | { type: "ExhaustAbilityActivated"; data: { player_id: PlayerId; source_id: ObjectId; is_mana_ability: boolean } }
  | { type: "ZoneChanged"; data: { object_id: ObjectId; from: Zone; to: Zone } }
  | { type: "LifeChanged"; data: { player_id: PlayerId; amount: number } }
  | { type: "ManaAdded"; data: { player_id: PlayerId; mana_type: ManaType; source_id: ObjectId; tapped_for_mana?: boolean } }
  | { type: "PermanentTapped"; data: { object_id: ObjectId } }
  | { type: "PlayerLost"; data: { player_id: PlayerId } }
  | { type: "MulliganStarted" }
  | { type: "CardsDrawn"; data: { player_id: PlayerId; count: number } }
  | { type: "CardDrawn"; data: { player_id: PlayerId; object_id: ObjectId; nth_in_turn: number; nth_in_step: number } }
  | { type: "PermanentUntapped"; data: { object_id: ObjectId } }
  | { type: "LandPlayed"; data: { object_id: ObjectId; player_id: PlayerId; from_zone: Zone } }
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
  | { type: "BecomesTarget"; data: { target: TargetRef; source_id: ObjectId } }
  | { type: "ReplacementApplied"; data: { source_id: ObjectId; event_type: string } }
  | { type: "Transformed"; data: { object_id: ObjectId } }
  | { type: "DayNightChanged"; data: { new_state: string } }
  | { type: "TurnedFaceUp"; data: { object_id: ObjectId } }
  | { type: "CardsRevealed"; data: { player: PlayerId; card_ids?: ObjectId[]; card_names: string[] } }
  | { type: "Regenerated"; data: { object_id: ObjectId } }
  | { type: "CreatureSuspected"; data: { object_id: ObjectId } }
  | { type: "Detained"; data: { object_id: ObjectId } }
  | { type: "CaseSolved"; data: { object_id: ObjectId } }
  | { type: "ClassLevelGained"; data: { object_id: ObjectId; level: number } }
  | { type: "RingTemptsYou"; data: { player_id: PlayerId } }
  | { type: "CompanionRevealed"; data: { player: PlayerId; card_name: string } }
  | { type: "CompanionMovedToHand"; data: { player: PlayerId; card_name: string } }
  | { type: "EnergyChanged"; data: { player: PlayerId; delta: number } }
  | { type: "PlayerCounterChanged"; data: { player: PlayerId; counter_kind: PlayerCounterKind; delta: number } }
  | { type: "SpeedChanged"; data: { player: PlayerId; old_speed: number | null; new_speed: number | null } }
  | { type: "CreatureExploited"; data: { exploiter: ObjectId; sacrificed: ObjectId } }
  | { type: "PowerToughnessChanged"; data: { object_id: ObjectId; power: number; toughness: number; power_delta: number; toughness_delta: number } }
  | { type: "RoomEntered"; data: { player_id: PlayerId; dungeon: DungeonId; room_index: number; room_name: string } }
  | { type: "BecomesPlotted"; data: { object_id: ObjectId; player_id: PlayerId } }
  | { type: "DungeonCompleted"; data: { player_id: PlayerId; dungeon: DungeonId } }
  | { type: "InitiativeTaken"; data: { player_id: PlayerId } }
  | { type: "DebugActionUsed"; data: { player_id: PlayerId; description: string } }
  | { type: "DebugPermissionGranted"; data: { host: PlayerId; player_id: PlayerId } }
  | { type: "DebugPermissionRevoked"; data: { host: PlayerId; player_id: PlayerId } };

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
  /**
   * Engine-authored display details keyed by stack entry id. Includes targets,
   * selected paid-cost facts, and public trigger context so stack UI does not
   * infer game logic from raw abilities.
   */
  stack_entry_details?: Record<string, StackEntryDisplay>;
  /**
   * Engine-authored "Auras attached to player X" projection. Players have no
   * `attachments` back-link on the GameObject side because they aren't
   * GameObjects — this map is the FE's only legitimate channel for "which
   * Auras enchant this player." Keyed by PlayerId-as-string per Rust's
   * BTreeMap<PlayerId, _> serde encoding. Empty/omitted when no Auras
   * enchant any player. Mirrors
   * `engine::game::derived_views::DerivedViews::auras_attached_to_player`.
   */
  auras_attached_to_player?: Record<string, ObjectId[]>;
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
  /**
   * CR 601.2f: The locked-in pending cast (cost, ability, object) while the
   * caster is mid-cast. Present during ManaPayment / cost-choice WaitingFor
   * states; the `cost` field is the engine-resolved total (base + Strive +
   * RaiseCost statics + commander tax - reductions). Absent when no cast is
   * in progress.
   */
  pending_cast?: PendingCast;
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
  /**
   * Per-object source attribution for layer-applied continuous effects,
   * rebuilt every layers pass. Maps each affected object's id to the set
   * of `EffectRef`s that contributed grants/modifications/removals to its
   * current characteristics. Display-only — game logic never reads it.
   *
   * Empty objects (no granted effects) are omitted, so most state.attribution
   * lookups for a given object id will be undefined.
   */
  attribution?: Record<string, ObjectAttribution>;
  /**
   * Runtime continuous effects from resolved spells/abilities. The frontend
   * dereferences `EffectRef::Transient` entries here to recover the
   * snapshotted `source_name` (which survives the spell's zone change to
   * the graveyard per CR 400.7) and the granted `ContinuousModification`.
   */
  transient_continuous_effects?: TransientContinuousEffect[];
  seat_order?: PlayerId[];
  format_config?: FormatConfig;
  /**
   * Players granted permission to submit `GameAction.Debug(_)` in a sandbox
   * game. Empty in non-sandbox games. The host (PlayerId(0)) is always seeded
   * into this set at game creation when the format flag is on.
   */
  debug_permitted?: PlayerId[];
  eliminated_players?: PlayerId[];
  public_revealed_cards?: ObjectId[];
  dungeon_progress?: Record<string, { current_dungeon: DungeonId | null; current_room: number; completed: DungeonId[] }>;
  initiative?: PlayerId | null;
  monarch?: PlayerId | null;
  city_blessing?: PlayerId[];
  ring_level?: Record<string, number>;
  ring_bearer?: Record<string, ObjectId | null>;
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
  outside_game_cards_brought_in?: OutsideGameCardUse[];
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
  debug_mode?: boolean;
}

export type AutoPassMode =
  | { type: "UntilStackEmpty"; initial_stack_len: number }
  | { type: "UntilEndOfTurn" };

// ── Source attribution (CR 613 layers) ───────────────────────────────────

/**
 * One CR 613 layer of the continuous-effect pipeline.
 *
 * Mirrors `engine::types::layers::Layer`. Serialized as the variant name
 * string by serde, so this is a plain TypeScript string union — match
 * directly with `"Ability"`, `"ModifyPT"`, etc.
 */
export type AttributionLayer =
  | "Copy"
  | "Control"
  | "Text"
  | "Type"
  | "Color"
  | "Ability"
  | "CharDef"
  | "SetPT"
  | "ModifyPT"
  | "SwitchPT"
  | "CounterPT";

/**
 * Reference to a single `ContinuousModification` that contributed to an
 * object's characteristics. Resolves either to a static ability on a
 * tracked-zone permanent or to a runtime transient effect from a resolved
 * spell/ability.
 *
 * The frontend dereferences a `Static` ref via
 *   state.objects[source].static_definitions[def_index].modifications[mod_index]
 * and a `Transient` ref via
 *   state.transient_continuous_effects.find(t => t.id === id).modifications[mod_index]
 */
export type EffectRef =
  | { type: "Transient"; data: { id: number; mod_index: number } }
  | {
      type: "Static";
      data: { source: ObjectId; def_index: number; mod_index: number };
    };

/**
 * Per-object record of which continuous effects contributed grants /
 * modifications / removals to that object during the last layers pass.
 *
 * Entries within a single layer bucket are in CR 613.7 timestamp order
 * (the engine applies effects timestamp-sorted before recording them).
 */
export interface ObjectAttribution {
  by_layer?: Partial<Record<AttributionLayer, EffectRef[]>>;
}

export interface TransientContinuousEffect {
  id: number;
  source_id: ObjectId;
  controller: PlayerId;
  timestamp: number;
  /** Snapshotted at the originating spell/ability's resolution time. */
  source_name: string;
  /** `ContinuousModification` payloads — opaque to the display layer; the
   *  FE only inspects the discriminant + a small subset of fields. */
  modifications: ContinuousModification[];
}

/**
 * Minimal display-layer shape for the engine's `ContinuousModification`
 * enum. Internally tagged (`#[serde(tag = "type")]`) so variant fields
 * flatten alongside the discriminant. Only the variants the FE currently
 * renders attribution for are typed; everything else falls through the
 * catch-all. Mirrors `engine::types::ability::ContinuousModification`.
 */
export type ContinuousModification =
  | { type: "AddKeyword"; keyword: Keyword }
  | { type: "RemoveKeyword"; keyword: Keyword }
  | { type: "AddPower"; value: number }
  | { type: "AddToughness"; value: number }
  | { type: string; [key: string]: unknown };

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
  BRACKET_ESTIMATION_UNSUPPORTED: "bracket-estimation/unsupported",
  /** Engine rejected game init because one or more decks are not bracket 5 at a cEDH table. */
  BRACKET_VIOLATION: "BRACKET_VIOLATION",
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
   * Engine-grouped per-object actions keyed by `GameAction::source_object()`.
   * May include mana actions omitted from flat `actions`; frontend uses this
   * for "what can I do with this card?" lookups instead of inferring action
   * availability from objects.
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

export interface BatchResolveResult {
  events: GameEvent[];
  waitingFor: WaitingFor;
  logEntries?: GameLogEntry[];
  itemsResolved: number;
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
  getAiAction(difficulty: string, playerId: number, waitingForType?: WaitingFor["type"]): Promise<GameAction | null> | GameAction | null;
  resolveAll?(
    requester: number,
    aiSeats: { playerId: number; difficulty: string }[],
    maxResolutions?: number,
  ): Promise<BatchResolveResult>;
  restoreState(state: GameState): void | Promise<void>;
  dispose(): void;

  /**
   * Estimates a Commander deck's bracket from card contents. Returns null
   * when the deck has no commander, is empty, or the adapter doesn't
   * support local deck analysis (multiplayer adapters throw via
   * `AdapterError` instead of silently returning null).
   *
   * Pure — no game state, no side effects. Safe to call on every deck edit.
   */
  estimateBracket(deck: BracketDeckRequest): Promise<BracketEstimate | null>;
}
