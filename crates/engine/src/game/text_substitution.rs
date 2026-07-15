//! CR 612: Text-changing effects — word replacement (Layer 3).
//!
//! Single authority for walking a [`GameObject`]'s derived characteristics and
//! either COLLECTING the text words of a category currently present on it
//! (CR 612.2 — the enumerator of legal `from` words) or REPLACING every instance
//! of one word (`from`) with another (`to`). Both directions share one traversal
//! ([`walk_object_words`]) so the set of characteristics a text-change reads and
//! the set it writes can never drift apart.
//!
//! CR 612.2 scoping: only words "used in the correct way" are touched —
//! - a Magic color word used as a color word (rules-text color predicates,
//!   `Protection`/`HexproofFrom` color params, `SetColor`/`AddColor`, devotion),
//! - a basic land type used as a land type (type-line subtypes, `Landwalk`,
//!   `SetBasicLandType`, nested subtype filters),
//! - a creature type used as a creature type (type-line subtypes, typal filters).
//!
//! Structurally EXCLUDED (never walked, per CR 612.2): the object's name /
//! base name, its Layer-5 `color` field, and its mana cost / mana-symbol pips —
//! these roots are simply not descended into. A mana SYMBOL ({R}) or a
//! color-set-size predicate is not a color WORD (CR 612.2 + CR 107.4), so those
//! carriers are explicit no-ops below.
//!
//! Every enum match here is exhaustive with no `_` wildcard: a future
//! word-bearing variant fails to compile until it is classified as a carrier,
//! a recursion point, or an explicit no-op.

use std::collections::BTreeSet;
use std::str::FromStr;
use std::sync::Arc;

use crate::game::game_object::GameObject;
use crate::types::ability::{
    AbilityDefinition, BasicLandType, ContinuousModification, DevotionColors, Effect, FilterProp,
    ObjectProperty, QuantityExpr, QuantityRef, StaticCondition, StaticDefinition, TargetFilter,
    TextWord, TextWordCategory, TriggerDefinition, TypeFilter, TypedFilter,
};
use crate::types::keywords::{HexproofFilter, Keyword, ProtectionTarget};
use crate::types::mana::ManaColor;

/// Direction of a text-word walk.
pub enum WordCursor<'a> {
    /// Accumulate every text word of the walk's category present on the object.
    Collect(&'a mut BTreeSet<TextWord>),
    /// Replace each instance of `from` with `to` (both of the walk's category).
    Replace {
        from: &'a TextWord,
        to: &'a TextWord,
    },
}

impl WordCursor<'_> {
    /// Visit a color-word carrier (`ManaColor`). Only acts under the color-word
    /// category (CR 612.2: a color word used as a color word).
    fn color(&mut self, category: TextWordCategory, c: &mut ManaColor) {
        if category != TextWordCategory::ColorWord {
            return;
        }
        match self {
            WordCursor::Collect(set) => {
                set.insert(TextWord::Color(*c));
            }
            WordCursor::Replace { from, to } => {
                if let (TextWord::Color(f), TextWord::Color(t)) = (&**from, &**to) {
                    if *c == *f {
                        *c = *t;
                    }
                }
            }
        }
    }

    /// Visit a basic-land-type carrier stored as a typed [`BasicLandType`]
    /// (`SetBasicLandType`). Only acts under the basic-land-type category.
    fn basic_land_type(&mut self, category: TextWordCategory, lt: &mut BasicLandType) {
        if category != TextWordCategory::BasicLandType {
            return;
        }
        match self {
            WordCursor::Collect(set) => {
                set.insert(TextWord::BasicLandType(*lt));
            }
            WordCursor::Replace { from, to } => {
                if let (TextWord::BasicLandType(f), TextWord::BasicLandType(t)) = (&**from, &**to) {
                    if *lt == *f {
                        *lt = *t;
                    }
                }
            }
        }
    }

    /// Visit a subtype string that may name a basic land type or a creature type
    /// (type-line subtypes, `AddSubtype`/`RemoveSubtype`, `TypeFilter::Subtype`).
    /// CR 612.2 + CR 205.3: the walk's `category` disambiguates which meaning the
    /// string carries — a "Mountain" subtype is a land type under the land
    /// category and (never a creature type) under the creature category. For
    /// creature-type collection this may over-report non-creature subtypes; the
    /// resolver intersects the result with the live creature-type set.
    fn subtype(&mut self, category: TextWordCategory, s: &mut String) {
        match self {
            WordCursor::Collect(set) => match category {
                TextWordCategory::BasicLandType => {
                    if let Ok(bt) = BasicLandType::from_str(s) {
                        set.insert(TextWord::BasicLandType(bt));
                    }
                }
                TextWordCategory::CreatureType => {
                    if BasicLandType::from_str(s).is_err() {
                        set.insert(TextWord::CreatureType(s.clone()));
                    }
                }
                TextWordCategory::ColorWord => {}
            },
            WordCursor::Replace { from, to } => match (category, &**from, &**to) {
                (
                    TextWordCategory::BasicLandType,
                    TextWord::BasicLandType(f),
                    TextWord::BasicLandType(t),
                ) if s.as_str() == f.as_subtype_str() => {
                    *s = t.as_subtype_str().to_string();
                }
                (
                    TextWordCategory::CreatureType,
                    TextWord::CreatureType(f),
                    TextWord::CreatureType(t),
                ) if s == f => {
                    *s = t.clone();
                }
                _ => {}
            },
        }
    }

    /// Visit a landwalk string. CR 612.2: landwalk names a land type, so only the
    /// basic-land-type category applies (delegates to [`Self::subtype`]).
    fn landwalk(&mut self, category: TextWordCategory, s: &mut String) {
        if category == TextWordCategory::BasicLandType {
            self.subtype(category, s);
        }
    }
}

