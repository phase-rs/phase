use std::collections::HashSet;

use serde::{Deserialize, Deserializer, Serialize};

use crate::game::game_object::{AttachTarget, DisplaySource};

use super::counter::CounterType;

use super::ability::{
    ContinuousModification, CopiableValues, Duration, FaceDownProfile, StaticDefinition, TargetRef,
};
use super::card::{PrintedCardRef, TokenImageRef};
use super::card_type::{CoreType, Supertype};
use super::identifiers::{ObjectId, ObjectIncarnationRef};
use super::keywords::Keyword;
use super::mana::{ManaColor, ManaType, UnitDecision};
use super::phase::Phase;
use super::player::{PlayerCounterKind, PlayerId};
use super::zones::Zone;

pub use super::zones::EtbTapState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReplacementId {
    pub source: ObjectId,
    pub index: usize,
}

/// CR 701.23a + CR 614.6: Final disposition of one found card after the
/// replacement pipeline. The modified form snapshots the selected source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchFoundDisposition {
    Original,
    Modified(BoundSearchFoundDisposition),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundSearchFoundDisposition {
    pub destination: Zone,
    /// CR 400.7: exact incarnation of the selected replacement source. A
    /// resumed choice consumes this snapshot without rebinding to a new object
    /// that later reused the same id.
    pub source: ObjectIncarnationRef,
    /// CR 611.2b + CR 609.4b: A permission rider bound at replacement
    /// selection time and installed only after the found card actually reaches
    /// exile. It contains no found-object identity; delivery supplies that
    /// independently.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<BoundSearchFoundGrant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundSearchFoundGrant {
    /// CR 400.7: exact incarnation of the replacement source whose effect
    /// created the permission.
    pub source: ObjectIncarnationRef,
    pub controller: PlayerId,
    pub grantee: PlayerId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mana_spend_permission: Option<super::ability::ManaSpendPermission>,
}

/// CR 616.1: Candidate data frozen when a SearchFound ordering
/// prompt is offered. Resume consumes this snapshot without consulting the
/// live source object or replacement registry again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundSearchFoundCandidate {
    pub replacement_id: ReplacementId,
    pub disposition: BoundSearchFoundDisposition,
    pub source_name: String,
    pub description: String,
    /// Whether the snapshotted definition may be declined. This is carried per
    /// candidate because a CR 616.1 ordering prompt can contain more than one
    /// optional SearchFound replacement; collapsing optionality onto the whole
    /// pending prompt would force one of them to apply.
    #[serde(default)]
    pub is_optional: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "type")]
pub enum AppliedReplacementKey {
    Object { source: ObjectId, index: usize },
    Floating { index: usize },
    StepEndMana { index: usize },
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(tag = "type")]
enum TaggedAppliedReplacementKey {
    Object { source: ObjectId, index: usize },
    Floating { index: usize },
    StepEndMana { index: usize },
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(untagged)]
enum AppliedReplacementKeyCompat {
    Tagged(TaggedAppliedReplacementKey),
    Legacy(ReplacementId),
}

#[derive(Debug, Clone, Copy)]
pub enum LegacySentinelCarrier {
    Floating,
    StepEndMana,
}

impl AppliedReplacementKeyCompat {
    fn into_key(self, sentinel_carrier: LegacySentinelCarrier) -> AppliedReplacementKey {
        match self {
            AppliedReplacementKeyCompat::Tagged(TaggedAppliedReplacementKey::Object {
                source,
                index,
            }) => AppliedReplacementKey::Object { source, index },
            AppliedReplacementKeyCompat::Tagged(TaggedAppliedReplacementKey::Floating {
                index,
            }) => AppliedReplacementKey::Floating { index },
            AppliedReplacementKeyCompat::Tagged(TaggedAppliedReplacementKey::StepEndMana {
                index,
            }) => AppliedReplacementKey::StepEndMana { index },
            AppliedReplacementKeyCompat::Legacy(ReplacementId {
                source: ObjectId(0),
                index,
            }) => match sentinel_carrier {
                LegacySentinelCarrier::Floating => AppliedReplacementKey::Floating { index },
                LegacySentinelCarrier::StepEndMana => AppliedReplacementKey::StepEndMana { index },
            },
            AppliedReplacementKeyCompat::Legacy(ReplacementId { source, index }) => {
                AppliedReplacementKey::Object { source, index }
            }
        }
    }
}

impl<'de> Deserialize<'de> for AppliedReplacementKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        AppliedReplacementKeyCompat::deserialize(deserializer)
            .map(|key| key.into_key(LegacySentinelCarrier::Floating))
    }
}

impl AppliedReplacementKey {
    pub fn object(source: ObjectId, index: usize) -> Self {
        Self::Object { source, index }
    }

    pub fn floating(index: usize) -> Self {
        Self::Floating { index }
    }

    pub fn step_end_mana(index: usize) -> Self {
        Self::StepEndMana { index }
    }

    pub fn source(self) -> ObjectId {
        match self {
            AppliedReplacementKey::Object { source, .. } => source,
            AppliedReplacementKey::Floating { .. } | AppliedReplacementKey::StepEndMana { .. } => {
                ObjectId(0)
            }
        }
    }

    pub fn index(self) -> usize {
        match self {
            AppliedReplacementKey::Object { index, .. }
            | AppliedReplacementKey::Floating { index }
            | AppliedReplacementKey::StepEndMana { index } => index,
        }
    }

    pub fn as_replacement_id(self) -> ReplacementId {
        ReplacementId {
            source: self.source(),
            index: self.index(),
        }
    }

    pub fn for_event(event: &ProposedEvent, id: ReplacementId) -> Self {
        if id.source != ObjectId(0) {
            return Self::object(id.source, id.index);
        }
        match event {
            ProposedEvent::EmptyManaPool { .. } => Self::step_end_mana(id.index),
            _ => Self::floating(id.index),
        }
    }
}

