pub use phase_ast::types::ability::*;

use std::collections::HashSet;

use serde::de;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::card::PrintedCardRef;
use super::card_type::CoreType;
use super::counter::CounterType;
use super::game_state::{CastOccurrence, LKISnapshot, MayTriggerOrigin, TriggerSourceContext};
use super::game_state_kinds::{DistributionUnit, TargetSelectionConstraint};
use super::identifiers::{
    CardId, EpicEffectId, ObjectId, ObjectIncarnationRef, LEGACY_INCARNATION,
};
use super::keywords::Keyword;
use super::mana::{ManaColor, ManaCost, ManaType};
use super::phase::Phase;
use super::player::PlayerId;
use super::proposed_event::AppliedReplacementKey;
use super::zones::Zone;
use crate::game::game_object::ObjectIncarnationRefExt;
use crate::types::object_data::DisplayIdentity;

/// CR 702.104a + CR 702.104b: The outcome of the Tribute choice the chosen opponent
/// made as the creature entered the battlefield. Persisted as a `ChosenAttribute` on
/// the Tribute creature so the companion "if tribute wasn't paid" trigger (CR
/// 702.104b) can read the decision. A typed enum rather than a `bool` so the absence
/// of any `TributeOutcome` remains distinguishable from an explicit `Declined`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TributeOutcome {
    /// The chosen opponent placed the Tribute +1/+1 counters (CR 702.104a).
    Paid,
    /// The chosen opponent declined (CR 702.104b: "if tribute wasn't paid").
    Declined,
}

/// A typed choice stored on a permanent (e.g., "choose a color" → Color(Red)).
/// The variant discriminant serves as the category key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum ChosenAttribute {
    Color(ManaColor),
    CreatureType(String),
    BasicLandType(BasicLandType),
    CardType(CoreType),
    OddOrEven(Parity),
    CardName(String),
    /// Stores a chosen number (e.g., "choose a number" for Talion).
    ///
    /// On the PLAYER axis (`Player::chosen_attributes`) this is the SECRET half
    /// of the CR 101.4 secret-number ledger: `game::visibility` redacts it from
    /// every viewer but its owner. `Effect::RevealChosenNumbers` converts it to
    /// [`ChosenAttribute::RevealedNumber`], which is public — that conversion is
    /// the card's "reveal" instruction as an observable state transition.
    Number(u32),
    /// CR 101.4 + CR 608.2c: A chosen number that a reveal instruction has
    /// PUBLISHED ("then all players reveal those numbers simultaneously").
    /// Identical in value to [`ChosenAttribute::Number`] and read
    /// interchangeably with it by `Player::chosen_number`; the two differ only
    /// in visibility, which is exactly what the reveal changes. Kept as a
    /// distinct variant rather than a flag so the secret and published states
    /// cannot be confused at a read site, and so `game::visibility` redacts on
    /// the type rather than on a condition it might forget to check.
    RevealedNumber(u32),
    /// Stores the chosen opponent/player ID.
    Player(PlayerId),
    /// Stores two chosen colors as a pair.
    TwoColors([ManaColor; 2]),
    /// CR 702.104a + CR 702.104b: Records whether the opponent chosen for the
    /// Tribute ETB replacement paid tribute or declined. Read by the companion
    /// `TriggerCondition::TributeNotPaid` evaluator.
    TributeOutcome(TributeOutcome),
    /// CR 608.2d: Records the typed keyword chosen from a `ChoiceType::Keyword`
    /// option list (Urborg / Walking Sponge "choose an ability the target has,
    /// remove it"). Read by `ContinuousModification::RemoveChosenKeyword` at
    /// Layer 6 evaluation to strip the chosen keyword from the recipient.
    Keyword(Keyword),
    /// CR 614.12c: Records the anchor-word label chosen as the permanent
    /// entered the battlefield (e.g. Frostcliff Siege "Jeskai" / "Temur",
    /// Khans of Tarkir Sieges "Khans" / "Dragons"). Read by
    /// `StaticCondition::ChosenLabelIs` and `TriggerCondition::ChosenLabelIs`
    /// to gate which of the two anchor-word-marked abilities (CR 607: linked
    /// abilities) functions while the permanent is on the battlefield. The
    /// label is stored case-canonicalised to match `ChoiceType::Labeled`'s
    /// capitalised option list.
    Label(String),
    /// CR 607.2d + CR 608.2c + CR 400.7: A remembered card object — the "last
    /// chosen card" for cards like Koh, the Face Stealer ("Koh has all activated
    /// and triggered abilities of the last chosen card"). Unlike every other
    /// variant, this is NOT a player-prompted choice category: it is written by
    /// `Effect::RememberCard` directly from the resolution chain's tracked set,
    /// not produced through `ChoiceType`/`ChoiceValue`/`from_choice` (the same
    /// engine-set precedent as `TributeOutcome`). It is consumed by
    /// `TargetFilter::ChosenCard` — the zone-agnostic remembered-object reader —
    /// on both the live-object path and the CR 603.10a leaves-the-battlefield
    /// look-back path. A reader whose linked ability requires a zone (CR 607.2a
    /// "exiled with") composes `FilterProp::InZone` at its emission site rather
    /// than in the shared reader. Being stored in `chosen_attributes`, the value
    /// is cleared automatically when the source permanent changes zones
    /// (CR 400.7). Replace-on-rechoose: `RememberCard` removes any prior `Card`
    /// before pushing.
    ///
    /// CR 400.7: the value is an [`ObjectIncarnationRef`] pin — the chosen
    /// object's storage id AND its incarnation at choice time (captured by
    /// `RememberCard` from the live object). An object that changes zones
    /// becomes a new object at the same storage id with a bumped incarnation,
    /// so a stale pin deliberately does not match the returned object. The
    /// pre-migration wire form stored a bare `ObjectId`; the
    /// `ObjectIncarnationRef` compat shim deserializes that legacy shape to
    /// [`LEGACY_INCARNATION`], a value no real incarnation can equal, so a
    /// legacy record matches nothing (fail-closed).
    Card(ObjectIncarnationRef),
    /// CR 608.2d + CR 122.1: The counter kind chosen from a `ChoiceType::CounterKind`
    /// option list (The Caves of Androzani "choose a counter on it"). Read by
    /// `Effect::PutChosenCounter` ("put an additional counter of that kind on
    /// that permanent") to add one counter of the chosen kind. Replace-on-rechoose
    /// (the per-iteration bind clears the prior `Counter` — see `bind_named_choice`).
    Counter(CounterType),
    /// CR 607.2d: The chosen seat direction (left/right) for a directional
    /// "choose left or right" ability linked to a "the [last] chosen direction"
    /// static (Pramikon, Sky Rampart; Mystic Barrier; Teyo, Geometric
    /// Tactician). Stored on the source's `chosen_attributes` and read by
    /// `GameObject::chosen_direction()` during the CR 508.1c attacker gate.
    /// Replace-on-rechoose: the {Left,Right} hijack in `choose.rs` removes any
    /// prior `Direction` before pushing, so only the "last chosen" survives.
    Direction(SeatDirection),
    /// CR 707.2c + CR 614.12a + CR 400.7: The copiable-values snapshot chosen as
    /// an `Effect::ChoosePermanent` Aura entered (Metamorphic Alteration:
    /// "choose a creature"). Like `Card` and `TributeOutcome`, this is
    /// ENGINE-SET (written directly at the `CopyTargetChoice` answer, not
    /// produced through `ChoiceType`/`from_choice`) — it records the frozen
    /// snapshot the companion copy effect was installed from, and, being stored
    /// in `chosen_attributes`, is cleared automatically when the Aura changes
    /// zones (CR 400.7), which is exactly the copy's lifetime. Boxed to keep
    /// the enum small (mirrors the copy-value box idiom).
    CopiableSnapshot(Box<LatchedCopiableSnapshot>),
    /// CR 106.1b + CR 400.7: The mana type(s) spent to pay a past activation of
    /// this permanent's own ability ("Note the type of mana spent to pay this
    /// activation cost" — Jeweled Amulet). Like `Card` and
    /// `TributeOutcome`, this is ENGINE-SET (written by `Effect::NoteManaSpent`
    /// from the source's transient `mana_spent_to_activate` payment latch, not
    /// produced through `ChoiceType`/`from_choice`) — it is never a
    /// player-prompted choice. Read by `ManaProduction::NotedType` at a
    /// companion mana ability's resolution. Being stored in `chosen_attributes`,
    /// it is cleared automatically when the source permanent changes zones (CR
    /// 400.7), matching the ruling that a freshly entered amulet has no noted
    /// type. Replace-on-rechoose: `Effect::NoteManaSpent` removes any prior
    /// `NotedManaSpent` before pushing.
    NotedManaSpent(Vec<ManaType>),
}

impl ChosenAttribute {
    /// The chosen creature type, if this is a creature-type choice.
    pub fn creature_type(&self) -> Option<&str> {
        match self {
            Self::CreatureType(chosen) => Some(chosen.as_str()),
            Self::Color(_)
            | Self::BasicLandType(_)
            | Self::CardType(_)
            | Self::OddOrEven(_)
            | Self::CardName(_)
            | Self::Number(_)
            | Self::RevealedNumber(_)
            | Self::Player(_)
            | Self::TwoColors(_)
            | Self::TributeOutcome(_)
            | Self::Keyword(_)
            | Self::Label(_)
            | Self::Card(_)
            | Self::Counter(_)
            | Self::Direction(_)
            | Self::CopiableSnapshot(_)
            | Self::NotedManaSpent(_) => None,
        }
    }

    /// Which category of choice this represents.
    pub fn choice_type(&self) -> ChoiceType {
        match self {
            Self::Color(_) => ChoiceType::color(),
            Self::CreatureType(_) => ChoiceType::creature_type(),
            Self::BasicLandType(_) => ChoiceType::BasicLandType,
            Self::CardType(_) => ChoiceType::card_type(),
            Self::OddOrEven(_) => ChoiceType::OddOrEven,
            Self::CardName(_) => ChoiceType::CardName,
            // CR 101.4: the secret and the published number came from the same
            // `NumberRange` prompt; revealing changes visibility, not category.
            // CR 107.1a/b: recovering the CATEGORY from a stored value cannot
            // recover the card's original bounds, so report the widest form the
            // rules allow rather than inventing a ceiling this value never had.
            Self::Number(_) | Self::RevealedNumber(_) => ChoiceType::NumberRange {
                min: 0,
                max: None,
                distinctness: NumberDistinctness::Repeatable,
            },
            // Player covers both Player and Opponent choice types
            Self::Player(_) => ChoiceType::player(),
            Self::TwoColors(_) => ChoiceType::TwoColors,
            // CR 702.104: Tribute outcome uses a dedicated prompt type rather than
            // a NamedChoice (two fixed labels: Paid / Declined). Classify under the
            // Labeled category so external listings (e.g., AI candidate generation)
            // can recognise it as a Yes/No-shaped prompt.
            Self::TributeOutcome(_) => ChoiceType::Labeled {
                options: vec!["Paid".to_string(), "Declined".to_string()],
            },
            // CR 608.2d: A category template — the concrete option list is
            // attached to each emission site (Urborg lists FirstStrike +
            // Swampwalk; another card might list any pair). Mirrors the
            // `NumberRange { min: 0, max: 20 }` template idiom.
            Self::Keyword(_) => ChoiceType::Keyword {
                options: Vec::new(),
                count: 1,
            },
            // CR 614.12c: Anchor-word labels are a free-form labeled choice
            // (the per-card option list — e.g. ["Jeskai", "Temur"] — is
            // emitted at the choice site, mirroring the `Keyword` template
            // idiom). The stored single label is one of those options.
            Self::Label(label) => ChoiceType::Labeled {
                options: vec![label.clone()],
            },
            // Engine-set, never player-prompted (written by `Effect::RememberCard`
            // from a tracked set, not via `from_choice`). `choice_type()` has no
            // callers for this variant; the empty `Labeled` template mirrors the
            // `Keyword`/`TributeOutcome` placeholder idiom rather than inventing a
            // spurious object-choice category.
            Self::Card(_) => ChoiceType::Labeled {
                options: Vec::new(),
            },
            // CR 608.2d + CR 122.1: A category template — the concrete counter-kind
            // option list is enumerated at each resolution from the target object's
            // counters (mirrors the `Keyword` template idiom).
            Self::Counter(_) => ChoiceType::CounterKind {
                options: Vec::new(),
            },
            // CR 607.2d: The directional choice is a two-option labeled prompt.
            // The {Left,Right} hijack in `choose.rs` recognises exactly this
            // option set to persist a typed `Direction` rather than a `Label`.
            Self::Direction(_) => ChoiceType::Labeled {
                options: vec!["Left".into(), "Right".into()],
            },
            // Engine-set, never player-prompted (written directly at the
            // `CopyTargetChoice` answer, not via `from_choice`). Mirrors the
            // `Card` placeholder: an empty `Labeled` template rather than
            // inventing a spurious copy-snapshot choice category.
            Self::CopiableSnapshot(_) => ChoiceType::Labeled {
                options: Vec::new(),
            },
            // Engine-set, never player-prompted (written by
            // `Effect::NoteManaSpent` from the cost-payment latch, not via
            // `from_choice`). Mirrors the `Card`/`CopiableSnapshot` placeholder.
            Self::NotedManaSpent(_) => ChoiceType::Labeled {
                options: Vec::new(),
            },
        }
    }

    /// Parse a player's string response into a typed ChosenAttribute.
    /// Returns None if the string doesn't match the expected choice type.
    pub fn from_choice(choice_type: ChoiceType, value: &str) -> Option<Self> {
        match ChoiceValue::from_choice(&choice_type, value)? {
            ChoiceValue::Color(color) => Some(Self::Color(color)),
            ChoiceValue::CreatureType(creature_type) => Some(Self::CreatureType(creature_type)),
            ChoiceValue::BasicLandType(land_type) => Some(Self::BasicLandType(land_type)),
            ChoiceValue::CardType(card_type) => Some(Self::CardType(card_type)),
            ChoiceValue::OddOrEven(parity) => Some(Self::OddOrEven(parity)),
            ChoiceValue::CardName(card_name) => Some(Self::CardName(card_name)),
            ChoiceValue::Player(id) => Some(Self::Player(id)),
            ChoiceValue::TwoColors(colors) => Some(Self::TwoColors(colors)),
            ChoiceValue::Number(n) => Some(Self::Number(n)),
            ChoiceValue::Keyword(keyword) => Some(Self::Keyword(keyword)),
            // CR 614.12c: Persist a labeled choice as an anchor-word label so
            // companion `ChosenLabelIs` conditions (static + trigger) can read
            // it for the lifetime of the permanent.
            ChoiceValue::Label(label) => Some(Self::Label(label)),
            // CR 608.2d + CR 122.1: Persist the chosen counter kind so a later
            // `Effect::PutChosenCounter` can read it.
            ChoiceValue::Counter(counter_type) => Some(Self::Counter(counter_type)),
            ChoiceValue::CardPredicate(_) => None,
            ChoiceValue::LandType(_) => None,
        }
    }
}

/// Why a specific activated ability is currently blocked from activation.
///
/// Display read-out only: carries no enforcement authority.
///
/// **Two channels, one enum.** The first three arms mirror the three CR 602.5
/// enforcement predicates in `game::casting`, in the order those gates consult
/// them, and are published on `GameObject::blocked_abilities` by the
/// `derived.rs` sweep. The fourth arm mirrors the CR 118.3 affordability gate
/// that follows them in the same function, and is published on the
/// legal-actions payload for the acting player only — see
/// `ai_support::activation_block_reasons`. A consumer of one channel will never
/// observe the other's kinds; the object field is NOT missing the fourth arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AbilityBlockKind {
    /// CR 602.5: A static ability prohibits this ability from being activated at
    /// all (Pithing Needle's named source, Immortal Sun's loyalty abilities).
    CantBeActivated,
    /// CR 602.5 + CR 117.1b: A timing restriction forbids activation right now
    /// (City of Solitude — only during the affected player's own turn).
    CantActivateDuring,
    /// CR 602.5: A temporary continuous effect prohibits this activity axis for
    /// the affected players (Kang-class `ProhibitActivity`).
    Prohibited,
    /// CR 118.3: The player can't pay this ability's activation cost right now —
    /// they lack the necessary resources ("a player with only 1 life can't pay a
    /// cost of 2 life"). Every other activation requirement is satisfied: the
    /// CR 602.5 prohibitions above, the zone, the timing restrictions, CR 302.6
    /// summoning sickness and target legality all passed.
    ///
    /// CR 602.2b + CR 601.2f: the cost weighed is the POST-REDUCTION activation
    /// cost — CR 602.2b makes an activated ability's activation cost the analog
    /// of a spell's mana cost "as referenced in rule 601.2f", so the gate applies
    /// cost reduction before asking whether the player can pay.
    ///
    /// `sources` is always empty for this arm: a resource shortfall is a property
    /// of the player's own board, so no external object prohibits it.
    ///
    /// **The name deliberately avoids CR 118.6's "unpayable".** CR 118.6 makes
    /// that a term of art for a cost that can NEVER be paid (an object with no
    /// mana cost, or a cost derived from one); this arm is the opposite — a
    /// shortfall *right now*, which more mana or more life clears. The
    /// distinction is enforced by `AbilityCost::payability_verdict_is_resource_based`,
    /// which keeps refusals that are structural rather than resource-based out of
    /// this arm.
    ///
    /// Published only on the legal-actions payload (see the two-channel note on
    /// this enum), never on `GameObject::blocked_abilities`.
    CostNotPayableNow,
}

/// A block reason paired with every prohibiting source object of this kind.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AbilityBlockReason {
    /// CR 602.5: sorted, deduped permanents whose static/effect each independently
    /// impose this block kind (two Pithing Needles naming the same card → both).
    ///
    /// CR 118.3: empty for `AbilityBlockKind::CostNotPayableNow` — a resource
    /// shortfall has no prohibiting source object.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<ObjectId>,
    #[serde(flatten)]
    pub kind: AbilityBlockKind,
}

/// A single blocked-ability read-out entry: which ability index is blocked and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbilityBlockEntry {
    /// Index into the object's activated-ability definition space
    /// (`activated_ability_definitions`): `0..printed_len` for printed abilities,
    /// `printed_len + offset` for runtime-granted ones.
    pub ability_index: usize,
    #[serde(flatten)]
    pub reason: AbilityBlockReason,
}

/// CR 608.2g + CR 609.4b: how a cast-during-resolution pays.
///
/// `Free` is the Cascade/Discover/Ripple/Suspend/free-window shape — the card is
/// cast without paying its mana cost, so the `ExileWithAltCost` permission zeros
/// the cost and payment is `Auto`. `FullCost` is the Quistis Trepe / Tinybones
/// the Pickpocket shape — "you may cast target X card from a graveyard, and mana
/// of any type can be spent to cast that spell." The caster pays the card's real
/// printed cost (CR 609.4b: any-type concession affects only *how* the cost is
/// paid, not the cost itself), so the permission carries `SelfManaCost` and
/// payment is `Manual` so a mana-payment window opens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ResolutionCastCost {
    /// Cast without paying the mana cost (Cascade, Discover, Ripple, Suspend,
    /// Invoke Calamity's free-cast window). Zero cost, `Auto` payment.
    Free,
    /// CR 609.4b: Cast paying the card's live printed cost. The optional
    /// `mana_spend_permission` rides onto the granted permission so mana of any
    /// type can be spent off-color (Quistis Trepe, Tinybones the Pickpocket).
    FullCost {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mana_spend_permission: Option<ManaSpendPermission>,
        /// CR 601.2b + CR 118.8: an additional mana cost the granting effect
        /// attaches ("by paying {R}{R} in addition to its other costs", Ogre
        /// Battlecaster), added to the tax-inclusive base before cost
        /// modifiers apply (CR 601.2f).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        additional_cost: Option<crate::types::mana::ManaCost>,
    },
    /// CR 118.9 + CR 702.62a: Cast paying a specific alternative mana cost
    /// borrowed from a keyword (e.g., The Face of Boe's suspend cost). The
    /// mana cost is explicit (not `SelfManaCost`) and payment is `Auto` so the
    /// pool is drained automatically during resolution (the triggering player's
    /// pool already holds the keyword cost before the trigger resolves). `None`
    /// from `effective_keyword_mana_cost` aborts before this is constructed
    /// (see `cast_from_zone::complete_hand_pick_cast_from_zone`).
    AlternativeMana { cost: crate::types::mana::ManaCost },
}

#[cfg(test)]
mod resolution_cast_face_policy_serde_tests {
    use super::test_helpers::policy;
    use super::*;
    use crate::types::game_state::CastOfferKind;

    fn window(policy: ResolutionCastFacePolicy) -> CastOfferKind {
        CastOfferKind::FreeCastWindow {
            candidates: vec![ObjectId(702)],
            remaining_casts: Some(2),
            remaining_mv_budget: Some(7),
            face_policy: policy,
            zones: vec![Zone::Exile],
            graveyard_replacement: None,
            enters_with_modifications: Default::default(),
            member_pool: vec![ObjectId(702)],
        }
    }

    #[test]
    fn resolution_cast_face_policy_round_trips_exactly_through_every_window_carrier() {
        let policy = policy();
        let cleanup = ResolutionCastCleanup {
            source_id: ObjectId(701),
            offer_id: None,
            face_policy: policy.clone(),
            exiled_misses: Vec::new(),
            reject_action: ResolutionMvRejectAction::RemainExiled,
            success_action: ResolutionCastSuccessAction::FreeCastOfferRemaining {
                controller: PlayerId(1),
                remaining_casts: Some(2),
                remaining_mv_budget: Some(7),
                face_policy: Box::new(policy.clone()),
                zones: vec![Zone::Exile],
                graveyard_replacement: None,
                enters_with_modifications: Default::default(),
                member_pool: vec![ObjectId(702)],
            },
            delayed_trigger_receipts: Vec::new(),
            indicated_action: None,
        };
        let round_tripped_policy: ResolutionCastFacePolicy =
            serde_json::from_value(serde_json::to_value(&policy).unwrap()).unwrap();
        let round_tripped_window: CastOfferKind =
            serde_json::from_value(serde_json::to_value(window(policy.clone())).unwrap()).unwrap();
        let round_tripped_cleanup: ResolutionCastCleanup =
            serde_json::from_value(serde_json::to_value(&cleanup).unwrap()).unwrap();

        assert_eq!(round_tripped_policy, policy);
        assert_eq!(round_tripped_window, window(policy.clone()));
        assert_eq!(round_tripped_cleanup, cleanup);
        let ResolutionCastSuccessAction::FreeCastOfferRemaining { face_policy, .. } =
            round_tripped_cleanup.success_action
        else {
            panic!("fixture must carry the re-offer chain");
        };
        assert_eq!(*face_policy, policy);
    }

    /// CR 608.2c + CR 611.2a (row 6.8): both serialized carriers of the gains
    /// rider (`CastOfferKind::FreeCastWindow`,
    /// `ResolutionCastSuccessAction::FreeCastOfferRemaining`) deserialize a
    /// pre-fix payload without the field to the default rider, omit the field
    /// when the rider is empty, and round-trip a carried rider (including its
    /// typed cast-quality predicate) exactly.
    #[test]
    fn gains_rider_field_back_compat_and_round_trip_on_both_carriers() {
        use crate::types::ability::{ContinuousModification, PendingEntersModifications};
        use crate::types::keywords::Keyword;

        let rider = PendingEntersModifications {
            modifications: vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Haste,
            }],
            // CR 608.2c + CR 601.2a: the printed quality rides the carrier as a
            // typed predicate; the round-trip must preserve it.
            cast_filter: Some(TargetFilter::Typed(
                TypedFilter::default().with_type(TypeFilter::Creature),
            )),
        };

        // FreeCastWindow — an empty rider is skipped on the wire and the
        // field-less shape deserializes to the same value.
        let window_value = serde_json::to_value(window(policy())).unwrap();
        assert!(
            window_value
                .as_object()
                .is_some_and(|object| !object.contains_key("enters_with_modifications")),
            "an empty window rider must be skipped on the wire"
        );
        assert_eq!(
            serde_json::from_value::<CastOfferKind>(window_value).unwrap(),
            window(policy())
        );

        // FreeCastWindow — a carried rider round-trips exactly.
        let ridden_window = CastOfferKind::FreeCastWindow {
            candidates: vec![ObjectId(702)],
            remaining_casts: Some(2),
            remaining_mv_budget: Some(7),
            face_policy: policy(),
            zones: vec![Zone::Exile],
            graveyard_replacement: None,
            enters_with_modifications: rider.clone(),
            member_pool: vec![ObjectId(702)],
        };
        let encoded = serde_json::to_value(&ridden_window).unwrap();
        assert_eq!(
            encoded["enters_with_modifications"],
            serde_json::to_value(&rider).unwrap(),
            "a carried rider must serialize under the engine field name"
        );
        // The typed cast-quality predicate is a populated sub-field, not an
        // omitted one, and survives on its own.
        assert!(
            encoded["enters_with_modifications"]["cast_filter"].is_object(),
            "a populated cast-quality predicate must serialize, got {encoded}"
        );
        assert_eq!(
            serde_json::from_value::<CastOfferKind>(encoded).unwrap(),
            ridden_window
        );

        // A rider with modifications but no printed quality omits only the
        // optional predicate sub-field, and the field-less shape reads back as
        // the unconditional rider.
        let unqualified = PendingEntersModifications {
            modifications: rider.modifications.clone(),
            cast_filter: None,
        };
        let encoded_unqualified = serde_json::to_value(&unqualified).unwrap();
        assert!(
            encoded_unqualified.get("cast_filter").is_none(),
            "an absent cast-quality predicate must be skipped, got {encoded_unqualified}"
        );
        assert_eq!(
            serde_json::from_value::<PendingEntersModifications>(encoded_unqualified).unwrap(),
            unqualified
        );

        // FreeCastOfferRemaining — the same three properties on the re-offer
        // carrier.
        let ridden_action = ResolutionCastSuccessAction::FreeCastOfferRemaining {
            controller: PlayerId(1),
            remaining_casts: Some(2),
            remaining_mv_budget: Some(7),
            face_policy: Box::new(policy()),
            zones: vec![Zone::Exile],
            graveyard_replacement: None,
            enters_with_modifications: rider.clone(),
            member_pool: vec![ObjectId(702)],
        };
        let encoded_action = serde_json::to_value(&ridden_action).unwrap();
        assert_eq!(
            encoded_action["enters_with_modifications"],
            serde_json::to_value(&rider).unwrap()
        );
        assert_eq!(
            serde_json::from_value::<ResolutionCastSuccessAction>(encoded_action).unwrap(),
            ridden_action
        );
        let empty_action = ResolutionCastSuccessAction::FreeCastOfferRemaining {
            controller: PlayerId(1),
            remaining_casts: Some(2),
            remaining_mv_budget: Some(7),
            face_policy: Box::new(policy()),
            zones: vec![Zone::Exile],
            graveyard_replacement: None,
            enters_with_modifications: Default::default(),
            member_pool: vec![ObjectId(702)],
        };
        let mut pre_fix_action = serde_json::to_value(&empty_action).unwrap();
        assert!(
            pre_fix_action
                .as_object()
                .is_some_and(|object| !object.contains_key("enters_with_modifications")),
            "an empty re-offer rider must be skipped on the wire"
        );
        // A pre-fix payload that never had the key at all is the same shape.
        pre_fix_action
            .as_object_mut()
            .unwrap()
            .remove("enters_with_modifications");
        assert_eq!(
            serde_json::from_value::<ResolutionCastSuccessAction>(pre_fix_action).unwrap(),
            empty_action,
            "a field-less payload must read as an empty rider"
        );
    }

    #[test]
    fn missing_or_malformed_window_face_policy_fails_closed() {
        let mut old_shape = serde_json::to_value(window(policy())).unwrap();
        old_shape
            .as_object_mut()
            .expect("enum serializes as object")
            .remove("face_policy");
        assert!(serde_json::from_value::<CastOfferKind>(old_shape).is_err());

        let mut malformed = serde_json::to_value(window(policy())).unwrap();
        malformed["face_policy"] = serde_json::json!({"filter": "not-a-filter"});
        assert!(serde_json::from_value::<CastOfferKind>(malformed).is_err());

        let mut cleanup = serde_json::to_value(ResolutionCastCleanup {
            source_id: ObjectId(701),
            offer_id: None,
            face_policy: policy(),
            exiled_misses: Vec::new(),
            reject_action: ResolutionMvRejectAction::RemainExiled,
            success_action: ResolutionCastSuccessAction::BottomMisses,
            delayed_trigger_receipts: Vec::new(),
            indicated_action: None,
        })
        .unwrap();
        cleanup
            .as_object_mut()
            .expect("struct serializes as object")
            .remove("face_policy");
        assert!(serde_json::from_value::<ResolutionCastCleanup>(cleanup).is_err());
    }
}

/// Which family a `DelayedTrigger` entry belongs to. A one-time boon is a
/// structurally distinct entry (holder-relative matching, "if you have a
/// boon" membership, the boon-only embedded-condition gate), not a flag on
/// an ordinary entry — hence a typed axis rather than a bool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DelayedTriggerKind {
    /// DEFAULT — an ordinary CR 603.7 delayed trigger. Every pre-existing
    /// entry is this variant.
    #[default]
    Ordinary,
    /// Digital-only Alchemy (no CR entry): a one-time boon installed by
    /// `Effect::CreateBoon`.
    Boon,
}

impl DelayedTriggerKind {
    /// Serde skip-helper: `Ordinary` is the default and is omitted from
    /// JSON, so every pre-existing serialized delayed trigger round-trips
    /// byte-identical.
    pub fn is_ordinary(&self) -> bool {
        matches!(self, DelayedTriggerKind::Ordinary)
    }

    /// True for a one-time boon entry.
    pub fn is_boon(self) -> bool {
        matches!(self, DelayedTriggerKind::Boon)
    }
}

/// CR 701.8a + CR 701.21a + CR 400.7: the slot naming the objects one
/// instruction can move from the battlefield to another zone — destroying
/// (CR 701.8a) or sacrificing (CR 701.21a) a permanent, returning it to its
/// owner's hand or library, or exiling it. `None` for every other instruction,
/// including known departures not classified here (`ChooseAndSacrificeRest`,
/// `Exploit`, `Meld`, `PutAtLibraryPosition`, `PutOnTopOrBottom`, and a permanent paid
/// as an unless cost or reached only through a nested definition such as
/// `ChooseOneOf` / `GenericEffect`): a documented under-approximation, so their
/// callers keep their default.
///
/// The slot is the instruction's printed object operand: a declared target,
/// a resolution-time choice (CR 608.2d), a mass population, or a self
/// reference; the caller decides which. Only the listed shapes are departures;
/// an instruction that removes objects some other way (a nested definition, a
/// replacement it installs) is not reported here.
///
/// Allow-list arms name every field, so a new field is a compile error until it
/// is classified. Every other variant is named in the `None` arm, so a new
/// variant is a compile error too.
pub(crate) fn battlefield_departure_slot(effect: &Effect) -> Option<&TargetFilter> {
    match effect {
        Effect::Destroy {
            target,
            cant_regenerate: _,
        }
        | Effect::DestroyAll {
            target,
            cant_regenerate: _,
        }
        | Effect::Sacrifice {
            target,
            count: _,
            min_count: _,
        }
        | Effect::Bounce {
            target,
            destination: _,
            selection: _,
        }
        | Effect::BounceAll {
            target,
            destination: _,
            count: _,
        } => Some(target),
        // CR 400.7: a zone change departs the battlefield only when it can move
        // an object out of it (an unrestricted or battlefield origin) and sends
        // it somewhere else.
        Effect::ChangeZone {
            origin,
            destination,
            target,
            owner_library: _,
            enter_transformed: _,
            enters_under: _,
            enter_tapped: _,
            enters_attacking: _,
            up_to: _,
            enter_with_counters: _,
            conditional_enter_with_counters: _,
            face_down_profile: _,
            enters_modified_if: _,
        }
        | Effect::ChangeZoneAll {
            origin,
            destination,
            target,
            enters_under: _,
            enter_tapped: _,
            enters_attacking: _,
            enter_with_counters: _,
            face_down_profile: _,
            library_position: _,
            library_shuffle: _,
            random_order: _,
        } => (*destination != Zone::Battlefield
            && matches!(origin, None | Some(Zone::Battlefield)))
        .then_some(target),
        Effect::StartYourEngines { .. }
        | Effect::ChangeSpeed { .. }
        | Effect::DealDamage { .. }
        | Effect::ApplyPostReplacementDamage { .. }
        | Effect::EachDealsDamageEqualToPower { .. }
        | Effect::EachSourceDealsDamage { .. }
        | Effect::Draw { .. }
        | Effect::Pump { .. }
        | Effect::PairWith { .. }
        | Effect::Regenerate { .. }
        | Effect::RemoveAllDamage { .. }
        | Effect::Counter { .. }
        | Effect::CounterAll { .. }
        | Effect::Token { .. }
        | Effect::GainLife { .. }
        | Effect::LoseLife { .. }
        | Effect::SetTapState { .. }
        | Effect::RemoveCounter { .. }
        | Effect::DiscardCard { .. }
        | Effect::Mill { .. }
        | Effect::Scry { .. }
        | Effect::PumpAll { .. }
        | Effect::DamageAll { .. }
        | Effect::DamageEachPlayer { .. }
        | Effect::Dig { .. }
        | Effect::GainControl { .. }
        | Effect::GainControlAll { .. }
        | Effect::ControlNextTurn { .. }
        | Effect::Attach { .. }
        | Effect::UnattachAll { .. }
        | Effect::Surveil { .. }
        | Effect::Fight { .. }
        | Effect::Explore
        | Effect::ExploreAll { .. }
        | Effect::Investigate
        | Effect::Tribute { .. }
        | Effect::TimeTravel
        | Effect::BecomeMonarch { .. }
        | Effect::NoOp
        | Effect::Proliferate
        | Effect::ProliferateTarget { .. }
        | Effect::Populate
        | Effect::Clash
        | Effect::Behold { .. }
        | Effect::EndTheTurn
        | Effect::EndCombatPhase
        | Effect::Vote { .. }
        | Effect::SeparateIntoPiles { .. }
        | Effect::ExileTopCardContest { .. }
        | Effect::SwitchPT { .. }
        | Effect::CopySpell { .. }
        | Effect::EpicCopy { .. }
        | Effect::CastCopyOfCard { .. }
        | Effect::CopyTokenOf { .. }
        | Effect::CreateTokenCopyFromPool { .. }
        | Effect::Myriad
        | Effect::Encore
        | Effect::CombineHost { .. }
        | Effect::ChooseAugmentAndCombineWithHost { .. }
        | Effect::Meld { .. }
        | Effect::ExileHaunting { .. }
        | Effect::HideawayConceal { .. }
        | Effect::CopyTokenBlockingAttacker { .. }
        | Effect::BecomeCopy { .. }
        | Effect::ChoosePermanent { .. }
        | Effect::GainActivatedAbilitiesOfTarget { .. }
        | Effect::ChooseCard { .. }
        | Effect::PutCounter { .. }
        | Effect::ChooseCounterKind { .. }
        | Effect::PutChosenCounter { .. }
        | Effect::PutCounterAll { .. }
        | Effect::MultiplyCounter { .. }
        | Effect::ChooseCounterAdjustment { .. }
        | Effect::DoublePT { .. }
        | Effect::DoublePTAll { .. }
        | Effect::MoveCounters { .. }
        | Effect::ReproduceEventCounters { .. }
        | Effect::Animate { .. }
        | Effect::ReturnAsAura { .. }
        | Effect::RegisterBending { .. }
        | Effect::GenericEffect { .. }
        | Effect::Cleanup { .. }
        | Effect::Mana { .. }
        | Effect::Discard { .. }
        | Effect::Shuffle { .. }
        | Effect::Transform { .. }
        | Effect::FlipPermanent { .. }
        | Effect::SearchLibrary { .. }
        | Effect::SearchOutsideGame { .. }
        | Effect::OpenBoosterPack { .. }
        | Effect::RevealHand { .. }
        | Effect::RevealFromHand { .. }
        | Effect::Reveal { .. }
        | Effect::RevealChosenNumbers { .. }
        | Effect::RevealTop { .. }
        | Effect::ExileTop { .. }
        | Effect::ExileFaceDownPile { .. }
        | Effect::TargetOnly { .. }
        | Effect::Choose { .. }
        | Effect::OpponentGuess { .. }
        | Effect::SwapChosenLabels { .. }
        | Effect::ChooseDamageSource { .. }
        | Effect::Suspect { .. }
        | Effect::Unsuspect { .. }
        | Effect::Connive { .. }
        | Effect::PhaseOut { .. }
        | Effect::PhaseIn { .. }
        | Effect::ForceBlock { .. }
        | Effect::ForceAttack { .. }
        | Effect::SolveCase
        | Effect::BecomePrepared { .. }
        | Effect::BecomeUnprepared { .. }
        | Effect::BecomeSaddled { .. }
        | Effect::SetClassLevel { .. }
        | Effect::CreateDelayedTrigger { .. }
        | Effect::AddTargetReplacement { .. }
        | Effect::AddRestriction { .. }
        | Effect::ReduceNextSpellCost { .. }
        | Effect::GrantNextSpellAbility { .. }
        | Effect::AddPendingETBCounters { .. }
        | Effect::AddPendingEntersModifications { .. }
        | Effect::CreateEmblem { .. }
        | Effect::CreateBoon { .. }
        | Effect::PayCost { .. }
        | Effect::CastFromZone { .. }
        | Effect::FreeCastFromZones { .. }
        | Effect::ExileResolvingSpellInsteadOfGraveyard { .. }
        | Effect::PreventDamage { .. }
        | Effect::CreateDamageReplacement { .. }
        | Effect::CreateDrawReplacement { .. }
        | Effect::CreatePlaneswalkReplacement { .. }
        | Effect::LoseTheGame { .. }
        | Effect::WinTheGame { .. }
        | Effect::RollDie { .. }
        | Effect::FlipCoins { .. }
        | Effect::RingTemptsYou
        | Effect::VentureIntoDungeon
        | Effect::VentureInto { .. }
        | Effect::TakeTheInitiative
        | Effect::ArrangePlanarDeckTop { .. }
        | Effect::Planeswalk
        // CR 701.33b: abandoning moves command-zone→scheme-deck, never a
        // battlefield departure.
        | Effect::AbandonScheme
        | Effect::ChaosEnsues
        | Effect::ReverseTurnOrder
        | Effect::RedistributeLifeTotals
        | Effect::OpenAttractions { .. }
        | Effect::RollToVisitAttractions
        | Effect::AssembleContraptions { .. }
        | Effect::AssembleContraptionsFromRollDifference
        | Effect::CrankContraptions { .. }
        | Effect::ReassembleContraption { .. }
        | Effect::AssembleContraptionOnSprocket { .. }
        | Effect::ReassembleContraptionOnSprocket { .. }
        | Effect::PutSticker { .. }
        | Effect::ApplySticker { .. }
        | Effect::ProcessRadCounters
        | Effect::GrantCastingPermission { .. }
        | Effect::ChooseFromZone { .. }
        | Effect::RememberCard { .. }
        | Effect::NoteManaSpent
        | Effect::NoteNumber { .. }
        | Effect::ForEachCategory { .. }
        | Effect::ChooseObjectsIntoTrackedSet { .. }
        | Effect::ChooseAndSacrificeRest { .. }
        | Effect::EachPlayerCopyChosen { .. }
        | Effect::Exploit { .. }
        | Effect::GainEnergy { .. }
        | Effect::GivePlayerCounter { .. }
        | Effect::LoseAllPlayerCounters { .. }
        | Effect::ExileFromTopUntil { .. }
        | Effect::RevealUntil { .. }
        | Effect::Discover { .. }
        | Effect::PhysicalContest { .. }
        | Effect::Heist { .. }
        | Effect::HeistExile
        | Effect::Cascade
        | Effect::Ripple { .. }
        | Effect::MiracleCast { .. }
        | Effect::MadnessCast { .. }
        | Effect::PutAtLibraryPosition { .. }
        | Effect::ChooseDrawnThisTurnPayOrTopdeck { .. }
        | Effect::PutOnTopOrBottom { .. }
        | Effect::GiftDelivery { .. }
        | Effect::Goad { .. }
        | Effect::GoadAll { .. }
        | Effect::Detain { .. }
        | Effect::SetRoomDoorLock { .. }
        | Effect::ExchangeControl { .. }
        | Effect::ChangeTargets { .. }
        | Effect::Manifest { .. }
        | Effect::ManifestDread
        | Effect::Cloak { .. }
        | Effect::TurnFaceUp { .. }
        | Effect::TurnFaceDown { .. }
        | Effect::ExtraTurn { .. }
        | Effect::GrantExtraLoyaltyActivations { .. }
        | Effect::SkipNextTurn { .. }
        | Effect::SkipNextStep { .. }
        | Effect::AdditionalPhase { .. }
        | Effect::Double { .. }
        | Effect::ModifyAnnouncedX { .. }
        | Effect::RuntimeHandled { .. }
        | Effect::Incubate { .. }
        | Effect::Amass { .. }
        | Effect::EmpowerJace { .. }
        | Effect::Monstrosity { .. }
        | Effect::Specialize
        | Effect::Renown { .. }
        | Effect::Bolster { .. }
        | Effect::Adapt { .. }
        | Effect::Learn
        | Effect::Forage
        | Effect::CompletePlayerAction { .. }
        | Effect::Harness
        | Effect::CollectEvidence { .. }
        | Effect::Endure { .. }
        | Effect::BlightEffect { .. }
        | Effect::Seek { .. }
        | Effect::SetLifeTotal { .. }
        | Effect::ExchangeLifeWithStat { .. }
        | Effect::ExchangeLifeTotals { .. }
        | Effect::SetDayNight { .. }
        | Effect::GiveControl { .. }
        | Effect::RemoveFromCombat { .. }
        | Effect::BecomeBlocked { .. }
        | Effect::Conjure { .. }
        | Effect::ApplyPerpetual { .. }
        | Effect::Intensify { .. }
        | Effect::DraftFromSpellbook { .. }
        | Effect::ChooseOneOf { .. }
        | Effect::Unimplemented { .. } => None,
    }
}

