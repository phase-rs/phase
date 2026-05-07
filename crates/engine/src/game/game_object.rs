use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::types::ability::{
    AbilityDefinition, AdditionalCost, BasicLandType, CastVariantPaid, CastingPermission,
    CastingRestriction, ChosenAttribute, ChosenSubtypeKind, ModalChoice, ReplacementDefinition,
    SolveCondition, SpellCastingOption, StaticDefinition, TriggerDefinition,
};
use crate::types::card::{LayoutKind, PrintedCardRef};
use crate::types::card_type::{CardType, CoreType};
use crate::types::counter::CounterType;
use crate::types::definitions::Definitions;
use crate::types::game_state::LKISnapshot;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::{Keyword, KeywordKind};
use crate::types::mana::{ColoredManaCount, ManaColor, ManaCost, ManaPip};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// Image-lookup routing hint for the display layer.
///
/// The frontend uses this to decide whether a `GameObject`'s art should be
/// fetched from the real-card database (Scryfall/MTGJSON entry keyed by name)
/// or from Scryfall's generic-token database. The two are disjoint: a
/// real-card name like "Lightning Bolt" never appears in the token database,
/// and a generic-token name like "Treasure" never appears in the card
/// database. Without this hint the frontend would have to infer routing from
/// `card_id == 0`, conflating "object has no card-database entry" with "art
/// should be looked up as a token" — which is wrong for token-copies of real
/// cards (Twinflame, Helm of the Host, Mirage Mirror, Vaultborn Tyrant LTB,
/// etc.) where `is_token = true` but the art belongs to a real card.
///
/// Independent of `is_token` (which is the CR 111.1 game-rules concept). A
/// token-copy of Bahamut has `is_token = true` AND
/// `display_source = DisplaySource::Card`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DisplaySource {
    /// Image lives in the real-card database (looked up by name).
    /// Default for fresh `GameObject`s including token-copies of real cards.
    #[default]
    Card,
    /// Image lives in Scryfall's generic-token database (Treasure, Spirit
    /// 1/1, Soldier 1/1, Saproling, Incubator, Army, etc.). Set explicitly
    /// at the few token-construction sites that fabricate a token from a
    /// `TokenSpec` rather than copying an existing object.
    Token,
}

/// CR 702.xxx: Prepared-permanent marker payload (Strixhaven).
///
/// Carried as `GameObject::prepared: Option<PreparedState>`. `Some(_)` means
/// the permanent is currently prepared and its controller may cast a copy of
/// its prepare-spell face; `None` means not prepared. The struct is
/// intentionally empty — extensibility (e.g. "prepared since turn N" for
/// future card support) is preserved without promoting the current encoding
/// to a bool. Assign full CR number when WotC publishes SOS CR update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PreparedState;

/// CR 702.103b: Bestow form marker — `Some(_)` while this object has the
/// type-changing effect that turns it into an Aura with "enchant creature".
/// Parallels `PreparedState` — empty struct in `Option` instead of bare `bool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BestowFormState;

/// CR 702.26b / CR 702.26c: Whether a permanent is phased in (normal) or
/// phased out (treated as though it doesn't exist). CR 702.26d: the phasing
/// event doesn't change the object's zone — status is the sole encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(tag = "status")]
pub enum PhaseStatus {
    #[default]
    PhasedIn,
    /// CR 702.26g: A phased-out permanent remembers how it phased out so it
    /// phases back in correctly. Indirectly-phased objects don't phase in on
    /// their own — they ride along with the host they were attached to.
    PhasedOut { cause: PhaseOutCause },
}

impl PhaseStatus {
    pub fn is_phased_in(&self) -> bool {
        matches!(self, PhaseStatus::PhasedIn)
    }

    pub fn is_phased_out(&self) -> bool {
        matches!(self, PhaseStatus::PhasedOut { .. })
    }
}

/// CR 702.26g: How a permanent came to be phased out. Determines whether it
/// phases back in on its own (direct) or alongside the host it was attached
/// to (indirect).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PhaseOutCause {
    /// Phased out via the phasing keyword or an explicit "phase out" effect.
    Directly,
    /// Phased out because an attached-to permanent phased out. CR 702.26g:
    /// won't phase in alone — phases in with its host.
    Indirectly,
}

/// Stored back-face data for double-faced cards (DFCs).
/// Populated when a Transform-layout card enters the game.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackFaceData {
    pub name: String,
    pub power: Option<i32>,
    pub toughness: Option<i32>,
    pub loyalty: Option<u32>,
    /// CR 310.4: Defense of a battle (printed number while off the battlefield).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defense: Option<u32>,
    pub card_types: CardType,
    pub mana_cost: ManaCost,
    pub keywords: Vec<Keyword>,
    pub abilities: Vec<AbilityDefinition>,
    pub trigger_definitions: Definitions<TriggerDefinition>,
    pub replacement_definitions: Definitions<ReplacementDefinition>,
    pub static_definitions: Definitions<StaticDefinition>,
    pub color: Vec<ManaColor>,
    pub printed_ref: Option<PrintedCardRef>,
    pub modal: Option<ModalChoice>,
    pub additional_cost: Option<AdditionalCost>,
    pub strive_cost: Option<ManaCost>,
    pub casting_restrictions: Vec<CastingRestriction>,
    pub casting_options: Vec<SpellCastingOption>,
    /// Source layout kind — distinguishes Modal DFCs from Transform DFCs
    /// so the engine can offer face-choice for MDFCs (CR 712.12).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout_kind: Option<LayoutKind>,
}

/// CR 719.3b: Tracks the solve state of a Case enchantment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseState {
    pub is_solved: bool,
    pub solve_condition: SolveCondition,
}

