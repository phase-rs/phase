//! CR 611.3a + CR 613.1d: type-changing statics with a "The same is true"
//! continuation, including Maskwood Nexus and the chosen-type family.

#[allow(unused_imports)]
use super::prelude::*;
use crate::parser::oracle_nom::error::oracle_err;

/// The grammatical scope named by the continuation's spell/card subjects.
/// This is deliberately separate from the battlefield antecedent: Oracle's
/// continuation supplies its own scope (for example, "creature spells"), so
/// it must not accidentally inherit `Other`, `Nontoken`, or `Sliver` from the
/// permanent-only antecedent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContinuationScope {
    Creature,
    NonlandPermanent,
    Card,
}

/// Parse the full, all-consuming two-sentence type-changing grammar:
///
/// "[As long as <condition>, ]<battlefield subjects> are <type change>. The
/// same is true for <spell subjects> and <owned card subjects>."
///
/// The result is intentionally ONE `StaticDefinition`. A static ability's
/// affected set is dynamic (CR 611.3a), including its battlefield, stack, and
/// card-zone arms, and the arms share one source/dependency timestamp.
pub(crate) fn parse_same_is_true_type_static(text: &str, lower: &str) -> Option<StaticDefinition> {
    let (parsed, _) = nom_on_lower(text, lower, |input| {
        all_consuming(parse_sentence).parse(input)
    })?;

    let mut definition = StaticDefinition::continuous()
        .affected(parsed.affected)
        .modifications(parsed.modifications)
        .description(text.to_string());
    if let Some(condition) = parsed.condition {
        definition = definition.condition(condition);
    }
    Some(definition)
}

/// Recognize the grammar prefix that belongs exclusively to this two-sentence
/// type-changing family. This deliberately does not consume the continuation
/// subjects: callers use it after [`parse_same_is_true_type_static`] fails to
/// prevent an older battlefield-only parser from accepting the antecedent and
/// silently discarding an incomplete "The same is true" tail.
///
/// CR 611.3a + CR 613.1d: a static continuous effect applies exactly to the
/// objects its complete text identifies; a malformed continuation must not
/// degrade into a narrower, partially modeled effect.
pub(crate) fn is_same_is_true_type_static_candidate(text: &str, lower: &str) -> bool {
    nom_on_lower(text, lower, parse_same_is_true_type_static_candidate_prefix).is_some()
}

fn parse_same_is_true_type_static_candidate_prefix(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        recognize((
            opt(terminated(
                preceded(tag("as long as "), nom_condition::parse_inner_condition),
                tag(", "),
            )),
            separated_list1(tag(" and "), parse_battlefield_subject),
            tag(" are "),
            parse_type_change,
            tag(". the same is true for "),
        )),
    )
    .parse(input)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedSameIsTrue {
    affected: TargetFilter,
    modifications: Vec<ContinuousModification>,
    condition: Option<StaticCondition>,
}

fn parse_sentence(input: &str) -> OracleResult<'_, ParsedSameIsTrue> {
    let (input, condition) = opt(terminated(
        preceded(tag("as long as "), nom_condition::parse_inner_condition),
        tag(", "),
    ))
    .parse(input)?;
    let (input, battlefield_filters) =
        separated_list1(tag(" and "), parse_battlefield_subject).parse(input)?;
    let (input, _) = tag(" are ").parse(input)?;
    let (input, modifications) = parse_type_change(input)?;
    let (input, _) = tag(". the same is true for ").parse(input)?;
    let (input, scope) = parse_spell_subject(input)?;
    let (input, _) = tag(" and ").parse(input)?;
    let (input, _) = parse_card_subject(scope, input)?;
    let (input, _) = opt(tag(".")).parse(input)?;

    let battlefield = union_filters(battlefield_filters);
    let spells = controlled_spell_filter(scope);
    let cards = owned_card_filter(scope);

    Ok((
        input,
        ParsedSameIsTrue {
            affected: TargetFilter::Or {
                filters: vec![battlefield, spells, cards],
            },
            modifications,
            condition,
        },
    ))
}

/// Parse the permanent-only antecedent and give every alternative an explicit
/// battlefield constraint. The continuation arms below have their own scopes.
fn parse_battlefield_subject(input: &str) -> OracleResult<'_, TargetFilter> {
    let (input, other) = opt(tag("other ")).parse(input)?;
    let (input, nontoken) = opt(tag("nontoken ")).parse(input)?;
    let (input, nonland) = opt(tag("nonland ")).parse(input)?;
    let (input, card_type) = nom_target::parse_type_filter_word(input)?;
    let (input, _) = tag(" you control").parse(input)?;

    let mut typed = TypedFilter::new(card_type).controller(ControllerRef::You);
    if nonland.is_some() {
        typed = typed.with_type(TypeFilter::Non(Box::new(TypeFilter::Land)));
    }

    let mut properties = vec![FilterProp::InZone {
        zone: Zone::Battlefield,
    }];
    if nontoken.is_some() {
        properties.push(FilterProp::NonToken);
    }
    if other.is_some() {
        properties.push(FilterProp::Another);
    }

    Ok((input, TargetFilter::Typed(typed.properties(properties))))
}