/// Mutable counterpart of [`mass_population_target`]. Kept adjacent so the two
/// variant lists are read and edited together.
pub(crate) fn mass_population_target_mut(effect: &mut Effect) -> Option<&mut TargetFilter> {
    match effect {
        Effect::ChangeZoneAll { target, .. }
        | Effect::DestroyAll { target, .. }
        | Effect::DamageAll { target, .. }
        | Effect::BounceAll { target, .. }
        | Effect::CounterAll { target, .. }
        | Effect::GainControlAll { target, .. }
        | Effect::PumpAll { target, .. }
        | Effect::PutCounterAll { target, .. }
        | Effect::DoublePTAll { target, .. } => Some(target),
        _ => None,
    }
}

/// CR 117.1 + CR 400.7j + CR 608.2k: Public characteristics of an object paid
/// as a cost for the resolving spell or ability. Effects can later refer to
/// that object even after the cost moved it to a public zone.
///
/// CR 400.7 + CR 608.2k: `incarnation` is the referent's
/// `GameObject::incarnation` at binding time. CR 608.2k keeps the reference
/// alive across *characteristic* changes, but an object that changes zones
/// becomes a new object (CR 400.7) that the reference must no longer name.
/// Because the engine reuses `ObjectId` as stable storage identity, the id
/// alone cannot distinguish "still the bound object" from "a new object at the
/// same id"; the incarnation epoch is what separates them. Consumers that act
/// on the *live* object (identity reads) must gate on
/// [`CostPaidObjectSnapshot::is_current`]. Consumers that read the frozen
/// `lki` are unaffected — LKI is the departed object's recorded state and is
/// incarnation-safe by construction (CR 608.2h).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostPaidObjectSnapshot {
    pub object_id: ObjectId,
    pub lki: LKISnapshot,
    /// CR 400.7: incarnation epoch captured at binding time. Legacy saves that
    /// predate this field deserialize to [`LEGACY_INCARNATION`], which can
    /// never equal a live object's incarnation, so such records are treated as
    /// stale (fail-closed) rather than silently naming a new object.
    #[serde(default = "legacy_incarnation")]
    pub incarnation: u64,
}

/// Serde default for [`CostPaidObjectSnapshot::incarnation`] and
/// `LinkedExileSnapshot::incarnation` on pre-migration saves. Mirrors
/// `ObjectIncarnationRefCompat`'s legacy arm
/// (`types/identifiers.rs`): a record written before the incarnation was
/// captured cannot prove which incarnation it bound, so it is pinned to a
/// sentinel that never matches a live object.
pub(crate) fn legacy_incarnation() -> u64 {
    crate::types::identifiers::LEGACY_INCARNATION
}

impl CostPaidObjectSnapshot {
    /// CR 400.7j + CR 608.2k: Capture the cost-paid referent from a
    /// live object, pinning its current incarnation. The single construction
    /// seam for production binding sites, so no seam can forget the epoch.
    pub fn capture(object: &crate::game::game_object::GameObject, lki: LKISnapshot) -> Self {
        Self {
            object_id: object.id,
            lki,
            incarnation: object.incarnation,
        }
    }

    /// CR 608.2c + CR 608.2h + CR 400.7: Bind a referent whose object has
    /// already left the zone the parent instruction moved it from, so its
    /// characteristics come from last known information rather than a live
    /// read. The object row survives the move with its incarnation already
    /// bumped (`game/zones.rs`), so `state_incarnation` is that post-move
    /// epoch: the reference names the object as the parent instruction left
    /// it, and goes stale if it moves again.
    ///
    /// Returns a snapshot pinned to [`LEGACY_INCARNATION`] when the row is
    /// gone entirely (ceased to exist), which no live object can match.
    pub fn capture_departed(
        state: &crate::types::game_state::GameState,
        object_id: ObjectId,
        lki: LKISnapshot,
    ) -> Self {
        let incarnation = state
            .objects
            .get(&object_id)
            .map_or(crate::types::identifiers::LEGACY_INCARNATION, |object| {
                object.incarnation
            });
        Self {
            object_id,
            lki,
            incarnation,
        }
    }

    /// CR 400.7: True when this snapshot still names the live object it was
    /// bound to. False when the referent left and returned (a new object at the
    /// same storage id), when it is gone entirely, or when the record is a
    /// pre-migration save with no captured incarnation.
    ///
    /// This is a strict full-pair compare and is deliberately NOT
    /// `ResolvedAbility::target_pin_is_current`, which fails *open* when no pin
    /// is recorded — a `CostPaidObject` referent never has an ordinary target
    /// pin, so that check would pass for free.
    pub fn is_current(&self, state: &crate::types::game_state::GameState) -> bool {
        crate::types::identifiers::ObjectIncarnationRef::of(self.object_id, self.incarnation)
            .is_current(state)
    }

    /// CR 400.7 + CR 608.2k: Re-pin this snapshot to the referent's CURRENT
    /// incarnation, keeping the characteristics captured at binding time.
    ///
    /// Cost-payment seams necessarily capture the referent BEFORE the cost moves
    /// it (the `lki` must record its pre-move characteristics — CR 608.2h), but
    /// that move is the cost's own and must NOT invalidate the reference: CR
    /// 608.2k exists so an effect can still refer to the object its cost moved.
    /// Re-pinning once the cost's moves are complete makes the pin mean "this
    /// object, as the cost left it", so only a LATER zone change reads as stale.
    ///
    /// Deliberately re-reads the live incarnation rather than assuming a fixed
    /// increment: a cost move may be redirected by a replacement effect or pass
    /// through more than one zone, so the delta is not reliably one.
    ///
    /// Leaves the pin untouched when the object no longer exists — the recorded
    /// epoch then still cannot match any live object, which is the correct
    /// fail-closed reading.
    pub fn repin_to_current_incarnation(&mut self, state: &crate::types::game_state::GameState) {
        if let Some(object) = state.objects.get(&self.object_id) {
            self.incarnation = object.incarnation;
        }
    }

    /// CR 608.2h: Build a FRESH capture of `object_id` from the LIVE object
    /// row, or `None` when that row is gone (nothing to read).
    ///
    /// The single construction seam for RE-capture, mirroring what
    /// [`Self::capture`] is for first capture. Both the in-place form
    /// ([`Self::refresh_capture_from_live`], used by the singular and plural
    /// ability-side carriers) and the stack object's `cast_cost_paid_object`
    /// carrier — which lives on `GameObject`, not on `ResolvedAbility`, and so
    /// cannot be reached by the ability traversal — go through here, so
    /// "what a re-capture reads" has exactly one implementation.
    pub fn recapture_from_live(
        state: &crate::types::game_state::GameState,
        object_id: ObjectId,
    ) -> Option<Self> {
        state
            .objects
            .get(&object_id)
            .map(|object| Self::capture(object, object.snapshot_for_mana_spent()))
    }

    /// CR 608.2h: RE-CAPTURE this snapshot's characteristics from the LIVE
    /// object in place, preserving its identity (same `object_id`).
    ///
    /// For a payment whose object moves are DEFERRED past the seam that
    /// published this snapshot: CR 608.2h calls for the object's last known
    /// information, i.e. its state as it MOST RECENTLY existed, not its state
    /// at selection time. CR 601.2g puts the mana-ability window between those
    /// two moments, so a deferred sacrifice selected before that window can be
    /// tapped for mana inside it and a snapshot frozen at selection would
    /// report an untapped permanent the game had already tapped.
    ///
    /// A no-op for a vanished row: the recorded characteristics are then
    /// already the last thing the game knew.
    pub fn refresh_capture_from_live(&mut self, state: &crate::types::game_state::GameState) {
        if let Some(refreshed) = Self::recapture_from_live(state, self.object_id) {
            *self = refreshed;
        }
    }

    /// CR 400.7 + CR 608.2k: The single authority for resolving this snapshot to
    /// a LIVE object id. Yields the id only while the snapshot still names the
    /// object it was bound to; a referent that left (with or without returning
    /// under the same storage id) yields `None`, and there is deliberately NO
    /// fallback to a same-id object.
    ///
    /// Every live-object consumer of a `CostPaidObject` / `AmassedArmy`
    /// referent must resolve through here rather than reading `object_id`
    /// directly, so the identity rule has one implementation instead of one per
    /// call site.
    ///
    /// This is NOT for readers of the frozen `lki`: CR 608.2h requires those to
    /// keep reporting the departed object's recorded characteristics, so they
    /// read `self.lki` and never call this.
    pub fn live_object_id(&self, state: &crate::types::game_state::GameState) -> Option<ObjectId> {
        self.is_current(state).then_some(self.object_id)
    }
}

/// CR 400.7 + CR 601.2c + CR 602.2b: One entry of the plural cost-paid
/// provenance authority (`ResolvedAbility::cost_paid_objects`).
///
/// The authority answers two questions whose CORRECT failure directions are
/// opposite:
///
/// * MEMBERSHIP — "was this object consumed by this ability's own cost?" Read by
///   `exclude_cost_paid_object_that_left_battlefield`. Failing OPEN here is a
///   rules violation: this engine pays a non-self Sacrifice/Discard/Exile cost
///   BEFORE the same ability's targets are chosen (a documented shortcut past
///   CR 601.2c / CR 602.2b's real target-before-cost order), so a paid object
///   that leaks back into the ability's own target pool was never legally
///   targetable. Membership is incarnation-IRRELEVANT: an object this cost
///   moved stays an object this cost moved however many times it moves again.
/// * LIVE PROVENANCE — "which live object, with which recorded characteristics,
///   did this cost bind?" Read by [`Self::live_object_id`] and every `lki`
///   consumer. Failing CLOSED is mandatory: `ObjectId` is reusable storage
///   identity, so an id alone cannot distinguish "still the bound object" from
///   "a new object at the same id" (CR 400.7).
///
/// A pre-migration persisted payload carried object IDS only. That is exact
/// membership and NO provenance whatsoever, which is precisely why it gets its
/// own variant instead of being dropped (fails membership open) or rebound into
/// a synthesized snapshot (fails provenance open, and would additionally have to
/// invent an [`LKISnapshot`] that CR 608.2h readers would then report as if it
/// were the departed object's real recorded state).
///
/// CR 701.9c: a LIVE payment reaches the same shape when a DISCARD cost's card
/// is redirected into a hidden zone without being revealed — the cost
/// really did move that card (membership is exact), but its characteristics are
/// undefined and CR 400.7j licenses finding only an object a cost moved to a
/// PUBLIC zone. Two code paths produce that classification: the random-discard
/// seam (`commit_random_discard_cost_picks`) classifies per pick at publication
/// time, and `ResolvedAbility::settle_cost_paid_provenance_recursive` demotes a
/// provisionally-`Captured` record whose object did not reach a public zone,
/// but ONLY when the settling seam passes
/// [`CostMoveOutcome::Discard`] — which is what covers the deterministic
/// discard paths, whose moves run through the replacement pipeline AFTER
/// publication. So `MembershipOnly` has three producers, the persisted
/// migration plus those two; it never means "this object was not paid".
///
/// CR 608.2h: demotion is deliberately NOT the general hidden-destination
/// answer, because it is LOSSY — it throws away the pre-move `lki` that CR
/// 608.2h names as the correct reading for an object no longer in the public
/// zone it was expected to be in ("Sacrifice a creature: … equal to its
/// power"). CR 701.9c, the rule that makes the characteristics *undefined*
/// rather than last-known, is DISCARD-scoped by its own text. A sacrifice,
/// exile, or return-to-hand cost move into a hidden zone therefore keeps its
/// `Captured` record with the pre-move `lki` intact and its pin left stale, so
/// [`Self::live_object_id`] still fails closed per CR 400.7j.
///
/// `MembershipOnly` is deliberately NOT boxed away behind the captured variant:
/// the enum's whole job is that a migrated record carries NOTHING but an id, and
/// the vector it lives in holds at most a handful of entries per resolving
/// ability (the objects one cost consumed). Keeping the snapshot inline also
/// keeps `Captured`'s wire form byte-identical to the bare snapshot it replaced.
/// Mirrors the existing `#[allow]`s on `Effect` / `WaitingFor` in this crate.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CostPaidObjectRecord {
    /// Captured at payment time by [`CostPaidObjectSnapshot::capture`]: public
    /// characteristics (CR 608.2h) plus the incarnation epoch (CR 400.7).
    Captured(CostPaidObjectSnapshot),
    /// A pre-migration payload carried object ids only. Membership is exact —
    /// the CR 601.2c exclusion needs nothing more — but this can NEVER be a
    /// live referent, because no incarnation was ever recorded. Structurally
    /// incapable of resolving live: it holds no snapshot to resolve through.
    ///
    /// CR 701.9c + CR 400.7j: also written by a live payment whose own move
    /// delivered the object into a HIDDEN zone (a discard cost — random or
    /// deterministic — redirected to a library or hand without being
    /// revealed). There the refusal is a rules requirement rather than a
    /// missing record: the characteristics are undefined, and only a move to a
    /// public zone lets the cost's own effects find the object.
    ///
    /// CR 608.2h: a NON-discard cost move (sacrifice, exile, return to hand)
    /// that ends in a hidden zone is NOT written here. CR 701.9c does not reach
    /// it, its last known information is exactly what the ability's own effects
    /// must read, and demoting would destroy that `lki`.
    MembershipOnly(ObjectId),
}

/// CR 701.9c + CR 608.2h: WHICH cost action produced the moves a settlement
/// pass is settling. The axis decides one thing and only one: whether a
/// `Captured` record whose object ended in a HIDDEN zone loses its captured
/// characteristics.
///
/// Typed rather than a bool: the two arms are different RULES, not two states
/// of one switch (`Discard` is licensed by CR 701.9c's discard-scoped
/// undefined-characteristics rule; `Relocation` is governed by CR 608.2h's
/// last-known-information rule), and a `settle(.., true)` at a call site would
/// say nothing about which rule the seam is claiming.
///
/// This axis is expected to GROW (a future cost action whose hidden-destination
/// rule is neither CR 701.9c's nor CR 608.2h's gets its own variant), so every
/// dispatch on it is an EXHAUSTIVE `match` with no wildcard arm — see
/// [`CostPaidObjectRecord::settle_after_cost_move`], the only such dispatch.
/// A `_ =>` fallback would silently hand each new variant whichever arm it
/// happened to share, and the demoting arm is LOSSY.
///
/// Why the settlement traversal is SCOPED to the ids its seam moved rather than
/// settling every record: one ability can settle twice with DIFFERENT outcomes.
/// `finalize_mana_payment_with_resume` settles a deferred spell sacrifice as
/// `Relocation`, then eleven lines later `pay_deferred_hand_discard_cost`
/// settles the same ability's random discard leg as `Discard`
/// (`finalize_mana_payment_with_phyrexian_choices` carries the same pair). An
/// unscoped second pass would apply CR 701.9c to the sacrifice record — a rule
/// discard-scoped by its own text that never reached it — and destroy the LKI
/// CR 608.2h preserves. That seam is live in code today and cold only because
/// no printed card pairs a deferred sacrifice with a random discard cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostMoveOutcome {
    /// CR 701.9a / CR 701.9b: the cost DISCARDED these objects, so CR 701.9c's
    /// undefined-characteristics rule applies if one of them landed in an
    /// unrevealed hidden zone.
    Discard,
    /// Any other cost move — sacrifice (CR 701.21a), exile (CR 701.13a),
    /// return to hand. CR 701.9c does not reach these; CR 608.2h keeps the
    /// pre-move LKI the correct answer for a hidden destination.
    Relocation,
}

impl CostPaidObjectRecord {
    /// CR 601.2c: The MEMBERSHIP projection — the storage id of the object this
    /// cost consumed. Exact for both variants and the only thing the
    /// target-candidate exclusion needs.
    pub fn object_id(&self) -> ObjectId {
        match self {
            Self::Captured(snapshot) => snapshot.object_id,
            Self::MembershipOnly(object_id) => *object_id,
        }
    }

    /// CR 608.2h: The captured characteristics/incarnation, when this record has
    /// any. `None` for a migrated id-only record — its LKI was never recorded
    /// and is deliberately NOT synthesized, since a fabricated `lki` would make
    /// CR 608.2h readers report characteristics the game never observed.
    pub fn snapshot(&self) -> Option<&CostPaidObjectSnapshot> {
        match self {
            Self::Captured(snapshot) => Some(snapshot),
            Self::MembershipOnly(_) => None,
        }
    }

    /// Mutable twin of [`Self::snapshot`]. The single seam through which the
    /// settlement traversal reaches a captured record, so `MembershipOnly`
    /// cannot be upgraded into a live referent by any caller.
    pub fn snapshot_mut(&mut self) -> Option<&mut CostPaidObjectSnapshot> {
        match self {
            Self::Captured(snapshot) => Some(snapshot),
            Self::MembershipOnly(_) => None,
        }
    }

    /// CR 400.7: True only when a CAPTURED record still names the live object it
    /// was bound to. Always false for `MembershipOnly`: with no recorded epoch
    /// there is nothing that could prove currency, and guessing would rebind a
    /// reused storage id to a different object.
    pub fn is_current(&self, state: &crate::types::game_state::GameState) -> bool {
        self.snapshot()
            .is_some_and(|snapshot| snapshot.is_current(state))
    }

    /// CR 400.7 + CR 400.7j: The single authority for resolving this record to a
    /// LIVE object id. Always `None` for `MembershipOnly` — a membership record
    /// is never a live referent.
    pub fn live_object_id(&self, state: &crate::types::game_state::GameState) -> Option<ObjectId> {
        self.snapshot()
            .and_then(|snapshot| snapshot.live_object_id(state))
    }

    /// CR 400.7 + CR 608.2k: Re-pin a captured record to the incarnation the
    /// cost's OWN move produced. A no-op for `MembershipOnly`, which has no pin
    /// to refresh and must not acquire one.
    pub fn repin_to_current_incarnation(&mut self, state: &crate::types::game_state::GameState) {
        if let Some(snapshot) = self.snapshot_mut() {
            snapshot.repin_to_current_incarnation(state);
        }
    }

    /// CR 400.7j + CR 701.9c + CR 608.2h: SETTLE this record against where THIS
    /// seam's own move ACTUALLY delivered the object, once those moves are
    /// complete. The caller has already established that this record's object
    /// is one the seam moved (see
    /// [`ResolvedAbility::settle_cost_paid_provenance_recursive`], which owns
    /// the scoping) and which cost action performed the move (`outcome`).
    ///
    /// Three outcomes, and which one applies is a per-object fact that cannot
    /// be known at publication time — capture must stay PRE-move so the `lki`
    /// records pre-move characteristics (CR 608.2h), while the destination is
    /// only decided afterwards by the replacement pipeline (CR 616.1):
    ///
    /// * delivered to a PUBLIC zone (either outcome) → re-pin. CR 400.7j is the
    ///   rule that keeps the reference alive across the cost's own move.
    /// * NOT public, [`CostMoveOutcome::Discard`] → DEMOTE to
    ///   [`Self::MembershipOnly`]. CR 701.9c leaves a discarded card put into an
    ///   unrevealed hidden zone with ALL values of its characteristics
    ///   undefined, so the captured snapshot may not be exposed at all; the
    ///   demotion preserves the exact CR 601.2c / CR 602.2b membership the
    ///   target-candidate exclusion reads, while withholding the snapshot and
    ///   any live reference.
    /// * NOT public, [`CostMoveOutcome::Relocation`] → LEAVE UNTOUCHED. CR
    ///   701.9c is discard-scoped and does not reach a sacrifice (CR 701.21a),
    ///   an exile (CR 701.13a), or a return to hand, so those characteristics
    ///   are not undefined — they are last known information, which CR 608.2h
    ///   makes the CORRECT answer for an object that is no longer in the public
    ///   zone it was expected to be in ("Sacrifice a creature: … equal to its
    ///   power"). Demoting here would destroy exactly the `lki` the ability's
    ///   own effects must read. Not re-pinning is equally deliberate: the pin
    ///   stays on the pre-move incarnation, so [`Self::live_object_id`] and
    ///   [`Self::is_current`] fail CLOSED (CR 400.7j licenses finding only a
    ///   PUBLIC-zone delivery) while the captured characteristics survive.
    ///
    /// A missing object row reads as NOT public, fail-closed: the destination
    /// cannot be proven public. That arm is deliberately NOT the
    /// sacrificed-token case (CR 704.5d) — a token's row is purged by the
    /// state-based-action sweep, which runs long after this settlement, so a
    /// token sacrificed to a cost still settles through the public-graveyard
    /// arm with its provenance intact.
    ///
    /// A `MembershipOnly` record is already settled and is never upgraded.
    pub fn settle_after_cost_move(
        &mut self,
        state: &crate::types::game_state::GameState,
        outcome: CostMoveOutcome,
    ) {
        let Some(object_id) = self.snapshot().map(|snapshot| snapshot.object_id) else {
            return;
        };
        let delivered_to_public_zone = state
            .objects
            .get(&object_id)
            .is_some_and(|object| object.zone.is_public());
        if delivered_to_public_zone {
            self.repin_to_current_incarnation(state);
            return;
        }
        // The destination is hidden. WHICH rule applies is decided entirely by
        // which cost action performed the move, so this dispatch is an
        // EXHAUSTIVE match with no wildcard arm: a future `CostMoveOutcome`
        // variant must state its own hidden-destination rule here and be
        // rejected by the compiler until it does. A `_ =>` fallback would hand
        // every new cost action the LOSSY demotion arm by default, which is
        // wrong for every non-discard move (it destroys the CR 608.2h `lki`).
        match outcome {
            // CR 701.9c: a DISCARDED card put into an unrevealed hidden zone has
            // all values of its characteristics undefined, so the snapshot may
            // not be exposed at all; membership (CR 601.2c) is all that
            // survives.
            CostMoveOutcome::Discard => *self = Self::MembershipOnly(object_id),
            // CR 608.2h: a non-discard cost move into a hidden zone leaves the
            // captured LAST KNOWN INFORMATION as the correct answer, so the
            // record is left exactly as it is — snapshot kept, pin deliberately
            // NOT refreshed, so the live reference stays refused (CR 400.7j).
            CostMoveOutcome::Relocation => {}
        }
    }

    /// CR 608.2h: RE-CAPTURE a captured record's characteristics from the LIVE
    /// object, preserving the record's identity (same `object_id`).
    ///
    /// For a payment whose object moves are DEFERRED past the seam that
    /// published this record: CR 608.2h calls for the object's last known
    /// information, i.e. its state as it most recently existed, not its state
    /// at selection time. A deferred sacrifice selected before the mana window
    /// can be tapped for mana inside that window, so the record published at
    /// selection would otherwise freeze an untapped `lki` that the game never
    /// observed at the moment of payment.
    ///
    /// Delegates the actual read to
    /// [`CostPaidObjectSnapshot::refresh_capture_from_live`], which the
    /// SINGULAR `cost_paid_object` carrier uses too, so both carriers re-capture
    /// identically by construction.
    ///
    /// A no-op for `MembershipOnly` (nothing to refresh, and it must never
    /// acquire a snapshot) and for a vanished row (no live object to read).
    pub fn refresh_capture_from_live(&mut self, state: &crate::types::game_state::GameState) {
        let Self::Captured(snapshot) = self else {
            return;
        };
        snapshot.refresh_capture_from_live(state);
    }
}

/// `Captured` writes EXACTLY the bytes a bare [`CostPaidObjectSnapshot`] writes,
/// so migrating the field's element type changes no current save, card-data
/// export, or P2P payload. `MembershipOnly` writes the bare object id, i.e. the
/// pre-migration `cost_paid_object_ids` element shape, so a migrated record
/// round-trips through the same wire form it was read from.
impl Serialize for CostPaidObjectRecord {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Captured(snapshot) => snapshot.serialize(serializer),
            Self::MembershipOnly(object_id) => object_id.serialize(serializer),
        }
    }
}

/// Shape-directed decode. The two forms are reliably distinguishable:
/// `CostPaidObjectSnapshot` always writes a JSON OBJECT (`object_id`/`lki` are
/// never skipped), while `ObjectId` is `#[serde(transparent)]` over `u64` and
/// therefore always writes a JSON NUMBER. Anything else is a decode error
/// rather than a silently-dropped (membership-open) entry.
impl<'de> Deserialize<'de> for CostPaidObjectRecord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        if value.is_object() {
            return serde_json::from_value::<CostPaidObjectSnapshot>(value)
                .map(Self::Captured)
                .map_err(de::Error::custom);
        }
        serde_json::from_value::<ObjectId>(value)
            .map(Self::MembershipOnly)
            .map_err(de::Error::custom)
    }
}

/// CR 601.2h + CR 602.2b: Compare two plural cost-paid provenance authorities
/// by their object-id sequence only, in payment order, without allocating.
///
/// `ResolvedAbility::cost_paid_objects` replaced a raw `Vec<ObjectId>`, whose
/// equality compared exactly these ids. A full `CostPaidObjectSnapshot`
/// comparison would additionally fold in `lki` and `incarnation`, which would
/// silently narrow every identity check that reaches `ResolvedAbility`
/// equality — stack copy/batch identity most of all. This helper is the single
/// authority that preserves the pre-existing id-sequence semantics, and both
/// `ResolvedAbility`'s manual `PartialEq` and `stack.rs`'s inert-trigger run
/// identity route through it.
///
/// Note the deliberate asymmetry with the SINGULAR `cost_paid_object`, which
/// keeps full-snapshot equality: that field is a resolution-time referent whose
/// incarnation is part of its meaning, and its equality behavior is unchanged.
///
/// CR 400.7: comparing [`CostPaidObjectRecord::object_id`] also makes a migrated
/// `MembershipOnly` record compare equal to the `Captured` record naming the
/// same object — correct here, because this comparison IS the membership axis.
pub(crate) fn cost_paid_object_snapshot_ids_eq(
    a: &[CostPaidObjectRecord],
    b: &[CostPaidObjectRecord],
) -> bool {
    a.iter()
        .map(CostPaidObjectRecord::object_id)
        .eq(b.iter().map(CostPaidObjectRecord::object_id))
}

/// CR 106.1b + CR 400.7 + CR 602.2b (issue #6504): The mana type(s) spent to
/// pay one activated ability's own mana sub-cost, snapshotted onto
/// `ResolvedAbility::noted_mana_payment` at the moment that specific
/// activation reached the stack. `source_incarnation` is the source's
/// `GameObject::incarnation` at that same moment — a companion "note the
/// type of mana spent to pay this activation cost" effect (Jeweled Amulet)
/// must refuse to act if the object's live incarnation no longer matches
/// (CR 400.7: bounced/flickered since).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotedManaPayment {
    pub types: Vec<ManaType>,
    pub source_incarnation: u64,
}

pub fn is_x_remove_counter_cost_count(count: u32) -> bool {
    count == REMOVE_COUNTER_COST_X
}

pub fn is_variable_remove_counter_cost_count(count: u32) -> bool {
    matches!(
        count,
        REMOVE_COUNTER_COST_X | REMOVE_COUNTER_COST_ANY_NUMBER | REMOVE_COUNTER_COST_ALL
    )
}

/// CR 601.2b/f + CR 113.2c: One announced instance of an additional cost on a
/// spell. The queue of these records models multiple independent keyword
/// instances; `AdditionalCost::Optional { repeatability: Repeatable }` models
/// the within-instance "any number of times" axis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdditionalCostInstance {
    #[serde(default, skip_serializing_if = "AdditionalCostOrigin::is_other")]
    pub origin: AdditionalCostOrigin,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub origin_ordinal: u32,
    pub cost: AdditionalCost,
    /// CR 611.3b: the permanent whose static ability grants this instance
    /// (Silverquill, the Disputant). `None` for a printed instance and for a
    /// grant no permanent generates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_source: Option<ObjectId>,
}

impl AdditionalCostInstance {
    pub fn new(origin: AdditionalCostOrigin, cost: AdditionalCost) -> Self {
        Self {
            origin,
            origin_ordinal: 0,
            cost,
            grant_source: None,
        }
    }

    pub fn with_grant_source(mut self, grant_source: Option<ObjectId>) -> Self {
        self.grant_source = grant_source;
        self
    }

    pub fn new_with_ordinal(
        origin: AdditionalCostOrigin,
        origin_ordinal: u32,
        cost: AdditionalCost,
    ) -> Self {
        Self {
            origin,
            origin_ordinal,
            cost,
            grant_source: None,
        }
    }
}

/// CR 113.2c: Payment record for one independently functioning non-kicker
/// additional-cost keyword instance. Repeatable instances record their payment
/// count; non-repeatable instances record `count == 1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdditionalCostInstancePayment {
    #[serde(default, skip_serializing_if = "AdditionalCostOrigin::is_other")]
    pub origin: AdditionalCostOrigin,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub origin_ordinal: u32,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub count: u32,
    /// CR 702.153b: the `grant_source` of the instance this payment was made
    /// for, so a trigger reads the payment of its own instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_source: Option<ObjectId>,
}

impl AdditionalCostInstancePayment {
    pub fn new(origin: AdditionalCostOrigin, count: u32) -> Self {
        Self {
            origin,
            origin_ordinal: 0,
            count,
            grant_source: None,
        }
    }

    pub fn new_with_ordinal(origin: AdditionalCostOrigin, origin_ordinal: u32, count: u32) -> Self {
        Self {
            origin,
            origin_ordinal,
            count,
            grant_source: None,
        }
    }
}

pub(crate) fn additional_cost_instance_payment_count(
    payments: &[AdditionalCostInstancePayment],
    origin: AdditionalCostOrigin,
) -> u32 {
    payments
        .iter()
        .filter(|payment| payment.origin == origin)
        .map(|payment| payment.count)
        .sum()
}

pub(crate) fn additional_cost_instance_payment_count_for_ordinal(
    payments: &[AdditionalCostInstancePayment],
    origin: AdditionalCostOrigin,
    origin_ordinal: u32,
) -> u32 {
    payments
        .iter()
        .filter(|payment| payment.origin == origin && payment.origin_ordinal == origin_ordinal)
        .map(|payment| payment.count)
        .sum()
}

/// Boxed variant of `deserialize_ability_cost_compat` for use on fields
/// declared as `Box<AbilityCost>` (e.g.,
/// `ManaAbilityResume::UnlessPayment.cost` is boxed to keep the enclosing
/// enum compact). Same backward-compat behavior as the unboxed variant.
pub fn deserialize_ability_cost_compat_boxed<'de, D>(d: D) -> Result<Box<AbilityCost>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_ability_cost_compat(d).map(Box::new)
}

/// CR 401.4 + CR 701.23a + CR 608.2c: Presentation metadata for a library-search
/// choice. The engine supplies this because the continuation determines whether
/// the owner may arrange the selected cards in one library position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SearchOrderingHint {
    #[default]
    Unordered,
    OrderedToLibraryTop,
}

/// CR 120.1 + CR 608.2b: How a `DamageSource::Target` clause's SUBJECT slot
/// resolved during chain descent.
///
/// The subject ("Target creature you control deals damage equal to its power
/// to any target") is declared by a PARENT node of the chain, while the damage
/// itself lives on the child. The runtime contract the damage resolver reads is
/// positional — `targets = [subject, recipient…]` — and it is reconstructed at
/// resolution by prepending the parent's chosen object onto the child's list.
///
/// CR 608.2b prunes an illegal target out of the parent's list before any
/// effect runs, which destroys the only evidence that a subject slot was ever
/// declared: the child would then read its own recipient out of `targets[0]`
/// and deal that recipient's power to itself. This binding preserves the
/// distinction the pruned list can no longer express.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetDamageSourceBinding {
    /// The parent's chosen object was prepended, so the subject is `targets[0]`.
    /// The id itself is deliberately NOT duplicated here: the target list stays
    /// the single source of truth for WHICH object it is, and this binding
    /// answers only WHETHER the positional contract holds. (It also keeps
    /// `SpellContext` — and through it `ResolutionFrame` — from growing.)
    Bound,
    /// CR 608.2b: the parent declared a subject slot whose chosen object was an
    /// illegal target at resolution. "If part of the effect requires
    /// information about an illegal target, it fails to determine any such
    /// information. Any part of the effect that requires that information won't
    /// happen." — the damage clause needs both the subject's identity (CR 120.1:
    /// an object that deals damage is the source of that damage) and its power,
    /// so it deals no damage. The rest of the chain still resolves.
    Illegal,
}