/// CR 612.2 enumerator: collect every text word of `category` currently present
/// on `obj` (the legal `from` words for a text-changing effect). Runs the shared
/// walker with a `Collect` cursor over a throwaway copy so no state is mutated.
pub fn collect_present_words(obj: &GameObject, category: TextWordCategory) -> BTreeSet<TextWord> {
    let mut set = BTreeSet::new();
    let mut scratch = obj.clone();
    walk_object_words(&mut scratch, category, &mut WordCursor::Collect(&mut set));
    set
}

/// CR 612.1 + CR 613.1c: The single traversal authority. Walks the object's live
/// (post-layer) word-bearing roots in place. Never descends into name / color /
/// mana-cost roots (CR 612.2 structural exclusion).
///
/// Ability *costs* and non-`affected`/`condition`/`modifications` static fields
/// (e.g. `StaticMode`, `attack_defended`) and `AbilityCondition` bodies are an
/// intentional coverage gap: no covered card changes a word buried there, and
/// leaving them out keeps the traversal to the roots CR 612 actually reaches for
/// this class. A future card needing them extends the roots here.
pub fn walk_object_words(
    obj: &mut GameObject,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    // Root 1: type-line subtypes (CR 205.3 — land / creature types).
    for subtype in obj.card_types.subtypes.iter_mut() {
        cursor.subtype(category, subtype);
    }
    // Root 2: keyword abilities (landwalk land types, protection / hexproof-from
    // color params).
    for keyword in obj.keywords.iter_mut() {
        walk_keyword(keyword, category, cursor);
    }
    // Root 3: activated / spell abilities (their effects and embedded filters).
    for ability in Arc::make_mut(&mut obj.abilities).iter_mut() {
        walk_ability_definition(ability, category, cursor);
    }
    // Root 4: triggered abilities.
    for i in 0..obj.trigger_definitions.len() {
        if let Some(trigger) = obj.trigger_definitions.get_mut(i) {
            walk_trigger_definition(trigger, category, cursor);
        }
    }
    // Root 5: static abilities (affected set, condition, layered modifications).
    for i in 0..obj.static_definitions.len() {
        if let Some(static_def) = obj.static_definitions.get_mut(i) {
            walk_static_definition(static_def, category, cursor);
        }
    }
}

fn walk_keyword(keyword: &mut Keyword, category: TextWordCategory, cursor: &mut WordCursor) {
    match keyword {
        // CR 702.16: protection from [color] carries a color WORD.
        Keyword::Protection(target) => walk_protection_target(target, category, cursor),
        // CR 702.11d: hexproof from [color] carries a color WORD.
        Keyword::HexproofFrom(filter) => walk_hexproof_filter(filter, category, cursor),
        // CR 702.14: landwalk names a land type.
        Keyword::Landwalk(land) => cursor.landwalk(category, land),
        // Every other keyword carries no color/land/creature WORD used as such.
        Keyword::Flying
        | Keyword::FirstStrike
        | Keyword::DoubleStrike
        | Keyword::Trample
        | Keyword::TrampleOverPlaneswalkers
        | Keyword::Deathtouch
        | Keyword::Lifelink
        | Keyword::Vigilance
        | Keyword::Haste
        | Keyword::Reach
        | Keyword::Defender
        | Keyword::Menace
        | Keyword::Indestructible
        | Keyword::Hexproof
        | Keyword::Shroud
        | Keyword::Flash
        | Keyword::Fear
        | Keyword::Intimidate
        | Keyword::Skulk
        | Keyword::Shadow
        | Keyword::Horsemanship
        | Keyword::Wither
        | Keyword::Infect
        | Keyword::Afflict(..)
        | Keyword::StartingIntensity(..)
        | Keyword::Prowess
        | Keyword::Undying
        | Keyword::Persist
        | Keyword::Cascade
        | Keyword::Exalted
        | Keyword::Flanking
        | Keyword::Evolve
        | Keyword::Extort
        | Keyword::Exploit
        | Keyword::Explore
        | Keyword::Ascend
        | Keyword::StartYourEngines
        | Keyword::Dredge(..)
        | Keyword::Modular(..)
        | Keyword::Renown(..)
        | Keyword::Fabricate(..)
        | Keyword::Annihilator(..)
        | Keyword::Bushido(..)
        | Keyword::Frenzy(..)
        | Keyword::Tribute(..)
        | Keyword::Soulbond
        | Keyword::Unearth(..)
        | Keyword::Convoke
        | Keyword::Waterbend
        | Keyword::Delve
        | Keyword::Devoid
        | Keyword::Changeling
        | Keyword::Phasing
        | Keyword::Battlecry
        | Keyword::Decayed
        | Keyword::Unleash
        | Keyword::Riot
        | Keyword::Afterlife(..)
        | Keyword::Enchant(..)
        | Keyword::EtbCounter { .. }
        | Keyword::Reconfigure(..)
        | Keyword::LivingWeapon
        | Keyword::JobSelect
        | Keyword::TotemArmor
        | Keyword::Bestow(..)
        | Keyword::Embalm(..)
        | Keyword::Eternalize(..)
        | Keyword::Fading(..)
        | Keyword::Vanishing(..)
        | Keyword::Kicker(..)
        | Keyword::Cycling(..)
        | Keyword::Flashback(..)
        | Keyword::Ward(..)
        | Keyword::Equip(..)
        | Keyword::Rampage(..)
        | Keyword::Absorb(..)
        | Keyword::Crew { .. }
        | Keyword::Partner(..)
        | Keyword::Companion(..)
        | Keyword::Ninjutsu(..)
        | Keyword::CommanderNinjutsu(..)
        | Keyword::Prowl(..)
        | Keyword::Morph(..)
        | Keyword::Megamorph(..)
        | Keyword::Mayhem(..)
        | Keyword::Madness(..)
        | Keyword::Miracle(..)
        | Keyword::Dash(..)
        | Keyword::Emerge(..)
        | Keyword::Escape(..)
        | Keyword::Harmonize(..)
        | Keyword::Evoke(..)
        | Keyword::Foretell(..)
        | Keyword::Mutate(..)
        | Keyword::Disturb(..)
        | Keyword::Disguise(..)
        | Keyword::Blitz(..)
        | Keyword::Overload(..)
        | Keyword::Spectacle(..)
        | Keyword::Surge(..)
        | Keyword::Encore(..)
        | Keyword::Buyback(..)
        | Keyword::Casualty(..)
        | Keyword::Echo(..)
        | Keyword::Entwine(..)
        | Keyword::Outlast(..)
        | Keyword::Scavenge(..)
        | Keyword::Reinforce { .. }
        | Keyword::Fortify(..)
        | Keyword::Prototype { .. }
        | Keyword::Plot(..)
        | Keyword::Craft { .. }
        | Keyword::Offspring(..)
        | Keyword::Impending { .. }
        | Keyword::LevelUp(..)
        | Keyword::Affinity(..)
        | Keyword::CumulativeUpkeep(..)
        | Keyword::Banding
        | Keyword::BandsWithOther(..)
        | Keyword::Epic
        | Keyword::Fuse
        | Keyword::Gravestorm
        | Keyword::Haunt
        | Keyword::Hideaway(..)
        | Keyword::Improvise
        | Keyword::Ingest
        | Keyword::Melee
        | Keyword::Mentor
        | Keyword::Myriad
        | Keyword::Provoke
        | Keyword::Rebound
        | Keyword::Retrace
        | Keyword::Ripple(..)
        | Keyword::SplitSecond
        | Keyword::Storm
        | Keyword::Suspend { .. }
        | Keyword::Totem
        | Keyword::Warp(..)
        | Keyword::Sneak(..)
        | Keyword::WebSlinging(..)
        | Keyword::Mobilize(..)
        | Keyword::Gift(..)
        | Keyword::Discover(..)
        | Keyword::Spree
        | Keyword::Ravenous
        | Keyword::Daybound
        | Keyword::Nightbound
        | Keyword::Enlist
        | Keyword::ReadAhead
        | Keyword::Compleated
        | Keyword::Conspire
        | Keyword::Demonstrate
        | Keyword::Dethrone
        | Keyword::DoubleTeam
        | Keyword::LivingMetal
        | Keyword::Poisonous(..)
        | Keyword::Bloodthirst(..)
        | Keyword::Amplify(..)
        | Keyword::Graft(..)
        | Keyword::Devour(..)
        | Keyword::Toxic(..)
        | Keyword::Saddle(..)
        | Keyword::Teamwork(..)
        | Keyword::Soulshift(..)
        | Keyword::Backup(..)
        | Keyword::Squad(..)
        | Keyword::Typecycling { .. }
        | Keyword::Firebending(..)
        | Keyword::Splice { .. }
        | Keyword::Bargain
        | Keyword::Sunburst
        | Keyword::Champion(..)
        | Keyword::Training
        | Keyword::Assist
        | Keyword::Augment
        | Keyword::Aftermath
        | Keyword::JumpStart
        | Keyword::Cipher
        | Keyword::Transmute(..)
        | Keyword::Transfigure(..)
        | Keyword::Escalate(..)
        | Keyword::Recover(..)
        | Keyword::Cleave(..)
        | Keyword::Undaunted
        | Keyword::Paradigm
        | Keyword::Station
        | Keyword::Replicate(..)
        | Keyword::Awaken { .. }
        | Keyword::ForMirrodin
        | Keyword::MoreThanMeetsTheEye(..)
        | Keyword::Freerunning(..)
        | Keyword::Increment
        | Keyword::Specialize(..)
        | Keyword::Offering(..)
        | Keyword::Unknown(..) => {}
    }
}

