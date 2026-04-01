use crate::database::legality::LegalityFormat;
use crate::database::CardDatabase;
use crate::game::game_object::GameObject;
use crate::game::static_abilities::{build_static_registry, StaticAbilityHandler};
use crate::game::triggers::build_trigger_registry;
use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, ActivationRestriction,
    AdditionalCost, AggregateFunction, ChoiceType, ContinuousModification, ControllerRef,
    CountScope, DelayedTriggerCondition, DoublePTMode, Duration, Effect, FilterProp,
    GainLifePlayer, ManaProduction, ObjectProperty, PlayerFilter, PtValue, QuantityExpr,
    QuantityRef, ReplacementDefinition, ReplacementMode, SharedQuality, StaticCondition,
    StaticDefinition, TargetFilter, TriggerDefinition, TypeFilter, TypedFilter, ZoneRef,
};
use crate::types::card::CardFace;
use crate::types::card_type::CoreType;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;
use crate::types::phase::Phase;
use crate::types::statics::StaticMode;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Data-carrying static mode variants that are supported but can't be registered
/// by exact key in the static registry (because the key includes runtime data).
fn is_data_carrying_static(mode: &StaticMode) -> bool {
    matches!(
        mode,
        StaticMode::ReduceAbilityCost { .. }
            | StaticMode::AdditionalLandDrop { .. }
            | StaticMode::ReduceCost { .. }
            | StaticMode::RaiseCost { .. }
            | StaticMode::DefilerCostReduction { .. }
            | StaticMode::CantCastDuring { .. }
            | StaticMode::PerTurnCastLimit { .. }
            | StaticMode::GraveyardCastPermission { .. }
    )
}

/// A lightweight node in the parse tree for a single card, representing one
/// parsed item (keyword, ability, trigger, static, or replacement) with its
/// support status and any nested children (sub-abilities, modal modes, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedItem {
    /// Category of the parsed item.
    pub category: ParseCategory,
    /// Human-readable label (e.g. "DealDamage", "Flying", "ChangesZone").
    pub label: String,
    /// Original Oracle text fragment that produced this item, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_text: Option<String>,
    /// Whether this specific item is supported by the engine.
    pub supported: bool,
    /// Key-value pairs of parsed parameters (e.g., target, amount, zone).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub details: Vec<(String, String)>,
    /// Nested items (sub-abilities, modal choices, composite costs).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub children: Vec<ParsedItem>,
}

/// The category of a parsed item in the coverage tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParseCategory {
    Keyword,
    Ability,
    Trigger,
    Static,
    Replacement,
    Cost,
}

/// An enriched gap entry with the handler key and the Oracle text that produced it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GapDetail {
    /// Handler key in "Category:label" format (e.g., "Effect:unknown", "Trigger:ChangesZone").
    pub handler: String,
    /// The Oracle text fragment that produced this gap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardCoverageResult {
    pub card_name: String,
    pub set_code: String,
    pub supported: bool,
    /// Enriched gaps with Oracle text fragments — replaces the old `missing_handlers`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gap_details: Vec<GapDetail>,
    /// Number of distinct gaps (`gap_details.len()`), a distance-to-supported metric.
    pub gap_count: usize,
    /// Original Oracle text for the card face.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oracle_text: Option<String>,
    /// Hierarchical parse tree showing what each piece of Oracle text was parsed into.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub parse_details: Vec<ParsedItem>,
}

/// A normalized Oracle text pattern with frequency and example cards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OraclePattern {
    pub pattern: String,
    pub count: usize,
    pub example_cards: Vec<String>,
}

/// A co-occurring gap handler that appears alongside another gap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoOccurrence {
    pub handler: String,
    pub shared_cards: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GapFrequency {
    pub handler: String,
    pub total_count: usize,
    /// How many unsupported cards have this as their ONLY gap (would be unlocked by fixing it).
    pub single_gap_cards: usize,
    /// Breakdown by format: how many single-gap cards are legal in each format.
    pub single_gap_by_format: BTreeMap<String, usize>,
    /// Top normalized Oracle text patterns within this gap, sorted by count.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub oracle_patterns: Vec<OraclePattern>,
    /// Ratio of single-gap cards to total count. `None` when `total_count < 5`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub independence_ratio: Option<f64>,
    /// Top co-occurring gap handlers, sorted by shared card count.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub co_occurrences: Vec<CoOccurrence>,
}

/// A set of gap handlers that, if ALL implemented, would fully unlock cards.
/// Only includes cards whose gap set is EXACTLY this set (not a superset).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GapBundle {
    pub handlers: Vec<String>,
    pub unlocked_cards: usize,
    pub unlocked_by_format: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageSummary {
    pub total_cards: usize,
    pub supported_cards: usize,
    pub coverage_pct: f64,
    pub keyword_count: usize,
    #[serde(default)]
    pub coverage_by_format: BTreeMap<String, FormatCoverageSummary>,
    pub cards: Vec<CardCoverageResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_gaps: Vec<GapFrequency>,
    /// Top 2-gap and 3-gap exact-match bundles that would unlock cards if all handlers implemented.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gap_bundles: Vec<GapBundle>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FormatCoverageSummary {
    pub total_cards: usize,
    pub supported_cards: usize,
    pub coverage_pct: f64,
}

/// Extract the effect variant name (e.g. "DealDamage", "Draw", "Unimplemented")
/// by serializing to JSON and reading the serde `type` tag.
fn effect_type_name(effect: &Effect) -> String {
    serde_json::to_value(effect)
        .ok()
        .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(String::from))
        .unwrap_or_else(|| "Unknown".to_string())
}

// ---------------------------------------------------------------------------
// Detail formatters — extract human-readable parameter summaries
// ---------------------------------------------------------------------------

fn fmt_target(filter: &TargetFilter) -> String {
    match filter {
        TargetFilter::None => "none".into(),
        TargetFilter::Any => "any target".into(),
        TargetFilter::Player => "player".into(),
        TargetFilter::Controller => "controller".into(),
        TargetFilter::SelfRef => "self".into(),
        TargetFilter::StackAbility => "ability on stack".into(),
        TargetFilter::StackSpell => "spell on stack".into(),
        TargetFilter::AttachedTo => "attached permanent".into(),
        TargetFilter::LastCreated => "last created".into(),
        TargetFilter::TriggeringSpellController => "triggering spell's controller".into(),
        TargetFilter::TriggeringSpellOwner => "triggering spell's owner".into(),
        TargetFilter::TriggeringPlayer => "triggering player".into(),
        TargetFilter::TriggeringSource => "triggering source".into(),
        TargetFilter::DefendingPlayer => "defending player".into(),
        TargetFilter::ParentTarget => "parent target".into(),
        TargetFilter::ParentTargetController => "parent target's controller".into(),
        TargetFilter::SpecificObject { id } => format!("object #{}", id.0),
        TargetFilter::TrackedSet { id } => format!("tracked set #{}", id.0),
        TargetFilter::ExiledBySource => "cards exiled by source".into(),
        TargetFilter::Not { filter } => format!("not {}", fmt_target(filter)),
        TargetFilter::Or { filters } => filters
            .iter()
            .map(fmt_target)
            .collect::<Vec<_>>()
            .join(" or "),
        TargetFilter::And { filters } => filters
            .iter()
            .map(fmt_target)
            .collect::<Vec<_>>()
            .join(" + "),
        TargetFilter::Typed(tf) => fmt_typed_filter(tf),
    }
}

fn fmt_typed_filter(tf: &TypedFilter) -> String {
    let mut parts = Vec::new();
    for prop in &tf.properties {
        match prop {
            FilterProp::Token => parts.push("token".into()),
            FilterProp::Attacking => parts.push("attacking".into()),
            FilterProp::Unblocked => parts.push("unblocked".into()),
            FilterProp::Tapped => parts.push("tapped".into()),
            FilterProp::Untapped => parts.push("untapped".into()),
            FilterProp::WithKeyword { value } => parts.push(format!("with {value:?}")),
            FilterProp::WithoutKeyword { value } => parts.push(format!("without {value:?}")),
            FilterProp::CountersGE {
                counter_type,
                count,
            } => parts.push(format!("{count}+ {} counters", counter_type.as_str())),
            FilterProp::CmcGE { value } => parts.push(format!("mv {}+", fmt_quantity(value))),
            FilterProp::CmcLE { value } => parts.push(format!("mv {}-", fmt_quantity(value))),
            FilterProp::CmcEQ { value } => parts.push(format!("mv {}", fmt_quantity(value))),
            FilterProp::SameName => parts.push("same name".into()),
            FilterProp::InZone { zone } => parts.push(format!("in {}", fmt_zone(zone))),
            FilterProp::Owned { controller } => parts.push(fmt_controller(controller)),
            FilterProp::EnchantedBy => parts.push("enchanted by self".into()),
            FilterProp::EquippedBy => parts.push("equipped by self".into()),
            FilterProp::Another => parts.push("another".into()),
            FilterProp::HasColor { color } => parts.push(format!("{color:?}").to_lowercase()),
            FilterProp::PowerLE { value } => parts.push(format!("power ≤{value}")),
            FilterProp::PowerGE { value } => parts.push(format!("power ≥{value}")),
            FilterProp::Multicolored => parts.push("multicolored".into()),
            FilterProp::HasSupertype { value } => {
                parts.push(format!("{value}").to_lowercase());
            }
            FilterProp::IsChosenCreatureType => parts.push("chosen type".into()),
            FilterProp::NotColor { color } => {
                parts.push(format!("non-{}", format!("{color:?}").to_lowercase()));
            }
            FilterProp::NotSupertype { value } => {
                parts.push(format!("non-{}", format!("{value}").to_lowercase()));
            }
            FilterProp::Suspected => parts.push("suspected".into()),
            FilterProp::ToughnessGTPower => parts.push("toughness > power".into()),
            FilterProp::DifferentNameFrom { .. } => parts.push("different name".into()),
            FilterProp::Other { value } => parts.push(value.clone()),
            FilterProp::InAnyZone { zones } => {
                let zone_strs: Vec<_> = zones.iter().map(fmt_zone).collect();
                parts.push(format!("in {}", zone_strs.join("/")));
            }
            FilterProp::SharesQuality { quality } => {
                let name = match quality {
                    SharedQuality::CreatureType => "creature type",
                    SharedQuality::Color => "color",
                    SharedQuality::CardType => "card type",
                };
                parts.push(format!("shares {name}"));
            }
            FilterProp::WasDealtDamageThisTurn => parts.push("dealt damage this turn".into()),
            FilterProp::EnteredThisTurn => parts.push("entered this turn".into()),
            FilterProp::AttackedThisTurn => parts.push("attacked this turn".into()),
            FilterProp::BlockedThisTurn => parts.push("blocked this turn".into()),
            FilterProp::AttackedOrBlockedThisTurn => {
                parts.push("attacked or blocked this turn".into());
            }
            FilterProp::HasSingleTarget => parts.push("single target".into()),
            FilterProp::FaceDown => parts.push("face-down".into()),
            FilterProp::TargetsOnly { filter } => {
                parts.push(format!("targets only {}", fmt_target(filter)));
            }
            FilterProp::Targets { filter } => {
                parts.push(format!("targets {}", fmt_target(filter)));
            }
            FilterProp::Named { name } => parts.push(format!("named \"{name}\"")),
        }
    }
    if let Some(ctrl) = &tf.controller {
        if tf.type_filters.is_empty() {
            // Player-targeting filter (e.g. "target opponent") — label as player, not permanent
            let label = match ctrl {
                ControllerRef::You => "you",
                ControllerRef::Opponent => "opponent",
            };
            parts.push(label.into());
        } else {
            parts.push(fmt_controller(ctrl));
        }
    }
    let type_str = if tf.type_filters.is_empty() {
        String::new()
    } else {
        tf.type_filters
            .iter()
            .map(fmt_type_filter)
            .collect::<Vec<_>>()
            .join(" ")
    };
    if parts.is_empty() {
        if type_str.is_empty() {
            "any".into()
        } else {
            type_str
        }
    } else {
        let props = parts.join(" ");
        if type_str.is_empty() {
            props
        } else {
            format!("{props} {type_str}")
        }
    }
}

fn fmt_type_filter(tf: &TypeFilter) -> String {
    match tf {
        TypeFilter::Creature => "creature",
        TypeFilter::Land => "land",
        TypeFilter::Artifact => "artifact",
        TypeFilter::Enchantment => "enchantment",
        TypeFilter::Instant => "instant",
        TypeFilter::Sorcery => "sorcery",
        TypeFilter::Planeswalker => "planeswalker",
        TypeFilter::Battle => "battle",
        TypeFilter::Permanent => "permanent",
        TypeFilter::Card => "card",
        TypeFilter::Any => "any",
        TypeFilter::Non(inner) => return format!("non-{}", fmt_type_filter(inner)),
        TypeFilter::Subtype(ref s) => return s.clone(),
        TypeFilter::AnyOf(ref filters) => {
            return filters
                .iter()
                .map(fmt_type_filter)
                .collect::<Vec<_>>()
                .join(" or ");
        }
    }
    .into()
}

fn fmt_controller(ctrl: &ControllerRef) -> String {
    match ctrl {
        ControllerRef::You => "you control",
        ControllerRef::Opponent => "opponent controls",
    }
    .into()
}

fn fmt_pt(p: &PtValue) -> String {
    match p {
        PtValue::Fixed(n) => format!("{n:+}"),
        PtValue::Variable(s) => format!("+{s}"),
        PtValue::Quantity(q) => format!("+{}", fmt_quantity(q)),
    }
}

fn fmt_quantity(q: &QuantityExpr) -> String {
    match q {
        QuantityExpr::Fixed { value } => value.to_string(),
        QuantityExpr::Ref { qty } => fmt_quantity_ref(qty),
        QuantityExpr::HalfRounded { inner, rounding } => {
            let dir = match rounding {
                crate::types::ability::RoundingMode::Up => "up",
                crate::types::ability::RoundingMode::Down => "down",
            };
            format!("half({}, rounded {})", fmt_quantity(inner), dir)
        }
        QuantityExpr::Offset { inner, offset } => {
            format!("{}+{}", fmt_quantity(inner), offset)
        }
        QuantityExpr::Multiply { factor, inner } => {
            format!("{}*{}", factor, fmt_quantity(inner))
        }
    }
}

fn fmt_duration(d: &Duration) -> String {
    match d {
        Duration::UntilEndOfTurn => "until end of turn",
        Duration::UntilEndOfCombat => "until end of combat",
        Duration::UntilYourNextTurn => "until your next turn",
        Duration::UntilHostLeavesPlay => "while on battlefield",
        Duration::ForAsLongAs { .. } => "for as long as condition",
        Duration::Permanent => "permanent",
    }
    .into()
}

fn fmt_qty(q: &QuantityExpr) -> String {
    match q {
        QuantityExpr::Fixed { value } => value.to_string(),
        QuantityExpr::Ref { qty } => format!("{qty:?}"),
        other => format!("{other:?}"),
    }
}

fn fmt_zone(z: &Zone) -> String {
    match z {
        Zone::Library => "library",
        Zone::Hand => "hand",
        Zone::Battlefield => "battlefield",
        Zone::Graveyard => "graveyard",
        Zone::Stack => "stack",
        Zone::Exile => "exile",
        Zone::Command => "command zone",
    }
    .into()
}

fn fmt_zone_ref(z: &ZoneRef) -> &'static str {
    match z {
        ZoneRef::Graveyard => "graveyard",
        ZoneRef::Exile => "exile",
        ZoneRef::Library => "library",
    }
}

fn fmt_quantity_ref(qty: &QuantityRef) -> String {
    match qty {
        QuantityRef::HandSize => "cards in hand".into(),
        QuantityRef::LifeTotal => "life total".into(),
        QuantityRef::GraveyardSize => "cards in graveyard".into(),
        QuantityRef::LifeAboveStarting => "life above starting".into(),
        QuantityRef::StartingLifeTotal => "starting life total".into(),
        QuantityRef::Speed => "speed".into(),
        QuantityRef::ObjectCount { filter } => format!("# of {}", fmt_target(filter)),
        QuantityRef::PlayerCount { filter } => format!("# of {}", fmt_player_filter(filter)),
        QuantityRef::CountersOnSelf { counter_type } => {
            format!("{counter_type} counters on self")
        }
        QuantityRef::CountersOnTarget { counter_type } => {
            format!("{counter_type} counters on target")
        }
        QuantityRef::Variable { name } => name.clone(),
        QuantityRef::SelfPower => "self power".into(),
        QuantityRef::SelfToughness => "self toughness".into(),
        QuantityRef::Aggregate {
            function,
            property,
            filter,
        } => {
            let func = match function {
                AggregateFunction::Max => "max",
                AggregateFunction::Min => "min",
                AggregateFunction::Sum => "total",
            };
            let prop = match property {
                ObjectProperty::Power => "power",
                ObjectProperty::Toughness => "toughness",
                ObjectProperty::ManaValue => "mana value",
            };
            format!("{func} {prop} of {}", fmt_target(filter))
        }
        QuantityRef::TargetPower => "target's power".into(),
        QuantityRef::TargetLifeTotal => "target's life total".into(),
        QuantityRef::Devotion { colors } => {
            let c: Vec<_> = colors.iter().map(fmt_mana_color_full).collect();
            format!("devotion to {}", c.join("/"))
        }
        QuantityRef::CardTypesInGraveyards { scope } => {
            format!("card types in {} graveyards", fmt_count_scope(scope))
        }
        QuantityRef::ZoneCardCount {
            zone,
            card_types,
            scope,
        } => {
            let types = if card_types.is_empty() {
                "cards".into()
            } else {
                card_types
                    .iter()
                    .map(fmt_type_filter)
                    .collect::<Vec<_>>()
                    .join("/")
                    + " cards"
            };
            format!(
                "{types} in {} {}",
                fmt_count_scope(scope),
                fmt_zone_ref(zone)
            )
        }
        QuantityRef::BasicLandTypeCount => "basic land types".into(),
        QuantityRef::TrackedSetSize => "cards moved".into(),
        QuantityRef::LifeLostThisTurn => "life lost this turn".into(),
        QuantityRef::EventContextAmount => "event amount".into(),
        QuantityRef::EventContextSourcePower => "source's power".into(),
        QuantityRef::EventContextSourceToughness => "source's toughness".into(),
        QuantityRef::EventContextSourceManaValue => "source's mana value".into(),
        QuantityRef::SpellsCastThisTurn { filter } => match filter {
            Some(filter) => format!("{} spells cast this turn", fmt_target(filter)),
            None => "spells cast this turn".into(),
        },
        QuantityRef::EnteredThisTurn { filter } => {
            format!("{} entered this turn", fmt_target(filter))
        }
        QuantityRef::CrimesCommittedThisTurn => "crimes committed this turn".into(),
        QuantityRef::LifeGainedThisTurn => "life gained this turn".into(),
        QuantityRef::PermanentsLeftBattlefieldThisTurn => {
            "permanents left battlefield this turn".into()
        }
        QuantityRef::TurnsTaken => "turns taken".into(),
        QuantityRef::ChosenNumber => "chosen number".into(),
        QuantityRef::CreaturesDiedThisTurn => "creatures died this turn".into(),
        QuantityRef::AttackedThisTurn => "attacked this turn".into(),
        QuantityRef::DescendedThisTurn => "descended this turn".into(),
        QuantityRef::SpellsCastLastTurn => "spells cast last turn".into(),
        QuantityRef::OpponentLifeLostThisTurn => "opponent life lost this turn".into(),
        QuantityRef::CounterAddedThisTurn => "counter added this turn".into(),
    }
}