fn parse_type_change(input: &str) -> OracleResult<'_, Vec<ContinuousModification>> {
    alt((
        value(
            vec![ContinuousModification::AddAllCreatureTypes],
            tag("every creature type"),
        ),
        parse_chosen_type_change,
        value(
            vec![ContinuousModification::AddType {
                core_type: CoreType::Artifact,
            }],
            terminated(tag("artifacts"), tag(" in addition to their other types")),
        ),
        parse_additive_subtype_change,
        value(
            vec![
                ContinuousModification::AddType {
                    core_type: CoreType::Kindred,
                },
                ContinuousModification::AddSubtype {
                    subtype: "Goblin".to_string(),
                },
            ],
            terminated(
                tag("kindred goblins"),
                tag(" in addition to their other types"),
            ),
        ),
    ))
    .parse(input)
}

/// CR 205.3: An additive bare word that is neither an existing card type nor
/// the two-word Kindred Goblin conclusion is a subtype grant (Roshan's
/// "Assassins in addition to their other types"). `parse_type_filter_word`
/// supplies canonical subtype spelling and word-boundary validation.
fn parse_additive_subtype_change(input: &str) -> OracleResult<'_, Vec<ContinuousModification>> {
    let (input, type_filter) = nom_target::parse_type_filter_word(input)?;
    let (input, _) = tag(" in addition to their other types").parse(input)?;
    let TypeFilter::Subtype(subtype) = type_filter else {
        return Err(oracle_err(input));
    };
    Ok((input, vec![ContinuousModification::AddSubtype { subtype }]))
}

fn parse_chosen_type_change(input: &str) -> OracleResult<'_, Vec<ContinuousModification>> {
    let (input, _) = tag("the chosen ").parse(input)?;
    let (input, _) = opt(tag("creature ")).parse(input)?;
    let (input, _) = tag("type").parse(input)?;
    let (input, additive) = opt(preceded(
        tag(" in addition to "),
        alt((
            tag("their other creature types"),
            tag("their other types"),
            tag("its other creature types"),
            tag("its other types"),
        )),
    ))
    .parse(input)?;

    let modifications = if additive.is_some() {
        vec![ContinuousModification::AddChosenSubtype {
            kind: ChosenSubtypeKind::CreatureType,
        }]
    } else {
        vec![
            ContinuousModification::RemoveAllSubtypes {
                set: SubtypeSet::Creature,
            },
            ContinuousModification::AddChosenSubtype {
                kind: ChosenSubtypeKind::CreatureType,
            },
        ]
    };
    Ok((input, modifications))
}

fn parse_spell_subject(input: &str) -> OracleResult<'_, ContinuationScope> {
    alt((
        value(
            ContinuationScope::Creature,
            tag("creature spells you control"),
        ),
        value(
            ContinuationScope::NonlandPermanent,
            tag("permanent spells you control"),
        ),
        value(ContinuationScope::Card, tag("spells you control")),
    ))
    .parse(input)
}

fn parse_card_subject(scope: ContinuationScope, input: &str) -> OracleResult<'_, ()> {
    match scope {
        ContinuationScope::Creature => value(
            (),
            tag("creature cards you own that aren't on the battlefield"),
        )
        .parse(input),
        ContinuationScope::NonlandPermanent => value(
            (),
            tag("nonland permanent cards you own that aren't on the battlefield"),
        )
        .parse(input),
        ContinuationScope::Card => {
            value((), tag("cards that you own that aren't on the battlefield")).parse(input)
        }
    }
}

fn union_filters(filters: Vec<TargetFilter>) -> TargetFilter {
    match filters.as_slice() {
        [only] => only.clone(),
        _ => TargetFilter::Or { filters },
    }
}

fn controlled_spell_filter(scope: ContinuationScope) -> TargetFilter {
    let mut typed = continuation_typed_filter(scope).controller(ControllerRef::You);
    typed = typed.properties(vec![FilterProp::InZone { zone: Zone::Stack }]);
    TargetFilter::Typed(typed)
}

fn owned_card_filter(scope: ContinuationScope) -> TargetFilter {
    let typed = continuation_typed_filter(scope).properties(vec![
        FilterProp::Owned {
            controller: ControllerRef::You,
        },
        FilterProp::RepresentedByCard,
        FilterProp::InAnyZone {
            zones: vec![
                Zone::Library,
                Zone::Hand,
                Zone::Graveyard,
                Zone::Stack,
                Zone::Exile,
                Zone::Command,
            ],
        },
    ]);
    TargetFilter::Typed(typed)
}

fn continuation_typed_filter(scope: ContinuationScope) -> TypedFilter {
    match scope {
        ContinuationScope::Creature => TypedFilter::creature(),
        ContinuationScope::NonlandPermanent => {
            TypedFilter::permanent().with_type(TypeFilter::Non(Box::new(TypeFilter::Land)))
        }
        ContinuationScope::Card => TypedFilter::card(),
    }
}