pub fn deserialize_applied_keys_step_end_mana<'de, D>(
    deserializer: D,
) -> Result<HashSet<AppliedReplacementKey>, D::Error>
where
    D: Deserializer<'de>,
{
    let keys = Vec::<AppliedReplacementKeyCompat>::deserialize(deserializer)?;
    Ok(keys
        .into_iter()
        .map(|key| key.into_key(LegacySentinelCarrier::StepEndMana))
        .collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CounterMoveStage {
    Remove,
    Add,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CounterPlacement {
    Object {
        #[serde(default)]
        actor: PlayerId,
        object_id: ObjectId,
        counter_type: CounterType,
    },
    Player {
        actor: PlayerId,
        player_id: PlayerId,
        counter_kind: PlayerCounterKind,
    },
    Energy {
        actor: PlayerId,
        player_id: PlayerId,
    },
}

impl CounterPlacement {
    pub fn object_id(&self) -> Option<ObjectId> {
        match self {
            CounterPlacement::Object { object_id, .. } => Some(*object_id),
            CounterPlacement::Player { .. } | CounterPlacement::Energy { .. } => None,
        }
    }

    pub fn player_id(&self) -> Option<PlayerId> {
        match self {
            CounterPlacement::Player { player_id, .. }
            | CounterPlacement::Energy { player_id, .. } => Some(*player_id),
            CounterPlacement::Object { .. } => None,
        }
    }

    pub fn actor(&self) -> PlayerId {
        match self {
            CounterPlacement::Object { actor, .. }
            | CounterPlacement::Player { actor, .. }
            | CounterPlacement::Energy { actor, .. } => *actor,
        }
    }
}

/// CR 111.1 + CR 111.4 + CR 111.10: The body characteristics of a token —
/// the fields that constitute its identity as a permanent, independent of
/// the runtime context in which it's created.
///
/// Shared by `TokenSpec` (runtime/resolved token creation), `TokenPreset`
/// (debug catalog entries), and `DebugAction::CreateToken` (debug-create
/// payload). Single source of truth for the token body shape — no parallel
/// field lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenCharacteristics {
    /// CR 111.4: The token's display name (same as its subtype(s) + "Token"
    /// unless the creating effect specifies otherwise).
    pub display_name: String,
    /// CR 208.2: Fixed power, or `None` for non-creature tokens.
    pub power: Option<i32>,
    /// CR 208.2: Fixed toughness, or `None` for non-creature tokens.
    pub toughness: Option<i32>,
    pub core_types: Vec<CoreType>,
    pub subtypes: Vec<String>,
    pub supertypes: Vec<Supertype>,
    pub colors: Vec<ManaColor>,
    pub keywords: Vec<Keyword>,
}

/// CR 111.1 + CR 111.4 + CR 111.10: Fully-resolved token creation specification.
///
/// `Effect::Token` carries authoring-time fields (`PtValue`, `QuantityExpr`,
/// `TargetFilter owner`) that must be resolved against game state before the
/// token hits the replacement pipeline. `TokenSpec` captures the resolved,
/// self-describing form used by `ProposedEvent::CreateToken` and the
/// post-accept apply path, so replacement matchers and modifiers see the full
/// characteristics of the token that's about to be created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSpec {
    pub characteristics: TokenCharacteristics,
    /// Original Forge-style script name (or custom name) used by the token
    /// parser on the apply path to re-derive attributes. Preserved so the
    /// existing `parse_token_script` dispatch still fires after widening.
    pub script_name: String,
    /// CR 113.3d: Static abilities granted to the token (e.g., "This token
    /// can't block.").
    pub static_abilities: Vec<StaticDefinition>,
    /// CR 122.6a: Counters placed on the token as it enters the battlefield
    /// (resolved from `QuantityExpr` at propose time).
    pub enter_with_counters: Vec<(CounterType, u32)>,
    /// CR 614.1: Token enters tapped.
    pub tapped: bool,
    /// CR 508.4: Token enters the battlefield attacking (not declared as
    /// attacker).
    pub enters_attacking: bool,
    /// CR 603.7: When set, the token is sacrificed at the end of the given
    /// duration (e.g., Mobilize tokens sacrificed at end of combat).
    pub sacrifice_at: Option<Duration>,
    /// CR 107.3a: Ability source — the object that created the token. Needed
    /// on the apply path for defending-player resolution (`enters_attacking`)
    /// and for the delayed-trigger source.
    pub source_id: ObjectId,
    /// CR 107.3a: Ability controller — the player who controls the effect
    /// creating the token (distinct from `owner`, the player to whom the
    /// token belongs).
    pub controller: PlayerId,
    /// CR 303.4 + CR 303.7: When the token is an Aura/Role created "attached to" a
    /// host, the resolved host (object or player). `None` for ordinary tokens.
    /// Resolved once at propose time so the replacement-safe apply path attaches
    /// each created token without re-reading ability.targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach_to: Option<AttachTarget>,
}

/// CR 707.2 + CR 707.5: Copy-token creation payload carried by the same
/// `CreateToken` proposed event that ordinary token creation uses for
/// replacement effects. `TokenSpec` remains the replacement-visible probe
/// characteristics; this payload carries the full copiable values needed once
/// the event is accepted, including display metadata that is not copiable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyTokenSpec {
    pub values: Box<CopiableValues>,
    pub display_source: DisplaySource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub printed_ref: Option<PrintedCardRef>,
    /// CR 111.1 + CR 707.2: exact token-art pointer of the copy source when it
    /// is itself a true token (`display_source == Token`). Carried so a
    /// token-copy of a token resolves the source token's art instead of falling
    /// back to a name+filter Scryfall search. `None` for printed-card sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_image_ref: Option<TokenImageRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_keywords: Vec<Keyword>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_modifications: Vec<ContinuousModification>,
    pub tapped: bool,
    pub enters_attacking: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sacrifice_at: Option<Duration>,
    pub source_id: ObjectId,
    pub controller: PlayerId,
}

