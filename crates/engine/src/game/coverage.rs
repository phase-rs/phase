use crate::database::legality::LegalityFormat;
use crate::database::CardDatabase;
use crate::game::game_object::GameObject;
use crate::game::static_abilities::{build_static_registry, StaticAbilityHandler};
use crate::game::triggers::build_trigger_registry;
use crate::parser::oracle::is_commander_permission_sentence;
use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, ActivationRestriction,
    AdditionalCost, AggregateFunction, ChoiceType, ContinuousModification, ControllerRef,
    CountScope, DelayedTriggerCondition, DoublePTMode, Duration, Effect, FilterProp,
    GainLifePlayer, GameRestriction, ManaProduction, ObjectProperty, PlayerFilter, PtValue,
    QuantityExpr, QuantityRef, ReplacementCondition, ReplacementDefinition, ReplacementMode,
    SharedQuality, StaticCondition, StaticDefinition, TargetFilter, TriggerDefinition, TypeFilter,
    TypedFilter, ZoneRef,
};
use crate::types::card::CardFace;
use crate::types::card_type::CoreType;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;
use crate::types::phase::Phase;
use crate::types::replacements::ReplacementEvent;
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
            | StaticMode::CantBeCast { .. }
            | StaticMode::CantCastDuring { .. }
            | StaticMode::PerTurnCastLimit { .. }
            | StaticMode::PerTurnDrawLimit { .. }
            | StaticMode::GraveyardCastPermission { .. }
            | StaticMode::CastFromHandFree { .. }
            | StaticMode::CastWithKeyword { .. }
            | StaticMode::MaximumHandSize { .. }
            | StaticMode::CantBeBlockedBy { .. }
            // CR 602.5 + CR 603.2a: CantBeActivated carries `who` + `source_filter`.
            | StaticMode::CantBeActivated { .. }
            // CR 701.23 + CR 609.3: CantSearchLibrary carries `cause`.
            | StaticMode::CantSearchLibrary { .. }
            // CR 603.2g: SuppressTriggers carries `source_filter` + `events`.
            | StaticMode::SuppressTriggers { .. }
            // CR 603.2d: DoubleTriggers carries the `TriggerCause` predicate.
            | StaticMode::DoubleTriggers { .. }
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
    /// Set codes the card has been printed in (from MTGJSON `printings`).
    /// Used by the coverage dashboard to aggregate cards by set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub printings: Vec<String>,
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
    /// Per-set coverage rollup. Each card counts toward every set it was
    /// printed in (via `CardCoverageResult::printings`). Consumers that
    /// want to hide small/low-coverage sets apply their own thresholds.
    #[serde(default)]
    pub coverage_by_set: BTreeMap<String, SetCoverageSummary>,
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

/// Per-set coverage totals. Mirrors `FormatCoverageSummary` so consumers
/// can treat format- and set-level rollups uniformly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetCoverageSummary {
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
        TargetFilter::PostReplacementSourceController => {
            "prevented event source's controller".into()
        }
        TargetFilter::SpecificObject { id } => format!("object #{}", id.0),
        TargetFilter::SpecificPlayer { id } => format!("player #{}", id.0),
        TargetFilter::TrackedSet { id } => format!("tracked set #{}", id.0),
        TargetFilter::TrackedSetFiltered { id, filter } => {
            format!("tracked set #{} matching {}", id.0, fmt_target(filter))
        }
        TargetFilter::ExiledBySource => "cards exiled by source".into(),
        TargetFilter::HasChosenName => "card with the chosen name".into(),
        TargetFilter::Named { name } => format!("card named {name}"),
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
        TargetFilter::Owner => "owner".into(),
    }
}

