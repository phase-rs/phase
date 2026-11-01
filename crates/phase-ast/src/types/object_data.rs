use serde::{Deserialize, Serialize};

use super::ability::{
    AbilityDefinition, AdditionalCost, CastingRestriction, ModalChoice, ReplacementDefinition,
    SpellCastingOption, StaticDefinition, TriggerDefinition, TriggerPrintedOrigin,
};
use super::card::{LayoutKind, PrintedCardRef, PrintedLoyalty, TokenArtDescriptor, TokenImageRef};
use super::card_type::CardType;
use super::definitions::Definitions;
use super::identifiers::ObjectId;
use super::keywords::Keyword;
use super::mana::{ManaColor, ManaCost};
use super::player::PlayerId;

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

/// Image-routing identity of a copy source, bundled so every carrier of it
/// (`ContinuousModification::CopyValues`, `LatchedCopiableSnapshot`,
/// `PrecomputedCopyValues`, `CopyTokenSpec`, the merge layer effect) moves the
/// same four fields together and cannot drop one.
///
/// NONE of these fields are CR 707.2 copiable values (status/art are not copied
/// per CR 707.2) — they are display routing only, deliberately kept off
/// `CopiableValues`, and mirror the flat `GameObject` storage (`display_source`
/// discriminates: `Card` => read `printed_ref`; `Token` => read
/// `token_image_ref`).
///
/// CR 111.1 + CR 707.2: a permanent that becomes a copy of a *token* stays a
/// nontoken (token-ness is created by a token-making effect per CR 111.1, and
/// is not among the CR 707.2 copiable values), but its name is the token's,
/// which only resolves in the token art database — so the source's
/// `display_source = Token` and `token_image_ref` must ride along, not just
/// `printed_ref`.
///
/// Serialized with `#[serde(flatten)]` by every carrier; every field defaults,
/// so payloads that predate any of the keys still deserialize.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DisplayIdentity {
    #[serde(default)]
    pub display_source: DisplaySource,
    /// Scryfall oracle-id pointer of the source when it is a printed card.
    /// `None` when the source is a true token (then `token_image_ref` carries
    /// the routing).
    #[serde(default)]
    pub printed_ref: Option<PrintedCardRef>,
    /// Exact token-art pointer of the source when it is a true token.
    /// `None` for printed-card sources.
    #[serde(default)]
    pub token_image_ref: Option<TokenImageRef>,
    /// Intrinsic token-art body of the source, carried so a copy of a token
    /// without an exact ref still renders from the source's shape rather than
    /// the recipient's stale descriptor. `None` for printed-card sources and
    /// pre-descriptor snapshots.
    #[serde(default)]
    pub token_art: Option<TokenArtDescriptor>,
}

/// Stored back-face data for double-faced cards (DFCs).
/// Populated when a Transform-layout card enters the game.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackFaceData {
    pub name: String,
    pub power: Option<i32>,
    pub toughness: Option<i32>,
    pub loyalty: Option<u32>,
    /// CR 306.5b: The face's printed loyalty determines loyalty counters on entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub printed_loyalty: Option<PrintedLoyalty>,
    /// CR 310.4: Defense of a battle (printed number while off the battlefield).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defense: Option<u32>,
    pub card_types: CardType,
    pub mana_cost: ManaCost,
    pub keywords: Vec<Keyword>,
    pub abilities: Vec<AbilityDefinition>,
    /// Stored card-face payload. Live object definitions are materialized with
    /// recipient-local occurrence provenance when this face is installed.
    pub trigger_definitions: Definitions<TriggerDefinition>,
    /// Semantic printed identity for each trigger slot when this face is a
    /// snapshot of copied values. Empty means derive identity from `printed_ref`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trigger_printed_origins: Vec<Option<TriggerPrintedOrigin>>,
    pub replacement_definitions: Definitions<ReplacementDefinition>,
    pub static_definitions: Definitions<StaticDefinition>,
    pub color: Vec<ManaColor>,
    pub printed_ref: Option<PrintedCardRef>,
    pub modal: Option<ModalChoice>,
    pub additional_cost: Option<AdditionalCost>,
    pub strive_cost: Option<ManaCost>,
    pub casting_restrictions: Vec<CastingRestriction>,
    pub casting_options: Vec<SpellCastingOption>,
    /// Parser diagnostics for THIS face — the `BackFaceData` half of
    /// `game::game_object::GameObject::parse_warnings`, and per-face for the same reason `abilities`
    /// is: the two faces are parsed independently, so a card whose front reads
    /// cleanly and whose back does not is the normal case rather than a corner one.
    ///
    /// Without this field the diagnostic was not a per-face fact at all. Face
    /// application copies field by field, so a transform kept the FRONT face's
    /// diagnostics on the object while displaying the back face's rules text: a
    /// front-clean card looked clean after transforming into a back face the parser
    /// could not fully read, and a front-dirty one kept a diagnostic that no longer
    /// described anything. `ai_support::shortcut_efficacy` reads the object field as
    /// evidence that the printed rules text is fully modelled, so the stale answer
    /// was load-bearing in both directions.
    ///
    /// `serde(default)` keeps every persisted dump loadable — an older
    /// `BackFaceData` simply carries no diagnostics — and `skip_serializing_if`
    /// keeps a clean parse byte-identical on the way out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parse_warnings: Vec<crate::types::oracle_diagnostic::OracleDiagnostic>,
    /// Source layout kind — distinguishes Modal DFCs from Transform DFCs
    /// so the engine can offer face-choice for MDFCs (CR 712.12).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout_kind: Option<LayoutKind>,
    /// #7565: set when this stored face is a swap SNAPSHOT of the object's
    /// other half (the live face is currently the alternative). Replaces the
    /// old implicit contract "snapshot => layout_kind erased", which muted the
    /// layout for every other consumer (cast-face prompt, MDFC land checks).
    /// `false` for a still-unswapped printed back face.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_swap_snapshot: bool,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum RoomDoor {
    Left,
    Right,
}