/// CR 701.31 + CR 901.9c + CR 701.31c: Which rules path is proposing a
/// planeswalk event. Scoped replacements (Fixed Point in Time) match only
/// [`PlanarDie`]; generic "if you would planeswalk" replacements (Susan Foreman)
/// match every cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PlaneswalkCause {
    /// CR 901.8 / CR 901.9c: Planeswalker symbol on the planar die.
    PlanarDie,
    /// CR 701.31: Planeswalk from a spell or ability instruction (including
    /// chained "then planeswalk" riders).
    Instruction,
    /// CR 701.31c / CR 312.5 / CR 704.6f: Phenomenon encounter, phenomenon
    /// state-based planeswalk, and other rules-process planeswalks that are not
    /// the planar-die ability and not an ability resolving on the stack.
    #[default]
    RulesProcess,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProposedEvent {
    ZoneChange {
        object_id: ObjectId,
        from: Zone,
        to: Zone,
        cause: Option<ObjectId>,
        /// CR 303.4f: When an Aura enters the battlefield by a non-spell
        /// effect and the effect does not specify what it enchants, the
        /// controller chooses a legal object or player as it enters. The
        /// ChangeZone pipeline resolves that choice before delivery and carries
        /// the chosen host here so the battlefield entry and attachment are one
        /// event.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attach_to: Option<AttachTarget>,
        /// Explicit ETB tap-state override carried through the replacement pipeline.
        /// `Unspecified` preserves any non-replacement tapped seed from the originating effect.
        #[serde(default)]
        enter_tapped: EtbTapState,
        /// Counters to place on this permanent as it enters the battlefield.
        /// Each entry is (counter_type, count). Set by ETB-counter replacements.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        enter_with_counters: Vec<(CounterType, u32)>,
        /// Override the controller on ETB. Used by Earthbending return ("under your control")
        /// and other "enters the battlefield under [player]'s control" effects.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        controller_override: Option<PlayerId>,
        /// CR 712.2: When true, the object enters the battlefield showing its back face.
        /// Set by "return ... transformed" effects.
        #[serde(default)]
        enter_transformed: bool,
        /// CR 708.2a + CR 708.3: When `Some`, the object is turned face down
        /// (before entering, CR 708.3) with these characteristics as it enters
        /// the battlefield. Carried through the replacement pipeline so the
        /// face-down state is established before ETB triggers would fire.
        /// Boxed so this rarely-set field doesn't inflate the size of every
        /// `ProposedEvent` (and the `Result<_, ProposedEvent>` pipeline).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        face_down_profile: Option<Box<FaceDownProfile>>,
        applied: HashSet<AppliedReplacementKey>,
    },
    Damage {
        source_id: ObjectId,
        target: TargetRef,
        amount: u32,
        is_combat: bool,
        applied: HashSet<AppliedReplacementKey>,
    },
    Draw {
        player_id: PlayerId,
        count: u32,
        applied: HashSet<AppliedReplacementKey>,
    },
    /// CR 701.23a + CR 614.1: One card found during a search, before the
    /// search instruction sends it to its printed destination.
    SearchFound {
        searcher: PlayerId,
        /// Semantic owner of the library participating in this search. `None`
        /// when Library was not among the effective searched zones.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        library_owner: Option<PlayerId>,
        object_id: ObjectId,
        disposition: SearchFoundDisposition,
        applied: HashSet<AppliedReplacementKey>,
    },
    /// CR 701.22a + CR 614.1a: A player is about to scry cards. Replacement
    /// effects can modify the count or replace the scry with another action.
    Scry {
        player_id: PlayerId,
        count: u32,
        applied: HashSet<AppliedReplacementKey>,
    },
    /// CR 701.17a + CR 614.1a: A player is about to mill cards. Count-level
    /// replacement effects such as "mill twice that many cards instead" must
    /// see the event before individual library cards move zones.
    Mill {
        player_id: PlayerId,
        count: u32,
        destination: Zone,
        applied: HashSet<AppliedReplacementKey>,
    },
    /// CR 705.1 + CR 614.1a: A player is about to flip a single coin. Carried
    /// through the replacement pipeline so per-flip "instead flip two and ignore
    /// one" effects (Krark's Thumb) double the count before the RNG runs. Per the
    /// card's 2019-01-25 ruling, each individual flip is replaced separately.
    CoinFlip {
        player_id: PlayerId,
        count: u32,
        applied: HashSet<AppliedReplacementKey>,
    },
    /// CR 701.37a + CR 614.1a: A creature is about to explore. Replacement
    /// effects can modify the explore action (e.g., add a scry prelude).
    Explore {
        object_id: ObjectId,
        applied: HashSet<AppliedReplacementKey>,
    },
    /// CR 701.50a + CR 614.1a: A creature is about to connive (draw N, discard N,
    /// +1/+1 per nonland discarded). Carried through the replacement pipeline so
    /// effects that intercept the connive keyword action (Leader, Super-Genius —
    /// "instead you draw a card, then that creature connives") see the event
    /// before the draw/discard/counter steps run. `count` is the connive N value,
    /// already resolved from `QuantityExpr` at propose time.
    Connive {
        object_id: ObjectId,
        count: u32,
        applied: HashSet<AppliedReplacementKey>,
    },
    /// CR 701.34a + CR 614.1a: A player is about to proliferate. Replacement
    /// effects can modify how many times the proliferate action is performed
    /// (Tekuthal, Inquiry Dominus — "proliferate twice instead").
    Proliferate {
        player_id: PlayerId,
        count: u32,
        applied: HashSet<AppliedReplacementKey>,
    },
    LifeGain {
        player_id: PlayerId,
        amount: u32,
        applied: HashSet<AppliedReplacementKey>,
    },
    LifeLoss {
        player_id: PlayerId,
        amount: u32,
        applied: HashSet<AppliedReplacementKey>,
    },
    AddCounter {
        /// CR 122.1 + CR 107.14: Counter placement may affect an object or a
        /// player. Energy is represented as a dedicated player field at runtime
        /// but is still a counter-placement event for replacement purposes.
        #[serde(flatten)]
        placement: CounterPlacement,
        count: u32,
        applied: HashSet<AppliedReplacementKey>,
    },
    RemoveCounter {
        object_id: ObjectId,
        counter_type: CounterType,
        count: u32,
        applied: HashSet<AppliedReplacementKey>,
    },
    /// CR 122.5: Moving a counter is atomic: remove it from one object and put
    /// it on another. Replacement effects see the remove and add stages, but
    /// the physical counter mutation is committed only after both stages survive.
    MoveCounter {
        #[serde(default)]
        actor: PlayerId,
        source_id: ObjectId,
        destination_id: ObjectId,
        counter_type: CounterType,
        remove_count: u32,
        add_count: u32,
        stage: CounterMoveStage,
        applied: HashSet<AppliedReplacementKey>,
    },
    /// CR 111.1 + CR 614.1a: Token creation event carrying the full
    /// self-describing token specification. Replacement effects can modify
    /// `count` (Doubling Season, Primal Vigor) or inspect `spec` for
    /// characteristic-based gating (e.g., "whenever a creature token you
    /// control would enter ...").
    ///
    /// `spec` is boxed so this variant doesn't dominate the enum size —
    /// `TokenSpec` is ~400 bytes of resolved characteristics, and most
    /// other variants are small IDs.
    CreateToken {
        owner: PlayerId,
        /// Resolved token characteristics, keyed by replacement pipeline
        /// matchers on the apply path to reproduce the token faithfully.
        spec: Box<TokenSpec>,
        /// CR 707.2: When present, the event creates tokens that are copies of
        /// a permanent. Replacement matching still reads `spec`; the apply path
        /// reads this payload so replacement-choice resume does not degrade to a
        /// generic token.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        copy: Option<Box<CopyTokenSpec>>,
        /// Explicit ETB tap-state override carried through the replacement pipeline.
        /// `Unspecified` preserves the token spec's authored `tapped` bit.
        #[serde(default)]
        enter_tapped: EtbTapState,
        /// CR 614.1a: Number of tokens to create. May be modified by replacement effects.
        count: u32,
        applied: HashSet<AppliedReplacementKey>,
    },
    TokenEntry {
        entry_ref: ObjectId,
        #[serde(default)]
        enter_tapped: EtbTapState,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        enter_with_counters: Vec<(CounterType, u32)>,
        applied: HashSet<AppliedReplacementKey>,
    },
    Discard {
        player_id: PlayerId,
        object_id: ObjectId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_id: Option<ObjectId>,
        /// CR 614.1a + CR 701.9a: `true` when the discard is caused by resolving
        /// a spell or ability effect; `false` for cost payment or turn-based
        /// actions (cleanup hand-size discard).
        #[serde(default)]
        caused_by_effect: bool,
        applied: HashSet<AppliedReplacementKey>,
    },
    Tap {
        object_id: ObjectId,
        applied: HashSet<AppliedReplacementKey>,
    },
    Untap {
        object_id: ObjectId,
        applied: HashSet<AppliedReplacementKey>,
    },
    /// CR 614.1e + CR 708.11: a permanent is being turned face up. "As ~ is turned
    /// face up" replacement effects apply here (megamorph/disguise).
    TurnFaceUp {
        object_id: ObjectId,
        applied: HashSet<AppliedReplacementKey>,
    },
    Destroy {
        object_id: ObjectId,
        source: Option<ObjectId>,
        /// CR 701.19c: When true, regeneration shields cannot prevent this destruction.
        cant_regenerate: bool,
        applied: HashSet<AppliedReplacementKey>,
    },
    Sacrifice {
        object_id: ObjectId,
        player_id: PlayerId,
        applied: HashSet<AppliedReplacementKey>,
    },
    /// CR 500.1 + CR 614.1b + CR 614.10: A turn is about to begin. Carried
    /// through the replacement pipeline so condition-gated skip effects
    /// (e.g., Stranglehold's "skip extra turns") can prevent the turn.
    ///
    /// `is_extra_turn` is true when this turn was granted by an effect
    /// (CR 500.7 — popped from `state.extra_turns`).
    BeginTurn {
        player_id: PlayerId,
        is_extra_turn: bool,
        applied: HashSet<AppliedReplacementKey>,
    },
    /// CR 500.1 + CR 614.1b: A phase/step is about to begin. Carried through
    /// the replacement pipeline so condition-gated skip effects can prevent
    /// the phase. Simple static-based skips (`StaticMode::SkipStep`) continue
    /// to short-circuit earlier in `turns.rs`; this pipeline path handles
    /// event-context-aware replacements.
    BeginPhase {
        player_id: PlayerId,
        phase: Phase,
        applied: HashSet<AppliedReplacementKey>,
    },
    /// CR 106.3 + CR 614.1a: Mana is about to be produced by a source and added
    /// to a player's mana pool. Carried through the replacement pipeline so
    /// static effects like Contamination ("produces {B} instead") can replace
    /// the produced mana type or amount before it enters the pool.
    ProduceMana {
        source_id: ObjectId,
        player_id: PlayerId,
        mana_type: ManaType,
        /// CR 106.3: Number of mana units of `mana_type` this event produces.
        #[serde(default = "default_produce_mana_count")]
        count: u32,
        /// CR 106.12: True when this production comes from activating a mana
        /// ability with the tap symbol in its cost.
        #[serde(default)]
        tapped_for_mana: bool,
        applied: HashSet<AppliedReplacementKey>,
    },
    /// CR 703.4q + CR 614.1a + CR 616.1: A player's step-end "empty unspent
    /// mana" event. Each entry in `units` describes one `ManaUnit` in the
    /// affected player's pool and its tentative disposition. Step-end mana
    /// handlers (Upwelling, Horizon Stone, Kruphix, Omnath, …) are
    /// replacement effects that flip a unit's disposition from `Drop` to
    /// `Keep` (CR 614.6) or `Recolor(_)` (CR 614.1a) when their filter
    /// matches.
    ///
    /// CR 616.1: When ≥2 handlers apply to the same emptying event, the
    /// affected player chooses ordering. The pipeline serializes choices in
    /// APNAP order across players via `pending_phase_transition_progress`.
    ///
    /// Expiry-bound units (`EndOfTurn` / `EndOfCombat`) do **not** enter
    /// this event — they are cleared by `clear_expiring_at_step_end` before
    /// event construction (preserves the H2 invariant from commit
    /// `e92fd3e19`).
    EmptyManaPool {
        player_id: PlayerId,
        units: Vec<UnitDecision>,
        #[serde(default, deserialize_with = "deserialize_applied_keys_step_end_mana")]
        applied: HashSet<AppliedReplacementKey>,
    },
    /// CR 701.31 + CR 614.1a: A player is about to planeswalk. All CR 701.31c
    /// causes except the starting-plane reveal route through
    /// `planechase::resolve_planeswalk_via_replacements`. Encounter / SBA /
    /// leave-game paths use [`PlaneswalkCause::RulesProcess`]; ability
    /// resolutions use [`PlaneswalkCause::Instruction`] or [`PlaneswalkCause::PlanarDie`].
    Planeswalk {
        player_id: PlayerId,
        /// Distinguishes planar-die planeswalks (CR 901.9c) from ability-
        /// instructed ones so scoped replacements (Fixed Point in Time) match
        /// only the cause their Oracle text names.
        #[serde(default)]
        cause: PlaneswalkCause,
        applied: HashSet<AppliedReplacementKey>,
    },
    /// CR 701.3a + CR 614.1a: An Aura, Equipment, or Fortification is about to
    /// become attached to an object via `Effect::Attach` (Equip activation, or
    /// any other "attach ~ to" effect). Carried through the replacement
    /// pipeline so "as it becomes attached, choose …" replacements
    /// (`ReplacementEvent::Attached`, Psychic Paper) can bind their choice as
    /// the attachment resolves, mirroring the "as ~ enters, choose" ETB
    /// analogue. Auras attaching to a PLAYER host use `attach_to_player`
    /// directly and never propose this event — only object hosts route
    /// through `Effect::Attach::resolve`.
    Attach {
        attachment_id: ObjectId,
        target_id: ObjectId,
        applied: HashSet<AppliedReplacementKey>,
    },
}