fn walk_protection_target(
    target: &mut ProtectionTarget,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    match target {
        ProtectionTarget::Color(color) => cursor.color(category, color),
        // CR 702.16a: a filter-quality protection may embed a color/type predicate.
        ProtectionTarget::Filter(filter) => walk_target_filter(filter, category, cursor),
        ProtectionTarget::CardType(..)
        | ProtectionTarget::Quality(..)
        | ProtectionTarget::Multicolored
        | ProtectionTarget::ChosenColor
        | ProtectionTarget::ChosenCardType
        | ProtectionTarget::Everything
        | ProtectionTarget::FromPlayer(..) => {}
    }
}

fn walk_hexproof_filter(
    filter: &mut HexproofFilter,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    match filter {
        HexproofFilter::Color(color) => cursor.color(category, color),
        HexproofFilter::CardType(..)
        | HexproofFilter::Quality(..)
        | HexproofFilter::ChosenColor => {}
    }
}

fn walk_target_filter(
    filter: &mut TargetFilter,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    match filter {
        TargetFilter::Typed(typed) => walk_typed_filter(typed, category, cursor),
        TargetFilter::Not { filter } => walk_target_filter(filter, category, cursor),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            for f in filters.iter_mut() {
                walk_target_filter(f, category, cursor);
            }
        }
        TargetFilter::None
        | TargetFilter::Any
        | TargetFilter::Player
        | TargetFilter::Controller
        | TargetFilter::SelfRef
        | TargetFilter::GrantingObject
        | TargetFilter::SourceOrPaired
        | TargetFilter::StackAbility { .. }
        | TargetFilter::StackSpell
        | TargetFilter::SpecificObject { .. }
        | TargetFilter::SpecificPlayer { .. }
        | TargetFilter::PlayerWhoChoseLabel { .. }
        | TargetFilter::Neighbor { .. }
        | TargetFilter::ScopedPlayer
        | TargetFilter::AttachedTo
        | TargetFilter::LastCreated
        | TargetFilter::LastRevealed
        | TargetFilter::CostPaidObject
        | TargetFilter::ChosenCard
        | TargetFilter::TrackedSet { .. }
        | TargetFilter::TrackedSetFiltered { .. }
        | TargetFilter::ExiledBySource
        | TargetFilter::ExiledCardByIndex { .. }
        | TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSource
        | TargetFilter::EventTarget
        | TargetFilter::TriggeringSourceController
        | TargetFilter::ParentTarget
        | TargetFilter::ParentTargetSlot { .. }
        | TargetFilter::ParentTargetController
        | TargetFilter::ParentTargetOwner
        | TargetFilter::SourceChosenPlayer
        | TargetFilter::OriginalController
        | TargetFilter::OriginalSource
        | TargetFilter::PostReplacementSourceController
        | TargetFilter::PostReplacementDamageTarget
        | TargetFilter::PostReplacementDamageTargetOwner
        | TargetFilter::DefendingPlayer
        | TargetFilter::HasChosenName
        | TargetFilter::ChosenDamageSource { .. }
        | TargetFilter::Named { .. }
        | TargetFilter::Owner
        | TargetFilter::AllPlayers => {}
    }
}