fn fmt_player_filter(pf: &PlayerFilter) -> String {
    match pf {
        PlayerFilter::Controller => "you",
        PlayerFilter::Opponent => "each opponent",
        PlayerFilter::OpponentLostLife => "each opponent who lost life this turn",
        PlayerFilter::OpponentGainedLife => "each opponent who gained life this turn",
        PlayerFilter::All => "each player",
        PlayerFilter::HighestSpeed => "each player with the highest speed",
    }
    .into()
}

fn fmt_mana_color_short(c: &ManaColor) -> &'static str {
    match c {
        ManaColor::White => "W",
        ManaColor::Blue => "U",
        ManaColor::Black => "B",
        ManaColor::Red => "R",
        ManaColor::Green => "G",
    }
}

fn fmt_mana_color_full(c: &ManaColor) -> &'static str {
    match c {
        ManaColor::White => "White",
        ManaColor::Blue => "Blue",
        ManaColor::Black => "Black",
        ManaColor::Red => "Red",
        ManaColor::Green => "Green",
    }
}

fn fmt_mana_production(mp: &ManaProduction) -> String {
    match mp {
        ManaProduction::Fixed { colors } => {
            if colors.is_empty() {
                "none".into()
            } else {
                colors
                    .iter()
                    .map(|c| format!("{{{}}}", fmt_mana_color_short(c)))
                    .collect()
            }
        }
        ManaProduction::Colorless { count } => format!("{{C}} x{}", fmt_quantity(count)),
        ManaProduction::AnyOneColor {
            count,
            color_options,
        } => {
            let opts: String = color_options
                .iter()
                .map(|c| format!("{{{}}}", fmt_mana_color_short(c)))
                .collect();
            format!("{} of {opts}", fmt_quantity(count))
        }
        ManaProduction::AnyCombination {
            count,
            color_options,
        } => {
            let opts: String = color_options
                .iter()
                .map(|c| format!("{{{}}}", fmt_mana_color_short(c)))
                .collect();
            format!("{} any combo of {opts}", fmt_quantity(count))
        }
        ManaProduction::ChosenColor { count } => {
            format!("{} of chosen color", fmt_quantity(count))
        }
    }
}

fn fmt_choice_type(ct: &ChoiceType) -> String {
    match ct {
        ChoiceType::CreatureType => "creature type",
        ChoiceType::Color => "color",
        ChoiceType::OddOrEven => "odd or even",
        ChoiceType::BasicLandType => "basic land type",
        ChoiceType::CardType => "card type",
        ChoiceType::CardName => "card name",
        ChoiceType::NumberRange { min, max } => return format!("number ({min}-{max})"),
        ChoiceType::Labeled { options } => return format!("one of: {}", options.join(", ")),
        ChoiceType::LandType => "land type",
        ChoiceType::Opponent => "opponent",
        ChoiceType::Player => "player",
        ChoiceType::TwoColors => "two colors",
    }
    .into()
}

fn fmt_delayed_condition(cond: &DelayedTriggerCondition) -> String {
    match cond {
        DelayedTriggerCondition::AtNextPhase { phase } => {
            format!("at next {}", fmt_phase(phase))
        }
        DelayedTriggerCondition::AtNextPhaseForPlayer { phase, .. } => {
            format!("at your next {}", fmt_phase(phase))
        }
        DelayedTriggerCondition::WhenLeavesPlay { .. } => "when leaves play".into(),
        DelayedTriggerCondition::WhenDies { .. } => "when dies".into(),
        DelayedTriggerCondition::WhenLeavesPlayFiltered { filter } => {
            format!("when {} leaves play", fmt_target(filter))
        }
        DelayedTriggerCondition::WhenEntersBattlefield { filter } => {
            format!("when {} enters", fmt_target(filter))
        }
        DelayedTriggerCondition::WhenDiesOrExiled { .. } => "when dies or exiled".into(),
        DelayedTriggerCondition::WheneverEvent { .. } => "whenever event this turn".into(),
    }
}

fn fmt_phase(p: &Phase) -> &'static str {
    match p {
        Phase::Untap => "untap",
        Phase::Upkeep => "upkeep",
        Phase::Draw => "draw",
        Phase::PreCombatMain => "precombat main",
        Phase::BeginCombat => "begin combat",
        Phase::DeclareAttackers => "declare attackers",
        Phase::DeclareBlockers => "declare blockers",
        Phase::CombatDamage => "combat damage",
        Phase::EndCombat => "end combat",
        Phase::PostCombatMain => "postcombat main",
        Phase::End => "end step",
        Phase::Cleanup => "cleanup",
    }
}

fn fmt_double_pt_mode(mode: &DoublePTMode) -> &'static str {
    match mode {
        DoublePTMode::Power => "power",
        DoublePTMode::Toughness => "toughness",
        DoublePTMode::PowerAndToughness => "power and toughness",
    }
}

fn fmt_ability_kind(kind: &AbilityKind) -> &'static str {
    match kind {
        AbilityKind::Spell => "spell",
        AbilityKind::Activated => "activated",
        AbilityKind::Database => "database",
        AbilityKind::BeginGame => "begin game",
    }
}

fn fmt_core_type(ct: &CoreType) -> &'static str {
    match ct {
        CoreType::Artifact => "artifact",
        CoreType::Creature => "creature",
        CoreType::Enchantment => "enchantment",
        CoreType::Instant => "instant",
        CoreType::Land => "land",
        CoreType::Planeswalker => "planeswalker",
        CoreType::Sorcery => "sorcery",
        CoreType::Tribal => "tribal",
        CoreType::Battle => "battle",
        CoreType::Kindred => "kindred",
        CoreType::Dungeon => "dungeon",
    }
}

fn fmt_count_scope(scope: &CountScope) -> &'static str {
    match scope {
        CountScope::Controller => "your",
        CountScope::All => "all",
        CountScope::Opponents => "opponents'",
    }
}