fn fmt_typed_filter(tf: &TypedFilter) -> String {
    let mut parts = Vec::new();
    for prop in &tf.properties {
        match prop {
            FilterProp::Token => parts.push("token".into()),
            FilterProp::Attacking => parts.push("attacking".into()),
            FilterProp::AttackingController => parts.push("attacking you".into()),
            FilterProp::Blocking => parts.push("blocking".into()),
            FilterProp::Unblocked => parts.push("unblocked".into()),
            FilterProp::Tapped => parts.push("tapped".into()),
            FilterProp::Untapped => parts.push("untapped".into()),
            FilterProp::WithKeyword { value } => parts.push(format!("with {value:?}")),
            FilterProp::HasKeywordKind { value } => {
                parts.push(format!("with {value:?}").to_lowercase())
            }
            FilterProp::WithoutKeyword { value } => parts.push(format!("without {value:?}")),
            FilterProp::WithoutKeywordKind { value } => {
                parts.push(format!("without {value:?}").to_lowercase())
            }
            FilterProp::CountersGE {
                counter_type,
                count,
            } => parts.push(format!(
                "{}+ {} counters",
                fmt_quantity(count),
                counter_type.as_str()
            )),
            FilterProp::CmcGE { value } => parts.push(format!("mv {}+", fmt_quantity(value))),
            FilterProp::CmcLE { value } => parts.push(format!("mv {}-", fmt_quantity(value))),
            FilterProp::CmcEQ { value } => parts.push(format!("mv {}", fmt_quantity(value))),
            FilterProp::SameName => parts.push("same name".into()),
            FilterProp::SameNameAsParentTarget => parts.push("same name as parent target".into()),
            FilterProp::NameMatchesAnyPermanent { controller } => match controller {
                Some(c) => parts.push(format!("name matches {} permanent", fmt_controller(c))),
                None => parts.push("name matches any permanent".into()),
            },
            FilterProp::InZone { zone } => parts.push(format!("in {}", fmt_zone(zone))),
            FilterProp::Owned { controller } => parts.push(fmt_controller(controller)),
            FilterProp::EnchantedBy => parts.push("enchanted by self".into()),
            FilterProp::EquippedBy => parts.push("equipped by self".into()),
            FilterProp::HasAttachment { kind, controller } => {
                let kind_s = match kind {
                    crate::types::ability::AttachmentKind::Aura => "aura",
                    crate::types::ability::AttachmentKind::Equipment => "equipment",
                };
                match controller {
                    None => parts.push(format!("attached by {kind_s}")),
                    Some(c) => parts.push(format!("attached by {kind_s} ({})", fmt_controller(c))),
                }
            }
            FilterProp::Another => parts.push("another".into()),
            FilterProp::OtherThanTriggerObject => parts.push("other".into()),
            FilterProp::HasColor { color } => parts.push(format!("{color:?}").to_lowercase()),
            FilterProp::PowerLE { value } => parts.push(format!("power ≤{}", fmt_quantity(value))),
            FilterProp::PowerGE { value } => parts.push(format!("power ≥{}", fmt_quantity(value))),
            FilterProp::Multicolored => parts.push("multicolored".into()),
            FilterProp::HasSupertype { value } => {
                parts.push(format!("{value}").to_lowercase());
            }
            FilterProp::IsChosenCreatureType => parts.push("chosen creature type".into()),
            FilterProp::IsChosenCardType => parts.push("chosen card type".into()),
            FilterProp::NotColor { color } => {
                parts.push(format!("non-{}", format!("{color:?}").to_lowercase()));
            }
            FilterProp::NotSupertype { value } => {
                parts.push(format!("non-{}", format!("{value}").to_lowercase()));
            }
            FilterProp::Suspected => parts.push("suspected".into()),
            // CR 700.9
            FilterProp::Modified => parts.push("modified".into()),
            // CR 700.6
            FilterProp::Historic => parts.push("historic".into()),
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
            FilterProp::Colorless => parts.push("colorless".into()),
            FilterProp::IsChosenColor => parts.push("chosen color".into()),
            FilterProp::PowerGTSource => parts.push("power > source".into()),
            FilterProp::ToughnessLE { value } => {
                parts.push(format!("toughness ≤{}", fmt_quantity(value)));
            }
            FilterProp::ToughnessGE { value } => {
                parts.push(format!("toughness ≥{}", fmt_quantity(value)));
            }
            FilterProp::AnyOf { props } => {
                let inner_tf = TypedFilter::default().properties(props.clone());
                parts.push(format!("any of ({})", fmt_typed_filter(&inner_tf)));
            }
            FilterProp::HasXInManaCost => parts.push("with {X} in cost".into()),
            FilterProp::HasAnyCounter => parts.push("with one or more counters".into()),
        }
    }
    if let Some(ctrl) = &tf.controller {
        if tf.type_filters.is_empty() {
            // Player-targeting filter (e.g. "target opponent") — label as player, not permanent
            let label = match ctrl {
                ControllerRef::You => "you",
                ControllerRef::Opponent => "opponent",
                ControllerRef::TargetPlayer => "target player",
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
        ControllerRef::TargetPlayer => "target player controls",
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
        Duration::UntilControllerNextUntapStep => "until controller's next untap step",
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
        ZoneRef::Hand => "hand",
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
        QuantityRef::ObjectCountDistinctNames { filter } => {
            format!("# of distinctly-named {}", fmt_target(filter))
        }
        QuantityRef::PlayerCount { filter } => format!("# of {}", fmt_player_filter(filter)),
        QuantityRef::CountersOnSelf { counter_type } => {
            format!("{counter_type} counters on self")
        }
        QuantityRef::CountersOnTarget { counter_type } => {
            format!("{counter_type} counters on target")
        }
        QuantityRef::AnyCountersOnTarget => "counters on target (any type)".into(),
        QuantityRef::AnyCountersOnSelf => "counters on self (any type)".into(),
        QuantityRef::CountersOnObjects {
            counter_type,
            filter,
        } => match counter_type {
            Some(ct) => format!("{ct} counters on {}", fmt_target(filter)),
            None => format!("counters on {}", fmt_target(filter)),
        },
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
        QuantityRef::DistinctCardTypesInZone { zone, scope } => {
            format!(
                "card types in {} {}",
                fmt_count_scope(scope),
                fmt_zone_ref(zone)
            )
        }
        QuantityRef::DistinctCardTypesExiledBySource => {
            "card types among cards exiled with source".into()
        }
        QuantityRef::CardsExiledBySource => "cards exiled with source".into(),
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
        QuantityRef::DistinctColorsAmongPermanents { filter } => {
            format!("# of colors among {}", fmt_target(filter))
        }
        QuantityRef::PreviousEffectAmount => "amount from preceding effect".into(),
        QuantityRef::TrackedSetSize => "cards moved".into(),
        QuantityRef::ExiledFromHandThisResolution => "cards exiled from hand this way".into(),
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
        QuantityRef::NonlandPermanentsLeftBattlefieldThisTurn => {
            "nonland permanents left battlefield this turn".into()
        }
        QuantityRef::TurnsTaken => "turns taken".into(),
        QuantityRef::ChosenNumber => "chosen number".into(),
        QuantityRef::CreaturesDiedThisTurn => "creatures died this turn".into(),
        QuantityRef::AttackedThisTurn => "attacked this turn".into(),
        QuantityRef::DescendedThisTurn => "descended this turn".into(),
        QuantityRef::SpellsCastLastTurn => "spells cast last turn".into(),
        QuantityRef::OpponentLifeLostThisTurn => "opponent life lost this turn".into(),
        QuantityRef::MaxLifeLostThisTurnAcrossPlayers => {
            "max life lost this turn across players".into()
        }
        QuantityRef::CounterAddedThisTurn => "counter added this turn".into(),
        QuantityRef::OpponentDiscardedCardThisTurn => "opponent discarded card this turn".into(),
        QuantityRef::OpponentLifeTotal => "opponent life total".into(),
        QuantityRef::OpponentHandSize => "opponent hand size".into(),
        QuantityRef::DungeonsCompleted => "dungeons completed".into(),
        QuantityRef::TargetZoneCardCount { .. } => "target zone card count".into(),
        QuantityRef::CostXPaid => "X paid for this spell".into(),
        QuantityRef::ManaSpentOnTriggeringSpell => "mana spent on triggering spell".into(),
        QuantityRef::ManaSpentOnSelf => "mana spent on self".into(),
        QuantityRef::ColorsSpentOnSelf => "colors of mana spent on self".into(),
        QuantityRef::EventContextSourceCostX => "X of triggering spell".into(),
        QuantityRef::ColorsInCommandersColorIdentity => {
            "# of colors in commander's color identity".into()
        }
        QuantityRef::AttachmentsOnLeavingObject { kind, controller } => {
            let kind_s = match kind {
                crate::types::ability::AttachmentKind::Aura => "auras",
                crate::types::ability::AttachmentKind::Equipment => "equipment",
            };
            match controller {
                None => format!("# of {kind_s} attached at ltb"),
                Some(c) => format!("# of {kind_s} ({}) attached at ltb", fmt_controller(c)),
            }
        }
        QuantityRef::PlayerCounter { kind, scope } => {
            let scope_s = match scope {
                CountScope::Controller => "you have",
                CountScope::Opponents => "each opponent has",
                CountScope::All => "each player has",
            };
            format!("# of {kind} counters {scope_s}")
        }
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
        PlayerFilter::ZoneChangedThisWay => "each player who changed a card this way",
        PlayerFilter::OwnersOfCardsExiledBySource => "owners of cards exiled with source",
        PlayerFilter::TriggeringPlayer => "the triggering player",
        PlayerFilter::OpponentOtherThanTriggering => "each other opponent",
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
        ManaProduction::Fixed { colors, .. } => {
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
            ..
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
        ManaProduction::ChosenColor { count, .. } => {
            format!("{} of chosen color", fmt_quantity(count))
        }
        ManaProduction::OpponentLandColors { count } => {
            format!("{} of opponent land colors", fmt_quantity(count))
        }
        ManaProduction::AnyTypeProduceableBy { count, land_filter } => {
            format!(
                "{} of any type {} could produce",
                fmt_quantity(count),
                fmt_target(land_filter)
            )
        }
        ManaProduction::ChoiceAmongExiledColors { .. } => "1 of exiled cards' colors".into(),
        ManaProduction::ChoiceAmongCombinations { options } => {
            let rendered: Vec<String> = options
                .iter()
                .map(|combo| {
                    combo
                        .iter()
                        .map(|c| format!("{{{}}}", fmt_mana_color_short(c)))
                        .collect::<String>()
                })
                .collect();
            format!("one of: {}", rendered.join(", "))
        }
        ManaProduction::Mixed {
            colorless_count,
            colors,
        } => {
            let colorless: String = (0..*colorless_count).map(|_| "{C}").collect();
            let colored: String = colors
                .iter()
                .map(|c| format!("{{{}}}", fmt_mana_color_short(c)))
                .collect();
            format!("{colorless}{colored}")
        }
        ManaProduction::AnyInCommandersColorIdentity { count, .. } => {
            format!("1 of commander's color identity x{}", fmt_quantity(count))
        }
        ManaProduction::DistinctColorsAmongPermanents { filter } => {
            format!("1 of each color among {}", fmt_target(filter))
        }
        ManaProduction::TriggerEventManaType => "1 of the triggering mana's type".to_string(),
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
        DelayedTriggerCondition::WhenNextEvent { .. } => "when next event this turn".into(),
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
        Effect::Draw { count, target } => {
            if !matches!(count, QuantityExpr::Fixed { value: 1 }) {
                d.push(("count".into(), fmt_quantity(count)));
            }
            if !matches!(target, TargetFilter::Controller) {
                d.push(("target".into(), fmt_target(target)));
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
        | Effect::Sacrifice { target, .. }
        | Effect::GainControl { target }
        | Effect::Attach { target }
        | Effect::Fight { target, .. }
        | Effect::CopySpell { target }
        | Effect::BecomeCopy { target, .. }
        | Effect::Suspect { target }
        | Effect::Connive { target, .. }
        | Effect::PhaseOut { target }
        | Effect::PhaseIn { target }
        | Effect::ForceBlock { target }
        | Effect::Transform { target }
        | Effect::Shuffle { target }
        | Effect::Reveal { target }
        | Effect::Regenerate { target } => {
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::DestroyAll { target, .. }
        | Effect::TapAll { target }
        | Effect::UntapAll { target }
        | Effect::BounceAll { target, .. }
        | Effect::CounterAll { target, .. }
        | Effect::DamageAll {
            amount: _,
            target,
            player_filter: _,
        } => {
            if !matches!(target, TargetFilter::None) {
                d.push(("filter".into(), fmt_target(target)));
            }
            if let Effect::DamageAll {
                amount,
                player_filter,
                ..
            } = effect
            {
                d.push(("amount".into(), fmt_quantity(amount)));
                if let Some(pf) = player_filter {
                    d.push(("player_filter".into(), format!("{pf:?}")));
                }
            }
            if let Effect::BounceAll {
                destination: Some(dest),
                ..
            } = effect
            {
                d.push(("destination".into(), format!("{dest:?}")));
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
        Effect::Scry { count, .. } | Effect::Surveil { count, .. } => {
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
        Effect::LoseLife { amount, .. } => {
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
            reveal,
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
            if *reveal {
                d.push(("reveal".into(), "true".into()));
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
            ..
        } => {
            d.push(("find".into(), fmt_target(filter)));
            // Skip only when the count is exactly `Fixed { 1 }` — dynamic counts
            // (e.g. `Variable("X")`) should always surface in the coverage breakdown.
            if !matches!(count, QuantityExpr::Fixed { value: 1 }) {
                d.push(("count".into(), fmt_quantity(count)));
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
        Effect::RegisterBending { kind } => {
            d.push(("kind".into(), format!("{kind:?}").to_ascii_lowercase()));
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
        Effect::RevealFromHand { filter, on_decline } => {
            if !matches!(filter, TargetFilter::Any) {
                d.push(("filter".into(), fmt_target(filter)));
            }
            if on_decline.is_some() {
                d.push(("on_decline".into(), "present".into()));
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
        Effect::FlipCoins {
            count,
            win_effect,
            lose_effect,
        } => {
            d.push(("count".into(), format!("{count:?}")));
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
        Effect::LoseAllPlayerCounters { target } => {
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::ExileFromTopUntil { filter } => {
            d.push(("until".into(), fmt_target(filter)));
        }
        Effect::RevealUntil {
            player,
            filter,
            kept_destination,
            rest_destination,
            ..
        } => {
            d.push(("player".into(), fmt_target(player)));
            d.push(("until".into(), fmt_target(filter)));
            d.push(("kept".into(), format!("{:?}", kept_destination)));
            d.push(("rest".into(), format!("{:?}", rest_destination)));
        }
        Effect::Discover { mana_value_limit } => {
            d.push(("mv limit".into(), mana_value_limit.to_string()));
        }
        // CR 702.85a: Cascade takes no parameters — source MV is read from the
        // stack object at resolution time.
        Effect::Cascade => {}
        // CR 702.94a: MiracleCast is an internal engine effect, not parsed from Oracle text.
        Effect::MiracleCast { .. } => {}
        // CR 702.35a: MadnessCast is synthesized from Keyword::Madness.
        Effect::MadnessCast { .. } => {}
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
        Effect::Detain { target } => {
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::ExtraTurn { target } => {
            d.push(("player".into(), fmt_target(target)));
        }
        Effect::SkipNextTurn { target, count } => {
            d.push(("player".into(), fmt_target(target)));
            if !matches!(
                count,
                crate::types::ability::QuantityExpr::Fixed { value: 1 }
            ) {
                d.push(("count".into(), format!("{count:?}")));
            }
        }
        Effect::ControlNextTurn {
            target,
            grant_extra_turn_after,
        } => {
            d.push(("player".into(), fmt_target(target)));
            if *grant_extra_turn_after {
                d.push(("extra turn after".into(), "yes".into()));
            }
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
        Effect::RemoveFromCombat { target } => {
            d.push(("target".into(), fmt_target(target)));
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
        Effect::Tribute { count } => {
            d.push(("count".into(), count.to_string()));
        }
        Effect::BecomePrepared { target } | Effect::BecomeUnprepared { target } => {
            d.push(("target".into(), fmt_target(target)));
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
        | Effect::ReduceNextSpellCost { .. }
        | Effect::GrantNextSpellAbility { .. }
        | Effect::CreateEmblem { .. }
        | Effect::PayCost { .. }
        | Effect::LoseTheGame
        | Effect::WinTheGame
        | Effect::RingTemptsYou
        | Effect::GrantCastingPermission { .. }
        | Effect::Manifest { .. }
        | Effect::ManifestDread
        | Effect::RuntimeHandled { .. }
        | Effect::ChangeTargets { .. }
        | Effect::ExchangeControl { .. }
        | Effect::Forage
        | Effect::Learn
        | Effect::SwitchPT { .. }
        | Effect::Populate
        | Effect::VentureIntoDungeon
        | Effect::VentureInto { .. }
        | Effect::TakeTheInitiative
        | Effect::Clash
        | Effect::Vote { .. }
        | Effect::Incubate { .. }
        | Effect::TimeTravel
        | Effect::Conjure { .. }
        | Effect::AddPendingETBCounters { .. }
        | Effect::ChooseAndSacrificeRest { .. }
        | Effect::ChooseOneOf { .. } => {}
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
        ContinuousModification::CopyValues { .. } => "copy values".into(),
        ContinuousModification::SetName { name } => format!("set name {name}"),
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
        ContinuousModification::SetPowerDynamic { .. } => "set base power dynamic".into(),
        ContinuousModification::SetToughnessDynamic { .. } => "set base toughness dynamic".into(),
        ContinuousModification::AddDynamicPower { .. } => "add dynamic power".into(),
        ContinuousModification::AddDynamicToughness { .. } => "add dynamic toughness".into(),
        ContinuousModification::AddDynamicKeyword { kind, .. } => {
            format!("dynamic keyword {kind:?}")
        }
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
        ContinuousModification::SwitchPowerToughness => "switch P/T".into(),
        ContinuousModification::AssignDamageFromToughness => "damage from toughness".into(),
        ContinuousModification::AssignDamageAsThoughUnblocked => {
            "damage as though unblocked".into()
        }
        ContinuousModification::ChangeController => "change controller".into(),
        ContinuousModification::SetBasicLandType { land_type } => {
            format!("set land type {}", land_type.as_subtype_str())
        }
        ContinuousModification::AssignNoCombatDamage => "assign no combat damage".into(),
        ContinuousModification::RetainPrintedTriggerFromSource {
            source_trigger_index,
        } => format!("retain printed trigger {source_trigger_index}"),
        ContinuousModification::AddSupertype { supertype } => {
            format!("add supertype {supertype}")
        }
        ContinuousModification::RemoveSupertype { supertype } => {
            format!("remove supertype {supertype}")
        }
        ContinuousModification::AddCounterOnEnter {
            counter_type,
            count,
            if_type,
        } => {
            let count_str = match count {
                crate::types::ability::QuantityExpr::Fixed { value } => value.to_string(),
                _ => format!("{count:?}"),
            };
            match if_type {
                Some(t) => format!(
                    "enter with {count_str} {counter_type} counter if {}",
                    fmt_core_type(t)
                ),
                None => format!("enter with {count_str} {counter_type} counter"),
            }
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
    // Composable modifications (GrantTrigger / GrantAbility) are emitted as
    // children, so list only the simple ones here as a joined pill.
    let simple: Vec<String> = stat
        .modifications
        .iter()
        .filter(|m| {
            !matches!(
                m,
                ContinuousModification::GrantTrigger { .. }
                    | ContinuousModification::GrantAbility { .. }
            )
        })
        .map(fmt_modification)
        .collect();
    if !simple.is_empty() {
        d.push(("mods".into(), simple.join(", ")));
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
    for def in face.abilities.iter() {
        items.push(build_ability_item(def));
    }

    // Triggers
    for trig in &face.triggers {
        items.push(build_trigger_item(trig, trigger_registry));
    }

    // Static abilities
    for stat in &face.static_abilities {
        let mode_supported =
            static_registry.contains_key(&stat.mode) || is_data_carrying_static(&stat.mode);
        let mut children = Vec::new();
        for modif in &stat.modifications {
            match modif {
                ContinuousModification::GrantTrigger { trigger } => {
                    children.push(build_trigger_item(trigger, trigger_registry));
                }
                ContinuousModification::GrantAbility { definition } => {
                    children.push(build_ability_item(definition));
                }
                _ => {}
            }
        }
        items.push(ParsedItem {
            category: ParseCategory::Static,
            label: format!("{}", stat.mode),
            source_text: stat.description.clone(),
            supported: mode_supported,
            details: static_details(stat),
            children,
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

/// Build a `ParsedItem` for a single `TriggerDefinition`, recursing into its
/// `execute` ability. Shared between top-level triggers and triggers granted
/// by static abilities (`ContinuousModification::GrantTrigger`).
fn build_trigger_item(
    trig: &TriggerDefinition,
    trigger_registry: &HashMap<TriggerMode, crate::game::triggers::TriggerMatcher>,
) -> ParsedItem {
    // CR 603.8: StateCondition triggers use the priority pipeline, not the
    // event-based trigger registry — they are supported.
    let mode_supported = !matches!(&trig.mode, TriggerMode::Unknown(_))
        && (trigger_registry.contains_key(&trig.mode)
            || matches!(&trig.mode, TriggerMode::StateCondition));
    let mut children = Vec::new();
    if let Some(execute) = &trig.execute {
        children.push(build_ability_item(execute));
    }
    ParsedItem {
        category: ParseCategory::Trigger,
        label: format!("{}", trig.mode),
        source_text: trig.description.clone(),
        supported: mode_supported && children.iter().all(|c| c.is_fully_supported()),
        details: trigger_details(trig),
        children,
    }
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

    // Else-ability chain (CR 608.2c: "Otherwise" branches)
    if let Some(else_ab) = &def.else_ability {
        children.push(build_ability_item(else_ab));
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
///
/// An additional cost ("As an additional cost to cast this spell, ...") is its
/// own Oracle line, so it must emit exactly one `ParsedItem` to keep
/// `count_effective_parsed_items` in parity with `count_effective_oracle_lines`.
/// Without this, cards with a concrete additional cost plus one spell effect
/// (e.g. Vicious Rivalry, Fix What's Broken) are falsely flagged by the
/// silent-drop audit: the Oracle line is counted but no parse item is emitted
/// because `build_cost_item` only emits for `Unimplemented` costs.
///
/// Behavior:
/// - If any underlying `AbilityCost` is `Unimplemented`, fall through to the
///   existing `build_cost_item` path which emits a `Cost:Unimplemented` item
///   (so `extract_gap_details` still surfaces the gap). This preserves the
///   pre-existing one-item-per-line parity in the unsupported case.
/// - Otherwise, emit a single supported `ParsedItem` describing the additional
///   cost kind, restoring parity for the supported case.
fn build_additional_cost_items(additional_cost: &AdditionalCost, items: &mut Vec<ParsedItem>) {
    if additional_cost_has_unimplemented(additional_cost) {
        match additional_cost {
            AdditionalCost::Optional(cost) | AdditionalCost::Required(cost) => {
                build_cost_item(cost, items);
            }
            AdditionalCost::Choice(first, second) => {
                build_cost_item(first, items);
                build_cost_item(second, items);
            }
        }
        return;
    }

    let label = match additional_cost {
        AdditionalCost::Optional(_) => "AdditionalCost:Optional",
        AdditionalCost::Required(_) => "AdditionalCost:Required",
        AdditionalCost::Choice(_, _) => "AdditionalCost:Choice",
    };
    items.push(ParsedItem {
        category: ParseCategory::Cost,
        label: label.to_string(),
        source_text: None,
        supported: true,
        details: vec![],
        children: vec![],
    });
}

/// Returns true if any leaf `AbilityCost` in the tree is `Unimplemented`.
fn additional_cost_has_unimplemented(additional_cost: &AdditionalCost) -> bool {
    match additional_cost {
        AdditionalCost::Optional(cost) | AdditionalCost::Required(cost) => {
            ability_cost_has_unimplemented(cost)
        }
        AdditionalCost::Choice(first, second) => {
            ability_cost_has_unimplemented(first) || ability_cost_has_unimplemented(second)
        }
    }
}

fn ability_cost_has_unimplemented(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::Unimplemented { .. } => true,
        AbilityCost::Composite { costs } => costs.iter().any(ability_cost_has_unimplemented),
        _ => false,
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
    for def in obj.abilities.iter() {
        if let Effect::Unimplemented { name, .. } = &*def.effect {
            missing.push(format!("Effect: {name}"));
        }
    }

    // 3. Check trigger modes against trigger registry
    // CR 603.8: StateCondition triggers use the priority pipeline, not the event registry.
    let trigger_registry = build_trigger_registry();
    // Classification scan: iterate every printed trigger/static regardless
    // of functioning state — we're computing coverage, not game behavior.
    for trig in obj.trigger_definitions.iter_all() {
        if matches!(&trig.mode, TriggerMode::Unknown(_))
            || (!trigger_registry.contains_key(&trig.mode)
                && !matches!(&trig.mode, TriggerMode::StateCondition))
        {
            missing.push(format!("Trigger: {}", trig.mode));
        }
    }

    // 4. Check static ability modes against static registry
    let static_registry = build_static_registry();
    for stat in obj.static_definitions.iter_all() {
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
    let valid_subtypes = collect_valid_subtypes(card_db);

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

        // Build the parse tree once — it feeds both the silent-drop check
        // (below) and gap_details (further down), so compute it up front.
        let parse_details = build_parse_details(face, &trigger_registry, &static_registry);

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

        // Check parse warnings
        check_parse_warnings(&face.parse_warnings, &mut missing);

        // Validate subtype references in AddSubtype modifications against
        // the printed-corpus lexicon. Catches parser misfires where English
        // filler words (`Gets`, `Until`, `And`) were tokenized as subtypes.
        check_subtype_lexicon(face, &valid_subtypes, &mut missing);

        // Flag cards whose parsed features have no runtime resolver. Without
        // this, a card can parse cleanly yet silently do nothing on resolution.
        check_resolver_features(face, &mut missing);

        // Flag cards where the parser consumed Oracle text without producing
        // a corresponding parse item. Uses the parse tree computed above.
        check_silent_drops(&face.oracle_text, &parse_details, &mut missing);

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

        let mut gap_details = extract_gap_details(&parse_details);
        // Append parse-warning gaps so they appear in per-card gap reporting.
        for warning in &face.parse_warnings {
            if warning.starts_with("target-fallback") {
                let handler = if warning.contains("trigger subject") {
                    "ParseWarning:trigger-subject".to_string()
                } else {
                    "ParseWarning:target-fallback".to_string()
                };
                gap_details.push(GapDetail {
                    handler,
                    source_text: Some(warning.clone()),
                });
            }
        }
        let gap_count = gap_details.len();

        let printings = card_db
            .printings_for(key)
            .map(|slice| slice.to_vec())
            .unwrap_or_default();

        cards.push(CardCoverageResult {
            card_name: face.name.clone(),
            set_code: String::new(),
            supported,
            gap_details,
            gap_count,
            oracle_text: face.oracle_text.clone(),
            parse_details,
            printings,
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

    // Per-set rollup: one entry per set code appearing in any card's
    // `printings`. A card with N printings contributes to N sets, matching
    // how the dashboard historically aggregated this client-side.
    let mut set_acc: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for card in &cards {
        for code in &card.printings {
            let entry = set_acc.entry(code.clone()).or_default();
            entry.0 += 1;
            if card.supported {
                entry.1 += 1;
            }
        }
    }
    let coverage_by_set = set_acc
        .into_iter()
        .map(|(set_code, (total_cards, supported_cards))| {
            let coverage_pct = if total_cards > 0 {
                (supported_cards as f64 / total_cards as f64) * 100.0
            } else {
                0.0
            };
            (
                set_code,
                SetCoverageSummary {
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
        coverage_by_set,
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
        // CR 603.8: StateCondition triggers are handled by the priority pipeline
        // (check_state_triggers), not the event-based trigger registry. They are supported.
        if matches!(&def.mode, TriggerMode::Unknown(_))
            || (!trigger_registry.contains_key(&def.mode)
                && !matches!(&def.mode, TriggerMode::StateCondition))
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

        if let Some(ReplacementCondition::Unrecognized { ref text }) = def.condition {
            let label = format!("Replacement:Unrecognized({})", truncate_label(text, 60));
            if !missing.contains(&label) {
                missing.push(label);
            }
        }
    }
}

/// Build a lexicon of every subtype that appears on at least one printed
/// card face. Used by [`check_subtype_lexicon`] to flag parser misfires:
/// any `AddSubtype { subtype }` whose value isn't a real printed subtype
/// (e.g. `"Gets"`, `"Until"`, `"+1/+1"`) signals that the animation or
/// static-ability parser tokenized English filler words as subtypes.
///
/// The MTG Comprehensive Rules define valid subtypes (CR 205.3), but the
/// printed corpus is the authoritative source for the engine — anything
/// that has appeared on a real card's type line is valid.
fn collect_valid_subtypes(card_db: &CardDatabase) -> HashSet<String> {
    card_db
        .face_iter()
        .flat_map(|(_, face)| face.card_type.subtypes.iter().cloned())
        .collect()
}

/// Visit every `ContinuousModification` reachable from a card face.
///
/// Walks abilities (including nested sub/mode chains and `GenericEffect`
/// static modifications), static abilities, triggers' execute bodies, and
/// replacements' execute/decline bodies. The visitor is invoked for each
/// modification so callers can inspect or validate the payload.
fn visit_face_modifications(face: &CardFace, visit: &mut impl FnMut(&ContinuousModification)) {
    for ability in face.abilities.iter() {
        visit_ability_modifications(ability, visit);
    }
    for stat in &face.static_abilities {
        for m in &stat.modifications {
            visit(m);
        }
    }
    for trigger in &face.triggers {
        if let Some(execute) = &trigger.execute {
            visit_ability_modifications(execute, visit);
        }
    }
    for replacement in &face.replacements {
        if let Some(execute) = &replacement.execute {
            visit_ability_modifications(execute, visit);
        }
        if let ReplacementMode::Optional {
            decline: Some(decline),
        } = &replacement.mode
        {
            visit_ability_modifications(decline, visit);
        }
    }
}

/// Recursively visit modifications inside an ability's effect graph.
/// Descends into `GenericEffect.static_abilities` (the typical carrier of
/// continuous modifications emitted from animations), sub-abilities, and
/// modal branches. Non-`GenericEffect` effects don't carry modifications.
fn visit_ability_modifications(
    def: &AbilityDefinition,
    visit: &mut impl FnMut(&ContinuousModification),
) {
    if let Effect::GenericEffect {
        static_abilities, ..
    } = &*def.effect
    {
        for stat in static_abilities {
            for m in &stat.modifications {
                visit(m);
            }
        }
    }
    if let Some(sub) = &def.sub_ability {
        visit_ability_modifications(sub, visit);
    }
    if let Some(else_ab) = &def.else_ability {
        visit_ability_modifications(else_ab, visit);
    }
    for mode_ability in &def.mode_abilities {
        visit_ability_modifications(mode_ability, visit);
    }
}

/// Validate every `AddSubtype` modification on the face against the lexicon
/// of real printed subtypes. Flags each invalid subtype as a distinct gap
/// label so the coverage reporter can group parser misfires by the bad value.
///
/// Background: the animation parser (see `parse_animation_types`) and static
/// parser can over-eagerly tokenize English filler words as subtypes
/// (e.g. `"Gets"`, `"Until"`, `"And"`). Those modifications never apply at
/// runtime but contaminate the coverage signal — without this check a card
/// whose only "supported" ability is a misparsed become would read as
/// supported in the dashboard.
fn check_subtype_lexicon(face: &CardFace, valid: &HashSet<String>, missing: &mut Vec<String>) {
    visit_face_modifications(face, &mut |m| {
        if let ContinuousModification::AddSubtype { subtype } = m {
            if !valid.contains(subtype) {
                let label = format!(
                    "ParserMisfire:InvalidSubtype({})",
                    truncate_label(subtype, 40)
                );
                if !missing.contains(&label) {
                    missing.push(label);
                }
            }
        }
    });
}

/// Flag cards where the parser consumed Oracle text without emitting a
/// corresponding parse item — a silent drop. Shares the oracle-line counting
/// logic with [`audit_silent_drops`] (used by the CLI audit) so both views
/// agree on what counts as a dropped line.
///
/// Background: `collect_ability_missing_parts` only flags `Effect::Unimplemented`
/// at the top of an ability. A parser can silently swallow a whole Oracle line
/// (or emit nothing at all) and the card still reports as supported. Folding
/// this into the supported predicate unballoons coverage by cards where the
/// parser accepted text but produced no runtime behavior for it.
fn check_silent_drops(
    oracle_text: &Option<String>,
    parse_details: &[ParsedItem],
    missing: &mut Vec<String>,
) {
    let Some(oracle_text) = oracle_text.as_ref().filter(|t| !t.is_empty()) else {
        return;
    };

    let effective_oracle = count_effective_oracle_lines(oracle_text);
    let effective_parsed = count_effective_parsed_items(parse_details);

    if effective_oracle > effective_parsed {
        let label = format!("SilentDrop:{}_of_{}", effective_parsed, effective_oracle);
        if !missing.contains(&label) {
            missing.push(label);
        }
    }
}

/// Flag cards whose parsed features aren't handled by any runtime resolver.
/// Shares the per-card feature extraction with [`audit_resolver_features`]
/// (used by the CLI audit) so both views agree on what counts as unhandled.
///
/// Background: `collect_ability_missing_parts` checks that the effect variant
/// is non-Unimplemented, but doesn't verify the resolver actually does
/// anything with the payload. E.g., a `Discover` effect may parse but have
/// no runtime handler — the card reads as supported yet silently does
/// nothing on resolution. Folding this into the supported predicate catches
/// those resolver gaps at coverage time.
fn check_resolver_features(face: &CardFace, missing: &mut Vec<String>) {
    let mut features = HashMap::new();
    extract_card_features(face, &mut features);
    for (feat, support) in features {
        if support == FeatureSupport::Unhandled {
            let label = format!("ResolverFeature:{feat}");
            if !missing.contains(&label) {
                missing.push(label);
            }
        }
    }
}

/// Target-fallback warnings indicate degraded targeting (TargetFilter::Any instead of a
/// specific filter). Cards with these have silently incorrect behavior at runtime.
fn check_parse_warnings(warnings: &[String], missing: &mut Vec<String>) {
    for warning in warnings {
        if warning.starts_with("target-fallback") {
            // Extract the sub-category for groupable gap labels:
            // "target-fallback: parse_target could not classify 'foo'" → "ParseWarning:parse_target"
            // "target-fallback: trigger subject parse fell back..." → "ParseWarning:trigger-subject"
            let label = if warning.contains("trigger subject") {
                "ParseWarning:trigger-subject".to_string()
            } else if warning.contains("parse_target could not classify") {
                "ParseWarning:target-fallback".to_string()
            } else {
                format!(
                    "ParseWarning:{}",
                    warning
                        .split(':')
                        .nth(1)
                        .unwrap_or("unknown")
                        .trim()
                        .split('\'')
                        .next()
                        .unwrap_or("unknown")
                        .trim()
                )
            };
            if !missing.contains(&label) {
                missing.push(label);
            }
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
        if is_commander_permission_sentence(&lower) {
            continue;
        }

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
        "choose six",
        "choose seven",
        "choose eight",
        "choose nine",
        "choose ten",
        "choose up to one",
        "choose up to two",
        "choose up to three",
        "choose up to four",
        "choose up to five",
        "choose up to six",
        "choose up to seven",
        "choose up to eight",
        "choose up to nine",
        "choose up to ten",
        "choose any number",
        "choose x.",
    ];
    CHOOSE_PHRASES.iter().any(|p| lower.contains(p))
}

/// Strip structural formatting prefixes from an Oracle line, returning the
/// semantic effect text. Handles:
/// - Modal bullet: "• Destroy target creature." → "destroy target creature."
/// - Saga chapter: "I, II — Create a 2/2 ..." → "create a 2/2 ..."
/// - Spree mode: "+ {1} — Destroy target artifact." → "destroy target artifact."
/// - Attraction/dungeon: "2—9 | Create two Treasure tokens." → "create two treasure tokens."
///
/// Returns `None` if the line is purely structural (modal header, saga reminder).
/// The returned text is already lowercased.
fn strip_structural_prefix(lower: &str) -> Option<String> {
    // Modal bullet prefix "• "
    if let Some(rest) = lower.strip_prefix('\u{2022}') {
        let rest = rest.trim_start();
        if rest.is_empty() {
            return None;
        }
        return Some(rest.to_string());
    }

    // Spree mode prefix: "+ {cost} — " (em-dash)
    if let Some(rest) = lower.strip_prefix("+ ") {
        // Skip the cost portion (everything up to " — ")
        if let Some(pos) = rest.find(" \u{2014} ") {
            let effect = &rest[pos + 4..]; // skip " — "
            if !effect.is_empty() {
                return Some(effect.to_string());
            }
        }
    }

    // Saga chapter prefix: roman numerals followed by " — "
    // Patterns: "i — ", "ii — ", "iii — ", "iv — ", "i, ii — ", "i, ii, iii — "
    if is_saga_chapter_line(lower) {
        if let Some(pos) = lower.find(" \u{2014} ") {
            let effect = &lower[pos + 4..]; // skip " — "
            if !effect.is_empty() {
                return Some(effect.to_string());
            }
        }
    }

    // Attraction/dungeon prefix: "N | " or "N—N | "
    if is_attraction_line(lower) {
        if let Some(pos) = lower.find(" | ") {
            let effect = &lower[pos + 3..];
            if !effect.is_empty() {
                return Some(effect.to_string());
            }
        }
    }

    None
}

/// Check if a line is a saga chapter line (starts with roman numerals + em-dash).
fn is_saga_chapter_line(lower: &str) -> bool {
    // Must start with a roman numeral character
    if !lower.starts_with('i') && !lower.starts_with('v') && !lower.starts_with('x') {
        return false;
    }
    // Find " — " (em-dash) delimiter
    let Some(dash_pos) = lower.find(" \u{2014} ") else {
        return false;
    };
    let prefix = &lower[..dash_pos];
    // Validate prefix is comma-separated roman numerals
    prefix
        .split(", ")
        .all(|part| matches!(part.trim(), "i" | "ii" | "iii" | "iv" | "v" | "vi" | "vii"))
}

/// Check if a line is an attraction/dungeon line ("N | " or "N—N | ") or a
/// level-up effect line ("N+ | " or "N-M | ").
fn is_attraction_line(lower: &str) -> bool {
    let Some(pipe_pos) = lower.find(" | ") else {
        return false;
    };
    let prefix = &lower[..pipe_pos];
    // Attraction/dungeon: "20", "1", "2—9", "10—19"
    // Level-up: "2+", "8+", "1-7"
    prefix.split('\u{2014}').all(|part| {
        let trimmed = part.trim().strip_suffix('+').unwrap_or(part.trim());
        !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_digit() || c == '-')
    })
}

/// Check if a line is a level-up effect line ("N+ | ..." or "N-M | ...").
fn is_level_effect_line(lower: &str) -> bool {
    let Some(pipe_pos) = lower.find(" | ") else {
        return false;
    };
    let prefix = lower[..pipe_pos].trim();
    // Level-up: "2+", "8+", "1-7", "10+"
    if let Some(digits) = prefix.strip_suffix('+') {
        return !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit());
    }
    // Range: "1-7"
    if let Some((a, b)) = prefix.split_once('-') {
        return !a.is_empty()
            && !b.is_empty()
            && a.chars().all(|c| c.is_ascii_digit())
            && b.chars().all(|c| c.is_ascii_digit());
    }
    false
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
            if is_commander_permission_sentence(stripped) {
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

    // Feature frequency: tag -> (count, example_cards, is_handled).
    // `is_handled` is derived from the compiler-checked classifier functions
    // (`condition_feature`, `quantity_ref_feature`, ...) at extraction time.
    let mut feature_freq: HashMap<String, (usize, Vec<String>, bool)> = HashMap::new();
    let mut flagged_cards = Vec::new();
    let mut total_audited = 0;

    for (key, face) in card_db.face_iter() {
        // Only audit cards the existing coverage considers "Fully Supported"
        if !is_card_supported(face, &trigger_registry, &static_registry) {
            continue;
        }
        total_audited += 1;

        let mut features: HashMap<String, FeatureSupport> = HashMap::new();
        extract_card_features(face, &mut features);

        // Record frequency for ALL features
        for (feat, support) in &features {
            let handled = *support == FeatureSupport::Handled;
            let entry = feature_freq
                .entry(feat.clone())
                .or_insert_with(|| (0, Vec::new(), handled));
            entry.0 += 1;
            if entry.1.len() < 3 {
                entry.1.push(key.to_string());
            }
        }

        // Flag unhandled features
        let unhandled: Vec<String> = features
            .iter()
            .filter(|(_, s)| **s == FeatureSupport::Unhandled)
            .map(|(f, _)| f.clone())
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
        .map(|(feat, (count, examples, handled))| FeatureUsage {
            feature: feat.clone(),
            card_count: *count,
            handled: *handled,
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
    for def in face.abilities.iter() {
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

/// Whether the resolver currently handles a given parsed feature.
///
/// The classification is produced by exhaustive `match` arms on the underlying
/// AST enums (`AbilityCondition`, `QuantityRef`, `PlayerFilter`, `StaticCondition`)
/// and on the closed set of structural ability-tree sites. Adding a new enum
/// variant is a compile error until the variant is classified here, which
/// prevents the silent drift that the old hand-maintained string registry
/// suffered from: a newly-parsed feature must be explicitly marked `Handled`
/// or `Unhandled` before the code builds.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum FeatureSupport {
    Handled,
    Unhandled,
}

/// Structural ability-tree sites — non-enum-variant features emitted during
/// feature extraction. Adding a variant here forces `structural_feature()` to
/// classify it, and any new emit site must route through this enum.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum StructuralFeature {
    Condition,
    ElseAbility,
    RepeatFor,
    ForwardResult,
    Duration,
    OptionalFor,
    MultiTarget,
    Distribute,
    AbilityModal,
    SpellModal,
    AdditionalCost,
    CostReduction,
    TriggerCondition,
}

impl StructuralFeature {
    fn tag(self) -> &'static str {
        use StructuralFeature::*;
        match self {
            Condition => "structural:condition",
            ElseAbility => "structural:else_ability",
            RepeatFor => "structural:repeat_for",
            ForwardResult => "structural:forward_result",
            Duration => "structural:duration",
            OptionalFor => "structural:optional_for",
            MultiTarget => "structural:multi_target",
            Distribute => "structural:distribute",
            AbilityModal => "structural:ability_modal",
            SpellModal => "structural:spell_modal",
            AdditionalCost => "structural:additional_cost",
            CostReduction => "structural:cost_reduction",
            TriggerCondition => "structural:trigger_condition",
        }
    }

    /// All existing structural sites are handled by `resolve_ability_chain`
    /// and related resolver entry points. New variants must classify here
    /// before they compile.
    fn support(self) -> FeatureSupport {
        use StructuralFeature::*;
        match self {
            Condition | ElseAbility | RepeatFor | ForwardResult | Duration | OptionalFor
            | MultiTarget | Distribute | AbilityModal | SpellModal | AdditionalCost
            | CostReduction | TriggerCondition => FeatureSupport::Handled,
        }
    }
}

/// Extract structural feature tags from a card's entire parsed data.
///
/// Each tag is mapped to `FeatureSupport::Handled` or `FeatureSupport::Unhandled`
/// via exhaustive matches on the source enum, so adding a new variant is a
/// compile error until it is explicitly classified.
fn extract_card_features(face: &CardFace, features: &mut HashMap<String, FeatureSupport>) {
    for def in face.abilities.iter() {
        extract_ability_features(def, features);
    }
    for trig in &face.triggers {
        if let Some(execute) = &trig.execute {
            extract_ability_features(execute, features);
        }
        // Trigger-level condition (intervening-if)
        if trig.condition.is_some() {
            emit_structural(features, StructuralFeature::TriggerCondition);
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
        emit_structural(features, StructuralFeature::AdditionalCost);
    }
    if face.modal.is_some() {
        emit_structural(features, StructuralFeature::SpellModal);
    }
}

fn emit_structural(features: &mut HashMap<String, FeatureSupport>, s: StructuralFeature) {
    features.insert(s.tag().to_string(), s.support());
}

/// Extract features from a static condition.
fn extract_static_condition_features(
    cond: &StaticCondition,
    features: &mut HashMap<String, FeatureSupport>,
) {
    // Compound conditions recurse; every other variant emits a single tag with
    // its compiler-checked handled/unhandled classification.
    match cond {
        StaticCondition::QuantityComparison { lhs, rhs, .. } => {
            let (name, support) = static_condition_feature(cond);
            features.insert(format!("static_condition:{name}"), support);
            extract_quantity_features(lhs, features);
            extract_quantity_features(rhs, features);
        }
        StaticCondition::And { conditions } | StaticCondition::Or { conditions } => {
            for sub in conditions {
                extract_static_condition_features(sub, features);
            }
        }
        _ => {
            // All other variants (including `Not`) emit a single tag. The
            // classifier carries compiler-enforced handled/unhandled status.
            let (name, support) = static_condition_feature(cond);
            features.insert(format!("static_condition:{name}"), support);
        }
    }
}

/// Recursively extract structural feature tags from an ability definition tree.
fn extract_ability_features(
    def: &AbilityDefinition,
    features: &mut HashMap<String, FeatureSupport>,
) {
    // Condition
    if let Some(ref cond) = def.condition {
        emit_structural(features, StructuralFeature::Condition);
        let (name, support) = condition_feature(cond);
        features.insert(format!("condition:{name}"), support);
        extract_condition_quantity_features(cond, features);
    }

    // Else ability
    if let Some(ref else_ab) = def.else_ability {
        emit_structural(features, StructuralFeature::ElseAbility);
        extract_ability_features(else_ab, features);
    }

    // Repeat-for
    if let Some(ref qty) = def.repeat_for {
        emit_structural(features, StructuralFeature::RepeatFor);
        extract_quantity_features(qty, features);
    }

    // Forward result
    if def.forward_result {
        emit_structural(features, StructuralFeature::ForwardResult);
    }

    // Player scope
    if let Some(ref scope) = def.player_scope {
        let (name, support) = player_filter_feature(scope);
        features.insert(format!("player_scope:{name}"), support);
    }

    // Optional-for (opponent may)
    if def.optional_for.is_some() {
        emit_structural(features, StructuralFeature::OptionalFor);
    }

    // Multi-target
    if def.multi_target.is_some() {
        emit_structural(features, StructuralFeature::MultiTarget);
    }

    // Distribute
    if def.distribute.is_some() {
        emit_structural(features, StructuralFeature::Distribute);
    }

    // Modal (on ability, not spell-level)
    if def.modal.is_some() {
        emit_structural(features, StructuralFeature::AbilityModal);
    }

    // Cost reduction
    if def.cost_reduction.is_some() {
        emit_structural(features, StructuralFeature::CostReduction);
    }

    // Duration (continuous effects from spells/abilities)
    if def.duration.is_some() {
        emit_structural(features, StructuralFeature::Duration);
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
fn extract_condition_quantity_features(
    cond: &AbilityCondition,
    features: &mut HashMap<String, FeatureSupport>,
) {
    if let AbilityCondition::QuantityCheck { lhs, rhs, .. } = cond {
        extract_quantity_features(lhs, features);
        extract_quantity_features(rhs, features);
    }
}

/// Extract QuantityRef variant tags from a QuantityExpr.
fn extract_quantity_features(qty: &QuantityExpr, features: &mut HashMap<String, FeatureSupport>) {
    match qty {
        QuantityExpr::Fixed { .. } => {}
        QuantityExpr::Ref { qty: qref } => {
            let (name, support) = quantity_ref_feature(qref);
            features.insert(format!("quantity_ref:{name}"), support);
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
fn extract_effect_quantity_features(
    effect: &Effect,
    features: &mut HashMap<String, FeatureSupport>,
) {
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

/// Map an `AbilityCondition` variant to its tag name and resolver-support class.
///
/// Adding a new variant to `AbilityCondition` produces a compile error here
/// until the variant is explicitly classified — this is what replaces the
/// hand-maintained `resolver_handled_features` string set.
fn condition_feature(cond: &AbilityCondition) -> (&'static str, FeatureSupport) {
    use FeatureSupport::*;
    match cond {
        // Handled by `evaluate_condition` / `resolve_ability_chain`
        // (crates/engine/src/game/effects/mod.rs).
        AbilityCondition::AdditionalCostPaid => ("AdditionalCostPaid", Handled),
        AbilityCondition::AdditionalCostPaidInstead => ("AdditionalCostPaidInstead", Handled),
        AbilityCondition::AdditionalCostNotPaid => ("AdditionalCostNotPaid", Handled),
        AbilityCondition::IfYouDo => ("IfYouDo", Handled),
        AbilityCondition::WhenYouDo => ("WhenYouDo", Handled),
        AbilityCondition::CastFromZone { .. } => ("CastFromZone", Handled),
        AbilityCondition::RevealedHasCardType { .. } => ("RevealedHasCardType", Handled),
        AbilityCondition::SourceDidNotEnterThisTurn => ("SourceDidNotEnterThisTurn", Handled),
        AbilityCondition::CastVariantPaid { .. } => ("CastVariantPaid", Handled),
        AbilityCondition::CastVariantPaidInstead { .. } => ("CastVariantPaidInstead", Handled),
        AbilityCondition::IfAPlayerDoes => ("IfAPlayerDoes", Handled),
        AbilityCondition::QuantityCheck { .. } => ("QuantityCheck", Handled),
        AbilityCondition::HasMaxSpeed => ("HasMaxSpeed", Handled),
        AbilityCondition::IsMonarch => ("IsMonarch", Handled),
        AbilityCondition::TargetHasKeywordInstead { .. } => ("TargetHasKeywordInstead", Handled),
        // CR 608.2c: active-player check; handled by `evaluate_condition` (effects/mod.rs).
        AbilityCondition::IsYourTurn { .. } => ("IsYourTurn", Handled),
        // CR 614.1a: `ConditionInstead` wraps a general condition with swap-on-true semantics.
        AbilityCondition::ConditionInstead { .. } => ("ConditionInstead", Handled),
        // CR 608.2c + CR 614.1d: "you control a/no [filter]" — handled by
        // evaluate_condition (effects/mod.rs); used by reveal-tribal land cycle
        // (Fortified Beachhead, Temple of the Dragon Queen) on_decline gating.
        AbilityCondition::ControllerControlsMatching { .. } => {
            ("ControllerControlsMatching", Handled)
        }
        // Variants below are parsed but have no runtime resolver today.
        AbilityCondition::TargetMatchesFilter { .. } => ("TargetMatchesFilter", Unhandled),
        AbilityCondition::SourceMatchesFilter { .. } => ("SourceMatchesFilter", Unhandled),
        AbilityCondition::ZoneChangedThisWay { .. } => ("ZoneChangedThisWay", Unhandled),
        AbilityCondition::SourceIsTapped { .. } => ("SourceIsTapped", Unhandled),
        // CR 608.2c: Compound condition — resolved recursively by `evaluate_condition`
        // (effects/mod.rs), which short-circuits on the first false child.
        AbilityCondition::And { .. } => ("And", Handled),
        // CR 730.2a: Daybound/Nightbound ETB initialization — handled by evaluate_condition.
        AbilityCondition::DayNightIsNeither => ("DayNightIsNeither", Handled),
        // CR 603.4: Per-ability per-turn resolution counter — handled by evaluate_condition.
        AbilityCondition::NthResolutionThisTurn { .. } => ("NthResolutionThisTurn", Handled),
    }
}

/// Map a `QuantityRef` variant to its tag name and resolver-support class.
/// Handled variants are resolved by `game::quantity::resolve_quantity`.
fn quantity_ref_feature(qref: &QuantityRef) -> (&'static str, FeatureSupport) {
    use FeatureSupport::*;
    match qref {
        QuantityRef::HandSize => ("HandSize", Handled),
        QuantityRef::LifeTotal => ("LifeTotal", Handled),
        QuantityRef::GraveyardSize => ("GraveyardSize", Handled),
        QuantityRef::LifeAboveStarting => ("LifeAboveStarting", Handled),
        QuantityRef::StartingLifeTotal => ("StartingLifeTotal", Unhandled),
        QuantityRef::Speed => ("Speed", Handled),
        QuantityRef::ObjectCount { .. } => ("ObjectCount", Handled),
        QuantityRef::ObjectCountDistinctNames { .. } => ("ObjectCountDistinctNames", Handled),
        QuantityRef::PlayerCount { .. } => ("PlayerCount", Handled),
        QuantityRef::CountersOnSelf { .. } => ("CountersOnSelf", Handled),
        QuantityRef::CountersOnTarget { .. } => ("CountersOnTarget", Handled),
        QuantityRef::AnyCountersOnTarget => ("AnyCountersOnTarget", Handled),
        QuantityRef::AnyCountersOnSelf => ("AnyCountersOnSelf", Handled),
        QuantityRef::CountersOnObjects { .. } => ("CountersOnObjects", Handled),
        QuantityRef::Variable { .. } => ("Variable", Handled),
        QuantityRef::SelfPower => ("SelfPower", Handled),
        QuantityRef::SelfToughness => ("SelfToughness", Handled),
        QuantityRef::Aggregate { .. } => ("Aggregate", Handled),
        QuantityRef::TargetPower => ("TargetPower", Handled),
        QuantityRef::TargetLifeTotal => ("TargetLifeTotal", Handled),
        QuantityRef::Devotion { .. } => ("Devotion", Handled),
        QuantityRef::DistinctCardTypesInZone { .. } => ("DistinctCardTypesInZone", Handled),
        QuantityRef::DistinctCardTypesExiledBySource => {
            ("DistinctCardTypesExiledBySource", Unhandled)
        }
        QuantityRef::CardsExiledBySource => ("CardsExiledBySource", Handled),
        QuantityRef::ZoneCardCount { .. } => ("ZoneCardCount", Handled),
        QuantityRef::BasicLandTypeCount => ("BasicLandTypeCount", Handled),
        QuantityRef::DistinctColorsAmongPermanents { .. } => {
            ("DistinctColorsAmongPermanents", Handled)
        }
        QuantityRef::PreviousEffectAmount => ("PreviousEffectAmount", Handled),
        QuantityRef::TrackedSetSize => ("TrackedSetSize", Handled),
        QuantityRef::ExiledFromHandThisResolution => ("ExiledFromHandThisResolution", Handled),
        QuantityRef::LifeLostThisTurn => ("LifeLostThisTurn", Handled),
        QuantityRef::EventContextAmount => ("EventContextAmount", Handled),
        QuantityRef::EventContextSourcePower => ("EventContextSourcePower", Handled),
        QuantityRef::EventContextSourceToughness => ("EventContextSourceToughness", Handled),
        QuantityRef::EventContextSourceManaValue => ("EventContextSourceManaValue", Handled),
        QuantityRef::SpellsCastThisTurn { .. } => ("SpellsCastThisTurn", Handled),
        QuantityRef::EnteredThisTurn { .. } => ("EnteredThisTurn", Handled),
        QuantityRef::CrimesCommittedThisTurn => ("CrimesCommittedThisTurn", Handled),
        QuantityRef::LifeGainedThisTurn => ("LifeGainedThisTurn", Handled),
        QuantityRef::PermanentsLeftBattlefieldThisTurn => {
            ("PermanentsLeftBattlefieldThisTurn", Unhandled)
        }
        QuantityRef::NonlandPermanentsLeftBattlefieldThisTurn => {
            ("NonlandPermanentsLeftBattlefieldThisTurn", Unhandled)
        }
        QuantityRef::TurnsTaken => ("TurnsTaken", Unhandled),
        QuantityRef::ChosenNumber => ("ChosenNumber", Unhandled),
        QuantityRef::CreaturesDiedThisTurn => ("CreaturesDiedThisTurn", Unhandled),
        QuantityRef::AttackedThisTurn => ("AttackedThisTurn", Unhandled),
        QuantityRef::DescendedThisTurn => ("DescendedThisTurn", Unhandled),
        QuantityRef::SpellsCastLastTurn => ("SpellsCastLastTurn", Unhandled),
        QuantityRef::OpponentLifeLostThisTurn => ("OpponentLifeLostThisTurn", Unhandled),
        QuantityRef::MaxLifeLostThisTurnAcrossPlayers => {
            ("MaxLifeLostThisTurnAcrossPlayers", Handled)
        }
        QuantityRef::CounterAddedThisTurn => ("CounterAddedThisTurn", Unhandled),
        QuantityRef::OpponentDiscardedCardThisTurn => ("OpponentDiscardedCardThisTurn", Handled),
        QuantityRef::OpponentLifeTotal => ("OpponentLifeTotal", Unhandled),
        QuantityRef::OpponentHandSize => ("OpponentHandSize", Unhandled),
        QuantityRef::DungeonsCompleted => ("DungeonsCompleted", Unhandled),
        QuantityRef::TargetZoneCardCount { .. } => ("TargetZoneCardCount", Unhandled),
        QuantityRef::CostXPaid => ("CostXPaid", Handled),
        QuantityRef::ManaSpentOnTriggeringSpell => ("ManaSpentOnTriggeringSpell", Handled),
        QuantityRef::ManaSpentOnSelf => ("ManaSpentOnSelf", Handled),
        QuantityRef::ColorsSpentOnSelf => ("ColorsSpentOnSelf", Handled),
        QuantityRef::EventContextSourceCostX => ("EventContextSourceCostX", Handled),
        QuantityRef::ColorsInCommandersColorIdentity => {
            ("ColorsInCommandersColorIdentity", Handled)
        }
        QuantityRef::AttachmentsOnLeavingObject { .. } => ("AttachmentsOnLeavingObject", Handled),
        QuantityRef::PlayerCounter { .. } => ("PlayerCounter", Handled),
    }
}

/// Map a `PlayerFilter` variant to its tag name and resolver-support class.
/// Handled variants are consumed by `resolve_ability_chain`'s player-scope expansion.
fn player_filter_feature(scope: &PlayerFilter) -> (&'static str, FeatureSupport) {
    use FeatureSupport::*;
    match scope {
        PlayerFilter::All => ("All", Handled),
        PlayerFilter::Opponent => ("Opponent", Handled),
        PlayerFilter::OpponentLostLife => ("OpponentLostLife", Handled),
        PlayerFilter::OpponentGainedLife => ("OpponentGainedLife", Handled),
        PlayerFilter::HighestSpeed => ("HighestSpeed", Handled),
        // Previously emitted via Debug formatting; never appeared in the handled set.
        PlayerFilter::Controller => ("Controller", Unhandled),
        PlayerFilter::ZoneChangedThisWay => ("ZoneChangedThisWay", Unhandled),
        PlayerFilter::OwnersOfCardsExiledBySource => ("OwnersOfCardsExiledBySource", Handled),
        PlayerFilter::TriggeringPlayer => ("TriggeringPlayer", Handled),
        PlayerFilter::OpponentOtherThanTriggering => ("OpponentOtherThanTriggering", Handled),
    }
}

/// Map a `StaticCondition` variant to its tag name and resolver-support class.
/// Handled variants are consumed by `static_abilities` / `layers` evaluation.
fn static_condition_feature(cond: &StaticCondition) -> (&'static str, FeatureSupport) {
    use FeatureSupport::*;
    match cond {
        StaticCondition::QuantityComparison { .. } => ("QuantityComparison", Handled),
        StaticCondition::DevotionGE { .. } => ("DevotionGE", Handled),
        StaticCondition::IsPresent { .. } => ("IsPresent", Handled),
        StaticCondition::ChosenColorIs { .. } => ("ChosenColorIs", Handled),
        StaticCondition::HasCounters { .. } => ("HasCounters", Handled),
        StaticCondition::ClassLevelGE { .. } => ("ClassLevelGE", Handled),
        StaticCondition::DuringYourTurn => ("DuringYourTurn", Handled),
        StaticCondition::SourceEnteredThisTurn => ("SourceEnteredThisTurn", Handled),
        StaticCondition::IsRingBearer => ("IsRingBearer", Handled),
        StaticCondition::RingLevelAtLeast { .. } => ("RingLevelAtLeast", Handled),
        StaticCondition::SourceIsTapped => ("SourceIsTapped", Handled),
        StaticCondition::SourceControllerEquals { .. } => ("SourceControllerEquals", Handled),
        StaticCondition::Unrecognized { .. } => ("Unrecognized", Handled),
        StaticCondition::None => ("None", Handled),
        // Variants below are parsed but not classified as handled by the prior registry.
        StaticCondition::HasMaxSpeed => ("HasMaxSpeed", Unhandled),
        StaticCondition::SpeedGE { .. } => ("SpeedGE", Unhandled),
        StaticCondition::And { .. } => ("And", Unhandled),
        StaticCondition::Or { .. } => ("Or", Unhandled),
        StaticCondition::Not { .. } => ("Not", Unhandled),
        StaticCondition::DefendingPlayerControls { .. } => ("DefendingPlayerControls", Unhandled),
        StaticCondition::SourceAttackingAlone => ("SourceAttackingAlone", Unhandled),
        StaticCondition::SourceIsAttacking => ("SourceIsAttacking", Unhandled),
        StaticCondition::SourceIsBlocking => ("SourceIsBlocking", Unhandled),
        StaticCondition::SourceIsBlocked => ("SourceIsBlocked", Unhandled),
        StaticCondition::IsMonarch => ("IsMonarch", Handled),
        StaticCondition::HasCityBlessing => ("HasCityBlessing", Handled),
        StaticCondition::CompletedADungeon => ("CompletedADungeon", Unhandled),
        StaticCondition::OpponentPoisonAtLeast { .. } => ("OpponentPoisonAtLeast", Unhandled),
        StaticCondition::UnlessPay { .. } => ("UnlessPay", Unhandled),
        StaticCondition::ControlsCommander => ("ControlsCommander", Unhandled),
        StaticCondition::SourceIsEquipped => ("SourceIsEquipped", Unhandled),
        StaticCondition::SourceIsMonstrous => ("SourceIsMonstrous", Unhandled),
        StaticCondition::SourceAttachedToCreature => ("SourceAttachedToCreature", Unhandled),
        StaticCondition::SourceMatchesFilter { .. } => ("SourceMatchesFilter", Unhandled),
        StaticCondition::SourceInZone { .. } => ("SourceInZone", Unhandled),
        StaticCondition::EnchantedIsFaceDown => ("EnchantedIsFaceDown", Handled),
    }
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
        }
        | Effect::FlipCoins {
            win_effect,
            lose_effect,
            ..
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
/// Finds the first positional occurrence in the string so that "+2/+2 ... +1/+1 counter"
/// correctly identifies +2/+2 as the primary modifier (not +1/+1 which is a counter).
fn extract_pt_modifier(lower: &str) -> Option<(i32, i32)> {
    // Find the earliest +N/ or -N/ pattern by scanning for sign+digits+slash
    let idx = lower.char_indices().find_map(|(i, c)| {
        if c != '+' && c != '-' {
            return None;
        }
        let rest = &lower[i + 1..]; // after the sign
        let digit_end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if digit_end == 0 {
            return None;
        }
        // Check if the char after digits is '/'
        if rest.as_bytes().get(digit_end) == Some(&b'/') {
            Some(i)
        } else {
            None
        }
    })?;

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

/// Returns true when the +N/+M counter mention in the Oracle line is NOT a counter-placement
/// effect — i.e., it's a filter, condition, cost, quantity reference, replacement, or quoted
/// sub-ability context. These should not be flagged as WrongParameter when no PutCounter
/// effect is found on the matched element.
fn is_non_effect_counter_context(lower: &str) -> bool {
    // Cost context: "+1/+1 counter" appears before a colon (ability cost, not effect)
    if let Some(colon_pos) = lower.find(':') {
        if let Some(counter_pos) = lower.find("counter") {
            // Only suppress if the counter mention is entirely in the cost portion
            if counter_pos < colon_pos {
                return true;
            }
        }
    }

    // Filter/condition phrases where the counter is a qualifier, not an operation
    let filter_phrases = [
        "with a +",
        "with a -",
        "with +",
        "with -",
        "with two +",
        "with two -",
        "with three +",
        "with three -",
        "with four +",
        "with five +",
        "with x +",
        "with that many +",
        "has a +",
        "has a -",
        "have a +",
        "have a -",
        "has five or more",
        "unless it has",
        "doesn't have a +",
        "doesn't have a -",
        "as long as",
        "each creature you control with",
        "each creature you control that has",
        "creatures you control with three or more",
        "creatures you control with",
    ];
    for phrase in &filter_phrases {
        if lower.contains(phrase) {
            // Ensure the +N/+N counter mention is actually near this phrase
            if let Some(phrase_pos) = lower.find(phrase) {
                // Look for counter mention after this phrase
                let after = &lower[phrase_pos..];
                if after.contains("counter") {
                    return true;
                }
            }
        }
    }

    // Quantity/for-each references: "number of +1/+1 counters", "for each +1/+1 counter"
    if lower.contains("number of") && lower.contains("counter") {
        return true;
    }
    if lower.contains("for each") && lower.contains("counter") {
        return true;
    }

    // Enters-with / escapes-with replacement: parsed as replacement, not PutCounter
    if (lower.contains("enters with")
        || lower.contains("enter with")
        || lower.contains("escapes with"))
        && lower.contains("counter")
    {
        return true;
    }

    // "remove ... counter" as the main verb (not cost) — removal, not placement
    if lower.contains("remove a +")
        || lower.contains("remove a -")
        || lower.contains("remove all +")
        || lower.contains("remove all -")
    {
        return true;
    }

    // Conditional/replacement: "if you would put ... counters" or "if you've put ... counters"
    if (lower.contains("if you would put") || lower.contains("if you've put"))
        && lower.contains("counter")
    {
        return true;
    }

    // "one or more +1/+1 counters are put" / "would be put" — trigger condition, not effect
    if lower.contains("counters are put") || lower.contains("counters would be put") {
        return true;
    }

    // Trigger conditions referencing counters (not placement effects):
    // "counter is put on" / "put one or more +1/+1 counters on" as trigger conditions
    if lower.contains("counter is put") || lower.contains("counter on it,") {
        return true;
    }
    // "whenever you put one or more +N/+N counters on" — trigger condition, not placement
    if lower.contains("you put one or more") && lower.contains("counter") {
        return true;
    }
    // "you may remove two +1/+1 counters" — removal, not placement
    if lower.contains("may remove") && lower.contains("counter") {
        return true;
    }
    // "had a +1/+1 counter" / "without a +1/+1 counter" — state checks, not placement
    if lower.contains("had a +")
        || lower.contains("had a -")
        || lower.contains("without a +")
        || lower.contains("without a -")
    {
        return true;
    }
    // "prevent that damage and put ... counters" — prevention replacement with counter placement
    if lower.contains("prevent") && lower.contains("counter") {
        return true;
    }
    // "additional +1/+1 counter" — enters-with-additional replacement, not direct PutCounter
    if lower.contains("additional +") || lower.contains("additional -") {
        return true;
    }
    // "remove a ... counter" with phrasing variants
    if lower.contains("remove a pupa counter")
        || lower.contains("remove a time counter")
        || lower.contains("remove a counter")
    {
        return true;
    }

    // Quoted sub-ability: counter mention inside granted ability text
    if let Some(quote_pos) = lower.find('"') {
        if let Some(counter_pos) = lower.find("counter") {
            if counter_pos > quote_pos {
                return true;
            }
        }
    }

    // "distribute ... counters" — different effect type than PutCounter
    if lower.contains("distribute") && lower.contains("counter") {
        return true;
    }
    // "instead put ... counters" / "put ... counters ... instead" — replacement effect
    if lower.contains("instead") && lower.contains("counter") {
        return true;
    }

    false
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
            ParsedElement::Ability(a) => {
                // Check the ability's own description first
                if let Some(desc) = a.description.as_ref() {
                    return Some(desc.to_lowercase());
                }
                // Fallback: for GenericEffect abilities with no top-level description,
                // concatenate nested static ability descriptions so the matcher can find
                // lines like "can't be blocked except by creatures with flying" or
                // "all creatures able to block this creature do so".
                if let Effect::GenericEffect {
                    static_abilities, ..
                } = &*a.effect
                {
                    let descs: Vec<String> = static_abilities
                        .iter()
                        .filter_map(|s| s.description.as_ref().map(|d| d.to_lowercase()))
                        .collect();
                    if !descs.is_empty() {
                        return Some(descs.join("; "));
                    }
                }
                None
            }
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
    // Replace the full card name (or comma-truncated/word-prefix form) with ~
    let mut result = lower.to_string();
    if !card_name_lower.is_empty() {
        // Try full name first
        result = result.replace(card_name_lower, "~");
        // Alchemy rebalance prefix: "a-armory veteran" → try "armory veteran"
        if !result.contains('~') {
            if let Some(stripped) = card_name_lower.strip_prefix("a-") {
                result = result.replace(stripped, "~");
            }
        }
        // Comma-truncated: "akiri, line-slinger" → "akiri"
        if let Some(short) = card_name_lower.split(',').next() {
            let short = short.trim();
            if short.len() > 2 {
                result = result.replace(short, "~");
                // Also try with Alchemy prefix stripped: "a-alrund" → "alrund"
                if !result.contains('~') {
                    if let Some(stripped) = short.strip_prefix("a-") {
                        if stripped.len() > 2 {
                            result = result.replace(stripped, "~");
                        }
                    }
                }
            }
        }
        // "of"-based: "rosie cotton of south lane" → "rosie cotton"
        if !result.contains('~') {
            if let Some(of_pos) = card_name_lower.find(" of ") {
                let short = &card_name_lower[..of_pos];
                if short.len() >= 3 {
                    result = result.replace(short, "~");
                }
            }
        }
        // First-word prefix: "bontu the glorified" → try "bontu the", "bontu"
        // Mirrors the parser's normalize_card_name_refs short-name strategy.
        // Always runs (even if `~` is already present from the parser) to ensure
        // consistent normalization between oracle lines and parsed descriptions.
        // Skips common MTG game terms that would cause false matches.
        {
            const GAME_TERM_BLOCKLIST: &[&str] = &[
                "quest", "spirit", "heart", "edge", "wall", "lake", "dream", "herald", "champion",
                "guardian", "master", "prophet", "bringer",
            ];
            let name_words: Vec<&str> = card_name_lower.split_whitespace().collect();
            for len in (1..name_words.len()).rev() {
                let candidate: String = name_words[..len].join(" ");
                if candidate.len() >= 3 {
                    // Skip single-word candidates that are common MTG game terms
                    if len == 1 && GAME_TERM_BLOCKLIST.contains(&candidate.as_str()) {
                        continue;
                    }
                    let replaced = result.replace(candidate.as_str(), "~");
                    if replaced != result {
                        result = replaced;
                        break;
                    }
                }
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
        "this equipment",
        "this vehicle",
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
    // CR 614.1a: A sub_ability chained via `ConditionInstead` (and similar
    // AbilityCondition wrappers) carries its own Oracle line text — e.g. an
    // "Infusion — If you gained life this turn, destroy all creatures instead."
    // line attached to the primary PumpAll ability on Withering Curse. The
    // per-line audit must match sub_ability descriptions too, otherwise such
    // lines are falsely reported as SilentDrop.
    fn push_ability_tree<'a>(def: &'a AbilityDefinition, out: &mut Vec<ParsedElement<'a>>) {
        out.push(ParsedElement::Ability(def));
        for mode_ab in &def.mode_abilities {
            out.push(ParsedElement::Ability(mode_ab));
        }
        if let Some(sub) = &def.sub_ability {
            push_ability_tree(sub, out);
        }
        if let Some(else_ab) = &def.else_ability {
            push_ability_tree(else_ab, out);
        }
    }
    for a in face.abilities.iter() {
        push_ability_tree(a, &mut elements);
    }
    for t in &face.triggers {
        elements.push(ParsedElement::Trigger(t));
        if let Some(exec) = &t.execute {
            push_ability_tree(exec, &mut elements);
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

        // Skip modal header lines ("Choose one —", "{cost}: Choose two —", etc.)
        if is_modal_header_line(&lower) {
            continue;
        }

        // Skip "Spree" keyword lines (the keyword itself, not the mode lines)
        if lower.starts_with("spree") {
            continue;
        }

        // Skip saga reminder text lines (already stripped of parens, but
        // sometimes "as this saga enters..." survives)
        if lower.starts_with("as this saga enters") {
            continue;
        }

        // Skip Case card "To solve —" condition lines (structural, like saga chapter markers)
        if lower.starts_with("to solve") {
            continue;
        }

        // Skip level-up header lines ("LEVEL 1-7", "LEVEL 8+")
        if lower.starts_with("level ")
            && lower[6..]
                .chars()
                .all(|c| c.is_ascii_digit() || c == '-' || c == '+')
        {
            continue;
        }

        // Skip Day/Night reminder lines ("If it's neither day nor night, it becomes day...")
        if lower.starts_with("if it's neither day nor night")
            || lower.starts_with("if it\u{2019}s neither day nor night")
        {
            continue;
        }

        // Strip structural prefixes (bullet, saga chapter, spree mode,
        // attraction/dungeon) to get the semantic effect text for matching.
        let effective_lower = strip_structural_prefix(&lower).unwrap_or_else(|| lower.clone());

        // Normalize self-references for matching
        let norm = normalize_for_matching(&effective_lower, &card_name_lower);

        // --- Match this line to parsed element(s) ---
        let matched: Vec<&ParsedElement<'_>> = elements
            .iter()
            .filter(|e| {
                if let Some(desc) = e.description_lower() {
                    // Try both raw and normalized matching against the effective text.
                    // Also normalize the description side to handle cases where the
                    // description has card-name references that norm also replaced.
                    let desc_norm = normalize_for_matching(&desc, &card_name_lower);
                    desc.contains(effective_lower.as_str())
                        || effective_lower.contains(desc.as_str())
                        || desc.contains(&norm)
                        || norm.contains(desc.as_str())
                        || desc_norm.contains(&norm)
                        || norm.contains(desc_norm.as_str())
                } else {
                    false
                }
            })
            .collect();

        // Check if this line's text matches any modal mode_description.
        // Collects matching mode abilities so property checks (duration, pump,
        // counter) can inspect them even when the top-level ability description
        // doesn't match the Oracle line.
        let modal_matched_abilities: Vec<&AbilityDefinition> = {
            let norm_modal = normalize_for_matching(&effective_lower, &card_name_lower);
            let desc_matches = |desc: &str| {
                let dl = desc.to_lowercase();
                let dn = normalize_for_matching(&dl, &card_name_lower);
                dl.contains(effective_lower.as_str())
                    || effective_lower.contains(dl.as_str())
                    || dl.contains(norm_modal.as_str())
                    || norm_modal.contains(dl.as_str())
                    || dn.contains(effective_lower.as_str())
                    || effective_lower.contains(dn.as_str())
            };
            let mut modal_abs: Vec<&AbilityDefinition> = Vec::new();
            // Collect from card-level modal + top-level abilities (spell modes)
            if let Some(ref modal) = face.modal {
                for (i, desc) in modal.mode_descriptions.iter().enumerate() {
                    if desc_matches(desc) {
                        if let Some(ab) = face.abilities.get(i) {
                            modal_abs.push(ab);
                        }
                    }
                }
            }
            // Collect from ability-level modals (activated/triggered modal abilities)
            for a in face.abilities.iter() {
                if let Some(ref modal) = a.modal {
                    for (i, desc) in modal.mode_descriptions.iter().enumerate() {
                        if desc_matches(desc) {
                            if let Some(ab) = a.mode_abilities.get(i) {
                                modal_abs.push(ab);
                            }
                        }
                    }
                }
            }
            // Collect from trigger execute modals
            for t in &face.triggers {
                if let Some(ref exec) = t.execute {
                    if let Some(ref modal) = exec.modal {
                        for (i, desc) in modal.mode_descriptions.iter().enumerate() {
                            if desc_matches(desc) {
                                if let Some(ab) = exec.mode_abilities.get(i) {
                                    modal_abs.push(ab);
                                }
                            }
                        }
                    }
                }
            }
            modal_abs
        };
        let covered_by_modal = !modal_matched_abilities.is_empty();

        // Check if this line matches a saga chapter trigger's effect
        let covered_by_saga = is_saga_chapter_line(&lower) && !face.triggers.is_empty();

        // Check if this is an attraction/dungeon/level-up line with parsed abilities.
        // Level-up effect lines (N+ | ...) are structural parts of leveler cards and
        // are always considered covered (the level-up keyword itself governs them).
        let covered_by_attraction = is_attraction_line(&lower)
            && (!face.abilities.is_empty()
                || !face.triggers.is_empty()
                || is_level_effect_line(&lower));

        // Also check if this line is covered by keywords, casting restrictions, or
        // other non-ability structured data
        let covered_by_keyword = face.keywords.iter().any(|k| {
            let kw_name = format!("{k:?}").to_lowercase();
            lower.starts_with(&kw_name)
        }) || is_keyword_line(&lower);
        let covered_by_casting =
            !face.casting_restrictions.is_empty() && lower.starts_with("cast this spell only ");
        // Casting option lines ("You may pay X rather than pay...", "If you control a
        // commander, you may cast this spell without paying its mana cost", etc.)
        let covered_by_casting_option = !face.casting_options.is_empty()
            && (effective_lower.contains("rather than pay")
                || effective_lower.contains("without paying")
                || effective_lower.contains("as though it had flash")
                || effective_lower.contains("you may cast this spell for")
                || effective_lower.contains("you may pay")
                || effective_lower.contains("you can't spend mana to cast"));
        let covered_by_additional_cost = face.additional_cost.is_some()
            && (lower.starts_with("as an additional cost ")
                || effective_lower.starts_with("as an additional cost ")
                || effective_lower.contains("behold"));
        // Enchant keyword lines ("Enchant creature", "Enchant land you control")
        let covered_by_enchant = lower.starts_with("enchant ");
        // Replacement effects with matching descriptions (enter-tapped, etc.)
        let covered_by_replacement = face.replacements.iter().any(|r| {
            r.description.as_ref().is_some_and(|d| {
                let dl = d.to_lowercase();
                let dn = normalize_for_matching(&dl, &card_name_lower);
                dl.contains(effective_lower.as_str())
                    || effective_lower.contains(dl.as_str())
                    || dn.contains(effective_lower.as_str())
                    || effective_lower.contains(dn.as_str())
                    || dl.contains(&norm)
                    || norm.contains(dl.as_str())
            })
        });

        // Static abilities matched by mode pattern when description matching fails.
        // Covers "you may cast/play ... from" (GraveyardCastPermission) and
        // "can't cast spells during" (CantCastDuring/PerTurnCastLimit) lines.
        let covered_by_static_mode = face.static_abilities.iter().any(|s| match &s.mode {
            StaticMode::GraveyardCastPermission { .. } => {
                effective_lower.contains("you may cast") || effective_lower.contains("you may play")
            }
            StaticMode::CantCastDuring { .. } => {
                effective_lower.contains("can't cast spells during")
                    || effective_lower.contains("can cast spells only during")
            }
            StaticMode::PerTurnCastLimit { .. } => {
                effective_lower.contains("can't cast more than")
                    || effective_lower.contains("cast no more than")
            }
            StaticMode::CantBeCast { .. } => {
                effective_lower.contains("can't cast") && !effective_lower.contains("during")
            }
            StaticMode::CantCastFrom => effective_lower.contains("can't cast"),
            StaticMode::RevealTopOfLibrary { .. } => {
                effective_lower.contains("play with the top card")
                    || effective_lower.contains("play with the top")
            }
            StaticMode::ReduceCost { .. } => {
                effective_lower.contains("cost") && effective_lower.contains("less")
            }
            StaticMode::RaiseCost { .. } => {
                effective_lower.contains("cost") && effective_lower.contains("more")
            }
            StaticMode::CantBeCountered => effective_lower.contains("can't be countered"),
            StaticMode::CantBeCopied => effective_lower.contains("can't be copied"),
            // CR 119.7: "can't gain life" or its compound form "life total can't change"
            // (Platinum Emperion / Teferi's Protection both emit CantGainLife from
            // the bidirectional life-lock phrase).
            StaticMode::CantGainLife => {
                effective_lower.contains("can't gain life")
                    || effective_lower.contains("life total can't change")
                    || effective_lower.contains("life totals can't change")
            }
            // CR 119.8: "can't lose life" or the compound life-lock phrase.
            StaticMode::CantLoseLife => {
                effective_lower.contains("can't lose life")
                    || effective_lower.contains("life total can't change")
                    || effective_lower.contains("life totals can't change")
            }
            StaticMode::CantLoseTheGame => {
                effective_lower.contains("don't lose the game")
                    || effective_lower.contains("can't lose the game")
            }
            StaticMode::CantWinTheGame => effective_lower.contains("can't win the game"),
            StaticMode::NoMaximumHandSize => effective_lower.contains("no maximum hand size"),
            StaticMode::MaximumHandSize { .. } => effective_lower.contains("maximum hand size is"),
            StaticMode::CantUntap => {
                effective_lower.contains("doesn't untap") || effective_lower.contains("don't untap")
            }
            StaticMode::CantAttack => effective_lower.contains("can't attack"),
            StaticMode::CantBlock => effective_lower.contains("can't block"),
            StaticMode::CantAttackOrBlock => effective_lower.contains("can't attack or block"),
            StaticMode::CastWithFlash => {
                effective_lower.contains("as though it had flash")
                    || effective_lower.contains("as though they had flash")
            }
            StaticMode::MayChooseNotToUntap => effective_lower.contains("may choose not to untap"),
            StaticMode::CantDraw { .. } => effective_lower.contains("can't draw"),
            StaticMode::PerTurnDrawLimit { .. } => effective_lower.contains("can't draw more than"),
            StaticMode::DoubleTriggers { .. } => {
                effective_lower.contains("triggers an additional time")
                    || effective_lower.contains("trigger an additional time")
            }
            StaticMode::CantBeBlocked => effective_lower.contains("can't be blocked"),
            StaticMode::CantBeBlockedExceptBy { .. } => {
                effective_lower.contains("can't be blocked")
            }
            StaticMode::CantBeBlockedBy { .. } => effective_lower.contains("can't be blocked"),
            StaticMode::CanAttackWithDefender => {
                effective_lower.contains("as though it didn't have defender")
            }
            _ => false,
        });

        // Check if an ability's GenericEffect contains a static mode matching the line.
        // Covers patterns like "All creatures able to block this creature do so" which
        // are parsed as GenericEffect with nested MustBeBlocked static, not top-level statics.
        let covered_by_ability_static_mode = face.abilities.iter().any(|a| {
            if let Effect::GenericEffect {
                static_abilities, ..
            } = &*a.effect
            {
                static_abilities.iter().any(|s| match &s.mode {
                    StaticMode::MustBeBlocked => {
                        effective_lower.contains("able to block")
                            && effective_lower.contains("do so")
                    }
                    StaticMode::CanAttackWithDefender => {
                        effective_lower.contains("as though it didn't have defender")
                    }
                    _ => false,
                })
            } else {
                false
            }
        });

        // Abilities matched by effect type when they lack a description.
        // Covers "damage can't be prevented" (AddRestriction/DamagePreventionDisabled),
        // "you may cast ... from" (CastFromZone), and similar patterns where the parser
        // produces the correct effect but doesn't attach a description string.
        let covered_by_ability_effect_type = face.abilities.iter().any(|a| {
            let pred = |d: &AbilityDefinition| match &*d.effect {
                Effect::AddRestriction { restriction, .. } => {
                    matches!(
                        restriction,
                        GameRestriction::DamagePreventionDisabled { .. }
                    ) && effective_lower.contains("can't be prevented")
                        && effective_lower.contains("damage")
                }
                Effect::CastFromZone { .. } => {
                    effective_lower.contains("you may cast")
                        || effective_lower.contains("you may play")
                }
                Effect::GiftDelivery { .. } => {
                    effective_lower.contains("gift was promised")
                        || effective_lower.contains("gift wasn't promised")
                }
                Effect::GenericEffect { .. } => {
                    // "don't lose unspent mana" parsed as GenericEffect
                    effective_lower.contains("don't lose unspent")
                }
                Effect::LoseTheGame => {
                    // "You don't lose the game for ..." parsed as LoseTheGame prevention
                    effective_lower.contains("don't lose the game")
                        || effective_lower.contains("can't lose the game")
                }
                Effect::PayCost { .. } => {
                    // "You may pay {X} rather than pay ..." — alternative cost patterns
                    effective_lower.contains("rather than pay")
                }
                Effect::TapAll { .. } => {
                    effective_lower.contains("tap") && !effective_lower.contains("untap")
                }
                Effect::UntapAll { .. } => effective_lower.contains("untap"),
                Effect::PreventDamage { .. } => {
                    // "If a source would deal damage to you, prevent N of that damage"
                    // Parsed as PreventDamage without a description string.
                    effective_lower.contains("prevent") && effective_lower.contains("damage")
                }
                Effect::CopySpell { .. } => {
                    // "You may have this creature enter as a copy of ..." lines
                    // (including "enter tapped as a copy of")
                    // Parsed as CopySpell without a description string.
                    effective_lower.contains("as a copy of")
                }
                Effect::Untap { .. } => {
                    // "Untap this creature during each other player's untap step" and similar
                    // Parsed as Untap without a description string.
                    effective_lower.contains("untap")
                        && (effective_lower.contains("untap step")
                            || effective_lower.contains("during each"))
                }
                Effect::Pump { .. } | Effect::PumpAll { .. } => {
                    // "All Saprolings get +1/+1" or "This creature gets +X/+X" lines
                    // Parsed as Pump/PumpAll without a description string.
                    (effective_lower.contains("get ") || effective_lower.contains("gets "))
                        && (effective_lower.contains('+') || effective_lower.contains('-'))
                        && effective_lower.contains('/')
                }
                _ => false,
            };
            ability_tree_any(a, &pred)
        });

        // Replacement effects matched by event type when description doesn't align.
        // Covers "prevent ... damage", "enters with ... counter", damage redirection,
        // and any "would ... instead" replacement effect pattern.
        let covered_by_replacement_event = face.replacements.iter().any(|r| match r.event {
            ReplacementEvent::DamageDone | ReplacementEvent::DealtDamage => {
                (effective_lower.contains("prevent") && effective_lower.contains("damage"))
                    || (effective_lower.contains("damage") && effective_lower.contains("instead"))
                    || effective_lower.contains("damage can't be prevented")
            }
            ReplacementEvent::ChangeZone | ReplacementEvent::Moved => {
                ((effective_lower.contains("enters with")
                    || effective_lower.contains("enter with"))
                    && effective_lower.contains("counter"))
                    || (effective_lower.contains("would") && effective_lower.contains("instead"))
                    || (effective_lower.contains("enters tapped"))
                    || (effective_lower.contains("enters untapped"))
                    || (effective_lower.contains("enter untapped"))
                    || (effective_lower.contains("enter tapped"))
            }
            ReplacementEvent::Discard => {
                effective_lower.contains("discard") && effective_lower.contains("instead")
            }
            ReplacementEvent::Draw | ReplacementEvent::DrawCards => {
                effective_lower.contains("draw") && effective_lower.contains("instead")
            }
            ReplacementEvent::Destroy => {
                effective_lower.contains("destroy") && effective_lower.contains("instead")
            }
            ReplacementEvent::GainLife => {
                effective_lower.contains("gain") && effective_lower.contains("instead")
            }
            ReplacementEvent::LoseLife => {
                effective_lower.contains("lose") && effective_lower.contains("instead")
            }
            ReplacementEvent::CreateToken => {
                effective_lower.contains("token") && effective_lower.contains("instead")
            }
            ReplacementEvent::AddCounter => {
                effective_lower.contains("counter") && effective_lower.contains("instead")
            }
            ReplacementEvent::ProduceMana => {
                effective_lower.contains("tapped for mana") && effective_lower.contains("instead")
            }
            _ => {
                // Generic fallback: any replacement with "would...instead" pattern
                effective_lower.contains("would") && effective_lower.contains("instead")
            }
        });
        // Broad "would...instead" lines with any replacement on the card
        let covered_by_any_replacement = !face.replacements.is_empty()
            && effective_lower.contains("would")
            && effective_lower.contains("instead");

        // Lines that are entirely within quotes are granted sub-abilities —
        // they are parsed as part of the parent static/trigger ability.
        let covered_by_quoted = {
            let trimmed = effective_lower.trim();
            // If the Oracle line itself is a quoted string from a parent line
            // (e.g. an ability grants a creature an ability in quotes),
            // check if ANY ability/trigger/static description contains this text.
            let is_inside_parent_quotes = face.abilities.iter().any(|a| {
                a.description.as_ref().is_some_and(|d| {
                    let dl = d.to_lowercase();
                    dl.contains(trimmed) && dl.contains('"')
                })
            }) || face.static_abilities.iter().any(|s| {
                s.description.as_ref().is_some_and(|d| {
                    let dl = d.to_lowercase();
                    dl.contains(trimmed) && dl.contains('"')
                })
            }) || face.triggers.iter().any(|t| {
                t.description.as_ref().is_some_and(|d| {
                    let dl = d.to_lowercase();
                    dl.contains(trimmed) && dl.contains('"')
                }) || t.execute.as_ref().is_some_and(|e| {
                    e.description.as_ref().is_some_and(|d| {
                        let dl = d.to_lowercase();
                        dl.contains(trimmed) && dl.contains('"')
                    })
                })
            });
            is_inside_parent_quotes
        };

        if matched.is_empty()
            && !covered_by_keyword
            && !covered_by_casting
            && !covered_by_casting_option
            && !covered_by_additional_cost
            && !covered_by_enchant
            && !covered_by_replacement
            && !covered_by_replacement_event
            && !covered_by_any_replacement
            && !covered_by_modal
            && !covered_by_saga
            && !covered_by_attraction
            && !covered_by_static_mode
            && !covered_by_ability_static_mode
            && !covered_by_ability_effect_type
            && !covered_by_quoted
        {
            // Unmatched line → SilentDrop (only for substantive lines)
            if effective_lower.len() > 20 {
                findings.push(SemanticFinding::SilentDrop {
                    oracle_line: line.to_string(),
                });
            }
            continue;
        }

        // Keyword/cost definition lines are structural — skip property checks
        // since they don't represent in-game effects with durations or P/T values.
        // Saga chapter lines, attraction lines, and quoted sub-abilities are also
        // structural matches that can't be checked against individual parsed elements.
        if matched.is_empty()
            && (covered_by_keyword
                || covered_by_enchant
                || covered_by_casting
                || covered_by_casting_option
                || covered_by_additional_cost
                || covered_by_saga
                || covered_by_attraction
                || covered_by_quoted)
        {
            continue;
        }

        // --- Check matched element(s) for expected properties ---
        // Use the FIRST matched element for property checks (most specific match).
        // If multiple match, any having the property is sufficient.
        // For modal lines, also check the matched mode abilities directly.

        // Helper: check if any modal-matched ability satisfies a predicate via ability_tree_any
        let modal_any = |pred: &dyn Fn(&AbilityDefinition) -> bool| -> bool {
            modal_matched_abilities
                .iter()
                .any(|a| ability_tree_any(a, &|d| pred(d)))
        };

        // 1. Condition check: does Oracle text contain condition language?
        if let Some(cond_label) = line_has_condition_text(&lower) {
            // Skip condition check for replacement effects — the "if" is inherently
            // part of the replacement's applicability condition (e.g., "If you control
            // two or more other lands, this land enters tapped."), not an ability condition.
            let all_replacements = !matched.is_empty()
                && matched
                    .iter()
                    .all(|e| matches!(e, ParsedElement::Replacement(_)));
            let any_has_condition = matched.iter().any(|e| e.has_condition() || e.has_unless())
                || modal_any(&|d: &AbilityDefinition| d.condition.is_some());
            if !any_has_condition
                && !covered_by_casting
                && !all_replacements
                && !covered_by_replacement
                && !covered_by_replacement_event
                && !covered_by_any_replacement
            {
                findings.push(SemanticFinding::DroppedCondition {
                    oracle_line: line.to_string(),
                    condition_text: cond_label.to_string(),
                });
            }
        }

        // 2. Duration check: does Oracle text contain duration language?
        if let Some(dur_label) = line_has_duration_text(&lower) {
            let any_has_duration = matched.iter().any(|e| e.has_duration())
                || modal_any(&|d: &AbilityDefinition| d.duration.is_some())
                // Fallback: for saga chapter lines, the matched element may be a static
                // but the duration lives on the trigger's execute ability. Check all triggers.
                || face.triggers.iter().any(|t| {
                    t.execute
                        .as_ref()
                        .is_some_and(|e| ability_tree_any(e, &|d| d.duration.is_some()))
                });
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
            // Skip if the +N/+M pattern is inside a quoted sub-ability
            let pt_in_quotes = lower_for_pt
                .find('"')
                .zip(lower_for_pt.find(&format!("{}{}/", if power >= 0 { "+" } else { "" }, power)))
                .is_some_and(|(quote_pos, pt_pos)| pt_pos > quote_pos);

            // Check if the +N/+M is preceded by "additional" — this is a conditional
            // addendum to a base pump on the same line, not independently checkable.
            let pt_is_additional = {
                let pt_str = format!("{}{}/", if power >= 0 { "+" } else { "" }, power);
                lower_for_pt
                    .find(&pt_str)
                    .is_some_and(|pos| pos >= 11 && lower_for_pt[..pos].contains("additional"))
            };

            if power == 0 && toughness == 0 {
                // +0/+0 is meaningless, skip
            } else if pt_in_quotes || pt_is_additional {
                // +N/+M is inside a quoted sub-ability — not a property of this line's element
            } else if is_counter_reference(&lower_for_pt) {
                // Skip false positives: counter mentioned in filter, condition, cost,
                // quantity reference, replacement, or quoted sub-ability context
                if !is_non_effect_counter_context(&lower_for_pt) {
                    let normalized =
                        crate::parser::oracle_effect::counter::normalize_counter_type(&format!(
                            "{}{}/{}{}",
                            if power >= 0 { "+" } else { "" },
                            power,
                            if toughness >= 0 { "+" } else { "" },
                            toughness
                        ));
                    let any_has_counter = matched.iter().any(|e| e.has_counter_effect(&normalized))
                        || modal_any(&|d: &AbilityDefinition| {
                            matches!(
                                &*d.effect,
                                Effect::PutCounter { counter_type: ct, .. }
                                | Effect::PutCounterAll { counter_type: ct, .. }
                                if ct == &normalized
                            )
                        });
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
                }
            } else {
                let any_has_pump = matched.iter().any(|e| e.has_pump(power, toughness))
                    || modal_any(&|d: &AbilityDefinition| pump_matches_oracle(d, power, toughness));
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

/// Returns true if the condition keyword appears after a sentence boundary (".", ". then "),
/// indicating it's a resolve-time conditional branch within effect text, not an
/// ability-gating condition.
/// E.g., "Draw a card. If you have the city's blessing, draw three cards instead."
fn is_resolve_time_conditional_branch(lower: &str, condition_phrase: &str) -> bool {
    // Find the position of the condition phrase
    let cond_pos = match lower.find(condition_phrase) {
        Some(pos) if pos == 0 || !lower.as_bytes()[pos - 1].is_ascii_alphabetic() => pos,
        _ => return false,
    };
    // Check if there's a sentence boundary before the condition phrase
    // Look for ". " before the condition phrase position
    lower[..cond_pos].contains(". ")
}

/// Returns true if the condition keyword appears inside a quoted sub-ability string.
/// E.g. `enchanted creature has "... if you control a Swamp ..."` — the "if" is inside
/// the granted ability's text, not a condition on the granting ability itself.
fn condition_inside_quotes(lower: &str, condition_phrase: &str) -> bool {
    if let Some(quote_pos) = lower.find('"') {
        if let Some(cond_pos) = lower.find(condition_phrase) {
            return cond_pos > quote_pos;
        }
    }
    false
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
        // Word-boundary check: ensure the phrase occurs at the start of the string or
        // after a non-alphabetic character (prevents "Phelddagrif gains" matching "if ").
        let has_phrase = lower
            .find(phrase)
            .is_some_and(|pos| pos == 0 || !lower.as_bytes()[pos - 1].is_ascii_alphabetic());
        if !has_phrase {
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
            // --- Conditional effect branches (resolve-time checks, not ability conditions) ---
            // "if it's a creature card" / "if it is a land" — reveal-and-check patterns
            || lower.contains("if it's a ")
            || lower.contains("if it is a ")
            || lower.contains("if it isn't a ")
            || lower.contains("if it's not a ")
            // "if that <noun>" — resolve-time state checks on a referenced object
            // (spell, land, permanent, creature, card, player, mana, equipment, etc.)
            || lower.contains("if that ")
            // "if they do" / "if they don't" — opponent/player action results
            || lower.contains("if they do")
            || lower.contains("if they don't")
            // "if you can't" / "if the player can't" — failure path, not a gating condition
            || lower.contains("if you can't")
            || lower.contains("if the player can't")
            // "if you chose" / "if you choose" — modal choice results
            || lower.contains("if you chose")
            || lower.contains("if you choose")
            // --- Replacement/prevention patterns (not ability conditions) ---
            // "if damage would be dealt" / "if noncombat damage would" — damage replacement
            || lower.contains("would be dealt")
            || lower.contains("would deal ")
            // "prevent that damage" with "if" — prevention replacement clause
            || (lower.contains("prevent that damage") && lower.contains("if "))
            // --- Mana/casting condition patterns (casting-time, not board-state conditions) ---
            // "if {U} was spent to cast" — mana-spent conditions
            || (lower.contains("was spent") && lower.contains("if "))
            // "if you cast" / "if it was [state]" / "if he was cast" — casting/state conditions
            || lower.contains("if you cast")
            || lower.contains("if it was ")
            || lower.contains("if he was cast")
            || lower.contains("if she was cast")
            // "if this spell was cast/foretold/etc." — casting-condition checks on self
            || lower.contains("if this spell")
            // --- Casting cost conditionals (part of casting system, not ability conditions) ---
            // "this spell costs {2} less to cast if..." — cost reduction conditions
            || lower.contains("this spell costs")
            || lower.contains("spells cost")
            || lower.contains("starting player")
            // "if the {cost} cost was paid" / "if its madness cost was paid"
            || lower.contains("cost was paid")
            // --- Duration patterns (audited by DroppedDuration, not DroppedCondition) ---
            // "for as long as" is a duration, not a condition
            || lower.contains("for as long as")
            // --- Resolve-time property checks (not gating conditions on the ability) ---
            // "if it has flying" / "if it has a counter" — state check on result object
            || lower.contains("if it has ")
            // "if its mana value" / "if its power" / "if its toughness"
            || lower.contains("if its mana value")
            || lower.contains("if its power")
            || lower.contains("if its toughness")
            // "if there's" / "if there is" / "if there are" — board state checks at resolution
            || lower.contains("if there's ")
            || lower.contains("if there is ")
            || lower.contains("if there are ")
            // --- Unless-pay patterns (cost alternatives, not ability conditions) ---
            || lower.contains("unless you pay")
            || lower.contains("unless a player")
            || lower.contains("unless its controller")
            || lower.contains("unless their controller")
            || lower.contains("unless that player")
            // --- Unless-action patterns (trigger-level sacrifice/discard alternatives) ---
            // "sacrifice X unless you Y" — the "unless" is part of the effect, not a
            // gating condition. These are audited for effect correctness, not condition presence.
            || lower.contains("unless you sacrifice")
            || lower.contains("unless you discard")
            || lower.contains("unless you exile")
            || lower.contains("unless you return")
            || lower.contains("unless you tap")
            || lower.contains("unless you reveal")
            || lower.contains("unless you remove")
            || lower.contains("unless you compliment")
            || lower.contains("unless they sacrifice")
            || lower.contains("unless they discard")
            || lower.contains("unless they exile")
            || lower.contains("unless they pay")
            || lower.contains("unless they return")
            || lower.contains("unless any player pays")
            // "unless [subject] control(s)" — resolve-time board-state check
            || lower.contains("unless you control")
            || lower.contains("unless they control")
            || lower.contains("unless it controls")
            // "unless [subject] has/have" — resolve-time state check
            || lower.contains("unless they have")
            || lower.contains("unless you have")
            // "unless [you] say" — Un-set flavor requirement
            || lower.contains("unless you say")
            // "unless [you] put" — resolve-time action alternative
            || lower.contains("unless you put")
            // "unless [subject] is/was" — resolve-time state check
            || lower.contains("unless it's")
            || lower.contains("unless it is")
            // "unless [game state condition]" — board-state gate
            || lower.contains("unless one of")
            || lower.contains("unless either")
            || lower.contains("unless defending player")
            // "unless that spell's controller" — resolve-time spell-controller check
            || lower.contains("unless that spell")
            || lower.contains("unless that creature")
            // "unless they're mana abilities" — structural restriction qualifier
            || lower.contains("unless they're mana abilities")
            // "unless {W} was spent" / "unless two or more colors of mana were spent" — casting conditions
            || (lower.contains("unless") && lower.contains("was spent"))
            || (lower.contains("unless") && lower.contains("were spent"))
            // --- Reminder text in parentheses (not part of the ability's condition) ---
            || lower.contains("(if ")
            || lower.contains("(unless ")
            // --- Cost-result conditionals (resolve-time checks on what was paid/sacrificed) ---
            // "if the sacrificed creature was a Human" / "if the discarded card was..."
            || lower.contains("if the sacrificed")
            || lower.contains("if the discarded")
            || lower.contains("if the exiled")
            // --- Enchanted/equipped state checks (resolve-time, not ability conditions) ---
            || lower.contains("if enchanted")
            || lower.contains("if equipped")
            // --- Replacement effect "if ... would" — already caught by the
            // "would ... instead" check but also catch standalone "if X would be destroyed"
            || lower.contains("would be destroyed")
            // --- Leyline / opening-hand structural patterns ---
            || lower.contains("in your opening hand")
            // --- Resolution-count conditions (not board-state gating) ---
            // "if this is the second time" / "if it's the third time" — ability resolution count
            || lower.contains("if this is the ")
            || lower.contains("if it's the second")
            || lower.contains("if it's the third")
            || lower.contains("if it's the first")
            // --- "Landfall — If you had a land enter" — keyword ability name, not standalone condition ---
            || lower.contains("if you had a land enter")
            // --- Team-based / event-based conditions (Archenemy, special events) ---
            || lower.contains("if you're on the")
            || lower.contains("if the mirrans")
            || lower.contains("if the phyrexians")
            // --- "Coven — If you control three or more creatures with different powers" ---
            // Coven is a keyword ability; the "if" is its intervening-if, but these are
            // typically on triggers that the auditor already checks. The ability description
            // uses the keyword name, not a standalone condition. Mark as structural.
            || (lower.starts_with("coven") && lower.contains("if "))
            // --- Activation/resolution count conditions ---
            || lower.contains("this ability has been activated")
            // --- Zone-referential conditions (structural, not board-state) ---
            // "if this card is suspended" / "if this card is in your graveyard"
            || lower.contains("is suspended")
            || lower.contains("if this card is in your")
            // --- Coin flip / die roll resolve-time results ---
            || lower.contains("if you lose the flip")
            || lower.contains("if you win the flip")
            || lower.contains("if the result")
            // --- Beheld mechanic: resolve-time check on previous action ---
            || lower.contains("beheld")
            // --- Color checks at resolution (not ability gating conditions) ---
            // "counter target spell if it's red" — resolve-time type/color check
            || lower.contains("if it's red")
            || lower.contains("if it's blue")
            || lower.contains("if it's green")
            || lower.contains("if it's white")
            || lower.contains("if it's black")
            || lower.contains("if it's colorless")
            // --- Self-state checks (resolve-time property queries on this object) ---
            // "if this creature is/has/didn't" — state of the source at resolution
            || lower.contains("if this creature is")
            || lower.contains("if this creature has")
            || lower.contains("if this creature didn't")
            || lower.contains("if this enchantment has")
            || lower.contains("if this enchantment is")
            || lower.contains("if this artifact is")
            || lower.contains("if this artifact has")
            || lower.contains("if this permanent is")
            || lower.contains("if this permanent has")
            // --- Turn-action resolve-time conditions ---
            // "if you attacked this turn" / "if you attacked with" — turn event checks
            || lower.contains("if you attacked")
            // "if you haven't cast a spell" / "if you didn't cast" — turn-action checks
            || lower.contains("if you haven't cast")
            || lower.contains("if you didn't cast")
            || lower.contains("if you didn't play")
            // "if a creature died this turn" — turn-event checks
            || lower.contains("if a creature died")
            // "if a permanent left the battlefield" — Void mechanic turn-event
            || lower.contains("if a permanent left")
            || lower.contains("if a nonland permanent left")
            || lower.contains("a spell was warped")
            // --- Object property checks at resolution ---
            // "if it shares a" — property comparison at resolution
            || lower.contains("if it shares")
            // "if it doesn't have" / "if it had no" — state check on result object
            || lower.contains("if it doesn't have")
            || lower.contains("if it had no")
            // "if it's on the battlefield" — zone check at resolution
            || lower.contains("if it's on the battlefield")
            // "this way" — resolve-time checks on what happened during resolution
            // "if you reveal a creature card this way" / "if a card is put into a graveyard this way"
            || lower.contains("this way")
            // "if it's paired" — paired state check at resolution
            || lower.contains("if it's paired")
            || lower.contains("if it is paired")
            // --- Object-referential resolve-time checks ---
            // "if you controlled that [object]" — state of the destroyed/exiled object
            || lower.contains("if you controlled that")
            // "if the player does" — player action result at resolution
            || lower.contains("if the player does")
            // "if defending player" — combat-time checks (not board-state gating)
            || lower.contains("if defending player")
            // "if [subject] is dealt damage" — resolve-time damage check
            || lower.contains("is dealt damage")
            // "if fewer than" / "if exactly" — resolve-time count checks
            || lower.contains("if fewer than")
            || lower.contains("if exactly ")
            // "if X is N or more" — X-spell resolve-time variable checks
            || lower.contains("if x is ")
            // "if it attacked or blocked this turn" — resolve-time combat state
            || lower.contains("if it attacked")
            || lower.contains("if it blocked")
            || lower.contains("it wasn't blocking")
            // "if the discovered card's" — resolve-time check on discovered card
            || lower.contains("if the discovered")
            // "if it's night" / "if it's day" — day/night state check (not ability gating)
            || lower.contains("if it's night")
            || lower.contains("if it's day")
            // "if it's an instant or sorcery" — resolve-time card type check
            || lower.contains("if it's an instant")
            || lower.contains("if it's a sorcery")
            // "if it isn't being declared" — replacement timing check
            || lower.contains("isn't being declared")
            // --- Resolve-time conditional branches in multi-sentence effect text ---
            // When "if" appears after a period ("."), it's a resolve-time branch within
            // the effect resolution, not an ability-gating condition.
            // E.g., "Draw a card. If you have the city's blessing, draw three instead."
            || is_resolve_time_conditional_branch(lower, phrase)
            // --- Turn-event resolve-time checks ("if you've [past tense]") ---
            // "if you've drawn three or more cards this turn" — turn-event tallies
            || lower.contains("if you've drawn")
            || lower.contains("if you've cast")
            || lower.contains("if you've put")
            || lower.contains("if you've gained")
            // "if you gained life this turn" / "if you lost life" — turn-event checks
            || lower.contains("if you gained life")
            || lower.contains("if you lost life")
            // --- Corruption/poison-based resolve-time checks ---
            // "if an opponent has three or more poison counters" — corrupted mechanic
            || lower.contains("poison counter")
            // --- Phase-check resolve-time conditions ---
            // "if it's your combat phase" / "if it's your main phase"
            || lower.contains("if it's your combat")
            || lower.contains("if it's your main")
            // --- Ability name keyword prefixes (not standalone conditions) ---
            // "Eminence — ..., if X is in the command zone" — keyword ability, condition is structural
            || lower.starts_with("eminence")
            // "Corrupted — ..., if an opponent has" — keyword ability prefix
            || lower.starts_with("corrupted")
            // --- Additional resolve-time state checks ---
            // "if a graveyard has twenty or more" — zone-state check at resolution
            || lower.contains("if a graveyard has")
            // "if it entered" / "if it entered under" — ETB state check at resolution
            || lower.contains("if it entered")
            // "if it's your turn" is already excluded, but also:
            // "if mana was/were spent" — already excluded
            // "if an opponent" followed by verb — resolve-time opponent-state check
            || lower.contains("if an opponent lost")
            || lower.contains("if an opponent discarded")
            // "if you control a [planeswalker name]" — resolve-time planeswalker check
            || (lower.contains("if you control a ") && lower.contains("planeswalker"))
            // "if you have a full party" — party mechanic resolve-time check
            || lower.contains("if you have a full party")
            // "if you have the city's blessing" — ascend mechanic resolve-time check
            || lower.contains("city's blessing")
            || lower.contains("city\u{2019}s blessing")
            // "if no mana was spent" — resolve-time casting check
            || lower.contains("if no mana was spent")
            // "if another permanent with the same name" — resolve-time board check
            || lower.contains("with the same name")
            // --- Gotcha mechanic (Un-sets) — structural, not game conditions ---
            || lower.contains("gotcha")
            // --- Ability word prefixes with conditions (resolve-time trigger conditions) ---
            || lower.starts_with("ferocious")
            || lower.starts_with("formidable")
            || lower.starts_with("hellbent")
            || lower.starts_with("morbid")
            || lower.starts_with("revolt")
            || lower.starts_with("threshold")
            || lower.starts_with("delirium")
            || lower.starts_with("metalcraft")
            || lower.starts_with("ascend")
            || lower.starts_with("domain")
            || lower.starts_with("spell mastery")
            // --- Panharmonicon-style conditions ---
            // "if [event] causes a triggered ability ... to trigger" — this is a static
            // ability condition (Panharmonicon), not an ability-gating condition.
            || lower.contains("causes a triggered ability")
            // "if an ability of a [type] triggers" — Panharmonicon variant
            || (lower.contains("if an ability of") && lower.contains("triggers"))
            // --- "if [it] isn't legendary" — copy exception clause, not ability condition ---
            || lower.contains("isn't legendary")
            // --- Meld conditions ("if you both own and control") — structural meld trigger ---
            || lower.contains("if you both own and control")
            // --- Target-referential conditions ("if it targets a") — resolve-time check ---
            || lower.contains("if it targets")
            // --- Exact-count conditions ("if you have exactly N") — win condition / resolve-time ---
            || lower.contains("if you have exactly")
            || lower.contains("if target player has exactly")
            // --- Total power conditions ("if creatures you control have total power") ---
            || lower.contains("total power")
            // --- Class/type transformation conditions (resolve-time state checks) ---
            // "if [name] is a Scout/Citizen/Detective" — leveler/class evolution checks
            || lower.contains(" is a scout")
            || lower.contains(" is a citizen")
            || lower.contains(" is a detective")
            // --- Turn-event tallies (resolve-time, not ability gating) ---
            // "if a counter was put on" — turn-event counter check
            || lower.contains("if a counter was put")
            // "if you sacrificed a permanent" — turn-event action check
            || lower.contains("if you sacrificed")
            // "if you gained or lost life" — combined life change check
            || lower.contains("if you gained or lost")
            // "if a land you controlled was put into a graveyard" — turn-event zone check
            || lower.contains("if a land you controlled was put")
            // "if the amount of mana spent" — mana-spent magnitude check
            || lower.contains("the amount of mana spent")
            // "if it didn't have" — resolve-time past-state check on object
            || lower.contains("if it didn't have")
            // "if you control another" — resolve-time board state check on object count
            || lower.contains("if you control another")
            // "if a triggered ability" — trigger-ability interaction (Panharmonicon variant)
            || lower.contains("if a triggered ability")
            // "if you haven't completed" — dungeon/quest state check
            || lower.contains("if you haven't completed")
            // --- Combat restriction "unless" patterns (resolve-time, not ability conditions) ---
            // "can't attack unless at least two" — combat restriction qualifier
            || lower.contains("unless at least")
            // "unless a creature with greater power" — combat restriction comparator
            || lower.contains("unless a creature with greater")
            // --- Board-state conditions in triggers (intervening-if, resolve-time) ---
            // "if [name] is in your graveyard or on the battlefield" — zone presence check
            || lower.contains("is in your graveyard")
            || lower.contains("is on the battlefield")
            // "if you control the creature with the greatest power" — comparator resolve check
            || lower.contains("the creature with the greatest")
            || lower.contains("the greatest power")
            // "if you have more cards in hand" — hand-size comparison check
            || lower.contains("more cards in hand")
            // "if you have four or more creature cards in your graveyard" — threshold-style
            || lower.contains("cards in your graveyard")
            // "if another creature entered the battlefield" — turn-event ETB check
            || lower.contains("if another creature entered")
            // "if you control an untapped land" — board state check
            || lower.contains("if you control an untapped")
            // "if you control an enchanted creature" / "if you control an equipped creature"
            || lower.contains("if you control an enchanted")
            || lower.contains("if you control an equipped")
            // "if you control an artifact and an enchantment" — multi-type board check
            || lower.contains("if you control an artifact and")
            // --- Reveal/check resolve-time patterns ---
            // "if you revealed a dragon card" — reveal-check cast-time condition
            || lower.contains("if you revealed")
            // "if you didn't attack with a creature this turn" — turn-action check
            || lower.contains("if you didn't attack")
            // "if an opponent has cast a spell" — opponent cast-action check
            || lower.contains("if an opponent has cast")
            // "if an opponent is the monarch" — special designation check
            || lower.contains("if an opponent is the monarch")
            // "if [you/player] controls more/fewer" — comparative board checks
            || lower.contains("controls more")
            || lower.contains("controls fewer")
            || lower.contains("control no ")
            // "if [subject] regenerated this turn" — turn-event state check
            || lower.contains("regenerated this turn")
            // "if three or more creatures died" — turn-event death count
            || lower.contains("creatures died this turn")
            // "if each player has an empty library" — zone-state check
            || lower.contains("has an empty library")
            // "if you control thirty or more" — threshold count check
            || lower.contains("you control thirty")
            || lower.contains("you control 200")
            || lower.contains("200 or more")
            // "if an artifact or creature was put" — turn-event zone check
            || lower.contains("was put into a graveyard")
            || lower.contains("were put into")
            // "if a player lost 4 or more life" — turn-event life loss check
            || lower.contains("a player lost")
            // "if this creature doesn't have a +1/+1 counter" — state check
            || lower.contains("doesn't have a +")
            // "if you cycled" — turn-event action check
            || lower.contains("if you cycled")
            // "if evidence was collected" — keyword mechanic resolve-time check
            || lower.contains("evidence was collected")
            // "if three or more cards were put into your graveyard" — turn-event zone check
            || lower.contains("cards were put into your graveyard")
            // "if an aura you controlled was attached" — turn-event attachment check
            || lower.contains("aura you controlled was attached")
            // "if a card left your graveyard" — turn-event zone check
            || lower.contains("a card left your graveyard")
            // "unless [subject] sacrifices" / "unless [opponent] pays" — already mostly covered
            // "unless he has" / "unless she has" — state check on target
            || lower.contains("unless he has")
            || lower.contains("unless she has")
            // "your team controls" — team-based check
            || lower.contains("your team controls")
            // "if it doesn't share a keyword" — property comparison check
            || lower.contains("doesn't share a keyword")
            // "if you control a desert or there is a desert" — multi-state board check
            || lower.contains("if you control a desert")
            // "if [name] is in the command zone" — command zone state check
            || lower.contains("in the command zone")
            // "if you control your commander" — commander-zone check
            || lower.contains("if you control your commander")
            // "if you had no cards in hand" — turn-start state check
            || lower.contains("had no cards in hand")
            // "if you have the initiative" — special designation check
            || lower.contains("you have the initiative")
            // "if no permanents left the battlefield" — turn-event check
            || lower.contains("no permanents left")
            // "if [this card is] the only creature card in your graveyard" — zone state check
            || lower.contains("only creature card in your graveyard")
            // "if you discarded a card this turn" — turn-event action check
            || lower.contains("if you discarded")
            // "if 4 or more damage was dealt" — turn-event damage check
            || lower.contains("damage was dealt to it")
            // "if each player has 10 or less life" — life total threshold
            || lower.contains("each player has 10")
            // "if it had power greater than" — resolve-time power comparison
            || lower.contains("it had power greater")
            // "if it had one or more +1/+1 counters" — resolve-time state check
            || lower.contains("it had one or more")
            // "if its controller is poisoned" — poison state check
            || lower.contains("controller is poisoned")
            // "if there were three or more card types" — resolve-time threshold
            || lower.contains("three or more card types")
            // "if all your commanders have been revealed" — commander reveal state
            || lower.contains("commanders have been revealed")
            // "if you control permanents with names" — win condition check
            || lower.contains("permanents with names")
            // "if a player has more life than each other player" — comparator check
            || lower.contains("more life than each other")
            || lower.contains("more creatures than")
            // "if an ability of a ninja creature" — ninja trigger interaction
            || lower.contains("ability of a ninja")
            // "if an opponent controls a swamp" — land-type board check
            || lower.contains("controls a swamp")
            || lower.contains("controls a plains")
            || lower.contains("controls a forest")
            || lower.contains("controls a mountain")
            || lower.contains("controls a island")
            // "unless [it/they] attacked or blocked" — combat state check
            || lower.contains("unless it attacked")
            || lower.contains("unless it blocked")
            // "unless target opponent pays" — payment alternative
            || lower.contains("unless target opponent pays")
            || lower.contains("unless target opponent sacrifices")
            // "if you have a card in hand" — resolve-time hand check
            || lower.contains("if you have a card in hand")
            // "if you pay {N} more to cast" — additional cost condition (casting option)
            || lower.contains("more to cast")
            // "if [subject] dealt damage" — turn-event damage check
            || lower.contains("dealt damage to an opponent this turn")
            || lower.contains("dealt damage to a player this turn")
            // "if one or more of them entered from a graveyard" — origin-zone check
            || lower.contains("entered from a graveyard")
            || lower.contains("was cast from a graveyard")
            || lower.contains("were cast from a graveyard")
            // --- "as long as" combat conditions (structural, not board-state gating) ---
            // "as long as it's attacking alone" — combat state qualifier
            || lower.contains("attacking alone")
            // "as long as you're the monarch" — special designation check
            || lower.contains("you're the monarch")
            // "as long as [name] is equipped" — equipment state check
            || lower.contains("is equipped")
            // --- Replacement effect "if [event]" patterns that start with "if" ---
            // "if a basic land you control is tapped for mana" — mana replacement
            || lower.contains("tapped for mana")
            // --- Quoted sub-abilities: condition is inside a granted ability, not on the granter ---
            || condition_inside_quotes(lower, phrase)
            // --- Turn-ownership conditions (not board-state gating) ---
            // "if it's not their turn" / "if it isn't that player's turn"
            || lower.contains("not their turn")
            || lower.contains("isn't that player's turn")
            || lower.contains("not that player's turn")
            // --- Source-state resolve-time checks ("if it's [modified/enchanted/etc.]") ---
            || lower.contains("if it's modified")
            || lower.contains("if it's enchanted")
            || lower.contains("if it's equipped")
            || lower.contains("if it's renowned")
            || lower.contains("if it's not suspected")
            || lower.contains("if it's tapped")
            || lower.contains("if it's outside")
            // "if it devoured a creature" — devour resolve-time check
            || lower.contains("devoured a creature")
            // --- "can't attack/block unless" — restriction qualifier, not ability condition ---
            || (lower.contains("can't attack") && lower.contains("unless"))
            || (lower.contains("can't block") && lower.contains("unless"))
            // --- Un-set flavor conditions ---
            || lower.contains("unless you insult")
            || lower.contains("unless they challenge")
            // --- Enchanted creature "unless" clauses (restriction qualifier) ---
            // "enchanted creature can't ... unless" — part of the restriction definition
            || (lower.starts_with("enchanted creature can't") && lower.contains("unless"))
            // --- "if [you] cast [it] from" — casting-origin condition ---
            || lower.contains("if you cast it from")
            || lower.contains("was cast from exile")
            // --- Triggered-ability intervening-if with "other than" (resolve-time filter) ---
            || lower.contains("other than your hand")
        {
            continue;
        }

        return Some(label);
    }

    None
}

/// Check if a line is a standalone keyword ability line (may be comma-separated).
/// Covers common keywords that don't always match the Keyword enum's Debug format.
/// Also covers keyword cost definition lines (escape, kicker, companion, cycling, equip, etc.)
/// which declare a cost or constraint rather than an in-game effect.
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
        "cumulative upkeep",
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
        // Keyword cost definition lines (not in-game effects)
        "escape\u{2014}", // em dash
        "escape —",
        "kicker ",
        "kicker\u{2014}",
        "companion \u{2014}",
        "companion\u{2014}",
        "friends forever",
        "prototype ",
        "overload ",
        "overload\u{2014}",
        "overload {",
        "bestow ",
        "bestow\u{2014}",
        "dash ",
        "dash\u{2014}",
        "emerge ",
        "emerge\u{2014}",
        "evoke ",
        "evoke\u{2014}",
        "ninjutsu ",
        "ninjutsu\u{2014}",
        "commander ninjutsu ",
        "commander ninjutsu\u{2014}",
        "craft with ",
        "craft\u{2014}",
        "disturb ",
        "disturb\u{2014}",
        "madness ",
        "madness\u{2014}",
        "miracle ",
        "miracle\u{2014}",
        "morph ",
        "morph\u{2014}",
        "megamorph ",
        "megamorph\u{2014}",
        "spectacle ",
        "spectacle\u{2014}",
        "encore ",
        "encore\u{2014}",
        "foretell ",
        "foretell\u{2014}",
        "blitz ",
        "blitz\u{2014}",
        "embalm ",
        "embalm\u{2014}",
        "eternalize ",
        "eternalize\u{2014}",
        "unearth ",
        "unearth\u{2014}",
        "flashback ",
        "flashback\u{2014}",
        "retrace ",
        "adapt ",
        "crew ",
        "reconfigure ",
        "channel\u{2014}",
        "channel ",
        "boast\u{2014}",
        "boast ",
        "scavenge ",
        "scavenge\u{2014}",
        "prowl ",
        "prowl\u{2014}",
        "buyback ",
        "buyback\u{2014}",
        "entwine ",
        "entwine\u{2014}",
        "amplify ",
        "bloodrush\u{2014}",
        "bloodrush ",
        "outlast ",
        "forecast\u{2014}",
        "forecast ",
        "transfigure ",
        "transmute ",
        "bargain",
        "casualty ",
        "connive",
        "exploit",
        "offspring ",
        "enlist",
        "living weapon",
        "living metal",
        "totem armor",
        "web-slinging ",
        "fabricate ",
        "investigate",
        "food ",
        "squad ",
        "backup ",
        "devour ",
        "modular ",
        "vanishing ",
        "fading ",
        "tribute ",
        "hideaway ",
        "storm",
        "annihilator ",
        "battle cry",
        "exalted",
        "soulbond",
        "evolve",
        "riot",
        "ascend",
        "afterlife ",
        "adventure ",
        "mobilize ",
        "gift ",
        // Additional keyword/ability-word patterns
        "impending ",
        "disguise ",
        "disguise\u{2014}",
        "champion a ",
        "champion an ",
        "echo\u{2014}",
        "echo {",
        "echo ",
        "splice onto ",
        "grandeur\u{2014}",
        "grandeur ",
        "more than meets the eye ",
        "more than meets the eye\u{2014}",
        "soulshift ",
        "level up ",
        "level up\u{2014}",
        "level up {",
        "plainswalk",
        "islandwalk",
        "swampwalk",
        "mountainwalk",
        "forestwalk",
        "regenerate",
        "phasing",
        "banding",
        "trample over planeswalkers",
        "suspend",
        "epic",
        "haunt",
        "gravestorm",
        "conspire",
        "retrace",
        "rebound",
        "miracle ",
        "cipher",
        "extort",
        "tribute ",
        "bolster ",
        "renown ",
        "skulk",
        "melee",
        "crew ",
        "partner with ",
        "mentor",
        "jump-start",
        "spectacle ",
        "escape\u{2014}",
        "escape ",
        "mutate ",
        "demonstrate",
        "decayed",
        "cleave ",
        "read ahead",
        "ravenous",
        "prototype ",
        "prototype\u{2014}",
        "collect evidence ",
        "saddle ",
        "harmonize ",
        "harmonize\u{2014}",
        "reinforce ",
        "reinforce\u{2014}",
        "recover\u{2014}",
        "recover—",
        "warp\u{2014}",
        "warp ",
    ];
    // Check if the line starts with any keyword (possibly comma-separated list)
    let trimmed = lower.trim().trim_end_matches('.');
    if KEYWORDS
        .iter()
        .any(|kw| trimmed.starts_with(kw) || trimmed == kw.trim())
    {
        return true;
    }
    // Cycling/landcycling keyword cost lines: "[type]cycling {cost}" patterns
    // e.g. "basic landcycling {2}", "mountaincycling {2}, forestcycling {2}"
    if trimmed.contains("cycling {") || trimmed.contains("cycling\u{2014}") {
        return true;
    }
    // Equip cost lines: "equip {N}", "equip legendary creature {N}", etc.
    // Only match simple cost declarations, not "equipped creature gets..." effect lines
    if trimmed.starts_with("equip") && trimmed.contains('{') && !trimmed.contains("equipped") {
        return true;
    }
    // Ability-word / named-ability patterns: "Word — Effect" or "Word Word — Effect"
    // These are ability words (Visit, Gotcha, Grandeur, etc.), named abilities
    // (Echo of the First Murder, Tragic Backstory, etc.), or variant cost abilities
    // (Max speed, Exhaust, Shieldwall, etc.).
    const ABILITY_WORDS: &[&str] = &[
        "visit",
        "gotcha",
        "max speed",
        "shieldwall",
        "body thief",
        "meet in reverse",
        "from the future",
        "tragic backstory",
        "collect evidence",
        "rope dart",
        "delirium",
        "hellbent",
        "threshold",
        "metalcraft",
        "morbid",
        "revolt",
        "ferocious",
        "formidable",
        "spell mastery",
        "raid",
        "domain",
        "converge",
        "will of the council",
        "council's dilemma",
        "lieutenant",
        "kinship",
        "fateful hour",
        "tempting offer",
        "join forces",
        "radiance",
        "chroma",
        "imprint",
        "grasp of fate",
        "eminence",
        "mono eminence",
        "bloodthirst",
        "landfall",
        "heroic",
        "inspired",
        "constellation",
        "rally",
        "cohort",
        "strive",
        "parley",
        "sweep",
        "grandeur",
        "channel",
        "bloodrush",
        "echo of",
    ];
    if let Some(prefix) = trimmed
        .find(" \u{2014} ")
        .map(|pos| &trimmed[..pos])
        .or_else(|| trimmed.find("\u{2014}").map(|pos| &trimmed[..pos]))
    {
        let prefix_lower = prefix.to_lowercase();
        if ABILITY_WORDS.iter().any(|aw| prefix_lower.starts_with(aw)) {
            return true;
        }
    }
    // Draft-related lines (Conspiracy cards, Un-sets)
    if trimmed.starts_with("reveal this card as you draft")
        || trimmed.starts_with("draft ")
        || trimmed.contains("you've drafted this draft round")
    {
        return true;
    }
    // "Reconfigure—Pay" or "reconfigure {" with alternative costs
    if trimmed.starts_with("reconfigure") {
        return true;
    }
    false
}

/// Check if an Oracle line contains duration language, returning the label if so.
/// Excludes duration phrases that appear only inside quoted sub-abilities.
fn line_has_duration_text(lower: &str) -> Option<&'static str> {
    // Exclusion: mana-retention phrases use "until end of turn" structurally
    // ("until end of turn, you don't lose this mana") — this is a mana pool rule,
    // not an effect duration that should appear in the duration field.
    if lower.contains("don't lose this mana")
        || lower.contains("you don't lose unspent")
        || lower.contains("don\u{2019}t lose this mana")
    {
        return None;
    }
    // Exclusion: "sacrifice it at the beginning of" — the duration is expressed
    // as a delayed trigger, not a Duration field on the ability itself.
    if lower.contains("sacrifice it at the beginning of")
        || lower.contains("sacrifice them at the beginning of")
    {
        return None;
    }
    // Exclusion: "[gets/has] ... until end of turn instead" — conditional upgrade
    // branches (e.g., "gets +2/+1 until end of turn instead"). The "instead" means
    // this is an alternative resolve-time path, not a guaranteed effect with a duration.
    if lower.contains("instead") && lower.contains("until end of turn") {
        return None;
    }
    // Exclusion: "play that card this turn" / "play ... for as long as" —
    // casting permissions where the duration is structural, not effect-based.
    if lower.contains("play that card this turn")
        || lower.contains("play it this turn")
        || (lower.contains("play") && lower.contains("for as long as"))
    {
        return None;
    }
    // Exclusion: "where x is" dynamic quantity pumps — the duration IS present
    // but the pump amount is dynamic and may not be parsed. The duration check
    // shouldn't fire just because the line mentions "until end of turn" in a
    // "gets +X/+X until end of turn, where X is" pattern.
    if lower.contains("where x is") || lower.contains("where x equals") {
        return None;
    }
    // Exclusion: "if ... was spent to cast" — mana-spent conditional pumps
    // where the condition makes the pump path-dependent.
    if lower.contains("was spent to cast") {
        return None;
    }
    // Exclusion: ability word prefixed lines — the condition is part of the
    // ability word pattern, and the duration is inside the conditional body.
    let duration_ability_words = [
        "coven",
        "landfall",
        "hellbent",
        "ferocious",
        "formidable",
        "descend",
        "grandeur",
        "lucky slots",
    ];
    for aw in &duration_ability_words {
        if lower.starts_with(aw) {
            return None;
        }
    }
    let duration_phrases: &[(&str, &str)] = &[
        ("until end of turn", "until end of turn"),
        ("until your next turn", "until your next turn"),
        ("for as long as ", "for as long as"),
        ("until end of combat", "until end of combat"),
    ];
    for &(phrase, label) in duration_phrases {
        if let Some(phrase_pos) = lower.find(phrase) {
            // Skip if the duration phrase is inside a quoted sub-ability
            if let Some(quote_pos) = lower.find('"') {
                if phrase_pos > quote_pos {
                    continue;
                }
            }
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
    use std::sync::Arc;

    use super::*;
    use crate::database::legality::{legalities_to_export_map, LegalityStatus};
    use crate::types::ability::{AbilityKind, Effect, ReplacementCondition, TargetFilter};
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

    /// Regression: [`check_subtype_lexicon`] must flag AddSubtype values
    /// that aren't in the printed-corpus lexicon, catching parser misfires
    /// where English filler words leak through as subtypes.
    #[test]
    fn check_subtype_lexicon_flags_unknown_subtype() {
        let mut face = CardFace {
            name: "Test".into(),
            ..Default::default()
        };
        face.abilities.push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GenericEffect {
                static_abilities: vec![StaticDefinition::continuous().modifications(vec![
                    ContinuousModification::AddSubtype {
                        subtype: "Dragon".into(),
                    },
                    ContinuousModification::AddSubtype {
                        subtype: "Gets".into(),
                    },
                ])],
                duration: None,
                target: None,
            },
        ));

        let valid: HashSet<String> = ["Dragon".to_string()].into_iter().collect();
        let mut missing = Vec::new();
        check_subtype_lexicon(&face, &valid, &mut missing);

        assert_eq!(
            missing,
            vec!["ParserMisfire:InvalidSubtype(Gets)".to_string()]
        );
    }

    #[test]
    fn check_subtype_lexicon_accepts_valid_subtypes() {
        let mut face = CardFace {
            name: "Test".into(),
            ..Default::default()
        };
        face.static_abilities
            .push(StaticDefinition::continuous().modifications(vec![
                ContinuousModification::AddSubtype {
                    subtype: "Assassin".into(),
                },
            ]));

        let valid: HashSet<String> = ["Assassin".to_string()].into_iter().collect();
        let mut missing = Vec::new();
        check_subtype_lexicon(&face, &valid, &mut missing);

        assert!(missing.is_empty());
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
        Arc::make_mut(&mut obj.abilities).push(crate::types::ability::AbilityDefinition::new(
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
        Arc::make_mut(&mut obj.abilities).push(crate::types::ability::AbilityDefinition::new(
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
            color_identity: vec![],
            scryfall_oracle_id: None,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: vec![],
            casting_options: vec![],
            solve_condition: None,
            parse_warnings: vec![],
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

    #[test]
    fn analyze_coverage_rolls_up_by_set() {
        // Two cards, overlapping sets: Alpha is supported and printed in
        // SET_A + SET_B; Beta is unsupported and printed in SET_B + SET_C.
        // Expected: SET_A = 1/1, SET_B = 1/2, SET_C = 0/1.
        let export = serde_json::json!({
            "alpha": {
                "name": "Alpha",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": [], "subtypes": [] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": legalities_to_export_map(&HashMap::from([
                    (LegalityFormat::Standard, LegalityStatus::Legal),
                ])),
                "printings": ["SET_A", "SET_B"],
            },
            "beta": {
                "name": "Beta",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": [], "subtypes": [] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [],
                "abilities": [{
                    "kind": "Spell",
                    "effect": { "type": "Unimplemented", "name": "beta_gap", "description": null },
                    "cost": null, "sub_ability": null, "duration": null, "description": null,
                    "target_prompt": null, "sorcery_speed": false, "condition": null,
                    "optional_targeting": false
                }],
                "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": legalities_to_export_map(&HashMap::from([
                    (LegalityFormat::Standard, LegalityStatus::Legal),
                ])),
                "printings": ["SET_B", "SET_C"],
            }
        })
        .to_string();

        let db = CardDatabase::from_json_str(&export).expect("test export should deserialize");
        let summary = analyze_coverage(&db);

        assert_eq!(
            summary.coverage_by_set.get("SET_A"),
            Some(&SetCoverageSummary {
                total_cards: 1,
                supported_cards: 1,
                coverage_pct: 100.0,
            })
        );
        assert_eq!(
            summary.coverage_by_set.get("SET_B"),
            Some(&SetCoverageSummary {
                total_cards: 2,
                supported_cards: 1,
                coverage_pct: 50.0,
            })
        );
        assert_eq!(
            summary.coverage_by_set.get("SET_C"),
            Some(&SetCoverageSummary {
                total_cards: 1,
                supported_cards: 0,
                coverage_pct: 0.0,
            })
        );
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
                    active_zones: vec![],
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
                    active_zones: vec![],
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

        let mut features: HashMap<String, FeatureSupport> = HashMap::new();
        extract_card_features(&face, &mut features);

        assert_eq!(
            features.get("condition:HasMaxSpeed"),
            Some(&FeatureSupport::Handled)
        );
        assert_eq!(
            features.get("player_scope:HighestSpeed"),
            Some(&FeatureSupport::Handled)
        );
        assert_eq!(
            features.get("quantity_ref:Speed"),
            Some(&FeatureSupport::Handled)
        );
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
                    target: TargetFilter::Controller,
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

    #[test]
    fn replacement_unrecognized_condition_counts_as_gap() {
        let mut face = make_face();
        face.replacements.push(
            ReplacementDefinition::new(ReplacementEvent::ChangeZone).condition(
                ReplacementCondition::Unrecognized {
                    text: "you revealed a Dragon card".to_string(),
                },
            ),
        );

        let gaps = card_face_gaps(&face);

        assert!(gaps
            .iter()
            .any(|gap| gap == "Replacement:Unrecognized(you revealed a Dragon card)"));
    }

    /// Regression: cards with a concrete `AdditionalCost` + one spell ability
    /// (e.g. Vicious Rivalry, Fix What's Broken) produce exactly one Oracle
    /// line for the "As an additional cost..." preamble. That line must be
    /// represented by a `ParsedItem` so that `count_effective_parsed_items`
    /// matches `count_effective_oracle_lines` and the silent-drop audit
    /// doesn't falsely flag the card as unsupported.
    #[test]
    fn additional_cost_emits_parsed_item_for_supported_cost() {
        let mut face = make_face();
        face.additional_cost = Some(AdditionalCost::Required(AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 0 },
        }));
        face.abilities.push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                damage_source: None,
            },
        ));

        let parse_details = build_parse_details_for_face(&face);
        // 1 ability + 1 additional-cost item = 2 parsed items, matching the
        // two Oracle lines ("As an additional cost..." + the spell effect).
        assert_eq!(count_effective_parsed_items(&parse_details), 2);

        let mut missing = Vec::new();
        check_silent_drops(
            &Some(
                "As an additional cost to cast this spell, pay X life.\n\
                 Destroy all artifacts and creatures with mana value X or less."
                    .to_string(),
            ),
            &parse_details,
            &mut missing,
        );
        assert!(
            missing.is_empty(),
            "supported additional cost should not trigger SilentDrop: {missing:?}"
        );
    }

    /// When the underlying additional cost is `Unimplemented`, the existing
    /// `Cost:Unimplemented` gap must still surface (used by `extract_gap_details`).
    #[test]
    fn additional_cost_unimplemented_still_surfaces_gap() {
        let mut face = make_face();
        face.additional_cost = Some(AdditionalCost::Required(AbilityCost::Unimplemented {
            description: "reveal a card with a red mana symbol in its mana cost".to_string(),
        }));

        let parse_details = build_parse_details_for_face(&face);
        let gaps = extract_gap_details(&parse_details);
        assert!(
            gaps.iter().any(|g| g.handler.starts_with("Cost:")),
            "unimplemented additional cost should surface as a gap: {gaps:?}"
        );
    }

    /// Regression: `count_effective_oracle_lines` must recognize modal
    /// headers with "choose up to four" (and higher cardinals) so spells
    /// like Moment of Reckoning don't inflate their Oracle-line count.
    #[test]
    fn count_effective_oracle_lines_recognizes_choose_up_to_four() {
        let text = "Choose up to four. You may choose the same mode more than once.\n\
                    \u{2022} Destroy target nonland permanent.\n\
                    \u{2022} Return target nonland permanent card from your graveyard to the battlefield.";
        // 1 modal header; both bullets fold into the header.
        assert_eq!(count_effective_oracle_lines(text), 1);
    }

    #[test]
    fn commander_permission_text_does_not_count_as_runtime_gap() {
        let parse_details = Vec::new();
        let mut missing = Vec::new();
        check_silent_drops(
            &Some("Teferi, Temporal Archmage can be your commander.".to_string()),
            &parse_details,
            &mut missing,
        );

        assert!(missing.is_empty());
        assert_eq!(
            count_effective_oracle_lines("Teferi, Temporal Archmage can be your commander."),
            0
        );
    }

    /// Regression: `AbilityCondition::IsYourTurn` is handled at runtime by
    /// `evaluate_condition`; the compiler-checked classifier must report it
    /// as `Handled` so cards like Rapier Wit aren't flagged as having an
    /// unhandled resolver feature.
    #[test]
    fn is_your_turn_condition_is_marked_handled() {
        let (name, support) = condition_feature(&AbilityCondition::IsYourTurn { negated: false });
        assert_eq!(name, "IsYourTurn");
        assert_eq!(
            support,
            FeatureSupport::Handled,
            "AbilityCondition::IsYourTurn must classify as Handled",
        );
    }
}