fn walk_typed_filter(typed: &mut TypedFilter, category: TextWordCategory, cursor: &mut WordCursor) {
    for tf in typed.type_filters.iter_mut() {
        walk_type_filter(tf, category, cursor);
    }
    for prop in typed.properties.iter_mut() {
        walk_filter_prop(prop, category, cursor);
    }
}

fn walk_type_filter(filter: &mut TypeFilter, category: TextWordCategory, cursor: &mut WordCursor) {
    match filter {
        // CR 205.3: a subtype token may be a land type or creature type.
        TypeFilter::Subtype(s) => cursor.subtype(category, s),
        TypeFilter::Non(inner) => walk_type_filter(inner, category, cursor),
        TypeFilter::AnyOf(inner) => {
            for f in inner.iter_mut() {
                walk_type_filter(f, category, cursor);
            }
        }
        TypeFilter::Creature
        | TypeFilter::Land
        | TypeFilter::Artifact
        | TypeFilter::Enchantment
        | TypeFilter::Instant
        | TypeFilter::Sorcery
        | TypeFilter::Planeswalker
        | TypeFilter::Battle
        | TypeFilter::Kindred
        | TypeFilter::Permanent
        | TypeFilter::Card
        | TypeFilter::Any => {}
    }
}

fn walk_filter_prop(prop: &mut FilterProp, category: TextWordCategory, cursor: &mut WordCursor) {
    match prop {
        // CR 105 color-word predicates.
        FilterProp::HasColor { color } => cursor.color(category, color),
        FilterProp::NotColor { color } => cursor.color(category, color),
        FilterProp::CanEnchant { target } => walk_target_filter(target, category, cursor),
        FilterProp::AnyOf { props } => {
            for fp in props.iter_mut() {
                walk_filter_prop(fp, category, cursor);
            }
        }
        FilterProp::Not { prop } => walk_filter_prop(prop, category, cursor),
        FilterProp::WithKeyword { value } | FilterProp::WithoutKeyword { value } => {
            walk_keyword(value, category, cursor)
        }
        FilterProp::Counters { count, .. } => walk_quantity_expr(count, category, cursor),
        FilterProp::Cmc { value, .. } => walk_quantity_expr(value, category, cursor),
        FilterProp::PtComparison { value, .. } => walk_quantity_expr(value, category, cursor),
        // CR 612.2 + CR 107.4: `ColorCount` / `ManaSymbolCount` measure set size or
        // mana pips, not color WORDS — not text-changed. `IsChosenColor` reads a
        // chosen ref, not a printed word.
        FilterProp::Token
        | FilterProp::NonToken
        | FilterProp::ControllerChoseLabel { .. }
        | FilterProp::ControllerMatches { .. }
        | FilterProp::WasPlayed
        | FilterProp::Attacking { .. }
        | FilterProp::Blocking
        | FilterProp::BlockingSource
        | FilterProp::CombatRelation { .. }
        | FilterProp::Unblocked
        | FilterProp::AttackingAlone
        | FilterProp::BlockingAlone
        | FilterProp::Tapped
        | FilterProp::Untapped
        | FilterProp::IsSaddled
        | FilterProp::SaddledSource
        | FilterProp::ConvokedSource
        | FilterProp::ProtectorMatches { .. }
        | FilterProp::HasHasteOrControlledSinceTurnBegan
        | FilterProp::HasKeywordKind { .. }
        | FilterProp::WithoutKeywordKind { .. }
        | FilterProp::ManaValueParity { .. }
        | FilterProp::ManaCostIn { .. }
        | FilterProp::InZone { .. }
        | FilterProp::Owned { .. }
        | FilterProp::Foretold
        | FilterProp::EnchantedBy
        | FilterProp::EquippedBy
        | FilterProp::AttachedToSource
        | FilterProp::AttachedToRecipient
        | FilterProp::HasAttachment { .. }
        | FilterProp::HasAnyAttachmentOf { .. }
        | FilterProp::Another
        | FilterProp::Unpaired
        | FilterProp::OtherThanTriggerObject
        | FilterProp::PowerGTSource
        | FilterProp::ColorCount { .. }
        | FilterProp::ManaSymbolCount { .. }
        | FilterProp::HasSupertype { .. }
        | FilterProp::IsChosenCreatureType
        | FilterProp::MostPrevalentCreatureTypeIn { .. }
        | FilterProp::IsChosenColor
        | FilterProp::IsChosenCardType
        | FilterProp::MatchesLastChosenCardPredicate
        | FilterProp::HasSingleTarget
        | FilterProp::Modal
        | FilterProp::NotSupertype { .. }
        | FilterProp::Suspected
        | FilterProp::Renowned
        | FilterProp::ToughnessGTPower
        | FilterProp::PowerExceedsBase
        | FilterProp::InTrackedSet { .. }
        | FilterProp::Modified
        | FilterProp::Historic
        | FilterProp::NotHistoric
        | FilterProp::DifferentNameFrom { .. }
        | FilterProp::DistinctFrom { .. }
        | FilterProp::InAnyZone { .. }
        | FilterProp::SharesQuality { .. }
        | FilterProp::WasDealtDamageThisTurn
        | FilterProp::EnteredThisTurn
        | FilterProp::ControlledContinuouslySinceTurnBegan
        | FilterProp::ZoneChangedThisTurn { .. }
        | FilterProp::AttackedThisTurn { .. }
        | FilterProp::BlockedThisTurn
        | FilterProp::AttackedOrBlockedThisTurn
        | FilterProp::CountersPutOnThisTurn { .. }
        | FilterProp::FaceDown
        | FilterProp::Transformed
        | FilterProp::TargetsOnly { .. }
        | FilterProp::Targets { .. }
        | FilterProp::CouldBeTargetedByTriggeringSpell
        | FilterProp::HasXInManaCost
        | FilterProp::HasXInActivationCost
        | FilterProp::WasKicked
        | FilterProp::HasManaAbility
        | FilterProp::HasNoAbilities
        | FilterProp::Named { .. }
        | FilterProp::SameName
        | FilterProp::SameNameAsParentTarget
        | FilterProp::NameMatchesAnyPermanent { .. }
        | FilterProp::IsCommander
        | FilterProp::SharesCreatureTypeWithCommander
        // CR 612.2: structural "represented by a card" predicate — no color/land/
        // creature-type word to change.
        | FilterProp::RepresentedByCard
        | FilterProp::Other { .. } => {}
    }
}

