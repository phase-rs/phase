// CR 613.3d (Layer 4) — type-changing static abilities.

#[allow(unused_imports)]
use super::prelude::*;
#[allow(unused_imports)]
use super::support::*;

/// CR 607.2d: Parse a self-chosen type static ability line.
pub(crate) fn parse_self_chosen_type_static(input: &str) -> OracleResult<'_, ChosenSubtypeKind> {
    let (input, kind) = alt((
        value(ChosenSubtypeKind::BasicLandType, tag("~ is")),
        value(ChosenSubtypeKind::CreatureType, tag("this creature is")),
        value(ChosenSubtypeKind::BasicLandType, tag("this land is")),
        value(ChosenSubtypeKind::BasicLandType, tag("this permanent is")),
    ))
    .parse(input)?;
    let (input, _) = tag(" the chosen type").parse(input)?;
    let (input, _) = opt(preceded(
        tag(" in addition to "),
        terminated(alt((tag("its"), tag("their"))), tag(" other types")),
    ))
    .parse(input)?;
    let (input, _) = opt(tag(".")).parse(input)?;
    eof.parse(input)?;
    Ok((input, kind))
}

pub(crate) fn parse_enchanted_land_chosen_type_static(
    tp: &TextPair<'_>,
    description: &str,
) -> Option<StaticDefinition> {
    let ((), _) = nom_on_lower(
        tp.original,
        tp.lower,
        parse_enchanted_land_chosen_type_static_sentence,
    )?;

    Some(
        StaticDefinition::continuous()
            .affected(TargetFilter::Typed(
                TypedFilter::land().properties(vec![FilterProp::EnchantedBy]),
            ))
            .modifications(vec![ContinuousModification::SetChosenBasicLandType])
            .description(description.to_string()),
    )
}

pub(crate) fn parse_enchanted_land_chosen_type_static_sentence(
    input: &str,
) -> OracleResult<'_, ()> {
    let (input, _) = tag("enchanted land is the chosen type").parse(input)?;
    let (input, _) = opt(alt((
        tag(" and loses its other land types"),
        tag(" and loses its other types"),
    )))
    .parse(input)?;
    let (input, _) = opt(tag(".")).parse(input)?;
    eof.parse(input)?;
    Ok((input, ()))
}

/// CR 607.2d: Subject scope for "<[scope]> are/is the chosen [creature] type in
/// addition to [their/its] other types" statics (Arcane Adaptation, Lifecraft
/// Engine, Xenograft, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChosenCreatureTypeStaticScope {
    Creatures,
    EachCreature,
    VehicleCreatures,
    /// CR 109.2a: a description naming a card plus a zone ("each creature card in
    /// your graveyard") means a card matching it in that stated zone — so this scope
    /// selects creature CARDS in the owner's graveyard (CR 400.3 + CR 109.5), never
    /// permanents on the battlefield (Ashes of the Fallen).
    GraveyardCreatureCards,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChosenCreatureTypeApplication {
    Additive,
    Replacing,
}

impl ChosenCreatureTypeStaticScope {
    fn target_filter(self) -> TargetFilter {
        match self {
            Self::Creatures | Self::EachCreature => {
                TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
            }
            // CR 301.7 + CR 607.2d: Lifecraft Engine grants a creature subtype to
            // Vehicle permanents you control — not the Creature card type.
            Self::VehicleCreatures => TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Subtype("Vehicle".to_string()))
                    .controller(ControllerRef::You),
            ),
            // CR 109.2a: "each creature card in your graveyard" names a card plus a
            // zone, so it matches creature CARDS in that stated zone.
            //
            // CR 400.3 + CR 109.5: a graveyard is an owner-defined zone — a card only
            // ever rests in its owner's graveyard — and "your" on a card that has no
            // controller resolves to its OWNER (CR 109.5). So the subject is scoped by
            // ownership (`FilterProp::Owned`), NOT `.controller(...)`: off the
            // battlefield a card has no meaningful controller, and the continuous-effect
            // matcher (layers.rs) evaluates a Typed filter's `controller` against the
            // effective-controller field. `Owned` matches `obj.owner` directly, paired
            // with the `InAnyZone` zone fold to select creature cards in your graveyard.
            Self::GraveyardCreatureCards => {
                TargetFilter::Typed(TypedFilter::creature().properties(vec![
                    FilterProp::Owned {
                        controller: ControllerRef::You,
                    },
                    FilterProp::InAnyZone {
                        zones: vec![Zone::Graveyard],
                    },
                ]))
            }
        }
    }
}

pub(crate) fn parse_arcane_adaptation_chosen_type_static(
    tp: &TextPair<'_>,
    description: &str,
) -> Option<StaticDefinition> {
    let ((scope, application), _) = nom_on_lower(
        tp.original,
        tp.lower,
        parse_chosen_creature_type_static_sentence_with_scope,
    )?;

    // CR 205.1b (in-addition retain) vs CR 205.1a (SET replace) + CR 613.1d
    // (Layer 4) + CR 607.2d (chosen-link). The additive forms (Arcane Adaptation,
    // Lifecraft Engine, Xenograft) add the chosen creature type while retaining
    // existing subtypes. The SET form (Conspiracy: "Creatures you control are the
    // chosen type") REPLACES the existing creature subtypes, modeled by composing
    // RemoveAllSubtypes{Creature} (wipe) then AddChosenSubtype (re-add the chosen
    // type) — the IDENTICAL pattern parse_enchanted_is_type uses (Frogify/Lignify).
    // RemoveAllSubtypes{Creature} retains against state.all_creature_types, so the
    // added chosen subtype SURVIVES the wipe (CR 613.7a intra-static written order)
    // and an artifact creature keeps its artifact subtypes (CR 205.1a).
    let modifications = match application {
        ChosenCreatureTypeApplication::Additive => vec![ContinuousModification::AddChosenSubtype {
            kind: ChosenSubtypeKind::CreatureType,
        }],
        ChosenCreatureTypeApplication::Replacing => match scope {
            // The graveyard sibling shares the creature-card subject, so a SET
            // printing would replace its creature types identically (no such
            // printing exists today — Ashes of the Fallen is additive).
            ChosenCreatureTypeStaticScope::Creatures
            | ChosenCreatureTypeStaticScope::EachCreature
            | ChosenCreatureTypeStaticScope::GraveyardCreatureCards => {
                vec![
                    ContinuousModification::RemoveAllSubtypes {
                        set: crate::types::card_type::SubtypeSet::Creature,
                    },
                    ContinuousModification::AddChosenSubtype {
                        kind: ChosenSubtypeKind::CreatureType,
                    },
                ]
            }
            // CR 301.7: a Vehicle-subtype grant has no known SET printing
            // (Lifecraft Engine is always additive). Fall back to additive so a
            // hypothetical non-additive vehicle line is never silently wiped of
            // its non-creature subtypes.
            ChosenCreatureTypeStaticScope::VehicleCreatures => {
                vec![ContinuousModification::AddChosenSubtype {
                    kind: ChosenSubtypeKind::CreatureType,
                }]
            }
        },
    };

    Some(
        StaticDefinition::continuous()
            .affected(scope.target_filter())
            .modifications(modifications)
            .description(description.to_string()),
    )
}

/// CR 305.6 + CR 607.2d + CR 613.1d (Layer 4): "Lands you control are the chosen
/// [land] type in addition to their other types" — the basic-land-type axis
/// sibling of [`parse_arcane_adaptation_chosen_type_static`] (which parameterizes
/// the CR 205.3g creature-subtype axis). Realmwright ("As ~ enters, choose a
/// basic land type. Lands you control are the chosen type in addition to their
/// other types.") is the type specimen. Additive only (CR 205.1b): the chosen
/// basic land type is added while each affected land RETAINS its existing
/// subtypes. Reuses the existing `AddChosenSubtype { kind: BasicLandType }`
/// runtime (game/layers.rs), the land-axis counterpart of the creature path's
/// `kind: CreatureType` — no new variant, no new runtime.
pub(crate) fn parse_chosen_land_type_static(
    tp: &TextPair<'_>,
    description: &str,
) -> Option<StaticDefinition> {
    nom_on_lower(
        tp.original,
        tp.lower,
        parse_chosen_land_type_static_sentence,
    )?;
    Some(
        StaticDefinition::continuous()
            .affected(TargetFilter::Typed(
                TypedFilter::land().controller(ControllerRef::You),
            ))
            .modifications(vec![ContinuousModification::AddChosenSubtype {
                kind: ChosenSubtypeKind::BasicLandType,
            }])
            .description(description.to_string()),
    )
}

/// nom body for [`parse_chosen_land_type_static`]: "lands you control are the
/// chosen [land ]type in addition to their other types[.]", consumed to `eof`
/// so a partial prefix can never mis-claim a longer line.
fn parse_chosen_land_type_static_sentence(input: &str) -> OracleResult<'_, ()> {
    let (input, _) = tag("lands you control are the chosen ").parse(input)?;
    let (input, _) = opt(tag("land ")).parse(input)?;
    let (input, _) = tag("type in addition to their other types").parse(input)?;
    let (input, _) = opt(tag(".")).parse(input)?;
    eof.parse(input)?;
    Ok((input, ()))
}

fn parse_chosen_creature_type_static_sentence_with_scope(
    input: &str,
) -> OracleResult<'_, (ChosenCreatureTypeStaticScope, ChosenCreatureTypeApplication)> {
    let (input, (scope, application)) = parse_chosen_creature_type_static_scope_body(input)?;
    Ok((input, (scope, application)))
}

/// CR 205.1a / CR 205.1b + CR 607.2d: Parse the "<scope> are the chosen [creature]
/// type [in addition to their other types]" body, returning the affected scope and
/// whether the effect is additive (CR 205.1b "in addition") or replacing (CR 205.1a
/// SET — Conspiracy's bare "are the chosen type"). The "in addition to ..." suffix is
/// OPTIONAL: Arcane Adaptation / Lifecraft Engine / Xenograft are additive; Conspiracy
/// omits it and replaces the creature types.
fn parse_chosen_creature_type_static_scope_body(
    input: &str,
) -> OracleResult<'_, (ChosenCreatureTypeStaticScope, ChosenCreatureTypeApplication)> {
    let (input, (pronoun, scope)) = parse_chosen_creature_type_static_subject(input)?;
    let (input, _) =
        alt((tag(" the chosen type"), tag(" the chosen creature type"))).parse(input)?;
    // CR 205.1b (additive) vs CR 205.1a (SET/replace): the "in addition to ..."
    // suffix is OPTIONAL — its presence selects additive, its absence selects
    // replacement (Conspiracy). Shared with the compound-subject sibling.
    let (input, addition) = match parse_chosen_type_additive_suffix(input, pronoun) {
        Ok((rest, ())) => (rest, true),
        Err(_) => (input, false),
    };
    let application = if addition {
        ChosenCreatureTypeApplication::Additive
    } else {
        ChosenCreatureTypeApplication::Replacing
    };
    let (input, _) = opt(tag(".")).parse(input)?;
    Ok((input, (scope, application)))
}

/// CR 205.1b: the additive "in addition to `<pronoun>` other [creature ]types"
/// retain-suffix, shared by the single-subject
/// ([`parse_chosen_creature_type_static_scope_body`]) and compound-subject
/// ([`parse_compound_you_control_chosen_type_static`]) chosen-type static
/// handlers. The optional "creature " before "types" accepts Rukarumel's
/// "…in addition to their other creature types" spelling; the single-subject
/// forms omit it (Arcane Adaptation → "…their other types"), so the `opt` matches
/// nothing there and their behavior is unchanged.
fn parse_chosen_type_additive_suffix<'a>(input: &'a str, pronoun: &str) -> OracleResult<'a, ()> {
    let (input, _) = tag(" in addition to ").parse(input)?;
    let (input, _) = tag(pronoun).parse(input)?;
    let (input, _) = tag(" other ").parse(input)?;
    let (input, _) = opt(tag("creature ")).parse(input)?;
    let (input, _) = tag("types").parse(input)?;
    Ok((input, ()))
}

pub(crate) fn parse_chosen_creature_type_static_subject(
    input: &str,
) -> OracleResult<'_, (&'static str, ChosenCreatureTypeStaticScope)> {
    alt((
        value(
            ("their", ChosenCreatureTypeStaticScope::Creatures),
            tag("creatures you control are"),
        ),
        value(
            ("its", ChosenCreatureTypeStaticScope::EachCreature),
            tag("each creature you control is"),
        ),
        value(
            ("their", ChosenCreatureTypeStaticScope::VehicleCreatures),
            tag("vehicle creatures you control are"),
        ),
        // CR 109.2a: the graveyard-scoped sibling names a card plus its zone, and so
        // reads "has" (a single card) rather than "are", with retention pronoun "its"
        // (Ashes of the Fallen).
        value(
            ("its", ChosenCreatureTypeStaticScope::GraveyardCreatureCards),
            tag("each creature card in your graveyard has"),
        ),
    ))
    .parse(input)
}

/// CR 611.3 + CR 607.2d + CR 205.1b + CR 205.3m: compound-subject sibling of
/// [`parse_arcane_adaptation_chosen_type_static`]. "`<X>` you control and `<Y>`
/// you control are the chosen type in addition to their other creature types"
/// (Rukarumel, Biologist) — two independently-resolvable `you control` conjuncts
/// joined by "and" that the single-subject dispatcher's fixed-arm subject matcher
/// (`parse_chosen_creature_type_static_subject`) can't reach.
///
/// Structural mirror of [`parse_compound_all_subjects_type_change`] (#5219's
/// compound-subject animation gate): delegate subject resolution WHOLE to the
/// already-generic [`parse_continuous_subject_filter`] (which unions "X you
/// control and Y you control" into an `Or` via
/// `parse_controlled_compound_continuous_subject_filter`) and own only the
/// predicate. Declines unless the subject is a genuine `Or` of 2+ filters, so
/// single-subject lines fall through to the existing sibling unchanged, and
/// declines non-additive (CR 205.1a SET) predicates. Reuses the fully-wired
/// `AddChosenSubtype { CreatureType }` runtime — no new variant, no new runtime.
pub(crate) fn parse_compound_you_control_chosen_type_static(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<StaticDefinition> {
    let (subject_tp, predicate_tp) = tp.split_around(" are ")?;
    let affected = parse_continuous_subject_filter(subject_tp.original)?;
    // Require a genuine compound (Or of 2+); a single-subject filter is owned by
    // `parse_arcane_adaptation_chosen_type_static`.
    match &affected {
        TargetFilter::Or { filters } if filters.len() >= 2 => {}
        _ => return None,
    }
    // Additive chosen-creature-type predicate only (CR 205.1b). A compound subject
    // is plural, so the retain pronoun is always "their".
    nom_on_lower(
        predicate_tp.original,
        predicate_tp.lower,
        parse_compound_chosen_type_additive_predicate,
    )?;

    Some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(vec![ContinuousModification::AddChosenSubtype {
                kind: ChosenSubtypeKind::CreatureType,
            }])
            .description(text.to_string()),
    )
}

/// nom body for [`parse_compound_you_control_chosen_type_static`]'s predicate,
/// consumed to `eof` so it never mis-claims a longer line: "the chosen [creature ]
/// type in addition to their other [creature ]types[.]". Additive only — a bare
/// "the chosen type" without the retain-suffix (CR 205.1a SET) fails, so a
/// hypothetical replacing compound is declined rather than silently reinterpreted.
fn parse_compound_chosen_type_additive_predicate(input: &str) -> OracleResult<'_, ()> {
    let (input, _) = alt((tag("the chosen creature type"), tag("the chosen type"))).parse(input)?;
    let (input, ()) = parse_chosen_type_additive_suffix(input, "their")?;
    let (input, _) = opt(tag(".")).parse(input)?;
    eof.parse(input)?;
    Ok((input, ()))
}

// CR 613.1d + CR 205.3m: "<creatures you control are> every creature type" —
// Layer 4 type-changing effect that adds every creature type (CR 205.3m) to each
// creature the controller has on the battlefield. Maskwood Nexus is the
// canonical printing; the static is the class of "<your creatures> are every
// creature type" effects, paralleling `parse_arcane_adaptation_chosen_type_static`
// for "the chosen type". The full two-sentence continuation is recognized by
// `same_is_true::parse_same_is_true_type_static` before this battlefield-only
// parser, so this function remains the single-sentence form's fallback.
pub(crate) fn parse_every_creature_type_static(
    tp: &TextPair<'_>,
    description: &str,
) -> Option<StaticDefinition> {
    let ((), _) = nom_on_lower(
        tp.original,
        tp.lower,
        parse_every_creature_type_static_sentence,
    )?;

    Some(
        StaticDefinition::continuous()
            .affected(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You),
            ))
            .modifications(vec![ContinuousModification::AddAllCreatureTypes])
            .description(description.to_string()),
    )
}