/// Extract key-value detail pairs from an `Effect`'s parameters.
fn effect_details(effect: &Effect) -> Vec<(String, String)> {
    let mut d = Vec::new();
    match effect {
        Effect::StartYourEngines { player_scope } => {
            d.push(("players".into(), fmt_player_filter(player_scope)));
        }
        Effect::IncreaseSpeed {
            player_scope,
            amount,
        } => {
            d.push(("players".into(), fmt_player_filter(player_scope)));
            d.push(("amount".into(), fmt_quantity(amount)));
        }
        Effect::DealDamage { amount, target, .. } => {
            d.push(("amount".into(), fmt_quantity(amount)));
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::Draw { count } => {
            if !matches!(count, QuantityExpr::Fixed { value: 1 }) {
                d.push(("count".into(), fmt_quantity(count)));
            }
        }
        Effect::ExileTop { player, count } => {
            d.push(("player".into(), fmt_target(player)));
            d.push(("count".into(), fmt_quantity(count)));
        }
        Effect::Pump {
            power,
            toughness,
            target,
        } => {
            d.push((
                "p/t".into(),
                format!("{}/{}", fmt_pt(power), fmt_pt(toughness)),
            ));
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::PumpAll {
            power,
            toughness,
            target,
        } => {
            d.push((
                "p/t".into(),
                format!("{}/{}", fmt_pt(power), fmt_pt(toughness)),
            ));
            if !matches!(target, TargetFilter::None) {
                d.push(("filter".into(), fmt_target(target)));
            }
        }
        Effect::Destroy { target, .. }
        | Effect::Tap { target }
        | Effect::Untap { target }
        | Effect::Sacrifice { target }
        | Effect::GainControl { target }
        | Effect::Attach { target }
        | Effect::Fight { target, .. }
        | Effect::CopySpell { target }
        | Effect::BecomeCopy { target, .. }
        | Effect::Suspect { target }
        | Effect::Connive { target, .. }
        | Effect::PhaseOut { target }
        | Effect::ForceBlock { target }
        | Effect::Transform { target }
        | Effect::Shuffle { target }
        | Effect::Regenerate { target } => {
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::DestroyAll { target, .. } | Effect::DamageAll { amount: _, target } => {
            if !matches!(target, TargetFilter::None) {
                d.push(("filter".into(), fmt_target(target)));
            }
            if let Effect::DamageAll { amount, .. } = effect {
                d.push(("amount".into(), fmt_quantity(amount)));
            }
        }
        Effect::DamageEachPlayer {
            amount,
            player_filter,
        } => {
            d.push(("amount".into(), fmt_quantity(amount)));
            d.push(("players".into(), fmt_player_filter(player_filter)));
        }
        Effect::Counter {
            target,
            source_static,
            ..
        } => {
            d.push(("target".into(), fmt_target(target)));
            if source_static.is_some() {
                d.push(("+ static".into(), "on source".into()));
            }
        }
        Effect::Token {
            name,
            power,
            toughness,
            types,
            colors,
            keywords,
            count,
            tapped,
            attach_to,
            ..
        } => {
            let mut desc = String::new();
            match count {
                QuantityExpr::Fixed { value: n } if *n != 1 => {
                    desc.push_str(&format!("{n}× "));
                }
                QuantityExpr::Ref { qty } => {
                    desc.push_str(&format!("{}× ", fmt_quantity_ref(qty)));
                }
                _ => {}
            }
            desc.push_str(&format!("{}/{} ", fmt_pt(power), fmt_pt(toughness)));
            if !colors.is_empty() {
                let c: Vec<_> = colors
                    .iter()
                    .map(|c| fmt_mana_color_full(c).to_string())
                    .collect();
                desc.push_str(&c.join("/"));
                desc.push(' ');
            }
            desc.push_str(name);
            if !types.is_empty() {
                desc.push_str(&format!(" ({})", types.join(" ")));
            }
            if !keywords.is_empty() {
                let kws: Vec<_> = keywords.iter().map(keyword_label).collect();
                desc.push_str(&format!(" with {}", kws.join(", ")));
            }
            if *tapped {
                desc.push_str(" tapped");
            }
            if attach_to.is_some() {
                desc.push_str(" attached");
            }
            d.push(("token".into(), desc));
        }
        Effect::AddCounter {
            counter_type,
            count,
            target,
        }
        | Effect::PutCounter {
            counter_type,
            count,
            target,
        }
        | Effect::PutCounterAll {
            counter_type,
            count,
            target,
        } => {
            d.push((
                "counter".into(),
                format!("{} {counter_type}", fmt_qty(count)),
            ));
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::RemoveCounter {
            counter_type,
            count,
            target,
        } => {
            d.push(("counter".into(), format!("{count} {counter_type}")));
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::MultiplyCounter {
            counter_type,
            multiplier,
            target,
        } => {
            d.push(("counter".into(), format!("{counter_type} ×{multiplier}")));
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::DoublePT { mode, target } => {
            d.push(("mode".into(), fmt_double_pt_mode(mode).into()));
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::DoublePTAll { mode, target } => {
            d.push(("mode".into(), fmt_double_pt_mode(mode).into()));
            d.push(("filter".into(), fmt_target(target)));
        }
        Effect::DiscardCard { count, target } => {
            if *count != 1 {
                d.push(("count".into(), count.to_string()));
            }
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::Discard { count, target, .. } => {
            d.push(("count".into(), fmt_quantity(count)));
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::Mill {
            count,
            target,
            destination,
        } => {
            d.push(("count".into(), fmt_quantity(count)));
            d.push(("target".into(), fmt_target(target)));
            if *destination != Zone::Graveyard {
                d.push(("destination".into(), format!("{destination:?}")));
            }
        }
        Effect::Scry { count } | Effect::Surveil { count } => {
            d.push(("count".into(), fmt_quantity(count)));
        }
        Effect::GainLife { amount, player } => {
            d.push(("amount".into(), fmt_quantity(amount)));
            if !matches!(player, GainLifePlayer::Controller) {
                d.push((
                    "player".into(),
                    match player {
                        GainLifePlayer::TargetedController => "target's controller",
                        GainLifePlayer::Controller => unreachable!(),
                    }
                    .into(),
                ));
            }
        }
        Effect::LoseLife { amount } => {
            d.push(("amount".into(), fmt_quantity(amount)));
        }
        Effect::ChangeZone {
            origin,
            destination,
            target,
            ..
        }
        | Effect::ChangeZoneAll {
            origin,
            destination,
            target,
        } => {
            if let Some(o) = origin {
                d.push(("from".into(), fmt_zone(o)));
            }
            d.push(("to".into(), fmt_zone(destination)));
            if !matches!(target, TargetFilter::None) {
                d.push(("target".into(), fmt_target(target)));
            }
        }
        Effect::Dig {
            count,
            destination,
            keep_count,
            up_to,
            filter,
            rest_destination,
        } => {
            d.push(("count".into(), fmt_qty(count)));
            if let Some(dest) = destination {
                d.push(("to".into(), fmt_zone(dest)));
            }
            if let Some(kc) = keep_count {
                d.push(("keep_count".into(), kc.to_string()));
            }
            if *up_to {
                d.push(("up_to".into(), "true".into()));
            }
            if !matches!(filter, TargetFilter::Any) {
                d.push(("filter".into(), fmt_target(filter)));
            }
            if let Some(rest) = rest_destination {
                d.push(("rest_to".into(), fmt_zone(rest)));
            }
        }
        Effect::Bounce {
            target,
            destination,
        } => {
            d.push(("target".into(), fmt_target(target)));
            if let Some(dest) = destination {
                d.push(("to".into(), fmt_zone(dest)));
            }
        }
        Effect::SearchLibrary {
            filter,
            count,
            reveal,
        } => {
            d.push(("find".into(), fmt_target(filter)));
            if *count != 1 {
                d.push(("count".into(), count.to_string()));
            }
            if *reveal {
                d.push(("reveal".into(), "yes".into()));
            }
        }
        Effect::Animate {
            power,
            toughness,
            types,
            target,
            ..
        } => {
            if let (Some(p), Some(t)) = (power, toughness) {
                d.push(("p/t".into(), format!("{p}/{t}")));
            }
            if !types.is_empty() {
                d.push(("types".into(), types.join(" ")));
            }
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::Choose {
            choice_type,
            persist,
        } => {
            d.push(("choice".into(), fmt_choice_type(choice_type)));
            if *persist {
                d.push(("persist".into(), "yes".into()));
            }
        }
        Effect::Mana { produced, .. } => {
            d.push(("mana".into(), fmt_mana_production(produced)));
        }
        Effect::RevealHand {
            target,
            card_filter,
            count,
        } => {
            d.push(("player".into(), fmt_target(target)));
            if !matches!(card_filter, TargetFilter::Any) {
                d.push(("card filter".into(), fmt_target(card_filter)));
            }
            if let Some(c) = count {
                d.push(("count".into(), fmt_quantity(c)));
            }
        }
        Effect::RevealTop { player, count } => {
            d.push(("player".into(), fmt_target(player)));
            d.push(("count".into(), count.to_string()));
        }
        Effect::TargetOnly { target } => {
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::ChooseCard { choices, target } => {
            if !choices.is_empty() {
                d.push(("choices".into(), choices.join(", ")));
            }
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::CreateDelayedTrigger {
            condition,
            uses_tracked_set,
            ..
        } => {
            d.push(("when".into(), fmt_delayed_condition(condition)));
            if *uses_tracked_set {
                d.push(("tracked".into(), "yes".into()));
            }
        }
        Effect::GenericEffect {
            static_abilities,
            duration,
            target,
        } => {
            if let Some(dur) = duration {
                d.push(("duration".into(), fmt_duration(dur)));
            }
            if let Some(t) = target {
                d.push(("target".into(), fmt_target(t)));
            }
            for stat in static_abilities {
                for modification in &stat.modifications {
                    d.push(("grants".into(), fmt_modification(modification)));
                }
                if let Some(affected) = &stat.affected {
                    if !matches!(affected, TargetFilter::None) {
                        d.push(("affects".into(), fmt_target(affected)));
                    }
                }
            }
        }
        Effect::SetClassLevel { level } => {
            d.push(("level".to_string(), level.to_string()));
        }
        Effect::CastFromZone {
            target,
            without_paying_mana_cost,
            ..
        } => {
            d.push(("target".into(), fmt_target(target)));
            if *without_paying_mana_cost {
                d.push(("free cast".into(), "yes".into()));
            }
        }
        Effect::RollDie { sides, results } => {
            d.push(("sides".into(), sides.to_string()));
            if !results.is_empty() {
                d.push(("branches".into(), results.len().to_string()));
            }
        }
        Effect::FlipCoin {
            win_effect,
            lose_effect,
        } => {
            if win_effect.is_some() {
                d.push(("win".into(), "yes".into()));
            }
            if lose_effect.is_some() {
                d.push(("lose".into(), "yes".into()));
            }
        }
        Effect::FlipCoinUntilLose { .. } => {
            d.push(("mode".into(), "until lose".into()));
        }
        Effect::MoveCounters {
            source,
            counter_type,
            target,
        } => {
            d.push(("source".into(), fmt_target(source)));
            if let Some(ct) = counter_type {
                d.push(("counter".into(), ct.clone()));
            } else {
                d.push(("counter".into(), "all".into()));
            }
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::Exploit { target } => {
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::PreventDamage {
            amount,
            target,
            scope,
            ..
        } => {
            d.push(("amount".into(), format!("{amount:?}")));
            d.push(("target".into(), fmt_target(target)));
            d.push(("scope".into(), format!("{scope:?}")));
        }
        Effect::ChooseFromZone { count, zone, .. } => {
            d.push(("count".into(), count.to_string()));
            d.push(("zone".into(), fmt_zone(zone)));
        }
        Effect::GainEnergy { amount } => {
            d.push(("amount".into(), amount.to_string()));
        }
        Effect::GivePlayerCounter {
            counter_kind,
            count,
            target,
        } => {
            d.push(("counter".into(), format!("{counter_kind:?}")));
            d.push(("count".into(), fmt_quantity(count)));
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::ExileFromTopUntil { filter } => {
            d.push(("until".into(), fmt_target(filter)));
        }
        Effect::Discover { mana_value_limit } => {
            d.push(("mv limit".into(), mana_value_limit.to_string()));
        }
        Effect::PutAtLibraryPosition { target, position } => {
            d.push(("target".into(), fmt_target(target)));
            d.push(("position".into(), format!("{position:?}")));
        }
        Effect::PutOnTopOrBottom { target } => {
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::Amass { subtype, count } => {
            d.push(("subtype".into(), subtype.clone()));
            d.push(("count".into(), fmt_quantity(count)));
        }
        Effect::Monstrosity { count } => {
            d.push(("counters".into(), fmt_quantity(count)));
        }
        Effect::Adapt { count } => {
            d.push(("counters".into(), fmt_quantity(count)));
        }
        Effect::Bolster { count } => {
            d.push(("counters".into(), fmt_quantity(count)));
        }
        Effect::Goad { target } => {
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::ExtraTurn { target } => {
            d.push(("player".into(), fmt_target(target)));
        }
        Effect::AdditionalCombatPhase {
            with_main_phase, ..
        } => {
            if *with_main_phase {
                d.push(("+ main phase".into(), "yes".into()));
            }
        }
        Effect::Double {
            target_kind,
            target,
        } => {
            d.push(("doubles".into(), format!("{target_kind:?}")));
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::CollectEvidence { amount } => {
            d.push(("amount".into(), amount.to_string()));
        }
        Effect::Endure { amount } => {
            d.push(("amount".into(), amount.to_string()));
        }
        Effect::BlightEffect { count, target } => {
            d.push(("count".into(), count.to_string()));
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::Seek {
            filter,
            count,
            destination,
            ..
        } => {
            d.push(("filter".into(), fmt_target(filter)));
            d.push(("count".into(), fmt_quantity(count)));
            if *destination != Zone::Hand {
                d.push(("to".into(), fmt_zone(destination)));
            }
        }
        Effect::SetLifeTotal { target, amount } => {
            d.push(("target".into(), fmt_target(target)));
            d.push(("amount".into(), fmt_quantity(amount)));
        }
        Effect::GiveControl {
            target, recipient, ..
        } => {
            d.push(("target".into(), fmt_target(target)));
            d.push(("to".into(), fmt_target(recipient)));
        }
        Effect::CopyTokenOf {
            target,
            enters_attacking,
            ..
        } => {
            d.push(("copies".into(), fmt_target(target)));
            if *enters_attacking {
                d.push(("attacking".into(), "yes".into()));
            }
        }
        Effect::ExploreAll { filter } => {
            d.push(("filter".into(), fmt_target(filter)));
        }
        Effect::GiftDelivery { kind } => {
            d.push(("gift".into(), format!("{kind:?}")));
        }
        Effect::SetDayNight { to } => {
            d.push(("to".into(), format!("{to:?}")));
        }
        // Effects with no interesting parameters
        Effect::Unimplemented { .. }
        | Effect::Explore
        | Effect::Investigate
        | Effect::BecomeMonarch
        | Effect::Proliferate
        | Effect::SolveCase
        | Effect::Cleanup { .. }
        | Effect::AddRestriction { .. }
        | Effect::CreateEmblem { .. }
        | Effect::PayCost { .. }
        | Effect::LoseTheGame
        | Effect::WinTheGame
        | Effect::RingTemptsYou
        | Effect::GrantCastingPermission { .. }
        | Effect::ManifestDread
        | Effect::RuntimeHandled { .. }
        | Effect::ChangeTargets { .. }
        | Effect::ExchangeControl
        | Effect::Forage
        | Effect::Learn => {}
    }
    d
}

/// Extract detail pairs from an `AbilityDefinition` (non-effect fields).
fn ability_details(def: &AbilityDefinition) -> Vec<(String, String)> {
    let mut d = Vec::new();
    if def.kind != AbilityKind::Spell {
        d.push(("kind".into(), fmt_ability_kind(&def.kind).into()));
    }
    if let Some(dur) = &def.duration {
        d.push(("duration".into(), fmt_duration(dur)));
    }
    if def.optional_targeting {
        d.push(("targeting".into(), "optional (up to)".into()));
    }
    if let Some(mt) = &def.multi_target {
        d.push((
            "targets".into(),
            match mt.max {
                Some(max) => format!("{}-{}", mt.min, max),
                None => format!("{}+", mt.min),
            },
        ));
    }
    if def.condition.is_some() {
        d.push(("conditional".into(), "yes".into()));
    }
    if def.sorcery_speed {
        d.push(("timing".into(), "sorcery speed".into()));
    }
    if let Some(modal) = &def.modal {
        d.push((
            "modal".into(),
            format!(
                "choose {}-{} of {}",
                modal.min_choices, modal.max_choices, modal.mode_count
            ),
        ));
    }
    d
}

/// Extract detail pairs from a `TriggerDefinition` (non-effect fields).
fn trigger_details(trig: &TriggerDefinition) -> Vec<(String, String)> {
    let mut d = Vec::new();
    if let Some(vc) = &trig.valid_card {
        d.push(("watches".into(), fmt_target(vc)));
    }
    if let Some(origin) = &trig.origin {
        d.push(("from".into(), fmt_zone(origin)));
    }
    if let Some(dest) = &trig.destination {
        d.push(("to".into(), fmt_zone(dest)));
    }
    if !trig.trigger_zones.is_empty() {
        let zones: Vec<_> = trig.trigger_zones.iter().map(fmt_zone).collect();
        d.push(("active in".into(), zones.join(", ")));
    }
    if let Some(phase) = &trig.phase {
        d.push(("phase".into(), fmt_phase(phase).into()));
    }
    if trig.optional {
        d.push(("optional".into(), "yes".into()));
    }
    match trig.damage_kind {
        crate::types::ability::DamageKindFilter::Any => {}
        crate::types::ability::DamageKindFilter::CombatOnly => {
            d.push(("damage kind".into(), "combat only".into()));
        }
        crate::types::ability::DamageKindFilter::NoncombatOnly => {
            d.push(("damage kind".into(), "noncombat only".into()));
        }
    }
    if let Some(vt) = &trig.valid_target {
        d.push(("valid target".into(), fmt_target(vt)));
    }
    if let Some(vs) = &trig.valid_source {
        d.push(("valid source".into(), fmt_target(vs)));
    }
    if trig.constraint.is_some() {
        d.push(("constraint".into(), "yes".into()));
    }
    if trig.condition.is_some() {
        d.push(("condition".into(), "yes".into()));
    }
    d
}

/// Format a single `ContinuousModification` as a human-readable string.
fn fmt_modification(m: &crate::types::ability::ContinuousModification) -> String {
    use crate::types::ability::ContinuousModification;
    match m {
        ContinuousModification::AddPower { value } => format!("power {:+}", value),
        ContinuousModification::AddToughness { value } => format!("toughness {:+}", value),
        ContinuousModification::SetPower { value } => format!("base power {value}"),
        ContinuousModification::SetToughness { value } => format!("base toughness {value}"),
        ContinuousModification::AddKeyword { keyword } => {
            format!("grant {}", keyword_label(keyword))
        }
        ContinuousModification::RemoveKeyword { keyword } => {
            format!("remove {}", keyword_label(keyword))
        }
        ContinuousModification::GrantAbility { .. } => "grant ability".into(),
        ContinuousModification::GrantTrigger { .. } => "grant trigger".into(),
        ContinuousModification::RemoveAllAbilities => "remove all abilities".into(),
        ContinuousModification::AddType { core_type } => {
            format!("add type {}", fmt_core_type(core_type))
        }
        ContinuousModification::RemoveType { core_type } => {
            format!("remove type {}", fmt_core_type(core_type))
        }
        ContinuousModification::AddSubtype { subtype } => format!("add subtype {subtype}"),
        ContinuousModification::RemoveSubtype { subtype } => {
            format!("remove subtype {subtype}")
        }
        ContinuousModification::SetDynamicPower { .. } => "dynamic power".into(),
        ContinuousModification::SetDynamicToughness { .. } => "dynamic toughness".into(),
        ContinuousModification::AddDynamicPower { .. } => "add dynamic power".into(),
        ContinuousModification::AddDynamicToughness { .. } => "add dynamic toughness".into(),
        ContinuousModification::AddAllCreatureTypes => "all creature types".into(),
        ContinuousModification::AddAllBasicLandTypes => "all basic land types".into(),
        ContinuousModification::AddChosenSubtype { .. } => "add chosen subtype".into(),
        ContinuousModification::AddChosenColor => "add chosen color".into(),
        ContinuousModification::SetColor { colors } => {
            let c: Vec<_> = colors
                .iter()
                .map(|c| fmt_mana_color_full(c).to_string())
                .collect();
            format!("set color {}", c.join("/"))
        }
        ContinuousModification::AddColor { color } => {
            format!("add color {}", fmt_mana_color_full(color))
        }
        ContinuousModification::AddStaticMode { mode } => format!("{mode}"),
        ContinuousModification::AssignDamageFromToughness => "damage from toughness".into(),
        ContinuousModification::ChangeController => "change controller".into(),
        ContinuousModification::SetBasicLandType { land_type } => {
            format!("set land type {}", land_type.as_subtype_str())
        }
    }
}

/// Derive a descriptive label for a `GenericEffect` from its static abilities.
///
/// Instead of showing "GenericEffect", surfaces the actual mechanics being granted
/// (e.g. "MustBeBlocked", "grant Flying + Haste", "power +2, toughness +2").
fn generic_effect_label(statics: &[StaticDefinition]) -> String {
    let mod_labels: Vec<String> = statics
        .iter()
        .flat_map(|s| s.modifications.iter().map(fmt_modification))
        .collect();

    if mod_labels.is_empty() {
        // Fall back to static modes if no modifications
        let modes: Vec<String> = statics.iter().map(|s| format!("{}", s.mode)).collect();
        if modes.is_empty() {
            return "GenericEffect".into();
        }
        return modes.join(" + ");
    }

    mod_labels.join(", ")
}

/// Extract detail pairs from a `StaticDefinition`.
fn static_details(stat: &StaticDefinition) -> Vec<(String, String)> {
    let mut d = Vec::new();
    if let Some(affected) = &stat.affected {
        d.push(("affects".into(), fmt_target(affected)));
    }
    if !stat.modifications.is_empty() {
        d.push(("modifications".into(), stat.modifications.len().to_string()));
    }
    if stat.condition.is_some() {
        d.push(("conditional".into(), "yes".into()));
    }
    if stat.characteristic_defining {
        d.push(("CDA".into(), "yes".into()));
    }
    if let Some(zone) = &stat.affected_zone {
        d.push(("zone".into(), fmt_zone(zone)));
    }
    d
}

/// Extract a human-readable label for a keyword.
fn keyword_label(kw: &Keyword) -> String {
    serde_json::to_value(kw)
        .ok()
        .and_then(|v| match &v {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Object(map) => map.keys().next().cloned(),
            _ => None,
        })
        .unwrap_or_else(|| format!("{kw:?}"))
}

/// Build a hierarchical parse tree from a `CardFace`, checking each item against
/// the engine's trigger and static registries for support status.
pub fn build_parse_details(
    face: &CardFace,
    trigger_registry: &HashMap<TriggerMode, crate::game::triggers::TriggerMatcher>,
    static_registry: &HashMap<StaticMode, StaticAbilityHandler>,
) -> Vec<ParsedItem> {
    let mut items = Vec::new();

    // Keywords
    for kw in &face.keywords {
        items.push(ParsedItem {
            category: ParseCategory::Keyword,
            label: keyword_label(kw),
            source_text: None,
            supported: !matches!(kw, Keyword::Unknown(_)),
            details: vec![],
            children: vec![],
        });
    }

    // Activated/spell abilities
    for def in &face.abilities {
        items.push(build_ability_item(def));
    }

    // Triggers
    for trig in &face.triggers {
        let mode_supported = !matches!(&trig.mode, TriggerMode::Unknown(_))
            && trigger_registry.contains_key(&trig.mode);
        let mut children = Vec::new();
        if let Some(execute) = &trig.execute {
            children.push(build_ability_item(execute));
        }
        items.push(ParsedItem {
            category: ParseCategory::Trigger,
            label: format!("{}", trig.mode),
            source_text: trig.description.clone(),
            supported: mode_supported && children.iter().all(|c| c.is_fully_supported()),
            details: trigger_details(trig),
            children,
        });
    }

    // Static abilities
    for stat in &face.static_abilities {
        items.push(ParsedItem {
            category: ParseCategory::Static,
            label: format!("{}", stat.mode),
            source_text: stat.description.clone(),
            supported: static_registry.contains_key(&stat.mode)
                || is_data_carrying_static(&stat.mode),
            details: static_details(stat),
            children: vec![],
        });
    }

    // Replacement effects
    for repl in &face.replacements {
        let mut children = Vec::new();
        let mut execute_supported = true;
        if let Some(execute) = &repl.execute {
            let item = build_ability_item(execute);
            execute_supported = item.is_fully_supported();
            children.push(item);
        }
        if let ReplacementMode::Optional {
            decline: Some(decline),
        } = &repl.mode
        {
            let item = build_ability_item(decline);
            if !item.is_fully_supported() {
                execute_supported = false;
            }
            children.push(item);
        }
        items.push(ParsedItem {
            category: ParseCategory::Replacement,
            label: format!("{}", repl.event),
            source_text: repl.description.clone(),
            supported: execute_supported,
            details: vec![],
            children,
        });
    }

    // Additional cost
    if let Some(additional_cost) = &face.additional_cost {
        build_additional_cost_items(additional_cost, &mut items);
    }

    items
}

/// Build a `ParsedItem` for a single `AbilityDefinition`, recursing into
/// sub-abilities and modal abilities.
fn build_ability_item(def: &AbilityDefinition) -> ParsedItem {
    let label = match &*def.effect {
        Effect::Unimplemented { name, .. } => name.clone(),
        Effect::GenericEffect {
            static_abilities, ..
        } => {
            let derived = generic_effect_label(static_abilities);
            if derived == "GenericEffect" && def.modal.is_some() {
                "Modal".into()
            } else {
                derived
            }
        }
        _ => effect_type_name(&def.effect),
    };
    let supported = !matches!(&*def.effect, Effect::Unimplemented { .. });
    let source_text = def.description.clone().or_else(|| match &*def.effect {
        Effect::Unimplemented { description, .. } => description.clone(),
        _ => None,
    });

    let mut details = effect_details(&def.effect);
    let ability_dets = ability_details(def);
    // Avoid duplicate keys (e.g. GenericEffect already emits "duration")
    for pair in ability_dets {
        if !details.iter().any(|(k, _)| k == &pair.0) {
            details.push(pair);
        }
    }

    let mut children = Vec::new();

    // Cost
    if let Some(cost) = &def.cost {
        build_cost_item(cost, &mut children);
    }

    // Sub-ability chain
    if let Some(sub) = &def.sub_ability {
        children.push(build_ability_item(sub));
    }

    // Modal abilities
    for mode_ability in &def.mode_abilities {
        children.push(build_ability_item(mode_ability));
    }

    ParsedItem {
        category: ParseCategory::Ability,
        label,
        source_text,
        supported,
        details,
        children,
    }
}

/// Build `ParsedItem` nodes for ability costs, only emitting items for
/// composite or unimplemented costs (simple costs are not interesting).
fn build_cost_item(cost: &AbilityCost, items: &mut Vec<ParsedItem>) {
    match cost {
        AbilityCost::Composite { costs } => {
            for nested in costs {
                build_cost_item(nested, items);
            }
        }
        AbilityCost::Unimplemented { description } => {
            items.push(ParsedItem {
                category: ParseCategory::Cost,
                label: description.clone(),
                source_text: Some(description.clone()),
                supported: false,
                details: vec![],
                children: vec![],
            });
        }
        _ => {}
    }
}

/// Build `ParsedItem` nodes for additional costs (kicker, etc.).
fn build_additional_cost_items(additional_cost: &AdditionalCost, items: &mut Vec<ParsedItem>) {
    match additional_cost {
        AdditionalCost::Optional(cost) | AdditionalCost::Required(cost) => {
            build_cost_item(cost, items);
        }
        AdditionalCost::Choice(first, second) => {
            build_cost_item(first, items);
            build_cost_item(second, items);
        }
    }
}

/// Normalize Oracle text into a canonical pattern for clustering.
///
/// Replaces concrete numbers, mana symbols, and p/t modifiers with placeholders
/// so that structurally identical Oracle phrases group together.
fn normalize_oracle_pattern(text: &str) -> String {
    let s = text.to_lowercase();
    let s = s.trim_end_matches('.');
    let mut result = String::with_capacity(s.len());
    let mut chars = s.char_indices().peekable();

    while let Some(&(i, ch)) = chars.peek() {
        // Handle {X} mana symbols — content inside braces is always ASCII
        if ch == '{' {
            if let Some(close_offset) = s[i..].find('}') {
                let inner = &s[i + 1..i + close_offset];
                let replacement = match inner.as_bytes() {
                    [c] if b"wubrgcsx".contains(c) => Some("{M}"),
                    _ if !inner.is_empty() && inner.bytes().all(|b| b.is_ascii_digit()) => {
                        Some("{N}")
                    }
                    [left, b'/', right]
                        if b"wubrgc".contains(left) && b"wubrgcp".contains(right) =>
                    {
                        Some(if *right == b'p' { "{M/P}" } else { "{M/M}" })
                    }
                    _ => None,
                };
                if let Some(rep) = replacement {
                    result.push_str(rep);
                    // Advance past the closing brace
                    let end = i + close_offset + 1;
                    while chars.peek().is_some_and(|&(pos, _)| pos < end) {
                        chars.next();
                    }
                    continue;
                }
            }
            result.push('{');
            chars.next();
            continue;
        }

        // Handle +N/+N or -N/-N p/t patterns (must check before digit replacement)
        if matches!(ch, '+' | '-') {
            let rest = &s[i..];
            if let Some(pt_len) = match_pt_pattern(rest) {
                result.push_str("+N/+N");
                let end = i + pt_len;
                while chars.peek().is_some_and(|&(pos, _)| pos < end) {
                    chars.next();
                }
                continue;
            }
        }

        // Replace digit sequences with N
        if ch.is_ascii_digit() {
            result.push('N');
            chars.next();
            while chars.peek().is_some_and(|&(_, c)| c.is_ascii_digit()) {
                chars.next();
            }
            continue;
        }

        // Collapse whitespace
        if ch.is_whitespace() {
            result.push(' ');
            chars.next();
            while chars.peek().is_some_and(|&(_, c)| c.is_whitespace()) {
                chars.next();
            }
            continue;
        }

        result.push(ch);
        chars.next();
    }

    result.trim().to_string()
}

/// Match a p/t pattern like `+3/+1` or `-2/-2` at the start of `s`.
/// Returns the byte length consumed, or `None` if no match.
fn match_pt_pattern(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.len() < 5 || !matches!(b[0], b'+' | b'-') {
        return None;
    }
    let mut i = 1;
    if i >= b.len() || !b[i].is_ascii_digit() {
        return None;
    }
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i >= b.len() || b[i] != b'/' {
        return None;
    }
    i += 1;
    if i >= b.len() || !matches!(b[i], b'+' | b'-') {
        return None;
    }
    i += 1;
    let start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i > start {
        Some(i)
    } else {
        None
    }
}

/// Walk a parse tree, collecting one `GapDetail` per unsupported item.
///
/// Deduplicates by `handler` key so each gap appears at most once per card.
/// Replacement nodes are skipped for handler key generation (they don't produce
/// handler keys in the `check_*` flow), but their children are always recursed.
fn extract_gap_details(items: &[ParsedItem]) -> Vec<GapDetail> {
    let mut seen = std::collections::HashSet::new();
    let mut details = Vec::new();
    extract_gap_details_inner(items, &mut seen, &mut details);
    details
}

fn extract_gap_details_inner(
    items: &[ParsedItem],
    seen: &mut std::collections::HashSet<String>,
    details: &mut Vec<GapDetail>,
) {
    for item in items {
        if item.category == ParseCategory::Replacement {
            // Replacements don't produce handler keys in check_*, but recurse into children
            extract_gap_details_inner(&item.children, seen, details);
            continue;
        }

        if !item.supported {
            let handler = match item.category {
                ParseCategory::Keyword => format!("Keyword:{}", item.label),
                ParseCategory::Ability => format!("Effect:{}", item.label),
                ParseCategory::Trigger => format!("Trigger:{}", item.label),
                ParseCategory::Static => format!("Static:{}", item.label),
                ParseCategory::Cost => format!("Cost:{}", item.label),
                ParseCategory::Replacement => unreachable!(),
            };
            if seen.insert(handler.clone()) {
                details.push(GapDetail {
                    handler,
                    source_text: item.source_text.clone(),
                });
            }
        }

        // Always recurse into children for nested unsupported items
        extract_gap_details_inner(&item.children, seen, details);
    }
}

impl ParsedItem {
    /// Returns true if this item and all its children are supported.
    pub fn is_fully_supported(&self) -> bool {
        self.supported && self.children.iter().all(ParsedItem::is_fully_supported)
    }
}

/// Check whether a game object has any mechanics the engine cannot handle.
///
/// Checks keywords (Unknown variant = unrecognized), abilities (api_type
/// not in effect registry), triggers (mode not in trigger registry), and
/// static abilities (mode not in static registry).
pub fn unimplemented_mechanics(obj: &GameObject) -> Vec<String> {
    let mut missing = Vec::new();

    // 1. Any Unknown keyword means the parser didn't recognize it
    for kw in &obj.keywords {
        if let Keyword::Unknown(s) = kw {
            missing.push(format!("Keyword: {s}"));
        }
    }

    // 2. Check abilities against known effect types
    for def in &obj.abilities {
        if let Effect::Unimplemented { name, .. } = &*def.effect {
            missing.push(format!("Effect: {name}"));
        }
    }

    // 3. Check trigger modes against trigger registry
    let trigger_registry = build_trigger_registry();
    for trig in &obj.trigger_definitions {
        if matches!(&trig.mode, TriggerMode::Unknown(_))
            || !trigger_registry.contains_key(&trig.mode)
        {
            missing.push(format!("Trigger: {}", trig.mode));
        }
    }

    // 4. Check static ability modes against static registry
    let static_registry = build_static_registry();
    for stat in &obj.static_definitions {
        if !static_registry.contains_key(&stat.mode) && !is_data_carrying_static(&stat.mode) {
            missing.push(format!("Static: {}", stat.mode));
        }
    }

    missing
}

/// Analyze card coverage by checking which cards have all their abilities,
/// triggers, keywords, and static abilities supported by the engine's registries.
pub fn analyze_coverage(card_db: &CardDatabase) -> CoverageSummary {
    let trigger_registry = build_trigger_registry();
    let static_registry = build_static_registry();

    // Count distinct keyword variants across all cards (excluding Unknown)
    let keyword_count = {
        let mut seen = std::collections::HashSet::new();
        for (_key, face) in card_db.face_iter() {
            for kw in &face.keywords {
                if !matches!(kw, Keyword::Unknown(_)) {
                    seen.insert(std::mem::discriminant(kw));
                }
            }
        }
        seen.len()
    };

    let mut cards = Vec::new();
    let mut freq: HashMap<String, usize> = HashMap::new();
    let mut coverage_by_format_accumulators: BTreeMap<String, (usize, usize)> = LegalityFormat::ALL
        .into_iter()
        .map(|format| (format.as_key().to_string(), (0, 0)))
        .collect();

    for (key, face) in card_db.face_iter() {
        let mut missing = Vec::new();

        // Check abilities
        check_abilities(&face.abilities, &mut missing);

        // Check additional cost
        check_additional_cost(&face.additional_cost, &mut missing);

        // Check triggers
        check_triggers(&face.triggers, &trigger_registry, &mut missing);

        // Check keywords
        check_keywords(&face.keywords, &mut missing);

        // Check static abilities
        check_statics(&face.static_abilities, &static_registry, &mut missing);

        // Check replacements
        check_replacements(&face.replacements, &mut missing);

        let supported = missing.is_empty();

        for m in &missing {
            *freq.entry(m.clone()).or_default() += 1;
        }

        for format in LegalityFormat::ALL {
            if card_db
                .legality_status(key, format)
                .is_some_and(|status| status.is_legal())
            {
                let entry = coverage_by_format_accumulators
                    .get_mut(format.as_key())
                    .expect("all legality formats must be pre-seeded");
                entry.0 += 1;
                if supported {
                    entry.1 += 1;
                }
            }
        }

        let parse_details = build_parse_details(face, &trigger_registry, &static_registry);
        let gap_details = extract_gap_details(&parse_details);
        let gap_count = gap_details.len();

        cards.push(CardCoverageResult {
            card_name: face.name.clone(),
            set_code: String::new(),
            supported,
            gap_details,
            gap_count,
            oracle_text: face.oracle_text.clone(),
            parse_details,
        });
    }

    let total_cards = cards.len();
    let supported_cards = cards.iter().filter(|c| c.supported).count();
    let coverage_pct = if total_cards > 0 {
        (supported_cards as f64 / total_cards as f64) * 100.0
    } else {
        0.0
    };

    // Internal frequency list — used to seed top_gaps but not stored on output
    let mut handler_frequency: Vec<(String, usize)> = freq.into_iter().collect();
    handler_frequency.sort_by_key(|b| std::cmp::Reverse(b.1));

    // Compute enriched top_gaps: single-gap counts, oracle patterns, co-occurrence
    let top_gaps = {
        // Single-gap card counts with format breakdown
        let mut gap_data: HashMap<String, (usize, BTreeMap<String, usize>)> = HashMap::new();
        for card in &cards {
            if card.gap_count == 1 {
                let handler = &card.gap_details[0].handler;
                let entry = gap_data.entry(handler.clone()).or_default();
                entry.0 += 1;
                for format in LegalityFormat::ALL {
                    if card_db
                        .legality_status(&card.card_name, format)
                        .is_some_and(|status| status.is_legal())
                    {
                        *entry.1.entry(format.as_key().to_string()).or_default() += 1;
                    }
                }
            }
        }

        // Build per-handler oracle pattern and co-occurrence data from gap_details
        let top_50_handlers: Vec<String> = handler_frequency
            .iter()
            .take(50)
            .map(|(h, _)| h.clone())
            .collect();
        let top_50_set: std::collections::HashSet<&str> =
            top_50_handlers.iter().map(|s| s.as_str()).collect();

        // Collect oracle patterns and co-occurrences for top-50 handlers
        let mut oracle_texts: HashMap<&str, HashMap<String, (usize, Vec<String>)>> = HashMap::new();
        let mut co_occur: HashMap<&str, HashMap<&str, usize>> = HashMap::new();

        for card in &cards {
            if card.gap_details.is_empty() {
                continue;
            }
            let card_handlers: Vec<&str> = card
                .gap_details
                .iter()
                .map(|g| g.handler.as_str())
                .collect();

            for gap in &card.gap_details {
                let handler = gap.handler.as_str();
                if !top_50_set.contains(handler) {
                    continue;
                }

                // Oracle pattern aggregation
                if let Some(text) = &gap.source_text {
                    let pattern = normalize_oracle_pattern(text);
                    let pattern_entry = oracle_texts.entry(handler).or_default();
                    let (count, examples) = pattern_entry
                        .entry(pattern)
                        .or_insert_with(|| (0, Vec::new()));
                    *count += 1;
                    if examples.len() < 3 {
                        examples.push(card.card_name.clone());
                    }
                }

                // Co-occurrence: count other handlers on this card
                for other in &card_handlers {
                    if *other != handler {
                        *co_occur
                            .entry(handler)
                            .or_default()
                            .entry(other)
                            .or_default() += 1;
                    }
                }
            }
        }

        handler_frequency
            .iter()
            .take(50)
            .map(|(handler, total_count)| {
                let (single_gap_cards, single_gap_by_format) =
                    gap_data.remove(handler.as_str()).unwrap_or_default();

                // Oracle patterns: sort by count, keep top 20
                let oracle_patterns = {
                    let mut patterns: Vec<OraclePattern> = oracle_texts
                        .remove(handler.as_str())
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(pattern, (count, example_cards))| OraclePattern {
                            pattern,
                            count,
                            example_cards,
                        })
                        .collect();
                    patterns.sort_by_key(|p| std::cmp::Reverse(p.count));
                    patterns.truncate(20);
                    patterns
                };

                // Independence ratio
                let independence_ratio = if *total_count >= 5 {
                    Some(single_gap_cards as f64 / *total_count as f64)
                } else {
                    None
                };

                // Co-occurrences: sort by shared count, keep top 10
                let co_occurrences = {
                    let mut co: Vec<CoOccurrence> = co_occur
                        .remove(handler.as_str())
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(h, shared_cards)| CoOccurrence {
                            handler: h.to_string(),
                            shared_cards,
                        })
                        .collect();
                    co.sort_by_key(|c| std::cmp::Reverse(c.shared_cards));
                    co.truncate(10);
                    co
                };

                GapFrequency {
                    handler: handler.clone(),
                    total_count: *total_count,
                    single_gap_cards,
                    single_gap_by_format,
                    oracle_patterns,
                    independence_ratio,
                    co_occurrences,
                }
            })
            .collect()
    };

    // Gap bundles: group unsupported cards by exact handler set (2-gap and 3-gap)
    let gap_bundles = {
        let mut bundle_map: HashMap<Vec<String>, (usize, BTreeMap<String, usize>)> = HashMap::new();

        for card in &cards {
            if card.gap_count == 2 || card.gap_count == 3 {
                let mut handlers: Vec<String> =
                    card.gap_details.iter().map(|g| g.handler.clone()).collect();
                handlers.sort();

                let entry = bundle_map.entry(handlers).or_default();
                entry.0 += 1;
                for format in LegalityFormat::ALL {
                    if card_db
                        .legality_status(&card.card_name, format)
                        .is_some_and(|status| status.is_legal())
                    {
                        *entry.1.entry(format.as_key().to_string()).or_default() += 1;
                    }
                }
            }
        }

        let mut two_gap: Vec<GapBundle> = Vec::new();
        let mut three_gap: Vec<GapBundle> = Vec::new();

        for (handlers, (unlocked_cards, unlocked_by_format)) in bundle_map {
            let bundle = GapBundle {
                handlers: handlers.clone(),
                unlocked_cards,
                unlocked_by_format,
            };
            if handlers.len() == 2 {
                two_gap.push(bundle);
            } else {
                three_gap.push(bundle);
            }
        }

        two_gap.sort_by_key(|b| std::cmp::Reverse(b.unlocked_cards));
        three_gap.sort_by_key(|b| std::cmp::Reverse(b.unlocked_cards));

        two_gap.truncate(30);
        three_gap.truncate(20);

        two_gap.extend(three_gap);
        two_gap
    };

    let coverage_by_format = coverage_by_format_accumulators
        .into_iter()
        .map(|(format, (total_cards, supported_cards))| {
            let coverage_pct = if total_cards > 0 {
                (supported_cards as f64 / total_cards as f64) * 100.0
            } else {
                0.0
            };
            (
                format,
                FormatCoverageSummary {
                    total_cards,
                    supported_cards,
                    coverage_pct,
                },
            )
        })
        .collect();

    CoverageSummary {
        total_cards,
        supported_cards,
        coverage_pct,
        keyword_count,
        coverage_by_format,
        cards,
        top_gaps,
        gap_bundles,
    }
}

pub fn card_face_has_unimplemented_parts(face: &CardFace) -> bool {
    ability_definitions_have_unimplemented_parts(&face.abilities)
        || face
            .additional_cost
            .as_ref()
            .is_some_and(additional_cost_has_unimplemented_parts)
        || face.triggers.iter().any(trigger_has_unimplemented_parts)
        || face
            .replacements
            .iter()
            .any(replacement_has_unimplemented_parts)
        || face
            .static_abilities
            .iter()
            .any(static_has_unimplemented_parts)
}

fn static_has_unimplemented_parts(def: &StaticDefinition) -> bool {
    matches!(def.condition, Some(StaticCondition::Unrecognized { .. }))
}

/// Returns the list of unsupported handler labels for a card face (e.g.
/// "Effect:Unimplemented", "Trigger:ChangesZone", "Keyword:someKeyword").
/// Empty means the card is fully supported.
pub fn card_face_gaps(face: &CardFace) -> Vec<String> {
    let trigger_registry = build_trigger_registry();
    let static_registry = build_static_registry();
    let mut missing = Vec::new();
    check_keywords(&face.keywords, &mut missing);
    check_abilities(&face.abilities, &mut missing);
    check_triggers(&face.triggers, &trigger_registry, &mut missing);
    check_statics(&face.static_abilities, &static_registry, &mut missing);
    check_additional_cost(&face.additional_cost, &mut missing);
    check_replacements(&face.replacements, &mut missing);
    missing
}

/// Convenience wrapper that builds the registries internally so callers
/// don't need to construct them.
pub fn build_parse_details_for_face(face: &CardFace) -> Vec<ParsedItem> {
    let trigger_registry = build_trigger_registry();
    let static_registry = build_static_registry();
    build_parse_details(face, &trigger_registry, &static_registry)
}

fn check_abilities(abilities: &[AbilityDefinition], missing: &mut Vec<String>) {
    for def in abilities {
        collect_ability_missing_parts(def, missing);
    }
}

fn check_triggers(
    triggers: &[TriggerDefinition],
    trigger_registry: &HashMap<TriggerMode, crate::game::triggers::TriggerMatcher>,
    missing: &mut Vec<String>,
) {
    for def in triggers {
        if let Some(execute) = &def.execute {
            collect_ability_missing_parts(execute, missing);
        }
        if matches!(&def.mode, TriggerMode::Unknown(_)) || !trigger_registry.contains_key(&def.mode)
        {
            let label = format!("Trigger:{}", def.mode);
            if !missing.contains(&label) {
                missing.push(label);
            }
        }
    }
}

fn check_keywords(keywords: &[Keyword], missing: &mut Vec<String>) {
    for kw in keywords {
        if let Keyword::Unknown(s) = kw {
            let label = format!("Keyword:{}", s);
            if !missing.contains(&label) {
                missing.push(label);
            }
        }
    }
}

fn check_additional_cost(additional_cost: &Option<AdditionalCost>, missing: &mut Vec<String>) {
    if let Some(additional_cost) = additional_cost {
        collect_additional_cost_missing_parts(additional_cost, missing);
    }
}

fn check_statics(
    statics: &[StaticDefinition],
    static_registry: &HashMap<StaticMode, StaticAbilityHandler>,
    missing: &mut Vec<String>,
) {
    for def in statics {
        if !static_registry.contains_key(&def.mode) && !is_data_carrying_static(&def.mode) {
            let label = format!("Static:{}", def.mode);
            if !missing.contains(&label) {
                missing.push(label);
            }
        }
        // Flag unrecognized conditions — these represent parser gaps where
        // the condition text wasn't decomposed into typed building blocks.
        if let Some(StaticCondition::Unrecognized { ref text }) = def.condition {
            let label = format!("Static:Unrecognized({})", truncate_label(text, 60));
            if !missing.contains(&label) {
                missing.push(label);
            }
        }
    }
}

fn truncate_label(text: &str, max: usize) -> &str {
    if text.len() <= max {
        text
    } else {
        &text[..max]
    }
}

fn check_replacements(replacements: &[ReplacementDefinition], missing: &mut Vec<String>) {
    for def in replacements {
        if let Some(execute) = &def.execute {
            collect_ability_missing_parts(execute, missing);
        }

        if let ReplacementMode::Optional {
            decline: Some(decline),
        } = &def.mode
        {
            collect_ability_missing_parts(decline, missing);
        }
    }
}

fn ability_definitions_have_unimplemented_parts(abilities: &[AbilityDefinition]) -> bool {
    abilities
        .iter()
        .any(ability_definition_has_unimplemented_parts)
}

fn trigger_has_unimplemented_parts(trigger: &TriggerDefinition) -> bool {
    trigger
        .execute
        .as_ref()
        .is_some_and(|execute| ability_definition_has_unimplemented_parts(execute))
}

fn replacement_has_unimplemented_parts(replacement: &ReplacementDefinition) -> bool {
    replacement
        .execute
        .as_ref()
        .is_some_and(|execute| ability_definition_has_unimplemented_parts(execute))
        || matches!(
            &replacement.mode,
            ReplacementMode::Optional {
                decline: Some(decline),
            } if ability_definition_has_unimplemented_parts(decline)
        )
}

fn ability_definition_has_unimplemented_parts(def: &AbilityDefinition) -> bool {
    matches!(*def.effect, Effect::Unimplemented { .. })
        || def
            .cost
            .as_ref()
            .is_some_and(ability_cost_has_unimplemented_parts)
        || def
            .sub_ability
            .as_ref()
            .is_some_and(|sub| ability_definition_has_unimplemented_parts(sub))
        || def
            .mode_abilities
            .iter()
            .any(ability_definition_has_unimplemented_parts)
}

fn additional_cost_has_unimplemented_parts(additional_cost: &AdditionalCost) -> bool {
    match additional_cost {
        AdditionalCost::Optional(cost) | AdditionalCost::Required(cost) => {
            ability_cost_has_unimplemented_parts(cost)
        }
        AdditionalCost::Choice(first, second) => {
            ability_cost_has_unimplemented_parts(first)
                || ability_cost_has_unimplemented_parts(second)
        }
    }
}

fn ability_cost_has_unimplemented_parts(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::Composite { costs } => costs.iter().any(ability_cost_has_unimplemented_parts),
        AbilityCost::Unimplemented { .. } => true,
        _ => false,
    }
}

fn collect_ability_missing_parts(def: &AbilityDefinition, missing: &mut Vec<String>) {
    if let Effect::Unimplemented { name, .. } = &*def.effect {
        let label = format!("Effect:{name}");
        if !missing.contains(&label) {
            missing.push(label);
        }
    }

    if let Some(cost) = &def.cost {
        collect_ability_cost_missing_parts(cost, missing);
    }

    if let Some(sub_ability) = &def.sub_ability {
        collect_ability_missing_parts(sub_ability, missing);
    }

    for mode_ability in &def.mode_abilities {
        collect_ability_missing_parts(mode_ability, missing);
    }
}

fn collect_additional_cost_missing_parts(
    additional_cost: &AdditionalCost,
    missing: &mut Vec<String>,
) {
    match additional_cost {
        AdditionalCost::Optional(cost) | AdditionalCost::Required(cost) => {
            collect_ability_cost_missing_parts(cost, missing);
        }
        AdditionalCost::Choice(first, second) => {
            collect_ability_cost_missing_parts(first, missing);
            collect_ability_cost_missing_parts(second, missing);
        }
    }
}

/// A card flagged by the silent-drop audit where Oracle text lines exceed
/// the number of parsed items, indicating the parser consumed text without
/// producing a corresponding ability definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SilentDropResult {
    pub card_name: String,
    pub oracle_lines: usize,
    pub parsed_items: usize,
    pub delta: usize,
    /// Oracle lines with no corresponding parse item (best-effort match).
    pub missing_lines: Vec<String>,
}

/// Audit all "supported" cards for silently dropped Oracle text lines.
///
/// Compares effective Oracle line count against effective parsed item count.
/// Cards where oracle lines exceed parsed items are flagged as potential
/// silent drops — the parser matched text but didn't emit an ability definition.
pub fn audit_silent_drops(summary: &CoverageSummary) -> Vec<SilentDropResult> {
    let mut results = Vec::new();

    for card in &summary.cards {
        if !card.supported {
            continue;
        }

        let oracle_text = match &card.oracle_text {
            Some(text) if !text.is_empty() => text,
            _ => continue,
        };

        let effective_oracle = count_effective_oracle_lines(oracle_text);
        let effective_parsed = count_effective_parsed_items(&card.parse_details);

        if effective_oracle > effective_parsed {
            let missing_lines = find_missing_lines(oracle_text, &card.parse_details);
            results.push(SilentDropResult {
                card_name: card.card_name.clone(),
                oracle_lines: effective_oracle,
                parsed_items: effective_parsed,
                delta: effective_oracle - effective_parsed,
                missing_lines,
            });
        }
    }

    results
}

/// Count effective Oracle text lines, accounting for modal/choose headers
/// that cover their following bullet points as a single unit.
fn count_effective_oracle_lines(oracle_text: &str) -> usize {
    let lines: Vec<&str> = oracle_text
        .split('\n')
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();

    let mut count = 0;
    let mut in_modal = false;

    for line in &lines {
        // Strip reminder text (parenthesized text)
        let stripped = strip_parenthesized_reminder(line);
        let stripped = stripped.trim();
        if stripped.is_empty() {
            continue;
        }

        let lower = stripped.to_lowercase();

        // Check if this line contains a modal header ("choose one —", "choose two.", etc.)
        // Handles standalone headers, triggered modals ("when enters, choose one —"),
        // activated modals ("{cost}: choose one —"), and period-terminated ("choose three.")
        if is_modal_header_line(&lower) {
            count += 1;
            in_modal = true;
            continue;
        }

        // Bullet points under a modal header are sub-items, not separate lines
        if in_modal && stripped.starts_with('\u{2022}') {
            // Don't count — part of the preceding choose header
            continue;
        }

        // Non-bullet line ends the modal section
        if in_modal && !stripped.starts_with('\u{2022}') {
            in_modal = false;
        }

        count += 1;
    }

    count
}

/// Check if a line contains a modal header pattern: "choose one", "choose two", etc.
/// Matches standalone, triggered, activated, and period-terminated forms.
fn is_modal_header_line(lower: &str) -> bool {
    const CHOOSE_PHRASES: &[&str] = &[
        "choose one",
        "choose two",
        "choose three",
        "choose four",
        "choose five",
        "choose up to one",
        "choose up to two",
        "choose up to three",
        "choose any number",
    ];
    CHOOSE_PHRASES.iter().any(|p| lower.contains(p))
}

/// Strip parenthesized reminder text from a line.
fn strip_parenthesized_reminder(line: &str) -> String {
    let mut result = String::with_capacity(line.len());
    let mut depth = 0u32;
    for ch in line.chars() {
        match ch {
            '(' => depth += 1,
            ')' if depth > 0 => depth -= 1,
            _ if depth == 0 => result.push(ch),
            _ => {}
        }
    }
    result
}

/// Count effective parsed items, recursively counting children for
/// modal/choose nodes (which represent multiple Oracle lines as one node).
fn count_effective_parsed_items(items: &[ParsedItem]) -> usize {
    let mut count = 0;
    for item in items {
        if item.children.is_empty() {
            count += 1;
        } else {
            // A modal/choose parent + its children count as 1 + children
            // (the header is the parent, each bullet is a child)
            count += 1 + item.children.len();
        }
    }
    count
}

/// Find Oracle text lines that have no corresponding parsed item by
/// matching against source_text fields in the parse tree.
fn find_missing_lines(oracle_text: &str, parse_details: &[ParsedItem]) -> Vec<String> {
    let mut source_texts: Vec<String> = Vec::new();
    collect_source_texts(parse_details, &mut source_texts);

    let source_lower: Vec<String> = source_texts.iter().map(|s| s.to_lowercase()).collect();

    oracle_text
        .split('\n')
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .filter(|line| {
            let lower = line.to_lowercase();
            let stripped = strip_parenthesized_reminder(&lower);
            let stripped = stripped.trim();
            if stripped.is_empty() {
                return false;
            }
            // A line is "missing" if no source_text contains it or is contained by it
            !source_lower
                .iter()
                .any(|src| src.contains(stripped) || stripped.contains(src.as_str()))
        })
        .map(|l| l.to_string())
        .collect()
}

/// Recursively collect all source_text values from the parse tree.
fn collect_source_texts(items: &[ParsedItem], out: &mut Vec<String>) {
    for item in items {
        if let Some(ref src) = item.source_text {
            out.push(src.clone());
        }
        collect_source_texts(&item.children, out);
    }
}

fn collect_ability_cost_missing_parts(cost: &AbilityCost, missing: &mut Vec<String>) {
    match cost {
        AbilityCost::Composite { costs } => {
            for nested_cost in costs {
                collect_ability_cost_missing_parts(nested_cost, missing);
            }
        }
        AbilityCost::Unimplemented { description } => {
            let label = format!("Cost:{description}");
            if !missing.contains(&label) {
                missing.push(label);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Resolver feature audit — detect structural features in parsed card data
// that the resolver may silently ignore.
// ---------------------------------------------------------------------------

/// A structural feature detected in a card's parsed ability data.
/// Features are string-tagged for extensibility: new features automatically
/// surface as unhandled when the parser emits them but the registry doesn't
/// include them.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ResolverFeature {
    /// Broad category: "structural", "condition", "quantity_ref"
    pub category: String,
    /// Specific feature tag, e.g. "else_ability", "QuantityCheck", "EventContextSourcePower"
    pub feature: String,
}

impl std::fmt::Display for ResolverFeature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.category, self.feature)
    }
}

/// Per-card audit result: features used that aren't in the known-handled registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolverAuditCard {
    pub card_name: String,
    pub unhandled_features: Vec<String>,
}

/// Frequency entry for a single feature across all audited cards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureUsage {
    pub feature: String,
    pub card_count: usize,
    pub handled: bool,
    pub example_cards: Vec<String>,
}

/// Aggregate audit results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolverAuditSummary {
    pub total_supported_audited: usize,
    pub cards_with_unhandled_features: usize,
    pub unhandled_features: Vec<FeatureUsage>,
    /// All features detected across supported cards, including handled ones.
    /// Useful for verifying the registry is comprehensive.
    pub all_features: Vec<FeatureUsage>,
    pub flagged_cards: Vec<ResolverAuditCard>,
}

/// Walk all "Fully Supported" cards and flag structural features that the
/// resolver may not handle. This catches the class of bug where the parser
/// correctly emits a field but the resolver silently skips it.
pub fn audit_resolver_features(card_db: &CardDatabase) -> ResolverAuditSummary {
    let trigger_registry = build_trigger_registry();
    let static_registry = build_static_registry();
    let handled = resolver_handled_features();

    // Collect per-card features, only for supported cards
    let mut feature_freq: HashMap<String, (usize, Vec<String>)> = HashMap::new();
    let mut flagged_cards = Vec::new();
    let mut total_audited = 0;

    for (key, face) in card_db.face_iter() {
        // Only audit cards the existing coverage considers "Fully Supported"
        if !is_card_supported(face, &trigger_registry, &static_registry) {
            continue;
        }
        total_audited += 1;

        let mut features = HashSet::new();
        extract_card_features(face, &mut features);

        // Record frequency for ALL features
        for feat in &features {
            let entry = feature_freq
                .entry(feat.clone())
                .or_insert_with(|| (0, Vec::new()));
            entry.0 += 1;
            if entry.1.len() < 3 {
                entry.1.push(key.to_string());
            }
        }

        // Flag unhandled features
        let unhandled: Vec<String> = features
            .iter()
            .filter(|f| !handled.contains(f.as_str()))
            .cloned()
            .collect();

        if !unhandled.is_empty() {
            flagged_cards.push(ResolverAuditCard {
                card_name: key.to_string(),
                unhandled_features: unhandled,
            });
        }
    }

    // Build frequency tables
    let mut all_features: Vec<FeatureUsage> = feature_freq
        .iter()
        .map(|(feat, (count, examples))| FeatureUsage {
            feature: feat.clone(),
            card_count: *count,
            handled: handled.contains(feat.as_str()),
            example_cards: examples.clone(),
        })
        .collect();
    all_features.sort_by_key(|f| std::cmp::Reverse(f.card_count));

    let unhandled_features: Vec<FeatureUsage> = all_features
        .iter()
        .filter(|f| !f.handled)
        .cloned()
        .collect();

    flagged_cards.sort_by_key(|c| std::cmp::Reverse(c.unhandled_features.len()));

    ResolverAuditSummary {
        total_supported_audited: total_audited,
        cards_with_unhandled_features: flagged_cards.len(),
        unhandled_features,
        all_features,
        flagged_cards,
    }
}

/// Quick check whether a card is "Fully Supported" by existing coverage criteria
/// (no Unimplemented effects, no Unknown triggers/statics/keywords).
fn is_card_supported(
    face: &CardFace,
    trigger_registry: &HashMap<TriggerMode, crate::game::triggers::TriggerMatcher>,
    static_registry: &HashMap<StaticMode, StaticAbilityHandler>,
) -> bool {
    // Check abilities
    for def in &face.abilities {
        if !is_ability_supported(def) {
            return false;
        }
    }
    // Check triggers
    for trig in &face.triggers {
        if matches!(&trig.mode, TriggerMode::Unknown(_))
            || !trigger_registry.contains_key(&trig.mode)
        {
            return false;
        }
        if let Some(execute) = &trig.execute {
            if !is_ability_supported(execute) {
                return false;
            }
        }
    }
    // Check statics
    for stat in &face.static_abilities {
        if !static_registry.contains_key(&stat.mode) && !is_data_carrying_static(&stat.mode) {
            return false;
        }
    }
    // Check replacements
    for repl in &face.replacements {
        if let Some(execute) = &repl.execute {
            if !is_ability_supported(execute) {
                return false;
            }
        }
    }
    // Check keywords
    for kw in &face.keywords {
        if matches!(kw, Keyword::Unknown(_)) {
            return false;
        }
    }
    true
}

/// Check if an ability definition tree has any Unimplemented effects.
fn is_ability_supported(def: &AbilityDefinition) -> bool {
    if matches!(&*def.effect, Effect::Unimplemented { .. }) {
        return false;
    }
    if let Some(sub) = &def.sub_ability {
        if !is_ability_supported(sub) {
            return false;
        }
    }
    if let Some(else_ab) = &def.else_ability {
        if !is_ability_supported(else_ab) {
            return false;
        }
    }
    for mode_ab in &def.mode_abilities {
        if !is_ability_supported(mode_ab) {
            return false;
        }
    }
    true
}

/// Extract structural feature tags from a card's entire parsed data.
fn extract_card_features(face: &CardFace, features: &mut HashSet<String>) {
    for def in &face.abilities {
        extract_ability_features(def, features);
    }
    for trig in &face.triggers {
        if let Some(execute) = &trig.execute {
            extract_ability_features(execute, features);
        }
        // Trigger-level condition (intervening-if)
        if trig.condition.is_some() {
            features.insert("structural:trigger_condition".into());
        }
    }
    for repl in &face.replacements {
        if let Some(execute) = &repl.execute {
            extract_ability_features(execute, features);
        }
    }
    // Static abilities with conditions
    for stat in &face.static_abilities {
        if let Some(ref cond) = stat.condition {
            extract_static_condition_features(cond, features);
        }
    }
    if face.additional_cost.is_some() {
        features.insert("structural:additional_cost".into());
    }
    if face.modal.is_some() {
        features.insert("structural:spell_modal".into());
    }
}

/// Extract features from a static condition.
fn extract_static_condition_features(cond: &StaticCondition, features: &mut HashSet<String>) {
    match cond {
        StaticCondition::QuantityComparison { lhs, rhs, .. } => {
            features.insert("static_condition:QuantityComparison".into());
            extract_quantity_features(lhs, features);
            extract_quantity_features(rhs, features);
        }
        StaticCondition::And { conditions } | StaticCondition::Or { conditions } => {
            for sub in conditions {
                extract_static_condition_features(sub, features);
            }
        }
        _ => {
            // Use debug formatting to capture variant name for less common conditions
            let variant = format!("{cond:?}");
            let name = variant
                .split(&[' ', '(', '{'][..])
                .next()
                .unwrap_or("Unknown");
            features.insert(format!("static_condition:{name}"));
        }
    }
}

/// Recursively extract structural feature tags from an ability definition tree.
fn extract_ability_features(def: &AbilityDefinition, features: &mut HashSet<String>) {
    // Condition
    if let Some(ref cond) = def.condition {
        features.insert("structural:condition".into());
        let variant = condition_variant_name(cond);
        features.insert(format!("condition:{variant}"));
        extract_condition_quantity_features(cond, features);
    }

    // Else ability
    if let Some(ref else_ab) = def.else_ability {
        features.insert("structural:else_ability".into());
        extract_ability_features(else_ab, features);
    }

    // Repeat-for
    if let Some(ref qty) = def.repeat_for {
        features.insert("structural:repeat_for".into());
        extract_quantity_features(qty, features);
    }

    // Forward result
    if def.forward_result {
        features.insert("structural:forward_result".into());
    }

    // Player scope
    if let Some(ref scope) = def.player_scope {
        let variant = format!("{scope:?}");
        let name = variant
            .split(&[' ', '(', '{'][..])
            .next()
            .unwrap_or("Unknown");
        features.insert(format!("player_scope:{name}"));
    }

    // Optional-for (opponent may)
    if def.optional_for.is_some() {
        features.insert("structural:optional_for".into());
    }

    // Multi-target
    if def.multi_target.is_some() {
        features.insert("structural:multi_target".into());
    }

    // Distribute
    if def.distribute.is_some() {
        features.insert("structural:distribute".into());
    }

    // Modal (on ability, not spell-level)
    if def.modal.is_some() {
        features.insert("structural:ability_modal".into());
    }

    // Cost reduction
    if def.cost_reduction.is_some() {
        features.insert("structural:cost_reduction".into());
    }

    // Duration (continuous effects from spells/abilities)
    if def.duration.is_some() {
        features.insert("structural:duration".into());
    }

    // Effect-level quantity refs (e.g., DealDamage with dynamic amount)
    extract_effect_quantity_features(&def.effect, features);

    // Recurse into sub-abilities
    if let Some(ref sub) = def.sub_ability {
        extract_ability_features(sub, features);
    }
    for mode_ab in &def.mode_abilities {
        extract_ability_features(mode_ab, features);
    }
}

/// Extract QuantityRef variants from within conditions.
fn extract_condition_quantity_features(cond: &AbilityCondition, features: &mut HashSet<String>) {
    if let AbilityCondition::QuantityCheck { lhs, rhs, .. } = cond {
        extract_quantity_features(lhs, features);
        extract_quantity_features(rhs, features);
    }
}

/// Extract QuantityRef variant tags from a QuantityExpr.
fn extract_quantity_features(qty: &QuantityExpr, features: &mut HashSet<String>) {
    match qty {
        QuantityExpr::Fixed { .. } => {}
        QuantityExpr::Ref { qty: qref } => {
            let variant = quantity_ref_variant_name(qref);
            features.insert(format!("quantity_ref:{variant}"));
        }
        QuantityExpr::Offset { inner, .. } | QuantityExpr::Multiply { inner, .. } => {
            extract_quantity_features(inner, features);
        }
        QuantityExpr::HalfRounded { inner, .. } => {
            extract_quantity_features(inner, features);
        }
    }
}

/// Extract QuantityRef variants from effect parameters (DealDamage amount, etc.).
fn extract_effect_quantity_features(effect: &Effect, features: &mut HashSet<String>) {
    match effect {
        Effect::DealDamage { amount, .. } => extract_quantity_features(amount, features),
        Effect::Draw { count, .. } => extract_quantity_features(count, features),
        Effect::Mill { count, .. } => extract_quantity_features(count, features),
        Effect::GainLife { amount, .. } => extract_quantity_features(amount, features),
        Effect::LoseLife { amount, .. } => extract_quantity_features(amount, features),
        Effect::IncreaseSpeed { amount, .. } => extract_quantity_features(amount, features),
        Effect::PutCounter { count, .. } => extract_quantity_features(count, features),
        Effect::PutCounterAll { count, .. } => extract_quantity_features(count, features),
        Effect::Token { count, .. } => extract_quantity_features(count, features),
        Effect::Pump {
            power, toughness, ..
        } => {
            if let PtValue::Quantity(qty) = power {
                extract_quantity_features(qty, features);
            }
            if let PtValue::Quantity(qty) = toughness {
                extract_quantity_features(qty, features);
            }
        }
        _ => {}
    }
}

/// Map an AbilityCondition to its variant name string.
fn condition_variant_name(cond: &AbilityCondition) -> &'static str {
    match cond {
        AbilityCondition::AdditionalCostPaid => "AdditionalCostPaid",
        AbilityCondition::AdditionalCostPaidInstead => "AdditionalCostPaidInstead",
        AbilityCondition::AdditionalCostNotPaid => "AdditionalCostNotPaid",
        AbilityCondition::IfYouDo => "IfYouDo",
        AbilityCondition::WhenYouDo => "WhenYouDo",
        AbilityCondition::CastFromZone { .. } => "CastFromZone",
        AbilityCondition::RevealedHasCardType { .. } => "RevealedHasCardType",
        AbilityCondition::SourceDidNotEnterThisTurn => "SourceDidNotEnterThisTurn",
        AbilityCondition::NinjutsuVariantPaid { .. } => "NinjutsuVariantPaid",
        AbilityCondition::NinjutsuVariantPaidInstead { .. } => "NinjutsuVariantPaidInstead",
        AbilityCondition::IfAPlayerDoes => "IfAPlayerDoes",
        AbilityCondition::QuantityCheck { .. } => "QuantityCheck",
        AbilityCondition::HasMaxSpeed => "HasMaxSpeed",
        AbilityCondition::TargetHasKeywordInstead { .. } => "TargetHasKeywordInstead",
        AbilityCondition::TargetMatchesFilter { .. } => "TargetMatchesFilter",
        AbilityCondition::SourceMatchesFilter { .. } => "SourceMatchesFilter",
        AbilityCondition::IsYourTurn { .. } => "IsYourTurn",
        AbilityCondition::ZoneChangedThisWay { .. } => "ZoneChangedThisWay",
    }
}

/// Map a QuantityRef to its variant name string.
fn quantity_ref_variant_name(qref: &QuantityRef) -> &'static str {
    match qref {
        QuantityRef::HandSize => "HandSize",
        QuantityRef::LifeTotal => "LifeTotal",
        QuantityRef::GraveyardSize => "GraveyardSize",
        QuantityRef::LifeAboveStarting => "LifeAboveStarting",
        QuantityRef::StartingLifeTotal => "StartingLifeTotal",
        QuantityRef::Speed => "Speed",
        QuantityRef::ObjectCount { .. } => "ObjectCount",
        QuantityRef::PlayerCount { .. } => "PlayerCount",
        QuantityRef::CountersOnSelf { .. } => "CountersOnSelf",
        QuantityRef::CountersOnTarget { .. } => "CountersOnTarget",
        QuantityRef::Variable { .. } => "Variable",
        QuantityRef::SelfPower => "SelfPower",
        QuantityRef::SelfToughness => "SelfToughness",
        QuantityRef::Aggregate { .. } => "Aggregate",
        QuantityRef::TargetPower => "TargetPower",
        QuantityRef::TargetLifeTotal => "TargetLifeTotal",
        QuantityRef::Devotion { .. } => "Devotion",
        QuantityRef::CardTypesInGraveyards { .. } => "CardTypesInGraveyards",
        QuantityRef::ZoneCardCount { .. } => "ZoneCardCount",
        QuantityRef::BasicLandTypeCount => "BasicLandTypeCount",
        QuantityRef::TrackedSetSize => "TrackedSetSize",
        QuantityRef::LifeLostThisTurn => "LifeLostThisTurn",
        QuantityRef::EventContextAmount => "EventContextAmount",
        QuantityRef::EventContextSourcePower => "EventContextSourcePower",
        QuantityRef::EventContextSourceToughness => "EventContextSourceToughness",
        QuantityRef::EventContextSourceManaValue => "EventContextSourceManaValue",
        QuantityRef::SpellsCastThisTurn { .. } => "SpellsCastThisTurn",
        QuantityRef::EnteredThisTurn { .. } => "EnteredThisTurn",
        QuantityRef::CrimesCommittedThisTurn => "CrimesCommittedThisTurn",
        QuantityRef::LifeGainedThisTurn => "LifeGainedThisTurn",
        QuantityRef::PermanentsLeftBattlefieldThisTurn => "PermanentsLeftBattlefieldThisTurn",
        QuantityRef::TurnsTaken => "TurnsTaken",
        QuantityRef::ChosenNumber => "ChosenNumber",
        QuantityRef::CreaturesDiedThisTurn => "CreaturesDiedThisTurn",
        QuantityRef::AttackedThisTurn => "AttackedThisTurn",
        QuantityRef::DescendedThisTurn => "DescendedThisTurn",
        QuantityRef::SpellsCastLastTurn => "SpellsCastLastTurn",
        QuantityRef::OpponentLifeLostThisTurn => "OpponentLifeLostThisTurn",
        QuantityRef::CounterAddedThisTurn => "CounterAddedThisTurn",
    }
}

/// Registry of resolver features that are known to be handled.
/// Any feature tag NOT in this set is flagged as potentially unhandled.
///
/// When you add resolver support for a new feature, add its tag here.
fn resolver_handled_features() -> HashSet<&'static str> {
    [
        // -- Structural features handled by resolve_ability_chain --
        "structural:condition",
        "structural:else_ability",
        "structural:repeat_for",
        "structural:forward_result",
        "structural:duration",
        "structural:optional_for",
        "structural:multi_target",
        "structural:distribute",
        "structural:ability_modal",
        "structural:spell_modal",
        "structural:additional_cost",
        "structural:cost_reduction",
        "structural:trigger_condition",
        // -- AbilityCondition variants handled by evaluate_condition --
        "condition:AdditionalCostPaid",
        "condition:AdditionalCostPaidInstead",
        "condition:AdditionalCostNotPaid",
        "condition:IfYouDo",
        "condition:IfAPlayerDoes",
        "condition:WhenYouDo",
        "condition:CastFromZone",
        "condition:RevealedHasCardType",
        "condition:SourceDidNotEnterThisTurn",
        "condition:NinjutsuVariantPaid",
        "condition:NinjutsuVariantPaidInstead",
        "condition:QuantityCheck",
        "condition:HasMaxSpeed",
        "condition:TargetHasKeywordInstead",
        // -- Player scope variants handled by resolve_ability_chain --
        "player_scope:All",
        "player_scope:Opponent",
        "player_scope:OpponentLostLife",
        "player_scope:OpponentGainedLife",
        "player_scope:HighestSpeed",
        // -- QuantityRef variants handled by resolve_quantity --
        "quantity_ref:HandSize",
        "quantity_ref:LifeTotal",
        "quantity_ref:GraveyardSize",
        "quantity_ref:LifeAboveStarting",
        "quantity_ref:Speed",
        "quantity_ref:ObjectCount",
        "quantity_ref:PlayerCount",
        "quantity_ref:CountersOnSelf",
        "quantity_ref:Variable",
        "quantity_ref:SelfPower",
        "quantity_ref:SelfToughness",
        "quantity_ref:Aggregate",
        "quantity_ref:TargetPower",
        "quantity_ref:TargetLifeTotal",
        "quantity_ref:Devotion",
        "quantity_ref:CardTypesInGraveyards",
        "quantity_ref:ZoneCardCount",
        "quantity_ref:BasicLandTypeCount",
        "quantity_ref:TrackedSetSize",
        "quantity_ref:LifeLostThisTurn",
        "quantity_ref:EventContextAmount",
        "quantity_ref:EventContextSourcePower",
        "quantity_ref:EventContextSourceToughness",
        "quantity_ref:EventContextSourceManaValue",
        "quantity_ref:SpellsCastThisTurn",
        "quantity_ref:EnteredThisTurn",
        "quantity_ref:CrimesCommittedThisTurn",
        "quantity_ref:LifeGainedThisTurn",
        // -- Static conditions handled by static_abilities / layers --
        "static_condition:QuantityComparison",
        "static_condition:DevotionGE",
        "static_condition:IsPresent",
        "static_condition:ChosenColorIs",
        "static_condition:HasCounters",
        "static_condition:ClassLevelGE",
        "static_condition:DuringYourTurn",
        "static_condition:SourceEnteredThisTurn",
        "static_condition:IsRingBearer",
        "static_condition:RingLevelAtLeast",
        "static_condition:SourceIsTapped",
        "static_condition:Unrecognized",
        "static_condition:None",
    ]
    .into_iter()
    .collect()
}

// ---------------------------------------------------------------------------
// Semantic audit — detect semantic mismatches between Oracle text and parsed
// ability data across all supported cards.
// ---------------------------------------------------------------------------

/// Walk an ability definition tree, visiting all nested `AbilityDefinition`s including
/// those embedded in compound effects (`FlipCoin`, `RollDie`, `GrantAbility`, etc.).
/// Returns `true` if the predicate returns `true` for any node in the tree.
fn ability_tree_any(def: &AbilityDefinition, pred: &impl Fn(&AbilityDefinition) -> bool) -> bool {
    if pred(def) {
        return true;
    }
    // Standard chaining: sub_ability, else_ability, mode_abilities
    if let Some(ref sub) = def.sub_ability {
        if ability_tree_any(sub, pred) {
            return true;
        }
    }
    if let Some(ref else_ab) = def.else_ability {
        if ability_tree_any(else_ab, pred) {
            return true;
        }
    }
    for mode_ab in &def.mode_abilities {
        if ability_tree_any(mode_ab, pred) {
            return true;
        }
    }
    // Compound effects that embed AbilityDefinitions
    match &*def.effect {
        Effect::FlipCoin {
            win_effect,
            lose_effect,
        } => {
            if let Some(ref w) = win_effect {
                if ability_tree_any(w, pred) {
                    return true;
                }
            }
            if let Some(ref l) = lose_effect {
                if ability_tree_any(l, pred) {
                    return true;
                }
            }
        }
        Effect::FlipCoinUntilLose { win_effect } if ability_tree_any(win_effect, pred) => {
            return true;
        }
        Effect::RollDie { results, .. } => {
            for branch in results {
                if ability_tree_any(&branch.effect, pred) {
                    return true;
                }
            }
        }
        Effect::CreateDelayedTrigger { effect, .. } if ability_tree_any(effect, pred) => {
            return true;
        }
        _ => {}
    }
    // ContinuousModification::GrantAbility inside GenericEffect
    if let Effect::GenericEffect {
        static_abilities, ..
    } = &*def.effect
    {
        for stat in static_abilities {
            for modif in &stat.modifications {
                if let ContinuousModification::GrantAbility { definition } = modif {
                    if ability_tree_any(definition, pred) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// A semantic finding detected during audit of a card's parsed data vs Oracle text.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SemanticFinding {
    /// Ability type mismatch: Oracle text suggests trigger but parsed as static, etc.
    WrongAbilityType {
        oracle_line: String,
        expected: String,
        actual: String,
    },
    /// A parsed ability contains Effect::Unimplemented or AbilityCost::Unimplemented sub-stubs.
    UnimplementedSubEffect {
        oracle_line: String,
        stub_description: String,
    },
    /// Condition field is None when Oracle text contains condition language.
    DroppedCondition {
        oracle_line: String,
        condition_text: String,
    },
    /// Duration field is None when Oracle text contains duration language.
    DroppedDuration {
        oracle_line: String,
        duration_text: String,
    },
    /// Parsed numeric parameter doesn't match Oracle text.
    WrongParameter {
        oracle_line: String,
        field: String,
        expected: String,
        actual: String,
    },
    /// Oracle line has no corresponding parsed item (silent drop).
    SilentDrop { oracle_line: String },
}

impl SemanticFinding {
    fn category_name(&self) -> &'static str {
        match self {
            SemanticFinding::WrongAbilityType { .. } => "WrongAbilityType",
            SemanticFinding::UnimplementedSubEffect { .. } => "UnimplementedSubEffect",
            SemanticFinding::DroppedCondition { .. } => "DroppedCondition",
            SemanticFinding::DroppedDuration { .. } => "DroppedDuration",
            SemanticFinding::WrongParameter { .. } => "WrongParameter",
            SemanticFinding::SilentDrop { .. } => "SilentDrop",
        }
    }
}

/// Per-card semantic audit results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticAuditCard {
    pub card_name: String,
    pub findings: Vec<SemanticFinding>,
}

/// Aggregate semantic audit results across all supported cards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticAuditSummary {
    pub total_supported_audited: usize,
    pub cards_with_findings: usize,
    pub finding_counts: HashMap<String, usize>,
    pub flagged_cards: Vec<SemanticAuditCard>,
}

/// Run a full semantic audit across all supported cards in the database.
///
/// Per-line structural comparison: each Oracle line is matched to its corresponding
/// parsed element(s) via description matching, then checked for expected properties.
pub fn audit_semantic(card_db: &CardDatabase) -> SemanticAuditSummary {
    let trigger_registry = build_trigger_registry();
    let static_registry = build_static_registry();

    let mut flagged_cards = Vec::new();
    let mut finding_counts: HashMap<String, usize> = HashMap::new();
    let mut total_audited = 0;

    for (key, face) in card_db.face_iter() {
        if !is_card_supported(face, &trigger_registry, &static_registry) {
            continue;
        }
        total_audited += 1;

        let oracle_text = match &face.oracle_text {
            Some(text) if !text.is_empty() => text.clone(),
            _ => continue,
        };

        let findings = audit_card_lines(&oracle_text, face);

        if !findings.is_empty() {
            for finding in &findings {
                *finding_counts
                    .entry(finding.category_name().to_string())
                    .or_default() += 1;
            }
            flagged_cards.push(SemanticAuditCard {
                card_name: key.to_string(),
                findings,
            });
        }
    }

    flagged_cards.sort_by_key(|c| std::cmp::Reverse(c.findings.len()));

    SemanticAuditSummary {
        total_supported_audited: total_audited,
        cards_with_findings: flagged_cards.len(),
        finding_counts,
        flagged_cards,
    }
}
// ---------------------------------------------------------------------------
// Shared utility functions for semantic audit
// ---------------------------------------------------------------------------

/// Check if an ability definition has a pump effect matching the given P/T values.
/// Checks `Effect::Pump`, `Effect::PumpAll`, and `Effect::GenericEffect` with
/// `AddPower`/`AddToughness` continuous modifications.
fn pump_matches_oracle(
    def: &AbilityDefinition,
    expected_power: i32,
    expected_toughness: i32,
) -> bool {
    fn pt_matches(power: &PtValue, toughness: &PtValue, ep: i32, et: i32) -> bool {
        let p_match = match power {
            PtValue::Fixed(v) => *v == ep,
            _ => true, // Dynamic quantities can't be checked statically
        };
        let t_match = match toughness {
            PtValue::Fixed(v) => *v == et,
            _ => true,
        };
        p_match && t_match
    }

    match &*def.effect {
        Effect::Pump {
            power, toughness, ..
        }
        | Effect::PumpAll {
            power, toughness, ..
        } if pt_matches(power, toughness, expected_power, expected_toughness) => {
            return true;
        }
        Effect::GenericEffect {
            static_abilities, ..
        } if static_has_pump_modification(static_abilities, expected_power, expected_toughness) => {
            return true;
        }
        _ => {}
    }
    false
}

/// Check if any static ability has AddPower/AddToughness modifications matching the given P/T.
fn static_has_pump_modification(
    statics: &[StaticDefinition],
    expected_power: i32,
    expected_toughness: i32,
) -> bool {
    for stat in statics {
        let mut power_match = expected_power == 0;
        let mut tough_match = expected_toughness == 0;
        for modif in &stat.modifications {
            match modif {
                ContinuousModification::AddPower { value } if *value == expected_power => {
                    power_match = true;
                }
                ContinuousModification::AddToughness { value } if *value == expected_toughness => {
                    tough_match = true;
                }
                // Dynamic P/T (e.g., "for each" pumps) satisfies any expected magnitude —
                // the actual value is resolved at runtime from game state.
                ContinuousModification::AddDynamicPower { .. } => {
                    power_match = true;
                }
                ContinuousModification::AddDynamicToughness { .. } => {
                    tough_match = true;
                }
                _ => {}
            }
        }
        if power_match && tough_match {
            return true;
        }
    }
    false
}

/// Extract a +N/+M or -N/-M modifier from Oracle text. Returns (power, toughness) as i32.
fn extract_pt_modifier(lower: &str) -> Option<(i32, i32)> {
    // Match patterns like "+2/+1", "-1/-1", "+0/+3"
    let idx = lower
        .find("+0/")
        .or_else(|| lower.find("+1/"))
        .or_else(|| lower.find("+2/"))
        .or_else(|| lower.find("+3/"))
        .or_else(|| lower.find("+4/"))
        .or_else(|| lower.find("+5/"))
        .or_else(|| lower.find("-1/"))
        .or_else(|| lower.find("-2/"))
        .or_else(|| lower.find("-3/"))
        .or_else(|| lower.find("-4/"))
        .or_else(|| lower.find("-5/"))?;

    let rest = &lower[idx..];
    let mut chars = rest.chars();
    let sign1 = chars.next()?;
    let power_str: String = chars.by_ref().take_while(|c| c.is_ascii_digit()).collect();
    let power: i32 = power_str.parse().ok()?;
    let power = if sign1 == '-' { -power } else { power };

    let sign2 = chars.next()?;
    if sign2 != '+' && sign2 != '-' {
        return None;
    }
    let tough_str: String = chars.take_while(|c| c.is_ascii_digit()).collect();
    let toughness: i32 = tough_str.parse().ok()?;
    let toughness = if sign2 == '-' { -toughness } else { toughness };

    Some((power, toughness))
}

/// Returns true if the Oracle line's +N/+M pattern refers to counters rather than a pump effect.
fn is_counter_reference(lower: &str) -> bool {
    if lower.contains("counter") {
        if let Some(idx) = lower.find('+').or_else(|| lower.find('-')) {
            let rest = &lower[idx..];
            let after_pattern = rest.find('/').map(|slash| {
                let after_slash = &rest[slash + 1..];
                let digits_end = after_slash
                    .find(|c: char| !c.is_ascii_digit() && c != '+' && c != '-')
                    .unwrap_or(after_slash.len());
                &after_slash[digits_end..]
            });
            if let Some(after) = after_pattern {
                let trimmed = after.trim_start();
                if trimmed.starts_with("counter") {
                    return true;
                }
            }
        }
    }
    if lower.contains("in the form of ") {
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// Per-line structural audit — matches each Oracle line to its parsed element
// and checks that specific element for expected properties.
// ---------------------------------------------------------------------------

/// A parsed element that can be matched to an Oracle line via its description.
enum ParsedElement<'a> {
    Ability(&'a AbilityDefinition),
    Trigger(&'a TriggerDefinition),
    Static(&'a StaticDefinition),
    Replacement(&'a ReplacementDefinition),
}

impl<'a> ParsedElement<'a> {
    fn description_lower(&self) -> Option<String> {
        match self {
            ParsedElement::Ability(a) => a.description.as_ref().map(|d| d.to_lowercase()),
            ParsedElement::Trigger(t) => {
                // Prefer the trigger's execute description, fall back to trigger description
                t.execute
                    .as_ref()
                    .and_then(|e| e.description.as_ref())
                    .or(t.description.as_ref())
                    .map(|d| d.to_lowercase())
            }
            ParsedElement::Static(s) => s.description.as_ref().map(|d| d.to_lowercase()),
            ParsedElement::Replacement(r) => r.description.as_ref().map(|d| d.to_lowercase()),
        }
    }

    /// Check if this element (or any nested ability) has a condition set.
    /// For abilities, also checks `activation_restrictions` for `RequiresCondition`
    /// entries (e.g., "activate only if you control an Island").
    fn has_condition(&self) -> bool {
        match self {
            ParsedElement::Ability(a) => ability_tree_any(a, &|d| {
                d.condition.is_some()
                    || d.activation_restrictions
                        .iter()
                        .any(|r| matches!(r, ActivationRestriction::RequiresCondition { .. }))
            }),
            ParsedElement::Trigger(t) => {
                t.condition.is_some()
                    || t.execute
                        .as_ref()
                        .is_some_and(|e| ability_tree_any(e, &|d| d.condition.is_some()))
            }
            ParsedElement::Static(s) => s.condition.is_some(),
            ParsedElement::Replacement(r) => {
                r.condition.is_some()
                    || r.execute
                        .as_ref()
                        .is_some_and(|e| ability_tree_any(e, &|d| d.condition.is_some()))
            }
        }
    }

    /// Check if this element (or any nested ability) has a duration set.
    fn has_duration(&self) -> bool {
        match self {
            ParsedElement::Ability(a) => ability_tree_any(a, &|d| d.duration.is_some()),
            ParsedElement::Trigger(t) => t
                .execute
                .as_ref()
                .is_some_and(|e| ability_tree_any(e, &|d| d.duration.is_some())),
            ParsedElement::Static(s) => s.condition.is_some(), // ForAsLongAs uses condition
            ParsedElement::Replacement(_) => false,
        }
    }

    /// Check if this element has a pump effect matching the given P/T.
    fn has_pump(&self, power: i32, toughness: i32) -> bool {
        match self {
            ParsedElement::Ability(a) => {
                ability_tree_any(a, &|d| pump_matches_oracle(d, power, toughness))
            }
            ParsedElement::Trigger(t) => t.execute.as_ref().is_some_and(|e| {
                ability_tree_any(e, &|d| pump_matches_oracle(d, power, toughness))
            }),
            ParsedElement::Static(s) => {
                static_has_pump_modification(std::slice::from_ref(s), power, toughness)
            }
            ParsedElement::Replacement(r) => r.execute.as_ref().is_some_and(|e| {
                ability_tree_any(e, &|d| pump_matches_oracle(d, power, toughness))
            }),
        }
    }

    /// Check if this element has a counter effect matching the given type.
    fn has_counter_effect(&self, counter_type: &str) -> bool {
        let counter_pred = |def: &AbilityDefinition| -> bool {
            matches!(
                &*def.effect,
                Effect::PutCounter { counter_type: ct, .. }
                | Effect::PutCounterAll { counter_type: ct, .. }
                if ct == counter_type
            )
        };
        match self {
            ParsedElement::Ability(a) => ability_tree_any(a, &counter_pred),
            ParsedElement::Trigger(t) => t
                .execute
                .as_ref()
                .is_some_and(|e| ability_tree_any(e, &counter_pred)),
            ParsedElement::Static(_) => false,
            ParsedElement::Replacement(r) => r
                .execute
                .as_ref()
                .is_some_and(|e| ability_tree_any(e, &counter_pred)),
        }
    }

    /// Check if this element has an "unless" payment (Counter with unless_payment, or
    /// trigger-level unless_pay).
    fn has_unless(&self) -> bool {
        let unless_pred = |d: &AbilityDefinition| -> bool {
            matches!(
                &*d.effect,
                Effect::Counter { unless_payment, .. } if unless_payment.is_some()
            )
        };
        match self {
            ParsedElement::Ability(a) => ability_tree_any(a, &unless_pred),
            ParsedElement::Trigger(t) => {
                t.unless_pay.is_some()
                    || t.execute
                        .as_ref()
                        .is_some_and(|e| ability_tree_any(e, &unless_pred))
            }
            ParsedElement::Static(_) | ParsedElement::Replacement(_) => false,
        }
    }
}

/// Normalize Oracle text for description matching: replace card-name self-references
/// with `~` so they match parsed descriptions (which use `~` normalization).
fn normalize_for_matching(lower: &str, card_name_lower: &str) -> String {
    // Replace the full card name (or comma-truncated form) with ~
    let mut result = lower.to_string();
    if !card_name_lower.is_empty() {
        // Try full name first, then comma-truncated (e.g. "Akiri, Line-Slinger" → "Akiri")
        result = result.replace(card_name_lower, "~");
        if let Some(short) = card_name_lower.split(',').next() {
            let short = short.trim();
            if short.len() > 2 {
                result = result.replace(short, "~");
            }
        }
    }
    // Normalize common self-reference phrases to ~
    for phrase in &[
        "this creature",
        "this permanent",
        "this enchantment",
        "this artifact",
        "this land",
        "this spell",
        "this aura",
    ] {
        result = result.replace(phrase, "~");
    }
    result
}

/// Per-line audit of a single card: match Oracle lines to parsed elements and check properties.
fn audit_card_lines(oracle_text: &str, face: &CardFace) -> Vec<SemanticFinding> {
    let mut findings = Vec::new();
    let card_name_lower = face.name.to_lowercase();

    // Build the pool of parsed elements
    let mut elements: Vec<ParsedElement<'_>> = Vec::new();
    for a in &face.abilities {
        elements.push(ParsedElement::Ability(a));
        for mode_ab in &a.mode_abilities {
            elements.push(ParsedElement::Ability(mode_ab));
        }
    }
    for t in &face.triggers {
        elements.push(ParsedElement::Trigger(t));
        if let Some(exec) = &t.execute {
            for mode_ab in &exec.mode_abilities {
                elements.push(ParsedElement::Ability(mode_ab));
            }
        }
    }
    for s in &face.static_abilities {
        elements.push(ParsedElement::Static(s));
    }
    for r in &face.replacements {
        elements.push(ParsedElement::Replacement(r));
    }

    for line in oracle_text
        .split('\n')
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
    {
        let stripped = strip_parenthesized_reminder(line);
        let stripped = stripped.trim();
        if stripped.is_empty() {
            continue;
        }
        let lower = stripped.to_lowercase();

        // Skip very short lines (single keywords, type lines)
        if lower.len() < 5 {
            continue;
        }

        // Normalize self-references for matching
        let norm = normalize_for_matching(&lower, &card_name_lower);

        // --- Match this line to parsed element(s) ---
        let matched: Vec<&ParsedElement<'_>> = elements
            .iter()
            .filter(|e| {
                if let Some(desc) = e.description_lower() {
                    // Try both raw and normalized matching
                    desc.contains(&lower)
                        || lower.contains(desc.as_str())
                        || desc.contains(&norm)
                        || norm.contains(desc.as_str())
                } else {
                    false
                }
            })
            .collect();

        // Also check if this line is covered by keywords, casting restrictions, or
        // other non-ability structured data
        let covered_by_keyword = face.keywords.iter().any(|k| {
            let kw_name = format!("{k:?}").to_lowercase();
            lower.starts_with(&kw_name)
        }) || is_keyword_line(&lower);
        let covered_by_casting =
            !face.casting_restrictions.is_empty() && lower.starts_with("cast this spell only ");
        let covered_by_additional_cost =
            face.additional_cost.is_some() && lower.starts_with("as an additional cost ");
        // Enchant keyword lines ("Enchant creature", "Enchant land you control")
        let covered_by_enchant = lower.starts_with("enchant ");
        // Replacement effects with matching descriptions (enter-tapped, etc.)
        let covered_by_replacement = face.replacements.iter().any(|r| {
            r.description.as_ref().is_some_and(|d| {
                let dl = d.to_lowercase();
                let dn = normalize_for_matching(&dl, &card_name_lower);
                dl.contains(&lower)
                    || lower.contains(dl.as_str())
                    || dn.contains(&lower)
                    || lower.contains(dn.as_str())
                    || dl.contains(&norm)
                    || norm.contains(dl.as_str())
            })
        });

        if matched.is_empty()
            && !covered_by_keyword
            && !covered_by_casting
            && !covered_by_additional_cost
            && !covered_by_enchant
            && !covered_by_replacement
        {
            // Unmatched line → SilentDrop (only for substantive lines)
            if lower.len() > 20 {
                findings.push(SemanticFinding::SilentDrop {
                    oracle_line: line.to_string(),
                });
            }
            continue;
        }

        // --- Check matched element(s) for expected properties ---
        // Use the FIRST matched element for property checks (most specific match).
        // If multiple match, any having the property is sufficient.

        // 1. Condition check: does Oracle text contain condition language?
        if let Some(cond_label) = line_has_condition_text(&lower) {
            let any_has_condition = matched.iter().any(|e| e.has_condition() || e.has_unless());
            if !any_has_condition && !covered_by_casting {
                findings.push(SemanticFinding::DroppedCondition {
                    oracle_line: line.to_string(),
                    condition_text: cond_label.to_string(),
                });
            }
        }

        // 2. Duration check: does Oracle text contain duration language?
        if let Some(dur_label) = line_has_duration_text(&lower) {
            let any_has_duration = matched.iter().any(|e| e.has_duration());
            if !any_has_duration {
                findings.push(SemanticFinding::DroppedDuration {
                    oracle_line: line.to_string(),
                    duration_text: dur_label.to_string(),
                });
            }
        }

        // 3. P/T parameter check: does Oracle text contain +N/+M that should be a pump or counter?
        let stripped_for_pt = strip_parenthesized_reminder(line);
        let lower_for_pt = stripped_for_pt.to_lowercase();
        if let Some((power, toughness)) = extract_pt_modifier(&lower_for_pt) {
            if power == 0 && toughness == 0 {
                // +0/+0 is meaningless, skip
            } else if is_counter_reference(&lower_for_pt) {
                let normalized =
                    crate::parser::oracle_effect::counter::normalize_counter_type(&format!(
                        "{}{}/{}{}",
                        if power >= 0 { "+" } else { "" },
                        power,
                        if toughness >= 0 { "+" } else { "" },
                        toughness
                    ));
                let any_has_counter = matched.iter().any(|e| e.has_counter_effect(&normalized));
                if !any_has_counter {
                    findings.push(SemanticFinding::WrongParameter {
                        oracle_line: line.to_string(),
                        field: "counter".to_string(),
                        expected: format!(
                            "{}{}/{}{}",
                            if power >= 0 { "+" } else { "" },
                            power,
                            if toughness >= 0 { "+" } else { "" },
                            toughness
                        ) + " counter",
                        actual: "no matching counter effect on this line's element".to_string(),
                    });
                }
            } else {
                let any_has_pump = matched.iter().any(|e| e.has_pump(power, toughness));
                if !any_has_pump {
                    findings.push(SemanticFinding::WrongParameter {
                        oracle_line: line.to_string(),
                        field: "pump".to_string(),
                        expected: format!(
                            "{}{}/{}{}",
                            if power >= 0 { "+" } else { "" },
                            power,
                            if toughness >= 0 { "+" } else { "" },
                            toughness,
                        ),
                        actual: "no matching pump effect on this line's element".to_string(),
                    });
                }
            }
        }

        // 4. Unimplemented stubs in matched elements
        for elem in &matched {
            if let ParsedElement::Ability(def) = elem {
                collect_unimplemented_from_tree(def, line, &mut findings);
            }
            if let ParsedElement::Trigger(t) = elem {
                if let Some(exec) = &t.execute {
                    collect_unimplemented_from_tree(exec, line, &mut findings);
                }
            }
            if let ParsedElement::Replacement(r) = elem {
                if let Some(exec) = &r.execute {
                    collect_unimplemented_from_tree(exec, line, &mut findings);
                }
            }
        }
    }

    findings
}

/// Check if an Oracle line contains condition language, returning the label if so.
/// Applies exclusion filters for patterns that aren't true ability conditions.
fn line_has_condition_text(lower: &str) -> Option<&'static str> {
    let condition_phrases: &[(&str, &str)] = &[
        ("if ", "if"),
        ("as long as ", "as long as"),
        ("unless ", "unless"),
    ];

    for &(phrase, label) in condition_phrases {
        if !lower.contains(phrase) {
            continue;
        }

        // Exclusions: patterns that look like conditions but aren't ability conditions
        if lower.contains("if able")
            || lower.starts_with("as long as ")
            || lower.contains("if you do")
            || lower.contains("if you don't")
            || lower.contains("was kicked")
            || lower.contains("is kicked")
            || (lower.starts_with("choose ") && lower.contains("if "))
            || lower.contains("if it's not your turn")
            || lower.contains("if it's your turn")
            || lower.contains("if no other ")
            || lower.contains("if no creatures ")
            // Replacement effect patterns (not ability conditions):
            // "if X would Y, Z instead" is the canonical CR 614.1a replacement structure.
            || (lower.contains(" would ") && lower.contains(" instead"))
            || (lower.contains("if you search") && lower.contains("shuffle"))
            || lower.contains("if a land is tapped for mana")
            || lower.contains("if a player would begin")
        {
            continue;
        }

        return Some(label);
    }

    None
}

/// Check if a line is a standalone keyword ability line (may be comma-separated).
/// Covers common keywords that don't always match the Keyword enum's Debug format.
fn is_keyword_line(lower: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "flying",
        "first strike",
        "double strike",
        "vigilance",
        "trample",
        "deathtouch",
        "lifelink",
        "haste",
        "reach",
        "menace",
        "hexproof",
        "indestructible",
        "flash",
        "defender",
        "prowess",
        "protection from ",
        "ward",
        "changeling",
        "partner",
        "shroud",
        "fear",
        "intimidate",
        "skulk",
        "shadow",
        "horsemanship",
        "flanking",
        "rampage ",
        "bushido ",
        "cumulative upkeep ",
        "affinity for ",
        "convoke",
        "delve",
        "improvise",
        "cascade",
        "mutate ",
        "infect",
        "wither",
        "undying",
        "persist",
        "devoid",
        "unleash",
        "extort",
        "dredge ",
        "suspend ",
    ];
    // Check if the line starts with any keyword (possibly comma-separated list)
    let trimmed = lower.trim().trim_end_matches('.');
    KEYWORDS
        .iter()
        .any(|kw| trimmed.starts_with(kw) || trimmed == kw.trim())
}

/// Check if an Oracle line contains duration language, returning the label if so.
fn line_has_duration_text(lower: &str) -> Option<&'static str> {
    let duration_phrases: &[(&str, &str)] = &[
        ("until end of turn", "until end of turn"),
        ("until your next turn", "until your next turn"),
        ("for as long as ", "for as long as"),
        ("until end of combat", "until end of combat"),
    ];
    for &(phrase, label) in duration_phrases {
        if lower.contains(phrase) {
            return Some(label);
        }
    }
    None
}

/// Recursively collect Unimplemented stubs from an ability tree.
fn collect_unimplemented_from_tree(
    def: &AbilityDefinition,
    oracle_line: &str,
    findings: &mut Vec<SemanticFinding>,
) {
    // Use ability_tree_any to traverse, but we need to collect (not just detect).
    // Walk manually for collection.
    if let Effect::Unimplemented {
        name, description, ..
    } = &*def.effect
    {
        let desc = description.as_deref().unwrap_or(name.as_str()).to_string();
        findings.push(SemanticFinding::UnimplementedSubEffect {
            oracle_line: oracle_line.to_string(),
            stub_description: desc,
        });
    }
    if let Some(AbilityCost::Unimplemented { description }) = &def.cost {
        findings.push(SemanticFinding::UnimplementedSubEffect {
            oracle_line: oracle_line.to_string(),
            stub_description: format!("Cost: {description}"),
        });
    }
    if let Some(ref sub) = def.sub_ability {
        collect_unimplemented_from_tree(sub, oracle_line, findings);
    }
    if let Some(ref else_ab) = def.else_ability {
        collect_unimplemented_from_tree(else_ab, oracle_line, findings);
    }
    for mode_ab in &def.mode_abilities {
        collect_unimplemented_from_tree(mode_ab, oracle_line, findings);
    }
}

/// Generate a markdown summary string from a `SemanticAuditSummary`.
pub fn format_semantic_audit_markdown(summary: &SemanticAuditSummary) -> String {
    let mut md = String::new();
    md.push_str("## Semantic Audit Summary\n\n");
    md.push_str(&format!(
        "- **Total supported cards audited:** {}\n",
        summary.total_supported_audited
    ));
    md.push_str(&format!(
        "- **Cards with findings:** {}\n",
        summary.cards_with_findings
    ));
    md.push_str("\n### Finding Counts by Category\n\n");
    md.push_str("| Category | Count |\n|----------|-------|\n");

    let mut sorted_counts: Vec<_> = summary.finding_counts.iter().collect();
    sorted_counts.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
    for (category, count) in &sorted_counts {
        md.push_str(&format!("| {category} | {count} |\n"));
    }

    // Top 20 most common finding patterns
    md.push_str("\n### Top 20 Finding Patterns\n\n");

    // Group findings by (category, description pattern)
    let mut pattern_freq: HashMap<String, (usize, Vec<String>)> = HashMap::new();
    for card in &summary.flagged_cards {
        for finding in &card.findings {
            let pattern_key = match finding {
                SemanticFinding::WrongAbilityType {
                    expected, actual, ..
                } => {
                    format!("WrongAbilityType: expected={expected}, actual={actual}")
                }
                SemanticFinding::UnimplementedSubEffect {
                    stub_description, ..
                } => {
                    format!("UnimplementedSubEffect: {stub_description}")
                }
                SemanticFinding::DroppedCondition { condition_text, .. } => {
                    format!("DroppedCondition: {condition_text}")
                }
                SemanticFinding::DroppedDuration { duration_text, .. } => {
                    format!("DroppedDuration: {duration_text}")
                }
                SemanticFinding::WrongParameter { field, .. } => {
                    format!("WrongParameter: {field}")
                }
                SemanticFinding::SilentDrop { .. } => "SilentDrop".to_string(),
            };
            let entry = pattern_freq
                .entry(pattern_key)
                .or_insert_with(|| (0, Vec::new()));
            entry.0 += 1;
            if entry.1.len() < 3 {
                entry.1.push(card.card_name.clone());
            }
        }
    }

    let mut patterns: Vec<_> = pattern_freq.into_iter().collect();
    patterns.sort_by_key(|(_, (count, _))| std::cmp::Reverse(*count));

    md.push_str("| Pattern | Count | Example Cards |\n|---------|-------|---------------|\n");
    for (pattern, (count, examples)) in patterns.iter().take(20) {
        let examples_str = examples.join(", ");
        md.push_str(&format!("| {pattern} | {count} | {examples_str} |\n"));
    }

    // Example cards for each category (3 each)
    md.push_str("\n### Example Cards by Category\n\n");
    let categories = [
        "WrongAbilityType",
        "UnimplementedSubEffect",
        "DroppedCondition",
        "DroppedDuration",
        "WrongParameter",
        "SilentDrop",
    ];
    for category in &categories {
        let examples: Vec<&str> = summary
            .flagged_cards
            .iter()
            .filter(|c| c.findings.iter().any(|f| f.category_name() == *category))
            .take(3)
            .map(|c| c.card_name.as_str())
            .collect();
        if !examples.is_empty() {
            md.push_str(&format!("**{category}:** {}\n\n", examples.join(", ")));
        }
    }

    md
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::legality::{legalities_to_export_map, LegalityStatus};
    use crate::types::ability::{AbilityKind, Effect, TargetFilter};
    use crate::types::card_type::CardType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::zones::Zone;

    fn make_obj() -> GameObject {
        GameObject::new(
            ObjectId(1),
            CardId(1),
            PlayerId(0),
            "Test Card".to_string(),
            Zone::Battlefield,
        )
    }

    #[test]
    fn vanilla_object_has_no_unimplemented_mechanics() {
        let obj = make_obj();
        assert!(unimplemented_mechanics(&obj).is_empty());
    }

    #[test]
    fn object_with_known_keyword_has_no_unimplemented() {
        let mut obj = make_obj();
        obj.keywords.push(Keyword::Flying);
        obj.keywords.push(Keyword::Haste);
        assert!(unimplemented_mechanics(&obj).is_empty());
    }

    #[test]
    fn object_with_unknown_keyword_has_unimplemented() {
        let mut obj = make_obj();
        obj.keywords
            .push(Keyword::Unknown("FutureKeyword".to_string()));
        assert!(!unimplemented_mechanics(&obj).is_empty());
    }

    #[test]
    fn object_with_registered_ability_has_no_unimplemented() {
        let mut obj = make_obj();
        obj.abilities
            .push(crate::types::ability::AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 3 },
                    target: TargetFilter::Any,
                    damage_source: None,
                },
            ));
        assert!(unimplemented_mechanics(&obj).is_empty());
    }

    #[test]
    fn object_with_unregistered_ability_has_unimplemented() {
        let mut obj = make_obj();
        obj.abilities
            .push(crate::types::ability::AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Unimplemented {
                    name: "Fateseal".to_string(),
                    description: None,
                },
            ));
        assert!(!unimplemented_mechanics(&obj).is_empty());
    }

    #[test]
    fn has_unimplemented_via_game_object_method() {
        let mut obj = make_obj();
        assert!(!obj.has_unimplemented_mechanics());
        obj.keywords.push(Keyword::Unknown("Bogus".to_string()));
        assert!(obj.has_unimplemented_mechanics());
    }

    fn make_face() -> CardFace {
        CardFace {
            name: "Test Card".to_string(),
            mana_cost: Default::default(),
            card_type: CardType::default(),
            power: None,
            toughness: None,
            loyalty: None,
            defense: None,
            oracle_text: None,
            non_ability_text: None,
            flavor_name: None,
            keywords: vec![],
            abilities: vec![],
            triggers: vec![],
            static_abilities: vec![],
            replacements: vec![],
            color_override: None,
            scryfall_oracle_id: None,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: vec![],
            casting_options: vec![],
            solve_condition: None,
            brawl_commander: false,
            metadata: Default::default(),
        }
    }

    #[test]
    fn card_face_with_nested_mode_unimplemented_is_detected() {
        let mut face = make_face();
        face.abilities.push(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Unimplemented {
                    name: "modal".to_string(),
                    description: None,
                },
            )
            .with_modal(
                crate::types::ability::ModalChoice {
                    min_choices: 1,
                    max_choices: 1,
                    mode_count: 1,
                    mode_descriptions: vec!["Mode".to_string()],
                    ..Default::default()
                },
                vec![AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Unimplemented {
                        name: "nested".to_string(),
                        description: None,
                    },
                )],
            ),
        );

        assert!(card_face_has_unimplemented_parts(&face));
    }

    #[test]
    fn card_face_with_unimplemented_additional_cost_is_detected() {
        let mut face = make_face();
        face.additional_cost = Some(AdditionalCost::Optional(AbilityCost::Unimplemented {
            description: "mystery cost".to_string(),
        }));

        assert!(card_face_has_unimplemented_parts(&face));
    }

    #[test]
    fn card_face_with_replacement_decline_unimplemented_is_detected() {
        let mut face = make_face();
        face.replacements
            .push(ReplacementDefinition::new(ReplacementEvent::Draw).mode(
                ReplacementMode::Optional {
                    decline: Some(Box::new(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::Unimplemented {
                            name: "decline".to_string(),
                            description: None,
                        },
                    ))),
                },
            ));

        assert!(card_face_has_unimplemented_parts(&face));
    }

    #[test]
    fn analyze_coverage_reports_legality_based_format_totals() {
        let supported = serde_json::json!({
            "alpha": {
                "name": "Alpha",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": [], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": legalities_to_export_map(&HashMap::from([
                    (LegalityFormat::Standard, LegalityStatus::Legal),
                    (LegalityFormat::Modern, LegalityStatus::Legal),
                ])),
            },
            "beta": {
                "name": "Beta",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": [], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [{
                    "kind": "Spell",
                    "effect": { "type": "Unimplemented", "name": "beta_gap", "description": null },
                    "cost": null,
                    "sub_ability": null,
                    "duration": null,
                    "description": null,
                    "target_prompt": null,
                    "sorcery_speed": false,
                    "condition": null,
                    "optional_targeting": false
                }],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": legalities_to_export_map(&HashMap::from([
                    (LegalityFormat::Standard, LegalityStatus::Legal),
                    (LegalityFormat::Commander, LegalityStatus::Legal),
                ])),
            }
        })
        .to_string();

        let db = CardDatabase::from_json_str(&supported).expect("test export should deserialize");
        let summary = analyze_coverage(&db);

        assert_eq!(summary.total_cards, 2);
        assert_eq!(summary.supported_cards, 1);
        assert_eq!(
            summary.coverage_by_format.get("standard"),
            Some(&FormatCoverageSummary {
                total_cards: 2,
                supported_cards: 1,
                coverage_pct: 50.0,
            })
        );
        assert_eq!(
            summary.coverage_by_format.get("modern"),
            Some(&FormatCoverageSummary {
                total_cards: 1,
                supported_cards: 1,
                coverage_pct: 100.0,
            })
        );
        assert_eq!(
            summary.coverage_by_format.get("commander"),
            Some(&FormatCoverageSummary {
                total_cards: 1,
                supported_cards: 0,
                coverage_pct: 0.0,
            })
        );

        // Verify gap_details on the unsupported card
        let beta = summary
            .cards
            .iter()
            .find(|c| c.card_name == "Beta")
            .unwrap();
        assert!(!beta.supported);
        assert_eq!(beta.gap_count, 1);
        assert_eq!(beta.gap_details[0].handler, "Effect:beta_gap");
    }

    // -----------------------------------------------------------------------
    // normalize_oracle_pattern tests
    // -----------------------------------------------------------------------

    #[test]
    fn normalize_replaces_digits_with_n() {
        assert_eq!(normalize_oracle_pattern("deals 3 damage"), "deals N damage");
    }

    #[test]
    fn normalize_replaces_mana_symbols() {
        assert_eq!(normalize_oracle_pattern("{2}{W}{U}"), "{N}{M}{M}");
    }

    #[test]
    fn normalize_replaces_hybrid_mana() {
        assert_eq!(normalize_oracle_pattern("{G/W}{B/P}"), "{M/M}{M/P}");
    }

    #[test]
    fn normalize_replaces_pt_modifiers() {
        assert_eq!(
            normalize_oracle_pattern("gets +2/+1 until"),
            "gets +N/+N until"
        );
        assert_eq!(normalize_oracle_pattern("gets -1/-1"), "gets +N/+N");
    }

    #[test]
    fn normalize_trims_trailing_period() {
        assert_eq!(normalize_oracle_pattern("Draw a card."), "draw a card");
    }

    #[test]
    fn normalize_collapses_whitespace() {
        assert_eq!(
            normalize_oracle_pattern("target   creature   gets"),
            "target creature gets"
        );
    }

    #[test]
    fn normalize_complex_oracle_text() {
        assert_eq!(
            normalize_oracle_pattern("Target creature gets +3/+3 and deals 2 damage."),
            "target creature gets +N/+N and deals N damage"
        );
    }

    #[test]
    fn normalize_preserves_non_mana_braces() {
        // Generic brace content that isn't a recognized mana symbol
        assert_eq!(normalize_oracle_pattern("{T}: Add {G}"), "{t}: add {M}");
    }

    // -----------------------------------------------------------------------
    // extract_gap_details tests
    // -----------------------------------------------------------------------

    #[test]
    fn extract_gap_details_from_unsupported_ability() {
        let items = vec![ParsedItem {
            category: ParseCategory::Ability,
            label: "unknown".to_string(),
            source_text: Some("exile target creature".to_string()),
            supported: false,
            details: vec![],
            children: vec![],
        }];
        let gaps = extract_gap_details(&items);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].handler, "Effect:unknown");
        assert_eq!(
            gaps[0].source_text.as_deref(),
            Some("exile target creature")
        );
    }

    #[test]
    fn extract_gap_details_deduplicates_by_handler() {
        let items = vec![
            ParsedItem {
                category: ParseCategory::Ability,
                label: "unknown".to_string(),
                source_text: Some("first line".to_string()),
                supported: false,
                details: vec![],
                children: vec![],
            },
            ParsedItem {
                category: ParseCategory::Ability,
                label: "unknown".to_string(),
                source_text: Some("second line".to_string()),
                supported: false,
                details: vec![],
                children: vec![],
            },
        ];
        let gaps = extract_gap_details(&items);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].source_text.as_deref(), Some("first line"));
    }

    #[test]
    fn extract_gap_details_recurses_into_replacement_children() {
        let items = vec![ParsedItem {
            category: ParseCategory::Replacement,
            label: "EntersBattlefield".to_string(),
            source_text: None,
            supported: true,
            details: vec![],
            children: vec![ParsedItem {
                category: ParseCategory::Ability,
                label: "unknown".to_string(),
                source_text: Some("do something".to_string()),
                supported: false,
                details: vec![],
                children: vec![],
            }],
        }];
        let gaps = extract_gap_details(&items);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].handler, "Effect:unknown");
    }

    #[test]
    fn extract_gap_details_skips_supported_items() {
        let items = vec![ParsedItem {
            category: ParseCategory::Keyword,
            label: "Flying".to_string(),
            source_text: None,
            supported: true,
            details: vec![],
            children: vec![],
        }];
        let gaps = extract_gap_details(&items);
        assert!(gaps.is_empty());
    }

    #[test]
    fn extract_gap_details_categories() {
        let items = vec![
            ParsedItem {
                category: ParseCategory::Keyword,
                label: "Bogus".to_string(),
                source_text: None,
                supported: false,
                details: vec![],
                children: vec![],
            },
            ParsedItem {
                category: ParseCategory::Trigger,
                label: "ChangesZone".to_string(),
                source_text: Some("when this enters".to_string()),
                supported: false,
                details: vec![],
                children: vec![],
            },
            ParsedItem {
                category: ParseCategory::Static,
                label: "Prevention".to_string(),
                source_text: None,
                supported: false,
                details: vec![],
                children: vec![],
            },
            ParsedItem {
                category: ParseCategory::Cost,
                label: "sacrifice a creature".to_string(),
                source_text: Some("sacrifice a creature".to_string()),
                supported: false,
                details: vec![],
                children: vec![],
            },
        ];
        let gaps = extract_gap_details(&items);
        assert_eq!(gaps.len(), 4);
        assert_eq!(gaps[0].handler, "Keyword:Bogus");
        assert_eq!(gaps[1].handler, "Trigger:ChangesZone");
        assert_eq!(gaps[2].handler, "Static:Prevention");
        assert_eq!(gaps[3].handler, "Cost:sacrifice a creature");
    }

    #[test]
    fn generic_effect_label_shows_static_modes() {
        use crate::types::ability::ContinuousModification;

        let def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GenericEffect {
                static_abilities: vec![StaticDefinition {
                    mode: StaticMode::MustBeBlocked,
                    affected: None,
                    modifications: vec![ContinuousModification::AddStaticMode {
                        mode: StaticMode::MustBeBlocked,
                    }],
                    condition: None,
                    affected_zone: None,
                    effect_zone: None,
                    characteristic_defining: false,
                    description: None,
                }],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
        );

        let item = build_ability_item(&def);
        assert_eq!(item.label, "MustBeBlocked");
        assert!(item
            .details
            .iter()
            .any(|(k, v)| k == "grants" && v == "MustBeBlocked"));
        assert!(item
            .details
            .iter()
            .any(|(k, v)| k == "duration" && v == "until end of turn"));
    }

    #[test]
    fn generic_effect_label_shows_keyword_grants() {
        use crate::types::ability::ContinuousModification;

        let def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GenericEffect {
                static_abilities: vec![StaticDefinition {
                    mode: StaticMode::Continuous,
                    affected: None,
                    modifications: vec![
                        ContinuousModification::AddKeyword {
                            keyword: Keyword::Flying,
                        },
                        ContinuousModification::AddKeyword {
                            keyword: Keyword::Haste,
                        },
                    ],
                    condition: None,
                    affected_zone: None,
                    effect_zone: None,
                    characteristic_defining: false,
                    description: None,
                }],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
        );

        let item = build_ability_item(&def);
        assert_eq!(item.label, "grant Flying, grant Haste");
    }

    #[test]
    fn speed_quantity_features_are_extracted_and_marked_handled() {
        let mut face = CardFace::default();
        face.abilities.push(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PutCounter {
                    counter_type: "P1P1".to_string(),
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::Speed,
                    },
                    target: TargetFilter::SelfRef,
                },
            )
            .condition(AbilityCondition::HasMaxSpeed)
            .player_scope(PlayerFilter::HighestSpeed),
        );

        let mut features = HashSet::new();
        extract_card_features(&face, &mut features);

        assert!(features.contains("condition:HasMaxSpeed"));
        assert!(features.contains("player_scope:HighestSpeed"));
        assert!(features.contains("quantity_ref:Speed"));

        let handled = resolver_handled_features();
        assert!(handled.contains("condition:HasMaxSpeed"));
        assert!(handled.contains("player_scope:HighestSpeed"));
        assert!(handled.contains("quantity_ref:Speed"));
    }

    // -----------------------------------------------------------------------
    // Semantic audit tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_audit_per_line_detects_dropped_condition() {
        let mut face = make_face();
        let oracle = "Target creature gets +2/+2 as long as you control a Dragon.";
        face.oracle_text = Some(oracle.to_string());
        // Ability with NO condition set — description must match the Oracle line
        face.abilities.push(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Pump {
                    power: PtValue::Fixed(2),
                    toughness: PtValue::Fixed(2),
                    target: TargetFilter::Any,
                },
            )
            .description(oracle.to_string()),
        );

        let findings = audit_card_lines(oracle, &face);

        assert!(
            findings
                .iter()
                .any(|f| matches!(f, SemanticFinding::DroppedCondition { condition_text, .. } if condition_text == "as long as")),
            "Should detect dropped 'as long as' condition: {findings:?}"
        );
    }

    #[test]
    fn test_audit_per_line_detects_unimplemented_stub() {
        let mut face = make_face();
        let oracle = "Fateseal 2.";
        face.oracle_text = Some(oracle.to_string());
        face.abilities.push(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Unimplemented {
                    name: "Fateseal".to_string(),
                    description: Some("Fateseal 2".to_string()),
                },
            )
            .description("Fateseal 2.".to_string()),
        );

        let findings = audit_card_lines(oracle, &face);

        assert!(
            findings
                .iter()
                .any(|f| matches!(f, SemanticFinding::UnimplementedSubEffect { stub_description, .. } if stub_description == "Fateseal 2")),
            "Should detect unimplemented stub: {findings:?}"
        );
    }

    #[test]
    fn test_audit_per_line_detects_dropped_duration() {
        let mut face = make_face();
        let oracle = "Target creature gets +3/+3 until end of turn.";
        face.oracle_text = Some(oracle.to_string());
        // Ability with no duration
        face.abilities.push(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Pump {
                    power: PtValue::Fixed(3),
                    toughness: PtValue::Fixed(3),
                    target: TargetFilter::Any,
                },
            )
            .description(oracle.to_string()),
        );

        let findings = audit_card_lines(oracle, &face);

        assert!(
            findings
                .iter()
                .any(|f| matches!(f, SemanticFinding::DroppedDuration { duration_text, .. } if duration_text == "until end of turn")),
            "Should detect dropped duration: {findings:?}"
        );
    }

    #[test]
    fn test_audit_per_line_no_false_positive_when_condition_present() {
        let mut face = make_face();
        let oracle = "Draw a card if you control an artifact.";
        face.oracle_text = Some(oracle.to_string());
        face.abilities.push(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                },
            )
            .condition(AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: TargetFilter::Any,
                    },
                },
                comparator: crate::types::ability::Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            })
            .description(oracle.to_string()),
        );

        let findings = audit_card_lines(oracle, &face);

        assert!(
            !findings
                .iter()
                .any(|f| matches!(f, SemanticFinding::DroppedCondition { .. })),
            "Should not flag when condition is present: {findings:?}"
        );
    }

    #[test]
    fn test_extract_pt_modifier() {
        assert_eq!(extract_pt_modifier("gets +2/+1 until"), Some((2, 1)));
        assert_eq!(extract_pt_modifier("gets -1/-1"), Some((-1, -1)));
        assert_eq!(extract_pt_modifier("gets +0/+3"), Some((0, 3)));
        assert_eq!(extract_pt_modifier("no modifier here"), None);
    }
}