fn walk_static_condition(
    condition: &mut StaticCondition,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    match condition {
        // CR 700.5: devotion text spells the color WORD (contrast the {R}-pip no-op).
        StaticCondition::DevotionGE { colors, .. } => {
            for c in colors.iter_mut() {
                cursor.color(category, c);
            }
        }
        StaticCondition::QuantityComparison { lhs, rhs, .. } => {
            walk_quantity_expr(lhs, category, cursor);
            walk_quantity_expr(rhs, category, cursor);
        }
        StaticCondition::And { conditions } | StaticCondition::Or { conditions } => {
            for c in conditions.iter_mut() {
                walk_static_condition(c, category, cursor);
            }
        }
        StaticCondition::Not { condition } => walk_static_condition(condition, category, cursor),
        StaticCondition::IsPresent { filter } => {
            if let Some(f) = filter {
                walk_target_filter(f, category, cursor);
            }
        }
        StaticCondition::DefendingPlayerControls { filter }
        | StaticCondition::SourceMatchesFilter { filter }
        | StaticCondition::TopOfLibraryMatches { filter }
        | StaticCondition::RecipientMatchesFilter { filter } => {
            walk_target_filter(filter, category, cursor)
        }
        StaticCondition::ChosenColorIs { .. }
        | StaticCondition::ChosenLabelIs { .. }
        | StaticCondition::HasMaxSpeed
        | StaticCondition::SpeedGE { .. }
        | StaticCondition::DayNightIs { .. }
        | StaticCondition::HasCounters { .. }
        | StaticCondition::CastVariantPaid { .. }
        | StaticCondition::RecipientHasCounters { .. }
        | StaticCondition::ClassLevelGE { .. }
        | StaticCondition::SourceAttackingAlone
        | StaticCondition::SourceIsAttacking
        | StaticCondition::SourceIsBlocking
        | StaticCondition::SourceIsBlocked
        | StaticCondition::IsMonarch
        | StaticCondition::IsInitiative
        | StaticCondition::NoMonarch
        | StaticCondition::HasCityBlessing
        | StaticCondition::CompletedADungeon
        | StaticCondition::WasStartingPlayer { .. }
        | StaticCondition::SpellCastWithVariantThisTurn { .. }
        | StaticCondition::OpponentPoisonAtLeast { .. }
        | StaticCondition::UnlessPay { .. }
        | StaticCondition::Unrecognized { .. }
        | StaticCondition::DuringYourTurn
        | StaticCondition::SharesColorWithMostCommonColorAmongPermanents
        | StaticCondition::SourceEnteredThisTurn
        | StaticCondition::SourceHasDealtDamage
        | StaticCondition::WasCast { .. }
        | StaticCondition::IsRingBearer
        | StaticCondition::RingLevelAtLeast { .. }
        | StaticCondition::ControlsCommander { .. }
        | StaticCondition::SourceIsTapped
        | StaticCondition::IsTapped { .. }
        | StaticCondition::SourceIsFaceUp
        | StaticCondition::SourceIsSaddled
        | StaticCondition::SourceControllerEquals { .. }
        | StaticCondition::SourceIsEquipped
        | StaticCondition::SourceIsEnchanted
        | StaticCondition::SourceIsMonstrous
        | StaticCondition::SourceIsHarnessed
        | StaticCondition::SourceAttachedToCreature
        | StaticCondition::RecipientAttackingOwnerTarget { .. }
        | StaticCondition::SourceIsPaired
        | StaticCondition::SourceInZone { .. }
        | StaticCondition::EnchantedIsFaceDown
        | StaticCondition::AdditionalCostPaid
        | StaticCondition::CastingAsVariant { .. }
        | StaticCondition::None => {}
    }
}

fn walk_quantity_expr(
    expr: &mut QuantityExpr,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    match expr {
        QuantityExpr::Ref { qty } => walk_quantity_ref(qty, category, cursor),
        QuantityExpr::Fixed { .. } => {}
        QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. } => walk_quantity_expr(inner, category, cursor),
        QuantityExpr::UpTo { max } => walk_quantity_expr(max, category, cursor),
        QuantityExpr::Power { exponent, .. } => walk_quantity_expr(exponent, category, cursor),
        QuantityExpr::Difference { left, right } => {
            walk_quantity_expr(left, category, cursor);
            walk_quantity_expr(right, category, cursor);
        }
        QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => {
            for e in exprs.iter_mut() {
                walk_quantity_expr(e, category, cursor);
            }
        }
    }
}