pub(crate) fn parse_every_creature_type_static_sentence(input: &str) -> OracleResult<'_, ()> {
    let (input, _) = parse_every_creature_type_static_prefix(input)?;
    let (input, _) = eof.parse(input)?;
    Ok((input, ()))
}

pub(crate) fn parse_every_creature_type_static_prefix(input: &str) -> OracleResult<'_, ()> {
    let (input, _) = parse_chosen_creature_type_static_subject(input)?;
    let (input, _) = tag(" every creature type").parse(input)?;
    let (input, _) = opt(tag(".")).parse(input)?;
    Ok((input, ()))
}

pub(crate) fn parse_collection_counter_play_permission_static(
    tp: &TextPair<'_>,
    description: &str,
) -> Option<StaticDefinition> {
    let ((), _) = nom_on_lower(tp.original, tp.lower, |input| {
        let (input, _) = tag("once each turn, you may play a card from exile with a collection counter on it if it was exiled by an ability you controlled").parse(input)?;
        let (input, _) = alt((
            tag(", and mana of any type can be spent to cast that spell"),
            tag(", and you may spend mana as though it were mana of any color to cast it"),
        ))
        .parse(input)?;
        let (input, _) = opt(tag(".")).parse(input)?;
        let (input, _) = eof.parse(input)?;
        Ok((input, ()))
    })?;

    Some(
        StaticDefinition::new(StaticMode::LinkedCollectionCounterPlayPermission)
            .description(description.to_string()),
    )
}

/// CR 205.1 / CR 205.3a: Extract additive-type modifications from a predicate
/// like `"are Food artifacts in addition to their other types"` or its
/// compound/granted-ability variants. Used both as the body of
/// `parse_subject_additive_type_static` (pure additive predicates) and as a
/// fallback inside `parse_continuous_modifications` (compound predicates
/// whose leading `have …` clause is already consumed upstream).
///
/// Returns `None` when:
/// * the clause does not contain an additive-type phrase,
/// * the type-word region is a placeholder handled by another specialized
///   extractor (`every basic land type`, `the chosen type`), or
/// * no valid type or subtype was recognized (unknown words are dropped —
///   the curated `SUBTYPES` list is authoritative).
pub(crate) fn parse_additive_type_clause_modifications(
    text: &str,
) -> Option<Vec<ContinuousModification>> {
    type VE<'a> = OracleError<'a>;

    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower)
        .trim_start()
        .trim_end()
        .trim_end_matches('.');
    let (_, clause_lower) = nom_primitives::scan_split_at_phrase(tp.lower, |i| {
        alt((
            tag::<_, _, VE>("are "),
            tag::<_, _, VE>("is "),
            tag::<_, _, VE>("and are "),
            tag::<_, _, VE>("and is "),
        ))
        .parse(i)
    })?;
    let clause_original = &tp.original[tp.original.len() - clause_lower.len()..];
    let (after_verb_lower, _) = alt((
        tag::<_, _, VE>("are "),
        tag::<_, _, VE>("is "),
        tag::<_, _, VE>("and are "),
        tag::<_, _, VE>("and is "),
    ))
    .parse(clause_lower)
    .ok()?;
    let after_verb_original = &clause_original[clause_original.len() - after_verb_lower.len()..];
    let (after_suffix_lower, type_words_lower) = terminated(
        take_until::<_, _, VE>(" in addition to "),
        (
            tag::<_, _, VE>(" in addition to "),
            alt((tag::<_, _, VE>("its"), tag::<_, _, VE>("their"))),
            tag::<_, _, VE>(" other "),
            // CR 105.2 + CR 205.1a: "colors and types" forms add a color (CR
            // 613.1e, layer 5) alongside the type/subtype additions. Longer
            // phrases first so "colors and types" wins over bare "types".
            alt((
                tag::<_, _, VE>("colors and creature types"),
                tag::<_, _, VE>("colors and types"),
                tag::<_, _, VE>("creature types"),
                tag::<_, _, VE>("land types"),
                tag::<_, _, VE>("types"),
                tag::<_, _, VE>("colors"),
            )),
        ),
    )
    .parse(after_verb_lower)
    .ok()?;
    let type_words = &after_verb_original[..type_words_lower.len()];
    let normalized_type_words = type_words_lower.trim();
    // Placeholders owned by other specialized extractors (basic-land-type copies,
    // chosen-type statics). Let those branches produce the correct modification.
    if matches!(
        normalized_type_words,
        "every basic land type" | "the chosen type" | "the chosen creature type"
    ) {
        return None;
    }
    // CR 205.3i + CR 305.7: "is every land type in addition to its other types"
    // grants all 17 land subtypes additively (Omo, Queen of Vesuva). The token
    // is already combinator-extracted above; gate it against the fixed CR
    // phrase here (mirrors the adjacent placeholder `matches!`).
    if normalized_type_words == "every land type" {
        return Some(vec![ContinuousModification::AddAllLandTypes]);
    }
    // CR 613.1f: route the trailing "and has <X>" conjunct through the shared
    // `parse_continuous_modifications` authority — the same one the sibling
    // `parse_enchanted_is_type` uses for its own trailing clause — rather than the
    // quoted-ability-only `parse_quoted_ability_modifications`. It subsumes the
    // quoted-ability parse and adds bare keyword handling, so a bare "and has
    // <keyword>" (Aurification's "…other creature types and has defender") composes
    // an `AddKeyword` instead of being silently dropped. Safe from the mutual
    // recursion with this function: the trailing clause carries no
    // "in addition to … types" phrase, so the additive fallback inside it declines.
    let after_suffix_original =
        &clause_original[clause_original.len() - after_suffix_lower.len()..];
    let granted_modifications = parse_continuous_modifications(after_suffix_original);

    let mut modifications = Vec::new();
    for raw_word in type_words.split_whitespace() {
        let word = raw_word.trim_matches(|c: char| c == ',' || c == '.');
        if word.is_empty() {
            continue;
        }
        if let Some(modification) = classify_additive_type_word(&word.to_lowercase()) {
            modifications.push(modification);
        }
    }

    modifications.extend(granted_modifications);
    (!modifications.is_empty()).then_some(modifications)
}

/// CR 205.1: Map a bare type word (singular or plural) to its `CoreType`.
pub(crate) fn core_type_from_additive_word(word: &str) -> Option<CoreType> {
    match word {
        "artifact" | "artifacts" => Some(CoreType::Artifact),
        "creature" | "creatures" => Some(CoreType::Creature),
        "enchantment" | "enchantments" => Some(CoreType::Enchantment),
        "land" | "lands" => Some(CoreType::Land),
        "planeswalker" | "planeswalkers" => Some(CoreType::Planeswalker),
        "battle" | "battles" => Some(CoreType::Battle),
        _ => None,
    }
}

/// Classify one word (already lowercased) of an additive "<...> in addition to
/// its other types" clause into its continuous modification — a single authority
/// spanning every characteristic a type-addition word can carry. `all_consuming`
/// asserts the whole token is the classified word.
///
/// - CR 105.2 + CR 613.1e (Layer 5): a color word maps to `AddColor`.
/// - CR 205.1 (Layer 4): a core-type word maps to `AddType`.
/// - CR 205.4a (Layer 4): a supertype word ("legendary"/"snow"/"basic") maps to
///   `AddSupertype` — previously dropped, so Super-Soldier Serum's "legendary
///   Soldier" kept only the subtype.
/// - CR 205.3a (Layer 4): a canonical subtype maps to `AddSubtype`; unrecognized
///   words are dropped rather than fabricated into non-MTG subtypes.
fn classify_additive_type_word(word: &str) -> Option<ContinuousModification> {
    if let Ok((_, color)) = all_consuming(nom_primitives::parse_color).parse(word) {
        return Some(ContinuousModification::AddColor { color });
    }
    if let Some(core_type) = core_type_from_additive_word(word) {
        return Some(ContinuousModification::AddType { core_type });
    }
    if let Ok((_, supertype)) = all_consuming(nom_target::parse_supertype_word).parse(word) {
        return Some(ContinuousModification::AddSupertype { supertype });
    }
    if let Some((canonical, _)) = parse_subtype(word) {
        return Some(ContinuousModification::AddSubtype { subtype: canonical });
    }
    None
}

/// CR 205.3 + CR 700.8: Parse a self-static of the form
/// `~ is also a <subtype>(, <subtype>)*[, [and|or] <subtype>]` into a vec of
/// `AddSubtype` modifications. The anchor `~` (set by `normalize_self_refs_for_static`)
/// scopes the match to source-self type grants — attached-object additive grants
/// ("Enchanted land is also a Plains") route through `parse_subject_additive_type_static`
/// instead. Returns `None` if the anchor doesn't match or any trailing text
/// remains after the subtype list, so other arms remain free to try the line.
///
/// CR 205.3d: An object can't gain a subtype that doesn't correspond to one of
/// its types. The pithy "X is also a Y" phrasing is exclusively used by
/// creature-subtype grants (party tribal: Cleric/Rogue/Warrior/Wizard, plus
/// scattered self-typegrant creatures); land/artifact/enchantment subtype
/// additions use the "in addition to its other types" phrasing handled by
/// `parse_subject_additive_type_static`. We therefore reject any token whose
/// canonical subtype maps to a non-creature core type so a stray Forest /
/// Equipment / Aura is not silently added to a creature.
pub(crate) fn try_parse_self_is_also_subtypes(
    tp: &TextPair<'_>,
) -> Option<Vec<ContinuousModification>> {
    type VE<'a> = OracleError<'a>;

    let (after_anchor, _): (&str, &str) = alt((
        tag::<_, _, VE>("~ is also a "),
        tag::<_, _, VE>("~ is also an "),
    ))
    .parse(tp.lower)
    .ok()?;

    fn parse_one(input: &str) -> nom::IResult<&str, String, OracleError<'_>> {
        match parse_subtype(input) {
            Some((canonical, len)) if infer_core_type_for_subtype(&canonical).is_none() => {
                Ok((&input[len..], canonical))
            }
            _ => Err(nom::Err::Error(nom::error::Error::new(
                input,
                nom::error::ErrorKind::Fail,
            ))),
        }
    }

    // Decomposes the separator into independent axes — connective phrase
    // (`,` optionally followed by `and`/`or`/`and/or`, or space-led
    // `and`/`or`/`and/or`) × mandatory trailing space × optional indefinite
    // article (`a `/`an `). Each axis is one `alt()`; the ≤14-form cartesian
    // product is composed, not enumerated, per the "compose combinators by
    // dimension" rule.
    fn parse_connective(input: &str) -> nom::IResult<&str, &str, OracleError<'_>> {
        // Order long-first within each branch so `, and/or` wins over the
        // bare `,` prefix in nom's left-to-right `alt` evaluation.
        alt((
            recognize((
                tag::<_, _, OracleError<'_>>(","),
                opt(preceded(
                    tag(" "),
                    alt((tag("and/or"), tag("and"), tag("or"))),
                )),
            )),
            recognize(preceded(
                tag(" "),
                alt((tag("and/or"), tag("and"), tag("or"))),
            )),
        ))
        .parse(input)
    }
    fn parse_sep(input: &str) -> nom::IResult<&str, (), OracleError<'_>> {
        let (input, _) = parse_connective(input)?;
        let (input, _) = tag(" ").parse(input)?;
        let (input, _) = opt(alt((tag("a "), tag("an ")))).parse(input)?;
        Ok((input, ()))
    }

    // `all_consuming` + `terminated` asserts the entire `after_anchor` slice
    // parses as `<subtype list><optional period><optional trailing space>` —
    // replaces the prior manual `.trim().is_empty()` trailing-text check with
    // an idiomatic nom assertion.
    let (_, names) = all_consuming(terminated(
        separated_list1(parse_sep, parse_one),
        (opt(tag::<_, _, VE>(".")), space0),
    ))
    .parse(after_anchor)
    .ok()?;

    if names.is_empty() {
        return None;
    }

    Some(
        names
            .into_iter()
            .map(|subtype| ContinuousModification::AddSubtype { subtype })
            .collect(),
    )
}