/// CR 608.2d vs CR 401.4: WHICH of a Telling Time-class remainder split's two
/// decisions a given `WaitingFor::DigRestSplitChoice` prompt still carries, and
/// therefore WHOSE decision it is.
///
/// A split bundles two rules-distinct choices with two distinct owners:
///
/// * the PARTITION — which cards take the library top and which take the
///   bottom. CR 608.2d makes this the choice of the player the effect gives it
///   to ("*you* ... put one on top of your library"), i.e. the dig's chooser.
/// * the ARRANGEMENT — the order of the cards landing in each position.
///   CR 401.4: "If an effect puts two or more cards in a specific position in a
///   library at the same time, **the owner of those cards** may arrange them in
///   any order." That is the LIBRARY'S OWNER, who need not be the chooser.
///
/// For every printed card today the two players coincide (Telling Time digs
/// "your library"), so the common case answers both in one prompt. Splitting
/// the vocabulary keeps a cross-player dig ("look at the top three cards of
/// target player's library ...") from silently handing CR 401.4's arrangement
/// to the chooser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DigRestSplitScope {
    /// The acting player is BOTH the chooser and the library's owner, so this
    /// one prompt answers the CR 608.2d partition and the CR 401.4 arrangement
    /// together. The default, and the only scope a same-player dig ever parks.
    #[default]
    PartitionAndOrder,
    /// The acting player is the chooser but NOT the library's owner. Only the
    /// CR 608.2d partition — the SET of cards in the leading `top_count`
    /// entries — is binding here; the within-pile order they submit is
    /// discarded and re-asked of the owner as an [`Self::OrderOnly`] prompt
    /// whenever a resulting pile holds two or more cards.
    PartitionOnly,
    /// The acting player is the library's OWNER and the partition is already
    /// settled. CR 401.4 is all that is left, so the submission must be a
    /// permutation that keeps the same cards in the leading `top_count`
    /// entries — the owner may reorder within each pile but may not move a
    /// card across the top/bottom boundary, which was never their decision.
    OrderOnly,
}

impl DigRestSplitScope {
    /// True when the submission's leading `top_count` entries are free to be
    /// any subset (the partition is still open), false when they are pinned to
    /// the already-settled top pile.
    pub fn partition_is_open(self) -> bool {
        matches!(
            self,
            DigRestSplitScope::PartitionAndOrder | DigRestSplitScope::PartitionOnly
        )
    }
}

/// The captured outcome of one discard instruction, retained only for its
/// direct sub-ability. `lki` is taken while the card is still in hand, so a
/// replacement redirect or the new object in its destination zone cannot
/// change the contingent condition's answer (CR 400.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscardedCardResult {
    pub object_id: ObjectId,
    pub lki: crate::types::game_state::LKISnapshot,
    pub final_zone: Zone,
}

/// CR 608.2c: Objects produced by a `forward_result` instruction, retained for
/// the remainder of that resolution chain. This is distinct from declared
/// targets: a producer may move zero objects (`Some(vec![])`), and that result
/// must not fall back to an earlier target selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardedResultContext {
    pub targets: Vec<TargetRef>,
    /// CR 400.7: Exact identities of object results at the moment the producer
    /// completed. A later continuation must not bind a new incarnation that
    /// reused the same storage `ObjectId`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub object_incarnations: Vec<ObjectIncarnationRef>,
}

impl ForwardedResultContext {
    /// Capture the complete ordered object result of a forwarding instruction.
    pub fn from_object_ids(
        state: &crate::types::game_state::GameState,
        object_ids: &[ObjectId],
    ) -> Self {
        Self {
            targets: object_ids.iter().copied().map(TargetRef::Object).collect(),
            object_incarnations: object_ids
                .iter()
                .filter_map(|id| state.objects.get(id).map(ObjectIncarnationRef::from_object))
                .collect(),
        }
    }

    /// CR 400.7: A forwarded object referent remains valid only while its
    /// producer-time incarnation is still current. Unpinned legacy payloads
    /// remain readable for wire compatibility.
    pub fn object_pin_is_current(
        &self,
        id: ObjectId,
        state: &crate::types::game_state::GameState,
    ) -> bool {
        self.object_incarnations
            .iter()
            .find(|pin| pin.object_id == id)
            .is_none_or(|pin| pin.is_current(state))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingForwardedZoneResult {
    pub producer: ObjectId,
    pub selected: Option<Vec<ObjectIncarnationRef>>,
    pub group: Option<crate::types::identifiers::LogicalZoneChangeGroupId>,
}

/// Casting-time facts that flow with a spell from casting through resolution.
/// Conditions in the sub_ability chain are evaluated against this context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SpellContext {
    /// Typed SearchLibrary intent for a face-down Exile destination.
    /// This is carried in the existing resolution context so it survives
    /// ordinary ability-chain handoffs without widening every ability literal.
    #[serde(default, skip_serializing_if = "ExileConcealment::is_public")]
    pub face_down_in_exile: ExileConcealment,
    /// CR 201.5a: the granter stamped on the definition this ability was built from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub granting_object: Option<ObjectIncarnationRef>,
    /// CR 603.7 + CR 603.10a + CR 608.2h: The battlefield-departure event a
    /// phase-delayed triggered ability was created under ("When this creature
    /// dies, at the beginning of the next end step, …"). The later phase event
    /// that fires the ability names no object, so object look-back reads ("that
    /// many", "its power", "this creature's counters") resolve against this
    /// departure's last-known information instead. Stamped only by
    /// `delayed_trigger::resolve`; `None` everywhere else, including every
    /// event-delayed trigger, which reads the event that fires it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creation_lookback_event: Option<Box<crate::types::events::GameEvent>>,
    /// CR 603.7a + CR 608.2c: the player each declared group of the creating chain named, still
    /// legal at creation, read through `targeting::carried_declared_player`; stamped only by
    /// `delayed_trigger::resolve`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outer_declared_players: Vec<(ChosenGroupId, PlayerId)>,
    /// Digital-only Alchemy (no CR entry): the noted number this boon
    /// grant captured. Each "note ..." + "you get a one-time boon with
    /// [... the noted number ...]" resolution snapshots the noting
    /// player's `Player::noted_number` into the granted ability at install
    /// time, so sequential notes (or notes from different cards) never
    /// leak into each other's boons: a boon reads what its own grant
    /// captured, not the live global. `None` outside boon grants (and for
    /// grants whose resolution noted nothing), where `NotedNumber` falls
    /// back to the live global. Stamped only by
    /// `game::effects::create_boon::resolve`; the stored ability rides the
    /// delayed-fire path untouched, so the capture survives fire, stack,
    /// and resolution without any trigger-entry lookup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boon_captured_noted_number: Option<i32>,
    /// Digital-only Alchemy (no CR entry): the matching entrant set of a
    /// batch-semantics boon trigger ("one or more creatures enter",
    /// Dunbarrow Revivalist), pinned at fire time in batch order. A
    /// one-shot delayed trigger fires once per batch, so the single fired
    /// event cannot serve a "one of them" resolution choice — the fire
    /// path stamps every matching entrant here (with incarnations, so a
    /// departed-and-returned entrant is a new object per CR 400.7) and the
    /// token host resolver offers the still-legal subset as the choice.
    /// Empty for every non-batch boon and outside boons; stamped only by
    /// the delayed-fire path in `game::triggers`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub boon_trigger_batch_objects: Vec<crate::types::identifiers::ObjectIncarnationRef>,
    /// CR 608.2c: The immediate `forward_result` producer's complete ordered
    /// result. `None` means no producer has run in this resolution; `Some([])`
    /// is a completed producer that moved no objects and intentionally blocks
    /// inherited-target fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forwarded_result_context: Option<Box<ForwardedResultContext>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_forwarded_zone_result: Option<PendingForwardedZoneResult>,
    /// CR 400.7j + CR 608.2c: every card an "exile cards … until …" loop moved
    /// to exile in this resolution, handed down the rest of its chain. "The
    /// other cards exiled this way" (Invasion of Alara) are found among exactly
    /// these cards. Empty outside such a chain.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exile_until_batch: Vec<crate::types::identifiers::ObjectIncarnationRef>,
    /// CR 610.3a / CR 610.3b: specified duration events observed after an activated
    /// ability was put onto the stack or a triggered ability triggered, but before
    /// this initial zone-change effect occurred.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub duration_events: Vec<DurationEvent>,
    /// CR 118.1 + CR 119.4b: A completed resolution-time `PayCost` that paid
    /// life reports this amount to the immediate following "that much" clause.
    /// `Some(0)` is distinct from no life-payment channel at all, and the value
    /// stays with a paused payment's continuation until that cost completes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pay_cost_paid_life_amount: Option<u32>,
    /// CR 602.2b + CR 601.2h + CR 608.2h: counters actually removed while paying
    /// THIS activated ability's cost (summed over its counter leaves), written by the
    /// activation's own cost payment as each counter leg completes (a removal that
    /// waits on a replacement choice is credited once it settles, CR 118.11); the
    /// mana route binds it at production (CR 605.3b). Taken off the
    /// root at the chain's depth-0 entry and seeded into `last_effect_amount`, so
    /// "… removed this way" / "that much" read it. `None` = no counter cost;
    /// `Some(0)` = a counter cost that removed none. CR 707.10: a copy of the
    /// ability copies all decisions made for it and can't pay the cost again, so a
    /// copy keeps the original's count (the official copier rulings: "If the activated
    /// ability's cost contains a choice, such as … a number of counters to remove,
    /// the copy uses that same information").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_removed_counter_amount: Option<u32>,
    /// CR 701.9a + CR 608.2c: The result of the immediately preceding discard
    /// instruction. `apply_parent_chain_context` copies this only to that
    /// discard's direct child and clears it on every other hand-off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct_discard_result: Option<DiscardedCardResult>,
    /// CR 701.20e + CR 608.2c: Exact parent-produced cards forwarded to one
    /// immediate "for each" child. The child uses this snapshot as its
    /// iteration universe instead of an unqualified battlefield census.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_target_iteration_members: Option<Vec<ObjectId>>,
    /// CR 608.2c: Result of the immediately preceding effect. Ordinary
    /// parent-to-child handoffs clear this field; only the producer's direct
    /// completion node may consume it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_effect_result: Option<EffectResolutionResult>,
    /// CR 601.2c + CR 115.1: For a target slot announced by "an opponent's
    /// choice", the opponent the spell's controller chose to make that choice.
    /// In a multiplayer game the controller picks which opponent announces;
    /// `None` until that choice is made (and for the single-opponent default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub announcing_opponent: Option<PlayerId>,
    /// CR 702.174a: Opponent chosen when the Gift additional cost was paid.
    /// Distinct from `announcing_opponent` (CR 115.1 target-chooser).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gift_recipient: Option<PlayerId>,
    /// Whether the spell's optional additional cost was paid during casting.
    #[serde(default)]
    pub additional_cost_paid: bool,
    /// CR 118.9: Whether the controller paid an alternative mana cost from
    /// `casting_options` (not an optional/additional cost such as kicker or
    /// pay-life alternatives).
    #[serde(default)]
    pub alternative_mana_cost_paid: bool,
    /// CR 118.9 + CR 601.2b: When a `CastWithAlternativeCost { OncePerTurn }`
    /// grant's alternative cost was applied to this cast, the granting permanent's
    /// id — read at `finalize_cast` to consume its per-turn slot in
    /// `GameState::alt_cost_grant_permissions_used`. `None` for self-options and
    /// `Unlimited` grants (nothing to consume).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt_cost_grant_source: Option<ObjectId>,
    /// CR 601.2a + CR 601.2b: For a card cast from the graveyard under a
    /// graveyard-cast permission (printed cost, Blitz, Bestow), the
    /// `GraveyardPermission` the player announced, carrying the per-type slot
    /// when one applies. Stamped as costs begin from the prepared cast. Read by
    /// the extra-cost lookup and by `finalize_cast`, so the permission whose
    /// rider was charged is the one spent even if the board changes during
    /// payment (a permission source sacrificed for mana). `None` for every other
    /// cast.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graveyard_permission_authority: Option<crate::types::game_state_kinds::CastingVariant>,
    /// CR 601.2f + CR 601.2h + CR 614.1c: the announced graveyard permission's
    /// terms (its extra cost and "enters with a counter" rider) as they were
    /// when the cast was announced, applied even if the permission's source
    /// leaves or loses the ability during casting. `None` for every other cast.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graveyard_permission_latch: Option<crate::types::game_state::GraveyardPermissionLatch>,
    /// CR 601.2b/f/h: Number of non-kicker additional-cost payments declared
    /// while casting this spell. Used by keyword abilities such as Squad
    /// (CR 702.157a), whose repeatable payment count is not a kicker count.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub additional_cost_payment_count: u32,
    /// CR 113.2c + CR 601.2b/f: Per-instance non-kicker additional-cost
    /// payments declared while casting this spell. This is the non-kicker
    /// analogue to `kickers_paid`: it is a strict superset of the legacy
    /// `additional_cost_paid` / `additional_cost_payment_count` aggregate facts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_cost_payments: Vec<AdditionalCostInstancePayment>,
    /// CR 702.33d + CR 702.33f: The list of kicker payments declared during
    /// casting, in payment order. For "Kicker {A} and/or {B}" cards (CR 702.33b),
    /// each chosen kicker pushes a corresponding `KickerVariant` entry. For
    /// multikicker (CR 702.33c), each repeated payment pushes another `First`.
    /// Single-kicker spells push at most one `First` entry. Empty when no
    /// kicker was paid.
    ///
    /// Linked to (and a strict superset of the information in)
    /// `additional_cost_paid` for kicker-bearing spells:
    ///   - empty                       ⇔ kicker not paid
    ///   - non-empty                   ⇔ kicker paid
    ///   - contains `KickerVariant::X` ⇔ that specific kicker variant was paid
    ///   - `len() >= n`                ⇔ kicker was paid at least N times
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kickers_paid: Vec<KickerVariant>,
    /// CR 700.2a + CR 700.2d + CR 601.2b: The modal-mode indices chosen for this
    /// spell as it was cast (ascending, with repeats per CR 700.2d). Populated in
    /// `handle_select_modes` (the modal-SPELL channel) from the same
    /// `sorted_indices` written to `PendingCast.chosen_modes`, and stamped onto
    /// the spell-on-stack `GameObject.chosen_modes` at cast finalize so
    /// cast-triggers (Riku: `QuantityRef::EventContextSourceModesChosen`) read the
    /// mode count. Empty for non-modal spells. Mirrors the `kickers_paid`
    /// cast-latch channel.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chosen_modes: Vec<usize>,
    /// Whether an optional "you may" effect was performed during resolution.
    /// Used by AbilityCondition::effect_performed() to gate dependent sub_abilities.
    #[serde(default)]
    pub optional_effect_performed: bool,
    /// CR 118.12: A mandatory instruction of this run ("sacrifice it and attach …")
    /// that did nothing, so a later member's "if you do" is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unperformed_compound_instruction: Option<EffectKind>,
    /// CR 608.2d: The just-resolved `Effect::OpponentGuess` outcome, stamped onto
    /// the stashed continuation chain by the guess answer handler. Tri-state:
    /// `None` = no guess happened (impossible commit per CR 609.3 / empty hand),
    /// `Some(GuessOutcome::Correct)` = guessed correctly,
    /// `Some(GuessOutcome::Incorrect)` = guessed incorrectly.
    /// Read by `EffectOutcomeSignal::Guessed { outcome }`; `None` makes both
    /// polarities false so neither rider branch fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guess_outcome: Option<GuessOutcome>,
    /// CR 608.2d: The player who accepted an "any opponent may" optional effect.
    /// Used to resolve "that player" / "them" backreferences and target scoping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepting_player: Option<PlayerId>,
    /// CR 603.4: The zone the spell was cast from. Propagated from casting through
    /// to ETB triggers so conditions like "if you cast it from your hand" can evaluate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cast_from_zone: Option<Zone>,
    /// CR 601.2a + CR 603.4: The player who cast the spell. Propagated with
    /// `cast_from_zone` for caster-scoped intervening-if conditions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cast_controller: Option<PlayerId>,
    /// CR 102.1 + CR 109.5 + CR 601.2: The active player whose turn it was when the spell
    /// was cast. Used by addendum-style conditions ("if you cast this spell during your
    /// main phase") to verify that the spell was cast during the caster's own turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cast_active_player: Option<PlayerId>,
    /// CR 601.2: Phase at the time the spell was cast. Used by addendum-style
    /// conditions such as "if you cast this spell during your main phase".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cast_phase: Option<Phase>,
    /// CR 601.2c + CR 608.2c: The resolving spell/ability's declared **object**
    /// targets, stamped once at `finalize_cast` (after targets are chosen, before
    /// resolution). Read by [`ObjectScope::ChainRootTarget`] from a nested
    /// sub-ability whose own `targets` name a different object — e.g. Dismantle's
    /// "put that many … counters on an artifact you control", where the sub's
    /// target is the resolution-chosen recipient but "that artifact" still means
    /// the spell's own target. Empty for every spell whose text makes no such
    /// back-reference, and for activated/triggered abilities (which never reach
    /// `finalize_cast`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chain_root_targets: Vec<TargetRef>,
    /// CR 601.2 + CR 608.2c: Presence filters the controller matched as the
    /// spell was cast. Used by effects that say "if you controlled a [filter]
    /// as you cast this spell"; the resolver checks this snapshot instead of
    /// the resolution-time battlefield.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub controller_controlled_as_cast: Vec<TargetFilter>,
    /// CR 702: Keyword origin carried from the parsed/synthesized definition
    /// into runtime resolution for keyword-specific events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ability_tag: Option<AbilityTag>,
    /// CR 701.27f: The source permanent's transformation generation captured
    /// when an activated or triggered ability was put onto the stack.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_transformation_count: Option<u32>,
    /// CR 701.3a + CR 608.2d: Semantic bindings for `Effect::Attach` roles:
    /// selected attachment targets, the selected host, and forwarded
    /// event-scoped attachment candidates. This runtime resolution context keeps
    /// those roles distinct across resolution-time choices without expanding
    /// `ResolvedAbility` literals.
    #[serde(default, skip_serializing_if = "AttachTargetBindings::is_empty")]
    pub attach_target_bindings: AttachTargetBindings,
    /// CR 120.1 + CR 608.2b: How the `DamageSource::Target` subject slot bound
    /// for the immediately following damage clause. Stamped by the chain
    /// descent that reconstructs the `[subject, recipient…]` contract and read
    /// by `deal_damage`'s subject authority. One-hop only: cleared by
    /// `apply_parent_chain_context` so it never reaches a grandchild.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_damage_source: Option<TargetDamageSourceBinding>,
    /// CR 601.2i + CR 400.7: For a triggered ability whose trigger event is a
    /// spell being cast ("that spell" / a self-cast "this spell"), that
    /// spell's object and incarnation as they stood when this ability was put
    /// on the stack. Bound at the single triggered-stack-entry constructor
    /// (`triggers.rs::push_pending_trigger_to_stack_with_firing_and_duration_events`)
    /// and re-bound when a pending entry's ability is assigned
    /// (`triggers.rs::assign_pending_trigger_entry_ability`); read by
    /// `targeting::triggering_spell`. `None` for a non-spell-cast trigger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triggering_spell: Option<ObjectIncarnationRef>,
    /// CR 602.2a + CR 607.1 + CR 613.1f: provenance of the activated ability this
    /// stack object came from on its source, bound at announcement
    /// (`casting::record_activation_announcement`). Never set for triggered
    /// abilities — theirs is read from `trigger_definition_ref` — so trigger
    /// batch-run context equality is unaffected. Not redacted from viewer states.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ability_provenance: Option<AbilityProvenance>,
}

impl SpellContext {
    pub fn record_additional_cost_payment(&mut self, origin: AdditionalCostOrigin, count: u32) {
        self.record_additional_cost_instance_payment(origin, 0, count);
    }

    pub fn record_additional_cost_instance_payment(
        &mut self,
        origin: AdditionalCostOrigin,
        origin_ordinal: u32,
        count: u32,
    ) {
        self.record_instance_payment(AdditionalCostInstancePayment::new_with_ordinal(
            origin,
            origin_ordinal,
            count,
        ));
    }

    /// CR 702.153b: record a payment for one queued instance, keeping the
    /// instance's `grant_source` on the payment.
    pub fn record_payment_for_instance(&mut self, instance: &AdditionalCostInstance, count: u32) {
        self.record_instance_payment(AdditionalCostInstancePayment {
            grant_source: instance.grant_source,
            ..AdditionalCostInstancePayment::new_with_ordinal(
                instance.origin,
                instance.origin_ordinal,
                count,
            )
        });
    }

    fn record_instance_payment(&mut self, payment: AdditionalCostInstancePayment) {
        if payment.count == 0 {
            return;
        }
        self.additional_cost_payment_count = self
            .additional_cost_payment_count
            .saturating_add(payment.count);
        self.additional_cost_payments.push(payment);
        self.additional_cost_paid = true;
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

    pub fn additional_cost_paid_matches(
        &self,
        source: AdditionalCostPaymentSource,
        variant: Option<KickerVariant>,
        kicker_cost: Option<&ManaCost>,
        min_count: u32,
    ) -> bool {
        if kicker_cost.is_some() && variant.is_none() {
            return false;
        }

        match variant {
            Some(kicker) => self.kickers_paid.contains(&kicker),
            None => {
                let non_kicker_count = if self.additional_cost_payments.is_empty() {
                    self.additional_cost_payment_count
                } else {
                    self.additional_cost_payments
                        .iter()
                        .map(|payment| payment.count)
                        .sum()
                };
                additional_cost_payment_count_matches(
                    source,
                    self.additional_cost_paid || non_kicker_count > 0,
                    self.kickers_paid.len(),
                    non_kicker_count,
                    min_count,
                )
            }
        }
    }
}

pub(crate) fn additional_cost_payment_count_matches(
    source: AdditionalCostPaymentSource,
    additional_cost_paid: bool,
    kicker_count: usize,
    non_kicker_count: u32,
    min_count: u32,
) -> bool {
    match source {
        AdditionalCostPaymentSource::Any => {
            if min_count == 0 || (min_count == 1 && additional_cost_paid) {
                true
            } else {
                kicker_count >= min_count as usize || non_kicker_count >= min_count
            }
        }
        AdditionalCostPaymentSource::Kicker => {
            let min_count = min_count.max(1);
            kicker_count >= min_count as usize
        }
        AdditionalCostPaymentSource::NonKicker => {
            if min_count == 0 {
                true
            } else {
                non_kicker_count >= min_count
                    || (min_count == 1 && additional_cost_paid && kicker_count == 0)
            }
        }
    }
}

/// Classifies a persisted trigger list without inferring runtime provenance.
/// A list is either wholly legacy payloads or wholly identity-bearing entries;
/// a mixture cannot establish an exact occurrence mapping.
pub(crate) fn legacy_trigger_entry_list(entries: &[TriggerEntry]) -> Result<bool, &'static str> {
    let has_legacy = entries.iter().any(|entry| {
        matches!(
            entry.occurrence,
            TriggerDefinitionOccurrenceRef::Unmaterialized
        )
    });
    if has_legacy
        && !entries.iter().all(|entry| {
            matches!(
                entry.occurrence,
                TriggerDefinitionOccurrenceRef::Unmaterialized
            )
        })
    {
        return Err("legacy trigger list mixes payload-only and identity-bearing entries");
    }
    Ok(has_legacy)
}

/// Materializes a payload-only list only when an ordered printed base set proves
/// every slot. Runtime copied and granted triggers have no equivalent proof.
pub(crate) fn materialize_legacy_printed_trigger_entries(
    entries: &mut Vec<TriggerEntry>,
    base_definitions: &[TriggerDefinition],
    base_set: TriggerBaseSetInstanceRef,
) -> Result<(), &'static str> {
    if !legacy_trigger_entry_list(entries)? {
        return Ok(());
    }
    if base_definitions.is_empty()
        || entries.len() != base_definitions.len()
        || !entries
            .iter()
            .zip(base_definitions)
            .all(|(entry, base)| entry.definition == *base)
    {
        return Err("legacy runtime trigger payload has no provable producer or base slot");
    }
    *entries = base_definitions
        .iter()
        .cloned()
        .enumerate()
        .map(|(printed_index, definition)| {
            TriggerEntry::new(
                TriggerDefinitionOccurrenceRef::Printed {
                    base_set,
                    printed_index,
                },
                definition,
            )
        })
        .collect();
    Ok(())
}

/// CR 614.6 + CR 615.5: Continuation effect that runs after a replacement
/// effect's modifications complete. Stashed by the replacement pipeline,
/// drained by callers (`engine_replacement`, `stack`, `deal_damage`,
/// `engine`).
///
/// Two binding states share one slot:
/// - `Template`: an `AbilityDefinition` AST that needs the replacement
///   source for resolution context (e.g. "As ~ enters, choose a basic land
///   type" — controller and source come from the resolving permanent).
/// - `Resolved`: a `ResolvedAbility` that already carries selected targets
///   and resolution-time context, ready for direct dispatch (e.g. Phyrexian
///   Hydra's prevented-damage follow-up captures the source and counter
///   quantity from the resolution that created the prevention shield).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "ability")]
pub enum PostReplacementContinuation {
    Template(Box<AbilityDefinition>),
    Resolved(Box<ResolvedAbility>),
}

/// CR 603.7a: what identifies a delayed triggered ability across the resolutions that create
/// it — the creating source's printed card, and the delayed condition and root effect before
/// any creation-time binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelayedAbilityOrigin {
    pub creator: PrintedCardRef,
    pub condition: DelayedTriggerCondition,
    pub effect: Effect,
}

/// CR 707.2b + CR 707.2c + CR 111.1: A copiable-values snapshot latched when an
/// `Effect::ChoosePermanent` choice is answered (`CopyTargetPurpose::PersistChosenAttribute`).
/// Per CR 707.2c the copiable values a static copy effect grants are determined
/// only when the effect first starts to apply, and per CR 707.2b later changes
/// to the chosen object never propagate — so the values are frozen here rather
/// than re-read live. The `display` identity is art routing only (NOT a CR 707.2
/// copiable value; see [`DisplayIdentity`]), carried so the enchanted host
/// renders the chosen creature's art; it mirrors the identical identity on
/// `ContinuousModification::CopyValues`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LatchedCopiableSnapshot {
    pub values: CopiableValues,
    #[serde(flatten)]
    pub display: DisplayIdentity,
}

/// CR 707.2 + CR 707.2c: Why a `WaitingFor::CopyTargetChoice` was raised, which
/// determines how the answer is consumed in `handle_copy_target_choice`.
/// `BecomeCopy` (default for serde back-compat with pre-existing serialized
/// enter-as-a-copy waits) resolves the entering object into a copy of the
/// chosen permanent. `PersistChosenAttribute` (Metamorphic Alteration) latches
/// the chosen permanent's copiable values onto the source Aura and installs a
/// copy effect on the Aura's enchanted host instead. `CopyTokenSource` (Esix,
/// Fractal Bloom) uses the chosen permanent as the copy SOURCE for a token-creation
/// substitution, transforming nothing and latching nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "type")]
pub enum CopyTargetPurpose {
    /// CR 707.2: the entering object (the clone) becomes a copy of the chosen
    /// permanent, acquiring its copiable values. Every classic enter-as-a-copy
    /// card (Clone, Phantasmal Image, Vesuva, meld/embalm copy tokens).
    #[default]
    BecomeCopy,
    /// CR 707.2c + CR 614.12a: the chosen permanent's copiable values are
    /// latched and applied to a *separate* recipient (the source Aura's host)
    /// rather than to the entering object. Metamorphic Alteration.
    PersistChosenAttribute,
    /// CR 707.1 + CR 614.1a: the chosen permanent is the **copy source** for a
    /// token-creation substitution — the replacement creates that many tokens
    /// that are copies of it (Esix, Fractal Bloom). Unlike `BecomeCopy`, nothing
    /// is transformed; unlike `PersistChosenAttribute`, nothing is latched.
    CopyTokenSource,
}

// ---------------------------------------------------------------------------
// Keyword-action payloads
// ---------------------------------------------------------------------------

/// Typed payload for activated keyword abilities resolved from the stack.
///
/// CR 113.3b requires activated abilities to go on the stack. CR 113.7a
/// requires last-known-information carry-through when the source leaves
/// its zone. Cost-payment snapshots (e.g. `Station::snapshot_power`) are
/// captured at announcement time so resolution is safe even if the paid
/// creature leaves the battlefield.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum KeywordAction {
    /// CR 702.6a: attach source equipment to target creature.
    Equip {
        equipment_id: ObjectId,
        target_creature_id: ObjectId,
    },
    /// CR 702.122a: the Vehicle becomes an artifact creature until end of turn.
    /// Paid-creature ids are captured for logging and trigger context only.
    Crew {
        vehicle_id: ObjectId,
        paid_creature_ids: Vec<ObjectId>,
    },
    /// CR 702.171a: this permanent becomes saddled until end of turn.
    /// Paid-creature ids are captured for logging and trigger context only.
    Saddle {
        mount_id: ObjectId,
        paid_creature_ids: Vec<ObjectId>,
    },
    /// CR 702.184a: put charge counters on the Spacecraft equal to the tapped
    /// creature's power. The power value is snapshot at cost-payment time so
    /// resolution is stable under CR 113.7a even if the paid creature leaves
    /// the battlefield between announcement and resolution.
    Station {
        spacecraft_id: ObjectId,
        paid_creature_id: ObjectId,
        snapshot_power: i32,
    },
}

// ---------------------------------------------------------------------------
// Resolved ability -- simplified, zero HashMap
// ---------------------------------------------------------------------------

/// CR 707.10 + CR 614.1a: Whether copy-count replacement effects (Twinning
/// Staff's "copy an additional time") have already been folded into a
/// `CopySpell` resolution's iteration count.
///
/// A `CopySpell` of a targeted spell pauses on `CopyRetarget` per copy; the
/// drain driver then resumes the next iteration with a single-iteration ability.
/// The bonus must apply to the copy *event* once (CR 614.5 — a replacement
/// effect doesn't invoke itself repeatedly; it gets only one opportunity to
/// affect an event), not per copy, so a resumed iteration is marked `Finalized`
/// and the count hook skips it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CopyCountStatus {
    /// Initial resolution — copy-count replacements not yet applied.
    #[default]
    Pending,
    /// Resumed iteration — the bonus is already folded into the iteration count.
    Finalized,
}

impl CopyCountStatus {
    /// True for the initial resolution (the only state in which copy-count
    /// replacements should be applied).
    pub fn is_pending(&self) -> bool {
        matches!(self, CopyCountStatus::Pending)
    }
}

/// CR 608.2c: Distinguishes WHY a chained `ParentTarget` child
/// ability was handed off with nothing to act on. The sources are mutually
/// exclusive per hand-off (only one effect can be the immediate parent of a
/// given child). Downstream consumers inspect the typed reason only when their
/// exact `ParentTarget` operation needs to distinguish a missing referent from
/// an ordinary empty target list. The first three used to be parallel boolean
/// fields on `ResolvedAbility` / `GameState` before being consolidated here
/// (see the PR #5834/#5836 review that requested this).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParentTargetMissingReason {
    /// CR 401.5 (issue #1365): A `Dig` (or `RevealTop`) looked at an empty library. Consulted
    /// by exact `ParentTarget` no-op guards: the `PutAtLibraryPosition` Dig-tail
    /// seam (`put_on_top.rs`) avoids the generic self-fallback, and optional
    /// cast/play operations avoid offering a nonexistent card regardless of
    /// whether their driver is immediate or lingering.
    /// Without the first guard, the Dig's own source (e.g. a reanimated
    /// Thassa's Oracle) would move into the library it just found empty.
    Dig,
    /// CR 609.3: A `ChooseFromZone` had no cards to choose from. Consumers
    /// that name the missing choice through `ParentTarget` (e.g. a chained
    /// perpetual rider, `perpetual.rs`) must no-op instead of using the
    /// shared source fallback.
    ChooseFromZone,
    /// CR 608.2c (issue #4950, Thoughtseize): A `RevealHand` reveal-choice
    /// ("choose a nonland card from it") came up with an empty eligible set —
    /// there was nothing to choose, so no object was ever bound as
    /// `ParentTarget`. Distinct from a `Discard`/`DiscardCard` whose OWN
    /// target is player-scoped (Tinybones' "target player discards a card",
    /// Sonic Shrieker's "they discard a card" forwarding a damaged PLAYER,
    /// not a chosen card): those never produce this reason, since no
    /// reveal-choice ran, and must still fall back to the generic
    /// hand-choice/random discard path. Consulted by `effects::discard::
    /// resolve` to resolve a `ParentTarget`-bound discard with zero forwarded
    /// objects to a hard no-op (CR 608.2c: nothing to choose) instead of the
    /// whole-hand fallback.
    RevealHandChoice,
    /// CR 609.3 + CR 608.2c (issue #8798, Tainted Pact): an `ExileTop` found
    /// an empty library, so no card was exiled and nothing was bound as
    /// "that card" for the chained `ParentTarget` child. Consulted by the
    /// `ParentTarget` `ChangeZone` no-op guard (`change_zone.rs`) and by the
    /// optional-effect feasibility probe (`optional_effect_is_infeasible`):
    /// "you may put that card into your hand" with no exiled card is neither
    /// an offerable option (CR 608.2d) nor a license for the generic
    /// source fallback, which would move the resolving spell itself.
    ExileTop,
    /// CR 701.20a + CR 603.12: an `Effect::RevealUntil` exhausted the library
    /// without revealing any card matching its until-filter, so no matching
    /// card was revealed "this way" and nothing is bound as "that card".
    /// Consulted by the `ParentTarget` `ChangeZone` no-op guard and the
    /// optional-effect feasibility probe (as every reason is), and by the
    /// `EffectOutcomeSignal::RevealUntilMatched` guard of a "When you reveal a
    /// <filter> card this way" reflexive: that trigger event did not occur.
    RevealUntil,
    /// CR 701.23a + CR 701.23b + CR 614.6 + CR 608.2c: a library search delivered
    /// no card — no candidate matched, the searcher chose to find none, or every
    /// found card was replaced (`apply_search_found_replacements`'s survivor set,
    /// and the zone-owner-elimination `&[]` finalize) — so nothing is bound as
    /// "it" / "that card" for the chained `ParentTarget` child. Read as `.is_some()`
    /// by the `ParentTarget` `ChangeZone` and `PutAtLibraryPosition` no-op guards
    /// and the optional-effect feasibility probe: without it the child's empty
    /// `ParentTarget` falls back to the ability source, and an Archdruid's Charm
    /// that found nothing would put itself into its owner's hand.
    SearchLibrary,
}

#[cfg(test)]
mod parent_target_missing_reason_tests {
    use super::{Effect, ParentTargetMissingReason, QuantityExpr, ResolvedAbility, TargetFilter};
    use crate::types::game_state::{GameState, PendingContinuation};
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    #[test]
    fn missing_referent_reasons_round_trip_and_default_on_parked_continuations() {
        for reason in [
            ParentTargetMissingReason::Dig,
            ParentTargetMissingReason::ChooseFromZone,
            ParentTargetMissingReason::RevealHandChoice,
            ParentTargetMissingReason::ExileTop,
            ParentTargetMissingReason::RevealUntil,
            ParentTargetMissingReason::SearchLibrary,
        ] {
            let json = serde_json::to_string(&Some(reason)).unwrap();
            assert_eq!(
                serde_json::from_str::<Option<ParentTargetMissingReason>>(&json).unwrap(),
                Some(reason)
            );
            let mut ability = ResolvedAbility::new(
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
                vec![],
                ObjectId(1),
                PlayerId(0),
            );
            assert!(serde_json::to_value(&ability)
                .unwrap()
                .get("parent_target_missing_reason")
                .is_none());
            let absent: ResolvedAbility =
                serde_json::from_value(serde_json::to_value(&ability).unwrap()).unwrap();
            assert_eq!(absent.parent_target_missing_reason, None);
            ability.parent_target_missing_reason = Some(reason);
            let mut state = GameState::new_two_player(7);
            state.park_ability_continuation(PendingContinuation::new(Box::new(ability), &state));
            state.last_parent_target_missing_reason = Some(reason);
            let json = serde_json::to_value(&state).unwrap();
            assert!(json.get("last_parent_target_missing_reason").is_none());
            let restored: GameState = serde_json::from_value(json).unwrap();
            assert_eq!(
                restored
                    .active_ability_continuation()
                    .unwrap()
                    .chain
                    .parent_target_missing_reason,
                Some(reason)
            );
            assert_eq!(restored.last_parent_target_missing_reason, None);
        }
    }
}

#[cfg(test)]
mod chosen_target_slots_tests {
    use super::*;