fn default_produce_mana_count() -> u32 {
    1
}

impl ProposedEvent {
    /// Construct a `ZoneChange` with default `enter_tapped: Unspecified` and empty `applied` set.
    pub fn zone_change(object_id: ObjectId, from: Zone, to: Zone, cause: Option<ObjectId>) -> Self {
        Self::ZoneChange {
            object_id,
            from,
            to,
            cause,
            attach_to: None,
            enter_tapped: EtbTapState::Unspecified,
            enter_with_counters: Vec::new(),
            controller_override: None,
            enter_transformed: false,
            face_down_profile: None,
            applied: HashSet::new(),
        }
    }

    /// CR 500.1 + CR 614.1b: Construct a `BeginTurn` proposed event.
    pub fn begin_turn(player_id: PlayerId, is_extra_turn: bool) -> Self {
        Self::BeginTurn {
            player_id,
            is_extra_turn,
            applied: HashSet::new(),
        }
    }

    /// CR 500.1 + CR 614.1b: Construct a `BeginPhase` proposed event.
    pub fn begin_phase(player_id: PlayerId, phase: Phase) -> Self {
        Self::BeginPhase {
            player_id,
            phase,
            applied: HashSet::new(),
        }
    }

    /// CR 701.31 + CR 901.9c + CR 614.1a: Construct a `Planeswalk` proposed
    /// event for the planar-die planeswalking ability (CR 901.8).
    pub fn planeswalk(player_id: PlayerId) -> Self {
        Self::planeswalk_with_applied(player_id, PlaneswalkCause::PlanarDie, HashSet::new())
    }