/// CR 205.1a + CR 613.1d (Layer 4) + CR 613.1f (Layer 6): Imprisoned-in-the-Moon
/// class — an Aura that turns the enchanted permanent into a colorless permanent
/// of a single card type (optionally with subtype[s]) carrying a granted ability
/// while stripping everything else: "Enchanted <subject> is a[n] colorless
/// [<subtype>...] <core type> with "<quoted ability>" and loses all other card
/// types and abilities." Imprisoned in the Moon ("Enchanted permanent is a
/// colorless land with "{T}: Add {C}" and loses all other card types and
/// abilities.") is the type specimen; Sugar Coat ("... is a colorless Food
/// artifact with "..." and loses ...") is the subtype-bearing sibling.
///
/// Emits, in written order: `SetCardTypes` (replace all card types with
/// <core type>, CR 205.1a), `SetColor([])` (become colorless, CR 105.2), then —
/// when a subtype is present — `RemoveAllSubtypes` for the core type's subtype
/// set followed by one `AddSubtype` per parsed subtype (CR 205.1a set
/// replacement, Layer 4 — e.g. Sugar Coat's Food, wiping any pre-existing
/// artifact subtype so a Clue host becomes "Food artifact", not "Clue Food
/// artifact"), `RemoveAllAbilities` (the permanent loses its own abilities,
/// Layer 6 CR 613.1f), then the `GrantAbility` produced by the shared
/// `parse_quoted_ability_modifications` authority. `RemoveAllAbilities` is
/// emitted BEFORE the grant so the granted ability SURVIVES the wipe (CR 613.6 —
/// the removal and the grant are parts of one continuous effect applied
/// together, so a grant emitted after the wipe within this single effect is not
/// itself removed; the same ordering the `RemoveAllSubtypes` → `AddChosenSubtype`
/// composition relies on). No new variant, no new runtime.
///
/// Dispatched BEFORE [`parse_enchanted_is_type`], whose ` with base power and
/// toughness ` split does not model a `with "<ability>"` clause and so drops
/// both the grant and the ability-strip (issue #4770). The plain family (Song of
/// the Dryads, Darksteel Mutation) carries no quoted-ability clause and falls
/// through unchanged. Closes #4770.
pub(crate) fn parse_enchanted_becomes_type_with_ability(
    tp: &TextPair<'_>,
    description: &str,
) -> Option<StaticDefinition> {
    use crate::types::card_type::CoreType;
    // Isolate the ` with ` seam so the quoted ability (original-case {T}/{C}
    // symbols) and the trailing ability-strip clause are parsed separately.
    let (before, after) = tp.split_around(" with ")?;

    // BEFORE: "enchanted <subject> is a[n] colorless <core type>".
    let before_rest = nom_tag_tp(&before, "enchanted ")?;
    let (r, perm_tf) = nom_target::parse_type_filter_word(before_rest.lower).ok()?;
    let (r, _) = alt((tag::<_, _, OracleError<'_>>(" is a "), tag(" is an ")))
        .parse(r)
        .ok()?;
    // CR 105.2: "colorless" is optional. Minimus Containment ("is a Treasure
    // artifact with ...") sets a card type without recoloring; Imprisoned in the
    // Moon / Sugar Coat ("colorless land" / "colorless Food artifact") also make
    // the permanent colorless. Only emit `SetColor([])` when it is stated.
    let (r, colorless) = opt(tag::<_, _, OracleError<'_>>("colorless "))
        .parse(r)
        .ok()?;
    // CR 205.3: optional subtype(s) preceding the core card type — Sugar Coat
    // ("colorless Food artifact ...") vs Imprisoned ("colorless land ...").
    // `parse_subtype` is case-insensitive (runs on the lowered slice) and a core
    // type word is never a subtype, so the loop stops before the head noun.
    let mut r = r;
    let mut subtypes: Vec<String> = Vec::new();
    while let Some((canonical, consumed)) = parse_subtype(r) {
        subtypes.push(canonical);
        r = r[consumed..].trim_start();
    }
    let (r, type_tf) = nom_target::parse_type_filter_word(r).ok()?;
    if !r.trim().is_empty() {
        return None;
    }
    let core_type = match type_tf {
        TypeFilter::Creature => CoreType::Creature,
        TypeFilter::Artifact => CoreType::Artifact,
        TypeFilter::Enchantment => CoreType::Enchantment,
        TypeFilter::Land => CoreType::Land,
        TypeFilter::Planeswalker => CoreType::Planeswalker,
        _ => return None,
    };

    // AFTER: `"<quoted ability>" and loses all other card types and abilities[.]`.
    // The quoted ability → GrantAbility via the shared authority (original case).
    let grant_modifications = parse_quoted_ability_modifications(after.original);
    if grant_modifications.is_empty() {
        return None;
    }
    // Structural gate: the ability-strip clause must follow the closing quote, so
    // this handler only claims the full Imprisoned shape. Consume the quoted
    // ability with combinators (open-quote, body, close-quote) rather than a raw
    // split, then require the trailing strip clause to full consumption.
    let (after_quote, _) = tag::<_, _, OracleError<'_>>("\"")
        .parse(after.lower.trim_start())
        .ok()?;
    let (after_quote, _) = take_until::<_, _, OracleError<'_>>("\"")
        .parse(after_quote)
        .ok()?;
    let (after_quote, _) = tag::<_, _, OracleError<'_>>("\"").parse(after_quote).ok()?;
    // Trailing ability-strip clause — two attested shapes, composed from optional
    // spans rather than enumerated:
    //   "and loses all other card types and abilities" (Imprisoned in the Moon)
    //   "and it loses all other abilities"              (Minimus Containment)
    // The "it" subject and the "card types and " span are each optional; the
    // effect is a full Layer-6 ability wipe either way (the `SetCardTypes` above
    // already replaced the card types, so an unstated "card types" phrase loses
    // nothing). A comma may sit between the closing quote and the clause.
    let after_strip = opt(tag::<_, _, OracleError<'_>>(","))
        .parse(after_quote.trim_start())
        .ok()?
        .0
        .trim_start();
    let (rest, _) = tag::<_, _, OracleError<'_>>("and ")
        .parse(after_strip)
        .ok()?;
    let (rest, _) = opt(tag::<_, _, OracleError<'_>>("it ")).parse(rest).ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>("loses all other ")
        .parse(rest)
        .ok()?;
    let (rest, _) = opt(tag::<_, _, OracleError<'_>>("card types and "))
        .parse(rest)
        .ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>("abilities").parse(rest).ok()?;
    let (rest, _) = opt(tag::<_, _, OracleError<'_>>(".")).parse(rest).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }

    let mut modifications = vec![ContinuousModification::SetCardTypes {
        core_types: vec![core_type],
    }];
    // CR 105.2 (Layer 5): only recolor to colorless when the text states it.
    if colorless.is_some() {
        modifications.push(ContinuousModification::SetColor { colors: Vec::new() });
    }
    // CR 205.1a (Layer 4): grant each parsed subtype (Sugar Coat → Food). Placed
    // with the other type-identity modifications, before the Layer-6 ability wipe.
    // Setting a subtype REPLACES the object's existing subtypes from the same
    // set (CR 205.1a), so wipe the core type's subtype set first — otherwise an
    // already-subtyped host keeps its old subtype (e.g. a Clue enchanted by Sugar
    // Coat would become "Clue Food artifact" instead of "Food artifact"). The
    // grammar guarantees each parsed subtype belongs to the core type's set, so a
    // single `RemoveAllSubtypes { set-of-core-type }` covers them all.
    if !subtypes.is_empty() {
        if let Some(set) = core_type_subtype_set(core_type) {
            modifications.push(ContinuousModification::RemoveAllSubtypes { set });
        }
    }
    for subtype in subtypes {
        modifications.push(ContinuousModification::AddSubtype { subtype });
    }
    modifications.push(ContinuousModification::RemoveAllAbilities);
    // GrantAbility AFTER RemoveAllAbilities so the granted ability survives.
    modifications.extend(grant_modifications);

    Some(
        StaticDefinition::continuous()
            .affected(TargetFilter::Typed(
                TypedFilter::new(perm_tf).properties(vec![FilterProp::EnchantedBy]),
            ))
            .modifications(modifications)
            .description(description.to_string()),
    )
}

/// CR 205.1a + CR 702.6: "Each `<subject>` is an Equipment with equip
/// `{N}` and \"`<quoted ability>`\"" — the become-Equipment anthem (Bram,
/// Baguette Brawler; Bludgeon Brawl). Each matching permanent gains the Equipment
/// artifact subtype (CR 205.1a — setting an artifact subtype replaces the
/// object's other artifact subtypes), the Equip keyword with the printed cost
/// (CR 702.6), and the quoted static ability (typically an "Equipped creature
/// gets +N/+0" anthem, granted via the shared quoted-ability authority).
pub(crate) fn parse_becomes_equipment_with_ability(
    tp: &TextPair<'_>,
    description: &str,
) -> Option<StaticDefinition> {
    let (subject_tp, rest_tp) = tp.split_around(" is an equipment with equip ")?;
    let affected = super::shared::parse_continuous_subject_filter(subject_tp.original)?;

    // rest: `{cost} and "<quoted ability>"[, where X is <…> mana value][.]`. The
    // equip cost precedes ` and "`; the quoted ability is bounded by its closing
    // quote, and any trailing `, where X is …` binding follows it.
    let (cost_tp, ability_tp) = rest_tp.split_around(" and \"")?;
    let (quoted_body_tp, tail_tp) = ability_tp.split_around("\"")?;
    // Punctuation cleanup on the post-quote chunk (a leading comma from the
    // split, a trailing period) before matching the binding.
    let tail_core = tail_tp.lower.trim().trim_matches([',', '.']).trim();

    // CR 202.3: Bludgeon Brawl binds X to "that artifact's mana value" — the
    // Equipment's own mana value — used for BOTH the equip cost ({X}) and the
    // granted anthem ("gets +X/+0"). Match the binding EXACTLY with full
    // consumption ("that artifact's mana value" — the unambiguous source; a bare
    // "its" could refer to the equipped creature), so extra rules text after the
    // binding is not accepted.
    let dynamic_self_mana_value = all_consuming(tag::<_, _, OracleError<'_>>(
        "where x is that artifact's mana value",
    ))
    .parse(tail_core)
    .is_ok();

    // Fail closed on any unrecognized tail: the only text this handler models
    // after the quoted ability is that exact binding. A non-empty tail that is
    // not exactly the binding — whether the binding is absent OR followed by an
    // extra rider ("…artifact's mana value, and it gains flying") — carries
    // unmodeled rules text and must NOT be silently dropped.
    if !tail_core.is_empty() && !dynamic_self_mana_value {
        return None;
    }

    // Equip cost: a bare `{X}` bound to the source's mana value lowers to
    // `ManaCost::SelfManaValue` (concretized at activation like a graveyard-grant
    // "encore {X}, where X is its mana value"); otherwise a fixed mana cost.
    let cost_text = cost_tp.lower.trim();
    let equip_cost = if cost_text == "{x}" && dynamic_self_mana_value {
        ManaCost::SelfManaValue
    } else {
        let (cost_rest, cost) = nom_primitives::parse_mana_cost(cost_text).ok()?;
        if !cost_rest.trim().is_empty() {
            return None;
        }
        cost
    };

    // Re-wrap the quoted body and delegate to the shared quoted-ability authority
    // (original case preserves any {symbols}).
    let quoted = format!("\"{}\"", quoted_body_tp.original.trim());
    let mut grant_modifications = parse_quoted_ability_modifications(&quoted);
    if grant_modifications.is_empty() {
        return None;
    }
    // CR 202.3: the standalone anthem parser reads "gets +X/+0" as the cost-X
    // paid; for a CONTINUOUS grant bound to the Equipment's mana value, rebind
    // that reference to `SelfManaValue` so it reads the source's mana value.
    if dynamic_self_mana_value {
        rebind_cost_x_to_self_mana_value(&mut grant_modifications);
    }

    // CR 205.1a: Equipment is an artifact subtype; setting it replaces the
    // object's existing artifact subtypes (Bram's Food → Equipment), so wipe the
    // artifact subtype set before granting Equipment.
    let mut modifications = Vec::new();
    if let Some(set) = core_type_subtype_set(CoreType::Artifact) {
        modifications.push(ContinuousModification::RemoveAllSubtypes { set });
    }
    modifications.push(ContinuousModification::AddSubtype {
        subtype: "Equipment".to_string(),
    });
    modifications.push(ContinuousModification::AddKeyword {
        keyword: Keyword::Equip(equip_cost),
    });
    modifications.extend(grant_modifications);

    Some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(modifications)
            .description(description.to_string()),
    )
}

/// CR 202.3: Rebind a granted anthem's `CostXPaid` power/toughness reference to
/// the source object's mana value (`SelfManaValue`). Used when a become-Equipment
/// grant binds X to "that artifact's mana value" (Bludgeon Brawl): the standalone
/// anthem parser reads the bare "gets +X/+0" as the cost-X paid, but for a
/// continuous grant X is a fixed characteristic of the granting Equipment.
/// Recurses into the granted `StaticDefinition` carried by `GrantStaticAbility`.
fn rebind_cost_x_to_self_mana_value(modifications: &mut [ContinuousModification]) {
    for modification in modifications.iter_mut() {
        match modification {
            ContinuousModification::GrantStaticAbility { definition } => {
                rebind_cost_x_to_self_mana_value(&mut definition.modifications);
            }
            ContinuousModification::AddDynamicPower { value }
            | ContinuousModification::AddDynamicToughness { value } => {
                if matches!(
                    value,
                    QuantityExpr::Ref {
                        qty: QuantityRef::CostXPaid
                    }
                ) {
                    *value = QuantityExpr::Ref {
                        qty: QuantityRef::SelfManaValue,
                    };
                }
            }
            _ => {}
        }
    }
}

/// CR 205.3: the subtype set correlated with a core card type. Used to wipe an
/// object's existing subtypes of that set before a set-replacement `AddSubtype`
/// (CR 205.1a). Returns `None` for core types that have no subtype set of
/// interest here.
fn core_type_subtype_set(
    core_type: crate::types::card_type::CoreType,
) -> Option<crate::types::card_type::SubtypeSet> {
    use crate::types::card_type::{CoreType, SubtypeSet};
    match core_type {
        CoreType::Creature => Some(SubtypeSet::Creature),
        CoreType::Artifact => Some(SubtypeSet::Artifact),
        CoreType::Enchantment => Some(SubtypeSet::Enchantment),
        CoreType::Land => Some(SubtypeSet::Land),
        CoreType::Planeswalker => Some(SubtypeSet::Planeswalker),
        _ => None,
    }
}

/// CR 613.1d + CR 205.1a: "Enchanted [permanent-type] is a/an [type] [with base P/T N/N]
/// [in addition to its other types]"
///
/// Handles type-changing aura effects like Ensoul Artifact, Imprisoned in the Moon,
/// and Darksteel Mutation. Reuses nom type-word and P/T combinators.
pub(crate) fn parse_enchanted_is_type(
    tp: &TextPair,
    description: &str,
) -> Option<StaticDefinition> {
    // Match "enchanted " prefix
    let rest_tp = nom_tag_tp(tp, "enchanted ")?;

    // Parse the enchanted permanent type using nom type-word combinator
    let (after_type, perm_tf) = nom_target::parse_type_filter_word(rest_tp.lower).ok()?;
    let after_type_lower = after_type.trim_start();

    // Must have " is a " or " is an " or " loses all abilities and is a "
    let mut modifications = Vec::new();
    type VE<'a> = OracleError<'a>;

    let is_rest_lower = if let Ok((r, _)) = alt((
        tag::<_, _, VE>("loses all abilities and is a "),
        tag::<_, _, VE>("loses all abilities and is an "),
    ))
    .parse(after_type_lower)
    {
        modifications.push(ContinuousModification::RemoveAllAbilities);
        r
    } else if let Ok((r, _)) =
        alt((tag::<_, _, VE>("is a "), tag::<_, _, VE>("is an "))).parse(after_type_lower)
    {
        r
    } else {
        return None;
    };

    let is_rest_lower = is_rest_lower.trim_end_matches('.');

    // Check for "in addition to its other types" suffix.
    // CR 205.1b: "in addition to its other types" retains all prior card types
    // (additive). Its absence means CR 205.1a applies: the new card type(s)
    // replace the existing ones.
    let (type_part, is_additive) =
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        if let Some(before) = is_rest_lower.strip_suffix(" in addition to its other types") {
            // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            (before.trim(), true)
        } else {
            (is_rest_lower, false)
        };

    // Try to parse "base power and toughness N/N" suffix.
    //
    // `pt_part` is everything after the " with base power and toughness "
    // token, e.g. for Darksteel Mutation: "0/1 and has indestructible, and it
    // loses all other abilities, card types, and creature types". `parse_pt_mod`
    // consumes only the leading "N/N" — the unconsumed remainder (the
    // "and has <kw> ... and it loses all ..." clause) is captured and fed to
    // `parse_continuous_modifications` below so it is not silently dropped.
    let (type_part, base_pt, trailing_clause) =
        if let Some((before_pt, pt_part)) =
            // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            type_part.rsplit_once(" with base power and toughness ")
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        {
            let pt_tail = pt_part.trim().trim_end_matches('.');
            if let Ok((remainder, (p, t))) = parse_pt_mod_with_remainder(pt_tail) {
                let remainder = remainder.trim().trim_end_matches('.').trim();
                let clause = (!remainder.is_empty()).then_some(remainder);
                (before_pt.trim(), Some((p, t)), clause)
            } else {
                (type_part, None, None)
            }
        } else {
            (type_part, None, None)
        };

    // Parse "N/N [color] [type] [subtype]" patterns for Darksteel Mutation style
    // e.g., "0/1 green Insect creature"
    let (type_part, inline_pt) = if let Ok((rest, (p, t))) = parse_pt_mod_with_remainder(type_part)
    {
        (rest.trim(), Some((p, t)))
    } else {
        (type_part, None)
    };

    // Parse optional color
    let (type_part, opt_color) = if let Ok((rest, color)) = nom_primitives::parse_color(type_part) {
        (rest.trim(), Some(color))
    } else if let Ok((rest, _)) = tag::<_, _, VE>("colorless ").parse(type_part) {
        // "colorless" removes all colors — handled via SetColor([])
        (rest.trim(), None)
    } else {
        (type_part, None)
    };
    let is_colorless = nom_primitives::scan_contains(is_rest_lower, "colorless");

    // Parse the target type(s) — use parse_type_filter_word for the main type.
    // Handle "[Subtype] [type]" patterns (e.g., "insect creature") by trying the
    // first word as a subtype and the second as a type if direct parse fails.
    use crate::types::card_type::CoreType;

    let (parsed_type, subtype_word, remainder) =
        if let Ok((remainder, target_tf)) = nom_target::parse_type_filter_word(type_part) {
            (Some(target_tf), None, remainder.trim())
        } else if let Some(space_pos) = type_part.find(' ') {
            // First word might be a subtype — try the rest as a type
            let maybe_subtype = &type_part[..space_pos];
            let after_subtype = type_part[space_pos..].trim();
            if let Ok((remainder, target_tf)) = nom_target::parse_type_filter_word(after_subtype) {
                // Capitalize the subtype for canonical form
                let capitalized = {
                    let mut chars = maybe_subtype.chars();
                    match chars.next() {
                        Some(first) => {
                            let mut s = first.to_uppercase().collect::<String>();
                            s.push_str(chars.as_str());
                            s
                        }
                        None => maybe_subtype.to_string(),
                    }
                };
                (Some(target_tf), Some(capitalized), remainder.trim())
            } else {
                (None, None, type_part)
            }
        } else {
            (None, None, type_part)
        };

    if let Some(target_tf) = parsed_type {
        // Collect the granted core types and subtypes separately so the
        // trailing-clause loss modifications can be inserted in the correct
        // written order: any `RemoveAllSubtypes` must precede `AddSubtype`
        // (the new creature type must survive the subtype wipe — CR 205.1b).
        let mut granted_core_types: Vec<CoreType> = Vec::new();
        let mut granted_subtypes: Vec<String> = Vec::new();

        // Route a parsed TypeFilter to the granted core-type list or the
        // granted-subtype list. `TypeFilter::Subtype` (e.g. "Insect") must be
        // emitted as `AddSubtype`, not dropped — CR 205.1b: a "[creature type]
        // artifact creature" replaces the creature type with that subtype.
        let classify_type =
            |tf: &TypeFilter, cores: &mut Vec<CoreType>, subs: &mut Vec<String>| match tf {
                TypeFilter::Creature => cores.push(CoreType::Creature),
                TypeFilter::Artifact => cores.push(CoreType::Artifact),
                TypeFilter::Enchantment => cores.push(CoreType::Enchantment),
                TypeFilter::Land => cores.push(CoreType::Land),
                TypeFilter::Planeswalker => cores.push(CoreType::Planeswalker),
                TypeFilter::Subtype(sub) => subs.push(sub.clone()),
                _ => {}
            };

        // Leading type word.
        classify_type(&target_tf, &mut granted_core_types, &mut granted_subtypes);

        // Subtype parsed from the "[Subtype] [type]" two-word branch.
        if let Some(sub) = subtype_word {
            granted_subtypes.push(sub);
        }

        // Parse any additional type words or subtypes from remainder
        // Handles "Insect artifact creature" where remainder = "creature" after parsing "artifact"
        let mut extra = remainder;
        while !extra.is_empty() {
            if let Ok((rest, extra_tf)) = nom_target::parse_type_filter_word(extra) {
                classify_type(&extra_tf, &mut granted_core_types, &mut granted_subtypes);
                extra = rest.trim();
            } else if is_capitalized_words(extra) {
                granted_subtypes.push(extra.to_string());
                break;
            } else {
                break;
            }
        }

        // CR 305.7: If a non-additive type-changing Aura sets exactly one
        // basic land subtype, keep the card-type replacement ("is a ... land")
        // and use SetBasicLandType instead of AddSubtype so the land-subtype
        // change removes rules-text abilities and old land subtypes.
        if !is_additive && granted_core_types == vec![CoreType::Land] && granted_subtypes.len() == 1
        {
            if let Some(basic_type) = parse_basic_land_type(&granted_subtypes[0].to_lowercase()) {
                let affected = TargetFilter::Typed(
                    TypedFilter::new(perm_tf).properties(vec![FilterProp::EnchantedBy]),
                );
                let mut mods = modifications;
                mods.push(ContinuousModification::SetCardTypes {
                    core_types: granted_core_types,
                });
                if let Some(color) = opt_color {
                    mods.push(ContinuousModification::SetColor {
                        colors: vec![color],
                    });
                } else if is_colorless {
                    mods.push(ContinuousModification::SetColor { colors: vec![] });
                }
                mods.push(ContinuousModification::SetBasicLandType {
                    land_type: basic_type,
                });
                return Some(
                    StaticDefinition::continuous()
                        .affected(affected)
                        .modifications(mods)
                        .description(description.to_string()),
                );
            }
        }

        // This branch handles type-*changing* auras that grant at least one
        // core card type ("is an Insect artifact creature ..."). A bare
        // "is a [land subtype]" ("Enchanted land is a Mountain") grants no
        // core type and is a basic-land-type change — defer to the dedicated
        // SetBasicLandType parser by returning None here.
        // CR 205.1a + CR 613.4b (issue #5300): the Lignify class names only a
        // creature SUBTYPE in the copula — "Enchanted creature is a Treefolk with
        // base power and toughness 0/4 and loses all abilities." Setting a creature
        // subtype replaces the object's creature subtypes but does NOT set or
        // overwrite its card types (CR 205.1a: removing/replacing a subtype does not
        // affect card types), so an enchanted Artifact/Land creature keeps
        // Artifact/Land (Lignify on Gingerbrute → Artifact Creature — Treefolk;
        // on Dryad Arbor → Land Creature — Forest Treefolk). We therefore do NOT
        // synthesize a Creature core type — leaving `granted_core_types` empty so
        // NO `SetCardTypes` is emitted — but still emit the subtype replacement
        // (`RemoveAllSubtypes{Creature}` → `AddSubtype`) below.
        //
        // CR 613.1d (Layer 4): the subtype-setting branch below is a
        // type-changing effect. Two disambiguators separate it from the basic-land
        // subtype change ("Enchanted land is a Mountain", which must return None
        // here and defer to SetBasicLandType): a base P/T is creature-only
        // (Lignify), OR the enchanted permanent itself is a creature — "Enchanted
        // CREATURE is a Flagbearer" (Coalition Flag) names a creature subtype even
        // without a base P/T, whereas the basic-land form enchants a LAND (`perm_tf`
        // = Land).
        let subtype_only_creature_change = granted_core_types.is_empty()
            && !granted_subtypes.is_empty()
            && (base_pt.is_some() || matches!(perm_tf, TypeFilter::Creature));
        if granted_core_types.is_empty() && !subtype_only_creature_change {
            return None;
        }

        // Parse the trailing "and has <kw> ... and it loses all other ..."
        // clause that the " with base power and toughness " split would
        // otherwise discard. `parse_continuous_modifications` turns "and has
        // <kw>" into `AddKeyword` and "loses all [other] abilities/creature
        // types" into `RemoveAllAbilities` / `RemoveAllSubtypes`.
        let mut clause_mods: Vec<ContinuousModification> = Vec::new();
        let mut loss_replaces_card_types = false;
        if let Some(clause) = trailing_clause {
            clause_mods = parse_continuous_modifications(clause);
            // CR 205.1b: an explicit "loses all other card types" makes the
            // type-set replacement exact — emit a single `SetCardTypes`
            // carrying the granted core types instead of additive `AddType`s.
            loss_replaces_card_types = scan_loss_enumeration(&clause.to_lowercase())
                .iter()
                .any(|m| matches!(m, LossMember::CardTypes));
        }

        // CR 205.1a + CR 613.1d (Layer 4): Two independent conditions each require
        // SetCardTypes (replacing) rather than AddType (additive):
        //   (A) loss_replaces_card_types: trailing clause explicitly says "loses
        //       all other card types" (Darksteel Mutation path — already working).
        //   (B) !is_additive: "in addition to its other types" is absent, so "is a
        //       [type]" replaces existing card types (Frogify, Lignify, etc.).
        // These are documented separately and combined into a single bool to avoid
        // emitting two SetCardTypes pushes.
        let needs_set_card_types = loss_replaces_card_types || !is_additive;

        // --- Assemble modifications in written (mod_index) order ---
        // 1. Core types: replacement (SetCardTypes) when CR 205.1a applies (no
        //    "in addition" suffix) or the clause says "loses all other card
        //    types"; else additive AddType (CR 205.1b "in addition").
        // CR 205.1a (issue #5300): the subtype-only Lignify branch grants NO core
        // type — emit no SetCardTypes so the object keeps its existing card types.
        if needs_set_card_types && !granted_core_types.is_empty() {
            modifications.push(ContinuousModification::SetCardTypes {
                core_types: granted_core_types.clone(),
            });
        } else {
            for ct in &granted_core_types {
                modifications.push(ContinuousModification::AddType { core_type: *ct });
            }
        }

        // 2. Color
        // CR 105.3 + CR 613.1e (Layer 5): a new color replaces all previous
        // colors unless the effect is "in addition"; additive "in addition to
        // its other types" appends via AddColor.
        if let Some(color) = opt_color {
            if is_additive {
                modifications.push(ContinuousModification::AddColor { color });
            } else {
                modifications.push(ContinuousModification::SetColor {
                    colors: vec![color],
                });
            }
        } else if is_colorless {
            modifications.push(ContinuousModification::SetColor { colors: vec![] });
        }

        // 3. Base P/T from explicit "with base power and toughness" or inline "N/N"
        if let Some((p, t)) = base_pt.or(inline_pt) {
            modifications.push(ContinuousModification::SetPower { value: p });
            modifications.push(ContinuousModification::SetToughness { value: t });
        }

        // CR 205.1a + CR 613.1d (Layer 4): Non-additive "is a [subtype] creature"
        // sets a new creature subtype, which replaces existing creature subtypes.
        // Auto-inject RemoveAllSubtypes{Creature} unless the trailing clause
        // already provides it (Darksteel Mutation explicitly says "loses all
        // other creature types" and its clause_mods contains the wipe).
        if !is_additive
            && (granted_core_types.contains(&CoreType::Creature) || subtype_only_creature_change)
            && !granted_subtypes.is_empty()
            && !modifications
                .iter()
                .chain(clause_mods.iter())
                .any(|m| matches!(m, ContinuousModification::RemoveAllSubtypes { .. }))
        {
            modifications.push(ContinuousModification::RemoveAllSubtypes {
                set: crate::types::card_type::SubtypeSet::Creature,
            });
        }

        // 4. Trailing-clause mods (AddKeyword, RemoveAllAbilities,
        //    RemoveAllSubtypes) — RemoveAllSubtypes here must precede the
        //    AddSubtype emissions below so the granted creature type survives.
        modifications.extend(clause_mods);

        // 5. Granted subtypes (e.g. AddSubtype(Insect)) — after any
        //    RemoveAllSubtypes wipe.
        for sub in granted_subtypes {
            modifications.push(ContinuousModification::AddSubtype { subtype: sub });
        }

        if modifications.is_empty() {
            return None;
        }

        let affected = TargetFilter::Typed(
            TypedFilter::new(perm_tf).properties(vec![FilterProp::EnchantedBy]),
        );

        return Some(
            StaticDefinition::continuous()
                .affected(affected)
                .modifications(modifications)
                .description(description.to_string()),
        );
    }

    None
}

/// CR 613.1d (Layer 4) + CR 205.1b + CR 205.2: Parse an attached-permanent
/// type-SWAP static of the form
/// "[Enchanted|Equipped] <subject> isn't a[n] <core type> and is a[n] <core
/// type> in addition to its other types."
///
/// Luxior, Giada's Gift and Luxior and Shadowspear are the type specimens
/// ("Equipped permanent isn't a planeswalker and is a creature in addition to
/// its other types."): the Equipment removes the equipped permanent's
/// Planeswalker card type while adding the Creature card type, turning an
/// equipped planeswalker into a creature. The two clauses decompose into two
/// independent Layer-4 modifications. The "isn't a[n] <T>" clause becomes
/// `RemoveType { core_type: <T> }` (CR 613.1d type removal — the permanent loses
/// the named card type). The "is a[n] <T> in addition to its other types" clause
/// becomes `AddType { core_type: <T> }` (CR 205.1b / CR 205.2 additive grant —
/// the permanent keeps its other card types and gains <T>).
///
/// Both reuse the existing `RemoveType` / `AddType` runtime (`game/layers.rs`)
/// applied at Layer 4 — no new engine variant, no new runtime. The subject is
/// the shared `attached_subject_filter` building block (Enchanted/Equipped
/// creature/permanent/land), which owns every attached-subject prefix including
/// the "equipped permanent" case a Luxior-style Equipment attached to a
/// non-creature permanent uses. The body is consumed to `eof`, so a partial
/// prefix can never mis-claim a longer or differently-shaped line.
pub(crate) fn parse_attached_isnt_and_is_type(
    tp: &TextPair<'_>,
    description: &str,
) -> Option<StaticDefinition> {
    let (affected, predicate) = attached_subject_filter(tp)?;
    let predicate_lower = predicate.trim().to_lowercase();
    let (_, modifications) = parse_isnt_and_is_additive_type_body(&predicate_lower).ok()?;
    Some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(modifications)
            .description(description.to_string()),
    )
}

/// nom body for [`parse_attached_isnt_and_is_type`]: "isn't a[n] <core type> and
/// is a[n] <core type> in addition to its other types[.]", consumed to `eof`.
/// CR 613.1d: the leading clause removes a card type; CR 205.1b / CR 205.2: the
/// trailing "in addition to its other types" clause adds a card type additively.
fn parse_isnt_and_is_additive_type_body(
    input: &str,
) -> OracleResult<'_, Vec<ContinuousModification>> {
    let (input, _) = alt((tag("isn't an "), tag("isn't a "))).parse(input)?;
    let (input, removed) = parse_core_type_word(input)?;
    let (input, _) = alt((tag(" and is an "), tag(" and is a "))).parse(input)?;
    let (input, added) = parse_core_type_word(input)?;
    let (input, _) = tag(" in addition to its other types").parse(input)?;
    let (input, _) = opt(tag(".")).parse(input)?;
    let (input, _) = eof.parse(input)?;
    Ok((
        input,
        vec![
            ContinuousModification::RemoveType { core_type: removed },
            ContinuousModification::AddType { core_type: added },
        ],
    ))
}

/// CR 205.2: Map a singular core-type word to its [`CoreType`]. Longest tokens
/// have no shared prefixes here, so left-to-right `alt` ordering is unambiguous.
fn parse_core_type_word(input: &str) -> OracleResult<'_, CoreType> {
    alt((
        value(CoreType::Artifact, tag("artifact")),
        value(CoreType::Battle, tag("battle")),
        value(CoreType::Creature, tag("creature")),
        value(CoreType::Enchantment, tag("enchantment")),
        value(CoreType::Planeswalker, tag("planeswalker")),
        value(CoreType::Land, tag("land")),
    ))
    .parse(input)
}

/// CR 205.1a + CR 205.2 + CR 205.3 + CR 613.1c: Scan text for a "becomes a
/// [subtype]* [core-type]+ in addition to its other types" descriptor and
/// decompose it into typed `ContinuousModification`s.
///
/// Uses nom combinators (`tag`, `alt`, `take_until`) to locate the descriptor
/// slice on the lowered text, then hands the original-cased slice to
/// [`super::oracle_effect::animation::parse_becomes_type_modifications`] which
/// reuses the existing animation type-sequence combinator for CR-205
/// token-by-token classification. One `AddType` per CR 205.2 core type and
/// one `AddSubtype` per CR 205.3 subtype are emitted; CR 205.4 supertypes are
/// recognized-and-discarded (animations don't grant supertypes).
pub(crate) fn parse_becomes_type_addition_modifications(
    tp: &TextPair<'_>,
) -> Vec<ContinuousModification> {
    type VE<'a> = OracleError<'a>;

    // Scan for the "becomes a"/"becomes an" phrase anywhere in the lowered
    // text, then locate the terminating "in addition to its other types"
    // clause. `scan_split_at_phrase` returns the lowered slice beginning at
    // the matched phrase.
    let Some((_, tail_lower)) = nom_primitives::scan_split_at_phrase(tp.lower, |i| {
        alt((
            tag::<_, _, VE>("becomes a "),
            tag::<_, _, VE>("becomes an "),
        ))
        .parse(i)
    }) else {
        return Vec::new();
    };
    let Ok::<_, nom::Err<VE<'_>>>((after_article_lower, _consumed)) =
        alt((tag("becomes a "), tag("becomes an "))).parse(tail_lower)
    else {
        return Vec::new();
    };

    // Extract the descriptor up to the first " in addition to" clause.
    let Ok::<_, nom::Err<VE<'_>>>((_, descriptor_lower)) =
        take_until(" in addition to")(after_article_lower)
    else {
        return Vec::new();
    };

    // Map the lowered descriptor back onto the original-cased text so the CR
    // 205.3 subtype grammar (which requires capitalized proper nouns) sees the
    // correct case.
    let start = tp.lower.len() - after_article_lower.len();
    let end = start + descriptor_lower.len();
    let descriptor_original = &tp.original[start..end];

    super::oracle_effect::animation::parse_becomes_type_modifications(descriptor_original)
}

/// CR 205.1a-b + CR 613.1d: bare "becomes a/an <descriptor>" type-changing
/// effects are replacement-form changes. Setting core card types replaces the
/// previous card-type set except for CR 205.1b's artifact-creature exception;
/// setting creature subtypes replaces the object's previous creature types.
pub(crate) fn parse_bare_becomes_type_replacement_modifications(
    tp: &TextPair<'_>,
) -> Vec<ContinuousModification> {
    type VE<'a> = OracleError<'a>;

    let Some((_, tail_lower)) = nom_primitives::scan_split_at_phrase(tp.lower, |i| {
        alt((
            tag::<_, _, VE>("becomes a "),
            tag::<_, _, VE>("becomes an "),
        ))
        .parse(i)
    }) else {
        return Vec::new();
    };
    let Ok::<_, nom::Err<VE<'_>>>((after_article_lower, _)) =
        alt((tag("becomes a "), tag("becomes an "))).parse(tail_lower)
    else {
        return Vec::new();
    };
    let (descriptor_lower, retained_core_type) =
        if let Some((descriptor_lower, retained_core_type)) =
            split_type_retention_clause(after_article_lower)
        {
            (descriptor_lower, Some(retained_core_type))
        } else {
            (after_article_lower, None)
        };
    if retained_core_type.is_none()
        && take_until::<_, _, VE>(" in addition to")
            .parse(descriptor_lower)
            .is_ok()
    {
        return Vec::new();
    }

    let Ok::<_, nom::Err<VE<'_>>>((_, descriptor_lower)) =
        parse_clause_before_optional_period(descriptor_lower)
    else {
        return Vec::new();
    };
    let (descriptor_lower, _) = strip_trailing_duration(descriptor_lower.trim());
    let descriptor_lower = descriptor_lower.trim();
    if descriptor_lower.is_empty() {
        return Vec::new();
    }

    let start = tp.lower.len() - after_article_lower.len();
    let end = start + descriptor_lower.len();
    let descriptor_original = &tp.original[start..end];
    let Some(spec) = super::oracle_effect::animation::parse_animation_spec(
        descriptor_original,
        &mut ParseContext::default(),
    ) else {
        return Vec::new();
    };
    let animation_modifications = super::oracle_effect::animation::animation_modifications(&spec);
    if let Some(core_type) = retained_core_type {
        let mut modifications = animation_modifications;
        if !modifications.contains(&ContinuousModification::AddType { core_type }) {
            modifications.push(ContinuousModification::AddType { core_type });
        }
        return modifications;
    }

    let core_types: Vec<CoreType> = animation_modifications
        .iter()
        .filter_map(|modification| match modification {
            ContinuousModification::AddType { core_type } => Some(*core_type),
            _ => None,
        })
        .collect();
    let keep_additive_core_types = core_types.len() == 2
        && core_types.contains(&CoreType::Artifact)
        && core_types.contains(&CoreType::Creature);

    let mut modifications = Vec::new();
    let mut set_core_types = false;
    let mut removed_subtype_sets = Vec::new();
    for modification in animation_modifications {
        if matches!(modification, ContinuousModification::AddType { .. }) {
            if core_types.is_empty() || keep_additive_core_types {
                modifications.push(modification);
            } else if !set_core_types {
                modifications.push(ContinuousModification::SetCardTypes {
                    core_types: core_types.clone(),
                });
                set_core_types = true;
            }
            continue;
        }

        if let ContinuousModification::AddSubtype { subtype } = &modification {
            let set = noncreature_subtype_set(subtype).unwrap_or(SubtypeSet::Creature);
            if !removed_subtype_sets.contains(&set) {
                modifications.push(ContinuousModification::RemoveAllSubtypes { set });
                removed_subtype_sets.push(set);
            }
        }
        modifications.push(modification);
    }
    modifications
}

/// CR 613.1d + CR 613.1g: "[pronoun]'s a/an <descriptor> [as long as <condition>]"
/// — self-referential conditional animation static. Covers:
///   - Dynamic-P/T-by-mana-value: "it's an artifact creature with power and
///     toughness each equal to its mana value" (Animate Artifact)
///   - Fixed P/T + types + keywords: "he's a 7/7 Dragon God creature with flying
///     and indestructible" (Grand Master of Flowers — CR 613.4b fixed P/T,
///     CR 613.1d type grant, CR 613.1g keyword grant)
///
/// Accepts gender-neutral and gendered pronouns ("it's", "~'s", "they're",
/// "he's", "she's"). Delegates body parsing to
/// `parse_animation_spec` + `animation_modifications` (which handles fixed P/T,
/// dynamic P/T-by-MV, types, subtypes, and keyword tails in one pass), falling
/// back to the prior type-only + MV-dynamic-P/T path if the spec parser returns
/// None.
pub(crate) fn parse_pronoun_becomes_type_static(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<StaticDefinition> {
    // STEP A.0 — peel an optional leading turn-restriction timing clause.
    // Mirror of `parse_compound_turn_counter_animation` (anthem.rs): the
    // alternate printing convention writes the turn restriction as a leading
    // timing prefix ("During your turn, ~ is a 4/4 ..." — Gideon-class; "During
    // turns other than yours, ~ is an artifact creature" — Midnight Mangler)
    // rather than a trailing "as long as it's your turn" clause. CR 611.3a: the
    // negated form lowers to `Not(DuringYourTurn)`. The peel is Option-returning,
    // so when absent it falls through to `*tp` unchanged and no condition is
    // attached.
    let (tp, turn_condition) = if let Some(rest) = nom_tag_tp(tp, "during your turn, ") {
        (rest, Some(StaticCondition::DuringYourTurn))
    } else if let Some(rest) = nom_tag_tp(tp, "during turns other than yours, ") {
        (
            rest,
            Some(StaticCondition::Not {
                condition: Box::new(StaticCondition::DuringYourTurn),
            }),
        )
    } else {
        (*tp, None)
    };

    // STEP A — peel a trailing " as long as <condition>" FIRST. The canonical
    // inverted-form rewrite produces "<effect> as long as <condition>"; the
    // condition must come off before the effect is parsed, or it leaks into
    // the " with " tail and never becomes a StaticCondition.
    let (effect_tp, condition_tp) = match tp.split_around(" as long as ") {
        Some((before, after)) => (before, Some(after)),
        None => (tp, None),
    };

    // STEP B — pronoun + article prefix. Accept gender-neutral ("it's", "~'s",
    // "they're") and gendered ("he's", "she's") pronouns; planeswalker
    // animation statics use gendered pronouns (Grand Master of Flowers, Kaito,
    // Gideon classes). Also accept the bare "is"-copula form ("~ is a/an ...")
    // produced when an inverted "As long as it's your turn, ~ is a ..." line
    // (Gideon Blackblade, #1155) is split by `parse_conditional_static` and the
    // pronoun-less effect clause "~ is a 4/4 ..." re-enters this parser.
    let body = nom_tag_tp(&effect_tp, "it's a ")
        .or_else(|| nom_tag_tp(&effect_tp, "it's an "))
        .or_else(|| nom_tag_tp(&effect_tp, "~'s a "))
        .or_else(|| nom_tag_tp(&effect_tp, "~'s an "))
        .or_else(|| nom_tag_tp(&effect_tp, "~ is a "))
        .or_else(|| nom_tag_tp(&effect_tp, "~ is an "))
        .or_else(|| nom_tag_tp(&effect_tp, "they're a "))
        .or_else(|| nom_tag_tp(&effect_tp, "they're an "))
        .or_else(|| nom_tag_tp(&effect_tp, "he's a "))
        .or_else(|| nom_tag_tp(&effect_tp, "he's an "))
        .or_else(|| nom_tag_tp(&effect_tp, "she's a "))
        .or_else(|| nom_tag_tp(&effect_tp, "she's an "))?;

    // STEP C — delegate body parsing to parse_animation_spec which handles
    // fixed P/T (CR 613.4b), dynamic P/T-by-mana-value, types (CR 613.1d),
    // subtypes (CR 205.3), and keyword tails (CR 613.1g) in one composable pass.
    let body_text = body.original.trim().trim_end_matches('.');
    let modifications = if let Some(spec) = super::oracle_effect::animation::parse_animation_spec(
        body_text,
        &mut ParseContext::default(),
    ) {
        super::oracle_effect::animation::animation_modifications(&spec)
    } else {
        // Fallback: type-token parse + mana-value dynamic P/T. Handles edge
        // cases where parse_animation_spec returns None (e.g., unusual clause
        // ordering not yet covered by the animation spec parser).
        let mut mods = Vec::new();
        let (type_part, with_tail) = match body.split_around(" with ") {
            Some((before, after)) => (before, Some(after)),
            None => (body, None),
        };
        mods.extend(
            super::oracle_effect::animation::parse_becomes_type_modifications(type_part.original),
        );
        if let Some(tail) = &with_tail {
            push_base_pt_mana_value_dynamic_modifications(&mut mods, tail.lower);
        }
        mods
    };

    if modifications.is_empty() {
        return None;
    }

    // CR 205.1a: an effect that sets an object's card type replaces its existing
    // card types unless it retains them (CR 205.1b). A pure non-creature
    // card-type change with no retention marker therefore REPLACES — Arixmethes,
    // Slumbering Isle's "it's a land. (It's not a creature.)" must stop the
    // Kraken from being a creature, not leave it a "Creature Land" (issue #5213).
    let modifications = maybe_replace_card_types(modifications, body.lower);

    // STEP D — attach the condition(s). The leading "during your turn, " timing
    // peel (STEP A.0) and the trailing " as long as <cond>" peel (STEP A) are
    // independent; either, both, or neither may be present.
    // CR 205.1b + CR 613.7: "~ is a [P/T] [types] creature ... that's still a
    // planeswalker" — additive type-change (AddType is non-replacing, so the
    // permanent retains its Planeswalker type while it is also a creature).
    let trailing_condition = condition_tp.map(|cond_tp| {
        let cond_text = cond_tp.original.trim().trim_end_matches('.');
        parse_static_condition(cond_text).unwrap_or(StaticCondition::Unrecognized {
            text: cond_text.to_string(),
        })
    });
    let condition = match (turn_condition, trailing_condition) {
        // CR 611.3a: when both a leading turn restriction and a trailing
        // "as long as" condition are present, compose via `And` rather than
        // dropping one (mirrors `parse_conditional_static` in anthem.rs).
        (Some(turn), Some(inner)) => Some(StaticCondition::And {
            conditions: vec![turn, inner],
        }),
        (Some(turn), None) => Some(turn),
        (None, Some(inner)) => Some(inner),
        (None, None) => None,
    };

    let mut def = StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(modifications)
        .description(text.to_string());
    if let Some(condition) = condition {
        def = def.condition(condition);
    }
    Some(def)
}

/// CR 205.1a / CR 205.1b: Decide whether a self type-change is a REPLACEMENT or
/// an additive grant. A set of `AddType` modifications is replaced by a single
/// `SetCardTypes` (which removes the object's prior card types, CR 205.1a) only
/// when ALL of these hold:
///   - every modification is an `AddType` (a pure card-type change — no P/T,
///     subtype, keyword, color, or supertype, which would signal an animation
///     that retains its card type),
///   - none of the added types is `Creature` (creature forms retain via CR
///     205.1b's "artifact creature" rule and P/T-bearing animations),
///   - the body carries no CR 205.1b retention marker ("in addition to" /
///     "still a[n]").
///
/// Otherwise the additive modifications are returned unchanged. This makes
/// Arixmethes' "it's a land" strip the Creature type while leaving creature
/// animations (Gideon Blackblade, Midnight Mangler, Circle of the Moon Druid)
/// on their additive path.
fn maybe_replace_card_types(
    modifications: Vec<ContinuousModification>,
    body_lower: &str,
) -> Vec<ContinuousModification> {
    if nom_primitives::scan_contains(body_lower, "in addition to")
        || nom_primitives::scan_contains(body_lower, "still a")
    {
        return modifications;
    }
    let mut core_types = Vec::new();
    for modification in &modifications {
        match modification {
            ContinuousModification::AddType { core_type } if *core_type != CoreType::Creature => {
                core_types.push(*core_type);
            }
            // Anything else (P/T, subtype, keyword, an added Creature type, …)
            // keeps the additive form.
            _ => return modifications,
        }
    }
    if core_types.is_empty() {
        return modifications;
    }
    vec![ContinuousModification::SetCardTypes { core_types }]
}

/// CR 205.2 + CR 613.1d + CR 613.4b + CR 611.3a: "Each noncreature <T> [you control]
/// is a[n] [<T>] creature with power and toughness each equal to its mana value
/// [as long as <condition>]." — March of the Machines class. The affirmative type
/// `<T>` must be artifact or enchantment. The second type token (if present) must
/// agree with `<T>`. Corpus members: March of the Machines, Karn, Silver Golem.
///
/// This is the noncreature-subject sibling of `parse_pronoun_becomes_type_static`
/// (which handles self-referential `it's a/an <types>` animations). Opalescence
/// (`"Each other non-Aura enchantment ..."`) starts with `"Each other"` and is
/// handled by a different parser arm — it is NOT in this class.
///
/// Composition: `nom_tag_tp` peels the subject prefix; `nom_target::parse_type_filter_word`
/// recognizes the affirmative type; `nom_tag_lower` (leading-space-anchored) peels
/// the optional controller clause and the copula; the dynamic-P/T-by-mana-value
/// tail is delegated to `push_base_pt_mana_value_dynamic_modifications`.
pub(crate) fn parse_each_noncreature_subject_is_creature_with_pt_mv(
    tp: &TextPair<'_>,
    description: &str,
) -> Option<StaticDefinition> {
    // STEP A — CR 611.3a: peel a trailing " as long as <condition>" FIRST.
    // The condition must come off before the effect is parsed, or it leaks into
    // the dynamic-P/T tail and never becomes a StaticCondition. Mirrors STEP A
    // of `parse_pronoun_becomes_type_static`.
    let (effect_tp, condition_tp) = match tp.split_around(" as long as ") {
        Some((before, after)) => (before, Some(after)),
        None => (*tp, None),
    };

    // STEP C.1 — strip "each noncreature " subject prefix.
    let rest_tp = nom_tag_tp(&effect_tp, "each noncreature ")?;

    // STEP C.2 — affirmative type word. Direct nom call: (remainder, value) ordering.
    let (after_subject_lower, affirmative_type) =
        nom_target::parse_type_filter_word(rest_tp.lower).ok()?;
    if !matches!(
        affirmative_type,
        TypeFilter::Artifact | TypeFilter::Enchantment
    ) {
        return None;
    }

    // STEP C.3 — optional " you control" (leading-space-anchored).
    // CR 109.5: "you/your" rebinding.
    let (rest_after_controller, controller): (&str, Option<ControllerRef>) =
        match nom_tag_lower(after_subject_lower, after_subject_lower, " you control") {
            Some(rest) => (rest, Some(ControllerRef::You)),
            None => (after_subject_lower, None),
        };

    // STEP C.4 — copula (leading-space-anchored). Try " is an " first (longer match).
    let after_copula = nom_tag_lower(rest_after_controller, rest_after_controller, " is an ")
        .or_else(|| nom_tag_lower(rest_after_controller, rest_after_controller, " is a "))?;

    // STEP D — optional adjective matching affirmative_type, then required "creature".
    // March of the Machines: "is an artifact creature ..." — adjective present.
    // Hypothetical sibling "is a creature ...": adjective absent (fall through).
    let after_adjective = match nom_target::parse_type_filter_word(after_copula) {
        Ok((rest, adj)) if adj == affirmative_type => rest,
        _ => after_copula,
    };
    // When STEP D consumed an adjective, `after_adjective` begins with " creature"
    // (the space between adjective and noun is still pending). When STEP D fell
    // through, `after_adjective == after_copula` already had its leading space
    // consumed by the " is a "/" is an " copula and now begins with "creature"
    // directly (no leading space). Both branches must succeed for the union.
    let after_creature = nom_tag_lower(after_adjective, after_adjective, " creature")
        .or_else(|| nom_tag_lower(after_adjective, after_adjective, "creature"))?;

    // STEP E — emit modifications.
    // CR 205.2 + CR 613.1d: Layer 4 add of the Creature core type.
    // CR 613.4b: Layer 7b set of base power/toughness (delegated).
    let mut modifications = vec![ContinuousModification::AddType {
        core_type: CoreType::Creature,
    }];
    if !push_base_pt_mana_value_dynamic_modifications(&mut modifications, after_creature) {
        return None;
    }

    // STEP F — build the affected-object selector: [<T>, Non(Creature)] + optional controller.
    let mut typed = TypedFilter::new(affirmative_type)
        .with_type(TypeFilter::Non(Box::new(TypeFilter::Creature)));
    if let Some(ctrl) = controller {
        typed = typed.controller(ctrl);
    }
    let affected = TargetFilter::Typed(typed);

    // STEP G — build the continuous static and re-attach the condition peeled
    // in STEP A. S8: description is the ORIGINAL line, not any peeled remainder.
    let mut def = StaticDefinition::continuous()
        .affected(affected)
        .modifications(modifications)
        .description(description.to_string());
    if let Some(cond_tp) = condition_tp {
        let cond_text = cond_tp.original.trim().trim_end_matches('.');
        let condition =
            parse_static_condition(cond_text).unwrap_or(StaticCondition::Unrecognized {
                text: cond_text.to_string(),
            });
        def = def.condition(condition);
    }
    Some(def)
}

/// CR 611.3 + CR 613.1d + CR 613.4b + CR 205.1b: "[During your turn, ]each
/// `<non-X Y>` and `<non-Z W>` [you control] [with mana value N or greater] is
/// a `<predicate>`" — a compound-subject continuous animation whose subject is
/// a heterogeneous union of negated-type legs sharing a trailing qualifier
/// (controller / mana-value threshold), and whose predicate grants a fixed
/// P/T plus type change PLUS a mixed bare-keyword/quoted-ability list. Corpus
/// member: Bello, Bard of the Brambles — "During your turn, each non-Equipment
/// artifact and non-Aura enchantment you control with mana value 4 or greater
/// is a 4/4 Elemental creature in addition to its other types and has
/// indestructible, haste, and \"Whenever this creature deals combat damage to
/// a player, draw a card.\""
///
/// Unlike `parse_compound_all_subjects_type_change` (Life and Limb: "All `<X>`
/// and all `<Y>` are ...", a REPEATED `all` quantifier per conjunct, each
/// resolved through a hand-rolled conjunct splitter), this class has a SINGLE
/// leading `each` quantifier and per-conjunct negated-type exclusions
/// ("non-Equipment", "non-Aura") — so the subject is delegated wholesale to
/// the general target-phrase grammar (`parse_type_phrase`) instead. That
/// grammar already recurses per "and"-leg (restarting its own leading `non-`
/// scan on each recursive call — see `starts_with_type_word`'s `non-` arm) and
/// backfills the shared trailing qualifiers (controller, mana value) from the
/// last leg onto every earlier leg via `distribute_controller_to_or` /
/// `distribute_properties_to_or`. Reusing it is a straight class-coverage win
/// over re-deriving that machinery in a bespoke splitter.
///
/// The predicate composes two parsers: `parse_animation_spec` (base P/T +
/// leading type/subtype grant, CR 613.4b + CR 205.1b layer 7b/4) and
/// `parse_additive_type_clause_modifications` — the SINGLE owner of everything
/// past that leading grant. The additive helper captures any EXTRA type noun
/// before "in addition to ..." (none for Bello, present for a Life-and-Limb-shaped
/// sibling) AND routes the trailing "... and has <X>" conjunct through
/// `parse_continuous_modifications`, which subsumes both the bare-keyword list
/// and the quoted-ability parse (CR 604.1 trigger / CR 702 keyword). A prior
/// revision parsed that tail a SECOND time locally to recover bare keywords the
/// helper then dropped; the helper no longer drops them, so the local re-parse
/// was pure duplication (it emitted each bare keyword twice) and has been
/// removed — the tail now has exactly one owner.
pub(crate) fn parse_each_compound_subject_type_change(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<StaticDefinition> {
    // STEP A — CR 611.3a: peel an optional leading "during your turn, " timing
    // window before the subject is parsed (mirrors the same idiom in cda.rs /
    // dispatch.rs / evasion.rs / keyword_grant.rs).
    let (tp, turn_condition) = match nom_tag_tp(tp, "during your turn, ") {
        Some(rest) => (rest, Some(StaticCondition::DuringYourTurn)),
        None => (*tp, None),
    };

    // STEP B — peel the single "each " subject quantifier.
    let rest_tp = nom_tag_tp(&tp, "each ")?;

    // STEP C — split subject from predicate on the copula. A singular "each
    // ..." subject always takes "is" (never "are").
    let (subject_tp, predicate_tp) = rest_tp.split_around(" is ")?;

    // STEP D — delegate the ENTIRE subject phrase to the general target-phrase
    // grammar instead of hand-rolling a conjunct splitter (see doc comment).
    let (affected, subject_rest) = parse_type_phrase(subject_tp.original);
    if !subject_rest.trim().is_empty() {
        return None;
    }
    // Guard: only claim a genuine compound (2+ leg) subject here — a
    // single-subject "each <T> is ..." line falls through to
    // `parse_each_noncreature_subject_is_creature_with_pt_mv` and friends.
    let TargetFilter::Or { filters } = &affected else {
        return None;
    };
    if filters.len() < 2 || !filters.iter().all(|f| matches!(f, TargetFilter::Typed(_))) {
        return None;
    }

    // STEP E — the predicate must be a creature-animation predicate carrying
    // the CR 205.1b additive marker; a bare replacement compound ("... are
    // Zombies") belongs to a type-replacement handler, not this additive one.
    let predicate = predicate_tp.original.trim().trim_end_matches('.');
    let predicate_lower = predicate.to_lowercase();
    if !nom_primitives::scan_contains(&predicate_lower, "creature") {
        return None;
    }
    if !nom_primitives::scan_contains(&predicate_lower, "in addition to its other")
        && !nom_primitives::scan_contains(&predicate_lower, "in addition to their other")
    {
        return None;
    }

    // STEP F — base P/T + leading type/subtype grant (CR 613.4b + CR 205.1b).
    let spec = super::oracle_effect::animation::parse_animation_spec(
        predicate,
        &mut ParseContext::default(),
    )?;
    let mut modifications = super::oracle_effect::animation::animation_modifications(&spec);
    if modifications.is_empty() {
        return None;
    }

    // STEP G — the shared additive-type-clause helper is the SINGLE owner of
    // everything after the animation spec's leading grant: any EXTRA type/subtype
    // noun before "in addition to ...", AND the full "... and has <X>" tail
    // (bare keywords + a quoted granted ability, CR 604.1 trigger / CR 702
    // keyword). The helper routes that tail through `parse_continuous_modifications`,
    // which subsumes both the bare-keyword list and the quoted-ability parse — so
    // this one call captures the mixed list without a second, redundant tail
    // parser here. Dedup against the animation spec keeps the shared leading
    // type/subtype (AddType Creature / AddSubtype Elemental) single.
    if let Some(additive) = parse_additive_type_clause_modifications(&format!("~ is {predicate}")) {
        for modification in additive {
            if !modifications.contains(&modification) {
                modifications.push(modification);
            }
        }
    }

    let mut def = StaticDefinition::continuous()
        .affected(affected)
        .modifications(modifications)
        .description(text.to_string());
    if let Some(condition) = turn_condition {
        def = def.condition(condition);
    }
    Some(def)
}

/// CR 205.1a: Parse "All permanents are [type] in addition to their other types."
/// Handles global type-addition effects like Mycosynth Lattice ("artifacts") and
/// Enchanted Evening ("enchantments").
pub(crate) fn parse_all_permanents_are_type(
    tp: &TextPair<'_>,
    description: &str,
) -> Option<StaticDefinition> {
    let rest_tp = nom_tag_tp(tp, "all permanents are ")?;
    let rest = rest_tp.lower.trim_end_matches('.');
    let type_part = rest.strip_suffix(" in addition to their other types")?; // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
                                                                             // Map the type word to a CoreType
    let core_type = match type_part.trim() {
        "artifacts" => CoreType::Artifact, // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        "enchantments" => CoreType::Enchantment, // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        "creatures" => CoreType::Creature, // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        "lands" => CoreType::Land, // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        _ => return None,
    };
    Some(
        StaticDefinition::continuous()
            .affected(TargetFilter::Typed(TypedFilter::permanent()))
            .modifications(vec![ContinuousModification::AddType { core_type }])
            .description(description.to_string()),
    )
}

/// CR 613.1e + CR 105.1 / CR 105.2c / CR 105.3: Parse "All [subject] are [color(s)]."
/// — a global color-defining static ability (Layer 5).
///
/// - CR 105.1 enumerates the five colors.
/// - CR 105.2c: "A colorless object has no color." → empty color set.
/// - CR 105.3 authorizes color-changing effects (new color replaces previous
///   colors unless the effect says "in addition").
/// - CR 613.1e places color-changing effects in Layer 5.
///
/// Covers the class of "All X are Y" color-setting statics — Darkest Hour
/// ("All creatures are black."), Thran Lens ("All permanents are colorless."),
/// Ghostflame Sliver ("All Slivers are colorless."), and every future card
/// sharing this shape. Composes existing building blocks rather than writing
/// one-off string dispatch:
///
/// - `nom_target::parse_type_filter_word` recognizes every plural core-type
///   subject (creatures, permanents, lands, artifacts, enchantments,
///   planeswalkers, battles) AND every plural subtype in the shared subtype
///   table (Slivers, Elves, Treasures, Zombies, ...).
/// - `parse_color_predicate` composes a `tag("colorless")` combinator with
///   the shared `parse_color_list` (giving single colors, "X and Y", and
///   "X, Y, and Z" forms for free per CR 105.1).
/// - `typed_filter_for_subtype` routes artifact/land/enchantment subtypes to
///   their correct core type (e.g., Treasure → Artifact, not Creature).
///
/// Dispatch ordering constraints are documented at the call site in
/// `parse_static_line_inner` and pinned by three regression tests below.
pub(crate) fn parse_all_subject_are_color(
    tp: &TextPair<'_>,
    description: &str,
) -> Option<StaticDefinition> {
    let rest_tp = nom_tag_tp(tp, "all ")?;
    // Subject: single shared combinator for both core types and plural subtypes.
    let (after_subject, type_filter) = nom_target::parse_type_filter_word(rest_tp.lower).ok()?;
    // Copula — require " are " with surrounding whitespace so we never eat
    // words like "aren't" or "area".
    let after_verb = nom_tag_lower(after_subject, after_subject, " are ")?;
    // Strip the terminal period (structural cleanup on a post-combinator
    // chunk — the subject and copula have already been consumed), then the
    // predicate must fully parse as a color expression or follow-on clauses
    // route elsewhere.
    let predicate = after_verb.trim().trim_end_matches('.');
    let (colors, extra_mods) = parse_color_and_trailing_modifications(predicate)?;

    let affected = match type_filter {
        TypeFilter::Subtype(s) => TargetFilter::Typed(typed_filter_for_subtype(&s)),
        other => TargetFilter::Typed(TypedFilter::new(other)),
    };
    let mut modifications = vec![ContinuousModification::SetColor { colors }];
    modifications.extend(extra_mods);
    Some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(modifications)
            .description(description.to_string()),
    )
}

/// CR 105.2 + CR 613.1e (Layer 5) + CR 613.1f: a color-defining static predicate
/// may compose a color expression with trailing keyword/pump modifications — "All
/// creatures are black and have deathtouch" (Onakke Catacomb), "... are white and
/// get +1/+1". The whole-predicate color case ("... are black") returns the colors
/// with no trailing modifications; the compound case peels the leading color
/// expression (itself possibly a multi-color "black and green" list) and parses
/// the "and <keyword|pump>" tail via [`parse_continuous_modifications`], so the
/// color is composed with — never shadowed by — the other modifications. Returns
/// `None` when the predicate is neither a pure color nor a genuine
/// "<color> and <modification>" compound, so non-color predicates route elsewhere.
fn parse_color_and_trailing_modifications(
    predicate: &str,
) -> Option<(Vec<ManaColor>, Vec<ContinuousModification>)> {
    if let Some(colors) = parse_color_predicate(predicate) {
        return Some((colors, Vec::new()));
    }
    let (color_part, _, mod_part) =
        nom_primitives::scan_preceded(predicate, and_modification_boundary)?;
    let colors = parse_color_predicate(color_part.trim())?;
    let mods = parse_continuous_modifications(mod_part.trim());
    if mods.is_empty() {
        return None;
    }
    Some((colors, mods))
}

/// The single `" and <modification>"` boundary that separates a color expression
/// from a trailing keyword/pump modification. Gated on a modification verb
/// (`have`/`get`/`gain`/`can't`) so a multi-color `"black and green"` list is
/// never split mid-color. Consumes only `"and "` (the verb is peeked) so the
/// remainder handed to [`parse_continuous_modifications`] keeps its leading verb.
fn and_modification_boundary(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        (
            tag::<_, _, OracleError<'_>>("and "),
            nom::combinator::peek(alt((
                tag("have "),
                tag("has "),
                tag("gets "),
                tag("get "),
                tag("gains "),
                tag("gain "),
                tag("can't "),
                tag("cant "),
                tag("cannot "),
            ))),
        ),
    )
    .parse(input)
}

/// CR 105.3: the additive "in addition to `<pronoun>` other colors" retain-
/// suffix, shared by the single-subject ([`parse_subject_is_color`]) and
/// multi-zone-compound ([`parse_compound_multi_zone_color_static`]) color
/// handlers. Mirrors [`parse_chosen_type_additive_suffix`] for the type axis.
fn parse_chosen_color_additive_suffix<'a>(input: &'a str, pronoun: &str) -> OracleResult<'a, ()> {
    let (input, _) = tag(" in addition to ").parse(input)?;
    let (input, _) = tag(pronoun).parse(input)?;
    let (input, _) = tag(" other colors").parse(input)?;
    Ok((input, ()))
}