    /// CR 601.2c: a record survives a save whether or not it holds a slot, and
    /// a node saved without the key restores as unrecorded.
    #[test]
    fn a_record_round_trips_and_a_missing_key_restores_unrecorded() {
        for slots in [
            ChosenTargetSlots::Recorded(Vec::new()),
            ChosenTargetSlots::Recorded(vec![0]),
            ChosenTargetSlots::Unrecorded,
        ] {
            let mut ability = ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                vec![],
                crate::types::identifiers::ObjectId(1),
                crate::types::player::PlayerId(0),
            );
            ability.chosen_target_slots = slots.clone();
            let json = serde_json::to_value(&ability).unwrap();
            assert_eq!(
                json.get("chosen_target_slots").is_some(),
                slots != ChosenTargetSlots::Unrecorded,
                "key presence for {slots:?}: {json}"
            );
            let back: ResolvedAbility = serde_json::from_value(json).unwrap();
            assert_eq!(back.chosen_target_slots, slots);
        }
    }

    fn node(targets: Vec<TargetRef>, chosen: Vec<usize>) -> ResolvedAbility {
        let mut node = ResolvedAbility::new(
            Effect::NoOp,
            targets,
            crate::types::identifiers::ObjectId(1),
            crate::types::player::PlayerId(0),
        );
        node.chosen_target_slots = ChosenTargetSlots::Recorded(chosen);
        node
    }

    /// CR 601.2c + CR 608.2d: the guesser is bound at resolution, not chosen,
    /// so prepending it leaves every node's chosen targets as they were.
    #[test]
    fn prepending_a_guesser_keeps_each_node_reading_its_chosen_targets() {
        let other = TargetRef::Player(crate::types::player::PlayerId(1));
        let guesser = crate::types::player::PlayerId(2);
        let object = TargetRef::Object(crate::types::identifiers::ObjectId(7));
        let sub = node(
            vec![other.clone(), TargetRef::Player(guesser), object.clone()],
            vec![0, 2],
        );
        let else_branch = node(vec![object.clone()], vec![0]);
        let mut root = node(Vec::new(), Vec::new())
            .sub_ability(sub)
            .else_ability(else_branch);

        root.push_front_player_target_recursive(guesser);

        let sub = root.sub_ability.as_deref().unwrap();
        assert_eq!(
            sub.targets,
            vec![TargetRef::Player(guesser), other.clone(), object.clone()]
        );
        let else_branch = root.else_ability.as_deref().unwrap();
        assert_eq!(
            else_branch.targets,
            vec![TargetRef::Player(guesser), object.clone()]
        );
        assert_eq!(
            crate::game::ability_utils::declared_targets_in_chain(&root),
            vec![other, object.clone(), object]
        );
    }

    /// CR 601.2c: a player chosen as a target stays chosen when the same
    /// player is moved to the front as the guesser.
    #[test]
    fn a_guesser_chosen_as_a_target_stays_chosen() {
        let guesser = crate::types::player::PlayerId(2);
        let object = TargetRef::Object(crate::types::identifiers::ObjectId(7));
        let mut root = node(vec![object.clone(), TargetRef::Player(guesser)], vec![0, 1]);

        root.push_front_player_target_recursive(guesser);

        assert_eq!(
            root.targets,
            vec![TargetRef::Player(guesser), object.clone()]
        );
        assert_eq!(
            crate::game::ability_utils::declared_targets_in_chain(&root),
            vec![object, TargetRef::Player(guesser)]
        );
    }

    /// CR 115.3 + CR 601.2c: a player chosen for two instances of "target"
    /// stays declared twice when the same player is moved to the front as the
    /// guesser.
    #[test]
    fn a_guesser_chosen_twice_stays_declared_twice() {
        let guesser = crate::types::player::PlayerId(2);
        let g = TargetRef::Player(guesser);
        let object = TargetRef::Object(crate::types::identifiers::ObjectId(7));
        let boards = [
            (vec![g.clone(), object.clone(), g.clone()], vec![0, 2]),
            (vec![object.clone(), g.clone(), g.clone()], vec![0, 1, 2]),
        ];

        let read: Vec<_> = boards
            .into_iter()
            .map(|(targets, chosen)| {
                let mut root = node(targets, chosen);
                root.push_front_player_target_recursive(guesser);
                let illegal =
                    crate::game::ability_utils::illegal_declared_target_slots(&root, &root, &root);
                (
                    root.targets.clone(),
                    crate::game::ability_utils::declared_targets_in_chain(&root),
                    illegal,
                )
            })
            .collect();

        let front = vec![g.clone(), object.clone(), g.clone()];
        assert_eq!(
            read,
            vec![
                (front.clone(), vec![g.clone(), g.clone()], Vec::new()),
                (front, vec![object, g.clone(), g], Vec::new()),
            ]
        );
    }

    /// CR 115.3 + CR 601.2c: a guesser chosen for a node's unless-payer and
    /// quantity slots keeps both when moved to the front, so the quantity slot
    /// still reads the guesser rather than the primary target.
    #[test]
    fn a_guesser_chosen_for_a_quantity_slot_keeps_its_reading() {
        use crate::game::ability_utils::{primary_announced_player, quantity_slot_player_ordinal};
        let guesser = crate::types::player::PlayerId(2);
        let g = TargetRef::Player(guesser);
        let primary = TargetRef::Player(crate::types::player::PlayerId(1));
        let mut root = node(vec![g.clone(), g.clone(), primary.clone()], vec![0, 1, 2]);
        root.effect = Effect::DealDamage {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::TargetZoneCardCount {
                    zone: ZoneRef::Hand,
                    scope: ControllerRef::TargetPlayer,
                    binding: CountBinding::Explicit,
                },
            },
            target: TargetFilter::Player,
            damage_source: None,
            excess: None,
        };
        root.unless_pay = Some(UnlessPayModifier {
            cost: AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 3 },
            },
            payer: TargetFilter::Typed(TypedFilter::default()),
        });
        let reads = |node: &ResolvedAbility| {
            let ordinal = quantity_slot_player_ordinal(&node.targets, Some(node));
            let counted = ordinal.and_then(|ordinal| {
                node.targets
                    .iter()
                    .filter(|target| matches!(target, TargetRef::Player(_)))
                    .nth(ordinal)
                    .cloned()
            });
            (
                ordinal,
                counted,
                primary_announced_player(&node.targets, node),
            )
        };
        assert_eq!(reads(&root), (Some(1), Some(g.clone()), Some(guesser)));

        root.push_front_player_target_recursive(guesser);

        assert_eq!(reads(&root), (Some(1), Some(g.clone()), Some(guesser)));
        assert_eq!(
            crate::game::ability_utils::declared_targets_in_chain(&root),
            vec![g.clone(), g, primary]
        );
    }

    /// CR 601.2c: a projection of a node's targets keeps each recorded slot on
    /// the target it named and drops the slots it does not keep.
    #[test]
    fn a_projection_keeps_recorded_slots_on_their_targets() {
        let [a, b, c] =
            [7, 8, 9].map(|id| TargetRef::Object(crate::types::identifiers::ObjectId(id)));
        let mut root = node(vec![a.clone(), b, c.clone()], vec![0, 1]);

        root.project_target_occurrences(&[2, 0]);

        assert_eq!(root.targets, vec![c, a.clone()]);
        assert_eq!(
            crate::game::ability_utils::declared_targets_in_chain(&root),
            vec![a]
        );
    }

    /// CR 601.2c: a target inserted without being chosen shifts the recorded
    /// slots after it and leaves those before it.
    #[test]
    fn an_unchosen_insert_keeps_recorded_slots_on_their_targets() {
        let first = TargetRef::Object(crate::types::identifiers::ObjectId(7));
        let last = TargetRef::Object(crate::types::identifiers::ObjectId(8));
        let inserted = TargetRef::Object(crate::types::identifiers::ObjectId(9));
        let mut root = node(vec![first.clone(), last.clone()], vec![0, 1]);

        root.insert_target(1, inserted.clone(), None);

        assert_eq!(root.targets, vec![first.clone(), inserted, last.clone()]);
        assert_eq!(
            crate::game::ability_utils::declared_targets_in_chain(&root),
            vec![first, last]
        );
    }
}

#[cfg(test)]
mod target_read_origin_tests {
    use super::*;

    /// CR 115.1 + CR 608.2c: `target_reads` rides both carriers. The
    /// parent-announcement origin serializes its key and round-trips; the
    /// default omits the key, round-trips, and is what a key-less payload
    /// (every pre-v93 save) deserializes to.
    #[test]
    fn target_reads_round_trips_and_omits_the_default() {
        let effect = Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        };
        for origin in [
            TargetReadOrigin::ParentAnnouncement,
            TargetReadOrigin::OwnAnnouncement,
        ] {
            let mut def = AbilityDefinition::new(AbilityKind::Spell, effect.clone());
            def.target_reads = origin;
            let json = serde_json::to_value(&def).unwrap();
            assert_eq!(
                json.get("target_reads").is_some(),
                origin == TargetReadOrigin::ParentAnnouncement,
                "definition key presence for {origin:?}: {json}"
            );
            let back: AbilityDefinition = serde_json::from_value(json).unwrap();
            assert_eq!(back.target_reads, origin);

            let mut resolved = ResolvedAbility::new(
                effect.clone(),
                vec![],
                crate::types::identifiers::ObjectId(1),
                crate::types::player::PlayerId(0),
            );
            resolved.target_reads = origin;
            let json = serde_json::to_value(&resolved).unwrap();
            assert_eq!(
                json.get("target_reads").is_some(),
                origin == TargetReadOrigin::ParentAnnouncement,
                "resolved key presence for {origin:?}: {json}"
            );
            let back: ResolvedAbility = serde_json::from_value(json).unwrap();
            assert_eq!(back.target_reads, origin);
        }
    }
}

/// CR 608.2c: what a chain split — a `player_scope` fan-out, or a multi-target
/// player subject — DETACHED from this node's chain.
///
/// Why this exists: the publish gate for a tracked-set producer must judge the
/// PRE-SPLIT chain. A splitter hands the per-iteration resolution a template
/// whose remainder has been detached, so a structural walk over that template
/// cannot see a producer surviving in the detached tail — the head would publish
/// where the undetached chain declines, binding a later `TrackedSet` consumer to
/// the wrong population. The verdict is purely structural, so it is computed
/// once at split time and carried on the template rather than re-derived.
///
/// SOLE WRITERS: `split_player_scope_chain`, `split_multi_target_player_chain`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DetachedRemainder {
    /// Nothing was detached, or the detached remainder holds no producer in
    /// publisher position. The template's own walk is the whole truth.
    #[default]
    NoProducer,
    /// The detached remainder holds a node in publisher position, so this node
    /// is NOT the sole producer of its pre-split chain and must not publish.
    HoldsPublisher,
}

/// CR 701.3a + CR 608.2d: The three semantic roles of an [`Effect::Attach`]
/// instruction: selected attachment targets, selected host, and forwarded
/// event-scoped attachment candidates.
///
/// `targets` remains the compatibility/chain anaphor projection. This narrow
/// binding preserves all three roles across resolution-time choices, where
/// appending a selected Equipment to the generic target vector would otherwise
/// make them ambiguous.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachTargetBindings {
    #[serde(flatten)]
    inner: Option<Box<AttachTargetBindingsInner>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct AttachTargetBindingsInner {
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "deserialize_attach_attachment_targets"
    )]
    attachment_targets: Vec<ObjectIncarnationRef>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_attach_host_target"
    )]
    host_target: Option<ObjectIncarnationRef>,
    /// Attachments that the immediately preceding forward-result instruction
    /// moved to the battlefield. This is distinct
    /// from `attachment_targets`: the former is the finite event-scoped choice
    /// population, while the latter records the one object the player selected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    attachment_candidates: Vec<ObjectIncarnationRef>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AttachObjectBindingCompat {
    Exact(ObjectIncarnationRef),
    Legacy(TargetRef),
}

impl AttachObjectBindingCompat {
    fn into_object_ref<E: de::Error>(self) -> Result<Option<ObjectIncarnationRef>, E> {
        match self {
            Self::Exact(reference) => Ok(Some(reference)),
            Self::Legacy(TargetRef::Object(object_id)) => Ok(Some(ObjectIncarnationRef::of(
                object_id,
                LEGACY_INCARNATION,
            ))),
            Self::Legacy(TargetRef::Player(_)) => Ok(None),
        }
    }
}

fn deserialize_attach_attachment_targets<'de, D>(
    deserializer: D,
) -> Result<Vec<ObjectIncarnationRef>, D::Error>
where
    D: Deserializer<'de>,
{
    Vec::<AttachObjectBindingCompat>::deserialize(deserializer)?
        .into_iter()
        .try_fold(Vec::new(), |mut bindings, binding| {
            let Some(reference) = binding.into_object_ref()? else {
                return Err(de::Error::custom("attachment role must be an object"));
            };
            bindings.push(reference);
            Ok(bindings)
        })
}

fn deserialize_attach_host_target<'de, D>(
    deserializer: D,
) -> Result<Option<ObjectIncarnationRef>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<AttachObjectBindingCompat>::deserialize(deserializer)?
        .map(AttachObjectBindingCompat::into_object_ref)
        .transpose()
        .map(Option::flatten)
}

impl AttachTargetBindings {
    pub(crate) fn is_empty(&self) -> bool {
        self.inner.as_deref().is_none_or(|inner| {
            inner.attachment_targets.is_empty()
                && inner.host_target.is_none()
                && inner.attachment_candidates.is_empty()
        })
    }

    pub(crate) fn attachment_targets(&self) -> &[ObjectIncarnationRef] {
        self.inner
            .as_deref()
            .map_or(&[], |inner| inner.attachment_targets.as_slice())
    }

    pub(crate) fn host_target(&self) -> Option<&ObjectIncarnationRef> {
        self.inner
            .as_deref()
            .and_then(|inner| inner.host_target.as_ref())
    }

    pub(crate) fn bind_attachment(&mut self, target: ObjectIncarnationRef) {
        self.inner
            .get_or_insert_default()
            .attachment_targets
            .push(target);
    }

    pub(crate) fn set_attachment_targets(&mut self, targets: Vec<ObjectIncarnationRef>) {
        self.inner.get_or_insert_default().attachment_targets = targets;
    }

    pub(crate) fn bind_host(&mut self, target: ObjectIncarnationRef) {
        self.inner.get_or_insert_default().host_target = Some(target);
    }

    pub(crate) fn bind_attachment_candidates(&mut self, candidates: Vec<ObjectIncarnationRef>) {
        self.inner.get_or_insert_default().attachment_candidates = candidates;
    }

    pub(crate) fn attachment_candidates(&self) -> &[ObjectIncarnationRef] {
        self.inner
            .as_deref()
            .map_or(&[], |inner| inner.attachment_candidates.as_slice())
    }
}

/// CR 601.2c: which of a node's `targets` were chosen as its targets.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChosenTargetSlots {
    /// No record.
    #[default]
    Unrecorded,
    /// The indices into `targets` of the targets chosen for this node; empty
    /// when it chose none.
    Recorded(Vec<usize>),
}

impl ChosenTargetSlots {
    /// Whether this node has no record. Such a node is saved without the key,
    /// and a missing key restores as [`Self::Unrecorded`].
    pub fn is_unrecorded(&self) -> bool {
        matches!(self, Self::Unrecorded)
    }

    /// The recorded slots; none for an unrecorded node.
    pub fn slots(&self) -> &[usize] {
        match self {
            Self::Unrecorded => &[],
            Self::Recorded(slots) => slots,
        }
    }

    /// Points each recorded slot at `moved(slot)`, dropping a slot that maps
    /// to `None`.
    pub(crate) fn remap(&mut self, moved: impl Fn(usize) -> Option<usize>) {
        if let Self::Recorded(slots) = self {
            *slots = slots.iter().filter_map(|&slot| moved(slot)).collect();
        }
    }
}

/// Runtime ability data passed to effect handlers at resolution time.
///
/// Serde goes through the derived implementation (`remote = "Self"`) plus the
/// hand-written `Deserialize` below, which normalizes and validates the target
/// occurrence pins of every decoded node — whatever owner holds it (stack,
/// resolution frames, pending casts, prompts) — before any reader sees it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct ResolvedAbility {
    pub effect: Effect,
    pub targets: Vec<TargetRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declares_chosen_group: Option<ChosenGroupId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reads_chosen_group: Option<ChosenGroupId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declares_return_result: Option<ReturnResultId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reads_return_result: Option<(ReturnResultId, ReturnResultReadSpec)>,
    /// Attribution only. Triggered abilities additionally carry the exact
    /// context below; callers must never use this raw id to rebind a departed
    /// source to a newer incarnation.
    pub source_id: ObjectId,
    /// CR 601.2i + CR 707.10: Exact finalized cast represented by this
    /// resolving spell ability. Spell copies that were not cast carry `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cast_occurrence: Option<CastOccurrence>,
    /// CR 400.7: Exact source incarnation captured for self-transform guards.
    /// The full `trigger_source` context remains the authority for all triggered
    /// source facts; this field preserves the source epoch for activated and
    /// delayed self-transform abilities that do not carry trigger provenance.
    /// For an activated ability this is the epoch the source had as the ability
    /// was put on the stack, after its costs were paid — the reference point the
    /// source-reconstruction consumers (Embalm/Eternalize/Encore copy sources,
    /// aura and equipment host references, craft returns) look the source up
    /// by. The announcement epoch is a separate field
    /// (`announced_source_incarnation`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_incarnation: Option<u64>,
    /// CR 400.7 + CR 602.2a: Exact source incarnation captured at an
    /// activation's announcement (`casting::record_activation_announcement`),
    /// before any cost that moves the source is paid (CR 601.2h). The
    /// source-subject condition gate reads this epoch for activated abilities,
    /// so a cost that moves the source (self-sacrifice, self-exile) cannot
    /// rebind the condition to the moved object's live state; an existing
    /// capture is never overwritten, and stack placement never restamps it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub announced_source_incarnation: Option<u64>,
    /// CR 400.7 + CR 113.7a: Complete event-time authority for a triggered
    /// source. This is recursive across an ability chain and replaces the old
    /// independent incarnation/card-id latches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_source: Option<TriggerSourceContext>,
    /// Exact live definition occurrence that created this triggered ability.
    /// `ability_index` remains presentation/compatibility only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_definition_ref: Option<TriggerDefinitionRef>,
    /// CR 603.7a: the creator-keyed identity a delayed triggered ability carries from its
    /// installation, on the root of its chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delayed_origin: Option<Box<DelayedAbilityOrigin>>,
    /// CR 400.7 + CR 509.1c: Exact attacker selected by a source- or
    /// event-source-referential force-block instruction. This is bound when the
    /// triggered ability is put on the stack, before targets are chosen; it is
    /// deliberately separate from the parsed grammatical selector on `Effect`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force_block_attacker: Option<ObjectIncarnationRef>,
    /// CR 400.7 + CR 603.7c: Incarnation pins for the object referents in
    /// `targets`, captured when a delayed triggered ability snapshotted its
    /// `ParentTarget` referent at creation. A delayed ability that refers to a
    /// particular object must not affect a later object that merely reuses the
    /// same storage `ObjectId` — CR 603.7c: "if that object left that zone and
    /// then returned, it's a new object and thus won't be affected."
    ///
    /// Empty for every ability that is not a pinned `ParentTarget` delayed
    /// trigger — including delayed triggers whose own condition names a ZONE
    /// CHANGE OF THE REFERENT, in either direction (CR 400.7e; see
    /// `condition_names_referent_zone_change`), where the referent is EXPECTED
    /// to have moved and must still be affected. An id with no pin here
    /// resolves unchanged, so the guard is inert outside the pinned path.
    ///
    /// Element-level pins are deliberately non-`Option` so an individual
    /// referent can never degrade to "always matches" (contrast
    /// `source_incarnation`'s `is_none_or` fail-open at `source_is_current`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_incarnations: Vec<ObjectIncarnationRef>,
    /// CR 400.7 + CR 601.2c: the incarnation announced for each target
    /// OCCURRENCE, aligned position-for-position with `targets` (`None` for a
    /// player, or an occurrence nothing pinned). Empty means every occurrence
    /// is unpinned. Separate from `target_incarnations`, whose keyed pins are
    /// reserved for delayed-trigger referents. Written only through the
    /// occurrence authority (`game::target_occurrences`), which moves targets
    /// and pins together.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_pins: Vec<Option<ObjectIncarnationRef>>,
    /// Decode-only carrier for the pre-positional, keyed pin list (wire key
    /// `selected_target_incarnations`). Never serialized, and always empty
    /// after decoding: `ResolvedAbility`'s `Deserialize` folds it into
    /// `target_pins` (each object occurrence takes the first legacy pin with
    /// its id; players and unmatched occurrences none; no pin invented), so a
    /// legacy save re-serializes in positional form.
    #[serde(rename = "selected_target_incarnations", default, skip_serializing)]
    pub legacy_selected_target_incarnations: Vec<ObjectIncarnationRef>,
    /// CR 601.2c: which of `targets` were chosen for this node (see
    /// [`ResolvedAbility::push_chosen_target`]).
    #[serde(default, skip_serializing_if = "ChosenTargetSlots::is_unrecorded")]
    pub chosen_target_slots: ChosenTargetSlots,
    /// CR 602.2b + CR 601.2f: self-referential activation cost modification
    /// carried from the printed ability definition so target-dependent riders
    /// can be applied after targets are committed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_cost_reduction: Option<CostReduction>,
    /// CR 602.2 + CR 601.2c: the facts of this activation captured before any
    /// of its cost is paid (its activator, source, and committed targets). It
    /// travels with the activation and is published to the turn's activation
    /// journal only when the ability is placed on the stack, so a reversed
    /// activation records nothing. Engine authority: never shown to a viewer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_record: Option<Box<crate::types::game_state::AbilityActivationRecord>>,
    /// CR 608.2b: Declared target slots — numbered as
    /// `ability_utils::flatten_targets_in_chain` numbers this chain — whose
    /// target failed the legality check made as the chain began to resolve.
    /// Stamped only on the resolution carrier's root by `stack::resolve_top`
    /// (overwritten on every resolution, empty when nothing was checked) and
    /// read only from the carrier, by
    /// `targeting::resolve_live_parent_slot_from_root`, which drops those
    /// slots' targets, and by `filter::declared_target_player`, which answers
    /// `Illegal` for a stamped player slot. A clone of the carrier (a spell copying
    /// itself, CR 707.10) carries the stamp unread until its own resolution
    /// overwrites it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub illegal_target_slots: Vec<usize>,
    /// CR 608.2b: Declared target slots (numbered like `illegal_target_slots`)
    /// the legality check made as the chain began to resolve did not judge: a
    /// `PassThrough` occurrence (an `Attach` node's unclaimed tail, carried for
    /// a downstream sibling). Its storage and the fizzle verdict are unchanged,
    /// but it supplies no information: `targeting::declared_slot_referent`
    /// reads nothing from it. Stamped beside `illegal_target_slots` on the
    /// resolution carrier's root by `stack::resolve_top`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unjudged_target_slots: Vec<usize>,
    /// CR 608.2b: This node's target-vector indices removed by the initial
    /// legality check for the current resolution. Unlike `illegal_target_slots`,
    /// these are local pre-compaction indices, not declared-chain identities.
    /// The resolution boundary overwrites this metadata before execution; target
    /// propagation only uses its presence to avoid refilling an emptied node.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub illegal_local_target_slots: Vec<usize>,
    pub controller: PlayerId,
    /// CR 109.5: The controller of the spell or ability before any
    /// resolution-time player-scope iteration rebinds the acting player.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_controller: Option<PlayerId>,
    /// CR 115.10 + CR 608.2c: Runtime-only current player for an "each
    /// player/opponent" instruction. This is intentionally distinct from
    /// `controller`, because CR 109.5 keeps "you/your" bound to the ability's
    /// controller while the instruction affects another player.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scoped_player: Option<PlayerId>,
    /// The kind of ability this was (activated, triggered, static, etc.).
    /// Carried through from `AbilityDefinition` to allow resolution guards (e.g. skipping
    /// `BeginGame` abilities during normal stack resolution).
    #[serde(default)]
    pub kind: AbilityKind,
    #[serde(default)]
    pub sub_ability: Option<Box<ResolvedAbility>>,
    /// CR 608.2c: Alternative branch ("Otherwise") executed when condition is not met.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub else_ability: Option<Box<ResolvedAbility>>,
    #[serde(default)]
    pub duration: Option<Duration>,
    /// Condition that must be met for this ability to execute during resolution.
    #[serde(default)]
    pub condition: Option<AbilityCondition>,
    /// Casting-time facts for evaluating conditions during resolution.
    #[serde(default)]
    pub context: SpellContext,
    /// When true, targeting is optional ("up to one"). Player may choose zero targets.
    #[serde(default)]
    pub optional_targeting: bool,
    /// CR 608.2d: Optional effect — controller prompted before execution.
    #[serde(default)]
    pub optional: bool,
    /// CR 608.2d: Event-relative player explicitly named by an optional subject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optional_player: Option<TargetFilter>,
    /// CR 608.2d: When set, an opponent chooses whether to perform this optional effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optional_for: Option<OpponentMayScope>,
    /// Variable-count targeting preserved from the originating ability definition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_target: Option<MultiTargetSpec>,
    /// CR 115.1 + CR 601.2c: Constraints the chosen target set must satisfy
    /// (e.g. combined mana value cap). Carried through from the originating
    /// `AbilityDefinition` so the resolution-time validator can enforce them
    /// against the announced/selected targets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_constraints: Vec<TargetSelectionConstraint>,
    /// CR 601.2c + CR 608.2d: Whether target-like filters are announced on the
    /// stack or selected during resolution.
    #[serde(default, skip_serializing_if = "TargetChoiceTiming::is_stack")]
    pub target_choice_timing: TargetChoiceTiming,
    /// Human-readable description of this ability (from Oracle text / trigger line).
    /// Used by `OptionalEffectChoice` to tell the player what they're choosing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// CR 700.2a + CR 700.2b + CR 700.2d + CR 608.2c: The selected modal
    /// descriptions, in the printed instruction order used to resolve them. This
    /// belongs to the stack ability as a whole; sub-ability chains remain the
    /// individual instructions selected from those modes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected_mode_labels: Vec<String>,
    /// CR 700.2 + CR 608.2c: Marks this node as the ROOT of one selected mode's
    /// instructions, and gives its position in the resolution order.
    ///
    /// CR 700.2: "Each of those options is a mode" — distinct modes are distinct
    /// instructions, not continuations of each other. CR 608.2c ("follows its
    /// instructions in the order written … apply the rules of English") makes an
    /// anaphor like "those cards" / "it" bind to its NEAREST antecedent, which
    /// can never be a sibling mode's population. `build_chained_resolved`
    /// linearizes every selected mode into ONE `sub_ability` chain, erasing that
    /// boundary from the chain shape; this field restores it.
    ///
    /// CR 700.2d: the value is the OCCURRENCE ORDINAL (0-based position within
    /// the ordered selection), NOT the printed mode index — "if a particular
    /// mode is chosen multiple times, the spell is treated as if that mode
    /// appeared that many times in sequence", so Eldrazi Confluence's `[1, 1]`
    /// yields ordinals `0` and `1` at the same printed index. Index-keying would
    /// collide the two occurrences into one instruction.
    ///
    /// `None` on every non-mode-root node, including within-mode continuation
    /// steps and every ability of a non-modal spell.
    ///
    /// SOLE WRITER: `game::ability_utils::build_chained_resolved`. Do not stamp
    /// it anywhere else — consumers rely on "is a mode root" being decidable
    /// from this field alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modal_instruction_ordinal: Option<usize>,
    /// CR 608.2c: the publisher-position verdict of the chain remainder a
    /// splitter detached from this node. See [`DetachedRemainder`]. Default
    /// (`NoProducer`) on every node that was never split.
    #[serde(default)]
    pub detached_remainder: DetachedRemainder,
    /// CR 608.2c: Repeat this ability N times (from "for each [X], [effect]").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_for: Option<QuantityExpr>,
    /// Minimum legal announced value for X. Defaults to zero; set to one by
    /// "X can't be 0" annotations.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub min_x_value: u32,
    /// CR 601.2b + CR 602.2b: the announce-time-locked definition of X, carried
    /// from `AbilityDefinition::announced_x`. Resolved ONCE at announcement into
    /// `chosen_x` (the CR 107.3i single-X-per-object channel); never re-read after.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub announced_x: Option<QuantityExpr>,
    /// Stack-copy restriction from "This ability can't be copied."
    #[serde(default, skip_serializing_if = "is_false")]
    pub cant_be_copied: bool,
    /// CR 608.2b + CR 101.1: root-only; read by stack::resolve_top. Copied from
    /// `AbilityDefinition::illegal_targets_disposition`; a sub-ability's value is ignored.
    #[serde(
        default,
        skip_serializing_if = "IllegalTargetsDisposition::is_does_not_resolve"
    )]
    pub illegal_targets_disposition: IllegalTargetsDisposition,
    /// CR 707.10 + CR 614.1a + CR 614.5: `Finalized` on a `repeat_for` iteration
    /// that the drain driver resumes after a per-copy pause, so the "copy an
    /// additional time" replacement bonus (Twinning Staff) is folded into the
    /// iteration count exactly once — at the initial resolution — and never
    /// re-applied on each resumed iteration (CR 614.5: a replacement effect gets
    /// only one opportunity to affect an event; re-applying would explode into
    /// runaway copies). Only read by the `CopySpell` count hook in
    /// `effects::resolve_effect`.
    #[serde(default, skip_serializing_if = "CopyCountStatus::is_pending")]
    pub copy_count_status: CopyCountStatus,
    /// When true, moved/created objects from this effect are forwarded to the sub_ability.
    #[serde(default)]
    pub forward_result: bool,
    /// CR 118.12: "Effect unless [player] pays {cost}" — tax trigger modifier.
    /// When set, the payer is offered a choice before this effect executes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unless_pay: Option<UnlessPayModifier>,
    /// CR 601.2d: Pre-assigned distribution from casting time ("divide N damage among").
    /// Each entry maps a target to its assigned portion. Read at resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distribution: Option<Vec<(TargetRef, u32)>>,
    /// CR 601.2d + CR 603.3d: Unassigned division metadata carried from the
    /// definition until this stack object's targets and portions are announced.
    /// Distinct from `distribution`, which stores the completed assignment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distribute: Option<DistributionUnit>,
    /// Player scope for "each player/opponent [effect]" patterns.
    /// When set, the effect iterates over matching players (each becomes the acting player).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub player_scope: Option<PlayerFilter>,
    /// CR 101.4 + CR 800.4: Override the default APNAP turn-order start for
    /// `player_scope` iteration. Carried through from `AbilityDefinition` so
    /// the iteration site in `effects/mod.rs` can call
    /// `players::apnap_order_from(state, starting_with, controller)`.
    /// `None` = use active player (standard APNAP). `Some(ControllerRef::You)`
    /// = start with `controller` (Join Forces: "Starting with you, ...").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starting_with: Option<ControllerRef>,
    /// CR 107.1b + CR 601.2f: The value of X chosen by the caster when this
    /// ability was cast/activated. `None` for abilities whose cost has no X.
    /// Read during resolution by `QuantityRef::Variable { name: "X" }`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen_x: Option<u32>,
    /// CR 117.1 + CR 400.7j + CR 608.2k: Public characteristics of the object
    /// paid as part of this resolving ability's cost, captured before it leaves
    /// its zone. Read by cost-paid-object-scoped quantity refs and
    /// `AbilityCondition::CostPaidObjectMatchesFilter` during resolution.
    /// Stays singular (the FIRST object chosen) because it backs referent
    /// wording like "the sacrificed creature's toughness", which is
    /// inherently single-object even when the cost consumed several.
    ///
    /// CR 601.2g + CR 608.2h: when the cost's own move is DEFERRED past the
    /// mana-ability window (a spell's sacrifice additional cost, paid at mana
    /// commit), this snapshot is RE-CAPTURED at the actual payment seam by
    /// [`ResolvedAbility::refresh_cost_paid_capture_recursive`] — "captured
    /// before it leaves its zone" means immediately before, not at selection,
    /// because the selected permanent can be tapped for that very mana in
    /// between.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_paid_object: Option<CostPaidObjectSnapshot>,
    /// CR 106.1b + CR 400.7 + CR 602.2b (issue #6504): The mana type(s) spent
    /// to pay THIS resolving ability's own mana sub-cost, plus the source's
    /// incarnation at the moment this activation reached the stack. Captured
    /// exactly once, synchronously, by `push_ability_entry` (the single
    /// authority where an activated ability reaches the stack) immediately
    /// after cost payment completes — before any later activation of the
    /// SAME permanent could occur. Unlike a per-object mutable latch, this
    /// snapshot travels with THIS activation instance, so a permanent
    /// untapped and reactivated (with a different payment) while this
    /// ability still sits unresolved on the stack cannot corrupt what this
    /// instance observed. Read by `Effect::NoteManaSpent` ("note the type of
    /// mana spent to pay this activation cost" — Jeweled Amulet), which also
    /// compares `source_incarnation` against the object's live incarnation
    /// before writing (CR 400.7: a bounced/flickered source is a new object
    /// with no memory of this activation's payment).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub noted_mana_payment: Option<NotedManaPayment>,
    /// CR 601.2h + CR 602.2b (issue #4948): EVERY object paid as
    /// part of this resolving ability's own cost — unlike `cost_paid_object`
    /// above, not just the first. This engine pays non-self
    /// Sacrifice/Discard/Exile costs BEFORE choosing this SAME ability's own
    /// targets (a documented CR 601.2c-vs-601.2h/602.2b ordering shortcut,
    /// see issue #1301's `exclude_cost_paid_object_that_left_battlefield`),
    /// so any object that already left its zone to pay that cost was never
    /// actually a legal target under the real target-before-cost order — no
    /// matter how many objects the cost consumed.
    ///
    /// CR 400.7 + CR 400.7j: this is the SINGLE authority for "the objects
    /// this ability's cost moved". Each entry is a
    /// [`CostPaidObjectRecord`], which carries either a full payment-time
    /// snapshot (post-cost INCARNATION plus characteristics) or — for a
    /// pre-migration persisted payload, and for a DISCARD payment whose own
    /// move delivered into an unrevealed hidden zone (CR 701.9c) — MEMBERSHIP
    /// only. The engine
    /// reuses `ObjectId`, so an id alone cannot tell "still the object the cost
    /// moved" from "a new object that later took the same id" (CR 400.7).
    /// Readers that only need membership (target-candidate exclusion) project
    /// [`CostPaidObjectRecord::object_id`], which is exact for BOTH variants;
    /// readers that act on the LIVE object must resolve through
    /// [`CostPaidObjectRecord::live_object_id`], which yields `None` for a
    /// membership-only record (fail closed) rather than rebinding a reused id —
    /// and equally for a `Captured` record whose non-discard cost move ended in
    /// a hidden zone, where CR 608.2h keeps the captured `lki` readable while
    /// the stale pin refuses the live reference.
    ///
    /// CR 601.2c: `alias = "cost_paid_object_ids"` migrates the pre-snapshot
    /// save shape (a raw id array) into `MembershipOnly` entries. Dropping
    /// those ids instead would fail the exclusion OPEN — the singular
    /// `cost_paid_object` fallback holds only the FIRST paid object, so the
    /// 2nd and later objects of a restored multi-object payment would become
    /// legal targets for the very ability whose cost consumed them. A payload
    /// carrying BOTH keys is a serde `duplicate field` error, which is the
    /// correct reading: two disagreeing authorities must not be merged.
    ///
    /// Deliberately NOT a second lockstep id vector: there is no parallel raw
    /// field to drift out of sync with these records. `cost_paid_object`
    /// remains the separate SINGULAR resolution-time referent.
    #[serde(
        default,
        alias = "cost_paid_object_ids",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub cost_paid_objects: Vec<CostPaidObjectRecord>,
    /// Public characteristics of an object chosen or moved by an earlier
    /// effect in the same resolving ability. This is distinct from
    /// `cost_paid_object`: the object was not paid as a cost, but later
    /// instructions may still refer to it after it left its zone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_context_object: Option<CostPaidObjectSnapshot>,
    /// CR 701.47c + CR 608.2c: Public characteristics of the Army chosen by
    /// the most recent amass instruction in this resolving ability. Carried
    /// separately from `effect_context_object` so generic demonstratives never
    /// accidentally bind to an amass-specific referent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amassed_army_object: Option<CostPaidObjectSnapshot>,
    /// CR 608.2c: Index of the printed ability this resolution came from on the
    /// source object's ability list. Identifies "this ability" for per-turn
    /// ordinary-resolution-condition tracking (`AbilityCondition::AbilityUseCountThisTurn`),
    /// not an intervening-if condition under CR 603.4. `None` ONLY for
    /// synthesized/runtime-only abilities (prowess, firebending), which have no
    /// printed `abilities[]` entry to index. Activated abilities ARE stamped:
    /// both paths that put one on the stack — `casting_costs::push_ability_entry`
    /// and `planeswalker::push_loyalty_ability_entry` — assign this field
    /// immediately before `push_to_stack`, and each pairs that stamp with the
    /// `restrictions::record_ability_activation` call that keys
    /// `GameState::activated_abilities_this_turn` by the SAME index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ability_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub may_trigger_origin: Option<MayTriggerOrigin>,
    /// CR 115.1 + CR 701.9b: Selection mode carried through from the originating
    /// `AbilityDefinition`. Read by `casting_targets`/`engine_modes`/`planeswalker`
    /// to short-circuit `WaitingFor::TargetSelection` for `Random` abilities.
    #[serde(default, skip_serializing_if = "TargetSelectionMode::is_chosen")]
    pub target_selection_mode: TargetSelectionMode,
    /// CR 601.2c + CR 603.3d: When set, this player (not the controller) announces
    /// this ability's target(s) at stack placement. `None` = controller chooses
    /// (default). Mirrors `target_selection_mode` (the same "by-whom are targets
    /// selected" axis). Distinct from CR 608.2d resolution-time "of their choice"
    /// sacrifices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_chooser: Option<TargetFilter>,
    /// CR 608.2c + CR 109.4: Players chosen by `Effect::Choose { choice_type:
    /// ChoiceType::Player }` instructions during this resolution, in chain
    /// order. The `WaitingFor::NamedChoice` answer handler appends to this list
    /// as each `Choose(Player)` resolves; `ControllerRef::ChosenPlayer { index }`
    /// reads it. Resolution-scoped (mirrors `chosen_x`): it survives the
    /// `Choose` → `drain_pending_continuation` → next `Choose` cycle within one
    /// ability resolution because it travels on the continuation chain, but it
    /// never leaks across abilities. Distinct-player rulings (Gluntch) read
    /// this list as the exclusion set when computing the next choice's options.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chosen_players: Vec<PlayerId>,
    /// CR 608.2c + CR 107.1c: per-iteration loop-continuation predicate carried
    /// through from the originating `AbilityDefinition`. When `Some`, the
    /// resolution chain is re-followed ("repeat this process") under this
    /// predicate. Read by the `repeat_until` dispatch in `resolve_ability_chain`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_until: Option<RepeatContinuation>,
    /// CR 614.5 + CR 616.1f: Replacement effects that already applied to the
    /// proposed event whose post-replacement continuation is resolving. When an
    /// interactive continuation later proposes a follow-up event (for example a
    /// replacement's ChooseOneOf branch creates the substitute token), that event
    /// must inherit this set so the same replacement cannot apply to itself again.
    #[serde(
        default,
        skip_serializing_if = "HashSet::is_empty",
        serialize_with = "crate::types::deterministic_serde::hash_set"
    )]
    pub replacement_applied: HashSet<AppliedReplacementKey>,
    /// CR 608.2c: How this ability links to its parent when present as a
    /// `sub_ability`. Copied through from the originating `AbilityDefinition`.
    /// `SequentialSibling` subs resolve even when an optional parent is declined.
    #[serde(default, skip_serializing_if = "SubAbilityLink::is_continuation")]
    pub sub_link: SubAbilityLink,
    /// CR 115.1 + CR 608.2c: Copied through from the originating
    /// `AbilityDefinition`. See [`TargetReadOrigin`].
    #[serde(default, skip_serializing_if = "TargetReadOrigin::is_own")]
    pub target_reads: TargetReadOrigin,
    /// CR 611.2e + CR 608.2c: Copied through from the originating
    /// `AbilityDefinition`. See [`ContinuousEffectOnset`].
    #[serde(
        default,
        skip_serializing_if = "ContinuousEffectOnset::is_when_followed"
    )]
    pub entry_onset: ContinuousEffectOnset,
    /// CR 702.1c ("the same is true") + CR 608.2c (written order): Copied through
    /// from the originating `AbilityDefinition`. When `ReplicatedOrBranch`, this
    /// `SequentialSibling` is an INDEPENDENT
    /// per-item OR-branch produced by keyword-list replication (Mutable Pupa,
    /// Kathril) and must be evaluated by `resolve_chain_body` regardless of a
    /// preceding sibling's failed gate. `Dependent` (default) preserves the
    /// prior skip-with-failed-predecessor behavior, except the narrow direct
    /// `AbilityUseCountThisTurn` ordinal-sibling case described by
    /// [`SiblingCondition`]. See [`SiblingCondition`].
    #[serde(default, skip_serializing_if = "SiblingCondition::is_default")]
    pub sibling_condition: SiblingCondition,
    /// CR 700.2b + CR 603.3c: Modal choice for a reflexive modal trigger whose modes
    /// are gated behind an optional cost (Caesar). Carried from the def so
    /// `try_materialize_reflexive_trigger` can hand it to the PendingTrigger and
    /// route to AbilityModeChoice. None for non-modal abilities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modal: Option<ModalChoice>,
    /// CR 700.2b: One AbilityDefinition per mode for the reflexive modal trigger.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mode_abilities: Vec<AbilityDefinition>,
    /// CR 608.2c + CR 609.3: Stamped by `effects::apply_parent_chain_context`
    /// at a parent->child hand-off (and carried down target-less hops), by
    /// `inherit_missing_parent_referent` onto an otherwise-branch or a declined
    /// optional's surviving branch, and by `finalize_standard_search_selection`
    /// onto the search's continuation (an empty delivered set; a non-empty one
    /// clears it). See [`ParentTargetMissingReason`] for what each variant
    /// gates and who consults it.
    ///
    /// Serialized: a child parked on a paused continuation carries this across
    /// a `GameState` round trip (reconnect, persistence, P2P resume), where the
    /// `WhenYouDo` gate and the `ParentTarget` guards still read it on resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_target_missing_reason: Option<ParentTargetMissingReason>,
    /// CR 608.2e + CR 603.10d: the boundary of the fan-out control-change
    /// instruction this node belongs to. Set ONLY by a fan-out driver
    /// (`effects::stamp_fan_out_control_boundary`) on its driver-local
    /// template, before its first seat; parked seats clone that template, so a
    /// resumed seat publishes against the same boundary. Never set on a stack
    /// entry, so copies never inherit it, and a following sequential
    /// instruction is a different node. Rules-authoritative: viewer copies drop
    /// it (it can carry hidden information).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_change_instruction: Option<crate::types::events::ControlChangeInstructionCarrier>,
}