fn walk_quantity_ref(qty: &mut QuantityRef, category: TextWordCategory, cursor: &mut WordCursor) {
    match qty {
        // CR 700.5: devotion to fixed colors spells color WORDS.
        QuantityRef::Devotion { colors } => match colors {
            DevotionColors::Fixed(v) => {
                for c in v.iter_mut() {
                    cursor.color(category, c);
                }
            }
            DevotionColors::ChosenColor => {}
        },
        QuantityRef::ObjectCount { filter }
        | QuantityRef::ObjectCountDistinct { filter, .. }
        | QuantityRef::ObjectCountBySharedQuality { filter, .. }
        | QuantityRef::CountersOnObjects { filter, .. }
        | QuantityRef::ControlledByEachPlayer { filter, .. }
        | QuantityRef::EnteredThisTurn { filter }
        | QuantityRef::SacrificedThisTurn { filter, .. }
        | QuantityRef::BattlefieldEntriesThisTurn { filter, .. }
        | QuantityRef::ZoneChangeCountThisTurn { filter, .. }
        | QuantityRef::TokensCreatedThisTurn { filter, .. }
        | QuantityRef::DistinctColorsAmongPermanents { filter }
        | QuantityRef::DistinctCounterKindsAmong { filter } => {
            walk_target_filter(filter, category, cursor)
        }
        QuantityRef::Aggregate {
            filter, property, ..
        }
        | QuantityRef::ZoneChangeAggregateThisTurn {
            filter, property, ..
        } => {
            walk_target_filter(filter, category, cursor);
            walk_object_property(property, category, cursor);
        }
        QuantityRef::TrackedSetAggregate { property, .. } => {
            walk_object_property(property, category, cursor)
        }
        QuantityRef::CounterAddedThisTurn { target, .. } => {
            walk_target_filter(target, category, cursor)
        }
        // Box<TargetFilter> fields need an explicit deref (a `|`-group binding
        // cannot mix `&mut TargetFilter` with `&mut Box<TargetFilter>`).
        QuantityRef::FilteredTrackedSetSize { filter, .. }
        | QuantityRef::TargetObjectManaValue { filter } => {
            walk_target_filter(filter, category, cursor)
        }
        QuantityRef::DamageDealtThisTurn { source, target, .. } => {
            walk_target_filter(source, category, cursor);
            walk_target_filter(target, category, cursor);
        }
        QuantityRef::ZoneCardCount {
            card_types, filter, ..
        } => {
            for tf in card_types.iter_mut() {
                walk_type_filter(tf, category, cursor);
            }
            if let Some(f) = filter {
                walk_target_filter(f, category, cursor);
            }
        }
        QuantityRef::SpellsCastThisTurn { filter, .. }
        | QuantityRef::AttackedThisTurn { filter, .. }
        | QuantityRef::SpellsCastThisGame { filter, .. } => {
            if let Some(f) = filter {
                walk_target_filter(f, category, cursor);
            }
        }
        QuantityRef::HandSize { .. }
        | QuantityRef::LifeTotal { .. }
        | QuantityRef::GraveyardSize { .. }
        | QuantityRef::LifeAboveStarting
        | QuantityRef::StartingLifeTotal
        | QuantityRef::TriggeringDiscoverValue
        | QuantityRef::PlayerCount { .. }
        | QuantityRef::CountersOn { .. }
        | QuantityRef::PlayerCounter { .. }
        | QuantityRef::TargetControllerCounter { .. }
        | QuantityRef::Variable { .. }
        | QuantityRef::Power { .. }
        | QuantityRef::Intensity { .. }
        | QuantityRef::Toughness { .. }
        | QuantityRef::ObjectManaValue { .. }
        | QuantityRef::ObjectColorCount { .. }
        | QuantityRef::ObjectNameWordCount { .. }
        | QuantityRef::ObjectTypelineComponentCount { .. }
        | QuantityRef::ManaSymbolsInManaCost { .. }
        | QuantityRef::SelfManaValue
        | QuantityRef::TargetZoneCardCount { .. }
        | QuantityRef::DistinctCardTypes { .. }
        | QuantityRef::DistinctSubtypes { .. }
        | QuantityRef::CardsExiledBySource
        | QuantityRef::ExiledCardPower { .. }
        | QuantityRef::BasicLandTypeCount { .. }
        | QuantityRef::TrackedSetSize
        | QuantityRef::ExiledFromHandThisResolution
        | QuantityRef::PreviousEffectAmount { .. }
        | QuantityRef::LifeLostThisTurn { .. }
        | QuantityRef::PartySize { .. }
        | QuantityRef::UnspentMana { .. }
        | QuantityRef::Speed { .. }
        | QuantityRef::EventContextAmount
        | QuantityRef::AttachmentsOnLeavingObject { .. }
        | QuantityRef::EventContextSourceCostX
        | QuantityRef::CrimesCommittedThisTurn
        | QuantityRef::BendTypesThisTurn
        | QuantityRef::LifeGainedThisTurn { .. }
        | QuantityRef::CardsDrawnThisTurn { .. }
        | QuantityRef::LandsPlayedThisTurn { .. }
        | QuantityRef::TurnsTaken
        | QuantityRef::ChosenNumber
        | QuantityRef::DescendedThisTurn
        | QuantityRef::LoyaltyAbilitiesActivatedThisTurn { .. }
        | QuantityRef::SpellsCastLastTurn
        | QuantityRef::CardsDiscardedThisTurn { .. }
        | QuantityRef::PlayerActionsThisTurn { .. }
        | QuantityRef::DungeonsCompleted
        | QuantityRef::CostXPaid
        | QuantityRef::KickerCount
        | QuantityRef::AdditionalCostPaymentCount
        | QuantityRef::AdditionalCostPaymentCountFor { .. }
        | QuantityRef::ConvokedCreatureCount
        | QuantityRef::TimesCostPaidThisResolution
        | QuantityRef::ManaSpentToCast { .. }
        | QuantityRef::ColorsInCommandersColorIdentity
        | QuantityRef::CommanderCastFromCommandZoneCount
        | QuantityRef::CommanderManaValue { .. }
        | QuantityRef::VoteCount { .. } => {}
    }
}