/// CR 109.2 + CR 400.1 + CR 611.3a: off-battlefield card population for the
/// Painter's Servant / Mycosynth Lattice Oxford subject. "Cards that aren't on
/// the battlefield" means every zone where a card can sit *except* the
/// battlefield and the stack (stack objects are spells, covered by a sibling
/// leg). Matches the zone set used by off-battlefield keyword grants
/// (`keyword_grant` "cards you own that aren't on the battlefield") plus
/// Library (Painter applies in all zones globally, not only "you own").
fn off_battlefield_card_zones() -> Vec<Zone> {
    vec![
        Zone::Library,
        Zone::Hand,
        Zone::Graveyard,
        Zone::Exile,
        Zone::Command,
    ]
}

/// CR 109.2 + CR 400.1: one Oxford leg of the multi-zone color subject class.
/// Built for the category of legs that appear in Painter's Servant / Mycosynth
/// Lattice — not a single card. Bare "spells" lowers via the same stack scoping
/// that Secret Arcade's spell conjunct uses (`TargetFilter::StackSpell`).
fn parse_multi_zone_color_subject_leg(input: &str) -> OracleResult<'_, TargetFilter> {
    // "cards that aren't on the battlefield" — optional plural spelling.
    if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("cards that aren't on the battlefield"),
        tag("card that isn't on the battlefield"),
        tag("cards that are not on the battlefield"),
    ))
    .parse(input)
    {
        return Ok((
            rest,
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::InAnyZone {
                zones: off_battlefield_card_zones(),
            }])),
        ));
    }

    // Bare "spells" / "spell" → stack objects (CR 109.2).
    if let Ok((rest, _)) = alt((tag::<_, _, OracleError<'_>>("spells"), tag("spell"))).parse(input)
    {
        return Ok((rest, TargetFilter::StackSpell));
    }

    // Bare "permanents" / "permanent" → battlefield permanents.
    if let Ok((rest, _)) =
        alt((tag::<_, _, OracleError<'_>>("permanents"), tag("permanent"))).parse(input)
    {
        return Ok((
            rest,
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Permanent)),
        ));
    }

    Err(nom::Err::Error(OracleError::new(
        input,
        nom::error::ErrorKind::Tag,
    )))
}

/// CR 611.3a: peel an Oxford / dual-leg multi-zone color subject into a genuine
/// `Or` of 2+ zone-scoped filters. Accepts the Painter's Servant / Mycosynth
/// Lattice canonical form
/// `"All cards that aren't on the battlefield, spells, and permanents"` and the
/// dual-leg `"…spells and permanents"` compression. Declines a single-leg
/// subject so Shifting Sky / Darkest Hour stay with the single-subject color
/// handlers.
fn parse_multi_zone_oxford_color_subject(input: &str) -> OracleResult<'_, TargetFilter> {
    let (input, _) = opt(alt((tag::<_, _, OracleError<'_>>("all "), tag("each ")))).parse(input)?;

    let (mut remaining, first) = parse_multi_zone_color_subject_leg(input)?;
    let mut filters = vec![first];

    // ", <leg>" middle legs (Oxford comma list).
    loop {
        let Ok((after_comma, _)) = tag::<_, _, OracleError<'_>>(", ").parse(remaining) else {
            break;
        };
        // Optional "and " after the final Oxford comma.
        let after_and = match tag::<_, _, OracleError<'_>>("and ").parse(after_comma) {
            Ok((rest, _)) => rest,
            Err(_) => after_comma,
        };
        let Ok((rest, leg)) = parse_multi_zone_color_subject_leg(after_and) else {
            break;
        };
        filters.push(leg);
        remaining = rest;
    }

    // Dual-leg / trailing " and <leg>" without a preceding comma.
    if let Ok((after_and, _)) = tag::<_, _, OracleError<'_>>(" and ").parse(remaining) {
        let (rest, leg) = parse_multi_zone_color_subject_leg(after_and)?;
        filters.push(leg);
        remaining = rest;
    }

    let remaining = remaining.trim();
    if !remaining.is_empty() {
        return Err(nom::Err::Error(OracleError::new(
            remaining,
            nom::error::ErrorKind::Eof,
        )));
    }
    if filters.len() < 2 {
        return Err(nom::Err::Error(OracleError::new(
            input,
            nom::error::ErrorKind::Verify,
        )));
    }
    Ok((remaining, TargetFilter::Or { filters }))
}

