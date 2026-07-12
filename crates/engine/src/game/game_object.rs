use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::types::ability::{
    additional_cost_instance_payment_count, additional_cost_instance_payment_count_for_ordinal,
    AbilityDefinition, AdditionalCost, AdditionalCostInstancePayment, AdditionalCostOrigin,
    BasicLandType, CastTimingPermission, CastVariantPaid, CastingPermission, CastingRestriction,
    ChosenAttribute, ChosenSubtypeKind, CostPaidObjectSnapshot, ModalChoice, ReplacementDefinition,
    SeatDirection, SolveCondition, SpellCastingOption, StaticDefinition, TriggerDefinition,
};
use crate::types::card::{LayoutKind, PrintedCardRef, TokenImageRef};
use crate::types::card_type::{CardType, CoreType};
use crate::types::counter::{counter_map_serde, CounterType};
use crate::types::definitions::Definitions;
use crate::types::game_state::{AttackDeclarationRecord, GameState, LKISnapshot};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::{Keyword, KeywordKind};
use crate::types::mana::{ColoredManaCount, ManaColor, ManaCost, ManaPip};
use crate::types::player::PlayerId;
use crate::types::stickers::AppliedSticker;
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

/// CR 702.140a-c: Mutate form marker — `Some(_)` while this object is a
/// mutating creature spell on the stack (cast for its mutate cost). Parallels
/// `BestowFormState`: an empty typed marker (not a bool) set when the mutate
/// cost is paid (`apply_mutate_form`) and cleared by `revert_mutate_form` when
/// the spell's target is illegal at resolution (CR 702.140b) so the spell
/// resolves as a plain creature spell. It does NOT persist onto the merged
/// permanent — the merge identity lives in `GameObject::merged_components`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MutateFormState;

/// CR 712.4c / CR 730.2: Which merge keyword built a merged permanent.
/// Disambiguates Meld (cannot transform — CR 712.4c) from Mutate, which
/// `merged_components.len()` alone cannot, since a two-creature mutate also
/// has `len() == 2`. The transform guard (CR 712.4c) keys on
/// `Some(MergeKind::Meld)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MergeKind {
    Mutate,
    Meld,
    Augment,
}

/// CR 702.160a: Prototype form marker — `Some(_)` means this object was cast
/// prototyped and should use the secondary power, toughness, and mana cost
/// characteristics while it is a spell or permanent on the battlefield.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrototypeFormState {
    pub mana_cost: ManaCost,
    pub power: i32,
    pub toughness: i32,
    pub colors: Vec<ManaColor>,
}

/// Oathbreaker RC: command-zone role marker for a signature spell.
///
/// A signature spell is an instant or sorcery that starts in the command zone,
/// uses commander-tax accounting, may be cast only while its owner's
/// Oathbreaker is controlled on the battlefield, and gets the same zone-return
/// treatment as other command-zone leaders. Stored as a typed marker to avoid
/// proliferating bare role booleans on `GameObject`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SignatureSpellState;