    /// CR 701.31 + CR 614.5: Construct a `Planeswalk` proposed event with an
    /// explicit cause and already-applied replacement keys (chained "then
    /// planeswalk" riders retain the originating replacement's applied set).
    pub fn planeswalk_with_applied(
        player_id: PlayerId,
        cause: PlaneswalkCause,
        applied: HashSet<AppliedReplacementKey>,
    ) -> Self {
        Self::Planeswalk {
            player_id,
            cause,
            applied,
        }
    }

    /// CR 701.34a + CR 614.1a: Construct a `Proliferate` proposed event.
    pub fn proliferate(player_id: PlayerId, count: u32) -> Self {
        Self::Proliferate {
            player_id,
            count,
            applied: HashSet::new(),
        }
    }

    /// CR 106.3 + CR 614.1a: Construct a `ProduceMana` proposed event.
    pub fn produce_mana(source_id: ObjectId, player_id: PlayerId, mana_type: ManaType) -> Self {
        Self::produce_mana_with_context(source_id, player_id, mana_type, false)
    }

    /// CR 106.3 + CR 106.12 + CR 614.1a: Construct a `ProduceMana` proposed
    /// event while preserving whether the mana was produced by tapping the
    /// source for mana.
    pub fn produce_mana_with_context(
        source_id: ObjectId,
        player_id: PlayerId,
        mana_type: ManaType,
        tapped_for_mana: bool,
    ) -> Self {
        Self::ProduceMana {
            source_id,
            player_id,
            mana_type,
            count: 1,
            tapped_for_mana,
            applied: HashSet::new(),
        }
    }

    pub fn battlefield_entry_tap_state(&self) -> Option<EtbTapState> {
        match self {
            ProposedEvent::ZoneChange { enter_tapped, .. }
            | ProposedEvent::CreateToken { enter_tapped, .. }
            | ProposedEvent::TokenEntry { enter_tapped, .. } => Some(*enter_tapped),
            _ => None,
        }
    }

    pub fn battlefield_entry_tap_state_mut(&mut self) -> Option<&mut EtbTapState> {
        match self {
            ProposedEvent::ZoneChange { enter_tapped, .. }
            | ProposedEvent::CreateToken { enter_tapped, .. }
            | ProposedEvent::TokenEntry { enter_tapped, .. } => Some(enter_tapped),
            _ => None,
        }
    }

    pub fn applied_set(&self) -> &HashSet<AppliedReplacementKey> {
        match self {
            ProposedEvent::ZoneChange { applied, .. }
            | ProposedEvent::Damage { applied, .. }
            | ProposedEvent::Draw { applied, .. }
            | ProposedEvent::SearchFound { applied, .. }
            | ProposedEvent::Scry { applied, .. }
            | ProposedEvent::Mill { applied, .. }
            | ProposedEvent::CoinFlip { applied, .. }
            | ProposedEvent::Explore { applied, .. }
            | ProposedEvent::Connive { applied, .. }
            | ProposedEvent::Proliferate { applied, .. }
            | ProposedEvent::LifeGain { applied, .. }
            | ProposedEvent::LifeLoss { applied, .. }
            | ProposedEvent::AddCounter { applied, .. }
            | ProposedEvent::RemoveCounter { applied, .. }
            | ProposedEvent::MoveCounter { applied, .. }
            | ProposedEvent::CreateToken { applied, .. }
            | ProposedEvent::TokenEntry { applied, .. }
            | ProposedEvent::Discard { applied, .. }
            | ProposedEvent::Tap { applied, .. }
            | ProposedEvent::Untap { applied, .. }
            | ProposedEvent::TurnFaceUp { applied, .. }
            | ProposedEvent::Destroy { applied, .. }
            | ProposedEvent::Sacrifice { applied, .. }
            | ProposedEvent::BeginTurn { applied, .. }
            | ProposedEvent::BeginPhase { applied, .. }
            | ProposedEvent::ProduceMana { applied, .. }
            | ProposedEvent::EmptyManaPool { applied, .. }
            | ProposedEvent::Planeswalk { applied, .. }
            | ProposedEvent::Attach { applied, .. } => applied,
        }
    }

    pub fn applied_set_mut(&mut self) -> &mut HashSet<AppliedReplacementKey> {
        match self {
            ProposedEvent::ZoneChange { applied, .. }
            | ProposedEvent::Damage { applied, .. }
            | ProposedEvent::Draw { applied, .. }
            | ProposedEvent::SearchFound { applied, .. }
            | ProposedEvent::Scry { applied, .. }
            | ProposedEvent::Mill { applied, .. }
            | ProposedEvent::CoinFlip { applied, .. }
            | ProposedEvent::Explore { applied, .. }
            | ProposedEvent::Connive { applied, .. }
            | ProposedEvent::Proliferate { applied, .. }
            | ProposedEvent::LifeGain { applied, .. }
            | ProposedEvent::LifeLoss { applied, .. }
            | ProposedEvent::AddCounter { applied, .. }
            | ProposedEvent::RemoveCounter { applied, .. }
            | ProposedEvent::MoveCounter { applied, .. }
            | ProposedEvent::CreateToken { applied, .. }
            | ProposedEvent::TokenEntry { applied, .. }
            | ProposedEvent::Discard { applied, .. }
            | ProposedEvent::Tap { applied, .. }
            | ProposedEvent::Untap { applied, .. }
            | ProposedEvent::TurnFaceUp { applied, .. }
            | ProposedEvent::Destroy { applied, .. }
            | ProposedEvent::Sacrifice { applied, .. }
            | ProposedEvent::BeginTurn { applied, .. }
            | ProposedEvent::BeginPhase { applied, .. }
            | ProposedEvent::ProduceMana { applied, .. }
            | ProposedEvent::EmptyManaPool { applied, .. }
            | ProposedEvent::Planeswalk { applied, .. }
            | ProposedEvent::Attach { applied, .. } => applied,
        }
    }