impl Serialize for ResolvedAbility {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        ResolvedAbility::serialize(self, serializer)
    }
}

impl<'de> Deserialize<'de> for ResolvedAbility {
    /// CR 400.7: the persisted-target-occurrence boundary. Every decoded node
    /// (its continuations decode through this same impl) folds a legacy keyed
    /// pin list into positional pins and is refused, as a decode error rather
    /// than a later panic or fail-open read, when its pins are not aligned with
    /// its targets.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut ability = ResolvedAbility::deserialize(deserializer)?;
        ability
            .normalize_decoded_target_pins()
            .map_err(de::Error::custom)?;
        Ok(ability)
    }
}

/// CR 400.7 + CR 601.2h: Structural equality for [`ResolvedAbility`].
///
/// Hand-written rather than derived for exactly ONE reason: `cost_paid_objects`
/// must compare by object-id SEQUENCE (see
/// `cost_paid_object_snapshot_ids_eq`) so it preserves the semantics of the
/// raw `Vec<ObjectId>` it replaced. Folding a full `CostPaidObjectRecord`
/// comparison in would make `lki`, `incarnation`, and the captured-vs-migrated
/// variant participate in every identity check that reaches `ResolvedAbility`
/// equality — `GameState`'s own
/// `PartialEq` over `stack` / `waiting_for` included — silently narrowing them.
/// Every OTHER field compares exactly as the derive did, including the singular
/// `cost_paid_object`, whose incarnation is part of its resolution-time meaning.
///
/// Both sides are destructured exhaustively with no wildcard arm, so a new
/// `ResolvedAbility` field is a compile error here (missing binding) or a
/// denied `unused_variables` warning (bound but not compared) rather than a
/// silent equality hole.
impl PartialEq for ResolvedAbility {
    fn eq(&self, other: &Self) -> bool {
        let Self {
            effect: a_effect,
            targets: a_targets,
            declares_chosen_group: a_declares_chosen_group,
            reads_chosen_group: a_reads_chosen_group,
            declares_return_result: a_declares_return_result,
            reads_return_result: a_reads_return_result,
            source_id: a_source_id,
            cast_occurrence: a_cast_occurrence,
            source_incarnation: a_source_incarnation,
            announced_source_incarnation: a_announced_source_incarnation,
            trigger_source: a_trigger_source,
            trigger_definition_ref: a_trigger_definition_ref,
            delayed_origin: a_delayed_origin,
            force_block_attacker: a_force_block_attacker,
            target_incarnations: a_target_incarnations,
            target_pins: _,
            legacy_selected_target_incarnations: _,
            chosen_target_slots: a_chosen_target_slots,
            activation_cost_reduction: a_activation_cost_reduction,
            activation_record: a_activation_record,
            illegal_target_slots: a_illegal_target_slots,
            unjudged_target_slots: a_unjudged_target_slots,
            illegal_local_target_slots: a_illegal_local_target_slots,
            controller: a_controller,
            original_controller: a_original_controller,
            scoped_player: a_scoped_player,
            kind: a_kind,
            sub_ability: a_sub_ability,
            else_ability: a_else_ability,
            duration: a_duration,
            condition: a_condition,
            context: a_context,
            optional_targeting: a_optional_targeting,
            optional: a_optional,
            optional_player: a_optional_player,
            optional_for: a_optional_for,
            multi_target: a_multi_target,
            target_constraints: a_target_constraints,
            target_choice_timing: a_target_choice_timing,
            description: a_description,
            selected_mode_labels: a_selected_mode_labels,
            modal_instruction_ordinal: a_modal_instruction_ordinal,
            detached_remainder: a_detached_remainder,
            repeat_for: a_repeat_for,
            min_x_value: a_min_x_value,
            announced_x: a_announced_x,
            cant_be_copied: a_cant_be_copied,
            illegal_targets_disposition: a_illegal_targets_disposition,
            copy_count_status: a_copy_count_status,
            forward_result: a_forward_result,
            unless_pay: a_unless_pay,
            distribution: a_distribution,
            distribute: a_distribute,
            player_scope: a_player_scope,
            starting_with: a_starting_with,
            chosen_x: a_chosen_x,
            cost_paid_object: a_cost_paid_object,
            noted_mana_payment: a_noted_mana_payment,
            cost_paid_objects: a_cost_paid_objects,
            effect_context_object: a_effect_context_object,
            amassed_army_object: a_amassed_army_object,
            ability_index: a_ability_index,
            may_trigger_origin: a_may_trigger_origin,
            target_selection_mode: a_target_selection_mode,
            target_chooser: a_target_chooser,
            chosen_players: a_chosen_players,
            repeat_until: a_repeat_until,
            replacement_applied: a_replacement_applied,
            sub_link: a_sub_link,
            target_reads: a_target_reads,
            entry_onset: a_entry_onset,
            sibling_condition: a_sibling_condition,
            modal: a_modal,
            mode_abilities: a_mode_abilities,
            parent_target_missing_reason: a_parent_target_missing_reason,
            control_change_instruction: a_control_change_instruction,
        } = self;
        let Self {
            effect: b_effect,
            targets: b_targets,
            declares_chosen_group: b_declares_chosen_group,
            reads_chosen_group: b_reads_chosen_group,
            declares_return_result: b_declares_return_result,
            reads_return_result: b_reads_return_result,
            source_id: b_source_id,
            cast_occurrence: b_cast_occurrence,
            source_incarnation: b_source_incarnation,
            announced_source_incarnation: b_announced_source_incarnation,
            trigger_source: b_trigger_source,
            trigger_definition_ref: b_trigger_definition_ref,
            delayed_origin: b_delayed_origin,
            force_block_attacker: b_force_block_attacker,
            target_incarnations: b_target_incarnations,
            target_pins: _,
            legacy_selected_target_incarnations: _,
            chosen_target_slots: b_chosen_target_slots,
            activation_cost_reduction: b_activation_cost_reduction,
            activation_record: b_activation_record,
            illegal_target_slots: b_illegal_target_slots,
            unjudged_target_slots: b_unjudged_target_slots,
            illegal_local_target_slots: b_illegal_local_target_slots,
            controller: b_controller,
            original_controller: b_original_controller,
            scoped_player: b_scoped_player,
            kind: b_kind,
            sub_ability: b_sub_ability,
            else_ability: b_else_ability,
            duration: b_duration,
            condition: b_condition,
            context: b_context,
            optional_targeting: b_optional_targeting,
            optional: b_optional,
            optional_player: b_optional_player,
            optional_for: b_optional_for,
            multi_target: b_multi_target,
            target_constraints: b_target_constraints,
            target_choice_timing: b_target_choice_timing,
            description: b_description,
            selected_mode_labels: b_selected_mode_labels,
            modal_instruction_ordinal: b_modal_instruction_ordinal,
            detached_remainder: b_detached_remainder,
            repeat_for: b_repeat_for,
            min_x_value: b_min_x_value,
            announced_x: b_announced_x,
            cant_be_copied: b_cant_be_copied,
            illegal_targets_disposition: b_illegal_targets_disposition,
            copy_count_status: b_copy_count_status,
            forward_result: b_forward_result,
            unless_pay: b_unless_pay,
            distribution: b_distribution,
            distribute: b_distribute,
            player_scope: b_player_scope,
            starting_with: b_starting_with,
            chosen_x: b_chosen_x,
            cost_paid_object: b_cost_paid_object,
            noted_mana_payment: b_noted_mana_payment,
            cost_paid_objects: b_cost_paid_objects,
            effect_context_object: b_effect_context_object,
            amassed_army_object: b_amassed_army_object,
            ability_index: b_ability_index,
            may_trigger_origin: b_may_trigger_origin,
            target_selection_mode: b_target_selection_mode,
            target_chooser: b_target_chooser,
            chosen_players: b_chosen_players,
            repeat_until: b_repeat_until,
            replacement_applied: b_replacement_applied,
            sub_link: b_sub_link,
            target_reads: b_target_reads,
            entry_onset: b_entry_onset,
            sibling_condition: b_sibling_condition,
            modal: b_modal,
            mode_abilities: b_mode_abilities,
            parent_target_missing_reason: b_parent_target_missing_reason,
            control_change_instruction: b_control_change_instruction,
        } = other;

        a_effect == b_effect
            && a_targets == b_targets
            && a_declares_chosen_group == b_declares_chosen_group
            && a_reads_chosen_group == b_reads_chosen_group
            && a_declares_return_result == b_declares_return_result
            && a_reads_return_result == b_reads_return_result
            && a_source_id == b_source_id
            && a_cast_occurrence == b_cast_occurrence
            && a_source_incarnation == b_source_incarnation
            && a_announced_source_incarnation == b_announced_source_incarnation
            && a_trigger_source == b_trigger_source
            && a_trigger_definition_ref == b_trigger_definition_ref
            && a_delayed_origin == b_delayed_origin
            && a_force_block_attacker == b_force_block_attacker
            && a_target_incarnations == b_target_incarnations
            // CR 400.7: occurrence pins compare in their normalized, aligned
            // form, so a legacy keyed decode equals its positional re-encode.
            && self.aligned_target_pins() == other.aligned_target_pins()
            && a_chosen_target_slots == b_chosen_target_slots
            && a_activation_cost_reduction == b_activation_cost_reduction
            && a_activation_record == b_activation_record
            && a_illegal_target_slots == b_illegal_target_slots
            && a_unjudged_target_slots == b_unjudged_target_slots
            && a_illegal_local_target_slots == b_illegal_local_target_slots
            && a_controller == b_controller
            && a_original_controller == b_original_controller
            && a_scoped_player == b_scoped_player
            && a_kind == b_kind
            && a_sub_ability == b_sub_ability
            && a_else_ability == b_else_ability
            && a_duration == b_duration
            && a_condition == b_condition
            && a_context == b_context
            && a_optional_targeting == b_optional_targeting
            && a_optional == b_optional
            && a_optional_player == b_optional_player
            && a_optional_for == b_optional_for
            && a_multi_target == b_multi_target
            && a_target_constraints == b_target_constraints
            && a_target_choice_timing == b_target_choice_timing
            && a_description == b_description
            && a_selected_mode_labels == b_selected_mode_labels
            && a_modal_instruction_ordinal == b_modal_instruction_ordinal
            && a_detached_remainder == b_detached_remainder
            && a_repeat_for == b_repeat_for
            && a_min_x_value == b_min_x_value
            && a_announced_x == b_announced_x
            && a_cant_be_copied == b_cant_be_copied
            && a_illegal_targets_disposition == b_illegal_targets_disposition
            && a_copy_count_status == b_copy_count_status
            && a_forward_result == b_forward_result
            && a_unless_pay == b_unless_pay
            && a_distribution == b_distribution
            && a_distribute == b_distribute
            && a_player_scope == b_player_scope
            && a_starting_with == b_starting_with
            && a_chosen_x == b_chosen_x
            && a_cost_paid_object == b_cost_paid_object
            && a_noted_mana_payment == b_noted_mana_payment
            // CR 601.2h: id-sequence only — see the impl doc comment above.
            && cost_paid_object_snapshot_ids_eq(a_cost_paid_objects, b_cost_paid_objects)
            && a_effect_context_object == b_effect_context_object
            && a_amassed_army_object == b_amassed_army_object
            && a_ability_index == b_ability_index
            && a_may_trigger_origin == b_may_trigger_origin
            && a_target_selection_mode == b_target_selection_mode
            && a_target_chooser == b_target_chooser
            && a_chosen_players == b_chosen_players
            && a_repeat_until == b_repeat_until
            && a_replacement_applied == b_replacement_applied
            && a_sub_link == b_sub_link
            && a_target_reads == b_target_reads
            && a_entry_onset == b_entry_onset
            && a_sibling_condition == b_sibling_condition
            && a_modal == b_modal
            && a_mode_abilities == b_mode_abilities
            && a_parent_target_missing_reason == b_parent_target_missing_reason
            && a_control_change_instruction == b_control_change_instruction
    }
}

/// Equality above is reflexive, symmetric, and transitive: every field either
/// delegates to its own `Eq` impl or (for `cost_paid_objects`) compares a
/// projected `ObjectId` sequence, which is itself an equivalence relation.
impl Eq for ResolvedAbility {}

/// CR 400.7 + CR 603.7c: the keyed delayed-referent eligibility predicate
/// shared by live indexed selection
/// ([`ResolvedAbility::target_pin_is_current`]) and resumed parked reads —
/// one policy, evaluated at read time on both paths. An id with no keyed
/// entry passes (fail-open: not a pinned delayed-trigger referent); a
/// recorded pin must be current.
pub fn keyed_target_pin_is_current(
    pins: &[crate::types::identifiers::ObjectIncarnationRef],
    id: crate::types::identifiers::ObjectId,
    state: &crate::types::game_state::GameState,
) -> bool {
    pins.iter()
        .find(|pin| pin.object_id == id)
        .is_none_or(|pin| pin.is_current(state))
}

impl ResolvedAbility {
    /// CR 113.7a + CR 603.3: the definition of the triggered ability this resolving node
    /// belongs to, read from the trigger-time snapshot of its source — never live state.
    pub fn triggered_definition(&self) -> Option<&TriggerDefinition> {
        let occurrence = &self.trigger_definition_ref.as_ref()?.occurrence;
        self.trigger_source
            .as_ref()?
            .trigger_entries
            .iter()
            .find(|entry| &entry.occurrence == occurrence)
            .map(TriggerEntry::definition)
    }

    /// CR 707.10 + CR 400.7: whether a copy of `self` resolves exactly as a copy
    /// of `other`. `PartialEq` compares `cost_paid_objects` by object id only (see
    /// its doc), but a copy uses the objects paid for its own original (CR 707.10)
    /// and each record names one incarnation of that object (CR 400.7), so the
    /// records must match in full. Cost records ride the root /
    /// sub / else chain that `add_cost_paid_records_recursive` writes.
    pub(crate) fn resolves_identically_to(&self, other: &Self) -> bool {
        self == other && self.cost_paid_records_match_recursive(other)
    }

    fn cost_paid_records_match_recursive(&self, other: &Self) -> bool {
        let chains_match = |a: &Option<Box<Self>>, b: &Option<Box<Self>>| match (a, b) {
            (Some(a), Some(b)) => a.cost_paid_records_match_recursive(b),
            (None, None) => true,
            _ => false,
        };
        self.cost_paid_objects == other.cost_paid_objects
            && chains_match(&self.sub_ability, &other.sub_ability)
            && chains_match(&self.else_ability, &other.else_ability)
    }

    /// CR 400.7d + CR 608.2k: True when this resolved ability tree reads the
    /// cost-paid object — i.e. references `ObjectScope::CostPaidObject` in a
    /// magnitude / P-T expression, `TargetFilter::CostPaidObject`, or
    /// `AbilityCondition::CostPaidObjectMatchesFilter` — anywhere in its effect,
    /// condition, or `sub_ability` / `else_ability` branches. Gates the emerge
    /// cast-cost-object propagation onto ETB triggers (`build_triggered_ability`,
    /// Adipose Offspring) and the enters-with replacement continuation stamp in
    /// `engine_replacement::apply_post_replacement_effect` (Dralnu's Pet), so
    /// only chains that consume the referent are stamped.
    ///
    /// The per-node walk is the type-owned [`Effect::references_cost_paid_object`]
    /// / [`AbilityCondition::references_cost_paid_object`]; this method adds only
    /// the chain traversal.
    pub(crate) fn references_cost_paid_object(&self) -> bool {
        self.effect.references_cost_paid_object()
            || self
                .condition
                .as_ref()
                .is_some_and(AbilityCondition::references_cost_paid_object)
            || self
                .sub_ability
                .as_deref()
                .is_some_and(ResolvedAbility::references_cost_paid_object)
            || self
                .else_ability
                .as_deref()
                .is_some_and(ResolvedAbility::references_cost_paid_object)
    }

    /// CR 610.3: The "until" event bounding this node's own zone change, for
    /// every bounded zone-move shape (single-object `ChangeZone` and mass
    /// `ChangeZoneAll`). The single classifier the CR 610.3a/b latch, the
    /// resolvers' refusal, and stack-reach readers share.
    pub(crate) fn bounded_zone_change_event(&self) -> Option<DurationEvent> {
        match self.effect {
            Effect::ChangeZone { .. } | Effect::ChangeZoneAll { .. } => {
                self.duration.as_ref().and_then(Duration::zone_change_event)
            }
            Effect::StartYourEngines { .. }
            | Effect::ChangeSpeed { .. }
            | Effect::DealDamage { .. }
            | Effect::ApplyPostReplacementDamage { .. }
            | Effect::EachDealsDamageEqualToPower { .. }
            | Effect::EachSourceDealsDamage { .. }
            | Effect::Draw { .. }
            | Effect::Pump { .. }
            | Effect::PairWith { .. }
            | Effect::Destroy { .. }
            | Effect::Regenerate { .. }
            | Effect::RemoveAllDamage { .. }
            | Effect::Counter { .. }
            | Effect::CounterAll { .. }
            | Effect::Token { .. }
            | Effect::GainLife { .. }
            | Effect::LoseLife { .. }
            | Effect::SetTapState { .. }
            | Effect::RemoveCounter { .. }
            | Effect::Sacrifice { .. }
            | Effect::DiscardCard { .. }
            | Effect::Mill { .. }
            | Effect::Scry { .. }
            | Effect::PumpAll { .. }
            | Effect::DamageAll { .. }
            | Effect::DamageEachPlayer { .. }
            | Effect::DestroyAll { .. }
            | Effect::Dig { .. }
            | Effect::GainControl { .. }
            | Effect::GainControlAll { .. }
            | Effect::ControlNextTurn { .. }
            | Effect::Attach { .. }
            | Effect::UnattachAll { .. }
            | Effect::Surveil { .. }
            | Effect::Fight { .. }
            | Effect::Bounce { .. }
            | Effect::BounceAll { .. }
            | Effect::Explore
            | Effect::ExploreAll { .. }
            | Effect::Investigate
            | Effect::Tribute { .. }
            | Effect::TimeTravel
            | Effect::BecomeMonarch { .. }
            | Effect::NoOp
            | Effect::Proliferate
            | Effect::ProliferateTarget { .. }
            | Effect::Populate
            | Effect::Clash
            | Effect::Behold { .. }
            | Effect::EndTheTurn
            | Effect::EndCombatPhase
            | Effect::Vote { .. }
            | Effect::SeparateIntoPiles { .. }
            | Effect::ExileTopCardContest { .. }
            | Effect::SwitchPT { .. }
            | Effect::CopySpell { .. }
            | Effect::EpicCopy { .. }
            | Effect::CastCopyOfCard { .. }
            | Effect::CopyTokenOf { .. }
            | Effect::CreateTokenCopyFromPool { .. }
            | Effect::Myriad
            | Effect::Encore
            | Effect::CombineHost { .. }
            | Effect::ChooseAugmentAndCombineWithHost { .. }
            | Effect::Meld { .. }
            | Effect::ExileHaunting { .. }
            | Effect::HideawayConceal { .. }
            | Effect::CopyTokenBlockingAttacker { .. }
            | Effect::BecomeCopy { .. }
            | Effect::ChoosePermanent { .. }
            | Effect::GainActivatedAbilitiesOfTarget { .. }
            | Effect::ChooseCard { .. }
            | Effect::PutCounter { .. }
            | Effect::ChooseCounterKind { .. }
            | Effect::PutChosenCounter { .. }
            | Effect::PutCounterAll { .. }
            | Effect::MultiplyCounter { .. }
            | Effect::ChooseCounterAdjustment { .. }
            | Effect::DoublePT { .. }
            | Effect::DoublePTAll { .. }
            | Effect::MoveCounters { .. }
            | Effect::ReproduceEventCounters { .. }
            | Effect::Animate { .. }
            | Effect::ReturnAsAura { .. }
            | Effect::RegisterBending { .. }
            | Effect::GenericEffect { .. }
            | Effect::Cleanup { .. }
            | Effect::Mana { .. }
            | Effect::Discard { .. }
            | Effect::Shuffle { .. }
            | Effect::Transform { .. }
            | Effect::FlipPermanent { .. }
            | Effect::SearchLibrary { .. }
            | Effect::SearchOutsideGame { .. }
            | Effect::OpenBoosterPack { .. }
            | Effect::RevealHand { .. }
            | Effect::RevealFromHand { .. }
            | Effect::Reveal { .. }
            | Effect::RevealChosenNumbers { .. }
            | Effect::RevealTop { .. }
            | Effect::ExileTop { .. }
            | Effect::ExileFaceDownPile { .. }
            | Effect::TargetOnly { .. }
            | Effect::Choose { .. }
            | Effect::OpponentGuess { .. }
            | Effect::SwapChosenLabels { .. }
            | Effect::ChooseDamageSource { .. }
            | Effect::Suspect { .. }
            | Effect::Unsuspect { .. }
            | Effect::Connive { .. }
            | Effect::PhaseOut { .. }
            | Effect::PhaseIn { .. }
            | Effect::ForceBlock { .. }
            | Effect::ForceAttack { .. }
            | Effect::SolveCase
            | Effect::BecomePrepared { .. }
            | Effect::BecomeUnprepared { .. }
            | Effect::BecomeSaddled { .. }
            | Effect::SetClassLevel { .. }
            | Effect::CreateDelayedTrigger { .. }
            | Effect::AddTargetReplacement { .. }
            | Effect::AddRestriction { .. }
            | Effect::ReduceNextSpellCost { .. }
            | Effect::GrantNextSpellAbility { .. }
            | Effect::AddPendingETBCounters { .. }
            | Effect::AddPendingEntersModifications { .. }
            | Effect::CreateEmblem { .. }
            | Effect::CreateBoon { .. }
            | Effect::PayCost { .. }
            | Effect::CastFromZone { .. }
            | Effect::FreeCastFromZones { .. }
            | Effect::ExileResolvingSpellInsteadOfGraveyard { .. }
            | Effect::PreventDamage { .. }
            | Effect::CreateDamageReplacement { .. }
            | Effect::CreateDrawReplacement { .. }
            | Effect::CreatePlaneswalkReplacement { .. }
            | Effect::LoseTheGame { .. }
            | Effect::WinTheGame { .. }
            | Effect::RollDie { .. }
            | Effect::FlipCoins { .. }
            | Effect::RingTemptsYou
            | Effect::VentureIntoDungeon
            | Effect::VentureInto { .. }
            | Effect::TakeTheInitiative
            | Effect::ArrangePlanarDeckTop { .. }
            | Effect::Planeswalk
            | Effect::AbandonScheme
            | Effect::ChaosEnsues
            | Effect::ReverseTurnOrder
            | Effect::RedistributeLifeTotals
            | Effect::OpenAttractions { .. }
            | Effect::RollToVisitAttractions
            | Effect::AssembleContraptions { .. }
            | Effect::AssembleContraptionsFromRollDifference
            | Effect::CrankContraptions { .. }
            | Effect::ReassembleContraption { .. }
            | Effect::AssembleContraptionOnSprocket { .. }
            | Effect::ReassembleContraptionOnSprocket { .. }
            | Effect::PutSticker { .. }
            | Effect::ApplySticker { .. }
            | Effect::ProcessRadCounters
            | Effect::GrantCastingPermission { .. }
            | Effect::ChooseFromZone { .. }
            | Effect::RememberCard { .. }
            | Effect::NoteManaSpent
            | Effect::NoteNumber { .. }
            | Effect::ForEachCategory { .. }
            | Effect::ChooseObjectsIntoTrackedSet { .. }
            | Effect::ChooseAndSacrificeRest { .. }
            | Effect::EachPlayerCopyChosen { .. }
            | Effect::Exploit { .. }
            | Effect::GainEnergy { .. }
            | Effect::GivePlayerCounter { .. }
            | Effect::LoseAllPlayerCounters { .. }
            | Effect::ExileFromTopUntil { .. }
            | Effect::RevealUntil { .. }
            | Effect::Discover { .. }
            | Effect::PhysicalContest { .. }
            | Effect::Heist { .. }
            | Effect::HeistExile
            | Effect::Cascade
            | Effect::Ripple { .. }
            | Effect::MiracleCast { .. }
            | Effect::MadnessCast { .. }
            | Effect::PutAtLibraryPosition { .. }
            | Effect::ChooseDrawnThisTurnPayOrTopdeck { .. }
            | Effect::PutOnTopOrBottom { .. }
            | Effect::GiftDelivery { .. }
            | Effect::Goad { .. }
            | Effect::GoadAll { .. }
            | Effect::Detain { .. }
            | Effect::SetRoomDoorLock { .. }
            | Effect::ExchangeControl { .. }
            | Effect::ChangeTargets { .. }
            | Effect::Manifest { .. }
            | Effect::ManifestDread
            | Effect::Cloak { .. }
            | Effect::TurnFaceUp { .. }
            | Effect::TurnFaceDown { .. }
            | Effect::ExtraTurn { .. }
            | Effect::GrantExtraLoyaltyActivations { .. }
            | Effect::SkipNextTurn { .. }
            | Effect::SkipNextStep { .. }
            | Effect::AdditionalPhase { .. }
            | Effect::Double { .. }
            | Effect::ModifyAnnouncedX { .. }
            | Effect::RuntimeHandled { .. }
            | Effect::Incubate { .. }
            | Effect::Amass { .. }
            | Effect::EmpowerJace { .. }
            | Effect::Monstrosity { .. }
            | Effect::Specialize
            | Effect::Renown { .. }
            | Effect::Bolster { .. }
            | Effect::Adapt { .. }
            | Effect::Learn
            | Effect::Forage
            | Effect::CompletePlayerAction { .. }
            | Effect::Harness
            | Effect::CollectEvidence { .. }
            | Effect::Endure { .. }
            | Effect::BlightEffect { .. }
            | Effect::Seek { .. }
            | Effect::SetLifeTotal { .. }
            | Effect::ExchangeLifeWithStat { .. }
            | Effect::ExchangeLifeTotals { .. }
            | Effect::SetDayNight { .. }
            | Effect::GiveControl { .. }
            | Effect::RemoveFromCombat { .. }
            | Effect::BecomeBlocked { .. }
            | Effect::Conjure { .. }
            | Effect::ApplyPerpetual { .. }
            | Effect::Intensify { .. }
            | Effect::DraftFromSpellbook { .. }
            | Effect::ChooseOneOf { .. }
            | Effect::Unimplemented { .. } => None,
        }
    }

    /// Whether this ability chain contains a zone change bounded by `event`.
    pub(crate) fn contains_duration_event(&self, event: DurationEvent) -> bool {
        self.bounded_zone_change_event() == Some(event)
            || self
                .sub_ability
                .as_ref()
                .is_some_and(|sub| sub.contains_duration_event(event))
            || self
                .else_ability
                .as_ref()
                .is_some_and(|branch| branch.contains_duration_event(event))
    }