/// CR 303.4 + CR 301.5: The host an attachment (Aura, Equipment, Fortification)
/// is attached to. Equipment and Fortification can attach only to objects
/// (CR 301.5 / CR 301.6); Auras can attach to objects OR players, depending on
/// the Aura's `Enchant <type>` keyword (CR 303.4 / CR 702.5).
///
/// Storing the host as a typed enum (rather than `Option<ObjectId>` plus a
/// parallel `Option<PlayerId>`) keeps "attached to whom" a single source of
/// truth and lets exhaustive `match` arms force every consumer to handle both
/// variants. Equipment-only call sites use `as_object()` with a CR-cited
/// `expect` to assert the rules invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum AttachTarget {
    /// CR 301.5 / CR 303.4f: attached to a permanent.
    Object(ObjectId),
    /// CR 303.4 + CR 702.5: attached to a player (Curse cycle, Faith's
    /// Fetters-class). Equipment can never be in this variant — CR 301.5
    /// restricts Equipment hosts to creatures.
    Player(PlayerId),
}

impl AttachTarget {
    /// Returns `Some(ObjectId)` for `Object`, `None` for `Player`. Use this at
    /// call sites that have a CR-grounded reason to expect an object host
    /// (e.g., Equipment per CR 301.5) — pair with `.expect("CR …")` to make
    /// the invariant explicit.
    pub fn as_object(&self) -> Option<ObjectId> {
        match self {
            AttachTarget::Object(id) => Some(*id),
            AttachTarget::Player(_) => None,
        }
    }

    /// Returns `Some(PlayerId)` for `Player`, `None` for `Object`. Mirror of
    /// `as_object`; used by player-aura code paths (Curse cycle, SBA CR 704.5n).
    pub fn as_player(&self) -> Option<PlayerId> {
        match self {
            AttachTarget::Player(pid) => Some(*pid),
            AttachTarget::Object(_) => None,
        }
    }
}

impl From<ObjectId> for AttachTarget {
    fn from(id: ObjectId) -> Self {
        AttachTarget::Object(id)
    }
}

/// CR 709.5c: Which half, or door, of a shared-type-line split permanent is
/// being locked or unlocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RoomDoor {
    Left,
    Right,
}

/// CR 709.5c: Unlocked designations carried by a Room permanent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomUnlockState {
    #[serde(default)]
    pub left_unlocked: bool,
    #[serde(default)]
    pub right_unlocked: bool,
}

impl RoomUnlockState {
    pub fn is_unlocked(&self, door: RoomDoor) -> bool {
        match door {
            RoomDoor::Left => self.left_unlocked,
            RoomDoor::Right => self.right_unlocked,
        }
    }