/// CR 702.148a-b + CR 612: Cleave form marker — `Some(_)` while this object's
/// cleave text-changing effect is live (the spell was cast for its cleave cost
/// and the bracket-removed ability set is currently installed on the object).
///
/// Unlike `BestowFormState` (an empty marker whose revert is formulaic — re-add
/// Creature, drop the synthesized Aura subtype/keyword), a cleave revert cannot
/// be recomputed: the text-changing effect swaps in a separately parsed ability
/// set, so restoring the printed form requires the captured snapshot of the four
/// ability classes as they were before the swap. This struct carries that
/// snapshot so `apply_zone_exit_cleanup` can restore it when the spell leaves
/// the stack (CR 702.148a: the abilities function only while the spell is on the
/// stack). Parallels `BestowFormState` — a typed `Option` marker, never a bool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleaveFormState {
    pub abilities: Arc<Vec<AbilityDefinition>>,
    pub triggers: Definitions<TriggerDefinition>,
    pub statics: Definitions<StaticDefinition>,
    pub replacements: Definitions<ReplacementDefinition>,
    pub base_abilities: Arc<Vec<AbilityDefinition>>,
    pub base_triggers: Arc<Vec<TriggerDefinition>>,
    pub base_statics: Arc<Vec<StaticDefinition>>,
    pub base_replacements: Arc<Vec<ReplacementDefinition>>,
}

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

    /// CR 709.5g: To lock a half, remove its unlocked designation. Returns
    /// whether the designation was actually removed (false if it was already
    /// locked). Mirror of [`unlock`], but no fully-unlocked outcome exists —
    /// locking only ever removes a designation.
    pub fn lock(&mut self, door: RoomDoor) -> bool {
        let was_unlocked = self.is_unlocked(door);
        match door {
            RoomDoor::Left => self.left_unlocked = false,
            RoomDoor::Right => self.right_unlocked = false,
        }
        was_unlocked
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoomUnlockOutcome {
    pub changed: bool,
    pub fully_unlocked: bool,
}

/// CR 114: Display-only provenance for an emblem — the name and printed-card
/// reference of the source that created it (e.g. the planeswalker whose
/// ultimate ability made the emblem). This is deliberately NOT the emblem's
/// own `printed_ref`: an emblem is neither a card nor a permanent (CR 114.5),
/// and setting `printed_ref` would make the layer system treat the emblem as
/// represented by that card and leak its types/P-T/abilities. This field is
/// purely presentational — the client uses it to render the emblem as a small
/// chip bearing the source's art crop and a "from <name>" label, mirroring
/// MTG Arena's emblem display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmblemSource {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub printed_ref: Option<PrintedCardRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameObject {
    pub id: ObjectId,
    pub card_id: CardId,
    pub owner: PlayerId,
    /// CR 110.2a + CR 613.1b: The controller before continuous control effects
    /// are applied. Usually the owner, but effects that put a permanent onto
    /// the battlefield under another player's control set this as the permanent
    /// enters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_controller: Option<PlayerId>,
    pub controller: PlayerId,
    pub zone: Zone,

    // Battlefield state
    pub tapped: bool,
    pub face_down: bool,
    pub flipped: bool,
    pub transformed: bool,
    /// CR 712.8a + CR 400.7: True when this object is showing its MDFC back face
    /// (set via ChooseModalFace back_face=true). Reverted to front face on any
    /// zone exit that is not to the battlefield (CR 712.8a: front face only in
    /// zones other than battlefield/stack), unlike transform DFCs which use the
    /// `transformed` flag.
    #[serde(default)]
    pub modal_back_face: bool,

    // Combat
    pub damage_marked: u32,
    pub dealt_deathtouch_damage: bool,

    // Attachments
    /// CR 303.4 + CR 301.5: Host this attachment is attached to.
    /// `None` if unattached. See `AttachTarget` for variants.
    pub attached_to: Option<AttachTarget>,
    pub attachments: Vec<ObjectId>,
    /// CR 702.95b-d: Soulbond pair relationship. Pairing is symmetric:
    /// if `A.paired_with == Some(B)`, then `B.paired_with == Some(A)`.
    /// This is independent from attachments; paired creatures are not
    /// attached to each other.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paired_with: Option<ObjectId>,
    /// CR 702.95a + CR 702.95e: The player who controlled this creature when the
    /// soulbond pair was formed. A pair persists only while *both* creatures
    /// remain on the battlefield under their respective pairing controllers; if
    /// another player gains control of either, the pair must break. Comparing the
    /// two creatures' current controllers to each other (rather than to this
    /// recorded value) misses the case where one effect gains control of both
    /// halves at once. `None` when the creature is unpaired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pair_controller: Option<PlayerId>,

    // Counters
    #[serde(with = "counter_map_serde")]
    pub counters: HashMap<CounterType, u32>,

    /// Alchemy Intensity — a per-card escalating value (digital-only, no CR
    /// entry). Initialized from the card's "Starting intensity N" at first
    /// characteristic application and incremented by `Effect::Intensify`. Like
    /// `counters`, it persists across zone changes (the object keeps its id), so
    /// a card's intensity follows it through hand/library/stack/battlefield.
    #[serde(default)]
    pub intensity: u32,

    /// Alchemy "perpetually" modifications applied to this card (digital-only, no
    /// CR entry). Like `intensity`, these persist across zone changes (the object
    /// keeps its id) and serialization, so a perpetual edit follows the card
    /// through hand/library/stack/battlefield for the rest of the game.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub perpetual_mods: Vec<crate::types::ability::PerpetualModification>,

    // Characteristics
    pub name: String,
    pub power: Option<i32>,
    pub toughness: Option<i32>,
    pub loyalty: Option<u32>,
    /// CR 310.4c: Defense of a battle on the battlefield — derived from defense
    /// counters. Kept in sync with `CounterType::Defense` by layer evaluation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defense: Option<u32>,
    /// CR 111.10: printed rules text for predefined tokens (Lander, etc.).
    /// Populated at token creation so the frontend can render alt-text / an
    /// `aria-label` when the Scryfall token image is unavailable. `None` for
    /// non-predefined objects (their text comes from the printed card).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_rules_text: Option<String>,
    pub card_types: CardType,
    /// CR 717.1: Which d6 results visit this Attraction (from card variant data).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attraction_lights: Vec<u8>,
    /// CR 717.2: Object is in the supplementary Attraction deck (command zone),
    /// tracked via `Player::attraction_deck` rather than `command_zone`.
    #[serde(default)]
    pub in_attraction_deck: bool,
    /// Unstable Contraptions: object is in the supplementary Contraption deck
    /// (command zone), tracked via `Player::contraption_deck`.
    #[serde(default)]
    pub in_contraption_deck: bool,
    /// Unstable Contraptions: the sprocket this Contraption occupies on the
    /// battlefield. `None` when it is not assembled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contraption_sprocket: Option<u8>,
    /// CR 123.1 + CR 123.5: Stickers are object state, distinct from counters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stickers: Vec<AppliedSticker>,
    pub mana_cost: ManaCost,
    pub keywords: Vec<Keyword>,
    /// Live abilities after layer evaluation. Wrapped in `Arc<Vec<_>>` so
    /// `GameState::clone()` shares the ability list across cloned states
    /// (AI search); mutations go through `Arc::make_mut` for copy-on-write.
    pub abilities: Arc<Vec<AbilityDefinition>>,
    pub trigger_definitions: Definitions<TriggerDefinition>,
    pub replacement_definitions: Definitions<ReplacementDefinition>,
    pub static_definitions: Definitions<StaticDefinition>,
    /// CR 702.148a-b + CR 612: When this object is a cleave spell, the alternate
    /// ability set produced by removing every square-bracketed span from its
    /// rules text. Projected from `CardFace::cleave_variant`. The casting flow
    /// swaps this onto `abilities`/`trigger_definitions`/etc. before preparing
    /// the spell when it is cast for its cleave cost. `None` for every other
    /// object, keeping serialized state byte-identical for the rest of the corpus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleave_variant: Option<crate::types::card::CleaveVariant>,
    pub color: Vec<ManaColor>,
    pub printed_ref: Option<PrintedCardRef>,
    /// Exact token-art lookup metadata, populated only when the engine can
    /// identify one printed token catalog entry without guessing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_image_ref: Option<TokenImageRef>,
    /// MTGJSON token UUIDs linked from this printed source card. Display/catalog
    /// metadata copied from `CardFace`; game rules never read it directly.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_related_token_ids: Vec<String>,

    /// Alchemy spellbook — the fixed list of card names this object can draft
    /// from, copied from `CardFace::metadata.spellbook`. Read by the
    /// `DraftFromSpellbook` resolver to present the choice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spellbook: Vec<String>,

    // Back face data for double-faced cards (DFCs)
    pub back_face: Option<BackFaceData>,

    /// Digital-only Specialize: specialized faces keyed by added color pip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub specialize_faces: Option<super::specialize::SpecializeFaceMap>,

    /// Digital-only Specialize: set after specializing; prevents re-specializing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub specialized_color: Option<ManaColor>,

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
    /// Display-identity baseline for the layer system. `printed_ref` is the
    /// Scryfall image pointer (oracle id + displayed face name), NOT a CR 707.2
    /// copiable characteristic — but it must track the currently displayed
    /// identity, so it is reset to this baseline each layer pass and overridden
    /// by copy effects (see `ContinuousModification::CopyValues`). Mirrors the
    /// `base_name`/`name` pair so a temporary copy's art reverts on expiry.
    #[serde(default)]
    pub base_printed_ref: Option<PrintedCardRef>,
    #[serde(default)]
    pub base_characteristics_initialized: bool,

    // Timestamp for layer ordering
    pub timestamp: u64,

    /// CR 400.7: Monotonic per-object incarnation, bumped on every real zone
    /// change (`bump_incarnation`) — battlefield entry via
    /// `reset_for_battlefield_entry`, plus every non-battlefield move in the zone
    /// movers. An object that leaves and re-enters any zone becomes a new object
    /// even though the engine reuses its `ObjectId` as storage identity. Pairing
    /// the id with this counter distinguishes the new object from the old one at
    /// the same id, so a pending ability that captured the previous incarnation no
    /// longer resolves its self-reference against the moved object (blink/flicker).
    #[serde(default)]
    pub incarnation: u64,

    // CR 603.6a: Turn on which this object entered the battlefield (global turn
    // counter). Used for "entered this turn" triggers and `EnteredThisTurn`
    // filters — NOT for summoning-sickness (see `summoning_sick`).
    pub entered_battlefield_turn: Option<u32>,

    // CR 702.187b: Global turn on which this card was put into a graveyard as a
    // result of a discard. Used by the Mayhem keyword's "as long as you
    // discarded this card this turn" gate. Compared against the current turn
    // number at query time, so it auto-expires when the turn advances; reset to
    // `None` whenever the object changes zones (a card that leaves the graveyard
    // and returns is a new object that was not discarded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discarded_turn: Option<u32>,

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

    /// CR 400.7d: an ability of a permanent may reference what costs were paid to
    /// cast the spell that became it. This snapshots the object paid as a cost to
    /// cast that spell (e.g. the creature sacrificed to Emerge), copied from the
    /// resolving spell's `ResolvedAbility.cost_paid_object` at cast resolution and
    /// propagated into source-bound triggered abilities so an ETB trigger can
    /// reference "the sacrificed creature's toughness" via
    /// `ObjectScope::CostPaidObject`. Cleared on battlefield entry (CR 400.7) and
    /// restored across the entry reset via `CastLinkSnapshot`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cast_cost_paid_object: Option<CostPaidObjectSnapshot>,

    /// CR 603.6a + CR 400.7: When this permanent was put onto the battlefield as
    /// part of resolving an ability's effect, this is the `ObjectId` of that
    /// ability's source permanent. Set by `deliver_replaced_zone_change` on
    /// battlefield entry; `None` for entries that are not ability-effect-driven
    /// (normal land plays, spell resolution to battlefield, combat, etc.).
    /// Read by `TriggerCondition::PlacedByAbilitySource` to implement
    /// anti-recursion intervening-ifs ("if it wasn't put onto the battlefield
    /// with this ability"). Cleared on battlefield exit/entry per CR 400.7 —
    /// a re-entering permanent is a new object with no memory of how it arrived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entered_via_ability_source: Option<ObjectId>,

    /// CR 601.3b + CR 702.8a: Which cast-timing permission was used to cast
    /// the spell that became this permanent, and on which turn. Used by trigger
    /// conditions that care whether normal sorcery timing was bypassed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cast_timing_permission: Option<(CastTimingPermission, u32)>,

    /// CR 107.3m: The value of X paid when the spell that produced this object
    /// was cast. Populated by `finalize_cast` from the pending ability's
    /// `chosen_x` and survives the stack → battlefield transition so that
    /// ETB replacement effects ("enters with X counters") and ETB triggered
    /// abilities that refer to X resolve against the actual paid amount.
    /// Resolved via `QuantityRef::CostXPaid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_x_paid: Option<u32>,

    /// CR 702.102b + CR 709.4d: `true` when this stack object is a *fused* split
    /// spell (both halves cast via Fuse), so its characteristics are the combined
    /// characteristics of both halves *while on the stack* — unlike a non-fused
    /// split spell, whose on-stack characteristics are those of the chosen half
    /// alone (CR 202.3d). Set at fuse finalize; only meaningful on the stack (off
    /// the stack a split card combines regardless, per CR 709.4). Read by
    /// [`GameObject::effective_mana_value`]/[`effective_colors`] so mana-value and
    /// color reads of a fused spell see both halves.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fused_split_spell: bool,

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
    /// CR 601.2b/f/h + CR 702.157a: Count of non-kicker repeatable
    /// additional costs paid while casting the spell that produced this
    /// permanent. Kept separate from `kickers_paid` so Squad does not inherit
    /// Kicker semantics.
    #[serde(default, skip_serializing_if = "is_zero_u32_field")]
    pub additional_cost_payment_count: u32,
    /// CR 607.2g + CR 702.157b/702.175b: Per-instance non-kicker
    /// additional-cost payments that produced this permanent, copied from
    /// `SpellContext.additional_cost_payments` at cast resolution for linked
    /// ETB triggers such as Squad and Offspring.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_cost_payments: Vec<AdditionalCostInstancePayment>,
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

    /// CR 702.160a: `Some(_)` while this object was cast prototyped. The
    /// layer system uses the stored secondary characteristics whenever the
    /// object is a creature; normal casts leave this unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prototype_form: Option<PrototypeFormState>,

    /// CR 702.140a-c: `Some(_)` while this object is a mutating creature spell on
    /// the stack (cast for its mutate cost). Set by `apply_mutate_form`; cleared
    /// by `revert_mutate_form` when the target is illegal at resolution
    /// (CR 702.140b). Does not persist onto the merged permanent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutate_form: Option<MutateFormState>,

    /// CR 730.2 + CR 702.140c: The ordered list of card/token `ObjectId`s that
    /// represent this merged permanent. EMPTY for non-merged objects. Convention:
    /// element `[0]` is the TOPMOST component (supplies copiable characteristics
    /// per CR 730.2a); later elements are progressively lower in the stack. The
    /// merged permanent itself always keeps the original target creature's
    /// `ObjectId` (CR 730.2c continuity) regardless of which component is topmost.
    /// Each component retains its ORIGINAL owner so CR 730.3 routes each to the
    /// correct player's zone when the merged permanent leaves the battlefield.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merged_components: Vec<ObjectId>,

    /// CR 712.4c / CR 730.2: Which merge keyword produced this merged permanent
    /// (`Mutate` vs `Meld`), or `None` for a non-merged object. The transform
    /// guard (CR 712.4c) keys on `Some(MergeKind::Meld)` to forbid transforming a
    /// melded permanent WITHOUT also blocking a two-creature mutate pile (which
    /// also has `merged_components.len() == 2`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_kind: Option<MergeKind>,

    /// CR 730.2a + CR 702.140e: Stable id of the layer-1 copy effect that
    /// represents this merged permanent's topmost copiable values plus component
    /// ability union. `None` for non-merged objects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_layer_effect_id: Option<u64>,

    /// CR 730.2d: A merged permanent is a token only if its TOPMOST component is a
    /// token. The survivor keeps its own `ObjectId` (CR 730.2c) but adopts the
    /// topmost component's token-ness while merged; this captures the survivor's
    /// intrinsic `is_token` (once, on the first merge that overrides it) so
    /// `merge::split_merged_permanent_on_leave` can restore it when the pile
    /// leaves the battlefield. `None` when no override is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_merge_is_token: Option<bool>,

    /// CR 730.3c: When a merged permanent leaves the battlefield it "becomes"
    /// multiple new objects (CR 730.3 / CR 400.7). Each absorbed component records
    /// the surviving object's id here, so that an effect which finds the object
    /// the merged permanent became — a flicker/blink referencing "it" — returns
    /// ALL of the components, not just the survivor (see
    /// `merge::expand_returned_merge_components`). Set when the component is split
    /// out on battlefield exit; cleared on any battlefield (re-)entry. `None` for
    /// objects that were never split out of a merged permanent this way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split_from_merge_survivor: Option<ObjectId>,

    /// CR 702.148a-b + CR 612: `Some(_)` while this object's cleave
    /// text-changing effect is live (the spell was cast for its cleave cost).
    /// Carries the printed-form ability snapshot captured before the swap so the
    /// printed text can be restored when the spell leaves the stack. Set by
    /// `apply_cleave_text_change`; cleared by `revert_cleave_text_change` and by
    /// the zone-exit cleanup in `apply_zone_exit_cleanup` (CR 702.148a).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleave_form: Option<CleaveFormState>,

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

    /// CR 606.3 + CR 606.1: Per-permanent loyalty-ability activation count for
    /// the current turn. Default cap is 1 (CR 606.3 "once per turn"); raised
    /// for the controller by `GameState::extra_loyalty_activations_this_turn`
    /// (The Chain Veil class). The gate logic lives in
    /// `planeswalker::can_activate_loyalty_ability`. The historical bool
    /// "loyalty_activated_this_turn" is replaced by `count > 0`. Cleared at
    /// turn start (CR 606.3 "that turn" reset) and on battlefield re-entry
    /// (CR 400.7 — a re-entering permanent is a new object with no memory).
    #[serde(skip_deserializing, default)]
    pub loyalty_activations_this_turn: u32,

    // Commander: whether this object is a commander card
    #[serde(default)]
    pub is_commander: bool,
    /// Oathbreaker RC: command-zone signature-spell role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_spell: Option<SignatureSpellState>,

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

    /// CR 114: Display-only provenance of the source that created this emblem
    /// (planeswalker, spell, etc.). Populated at creation in `create_emblem`;
    /// `None` for every non-emblem object. See [`EmblemSource`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emblem_source: Option<EmblemSource>,

    /// CR 111.1: Whether this object is a token (not a card).
    #[serde(default)]
    pub is_token: bool,

    /// CR 707.10 + CR 707.12a: Whether this object is a COPY of a card or spell
    /// and is therefore NOT "represented by a card". Set by copy-creation effects
    /// that keep `is_token = false` (notably `Effect::CastCopyOfCard`, used by
    /// Mizzix's Mastery and Cipher's recast); token copies are marked via
    /// `is_token` instead. Read through [`GameObject::is_represented_by_a_card`]
    /// by abilities gated on "if this spell is represented by a card" (e.g.
    /// Cipher's encode-on-resolution, CR 702.99a).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_copy: bool,

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

    /// CR 701.64b: Harnessed designation. Once a permanent becomes harnessed it
    /// stays harnessed until it leaves the battlefield. Like `monstrous`, this is
    /// a pure marker — neither an ability nor part of copiable values. Only
    /// permanents can be harnessed. Read by the ∞ (Infinity) static-ability gate
    /// (CR 702.186b: "∞ — [Ability]" grants [Ability] as long as harnessed).
    #[serde(default)]
    pub harnessed: bool,

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

    /// CR 702.171c: The creatures that saddled this permanent (tapped to pay the
    /// saddle cost). Cleared in lockstep with `is_saddled` at end of turn or when
    /// the permanent leaves the battlefield.
    #[serde(default)]
    pub saddled_by: Vec<ObjectId>,

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

    /// CR 601.2a + CR 603.4: Transient field tracking the player who cast the
    /// spell that became this permanent. Paired with `cast_from_zone` for
    /// intervening-if clauses such as "if you cast it from your graveyard".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cast_controller: Option<PlayerId>,

    /// CR 611.2f: Spell keywords effective AT CAST TIME (printed + statically /
    /// transiently granted), snapshotted during `finalize_cast` BEFORE
    /// `record_spell_cast_from_zone` increments the turn's spell history. Cast-time
    /// "first qualifying spell each turn" grants (a `SpellsCastThisTurn == 0`-gated
    /// `CastWithKeyword` static) must attach to THIS spell at the moment it is put
    /// on the stack; re-querying the grant in `process_triggers` (post-record)
    /// would see the spell already counted and wrongly drop the grant. Consumed by
    /// the post-record SpellCast trigger seams (Cascade, Demonstrate). Transient:
    /// cleared on zone change, mirroring `cast_from_zone`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cast_spell_keywords: Vec<Keyword>,

    /// CR 614.1a + CR 608.2n + CR 607.2b + CR 406.6: While present, this spell
    /// is exiled instead of being put into its owner's graveyard as it resolves,
    /// and the resulting exile is recorded as "exiled with" the stored source.
    /// Set by `Effect::ExileResolvingSpellInsteadOfGraveyard` (Rod of
    /// Absorption's "exile it instead of putting it into a graveyard as it
    /// resolves" rider); consumed by the stack-resolution router.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exile_from_stack_linked_source: Option<ObjectId>,

    /// CR 305.1 + CR 603.4: Transient field tracking the zone a land was played
    /// from. Consumed by ETB trigger processing for conditions like "without
    /// being played"; permanents put onto the battlefield by effects leave this
    /// unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub played_from_zone: Option<Zone>,

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

    /// CR 702.150a: Number of this object's Phyrexian mana symbols that the
    /// caster chose to pay with **life** (2 life each). Set at cast finalization
    /// from the `ShardChoice::PayLife` selections; read when the object enters as
    /// a planeswalker with `Keyword::Compleated` to reduce its entering loyalty by
    /// two per symbol. Like `mana_spent_to_cast_amount`, this is a historical cast
    /// fact that persists through resolution; initialized to 0 by `GameObject::new`.
    #[serde(default, skip_serializing_if = "is_zero_u32_field")]
    pub phyrexian_life_paid: u32,

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

/// CR 104.4b compile-time totality guard for `objects_content_eq`/`object_content_eq`
/// (types/game_state.rs) — the §5.2c 136-field partition. `GameObject` deliberately
/// does NOT derive `PartialEq` (constant-depth loop detection must omit `timestamp`
/// / `incarnation`), so the row comparator is hand-rolled and needs this no-`..`
/// destructure: adding a field breaks the build until it is classified into a
/// bucket (compared / omitted-safe-by-write-site / immutable / projected). Fail-
/// closed — a new per-object accumulator cannot silently escape the partition.
#[cfg(test)]
fn _gameobject_partition_is_total(o: &GameObject) {
    let GameObject {
        id: _,
        card_id: _,
        owner: _,
        base_controller: _,
        controller: _,
        zone: _,
        tapped: _,
        face_down: _,
        flipped: _,
        transformed: _,
        modal_back_face: _,
        damage_marked: _,
        dealt_deathtouch_damage: _,
        attached_to: _,
        attachments: _,
        paired_with: _,
        pair_controller: _,
        counters: _,
        intensity: _,
        perpetual_mods: _,
        name: _,
        power: _,
        toughness: _,
        loyalty: _,
        defense: _,
        token_rules_text: _,
        card_types: _,
        attraction_lights: _,
        in_attraction_deck: _,
        in_contraption_deck: _,
        contraption_sprocket: _,
        stickers: _,
        mana_cost: _,
        keywords: _,
        abilities: _,
        trigger_definitions: _,
        replacement_definitions: _,
        static_definitions: _,
        cleave_variant: _,
        color: _,
        printed_ref: _,
        token_image_ref: _,
        source_related_token_ids: _,
        spellbook: _,
        back_face: _,
        specialize_faces: _,
        specialized_color: _,
        base_power: _,
        base_toughness: _,
        base_name: _,
        base_loyalty: _,
        base_defense: _,
        base_card_types: _,
        base_mana_cost: _,
        base_keywords: _,
        base_abilities: _,
        base_trigger_definitions: _,
        base_replacement_definitions: _,
        base_static_definitions: _,
        base_color: _,
        base_printed_ref: _,
        base_characteristics_initialized: _,
        timestamp: _,
        incarnation: _,
        entered_battlefield_turn: _,
        discarded_turn: _,
        summoning_sick: _,
        echo_due: _,
        cast_variant_paid: _,
        cast_cost_paid_object: _,
        entered_via_ability_source: _,
        cast_timing_permission: _,
        cost_x_paid: _,
        fused_split_spell: _,
        kickers_paid: _,
        additional_cost_payment_count: _,
        additional_cost_payments: _,
        convoked_creatures: _,
        bestow_form: _,
        prototype_form: _,
        mutate_form: _,
        merged_components: _,
        merge_kind: _,
        merge_layer_effect_id: _,
        pre_merge_is_token: _,
        split_from_merge_survivor: _,
        cleave_form: _,
        unimplemented_mechanics: _,
        has_summoning_sickness: _,
        devotion: _,
        has_mana_ability: _,
        mana_ability_index: _,
        available_mana_pips: _,
        loyalty_activations_this_turn: _,
        is_commander: _,
        signature_spell: _,
        commander_tax: _,
        is_renowned: _,
        is_emblem: _,
        emblem_source: _,
        is_token: _,
        is_copy: _,
        display_source: _,
        modal: _,
        additional_cost: _,
        strive_cost: _,
        casting_restrictions: _,
        casting_options: _,
        casting_permissions: _,
        foretold: _,
        chosen_attributes: _,
        goaded_by: _,
        detained_by: _,
        is_suspected: _,
        monstrous: _,
        harnessed: _,
        prepared: _,
        is_saddled: _,
        saddled_by: _,
        assigns_damage_from_toughness: _,
        assigns_damage_as_though_unblocked: _,
        assigns_no_combat_damage: _,
        case_state: _,
        room_unlocks: _,
        class_level: _,
        cast_from_zone: _,
        cast_controller: _,
        cast_spell_keywords: _,
        exile_from_stack_linked_source: _,
        played_from_zone: _,
        mana_spent_to_cast: _,
        colors_spent_to_cast: _,
        mana_spent_to_cast_amount: _,
        phyrexian_life_paid: _,
        mana_spent_source_snapshots: _,
        phase_status: _,
    } = o;
}

/// CR 205.2 + CR 205.2a: Resolve a stored card-type choice from a chosen-attribute
/// slice. The generic "choose a card type" persists as a `CardType` attribute; a
/// restricted card-type choice ("Choose creature or land", Winding Way) parses as
/// a `Labeled` modal option list and persists as a capitalized `Label`, which is
/// parsed back to its `CoreType`. Shared by `GameObject::chosen_card_type` and
/// the `FilterProp::IsChosenCardType` matcher so both forms bind uniformly.
pub(crate) fn chosen_card_type_of(attrs: &[ChosenAttribute]) -> Option<CoreType> {
    attrs.iter().find_map(|a| match a {
        ChosenAttribute::CardType(t) => Some(*t),
        ChosenAttribute::Label(label) => label.parse::<CoreType>().ok(),
        _ => None,
    })
}

impl GameObject {
    /// Apply an Alchemy "perpetually" modification to this card: record it on the
    /// object (so it persists across zones/serialization and can be re-applied
    /// after a copy rebuilds base characteristics) and edit the corresponding
    /// persistent characteristic. Increment 1: base power/toughness.
    pub fn apply_perpetual_modification(
        &mut self,
        modification: &crate::types::ability::PerpetualModification,
        all_creature_types: &[String],
    ) {
        use crate::types::ability::PerpetualModification;
        use crate::types::card_type::CoreType;
        match modification {
            PerpetualModification::SetBasePowerToughness { power, toughness } => {
                // The base_* fields are the persistent baseline the layer pass
                // copies into live P/T each recalc, so editing them here makes the
                // change permanent and zone-independent.
                self.base_power = Some(*power);
                self.base_toughness = Some(*toughness);
            }
            PerpetualModification::ModifyPowerToughness {
                power_delta,
                toughness_delta,
            } => {
                let base_power = self
                    .base_power
                    .or(self.power)
                    .unwrap_or(0)
                    .saturating_add(*power_delta);
                let base_toughness = self
                    .base_toughness
                    .or(self.toughness)
                    .unwrap_or(0)
                    .saturating_add(*toughness_delta);
                self.base_power = Some(base_power);
                self.base_toughness = Some(base_toughness);
            }
            PerpetualModification::GrantKeywords { keywords } => {
                for keyword in keywords {
                    if !self.keywords.contains(keyword) {
                        self.keywords.push(keyword.clone());
                    }
                    // CR 613.1: perpetual keyword grants must survive the layer
                    // pass's `keywords = base_keywords.clone()` reset — mirror
                    // base_* P/T edits and the crew-keyword test seeding pattern.
                    if !self.base_keywords.contains(keyword) {
                        self.base_keywords.push(keyword.clone());
                    }
                }
            }
            PerpetualModification::Become {
                creature_subtypes,
                power,
                toughness,
                keywords,
            } => {
                // CR 613.1d + CR 613.1f + CR 613.4b: update the persistent
                // type, keyword, and base-P/T baselines while retaining
                // non-creature subtypes (Artifact, Aura, etc.).
                self.sync_missing_base_characteristics();
                if !self
                    .base_card_types
                    .core_types
                    .contains(&CoreType::Creature)
                {
                    self.base_card_types.core_types.push(CoreType::Creature);
                }
                self.base_card_types.subtypes.retain(|subtype| {
                    !all_creature_types
                        .iter()
                        .any(|creature_type| creature_type.eq_ignore_ascii_case(subtype))
                });
                for subtype in creature_subtypes {
                    if !self
                        .base_card_types
                        .subtypes
                        .iter()
                        .any(|existing| existing.eq_ignore_ascii_case(subtype))
                    {
                        self.base_card_types.subtypes.push(subtype.clone());
                    }
                }
                self.base_power = Some(*power);
                self.base_toughness = Some(*toughness);
                for keyword in keywords {
                    if !self.base_keywords.contains(keyword) {
                        self.base_keywords.push(keyword.clone());
                    }
                }
            }
            PerpetualModification::ModifyCost { mode, amount } => {
                // CR 601.2f: realize the perpetual self-cost modifier as a
                // synthetic self-spell `ModifyCost` static. The self-spell cost collector
                // reads LIVE `static_definitions` (casting.rs `collect_self_spell_cost_modifiers`)
                // and the hand-zone layer pass re-syncs only `keywords` from base
                // (layers.rs) — so push to BOTH live and base, mirroring the GrantKeywords
                // arm (keywords + base_keywords): the live copy makes it visible to a
                // from-hand cast immediately; the base copy survives the battlefield layer
                // reset (`static_definitions = base.clone()`). `apply_perpetual_modification`
                // runs once per `ApplyPerpetual` resolution (single caller, effects/perpetual.rs)
                // so there is no double-injection; multiple distinct grants intentionally stack.
                use crate::types::ability::TargetFilter;
                use crate::types::statics::StaticMode;
                self.sync_missing_base_characteristics();
                let synthetic =
                    crate::types::ability::StaticDefinition::new(StaticMode::ModifyCost {
                        mode: *mode,
                        amount: amount.clone(),
                        spell_filter: None,
                        dynamic_count: None,
                    })
                    .affected(TargetFilter::SelfRef)
                    .active_zones(crate::types::zones::self_spell_cost_mod_active_zones());
                self.static_definitions.push(synthetic.clone());
                Arc::make_mut(&mut self.base_static_definitions).push(synthetic);
            }
        }
        self.perpetual_mods.push(modification.clone());
    }

    pub fn instance_payment_count(&self, origin: AdditionalCostOrigin) -> u32 {
        additional_cost_instance_payment_count(&self.additional_cost_payments, origin)
    }

    pub fn instance_payment_count_for_ordinal(
        &self,
        origin: AdditionalCostOrigin,
        origin_ordinal: u32,
    ) -> u32 {
        additional_cost_instance_payment_count_for_ordinal(
            &self.additional_cost_payments,
            origin,
            origin_ordinal,
        )
    }

    /// Oathbreaker RC: true for the command-zone signature spell role.
    pub fn is_signature_spell(&self) -> bool {
        self.signature_spell.is_some()
    }

    /// Oathbreaker RC: mark this command-zone object as a signature spell.
    pub fn mark_signature_spell(&mut self) {
        self.signature_spell = Some(SignatureSpellState);
    }

    /// CR 903 + Oathbreaker RC: command-zone cards that use commander tax and
    /// zone-return handling.
    pub fn uses_command_zone_rules(&self) -> bool {
        self.is_commander || self.is_signature_spell()
    }

    /// CR 202.3d + CR 709.4/709.4b/709.4d: A split card's mana value and colors
    /// are the COMBINED value of both halves off the stack, AND for a *fused*
    /// split spell on the stack (CR 702.102b — both halves were cast). A *non-fused*
    /// split spell on the stack uses only the chosen half. When this returns
    /// `Some(bf)`, `bf` is the *other* half's back-face data (its `mana_cost`/
    /// `color` describe the half NOT stored in `self`), and the caller should
    /// combine it with `self`.
    ///
    /// CR 709.5 / CR 709.5c: A Room permanent ON THE BATTLEFIELD is characterized
    /// by its unlocked-half static abilities (the "left/right half unlocked"
    /// designations are battlefield-only, CR 709.5c), NOT this naive combine, so a
    /// Room on the battlefield is gated out here and falls through to the
    /// single-face path. A Room card OFF the battlefield still combines per
    /// CR 709.4. `room_unlocks` is populated on any Room card regardless of zone
    /// (see `apply_card_face_to_object`), so the gate keys on the actual zone —
    /// a Room card in hand/graveyard/exile has `zone != Battlefield` and combines.
    fn split_half_to_combine(&self) -> Option<&BackFaceData> {
        let bf = self.back_face.as_ref()?;
        if bf.layout_kind != Some(LayoutKind::Split) {
            return None;
        }
        // CR 709.5c: a Room on the battlefield is characterized by its unlocked
        // halves, not a naive combine.
        let is_battlefield_room = self.zone == Zone::Battlefield && self.room_unlocks.is_some();
        if is_battlefield_room {
            return None;
        }
        // CR 202.3d + CR 709.4d: combine off the stack, or on the stack when this
        // is a fused split spell. A non-fused split spell on the stack keeps the
        // chosen half only.
        let combine = self.zone != Zone::Stack || self.fused_split_spell;
        combine.then_some(bf)
    }

    /// CR 202.3d + CR 709.4b: This object's mana value accounting for the split
    /// card rule. Off the stack, a split card's mana value is the combined mana
    /// value of both halves; in every other case it is this object's own cost
    /// (including announced X while on the stack, per CR 202.3e). Every off-stack
    /// mana-value read for a split-capable object must route through here rather
    /// than reading `self.mana_cost.mana_value()` directly.
    pub fn effective_mana_value(&self) -> u32 {
        match self.split_half_to_combine() {
            // CR 202.3e: X = 0 off the stack, so `mana_value()` (X treated as 0)
            // on each half is the correct combined off-stack mana value. A fused
            // split spell on the stack also reaches this arm; no printed Fuse card
            // has {X} in either half, so summing X-as-0 mana values is exact there.
            Some(bf) => self.mana_cost.mana_value() + bf.mana_cost.mana_value(),
            None => self
                .mana_cost
                .mana_value_with_x(self.zone, self.cost_x_paid),
        }
    }

    /// CR 202.3d + CR 709.4/709.4b: This object's colors accounting for the split
    /// card rule. Off the stack, a split card's colors are determined from the
    /// combined mana cost of both halves; otherwise they are this object's own
    /// colors. The union is de-duplicated in canonical WUBRG order
    /// (`ManaColor::ALL`) so the result is deterministic and order-stable.
    pub fn effective_colors(&self) -> Vec<ManaColor> {
        match self.split_half_to_combine() {
            Some(bf) => ManaColor::ALL
                .into_iter()
                .filter(|c| self.color.contains(c) || bf.color.contains(c))
                .collect(),
            None => self.color.clone(),
        }
    }

    /// The other Split half to combine when this object is being cast as a FUSED
    /// split spell (CR 702.102b). `None` for non-fused casts and non-split objects,
    /// so callers combine both halves ONLY for a fused spell. Distinct from
    /// `split_half_to_combine`, which also fires for ANY split card off the stack
    /// (the object-characteristic rule, CR 709.4). `fused` is the caller's
    /// determination — either the persisted `fused_split_spell` marker
    /// (already-finalized casts) OR a pre-payment `CastingVariant::Fuse` override,
    /// which is not yet reflected in the marker while enumerating / preparing on an
    /// immutable `&GameState`. The single-face guard (`layout_kind == Split`) still
    /// applies, so a non-split object returns `None` even when `fused == true`.
    fn fused_split_half_for(&self, fused: bool) -> Option<&BackFaceData> {
        if !fused {
            return None;
        }
        self.back_face
            .as_ref()
            .filter(|bf| bf.layout_kind == Some(LayoutKind::Split))
    }

    /// CR 202.3d + CR 709.4d + CR 702.102b + CR 202.3e: The mana value of the SPELL
    /// this object represents while being cast / on the stack. For a FUSED split
    /// spell (both halves cast) this is the COMBINED mana value of both halves; for
    /// every other object it is the object's own cost, honoring announced X on the
    /// stack. Distinct from [`effective_mana_value`](Self::effective_mana_value),
    /// which ALSO combines a split card merely SITTING off the stack: mid-cast the
    /// spell is still in its origin zone yet must be characterized as its single
    /// (chosen) half unless it was fused, so restricted-mana payment metadata and
    /// spell-cast history must key on the fuse marker, not the zone. The
    /// `fused_split_spell` marker is set BEFORE mana payment so both consumers see
    /// the combined value.
    pub fn spell_mana_value(&self) -> u32 {
        self.spell_mana_value_for(self.fused_split_spell)
    }

    /// Variant-aware sibling of [`spell_mana_value`](Self::spell_mana_value).
    /// `fused` lets a pre-payment caller (option enumeration / cast preparation on
    /// an immutable `&GameState`, where the `fused_split_spell` marker is not yet
    /// set) request the COMBINED mana value a fused split spell would present to
    /// spell filters (CR 202.3d + CR 702.102b + CR 709.4d). The public
    /// [`spell_mana_value`](Self::spell_mana_value) delegates with the persisted
    /// marker so its existing callers stay byte-identical.
    pub fn spell_mana_value_for(&self, fused: bool) -> u32 {
        match self.fused_split_half_for(fused) {
            // Fuse cards carry no {X} in either half, so summing X-as-0 mana values
            // is exact (CR 202.3e is moot here).
            Some(bf) => self.mana_cost.mana_value() + bf.mana_cost.mana_value(),
            None => self
                .mana_cost
                .mana_value_with_x(self.zone, self.cost_x_paid),
        }
    }

    /// CR 202.3d + CR 709.4d + CR 702.102b: The colors of the SPELL this object
    /// represents while being cast / on the stack — the COMBINED colors of both
    /// halves for a fused split spell, otherwise the object's own colors. See
    /// [`spell_mana_value`](Self::spell_mana_value) for why this keys on the
    /// `fused_split_spell` marker rather than the zone gate used by
    /// `effective_colors`.
    pub fn spell_colors(&self) -> Vec<ManaColor> {
        self.spell_colors_for(self.fused_split_spell)
    }

    /// Variant-aware sibling of [`spell_colors`](Self::spell_colors). `fused`
    /// requests the COMBINED colors (CR 202.3d + CR 702.102b) a fused split spell
    /// would present pre-payment, before the `fused_split_spell` marker is set.
    /// The public [`spell_colors`](Self::spell_colors) delegates with the marker.
    pub fn spell_colors_for(&self, fused: bool) -> Vec<ManaColor> {
        match self.fused_split_half_for(fused) {
            Some(bf) => ManaColor::ALL
                .into_iter()
                .filter(|c| self.color.contains(c) || bf.color.contains(c))
                .collect(),
            None => self.color.clone(),
        }
    }

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
            trigger_definitions: self.trigger_definitions.iter_all().cloned().collect(),
            power: self.power,
            toughness: self.toughness,
            // CR 208.4b + CR 613.4b: Snapshot the layer-7b base values the same
            // way `power`/`toughness` capture the post-layer-7 current values,
            // so `PtComparison { scope: Base }` look-back filters read the
            // event-time base (a base-1/1 with a +1/+1 counter records base 1,
            // current 2).
            base_power: self.base_power,
            base_toughness: self.base_toughness,
            // CR 709.4b: Off the stack, a split card's colors are the combined
            // colors of both halves (`effective_colors` no-ops for single-face).
            colors: self.effective_colors(),
            // CR 202.3d + CR 202.3e: On the stack, X equals the announced value
            // and a split spell's mana value is the chosen half; off the stack a
            // split card's mana value is the combined value of both halves.
            mana_value: self.effective_mana_value(),
            controller: self.controller,
            owner: self.owner,
            from_zone: from,
            cast_from_zone: self.cast_from_zone,
            played_from_zone: self.played_from_zone,
            to_zone: to,
            attachments: Vec::new(),
            linked_exile_snapshot: Vec::new(),
            // CR 111.1: Token-ness is a stable identity of the object,
            // snapshotted for post-LTB trigger-filter evaluation (e.g.,
            // "whenever a creature token dies").
            is_token: self.is_token,
            combat_status: Default::default(),
            co_departed: Vec::new(),
            attached_to: self.attached_to,
            // CR 400.7: filled in by `move_to_zone` from the live object AFTER the
            // battlefield-entry incarnation bump; `None` here (pre-entry snapshot).
            entered_incarnation: None,
            turn_zone_change_index: 0,
            // CR 701.60b: Snapshot suspected status at the moment of the move,
            // before `move_to_zone` resets the live flag — so an LTB / cost-paid
            // look-back ("the sacrificed creature was suspected") reads it.
            is_suspected: self.is_suspected,
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
        if self.base_name.is_empty() && !self.name.is_empty() {
            self.base_name = self.name.clone();
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
        if self.base_printed_ref.is_none() && self.printed_ref.is_some() {
            self.base_printed_ref = self.printed_ref.clone();
        }

        self.base_characteristics_initialized = true;
    }

    pub fn new(id: ObjectId, card_id: CardId, owner: PlayerId, name: String, zone: Zone) -> Self {
        GameObject {
            id,
            card_id,
            owner,
            base_controller: Some(owner),
            controller: owner,
            zone,
            tapped: false,
            face_down: false,
            flipped: false,
            transformed: false,
            modal_back_face: false,
            damage_marked: 0,
            dealt_deathtouch_damage: false,
            attached_to: None,
            attachments: Vec::new(),
            paired_with: None,
            pair_controller: None,
            counters: HashMap::new(),
            intensity: 0,
            perpetual_mods: Vec::new(),
            name: name.clone(),
            power: None,
            toughness: None,
            loyalty: None,
            defense: None,
            token_rules_text: None,
            card_types: CardType::default(),
            attraction_lights: Vec::new(),
            in_attraction_deck: false,
            in_contraption_deck: false,
            contraption_sprocket: None,
            stickers: Vec::new(),
            mana_cost: ManaCost::default(),
            keywords: Vec::new(),
            abilities: Arc::new(Vec::new()),
            trigger_definitions: Definitions::default(),
            replacement_definitions: Definitions::default(),
            static_definitions: Definitions::default(),
            color: Vec::new(),
            printed_ref: None,
            base_printed_ref: None,
            token_image_ref: None,
            source_related_token_ids: Vec::new(),
            spellbook: Vec::new(),
            back_face: None,
            specialize_faces: None,
            specialized_color: None,
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
            incarnation: 0,
            entered_battlefield_turn: None,
            discarded_turn: None,
            summoning_sick: false,
            echo_due: false,
            cast_variant_paid: None,
            cast_cost_paid_object: None,
            entered_via_ability_source: None,
            cast_timing_permission: None,
            cost_x_paid: None,
            fused_split_spell: false,
            kickers_paid: Vec::new(),
            additional_cost_payment_count: 0,
            additional_cost_payments: Vec::new(),
            convoked_creatures: Vec::new(),
            bestow_form: None,
            prototype_form: None,
            mutate_form: None,
            merged_components: Vec::new(),
            merge_kind: None,
            pre_merge_is_token: None,
            merge_layer_effect_id: None,
            split_from_merge_survivor: None,
            cleave_form: None,
            cleave_variant: None,
            unimplemented_mechanics: Vec::new(),
            has_summoning_sickness: false,
            has_mana_ability: false,
            mana_ability_index: None,
            devotion: None,
            available_mana_pips: Vec::new(),
            loyalty_activations_this_turn: 0,
            is_commander: false,
            signature_spell: None,
            commander_tax: None,
            is_renowned: false,
            is_emblem: false,
            emblem_source: None,
            is_token: false,
            is_copy: false,
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
            harnessed: false,
            prepared: None,
            is_saddled: false,
            saddled_by: Vec::new(),
            assigns_damage_from_toughness: false,
            assigns_damage_as_though_unblocked: false,
            assigns_no_combat_damage: false,
            case_state: None,
            room_unlocks: None,
            class_level: None,
            cast_from_zone: None,
            cast_controller: None,
            cast_spell_keywords: Vec::new(),
            exile_from_stack_linked_source: None,
            played_from_zone: None,
            mana_spent_to_cast: false,
            colors_spent_to_cast: ColoredManaCount::default(),
            mana_spent_to_cast_amount: 0,
            phyrexian_life_paid: 0,
            mana_spent_source_snapshots: Vec::new(),
            phase_status: PhaseStatus::PhasedIn,
        }
    }

    /// Capture public object characteristics for event-time look-back queries.
    pub fn snapshot_public_characteristics(&self) -> LKISnapshot {
        LKISnapshot {
            name: self.name.clone(),
            token_image_ref: self.token_image_ref.clone(),
            power: self.power,
            toughness: self.toughness,
            // CR 208.4b + CR 613.4b: Layer-7b base values, mirroring how
            // `power`/`toughness` capture the post-layer-7 current values.
            base_power: self.base_power,
            base_toughness: self.base_toughness,
            // CR 202.3d + CR 709.4b: combined mana value / colors for a split card
            // off the stack (no-op for single-face, on-stack, and battlefield
            // Rooms, which gate out) so look-back queries read the CR-correct
            // characteristics — mirrors `snapshot_for_zone_change`.
            mana_value: self.effective_mana_value(),
            controller: self.controller,
            owner: self.owner,
            card_types: self.card_types.core_types.clone(),
            subtypes: self.card_types.subtypes.clone(),
            supertypes: self.card_types.supertypes.clone(),
            keywords: self.keywords.clone(),
            colors: self.effective_colors(),
            chosen_attributes: self.chosen_attributes.clone(),
            counters: self.counters.clone(),
            // CR 110.5: Capture live tap status. This snapshot is taken while the
            // object is still in its public zone (mana-spent / attack-declaration
            // captures), so `self.tapped` is authoritative.
            tapped: self.tapped,
            // CR 701.60b: Capture live suspected status. Taken while the object is
            // still on the battlefield (cost-paid snapshot precedes the sacrifice
            // zone-change that resets the flag), so `self.is_suspected` is authoritative.
            is_suspected: self.is_suspected,
        }
    }

    /// CR 106.3 + CR 601.2h: Capture the public source characteristics needed
    /// by source-qualified "mana spent to cast" effects.
    pub fn snapshot_for_mana_spent(&self) -> LKISnapshot {
        self.snapshot_public_characteristics()
    }

    /// CR 508.1a: Capture the public characteristics of a creature when it is
    /// declared as an attacker, so later "attacked with <quality> this turn"
    /// queries do not depend on the attacker still existing.
    pub fn snapshot_for_attack_declaration(&self, object_id: ObjectId) -> AttackDeclarationRecord {
        AttackDeclarationRecord {
            object_id,
            lki: self.snapshot_public_characteristics(),
            is_token: self.is_token,
            is_commander: self.is_commander,
        }
    }

    /// CR 400.7: Advance this object's incarnation epoch by one. The single bump
    /// primitive — every real zone change (battlefield entry via
    /// `reset_for_battlefield_entry`, and every non-battlefield move in the zone
    /// movers) routes through here so a self-reference captured for the previous
    /// incarnation no longer matches the new object.
    pub fn bump_incarnation(&mut self) {
        self.incarnation += 1;
    }

    /// CR 400.7: Reset transient battlefield state when a permanent enters the battlefield.
    /// A permanent entering the battlefield is a new object with no memory of its previous
    /// existence. Callers that need enter_tapped=true override `tapped` after this call.
    pub fn reset_for_battlefield_entry(&mut self, turn_number: u32, timestamp: u64) {
        // CR 400.7: This (re-)entry creates a new object at the same storage id.
        // Bump the incarnation so self-references captured by abilities created
        // for the previous incarnation no longer match this permanent.
        self.bump_incarnation();
        // CR 613.7d: an object receives a timestamp when it enters a zone. Stage 2
        // stamps battlefield entries only; all-zone entry stamping (graveyard/exile-
        // functioning statics) is a deferred hook (see scope boundary).
        self.timestamp = timestamp;
        self.base_controller = Some(self.owner);
        self.controller = self.owner;
        self.entered_battlefield_turn = Some(turn_number);
        // CR 730.3c + CR 400.7: a split-out merge component that (re-)enters the
        // battlefield is a fresh permanent — drop the survivor back-link so it is
        // not re-collected by a later continuity-reference return.
        self.split_from_merge_survivor = None;
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
        self.loyalty_activations_this_turn = 0;
        self.is_suspected = false;
        self.is_renowned = false;
        self.monstrous = false;
        // CR 701.64b: Harnessed clears when a permanent leaves the battlefield.
        self.harnessed = false;
        self.foretold = false;
        // CR 702.xxx: Prepared (Strixhaven) is a new-object-on-entry reset, per
        // CR 400.7. A re-entering permanent has no memory of a prior prepared
        // state. Assign when WotC publishes SOS CR update.
        self.prepared = None;
        self.is_saddled = false;
        self.saddled_by.clear();
        self.paired_with = None;
        self.pair_controller = None;
        self.chosen_attributes.clear();
        self.cast_variant_paid = None;
        // CR 400.7d: the cast-cost-paid object (e.g. the emerge-sacrificed
        // creature) is bound to the casting event that produced this object. A
        // re-entering permanent has no memory of it — clear here and let the
        // cast resolution path restore it via `CastLinkSnapshot`.
        self.cast_cost_paid_object = None;
        // CR 400.7 + CR 603.6a: Ability-placement provenance is per-entry. Clear
        // it here so the set-block in `deliver_replaced_zone_change` repopulates
        // it only for ability-effect-driven entries (Kodama anti-recursion guard).
        self.entered_via_ability_source = None;
        self.cast_timing_permission = None;
        // CR 400.7d + CR 702.33d: cast provenance and kicker payments are
        // bound to the casting event that produced this object. A re-entering
        // permanent has no memory of prior cast links — clear before the cast
        // resolution path repopulates from the resolving spell's context.
        self.cast_from_zone = None;
        self.cast_controller = None;
        // CR 611.2f: the cast-time keyword snapshot is bound to the same casting
        // event as `cast_from_zone`; clear it on zone change for the same reason.
        self.cast_spell_keywords.clear();
        self.kickers_paid.clear();
        self.additional_cost_payment_count = 0;
        self.additional_cost_payments.clear();
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

    /// CR 613.1 + CR 400.7: Revert layer-derived characteristics to the object's
    /// printed baseline. Mirrors the per-object reset in `evaluate_layers` Step 1
    /// (layers.rs) but runs at zone-exit time so off-battlefield objects — e.g. a
    /// Vesuva copy sacrificed to the legend rule — do not retain copied name, types,
    /// or abilities in the graveyard after copy effects are pruned.
    pub fn revert_layered_characteristics_to_base(&mut self) {
        self.sync_missing_base_characteristics();
        self.name = self.base_name.clone();
        self.power = self.base_power;
        self.toughness = self.base_toughness;
        self.loyalty = self.base_loyalty;
        // CR 310.4a + CR 400.7: Battle defense reverts to printed baseline off the battlefield.
        self.defense = self.base_defense;
        self.card_types = self.base_card_types.clone();
        self.mana_cost = self.base_mana_cost.clone();
        self.keywords = self.base_keywords.clone();
        self.abilities = Arc::clone(&self.base_abilities);
        self.trigger_definitions = Arc::clone(&self.base_trigger_definitions).into();
        self.replacement_definitions = Arc::clone(&self.base_replacement_definitions).into();
        self.static_definitions = Arc::clone(&self.base_static_definitions).into();
        self.color = self.base_color.clone();
        self.printed_ref = self.base_printed_ref.clone();
        self.controller = self.base_controller.unwrap_or(self.owner);
        self.assigns_damage_from_toughness = false;
        self.assigns_damage_as_though_unblocked = false;
        self.assigns_no_combat_damage = false;
    }

    /// CR 400.7: Clear battlefield-only designations when a permanent leaves the battlefield.
    /// Separate from entry reset because some state (counters, transform) is already handled
    /// by `apply_zone_exit_cleanup` in zones.rs.
    pub fn reset_for_battlefield_exit(&mut self) {
        self.base_controller = Some(self.owner);
        // CR 701.37b: Monstrous designation clears when a permanent leaves the battlefield.
        self.monstrous = false;
        // CR 701.64b: Harnessed designation clears when a permanent leaves the battlefield.
        self.harnessed = false;
        // CR 701.15a / CR 701.35a: Goad and detain are battlefield-only designations.
        self.goaded_by.clear();
        self.detained_by.clear();
        // CR 701.60a / CR 702.112b: Suspect and renowned are battlefield designations.
        self.is_suspected = false;
        self.is_renowned = false;
        // CR 400.7 + CR 702.150a: Compleated's life-payment count belongs to
        // the cast that created this permanent. Once it leaves the battlefield,
        // a later entry has no memory of that payment.
        self.phyrexian_life_paid = 0;
        // CR 702.171b: Saddled clears when the Mount leaves the battlefield.
        self.is_saddled = false;
        self.saddled_by.clear();
        // CR 702.xxx: Prepared (Strixhaven) is a battlefield-only designation —
        // clears on BF exit, paralleling monstrous/suspected. CR 400.7: a
        // re-entering permanent is a new object with no memory of its previous
        // prepared state. Assign when WotC publishes SOS CR update.
        self.prepared = None;
        // CR 107.3m: The paid-X value is tied to the spell-resolution that brought
        // this permanent to the battlefield. When the permanent leaves, the value
        // is no longer meaningful; a re-cast will re-populate it via `finalize_cast`.
        self.cost_x_paid = None;
        // CR 400.7 + CR 603.4: `cast_from_zone` records how this permanent
        // arrived on the battlefield, kept alive so `WasCast` ETB intervening-if
        // re-checks resolve correctly. A permanent that leaves the battlefield
        // is a new object on any re-entry — clear the stale cast provenance.
        self.cast_from_zone = None;
        self.cast_controller = None;
        // CR 611.2f: the cast-time keyword snapshot is bound to the same casting
        // event as `cast_from_zone`; clear it on the same zone-change boundary.
        self.cast_spell_keywords.clear();
        // CR 400.7 + CR 603.6a: Ability-placement provenance is battlefield-entry
        // scoped — a permanent that leaves the battlefield is a new object on any
        // re-entry. Clear conservatively on exit, mirroring `cast_from_zone`.
        self.entered_via_ability_source = None;
        // CR 305.1 + CR 603.4: Land-play provenance is likewise battlefield-
        // entry scoped and must not survive a later zone change.
        self.played_from_zone = None;
        self.convoked_creatures.clear();
        // CR 702.103f: `bestow_form` is intentionally NOT cleared here.
        // The zone-exit cleanup in `apply_zone_exit_cleanup` (zones.rs) reads
        // the flag to decide whether to revert the bestow type-changing effect
        // (re-add Creature core type, drop synthesized Aura subtype + enchant
        // creature keyword) — clearing it here would leave the GY/exile object
        // stuck in Aura form because the revert block would skip it. The
        // SBA path (CR 702.103f override) handles the in-place battlefield
        // revert explicitly.
        // CR 730.3: A merged permanent's components are split into their owners'
        // zones by `merge::split_merged_permanent_on_leave` at the battlefield-
        // exit seam, BEFORE this reset runs on the surviving object. The merge
        // identity is battlefield-scoped (CR 400.7), so clear it here so a
        // re-entering object is not stuck carrying stale component ids. `mutate_form`
        // (stack-only, paralleling `bestow_form`) is intentionally NOT cleared here.
        self.merged_components.clear();
        // CR 712.4c / CR 730.2 + CR 400.7: the merge-kind discriminator is
        // battlefield-scoped like the rest of the merge identity; clear it so a
        // re-entering object is not stuck as a phantom Meld/Mutate survivor.
        self.merge_kind = None;
        // CR 730.2d + CR 400.7: the topmost-derived token-ness override is
        // battlefield-scoped. `split_merged_permanent_on_leave` restores it before
        // this reset runs; clear it defensively so a re-entering object never
        // carries a stale override value.
        self.pre_merge_is_token = None;
        // CR 730.3 + CR 400.7: merge copy effects are battlefield-scoped and are
        // pruned at the battlefield-exit seam before this reset. Clear the stored
        // id so a re-entering object cannot point at a stale transient effect.
        self.merge_layer_effect_id = None;
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

    /// CR 702.26b: Only phased-out permanents on the battlefield are treated
    /// as though they do not exist.
    pub fn is_phased_out_permanent(&self) -> bool {
        self.zone == Zone::Battlefield && self.is_phased_out()
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

    /// CR 205.2: Look up a stored card-type choice (e.g. the card
    /// type chosen as this permanent entered the battlefield).
    ///
    /// CR 205.2a: A *restricted* card-type choice ("Choose creature or land",
    /// Winding Way) parses as a `Labeled` modal option list rather than the
    /// generic "choose a card type", so it persists as a capitalized `Label`
    /// rather than a `CardType`. The label still names a card type, so fall back
    /// to parsing it (e.g. "Creature" → `CoreType::Creature`) — this lets every
    /// "of the chosen type" reader (cost reduction, protection, the reveal-and-
    /// partition move) bind a restricted card-type choice uniformly.
    pub fn chosen_card_type(&self) -> Option<CoreType> {
        chosen_card_type_of(&self.chosen_attributes)
    }

    /// Look up a stored basic land type choice.
    pub fn chosen_basic_land_type(&self) -> Option<BasicLandType> {
        self.chosen_attributes.iter().find_map(|a| match a {
            ChosenAttribute::BasicLandType(t) => Some(*t),
            _ => None,
        })
    }

    /// Look up a stored creature type choice.
    ///
    /// CR 613.7: Reads the LAST `ChosenAttribute::CreatureType`, so that a
    /// re-choice (which appends to `chosen_attributes`, since the vector is only
    /// cleared on leave-battlefield) supersedes the prior choice — the most
    /// recent persisted choice wins. ETB-once cards have a single entry, so the
    /// last entry equals the first and behavior is unchanged. Kept consistent
    /// with `chosen_card_name` so a same-clause read of "the last chosen name and
    /// creature type" (Psychic Paper) reports both halves from the same choice.
    pub fn chosen_creature_type(&self) -> Option<&str> {
        self.chosen_attributes.iter().rev().find_map(|a| match a {
            ChosenAttribute::CreatureType(s) => Some(s.as_str()),
            _ => None,
        })
    }

    /// CR 612.8 + CR 613.7: The most recently chosen card name (Psychic Paper's
    /// "the last chosen name"). Reads the LAST `ChosenAttribute::CardName` so a
    /// re-attach that chooses again (which appends, since `chosen_attributes` only
    /// clears on leave-battlefield) supersedes the prior choice. Read by
    /// `ContinuousModification::SetChosenName` at Layer 3 evaluation.
    pub fn chosen_card_name(&self) -> Option<&str> {
        self.chosen_attributes.iter().rev().find_map(|a| match a {
            ChosenAttribute::CardName(s) => Some(s.as_str()),
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

    /// CR 608.2d: Look up a stored chosen keyword (Urborg / Walking Sponge
    /// "choose an ability the target has, then remove it"). Read by
    /// `ContinuousModification::RemoveChosenKeyword` at Layer 6 evaluation
    /// to strip the chosen keyword from the recipient.
    pub fn chosen_keyword(&self) -> Option<&Keyword> {
        self.chosen_attributes.iter().find_map(|a| match a {
            ChosenAttribute::Keyword(k) => Some(k),
            _ => None,
        })
    }

    /// CR 608.2d: Look up ALL stored chosen keywords (Greymond, Avacyn's
    /// Stalwart "choose two abilities from among first strike, vigilance, and
    /// lifelink" persists two `ChosenAttribute::Keyword` entries). The plural
    /// companion to `chosen_keyword`; read by
    /// `ContinuousModification::AddChosenKeyword` at Layer 6 evaluation so a
    /// multi-keyword choice grants every chosen ability, not just the first.
    pub fn chosen_keywords(&self) -> Vec<&Keyword> {
        self.chosen_attributes
            .iter()
            .filter_map(|a| match a {
                ChosenAttribute::Keyword(k) => Some(k),
                _ => None,
            })
            .collect()
    }

    /// CR 614.12c + CR 607.2d: Look up the persisted anchor-word label chosen
    /// as this permanent entered the battlefield (e.g. "Jeskai" / "Temur" on
    /// Frostcliff Siege, "Khans" / "Dragons" on a Khans of Tarkir Siege).
    /// Read by `StaticCondition::ChosenLabelIs` and
    /// `TriggerCondition::ChosenLabelIs` to gate the linked anchor-word
    /// abilities for the lifetime of the permanent.
    pub fn chosen_label(&self) -> Option<&str> {
        self.chosen_attributes.iter().find_map(|a| match a {
            ChosenAttribute::Label(s) => Some(s.as_str()),
            _ => None,
        })
    }

    /// CR 607.2d + CR 508.1c: Look up the persisted chosen seat direction
    /// (left/right) for a directional attack-restriction source (Pramikon,
    /// Sky Rampart; Mystic Barrier; Teyo, Geometric Tactician). Returns `None`
    /// until a direction has been chosen, in which case the restriction is
    /// inert. Read by the CR 508.1c attacker-declaration gate in `combat.rs`.
    pub fn chosen_direction(&self) -> Option<SeatDirection> {
        self.chosen_attributes.iter().find_map(|a| match a {
            ChosenAttribute::Direction(d) => Some(*d),
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
        self.chosen_player()
    }

    /// CR 613.1: The player persisted on this permanent via
    /// `ChosenAttribute::Player` — the player chosen by an "as ~ enters the
    /// battlefield, choose a player" replacement. Single authority for the
    /// durable chosen player: used by `protector` (Battles) and by the
    /// `SourceChosenPlayer` controller-ref / player-scope for CDAs such as
    /// Sewer Nemesis and Skyshroud War Beast.
    pub fn chosen_player(&self) -> Option<PlayerId> {
        self.chosen_attributes.iter().find_map(|a| match a {
            ChosenAttribute::Player(p) => Some(*p),
            _ => None,
        })
    }

    /// CR 111.1 + CR 707.10 + CR 707.12a: Whether this object is "represented by
    /// a card" — i.e. a real card, not a token (CR 111.1) and not a copy
    /// (CR 707.10/707.12a). Abilities that act "if this spell is represented by a
    /// card" (Cipher's encode-on-resolution, CR 702.99a) gate on this.
    pub fn is_represented_by_a_card(&self) -> bool {
        !self.is_token && !self.is_copy
    }

    /// CR 702.66a: Delve may exile only a card from its owner's graveyard.
    pub fn is_delve_eligible(&self, player: PlayerId) -> bool {
        self.owner == player && self.zone == Zone::Graveyard && self.is_represented_by_a_card()
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

    /// CR 702.51a: Whether this object can be tapped for convoke mana.
    /// Requires: on battlefield, untapped, creature, controlled by `player`.
    pub fn is_convoke_eligible(&self, player: PlayerId) -> bool {
        self.controller == player
            && self.zone == Zone::Battlefield
            && !self.tapped
            && self.card_types.core_types.contains(&CoreType::Creature)
    }

    /// Whether this object can be tapped for waterbend mana.
    /// Requires: on battlefield, untapped, creature or artifact, controlled by `player`.
    pub fn is_waterbend_eligible(&self, player: PlayerId) -> bool {
        self.controller == player
            && self.zone == Zone::Battlefield
            && !self.tapped
            && (self.card_types.core_types.contains(&CoreType::Creature)
                || self.card_types.core_types.contains(&CoreType::Artifact))
    }

    /// CR 702.126a: Whether this object can be tapped for improvise mana.
    /// Requires: on battlefield, untapped, artifact, controlled by `player`.
    pub fn is_improvise_eligible(&self, player: PlayerId) -> bool {
        self.controller == player
            && self.zone == Zone::Battlefield
            && !self.tapped
            && self.card_types.core_types.contains(&CoreType::Artifact)
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

/// CR 607.2d + CR 608.2c: Resolve "the chosen player" from the source's
/// linked persisted choice. Triggered abilities may resolve after the source
/// left the battlefield; in that case the LKI cache carries the source choices
/// as they last existed in the public zone.
pub(crate) fn source_chosen_player(state: &GameState, source_id: ObjectId) -> Option<PlayerId> {
    state
        .objects
        .get(&source_id)
        .and_then(GameObject::chosen_player)
        .or_else(|| {
            state.lki_cache.get(&source_id).and_then(|lki| {
                lki.chosen_attributes.iter().find_map(|attr| match attr {
                    ChosenAttribute::Player(player) => Some(*player),
                    _ => None,
                })
            })
        })
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

    /// CR 702.26: `phase_status` must be exposed on the wire so the FE can
    /// render a phased-out tint on individual permanents. The serde shape is
    /// the tagged enum `{ "status": "PhasedOut", "cause": "Directly" }` which
    /// the TS `PhaseStatus` type mirrors in `client/src/adapter/types.ts`.
    #[test]
    fn phase_status_roundtrips_via_wire_format() {
        let mut obj = GameObject::new(
            ObjectId(1),
            CardId(100),
            PlayerId(0),
            "Test Card".to_string(),
            Zone::Battlefield,
        );
        obj.phase_status = PhaseStatus::PhasedOut {
            cause: PhaseOutCause::Directly,
        };

        let json = serde_json::to_value(&obj).unwrap();
        assert_eq!(json["phase_status"]["status"], "PhasedOut");
        assert_eq!(json["phase_status"]["cause"], "Directly");

        let deserialized: GameObject = serde_json::from_value(json).unwrap();
        assert!(deserialized.is_phased_out());
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
    fn chosen_card_name_returns_last_choice() {
        // CR 613.7: re-attach appends a second CardName; the most recent wins.
        let mut obj = GameObject::new(
            ObjectId(1),
            CardId(100),
            PlayerId(0),
            "Psychic Paper".to_string(),
            Zone::Battlefield,
        );
        assert!(obj.chosen_card_name().is_none());
        obj.chosen_attributes
            .push(ChosenAttribute::CardName("Llanowar Elves".to_string()));
        assert_eq!(obj.chosen_card_name(), Some("Llanowar Elves"));
        obj.chosen_attributes
            .push(ChosenAttribute::CardName("Grizzly Bears".to_string()));
        assert_eq!(obj.chosen_card_name(), Some("Grizzly Bears"));
    }

    #[test]
    fn chosen_creature_type_returns_last_choice() {
        // CR 613.7: re-attach appends a second CreatureType; the most recent wins.
        let mut obj = GameObject::new(
            ObjectId(1),
            CardId(100),
            PlayerId(0),
            "Psychic Paper".to_string(),
            Zone::Battlefield,
        );
        assert!(obj.chosen_creature_type().is_none());
        obj.chosen_attributes
            .push(ChosenAttribute::CreatureType("Elf".to_string()));
        assert_eq!(obj.chosen_creature_type(), Some("Elf"));
        obj.chosen_attributes
            .push(ChosenAttribute::CreatureType("Bear".to_string()));
        assert_eq!(obj.chosen_creature_type(), Some("Bear"));
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

    // ---------------------------------------------------------------------
    // CR 202.3d + CR 709.4/709.4b split-card off-stack mana value & colors.
    //
    // Assault // Battery (fixture): Assault {R} = MV 1 (Red), Battery {3}{G} =
    // MV 4 (Green). Off the stack the combined characteristics are MV 5 and
    // colors {Red, Green}. Each test drives `add_real_card` (which populates
    // `back_face` via `populate_back_face_if_dfc`) so it exercises the real
    // parsed card, then reads the fix's helpers / production seams. Every
    // assertion FAILS on the pre-fix front-only read.
    // ---------------------------------------------------------------------

    use crate::game::scenario::{GameScenario, P0};
    use crate::game::scenario_db::GameScenarioDbExt;
    use crate::test_support::shared_card_db;
    use crate::types::ability::{Comparator, FilterProp, QuantityExpr, TargetFilter, TypedFilter};

    /// (a) A split card in library/graveyard/hand reports the COMBINED mana value
    /// of both halves (5), not the front half alone (1). Reverting the fix makes
    /// `effective_mana_value()` return 1 and every assertion fails.
    #[test]
    fn split_card_effective_mana_value_is_combined_off_stack() {
        let db = shared_card_db();
        for zone in [Zone::Library, Zone::Graveyard, Zone::Hand, Zone::Exile] {
            let mut sc = GameScenario::new();
            let id = sc.add_real_card(P0, "Assault", zone, db);
            let obj = sc.state.objects.get(&id).unwrap();
            assert_eq!(
                obj.back_face.as_ref().map(|b| b.name.as_str()),
                Some("Battery"),
                "back_face must hydrate the other split half off the stack in {zone:?}"
            );
            assert_eq!(
                obj.effective_mana_value(),
                5,
                "Assault // Battery combined MV must be 5 in {zone:?} (front-only = 1)"
            );
        }
    }

    /// (b) A split card off the stack has the COMBINED colors of both halves.
    /// Assault // Battery is {R} + {3}{G} → {Red, Green}. Front-only reports only
    /// {Red}, so the Green assertion fails on revert.
    #[test]
    fn split_card_effective_colors_are_combined_off_stack() {
        let db = shared_card_db();
        let mut sc = GameScenario::new();
        let id = sc.add_real_card(P0, "Assault", Zone::Hand, db);
        let colors = sc.state.objects.get(&id).unwrap().effective_colors();
        assert!(
            colors.contains(&ManaColor::Red) && colors.contains(&ManaColor::Green),
            "combined colors must include both Red and Green, got {colors:?}"
        );
        assert_eq!(
            colors.len(),
            2,
            "exactly the two half colors, WUBRG-ordered"
        );
        // Canonical WUBRG order (ManaColor::ALL): Red precedes Green.
        assert_eq!(colors, vec![ManaColor::Red, ManaColor::Green]);
    }

    /// (c) A production `FilterProp::Cmc { GE, 5 }` MATCHES a split card off the
    /// stack (combined MV 5) and a `HasColor { Green }` filter matches its
    /// combined colors; a plain {2}{R} MV-3 single-face card does NOT match
    /// either. Reverting the fix drops the Cmc/color match on the split card.
    #[test]
    fn cmc_and_color_filters_see_combined_split_characteristics() {
        let db = shared_card_db();
        let mut sc = GameScenario::new();
        let split = sc.add_real_card(P0, "Assault", Zone::Graveyard, db);
        let ogre = sc.add_real_card(P0, "Gray Ogre", Zone::Graveyard, db);
        let state = sc.state;

        let cmc_ge_5 = TargetFilter::Typed(TypedFilter {
            properties: vec![FilterProp::Cmc {
                comparator: Comparator::GE,
                value: QuantityExpr::Fixed { value: 5 },
            }],
            ..TypedFilter::card()
        });
        let has_green = TargetFilter::Typed(TypedFilter {
            properties: vec![FilterProp::HasColor {
                color: ManaColor::Green,
            }],
            ..TypedFilter::card()
        });

        let ctx = crate::game::filter::FilterContext::from_source(&state, split);
        assert!(
            crate::game::filter::matches_target_filter(&state, split, &cmc_ge_5, &ctx),
            "split card off the stack must match Cmc >= 5 (combined MV)"
        );
        assert!(
            crate::game::filter::matches_target_filter(&state, split, &has_green, &ctx),
            "split card off the stack must match HasColor(Green) (combined colors)"
        );
        // Negative: a plain {2}{R} MV-3 Red card matches neither.
        assert!(
            !crate::game::filter::matches_target_filter(&state, ogre, &cmc_ge_5, &ctx),
            "a plain {{2}}{{R}} MV-3 card must NOT match Cmc >= 5"
        );
        assert!(
            !crate::game::filter::matches_target_filter(&state, ogre, &has_green, &ctx),
            "a mono-red card must NOT match HasColor(Green)"
        );
    }

    /// (d) The zone-change LKI snapshot (`snapshot_for_zone_change`) captures the
    /// COMBINED mana value for a dying split card, so an MV-gated look-back
    /// trigger ("a card with MV 5 leaves") reads 5, not 1. A plain MV-3
    /// single-face card snapshots 3. Reverting the fix snapshots 1.
    #[test]
    fn zone_change_snapshot_records_combined_split_mana_value() {
        let db = shared_card_db();
        let mut sc = GameScenario::new();
        let split = sc.add_real_card(P0, "Assault", Zone::Battlefield, db);
        let ogre = sc.add_real_card(P0, "Gray Ogre", Zone::Battlefield, db);
        let state = &sc.state;

        let split_record = state.objects.get(&split).unwrap().snapshot_for_zone_change(
            split,
            Some(Zone::Battlefield),
            Zone::Graveyard,
        );
        assert_eq!(
            split_record.mana_value, 5,
            "dying split card's zone-change record must snapshot combined MV 5"
        );

        let ogre_record = state.objects.get(&ogre).unwrap().snapshot_for_zone_change(
            ogre,
            Some(Zone::Battlefield),
            Zone::Graveyard,
        );
        assert_eq!(
            ogre_record.mana_value, 3,
            "a plain {{2}}{{R}} single-face card snapshots MV 3, unaffected by the fix"
        );
    }

    /// (g) A non-split {2}{R} card reports MV 3 in every zone — the fix must not
    /// perturb single-face cards (no `back_face`, so the gate returns None).
    #[test]
    fn single_face_card_mana_value_unchanged_in_all_zones() {
        let db = shared_card_db();
        for zone in [
            Zone::Hand,
            Zone::Graveyard,
            Zone::Library,
            Zone::Battlefield,
        ] {
            let mut sc = GameScenario::new();
            let id = sc.add_real_card(P0, "Gray Ogre", zone, db);
            let obj = sc.state.objects.get(&id).unwrap();
            assert_eq!(
                obj.effective_mana_value(),
                3,
                "Gray Ogre {{2}}{{R}} must report MV 3 in {zone:?}"
            );
            assert_eq!(
                obj.effective_colors(),
                vec![ManaColor::Red],
                "Gray Ogre is mono-red in {zone:?}"
            );
        }
    }

    /// OR-gate anchor for the pre-payment fuse projection (PR #5093). The
    /// `spell_mana_value_for(fused)` / `spell_colors_for(fused)` helpers let a
    /// pre-payment caller (option enumeration / cast preparation on an immutable
    /// `&GameState`, before the `fused_split_spell` marker is set) request the
    /// COMBINED characteristics a fused split spell would present to spell filters
    /// (CR 202.3d + CR 702.102b). `fused = false` reports the front half; `true`
    /// reports both halves combined — WITHOUT ever touching the marker. Reverting
    /// the `_for` split (making the projection key only on the marker) makes the
    /// `true` case still report the front half and fails these assertions.
    #[test]
    fn spell_mana_value_and_colors_for_fused_hint_combine_without_marker() {
        let db = shared_card_db();
        let mut sc = GameScenario::new();
        // Breaking // Entering: Breaking {U}{B} (MV 2, {U,B}) front + Entering
        // {4}{B}{R} (MV 6, {B,R}) back Split half. Combined MV 8, colors {U,B,R}.
        let breaking = sc.add_real_card(P0, "Breaking", Zone::Hand, db);
        let obj = sc.state.objects.get(&breaking).unwrap();

        // Marker is NOT set — the object is a raw hand card mid-enumeration.
        assert!(
            !obj.fused_split_spell,
            "fixture must exercise the marker-independent `_for` path"
        );

        // fused = false: front half only (MV 2, no red).
        assert_eq!(
            obj.spell_mana_value_for(false),
            2,
            "spell_mana_value_for(false) reports the front half MV (2)"
        );
        assert!(
            !obj.spell_colors_for(false).contains(&ManaColor::Red),
            "spell_colors_for(false) is the front half (no red)"
        );

        // fused = true: combined halves (MV 8, includes red) — no marker set.
        assert_eq!(
            obj.spell_mana_value_for(true),
            8,
            "spell_mana_value_for(true) reports the COMBINED MV (8) with no marker set"
        );
        assert!(
            obj.spell_colors_for(true).contains(&ManaColor::Red),
            "spell_colors_for(true) includes Entering's red with no marker set"
        );

        // The public marker-keyed accessors still report the front half (marker unset).
        assert_eq!(
            obj.spell_mana_value(),
            2,
            "public spell_mana_value() stays marker-keyed (front half while marker unset)"
        );
    }

    /// (h) The Room gate (CR 709.5 / CR 709.5c): a Room card ON the battlefield is
    /// characterized by its unlocked-half static abilities, so it is NOT
    /// over-combined — `effective_mana_value` returns the single (front) half. The
    /// SAME Room card in hand combines both halves per CR 709.4. This proves the
    /// zone-aware battlefield-Room gate. Bottomless Pool // Locker Room:
    /// {U} + {4}{U} → combined MV 6, front-only MV 1.
    ///
    /// Note: `room_unlocks` is populated on any Room card regardless of zone (by
    /// `apply_card_face_to_object`), so the gate must key on the actual zone —
    /// `room_unlocks.is_some()` alone would wrongly exclude off-battlefield Rooms.
    #[test]
    fn room_permanent_on_battlefield_is_not_over_combined() {
        let db = shared_card_db();

        // On the battlefield: gated out → single (front) half MV 1.
        let mut sc_bf = GameScenario::new();
        let bf_id = sc_bf.add_real_card(P0, "Bottomless Pool", Zone::Battlefield, db);
        let bf_obj = sc_bf.state.objects.get(&bf_id).unwrap();
        assert_eq!(
            bf_obj.zone,
            Zone::Battlefield,
            "the Room entered the battlefield"
        );
        assert!(
            bf_obj.room_unlocks.is_some(),
            "a Room on the battlefield carries room_unlocks (CR 709.5c)"
        );
        assert_eq!(
            bf_obj.effective_mana_value(),
            1,
            "a battlefield Room is gated out of the naive combine (front half MV 1)"
        );

        // In hand: off the battlefield → combines to MV 6 (CR 709.4), even though
        // `room_unlocks` is populated at card creation.
        let mut sc_hand = GameScenario::new();
        let hand_id = sc_hand.add_real_card(P0, "Bottomless Pool", Zone::Hand, db);
        let hand_obj = sc_hand.state.objects.get(&hand_id).unwrap();
        assert_eq!(hand_obj.zone, Zone::Hand, "the Room card is in hand");
        assert_eq!(
            hand_obj.effective_mana_value(),
            6,
            "a Room card in hand combines both halves (CR 709.4b): {{U}} + {{4}}{{U}} = 6"
        );
    }
}