    pub(crate) fn record_duration_event_recursive(&mut self, event: DurationEvent) {
        if self.bounded_zone_change_event() == Some(event)
            && !self.context.duration_events.contains(&event)
        {
            self.context.duration_events.push(event);
        }
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.record_duration_event_recursive(event);
        }
        if let Some(branch) = self.else_ability.as_mut() {
            branch.record_duration_event_recursive(event);
        }
    }

    /// Clears the latched "until" events on this ability and every sub/else
    /// branch, mirroring [`Self::record_duration_event_recursive`].
    pub(crate) fn clear_duration_events_recursive(&mut self) {
        self.context.duration_events.clear();
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.clear_duration_events_recursive();
        }
        if let Some(branch) = self.else_ability.as_mut() {
            branch.clear_duration_events_recursive();
        }
    }

    /// Build from a typed Effect. Simply stores the fields.
    pub fn new(
        effect: Effect,
        targets: Vec<TargetRef>,
        source_id: ObjectId,
        controller: PlayerId,
    ) -> Self {
        Self {
            effect,
            targets,
            declares_chosen_group: None,
            reads_chosen_group: None,
            declares_return_result: None,
            reads_return_result: None,
            source_id,
            cast_occurrence: None,
            controller,
            original_controller: None,
            scoped_player: None,
            kind: AbilityKind::default(),
            sub_ability: None,
            else_ability: None,
            duration: None,
            condition: None,
            context: SpellContext::default(),
            optional_targeting: false,
            optional: false,
            optional_player: None,
            optional_for: None,
            multi_target: None,
            target_constraints: Vec::new(),
            target_choice_timing: TargetChoiceTiming::Stack,
            description: None,
            selected_mode_labels: Vec::new(),
            modal_instruction_ordinal: None,
            detached_remainder: DetachedRemainder::NoProducer,
            repeat_for: None,
            min_x_value: 0,
            announced_x: None,
            cant_be_copied: false,
            illegal_targets_disposition: IllegalTargetsDisposition::DoesNotResolve,
            copy_count_status: CopyCountStatus::Pending,
            forward_result: false,
            unless_pay: None,
            distribution: None,
            distribute: None,
            player_scope: None,
            starting_with: None,
            chosen_x: None,
            cost_paid_object: None,
            noted_mana_payment: None,
            cost_paid_objects: Vec::new(),
            effect_context_object: None,
            amassed_army_object: None,
            ability_index: None,
            may_trigger_origin: None,
            target_selection_mode: TargetSelectionMode::Chosen,
            target_chooser: None,
            chosen_players: Vec::new(),
            repeat_until: None,
            replacement_applied: HashSet::new(),
            sub_link: SubAbilityLink::ContinuationStep,
            target_reads: TargetReadOrigin::OwnAnnouncement,
            entry_onset: ContinuousEffectOnset::WhenFollowed,
            sibling_condition: SiblingCondition::Dependent,
            source_incarnation: None,
            announced_source_incarnation: None,
            trigger_source: None,
            trigger_definition_ref: None,
            delayed_origin: None,
            force_block_attacker: None,
            target_incarnations: Vec::new(),
            target_pins: Vec::new(),
            legacy_selected_target_incarnations: Vec::new(),
            chosen_target_slots: ChosenTargetSlots::Unrecorded,
            activation_cost_reduction: None,
            activation_record: None,
            illegal_target_slots: Vec::new(),
            unjudged_target_slots: Vec::new(),
            illegal_local_target_slots: Vec::new(),
            modal: None,
            mode_abilities: Vec::new(),
            parent_target_missing_reason: None,
            control_change_instruction: None,
        }
    }

    /// CR 601.2c: Appends a target chosen for this node and records its slot.
    pub(crate) fn push_chosen_target(&mut self, target: TargetRef) {
        let slot = self.targets.len();
        match &mut self.chosen_target_slots {
            ChosenTargetSlots::Recorded(slots) => slots.push(slot),
            unrecorded @ ChosenTargetSlots::Unrecorded => {
                *unrecorded = ChosenTargetSlots::Recorded(vec![slot]);
            }
        }
        self.push_target(target);
    }

    /// CR 601.2c: Makes `target` the first entry of `targets`. Recorded slots
    /// keep naming the targets they named; the first entry is recorded only if
    /// an occurrence it replaces was.
    ///
    /// CR 115.3: the first entry replaces every unrecorded occurrence and the
    /// first recorded one; each further recorded occurrence is kept.
    pub(crate) fn move_target_to_front(&mut self, target: TargetRef) {
        let recorded = self.chosen_target_slots.slots();
        let mut front_is_recorded = false;
        let mut moved = Vec::with_capacity(self.targets.len());
        let mut occurrences = vec![(target.clone(), None)];
        for (slot, (entry, pin)) in self.target_occurrences().into_iter().enumerate() {
            let is_recorded = recorded.contains(&slot);
            if entry == target && !(is_recorded && front_is_recorded) {
                front_is_recorded |= is_recorded;
                moved.push(0);
            } else {
                moved.push(occurrences.len());
                occurrences.push((entry, pin));
            }
        }
        self.replace_target_occurrences(occurrences);
        self.chosen_target_slots
            .remap(|slot| moved.get(slot).copied());
    }

    /// CR 601.2c + CR 603.3d: Starts the target record of every node of this
    /// chain that has none, so a node no assignment reached declares nothing.
    pub(crate) fn start_target_record_recursive(&mut self) {
        if self.chosen_target_slots.is_unrecorded() {
            self.chosen_target_slots = ChosenTargetSlots::Recorded(Vec::new());
        }
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.start_target_record_recursive();
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.start_target_record_recursive();
        }
    }

    pub(crate) fn bind_attach_attachment_target(&mut self, target: ObjectIncarnationRef) {
        self.context.attach_target_bindings.bind_attachment(target);
    }

    pub(crate) fn set_attach_attachment_targets(&mut self, targets: Vec<ObjectIncarnationRef>) {
        self.context
            .attach_target_bindings
            .set_attachment_targets(targets);
    }

    pub(crate) fn bind_attach_host_target(&mut self, target: ObjectIncarnationRef) {
        self.context.attach_target_bindings.bind_host(target);
    }

    pub(crate) fn bind_attach_attachment_candidates(
        &mut self,
        candidates: Vec<ObjectIncarnationRef>,
    ) {
        self.context
            .attach_target_bindings
            .bind_attachment_candidates(candidates);
    }

    pub(crate) fn attach_attachment_targets(&self) -> &[ObjectIncarnationRef] {
        self.context.attach_target_bindings.attachment_targets()
    }

    pub(crate) fn attach_host_target(&self) -> Option<&ObjectIncarnationRef> {
        self.context.attach_target_bindings.host_target()
    }

    pub(crate) fn attach_attachment_candidates(&self) -> &[ObjectIncarnationRef] {
        self.context.attach_target_bindings.attachment_candidates()
    }

    /// Stamp or clear one cast coordinate on every complete ability graph
    /// stored by this node.
    pub fn set_cast_occurrence_recursive(&mut self, occurrence: Option<CastOccurrence>) {
        self.cast_occurrence = occurrence;
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_cast_occurrence_recursive(occurrence);
        }
        if let Some(branch) = self.else_ability.as_mut() {
            branch.set_cast_occurrence_recursive(occurrence);
        }
    }

    pub(crate) fn normalize_cast_occurrence_for_loop_recursive(&mut self) {
        if let Some(occurrence) = self.cast_occurrence.as_mut() {
            occurrence.turn_journal_index = 0;
        }
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.normalize_cast_occurrence_for_loop_recursive();
        }
        if let Some(branch) = self.else_ability.as_mut() {
            branch.normalize_cast_occurrence_for_loop_recursive();
        }
    }

    pub(crate) fn cast_occurrence_matches_recursive(&self, occurrence: CastOccurrence) -> bool {
        self.cast_occurrence == Some(occurrence)
            && self
                .sub_ability
                .as_deref()
                .is_none_or(|sub| sub.cast_occurrence_matches_recursive(occurrence))
            && self
                .else_ability
                .as_deref()
                .is_none_or(|branch| branch.cast_occurrence_matches_recursive(occurrence))
    }

    pub fn set_may_trigger_origin_recursive(&mut self, origin: MayTriggerOrigin) {
        self.may_trigger_origin = Some(origin.clone());
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_may_trigger_origin_recursive(origin.clone());
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_may_trigger_origin_recursive(origin);
        }
    }

    /// Install one owned triggered-source authority over this entire ability
    /// chain. Each branch resolves against the same observed incarnation.
    pub fn set_trigger_source_recursive(&mut self, source: TriggerSourceContext) {
        self.source_id = source.identity.reference.object_id;
        self.source_incarnation = Some(source.identity.reference.incarnation);
        self.controller = source.lki.controller;
        self.original_controller = Some(source.lki.controller);
        self.trigger_source = Some(source.clone());
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_trigger_source_recursive(source.clone());
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_trigger_source_recursive(source);
        }
    }

    /// CR 400.7 + CR 113.7a: carry a random-result branch's source identity
    /// without changing its actor, targets, pins, or other resolution context.
    /// Different-source nodes bound the traversal, including their descendants;
    /// nested definition payloads inherit only when they are materialized.
    pub(crate) fn set_result_branch_source_identity_recursive(
        &mut self,
        source_id: ObjectId,
        trigger_source: Option<&TriggerSourceContext>,
        source_incarnation: u64,
    ) {
        if self.source_id != source_id {
            return;
        }
        self.trigger_source = trigger_source.cloned();
        self.source_incarnation = Some(source_incarnation);
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_result_branch_source_identity_recursive(
                source_id,
                trigger_source,
                source_incarnation,
            );
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_result_branch_source_identity_recursive(
                source_id,
                trigger_source,
                source_incarnation,
            );
        }
    }

    /// Digital-only Alchemy (no CR entry): stamp source-less gameplay identity
    /// over this entire ability chain. Writes `source_id` ONLY — controller,
    /// `original_controller`, `trigger_source`, incarnation pins untouched, so
    /// creation provenance (matchers, display) survives. Single call site:
    /// boon install (`create_boon.rs`), whose release note ("a one-time boon
    /// is a single-use triggered ability with no source") makes the built
    /// ability gameplay-source-less while the `DelayedTrigger` entry keeps the
    /// creator. Walk mirrors `set_trigger_source_recursive` exactly.
    pub fn set_gameplay_source_recursive(&mut self, source: ObjectId) {
        self.source_id = source;
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_gameplay_source_recursive(source);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_gameplay_source_recursive(source);
        }
    }

    /// CR 603.7 + CR 603.10a: Record the battlefield-departure event this
    /// phase-delayed ability was created under on every node of its chain, so
    /// a node that resolves without a parent hand-off still carries it.
    pub fn set_creation_lookback_event_recursive(
        &mut self,
        event: &crate::types::events::GameEvent,
    ) {
        self.context.creation_lookback_event = Some(Box::new(event.clone()));
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_creation_lookback_event_recursive(event);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_creation_lookback_event_recursive(event);
        }
    }

    /// Visit every carried creation look-back event in this chain. Used by the
    /// viewer projection to redact hidden-zone records (CR 400.2).
    pub fn for_each_creation_lookback_event_mut(
        &mut self,
        f: &mut impl FnMut(&mut crate::types::events::GameEvent),
    ) {
        if let Some(event) = self.context.creation_lookback_event.as_deref_mut() {
            f(event);
        }
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.for_each_creation_lookback_event_mut(f);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.for_each_creation_lookback_event_mut(f);
        }
    }

    /// CR 608.2c: Updates the owned source projection for the current
    /// resolution segment without rebinding its source/controller or crossing
    /// an independent sequential instruction.
    pub fn update_trigger_source_context_in_resolution_segment(
        &mut self,
        source: TriggerSourceContext,
    ) {
        self.trigger_source = Some(source.clone());
        if let Some(sub) = self
            .sub_ability
            .as_mut()
            .filter(|sub| sub.sub_link == SubAbilityLink::ContinuationStep)
        {
            sub.update_trigger_source_context_in_resolution_segment(source.clone());
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.update_trigger_source_context_in_resolution_segment(source);
        }
    }

    /// CR 608.2c + CR 608.2d: Publish a name announced while applying this
    /// effect to every later instruction of the same resolution, including
    /// independent sequential siblings ("Choose a card name. Target player
    /// discards a card with that name."), which the segment-scoped update
    /// above deliberately skips.
    pub fn update_trigger_source_context_through_chain(&mut self, source: TriggerSourceContext) {
        self.trigger_source = Some(source.clone());
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.update_trigger_source_context_through_chain(source.clone());
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.update_trigger_source_context_through_chain(source);
        }
    }

    /// Install the exact definition occurrence over every continuation branch.
    pub fn set_trigger_definition_ref_recursive(&mut self, definition_ref: TriggerDefinitionRef) {
        self.trigger_definition_ref = Some(definition_ref.clone());
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_trigger_definition_ref_recursive(definition_ref.clone());
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_trigger_definition_ref_recursive(definition_ref);
        }
    }

    /// Preserve the exact named attacker through target selection and every
    /// continuation of a triggered force-block instruction. The parser carries
    /// only its grammatical selector; this stores the one event-time object
    /// identity that resolution is permitted to use.
    pub fn bind_force_block_attacker_recursive(
        &mut self,
        event_attacker: Option<ObjectIncarnationRef>,
    ) {
        if let Effect::ForceBlock {
            attacker: Some(selector),
            ..
        } = &self.effect
        {
            self.force_block_attacker = match selector {
                ForceBlockAttackerRef::Source => self
                    .trigger_source
                    .as_ref()
                    .map(|source| source.identity.reference),
                ForceBlockAttackerRef::EventSource => event_attacker,
            };
        }
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.bind_force_block_attacker_recursive(event_attacker);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.bind_force_block_attacker_recursive(event_attacker);
        }
    }

    /// Whether any resolution branch names the attacker from its triggering
    /// attack event. Such an ability must receive one singleton attack event
    /// per pending trigger; a plural event has no single "that Wolf" referent.
    pub fn has_event_source_force_block_recursive(&self) -> bool {
        matches!(
            &self.effect,
            Effect::ForceBlock {
                attacker: Some(ForceBlockAttackerRef::EventSource),
                ..
            }
        ) || self
            .sub_ability
            .as_ref()
            .is_some_and(|sub| sub.has_event_source_force_block_recursive())
            || self
                .else_ability
                .as_ref()
                .is_some_and(|else_branch| else_branch.has_event_source_force_block_recursive())
    }

    /// Bind only source-referential force-block effects at the shared stack
    /// boundary. Event-source effects are intentionally left untouched because
    /// their authority is the singleton triggering event, not the stack entry's
    /// source object.
    pub fn bind_force_block_source_recursive(&mut self, source: Option<ObjectIncarnationRef>) {
        if source.is_some()
            && matches!(
                &self.effect,
                Effect::ForceBlock {
                    attacker: Some(ForceBlockAttackerRef::Source),
                    ..
                }
            )
        {
            self.force_block_attacker = source;
        }
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.bind_force_block_source_recursive(source);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.bind_force_block_source_recursive(source);
        }
    }

    /// Yields each `MayTriggerOrigin::Definition` in this ability tree, so loop
    /// normalization can rank its identities with the other carriers (CR 400.7).
    pub(crate) fn for_each_may_trigger_origin_mut(
        &mut self,
        f: &mut impl FnMut(&mut TriggerDefinitionRef),
    ) {
        if let Some(MayTriggerOrigin::Definition { definition_ref }) =
            self.may_trigger_origin.as_mut()
        {
            f(definition_ref);
        }
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.for_each_may_trigger_origin_mut(f);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.for_each_may_trigger_origin_mut(f);
        }
    }

    /// Clears provenance that distinguishes otherwise identical triggered
    /// abilities for structural comparison. This deliberately clears the
    /// complete owned authorities together; retaining either a source context
    /// or a definition occurrence would reintroduce object-identity drift.
    pub fn clear_trigger_identity_recursive(&mut self) {
        self.source_id = ObjectId(0);
        self.source_incarnation = None;
        self.trigger_source = None;
        self.trigger_definition_ref = None;
        self.force_block_attacker = None;
        // CR 104.4b: the pin names an advancing incarnation of the triggering
        // spell (CR 400.7), so it is cleared alongside the other per-instance
        // identity fields above for the same loop-equality reason.
        self.context.triggering_spell = None;
        // CR 104.4b + CR 400.7: `normalize_for_loop` compares canonicalized
        // clones for repeated-position equality, and the all-zone incarnation
        // bump advances a pinned referent's epoch on every zone change. A
        // pinned delayed trigger riding on a zone-cycling loop would otherwise
        // carry a growing epoch into loop equality and never confirm a draw.
        //
        // CR 603.3b: the same stripping is correct for the other caller,
        // `normalize_ability_identity`, which decides whether two triggered
        // abilities are genuinely indistinguishable for auto-ordering — a
        // per-instance referent epoch is exactly the object identity that
        // comparison must ignore. Both callers operate on clones, so no
        // production pin is ever cleared. This is deliberately the opposite of
        // `inert_trigger_abilities_eq_ignoring_provenance` (`game/stack.rs`),
        // which DOES compare `target_incarnations`: there the question is
        // whether two abilities would resolve identically, and two pins at
        // different epochs would not. Same field, different questions.
        self.target_incarnations.clear();
        self.target_pins.clear();
        self.legacy_selected_target_incarnations.clear();
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.clear_trigger_identity_recursive();
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.clear_trigger_identity_recursive();
        }
    }

    /// CR 104.4b: Rewrites every `Effect::EpicCopy` handle on this chain (root,
    /// then the `sub_ability` / `else_ability` nesting
    /// `clear_trigger_identity_recursive` descends) through `rename`. Used by
    /// `GameState::normalize_for_loop` to compare in-flight upkeep copies by
    /// the snapshot their handles name rather than by position.
    pub(crate) fn rename_epic_effect_handles_recursive(
        &mut self,
        rename: &impl Fn(EpicEffectId) -> EpicEffectId,
    ) {
        if let Effect::EpicCopy { epic } = &mut self.effect {
            *epic = rename(*epic);
        }
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.rename_epic_effect_handles_recursive(rename);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.rename_epic_effect_handles_recursive(rename);
        }
    }

    pub fn trigger_source_incarnation(&self) -> Option<u64> {
        self.trigger_source
            .as_ref()
            .map(|source| source.identity.reference.incarnation)
    }

    pub fn trigger_source_card_id(&self) -> Option<CardId> {
        self.trigger_source.as_ref().map(|source| source.card_id)
    }

    /// CR 608.2h + CR 603.7a: Propagate the creating ability's chain-root
    /// target list to this ability and every continuation branch. A carrier
    /// that materializes a nested `AbilityDefinition` payload WHILE STILL
    /// WITHIN an active resolution chain (a delayed trigger's payload, a
    /// vote's per-choice effect, a coin flip's win/lose branch, a die-roll
    /// result branch, a reveal-from-hand decline) must carry the creating
    /// ability's `chain_root_targets` onto the freshly-built `ResolvedAbility`
    /// it produces — `build_resolved_from_def` gives that ability a BRAND NEW
    /// `SpellContext`, so without this, any `ObjectScope::ChainRootTarget`
    /// read nested inside the payload silently resolves against an empty
    /// list (reads as 0) once the payload actually executes. Narrow single-field
    /// analog of `set_context_recursive`, mirroring
    /// `set_source_incarnation_recursive`'s shape rather than overwriting the
    /// whole context (which would also clobber the freshly-built ability's own
    /// `ability_tag`/`chosen_x`/other per-node context fields).
    pub fn set_chain_root_targets_recursive(&mut self, chain_root_targets: Vec<TargetRef>) {
        self.context.chain_root_targets = chain_root_targets.clone();
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_chain_root_targets_recursive(chain_root_targets.clone());
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_chain_root_targets_recursive(chain_root_targets);
        }
    }

    /// CR 400.7: Propagate the source's captured incarnation to this ability
    /// and every continuation branch. Used by self-transform guards that are
    /// intentionally distinct from full triggered-source provenance.
    pub fn set_source_incarnation_recursive(&mut self, incarnation: Option<u64>) {
        self.source_incarnation = incarnation;
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_source_incarnation_recursive(incarnation);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_source_incarnation_recursive(incarnation);
        }
    }

    /// CR 400.7 + CR 602.2a: Propagate the source incarnation captured at an
    /// activation's announcement — before any cost is paid — to this ability
    /// and every continuation branch. Called only by
    /// `casting::record_activation_announcement`; stack placement and trigger
    /// creation never touch it.
    pub fn set_announced_source_incarnation_recursive(&mut self, incarnation: Option<u64>) {
        self.announced_source_incarnation = incarnation;
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_announced_source_incarnation_recursive(incarnation);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_announced_source_incarnation_recursive(incarnation);
        }
    }

    /// CR 400.7 + CR 603.7c: Pin this ability's object referents — and every
    /// continuation branch's — to the incarnations observed at delayed-trigger
    /// creation. Recursive because chain propagation copies `targets` into
    /// sub-abilities at fire time, so each link must carry the same pins.
    pub fn set_target_incarnations_recursive(&mut self, pins: Vec<ObjectIncarnationRef>) {
        self.target_incarnations = pins.clone();
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_target_incarnations_recursive(pins.clone());
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_target_incarnations_recursive(pins);
        }
    }

    /// CR 115.1a/c/d + CR 400.7 + CR 601.2c + CR 602.2b + CR 603.3d: Capture ordinary object targets at the shared
    /// announcement/selection seam, one pin per occurrence
    /// (`announce_target_pins`). Existing `target_incarnations` belong to
    /// delayed-trigger referents and are not replaced by a later assignment.
    pub fn capture_target_incarnations_recursive(
        &mut self,
        state: &crate::types::game_state::GameState,
    ) {
        self.announce_target_pins(state);
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.capture_target_incarnations_recursive(state);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.capture_target_incarnations_recursive(state);
        }
    }
    /// CR 400.7 + CR 603.7c: True when `id` may still be affected by this
    /// ability.
    ///
    /// KEYED, not positional: the same predicate applies to an id drawn from
    /// `self.targets` or from any handler's local projection — which is why the
    /// handler-direct reads share one authority. It also means a wholesale
    /// `targets` overwrite (`triggers.rs`, the Stationed/VehicleCrewed/Saddled
    /// reseed) degrades safely: the new id has no pin and passes through.
    ///
    /// FAIL-OPEN AT THE LOOKUP, NEVER AT THE PIN: `is_none_or` here means "no
    /// pin was ever recorded for this id, so this is not a pinned
    /// delayed-trigger referent and it passes through unchanged". Once a pin
    /// exists it is compared by strict equality with no escape hatch. Do not
    /// convert this to an `Option` on the element — that reintroduces the
    /// `source_is_current` anti-pattern.
    pub fn target_pin_is_current(
        &self,
        id: ObjectId,
        state: &crate::types::game_state::GameState,
    ) -> bool {
        keyed_target_pin_is_current(&self.target_incarnations, id, state)
    }

    /// CR 603.7c + CR 400.7: The subset of `targets` this ability may still
    /// affect. An object target whose creation-time pin no longer matches the
    /// live object is dropped: it left its zone (and possibly returned), so it
    /// is a new object. Player targets and unpinned object targets pass through.
    ///
    /// POSITIONAL CONSUMERS MUST NOT USE THIS. `effects::effect_object_targets`
    /// indexes `ParentTargetSlot { index }` into whatever slice it is handed,
    /// so handing it a filtered slice would RENUMBER the declared slots.
    /// Callers that may see a `ParentTargetSlot` filter pass the raw
    /// `&self.targets` instead — see `sacrifice.rs`.
    pub fn live_object_targets(
        &self,
        state: &crate::types::game_state::GameState,
    ) -> Vec<TargetRef> {
        self.live_object_targets_excluding(state, None)
    }

    /// Live announced targets minus the announced entry at `excluded` (a
    /// separately announced quantity slot serving the magnitude, not receipt —
    /// CR 115.1 + CR 601.2c). `None` is exactly [`Self::live_object_targets`].
    pub fn live_object_targets_excluding(
        &self,
        state: &crate::types::game_state::GameState,
        excluded: Option<usize>,
    ) -> Vec<TargetRef> {
        self.targets
            .iter()
            .enumerate()
            .filter(|(index, _)| Some(*index) != excluded)
            .map(|(_, target)| target)
            .filter(|target| match target {
                TargetRef::Object(id) => self.target_pin_is_current(*id, state),
                TargetRef::Player(_) => true,
            })
            .cloned()
            .collect()
    }

    /// CR 603.7c: True when this ability pinned at least one object referent and
    /// every one of them has gone stale. Distinguishes "the referent became a
    /// new object" (the effect must do nothing) from "no referent was ever
    /// chosen" (which legitimately falls back elsewhere).
    pub fn pinned_object_targets_all_stale(
        &self,
        state: &crate::types::game_state::GameState,
    ) -> bool {
        !self.target_incarnations.is_empty()
            && self
                .targets
                .iter()
                .any(|t| matches!(t, TargetRef::Object(_)))
            && !self.targets.iter().any(
                |t| matches!(t, TargetRef::Object(id) if self.target_pin_is_current(*id, state)),
            )
    }

    /// Test-only fixture helper for a triggered ability whose source has
    /// already left the object map. Production code must capture a real
    /// [`TriggerSourceContext`] at collection time instead.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_test_trigger_source_recursive(&mut self, incarnation: u64, card_id: CardId) {
        let mut source = crate::game::game_object::GameObject::new(
            self.source_id,
            card_id,
            self.controller,
            "test trigger source".to_string(),
            Zone::Battlefield,
        );
        source.controller = self.controller;
        source.incarnation = incarnation;
        let context = source
            .snapshot_for_zone_change(source.id, Some(Zone::Battlefield), Zone::Battlefield)
            .trigger_source_context
            .expect("test source snapshot always contains trigger context");
        self.set_trigger_source_recursive(context);
    }

    /// CR 701.27f: Propagate the source's captured transformation generation
    /// through every branch of an activated or triggered ability.
    pub fn set_source_transformation_count_recursive(&mut self, count: Option<u32>) {
        self.context.source_transformation_count = count;
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_source_transformation_count_recursive(count);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_source_transformation_count_recursive(count);
        }
    }

    /// CR 602.2a + CR 607.1: Propagate the announced activated ability's
    /// provenance through every branch of the ability.
    pub fn set_source_ability_provenance_recursive(
        &mut self,
        provenance: Option<AbilityProvenance>,
    ) {
        self.context.source_ability_provenance = provenance;
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_source_ability_provenance_recursive(provenance);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_source_ability_provenance_recursive(provenance);
        }
    }

    /// CR 607.1 + CR 113.7a: single read authority for the provenance of the
    /// ability this stack object came from on its source — the activated fact
    /// latched at announcement, else the trigger definition occurrence it was
    /// created from. `None` when neither is attributable.
    pub fn source_ability_provenance(&self) -> Option<AbilityProvenance> {
        self.context.source_ability_provenance.or_else(|| {
            self.trigger_definition_ref
                .as_ref()
                .and_then(|definition| definition.occurrence.provenance())
        })
    }
    /// CR 400.7: True when the triggered source matches the object in its captured
    /// `expected_zone` at the captured incarnation.
    fn source_is_current_via_zone_match(
        &self,
        state: &crate::types::game_state::GameState,
        source: &TriggerSourceContext,
    ) -> bool {
        let captured = source.identity.reference.incarnation;
        let current = state.objects.get(&self.source_id).and_then(|object| {
            (object.zone == source.identity.expected_zone).then_some(object.incarnation)
        });
        current == Some(captured)
    }

    /// CR 400.7j (+ CR 400.7g/h cast hop): a source that moved (possibly twice)
    /// as part of THIS resolution relatches to its current incarnation. Bind to
    /// `original_stamp` so only the ability that captured the pre-move identity
    /// relatches — a stale-stamped delayed trigger for the same object cannot
    /// ride the record.
    fn source_is_current_via_relatch(
        &self,
        state: &crate::types::game_state::GameState,
        source: &TriggerSourceContext,
    ) -> bool {
        let captured = source.identity.reference.incarnation;
        let current_incarnation = state
            .objects
            .get(&self.source_id)
            .map(|object| object.incarnation);
        matches!(state.resolution_source_relatch, Some(r)
            if r.object_id == self.source_id
                && r.original_stamp == captured
                && Some(r.current_incarnation) == current_incarnation)
    }

    /// CR 113.7 + CR 400.7: the ability's source as the object it was when the ability was
    /// created; a spell, which captures no incarnation, is its current stack object.
    pub fn source_ref(
        &self,
        state: &crate::types::game_state::GameState,
    ) -> Option<crate::types::identifiers::ObjectIncarnationRef> {
        let incarnation = self.source_incarnation.or_else(|| {
            state
                .objects
                .get(&self.source_id)
                .map(|obj| obj.incarnation)
        })?;
        Some(crate::types::identifiers::ObjectIncarnationRef::of(
            self.source_id,
            incarnation,
        ))
    }

    /// CR 113.7a + CR 400.7 + CR 603.7d + CR 603.7e: the source object an effect this ability creates refers to: the object a self-reference binds to while the ability can find it (CR 400.7e/f/j); else the source named by the ability's own trigger provenance (`trigger_source`, the event-time source of a triggered ability and, for a delayed triggered ability, the spell that created it (CR 603.7d) or the source of the ability that created it (CR 603.7e)), which stack placement never restamps; else the incarnation the source had when the ability was put on the stack. A source that has since left its zone is answered by its last known information (CR 608.2h).
    pub fn effect_source_ref(
        &self,
        state: &crate::types::game_state::GameState,
    ) -> Option<crate::types::identifiers::ObjectIncarnationRef> {
        self.self_ref_binding(state)
            .and_then(|id| state.objects.get(&id))
            .map(crate::types::identifiers::ObjectIncarnationRef::from_object)
            .or_else(|| {
                self.trigger_source
                    .as_ref()
                    .filter(|source| source.identity.reference.object_id == self.source_id)
                    .map(|source| source.identity.reference)
            })
            .or_else(|| self.source_ref(state))
    }

    /// CR 400.7: True if the ability's source is still the same object instance it
    /// was when the ability was created. Full triggered-source provenance takes
    /// precedence; activated and delayed self-transform abilities fall back to
    /// their captured source incarnation.
    pub fn source_is_current(&self, state: &crate::types::game_state::GameState) -> bool {
        match self.trigger_source.as_ref() {
            None => {
                let current_incarnation = state
                    .objects
                    .get(&self.source_id)
                    .map(|object| object.incarnation);
                self.source_incarnation.is_none_or(|captured| {
                    current_incarnation == Some(captured)
                        || matches!(state.resolution_source_relatch, Some(r)
                            if r.object_id == self.source_id
                                && r.original_stamp == captured
                                && Some(r.current_incarnation) == current_incarnation)
                })
            }
            Some(source) => {
                self.source_is_current_via_zone_match(state, source)
                    || self.source_is_current_via_relatch(state, source)
            }
        }
    }

    /// CR 400.7e: True when `SelfRef` may bind to the immediate successor of the
    /// source's own recorded zone-change event (a dies trigger's "it" in the
    /// graveyard). The per-turn record check is load-bearing: if the object has
    /// moved again after the triggering event, it is no longer that immediate
    /// successor and a same-id return must not be rebound as the old source.
    fn self_ref_own_departure_successor(
        &self,
        state: &crate::types::game_state::GameState,
    ) -> bool {
        if self.trigger_source.is_none() {
            return true;
        }
        state
            .objects
            .get(&self.source_id)
            .is_some_and(|object| self.is_own_departure_successor_in(state, object.zone, None))
    }

    /// CR 400.7e: True when the source, sitting in public `zone`, is the
    /// immediate successor of its own triggering zone change: the triggering
    /// event moved this exact captured identity into `zone`, and no later zone
    /// change of the same storage id has been recorded. A trigger cannot find
    /// the new object in a hidden zone. Callers pass the zone the source
    /// occupies, or, for a move still being applied, the zone it is leaving
    /// plus that move's departing incarnation: an incarnation departs only
    /// once, so a record carrying it is that in-flight move, not an earlier one.
    pub(crate) fn is_own_departure_successor_in(
        &self,
        state: &crate::types::game_state::GameState,
        zone: Zone,
        in_flight_departure: Option<u64>,
    ) -> bool {
        let Some(source) = self.trigger_source.as_ref() else {
            return false;
        };
        let Some(crate::types::GameEvent::ZoneChanged {
            object_id,
            to,
            record,
            ..
        }) = state.current_trigger_event.as_ref()
        else {
            return false;
        };
        if *object_id != self.source_id
            || record.trigger_source_context().is_none_or(|event_source| {
                event_source.identity.reference != source.identity.reference
            })
            || zone != *to
            || !zone.is_public()
        {
            return false;
        }

        let Some(recorded_event) = state
            .zone_changes_this_turn
            .get(record.turn_zone_change_index)
        else {
            return false;
        };
        recorded_event
            .trigger_source_context()
            .is_some_and(|event_source| {
                event_source.identity.reference == source.identity.reference
            })
            && state
                .zone_changes_this_turn
                .iter()
                .skip(record.turn_zone_change_index + 1)
                .all(|later| {
                    later.object_id != self.source_id
                        || in_flight_departure.is_some_and(|departing| {
                            later.trigger_source_context().is_some_and(|context| {
                                context.identity.reference.incarnation == departing
                            })
                        })
                })
    }

    /// CR 400.7e: True when an off-battlefield zone-match would mis-latch SelfRef
    /// because the source co-departed into the expected public zone during this
    /// trigger event (issue #6427). Provenance requires `ZoneChangeRecord.co_departed`,
    /// not temporal coincidence with another object's zone change.
    fn is_co_departure_mislatch_zone_match(
        &self,
        state: &crate::types::game_state::GameState,
        source: &TriggerSourceContext,
    ) -> bool {
        if source.identity.expected_zone == Zone::Battlefield {
            return false;
        }
        if !self.source_is_current_via_zone_match(state, source) {
            return false;
        }
        if self.source_is_current_via_relatch(state, source) {
            return false;
        }
        let Some(event) = state.current_trigger_event.as_ref() else {
            return false;
        };
        !crate::types::events::source_was_not_co_departed_into_zone(
            event,
            self.source_id,
            source.identity.expected_zone,
        )
    }

    /// CR 400.7 + CR 608.2c: The object an untargeted self-reference ("~", "this
    /// creature", or an implicit source subject) binds to at resolution, or
    /// `None` when there is none. The single authority for effect resolvers
    /// that bind `SelfRef` (or an implicit-source filter) without going through
    /// `targeting::resolved_object_ids_for_filter`.
    ///
    /// The source is bound only while this ability can find it
    /// (`finds_source_successor`): it is still the object it was when the
    /// ability was created, or it is the new object one of CR 400.7's
    /// exceptions lets the ability find — the zone a zone-change trigger moved
    /// it to (CR 400.7e), an Aura that went to the graveyard with the
    /// permanent it enchanted (CR 400.7f), or the object the ability's own
    /// resolution moved it to (CR 400.7j, read through
    /// `resolution_source_relatch`: Bogardan Phoenix's dies trigger returns the
    /// card and then puts the death counter "on it"). A source that left and
    /// returned any other way is a new object, so the instruction has no
    /// referent: Carrion Feeder sacrificed to its own ability and returned by
    /// Supernatural Stamina; a creature blinked in response to its own
    /// activation; Cinder Wall blinked after its blocks trigger created the
    /// end-of-combat "destroy it" (CR 603.7c). A cost that moves the source
    /// (Suspend, Plot) is paid before the incarnation is captured, which is CR
    /// 400.7j as well — and only that object, not what it becomes after moving
    /// again.
    ///
    /// A forward-result continuation whose `source_id` was rebound by
    /// `rebind_child_to_forwarded_objects` to an object its producer moved
    /// keeps that rebound binding unchanged here; its trigger provenance still
    /// names the original source, so the provenance check cannot judge it. This
    /// preserves the existing forward-result binding and is not a rules claim:
    /// a rebound child whose SelfRef prints the source's own name (General Leo
    /// Cristophe) is mis-bound upstream, and that is a separate defect.
    pub fn self_ref_binding(
        &self,
        state: &crate::types::game_state::GameState,
    ) -> Option<crate::types::identifiers::ObjectId> {
        let rebound_to_moved_object = self
            .trigger_source
            .as_ref()
            .is_some_and(|source| source.identity.reference.object_id != self.source_id);
        (rebound_to_moved_object || self.finds_source_successor(state)).then_some(self.source_id)
    }

    /// Returns whether a self-reference can resolve to the source's current
    /// object. A normal triggered source must still be its exact captured
    /// incarnation. The one exception is the immediate successor of the
    /// source's own recorded zone-change event: a dies trigger's "it" refers
    /// to the card now in the graveyard, while its source facts remain LKI.
    ///
    /// Off-battlefield zone-match currency alone (co-departure mis-latch) does
    /// not suffice — CR 400.7e own-departure successor proof is required.
    ///
    /// CR 400.7 + CR 603.6c + CR 704.5m: A further exception is an Aura that
    /// latched its AttachedTo dies trigger while still on the battlefield
    /// (Necrotic Plague), then moved to the graveyard by SBAs before
    /// resolution. `SelfRef` return-from-GY must bind that graveyard object
    /// without rewriting `trigger_source` (which would flip `source_read` to
    /// ExactLive and drop LKI attachments/counters for other dies triggers).
    pub fn self_ref_is_current(&self, state: &crate::types::game_state::GameState) -> bool {
        if self.source_is_current(state) {
            // CR 400.7e: co-departure mis-latch — zone-match currency alone cannot
            // bind SelfRef when co_departed proves the source arrived with another
            // object's departure; require own-departure successor proof instead.
            if let Some(source) = self.trigger_source.as_ref() {
                if self.is_co_departure_mislatch_zone_match(state, source) {
                    return self.self_ref_own_departure_successor(state);
                }
            }
            return true;
        }
        // CR 400.7: without trigger provenance the captured `source_incarnation`
        // is the whole identity, and `source_is_current` already said it is
        // stale (the relatch for a self-move during this resolution is read
        // there). The source left and returned — or never came back — so "~"
        // names an object that no longer exists.
        let Some(_source) = self.trigger_source.as_ref() else {
            return false;
        };
        self.self_ref_own_departure_successor(state)
            || self.self_ref_post_sba_graveyard_return(state)
    }

    /// CR 400.7 + CR 704.5m: True when this ability returns `SelfRef` from the
    /// graveyard and the source is now there after an SBA move that followed a
    /// battlefield latch (AttachedTo dies → Aura to GY before resolution).
    ///
    /// Provenance mirrors `self_ref_own_departure_successor`: the BF→GY record
    /// must carry the captured source identity (incarnation), and no later
    /// same-id zone change may have occurred — a same-id blink/re-entry must
    /// not be rebound as the old source.
    fn self_ref_post_sba_graveyard_return(
        &self,
        state: &crate::types::game_state::GameState,
    ) -> bool {
        let Some(source) = self.trigger_source.as_ref() else {
            return false;
        };
        if source.identity.expected_zone != crate::types::zones::Zone::Battlefield {
            return false;
        }
        if source.identity.reference.object_id != self.source_id {
            return false;
        }
        if !self.effect_returns_self_ref_from_graveyard() {
            return false;
        }
        if !state
            .objects
            .get(&self.source_id)
            .is_some_and(|object| object.zone == crate::types::zones::Zone::Graveyard)
        {
            return false;
        }

        self.source_latest_move_is_latched_battlefield_to_graveyard(state, source)
    }

    /// CR 400.7: True when the source's most recent zone change this turn is
    /// the battlefield-to-graveyard move of exactly the latched incarnation, so
    /// the graveyard card is that departure's immediate successor. A same-id
    /// blink or re-entry after that move is a newer object and fails.
    fn source_latest_move_is_latched_battlefield_to_graveyard(
        &self,
        state: &crate::types::game_state::GameState,
        source: &TriggerSourceContext,
    ) -> bool {
        // CR 400.7e: Find the BF→GY departure whose captured identity matches
        // the ability's latched stamp, then require it is still the latest
        // same-id zone change this turn.
        let Some(departure_index) = state
            .zone_changes_this_turn
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, record)| {
                (record.object_id == self.source_id
                    && record.from_zone == Some(crate::types::zones::Zone::Battlefield)
                    && record.to_zone == crate::types::zones::Zone::Graveyard
                    && record.trigger_source_context().is_some_and(|event_source| {
                        event_source.identity.reference == source.identity.reference
                    }))
                .then_some(index)
            })
        else {
            return false;
        };

        state
            .zone_changes_this_turn
            .iter()
            .skip(departure_index + 1)
            .all(|later| later.object_id != self.source_id)
    }

    /// CR 400.7f: True when this ability triggered on the departure of a
    /// permanent its source was attached to, and the source (an Aura latched
    /// on the battlefield) is now the graveyard card it became by that same
    /// departure or by the CR 704.5m state-based action that followed it. Such
    /// an ability "can find the new object" the Aura became; no other move of
    /// the source can be found (CR 400.7). The Aura fact is the host departure
    /// record's attachment kind, captured as the source was attached when the
    /// host left: CR 400.7f reaches "each Aura enchanting that permanent", so
    /// an Equipment (CR 301.5) attached to it is not found.
    fn finds_aura_after_its_host_left(&self, state: &crate::types::game_state::GameState) -> bool {
        let Some(source) = self.trigger_source.as_ref() else {
            return false;
        };
        if source.identity.expected_zone != Zone::Battlefield
            || source.identity.reference.object_id != self.source_id
            || !state
                .objects
                .get(&self.source_id)
                .is_some_and(|object| object.zone == Zone::Graveyard)
        {
            return false;
        }
        let Some(crate::types::GameEvent::ZoneChanged {
            object_id, record, ..
        }) = state.current_trigger_event.as_ref()
        else {
            return false;
        };
        *object_id != self.source_id
            && record.from_zone == Some(Zone::Battlefield)
            && record.attachments.iter().any(|attachment| {
                attachment.kind == AttachmentKind::Aura
                    && attachment.identity == Some(source.identity.reference)
            })
            && self.source_latest_move_is_latched_battlefield_to_graveyard(state, source)
    }

    /// CR 400.7e + CR 400.7f + CR 400.7j: True when this ability can find the
    /// new object its source became after the source changed zones since the
    /// ability was created — the exceptions to CR 400.7's rule that a moved
    /// object is a new object with no relation to its previous existence. A
    /// source that has not moved is trivially found. A delayed triggered
    /// ability this ability creates has the same source (CR 603.7e), so this
    /// is also whether that delayed ability's source binding may follow the
    /// source to its current zone.
    pub(crate) fn finds_source_successor(
        &self,
        state: &crate::types::game_state::GameState,
    ) -> bool {
        self.self_ref_is_current(state) || self.finds_aura_after_its_host_left(state)
    }

    /// True when this ability (or a nested sub) is a `ChangeZone` of `SelfRef`
    /// whose origin is the graveyard.
    fn effect_returns_self_ref_from_graveyard(&self) -> bool {
        fn change_zone_self_from_gy(effect: &Effect) -> bool {
            matches!(
                effect,
                Effect::ChangeZone {
                    origin: Some(crate::types::zones::Zone::Graveyard),
                    target: TargetFilter::SelfRef,
                    ..
                }
            )
        }
        fn walk(ability: &ResolvedAbility) -> bool {
            change_zone_self_from_gy(&ability.effect)
                || ability.sub_ability.as_deref().is_some_and(walk)
                || ability.else_ability.as_deref().is_some_and(walk)
        }
        walk(self)
    }

    /// CR 608.2c: Bind a tracked-set sentinel (`TrackedSetId(0)`) to a CONCRETE
    /// tracked-set id across this ability's effect and every sub/else branch.
    ///
    /// Used when a *cost* (not an effect) publishes the "those exiled cards" set:
    /// the set must survive the activation→resolution gap, during which
    /// intervening instant-speed effects can publish their own tracked sets and
    /// clobber `chain_tracked_set_id` / the latest-id heuristic (and `state.
    /// chain_tracked_set_id` is reset to `None` at depth-0 resolution anyway).
    /// Rewriting the sentinel to the concrete id in the resolving ability at
    /// cost-payment time — before the ability is pushed onto the stack — makes
    /// the binding immune to that gap (Baron Helmut Zemo's Boast).
    pub fn bind_tracked_set_sentinel_recursive(
        &mut self,
        concrete: crate::types::identifiers::TrackedSetId,
    ) {
        self.effect.bind_tracked_set_sentinel(concrete);
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.bind_tracked_set_sentinel_recursive(concrete);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.bind_tracked_set_sentinel_recursive(concrete);
        }
    }

    /// Propagate a chosen X value to this ability and every sub/else branch.
    /// CR 107.1b: X is one value per cast — the same for all effects produced.
    pub fn set_chosen_x_recursive(&mut self, value: u32) {
        self.chosen_x = Some(value);
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_chosen_x_recursive(value);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_chosen_x_recursive(value);
        }
    }

    /// CR 117.1 + CR 400.7j + CR 608.2k: Stamp the cost-paid object across this
    /// ability and every sub/else branch. Captured at cost-payment time (before
    /// the object leaves its zone) and read by cost-paid-object refs during
    /// resolution.
    pub fn set_cost_paid_object_recursive(&mut self, snapshot: CostPaidObjectSnapshot) {
        self.cost_paid_object = Some(snapshot.clone());
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_cost_paid_object_recursive(snapshot.clone());
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_cost_paid_object_recursive(snapshot);
        }
    }

    /// CR 400.7 + CR 400.7j + CR 608.2k + CR 701.9c: SETTLE this ability's (and
    /// every sub/else branch's) cost-paid provenance once ONE cost-move seam's
    /// own object moves are complete. Mirrors `set_cost_paid_object_recursive`'s
    /// traversal.
    ///
    /// Named for the job it actually does — it is not a bare re-pin. Settlement
    /// is SCOPED and TYPED, and both parameters are load-bearing:
    ///
    /// * `moved` — the ids THIS seam just moved. A cast can pay several
    ///   non-mana components (discard a card, exile a card, then sacrifice a
    ///   permanent at mana commit); every one of them calls this traversal, and
    ///   the plural authority accumulates ALL of their records. A record whose
    ///   object is not in `moved` belongs to a DIFFERENT component that already
    ///   completed and settled its own move, so it is skipped entirely: settling
    ///   it again would judge it against a move that was never its own — either
    ///   re-pinning across a later, unrelated zone change (CR 400.7 says that is
    ///   a new object) or demoting a record that legitimately settled public.
    ///   Mirrors the pre-move counterpart
    ///   [`Self::refresh_cost_paid_capture_recursive`], which is scoped the same
    ///   way for the same reason.
    /// * `outcome` — WHICH cost action performed those moves, because that
    ///   decides the hidden-destination rule. See
    ///   [`CostPaidObjectRecord::settle_after_cost_move`] for the per-record
    ///   matrix; in short, a public delivery re-pins under CR 400.7j either way,
    ///   a hidden [`CostMoveOutcome::Discard`] demotes to
    ///   `CostPaidObjectRecord::MembershipOnly` under CR 701.9c, and a hidden
    ///   [`CostMoveOutcome::Relocation`] is left alone — CR 701.9c is
    ///   discard-scoped, and CR 608.2h makes the captured pre-move `lki` the
    ///   correct reading for a sacrificed/exiled/returned object, so demoting
    ///   would destroy the very information the ability's effects need. Its pin
    ///   stays stale, so the LIVE reference still fails closed.
    ///
    /// The demotion has to live HERE, post-move, because capture has to stay
    /// pre-move (the `lki` must record pre-move characteristics — CR 608.2h)
    /// and the destination is not known until the replacement pipeline
    /// (CR 616.1) has run. Routing it through the traversal every cost-move
    /// seam already calls is what makes deterministic discard (immediate and
    /// resumed) and random discard classify IDENTICALLY: changing how the
    /// discarded card is chosen does not change the authority rule.
    /// `commit_random_discard_cost_picks` additionally classifies at
    /// publication time; that is belt-and-braces (it prevents even a transient
    /// `Captured` for a hidden card) and agrees with this rule by construction.
    ///
    /// The SINGULAR `cost_paid_object` referent keeps a plain, UNCONDITIONAL
    /// re-pin here: unscoped by `moved` and unaffected by `outcome`. That is
    /// this traversal's whole singular job and it is deliberately narrow — a
    /// re-pin moves the INCARNATION epoch, never the characteristics. Refreshing
    /// the singular's CHARACTERISTICS across a deferred payment belongs to the
    /// pre-move counterpart [`Self::refresh_cost_paid_capture_recursive`],
    /// which now covers both carriers under one scoping rule; settling is not
    /// re-capturing, and the two must not be merged.
    ///
    /// This is the SINGLE traversal authority for cost-paid provenance
    /// settlement: it covers the singular `cost_paid_object` referent AND every
    /// entry of the plural `cost_paid_objects` authority, so a cost-payment
    /// seam that already calls it cannot forget one of the two. Do not add a
    /// parallel traversal. Its pre-move counterpart — the only other traversal
    /// in this family — is
    /// [`Self::refresh_cost_paid_capture_recursive`], whose job is capture, not
    /// settlement.
    ///
    /// CR 400.7: a migrated `CostPaidObjectRecord::MembershipOnly` entry has no
    /// pin to refresh and must never acquire one, so the per-record method
    /// no-ops for it. The traversal stays the single authority either way.
    pub fn settle_cost_paid_provenance_recursive(
        &mut self,
        state: &crate::types::game_state::GameState,
        moved: &[ObjectId],
        outcome: CostMoveOutcome,
    ) {
        if let Some(snapshot) = self.cost_paid_object.as_mut() {
            snapshot.repin_to_current_incarnation(state);
        }
        for record in self.cost_paid_objects.iter_mut() {
            if moved.contains(&record.object_id()) {
                record.settle_after_cost_move(state, outcome);
            }
        }
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.settle_cost_paid_provenance_recursive(state, moved, outcome);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.settle_cost_paid_provenance_recursive(state, moved, outcome);
        }
    }

    /// CR 608.2h + CR 601.2h: RE-CAPTURE this ability's cost-paid provenance
    /// for `object_ids` from the live objects, immediately BEFORE the payment
    /// seam that moves them. Covers BOTH carriers — the singular
    /// `cost_paid_object` referent and every `Captured` entry of the plural
    /// `cost_paid_objects` authority.
    ///
    /// The pre-move counterpart of
    /// [`Self::settle_cost_paid_provenance_recursive`], and the only other
    /// traversal in this family. Their jobs are disjoint and both are needed:
    /// this one is CAPTURE (run before the move, it decides WHAT the record
    /// says); that one is SETTLEMENT (run after the move, it decides whether
    /// the record may still say it and which incarnation it names).
    ///
    /// Needed because one payment defers its moves past the seam that published
    /// the record: a spell sacrifice selected before the mana window
    /// (`handle_sacrifice_for_cost`'s deferral branch) is paid later, at mana
    /// commit, and the permanent can be tapped for that very mana in between
    /// (CR 601.2g puts that window between selection and payment). CR 608.2h
    /// calls for the object's last known information — its state as it most
    /// recently existed — so the record must be re-captured at the ACTUAL
    /// payment seam rather than frozen at selection.
    ///
    /// Scoped by `object_ids` on purpose, and IDENTICALLY for both carriers:
    /// only the objects THIS deferred payment is about to move may be
    /// re-captured. A referent outside that set belongs to a cost component
    /// that already completed its own move and settled, and re-capturing it
    /// would overwrite a correct post-move LKI with a live read.
    /// `MembershipOnly` records are never touched — they have no snapshot and
    /// must never acquire one (CR 400.7).
    ///
    /// The SINGULAR referent is included because the deferral makes its
    /// selection-time freeze actively WRONG, not merely historical: it is the
    /// value `ObjectScope::CostPaidObject` quantity resolution reads
    /// (`game/quantity.rs`) and the value `triggers.rs` copies onto a resolved
    /// ETB trigger via the spell's `cast_cost_paid_object`, so an unrefreshed
    /// singular reports selection-time state to effects and triggers that
    /// CR 608.2h entitles to the state immediately before the actual payment.
    /// The settlement traversal only RE-PINS the singular (incarnation), which
    /// cannot fix stale characteristics — capture is this traversal's job.
    ///
    /// The spell object's `cast_cost_paid_object` carrier is the one cost-paid
    /// carrier this traversal cannot reach: it lives on `GameObject`, not on
    /// `ResolvedAbility`. Its deferred-payment refresh is performed at the two
    /// finalizer call sites (`game/casting_costs.rs`) under the same scoping,
    /// through the same [`CostPaidObjectSnapshot::recapture_from_live`] seam.
    pub fn refresh_cost_paid_capture_recursive(
        &mut self,
        state: &crate::types::game_state::GameState,
        object_ids: &[ObjectId],
    ) {
        if object_ids.is_empty() {
            return;
        }
        if let Some(snapshot) = self.cost_paid_object.as_mut() {
            if object_ids.contains(&snapshot.object_id) {
                snapshot.refresh_capture_from_live(state);
            }
        }
        for record in self.cost_paid_objects.iter_mut() {
            if object_ids.contains(&record.object_id()) {
                record.refresh_capture_from_live(state);
            }
        }
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.refresh_cost_paid_capture_recursive(state, object_ids);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.refresh_cost_paid_capture_recursive(state, object_ids);
        }
    }

    /// CR 106.1b + CR 400.7 + CR 602.2b (issue #6504): Stamp this activation's
    /// noted-mana-payment snapshot across this ability and every sub/else
    /// branch — mirrors `set_cost_paid_object_recursive`. Necessary because
    /// `Effect::NoteManaSpent` is typically chained as a `sub_ability` (e.g.
    /// Jeweled Amulet: `PutCounter { sub_ability: NoteManaSpent }`), which
    /// resolves as its OWN separate `ResolvedAbility` node distinct from the
    /// top-level ability `push_ability_entry` captured the payment onto;
    /// without this recursive stamp the sub-ability would read the field's
    /// `None` default and silently note nothing.
    pub fn set_noted_mana_payment_recursive(&mut self, payment: NotedManaPayment) {
        self.noted_mana_payment = Some(payment.clone());
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_noted_mana_payment_recursive(payment.clone());
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_noted_mana_payment_recursive(payment);
        }
    }

    /// CR 707.10 (issue #6504): Clear a noted-mana-payment snapshot across
    /// this ability and every sub/else branch. A copy of an activated
    /// ability is not itself activated, so it never paid a mana cost — a
    /// naive struct clone otherwise carries the ORIGINAL activation's
    /// payment along, and `Effect::NoteManaSpent` resolving on the copy
    /// would falsely note mana the copy never spent. Called when normalizing
    /// a copied activated/triggered ability (`preserve_ability_copy_source_
    /// recursive`), mirroring `set_noted_mana_payment_recursive`'s recursion
    /// shape in reverse.
    pub fn clear_noted_mana_payment_recursive(&mut self) {
        self.noted_mana_payment = None;
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.clear_noted_mana_payment_recursive();
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.clear_noted_mana_payment_recursive();
        }
    }

    /// CR 601.2h + CR 602.2b (issue #4948): Record EVERY object
    /// paid as part of this ability's own cost (mirrors
    /// `set_cost_paid_object_recursive`'s recursion into `sub_ability` /
    /// `else_ability`, but accumulates every referent instead of overwriting a
    /// single one). Call this alongside — not instead of —
    /// `set_cost_paid_object_recursive` at every non-self
    /// Sacrifice/Discard/Exile cost-payment site; the singular field keeps
    /// its own resolution-time referent semantics.
    ///
    /// CR 400.7: the appended entries are full snapshots captured BEFORE the
    /// cost's own move, so [`Self::settle_cost_paid_provenance_recursive`] must
    /// run once that seam's moves complete, scoped to the ids this seam moved
    /// and tagged with its [`CostMoveOutcome`] — it settles these entries
    /// through the same single traversal that repins `cost_paid_object`.
    ///
    /// CR 400.7j: this snapshot-taking form is the INTENDED-public-destination
    /// front door — every one of its callers pays a cost whose own move AIMS at
    /// a public zone (discard → graveyard, sacrifice → graveyard, exile →
    /// exile). It is NOT a promise that the move lands there: a replacement
    /// effect (CR 616.1) can redirect any of them into a hidden zone. What
    /// happens then depends on the cost action, not on the destination alone:
    /// CR 701.9c makes a DISCARDED card's characteristics undefined, so that
    /// `Captured` entry is PROVISIONAL and the post-move settlement demotes it;
    /// a sacrificed, exiled, or returned object keeps its captured entry,
    /// because CR 608.2h makes the captured pre-move `lki` its last known
    /// information and the stale pin already refuses the live reference.
    /// A payment that already KNOWS the per-object outcome at publication time
    /// may classify up front and call [`Self::add_cost_paid_records_recursive`]
    /// directly (`commit_random_discard_cost_picks` does, so a hidden result is
    /// never even transiently `Captured`); settlement then agrees with it.
    pub fn add_cost_paid_objects_recursive(&mut self, snapshots: &[CostPaidObjectSnapshot]) {
        let records = snapshots
            .iter()
            .cloned()
            .map(CostPaidObjectRecord::Captured)
            .collect::<Vec<_>>();
        self.add_cost_paid_records_recursive(&records);
    }

    /// CR 601.2h + CR 602.2b + CR 400.7j: the records-taking primitive behind
    /// [`Self::add_cost_paid_objects_recursive`] and the SINGLE traversal
    /// authority for appending to the plural cost-paid provenance.
    ///
    /// Exists because the record VARIANT is a per-object question that ONE
    /// payment seam can already answer at publication time: a random discard
    /// cost (CR 701.9b) whose card a replacement effect redirected into a
    /// hidden zone is still a card this cost moved (exact membership,
    /// CR 601.2c), but CR 701.9c leaves its characteristics undefined and
    /// CR 400.7j licenses finding only an object the cost moved to a PUBLIC
    /// zone — so that entry must be `CostPaidObjectRecord::MembershipOnly`
    /// and must never acquire a snapshot or a live reference. Classifying at the
    /// call site and appending through here keeps that decision out of the
    /// traversal, so there is still only one recursion for a future sub/else
    /// branch to be forgotten by.
    ///
    /// This is belt-and-braces, not the only enforcement: a DISCARD seam's
    /// post-move [`Self::settle_cost_paid_provenance_recursive`] call
    /// ([`CostMoveOutcome::Discard`]) applies the SAME CR 701.9c rule to
    /// whatever was published, so a discard seam that cannot classify up front
    /// (the deterministic discard paths, which run their moves through the
    /// replacement pipeline after publishing) is covered too. Classifying
    /// here additionally prevents even a TRANSIENT `Captured` record for a card
    /// whose characteristics are undefined.
    pub fn add_cost_paid_records_recursive(&mut self, records: &[CostPaidObjectRecord]) {
        self.cost_paid_objects.extend_from_slice(records);
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.add_cost_paid_records_recursive(records);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.add_cost_paid_records_recursive(records);
        }
    }

    /// Stamp an object selected by a previous effect in this same resolution
    /// across the continuation chain. Used by sacrifice-as-effect patterns
    /// whose later instructions reference "that creature" after it has left
    /// the battlefield.
    pub fn set_effect_context_object_recursive(&mut self, snapshot: CostPaidObjectSnapshot) {
        self.effect_context_object = Some(snapshot.clone());
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_effect_context_object_recursive(snapshot.clone());
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_effect_context_object_recursive(snapshot);
        }
    }

    /// CR 701.9a + CR 608.2c: Stamp one completed discard result onto exactly
    /// the deferred direct child. Descendants and alternate branches are
    /// explicitly cleared so this one contingent fact cannot leak past its
    /// immediate consumer.
    pub fn set_direct_discard_result_for_immediate_node(&mut self, result: DiscardedCardResult) {
        self.context.direct_discard_result = Some(result);
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.clear_direct_discard_result_recursive();
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.clear_direct_discard_result_recursive();
        }
    }

    fn clear_direct_discard_result_recursive(&mut self) {
        self.context.direct_discard_result = None;
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.clear_direct_discard_result_recursive();
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.clear_direct_discard_result_recursive();
        }
    }

    /// CR 608.2c: Stamp one completed effect result onto exactly the deferred
    /// direct child. Descendants and alternate branches are cleared so the
    /// result cannot leak beyond its typed completion node.
    pub fn set_prior_effect_result_for_immediate_node(&mut self, result: EffectResolutionResult) {
        self.context.prior_effect_result = Some(result);
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.clear_prior_effect_result_recursive();
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.clear_prior_effect_result_recursive();
        }
    }

    pub(crate) fn clear_prior_effect_result_recursive(&mut self) {
        self.context.prior_effect_result = None;
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.clear_prior_effect_result_recursive();
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.clear_prior_effect_result_recursive();
        }
    }

    /// CR 701.47c + CR 608.2c: Stamp the Army chosen by an amass instruction
    /// across every continuation branch in this resolution.
    pub fn set_amassed_army_object_recursive(&mut self, snapshot: CostPaidObjectSnapshot) {
        self.amassed_army_object = Some(snapshot.clone());
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_amassed_army_object_recursive(snapshot.clone());
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_amassed_army_object_recursive(snapshot);
        }
    }

    /// Bind the current player for a `player_scope` resolution pass across the
    /// local ability chain. This intentionally does not change `controller`.
    pub fn set_scoped_player_recursive(&mut self, player: PlayerId) {
        self.scoped_player = Some(player);
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_scoped_player_recursive(player);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_scoped_player_recursive(player);
        }
    }

    /// CR 614.5 + CR 616.1f: Carry replacement application identity through a
    /// post-replacement continuation and any branch/sub-chain it resolves.
    pub fn set_replacement_applied_recursive(&mut self, applied: HashSet<AppliedReplacementKey>) {
        self.replacement_applied = applied.clone();
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_replacement_applied_recursive(applied.clone());
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_replacement_applied_recursive(applied);
        }
    }

    /// CR 608.2c: A parsed ability chain is ONE printed ability — every link is
    /// a later instruction of the same text, not an ability of its own. The
    /// parser records the printed text only on the chain head, so any prompt a
    /// chained link opens has nothing to show the player.
    ///
    /// That is not cosmetic. A `WaitingFor::OptionalEffectChoice` raised by a
    /// chained link ("If you do, you may cast the copy without paying its mana
    /// cost" — Isochron Scepter) rendered as a bare Yes/No with no question, so
    /// declining it looked identical to dismissing a stray dialog. The player
    /// had already paid the activation cost, and the decline silently discarded
    /// the copy. 152 optional chain links in the 4k-card test fixture alone open
    /// a prompt this way.
    ///
    /// Fill every link that carries no text of its own with the head's printed
    /// text. A link that DOES carry its own text — a modal branch label such as
    /// "tap" / "untap" — keeps it, while the head's text still reaches that
    /// link's own children: one value propagated down the chain, mirroring
    /// [`Self::set_scoped_player_recursive`]. Idempotent, so a chain built once
    /// and re-backfilled after a later head assignment is unchanged.
    pub fn backfill_chain_description(&mut self) {
        let Some(description) = self.description.clone() else {
            return;
        };
        self.fill_missing_link_descriptions(&description);
    }

    /// Recursive half of [`Self::backfill_chain_description`], kept separate so
    /// the public entry always sources the text from the chain head rather than
    /// from whichever link the recursion currently sits on.
    fn fill_missing_link_descriptions(&mut self, description: &str) {
        for link in [self.sub_ability.as_mut(), self.else_ability.as_mut()]
            .into_iter()
            .flatten()
        {
            if link.description.is_none() {
                link.description = Some(description.to_string());
            }
            link.fill_missing_link_descriptions(description);
        }
    }

    /// CR 608.2c: Stamp `context.optional_effect_performed` across the local
    /// ability chain. Used when an optional effect is accepted after its prompt
    /// suspended the parent chain — the stashed "If you do" continuation was
    /// captured before the choice, so its context must be updated retroactively
    /// for `IfYouDo` / `IfAPlayerDoes` gates to evaluate correctly.
    pub fn set_optional_effect_performed_recursive(&mut self, performed: bool) {
        self.context.optional_effect_performed = performed;
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_optional_effect_performed_recursive(performed);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_optional_effect_performed_recursive(performed);
        }
    }

    /// Marks only the first source-ordered `if you do` gate in a suspended
    /// continuation. A paid resolution-time cast completes one
    /// optional instruction; later, independent optional instructions must not
    /// inherit that result.
    pub fn set_first_optional_effect_performed_gate(&mut self, performed: bool) -> bool {
        if self
            .condition
            .as_ref()
            .is_some_and(AbilityCondition::is_optional_effect_performed)
        {
            self.context.optional_effect_performed = performed;
            return true;
        }
        if self
            .sub_ability
            .as_mut()
            .is_some_and(|sub| sub.set_first_optional_effect_performed_gate(performed))
        {
            return true;
        }
        self.else_ability.as_mut().is_some_and(|else_branch| {
            else_branch.set_first_optional_effect_performed_gate(performed)
        })
    }

    /// CR 608.2c: Does any node in this local ability chain carry an
    /// `EffectOutcome { OptionalEffectPerformed }` ("if you do") gate? Mirrors the
    /// traversal of [`Self::set_optional_effect_performed_recursive`] (self →
    /// sub-ability → else-branch). Used at the paid during-resolution cast-commit
    /// point to gate the retroactive latch: only a stashed continuation carrying
    /// such a rider (Conduit of Worlds' "If you do, you can't cast additional
    /// spells this turn") is stamped, so the shared cast-finalize path does not
    /// misfire on a during-resolution cast without an "if you do" rider (Cascade,
    /// Discover) or on a normal hand cast.
    pub fn has_optional_effect_performed_gate(&self) -> bool {
        self.condition
            .as_ref()
            .is_some_and(AbilityCondition::is_optional_effect_performed)
            || self
                .sub_ability
                .as_ref()
                .is_some_and(|sub| sub.has_optional_effect_performed_gate())
            || self
                .else_ability
                .as_ref()
                .is_some_and(|else_branch| else_branch.has_optional_effect_performed_gate())
    }

    /// CR 608.2d: Stamp `context.guess_outcome` across the local ability chain.
    /// Used when the `Effect::OpponentGuess` answer arrives after the prompt
    /// suspended the parent chain — the stashed branch continuation was captured
    /// before the guess, so its context must be updated retroactively for
    /// `EffectOutcomeSignal::Guessed { outcome }` gates to evaluate correctly on
    /// drain (mirrors `set_optional_effect_performed_recursive`).
    pub fn set_guess_outcome_recursive(&mut self, outcome: Option<GuessOutcome>) {
        self.context.guess_outcome = outcome;
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_guess_outcome_recursive(outcome);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_guess_outcome_recursive(outcome);
        }
    }

    /// CR 608.2d: Make `player` available as a player target across the chain so
    /// a `TargetFilter::ParentTarget` anaphor ("they lose life ...") resolves to
    /// the guesser. Inserted at the front so it wins the first-player-target
    /// lookup.
    pub fn push_front_player_target_recursive(&mut self, player: PlayerId) {
        self.move_target_to_front(TargetRef::Player(player));
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.push_front_player_target_recursive(player);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.push_front_player_target_recursive(player);
        }
    }

    pub fn set_original_controller_recursive(&mut self, player: PlayerId) {
        self.original_controller = Some(player);
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_original_controller_recursive(player);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_original_controller_recursive(player);
        }
    }

    /// CR 608.2: Rebind the acting controller across this ability and every
    /// sub/else branch. Used by the `player_scope` driver: when a compound
    /// "each player <verb1>, <verb2>, then <verb3>" instruction iterates, the
    /// SCOPED player is the acting controller for the WHOLE chain, not just the
    /// top clause — so a co-scoped sub-clause's implicit-controller recipient
    /// (e.g. `LoseLife { target: None }`, "each player ... loses life") and any
    /// generic handler that reads `controller` resolve to the iterating player.
    /// The printed ability controller is preserved separately via
    /// `original_controller` (CR 109.5), so "you" references are unaffected.
    pub fn set_controller_recursive(&mut self, player: PlayerId) {
        self.controller = player;
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_controller_recursive(player);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_controller_recursive(player);
        }
    }

    /// CR 608.2c + CR 109.4: Propagate the resolution-scoped chosen-players
    /// list across this ability and every sub/else branch. Called by the
    /// `WaitingFor::NamedChoice` answer handler after appending a freshly
    /// chosen player, so a later `Choose(Player)` or a `ChosenPlayer { index }`
    /// reference deeper in the continuation chain sees every earlier choice.
    pub fn set_chosen_players_recursive(&mut self, players: &[PlayerId]) {
        self.chosen_players = players.to_vec();
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_chosen_players_recursive(players);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_chosen_players_recursive(players);
        }
    }

    pub fn set_context_recursive(&mut self, context: SpellContext) {
        self.context = context.clone();
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_context_recursive(context.clone());
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_context_recursive(context);
        }
    }

    pub fn kind(mut self, kind: AbilityKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn sub_ability(mut self, ability: ResolvedAbility) -> Self {
        self.sub_ability = Some(Box::new(ability));
        self
    }

    pub fn else_ability(mut self, ability: ResolvedAbility) -> Self {
        self.else_ability = Some(Box::new(ability));
        self
    }

    pub fn duration(mut self, duration: Duration) -> Self {
        self.duration = Some(duration);
        self
    }

    pub fn condition(mut self, condition: AbilityCondition) -> Self {
        self.condition = Some(condition);
        self
    }

    pub fn context(mut self, context: SpellContext) -> Self {
        self.context = context;
        self
    }

    /// Extract the first `TargetRef::Player` from targets, or default to controller.
    /// Used by effects that target a player (mill, discard, life loss, shuffle, etc.).
    pub fn target_player(&self) -> PlayerId {
        self.targets
            .iter()
            .find_map(|t| match t {
                TargetRef::Player(pid) => Some(*pid),
                _ => None,
            })
            .unwrap_or(self.controller)
    }

    /// CR 601.2c: Whether this ability permits choosing zero targets — i.e. an
    /// empty legal-target set is acceptable rather than an error. True when the
    /// ability-wide `optional_targeting` ("up to one") flag is set, or when its
    /// `multi_target` spec ("up to N") has a minimum of zero. Both fields encode
    /// the same "zero targets is legal" fact, so target-slot collection must
    /// honor either.
    pub fn targeting_is_optional(&self) -> bool {
        self.optional_targeting
            || self
                .multi_target
                .as_ref()
                .is_some_and(MultiTargetSpec::min_is_fixed_zero)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::events::GameEvent;
    use crate::types::game_state::{DelayedTrigger, GameState, ZoneChangeRecord};
    use crate::types::zones::{EtbTapState, Zone};

    /// CR 607.1 + CR 607.5 + CR 613.1f: every trigger occurrence classifies
    /// exhaustively — printed and copied-value occurrences are characteristic
    /// of their copiable set, layer-6 grants are granted, and occurrences with
    /// no attributable set are `None`. `ResolvedAbility::source_ability_provenance`
    /// prefers the announcement latch, then the trigger occurrence.
    #[test]
    fn ability_provenance_classifies_occurrences_and_prefers_the_latch() {
        let copy = CopyEffectInstanceRef {
            continuous_effect_id: 3,
            modification_index: 1,
        };
        let grant = TriggerGrantInstanceRef(2);
        let printed = TriggerDefinitionOccurrenceRef::Printed {
            base_set: TriggerBaseSetInstanceRef::INITIAL,
            printed_index: 0,
        };
        let cases = [
            (
                printed.clone(),
                Some(AbilityProvenance::Characteristic(CharacteristicSetRef::Own)),
            ),
            (
                TriggerDefinitionOccurrenceRef::CopiedValue {
                    copy_effect: copy,
                    copied_slot: 0,
                    printed_origin: None,
                },
                Some(AbilityProvenance::Characteristic(
                    CharacteristicSetRef::Copied(copy),
                )),
            ),
            (
                TriggerDefinitionOccurrenceRef::KeywordCompanion {
                    grant_instance: grant,
                    companion_index: 0,
                },
                Some(AbilityProvenance::Granted),
            ),
            (
                TriggerDefinitionOccurrenceRef::Granted {
                    grant_instance: grant,
                },
                Some(AbilityProvenance::Granted),
            ),
            (
                TriggerDefinitionOccurrenceRef::ExpandedGrant {
                    grant_instance: grant,
                    provider: Box::new(TriggerDefinitionRef {
                        source: ObjectIncarnationRef::of(ObjectId(1), 0),
                        occurrence: printed.clone(),
                    }),
                    provider_output_index: 0,
                },
                Some(AbilityProvenance::Granted),
            ),
            (
                TriggerDefinitionOccurrenceRef::CopyRetained {
                    grant_instance: grant,
                    source_base_set: TriggerBaseSetInstanceRef::INITIAL,
                    source_printed_index: 0,
                },
                None,
            ),
            (TriggerDefinitionOccurrenceRef::Unmaterialized, None),
        ];
        for (occurrence, expected) in &cases {
            assert_eq!(occurrence.provenance(), *expected, "{occurrence:?}");
        }

        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            crate::types::player::PlayerId(0),
        );
        assert_eq!(ability.source_ability_provenance(), None, "neither");
        ability.trigger_definition_ref = Some(TriggerDefinitionRef {
            source: ObjectIncarnationRef::of(ObjectId(1), 0),
            occurrence: printed,
        });
        assert_eq!(
            ability.source_ability_provenance(),
            Some(AbilityProvenance::Characteristic(CharacteristicSetRef::Own)),
            "the trigger occurrence answers"
        );
        ability.set_source_ability_provenance_recursive(Some(AbilityProvenance::Granted));
        assert_eq!(
            ability.source_ability_provenance(),
            Some(AbilityProvenance::Granted),
            "the announcement latch wins"
        );
    }

    #[test]
    fn attach_target_bindings_keep_spell_context_construction_and_wire_shape_stable() {
        let external_style = SpellContext {
            optional_effect_performed: true,
            ..SpellContext::default()
        };
        assert!(external_style.attach_target_bindings.is_empty());
        assert!(external_style.forwarded_result_context.is_none());

        let absent: SpellContext =
            serde_json::from_value(serde_json::json!({})).expect("legacy context deserializes");
        assert!(absent.attach_target_bindings.is_empty());
        assert!(absent.forwarded_result_context.is_none());

        let empty_forwarded: SpellContext = serde_json::from_value(serde_json::json!({
            "forwarded_result_context": { "targets": [] }
        }))
        .expect("empty forward-result context deserializes");
        assert_eq!(
            empty_forwarded.forwarded_result_context,
            Some(Box::new(ForwardedResultContext {
                targets: vec![],
                object_incarnations: vec![],
            }))
        );

        let empty: SpellContext = serde_json::from_value(serde_json::json!({
            "attach_target_bindings": {}
        }))
        .expect("empty attachment bindings deserialize");
        assert!(empty.attach_target_bindings.is_empty());

        let legacy: SpellContext = serde_json::from_value(serde_json::json!({
            "attach_target_bindings": {
                "attachment_targets": [{ "Object": 11 }],
                "host_target": { "Object": 12 }
            }
        }))
        .expect("legacy TargetRef attachment bindings deserialize");
        assert_eq!(
            legacy.attach_target_bindings.attachment_targets(),
            &[ObjectIncarnationRef::of(ObjectId(11), LEGACY_INCARNATION)]
        );
        assert_eq!(
            legacy.attach_target_bindings.host_target(),
            Some(&ObjectIncarnationRef::of(ObjectId(12), LEGACY_INCARNATION))
        );

        let mut populated = SpellContext::default();
        populated
            .attach_target_bindings
            .bind_attachment(ObjectIncarnationRef::of(ObjectId(11), 1));
        populated
            .attach_target_bindings
            .bind_host(ObjectIncarnationRef::of(ObjectId(12), 2));
        populated
            .attach_target_bindings
            .bind_attachment_candidates(vec![ObjectIncarnationRef::of(ObjectId(13), 2)]);
        let wire = serde_json::to_value(&populated).expect("attachment bindings serialize");
        assert_eq!(
            wire["attach_target_bindings"]["attachment_targets"],
            serde_json::json!([{ "object_id": 11, "incarnation": 1 }])
        );
        assert_eq!(
            wire["attach_target_bindings"]["host_target"],
            serde_json::json!({ "object_id": 12, "incarnation": 2 })
        );
        assert_eq!(
            wire["attach_target_bindings"]["attachment_candidates"],
            serde_json::json!([{ "object_id": 13, "incarnation": 2 }])
        );
        assert_eq!(
            serde_json::from_value::<SpellContext>(wire).expect("attachment bindings round-trip"),
            populated
        );
    }

    #[test]
    fn spell_context_creation_lookback_round_trips() {
        let event = GameEvent::ZoneChanged {
            object_id: ObjectId(7),
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            record: Box::new(ZoneChangeRecord::test_minimal(
                ObjectId(7),
                Some(Zone::Battlefield),
                Zone::Graveyard,
            )),
        };

        let populated = SpellContext {
            creation_lookback_event: Some(Box::new(event.clone())),
            ..SpellContext::default()
        };
        let round_tripped: SpellContext =
            serde_json::from_value(serde_json::to_value(&populated).expect("context serializes"))
                .expect("context deserializes");
        assert_eq!(round_tripped, populated);

        let absent = SpellContext::default();
        let absent_round_tripped: SpellContext =
            serde_json::from_value(serde_json::to_value(&absent).expect("context serializes"))
                .expect("context deserializes");
        assert_eq!(absent_round_tripped, absent);
        assert!(serde_json::to_value(&absent)
            .expect("context serializes")
            .get("creation_lookback_event")
            .is_none());

        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            crate::types::player::PlayerId(0),
        );
        ability.context = populated;

        let mut state = GameState::new_two_player(42);
        state.delayed_triggers.push(DelayedTrigger::new(
            DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
            Box::new(ability),
            crate::types::player::PlayerId(0),
            ObjectId(1),
            true,
        ));
        let cloned = state.clone();
        assert_eq!(cloned, state);
    }

    #[test]
    fn first_optional_effect_gate_does_not_latch_a_later_independent_gate() {
        let mut first = ResolvedAbility::new(Effect::NoOp, vec![], ObjectId(1), PlayerId(0));
        first.condition = Some(AbilityCondition::EffectOutcome {
            signal: EffectOutcomeSignal::OptionalEffectPerformed,
        });

        let mut second = ResolvedAbility::new(Effect::NoOp, vec![], ObjectId(1), PlayerId(0));
        second.condition = Some(AbilityCondition::EffectOutcome {
            signal: EffectOutcomeSignal::OptionalEffectPerformed,
        });
        first.sub_ability = Some(Box::new(second));

        assert!(first.set_first_optional_effect_performed_gate(true));
        assert!(first.context.optional_effect_performed);
        assert!(
            !first
                .sub_ability
                .as_ref()
                .expect("second independent gate exists")
                .context
                .optional_effect_performed
        );
    }

    #[test]
    fn resolved_ability_serializes_and_roundtrips() {
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
            vec![TargetRef::Object(ObjectId(10))],
            ObjectId(1),
            PlayerId(0),
        );
        let json = serde_json::to_string(&ability).unwrap();
        let deserialized: ResolvedAbility = serde_json::from_str(&json).unwrap();
        assert_eq!(ability, deserialized);
    }

    /// CR 608.2b: resolution legality stamps survive a persist/restore round
    /// trip, while unstamped abilities omit both fields.
    #[test]
    fn resolved_ability_illegal_target_slots_roundtrip() {
        let mut ability = ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Any,
            },
            vec![
                TargetRef::Object(ObjectId(10)),
                TargetRef::Object(ObjectId(11)),
            ],
            ObjectId(1),
            PlayerId(0),
        );
        let unstamped = serde_json::to_string(&ability).unwrap();
        assert!(
            !unstamped.contains("illegal_target_slots")
                && !unstamped.contains("illegal_local_target_slots"),
            "empty stamps are not serialized"
        );

        ability.illegal_target_slots = vec![1];
        let legacy_json = serde_json::to_string(&ability).unwrap();
        let legacy: ResolvedAbility = serde_json::from_str(&legacy_json).unwrap();
        assert_eq!(legacy.illegal_target_slots, vec![1]);
        assert!(
            legacy.illegal_local_target_slots.is_empty(),
            "a legacy payload without the local stamp defaults empty"
        );

        ability.illegal_local_target_slots = vec![0];
        let json = serde_json::to_string(&ability).unwrap();
        let deserialized: ResolvedAbility = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.illegal_target_slots, vec![1]);
        assert_eq!(deserialized.illegal_local_target_slots, vec![0]);
        assert_eq!(ability, deserialized);
    }

    #[test]
    fn resolved_ability_with_sub_ability_roundtrips() {
        let sub = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(1),
            PlayerId(0),
        )
        .sub_ability(sub);
        let json = serde_json::to_string(&ability).unwrap();
        let deserialized: ResolvedAbility = serde_json::from_str(&json).unwrap();
        assert_eq!(ability, deserialized);
    }

    #[test]
    fn resolved_ability_no_hashmap_fields() {
        // Verify ResolvedAbility can be created and round-tripped without any HashMap fields
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Controller,
            },
            vec![TargetRef::Player(PlayerId(0))],
            ObjectId(1),
            PlayerId(0),
        );
        let json = serde_json::to_string(&ability).unwrap();
        let deserialized: ResolvedAbility = serde_json::from_str(&json).unwrap();
        assert_eq!(ability, deserialized);
    }

    #[test]
    fn selected_mode_labels_are_legacy_compatible_and_preserved_by_clone() {
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        let legacy_json = serde_json::to_string(&ability).expect("serialize empty ability");
        assert!(
            !legacy_json.contains("selected_mode_labels"),
            "empty labels must not change saved-game or client wire shape",
        );
        let legacy: ResolvedAbility =
            serde_json::from_str(&legacy_json).expect("legacy ability without labels deserializes");
        assert!(legacy.selected_mode_labels.is_empty());

        ability.selected_mode_labels = vec!["Draw a card.".to_string()];
        let copied = ability.clone();
        let json = serde_json::to_string(&ability).expect("serialize selected mode");
        assert!(json.contains("selected_mode_labels"));
        let deserialized: ResolvedAbility = serde_json::from_str(&json).expect("deserialize mode");
        assert_eq!(copied, deserialized);
    }

    #[test]
    fn resolved_ability_duration_roundtrips() {
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![TargetRef::Object(ObjectId(10))],
            ObjectId(1),
            PlayerId(0),
        )
        .duration(Duration::UntilHostLeavesPlay);
        let json = serde_json::to_string(&ability).unwrap();
        let deserialized: ResolvedAbility = serde_json::from_str(&json).unwrap();
        assert_eq!(ability, deserialized);
        assert_eq!(deserialized.duration, Some(Duration::UntilHostLeavesPlay));
    }

    // -----------------------------------------------------------------------
    // self_ref_is_current co-departure mis-latch gate (issue #6427)
    // -----------------------------------------------------------------------

    mod self_ref_is_current_tests {
        use super::*;
        use crate::game::game_object::GameObject;
        use crate::game::scenario::GameScenario;
        use crate::game::zones::move_to_zone;
        use crate::types::events::GameEvent;
        use crate::types::game_state::{ResolutionSourceRelatch, ZoneChangeRecord};
        use crate::types::player::PlayerId;

        const P0: PlayerId = PlayerId(0);

        fn ability_with_mislatch(
            source_id: ObjectId,
            expected_zone: Zone,
            incarnation: u64,
        ) -> ResolvedAbility {
            let mut ability = ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                vec![],
                source_id,
                P0,
            );
            ability.set_test_trigger_source_recursive(incarnation, CardId(1));
            ability
                .trigger_source
                .as_mut()
                .unwrap()
                .identity
                .expected_zone = expected_zone;
            ability
        }

        fn snapshot_source_context(object: &GameObject, from: Zone) -> TriggerSourceContext {
            object
                .snapshot_for_zone_change(object.id, Some(from), object.zone)
                .trigger_source_context
                .expect("snapshot always carries trigger context")
        }

        fn zone_changed_event(
            object_id: ObjectId,
            from: Zone,
            to: Zone,
            source_context: Option<TriggerSourceContext>,
            turn_index: usize,
        ) -> GameEvent {
            let mut record = ZoneChangeRecord::test_minimal(object_id, Some(from), to);
            record.trigger_source_context = source_context;
            record.turn_zone_change_index = turn_index;
            GameEvent::ZoneChanged {
                object_id,
                from: Some(from),
                to,
                record: Box::new(record),
            }
        }

        /// CR 400.7e: own-departure successor succeeds even when expected_zone was
        /// mis-latched to the graveyard during co-departure collection.
        #[test]
        fn self_ref_succeeds_own_departure_through_mislatch_gate() {
            let mut scenario = GameScenario::new();
            let source = scenario.add_vanilla(P0, 2, 2);
            let mut runner = scenario.build();
            let mut events = Vec::new();

            move_to_zone(runner.state_mut(), source, Zone::Graveyard, &mut events);
            let incarnation = runner.state().objects[&source].incarnation;
            let source_context = snapshot_source_context(
                runner.state().objects.get(&source).unwrap(),
                Zone::Battlefield,
            );

            let mut ability = ability_with_mislatch(source, Zone::Graveyard, incarnation);
            ability.trigger_source = Some(source_context.clone());

            let record = {
                let mut record = ZoneChangeRecord::test_minimal(
                    source,
                    Some(Zone::Battlefield),
                    Zone::Graveyard,
                );
                record.trigger_source_context = Some(source_context);
                record.turn_zone_change_index = 0;
                record
            };
            runner.state_mut().zone_changes_this_turn = vec![record].into();
            runner.state_mut().current_trigger_event = Some(zone_changed_event(
                source,
                Zone::Battlefield,
                Zone::Graveyard,
                runner.state().zone_changes_this_turn[0]
                    .trigger_source_context
                    .clone(),
                0,
            ));

            assert!(
                ability.self_ref_is_current(runner.state()),
                "own-departure successor must bind SelfRef through the mis-latch gate"
            );
        }

        /// CR 400.7 + CR 603.7c: `self_ref_binding` binds a triggered source
        /// only while the ability can still find it. A blinked source is a new
        /// object and is not bound. A forward-result continuation whose
        /// `source_id` was rebound to an object its producer moved keeps that
        /// rebound binding unchanged; that row pins preserved behavior and makes
        /// no rules claim.
        #[test]
        fn self_ref_binding_binds_a_triggered_source_only_while_it_can_be_found() {
            let mut scenario = GameScenario::new();
            let source = scenario.add_vanilla(P0, 2, 2);
            let moved = scenario.add_vanilla(P0, 1, 1);
            let mut runner = scenario.build();
            let mut events = Vec::new();

            let incarnation = runner.state().objects[&source].incarnation;
            let ability = ability_with_mislatch(source, Zone::Battlefield, incarnation);
            assert_eq!(
                ability.self_ref_binding(runner.state()),
                Some(source),
                "reach: the unmoved triggered source is bound"
            );

            // The forward-result producer moved `moved`; its continuation is
            // rebound to it.
            move_to_zone(runner.state_mut(), moved, Zone::Exile, &mut events);
            move_to_zone(runner.state_mut(), moved, Zone::Battlefield, &mut events);
            let mut rebound = ability.clone();
            rebound.source_id = moved;

            // The source itself is blinked.
            move_to_zone(runner.state_mut(), source, Zone::Exile, &mut events);
            move_to_zone(runner.state_mut(), source, Zone::Battlefield, &mut events);
            assert_ne!(runner.state().objects[&source].incarnation, incarnation);
            assert_eq!(
                ability.self_ref_binding(runner.state()),
                None,
                "CR 400.7: the permanent that returned is a new object"
            );
            assert_eq!(
                rebound.self_ref_binding(runner.state()),
                Some(moved),
                "the rebound forward-result binding is preserved unchanged"
            );
        }

        /// Draw (non-zone-change) trigger from graveyard: zone-match SelfRef must
        /// stay valid without own-departure successor proof.
        #[test]
        fn self_ref_succeeds_graveyard_trigger_on_non_zone_change_event() {
            let mut scenario = GameScenario::new();
            let source = scenario.add_vanilla(P0, 2, 2);
            let mut runner = scenario.build();
            let mut events = Vec::new();

            move_to_zone(runner.state_mut(), source, Zone::Graveyard, &mut events);
            let incarnation = runner.state().objects[&source].incarnation;

            let ability = ability_with_mislatch(source, Zone::Graveyard, incarnation);

            runner.state_mut().current_trigger_event = Some(GameEvent::CardsDrawn {
                player_id: P0,
                count: 1,
            });

            assert!(
                ability.self_ref_is_current(runner.state()),
                "graveyard trigger on a draw event must not require own-departure successor"
            );
        }

        /// Co-departure mis-latch: source in GY but the trigger event is another
        /// object's departure — SelfRef must fail closed.
        #[test]
        fn self_ref_fails_co_departed_mislatch_graveyard() {
            let mut scenario = GameScenario::new();
            let source = scenario.add_vanilla(P0, 2, 2);
            let other = scenario.add_vanilla(P0, 1, 1);
            let mut runner = scenario.build();
            let mut events = Vec::new();

            move_to_zone(runner.state_mut(), source, Zone::Graveyard, &mut events);
            move_to_zone(runner.state_mut(), other, Zone::Graveyard, &mut events);
            let incarnation = runner.state().objects[&source].incarnation;
            let source_context = snapshot_source_context(
                runner.state().objects.get(&source).unwrap(),
                Zone::Battlefield,
            );
            let other_context = snapshot_source_context(
                runner.state().objects.get(&other).unwrap(),
                Zone::Battlefield,
            );

            let ability = ability_with_mislatch(source, Zone::Graveyard, incarnation);

            let source_record = {
                let mut record = ZoneChangeRecord::test_minimal(
                    source,
                    Some(Zone::Battlefield),
                    Zone::Graveyard,
                );
                record.trigger_source_context = Some(source_context);
                record.turn_zone_change_index = 0;
                record
            };
            let other_record = {
                let mut record =
                    ZoneChangeRecord::test_minimal(other, Some(Zone::Battlefield), Zone::Graveyard);
                record.trigger_source_context = Some(other_context.clone());
                record.turn_zone_change_index = 1;
                record.co_departed = vec![source];
                record
            };
            runner.state_mut().zone_changes_this_turn = vec![source_record, other_record].into();
            runner.state_mut().current_trigger_event = Some({
                let mut event = zone_changed_event(
                    other,
                    Zone::Battlefield,
                    Zone::Graveyard,
                    Some(other_context),
                    1,
                );
                if let GameEvent::ZoneChanged { record, .. } = &mut event {
                    record.co_departed = vec![source];
                }
                event
            });

            assert!(
                ability.source_is_current(runner.state()),
                "reach guard: mis-latch fixture must still satisfy zone-match source_is_current"
            );
            assert!(
                !ability.self_ref_is_current(runner.state()),
                "co-departed mis-latch must not let SelfRef bind when co_departed proves provenance"
            );
        }

        /// Exile mis-latch sibling: same gate applies off the battlefield.
        #[test]
        fn self_ref_fails_co_departed_mislatch_exile() {
            let mut scenario = GameScenario::new();
            let source = scenario.add_vanilla(P0, 2, 2);
            let other = scenario.add_vanilla(P0, 1, 1);
            let mut runner = scenario.build();
            let mut events = Vec::new();

            move_to_zone(runner.state_mut(), source, Zone::Exile, &mut events);
            move_to_zone(runner.state_mut(), other, Zone::Exile, &mut events);
            let incarnation = runner.state().objects[&source].incarnation;
            let source_context = snapshot_source_context(
                runner.state().objects.get(&source).unwrap(),
                Zone::Battlefield,
            );
            let other_context = snapshot_source_context(
                runner.state().objects.get(&other).unwrap(),
                Zone::Battlefield,
            );

            let ability = ability_with_mislatch(source, Zone::Exile, incarnation);

            let source_record = {
                let mut record =
                    ZoneChangeRecord::test_minimal(source, Some(Zone::Battlefield), Zone::Exile);
                record.trigger_source_context = Some(source_context);
                record.turn_zone_change_index = 0;
                record
            };
            let other_record = {
                let mut record =
                    ZoneChangeRecord::test_minimal(other, Some(Zone::Battlefield), Zone::Exile);
                record.trigger_source_context = Some(other_context.clone());
                record.turn_zone_change_index = 1;
                record.co_departed = vec![source];
                record
            };
            runner.state_mut().zone_changes_this_turn = vec![source_record, other_record].into();
            runner.state_mut().current_trigger_event = Some({
                let mut event = zone_changed_event(
                    other,
                    Zone::Battlefield,
                    Zone::Exile,
                    Some(other_context),
                    1,
                );
                if let GameEvent::ZoneChanged { record, .. } = &mut event {
                    record.co_departed = vec![source];
                }
                event
            });

            assert!(
                ability.source_is_current(runner.state()),
                "reach guard: exile mis-latch fixture must still satisfy zone-match source_is_current"
            );
            assert!(
                !ability.self_ref_is_current(runner.state()),
                "exile mis-latch must fail when the trigger event co-departed the source"
            );
        }

        /// CR 400.7e: source already in graveyard when another object changes
        /// zones must not be rejected — co-departure provenance is absent.
        #[test]
        fn self_ref_succeeds_unchanged_graveyard_on_other_zone_change() {
            let mut scenario = GameScenario::new();
            let source = scenario.add_vanilla(P0, 2, 2);
            let other = scenario.add_vanilla(P0, 1, 1);
            let mut runner = scenario.build();
            let mut events = Vec::new();

            move_to_zone(runner.state_mut(), source, Zone::Graveyard, &mut events);
            let incarnation = runner.state().objects[&source].incarnation;
            let other_context = snapshot_source_context(
                runner.state().objects.get(&other).unwrap(),
                Zone::Battlefield,
            );

            let ability = ability_with_mislatch(source, Zone::Graveyard, incarnation);

            runner.state_mut().current_trigger_event = Some(zone_changed_event(
                other,
                Zone::Battlefield,
                Zone::Graveyard,
                Some(other_context),
                0,
            ));

            assert!(
                ability.self_ref_is_current(runner.state()),
                "unchanged graveyard source must bind SelfRef on another object's zone change"
            );
        }

        /// Exile sibling: unchanged exile source survives another object's departure.
        #[test]
        fn self_ref_succeeds_unchanged_exile_on_other_zone_change() {
            let mut scenario = GameScenario::new();
            let source = scenario.add_vanilla(P0, 2, 2);
            let other = scenario.add_vanilla(P0, 1, 1);
            let mut runner = scenario.build();
            let mut events = Vec::new();

            move_to_zone(runner.state_mut(), source, Zone::Exile, &mut events);
            let incarnation = runner.state().objects[&source].incarnation;
            let other_context = snapshot_source_context(
                runner.state().objects.get(&other).unwrap(),
                Zone::Battlefield,
            );

            let ability = ability_with_mislatch(source, Zone::Exile, incarnation);

            runner.state_mut().current_trigger_event = Some(zone_changed_event(
                other,
                Zone::Battlefield,
                Zone::Exile,
                Some(other_context),
                0,
            ));

            assert!(
                ability.self_ref_is_current(runner.state()),
                "unchanged exile source must bind SelfRef on another object's zone change"
            );
        }

        /// CR 400.7j relatch bypasses the off-battlefield mis-latch gate.
        #[test]
        fn self_ref_relatch_bypasses_off_battlefield_mislatch_gate() {
            let mut scenario = GameScenario::new();
            let source = scenario.add_vanilla(P0, 2, 2);
            let mut runner = scenario.build();
            let mut events = Vec::new();

            let captured = runner.state().objects[&source].incarnation;
            move_to_zone(runner.state_mut(), source, Zone::Exile, &mut events);
            let current = runner.state().objects[&source].incarnation;

            let ability = ability_with_mislatch(source, Zone::Exile, captured);
            runner.state_mut().resolution_source_relatch = Some(ResolutionSourceRelatch {
                object_id: source,
                original_stamp: captured,
                current_incarnation: current,
            });

            assert!(
                ability.self_ref_is_current(runner.state()),
                "relatch must early-true without requiring own-departure successor proof"
            );
        }

        fn gy_return_ability(
            source_id: ObjectId,
            source_context: TriggerSourceContext,
        ) -> ResolvedAbility {
            let mut ability = ResolvedAbility::new(
                Effect::ChangeZone {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Battlefield,
                    target: TargetFilter::SelfRef,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                },
                vec![],
                source_id,
                P0,
            );
            ability.trigger_source = Some(source_context);
            ability
        }

        /// CR 400.7 + CR 704.5m: post-SBA BF→GY departure with matching captured
        /// identity permits SelfRef return-from-GY (Necrotic Plague).
        #[test]
        fn self_ref_post_sba_gy_return_binds_matching_bf_departure() {
            let mut scenario = GameScenario::new();
            let source = scenario.add_vanilla(P0, 2, 2);
            let mut runner = scenario.build();

            let bf_context = {
                let obj = runner.state().objects.get(&source).unwrap();
                crate::game::triggers::trigger_source_context_for_latch(runner.state(), obj)
            };
            let mut events = Vec::new();
            move_to_zone(runner.state_mut(), source, Zone::Graveyard, &mut events);

            let record = {
                let mut record = ZoneChangeRecord::test_minimal(
                    source,
                    Some(Zone::Battlefield),
                    Zone::Graveyard,
                );
                record.trigger_source_context = Some(bf_context.clone());
                record.turn_zone_change_index = 0;
                record
            };
            runner.state_mut().zone_changes_this_turn = vec![record].into();

            let ability = gy_return_ability(source, bf_context);
            assert!(
                ability.self_ref_is_current(runner.state()),
                "matching BF→GY departure must bind SelfRef return-from-GY"
            );
        }

        /// CR 400.7e: a same-id second zone change after the BF→GY departure
        /// must not rebind SelfRef to the stale source.
        #[test]
        fn self_ref_post_sba_gy_return_rejects_stale_after_second_zone_change() {
            let mut scenario = GameScenario::new();
            let source = scenario.add_vanilla(P0, 2, 2);
            let mut runner = scenario.build();

            let bf_context = {
                let obj = runner.state().objects.get(&source).unwrap();
                crate::game::triggers::trigger_source_context_for_latch(runner.state(), obj)
            };
            let mut events = Vec::new();
            move_to_zone(runner.state_mut(), source, Zone::Graveyard, &mut events);
            move_to_zone(runner.state_mut(), source, Zone::Exile, &mut events);

            let gy_record = {
                let mut record = ZoneChangeRecord::test_minimal(
                    source,
                    Some(Zone::Battlefield),
                    Zone::Graveyard,
                );
                record.trigger_source_context = Some(bf_context.clone());
                record.turn_zone_change_index = 0;
                record
            };
            let exile_record = {
                let mut record =
                    ZoneChangeRecord::test_minimal(source, Some(Zone::Graveyard), Zone::Exile);
                record.turn_zone_change_index = 1;
                record
            };
            // Leave the object in GY for the zone gate so only the second-move
            // provenance rejects — mirrors a same-id return that later left again.
            move_to_zone(runner.state_mut(), source, Zone::Graveyard, &mut events);
            runner.state_mut().zone_changes_this_turn = vec![gy_record, exile_record].into();

            let ability = gy_return_ability(source, bf_context);
            assert!(
                !ability.self_ref_is_current(runner.state()),
                "second same-id zone change must reject post-SBA SelfRef return"
            );
        }

        /// CR 400.7f + CR 301.5: a host-departure trigger finds the graveyard
        /// card its attached source became only when that source was an Aura
        /// enchanting the host. Each fixture is the same co-departure — the
        /// source's own BF→GY move carries the latched identity and the host's
        /// departure record lists it as an attachment — differing only in the
        /// recorded attachment kind, so the Equipment row fails on the Aura
        /// requirement alone.
        #[test]
        fn host_departure_finds_only_an_aura_source_in_the_graveyard() {
            use crate::types::game_state::AttachmentSnapshot;

            for (kind, finds) in [
                (AttachmentKind::Aura, true),
                (AttachmentKind::Equipment, false),
            ] {
                let mut scenario = GameScenario::new();
                let host = scenario.add_vanilla(P0, 2, 2);
                let source = scenario.add_vanilla(P0, 1, 1);
                let mut runner = scenario.build();

                let source_context = {
                    let obj = runner.state().objects.get(&source).unwrap();
                    crate::game::triggers::trigger_source_context_for_latch(runner.state(), obj)
                };
                let mut events = Vec::new();
                move_to_zone(runner.state_mut(), host, Zone::Graveyard, &mut events);
                move_to_zone(runner.state_mut(), source, Zone::Graveyard, &mut events);

                let mut host_record =
                    ZoneChangeRecord::test_minimal(host, Some(Zone::Battlefield), Zone::Graveyard);
                host_record.turn_zone_change_index = 0;
                host_record.attachments = vec![AttachmentSnapshot {
                    object_id: source,
                    identity: Some(source_context.identity.reference),
                    controller: P0,
                    kind: kind.clone(),
                }];
                let mut source_record = ZoneChangeRecord::test_minimal(
                    source,
                    Some(Zone::Battlefield),
                    Zone::Graveyard,
                );
                source_record.trigger_source_context = Some(source_context.clone());
                source_record.turn_zone_change_index = 1;
                runner.state_mut().zone_changes_this_turn =
                    vec![host_record.clone(), source_record].into();
                runner.state_mut().current_trigger_event = Some(GameEvent::ZoneChanged {
                    object_id: host,
                    from: Some(Zone::Battlefield),
                    to: Zone::Graveyard,
                    record: Box::new(host_record),
                });

                let mut ability = ResolvedAbility::new(
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                    vec![],
                    source,
                    P0,
                );
                ability.trigger_source = Some(source_context);

                assert_eq!(
                    ability.finds_source_successor(runner.state()),
                    finds,
                    "a {kind:?} source whose host left must be found only if it is an Aura"
                );
            }
        }
    }

    // ===== `Effect::target_filter_mut` is `Effect::target_filter`'s twin =====
    //
    // The `syn` helpers below mirror `types/statics.rs`'s test helpers of the
    // same names, which are private to that module's tests.
}

