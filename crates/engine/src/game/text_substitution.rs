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
//! CR 612.2a is the one documented exception to the name exclusion, and it is
//! about a token a spell/ability CREATES, not about the object's own name:
//! "Most spells and abilities that create creature tokens use creature types to
//! define both the creature types and the names of the tokens. A text-changing
//! effect that affects such a spell or an object with such an ability can change
//! these words because they're being used as creature types, even though they're
//! also being used as names." So a token-creation spec's creature-type words are
//! rewritten in BOTH the token's types and its name — see
//! [`WordCursor::token_spec`]. The object's OWN name is still never touched.
//!
//! Every enum match here is exhaustive with no `_` wildcard: a future
//! word-bearing variant fails to compile until it is classified as a carrier,
//! a recursion point, or an explicit no-op.

use std::collections::BTreeSet;
use std::str::FromStr;
use std::sync::Arc;

use crate::game::game_object::GameObject;
use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, ActivationRestriction,
    AttackersDeclaredCountSubject, BasicLandType, CardTypeSetSource, ContinuousModification,
    CostReduction, CounterSourceRider, DelayedTriggerCondition, DevotionColors, Duration, Effect,
    ExiledSpellRider, FilterProp, ObjectProperty, ParsedCondition, PlayerFilter, PtValue,
    QuantityExpr, QuantityRef, RepeatContinuation, ReplacementCondition, ReplacementDefinition,
    ReplacementMode, StaticCondition, StaticDefinition, TargetFilter, TextWord, TextWordCategory,
    TriggerCondition, TriggerConstraint, TriggerDefinition, TypeFilter, TypedFilter,
    UnlessPayModifier, UntilCondition, VoteSubject,
};
use crate::types::keywords::{HexproofFilter, Keyword, ProtectionTarget};
use crate::types::mana::ManaColor;
use crate::types::statics::{
    BlockExceptionKind, CostPaymentProhibition, HandSizeModification, StaticMode,
};

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

    /// CR 612.2a + CR 612.4: visit a token-creation spec's declared subtype list
    /// and the token NAME those subtypes define.
    ///
    /// CR 612.2 normally protects names ("An effect that changes a color word or
    /// a subtype can't change a card name"), but CR 612.2a is the explicit
    /// carve-out: "Most spells and abilities that create creature tokens use
    /// creature types to define both the creature types and the names of the
    /// tokens. A text-changing effect that affects such a spell or an object with
    /// such an ability can change these words because they're being used as
    /// creature types, even though they're also being used as names." So after
    /// Artificial Evolution changes Goblin to Elf, a "create a 1/1 red Goblin
    /// creature token" ability creates an Elf token *named* Elf.
    ///
    /// `types` routes through the shared [`Self::subtype`] path, so the walk's
    /// `category` still disambiguates land types from creature types and the
    /// resolver's live-creature-type intersection still applies.
    ///
    /// The `name` rewrite is deliberately narrower than the `types` rewrite:
    /// - it applies ONLY under [`TextWordCategory::CreatureType`], because
    ///   CR 612.2a names creature types specifically — a color word or basic land
    ///   type appearing in a token's name is still a NAME under CR 612.2;
    /// - it applies ONLY to name words the same spec also declares in `types`,
    ///   which is exactly CR 612.2a's "use creature types to define ... the
    ///   names" condition. A predefined token name that is not backed by a
    ///   declared creature type (Treasure, Food, a Role name) is a name used as a
    ///   name and stays untouched.
    ///
    /// Collection needs no name pass: every word the name can legally contribute
    /// is, by that same condition, already declared in `types`.
    fn token_spec(&mut self, category: TextWordCategory, name: &mut String, types: &mut [String]) {
        // Snapshot the PRINTED type words before substituting, so the CR 612.2a
        // "the name is defined by these creature types" test reads the spec as
        // printed rather than as already-rewritten.
        let declared: Vec<String> = types.to_vec();
        for token_type in types.iter_mut() {
            self.subtype(category, token_type);
        }
        if category != TextWordCategory::CreatureType {
            return;
        }
        let WordCursor::Replace { from, to } = self else {
            return;
        };
        let (TextWord::CreatureType(f), TextWord::CreatureType(t)) = (&**from, &**to) else {
            return;
        };
        if !declared.iter().any(|d| d == f) {
            return;
        }
        if name.split_whitespace().any(|word| word == f) {
            *name = name
                .split_whitespace()
                .map(|word| if word == f { t.as_str() } else { word })
                .collect::<Vec<_>>()
                .join(" ");
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
/// CR 612.1 + CR 118.12: ability *costs* (`AbilityDefinition.cost` — Goblin
/// Chirurgeon's "Sacrifice a Goblin"), the resolution `condition`
/// (`AbilityCondition` — "if you control a Goblin"), the `unless_pay` modifier,
/// the `repeat_until` loop predicate, the self-referential `cost_reduction`
/// (count + `ParsedCondition` gate), the `activation_restrictions`
/// (`ParsedCondition` gates), the `activator_filter` / `player_scope`
/// (`PlayerFilter` roots), and the `target_chooser` / `announced_x` roots ARE
/// walked (see [`walk_ability_cost`] / [`walk_ability_condition`] /
/// [`walk_cost_reduction`] / [`walk_activation_restriction`] /
/// [`walk_player_filter`]). Trigger event-shape filters (`valid_card` and its
/// `valid_target` / `valid_source` / `valid_subject_player` siblings), a
/// trigger's `unless_pay`, its intervening-if `condition` (`TriggerCondition`),
/// and its rate-limit `constraint` (`TriggerConstraint`) are walked too. The
/// `PlayerFilter` root is descended into wherever it appears
/// (`AbilityCondition::ScopedPlayerMatches`, `TriggerCondition::DuringPlayersTurn`,
/// `player_scope`, `activator_filter`, and nested self-composed anchors). The
/// static `per_player_condition` (`ParsedCondition`) is walked
/// (see [`walk_static_definition`] / [`walk_parsed_condition`]).
///
/// CR 614.1: replacement effects (`replacement_definitions`, Root 6) are walked —
/// their event filter, applicability `ReplacementCondition`, resulting ability,
/// damage source / redirect filters, AND their `mode`'s optional/pay-cost
/// `decline` continuation (see [`walk_replacement_definition`] /
/// [`walk_replacement_mode`]).
/// CR 611.2b: a "for as long as [condition]" `Duration` (on `AbilityDefinition`
/// and on the duration-bearing `Effect` variants) is walked via [`walk_duration`].
///
/// CR 603.7 + CR 611.2: cross-referenced wrapper enums that embed a walked carrier
/// are recursed too — a delayed trigger's firing `condition`
/// (`DelayedTriggerCondition`, on `CreateDelayedTrigger` and the Feather-style
/// `ExiledSpellRider` return timing) and a counter effect's `source_rider`
/// (`CounterSourceRider::LosesAbilities` — the installed `StaticDefinition` +
/// `Duration`) name their object class in a filter / granted static.
///
/// Every reachable word-bearing carrier field on a battlefield `GameObject`'s
/// live characteristics is now recursed (a mechanical per-carrier-type sweep of
/// the AST — see the module test-suite). The surfaces below are the ONLY roots
/// left un-walked, and each is genuinely wordless (a pip / marker / core-type,
/// not a color/land/creature WORD used as such), structurally excluded by CR 612,
/// unreachable for battlefield permanents, or a deliberately-red secondary filter:
/// - STRUCTURAL (CR 612.2): the object's name / base name, its Layer-5 `color`
///   field, and its mana cost / mana-symbol pips — not descended into;
/// - PIPS, not color WORDS (CR 107.4): `QuantityRef::ManaSymbolsInManaCost`,
///   `FilterProp::ManaSymbolCount`, `StaticMode::PayLifeAsColoredMana`, the
///   replacement `mana_modification`, and every `AbilityCost::Mana` / keyword
///   mana-symbol cost — a `{R}` symbol is not the word "red";
/// - MARKER / KIND enums, not printed words: `KeywordKind`
///   (`FilterProp::HasKeywordKind`, `AbilityCost::KeywordCostOfCastSpell`,
///   `StaticMode::AlternativeKeywordCost`), `CoreType`
///   (`TriggerCondition::WasType`, `ReplacementCondition::TokenCoreTypeMatches`),
///   counter kinds (CR 122.1), `SubtypeSet` / `ChosenSubtypeKind` (bulk "all
///   creature types" markers, no single spelled subtype);
/// - the `base_*` printed baselines (`base_abilities`, `base_keywords`,
///   `base_card_types`, `base_trigger/replacement/static_definitions`, …): the
///   layer system re-seeds the live roots from these each pass and re-applies the
///   swap on the live copy (CR 613.1c), so walking the baselines would double-
///   apply — they are deliberately not descended into;
/// - alternate-face / alternate-cast characteristic sets that are NOT the object's
///   current battlefield characteristics: `back_face` (DFC other face),
///   `specialize_faces` (Alchemy specialize faces), `cleave_variant` (spell-only
///   alternate ability set, always `None` on a battlefield permanent) — CR 612
///   changes the current characteristics only;
/// - `perpetual_mods` (digital-only Alchemy perpetual edits), `stickers`
///   (name/art/P-T stickers — no color/land/creature WORD), and
///   `token_rules_text` (display-only alt text);
/// - EVERY leaf-`Effect` word carrier IS now walked ([`walk_effect`] is an
///   exhaustive `_`-free per-variant match): a leaf effect's own declared
///   target/source filter (via `Effect::target_filter_mut`) AND all its SECONDARY
///   carriers — a secondary `TargetFilter` (`Fight.subject`, `Attach.attachment`,
///   `Behold.filter`, `SearchLibrary.filter`, `MoveCounters.source`, `PayCost.payer`,
///   `ExchangeControl.target_a/b`, `ReturnAsAura.enchant_filter`, …), a subtype
///   `String`/`Vec<String>` (`Amass.subtype`, `Animate.types`/`remove_types`), a
///   `Keyword` (`Animate.keywords`), a `Vec<ContinuousModification>`
///   (`CopySpell.additional_modifications`, `ReturnAsAura.grants`,
///   `AddPendingEntersModifications`, `EachPlayerCopyChosen.copy_modifications`),
///   an `AbilityCost` (`PayCost.cost` — "Sacrifice a Goblin"), a `PlayerFilter`
///   (`StartYourEngines`/`ChangeSpeed.player_scope`, `DamageEachPlayer.player_filter`,
///   `Conjure.library_players`, `ChooseOneOf.chooser`), a `QuantityExpr`
///   count/amount (`Draw.count`, `DealDamage.amount`, `Discover.mana_value_limit`,
///   …) or `PtValue`-wrapped quantity (`Pump.power/toughness`, `Animate.power`),
///   and `UntilCondition::NextMatches` (`ExileFromTopUntil.until`);
/// - CR 612.2a CARVE-OUT — token-creation specs ARE walked, name included. CR
///   612.2 says a subtype/color change "can't change a card name", but CR 612.2a
///   is the explicit exception: "Most spells and abilities that create creature
///   tokens use creature types to define both the creature types and the names of
///   the tokens. A text-changing effect that affects such a spell or an object
///   with such an ability can change these words because they're being used as
///   creature types, even though they're also being used as names." So
///   `Effect::Token`'s `name` + `types` (and `colors` / `keywords` / `count` /
///   `enter_with_counters` / `static_abilities`), the replacement-side
///   `TokenSpec.characteristics.display_name` + `.subtypes`
///   (`additional_token_spec` / `ensure_token_specs`, see [`walk_token_spec`]),
///   and the face-down body `FaceDownProfile.subtypes` on
///   `ChangeZone`/`ChangeZoneAll`/`Manifest`/`TurnFaceDown` (CR 708.2a, see
///   [`walk_face_down_profile`]) are all rewritten under the same category
///   discipline as a printed type line. The name half of the carve-out is
///   deliberately narrower than the type half — see [`WordCursor::token_spec`].
///   `CopyTokenOf` / `CreateTokenCopyFromPool` take their name and types from the
///   COPIED object (CR 707.2) and print none of their own, so CR 612.2a has
///   nothing to reach there; their printed carriers (`source_filter`, `count`,
///   the "except it has …" `extra_keywords` / `additional_modifications`,
///   `type_filter`, `mv_bound`) are walked instead;
/// - the ONLY deliberately-red `Effect` surfaces (coverage stays red rather than
///   silently mis-substituting; no covered card changes a word inside one):
///   the mass-population object `target`/`filter` of every `*All` /
///   population effect (`DestroyAll`/`PumpAll`/`DamageAll.target`/
///   `ChangeZoneAll.target`/`BounceAll.target`/`CounterAll`/`GainControlAll`/
///   `GoadAll`/`ExploreAll`/… — but a non-object carrier on the SAME effect, e.g.
///   `PumpAll.power`/`DamageAll.amount`/`ChangeZoneAll.enter_with_counters`, IS
///   walked), `PreventDamage.damage_source_filter`, `CastFromZone.alt_ability_cost`,
///   the `EpicCopy` resolved-spell snapshot, the specialized non-listed sub-enums
///   (`DamageTargetFilter`/`DamageRedirectTarget`, `GuessSubject`,
///   `PerpetualModification`, `IntensityScope`, `ForEachCategoryAction`,
///   name/label `String`s), the Forge-script token identifier
///   (`TokenSpec.script_name` — a serialized import identifier, not printed rules
///   text), and the replacement `runtime_execute` field
///   (see [`walk_replacement_definition`]);
/// - alternative / additional CAST-cost riders on keywords and statics
///   (`AbilityCost` on Evoke/Bestow/…, `StaticMode::CastWithAlternativeCost` /
///   `ImposeAdditionalCost.cost` / `AlternativeKeywordCost.cost` / permission
///   `alt_cost` / `extra_cost`) — a casting cost no covered card text-changes;
/// - the static `attack_defended` field and `StaticCondition::UnlessPay.defended`
///   (an `AttackTargetFilter` — a player / planeswalker / battle defended-scope,
///   no color/land/creature WORD). The static `mode` (`StaticMode`) IS walked
///   (see [`walk_static_mode`]), so its evasion / protection / cost-filter / color
///   params — including a granted `AddStaticMode` and a dynamic `MaximumHandSize`
///   — are covered;
/// - the `CastingRestriction` / `SpellCastingOption` / `CastingPermission`
///   `ParsedCondition` / `Duration` roots, which live on card-level casting
///   options rather than on any battlefield-`GameObject` ability/trigger/static/
///   replacement walked here (unreachable for this class).
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
    // Root 4: triggered abilities. Live entries are identity-bearing
    // (`TriggerEntry`): only the `definition` payload carries printed rules text,
    // so the walk projects it and leaves the `occurrence` provenance ref alone
    // (CR 612.2 — an occurrence id is a runtime identity marker, not a word).
    for i in 0..obj.trigger_definitions.len() {
        if let Some(trigger) = obj.trigger_definitions.get_mut(i) {
            walk_trigger_definition(&mut trigger.definition, category, cursor);
        }
    }
    // Root 5: static abilities (affected set, condition, layered modifications).
    for i in 0..obj.static_definitions.len() {
        if let Some(static_def) = obj.static_definitions.get_mut(i) {
            walk_static_definition(static_def, category, cursor);
        }
    }
    // Root 6: replacement effects (CR 614). Their event filter (`valid_card`),
    // applicability `condition` (`ReplacementCondition`), resulting `execute`
    // ability, and damage source / redirect filters are all rules-text carriers
    // ("if you control a Forest", "unless you control a Plains", "whenever a
    // Goblin would enter"). Re-seeded from `base_replacement_definitions` on each
    // layer pass (`GameObject::revert_layered_characteristics_to_base`), exactly
    // like the other live roots.
    for i in 0..obj.replacement_definitions.len() {
        if let Some(replacement) = obj.replacement_definitions.get_mut(i) {
            walk_replacement_definition(replacement, category, cursor);
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
        // CR 702.5a: "Enchant [quality]" names the object class the Aura attaches
        // to — a `TargetFilter` that can carry a creature/land type or color word.
        Keyword::Enchant(filter) => walk_target_filter(filter, category, cursor),
        // CR 702.167b: "Craft with [type]" — the materials filter names a type.
        Keyword::Craft { materials, .. } => walk_target_filter(materials, category, cursor),
        // CR 702.41a: "Affinity for [type]" names a permanent type/subtype
        // (e.g. Affinity for Plains — a basic land type).
        Keyword::Affinity(filter) => walk_typed_filter(filter, category, cursor),
        // CR 702.29 / 702.47a / 702.72a / 702.22 / 702.48a: keyword parameters that
        // ARE a subtype word used as such — "{subtype}cycling" (Plainscycling names
        // a basic land type, Slivercycling a creature type), "Splice onto [subtype]",
        // "Champion a [type]", "Bands with other [quality]", "[creature type]
        // offering" (Offering — CR 702.48a, "Fox offering" names a creature type).
        // All route through the category-disambiguating subtype cursor (the same one
        // used for type-line subtypes and `Landwalk`).
        Keyword::Typecycling { subtype, .. }
        | Keyword::Splice { subtype, .. }
        | Keyword::Champion(subtype)
        | Keyword::BandsWithOther(subtype)
        | Keyword::Offering(subtype) => cursor.subtype(category, subtype),
        // CR 702.181a / CR 702.189: Mobilize N / Firebending N carry a dynamic
        // count quantity (usually `Fixed`, but a granted "where X is its power"
        // form embeds a `QuantityExpr` whose typed `ObjectCount` filter can name a
        // creature/land/color word).
        Keyword::Mobilize(count) | Keyword::Firebending(count) => {
            walk_quantity_expr(count, category, cursor)
        }
        // The remaining keywords carry no color/land/creature WORD used as such in
        // any parameter. A keyword's activation / alternative *cost* (mana pips, or
        // an embedded `AbilityCost` on Evoke/Echo/Bestow/Escalate/Cumulative
        // upkeep/…) is a secondary carrier left intentionally red — consistent with
        // the secondary-cost/filter exclusion documented on [`walk_object_words`];
        // no covered card changes a color/land/creature word inside a keyword cost.
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
        | Keyword::Offspring(..)
        | Keyword::Impending { .. }
        | Keyword::LevelUp(..)
        | Keyword::CumulativeUpkeep(..)
        | Keyword::Banding
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
        | Keyword::Bargain
        | Keyword::Sunburst
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
        // CR 609.7b: "a [color] source of your choice" — the optional legality
        // filter can name a color / type word used as such.
        TargetFilter::ChosenDamageSource { filter } => {
            if let Some(f) = filter {
                walk_target_filter(f, category, cursor);
            }
        }
        // CR 612.2: a tracked set refined by a typed filter (`Typed(Land)`, …)
        // carries that nested filter's type word used as such.
        TargetFilter::TrackedSetFiltered { filter, .. } => {
            walk_target_filter(filter, category, cursor)
        }
        // CR 612.2 + CR 205.2a: `ControllerAndControlledPermanents.permanent_type`
        // is an `Option<CoreType>` (a card TYPE, not a subtype or color word), and
        // the player leg is a controller reference — no printed word to change.
        // `Opponent` is a player-scope reference (CR 102.2). `PostReplacement*` are
        // runtime event-context refs into the prevented damage event (CR 615.5).
        TargetFilter::None
        | TargetFilter::Any
        | TargetFilter::Player
        | TargetFilter::Controller
        | TargetFilter::Opponent
        | TargetFilter::ControllerAndControlledPermanents { .. }
        | TargetFilter::SelfRef
        | TargetFilter::GrantingObject
        | TargetFilter::SourceOrPaired
        | TargetFilter::StackAbility { .. }
        | TargetFilter::StackSpell
        | TargetFilter::SpecificObject { .. }
        // NB: `ChosenDamageSource` / `TrackedSetFiltered` are carriers above.
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
        | TargetFilter::PostReplacementDamageSource
        | TargetFilter::PostReplacementDamageTarget
        | TargetFilter::PostReplacementDamageTargetOwner
        | TargetFilter::DefendingPlayer
        | TargetFilter::HasChosenName
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
        // CR 109.5: "controlled by a player who controls a Goblin" nests a
        // `PlayerFilter` whose control sub-filter can name a type/color word.
        FilterProp::ControllerMatches { player } => {
            walk_player_filter(player, category, cursor)
        }
        // CR 609.7: "shares a [quality] with [reference]" — the optional reference
        // filter can name a creature type / land type / color word.
        FilterProp::SharesQuality { reference, .. } => {
            if let Some(f) = reference {
                walk_target_filter(f, category, cursor);
            }
        }
        // CR 115.x: "that targets only [filter]" / "that targets [filter]" nests a
        // spell-target filter that can name a type.
        FilterProp::TargetsOnly { filter } | FilterProp::Targets { filter } => {
            walk_target_filter(filter, category, cursor)
        }
        // CR 201.5 + CR 609.7: the object-identity duals of `SharesQuality` — the
        // referenced filter ("with a different name than target Goblin", "distinct
        // from target Forest") can name a creature type / land type / color word.
        FilterProp::DifferentNameFrom { filter } => walk_target_filter(filter, category, cursor),
        FilterProp::DistinctFrom { reference } => walk_target_filter(reference, category, cursor),
        // CR 612.2 + CR 107.4: `ColorCount` / `ManaSymbolCount` measure set size or
        // mana pips, not color WORDS — not text-changed. `IsChosenColor` reads a
        // chosen ref, not a printed word.
        FilterProp::Token
        | FilterProp::NonToken
        | FilterProp::ControllerChoseLabel { .. }
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
        // CR 701.15b/c: "goaded" is a designation marker, not a printed
        // color/land/creature word.
        | FilterProp::Goaded
        // CR 715.2: "has an Adventure" is a structural card-shape predicate (an
        // alternate face exists), not a subtype or color word.
        | FilterProp::HasAdventure
        | FilterProp::ToughnessGTPower
        | FilterProp::PowerExceedsBase
        | FilterProp::InTrackedSet { .. }
        | FilterProp::Modified
        | FilterProp::Historic
        | FilterProp::NotHistoric
        | FilterProp::InAnyZone { .. }
        | FilterProp::WasDealtDamageThisTurn
        // CR 120.1: the active-voice damage-role dual of `WasDealtDamageThisTurn`
        // — an event-history predicate with no printed word.
        | FilterProp::DealtDamageThisTurn
        | FilterProp::EnteredThisTurn
        | FilterProp::ControlledContinuouslySinceTurnBegan
        | FilterProp::ZoneChangedThisTurn { .. }
        | FilterProp::AttackedThisTurn { .. }
        | FilterProp::BlockedThisTurn
        | FilterProp::AttackedOrBlockedThisTurn
        | FilterProp::CountersPutOnThisTurn { .. }
        | FilterProp::FaceDown
        | FilterProp::Transformed
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
        // CR 105 + CR 612.2: "the chosen color is [color]" spells a color WORD used
        // as such (sibling to `DevotionGE.colors` / `ManaColorSpent.color`).
        StaticCondition::ChosenColorIs { color } => cursor.color(category, color),
        StaticCondition::ChosenLabelIs { .. }
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

/// CR 613.4 + CR 612.1: Walk the word-bearing children of a `PtValue`. Only the
/// `Quantity` variant wraps a `QuantityExpr` whose typed `ObjectCount` /
/// `Devotion` reference can name a creature/land/color word ("gets +X/+X where X
/// is the number of Goblins you control"). `Fixed`/`Variable` carry a scalar / an
/// X marker, not a printed word. No `_` wildcard — a future word-bearing `PtValue`
/// variant fails to compile until classified.
fn walk_pt_value(value: &mut PtValue, category: TextWordCategory, cursor: &mut WordCursor) {
    match value {
        PtValue::Quantity(q) => walk_quantity_expr(q, category, cursor),
        PtValue::Fixed(_) | PtValue::Variable(_) => {}
    }
}

/// CR 701.13a + CR 612.1: Walk the word-bearing children of an `UntilCondition`
/// (the `until` axis of `Effect::ExileFromTopUntil`). `NextMatches` carries a
/// `TargetFilter` that can name a creature/land/color word ("exile ... until you
/// exile a Goblin card"); `CumulativeThreshold` carries a `QuantityExpr` threshold
/// and an `ObjectProperty` (the CR-612.2 no-op — power/toughness/mana value).
/// No `_` wildcard — a future word-bearing `UntilCondition` variant fails to
/// compile until classified.
fn walk_until_condition(
    until: &mut UntilCondition,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    match until {
        UntilCondition::NextMatches { filter } => walk_target_filter(filter, category, cursor),
        UntilCondition::CumulativeThreshold {
            property,
            threshold,
            ..
        } => {
            walk_object_property(property, category, cursor);
            walk_quantity_expr(threshold, category, cursor);
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
        // CR 101.2 + CR 109.5: "the number of players who control a Goblin" nests a
        // `PlayerFilter` whose control sub-filter can name a type/color word.
        QuantityRef::PlayerCount { filter } => walk_player_filter(filter, category, cursor),
        // CR 603.2c + CR 109.5: "for each opponent dealt damage" counts the event
        // batch's players through a `PlayerFilter` whose control / attribute
        // sub-filters can name a creature type / land type / color word.
        QuantityRef::EventContextPlayerCount { filter } => {
            walk_player_filter(filter, category, cursor)
        }
        // CR 612.2: "unspent [color] mana" spells the color WORD used as such —
        // consistent with `StaticMode::StepEndUnspentMana` (`None` is the any-color
        // form; contrast the `ManaSymbolsInManaCost` pip-count no-op below).
        QuantityRef::UnspentMana { color } => {
            if let Some(c) = color {
                cursor.color(category, c);
            }
        }
        // CR 205.2a / CR 205.3: distinct-card-type / distinct-subtype counts scan a
        // parameterized `CardTypeSetSource` whose `Objects` variant nests a
        // `TargetFilter` that can name a type/color word (mirrors `ZoneCardCount`).
        QuantityRef::DistinctCardTypes { source }
        | QuantityRef::DistinctSubtypes { source, .. } => {
            walk_card_type_set_source(source, category, cursor)
        }
        QuantityRef::HandSize { .. }
        | QuantityRef::LifeTotal { .. }
        | QuantityRef::GraveyardSize { .. }
        | QuantityRef::LifeAboveStarting
        | QuantityRef::StartingLifeTotal
        | QuantityRef::TriggeringDiscoverValue
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
        | QuantityRef::CardsExiledBySource
        | QuantityRef::ExiledCardPower { .. }
        | QuantityRef::BasicLandTypeCount { .. }
        | QuantityRef::TrackedSetSize
        | QuantityRef::ExiledFromHandThisResolution
        | QuantityRef::PreviousEffectAmount { .. }
        | QuantityRef::LifeLostThisTurn { .. }
        | QuantityRef::PartySize { .. }
        | QuantityRef::Speed { .. }
        | QuantityRef::EventContextAmount
        | QuantityRef::AttachmentsOnLeavingObject { .. }
        | QuantityRef::EventContextSourceCostX
        // CR 700.2d: a count of modes chosen on the triggering spell — a runtime
        // event-context snapshot, no printed word.
        | QuantityRef::EventContextSourceModesChosen
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

/// CR 205.2a / CR 205.3 + CR 612.1: Walk the word-bearing children of a
/// `CardTypeSetSource` scan axis. Only the `Objects` variant nests a battlefield
/// `TargetFilter` that can name a creature type / land type / color word; the
/// zone / linked-exile / tracked-set axes carry no printed word. No `_` wildcard —
/// a future word-bearing `CardTypeSetSource` variant fails to compile until
/// classified.
fn walk_card_type_set_source(
    source: &mut CardTypeSetSource,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    match source {
        CardTypeSetSource::Objects { filter } => walk_target_filter(filter, category, cursor),
        CardTypeSetSource::Zone { .. }
        | CardTypeSetSource::ExiledBySource
        | CardTypeSetSource::TrackedSet { .. } => {}
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

/// CR 612.1 + CR 612.2: Walk the word-bearing children of an activated / additional
/// ability *cost*. A creature type / basic land type / color word can appear in a
/// cost's object filter (CR 701.21 "Sacrifice a Goblin" — Goblin Chirurgeon), in a
/// dynamic quantity (`ManaDynamic` / `PayLife` over a typed `ObjectCount`), or in a
/// nested effect / sub-cost. Filters recurse through the shared [`walk_target_filter`];
/// quantities through [`walk_quantity_expr`]; nested effects through [`walk_effect`];
/// the aggregate `ObjectProperty` through the CR-612.2 no-op [`walk_object_property`].
/// Every variant is classified with no `_` wildcard — a future word-bearing cost
/// variant fails to compile until handled.
fn walk_ability_cost(cost: &mut AbilityCost, category: TextWordCategory, cursor: &mut WordCursor) {
    match cost {
        // CR 701.21: "Sacrifice a [creature type]" — the sacrifice filter names a
        // creature-type / land-type / color word used as such (Goblin Chirurgeon).
        AbilityCost::Sacrifice(sac) => walk_target_filter(&mut sac.target, category, cursor),
        AbilityCost::Discard { count, filter, .. } => {
            walk_quantity_expr(count, category, cursor);
            if let Some(f) = filter {
                walk_target_filter(f, category, cursor);
            }
        }
        // Optional-filter costs (`RemoveCounter`'s `target` is the same Option<TargetFilter> shape).
        AbilityCost::Exile { filter, .. }
        | AbilityCost::ReturnToHand { filter, .. }
        | AbilityCost::Reveal { filter, .. }
        | AbilityCost::RemoveCounter { target: filter, .. } => {
            if let Some(f) = filter {
                walk_target_filter(f, category, cursor);
            }
        }
        // Required-filter costs.
        AbilityCost::ExileMaterials {
            materials: filter, ..
        }
        | AbilityCost::TapCreatures { filter, .. }
        | AbilityCost::UnattachFrom { filter, .. }
        | AbilityCost::Behold { filter, .. } => walk_target_filter(filter, category, cursor),
        AbilityCost::ExileWithAggregate {
            filter, property, ..
        } => {
            walk_target_filter(filter, category, cursor);
            walk_object_property(property, category, cursor);
        }
        AbilityCost::ManaDynamic { quantity } => walk_quantity_expr(quantity, category, cursor),
        AbilityCost::PayLife { amount }
        | AbilityCost::PayEnergy { amount }
        | AbilityCost::PaySpeed { amount } => walk_quantity_expr(amount, category, cursor),
        AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => {
            for c in costs.iter_mut() {
                walk_ability_cost(c, category, cursor);
            }
        }
        AbilityCost::PerCounter { target, base, .. } => {
            walk_target_filter(target, category, cursor);
            walk_ability_cost(base, category, cursor);
        }
        AbilityCost::EffectCost { effect } => walk_effect(effect, category, cursor),
        // CR 612.2 + CR 107.4: mana pips ({R}), tap/untap, loyalty, evidence value,
        // a keyword-derived cost, and Waterbend/Ninjutsu mana carry no color / land /
        // creature WORD used as such.
        AbilityCost::Mana { .. }
        | AbilityCost::Tap
        | AbilityCost::Untap
        | AbilityCost::Loyalty { .. }
        | AbilityCost::CollectEvidence { .. }
        | AbilityCost::Unattach
        | AbilityCost::Mill { .. }
        | AbilityCost::Exert
        | AbilityCost::Blight { .. }
        | AbilityCost::Waterbend { .. }
        | AbilityCost::NinjutsuFamily { .. }
        | AbilityCost::KeywordCostOfCastSpell { .. }
        | AbilityCost::Unimplemented { .. } => {}
    }
}

/// CR 612.1 + CR 612.2: Walk the word-bearing children of an ability's resolution
/// `condition`. A creature type / land type / color word can live inside an anaphoric
/// filter ("if this permanent is a Goblin", "if you control a Goblin"), a keyword
/// param ("if it has islandwalk"), a color-spent gate ("if white mana was spent"), or
/// a nested compound. All filters recurse through [`walk_target_filter`]; keywords
/// through [`walk_keyword`]; quantities through [`walk_quantity_expr`]. No `_`
/// wildcard — a future word-bearing condition variant fails to compile until handled.
fn walk_ability_condition(
    condition: &mut AbilityCondition,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    match condition {
        // CR 612.2: "if white mana was spent to cast this spell" spells a color WORD.
        AbilityCondition::ManaColorSpent { color, .. } => cursor.color(category, color),
        // Anaphoric / control / zone-change object filters that can name a type.
        AbilityCondition::TargetSharesNameWithOtherExiledThisWay { target: filter }
        | AbilityCondition::TargetMatchesFilter { filter, .. }
        | AbilityCondition::TriggeringSpellTargetsFilter { filter }
        | AbilityCondition::SourceMatchesFilter { filter }
        // CR 615.5 + CR 120.1: "if damage from a creature source is prevented this
        // way" gates on a `TargetFilter` naming the prevented event's damage
        // source — the same word-bearing carrier as `SourceMatchesFilter`.
        | AbilityCondition::PostReplacementDamageSourceMatchesFilter { filter }
        | AbilityCondition::ZoneChangeObjectMatchesFilter { filter, .. }
        | AbilityCondition::ControllerControlsMatching { filter }
        | AbilityCondition::ControllerControlledMatchingAsCast { filter }
        | AbilityCondition::ZoneChangedThisWay { filter }
        | AbilityCondition::CostPaidObjectMatchesFilter { filter } => {
            walk_target_filter(filter, category, cursor)
        }
        AbilityCondition::ObjectsShareQuality {
            subject, reference, ..
        } => {
            walk_target_filter(subject, category, cursor);
            walk_target_filter(reference, category, cursor);
        }
        // CR 205.3m: the revealed-card gate can carry a subtype filter and an extra
        // filter prop (e.g. a "Kraken … creature card" constraint).
        AbilityCondition::RevealedHasCardType {
            additional_filter,
            subtype_filter,
            ..
        } => {
            if let Some(fp) = additional_filter {
                walk_filter_prop(fp, category, cursor);
            }
            if let Some(f) = subtype_filter {
                walk_target_filter(f, category, cursor);
            }
        }
        // CR 702.14 / 702.16 / 702.11d: a keyword param may name a land type / color.
        AbilityCondition::TargetHasKeywordInstead { keyword }
        | AbilityCondition::SourceLacksKeyword { keyword } => {
            walk_keyword(keyword, category, cursor)
        }
        AbilityCondition::QuantityCheck { lhs, rhs, .. } => {
            walk_quantity_expr(lhs, category, cursor);
            walk_quantity_expr(rhs, category, cursor);
        }
        AbilityCondition::PreviousEffectAmount { rhs, .. } => {
            walk_quantity_expr(rhs, category, cursor)
        }
        AbilityCondition::ConditionInstead { inner } => {
            walk_ability_condition(inner, category, cursor)
        }
        // CR 101.2 + CR 109.5: the scoped `PlayerFilter` can embed a
        // controls-count / player-attribute sub-filter naming a type/color word
        // ("each opponent who controls a Goblin").
        AbilityCondition::ScopedPlayerMatches { filter } => {
            walk_player_filter(filter, category, cursor)
        }
        AbilityCondition::Not { condition } => walk_ability_condition(condition, category, cursor),
        AbilityCondition::And { conditions } | AbilityCondition::Or { conditions } => {
            for c in conditions.iter_mut() {
                walk_ability_condition(c, category, cursor);
            }
        }
        // No color / land / creature WORD used as such: payment flags, phase / timing,
        // controller designations, mana-symbol (`ManaCost`) kicker cost, and coin /
        // outcome signals.
        AbilityCondition::AdditionalCostPaid { .. }
        | AbilityCondition::AdditionalCostPaidInstead
        | AbilityCondition::AlternativeManaCostPaid
        | AbilityCondition::EffectOutcome { .. }
        | AbilityCondition::EventOutcomeWon
        | AbilityCondition::CoinFlipOutcome { .. }
        | AbilityCondition::WhenYouDo
        | AbilityCondition::WasCast { .. }
        | AbilityCondition::CastDuringPhase { .. }
        | AbilityCondition::CurrentPhaseIs { .. }
        | AbilityCondition::CastTimingPermission { .. }
        | AbilityCondition::SourceEnteredThisTurn
        | AbilityCondition::CastVariantPaid { .. }
        | AbilityCondition::CastVariantPaidInstead { .. }
        | AbilityCondition::HasMaxSpeed
        | AbilityCondition::IsMonarch
        | AbilityCondition::IsInitiative
        | AbilityCondition::HasCityBlessing
        | AbilityCondition::IsRingBearer
        | AbilityCondition::CompletedDungeon { .. }
        | AbilityCondition::HasObjectTarget
        | AbilityCondition::IsYourTurn
        | AbilityCondition::WasStartingPlayer { .. }
        | AbilityCondition::SpellCastWithVariantThisTurn { .. }
        | AbilityCondition::FirstCombatPhaseOfTurn
        | AbilityCondition::FirstEndStepOfTurn
        | AbilityCondition::SourceIsTapped
        | AbilityCondition::SourceAttachedToCreature
        | AbilityCondition::DayNightIsNeither
        | AbilityCondition::DayNightIs { .. }
        | AbilityCondition::NthResolutionThisTurn { .. } => {}
    }
}

/// CR 118.12: an "unless [player] pays [cost]" modifier bundles a payment cost
/// (whose object filter can name a type) and a `payer` filter — both walked.
fn walk_unless_pay(
    modifier: &mut UnlessPayModifier,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    walk_ability_cost(&mut modifier.cost, category, cursor);
    walk_target_filter(&mut modifier.payer, category, cursor);
}

/// CR 608.2c: a "repeat this process" loop predicate. Only the `WhileCondition`
/// shape carries a filter word (via its `AbilityCondition`); the others hold none.
fn walk_repeat_continuation(
    cont: &mut RepeatContinuation,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    match cont {
        RepeatContinuation::WhileCondition { condition, .. } => {
            walk_ability_condition(condition, category, cursor)
        }
        RepeatContinuation::ControllerChoice | RepeatContinuation::UntilStopConditions { .. } => {}
    }
}

/// CR 612.1 + CR 612.2: Walk the word-bearing children of a `PlayerFilter` root.
/// CR 109.5: a player filter's control-count / player-attribute sub-filters can
/// name a creature type / land type / color word ("each opponent who controls a
/// Goblin", "each player who controls more Elves than you"); its damage-source
/// gate and its self-composed `AllExcept` anchor are recursion points too. Every
/// non-filter designation (controller / opponent / attacking / triggering
/// anchors) is an explicit no-op. No `_` wildcard — a future word-bearing
/// `PlayerFilter` variant fails to compile until classified.
fn walk_player_filter(pf: &mut PlayerFilter, category: TextWordCategory, cursor: &mut WordCursor) {
    match pf {
        // CR 120.9: the damage-source qualifier is a `TargetFilter` and can name a type.
        PlayerFilter::OpponentDealtDamage { source, .. } => {
            if let Some(f) = source {
                walk_target_filter(f, category, cursor);
            }
        }
        // CR 608.2h: the exclusion anchor is itself a `PlayerFilter`.
        PlayerFilter::AllExcept { exclude } => walk_player_filter(exclude, category, cursor),
        // CR 109.5: "each player who controls [comparator] [filter]" — the filter
        // and the comparison count both carry word-bearing sub-structure.
        PlayerFilter::ControlsCount { filter, count, .. } => {
            walk_target_filter(filter, category, cursor);
            walk_quantity_expr(count, category, cursor);
        }
        // CR 119.1 / CR 402.1: the scalar attribute ref and its threshold value
        // are quantities that may embed a typed `ObjectCount`.
        PlayerFilter::PlayerAttribute { attr, value, .. } => {
            walk_quantity_ref(attr, category, cursor);
            walk_quantity_expr(value, category, cursor);
        }
        // No color / land / creature WORD used as such: controller / opponent /
        // defending designations, attack / trigger / vote / chosen-player anchors.
        PlayerFilter::Controller
        | PlayerFilter::Opponent
        | PlayerFilter::DefendingPlayer
        | PlayerFilter::OpponentLostLife
        | PlayerFilter::OpponentGainedLife
        | PlayerFilter::HasLostTheGame
        | PlayerFilter::OpponentAttacked { .. }
        | PlayerFilter::OpponentAttackingEnchantedPlayer
        | PlayerFilter::All
        | PlayerFilter::HighestSpeed
        | PlayerFilter::ZoneChangedThisWay
        | PlayerFilter::PerformedActionThisWay { .. }
        | PlayerFilter::OwnersOfCardsExiledBySource
        | PlayerFilter::TriggeringPlayer
        | PlayerFilter::OpponentOtherThanTriggering
        | PlayerFilter::OpponentOfTriggeringPlayer
        | PlayerFilter::OpponentOfTriggeringPlayerNotAttacked
        | PlayerFilter::VotedFor { .. }
        | PlayerFilter::ParentObjectTargetController
        | PlayerFilter::ChosenPlayer { .. }
        | PlayerFilter::ParentObjectTargetOwner => {}
    }
}

/// CR 612.1 + CR 603.4: Walk the word-bearing children of a trigger's
/// intervening-if `TriggerCondition`. A creature type / land type / color word
/// can live in a control / event-subject / cast-history filter
/// ("if you control a Goblin", "if it targets a Goblin", "if white mana was
/// spent"), a nested `PlayerFilter` ("during that player's turn"), a quantity
/// comparison, or a composite And/Or/Not. Every filter recurses via
/// [`walk_target_filter`]; `ManaColorSpent` is a color-word carrier; player and
/// quantity references delegate to their walkers. No `_` wildcard — a future
/// word-bearing `TriggerCondition` variant fails to compile until classified.
fn walk_trigger_condition(
    cond: &mut TriggerCondition,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    match cond {
        // CR 603.4: control / defending-player presence gates naming a type.
        TriggerCondition::ControlsType { filter }
        | TriggerCondition::ControlCount { filter, .. }
        | TriggerCondition::ControlsNone { filter }
        | TriggerCondition::DefendingPlayerControlsNone { filter } => {
            walk_target_filter(filter, category, cursor)
        }
        // CR 120.1: damage-source / event-subject / cast-spell gates naming a type.
        TriggerCondition::DealtDamageThisTurnBySource { source } => {
            walk_target_filter(source, category, cursor)
        }
        TriggerCondition::ZoneChangeObjectMatchesFilter { filter, .. }
        | TriggerCondition::SourceMatchesFilter { filter }
        | TriggerCondition::EventDamageSourceMatchesFilter { filter }
        | TriggerCondition::EventObjectMatchesFilter { filter }
        | TriggerCondition::TriggeringSpellTargetsFilter { filter }
        | TriggerCondition::TriggeringSpellMatchesFilter { filter } => {
            walk_target_filter(filter, category, cursor)
        }
        // CR 506.5 / CR 603.4: optional co-attacker and cast-spell filters.
        TriggerCondition::MinCoAttackers { filter, .. }
        | TriggerCondition::CastSpellThisTurn { filter } => {
            if let Some(f) = filter {
                walk_target_filter(f, category, cursor);
            }
        }
        // CR 102.1: "if it's [player]'s turn" nests a `PlayerFilter`.
        TriggerCondition::DuringPlayersTurn { player } => {
            walk_player_filter(player, category, cursor)
        }
        // CR 207.2c: "if [N] mana of [color] was spent" spells a color WORD.
        TriggerCondition::ManaColorSpent { color, .. } => cursor.color(category, color),
        // CR 508.1: the attackers-declared count subject carries an optional
        // condition-level type filter naming a creature type used as such — "if
        // two or more Pirates attacked this combat" counts only Pirate attackers.
        // Both subject axes (`Controller` / `AttackTarget`) hold the same
        // `Option<TargetFilter>`, so both are recursion points.
        TriggerCondition::AttackersDeclaredCount { subject, .. } => match subject {
            AttackersDeclaredCountSubject::Controller { filter, .. }
            | AttackersDeclaredCountSubject::AttackTarget { filter, .. } => {
                if let Some(f) = filter {
                    walk_target_filter(f, category, cursor);
                }
            }
        },
        TriggerCondition::QuantityComparison { lhs, rhs, .. } => {
            walk_quantity_expr(lhs, category, cursor);
            walk_quantity_expr(rhs, category, cursor);
        }
        TriggerCondition::And { conditions } | TriggerCondition::Or { conditions } => {
            for c in conditions.iter_mut() {
                walk_trigger_condition(c, category, cursor);
            }
        }
        TriggerCondition::Not { condition } => walk_trigger_condition(condition, category, cursor),
        // CR 400.7 / CR 111.1: `WasType`/`HadCounters` name a CORE type / counter
        // kind, not a color/land/creature WORD (CR 612.2). Every remaining variant
        // is a phase / timing / designation / event-shape / ordinal predicate with
        // no printed color/land/creature word used as such.
        TriggerCondition::GainedLife { .. }
        | TriggerCondition::LostLife
        | TriggerCondition::Descended
        | TriggerCondition::NoSpellsCastLastTurn
        | TriggerCondition::TwoOrMoreSpellsCastLastTurn
        | TriggerCondition::SourceEnteredThisTurn
        | TriggerCondition::EchoDue
        | TriggerCondition::SolveConditionMet
        | TriggerCondition::ClassLevelGE { .. }
        | TriggerCondition::SourceIsHarnessed
        | TriggerCondition::AttractionVisitRoll { .. }
        | TriggerCondition::WasCast { .. }
        | TriggerCondition::WasPlayed
        | TriggerCondition::AdditionalCostPaid { .. }
        | TriggerCondition::SourceIsAttacking
        | TriggerCondition::CastVariantPaid { .. }
        | TriggerCondition::CastVariantPaidPersistent { .. }
        | TriggerCondition::ActivatedAbilityIsNonMana
        | TriggerCondition::DealtDamageBySourceThisTurn
        | TriggerCondition::FirstTimeObjectTappedThisTurn
        | TriggerCondition::FirstTimeObjectCountersAddedThisTurn
        | TriggerCondition::WasType { .. }
        | TriggerCondition::LifeTotalGE { .. }
        | TriggerCondition::AttackedThisTurn
        | TriggerCondition::FirstCombatPhaseOfTurn
        | TriggerCondition::HasMaxSpeed
        | TriggerCondition::IsMonarch
        | TriggerCondition::IsInitiative
        | TriggerCondition::NoMonarch
        | TriggerCondition::WasStartingPlayer { .. }
        | TriggerCondition::SpellCastWithVariantThisTurn { .. }
        | TriggerCondition::HasCityBlessing
        | TriggerCondition::CompletedDungeon { .. }
        | TriggerCondition::SourceIsTapped
        | TriggerCondition::SourceIsTransformed
        | TriggerCondition::SourceIsFaceUp
        | TriggerCondition::SourceIsFaceDown
        | TriggerCondition::SourceInZone { .. }
        | TriggerCondition::CounterAddedThisTurn
        | TriggerCondition::LostLifeLastTurn
        | TriggerCondition::TributeNotPaid
        | TriggerCondition::CastDuringPhase { .. }
        | TriggerCondition::CastTimingPermission { .. }
        | TriggerCondition::ManaSpentCondition { .. }
        | TriggerCondition::HadCounters { .. }
        | TriggerCondition::ControlsCommander { .. }
        | TriggerCondition::IsRenowned { .. }
        | TriggerCondition::HasCounters { .. }
        | TriggerCondition::ZoneChangeObjectIsTapped
        | TriggerCondition::DamagedPlayerIsEventSourceOwner
        | TriggerCondition::ChosenLabelIs { .. }
        | TriggerCondition::ExceptFirstDrawInDrawStep
        | TriggerCondition::PlacedByAbilitySource => {}
    }
}

/// CR 612.1 + CR 603.4: Walk the word-bearing children of a `TriggerConstraint`
/// rate-limiter. Only `NthSpellThisTurn` carries an optional spell `TargetFilter`
/// ("your Nth noncreature spell"); every other constraint is a pure count /
/// timing / controller gate. No `_` wildcard.
fn walk_trigger_constraint(
    c: &mut TriggerConstraint,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    match c {
        TriggerConstraint::NthSpellThisTurn { filter, .. } => {
            if let Some(f) = filter {
                walk_target_filter(f, category, cursor);
            }
        }
        TriggerConstraint::OncePerTurn
        | TriggerConstraint::OncePerGame
        | TriggerConstraint::OnlyDuringYourTurn
        | TriggerConstraint::NthDrawThisTurn { .. }
        | TriggerConstraint::OnlyDuringOpponentsTurn
        | TriggerConstraint::OnlyDuringYourMainPhase
        | TriggerConstraint::AtClassLevel { .. }
        | TriggerConstraint::MaxTimesPerTurn { .. }
        | TriggerConstraint::OncePerOpponentPerTurn
        | TriggerConstraint::EventSourceControlledBy { .. } => {}
    }
}

/// CR 612.1 + CR 601.3 / CR 602.5: Walk the word-bearing children of a parsed
/// cast/activation restriction condition (`StaticDefinition.per_player_condition`,
/// `CostReduction.condition`, `ActivationRestriction::RequiresCondition`). A
/// creature type / land type / color word can live in a subtype/color/keyword
/// leaf ("you control a Forest", "an artifact creature", "a creature with
/// flying"), a `TargetFilter`, a nested `PlayerFilter`, a quantity comparison, or
/// a composite And/Or/Not. No `_` wildcard — a future word-bearing
/// `ParsedCondition` variant fails to compile until classified.
fn walk_parsed_condition(
    condition: &mut ParsedCondition,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    match condition {
        // CR 205.3: land / creature subtype leaves naming a type WORD.
        ParsedCondition::ZoneSubtypeCardCountAtLeast { subtype, .. }
        | ParsedCondition::YouControlSubtypeCountAtLeast { subtype, .. }
        | ParsedCondition::YouControlSubtypeOrGraveyardCardSubtype { subtype } => {
            cursor.subtype(category, subtype)
        }
        ParsedCondition::YouControlLandSubtypeAny { subtypes } => {
            for s in subtypes.iter_mut() {
                cursor.subtype(category, s);
            }
        }
        // CR 105: color-word leaves.
        ParsedCondition::SourceIsColor { color }
        | ParsedCondition::YouControlColorPermanentCountAtLeast { color, .. } => {
            cursor.color(category, color)
        }
        // CR 702.x: keyword params can name a land type (landwalk) / color (protection).
        ParsedCondition::SourceLacksKeyword { keyword }
        | ParsedCondition::ControlsCreatureWithKeyword { keyword, .. } => {
            walk_keyword(keyword, category, cursor)
        }
        // Required / optional object filters.
        ParsedCondition::BattlefieldEntriesThisTurn { filter, .. }
        | ParsedCondition::SpellTargetsFilter { filter } => {
            walk_target_filter(filter, category, cursor)
        }
        ParsedCondition::YouAttackedWithAtLeast { filter, .. }
        | ParsedCondition::YouCastSpellThisTurn { filter } => {
            if let Some(f) = filter {
                walk_target_filter(f, category, cursor);
            }
        }
        // CR 602.5b: a nested player filter can name a controlled type.
        ParsedCondition::PlayerCountAtLeast { filter, .. } => {
            walk_player_filter(filter, category, cursor)
        }
        // Quantity comparisons — `QuantityVsEachOpponent` compares two refs.
        ParsedCondition::QuantityVsEachOpponent { lhs, rhs, .. } => {
            walk_quantity_ref(lhs, category, cursor);
            walk_quantity_ref(rhs, category, cursor);
        }
        ParsedCondition::QuantityComparison { lhs, rhs, .. } => {
            walk_quantity_expr(lhs, category, cursor);
            walk_quantity_expr(rhs, category, cursor);
        }
        ParsedCondition::And { conditions } | ParsedCondition::Or { conditions } => {
            for c in conditions.iter_mut() {
                walk_parsed_condition(c, category, cursor);
            }
        }
        ParsedCondition::Not { condition } => walk_parsed_condition(condition, category, cursor),
        // CR 612.2: core-type / zone / count / timing / power / life / named
        // predicates carry no color/land/creature WORD used as such. (`ZoneCore*`,
        // `YouControl*CoreType*`, `*NamedPlaneswalker`, `*NamedCreature` name a
        // CORE type or a card NAME, not a subtype word — CR 205.2/CR 201.)
        ParsedCondition::SourceInZone { .. }
        | ParsedCondition::SourceIsAttacking
        | ParsedCondition::SourceIsAttackingOrBlocking
        | ParsedCondition::SourceIsBlocked
        | ParsedCondition::SourcePowerAtLeast { .. }
        | ParsedCondition::SourceHasCounterAtLeast { .. }
        | ParsedCondition::SourceHasNoCounter { .. }
        | ParsedCondition::SourceEnteredThisTurn
        | ParsedCondition::SourceAttackedThisTurn
        | ParsedCondition::SourceIsCreature
        | ParsedCondition::SourceAttachedTo { .. }
        | ParsedCondition::SourceUntappedAttachedTo { .. }
        | ParsedCondition::FirstSpellThisGame
        | ParsedCondition::OpponentSearchedLibraryThisTurn
        | ParsedCondition::BeenAttackedThisStep
        | ParsedCondition::ZoneCardCountAtLeast { .. }
        | ParsedCondition::ZoneCardTypeCountAtLeast { .. }
        | ParsedCondition::ZoneCoreTypeCardCountAtLeast { .. }
        | ParsedCondition::OpponentPoisonAtLeast { .. }
        | ParsedCondition::HandSizeExact { .. }
        | ParsedCondition::HandSizeOneOf { .. }
        | ParsedCondition::CreaturesYouControlTotalPowerAtLeast { .. }
        | ParsedCondition::YouControlCoreTypeCountAtLeast { .. }
        | ParsedCondition::YouControlLegendaryCreature
        | ParsedCondition::YouControlNamedPlaneswalker { .. }
        | ParsedCondition::YouControlCreatureWithPowerAtLeast { .. }
        | ParsedCondition::YouControlCreatureWithPt { .. }
        | ParsedCondition::YouControlAnotherColorlessCreature
        | ParsedCondition::YouControlSnowPermanentCountAtLeast { .. }
        | ParsedCondition::YouControlDifferentPowerCreatureCountAtLeast { .. }
        | ParsedCondition::YouControlLandsWithSameNameAtLeast { .. }
        | ParsedCondition::YouControlNoCreatures
        | ParsedCondition::YouAttackedThisTurn
        | ParsedCondition::YouAttackedSourceControllerThisTurn
        | ParsedCondition::YouPlayedLandThisTurn
        | ParsedCondition::YouCastNoncreatureSpellThisTurn
        | ParsedCondition::YouCastSpellCountAtLeast { .. }
        | ParsedCondition::YouGainedLifeThisTurn
        | ParsedCondition::YouCreatedTokenThisTurn
        | ParsedCondition::YouDiscardedCardThisTurn
        | ParsedCondition::YouSacrificedArtifactThisTurn
        | ParsedCondition::CreatureDiedThisTurn
        | ParsedCondition::YouHadCreatureEnterThisTurn
        | ParsedCondition::YouHadAngelOrBerserkerEnterThisTurn
        | ParsedCondition::YouHadArtifactEnterThisTurn
        | ParsedCondition::CardsLeftYourGraveyardThisTurnAtLeast { .. }
        | ParsedCondition::HasCityBlessing
        | ParsedCondition::IsYourTurn => {}
    }
}

/// CR 601.2f + CR 602.2b: Walk a `CostReduction`'s word-bearing children — the
/// counted quantity (a typed `ObjectCount` filter) and the optional conditional
/// gate (`ParsedCondition`).
fn walk_cost_reduction(
    reduction: &mut CostReduction,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    walk_quantity_expr(&mut reduction.count, category, cursor);
    if let Some(condition) = &mut reduction.condition {
        walk_parsed_condition(condition, category, cursor);
    }
}

/// CR 602.5 + CR 612.1: Walk an activation restriction's word-bearing children.
/// Only `RequiresCondition` carries a `ParsedCondition`; every other restriction
/// is a count / timing / designation gate. No `_` wildcard.
fn walk_activation_restriction(
    restriction: &mut ActivationRestriction,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    match restriction {
        ActivationRestriction::RequiresCondition { condition } => {
            if let Some(cond) = condition {
                walk_parsed_condition(cond, category, cursor);
            }
        }
        ActivationRestriction::AsSorcery
        | ActivationRestriction::AsInstant
        | ActivationRestriction::DuringYourTurn
        | ActivationRestriction::DuringYourUpkeep
        | ActivationRestriction::DuringCombat
        | ActivationRestriction::BeforeAttackersDeclared
        | ActivationRestriction::BeforeCombatDamage
        | ActivationRestriction::OnlyOnceEachTurn
        | ActivationRestriction::OnlyOnce
        | ActivationRestriction::MaxTimesEachTurn { .. }
        | ActivationRestriction::IsSolved
        | ActivationRestriction::SourceIsHarnessed
        | ActivationRestriction::ClassLevelIs { .. }
        | ActivationRestriction::LevelCounterRange { .. }
        | ActivationRestriction::CounterThreshold { .. }
        | ActivationRestriction::MatchesCardCastTiming => {}
    }
}

/// CR 611.2b + CR 612.1: Walk the word-bearing children of an effect/ability
/// `Duration`. Only the `ForAsLongAs` shape carries a rules-text condition
/// (`StaticCondition`) that can name a creature type / land type / color word
/// used as such ("for as long as you control a Goblin", "as long as you control
/// a Forest"); every other duration is a pure turn / phase / permanence marker
/// with no printed word. No `_` wildcard — a future word-bearing `Duration`
/// variant fails to compile until classified.
fn walk_duration(dur: &mut Duration, category: TextWordCategory, cursor: &mut WordCursor) {
    match dur {
        Duration::ForAsLongAs { condition } => walk_static_condition(condition, category, cursor),
        Duration::UntilEndOfTurn
        | Duration::UntilEndOfCombat
        | Duration::UntilNextTurnOf { .. }
        | Duration::UntilEndOfNextTurnOf { .. }
        | Duration::UntilHostLeavesPlay
        | Duration::UntilNextStepOf { .. }
        | Duration::UntilSourceExilesAnotherCard
        | Duration::Permanent => {}
    }
}

fn walk_ability_definition(
    ability: &mut AbilityDefinition,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    walk_effect(&mut ability.effect, category, cursor);
    // CR 612.1 + CR 118.12: an ability's activation/additional cost, its
    // resolution condition, and its unless-pay modifier are rules-text carriers —
    // "Sacrifice a Goblin" / "if you control a Goblin" / "unless you sacrifice a
    // Goblin" all name a creature type used as a creature type (CR 612.2).
    if let Some(cost) = &mut ability.cost {
        walk_ability_cost(cost, category, cursor);
    }
    if let Some(condition) = &mut ability.condition {
        walk_ability_condition(condition, category, cursor);
    }
    if let Some(unless) = &mut ability.unless_pay {
        walk_unless_pay(unless, category, cursor);
    }
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
    // CR 608.2c: a "repeat this process while <condition>" predicate may gate on a
    // typed filter ("if the exiled card is a land card, repeat this process").
    if let Some(repeat_until) = &mut ability.repeat_until {
        walk_repeat_continuation(repeat_until, category, cursor);
    }
    // CR 601.2f + CR 602.2b: a self-referential cost reduction ("costs {N} less
    // for each [filter]" / "... if [condition]") carries a typed count and a
    // `ParsedCondition` gate.
    if let Some(reduction) = &mut ability.cost_reduction {
        walk_cost_reduction(reduction, category, cursor);
    }
    // CR 602.5: an "activate only if [condition]" restriction carries a
    // `ParsedCondition` naming a type/color/keyword.
    for restriction in ability.activation_restrictions.iter_mut() {
        walk_activation_restriction(restriction, category, cursor);
    }
    // CR 602.2a + CR 109.5: the activator and per-player scope roots are
    // `PlayerFilter`s whose control-count / attribute sub-filters can name a type.
    if let Some(activator) = &mut ability.activator_filter {
        walk_player_filter(activator, category, cursor);
    }
    if let Some(scope) = &mut ability.player_scope {
        walk_player_filter(scope, category, cursor);
    }
    // CR 601.2c + CR 601.2b: the target-chooser filter and the announced-X
    // quantity are word-bearing roots (a "target chooser" or measured-X
    // expression can embed a typed filter).
    if let Some(chooser) = &mut ability.target_chooser {
        walk_target_filter(chooser, category, cursor);
    }
    if let Some(announced_x) = &mut ability.announced_x {
        walk_quantity_expr(announced_x, category, cursor);
    }
    // CR 611.2b: a "for as long as [condition]" duration gates the ability's
    // continuous effect on a word-bearing `StaticCondition`.
    if let Some(duration) = &mut ability.duration {
        walk_duration(duration, category, cursor);
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
    // CR 603.2: the trigger's event-shape filters name the object classes that fire
    // it — a creature type / land type / color word ("whenever a Goblin you control
    // dies", "whenever a red creature attacks") lives in `valid_card` and its
    // sibling target/source/subject filters.
    if let Some(valid_card) = &mut trigger.valid_card {
        walk_target_filter(valid_card, category, cursor);
    }
    if let Some(valid_target) = &mut trigger.valid_target {
        walk_target_filter(valid_target, category, cursor);
    }
    if let Some(valid_source) = &mut trigger.valid_source {
        walk_target_filter(valid_source, category, cursor);
    }
    if let Some(valid_subject_player) = &mut trigger.valid_subject_player {
        walk_target_filter(valid_subject_player, category, cursor);
    }
    // CR 603.2: a disjunctive zone-change trigger ("whenever a Goblin dies or a
    // Goblin card is put into a graveyard from anywhere") names its object class
    // in each clause's `valid_card`, not only the top-level filter.
    for clause in trigger.zone_change_clauses.iter_mut() {
        if let Some(valid_card) = &mut clause.valid_card {
            walk_target_filter(valid_card, category, cursor);
        }
    }
    // CR 118.12: a tax trigger's unless-pay bundles a cost + payer filter.
    if let Some(unless) = &mut trigger.unless_pay {
        walk_unless_pay(unless, category, cursor);
    }
    // CR 603.4: the intervening-if `TriggerCondition` ("... if you control a
    // Goblin, ...") carries word-bearing control / event-subject / color filters.
    if let Some(condition) = &mut trigger.condition {
        walk_trigger_condition(condition, category, cursor);
    }
    // CR 603.4: a rate-limit `TriggerConstraint` ("... your Nth [type] spell ...")
    // can carry a spell filter naming a type.
    if let Some(constraint) = &mut trigger.constraint {
        walk_trigger_constraint(constraint, category, cursor);
    }
}

/// CR 612.1 + CR 612.2: Walk the word-bearing children of a `StaticMode`. A
/// creature type / land type / color word can live in an evasion / block /
/// attachment filter ("can't be blocked by Goblins", "can be attached only to a
/// legendary creature"), a protection quality ("protection from the chosen
/// color"), a keyword parameter ("can't have flying"/landwalk), a cost-scope
/// spell filter ("black permanent spells cost less", "creature spells you cast
/// have convoke"), a spelled-out color word ("green permanent spells", "unspent
/// green mana"), a landwalk qualifier, or a nested quantity / player filter.
/// Filters recurse via [`walk_target_filter`]; keywords via [`walk_keyword`];
/// colors via [`WordCursor::color`]; the landwalk qualifier via
/// [`WordCursor::subtype`]. No `_` wildcard — a future word-bearing `StaticMode`
/// variant fails to compile until classified.
///
/// Intentionally NOT walked (coverage stays red, consistent with the keyword /
/// secondary-cost exclusion on [`walk_object_words`]): a static's alternative /
/// additional CAST-cost riders (`CastWithAlternativeCost.cost`,
/// `ImposeAdditionalCost.cost`, `AlternativeKeywordCost.cost`, the permission
/// `alt_cost` / `extra_cost` fields) — a mana-symbol / keyword casting cost that
/// no covered card text-changes into a creature/land/color word. `PayLifeAsColoredMana`
/// names a mana SYMBOL ({B}), not a color word (CR 612.2 + CR 107.4).
fn walk_static_mode(mode: &mut StaticMode, category: TextWordCategory, cursor: &mut WordCursor) {
    match mode {
        // -- Evasion / block / attachment filters (CR 509.1b / 301.5 / 303.4) --
        StaticMode::CantBeActivated { source_filter, .. }
        | StaticMode::AttachmentRestriction {
            filter: source_filter,
        }
        | StaticMode::CantBeBlockedBy {
            filter: source_filter,
        }
        | StaticMode::BlockRestriction {
            filter: source_filter,
        }
        | StaticMode::SuppressTriggers { source_filter, .. }
        | StaticMode::MaxUntapPerType {
            filter: source_filter,
            ..
        } => walk_target_filter(source_filter, category, cursor),
        // CR 509.1b: "can't be blocked except by [quality]" nests a `TargetFilter`.
        StaticMode::CantBeBlockedExceptBy { kind } => match kind {
            BlockExceptionKind::Quality(filter) => walk_target_filter(filter, category, cursor),
            BlockExceptionKind::MinBlockers { .. } => {}
        },
        // CR 509.1c: positive block requirements carry an optional blocker filter.
        StaticMode::MustBeBlocked { by: filter }
        | StaticMode::MustBeBlockedByAll { blockers: filter }
        | StaticMode::PerTurnCastLimit {
            spell_filter: filter,
            ..
        } => {
            if let Some(f) = filter {
                walk_target_filter(f, category, cursor);
            }
        }
        // CR 601.2f: cost-scope spell filter + dynamic multiplier.
        StaticMode::ModifyCost {
            spell_filter,
            dynamic_count,
            ..
        } => {
            if let Some(f) = spell_filter {
                walk_target_filter(f, category, cursor);
            }
            if let Some(q) = dynamic_count {
                walk_quantity_ref(q, category, cursor);
            }
        }
        // CR 601.2f + CR 118.8: additional-cost tax carries a spell filter (the
        // AbilityCost rider is a secondary cost left red, see the doc note above).
        StaticMode::ImposeAdditionalCost { spell_filter, .. } => {
            if let Some(f) = spell_filter {
                walk_target_filter(f, category, cursor);
            }
        }
        // CR 601.2f + CR 602.2: ability-cost reduction carries a dynamic multiplier
        // and an optional activator `PlayerFilter`.
        StaticMode::ReduceAbilityCost {
            dynamic_count,
            activator,
            ..
        } => {
            if let Some(q) = dynamic_count {
                walk_quantity_ref(q, category, cursor);
            }
            if let Some(p) = activator {
                walk_player_filter(p, category, cursor);
            }
        }
        // CR 118.3 + CR 601.2h: "can't sacrifice [filter] to pay costs" nests a filter.
        StaticMode::CantPayCost { cost, .. } => match cost {
            CostPaymentProhibition::Sacrifice { filter } => {
                walk_target_filter(filter, category, cursor)
            }
            CostPaymentProhibition::PayLife => {}
        },
        // CR 609.4b: any-color spend permission carries two scope filters.
        StaticMode::SpendManaAsAnyColor {
            spell_filter,
            activation_source_filter,
        } => {
            if let Some(f) = spell_filter {
                walk_target_filter(f, category, cursor);
            }
            if let Some(f) = activation_source_filter {
                walk_target_filter(f, category, cursor);
            }
        }
        // CR 702.51a / CR 702.x: keyword parameters (landwalk land type, protection
        // color, "can't have [keyword]").
        StaticMode::CastWithKeyword { keyword } | StaticMode::CantHaveKeyword { keyword } => {
            walk_keyword(keyword, category, cursor)
        }
        // CR 702.16: player protection quality can name a color.
        StaticMode::PlayerProtection(target) => walk_protection_target(target, category, cursor),
        // CR 118.12a: "[color] permanent spells cost less" spells a color WORD.
        StaticMode::DefilerCostReduction { color, .. } => cursor.color(category, color),
        // CR 612.2: "unspent [color] mana" (Omnath) spells a color WORD used as
        // such (contrast the {B} mana-symbol no-op below); `None` is the any-color
        // form.
        StaticMode::StepEndUnspentMana { filter, .. } => {
            if let Some(c) = filter {
                cursor.color(category, c);
            }
        }
        // CR 702.14d: landwalk-cancel qualifier is a basic-land-type name ("Swamp").
        StaticMode::IgnoreLandwalkForBlocking { qualifier } => {
            if let Some(q) = qualifier {
                cursor.subtype(category, q);
            }
        }
        // CR 508 + CR 612.1: "your maximum hand size is equal to [quantity]" — the
        // dynamic `EqualTo` quantity can embed a typed `ObjectCount` filter naming a
        // type/color word. The `SetTo`/`AdjustedBy` forms are constant scalars.
        StaticMode::MaximumHandSize { modification } => {
            if let HandSizeModification::EqualTo(qty) = modification {
                walk_quantity_expr(qty, category, cursor);
            }
        }
        // No color / land / creature WORD used as such: nullary keyword/evasion
        // markers, count / timing / designation gates, cast-permission scaffolding
        // (its cost riders are red — see doc), core-type / counter-type / name /
        // mana-symbol carriers, and player-scope prohibitions.
        StaticMode::Continuous
        | StaticMode::DamageNotRemovedDuringCleanup
        | StaticMode::CantAttack
        | StaticMode::CantBlock
        | StaticMode::CantAttackOrBlock
        | StaticMode::AttackOnlyNeighbor
        | StaticMode::CantBecomeSuspected
        | StaticMode::MaxAttackersEachCombat { .. }
        | StaticMode::MaxBlockersEachCombat { .. }
        | StaticMode::CantBeTargeted
        | StaticMode::CantBeCast { .. }
        | StaticMode::CantSearchLibrary { .. }
        | StaticMode::RestrictLibrarySearchToTop { .. }
        // CR 723.1a: a `ProhibitionScope` player-scope decision-authority override —
        // no color / land / creature WORD (sibling of the two search prohibitions).
        | StaticMode::ControlPlayersDuringOwnLibrarySearch { .. }
        | StaticMode::CantCauseSacrificeOrExile { .. }
        | StaticMode::CastWithFlash
        | StaticMode::GrantsExtraVote
        | StaticMode::GrantsExtraVillainousChoice
        | StaticMode::CastWithAlternativeCost { .. }
        | StaticMode::AlternativeKeywordCost { .. }
        | StaticMode::ReduceActionCost { .. }
        | StaticMode::ModifyActivationLimit { .. }
        | StaticMode::ActivateAsInstant { .. }
        | StaticMode::CantGainLife
        | StaticMode::CantLoseLife
        | StaticMode::MustAttack
        | StaticMode::MustAttackPlayer { .. }
        | StaticMode::MustBlock
        | StaticMode::MustBlockAttacker { .. }
        | StaticMode::CantDraw { .. }
        | StaticMode::DrawFromBottom { .. }
        | StaticMode::DoubleTriggers { .. }
        | StaticMode::IgnoreHexproof
        | StaticMode::ExtraBlockers { .. }
        | StaticMode::RevealTopOfLibrary { .. }
        | StaticMode::RevealHand { .. }
        | StaticMode::GraveyardCastPermission { .. }
        | StaticMode::TopOfLibraryCastPermission { .. }
        | StaticMode::TopOfLibraryHasPlot
        | StaticMode::TopOfLibraryPlotPermission
        | StaticMode::CastFromHandFree { .. }
        | StaticMode::ExileCastPermission { .. }
        | StaticMode::LinkedCollectionCounterPlayPermission
        | StaticMode::CountersPersistAcrossZones { .. }
        | StaticMode::CantBeCountered
        | StaticMode::CantBeCopied
        | StaticMode::CantEnterBattlefieldFrom
        | StaticMode::CantCastFrom { .. }
        | StaticMode::CantCastDuring { .. }
        | StaticMode::CantActivateDuring { .. }
        | StaticMode::PerTurnDrawLimit { .. }
        | StaticMode::CantBeBlocked
        | StaticMode::CantBeBlockedByMoreThan { .. }
        | StaticMode::CantBeBlockedUnlessAllBlock
        | StaticMode::Protection
        | StaticMode::Indestructible
        | StaticMode::CantBeDestroyed
        | StaticMode::CantBeRegenerated
        | StaticMode::FlashBack
        | StaticMode::Shroud
        | StaticMode::Hexproof
        | StaticMode::Vigilance
        | StaticMode::Menace
        | StaticMode::Reach
        | StaticMode::Flying
        | StaticMode::Trample
        | StaticMode::Deathtouch
        | StaticMode::Lifelink
        | StaticMode::CantTap
        | StaticMode::CantUntap
        | StaticMode::Goaded
        // CR 508.1d + CR 701.15b: nullary combat requirement — the avoided
        // player rides `StaticDefinition::source_controller`, so this mode
        // prints no color/land/creature-type word for CR 612.2 to reach.
        | StaticMode::MustAttackAwayFromSource
        | StaticMode::CombatAlone { .. }
        | StaticMode::CantCrew
        | StaticMode::CantPhaseIn
        | StaticMode::CrewContribution { .. }
        | StaticMode::MayLookAtTopOfLibrary
        | StaticMode::MayLookAtFaceDown
        | StaticMode::CantBeTurnedFaceUp
        | StaticMode::MayChooseNotToUntap
        | StaticMode::AdditionalLandDrop { .. }
        | StaticMode::EmblemStatic
        | StaticMode::NoMaximumHandSize
        | StaticMode::MayPlayAdditionalLand
        | StaticMode::CantWinTheGame
        | StaticMode::CantLoseTheGame
        | StaticMode::LegendRuleDoesntApply
        | StaticMode::SpeedCanIncreaseBeyondFour
        | StaticMode::SkipStep { .. }
        | StaticMode::PayLifeAsColoredMana { .. }
        | StaticMode::CanAttackWithDefender
        | StaticMode::CanActivateAbilitiesAsThoughHaste
        | StaticMode::CanBlockShadow
        | StaticMode::AssignNoCombatDamage
        | StaticMode::UntapsDuringEachOtherPlayersUntapStep
        | StaticMode::EntersWithAdditionalCounters { .. }
        | StaticMode::CountersCantBeRemoved { .. }
        | StaticMode::CountsAsNamed { .. }
        | StaticMode::Other(..) => {}
    }
}

fn walk_static_definition(
    static_def: &mut StaticDefinition,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    // CR 612.1: the static's `mode` carries word-bearing evasion / protection /
    // cost-filter / color parameters ("can't be blocked by Goblins", "protection
    // from the chosen color", "black permanent spells cost less").
    walk_static_mode(&mut static_def.mode, category, cursor);
    if let Some(affected) = &mut static_def.affected {
        walk_target_filter(affected, category, cursor);
    }
    if let Some(condition) = &mut static_def.condition {
        walk_static_condition(condition, category, cursor);
    }
    // CR 101.2 + CR 109.5: the per-affected-player gate is a `ParsedCondition`
    // ("each opponent who controls a Goblin can't ...") naming a type/color/keyword.
    if let Some(per_player) = &mut static_def.per_player_condition {
        walk_parsed_condition(per_player, category, cursor);
    }
    for modification in static_def.modifications.iter_mut() {
        walk_continuous_modification(modification, category, cursor);
    }
}

/// CR 612.2a + CR 612.4 + CR 111.1: Walk a resolved [`TokenSpec`] creation
/// payload — the replacement-side sibling of the `Effect::Token` spec walked in
/// [`walk_effect`] (Chatterfang's "plus that many 1/1 green Squirrel creature
/// tokens", Academy Manufactor's Clue/Food/Treasure set).
///
/// The `display_name` + `subtypes` pair goes through [`WordCursor::token_spec`],
/// so CR 612.2a's "these words define both the creature types and the names"
/// carve-out applies here exactly as it does to `Effect::Token`. `colors` are
/// color words used as color words, `keywords` carry the usual landwalk /
/// protection color params, `static_abilities` are full granted statics, and a
/// `sacrifice_at` duration can gate on a word-bearing `StaticCondition`.
///
/// Not walked, and each is genuinely wordless or structurally excluded:
/// `core_types` / `supertypes` (CR 205.1a marker enums), `power` / `toughness`
/// and the already-resolved `enter_with_counters` counts (numbers), `source_id`
/// / `controller` / `attach_to` (runtime identity refs, CR 612.2), and
/// `script_name` — a serialized Forge script identifier (`r_1_1_goblin`), not
/// printed rules text. A spec that carries ONLY a script name declares no
/// `subtypes`, so CR 612.2a's "defined by these creature types" precondition is
/// unmet and the walk leaves it alone rather than guessing: coverage for that
/// import-only shape stays red instead of silently mis-substituting.
fn walk_token_spec(
    spec: &mut crate::types::proposed_event::TokenSpec,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    let characteristics = &mut spec.characteristics;
    cursor.token_spec(
        category,
        &mut characteristics.display_name,
        &mut characteristics.subtypes,
    );
    for color in characteristics.colors.iter_mut() {
        cursor.color(category, color);
    }
    for keyword in characteristics.keywords.iter_mut() {
        walk_keyword(keyword, category, cursor);
    }
    for static_def in spec.static_abilities.iter_mut() {
        walk_static_definition(static_def, category, cursor);
    }
    if let Some(dur) = &mut spec.sacrifice_at {
        walk_duration(dur, category, cursor);
    }
}

/// CR 614.1 + CR 612.1: Walk the word-bearing children of a replacement effect.
/// A creature type / land type / color word can live in the event-shape filter
/// (`valid_card` — "whenever a Goblin would enter"), the applicability
/// `condition` (`ReplacementCondition` — "unless you control a Plains"), the
/// resulting `execute` ability (its effects/filters), the damage source /
/// redirect filters, or the additional-token replacement's own subtypes.
///
/// CR 612.2a: the token-spec creation fields (`additional_token_spec` /
/// `ensure_token_specs`) ARE walked, via [`walk_token_spec`] — their creature-type
/// words define both the created token's types and its name, matching the
/// `Effect::Token` treatment in [`walk_effect`].
///
/// Intentionally NOT walked (coverage stays red rather than silently
/// mis-substituting): `runtime_execute` (a resolution-time `ResolvedAbility`
/// continuation snapshot, not printed rules text);
/// `mana_modification` (a mana SYMBOL, CR 612.2 + CR 107.4); `counter_match`
/// (a counter kind, CR 122.1, not a color/land/creature word); and the scalar
/// scope / expiry / player-axis fields.
fn walk_replacement_definition(
    replacement: &mut ReplacementDefinition,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    // CR 603.2 / CR 614.1: the event-shape filter names the object class the
    // replacement watches.
    if let Some(valid_card) = &mut replacement.valid_card {
        walk_target_filter(valid_card, category, cursor);
    }
    // CR 614.1c/d: the applicability condition.
    if let Some(condition) = &mut replacement.condition {
        walk_replacement_condition(condition, category, cursor);
    }
    // CR 614.1: the resulting effect (an ability with its own effects/filters).
    if let Some(execute) = &mut replacement.execute {
        walk_ability_definition(execute, category, cursor);
    }
    // CR 120.1 + CR 614.1a: damage source / redirect filters can name a type/color.
    if let Some(source) = &mut replacement.damage_source_filter {
        walk_target_filter(source, category, cursor);
    }
    if let Some(redirect) = &mut replacement.redirect_target {
        walk_target_filter(redirect, category, cursor);
    }
    // CR 612.2a + CR 614.1a: the appended / ensured token-creation specs.
    if let Some(spec) = &mut replacement.additional_token_spec {
        walk_token_spec(spec, category, cursor);
    }
    if let Some(specs) = &mut replacement.ensure_token_specs {
        for spec in specs.iter_mut() {
            walk_token_spec(spec, category, cursor);
        }
    }
    // CR 614.10 + CR 118.12a: an optional / pay-cost replacement carries a
    // `decline` continuation ability (and a `MayCost` payment) that are word-
    // bearing children — walked via [`walk_replacement_mode`].
    walk_replacement_mode(&mut replacement.mode, category, cursor);
}

/// CR 614.10 + CR 612.1: Walk the word-bearing children of a `ReplacementMode`.
/// The `Optional` / `MayCost` decline continuation is a full `AbilityDefinition`
/// (its effects/filters can name a type/color); `MayCost` additionally bundles an
/// `AbilityCost` whose object filter can name a type. `Mandatory` carries none.
/// No `_` wildcard — a future word-bearing `ReplacementMode` variant fails to
/// compile until classified.
fn walk_replacement_mode(
    mode: &mut ReplacementMode,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    match mode {
        ReplacementMode::Optional { decline } => {
            if let Some(d) = decline {
                walk_ability_definition(d, category, cursor);
            }
        }
        ReplacementMode::MayCost { cost, decline } => {
            walk_ability_cost(cost, category, cursor);
            if let Some(d) = decline {
                walk_ability_definition(d, category, cursor);
            }
        }
        ReplacementMode::Mandatory => {}
    }
}

/// CR 614.1c/d + CR 612.1: Walk the word-bearing children of a
/// `ReplacementCondition`. A creature type / land type can live in a control /
/// token / damage-source subtype leaf ("unless you control a Plains", "if you
/// control a Goblin", "if you would create a Treasure token"); a color word in a
/// filter's color predicate. Subtype string sets route through the
/// category-disambiguating [`WordCursor::subtype`]; filters recurse via
/// [`walk_target_filter`] / [`walk_typed_filter`]; quantity gates via
/// [`walk_quantity_expr`]. No `_` wildcard — a future word-bearing
/// `ReplacementCondition` variant fails to compile until classified.
fn walk_replacement_condition(
    condition: &mut ReplacementCondition,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    match condition {
        ReplacementCondition::And { conditions } => {
            for c in conditions.iter_mut() {
                walk_replacement_condition(c, category, cursor);
            }
        }
        // CR 205.3: subtype-string leaves naming a land / creature type WORD.
        ReplacementCondition::UnlessControlsSubtype { subtypes }
        | ReplacementCondition::TokenSubtypeMatches { subtypes } => {
            for s in subtypes.iter_mut() {
                cursor.subtype(category, s);
            }
        }
        // CR 614.1d: control-count / control-presence gates carry a `TargetFilter`.
        ReplacementCondition::UnlessControlsMatching { filter }
        | ReplacementCondition::UnlessControlsCountMatching { filter, .. }
        | ReplacementCondition::IfControlsMatching { filter, .. }
        // CR 120.1 + CR 614.1a: damage-source gate naming a type/color.
        | ReplacementCondition::DealtDamageThisTurnBySource { source: filter } => {
            walk_target_filter(filter, category, cursor)
        }
        // CR 614.1c: fast-land control gate carries a `TypedFilter`.
        ReplacementCondition::UnlessControlsOtherLeq { filter, .. } => {
            walk_typed_filter(filter, category, cursor)
        }
        ReplacementCondition::UnlessQuantity { lhs, rhs, .. }
        | ReplacementCondition::OnlyIfQuantity { lhs, rhs, .. } => {
            walk_quantity_expr(lhs, category, cursor);
            walk_quantity_expr(rhs, category, cursor);
        }
        // CR 612.2: no color/land/creature WORD used as such — life / turn /
        // player-count / speed / cast-variant / zone-origin / kicker / counter-type
        // / core-type / draw-step / class-level / control-of-source / free-text
        // gates. (`TokenCoreTypeMatches` names a CORE type per CR 111.1, not a
        // subtype word; `Unrecognized` is opaque deferred text.)
        ReplacementCondition::UnlessPlayerLifeAtMost { .. }
        | ReplacementCondition::UnlessMultipleOpponents
        | ReplacementCondition::UnlessYourTurn
        | ReplacementCondition::HasMaxSpeed
        | ReplacementCondition::CastViaEscape
        | ReplacementCondition::CastVariantPaid { .. }
        | ReplacementCondition::CastFromZone { .. }
        | ReplacementCondition::EnteredFromZone { .. }
        | ReplacementCondition::YouAttackedThisTurn
        | ReplacementCondition::OpponentDamagedThisTurn
        | ReplacementCondition::CastViaKicker { .. }
        | ReplacementCondition::SourceTappedState { .. }
        | ReplacementCondition::EventSourceControlledBy { .. }
        | ReplacementCondition::EffectCausedDiscard
        | ReplacementCondition::OnlyExtraTurn
        | ReplacementCondition::TokenCoreTypeMatches { .. }
        | ReplacementCondition::FirstTokenCreationEachTurn { .. }
        | ReplacementCondition::ExceptFirstDrawInDrawStep
        | ReplacementCondition::ClassLevelGE { .. }
        | ReplacementCondition::DuringUntapStep
        | ReplacementCondition::DuringDrawStep { .. }
        | ReplacementCondition::ControllerControlsSource { .. }
        | ReplacementCondition::Unrecognized { .. } => {}
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
        // CR 613.1f + CR 612.1: a granted rule-modification static `mode` carries
        // the same word-bearing evasion / protection / cost-filter / color params as
        // any static's mode (e.g. a granted "can't be blocked by Goblins").
        ContinuousModification::AddStaticMode { mode } => {
            walk_static_mode(mode, category, cursor)
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
        // CR 707.9f + CR 612.1: the enters-with counter `count` is a `QuantityExpr`
        // whose typed `ObjectCount` filter can name a type/color word (usually a
        // `Fixed` scalar).
        ContinuousModification::AddCounterOnEnter { count, .. } => {
            walk_quantity_expr(count, category, cursor)
        }
        ContinuousModification::CopyValues { .. }
        | ContinuousModification::SetName { .. }
        // CR 612.2 + CR 612.8: a literal NAME is not a color/land/creature word
        // used as such — names are structurally excluded from the walk (sibling of
        // `SetName` / `SetChosenName`).
        | ContinuousModification::SetTextName { .. }
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
        // CR 707.9a: a nullary "retain this object's other abilities" copy marker —
        // it names no word; the retained abilities are read from the source object's
        // own base sets, which the layer system re-seeds and re-walks (sibling of
        // the two `RetainPrinted*FromSource` markers).
        | ContinuousModification::RetainAllOtherAbilitiesFromSource
        | ContinuousModification::AddSupertype { .. }
        | ContinuousModification::RemoveSupertype { .. }
        | ContinuousModification::SetStartingLoyalty { .. }
        | ContinuousModification::RemoveManaCost => {}
    }
}

/// CR 603.7 + CR 612.1: Walk the word-bearing children of a
/// `DelayedTriggerCondition`. A creature type / land type / color word can live in
/// a filtered firing gate ("when a Goblin dies", "when a Forest enters") or an
/// embedded event trigger (`WheneverEvent` / `WhenNextEvent`). Filters recurse via
/// [`walk_target_filter`]; embedded triggers via [`walk_trigger_definition`]. The
/// phase / object-id / player timing markers carry no printed word. No `_`
/// wildcard — a future word-bearing variant fails to compile until classified.
fn walk_delayed_trigger_condition(
    condition: &mut DelayedTriggerCondition,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    match condition {
        DelayedTriggerCondition::WhenDies { filter }
        | DelayedTriggerCondition::WhenLeavesPlayFiltered { filter }
        | DelayedTriggerCondition::WhenEntersBattlefield { filter }
        | DelayedTriggerCondition::WhenDiesOrExiled { filter } => {
            walk_target_filter(filter, category, cursor)
        }
        DelayedTriggerCondition::WheneverEvent { trigger } => {
            walk_trigger_definition(trigger, category, cursor)
        }
        DelayedTriggerCondition::WhenNextEvent {
            trigger,
            or_trigger,
            ..
        } => {
            walk_trigger_definition(trigger, category, cursor);
            if let Some(or_t) = or_trigger {
                walk_trigger_definition(or_t, category, cursor);
            }
        }
        DelayedTriggerCondition::AtNextPhase { .. }
        | DelayedTriggerCondition::AtNextPhaseForPlayer { .. }
        | DelayedTriggerCondition::WhenLeavesPlay { .. } => {}
    }
}

/// CR 603.7a + CR 612.1: Walk the word-bearing children of an `ExiledSpellRider`.
/// Only the `ReturnTo` timing (a `DelayedTriggerCondition`) can carry a filtered
/// firing gate; `BecomePlotted` carries none. No `_` wildcard.
fn walk_exiled_spell_rider(
    rider: &mut ExiledSpellRider,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    match rider {
        ExiledSpellRider::ReturnTo { timing, .. } => {
            walk_delayed_trigger_condition(timing, category, cursor)
        }
        ExiledSpellRider::BecomePlotted => {}
    }
}

/// CR 612.2 + CR 708.2a: Walk a face-down-entry characteristics spec. The
/// `subtypes` an effect specifies for a face-down permanent ("They're 2/2
/// **Cyberman** artifact creatures.", "It's a **Forest** land.") are words used
/// as subtypes in exactly the sense of CR 612.2, so they route through the same
/// category-disciplined [`WordCursor::subtype`] path as a printed type line —
/// the CR 612.2a token-spec sibling for face-down bodies.
///
/// The remaining fields are genuinely wordless: `power` / `toughness` are
/// numbers, `body` / `extra_core_types` are `CoreType` markers (CR 205.1a — not
/// a color/land/creature WORD), and `ward` is a mana-symbol cost (CR 107.4 — a
/// pip, not a color word), consistent with the module-level pip exclusion.
fn walk_face_down_profile(
    profile: &mut crate::types::ability::FaceDownProfile,
    category: TextWordCategory,
    cursor: &mut WordCursor,
) {
    for subtype in profile.subtypes.iter_mut() {
        cursor.subtype(category, subtype);
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
        // CR 603.7: a delayed trigger carries both its firing `condition`
        // (`DelayedTriggerCondition` — its object filter / embedded trigger can
        // name a type) and its resulting `effect` ability.
        Effect::CreateDelayedTrigger {
            condition, effect, ..
        } => {
            walk_delayed_trigger_condition(condition, category, cursor);
            walk_ability_definition(effect, category, cursor);
        }
        // CR 603.7a + CR 608.2n: a Feather-style exile-instead-of-graveyard rider
        // can arm a filtered delayed return trigger whose timing carries a filter.
        Effect::ExileResolvingSpellInsteadOfGraveyard { on_exile, .. } => {
            if let Some(rider) = on_exile {
                walk_exiled_spell_rider(rider, category, cursor);
            }
        }
        // CR 701.6a + CR 611.2: a counter effect's `source_rider` can install a
        // static ability on the countered source ("that permanent loses all
        // abilities for as long as ~"), a full `StaticDefinition` + `Duration`.
        Effect::Counter {
            source_rider: Some(rider),
            ..
        } => match rider {
            CounterSourceRider::LosesAbilities {
                static_def,
                duration,
            } => {
                walk_static_definition(static_def, category, cursor);
                walk_duration(duration, category, cursor);
            }
            CounterSourceRider::Destroy => {}
        },
        // CR 614.1 + CR 612.1: "the next [filter] you cast this turn gains
        // [replacement]" installs a full `ReplacementDefinition` on a chosen target
        // — both the installed replacement (its event filter / condition / effect)
        // and the `target` filter naming the recipient class are word-bearing
        // carriers. Not surfaced by `target_filter_mut` (returns `None`), so both
        // are walked here.
        Effect::AddTargetReplacement {
            replacement,
            target,
        } => {
            walk_replacement_definition(replacement, category, cursor);
            walk_target_filter(target, category, cursor);
        }
        // CR 705.2: `flipper` (who flips) is a `TargetFilter` player ref not
        // surfaced by `target_filter_mut` (returns `None`); walking it keeps the
        // per-carrier sweep total (player refs are wordless leaves, but a future
        // typed variant is reached). `FlipCoins` additionally carries a `count`.
        Effect::FlipCoin {
            win_effect,
            lose_effect,
            flipper,
        } => {
            if let Some(w) = win_effect {
                walk_ability_definition(w, category, cursor);
            }
            if let Some(l) = lose_effect {
                walk_ability_definition(l, category, cursor);
            }
            walk_target_filter(flipper, category, cursor);
        }
        Effect::FlipCoins {
            count,
            win_effect,
            lose_effect,
            flipper,
        } => {
            walk_quantity_expr(count, category, cursor);
            if let Some(w) = win_effect {
                walk_ability_definition(w, category, cursor);
            }
            if let Some(l) = lose_effect {
                walk_ability_definition(l, category, cursor);
            }
            walk_target_filter(flipper, category, cursor);
        }
        Effect::FlipCoinUntilLose { win_effect } => {
            walk_ability_definition(win_effect, category, cursor)
        }
        // CR 706.1: `count` ("roll X dice") is a `QuantityExpr` carrier alongside
        // each result branch's effect ability.
        Effect::RollDie { count, results, .. } => {
            walk_quantity_expr(count, category, cursor);
            for branch in results.iter_mut() {
                walk_ability_definition(&mut branch.effect, category, cursor);
            }
        }
        // CR 701.55: `chooser` is a `PlayerFilter` whose control-count sub-filter
        // can name a type; each branch is a full sub-ability.
        Effect::ChooseOneOf { chooser, branches } => {
            walk_player_filter(chooser, category, cursor);
            for branch in branches.iter_mut() {
                walk_ability_definition(branch, category, cursor);
            }
        }
        // CR 701.38b: a `Named` vote resolves `per_choice_effect` per ballot; an
        // `Objects` vote enumerates candidates via `candidate_filter` and resolves
        // `outcome_template` per winner (Council's Judgment). Both the candidate
        // filter and the outcome template are word-bearing carriers naming the
        // voted-on object class / its resulting effect.
        Effect::Vote {
            per_choice_effect,
            subject,
            ..
        } => {
            for sub in per_choice_effect.iter_mut() {
                walk_ability_definition(sub, category, cursor);
            }
            match subject {
                VoteSubject::Objects {
                    candidate_filter,
                    outcome_template,
                } => {
                    walk_target_filter(candidate_filter, category, cursor);
                    walk_ability_definition(outcome_template, category, cursor);
                }
                VoteSubject::Named => {}
            }
        }
        // CR 700.3: `object_filter` constrains each subject's eligible set (a typed
        // "creatures" / "Goblins" filter) — a word-bearing carrier not surfaced by
        // `target_filter_mut` (`SeparateIntoPiles` has no targeting slot).
        Effect::SeparateIntoPiles {
            object_filter,
            chosen_pile_effect,
            unchosen_pile_effect,
            ..
        } => {
            walk_target_filter(object_filter, category, cursor);
            walk_ability_definition(chosen_pile_effect, category, cursor);
            if let Some(unchosen) = unchosen_pile_effect {
                walk_ability_definition(unchosen, category, cursor);
            }
        }
        // CR 701.20a: `filter` restricts the self-reveal ("reveal a [type] card from
        // your hand") — a word-bearing carrier not surfaced by `target_filter_mut`.
        Effect::RevealFromHand { filter, on_decline } => {
            walk_target_filter(filter, category, cursor);
            if let Some(sub) = on_decline {
                walk_ability_definition(sub, category, cursor);
            }
        }
        // CR 611.2b: `GenericEffect` additionally carries a "for as long as"
        // `duration` whose `StaticCondition` can name a type/color word.
        Effect::GenericEffect {
            static_abilities,
            duration,
            ..
        } => {
            for static_def in static_abilities.iter_mut() {
                walk_static_definition(static_def, category, cursor);
            }
            if let Some(dur) = duration {
                walk_duration(dur, category, cursor);
            }
        }
        // CR 612.2a + CR 612.4: the token-creation spec is a first-class word
        // carrier, not a red surface. Its `types` are subtypes used as subtypes
        // and its `name` is defined by those creature types (the CR 612.2a
        // carve-out from the CR 612.2 name protection) — both go through
        // [`WordCursor::token_spec`]. The spec's `colors` are color words used as
        // color words ("a 1/1 **red** Goblin creature token"), its granted
        // `keywords` carry the same landwalk / protection color params as any
        // printed keyword, and its `count` / `enter_with_counters` quantities can
        // embed a typed `ObjectCount` filter ("for each Goblin you control").
        // The `owner` / `attach_to` axes are already reached above via
        // `target_filter_mut`.
        Effect::Token {
            name,
            types,
            colors,
            keywords,
            static_abilities,
            count,
            enter_with_counters,
            ..
        } => {
            cursor.token_spec(category, name, types);
            for color in colors.iter_mut() {
                cursor.color(category, color);
            }
            for keyword in keywords.iter_mut() {
                walk_keyword(keyword, category, cursor);
            }
            for static_def in static_abilities.iter_mut() {
                walk_static_definition(static_def, category, cursor);
            }
            walk_quantity_expr(count, category, cursor);
            for (_, qty) in enter_with_counters.iter_mut() {
                walk_quantity_expr(qty, category, cursor);
            }
        }
        // CR 611.2b: effects that install a continuous modification carry a
        // `duration` (`Option<Duration>`/`Duration`) whose `ForAsLongAs` shape
        // gates on a word-bearing `StaticCondition`. Their primary target filter
        // is already reached above via `target_filter_mut`; here the duration is
        // the additional word-bearing child. (`PreventDamage.damage_source_filter`
        // and `CastFromZone.alt_ability_cost` are secondary effect filters left
        // intentionally red, consistent with the mass-filter exclusion documented
        // below.)
        // `ChangeTextWords.excluded_to` holds concrete `TextWord` operands (not
        // words used as words on this object — sibling to `ReplaceTextWord`);
        // `CastFromZone.alt_ability_cost` is the intentionally-red secondary cast
        // cost (see [`walk_object_words`]). Only the `duration` is walked.
        Effect::ChangeTextWords { duration, .. } | Effect::CastFromZone { duration, .. } => {
            if let Some(dur) = duration {
                walk_duration(dur, category, cursor);
            }
        }
        // CR 707.2 + CR 611.2c: `recipient` (the object(s) that become the copy —
        // "Shards you control") and the `additional_modifications` "except …"
        // exceptions are word-bearing carriers alongside the `duration`; the copy
        // *source* `target` is reached via `target_filter_mut`.
        Effect::BecomeCopy {
            recipient,
            duration,
            additional_modifications,
            ..
        } => {
            walk_target_filter(recipient, category, cursor);
            for modification in additional_modifications.iter_mut() {
                walk_continuous_modification(modification, category, cursor);
            }
            if let Some(dur) = duration {
                walk_duration(dur, category, cursor);
            }
        }
        // CR 611.2c: `recipient` (who receives the abilities — a typed group filter
        // "each Horror you control") is a word-bearing carrier alongside `duration`;
        // the donor `target` is reached via `target_filter_mut`.
        Effect::GainActivatedAbilitiesOfTarget {
            recipient,
            duration,
            ..
        } => {
            walk_target_filter(recipient, category, cursor);
            if let Some(dur) = duration {
                walk_duration(dur, category, cursor);
            }
        }
        // CR 615.11: `amount_dynamic` ("prevent X … where X is <quantity>") is a
        // word-bearing `QuantityExpr` carrier alongside the shield `duration`. The
        // `damage_source_filter` stays intentionally red (see [`walk_object_words`]).
        Effect::PreventDamage {
            amount_dynamic,
            prevention_duration,
            ..
        } => {
            if let Some(amount) = amount_dynamic {
                walk_quantity_expr(amount, category, cursor);
            }
            if let Some(dur) = prevention_duration {
                walk_duration(dur, category, cursor);
            }
        }
        // CR 508.1d: `required_player` (whom the creature must attack) is a
        // `TargetFilter` carrier alongside the `duration`; the attacker `target`
        // is reached via `target_filter_mut`.
        Effect::ForceAttack {
            required_player,
            duration,
            ..
        } => {
            walk_target_filter(required_player, category, cursor);
            walk_duration(duration, category, cursor);
        }
        Effect::CreateEmblem { statics, triggers } => {
            for static_def in statics.iter_mut() {
                walk_static_definition(static_def, category, cursor);
            }
            for trigger in triggers.iter_mut() {
                walk_trigger_definition(trigger, category, cursor);
            }
        }
        // ================= CR 612.1: secondary word-bearing carriers =================
        // Every arm below walks the carrier fields NOT already reached through the
        // `target_filter_mut()` primary-target walk above. Effects whose only
        // carrier is that primary target (or which are genuinely wordless / hold an
        // intentionally-red mass or creation-spec field) fall through to the final
        // no-op group.

        // ---- PlayerFilter roots (CR 109.5 — control-count / attribute sub-filters) ----
        Effect::StartYourEngines { player_scope } => {
            walk_player_filter(player_scope, category, cursor)
        }
        Effect::ChangeSpeed {
            player_scope,
            amount,
            ..
        } => {
            walk_player_filter(player_scope, category, cursor);
            walk_quantity_expr(amount, category, cursor);
        }
        Effect::DamageEachPlayer {
            amount,
            player_filter,
        } => {
            walk_quantity_expr(amount, category, cursor);
            walk_player_filter(player_filter, category, cursor);
        }
        // CR 120.3: DamageAll's `target` is a mass object-population filter (left
        // red like DestroyAll), but its `amount` and `player_filter` are ordinary
        // carriers ("N damage where N is the number of Goblins", "each opponent").
        Effect::DamageAll {
            amount,
            player_filter,
            ..
        } => {
            walk_quantity_expr(amount, category, cursor);
            if let Some(pf) = player_filter {
                walk_player_filter(pf, category, cursor);
            }
        }
        // CR 101.2: Conjure's `library_players` scopes which players' libraries
        // receive the conjured cards (an "each player" `PlayerFilter`).
        Effect::Conjure {
            library_players, ..
        } => {
            if let Some(pf) = library_players {
                walk_player_filter(pf, category, cursor);
            }
        }

        // ---- Single-`amount` QuantityExpr carriers (CR 107.3 + CR 608.2c) ----
        Effect::DealDamage { amount, .. }
        | Effect::GainLife { amount, .. }
        | Effect::GainEnergy { amount }
        | Effect::SetLifeTotal { amount, .. }
        | Effect::GrantExtraLoyaltyActivations { amount, .. }
        | Effect::Intensify { amount, .. } => walk_quantity_expr(amount, category, cursor),
        Effect::LoseLife { amount, .. } => walk_quantity_expr(amount, category, cursor),
        Effect::Discover {
            mana_value_limit, ..
        } => walk_quantity_expr(mana_value_limit, category, cursor),

        // ---- Single-`count` QuantityExpr carriers (CR 107.3 + CR 608.2c) ----
        // The `target` (or `player`) each carries is reached via `target_filter_mut`
        // (or is a mass filter left red); here the count is the added carrier.
        Effect::Draw { count, .. }
        | Effect::RemoveCounter { count, .. }
        | Effect::Sacrifice { count, .. }
        | Effect::Mill { count, .. }
        | Effect::Scry { count, .. }
        | Effect::Surveil { count, .. }
        | Effect::Connive { count, .. }
        | Effect::PutCounter { count, .. }
        | Effect::PutChosenCounter { count, .. }
        | Effect::PutCounterAll { count, .. }
        | Effect::ChooseCounterAdjustment { count, .. }
        | Effect::ExileTop { count, .. }
        | Effect::GivePlayerCounter { count, .. }
        | Effect::AddPendingETBCounters { count, .. }
        | Effect::SkipNextTurn { count, .. }
        | Effect::SkipNextStep { count, .. }
        | Effect::PutAtLibraryPosition { count, .. }
        | Effect::AssembleContraptions { count }
        | Effect::Incubate { count, .. }
        | Effect::Monstrosity { count }
        | Effect::Renown { count }
        | Effect::Bolster { count, .. }
        | Effect::Adapt { count, .. } => walk_quantity_expr(count, category, cursor),

        // ---- PtValue quantities (CR 613.4) ----
        Effect::Pump {
            power, toughness, ..
        }
        | Effect::PumpAll {
            power, toughness, ..
        } => {
            walk_pt_value(power, category, cursor);
            walk_pt_value(toughness, category, cursor);
        }
        // CR 613.4 / CR 205.1a: Animate grants base P/T (`Option<PtValue>`), a
        // typed-subtype `types`/`remove_types` set, and granted `keywords`.
        Effect::Animate {
            power,
            toughness,
            types,
            remove_types,
            keywords,
            ..
        } => {
            if let Some(p) = power {
                walk_pt_value(p, category, cursor);
            }
            if let Some(t) = toughness {
                walk_pt_value(t, category, cursor);
            }
            for subtype in types.iter_mut().chain(remove_types.iter_mut()) {
                cursor.subtype(category, subtype);
            }
            for keyword in keywords.iter_mut() {
                walk_keyword(keyword, category, cursor);
            }
        }

        // ---- Multi-carrier damage effects ----
        // CR 120.1: both source groups and the shared recipient are word-bearing
        // filters (none surfaced by `target_filter_mut`).
        Effect::EachDealsDamageEqualToPower {
            sources,
            recipient,
            extra_source,
        } => {
            walk_target_filter(sources, category, cursor);
            walk_target_filter(recipient, category, cursor);
            if let Some(extra) = extra_source {
                walk_target_filter(extra, category, cursor);
            }
        }
        // CR 120.1 + CR 608.2: the source class and per-batch amount; the `Shared`
        // recipient is reached via `target_filter_mut`, `EachController` is wordless.
        Effect::EachSourceDealsDamage {
            sources, amount, ..
        } => {
            walk_target_filter(sources, category, cursor);
            walk_quantity_expr(amount, category, cursor);
        }

        // ---- Secondary TargetFilter carriers (CR 612.2) ----
        Effect::Attach { attachment, .. } | Effect::UnattachAll { attachment, .. } => {
            walk_target_filter(attachment, category, cursor)
        }
        Effect::Fight { subject, .. } => walk_target_filter(subject, category, cursor),
        // CR 701.63a: the enduring permanent (`subject`) plus the N/N counter count.
        Effect::Endure { amount, subject } => {
            walk_quantity_expr(amount, category, cursor);
            walk_target_filter(subject, category, cursor);
        }
        Effect::Behold { filter } | Effect::FreeCastFromZones { filter, .. } => {
            walk_target_filter(filter, category, cursor)
        }
        // `ChooseAugmentAndCombineWithHost.filter` is a `Box<TargetFilter>`; the
        // `host` is reached via `target_filter_mut`.
        Effect::ChooseAugmentAndCombineWithHost { filter, .. } => {
            walk_target_filter(filter, category, cursor)
        }
        Effect::ChooseDamageSource { source_filter } => {
            walk_target_filter(source_filter, category, cursor)
        }
        Effect::GiveControl { recipient, .. } => walk_target_filter(recipient, category, cursor),
        Effect::TurnFaceUp { target } => walk_target_filter(target, category, cursor),
        Effect::ExchangeControl { target_a, target_b }
        | Effect::ExchangeLifeTotals {
            player_a: target_a,
            player_b: target_b,
        } => {
            walk_target_filter(target_a, category, cursor);
            walk_target_filter(target_b, category, cursor);
        }
        Effect::Meld {
            source_filter,
            partner_filter,
            ..
        } => {
            walk_target_filter(source_filter, category, cursor);
            walk_target_filter(partner_filter, category, cursor);
        }
        // CR 707.2 + CR 707.9: `CopyTokenOf` takes its name / types / colors from
        // the COPIED object (CR 707.2), so it has no printed token name or type
        // list of its own and CR 612.2a has nothing to reach there — the copied
        // object's own words are text-changed on that object, not here. What it
        // does print is the non-targeting copy-source `source_filter` ("for each
        // Goblin you control, create a token that's a copy of it"), the "except it
        // has …" `extra_keywords` / `additional_modifications` riders, and the
        // token `count`. The `target` / `owner` pair follows the shared
        // `target_filter_mut` convention documented on [`walk_effect`].
        Effect::CopyTokenOf {
            source_filter,
            count,
            extra_keywords,
            additional_modifications,
            ..
        } => {
            if let Some(f) = source_filter {
                walk_target_filter(f, category, cursor);
            }
            walk_quantity_expr(count, category, cursor);
            for keyword in extra_keywords.iter_mut() {
                walk_keyword(keyword, category, cursor);
            }
            for modification in additional_modifications.iter_mut() {
                walk_continuous_modification(modification, category, cursor);
            }
        }
        Effect::CopyTokenBlockingAttacker {
            source_filter,
            owner,
        } => {
            walk_target_filter(source_filter, category, cursor);
            walk_target_filter(owner, category, cursor);
        }
        // CR 701.9a: the format-pool copy source's owner + type filter and its
        // mana-value bound / token count (inventory carriers, not a token
        // creation-spec exclusion).
        Effect::CreateTokenCopyFromPool {
            owner,
            type_filter,
            mv_bound,
            count,
            ..
        } => {
            walk_target_filter(owner, category, cursor);
            walk_target_filter(type_filter, category, cursor);
            walk_quantity_expr(mv_bound, category, cursor);
            walk_quantity_expr(count, category, cursor);
        }
        Effect::ChangeTargets { forced_to, .. } => {
            if let Some(f) = forced_to {
                walk_target_filter(f, category, cursor);
            }
        }
        Effect::ReduceNextSpellCost { spell_filter, .. }
        | Effect::GrantNextSpellAbility { spell_filter, .. } => {
            if let Some(f) = spell_filter {
                walk_target_filter(f, category, cursor);
            }
        }
        Effect::ChooseFromZone { filter, .. } => {
            if let Some(f) = filter {
                walk_target_filter(f, category, cursor);
            }
        }
        Effect::ChooseObjectsIntoTrackedSet {
            chooser, filter, ..
        } => {
            walk_target_filter(chooser, category, cursor);
            walk_target_filter(filter, category, cursor);
        }
        Effect::ChooseAndSacrificeRest {
            choose_filter,
            sacrifice_filter,
            total_power_cap,
            ..
        } => {
            walk_target_filter(choose_filter, category, cursor);
            walk_target_filter(sacrifice_filter, category, cursor);
            if let Some(cap) = total_power_cap {
                walk_quantity_expr(cap, category, cursor);
            }
        }
        // CR 614.9 + CR 615: ordinary source/recipient object filters are walked;
        // the specialized `DamageTargetFilter` / `DamageRedirectTarget` axes are
        // not among the listed carrier types (left red).
        Effect::CreateDamageReplacement {
            source_filter,
            redirect_object_filter,
            recipient_object_filter,
            ..
        } => {
            for f in [
                source_filter,
                redirect_object_filter,
                recipient_object_filter,
            ]
            .into_iter()
            .flatten()
            {
                walk_target_filter(f, category, cursor);
            }
        }
        Effect::AssembleContraptionOnSprocket { target, .. } => {
            walk_target_filter(target, category, cursor)
        }

        // ---- Effects with a `target`/`player` primary NOT surfaced by
        // `target_filter_mut` (in its `None` group) plus a count / filter ----
        // CR 708.2a: the manifest body's effect-specified `profile` subtypes are
        // words used as subtypes (see [`walk_face_down_profile`]).
        Effect::Manifest {
            target,
            count,
            profile,
            ..
        } => {
            walk_target_filter(target, category, cursor);
            walk_quantity_expr(count, category, cursor);
            if let Some(p) = profile {
                walk_face_down_profile(p, category, cursor);
            }
        }
        // CR 708.2a + CR 708.2b: "Turn target creature face down. It's a 2/2
        // Cyberman artifact creature." — the specified body's subtypes are words
        // used as subtypes; `target` is reached via `target_filter_mut`.
        Effect::TurnFaceDown { profile, .. } => {
            if let Some(p) = profile {
                walk_face_down_profile(p, category, cursor);
            }
        }
        Effect::Cloak {
            target,
            count,
            object_source,
            // CR 110.2a: `enters_under` is an `Option<ControllerRef>` — a
            // player-role reference, not a printed word, so CR 612.2 has
            // nothing to replace in it (mirrors `Effect::Manifest`).
            enters_under: _,
        } => {
            walk_target_filter(target, category, cursor);
            walk_quantity_expr(count, category, cursor);
            if let Some(f) = object_source {
                walk_target_filter(f, category, cursor);
            }
        }
        Effect::ExileFromTopUntil { until, .. } => walk_until_condition(until, category, cursor),
        Effect::RevealUntil { filter, count, .. } | Effect::Seek { filter, count, .. } => {
            walk_target_filter(filter, category, cursor);
            walk_quantity_expr(count, category, cursor);
        }
        Effect::SearchLibrary { filter, count, .. }
        | Effect::SearchOutsideGame { filter, count, .. } => {
            walk_target_filter(filter, category, cursor);
            walk_quantity_expr(count, category, cursor);
        }
        Effect::Dig {
            count,
            keep_count_expr,
            filter,
            ..
        } => {
            walk_quantity_expr(count, category, cursor);
            if let Some(k) = keep_count_expr {
                walk_quantity_expr(k, category, cursor);
            }
            walk_target_filter(filter, category, cursor);
        }
        // CR 612.2 + CR 901.4: the planar-deck analog of `Dig`. The planar deck
        // holds only plane / phenomenon cards, so there is no object filter here,
        // but both counts are `QuantityExpr`s whose typed `ObjectCount` filter can
        // name a creature/land/color word ("look at the top X, where X is …").
        Effect::ArrangePlanarDeckTop { count, keep_on_top } => {
            walk_quantity_expr(count, category, cursor);
            walk_quantity_expr(keep_on_top, category, cursor);
        }
        Effect::RevealHand {
            card_filter, count, ..
        } => {
            walk_target_filter(card_filter, category, cursor);
            if let Some(c) = count {
                walk_quantity_expr(c, category, cursor);
            }
        }
        Effect::Discard {
            count,
            unless_filter,
            filter,
            ..
        } => {
            walk_quantity_expr(count, category, cursor);
            if let Some(f) = unless_filter {
                walk_target_filter(f, category, cursor);
            }
            if let Some(f) = filter {
                walk_target_filter(f, category, cursor);
            }
        }
        Effect::MoveCounters { source, count, .. } => {
            walk_target_filter(source, category, cursor);
            if let Some(c) = count {
                walk_quantity_expr(c, category, cursor);
            }
        }
        Effect::CastCopyOfCard { count, .. } | Effect::BounceAll { count, .. } => {
            if let Some(c) = count {
                walk_quantity_expr(c, category, cursor);
            }
        }
        Effect::PutSticker {
            count,
            max_ticket_cost,
            ..
        } => {
            walk_quantity_expr(count, category, cursor);
            if let Some(m) = max_ticket_cost {
                walk_quantity_expr(m, category, cursor);
            }
        }
        Effect::ChooseDrawnThisTurnPayOrTopdeck {
            count,
            life_payment,
            ..
        } => {
            walk_quantity_expr(count, category, cursor);
            walk_quantity_expr(life_payment, category, cursor);
        }
        // CR 508.1c: the additional combat's `attacker_restriction` filter plus the
        // count of scheduled phases.
        Effect::AdditionalPhase {
            count,
            attacker_restriction,
            ..
        } => {
            walk_quantity_expr(count, category, cursor);
            if let Some(f) = attacker_restriction {
                walk_target_filter(f, category, cursor);
            }
        }
        // CR 400.7 + CR 614.1c: enters-with counter quantities and the conditional
        // enters-with gate filter (the mass `ChangeZoneAll` counterpart walks only
        // the counter quantities; its object `target` is a mass filter left red).
        Effect::ChangeZone {
            enter_with_counters,
            conditional_enter_with_counters,
            enters_modified_if,
            face_down_profile,
            ..
        } => {
            for (_, count) in enter_with_counters.iter_mut() {
                walk_quantity_expr(count, category, cursor);
            }
            for (gate, _, count) in conditional_enter_with_counters.iter_mut() {
                walk_target_filter(gate, category, cursor);
                walk_quantity_expr(count, category, cursor);
            }
            if let Some(f) = enters_modified_if {
                walk_target_filter(f, category, cursor);
            }
            // CR 708.2a: the face-down entry body's subtypes.
            if let Some(p) = face_down_profile {
                walk_face_down_profile(p, category, cursor);
            }
        }
        Effect::ChangeZoneAll {
            enter_with_counters,
            face_down_profile,
            ..
        } => {
            for (_, count) in enter_with_counters.iter_mut() {
                walk_quantity_expr(count, category, cursor);
            }
            if let Some(p) = face_down_profile {
                walk_face_down_profile(p, category, cursor);
            }
        }

        // ---- Vec<ContinuousModification> secondary carriers (CR 707.9) ----
        Effect::CopySpell {
            additional_modifications,
            ..
        }
        | Effect::AddPendingEntersModifications {
            modifications: additional_modifications,
        } => {
            for modification in additional_modifications.iter_mut() {
                walk_continuous_modification(modification, category, cursor);
            }
        }
        // CR 707.2 + CR 707.9: the eligible object class plus the "except …"
        // modifications applied to each created copy.
        Effect::EachPlayerCopyChosen {
            choose_filter,
            copy_modifications,
            ..
        } => {
            walk_target_filter(choose_filter, category, cursor);
            for modification in copy_modifications.iter_mut() {
                walk_continuous_modification(modification, category, cursor);
            }
        }
        // CR 702.5a + CR 604.1: the Aura enchant filter and the granted body's
        // continuous modifications.
        Effect::ReturnAsAura {
            enchant_filter,
            grants,
        } => {
            walk_target_filter(enchant_filter, category, cursor);
            for modification in grants.iter_mut() {
                walk_continuous_modification(modification, category, cursor);
            }
        }

        // ---- AbilityCost / subtype carriers ----
        // CR 118.1: a resolution-time `PayCost` bundles a full `AbilityCost` (whose
        // object filter can name a type — "Sacrifice a Goblin"), an optional scale
        // quantity, and the payer filter.
        Effect::PayCost { cost, scale, payer } => {
            walk_ability_cost(cost, category, cursor);
            if let Some(s) = scale {
                walk_quantity_expr(s, category, cursor);
            }
            walk_target_filter(payer, category, cursor);
        }
        // CR 701.47a: Amass names a creature subtype used as such, plus a count.
        Effect::Amass { subtype, count } => {
            cursor.subtype(category, subtype);
            walk_quantity_expr(count, category, cursor);
        }

        // ================= Genuinely wordless / primary-only / red =================
        // Every effect below either (a) exposes its only word carrier through the
        // `target_filter_mut()` walk above, (b) carries no color/land/creature WORD
        // (fixed counts, markers, ids, name/label strings, mana pips), or (c) holds
        // a deliberately-red carrier: a mass-population `*All` object filter, a
        // perpetual creation-spec, a resolved-spell snapshot (`EpicCopy`), a
        // secondary cast cost, or a specialized non-listed sub-enum
        // (`GuessSubject`, `PerpetualModification`, `IntensityScope`,
        // `ForEachCategoryAction`, `DamageTargetFilter`, etc.). Token-creation
        // and face-down creation specs are NO LONGER in this bucket — CR 612.2a /
        // CR 708.2a route them through `WordCursor::token_spec` /
        // `walk_face_down_profile` above.
        Effect::ApplyPostReplacementDamage { .. }
        | Effect::PairWith { .. }
        | Effect::Destroy { .. }
        | Effect::Regenerate { .. }
        | Effect::RemoveAllDamage { .. }
        | Effect::Counter { .. }
        | Effect::CounterAll { .. }
        | Effect::SetTapState { .. }
        | Effect::DiscardCard { .. }
        | Effect::DestroyAll { .. }
        | Effect::GainControl { .. }
        | Effect::GainControlAll { .. }
        | Effect::ControlNextTurn { .. }
        | Effect::Bounce { .. }
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
        | Effect::EndTheTurn
        | Effect::EndCombatPhase
        | Effect::SwitchPT { .. }
        | Effect::EpicCopy { .. }
        | Effect::Myriad
        | Effect::Encore
        | Effect::CombineHost { .. }
        | Effect::ExileHaunting { .. }
        | Effect::HideawayConceal { .. }
        | Effect::ChooseCard { .. }
        | Effect::ChooseCounterKind { .. }
        | Effect::MultiplyCounter { .. }
        | Effect::DoublePT { .. }
        | Effect::DoublePTAll { .. }
        | Effect::RegisterBending { .. }
        | Effect::Cleanup { .. }
        | Effect::Mana { .. }
        | Effect::Shuffle { .. }
        | Effect::Transform { .. }
        | Effect::Reveal { .. }
        | Effect::RevealTop { .. }
        | Effect::TargetOnly { .. }
        | Effect::Choose { .. }
        | Effect::OpponentGuess { .. }
        | Effect::SwapChosenLabels { .. }
        | Effect::Suspect { .. }
        | Effect::Unsuspect { .. }
        | Effect::PhaseOut { .. }
        | Effect::PhaseIn { .. }
        | Effect::ForceBlock { .. }
        | Effect::SolveCase
        | Effect::BecomePrepared { .. }
        | Effect::BecomeUnprepared { .. }
        | Effect::BecomeSaddled { .. }
        | Effect::SetClassLevel { .. }
        | Effect::AddRestriction { .. }
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
        | Effect::AssembleContraptionsFromRollDifference
        | Effect::CrankContraptions { .. }
        | Effect::ReassembleContraption { .. }
        | Effect::ReassembleContraptionOnSprocket { .. }
        | Effect::ApplySticker { .. }
        | Effect::ProcessRadCounters
        | Effect::GrantCastingPermission { .. }
        | Effect::RememberCard { .. }
        | Effect::ForEachCategory { .. }
        | Effect::LoseAllPlayerCounters { .. }
        | Effect::Exploit { .. }
        | Effect::Heist { .. }
        | Effect::HeistExile
        | Effect::Cascade
        | Effect::Ripple { .. }
        | Effect::MiracleCast { .. }
        | Effect::MadnessCast { .. }
        | Effect::PutOnTopOrBottom { .. }
        | Effect::GiftDelivery { .. }
        | Effect::Goad { .. }
        | Effect::GoadAll { .. }
        | Effect::Detain { .. }
        | Effect::SetRoomDoorLock { .. }
        | Effect::ManifestDread
        | Effect::ExtraTurn { .. }
        | Effect::Double { .. }
        | Effect::RuntimeHandled { .. }
        | Effect::Specialize
        | Effect::Learn
        | Effect::Forage
        | Effect::Harness
        | Effect::CollectEvidence { .. }
        | Effect::BlightEffect { .. }
        | Effect::ExchangeLifeWithStat { .. }
        | Effect::SetDayNight { .. }
        | Effect::RemoveFromCombat { .. }
        | Effect::BecomeBlocked { .. }
        | Effect::ApplyPerpetual { .. }
        | Effect::DraftFromSpellbook { .. }
        | Effect::Unimplemented { .. } => {}
    }
}