    pub fn already_applied(&self, id: &ReplacementId) -> bool {
        self.applied_set()
            .contains(&AppliedReplacementKey::for_event(self, *id))
    }

    pub fn mark_applied(&mut self, id: ReplacementId) {
        let key = AppliedReplacementKey::for_event(self, id);
        self.applied_set_mut().insert(key);
    }

    pub fn affected_player(&self, state: &crate::types::game_state::GameState) -> PlayerId {
        match self {
            // CR 614.12 + CR 109.4: A permanent entering under another player's
            // control (Tergrid's "onto the battlefield under your control",
            // reanimation "under your control", etc.) carries a
            // `controller_override`. The object itself is still in its origin
            // zone — typically a graveyard, where CR 109.4 gives it no controller
            // so `o.controller` defaults to the owner. "As-it-enters" replacement
            // effects (Mirrormade's "enter as a copy", CR 707.9) must be offered
            // to the controller the permanent WOULD have on the battlefield, so
            // honor the override before falling back to the object's controller.
            ProposedEvent::ZoneChange {
                object_id,
                controller_override,
                ..
            } => controller_override
                .or_else(|| state.objects.get(object_id).map(|o| o.controller))
                .unwrap_or(PlayerId(0)),
            ProposedEvent::Tap { object_id, .. }
            | ProposedEvent::Untap { object_id, .. }
            | ProposedEvent::TurnFaceUp { object_id, .. }
            | ProposedEvent::Destroy { object_id, .. }
            | ProposedEvent::RemoveCounter { object_id, .. }
            | ProposedEvent::Explore { object_id, .. }
            // CR 701.50a: The conniving permanent's controller is the affected
            // player — they draw/discard and choose the connive replacement order.
            | ProposedEvent::Connive { object_id, .. } => state
                .objects
                .get(object_id)
                .map(|o| o.controller)
                .unwrap_or(PlayerId(0)),
            ProposedEvent::AddCounter { placement, .. } => match placement {
                CounterPlacement::Object { object_id, .. } => state
                    .objects
                    .get(object_id)
                    .map(|o| o.controller)
                    .unwrap_or(PlayerId(0)),
                CounterPlacement::Player { player_id, .. }
                | CounterPlacement::Energy { player_id, .. } => *player_id,
            },
            ProposedEvent::MoveCounter {
                source_id,
                destination_id,
                stage,
                ..
            } => {
                let affected_id = match stage {
                    CounterMoveStage::Remove => source_id,
                    CounterMoveStage::Add => destination_id,
                };
                state
                    .objects
                    .get(affected_id)
                    .map(|o| o.controller)
                    .unwrap_or(PlayerId(0))
            }
            ProposedEvent::Damage { target, .. } => match target {
                TargetRef::Player(pid) => *pid,
                TargetRef::Object(oid) => state
                    .objects
                    .get(oid)
                    .map(|o| o.controller)
                    .unwrap_or(PlayerId(0)),
            },
            ProposedEvent::Draw { player_id, .. }
            | ProposedEvent::Scry { player_id, .. }
            | ProposedEvent::Mill { player_id, .. }
            | ProposedEvent::Proliferate { player_id, .. }
            | ProposedEvent::CoinFlip { player_id, .. }
            | ProposedEvent::LifeGain { player_id, .. }
            | ProposedEvent::LifeLoss { player_id, .. }
            | ProposedEvent::Discard { player_id, .. }
            | ProposedEvent::Sacrifice { player_id, .. }
            | ProposedEvent::BeginTurn { player_id, .. }
            | ProposedEvent::BeginPhase { player_id, .. }
            | ProposedEvent::ProduceMana { player_id, .. }
            | ProposedEvent::EmptyManaPool { player_id, .. }
            | ProposedEvent::Planeswalk { player_id, .. } => *player_id,
            // CR 616.1: a card in a library has no controller, so its owner
            // chooses among applicable replacements. `None` is reserved for a
            // nonlibrary selection, where no SearchFound replacement applies.
            ProposedEvent::SearchFound {
                searcher,
                library_owner,
                ..
            } => library_owner.unwrap_or(*searcher),
            ProposedEvent::CreateToken { owner, .. } => *owner,
            ProposedEvent::TokenEntry { entry_ref, .. } => state
                .liminal_entries
                .get(entry_ref)
                .map(|entry| entry.object.controller)
                .unwrap_or(PlayerId(0)),
            // CR 701.3a: The attaching Aura/Equipment's controller is the
            // affected player — they are the one who would choose a
            // replacement order (CR 616.1) and bind any "as it becomes
            // attached, choose" continuation.
            ProposedEvent::Attach { attachment_id, .. } => state
                .objects
                .get(attachment_id)
                .map(|o| o.controller)
                .unwrap_or(PlayerId(0)),
        }
    }