/// CR 105.3 + CR 613.1e: additive chosen-color predicate
/// `"the chosen color in addition to their other colors[.]"`.
fn parse_chosen_color_additive_predicate(input: &str) -> OracleResult<'_, ()> {
    let (input, _) = tag("the chosen color").parse(input)?;
    let (input, ()) = parse_chosen_color_additive_suffix(input, "their")?;
    let (input, _) = opt(tag(".")).parse(input)?;
    eof.parse(input)?;
    Ok((input, ()))
}

/// CR 105.2c / CR 105.1 / CR 105.3 + CR 613.1e: color predicate owned by the
/// multi-zone compound handler — additive chosen color (Painter's Servant) or
/// a fully-consumed fixed/colorless expression (Mycosynth Lattice).
fn parse_multi_zone_color_predicate(input: &str) -> OracleResult<'_, Vec<ContinuousModification>> {
    if let Ok((rest, ())) = parse_chosen_color_additive_predicate(input) {
        // CR 105.3: Painter's Servant retain-suffix → add (not replace).
        return Ok((
            rest,
            vec![ContinuousModification::AddChosenColor {
                mode: ColorChangeMode::Add,
            }],
        ));
    }

    let (input, predicate) = alt((
        terminated(take_until::<_, _, OracleError<'_>>("."), tag(".")),
        rest,
    ))
    .parse(input)?;
    eof::<_, OracleError<'_>>(input)?;
    let colors = parse_color_predicate(predicate.trim())
        .ok_or_else(|| nom::Err::Error(OracleError::new(predicate, nom::error::ErrorKind::Fail)))?;
    Ok((input, vec![ContinuousModification::SetColor { colors }]))
}