    pub fn unlock(&mut self, door: RoomDoor) -> RoomUnlockOutcome {
        let was_unlocked = self.is_unlocked(door);
        let was_fully_unlocked = self.left_unlocked && self.right_unlocked;
        match door {
            RoomDoor::Left => self.left_unlocked = true,
            RoomDoor::Right => self.right_unlocked = true,
        }
        RoomUnlockOutcome {
            changed: !was_unlocked,
            fully_unlocked: !was_fully_unlocked && self.left_unlocked && self.right_unlocked,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoomUnlockOutcome {
    pub changed: bool,
    pub fully_unlocked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameObject {
    pub id: ObjectId,
    pub card_id: CardId,
    pub owner: PlayerId,
    pub controller: PlayerId,
    pub zone: Zone,

    // Battlefield state
    pub tapped: bool,
    pub face_down: bool,
    pub flipped: bool,
    pub transformed: bool,

    // Combat
    pub damage_marked: u32,
    pub dealt_deathtouch_damage: bool,

    // Attachments
    /// CR 303.4 + CR 301.5: Host this attachment is attached to.
    /// `None` if unattached. See `AttachTarget` for variants.
    pub attached_to: Option<AttachTarget>,
    pub attachments: Vec<ObjectId>,

    // Counters
    pub counters: HashMap<CounterType, u32>,

    // Characteristics
    pub name: String,
    pub power: Option<i32>,
    pub toughness: Option<i32>,
    pub loyalty: Option<u32>,
    /// CR 310.4c: Defense of a battle on the battlefield — derived from defense
    /// counters. Kept in sync with `CounterType::Defense` by layer evaluation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defense: Option<u32>,
    pub card_types: CardType,
    pub mana_cost: ManaCost,
    pub keywords: Vec<Keyword>,
    /// Live abilities after layer evaluation. Wrapped in `Arc<Vec<_>>` so
    /// `GameState::clone()` shares the ability list across cloned states
    /// (AI search); mutations go through `Arc::make_mut` for copy-on-write.
    pub abilities: Arc<Vec<AbilityDefinition>>,
    pub trigger_definitions: Definitions<TriggerDefinition>,
    pub replacement_definitions: Definitions<ReplacementDefinition>,
    pub static_definitions: Definitions<StaticDefinition>,
    pub color: Vec<ManaColor>,
    pub printed_ref: Option<PrintedCardRef>,

    // Back face data for double-faced cards (DFCs)
    pub back_face: Option<BackFaceData>,

    // Base characteristics (for layer system)
    pub base_power: Option<i32>,
    pub base_toughness: Option<i32>,
    #[serde(default)]
    pub base_name: String,
    #[serde(default)]
    pub base_loyalty: Option<u32>,
    /// CR 310.4a: Printed defense number (off-battlefield defense).
    #[serde(default)]
    pub base_defense: Option<u32>,
    pub base_card_types: CardType,
    #[serde(default)]
    pub base_mana_cost: ManaCost,
    pub base_keywords: Vec<Keyword>,
    /// CR 613.1: Printed baseline abilities. Wrapped in `Arc<Vec<_>>` so
    /// `GameState::clone()` (called constantly by the AI search) shares
    /// the printed-card slice instead of deep-cloning it per search node.
    /// Writes use `Arc::make_mut` for copy-on-write semantics.
    pub base_abilities: Arc<Vec<AbilityDefinition>>,
    /// CR 613.1: Printed baselines captured at `GameObject` construction —
    /// the values on the card (or defined by the effect that created this
    /// object) before any continuous effects apply. They are rebuilt, not
    /// runtime-mutated, so they intentionally use plain `Vec<T>` rather
    /// than the `Definitions<T>` wrapper that gates live reads.
    /// Wrapped in `Arc` for structural sharing across cloned `GameState`s.
    pub base_trigger_definitions: Arc<Vec<TriggerDefinition>>,
    /// CR 613.1: printed-card baseline for replacement definitions. See
    /// `base_trigger_definitions`.
    pub base_replacement_definitions: Arc<Vec<ReplacementDefinition>>,
    /// CR 613.1: printed-card baseline for static definitions. See
    /// `base_trigger_definitions`.
    pub base_static_definitions: Arc<Vec<StaticDefinition>>,
    pub base_color: Vec<ManaColor>,
    #[serde(default)]
    pub base_characteristics_initialized: bool,

    // Timestamp for layer ordering
    pub timestamp: u64,

    // CR 603.6a: Turn on which this object entered the battlefield (global turn
    // counter). Used for "entered this turn" triggers and `EnteredThisTurn`
    // filters — NOT for summoning-sickness (see `summoning_sick`).
    pub entered_battlefield_turn: Option<u32>,

    /// CR 302.6: Summoning-sickness state flag. True when this permanent has
    /// NOT been continuously under its controller's control since that player's
    /// most recent turn began — i.e., it can't attack or pay `{T}`/`{Q}` costs
    /// (haste overrides at query time). Event-driven: set true on ETB; cleared
    /// to false at the start of controller's next turn (see `start_next_turn`).
    /// Query via `combat::has_summoning_sickness` which folds in Haste +
    /// non-creature short-circuits.
    #[serde(default)]
    pub summoning_sick: bool,

    /// CR 702.30a: Echo triggers at the controller's next upkeep after this
    /// permanent came under their control, then never again for the same object.
    #[serde(default)]
    pub echo_due: bool,

    /// CR 702.49 + CR 702.190a: Which alt-cost cast/activation variant was paid to put this
    /// permanent onto the battlefield, and on which turn. Used by trigger conditions and
    /// ability conditions that check "if its sneak/ninjutsu cost was paid this turn."
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cast_variant_paid: Option<(CastVariantPaid, u32)>,

    /// CR 107.3m: The value of X paid when the spell that produced this object
    /// was cast. Populated by `finalize_cast` from the pending ability's
    /// `chosen_x` and survives the stack → battlefield transition so that
    /// ETB replacement effects ("enters with X counters") and ETB triggered
    /// abilities that refer to X resolve against the actual paid amount.
    /// Resolved via `QuantityRef::CostXPaid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_x_paid: Option<u32>,

    /// CR 702.33d + CR 702.33f: Kicker payments declared while casting the
    /// spell that produced this permanent, in payment order. Mirrors
    /// `SpellContext.kickers_paid`; copied at cast resolution from the
    /// resolving spell's ability context so ETB replacement effects
    /// (`ReplacementCondition::CastViaKicker`) and ETB triggered abilities
    /// (`AbilityCondition::AdditionalCostPaid` with kicker variant or
    /// `min_count >= 2`) can evaluate against the paid kicker(s) after the
    /// spell has left the stack.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kickers_paid: Vec<crate::types::ability::KickerVariant>,
    /// CR 702.51c: Creatures tapped to pay the convoke cost of the spell that
    /// produced this object. Stored as object ids so future convoke-reference
    /// classes can inspect identity; `QuantityRef::ConvokedCreatureCount`
    /// currently resolves the count.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub convoked_creatures: Vec<ObjectId>,

    /// CR 702.103b + CR 702.103f: `Some(_)` while this object is in the
    /// "bestowed Aura" form. Set by `apply_bestow_aura_form`; cleared per
    /// CR 702.103e–g (illegal target, unattach, zone exit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bestow_form: Option<BestowFormState>,

    // Coverage: lists unimplemented mechanics (computed for serialization, not persisted)
    #[serde(skip_deserializing, default, skip_serializing_if = "Vec::is_empty")]
    pub unimplemented_mechanics: Vec<String>,

    // Derived field: true when this creature can't attack/block due to summoning sickness.
    // Computed before serialization, not persisted.
    #[serde(skip_deserializing, default)]
    pub has_summoning_sickness: bool,

    // Derived field: devotion count for cards that reference devotion.
    // Computed before serialization based on DevotionColors in static params.
    #[serde(skip_deserializing, default, skip_serializing_if = "Option::is_none")]
    pub devotion: Option<u32>,

    // Derived field: true when this permanent has an activatable mana ability.
    // Computed before serialization, not persisted.
    #[serde(skip_deserializing, default)]
    pub has_mana_ability: bool,

    // Derived field: ability index of the first mana ability, for frontend dispatch.
    // Computed before serialization, not persisted.
    #[serde(skip_deserializing, default, skip_serializing_if = "Option::is_none")]
    pub mana_ability_index: Option<usize>,

    // Derived field: currently available mana pips for this object — typed
    // projection of every applicable `ManaProduction` variant. Always
    // serialized (even when empty) so the frontend can distinguish
    // "no producers" from "field absent" on the wire. Derived per-tick by
    // `display_land_mana_pips` from the source's mana abilities + activation
    // constraints.
    #[serde(skip_deserializing, default)]
    pub available_mana_pips: Vec<ManaPip>,

    // Planeswalker: whether a loyalty ability has been activated this turn
    #[serde(skip_deserializing, default)]
    pub loyalty_activated_this_turn: bool,

    // Commander: whether this object is a commander card
    #[serde(default)]
    pub is_commander: bool,

    /// CR 903.8: Commander tax — pre-computed {2} per previous cast from command zone.
    /// Display-only: computed by `derive_display_state()`.
    #[serde(skip_deserializing, default, skip_serializing_if = "Option::is_none")]
    pub commander_tax: Option<u32>,

    /// CR 702.112a: Whether this creature has become renowned.
    /// Set to true when renown triggers (damage dealt while not yet renowned).
    #[serde(default)]
    pub is_renowned: bool,

    /// CR 114.5: Whether this object is an emblem (immune to removal, persists in command zone)
    #[serde(default)]
    pub is_emblem: bool,

    /// CR 111.1: Whether this object is a token (not a card).
    #[serde(default)]
    pub is_token: bool,

    /// Image-lookup routing hint for the display layer. See `DisplaySource`
    /// for the rationale. Independent of `is_token` — a token-copy of a
    /// real card carries `is_token = true` AND `DisplaySource::Card`.
    #[serde(default)]
    pub display_source: DisplaySource,

    /// Modal spell metadata ("Choose one —", etc.). Copied from CardFace at load time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modal: Option<ModalChoice>,

    /// Additional casting cost. Copied from CardFace at load time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_cost: Option<AdditionalCost>,

    /// CR 207.2c + CR 601.2f: Strive per-target surcharge cost. Copied from CardFace at load time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strive_cost: Option<ManaCost>,

    /// Spell-casting restrictions. Copied from CardFace at load time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub casting_restrictions: Vec<CastingRestriction>,

    /// Spell-casting options. Copied from CardFace at load time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub casting_options: Vec<SpellCastingOption>,

    /// CR 715.3d: Runtime casting permissions (e.g., Adventure creature castable from exile).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub casting_permissions: Vec<CastingPermission>,

    /// CR 702.143c-d: Whether this card in exile is foretold. Cleared when
    /// the card leaves exile because a zone change creates a new object.
    #[serde(default)]
    pub foretold: bool,

    /// Choices made as this permanent entered (e.g., "choose a color").
    /// Persists for the object's lifetime on the battlefield.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chosen_attributes: Vec<ChosenAttribute>,

    /// CR 701.15c: Which players have goaded this creature. A goaded creature must attack
    /// each combat if able and must attack a player other than the goading player(s) if able.
    /// Multiple players can goad the same creature, creating additional combat requirements.
    #[serde(default, skip_serializing_if = "std::collections::HashSet::is_empty")]
    pub goaded_by: std::collections::HashSet<PlayerId>,

    /// CR 701.35a: Which players have detained this permanent. A detained permanent
    /// can't attack or block and its activated abilities can't be activated until the
    /// detaining player's next turn. Cleared during layer evaluation like goaded_by.
    #[serde(default, skip_serializing_if = "std::collections::HashSet::is_empty")]
    pub detained_by: std::collections::HashSet<PlayerId>,

    /// CR 701.60a: Whether this creature is currently suspected.
    /// The designation is the source of truth; menace and CantBlock are derived
    /// via `base_keywords`/`base_static_definitions` (Option C architecture).
    #[serde(default)]
    pub is_suspected: bool,

    /// CR 701.37b: Monstrous designation. Stays until the permanent leaves the battlefield.
    /// Not an ability or copiable value — purely a marker for monstrosity and related abilities.
    #[serde(default)]
    pub monstrous: bool,

    /// CR 702.xxx: Prepared (Strixhaven) designation. Present only on a
    /// permanent whose printed-card layout is `CardLayout::Prepare(a, b)`.
    /// While prepared, the controller may activate a synthesized priority-time
    /// cast-offer that creates a token spell-copy of face `b` on the stack
    /// (CR 707.10 copy semantics); casting unprepares (reminder text: "Doing
    /// so unprepares it."). Cleared by `reset_for_battlefield_exit` (CR 400.7 —
    /// a permanent that leaves the battlefield becomes a new object with no
    /// memory of its previous existence). `Option<PreparedState>` over a bool
    /// per project idiom (no bool flags). Assign when WotC publishes SOS CR
    /// update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared: Option<PreparedState>,

    /// CR 702.171b: Saddled designation. A permanent stays saddled until the end
    /// of the turn or it leaves the battlefield. Not a copiable value — purely
    /// a marker for saddle-triggered abilities and "saddled Mount" filters.
    #[serde(default)]
    pub is_saddled: bool,

    /// CR 613.11 + CR 510.1a: This creature assigns combat damage equal to its
    /// toughness rather than its power. Set after object-characteristic layers.
    #[serde(default)]
    pub assigns_damage_from_toughness: bool,

    /// CR 510.1c: This creature assigns combat damage as though it weren't blocked.
    /// Set after object-characteristic layers.
    #[serde(default)]
    pub assigns_damage_as_though_unblocked: bool,

    /// CR 510.1a: This creature assigns no combat damage.
    /// Set after object-characteristic layers (e.g., "~ assigns no combat damage").
    #[serde(default)]
    pub assigns_no_combat_damage: bool,

    /// CR 719.3b: Case enchantment solve state. Present only on Case permanents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub case_state: Option<CaseState>,

    /// CR 709.5c: Unlocked door designations for shared-type-line Room
    /// permanents. Present only on permanents with the Room subtype.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_unlocks: Option<RoomUnlockState>,

    /// CR 716.3: Class enchantment level. Present only on Class permanents.
    /// Class level is NOT a counter (CR 716) — proliferate/counter manipulation must not interact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class_level: Option<u8>,

    /// CR 400.7d: Transient field tracking the zone a spell was cast from.
    /// Set when a spell resolves to a permanent; consumed by ETB trigger processing
    /// to evaluate conditions like "if you cast it from your hand".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cast_from_zone: Option<Zone>,

    /// CR 601.2h: Whether mana was actually spent to cast this object.
    /// Set during casting finalization when mana is paid. Used for trigger conditions
    /// like "if no mana was spent to cast it" (e.g., Satoru, the Infiltrator).
    #[serde(default)]
    pub mana_spent_to_cast: bool,

    /// CR 601.2h: Per-color breakdown of mana spent to cast this object.
    /// Populated during casting finalization; consumed by trigger conditions
    /// like Adamant (CR 207.2c). Cleared in lockstep with `mana_spent_to_cast`
    /// (see `triggers::clear_transient_cast_state`).
    #[serde(default, skip_serializing_if = "ColoredManaCount::is_empty")]
    pub colors_spent_to_cast: ColoredManaCount,

    /// CR 601.2h: Total amount of mana actually spent to cast this object
    /// (sum across all colors and generic). Populated during casting
    /// finalization alongside `mana_spent_to_cast` and `colors_spent_to_cast`.
    /// Consumed by spent-mana quantity refs for intervening-if
    /// comparisons (Increment, CR 603.4) and self-referential spell effects
    /// for spell-resolution effects that read their own cost (Molten Note,
    /// "deals damage equal to the amount of mana spent to cast this spell").
    ///
    /// Unlike `mana_spent_to_cast` / `colors_spent_to_cast`, this field is NOT
    /// cleared after trigger collection — it is a historical fact about the
    /// object that remains valid through spell resolution and beyond. Set once
    /// at cast finalization; initialized to 0 by `GameObject::new`.
    #[serde(default, skip_serializing_if = "is_zero_u32_field")]
    pub mana_spent_to_cast_amount: u32,

    /// CR 106.3 + CR 601.2h: Source snapshots for each mana spent to cast this
    /// object. One entry per spent mana lets source-qualified dynamic quantities
    /// count "mana from a Cave/Treasure/artifact source" without depending on
    /// the mana source still existing or retaining the same characteristics.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mana_spent_source_snapshots: Vec<crate::types::game_state::ManaSpentSourceSnapshot>,

    /// CR 702.26b / CR 702.26d: Phasing status. A phased-out permanent stays
    /// on the battlefield but is treated as though it doesn't exist for almost
    /// all rules queries. Defaults to `PhasedIn` for replay compatibility.
    #[serde(default)]
    pub phase_status: PhaseStatus,
}

impl GameObject {
    /// CR 603.10 + CR 400.7: Snapshot this object's public characteristics
    /// for a zone-change event. The record captures state *at the moment of
    /// the move* so zone-change trigger filters and past-tense conditions
    /// evaluate against the event-time object, not its post-move shape.
    pub fn snapshot_for_zone_change(
        &self,
        object_id: ObjectId,
        from: Option<Zone>,
        to: Zone,
    ) -> crate::types::game_state::ZoneChangeRecord {
        crate::types::game_state::ZoneChangeRecord {
            object_id,
            name: self.name.clone(),
            core_types: self.card_types.core_types.clone(),
            subtypes: self.card_types.subtypes.clone(),
            supertypes: self.card_types.supertypes.clone(),
            keywords: self.keywords.clone(),
            power: self.power,
            toughness: self.toughness,
            colors: self.color.clone(),
            mana_value: self.mana_cost.mana_value(),
            controller: self.controller,
            owner: self.owner,
            from_zone: from,
            to_zone: to,
            attachments: Vec::new(),
            linked_exile_snapshot: Vec::new(),
            // CR 111.1: Token-ness is a stable identity of the object,
            // snapshotted for post-LTB trigger-filter evaluation (e.g.,
            // "whenever a creature token dies").
            is_token: self.is_token,
            combat_status: Default::default(),
        }
    }

    pub fn sync_missing_base_characteristics(&mut self) {
        if self.base_characteristics_initialized {
            return;
        }

        if self.base_power.is_none() && self.power.is_some() {
            self.base_power = self.power;
        }
        if self.base_toughness.is_none() && self.toughness.is_some() {
            self.base_toughness = self.toughness;
        }
        if self.base_loyalty.is_none() && self.loyalty.is_some() {
            self.base_loyalty = self.loyalty;
        }
        if self.base_card_types == CardType::default() && self.card_types != CardType::default() {
            self.base_card_types = self.card_types.clone();
        }
        if self.base_mana_cost == ManaCost::default() && self.mana_cost != ManaCost::default() {
            self.base_mana_cost = self.mana_cost.clone();
        }
        if self.base_keywords.is_empty() && !self.keywords.is_empty() {
            self.base_keywords = self.keywords.clone();
        }
        if self.base_abilities.is_empty() && !self.abilities.is_empty() {
            // Both sides are `Arc<Vec<_>>` — refcount-only clone.
            self.base_abilities = Arc::clone(&self.abilities);
        }
        if self.base_trigger_definitions.is_empty() && !self.trigger_definitions.is_empty() {
            self.base_trigger_definitions =
                Arc::new(self.trigger_definitions.iter_all().cloned().collect());
        }
        if self.base_replacement_definitions.is_empty() && !self.replacement_definitions.is_empty()
        {
            self.base_replacement_definitions =
                Arc::new(self.replacement_definitions.iter_all().cloned().collect());
        }
        if self.base_static_definitions.is_empty() && !self.static_definitions.is_empty() {
            self.base_static_definitions =
                Arc::new(self.static_definitions.iter_all().cloned().collect());
        }
        if self.base_color.is_empty() && !self.color.is_empty() {
            self.base_color = self.color.clone();
        }

        self.base_characteristics_initialized = true;
    }

    pub fn new(id: ObjectId, card_id: CardId, owner: PlayerId, name: String, zone: Zone) -> Self {
        GameObject {
            id,
            card_id,
            owner,
            controller: owner,
            zone,
            tapped: false,
            face_down: false,
            flipped: false,
            transformed: false,
            damage_marked: 0,
            dealt_deathtouch_damage: false,
            attached_to: None,
            attachments: Vec::new(),
            counters: HashMap::new(),
            name: name.clone(),
            power: None,
            toughness: None,
            loyalty: None,
            defense: None,
            card_types: CardType::default(),
            mana_cost: ManaCost::default(),
            keywords: Vec::new(),
            abilities: Arc::new(Vec::new()),
            trigger_definitions: Definitions::default(),
            replacement_definitions: Definitions::default(),
            static_definitions: Definitions::default(),
            color: Vec::new(),
            printed_ref: None,
            back_face: None,
            base_power: None,
            base_toughness: None,
            base_name: name.clone(),
            base_loyalty: None,
            base_defense: None,
            base_card_types: CardType::default(),
            base_mana_cost: ManaCost::default(),
            base_keywords: Vec::new(),
            base_abilities: Arc::new(Vec::new()),
            base_trigger_definitions: Default::default(),
            base_replacement_definitions: Default::default(),
            base_static_definitions: Default::default(),
            base_color: Vec::new(),
            base_characteristics_initialized: false,
            timestamp: 0,
            entered_battlefield_turn: None,
            summoning_sick: false,
            echo_due: false,
            cast_variant_paid: None,
            cost_x_paid: None,
            kickers_paid: Vec::new(),
            convoked_creatures: Vec::new(),
            bestow_form: None,
            unimplemented_mechanics: Vec::new(),
            has_summoning_sickness: false,
            has_mana_ability: false,
            mana_ability_index: None,
            devotion: None,
            available_mana_pips: Vec::new(),
            loyalty_activated_this_turn: false,
            is_commander: false,
            commander_tax: None,
            is_renowned: false,
            is_emblem: false,
            is_token: false,
            display_source: DisplaySource::Card,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: Vec::new(),
            casting_options: Vec::new(),
            casting_permissions: Vec::new(),
            foretold: false,
            chosen_attributes: Vec::new(),
            goaded_by: std::collections::HashSet::new(),
            detained_by: std::collections::HashSet::new(),
            is_suspected: false,
            monstrous: false,
            prepared: None,
            is_saddled: false,
            assigns_damage_from_toughness: false,
            assigns_damage_as_though_unblocked: false,
            assigns_no_combat_damage: false,
            case_state: None,
            room_unlocks: None,
            class_level: None,
            cast_from_zone: None,
            mana_spent_to_cast: false,
            colors_spent_to_cast: ColoredManaCount::default(),
            mana_spent_to_cast_amount: 0,
            mana_spent_source_snapshots: Vec::new(),
            phase_status: PhaseStatus::PhasedIn,
        }
    }

    /// CR 106.3 + CR 601.2h: Capture the public source characteristics needed
    /// by source-qualified "mana spent to cast" effects.
    pub fn snapshot_for_mana_spent(&self) -> LKISnapshot {
        LKISnapshot {
            name: self.name.clone(),
            power: self.power,
            toughness: self.toughness,
            mana_value: self.mana_cost.mana_value(),
            controller: self.controller,
            owner: self.owner,
            card_types: self.card_types.core_types.clone(),
            subtypes: self.card_types.subtypes.clone(),
            supertypes: self.card_types.supertypes.clone(),
            keywords: self.keywords.clone(),
            colors: self.color.clone(),
            counters: self.counters.clone(),
        }
    }

    /// CR 400.7: Reset transient battlefield state when a permanent enters the battlefield.
    /// A permanent entering the battlefield is a new object with no memory of its previous
    /// existence. Callers that need enter_tapped=true override `tapped` after this call.
    pub fn reset_for_battlefield_entry(&mut self, turn_number: u32) {
        self.entered_battlefield_turn = Some(turn_number);
        // CR 302.6: A permanent that enters the battlefield has not been
        // continuously under its controller's control since that player's
        // most recent turn began. Cleared at controller's next turn start
        // (see `turns::start_next_turn`). Haste is folded in at query time
        // by `combat::has_summoning_sickness`, so the flag is set
        // unconditionally here; the query short-circuits for non-creatures.
        self.summoning_sick = true;
        self.echo_due = self
            .keywords
            .iter()
            .any(|kw| matches!(kw, Keyword::Echo(_)));
        self.tapped = false;
        self.damage_marked = 0;
        self.dealt_deathtouch_damage = false;
        self.loyalty_activated_this_turn = false;
        self.is_suspected = false;
        self.is_renowned = false;
        self.monstrous = false;
        self.foretold = false;
        // CR 702.xxx: Prepared (Strixhaven) is a new-object-on-entry reset, per
        // CR 400.7. A re-entering permanent has no memory of a prior prepared
        // state. Assign when WotC publishes SOS CR update.
        self.prepared = None;
        self.is_saddled = false;
        self.chosen_attributes.clear();
        self.cast_variant_paid = None;
        // CR 400.7 + CR 702.33d: kicker payments are bound to the casting
        // event that produced this object. A re-entering permanent has no
        // memory of prior kicker payments — clear before the cast resolution
        // path repopulates from the resolving spell's `SpellContext`.
        self.kickers_paid.clear();
        // CR 400.7 + CR 702.51c: convoked-creature history is tied to the
        // spell-resolution event that created this object. A re-entering
        // permanent has no memory of a prior convoke payment.
        self.convoked_creatures.clear();
        self.goaded_by.clear();
        self.detained_by.clear();

        // CR 400.7: A Class that re-enters is a new object at level 1.
        if self.class_level.is_some() {
            self.class_level = Some(1);
        }
        // CR 719.3b: Solved designation stays until it leaves the battlefield.
        if let Some(ref mut cs) = self.case_state {
            cs.is_solved = false;
        }
        if self.card_types.subtypes.iter().any(|s| s == "Room") {
            self.room_unlocks = Some(RoomUnlockState::default());
        }
    }

    /// CR 400.7: Clear battlefield-only designations when a permanent leaves the battlefield.
    /// Separate from entry reset because some state (counters, transform) is already handled
    /// by `apply_zone_exit_cleanup` in zones.rs.
    pub fn reset_for_battlefield_exit(&mut self) {
        // CR 701.37b: Monstrous designation clears when a permanent leaves the battlefield.
        self.monstrous = false;
        // CR 701.15a / CR 701.35a: Goad and detain are battlefield-only designations.
        self.goaded_by.clear();
        self.detained_by.clear();
        // CR 701.60a / CR 702.112b: Suspect and renowned are battlefield designations.
        self.is_suspected = false;
        self.is_renowned = false;
        // CR 702.171b: Saddled clears when the Mount leaves the battlefield.
        self.is_saddled = false;
        // CR 702.xxx: Prepared (Strixhaven) is a battlefield-only designation —
        // clears on BF exit, paralleling monstrous/suspected. CR 400.7: a
        // re-entering permanent is a new object with no memory of its previous
        // prepared state. Assign when WotC publishes SOS CR update.
        self.prepared = None;
        // CR 107.3m: The paid-X value is tied to the spell-resolution that brought
        // this permanent to the battlefield. When the permanent leaves, the value
        // is no longer meaningful; a re-cast will re-populate it via `finalize_cast`.
        self.cost_x_paid = None;
        self.convoked_creatures.clear();
        // CR 702.103f: `bestow_form` is intentionally NOT cleared here.
        // The zone-exit cleanup in `apply_zone_exit_cleanup` (zones.rs) reads
        // the flag to decide whether to revert the bestow type-changing effect
        // (re-add Creature core type, drop synthesized Aura subtype + enchant
        // creature keyword) — clearing it here would leave the GY/exile object
        // stuck in Aura form because the revert block would skip it. The
        // SBA path (CR 702.103f override) handles the in-place battlefield
        // revert explicitly.
        self.room_unlocks = None;
    }

    /// Check if this object has a specific keyword, using discriminant-based matching.
    pub fn has_keyword(&self, keyword: &Keyword) -> bool {
        super::keywords::has_keyword(self, keyword)
    }

    /// CR 702.26b: Whether this object is currently phased in (normal state).
    pub fn is_phased_in(&self) -> bool {
        self.phase_status.is_phased_in()
    }

    /// CR 702.26b: Whether this object is currently phased out (treated as
    /// though it doesn't exist for almost all rules queries).
    pub fn is_phased_out(&self) -> bool {
        self.phase_status.is_phased_out()
    }

    pub fn has_keyword_kind(&self, kind: KeywordKind) -> bool {
        super::keywords::has_keyword_kind(self, kind)
    }

    /// Check if this object uses any mechanics the engine cannot handle.
    pub fn has_unimplemented_mechanics(&self) -> bool {
        !super::coverage::unimplemented_mechanics(self).is_empty()
    }

    /// Look up a stored choice by category.
    pub fn chosen_color(&self) -> Option<ManaColor> {
        self.chosen_attributes.iter().find_map(|a| match a {
            ChosenAttribute::Color(c) => Some(*c),
            _ => None,
        })
    }

    /// Look up a stored basic land type choice.
    pub fn chosen_basic_land_type(&self) -> Option<BasicLandType> {
        self.chosen_attributes.iter().find_map(|a| match a {
            ChosenAttribute::BasicLandType(t) => Some(*t),
            _ => None,
        })
    }

    /// Look up a stored creature type choice.
    pub fn chosen_creature_type(&self) -> Option<&str> {
        self.chosen_attributes.iter().find_map(|a| match a {
            ChosenAttribute::CreatureType(s) => Some(s.as_str()),
            _ => None,
        })
    }

    /// Look up a stored chosen number (e.g., Talion's "choose a number").
    pub fn chosen_number(&self) -> Option<u8> {
        self.chosen_attributes.iter().find_map(|a| match a {
            ChosenAttribute::Number(n) => Some(*n),
            _ => None,
        })
    }

    /// CR 310.8a + CR 310.8e: Return this battle's protector, if any. Derived
    /// from `ChosenAttribute::Player` stored when the Siege's "As ~ enters"
    /// replacement resolved. Non-battle permanents return `None`.
    pub fn protector(&self) -> Option<PlayerId> {
        if !self.card_types.core_types.contains(&CoreType::Battle) {
            return None;
        }
        self.chosen_attributes.iter().find_map(|a| match a {
            ChosenAttribute::Player(p) => Some(*p),
            _ => None,
        })
    }

    /// CR 714.1: Returns the final chapter number for a Saga, or None if not a Saga.
    /// Derived at runtime from the maximum threshold in the trigger definitions' counter filters.
    pub fn final_chapter_number(&self) -> Option<u32> {
        if !self.card_types.subtypes.iter().any(|s| s == "Saga") {
            return None;
        }
        // Structural scan of this Saga's own triggers — intrinsic to the
        // card, not subject to functioning gates. `iter_all` is pub(crate).
        self.trigger_definitions
            .iter_all()
            .filter_map(|t| t.counter_filter.as_ref().and_then(|f| f.threshold))
            .max()
    }

    /// CR 702.51a: Whether this object can be tapped for convoke/waterbend mana.
    /// Requires: on battlefield, untapped, creature or artifact, controlled by `player`.
    pub fn is_convoke_eligible(&self, player: PlayerId) -> bool {
        self.controller == player
            && self.zone == Zone::Battlefield
            && !self.tapped
            && (self.card_types.core_types.contains(&CoreType::Creature)
                || self.card_types.core_types.contains(&CoreType::Artifact))
    }

    /// Get the chosen subtype as a string, unified across creature types and basic land types.
    /// Used by the layer system's `AddChosenSubtype` modification.
    pub fn chosen_subtype_str(&self, kind: &ChosenSubtypeKind) -> Option<String> {
        match kind {
            ChosenSubtypeKind::CreatureType => self.chosen_creature_type().map(|s| s.to_string()),
            ChosenSubtypeKind::BasicLandType => self
                .chosen_basic_land_type()
                .map(|t| t.as_subtype_str().to_string()),
        }
    }
}

/// Serde helper: skip serialization when a `u32` field is zero.
fn is_zero_u32_field(n: &u32) -> bool {
    *n == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::counter::parse_counter_type;

    #[test]
    fn game_object_has_all_rules_relevant_fields() {
        let obj = GameObject::new(
            ObjectId(1),
            CardId(100),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );

        assert_eq!(obj.id, ObjectId(1));
        assert_eq!(obj.card_id, CardId(100));
        assert_eq!(obj.owner, PlayerId(0));
        assert_eq!(obj.controller, PlayerId(0));
        assert_eq!(obj.zone, Zone::Hand);
        assert!(!obj.tapped);
        assert!(!obj.face_down);
        assert!(!obj.flipped);
        assert!(!obj.transformed);
        assert_eq!(obj.damage_marked, 0);
        assert!(!obj.dealt_deathtouch_damage);
        assert!(obj.attached_to.is_none());
        assert!(obj.attachments.is_empty());
        assert!(obj.counters.is_empty());
        assert_eq!(obj.name, "Lightning Bolt");
        assert!(obj.power.is_none());
        assert!(obj.toughness.is_none());
        assert!(obj.loyalty.is_none());
        assert!(obj.keywords.is_empty());
        assert!(obj.abilities.is_empty());
        assert!(obj.color.is_empty());
        assert!(obj.entered_battlefield_turn.is_none());
    }

    #[test]
    fn counter_type_covers_required_variants() {
        let counters = [
            CounterType::Plus1Plus1,
            CounterType::Minus1Minus1,
            CounterType::Loyalty,
            CounterType::Generic("charge".to_string()),
        ];
        assert_eq!(counters.len(), 4);
    }

    #[test]
    fn game_object_serializes_and_roundtrips() {
        let obj = GameObject::new(
            ObjectId(1),
            CardId(100),
            PlayerId(0),
            "Test Card".to_string(),
            Zone::Battlefield,
        );
        let json = serde_json::to_string(&obj).unwrap();
        let deserialized: GameObject = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "Test Card");
        assert_eq!(deserialized.id, ObjectId(1));
    }

    #[test]
    fn chosen_color_returns_stored_color() {
        let mut obj = GameObject::new(
            ObjectId(1),
            CardId(100),
            PlayerId(0),
            "Test Land".to_string(),
            Zone::Battlefield,
        );
        assert!(obj.chosen_color().is_none());
        obj.chosen_attributes
            .push(ChosenAttribute::Color(ManaColor::Red));
        assert_eq!(obj.chosen_color(), Some(ManaColor::Red));
    }

    #[test]
    fn chosen_basic_land_type_returns_stored_type() {
        let mut obj = GameObject::new(
            ObjectId(1),
            CardId(100),
            PlayerId(0),
            "Test Land".to_string(),
            Zone::Battlefield,
        );
        obj.chosen_attributes
            .push(ChosenAttribute::BasicLandType(BasicLandType::Forest));
        assert_eq!(obj.chosen_basic_land_type(), Some(BasicLandType::Forest));
    }

    #[test]
    fn controller_defaults_to_owner() {
        let obj = GameObject::new(
            ObjectId(1),
            CardId(1),
            PlayerId(1),
            "Card".to_string(),
            Zone::Hand,
        );
        assert_eq!(obj.controller, obj.owner);
    }

    #[test]
    fn parse_counter_type_lore() {
        assert_eq!(parse_counter_type("lore"), CounterType::Lore);
        assert_eq!(parse_counter_type("LORE"), CounterType::Lore);
        assert_eq!(parse_counter_type("lore counter"), CounterType::Lore);
    }

    #[test]
    fn final_chapter_number_returns_max() {
        use crate::types::ability::{CounterTriggerFilter, TriggerDefinition};
        use crate::types::triggers::TriggerMode;

        let mut obj = GameObject::new(
            ObjectId(1),
            CardId(1),
            PlayerId(0),
            "The Eldest Reborn".to_string(),
            Zone::Battlefield,
        );
        obj.card_types.subtypes.push("Saga".to_string());
        obj.trigger_definitions = vec![
            TriggerDefinition::new(TriggerMode::CounterAdded).counter_filter(
                CounterTriggerFilter {
                    counter_type: CounterType::Lore,
                    threshold: Some(1),
                },
            ),
            TriggerDefinition::new(TriggerMode::CounterAdded).counter_filter(
                CounterTriggerFilter {
                    counter_type: CounterType::Lore,
                    threshold: Some(2),
                },
            ),
            TriggerDefinition::new(TriggerMode::CounterAdded).counter_filter(
                CounterTriggerFilter {
                    counter_type: CounterType::Lore,
                    threshold: Some(3),
                },
            ),
        ]
        .into();
        assert_eq!(obj.final_chapter_number(), Some(3));
    }

    #[test]
    fn final_chapter_number_non_saga() {
        let obj = GameObject::new(
            ObjectId(1),
            CardId(1),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        assert_eq!(obj.final_chapter_number(), None);
    }
}