/// CR 612.2 + CR 107.4: object properties reference power/toughness/mana value or
/// a mana SYMBOL count — none is a color/land/creature WORD. All no-op; exists so
/// a future word-bearing `ObjectProperty` variant must be classified.
fn walk_object_property(
    property: &mut ObjectProperty,
    _category: TextWordCategory,
    _cursor: &mut WordCursor,
) {
    match property {
        ObjectProperty::Power
        | ObjectProperty::Toughness
        | ObjectProperty::ManaValue
        | ObjectProperty::ManaSymbolCount(..) => {}
    }
}

fn walk_ability_definition(
    ability: &mut AbilityDefinition,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    walk_effect(&mut ability.effect, category, cursor);
    if let Some(sub) = &mut ability.sub_ability {
        walk_ability_definition(sub, category, cursor);
    }
    if let Some(else_ability) = &mut ability.else_ability {
        walk_ability_definition(else_ability, category, cursor);
    }
    for mode in ability.mode_abilities.iter_mut() {
        walk_ability_definition(mode, category, cursor);
    }
    if let Some(repeat) = &mut ability.repeat_for {
        walk_quantity_expr(repeat, category, cursor);
    }
}

fn walk_trigger_definition(
    trigger: &mut TriggerDefinition,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    if let Some(execute) = &mut trigger.execute {
        walk_ability_definition(execute, category, cursor);
    }
    if let Some(valid_card) = &mut trigger.valid_card {
        walk_target_filter(valid_card, category, cursor);
    }
}

fn walk_static_definition(
    static_def: &mut StaticDefinition,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    if let Some(affected) = &mut static_def.affected {
        walk_target_filter(affected, category, cursor);
    }
    if let Some(condition) = &mut static_def.condition {
        walk_static_condition(condition, category, cursor);
    }
    for modification in static_def.modifications.iter_mut() {
        walk_continuous_modification(modification, category, cursor);
    }
}

fn walk_continuous_modification(
    modification: &mut ContinuousModification,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    match modification {
        ContinuousModification::SetColor { colors } => {
            for c in colors.iter_mut() {
                cursor.color(category, c);
            }
        }
        ContinuousModification::AddColor { color } => cursor.color(category, color),
        ContinuousModification::SetBasicLandType { land_type } => {
            cursor.basic_land_type(category, land_type)
        }
        ContinuousModification::AddSubtype { subtype }
        | ContinuousModification::RemoveSubtype { subtype } => cursor.subtype(category, subtype),
        ContinuousModification::AddKeyword { keyword }
        | ContinuousModification::RemoveKeyword { keyword } => {
            walk_keyword(keyword, category, cursor)
        }
        ContinuousModification::GrantAbility { definition } => {
            walk_ability_definition(definition, category, cursor)
        }
        ContinuousModification::GrantStaticAbility { definition } => {
            walk_static_definition(definition, category, cursor)
        }
        ContinuousModification::GrantTrigger { trigger } => {
            walk_trigger_definition(trigger, category, cursor)
        }
        ContinuousModification::GrantAllActivatedAbilitiesOf { source, .. }
        | ContinuousModification::GrantAllTriggeredAbilitiesOf { source } => {
            walk_target_filter(source, category, cursor)
        }
        ContinuousModification::SetDynamicPower { value }
        | ContinuousModification::SetDynamicToughness { value }
        | ContinuousModification::SetPowerDynamic { value }
        | ContinuousModification::SetToughnessDynamic { value }
        | ContinuousModification::AddDynamicPower { value }
        | ContinuousModification::AddDynamicToughness { value }
        | ContinuousModification::AddDynamicKeyword { value, .. } => {
            walk_quantity_expr(value, category, cursor)
        }
        ContinuousModification::CopyValues { .. }
        | ContinuousModification::SetName { .. }
        | ContinuousModification::AddPower { .. }
        | ContinuousModification::AddToughness { .. }
        | ContinuousModification::SetPower { .. }
        | ContinuousModification::SetToughness { .. }
        | ContinuousModification::RemoveAllAbilities
        | ContinuousModification::AddType { .. }
        | ContinuousModification::RemoveType { .. }
        | ContinuousModification::SetCardTypes { .. }
        | ContinuousModification::RemoveAllSubtypes { .. }
        | ContinuousModification::AddKeywordWithDerivedCost { .. }
        | ContinuousModification::AddAllCreatureTypes
        | ContinuousModification::AddAllBasicLandTypes
        | ContinuousModification::AddAllLandTypes
        | ContinuousModification::AddChosenSubtype { .. }
        | ContinuousModification::AddChosenColor { .. }
        | ContinuousModification::RemoveChosenKeyword
        | ContinuousModification::AddChosenKeyword
        | ContinuousModification::AddStaticMode { .. }
        | ContinuousModification::SwitchPowerToughness
        | ContinuousModification::AssignDamageFromToughness
        | ContinuousModification::AssignDamageAsThoughUnblocked
        | ContinuousModification::AssignNoCombatDamage
        | ContinuousModification::ChangeController
        | ContinuousModification::SetChosenBasicLandType
        | ContinuousModification::SetChosenName
        // CR 612.1: a nested text-word replacement carries concrete operands, not
        // printed words used as words on this object.
        | ContinuousModification::ReplaceTextWord { .. }
        | ContinuousModification::RetainPrintedTriggerFromSource { .. }
        | ContinuousModification::RetainPrintedAbilityFromSource { .. }
        | ContinuousModification::AddSupertype { .. }
        | ContinuousModification::RemoveSupertype { .. }
        | ContinuousModification::AddCounterOnEnter { .. }
        | ContinuousModification::SetStartingLoyalty { .. }
        | ContinuousModification::RemoveManaCost => {}
    }
}