/// CR 611.3 + CR 611.3a + CR 105.3 + CR 613.1e: compound-subject sibling of
/// [`parse_subject_is_color`]. "`All cards that aren't on the battlefield,
/// spells, and permanents are <color predicate>`" (Painter's Servant /
/// Mycosynth Lattice) — an Oxford multi-zone subject the single-subject color
/// dispatcher can't reach because (1) `parse_continuous_subject_filter` has no
/// Oxford peeler for this class and (2) Painter's additive chosen-color suffix
/// rejects `all_consuming("the chosen color")`.
///
/// Structural mirror of [`parse_compound_you_control_chosen_type_static`] (#5406):
/// own only the color predicate, require a genuine `Or` of 2+ zone-scoped legs,
/// reuse the fully-wired `AddChosenColor` / `SetColor` runtime — no new variant.
pub(crate) fn parse_compound_multi_zone_color_static(
    tp: &TextPair<'_>,
    description: &str,
) -> Option<StaticDefinition> {
    let (subject_tp, predicate_tp) = tp.split_around(" are ")?;
    let (_, affected) = parse_multi_zone_oxford_color_subject(subject_tp.lower.trim()).ok()?;
    match &affected {
        TargetFilter::Or { filters } if filters.len() >= 2 => {}
        _ => return None,
    }

    let (modifications, _) = nom_on_lower(
        predicate_tp.original,
        predicate_tp.lower,
        parse_multi_zone_color_predicate,
    )?;

    Some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(modifications)
            .description(description.to_string()),
    )
}