#[cfg(test)]
mod modal_ability_tests {
    use super::*;

    /// CR 608.2d: `push_front_player_target_recursive` must promote a guesser
    /// from any non-leading slot to index 0. This is the regression for the
    /// prior bug where a continuation with targets
    /// `[Player(other), Player(guesser)]` — guesser in slot 1 — was NOT
    /// corrected before the fix, causing `target_player()` to bind `other`
    /// instead of `guesser` for the "they lose life" rider.
    #[test]
    fn push_front_player_target_recursive_promotes_non_leading_guesser() {
        let other = PlayerId(1);
        let guesser = PlayerId(2);

        // Hostile fixture: [Player(other), Player(guesser)] — guesser at slot 1,
        // not slot 0. Build the same layout into a sub_ability to verify recursion.
        let sub = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 1 },
                target: None,
            },
            vec![TargetRef::Player(other), TargetRef::Player(guesser)],
            ObjectId(1),
            PlayerId(0),
        );
        let mut ability = ResolvedAbility::new(
            Effect::NoOp,
            vec![TargetRef::Player(other), TargetRef::Player(guesser)],
            ObjectId(1),
            PlayerId(0),
        )
        .sub_ability(sub);

        ability.push_front_player_target_recursive(guesser);

        // Top-level: guesser must be at index 0 and appear exactly once.
        assert_eq!(
            ability.targets[0],
            TargetRef::Player(guesser),
            "guesser must be promoted to index 0 in top-level targets"
        );
        assert_eq!(
            ability
                .targets
                .iter()
                .filter(|t| **t == TargetRef::Player(guesser))
                .count(),
            1,
            "guesser must appear exactly once in top-level targets after promotion"
        );

        // Sub-ability: the fix must propagate recursively.
        let sub = ability.sub_ability.as_ref().unwrap();
        assert_eq!(
            sub.targets[0],
            TargetRef::Player(guesser),
            "guesser must be promoted to index 0 in sub_ability targets"
        );
        assert_eq!(
            sub.targets
                .iter()
                .filter(|t| **t == TargetRef::Player(guesser))
                .count(),
            1,
            "guesser must appear exactly once in sub_ability targets after promotion"
        );
    }
}