    /// Returns the primary object affected by this event, if any.
    pub fn affected_object_id(&self) -> Option<ObjectId> {
        match self {
            ProposedEvent::ZoneChange { object_id, .. }
            | ProposedEvent::Tap { object_id, .. }
            | ProposedEvent::Untap { object_id, .. }
            | ProposedEvent::TurnFaceUp { object_id, .. }
            | ProposedEvent::Destroy { object_id, .. }
            | ProposedEvent::RemoveCounter { object_id, .. }
            | ProposedEvent::Discard { object_id, .. }
            | ProposedEvent::Sacrifice { object_id, .. }
            | ProposedEvent::Explore { object_id, .. }
            | ProposedEvent::SearchFound { object_id, .. }
            // CR 614.1a: the conniving permanent is the affected object the
            // `valid_card` filter ("a creature you control") is matched against.
            | ProposedEvent::Connive { object_id, .. } => Some(*object_id),
            ProposedEvent::TokenEntry { entry_ref, .. } => Some(*entry_ref),
            ProposedEvent::AddCounter { placement, .. } => placement.object_id(),
            ProposedEvent::MoveCounter {
                source_id,
                destination_id,
                stage,
                ..
            } => Some(match stage {
                CounterMoveStage::Remove => *source_id,
                CounterMoveStage::Add => *destination_id,
            }),
            // CR 106.3: The mana source (land being tapped) is the affected object —
            // this is what `valid_card` filters are matched against.
            ProposedEvent::ProduceMana { source_id, .. } => Some(*source_id),
            ProposedEvent::Damage { target, .. } => match target {
                TargetRef::Object(oid) => Some(*oid),
                TargetRef::Player(_) => None,
            },
            ProposedEvent::Draw { .. }
            | ProposedEvent::Scry { .. }
            | ProposedEvent::Mill { .. }
            | ProposedEvent::Proliferate { .. }
            | ProposedEvent::CoinFlip { .. }
            | ProposedEvent::LifeGain { .. }
            | ProposedEvent::LifeLoss { .. }
            | ProposedEvent::CreateToken { .. }
            | ProposedEvent::BeginTurn { .. }
            | ProposedEvent::BeginPhase { .. }
            | ProposedEvent::EmptyManaPool { .. }
            // CR 701.31: a planeswalk has no affected object — the planar deck
            // rotation is not an object-scoped event.
            | ProposedEvent::Planeswalk { .. } => None,
            // CR 701.3a: the attaching Aura/Equipment is the object
            // `valid_card` filters (and "as it becomes attached" replacements)
            // match against.
            ProposedEvent::Attach { attachment_id, .. } => Some(*attachment_id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposed_event_variants_compile() {
        // Verify all variants compile, including the parameterized counter
        // placement recipients.
        let events: Vec<ProposedEvent> = vec![
            ProposedEvent::zone_change(ObjectId(1), Zone::Battlefield, Zone::Graveyard, None),
            ProposedEvent::Damage {
                source_id: ObjectId(1),
                target: TargetRef::Player(PlayerId(0)),
                amount: 3,
                is_combat: false,
                applied: HashSet::new(),
            },
            ProposedEvent::Draw {
                player_id: PlayerId(0),
                count: 1,
                applied: HashSet::new(),
            },
            ProposedEvent::Scry {
                player_id: PlayerId(0),
                count: 1,
                applied: HashSet::new(),
            },
            ProposedEvent::Mill {
                player_id: PlayerId(0),
                count: 1,
                destination: Zone::Graveyard,
                applied: HashSet::new(),
            },
            ProposedEvent::CoinFlip {
                player_id: PlayerId(0),
                count: 1,
                applied: HashSet::new(),
            },
            ProposedEvent::LifeGain {
                player_id: PlayerId(0),
                amount: 3,
                applied: HashSet::new(),
            },
            ProposedEvent::LifeLoss {
                player_id: PlayerId(0),
                amount: 3,
                applied: HashSet::new(),
            },
            ProposedEvent::AddCounter {
                placement: CounterPlacement::Object {
                    actor: PlayerId(0),
                    object_id: ObjectId(1),
                    counter_type: CounterType::Plus1Plus1,
                },
                count: 1,
                applied: HashSet::new(),
            },
            ProposedEvent::AddCounter {
                placement: CounterPlacement::Player {
                    actor: PlayerId(0),
                    player_id: PlayerId(0),
                    counter_kind: PlayerCounterKind::Poison,
                },
                count: 1,
                applied: HashSet::new(),
            },
            ProposedEvent::AddCounter {
                placement: CounterPlacement::Energy {
                    actor: PlayerId(0),
                    player_id: PlayerId(0),
                },
                count: 1,
                applied: HashSet::new(),
            },
            ProposedEvent::RemoveCounter {
                object_id: ObjectId(1),
                counter_type: CounterType::Plus1Plus1,
                count: 1,
                applied: HashSet::new(),
            },
            ProposedEvent::MoveCounter {
                actor: PlayerId(0),
                source_id: ObjectId(1),
                destination_id: ObjectId(2),
                counter_type: CounterType::Plus1Plus1,
                remove_count: 1,
                add_count: 1,
                stage: CounterMoveStage::Remove,
                applied: HashSet::new(),
            },
            ProposedEvent::CreateToken {
                owner: PlayerId(0),
                spec: Box::new(TokenSpec {
                    characteristics: TokenCharacteristics {
                        display_name: "Soldier".to_string(),
                        power: Some(1),
                        toughness: Some(1),
                        core_types: Vec::new(),
                        subtypes: Vec::new(),
                        supertypes: Vec::new(),
                        colors: Vec::new(),
                        keywords: Vec::new(),
                    },
                    script_name: "w_1_1_soldier".to_string(),
                    static_abilities: Vec::new(),
                    enter_with_counters: Vec::new(),
                    tapped: false,
                    enters_attacking: false,
                    sacrifice_at: None,
                    source_id: ObjectId(1),
                    controller: PlayerId(0),
                    attach_to: None,
                }),
                copy: None,
                enter_tapped: EtbTapState::Unspecified,
                count: 1,
                applied: HashSet::new(),
            },
            ProposedEvent::Discard {
                player_id: PlayerId(0),
                object_id: ObjectId(2),
                source_id: None,
                caused_by_effect: false,
                applied: HashSet::new(),
            },
            ProposedEvent::Tap {
                object_id: ObjectId(1),
                applied: HashSet::new(),
            },
            ProposedEvent::Untap {
                object_id: ObjectId(1),
                applied: HashSet::new(),
            },
            ProposedEvent::Destroy {
                object_id: ObjectId(1),
                source: None,
                cant_regenerate: false,
                applied: HashSet::new(),
            },
            ProposedEvent::Sacrifice {
                object_id: ObjectId(1),
                player_id: PlayerId(0),
                applied: HashSet::new(),
            },
            ProposedEvent::begin_turn(PlayerId(0), false),
            ProposedEvent::begin_phase(PlayerId(0), Phase::Untap),
            ProposedEvent::produce_mana(ObjectId(1), PlayerId(0), ManaType::Green),
            ProposedEvent::EmptyManaPool {
                player_id: PlayerId(0),
                units: Vec::new(),
                applied: HashSet::new(),
            },
            ProposedEvent::Attach {
                attachment_id: ObjectId(1),
                target_id: ObjectId(2),
                applied: HashSet::new(),
            },
        ];
        assert_eq!(events.len(), 24);
    }

    #[test]
    fn replacement_id_equality_and_hash() {
        let id1 = ReplacementId {
            source: ObjectId(1),
            index: 0,
        };
        let id2 = ReplacementId {
            source: ObjectId(1),
            index: 0,
        };
        let id3 = ReplacementId {
            source: ObjectId(1),
            index: 1,
        };
        assert_eq!(id1, id2);
        assert_ne!(id1, id3);

        let mut set = HashSet::new();
        set.insert(id1);
        assert!(set.contains(&id2));
        assert!(!set.contains(&id3));
    }

    #[test]
    fn add_counter_object_serde_keeps_legacy_flat_shape() {
        let event = ProposedEvent::AddCounter {
            placement: CounterPlacement::Object {
                actor: PlayerId(0),
                object_id: ObjectId(1),
                counter_type: CounterType::Plus1Plus1,
            },
            count: 1,
            applied: HashSet::new(),
        };

        let value = serde_json::to_value(&event).unwrap();
        let add_counter = value
            .get("AddCounter")
            .expect("externally tagged AddCounter variant")
            .as_object()
            .expect("AddCounter payload object");
        assert!(add_counter.get("placement").is_none());
        assert!(add_counter.get("actor").is_some());
        assert!(add_counter.get("object_id").is_some());
        assert!(add_counter.get("counter_type").is_some());

        let roundtrip: ProposedEvent = serde_json::from_value(value).unwrap();
        assert!(matches!(
            roundtrip,
            ProposedEvent::AddCounter {
                placement: CounterPlacement::Object {
                    actor: PlayerId(0),
                    object_id: ObjectId(1),
                    counter_type: CounterType::Plus1Plus1,
                },
                count: 1,
                ..
            }
        ));
    }

    #[test]
    fn add_counter_object_serde_accepts_legacy_missing_actor() {
        let event = ProposedEvent::AddCounter {
            placement: CounterPlacement::Object {
                actor: PlayerId(0),
                object_id: ObjectId(1),
                counter_type: CounterType::Plus1Plus1,
            },
            count: 1,
            applied: HashSet::new(),
        };
        let mut value = serde_json::to_value(&event).unwrap();
        value
            .get_mut("AddCounter")
            .and_then(|payload| payload.as_object_mut())
            .expect("AddCounter payload object")
            .remove("actor");

        let roundtrip: ProposedEvent = serde_json::from_value(value).unwrap();
        assert!(matches!(
            roundtrip,
            ProposedEvent::AddCounter {
                placement: CounterPlacement::Object {
                    actor: PlayerId(0),
                    object_id: ObjectId(1),
                    counter_type: CounterType::Plus1Plus1,
                },
                count: 1,
                ..
            }
        ));
    }

    #[test]
    fn mark_applied_and_already_applied() {
        let mut event = ProposedEvent::Draw {
            player_id: PlayerId(0),
            count: 1,
            applied: HashSet::new(),
        };
        let rid = ReplacementId {
            source: ObjectId(5),
            index: 0,
        };
        assert!(!event.already_applied(&rid));
        event.mark_applied(rid);
        assert!(event.already_applied(&rid));
    }

    #[test]
    fn move_counter_stage_controls_affected_object() {
        let remove = ProposedEvent::MoveCounter {
            actor: PlayerId(0),
            source_id: ObjectId(1),
            destination_id: ObjectId(2),
            counter_type: CounterType::Plus1Plus1,
            remove_count: 1,
            add_count: 1,
            stage: CounterMoveStage::Remove,
            applied: HashSet::new(),
        };
        let add = ProposedEvent::MoveCounter {
            actor: PlayerId(0),
            source_id: ObjectId(1),
            destination_id: ObjectId(2),
            counter_type: CounterType::Plus1Plus1,
            remove_count: 1,
            add_count: 1,
            stage: CounterMoveStage::Add,
            applied: HashSet::new(),
        };

        assert_eq!(remove.affected_object_id(), Some(ObjectId(1)));
        assert_eq!(add.affected_object_id(), Some(ObjectId(2)));
    }

    /// SHAPE: `ProposedEvent::EmptyManaPool` survives a serde roundtrip with
    /// non-empty `units` and `applied` populated. Verifies the new variant
    /// participates in the discriminated-union tag/content protocol used over
    /// the WASM boundary and in persisted state snapshots.
    #[test]
    fn empty_mana_pool_serde_roundtrip() {
        use crate::types::mana::UnitDisposition;
        let event = ProposedEvent::EmptyManaPool {
            player_id: PlayerId(1),
            units: vec![
                UnitDecision {
                    pool_index: 0,
                    color: ManaType::Green,
                    disposition: UnitDisposition::Drop,
                },
                UnitDecision {
                    pool_index: 1,
                    color: ManaType::Red,
                    disposition: UnitDisposition::Recolor(ManaType::Colorless),
                },
                UnitDecision {
                    pool_index: 2,
                    color: ManaType::White,
                    disposition: UnitDisposition::Keep,
                },
            ],
            applied: {
                let mut s = HashSet::new();
                s.insert(AppliedReplacementKey::object(ObjectId(42), 3));
                s
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: ProposedEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn applied_key_keeps_floating_and_step_end_mana_distinct() {
        let mut applied = HashSet::new();
        applied.insert(AppliedReplacementKey::floating(0));
        applied.insert(AppliedReplacementKey::step_end_mana(0));

        assert_eq!(applied.len(), 2);
        assert!(applied.contains(&AppliedReplacementKey::floating(0)));
        assert!(applied.contains(&AppliedReplacementKey::step_end_mana(0)));
    }

    #[test]
    fn legacy_empty_mana_pool_sentinel_deserializes_to_step_end_mana() {
        let json = serde_json::json!({
            "EmptyManaPool": {
                "player_id": 0,
                "units": [],
                "applied": [{ "source": 0, "index": 2 }]
            }
        });

        let event: ProposedEvent = serde_json::from_value(json).unwrap();
        let ProposedEvent::EmptyManaPool { applied, .. } = event else {
            panic!("expected EmptyManaPool");
        };
        assert!(applied.contains(&AppliedReplacementKey::step_end_mana(2)));
        assert!(!applied.contains(&AppliedReplacementKey::floating(2)));
    }

    #[test]
    fn legacy_token_entry_sentinel_deserializes_to_floating() {
        let json = serde_json::json!({
            "TokenEntry": {
                "entry_ref": 99,
                "applied": [{ "source": 0, "index": 2 }]
            }
        });

        let event: ProposedEvent = serde_json::from_value(json).unwrap();
        let ProposedEvent::TokenEntry { applied, .. } = event else {
            panic!("expected TokenEntry");
        };
        assert!(applied.contains(&AppliedReplacementKey::floating(2)));
        assert!(!applied.contains(&AppliedReplacementKey::step_end_mana(2)));
    }
}