/// CR 613.1e (Layer 5) + CR 105.2 / CR 105.3: Parse a color-defining static of
/// the form "[subject] is/are [color expression]." for an ARBITRARY filter
/// subject — generalizing [`parse_all_subject_are_color`] (which only accepts the
/// `"All ..."` quantifier) to the full subject grammar via
/// `parse_continuous_subject_filter` (handles "Each", "All", controller suffixes,
/// "nonland permanent you control", etc.).
///
/// Two predicate families compose here:
/// - Fixed colors / "all colors" / "colorless" via `parse_color_predicate`
///   (Leyline of the Guildpact: "Each nonland permanent you control is all
///   colors" → `SetColor(WUBRG)`).
/// - "the chosen color" → `AddChosenColor`, reading the source's
///   `ChosenAttribute::Color` (Shimmerwilds Growth: "Enchanted land is the chosen
///   color" — a preceding `As ~ enters, choose a color` binds the attribute).
///   The additive retain-suffix `"in addition to their/its other colors"`
///   (CR 105.3) is accepted via [`parse_chosen_color_additive_suffix`].
///
/// The copula is " is " (singular subject) or " are " (plural subject); both
/// route to the same color modification. Dispatched AFTER the specialized
/// `parse_all_subject_are_color` ("All ..." quantifier, with correct
/// artifact/land subtype core-type routing) and `parse_self_subject_is_color_cda`
/// (self-referential CDA color lines, all-zone + `characteristic_defining`), so
/// those keep ownership of their cases and this branch only claims the residual
/// general-filter subjects. A self-referential subject is declined here outright
/// (it must be a CDA, never a plain Layer-5 static). Multi-zone Oxford compounds
/// belong to [`parse_compound_multi_zone_color_static`].
pub(crate) fn parse_subject_is_color(
    tp: &TextPair<'_>,
    description: &str,
) -> Option<StaticDefinition> {
    // Split on the copula. " are " is tried first so a plural subject ending in
    // "...s is..." cannot be mis-split (there is no such grammatical form for
    // these statics; the copula is always a whole word with surrounding spaces).
    let (subject_tp, predicate_tp) = tp
        .split_around(" is ")
        .or_else(|| tp.split_around(" are "))?;
    let subject = subject_tp.original.trim();
    let predicate = predicate_tp.original.trim().trim_end_matches('.');
    let predicate_lower = predicate.to_lowercase();

    // Decline the multi-zone Oxford compound subject so the dedicated compound
    // handler owns it (Painter's Servant / Mycosynth Lattice).
    if parse_multi_zone_oxford_color_subject(subject_tp.lower.trim()).is_ok() {
        return None;
    }

    // The subject must resolve to a concrete filter — otherwise this is not a
    // color-defining static (a bare "It is ..." / "this is ..." anaphor falls
    // through to other dispatch branches). Attached subjects ("Enchanted land",
    // Shimmerwilds Growth) carry the `EnchantedBy` filter; the general subject
    // grammar covers controller-scoped/quantified filters (Leyline of the
    // Guildpact: "Each nonland permanent you control").
    let subject_lower = subject.to_lowercase();
    // `attached_subject_filter` matches "enchanted <type> " WITH a trailing
    // space, so probe a space-suffixed copy and require the remainder to be empty.
    let subject_with_space = format!("{subject} ");
    let subject_space_lower = format!("{subject_lower} ");
    let attached = attached_subject_filter(&TextPair::new(
        subject_with_space.as_str(),
        subject_space_lower.as_str(),
    ));
    let affected = match attached {
        Some((filter, rest)) if rest.trim().is_empty() => filter,
        _ => parse_continuous_subject_filter(subject)?,
    };

    // CR 604.3: a self-referential color line ("~ is colorless") is a
    // characteristic-defining ability owned by `parse_self_subject_is_color_cda`
    // (it sets `characteristic_defining` and functions in all zones). Decline so
    // it is never emitted as a plain Layer-5 static.
    if matches!(affected, TargetFilter::SelfRef) {
        return None;
    }

    // CR 105.3: "the chosen color" [+ optional additive retain-suffix] reads the
    // source's chosen color attribute. Bare form → set/replace
    // ([`ColorChangeMode::Set`], Shimmerwilds Growth / Shifting Sky). Additive
    // "in addition to their/its other colors" → retain
    // ([`ColorChangeMode::Add`], Painter's Servant class). One composed
    // all_consuming parser owns both forms.
    let chosen_color_mode = all_consuming(|i| {
        let (i, _) = tag::<_, _, OracleError<'_>>("the chosen color").parse(i)?;
        let (i, additive) = opt(alt((
            |i| parse_chosen_color_additive_suffix(i, "their"),
            |i| parse_chosen_color_additive_suffix(i, "its"),
        )))
        .parse(i)?;
        Ok((
            i,
            if additive.is_some() {
                ColorChangeMode::Add
            } else {
                ColorChangeMode::Set
            },
        ))
    })
    .parse(predicate_lower.as_str())
    .ok()
    .map(|(_, mode)| mode);

    if let Some(mode) = chosen_color_mode {
        return Some(
            StaticDefinition::continuous()
                .affected(affected)
                .modifications(vec![ContinuousModification::AddChosenColor { mode }])
                .description(description.to_string()),
        );
    }

    // Fixed colors / "all colors" / "colorless" (CR 105.1 / CR 105.2 / CR 105.2c),
    // optionally composed with a trailing keyword/pump modification
    // ("... are black and have deathtouch" — Onakke Catacomb).
    let (colors, extra_mods) = parse_color_and_trailing_modifications(&predicate_lower)?;
    let mut modifications = vec![ContinuousModification::SetColor { colors }];
    modifications.extend(extra_mods);
    Some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(modifications)
            .description(description.to_string()),
    )
}

/// CR 205.4b + CR 613.1d (Layer 4): Parse a supertype-defining static of the form
/// "[subject] is/are [no longer|not] [supertype]." — the supertype sibling of
/// [`parse_subject_is_color`]. Adds a supertype (Leyline of Singularity: "All
/// nonland permanents are legendary"; Sixth Stage of Magic Design: "All creatures
/// are legendary") or removes one via a "no longer"/"not" negation (Melting: "All
/// lands are no longer snow").
///
/// Reuses the shared subject grammar (`parse_continuous_subject_filter`, which
/// handles "All"/"Each", "nonland permanents", controller suffixes), the shared
/// `parse_supertype_word` token (the full CR 205.4a set: legendary/basic/snow/
/// world/ongoing), and the existing
/// `ContinuousModification::AddSupertype`/`RemoveSupertype` runtime (applied at
/// Layer 4 in `game/layers.rs`) — no new variant or runtime. Dispatched after the
/// color/land-type branches; the supertype predicate is disjoint from color and
/// land-type words, so no branch is stolen. `all_consuming` on the supertype word
/// keeps this to the BARE form ("… are legendary."): a trailing tail ("… in
/// addition to their other types") or unrecognized predicate leaves the line
/// unsupported (returns `None`) rather than being mis-claimed. A self-referential
/// subject is declined — it would be a CDA, not a plain Layer-4 static.
pub(crate) fn parse_subject_is_supertype(
    tp: &TextPair<'_>,
    description: &str,
) -> Option<StaticDefinition> {
    let (subject_tp, predicate_tp) = tp
        .split_around(" is ")
        .or_else(|| tp.split_around(" are "))?;
    let subject = subject_tp.original.trim();
    let predicate = predicate_tp.original.trim().trim_end_matches('.').trim();

    let affected = parse_continuous_subject_filter(subject)?;
    // CR 604.3: a self-referential line would be a CDA, not a Layer-4 static.
    if matches!(affected, TargetFilter::SelfRef) {
        return None;
    }
    // CR 613 dispatch ownership: the attached "Enchanted/Equipped [type] is
    // [supertype]" Aura/Equipment form is owned by the predicate seam
    // (`parse_supertype_grant` via `parse_continuous_gets_has` — Glittering Frost,
    // In Bolas's Clutches). Decline an attached-subject filter here so this general
    // "[subject] is/are [supertype]" static path owns only the standalone-subject
    // form and never double-handles an attached-subject grant.
    if let TargetFilter::Typed(ref tf) = affected {
        if tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::EnchantedBy | FilterProp::EquippedBy))
        {
            return None;
        }
    }

    // CR 205.4b: an object can gain or lose a supertype ("When an object gains or
    // loses a supertype…") — an optional "no longer"/"not" negation flips this
    // parse to a supertype REMOVAL.
    let predicate_lower = predicate.to_lowercase();
    let (supertype_input, is_remove) = match opt(alt((
        tag::<_, _, OracleError<'_>>("no longer "),
        tag("not "),
    )))
    .parse(predicate_lower.as_str())
    {
        Ok((rest, negation)) => (rest, negation.is_some()),
        Err(_) => (predicate_lower.as_str(), false),
    };
    let (_, supertype) = all_consuming(nom_target::parse_supertype_word)
        .parse(supertype_input)
        .ok()?;

    let modification = if is_remove {
        ContinuousModification::RemoveSupertype { supertype }
    } else {
        ContinuousModification::AddSupertype { supertype }
    };
    Some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(vec![modification])
            .description(description.to_string()),
    )
}

/// CR 305.7: Decompose the predicate of a land type-changing static into layer-4
/// modifications — replacement (`SetBasicLandType`), additive (`AddSubtype`), or
/// all-basic-types (`AddAllBasicLandTypes`). Shared by the single-subject and
/// compound-subject land type-change handlers.
fn parse_land_type_change_modifications(rest: &str) -> Option<Vec<ContinuousModification>> {
    let lower_rest = rest.to_lowercase();

    // "every basic land type in addition to their other types"
    if nom_tag_lower(&lower_rest, &lower_rest, "every basic land type").is_some()
        && nom_primitives::scan_contains(&lower_rest, "in addition to")
    {
        return Some(vec![ContinuousModification::AddAllBasicLandTypes]);
    }

    // "[Type] in addition to {its/their} other {land }types" → AddSubtype (additive)
    if let Some(type_part) = strip_in_addition_suffix(&lower_rest) {
        let basic_type = parse_basic_land_type_plural(type_part.trim())?;
        return Some(vec![ContinuousModification::AddSubtype {
            subtype: basic_type.as_subtype_str().to_string(),
        }]);
    }

    // CR 305.7: Replacement semantics — "[Type]" or "[Types]" → SetBasicLandType
    // Try multi-type list first: "Mountain, Forest, and Plains"
    if let Some(types) = parse_basic_land_type_list(rest.trim()) {
        if types.len() == 1 {
            return Some(vec![ContinuousModification::SetBasicLandType {
                land_type: types[0],
            }]);
        }
        // CR 305.7: Multiple types — first SetBasicLandType clears old subtypes,
        // subsequent AddSubtype entries add the remaining types.
        let mut mods = vec![ContinuousModification::SetBasicLandType {
            land_type: types[0],
        }];
        for &lt in &types[1..] {
            mods.push(ContinuousModification::AddSubtype {
                subtype: lt.as_subtype_str().to_string(),
            });
        }
        return Some(mods);
    }

    None
}

/// CR 305.7: Parse "[Subject] lands are [type]" land type-changing static abilities.
/// Handles replacement ("Nonbasic lands are Mountains"), additive ("Each land is a
/// Swamp in addition to its other land types"), and all-basic-types ("Lands you control
/// are every basic land type in addition to their other types").
pub(crate) fn parse_land_type_change(tp: &TextPair<'_>, text: &str) -> Option<StaticDefinition> {
    let (subject_tp, rest_tp) = tp
        .split_around(" are ")
        .or_else(|| tp.split_around(" is a "))
        .or_else(|| tp.split_around(" is an "))
        .or_else(|| tp.split_around(" is "))?;
    let subject = subject_tp.original;
    let rest = rest_tp.original.trim().trim_end_matches('.');

    // Only proceed if subject is a land-type-change subject (avoids matching non-land patterns).
    let affected = parse_land_type_change_subject(subject)?;
    let modifications = parse_land_type_change_modifications(rest)?;
    Some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(modifications)
            .description(text.to_string()),
    )
}

/// CR 613.4b + CR 205.1b: Merge a creature-animation predicate with the additive
/// type/subtype grants past `parse_animation_spec`'s internal `" and "` stop.
/// `parse_animation_spec` supplies base P/T (layer 7b), set color (layer 5),
/// and leading creature type/subtype grants; `parse_additive_type_clause_modifications`
/// supplies the trailing `"and <type> lands in addition to their other types"`
/// nouns (layer 4). Only additive `AddType` / `AddSubtype` grants are merged —
/// the animation spec's set color takes precedence over the additive parser's
/// additive color.
///
/// Shared by [`parse_land_animation`] (single-subject) and
/// [`parse_compound_all_subjects_type_change`] (compound-subject).
fn merge_creature_animation_with_additive_type_modifications(
    predicate: &str,
) -> Option<Vec<ContinuousModification>> {
    let spec = super::oracle_effect::animation::parse_animation_spec(
        predicate,
        &mut ParseContext::default(),
    )?;
    let mut modifications = super::oracle_effect::animation::animation_modifications(&spec);
    if modifications.is_empty() {
        return None;
    }
    if let Some(additive) = parse_additive_type_clause_modifications(&format!("~ are {predicate}"))
    {
        for modification in additive {
            let is_type_grant = matches!(
                modification,
                ContinuousModification::AddType { .. } | ContinuousModification::AddSubtype { .. }
            );
            if is_type_grant && !modifications.contains(&modification) {
                modifications.push(modification);
            }
        }
    }
    Some(modifications)
}