/// CR 612.1: Walk the word-bearing children of an ability's effect. Descends into
/// nested-ability composites (so granted statics/keywords/subtype filters are
/// reached) and the two nested-effect replacement builders.
///
/// CR 612.2: a color word / creature type / land type can also live inside a
/// leaf effect's own target/source `TargetFilter` (e.g. "{T}: Destroy target red
/// creature", "Pump target Zombie", a "target Zombie ... gains ..." grant). That
/// instance is reached first, through the shared [`Effect::target_filter_mut`]
/// accessor + the same [`walk_target_filter`] traversal used for
/// `StaticDefinition.affected` — so a text-changing effect offers and rewrites it
/// too. `target_filter_mut` classification mirrors `Effect::target_filter`, so
/// mass-population filters (`DestroyAll`/`PumpAll`/etc.) and the alternate
/// `CopyTokenOf`/`Token` owner axis are surfaced only where that targeting-layer
/// accessor surfaces them; no covered card changes a word inside a mass filter,
/// and coverage there stays red rather than silently mis-substituting.
fn walk_effect(effect: &mut Effect, category: TextWordCategory, cursor: &mut WordCursor) {
    // Reach a word embedded in this effect's own declared target/source filter
    // before dispatching the structural / nested-ability arms below. The borrow
    // ends with the `if let`, so the `match effect` re-borrow is disjoint.
    if let Some(filter) = effect.target_filter_mut() {
        walk_target_filter(filter, category, cursor);
    }
    match effect {
        Effect::CreateDrawReplacement { replacement_effect }
        | Effect::CreatePlaneswalkReplacement { replacement_effect } => {
            walk_effect(replacement_effect, category, cursor)
        }
        Effect::CreateDelayedTrigger { effect, .. } => {
            walk_ability_definition(effect, category, cursor)
        }
        Effect::FlipCoin {
            win_effect,
            lose_effect,
            ..
        }
        | Effect::FlipCoins {
            win_effect,
            lose_effect,
            ..
        } => {
            if let Some(w) = win_effect {
                walk_ability_definition(w, category, cursor);
            }
            if let Some(l) = lose_effect {
                walk_ability_definition(l, category, cursor);
            }
        }
        Effect::FlipCoinUntilLose { win_effect } => {
            walk_ability_definition(win_effect, category, cursor)
        }
        Effect::RollDie { results, .. } => {
            for branch in results.iter_mut() {
                walk_ability_definition(&mut branch.effect, category, cursor);
            }
        }
        Effect::ChooseOneOf { branches, .. } => {
            for branch in branches.iter_mut() {
                walk_ability_definition(branch, category, cursor);
            }
        }
        Effect::Vote {
            per_choice_effect, ..
        } => {
            for sub in per_choice_effect.iter_mut() {
                walk_ability_definition(sub, category, cursor);
            }
        }
        Effect::SeparateIntoPiles {
            chosen_pile_effect,
            unchosen_pile_effect,
            ..
        } => {
            walk_ability_definition(chosen_pile_effect, category, cursor);
            if let Some(unchosen) = unchosen_pile_effect {
                walk_ability_definition(unchosen, category, cursor);
            }
        }
        Effect::RevealFromHand { on_decline, .. } => {
            if let Some(sub) = on_decline {
                walk_ability_definition(sub, category, cursor);
            }
        }
        Effect::GenericEffect {
            static_abilities, ..
        }
        | Effect::Token {
            static_abilities, ..
        } => {
            for static_def in static_abilities.iter_mut() {
                walk_static_definition(static_def, category, cursor);
            }
        }
        Effect::CreateEmblem { statics, triggers } => {
            for static_def in statics.iter_mut() {
                walk_static_definition(static_def, category, cursor);
            }
            for trigger in triggers.iter_mut() {
                walk_trigger_definition(trigger, category, cursor);
            }
        }
        Effect::ChangeTextWords { .. }
        | Effect::StartYourEngines { .. }
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
        | Effect::ChangeZone { .. }
        | Effect::ChangeZoneAll { .. }
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
        | Effect::BecomeMonarch
        | Effect::NoOp
        | Effect::Proliferate
        | Effect::ProliferateTarget { .. }
        | Effect::Populate
        | Effect::Clash
        | Effect::Behold { .. }
        | Effect::EndTheTurn
        | Effect::EndCombatPhase
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
        | Effect::Animate { .. }
        | Effect::ReturnAsAura { .. }
        | Effect::RegisterBending { .. }
        | Effect::Cleanup { .. }
        | Effect::Mana { .. }
        | Effect::Discard { .. }
        | Effect::Shuffle { .. }
        | Effect::Transform { .. }
        | Effect::SearchLibrary { .. }
        | Effect::SearchOutsideGame { .. }
        | Effect::RevealHand { .. }
        | Effect::Reveal { .. }
        | Effect::RevealTop { .. }
        | Effect::ExileTop { .. }
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
        | Effect::AddTargetReplacement { .. }
        | Effect::AddRestriction { .. }
        | Effect::ReduceNextSpellCost { .. }
        | Effect::GrantNextSpellAbility { .. }
        | Effect::AddPendingETBCounters { .. }
        | Effect::AddPendingEntersModifications { .. }
        | Effect::PayCost { .. }
        | Effect::CastFromZone { .. }
        | Effect::FreeCastFromZones { .. }
        | Effect::ExileResolvingSpellInsteadOfGraveyard { .. }
        | Effect::PreventDamage { .. }
        | Effect::CreateDamageReplacement { .. }
        | Effect::LoseTheGame { .. }
        | Effect::WinTheGame { .. }
        | Effect::RingTemptsYou
        | Effect::VentureIntoDungeon
        | Effect::VentureInto { .. }
        | Effect::TakeTheInitiative
        | Effect::Planeswalk
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
        | Effect::RuntimeHandled { .. }
        | Effect::Incubate { .. }
        | Effect::Amass { .. }
        | Effect::Monstrosity { .. }
        | Effect::Specialize
        | Effect::Renown { .. }
        | Effect::Bolster { .. }
        | Effect::Adapt { .. }
        | Effect::Learn
        | Effect::Forage
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
        | Effect::Unimplemented { .. } => {}
    }
}