#[cfg(test)]
mod cost_paid_provenance_serde_tests {
    use super::*;

    fn sample_ability() -> ResolvedAbility {
        ResolvedAbility::new(Effect::NoOp, vec![], ObjectId(1), PlayerId(0))
    }

    /// CR 400.7: A restored `ResolvedAbility` with no recorded cost-paid
    /// provenance must come back with an EMPTY authority. `#[serde(default)]`
    /// is what makes the restore/P2P path fail closed rather than erroring or
    /// inventing a referent.
    #[test]
    fn missing_cost_paid_objects_restores_empty() {
        let json = serde_json::to_value(sample_ability()).expect("ability serializes");
        assert!(
            json.get("cost_paid_objects").is_none(),
            "an empty authority is elided on the wire: {json}"
        );
        let restored: ResolvedAbility =
            serde_json::from_value(json).expect("an ability with no provenance restores");
        assert!(restored.cost_paid_objects.is_empty());
    }

    /// CR 601.2c + CR 400.7: Saves written before the snapshot authority existed
    /// carry a raw `cost_paid_object_ids` list. Those ids are exact MEMBERSHIP
    /// ("this cost consumed these objects") and no provenance at all, so they
    /// migrate to `CostPaidObjectRecord::MembershipOnly`: membership preserved,
    /// liveness refused.
    ///
    /// Dropping them instead — the pre-fix reading — failed the CR 601.2c
    /// exclusion OPEN, because the singular `cost_paid_object` fallback names
    /// only the FIRST paid object.
    #[test]
    fn a_legacy_raw_id_field_migrates_to_membership_records() {
        let mut json = serde_json::to_value(sample_ability()).expect("ability serializes");
        json.as_object_mut()
            .expect("a resolved ability serializes as a JSON object")
            .insert(
                "cost_paid_object_ids".to_string(),
                serde_json::json!([11, 12, 13]),
            );
        let restored: ResolvedAbility =
            serde_json::from_value(json).expect("a legacy payload still restores");

        assert_eq!(
            restored
                .cost_paid_objects
                .iter()
                .map(CostPaidObjectRecord::object_id)
                .collect::<Vec<_>>(),
            vec![ObjectId(11), ObjectId(12), ObjectId(13)],
            "membership must survive the migration, in payment order"
        );
        for record in &restored.cost_paid_objects {
            assert!(
                matches!(record, CostPaidObjectRecord::MembershipOnly(_)),
                "a raw id carries no captured provenance: {record:?}"
            );
            assert!(
                record.snapshot().is_none(),
                "CR 608.2h: a migrated record must expose no LKI — synthesizing one \
                 would report characteristics the game never observed"
            );
        }
    }

    /// A minimal CAPTURED wire shape. Only `LKISnapshot`'s non-defaulted fields
    /// are spelled out, because this fixture is about the record's SHAPE (a JSON
    /// object, versus a legacy id's JSON number), not about LKI content.
    fn captured_record_json(object_id: u64, incarnation: u64) -> serde_json::Value {
        serde_json::json!({
            "object_id": object_id,
            "lki": {
                "name": "Paid Object",
                "power": null,
                "toughness": null,
                "mana_value": 0,
                "controller": 0,
                "owner": 0
            },
            "incarnation": incarnation
        })
    }

    /// The migration must not move a single byte of a CURRENT save: a `Captured`
    /// record decodes from, and re-encodes to, exactly the bare
    /// `CostPaidObjectSnapshot` wire form it replaced.
    #[test]
    fn a_captured_record_is_wire_identical_to_a_bare_snapshot() {
        let wire = captured_record_json(7, 3);
        let snapshot: CostPaidObjectSnapshot =
            serde_json::from_value(wire.clone()).expect("the fixture is a valid snapshot");
        let record: CostPaidObjectRecord =
            serde_json::from_value(wire).expect("a JSON object decodes as a captured record");

        assert_eq!(record, CostPaidObjectRecord::Captured(snapshot.clone()));
        assert_eq!(
            serde_json::to_value(&record).expect("a captured record serializes"),
            serde_json::to_value(&snapshot).expect("a bare snapshot serializes"),
            "a captured record must be byte-identical to the snapshot it wraps"
        );
    }

    /// Shape-directed decode, entry by entry: a partially-migrated authority
    /// (one captured record plus one legacy id) keeps BOTH, each read as what it
    /// actually is, and round-trips to the same wire form.
    #[test]
    fn a_mixed_authority_decodes_each_entry_by_its_own_shape() {
        let wire = serde_json::json!([captured_record_json(7, 3), 12]);
        let records: Vec<CostPaidObjectRecord> =
            serde_json::from_value(wire).expect("a mixed authority decodes");

        assert_eq!(records.len(), 2);
        assert!(matches!(records[0], CostPaidObjectRecord::Captured(_)));
        assert_eq!(
            records[1],
            CostPaidObjectRecord::MembershipOnly(ObjectId(12))
        );
        assert_eq!(
            records
                .iter()
                .map(CostPaidObjectRecord::object_id)
                .collect::<Vec<_>>(),
            vec![ObjectId(7), ObjectId(12)],
            "CR 601.2c: membership is exact for both variants"
        );
        // Each entry re-encodes in the SHAPE it was read from. (Only the shapes
        // are compared: re-serializing a decoded snapshot also materializes
        // `LKISnapshot`'s serde-defaulted fields, which the minimal fixture
        // above deliberately omits.)
        let round_tripped = serde_json::to_value(&records).expect("a mixed authority serializes");
        let round_tripped = round_tripped
            .as_array()
            .expect("the authority serializes as an array");
        assert!(
            round_tripped[0].is_object(),
            "a captured record stays a JSON object: {}",
            round_tripped[0]
        );
        assert_eq!(
            round_tripped[1],
            serde_json::json!(12),
            "a membership record stays the bare legacy id"
        );
    }

    /// A current snapshot array still decodes as the snapshot authority it
    /// always was — the alias widens the accepted input, it does not reinterpret
    /// the existing key.
    #[test]
    fn a_current_snapshot_array_still_decodes_as_captured_records() {
        let mut json = serde_json::to_value(sample_ability()).expect("ability serializes");
        json.as_object_mut()
            .expect("a resolved ability serializes as a JSON object")
            .insert(
                "cost_paid_objects".to_string(),
                serde_json::json!([captured_record_json(21, 5), captured_record_json(22, 6)]),
            );
        let restored: ResolvedAbility =
            serde_json::from_value(json).expect("a current payload restores");

        assert_eq!(
            restored
                .cost_paid_objects
                .iter()
                .map(CostPaidObjectRecord::object_id)
                .collect::<Vec<_>>(),
            vec![ObjectId(21), ObjectId(22)]
        );
        for record in &restored.cost_paid_objects {
            assert!(
                record.snapshot().is_some(),
                "a captured entry keeps its provenance: {record:?}"
            );
        }
    }

    /// CR 400.7: two disagreeing authorities in one payload must NOT be merged —
    /// serde's `duplicate field` error is the correct fail-closed reading, and no
    /// production writer ever emits both keys.
    #[test]
    fn a_payload_carrying_both_cost_paid_keys_is_rejected() {
        let mut json = serde_json::to_value(sample_ability()).expect("ability serializes");
        let object = json
            .as_object_mut()
            .expect("a resolved ability serializes as a JSON object");
        object.insert(
            "cost_paid_objects".to_string(),
            serde_json::json!([captured_record_json(31, 1)]),
        );
        object.insert("cost_paid_object_ids".to_string(), serde_json::json!([32]));

        let error = serde_json::from_value::<ResolvedAbility>(json)
            .expect_err("a payload with two cost-paid authorities must not decode");
        assert!(
            error.to_string().contains("duplicate field"),
            "expected a duplicate-field rejection, got: {error}"
        );
    }
}

#[cfg(test)]
mod chain_description_backfill_tests {
    use super::*;

    /// Build a three-deep chain: a head with printed text, a middle link with
    /// none (the shape Isochron Scepter's "If you do, you may cast the copy"
    /// sub-ability has), and a leaf that carries its own modal branch label.
    fn three_deep_chain() -> ResolvedAbility {
        let leaf = ResolvedAbility {
            description: Some("tap".to_string()),
            ..ResolvedAbility::new(
                Effect::Surveil {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                ObjectId(1),
                PlayerId(0),
            )
        };
        let middle = ResolvedAbility {
            sub_ability: Some(Box::new(leaf)),
            ..ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                ObjectId(1),
                PlayerId(0),
            )
        };
        ResolvedAbility {
            description: Some("printed ability text".to_string()),
            sub_ability: Some(Box::new(middle)),
            ..ResolvedAbility::new(
                Effect::Scry {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                ObjectId(1),
                PlayerId(0),
            )
        }
    }

    #[test]
    fn backfill_fills_textless_links_and_preserves_their_own_text() {
        let mut chain = three_deep_chain();
        chain.backfill_chain_description();

        let middle = chain.sub_ability.as_deref().expect("middle link");
        assert_eq!(
            middle.description.as_deref(),
            Some("printed ability text"),
            "a link with no text of its own inherits the head's printed text"
        );

        let leaf = middle.sub_ability.as_deref().expect("leaf link");
        assert_eq!(
            leaf.description.as_deref(),
            Some("tap"),
            "a link that carries its own label keeps it"
        );
    }

    /// The head's text must reach links BELOW one that carries its own label —
    /// a recursion that sourced the text from the current link would stamp
    /// "tap" here instead.
    #[test]
    fn backfill_reaches_links_below_a_labelled_link() {
        let mut chain = three_deep_chain();
        let deepest = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        chain
            .sub_ability
            .as_mut()
            .and_then(|middle| middle.sub_ability.as_mut())
            .expect("leaf link")
            .sub_ability = Some(Box::new(deepest));

        chain.backfill_chain_description();

        let below_label = chain
            .sub_ability
            .as_deref()
            .and_then(|middle| middle.sub_ability.as_deref())
            .and_then(|leaf| leaf.sub_ability.as_deref())
            .expect("link below the labelled one");
        assert_eq!(
            below_label.description.as_deref(),
            Some("printed ability text"),
            "the head's text propagates past a labelled link, not the label"
        );
    }

    /// `else_ability` is the branch sibling of `sub_ability` and raises its own
    /// prompts, so it takes the same backfill.
    #[test]
    fn backfill_covers_the_else_branch() {
        let mut chain = three_deep_chain();
        chain.else_ability = Some(Box::new(ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        )));

        chain.backfill_chain_description();

        assert_eq!(
            chain
                .else_ability
                .as_deref()
                .expect("else branch")
                .description
                .as_deref(),
            Some("printed ability text"),
        );
    }

    /// A head with no text of its own leaves the chain untouched rather than
    /// stamping `None` over a link that has one.
    #[test]
    fn backfill_with_no_head_text_is_a_no_op() {
        let mut chain = three_deep_chain();
        chain.description = None;
        chain.backfill_chain_description();

        let middle = chain.sub_ability.as_deref().expect("middle link");
        assert_eq!(middle.description, None);
        assert_eq!(
            middle
                .sub_ability
                .as_deref()
                .expect("leaf link")
                .description
                .as_deref(),
            Some("tap"),
        );
    }
}