/// CR 613.1d (Layer 4) + CR 613.4b (Layer 7b) + CR 205.1b: Parse a continuous
/// static that animates a population of lands into creatures while they remain
/// lands — "All lands are 1/1 creatures that are still lands" (Living Plane,
/// Nature's Revolt), "Lands you control are X/X creatures that are still lands".
///
/// The land subject is shared with [`parse_land_type_change`]; the
/// "[P/T] creature[s] ..." remainder is delegated to the animation building
/// block (`parse_animation_spec` + `animation_modifications`), so power/
/// toughness (Layer 7b), color, keyword, and creature-subtype grants all
/// compose for free. `animation_modifications` adds the creature type
/// additively (CR 613.1d), and card types stay additive, so the land keeps its
/// land type — the "that are still lands" tail (CR 205.1b) merely confirms that
/// reading and is consumed by `split_type_retention_clause`.
///
/// When the predicate instead uses the explicit CR 205.1b additive marker
/// ("… and <type> lands in addition to their other types"), trailing land-type
/// grants are merged via [`merge_creature_animation_with_additive_type_modifications`].
///
/// Dispatched before `parse_land_type_change`; the `"creature"` guard makes
/// land *type* lines ("Lands you control are Plains") fall through unclaimed.
pub(crate) fn parse_land_animation(tp: &TextPair<'_>, text: &str) -> Option<StaticDefinition> {
    let (subject_tp, rest_tp) = tp
        .split_around(" are ")
        .or_else(|| tp.split_around(" is a "))
        .or_else(|| tp.split_around(" is an "))?;
    let affected = parse_land_type_change_subject(subject_tp.original)?;

    let rest = rest_tp.original.trim().trim_end_matches('.');
    let lower_rest = rest.to_lowercase();
    // Only claim creature-animation remainders so land type-change lines
    // ("Lands you control are Plains") fall through to parse_land_type_change.
    if !nom_primitives::scan_contains(&lower_rest, "creature") {
        return None;
    }

    // CR 205.1b: strip the "that are still land(s)" / "that's still a land"
    // retention tail. The creature type is added additively, so retention is
    // the default behavior; the clause is confirmatory. Split on the lowercased
    // text and reuse the byte offset on the original to preserve subtype casing.
    let animation_text = match super::grammar::split_type_retention_clause(&lower_rest) {
        Some((descriptor_lower, _retained)) => &rest[..descriptor_lower.len()],
        None => rest,
    }
    .trim();

    let modifications = if super::oracle_effect::animation::has_in_addition_to_other_types(rest) {
        // CR 205.1b: predicates that use the explicit additive marker ("… and
        // <type> lands in addition to their other types") carry trailing land-type
        // grants past the animation parser's internal " and " stop — the same merge
        // `parse_compound_all_subjects_type_change` applies for compound subjects.
        merge_creature_animation_with_additive_type_modifications(animation_text)?
    } else {
        let spec = super::oracle_effect::animation::parse_animation_spec(
            animation_text,
            &mut ParseContext::default(),
        )?;
        let modifications = super::oracle_effect::animation::animation_modifications(&spec);
        if modifications.is_empty() {
            return None;
        }
        modifications
    };
    Some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(modifications)
            .description(text.to_string()),
    )
}

/// CR 611.3 + CR 613.1 + CR 613.4b + CR 205.1b: "All `<X>` and all `<Y>` are
/// `<predicate>`" — a compound-subject continuous animation/type-change where a
/// single predicate applies uniformly to every object matching either subject.
///
/// Life and Limb is the canonical member: "All Forests and all Saprolings are
/// 1/1 green Saproling creatures and Forest lands in addition to their other
/// types." Neither single-subject parser is complete for this predicate — the
/// animation path drops the trailing additive "and `<type>` lands", and the
/// creature-subtype path drops the base P/T — so the compound line is dispatched
/// here. The subjects distribute into an `Or` filter (CR 611.3: the same
/// continuous effect applies to every object in the affected set), and the
/// compound predicate is parsed once into a uniform modification set that both
/// subjects share.
///
/// The predicate reuses the two tested predicate parsers: `parse_animation_spec`
/// supplies the base P/T (CR 613.4b, layer 7b), the set color (CR 105.2, layer
/// 5) and the leading type/subtype grants; `parse_additive_type_clause_modifications`
/// supplies the additive type/subtype nouns the animation parser stops short of
/// at the internal " and " (CR 205.1b, layer 4). Only additive `AddType` /
/// `AddSubtype` grants are merged in — the animation spec's set color takes
/// precedence over the additive parser's additive color.
pub(crate) fn parse_compound_all_subjects_type_change(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<StaticDefinition> {
    let (subject_tp, predicate_tp) = tp.split_around(" are ")?;
    // Require a genuine "all <X> and all <Y>" conjunction so single-subject
    // "all <X> are ..." lines fall through to their dedicated dispatchers.
    let affected = parse_compound_all_subjects_filter(subject_tp.original)?;

    let predicate = predicate_tp.original.trim().trim_end_matches('.').trim();
    let predicate_lower = predicate.to_lowercase();
    // Claim only creature-animation predicates; bare land type-change compounds
    // ("All Mountains and all Forests are Plains") are owned by
    // `parse_compound_all_subjects_land_type_change`.
    if !nom_primitives::scan_contains(&predicate_lower, "creature") {
        return None;
    }
    // CR 205.1b: this handler applies strictly ADDITIVE type/subtype semantics
    // (`AddType` / `AddSubtype`), so it must only claim predicates that carry the
    // "in addition to {their|its} other types" marker. A compound predicate
    // without it ("All X and all Y are Zombies") is a type REPLACEMENT (CR 205.1a
    // `SetCardTypes`) that must fall through to a replacement-semantics handler
    // rather than be silently reinterpreted as additive.
    if !nom_primitives::scan_contains(&predicate_lower, "in addition to their other")
        && !nom_primitives::scan_contains(&predicate_lower, "in addition to its other")
    {
        return None;
    }
    let modifications = merge_creature_animation_with_additive_type_modifications(predicate)?;

    Some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(modifications)
            .description(text.to_string()),
    )
}

/// CR 611.3 + CR 613.1 + CR 613.4b + CR 205.1a: "All `<X>` and all `<Y>` are
/// `<predicate>`" — a compound-subject continuous animation where a single
/// replacement predicate applies uniformly to every object matching either
/// subject, without the CR 205.1b additive marker.
///
/// Sibling of [`parse_compound_all_subjects_type_change`]: a bare
/// "are `<P/T>` `<type>` creatures" compound replaces creature subtypes (CR
/// 205.1a) rather than retaining them additively. The subjects distribute into
/// an `Or` filter and the predicate is parsed once via `parse_animation_spec` +
/// `animation_modifications_with_replacement`.
pub(crate) fn parse_compound_all_subjects_type_replacement(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<StaticDefinition> {
    let (subject_tp, predicate_tp) = tp.split_around(" are ")?;
    let affected = parse_compound_all_subjects_filter(subject_tp.original)?;

    let predicate = predicate_tp.original.trim().trim_end_matches('.').trim();
    // CR 205.1b: additive predicates are owned by the additive compound handler.
    if super::oracle_effect::animation::has_in_addition_to_other_types(predicate) {
        return None;
    }

    let spec = super::oracle_effect::animation::parse_animation_spec(
        predicate,
        &mut ParseContext::default(),
    )?;
    let modifications =
        super::oracle_effect::animation::animation_modifications_with_replacement(&spec, false);
    if modifications.is_empty()
        || !modifications.iter().any(|modification| {
            matches!(
                modification,
                ContinuousModification::AddType {
                    core_type: CoreType::Creature
                }
            )
        })
    {
        return None;
    }

    Some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(modifications)
            .description(text.to_string()),
    )
}

/// CR 611.3 + CR 305.7 + CR 205.1b: "All `<X>` and all `<Y>` are `<land-type
/// predicate>`" — a compound-subject land type-change where one predicate applies
/// uniformly to every object matching either land subject conjunct.
///
/// Sibling of the compound animation handlers: `parse_land_type_change` only
/// resolves single-subject land filters. Subjects distribute into an `Or` filter
/// via [`parse_compound_all_subjects_land_filter`] (land-only conjuncts — mixed
/// land/creature compounds like Life and Limb stay on the animation path). The
/// predicate is parsed once through [`parse_land_type_change_modifications`]. The
/// `"creature"` guard keeps animation compounds on the animation dispatch path.
pub(crate) fn parse_compound_all_subjects_land_type_change(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<StaticDefinition> {
    let (subject_tp, predicate_tp) = tp.split_around(" are ")?;
    let affected = parse_compound_all_subjects_land_filter(subject_tp.original)?;

    let predicate = predicate_tp.original.trim().trim_end_matches('.').trim();
    let predicate_lower = predicate.to_lowercase();
    if nom_primitives::scan_contains(&predicate_lower, "creature") {
        return None;
    }

    let modifications = parse_land_type_change_modifications(predicate)?;
    Some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(modifications)
            .description(text.to_string()),
    )
}

/// Parse "all `<X>` and all `<Y>`[ and all `<Z>`…]" into an `Or` of per-subject
/// filters. Peels conjuncts on the " and all " seam (so every conjunct after the
/// first is an `all `-quantified subject) and parses each through the shared
/// subject parser. Requires 2+ conjuncts, all of which resolve to a filter;
/// returns None otherwise so single-subject lines fall through to their
/// dedicated dispatchers. The mandatory second `all ` quantifier is what
/// distinguishes this compound animation subject from an incidental " and "
/// inside a lone subject phrase.
fn parse_compound_all_subjects_filter(subject: &str) -> Option<TargetFilter> {
    let conjuncts = super::static_helpers::peel_compound_all_quantified_conjuncts(subject)?;
    let filters: Vec<TargetFilter> = conjuncts
        .iter()
        .map(|conjunct| parse_compound_subject_conjunct(conjunct.trim()))
        .collect::<Option<_>>()?;
    Some(TargetFilter::Or { filters })
}

/// Resolve one conjunct of a compound animation subject to a filter. A basic
/// land-type conjunct ("Forests") must resolve to a `Land` + subtype filter
/// (CR 305.6), so try the land-type-change subject parser first; the shared
/// subject parser (which defaults a bare subtype to a *creature* subtype)
/// handles the creature-subtype conjuncts ("Saprolings").
fn parse_compound_subject_conjunct(conjunct: &str) -> Option<TargetFilter> {
    parse_land_type_change_subject(conjunct)
        .or_else(|| super::shared::parse_continuous_subject_filter(conjunct))
}

/// Parse "all `<X>` and all `<Y>`[ and all `<Z>`…]" into an `Or` of land-only
/// per-subject filters. Each conjunct must resolve through
/// [`parse_land_type_change_subject`] — mixed land/creature compounds (Life and
/// Limb's "Forests and Saprolings") return `None` so animation handlers keep
/// ownership.
fn parse_compound_all_subjects_land_filter(subject: &str) -> Option<TargetFilter> {
    let conjuncts = super::static_helpers::peel_compound_all_quantified_conjuncts(subject)?;
    let filters: Vec<TargetFilter> = conjuncts
        .iter()
        .map(|conjunct| parse_land_type_change_subject(conjunct.trim()))
        .collect::<Option<_>>()?;
    Some(TargetFilter::Or { filters })
}

/// Parse the subject of a land type-change line into a TargetFilter.
pub(crate) fn parse_land_type_change_subject(subject: &str) -> Option<TargetFilter> {
    match subject.to_lowercase().as_str() {
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        "nonbasic lands" => Some(TargetFilter::Typed(TypedFilter::land().properties(vec![
            // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            FilterProp::NotSupertype {
                value: Supertype::Basic,
            },
        ]))),
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        "lands you control" => Some(TargetFilter::Typed(
            // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            TypedFilter::land().controller(ControllerRef::You),
        )),
        "each land" | "all lands" => Some(TargetFilter::Typed(TypedFilter::land())),
        // CR 305.7: Aura enchantments that change the enchanted land's type.
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        "enchanted land" => Some(TargetFilter::Typed(
            // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            TypedFilter::land().properties(vec![FilterProp::EnchantedBy]),
        )),
        // CR 305.6 + CR 305.7: "[all] <basic land type>[s] [you control]" — every
        // permanent with the named basic land subtype (Conversion, Glaciers "All
        // Mountains are Plains"), optionally narrowed to the static's controller
        // (Ambush Commander "Forests you control ..."). Composes over all five basic
        // land types and both controller scopes, not one card.
        other => parse_basic_land_type_subject_scoped(other),
    }
}

/// CR 305.6 + CR 109.5: resolve a plural basic-land-type subject that carries an
/// optional leading `all ` and an optional trailing ` you control` controller
/// scope — `Forests`, `all Mountains`, `Forests you control` (Ambush Commander).
/// The type word is resolved by [`parse_basic_land_type_plural`]; the trailing
/// controller clause is peeled with nom (`take_until` + `tag`) so the
/// controller-scoped form reuses the same type grammar as its unscoped sibling
/// instead of needing its own dispatch arm (CR 109.5: `you` on a static ability's
/// object refers to that object's controller).
fn parse_basic_land_type_subject_scoped(subject: &str) -> Option<TargetFilter> {
    let type_word = opt(tag::<_, _, OracleError<'_>>("all "))
        .parse(subject)
        .map(|(rest, _)| rest.trim())
        .unwrap_or(subject);
    let (type_word, controller) = match all_consuming(terminated(
        take_until::<_, _, OracleError<'_>>(" you control"),
        tag(" you control"),
    ))
    .parse(type_word)
    {
        Ok((_, head)) => (head, Some(ControllerRef::You)),
        Err(_) => (type_word, None),
    };
    let basic = parse_basic_land_type_plural(type_word)?;
    let mut filter = TypedFilter::land().subtype(basic.as_subtype_str().to_string());
    if let Some(controller) = controller {
        filter = filter.controller(controller);
    }
    Some(TargetFilter::Typed(filter))
}

/// CR 702.73a + CR 205.3 + CR 604.3 + CR 613.1d: Parse "[subject] {is|are}
/// every creature type" — the Changeling-class type grant in static form.
///
/// Self-reference (`~`) becomes a CDA so the grant functions in all zones
/// per CR 604.3 (Mistform Ultimus, Dr. Julius Jumblemorph reminder
/// "even if this card isn't on the battlefield"). Filter subjects produce
/// a normal battlefield-scoped continuous static for the same predicate.
///
/// Most filter-subject cards (e.g. Maskwood Nexus's "Creatures you control
/// are every creature type") are caught upstream by `parse_continuous_gets_has`
/// once `parse_continuous_modifications` recognizes the predicate; this
/// dispatcher catches the residual subject shapes that those code paths
/// don't strip, plus every self-reference grant.
///
/// Returns None when the line's subject doesn't map to a recognized filter.
pub(crate) fn parse_all_creature_types_grant(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<StaticDefinition> {
    let (subject_tp, rest_tp) = tp
        .split_around(" is every creature type")
        .or_else(|| tp.split_around(" are every creature type"))?;
    // The predicate must terminate the line — only punctuation and trailing
    // whitespace may remain. Anything else (e.g., a hypothetical "in addition
    // to ..." extension) is outside the AddAllCreatureTypes contract and
    // should fall through to other parsers rather than be silently dropped.
    let tail = rest_tp.lower.trim().trim_end_matches('.').trim();
    if !tail.is_empty() {
        return None;
    }
    let subject = subject_tp.lower.trim();

    if subject == "~" {
        // CR 604.3 + CR 604.3a: Self-reference type-defining grant. Meets the
        // CDA criteria (defines subtypes, printed on the card it affects,
        // does not affect other objects) and so functions in all zones —
        // mirroring `synthesize_changeling_cda` for the Changeling keyword.
        return Some(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::AddAllCreatureTypes])
                .cda()
                .description(text.to_string()),
        );
    }

    let affected = parse_creature_type_change_subject(subject)?;
    Some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(vec![ContinuousModification::AddAllCreatureTypes])
            .description(text.to_string()),
    )
}

/// CR 205.3 + CR 613.1d: Map the subject of an "{is|are} every creature
/// type" static into a TargetFilter restricting which battlefield objects
/// receive the grant. Sibling of `parse_land_type_change_subject` for the
/// CR 702.73a creature-type class. Counter-conditioned subjects ("each nonland
/// creature with an everything counter on it" — Omo, Queen of Vesuva) are
/// handled by `parse_counter_conditioned_nonland_creature_subject`.
pub(crate) fn parse_creature_type_change_subject(subject: &str) -> Option<TargetFilter> {
    // Combinator dispatch — each subject phrase maps to its TypedFilter
    // shape. `all_consuming` requires the whole subject to be matched, so a
    // partial prefix like "creatures" inside "creatures with X" does not
    // false-positive. "creatures" must come last among the bare-creature
    // arms so the longer "creatures you control" prefix wins first.
    all_consuming(alt((
        // CR 205.3 + CR 122.1: "each nonland creature with an everything counter
        // on it" (Omo, Queen of Vesuva). The nonland constraint reuses the
        // existing `TypeFilter::Non` building block; the counter clause is
        // delegated to `parse_counter_suffix`. No new engine surface.
        parse_counter_conditioned_nonland_creature_subject,
        map(
            tag::<_, _, OracleError<'_>>("creatures you control"),
            |_| TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
        ),
        map(tag("enchanted creature"), |_| {
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]))
        }),
        map(tag("equipped creature"), |_| {
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EquippedBy]))
        }),
        map(
            alt((tag("each creature"), tag("all creatures"), tag("creatures"))),
            |_| TargetFilter::Typed(TypedFilter::creature()),
        ),
    )))
    .parse(subject)
    .ok()
    .map(|(_, filter)| filter)
}
