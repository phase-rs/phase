use super::*;
use crate::game::scenario::{GameScenario, P0, P1};
use crate::parser::oracle::parse_oracle_text;
use crate::parser::oracle_ir::context::ParseContext;
use crate::parser::oracle_ir::diagnostic::OracleDiagnostic;
use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, AggregateFunction, AttackScope,
    AttackSubject, BounceSelection, CardSelectionMode, CastingPermission, ChosenAttribute,
    Comparator, ContinuousModification, ControllerRef, CountScope, DamageChannel,
    DamageModification, DamageSource, DelayedTriggerCondition, DiscardSelfScope, Duration, Effect,
    EffectScope, FilterProp, ManaContribution, ManaProduction, ManaSpendPermission, ObjectScope,
    PlayerFilter, PlayerScope, PtStat, PtValue, PtValueScope, QuantityExpr, QuantityRef,
    SharedQuality, TapStateChange, TargetFilter, TriggerCondition, TypeFilter, TypedFilter,
    ZoneRef,
};
use crate::types::counter::{CounterMatch, CounterType};
use crate::types::game_state::WaitingFor;
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use crate::types::replacements::ReplacementEvent;
use crate::types::statics::{CastFrequency, StaticMode};

// --- Fix B: damage-recipient qualifier (player axis preserved + object axis added) ---

#[test]
fn parse_damage_to_qualifier_preserves_player_recipients() {
    // CR 120.3 + CR 102.2: player-recipient phrasings must keep their
    // existing player-scope filters after the object arm was added.
    assert_eq!(
        parse_damage_to_qualifier("to a player"),
        Some(TargetFilter::Player)
    );
    assert_eq!(
        parse_damage_to_qualifier("to you"),
        Some(TargetFilter::Controller)
    );
    // "to an opponent" → controller-only Typed (the player-scope convention).
    match parse_damage_to_qualifier("to an opponent") {
        Some(TargetFilter::Typed(TypedFilter {
            type_filters,
            controller: Some(ControllerRef::Opponent),
            properties,
        })) => {
            assert!(type_filters.is_empty(), "opponent filter is player-scope");
            assert!(properties.is_empty());
        }
        other => panic!("expected opponent player-scope filter, got {other:?}"),
    }
    // "to a player or planeswalker" stays an Or naming a player slot.
    match parse_damage_to_qualifier("to a player or planeswalker") {
        Some(TargetFilter::Or { filters }) => {
            assert!(filters.iter().any(|f| matches!(f, TargetFilter::Player)));
        }
        other => panic!("expected Or {{ Player, Planeswalker }}, got {other:?}"),
    }
}

// --- CR 120.1 + CR 120.1a: "or battle" damage-recipient qualifier ---

#[test]
fn parse_damage_to_qualifier_player_or_battle() {
    // CR 120.1a: "to a player or battle" must produce an Or filter
    // containing both Player and Battle (Archpriest of Shadows class).
    match parse_damage_to_qualifier("to a player or battle") {
        Some(TargetFilter::Or { filters }) => {
            assert!(filters.iter().any(|f| matches!(f, TargetFilter::Player)));
            assert!(filters.iter().any(|f| matches!(
                f,
                TargetFilter::Typed(TypedFilter { type_filters, .. })
                if type_filters.contains(&TypeFilter::Battle)
            )));
        }
        other => panic!("expected Or {{ Player, Battle }}, got {other:?}"),
    }
}

#[test]
fn parse_damage_to_qualifier_opponent_or_battle() {
    // CR 120.1a: "to an opponent or battle" — Bloodfeather Phoenix class.
    match parse_damage_to_qualifier("to an opponent or battle") {
        Some(TargetFilter::Or { filters }) => {
            assert_eq!(filters.len(), 2);
            assert!(filters.iter().any(|f| matches!(
                f,
                TargetFilter::Typed(TypedFilter {
                    controller: Some(ControllerRef::Opponent),
                    ..
                })
            )));
            assert!(filters.iter().any(|f| matches!(
                f,
                TargetFilter::Typed(TypedFilter { type_filters, .. })
                if type_filters.contains(&TypeFilter::Battle)
            )));
        }
        other => panic!("expected Or {{ Opponent, Battle }}, got {other:?}"),
    }
}

#[test]
fn parse_damage_to_qualifier_player_planeswalker_or_battle() {
    // CR 120.1a: Three-way disjunction — Farseeing Flockmate class.
    match parse_damage_to_qualifier("to a player, planeswalker, or battle") {
        Some(TargetFilter::Or { filters }) => {
            assert_eq!(filters.len(), 3);
            assert!(filters.iter().any(|f| matches!(f, TargetFilter::Player)));
            assert!(filters.iter().any(|f| matches!(
                f,
                TargetFilter::Typed(TypedFilter { type_filters, .. })
                if type_filters.contains(&TypeFilter::Planeswalker)
            )));
            assert!(filters.iter().any(|f| matches!(
                f,
                TargetFilter::Typed(TypedFilter { type_filters, .. })
                if type_filters.contains(&TypeFilter::Battle)
            )));
        }
        other => panic!("expected Or {{ Player, Planeswalker, Battle }}, got {other:?}"),
    }
}

#[test]
fn parse_kookus_named_creature_condition_preserves_literal_name_and_body() {
    let parsed = parse_oracle_text(
        "Trample\nAt the beginning of your upkeep, if you don't control a creature named Keeper of Kookus, this creature deals 3 damage to you and attacks this turn if able.\n{R}: This creature gets +1/+0 until end of turn.",
        "Kookus",
        &[],
        &[],
        &[],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|trigger| {
            trigger
                .description
                .as_deref()
                .is_some_and(|description| description.contains("Keeper of Kookus"))
        })
        .expect("Kookus upkeep trigger should parse");

    let condition = trigger.condition.clone().expect("trigger condition");
    match condition {
        TriggerCondition::QuantityComparison {
            lhs:
                QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount { filter },
                },
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: 0 },
        } => match filter {
            TargetFilter::Typed(TypedFilter { properties, .. }) => {
                assert!(properties.iter().any(|prop| matches!(
                    prop,
                    FilterProp::Named { name } if name == "keeper of kookus"
                )));
            }
            other => panic!("expected named creature filter, got {other:?}"),
        },
        other => panic!("expected no-Keeper condition, got {other:?}"),
    }

    let execute = trigger.execute.as_ref().expect("trigger body");
    match execute.effect.as_ref() {
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Controller,
            ..
        } => {}
        other => panic!("expected damage to controller, got {other:?}"),
    }

    let must_attack = execute
        .sub_ability
        .as_ref()
        .expect("must-attack continuation");
    match must_attack.effect.as_ref() {
        Effect::GenericEffect {
            static_abilities,
            duration: Some(Duration::UntilEndOfTurn),
            ..
        } => assert!(static_abilities
            .iter()
            .any(|static_ability| static_ability.mode == StaticMode::MustAttack)),
        other => panic!("expected must-attack continuation, got {other:?}"),
    }
}

#[test]
fn glory_of_battle_trigger_gates_on_creature_recipient() {
    // CR 120.3: "Whenever ~ deals damage to a creature, put a +1/+1 counter
    // on ~" (Strax, Sontaran Nurse — Glory of Battle) must set a typed
    // creature `valid_target` so the trigger fires only on creature damage.
    let parsed = crate::parser::oracle::parse_oracle_text(
        "Whenever Strax deals damage to a creature, put a +1/+1 counter on Strax.",
        "Strax, Sontaran Nurse",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| matches!(t.mode, TriggerMode::DamageDone))
        .expect("DamageDone trigger");
    match &trigger.valid_target {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
        }
        other => panic!("expected creature-scoped valid_target, got {other:?}"),
    }
}

fn blocking_source_beyond_first_expr() -> QuantityExpr {
    let count_minus_one = QuantityExpr::Offset {
        inner: Box::new(QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: None,
                    properties: vec![FilterProp::BlockingSource],
                }),
            },
        }),
        offset: -1,
    };
    QuantityExpr::ClampMin {
        inner: Box::new(count_minus_one),
        minimum: 0,
    }
}

fn assert_owned_by_you(filter: &TargetFilter) {
    match filter {
        TargetFilter::Typed(typed) => assert!(
            typed.properties.contains(&FilterProp::Owned {
                controller: ControllerRef::You,
            }),
            "expected Owned(You) property in {typed:?}"
        ),
        TargetFilter::Or { filters } => {
            assert!(!filters.is_empty(), "expected non-empty Or filter");
            for filter in filters {
                assert_owned_by_you(filter);
            }
        }
        other => panic!("expected Typed or Or filter, got {other:?}"),
    }
}

fn assert_owned_by_opponent(filter: &TargetFilter) {
    match filter {
        TargetFilter::Typed(typed) => assert!(
            typed.properties.contains(&FilterProp::Owned {
                controller: ControllerRef::Opponent,
            }),
            "expected Owned(Opponent) property in {typed:?}"
        ),
        TargetFilter::Or { filters } => {
            assert!(!filters.is_empty(), "expected non-empty Or filter");
            for filter in filters {
                assert_owned_by_opponent(filter);
            }
        }
        other => panic!("expected Typed or Or filter, got {other:?}"),
    }
}

#[test]
fn parse_post_spell_modifier_creature_type_does_not_share_reference() {
    use crate::types::ability::{FilterProp, SharedQuality, SharedQualityRelation, TargetFilter};

    let filter = parse_post_spell_modifier(
        "that doesn't share a creature type with a creature you control or a creature card in your graveyard",
    )
    .expect("expected SharesQuality post-spell modifier");
    let TargetFilter::Typed(tf) = filter else {
        panic!("expected Typed filter, got {filter:?}");
    };
    let shares_quality = tf
        .properties
        .iter()
        .find_map(|p| match p {
            FilterProp::SharesQuality {
                quality,
                relation,
                reference,
            } => Some((quality, relation, reference.as_deref())),
            _ => None,
        })
        .expect("expected SharesQuality property");
    assert_eq!(*shares_quality.0, SharedQuality::CreatureType);
    assert_eq!(*shares_quality.1, SharedQualityRelation::DoesNotShare);
    let reference = shares_quality.2.expect("expected disjunctive reference");
    let TargetFilter::Or { filters } = reference else {
        panic!("expected Or reference filter, got {reference:?}");
    };
    assert_eq!(filters.len(), 2);
}

#[test]
fn parse_post_spell_modifier_cast_origin_from_nonhand() {
    // CR 601.2a: "from anywhere other than your hand" → InAnyZone over the
    // cast-capable zones except the hand.
    let expected = crate::parser::oracle_target::cast_capable_zones_except(Zone::Hand);
    let filter = parse_post_spell_modifier("from anywhere other than your hand")
        .expect("expected a cast-origin filter");
    let TargetFilter::Typed(tf) = filter else {
        panic!("expected Typed filter");
    };
    assert!(
        tf.properties.contains(&FilterProp::InAnyZone {
            zones: expected.clone()
        }),
        "expected InAnyZone({expected:?}), got {:?}",
        tf.properties
    );
}

#[test]
fn parse_post_spell_modifier_cast_origin_single_zone() {
    // CR 601.2a: "from exile" / "from your graveyard" → InZone(single).
    for (text, zone) in [
        ("from exile", Zone::Exile),
        ("from your graveyard", Zone::Graveyard),
    ] {
        let filter = parse_post_spell_modifier(text)
            .unwrap_or_else(|| panic!("expected a filter for {text:?}"));
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected Typed filter for {text:?}");
        };
        assert!(
            tf.properties.contains(&FilterProp::InZone { zone }),
            "expected InZone({zone:?}) for {text:?}, got {:?}",
            tf.properties
        );
    }
}

#[test]
fn parse_post_spell_modifier_rejects_unsupported_origin() {
    // CR 601.2a: only the printed cast-origin forms are recognized; an
    // unmodeled exclusion ("from anywhere other than your graveyard") must
    // return None so the first-spell parser reports UnsupportedQualifier.
    assert_eq!(
        parse_post_spell_modifier("from anywhere other than your graveyard"),
        None
    );
}

#[test]
fn parse_origin_constraint_tail_excludes_graveyard_and_exile_with_one_of() {
    // CR 603.6c: "from anywhere other than a graveyard or exile" is the
    // list-form negation introduced for "Name Sticker" Goblin. The parser
    // models this with the existing positive `OneOf` set over all concrete
    // zones except Graveyard and Exile, not a new negated-set variant.
    let (_rest, constraint) = parse_origin_constraint_tail(
        "from anywhere other than a graveyard or exile",
        parse_cast_origin_zone,
    )
    .expect("parse list-form negation");
    assert_eq!(
        constraint,
        OriginConstraint::OneOf(vec![
            Zone::Library,
            Zone::Hand,
            Zone::Battlefield,
            Zone::Stack,
            Zone::Command,
        ]),
    );

    // Single-zone negation must still collapse to `NotEquals` so existing
    // card-data snapshots (Ghostly Pilferer, Syr Konrad clause 2) remain
    // byte-identical.
    let (_rest, single) = parse_origin_constraint_tail(
        "from anywhere other than their hand",
        parse_cast_origin_zone,
    )
    .expect("parse single-zone negation");
    assert_eq!(single, OriginConstraint::NotEquals(Zone::Hand));
}

#[test]
fn winter_soldier_reborn_avenger_attack_reanimation_trigger() {
    use crate::types::ability::{Effect, QuantityExpr, TargetFilter, TypeFilter};
    use crate::types::counter::CounterType;
    let def = parse_trigger_line(
            "Whenever Winter Soldier attacks, return target creature card with mana value less than or equal to Winter Soldier's power from your graveyard to the battlefield. If a Hero enters this way, it enters with an additional +1/+1 counter on it.",
            "Winter Soldier, Reborn Avenger",
        );
    assert_eq!(def.mode, TriggerMode::Attacks);
    let execute = def.execute.expect("execute");
    assert!(
        matches!(execute.effect.as_ref(), Effect::ChangeZone { .. }),
        "expected ChangeZone reanimation, got {:?}",
        execute.effect
    );
    assert!(execute.forward_result);
    let Effect::ChangeZone {
        conditional_enter_with_counters,
        ..
    } = execute.effect.as_ref()
    else {
        panic!("expected ChangeZone head");
    };
    assert_eq!(
        conditional_enter_with_counters.len(),
        1,
        "Hero counter rider must fold into conditional_enter_with_counters"
    );
    let (filter, counter_type, count) = &conditional_enter_with_counters[0];
    assert_eq!(*counter_type, CounterType::Plus1Plus1);
    assert!(
        matches!(count, QuantityExpr::Fixed { value: 1 }),
        "expected one additional +1/+1 counter, got {count:?}"
    );
    let TargetFilter::Typed(typed) = filter else {
        panic!("expected Hero filter, got {filter:?}");
    };
    assert!(typed
        .type_filters
        .contains(&TypeFilter::Subtype("Hero".into())));
    assert!(
        execute.sub_ability.is_none(),
        "counter rider must not remain as a PutCounter sub-ability"
    );
}

#[test]
fn parses_phases_in_trigger_as_phase_in_mode() {
    let def = parse_trigger_line(
        "Whenever Warping Wurm phases in, put a +1/+1 counter on it.",
        "Warping Wurm",
    );

    assert_eq!(def.mode, TriggerMode::PhaseIn);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

#[test]
fn parses_phases_out_trigger_as_phase_out_mode() {
    // CR 702.26b: "Whenever ~ phases out, discard a card." (Teferi's Imp)
    let def = parse_trigger_line(
        "Whenever this creature phases out, discard a card.",
        "Teferi's Imp",
    );

    assert_eq!(def.mode, TriggerMode::PhaseOut);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

#[test]
fn parses_specializes_trigger_as_specializes_mode() {
    // Digital-only Alchemy: "When ~ specializes, draw a card."
    let def = parse_trigger_line(
        "When Jaheira, Insightful Harper specializes, draw a card.",
        "Jaheira, Insightful Harper",
    );

    assert_eq!(def.mode, TriggerMode::Specializes);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

#[test]
fn static_condition_to_trigger_condition_source_in_battlefield() {
    // SUB-FIX B regression: the existing SourceInZone mapper must pass
    // Zone::Battlefield through unchanged so the new battlefield condition
    // arm hoists to a trigger-level intervening-if (CR 603.4).
    use crate::types::zones::Zone;
    let mapped = static_condition_to_trigger_condition(&StaticCondition::SourceInZone {
        zone: Zone::Battlefield,
    });
    assert_eq!(
        mapped,
        Some(TriggerCondition::SourceInZone {
            zone: Zone::Battlefield,
        }),
    );
}

#[test]
fn intervening_if_source_attacked_this_turn_populates_condition() {
    // CR 508.1 + CR 603.4: a source-scoped "if ~ attacked this turn"
    // intervening-if must gate the trigger on the ability's own source creature
    // having declared as an attacker this turn — not resolve unconditionally.
    // Composed from the existing, already-evaluated `FilterProp::AttackedThisTurn`
    // via `SourceMatchesFilter`, so no new `TriggerCondition` variant is added.
    // Distinct from the player-scoped `YouAttackedThisTurn`.
    let expected = Some(TriggerCondition::SourceMatchesFilter {
        filter: TargetFilter::Typed(
            TypedFilter::creature()
                .properties(vec![FilterProp::AttackedThisTurn { defender: None }]),
        ),
    });

    // Riders of the Mark — phase trigger. Without the gate it would bounce
    // itself to hand every end step even when it never attacked.
    let riders = parse_trigger_line(
        "At the beginning of your end step, if Riders of the Mark attacked this turn, \
         return it to its owner's hand.",
        "Riders of the Mark",
    );
    assert_eq!(riders.condition, expected);
    // The intervening-if clause is stripped, so the effect still parses.
    assert!(riders.execute.is_some());

    // Taigam, Ojutai Master — the same source-scoped gate on a spell-cast
    // trigger (would otherwise grant rebound unconditionally).
    let taigam = parse_trigger_line(
        "Whenever you cast an instant or sorcery spell from your hand, if Taigam, \
         Ojutai Master attacked this turn, that spell gains rebound.",
        "Taigam, Ojutai Master",
    );
    assert_eq!(taigam.condition, expected);
    assert!(taigam.execute.is_some());
}

#[test]
fn intervening_if_source_attacked_or_blocked_this_turn_populates_condition() {
    // CR 508.1 + CR 509.1 + CR 603.4: the "attacked or blocked" sibling of the
    // attacked-only intervening-if. Gates on the source creature having attacked
    // OR blocked this turn, composed from the existing, already-evaluated
    // `FilterProp::AttackedOrBlockedThisTurn` via `SourceMatchesFilter` — no new
    // `TriggerCondition` variant.
    let expected = Some(TriggerCondition::SourceMatchesFilter {
        filter: TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::AttackedOrBlockedThisTurn]),
        ),
    });

    // Inferno Hellion — without the gate it would shuffle itself into its
    // owner's library every end step, even on a turn it neither attacked nor
    // blocked.
    let hellion = parse_trigger_line(
        "At the beginning of each end step, if Inferno Hellion attacked or blocked this turn, \
         its owner shuffles it into their library.",
        "Inferno Hellion",
    );
    assert_eq!(hellion.condition, expected);
    // The intervening-if clause is stripped, so the effect still parses.
    assert!(hellion.execute.is_some());
}

#[test]
fn intervening_if_source_has_counters_on_it_populates_condition() {
    // CR 603.4 + CR 122: source-scoped "if ~ has counters on it" gates the
    // trigger on the source permanent currently having at least one counter of
    // any type, mapping to the existing, already-evaluated
    // `TriggerCondition::HasCounters` — no new variant. Distinct from the
    // past-tense event-subject "if it had counters on it" (`HadCounters`).
    let expected = Some(TriggerCondition::HasCounters {
        counters: CounterMatch::Any,
        minimum: 1,
        maximum: None,
    });

    // The Ozolith — without the gate it would offer the counter-move every
    // combat even when it holds no counters.
    let ozolith = parse_trigger_line(
        "At the beginning of combat on your turn, if The Ozolith has counters on it, \
         you may move all counters from The Ozolith onto target creature.",
        "The Ozolith",
    );
    assert_eq!(ozolith.condition, expected);
    // The intervening-if clause is stripped, so the effect still parses.
    assert!(ozolith.execute.is_some());

    // Denry Klin, Editor in Chief — the same source-scoped gate on an ETB
    // trigger. The card's Oracle text uses the comma-based short self-name
    // "Denry Klin" (not the full "Denry Klin, Editor in Chief"); passing the
    // full card name exercises the real short-name → `~` normalization path.
    let denry = parse_trigger_line(
        "Whenever a nontoken creature you control enters, if Denry Klin has counters on it, \
         proliferate.",
        "Denry Klin, Editor in Chief",
    );
    assert_eq!(denry.condition, expected);
    assert!(denry.execute.is_some());
}

#[test]
fn trigger_etb_self() {
    let def = parse_trigger_line(
        "When this creature enters, it deals 1 damage to each opponent.",
        "Goblin Chainwhirler",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert!(def.execute.is_some());
}

// CR 603.6a + CR 603.6c: SelfRef-ETB with a list-form negated origin —
// "Name Sticker" Goblin. The "from anywhere other than a graveyard or
// exile" tail must route through `zone_change_clauses` so the disjunctive
// matcher can enforce the positive non-excluded `OneOf` origin set; the scalar
// `origin`/`valid_card`/`destination` fields only model a single positive
// zone and would silently drop the negation otherwise.
#[test]
fn trigger_etb_self_from_anywhere_other_than_graveyard_or_exile() {
    let def = parse_trigger_line(
        "When this creature enters from anywhere other than a graveyard or exile, \
             create a Treasure token.",
        "\"Name Sticker\" Goblin",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    // Scalar discriminator fields cleared — the disjunctive clause path
    // supersedes them (see `match_changes_zone`).
    assert_eq!(def.origin, None);
    assert_eq!(def.destination, None);
    assert_eq!(def.valid_card, None);
    assert_eq!(def.zone_change_clauses.len(), 1);
    let clause = &def.zone_change_clauses[0];
    assert_eq!(
        clause.origin,
        OriginConstraint::OneOf(vec![
            Zone::Library,
            Zone::Hand,
            Zone::Battlefield,
            Zone::Stack,
            Zone::Command,
        ]),
    );
    assert_eq!(clause.destination, Some(Zone::Battlefield));
    assert_eq!(clause.valid_card, Some(TargetFilter::SelfRef));
    assert!(def.execute.is_some());
}

/// CR 603.10a + CR 603.4 (issue #4521): Kishla Skimmer — "Whenever a card
/// leaves your graveyard during your turn, draw a card. This ability
/// triggers only once each turn." The singular leaves-a-graveyard form must
/// route through `ChangesZone` with a graveyard-origin clause AND carry the
/// trailing "during your turn" as a `DuringPlayersTurn { Controller }`
/// intervening-if. Before the fix the un-peeled "during your turn" suffix
/// made `parse_zone_change_clause` reject the clause and the trigger
/// collapsed to `Unknown` (never firing). The plural batched-leave path and
/// the `LeavesBattlefield` branch already peel this suffix; the singular
/// non-battlefield-zone branch did not.
#[test]
fn trigger_card_leaves_your_graveyard_during_your_turn_once_each_turn() {
    let def = parse_trigger_line(
        "Whenever a card leaves your graveyard during your turn, draw a card. \
             This ability triggers only once each turn.",
        "Kishla Skimmer",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.zone_change_clauses.len(), 1);
    let clause = &def.zone_change_clauses[0];
    // CR 603.10a: origin is the graveyard; destination is unconstrained.
    assert_eq!(clause.origin, OriginConstraint::Equals(Zone::Graveyard));
    assert_eq!(clause.destination, None);
    // CR 603.4: the "during your turn" suffix becomes an intervening-if.
    assert_eq!(
        def.condition,
        Some(TriggerCondition::DuringPlayersTurn {
            player: PlayerFilter::Controller,
        }),
    );
    // CR 603.2h: "triggers only once each turn" → OncePerTurn.
    assert_eq!(
        def.constraint,
        Some(crate::types::ability::TriggerConstraint::OncePerTurn),
    );
    assert!(def.execute.is_some());
}

/// CR 608.2c + CR 712.2 (issue #4543): Tamiyo, Inquisitive Student —
/// "When you draw your third card in a turn, exile Tamiyo, then return her
/// to the battlefield transformed under her owner's control." The exile
/// step targets the source (`~` → SelfRef); the "return her ... transformed"
/// sub-clause's bare "her" must ALSO bind to the source (SelfRef), not to
/// `TriggeringSource`. The trigger subject is the player ("you draw …"), so
/// the source has no entry in the CardDrawn event — before the fix the
/// return resolved to an empty target set and Tamiyo was stranded in exile.
/// The SelfRef-anaphor rewrite was gated on a typed trigger subject; it now
/// fires for player/event subjects too because the antecedent is the named
/// prior clause.
#[test]
fn tamiyo_flip_return_transformed_binds_to_self_ref() {
    use crate::types::ability::Effect;

    let def = parse_trigger_line(
            "When you draw your third card in a turn, exile Tamiyo, then return her to the battlefield transformed under her owner's control.",
            "Tamiyo, Inquisitive Student",
        );
    let exec = def.execute.as_deref().expect("Tamiyo trigger execute body");
    // Exile step: source named via `~`.
    let Effect::ChangeZone {
        destination,
        target,
        ..
    } = &*exec.effect
    else {
        panic!("expected exile ChangeZone head, got {:?}", exec.effect);
    };
    assert_eq!(*destination, Zone::Exile);
    assert_eq!(*target, TargetFilter::SelfRef);
    // Return step: "her" must bind to the source and enter transformed.
    let sub = exec.sub_ability.as_deref().expect("return sub_ability");
    let Effect::ChangeZone {
        destination,
        target,
        enter_transformed,
        ..
    } = &*sub.effect
    else {
        panic!("expected return ChangeZone sub, got {:?}", sub.effect);
    };
    assert_eq!(*destination, Zone::Battlefield);
    assert!(
        *enter_transformed,
        "return must enter transformed (CR 712.2)"
    );
    assert_eq!(
        *target,
        TargetFilter::SelfRef,
        "the 'return her transformed' anaphor must bind to the source (SelfRef), \
             not TriggeringSource — otherwise the CardDrawn event has no source and \
             Tamiyo is stranded in exile"
    );
}

/// SHAPE TEST — issue #3299: `parse_trigger_lines` must not compound-split
/// Syr Konrad's disjunctive zone-change condition into separate triggers.
#[test]
fn parse_syr_konrad_trigger_lines_stays_single_disjunctive_trigger() {
    const ORACLE: &str = "Whenever another creature dies, or a creature card \
            is put into a graveyard from anywhere other than the battlefield, or a \
            creature card leaves your graveyard, Syr Konrad, the Grim deals 1 damage \
            to each opponent.";
    let defs = parse_trigger_lines(ORACLE, "Syr Konrad, the Grim");
    assert_eq!(
        defs.len(),
        1,
        "expected one disjunctive trigger, got {} triggers: {:?}",
        defs.len(),
        defs.iter()
            .map(|d| d.description.as_deref().unwrap_or(""))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        defs[0].zone_change_clauses.len(),
        3,
        "expected 3 zone_change_clauses, got {:?}",
        defs[0].zone_change_clauses
    );
}

// SHAPE TEST — issue #411: Syr Konrad's three-way disjunctive zone-change
// trigger. Asserts the parsed `TriggerDefinition` SHAPE; it does not drive
// the runtime apply pipeline (see `triggers.rs` for the runtime test).
#[test]
fn parse_syr_konrad_disjunctive_zone_change() {
    let def = parse_trigger_line(
        "Whenever another creature dies, or a creature card is put into a graveyard \
             from anywhere other than the battlefield, or a creature card leaves your \
             graveyard, Syr Konrad, the Grim deals 1 damage to each opponent.",
        "Syr Konrad, the Grim",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(
        def.zone_change_clauses.len(),
        3,
        "expected 3 disjunctive clauses, got {:?}",
        def.zone_change_clauses
    );

    // Clause 1: another creature dies (battlefield -> graveyard).
    let c1 = &def.zone_change_clauses[0];
    assert_eq!(c1.origin, OriginConstraint::Equals(Zone::Battlefield));
    assert_eq!(c1.destination, Some(Zone::Graveyard));
    let TargetFilter::Typed(tf1) = c1.valid_card.as_ref().expect("clause 1 valid_card") else {
        panic!("clause 1 valid_card not Typed: {:?}", c1.valid_card);
    };
    assert!(tf1.type_filters.contains(&TypeFilter::Creature));
    assert!(tf1.properties.contains(&FilterProp::Another));

    // Clause 2: creature card put into graveyard from anywhere but battlefield.
    let c2 = &def.zone_change_clauses[1];
    assert_eq!(c2.origin, OriginConstraint::NotEquals(Zone::Battlefield));
    assert_eq!(c2.destination, Some(Zone::Graveyard));
    assert!(c2.valid_card.is_some());

    // Clause 3: creature card leaves your graveyard (any destination).
    let c3 = &def.zone_change_clauses[2];
    assert_eq!(c3.origin, OriginConstraint::Equals(Zone::Graveyard));
    assert_eq!(c3.destination, None);
    let TargetFilter::Typed(tf3) = c3.valid_card.as_ref().expect("clause 3 valid_card") else {
        panic!("clause 3 valid_card not Typed: {:?}", c3.valid_card);
    };
    assert_eq!(tf3.controller, Some(ControllerRef::You));

    // The effect must be DamageEachPlayer — no Unimplemented leakage.
    let execute = def.execute.as_ref().expect("execute ability");
    fn has_damage_each_player(ability: &AbilityDefinition) -> bool {
        matches!(*ability.effect, Effect::DamageEachPlayer { .. })
            || ability
                .sub_ability
                .as_ref()
                .is_some_and(|s| has_damage_each_player(s))
    }
    fn has_unimplemented(ability: &AbilityDefinition) -> bool {
        matches!(*ability.effect, Effect::Unimplemented { .. })
            || ability
                .sub_ability
                .as_ref()
                .is_some_and(|s| has_unimplemented(s))
    }
    assert!(
        has_damage_each_player(execute),
        "expected DamageEachPlayer effect, got {:?}",
        execute.effect
    );
    assert!(
        !has_unimplemented(execute),
        "effect chain leaked Unimplemented: {:?}",
        execute
    );
}

// CR 121.1 + CR 603.4: "draw cards equal to the difference" inside a trigger
// body. The "if you have fewer than N cards in hand" gate is hoisted to the
// trigger-level condition, so the anaphoric draw count must resolve against
// those operands (Difference{HandSize, N}), not leak as Unimplemented.
// Kozilek the Great Distortion, Damia Sage of Stone, Krang Master Mind,
// The Ten Rings, Doctor Octopus.
#[test]
fn parse_difference_draw_trigger_resolves_against_hoisted_hand_size_gate() {
    for (text, name, threshold) in [
        (
            "When you cast this spell, if you have fewer than seven cards in hand, \
                 draw cards equal to the difference.",
            "Kozilek, the Great Distortion",
            7,
        ),
        (
            "At the beginning of your end step, if you have fewer than ten cards in hand, \
                 draw cards equal to the difference.",
            "The Ten Rings",
            10,
        ),
    ] {
        let defs = parse_trigger_lines(text, name);
        assert_eq!(defs.len(), 1, "{name}: expected one trigger: {defs:?}");
        let execute = defs[0].execute.as_ref().expect("execute ability");
        match &*execute.effect {
            Effect::Draw {
                count: QuantityExpr::Difference { left, right },
                ..
            } => {
                assert_eq!(
                    **right,
                    QuantityExpr::Fixed { value: threshold },
                    "{name}: wrong difference threshold"
                );
                assert!(
                    matches!(
                        **left,
                        QuantityExpr::Ref {
                            qty: QuantityRef::HandSize { .. }
                        }
                    ),
                    "{name}: expected HandSize lhs, got {left:?}"
                );
            }
            other => panic!("{name}: expected Draw with Difference count, got {other:?}"),
        }
    }
}

// CR 603.6a + CR 110.5b: "When this land enters untapped, ..." — Gingerbread
// Cabin class. The trigger must carry `Not { Box::new(ZoneChangeObjectIsTapped) }`
// so it only fires when the ETB-tapped replacement did NOT apply. For a
// SelfRef trigger the entering object IS the source, so the evaluator's
// `source_id` fallback resolves to the same permanent.
#[test]
fn parse_serial_attack_block_target_compound() {
    let defs = parse_trigger_lines(
        "Whenever this creature attacks, blocks, or becomes the target of a spell, \
             it deals damage equal to its power to each opponent.",
        "Giggling Skitterspike",
    );

    assert_eq!(defs.len(), 3, "expected three trigger branches: {defs:?}");
    assert_eq!(defs[0].mode, TriggerMode::Attacks);
    assert_eq!(defs[0].valid_card, Some(TargetFilter::SelfRef));
    assert_eq!(defs[1].mode, TriggerMode::Blocks);
    assert_eq!(defs[1].valid_card, Some(TargetFilter::SelfRef));
    assert_eq!(defs[2].mode, TriggerMode::BecomesTarget);
    assert_eq!(defs[2].valid_card, Some(TargetFilter::SelfRef));
    assert_eq!(defs[2].valid_source, Some(TargetFilter::StackSpell));

    fn has_damage_each_player(ability: &AbilityDefinition) -> bool {
        matches!(*ability.effect, Effect::DamageEachPlayer { .. })
            || ability
                .sub_ability
                .as_ref()
                .is_some_and(|s| has_damage_each_player(s))
    }
    fn has_unimplemented(ability: &AbilityDefinition) -> bool {
        matches!(*ability.effect, Effect::Unimplemented { .. })
            || ability
                .sub_ability
                .as_ref()
                .is_some_and(|s| has_unimplemented(s))
    }
    fn damage_each_player_amount(ability: &AbilityDefinition) -> Option<&QuantityExpr> {
        match ability.effect.as_ref() {
            Effect::DamageEachPlayer { amount, .. } => Some(amount),
            _ => ability
                .sub_ability
                .as_ref()
                .and_then(|s| damage_each_player_amount(s)),
        }
    }

    for def in &defs {
        let execute = def.execute.as_ref().expect("execute ability");
        assert!(
            has_damage_each_player(execute),
            "expected DamageEachPlayer effect, got {:?}",
            execute.effect
        );
        assert!(
            !has_unimplemented(execute),
            "effect chain leaked Unimplemented: {:?}",
            execute
        );
        assert!(
            matches!(
                damage_each_player_amount(execute),
                Some(QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: crate::types::ability::ObjectScope::Source
                    }
                })
            ),
            "expected damage amount to use source power, got {:?}",
            damage_each_player_amount(execute)
        );
    }
}

#[test]
fn trigger_etb_self_enters_untapped_attaches_condition() {
    let def = parse_trigger_line(
        "When this land enters untapped, create a Food token.",
        "Gingerbread Cabin",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert_eq!(
        def.condition,
        Some(TriggerCondition::Not {
            condition: Box::new(TriggerCondition::ZoneChangeObjectIsTapped)
        })
    );
    assert!(def.execute.is_some());
}

#[test]
fn trigger_lands_enter_without_being_played_attaches_not_was_played() {
    let def = parse_trigger_line(
            "Whenever one or more lands enter under an opponent's control without being played, you may search your library for a Plains card, put it onto the battlefield tapped, then shuffle.",
            "Deep Gnome Terramancer",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    let Some(TargetFilter::Typed(valid_card)) = &def.valid_card else {
        panic!("expected Typed valid_card, got {:?}", def.valid_card);
    };
    assert_eq!(valid_card.controller, Some(ControllerRef::Opponent));
    assert!(valid_card.type_filters.contains(&TypeFilter::Land));
    assert_eq!(
        def.condition,
        Some(TriggerCondition::Not {
            condition: Box::new(TriggerCondition::WasPlayed)
        })
    );
}

#[test]
fn trigger_etb_self_if_not_token_attaches_zone_change_non_token_condition() {
    let def = parse_trigger_line(
        "When this creature enters, if it isn't a token, create two tokens that are copies of it.",
        "Gruff Triplets",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));

    match def.condition {
        Some(TriggerCondition::ZoneChangeObjectMatchesFilter {
            origin,
            destination,
            filter: TargetFilter::Typed(typed),
        }) => {
            assert_eq!(origin, None);
            assert_eq!(destination, Zone::Battlefield);
            assert!(typed.type_filters.contains(&TypeFilter::Permanent));
            assert!(
                typed.properties.contains(&FilterProp::NonToken),
                "expected NonToken condition, got {typed:?}"
            );
        }
        other => panic!("expected zone-change NonToken condition, got {other:?}"),
    }

    let execute = def.execute.as_ref().expect("execute ability");
    assert!(matches!(
        execute.effect.as_ref(),
        Effect::CopyTokenOf {
            count: QuantityExpr::Fixed { value: 2 },
            ..
        }
    ));
}

/// CR 603.4 + CR 111.1: Life of the Party — "if it's not a token" uses the
/// `'s not` contraction axis; must hoist to the same NonToken intervening-if
/// as the explicit "if it isn't a token" form.
#[test]
fn trigger_etb_self_if_its_not_token_attaches_zone_change_non_token_condition() {
    for text in [
            "When this creature enters, if it's not a token, each opponent creates a token that's a copy of it.",
            "When this creature enters, if it is not a token, each opponent creates a token that's a copy of it.",
        ] {
            let def = parse_trigger_line(text, "Life of the Party");
            assert_eq!(def.mode, TriggerMode::ChangesZone);
            assert_eq!(def.destination, Some(Zone::Battlefield));
            match def.condition {
                Some(TriggerCondition::ZoneChangeObjectMatchesFilter {
                    filter: TargetFilter::Typed(typed),
                    ..
                }) => assert!(
                    typed.properties.contains(&FilterProp::NonToken),
                    "expected NonToken for {text:?}, got {typed:?}"
                ),
                other => panic!("expected NonToken intervening-if for {text:?}, got {other:?}"),
            }
        }
}

/// CR 603.4 + CR 603.6a + CR 208.1: Hulkling, Burgeoning Bruiser —
/// "Whenever another creature you control enters, if it has greater power or
/// toughness than ~, put a +1/+1 counter on ~." The OR intervening-if must
/// hoist to a `ZoneChangeObjectMatchesFilter` with destination Battlefield
/// (NOT the Graveyard look-back path) carrying an `AnyOf` of the two
/// source-relative `PtComparison` props. The effect (PutCounter +1/+1 on
/// SelfRef) must survive condition stripping. Fail-before: `def.condition`
/// is `None` (the intervening-if is dropped).
#[test]
fn trigger_hulkling_etb_greater_power_or_toughness_than_source() {
    let def = parse_trigger_line(
            "Whenever another creature you control enters, if it has greater power or toughness than Hulkling, put a +1/+1 counter on Hulkling.",
            "Hulkling, Burgeoning Bruiser",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);

    let power_prop = FilterProp::PtComparison {
        stat: PtStat::Power,
        scope: PtValueScope::Current,
        comparator: Comparator::GT,
        value: QuantityExpr::Ref {
            qty: QuantityRef::Power {
                scope: ObjectScope::Source,
            },
        },
    };
    let toughness_prop = FilterProp::PtComparison {
        stat: PtStat::Toughness,
        scope: PtValueScope::Current,
        comparator: Comparator::GT,
        value: QuantityExpr::Ref {
            qty: QuantityRef::Toughness {
                scope: ObjectScope::Source,
            },
        },
    };

    match def.condition {
        Some(TriggerCondition::ZoneChangeObjectMatchesFilter {
            origin,
            destination,
            filter: TargetFilter::Typed(ref typed),
        }) => {
            assert_eq!(origin, None);
            assert_eq!(destination, Zone::Battlefield);
            assert!(
                typed.type_filters.contains(&TypeFilter::Creature),
                "expected creature type filter, got {typed:?}"
            );
            assert_eq!(
                typed.properties,
                vec![FilterProp::AnyOf {
                    props: vec![power_prop, toughness_prop],
                }],
                "expected AnyOf of source-relative power+toughness, got {typed:?}"
            );
        }
        ref other => panic!("expected ZoneChangeObjectMatchesFilter, got {other:?}"),
    }

    // Effect must remain PutCounter +1/+1 on the source (SelfRef) — the
    // condition strip must not corrupt the effect clause.
    let execute = def.execute.as_ref().expect("execute ability");
    match execute.effect.as_ref() {
        Effect::PutCounter {
            counter_type,
            count,
            target,
        } => {
            assert_eq!(*counter_type, CounterType::Plus1Plus1);
            assert_eq!(*count, QuantityExpr::Fixed { value: 1 });
            assert_eq!(*target, TargetFilter::SelfRef);
        }
        other => panic!("expected PutCounter +1/+1 on SelfRef, got {other:?}"),
    }
}

/// CR 603.4 + CR 208.1: single-stat "if it has greater power than ~" emits
/// exactly ONE `PtComparison(Power)` — NOT wrapped in `AnyOf`. Mirror for
/// toughness. Proves the disjunction is only produced for the "or" form.
#[test]
fn parse_entering_single_stat_greater_than_source_emits_one_prop() {
    for (text, stat, qty) in [
        (
            "if it has greater power than ~",
            PtStat::Power,
            QuantityRef::Power {
                scope: ObjectScope::Source,
            },
        ),
        (
            "if it has greater toughness than ~",
            PtStat::Toughness,
            QuantityRef::Toughness {
                scope: ObjectScope::Source,
            },
        ),
    ] {
        let (rest, condition) =
            parse_entering_pt_vs_source_condition(text).expect("single-stat parses");
        assert_eq!(rest, "", "fully consumed for {text:?}");
        match condition {
            TriggerCondition::ZoneChangeObjectMatchesFilter {
                destination,
                filter: TargetFilter::Typed(typed),
                ..
            } => {
                assert_eq!(destination, Zone::Battlefield);
                assert_eq!(
                    typed.properties,
                    vec![FilterProp::PtComparison {
                        stat,
                        scope: PtValueScope::Current,
                        comparator: Comparator::GT,
                        value: QuantityExpr::Ref { qty },
                    }],
                    "single-stat {text:?} must be one PtComparison, not AnyOf"
                );
                assert!(
                    !matches!(typed.properties.first(), Some(FilterProp::AnyOf { .. })),
                    "single-stat {text:?} must not wrap in AnyOf"
                );
            }
            other => panic!("expected ZoneChange condition for {text:?}, got {other:?}"),
        }
    }
}

/// Regression: a fixed-threshold P/T constraint must stay a literal
/// `PtComparison` with a `Fixed` value — the new source-relative arm must NOT
/// hijack it. Drives the shared `nom_filter::parse_pt_comparison` building
/// block to confirm "power 4 or greater" remains fixed (no `Source` ref).
#[test]
fn fixed_threshold_pt_comparison_stays_fixed() {
    use crate::parser::oracle_nom::filter::parse_pt_comparison;
    let (rest, prop) = parse_pt_comparison("power 4 or greater").expect("fixed P/T parses");
    assert_eq!(rest, "");
    assert_eq!(
        prop,
        FilterProp::PtComparison {
            stat: PtStat::Power,
            scope: PtValueScope::Current,
            comparator: Comparator::GE,
            value: QuantityExpr::Fixed { value: 4 },
        },
        "fixed-threshold P/T must remain a Fixed PtComparison, not source-relative"
    );
}

/// CR 603.4 + CR 701.15b: Life of the Party — token-copy ETB sub-clause
/// "The tokens are goaded for the rest of the game" must rewrite to a
/// permanent GenericEffect on LastCreated, not Unimplemented.
#[test]
fn trigger_life_of_the_party_etb_goads_created_tokens() {
    let def = parse_trigger_line(
            "When this creature enters, if it's not a token, each opponent creates a token that's a copy of it. The tokens are goaded for the rest of the game.",
            "Life of the Party",
        );
    let execute = def.execute.as_ref().expect("execute ability");
    assert!(
        matches!(execute.effect.as_ref(), Effect::CopyTokenOf { .. }),
        "expected CopyTokenOf primary, got {:?}",
        execute.effect
    );
    let sub = execute.sub_ability.as_ref().expect("goad sub_ability");
    match sub.effect.as_ref() {
        Effect::GenericEffect {
            static_abilities,
            duration,
            target,
        } => {
            assert_eq!(*target, Some(TargetFilter::LastCreated));
            assert_eq!(*duration, Some(Duration::Permanent));
            assert!(static_abilities[0].modifications.iter().any(|m| matches!(
                m,
                ContinuousModification::AddStaticMode {
                    mode: StaticMode::Goaded
                }
            )));
        }
        other => panic!("expected GenericEffect goad sub, got {other:?}"),
    }
}

#[test]
fn zone_change_token_predicate_parses_present_and_past_negation_forms() {
    for (text, expected_prop) in [
        ("is a token", FilterProp::Token),
        ("was a token", FilterProp::Token),
        ("isn't a token", FilterProp::NonToken),
        ("is not a token", FilterProp::NonToken),
        ("wasn't a token", FilterProp::NonToken),
        ("was not a token", FilterProp::NonToken),
    ] {
        let (rest, condition) =
            parse_zone_change_object_token_predicate(text).expect("token predicate parses");
        assert_eq!(rest, "");

        match condition {
            TriggerCondition::ZoneChangeObjectMatchesFilter {
                filter: TargetFilter::Typed(typed),
                ..
            } => assert!(
                typed.properties.contains(&expected_prop),
                "expected {expected_prop:?} for {text}, got {typed:?}"
            ),
            other => panic!("expected token filter condition for {text}, got {other:?}"),
        }
    }
}

#[test]
fn trigger_dies_if_it_was_enchanted_attaches_attachment_lookback() {
    let def = parse_trigger_line(
        "When this creature dies, if it was enchanted, create a Junk token.",
        "Gunner Conscript",
    );
    match def.condition {
        Some(TriggerCondition::ZoneChangeObjectMatchesFilter {
            filter: TargetFilter::Typed(typed),
            ..
        }) => {
            let has_aura = typed.properties.iter().any(|p| match p {
                FilterProp::HasAttachment { kind, .. } => *kind == AttachmentKind::Aura,
                FilterProp::HasAnyAttachmentOf { kinds, .. } => {
                    kinds.contains(&AttachmentKind::Aura)
                }
                _ => false,
            });
            assert!(has_aura, "expected enchanted lookback, got {typed:?}");
        }
        other => panic!("expected attachment intervening-if, got {other:?}"),
    }
}

/// CR 208.1 + CR 603.4 + CR 603.10a + CR 608.2h: Deathknell Berserker -- a
/// dies-trigger intervening "if" gated on the creature's last-known power
/// ("if its power was 3 or greater"). The look-back reads the dying creature's
/// last-known power (CR 603.10a + CR 608.2h), so the possessive-subject
/// threshold uses past-tense "was". Before the linking-verb axis accepted
/// "was", the condition silently swallowed and the Zombie Berserker token was
/// created on every death regardless of power.
///
/// Asserted shape:
/// - Trigger-level `condition` is a `QuantityComparison` on Power(Source) >= 3.
/// - The execute effect is Token creation (the Zombie Berserker), not Unimplemented.
#[test]
fn parse_deathknell_berserker_dies_power_lki_intervening_if() {
    let def = parse_trigger_line(
        "When this creature dies, if its power was 3 or greater, create a 2/2 black Zombie Berserker creature token.",
        "Deathknell Berserker",
    );

    assert_eq!(
        def.condition,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: crate::types::ability::ObjectScope::Source,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 3 },
        }),
        "trigger-level intervening-if must be Power(Source) >= 3, got {:?}",
        def.condition,
    );

    let execute = def.execute.as_deref().expect("execute must be Some");
    assert!(
        matches!(*execute.effect, Effect::Token { .. }),
        "execute effect must be Token creation, got {:?}",
        execute.effect,
    );
    assert!(
        !matches!(*execute.effect, Effect::Unimplemented { .. }),
        "execute effect must not be Unimplemented",
    );
}

/// CR 115.1d: "attach any number of target Equipment you control to it"
/// (Super-Soldier Serum) is a variable-count target down to zero. The execute
/// ability must carry a `multi_target` spec so the player can decline or
/// choose multiple Equipment.
#[test]
fn trigger_attacks_or_blocks_attach_any_number_optional_targeting() {
    use crate::types::ability::MultiTargetSpec;

    let def = parse_trigger_line(
            "Whenever enchanted creature attacks or blocks, attach any number of target Equipment you control to it.",
            "Super-Soldier Serum",
        );
    assert_eq!(def.mode, TriggerMode::AttacksOrBlocks);
    let execute = def
        .execute
        .as_deref()
        .expect("attach body must lower to an execute ability");
    assert!(
        matches!(execute.effect.as_ref(), Effect::Attach { .. }),
        "expected Attach effect, got {:?}",
        execute.effect
    );
    assert_eq!(
        execute.multi_target,
        Some(MultiTargetSpec::unlimited(0)),
        "\"any number of target\" must surface an unlimited zero-min target spec"
    );
}

#[test]
fn trigger_attach_any_number_in_chain_stays_on_attach_node() {
    use crate::types::ability::MultiTargetSpec;

    let def = parse_trigger_line(
            "Whenever enchanted creature attacks or blocks, draw a card. Attach any number of target Equipment you control to it.",
            "Serum Chain Test",
        );
    let execute = def
        .execute
        .as_deref()
        .expect("trigger body must lower to an execute ability");
    assert!(
        execute.multi_target.is_none(),
        "head ability must not inherit attach target cardinality"
    );
    let attach = execute
        .sub_ability
        .as_deref()
        .expect("attach must be chained after draw");
    assert!(
        matches!(attach.effect.as_ref(), Effect::Attach { .. }),
        "expected Attach sub-ability, got {:?}",
        attach.effect
    );
    assert_eq!(
        attach.multi_target,
        Some(MultiTargetSpec::unlimited(0)),
        "Attach sub-ability must carry the any-number target spec"
    );
}

#[test]
fn zone_change_token_contraction_intervening_if_parses_its_not_a_token() {
    let (rest, condition) = parse_zone_change_object_token_contraction_intervening_if(
        "if it's not a token, create a token",
    )
    .expect("contraction intervening-if parses");
    assert_eq!(rest, ", create a token");
    match condition {
        TriggerCondition::ZoneChangeObjectMatchesFilter {
            filter: TargetFilter::Typed(typed),
            ..
        } => assert!(typed.properties.contains(&FilterProp::NonToken)),
        other => panic!("expected NonToken condition, got {other:?}"),
    }
}

/// Issue #3682 — Chainer, Nightmare Adept: "if you didn't cast it from your
/// hand" must hoist as `Not(WasCast { zone: Hand, caster=you, owner=you })`,
/// NOT as `Not(EffectOutcome(OptionalEffectPerformed))`. CR 404.1: "your
/// hand" scopes both the caster and the owner-specific zone to you.
#[test]
fn trigger_intervening_if_negated_cast_from_hand_chainer() {
    let def = parse_trigger_line(
            "Whenever a nontoken creature you control enters, if you didn't cast it from your hand, it gains haste until your next turn.",
            "Chainer, Nightmare Adept",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    match &def.condition {
        Some(TriggerCondition::Not { condition }) => {
            assert!(
                matches!(
                    condition.as_ref(),
                    TriggerCondition::WasCast {
                        zone: Some(Zone::Hand),
                        controller: Some(ControllerRef::You),
                        owner: Some(ControllerRef::You),
                    }
                ),
                "expected WasCast {{ zone: Hand, you/you }}, got {:?}",
                condition
            );
        }
        other => panic!("expected Not(WasCast {{ zone: Hand, you/you }}), got {other:?}"),
    }
}

/// Discordant Spirit: "if it's an opponent's turn" must hoist as the
/// intervening-if condition. CR 102.1 + CR 102.2: a turn is never vacant, so
/// "an opponent's turn" is "the active player is any non-controller" —
/// `Not(DuringPlayersTurn { Controller })`, equivalent to "it's not your
/// turn". Without this the condition was silently dropped and the counter
/// would be placed on the controller's own end step too.
#[test]
fn trigger_intervening_if_opponents_turn_discordant_spirit() {
    let def = parse_trigger_line(
            "At the beginning of each end step, if it's an opponent's turn, put a +1/+1 counter on this creature for each 1 damage dealt to you this turn.",
            "Discordant Spirit",
        );
    match &def.condition {
        Some(TriggerCondition::Not { condition }) => {
            assert!(
                matches!(
                    condition.as_ref(),
                    TriggerCondition::DuringPlayersTurn {
                        player: PlayerFilter::Controller,
                    }
                ),
                "expected Not(DuringPlayersTurn {{ Controller }}), got {condition:?}"
            );
        }
        other => panic!("expected Not(DuringPlayersTurn), got {other:?}"),
    }
}

#[test]
fn trigger_intervening_if_spell_from_hand_this_turn_attaches_condition() {
    let def = parse_trigger_line(
            "At the beginning of your end step, if you haven't cast a spell from your hand this turn, draw a card.",
            "Jem Lightfoote, Sky Explorer",
        );
    match def.condition {
        Some(TriggerCondition::QuantityComparison {
            lhs:
                QuantityExpr::Ref {
                    qty:
                        QuantityRef::SpellsCastThisTurn {
                            scope: CountScope::Controller,
                            filter: Some(TargetFilter::Typed(TypedFilter { properties, .. })),
                        },
                },
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: 0 },
        }) => assert!(properties.contains(&FilterProp::InZone { zone: Zone::Hand })),
        Some(TriggerCondition::And { conditions }) => {
            assert!(conditions.iter().any(|condition| matches!(
                condition,
                TriggerCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::SpellsCastThisTurn {
                            scope: CountScope::Controller,
                            filter: Some(TargetFilter::Typed(TypedFilter { properties, .. })),
                        },
                    },
                    comparator: Comparator::EQ,
                    rhs: QuantityExpr::Fixed { value: 0 },
                } if properties.contains(&FilterProp::InZone { zone: Zone::Hand })
            )))
        }
        other => panic!("expected cast-origin intervening condition, got {other:?}"),
    }
}

#[test]
fn trigger_intervening_if_source_didnt_attack_this_turn_attaches_condition() {
    let def = parse_trigger_line(
            "At the beginning of your end step, if this creature didn't attack this turn, put a +1/+1 counter on it.",
            "Air Nomad Student",
        );
    assert!(matches!(
        def.condition,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount {
                    filter: TargetFilter::And { .. },
                },
            },
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: 0 },
        })
    ));
}

/// CR 506.2 + CR 508.6 + CR 603.4 (issue #2924): Suppressor Skyguard's
/// attack-trigger intervening-if must hoist to `def.condition` as a
/// `PlayerCount(OpponentOfTriggeringPlayerNotAttacked) >= 1` comparison.
/// Bug A was the condition being dropped (`None`); this test fails if the
/// new combinator/bridge regresses.
#[test]
fn suppressor_skyguard_attack_trigger_hoists_unattacked_opponent_condition() {
    let def = parse_trigger_line(
            "Whenever a player attacks you, if that player has another opponent who isn't being attacked, prevent all combat damage that would be dealt to you this combat.",
            "Suppressor Skyguard",
        );
    let expected = TriggerCondition::QuantityComparison {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentOfTriggeringPlayerNotAttacked,
            },
        },
        comparator: Comparator::GE,
        rhs: QuantityExpr::Fixed { value: 1 },
    };
    match def.condition {
        Some(cond) if cond == expected => {}
        Some(TriggerCondition::And { conditions }) => assert!(
            conditions.contains(&expected),
            "And condition missing the unattacked-opponent comparison: {conditions:?}"
        ),
        other => panic!("expected hoisted unattacked-opponent condition, got {other:?}"),
    }
}

#[test]
fn trigger_intervening_if_no_creatures_attacked_this_turn_attaches_condition() {
    let def = parse_trigger_line(
            "At the beginning of each player's end step, if no creatures attacked this turn, put a fury counter on this creature.",
            "Charging Cinderhorn",
        );
    assert!(matches!(
        def.condition,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::AttackedThisTurn {
                    scope: CountScope::All,
                    filter: Some(TargetFilter::Typed(ref tf)),
                },
            },
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: 0 },
        }) if tf.type_filters.contains(&TypeFilter::Creature)
    ));
}

// CR 603.6a + CR 110.5b: "Whenever a permanent you control enters tapped, ..." —
// Amulet of Vigor class. The `enters tapped` rider must set
// `ZoneChangeObjectIsTapped` (fires only when the *entering* permanent is
// tapped — NOT the ability source). For observer triggers the entering
// permanent differs from the ability source, so the subject-correct variant
// is required.
#[test]
fn trigger_etb_subject_enters_tapped_attaches_condition() {
    let def = parse_trigger_line(
        "Whenever a permanent you control enters tapped, untap it.",
        "Amulet of Vigor",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    assert_eq!(
        def.condition,
        Some(TriggerCondition::ZoneChangeObjectIsTapped)
    );
}

/// CR 608.2k: the "untap it"/"tap it" bare-object-pronoun anaphor binds by
/// the trigger SUBJECT, not to `ParentTarget` (which resolves against the
/// effect's empty target list on a primary, non-targeted trigger effect and
/// silently no-ops the untap). This is a parse-shape building-block test for
/// the whole class, exercised end-to-end by
/// `tests/integration/issue_2915_alexios.rs`:
///   - typed/attached subject ("a permanent you control", "equipped
///     creature") → `TriggeringSource` (the entering/attacking object),
///     matching the sibling sacrifice/destroy/exile anaphor verbs;
///   - self subject (the source named in the same instruction, Alexios's
///     "gains control of ~, untaps it") → `SelfRef`.
#[test]
fn untap_it_anaphor_binds_to_trigger_subject_not_parent_target() {
    fn find_untap_target(ability: &AbilityDefinition) -> &TargetFilter {
        let mut node = Some(ability);
        while let Some(current) = node {
            if let Effect::SetTapState {
                target,
                state: TapStateChange::Untap,
                ..
            } = current.effect.as_ref()
            {
                return target;
            }
            node = current.sub_ability.as_deref();
        }
        panic!("expected an Untap SetTapState in the trigger chain");
    }

    // Typed subject: "it" is the entering permanent (the triggering object).
    let amulet = parse_trigger_line(
        "Whenever a permanent you control enters tapped, untap it.",
        "Amulet of Vigor",
    );
    assert_eq!(
        find_untap_target(amulet.execute.as_ref().expect("execute present")),
        &TargetFilter::TriggeringSource,
        "typed-subject 'untap it' must bind to the triggering object"
    );

    // Attached subject: "it" is the equipped (attacking) creature.
    let genji = parse_trigger_line(
        "Whenever equipped creature attacks, untap it.",
        "Genji Glove",
    );
    assert_eq!(
        find_untap_target(genji.execute.as_ref().expect("execute present")),
        &TargetFilter::TriggeringSource,
        "attached-subject 'untap it' must bind to the triggering object"
    );

    // Self subject: "it" is the source named earlier in the instruction.
    let alexios = parse_trigger_line(
        "At the beginning of each player's upkeep, that player gains control of \
             Alexios, untaps it, and puts a +1/+1 counter on it. It gains haste until \
             end of turn.",
        "Alexios, Deimos of Kosmos",
    );
    assert_eq!(
        find_untap_target(alexios.execute.as_ref().expect("execute present")),
        &TargetFilter::SelfRef,
        "self-subject 'untaps it' must bind to the named source"
    );
}

// CR 603.6a + CR 110.5b: "Whenever an artifact or creature an opponent
// controls enters untapped, ..." — Charismatic Conqueror class. The
// `enters untapped` rider must wrap `ZoneChangeObjectIsTapped` in `Not`,
// and the subject is the entering (opponent's) permanent — an observer
// trigger where source ≠ entering object.
#[test]
fn trigger_etb_subject_enters_untapped_attaches_negated_condition() {
    let def = parse_trigger_line(
            "Whenever an artifact or creature an opponent controls enters untapped, they may tap that permanent. If they don't, you create a 1/1 white Vampire creature token with lifelink.",
            "Charismatic Conqueror",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    assert_eq!(
        def.condition,
        Some(TriggerCondition::Not {
            condition: Box::new(TriggerCondition::ZoneChangeObjectIsTapped)
        })
    );
}

// Guard: a bare "enters" (no tapped-state rider) must NOT attach a
// tapped-state condition (neither `SourceIsTapped` nor
// `ZoneChangeObjectIsTapped`).
#[test]
fn trigger_etb_bare_enters_has_no_tapped_condition() {
    let def = parse_trigger_line(
        "When this creature enters, draw a card.",
        "Elvish Visionary",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    let is_tapped_condition = |c: &TriggerCondition| {
        matches!(
            c,
            TriggerCondition::SourceIsTapped | TriggerCondition::ZoneChangeObjectIsTapped
        )
    };
    assert!(
        !def.condition.as_ref().is_some_and(is_tapped_condition)
            && !matches!(
                &def.condition,
                Some(TriggerCondition::Not { condition })
                    if is_tapped_condition(condition)
            ),
        "bare `enters` must not attach a tapped-state condition; got {:?}",
        def.condition
    );
}

// Guard: Genesis Chamber's "if ~ is untapped" intervening-if goes through
// `static_condition_to_trigger_condition` — a DIFFERENT parser path from
// `parse_enters_tapped_state_rider` — and must keep emitting the
// source-bound `SourceIsTapped` (the subject is the ability's own source,
// not a zone-change event object). This pins the two paths as distinct so
// the ETB-rider fix does not leak into source-bound intervening-ifs.
#[test]
fn trigger_intervening_if_source_untapped_keeps_source_is_tapped() {
    let def = parse_trigger_line(
            "Whenever a nontoken creature enters, if this artifact is untapped, that creature's controller creates a 1/1 colorless Myr artifact creature token.",
            "Genesis Chamber",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(
        def.condition,
        Some(TriggerCondition::Not {
            condition: Box::new(TriggerCondition::SourceIsTapped)
        })
    );
}

#[test]
fn trigger_first_combat_phase_condition() {
    let def = parse_trigger_line(
            "Whenever equipped creature attacks, if it's the first combat phase of the turn, untap it. After this phase, there is an additional combat phase.",
            "Genji Glove",
        );
    assert_eq!(
        def.condition,
        Some(TriggerCondition::FirstCombatPhaseOfTurn)
    );
}

#[test]
fn trigger_first_combat_phase_followup_condition() {
    let def = parse_trigger_line(
            "Whenever a Samurai or Warrior you control attacks alone, untap it. If it's the first combat phase of the turn, there is an additional combat phase after this phase.",
            "A-Raiyuu, Storm's Edge",
        );
    assert!(def.condition.is_none());
    let followup = def
        .execute
        .as_ref()
        .and_then(|ability| ability.sub_ability.as_deref())
        .expect("additional combat follow-up");
    assert_eq!(
        followup.condition,
        Some(AbilityCondition::FirstCombatPhaseOfTurn)
    );
}

// Word-boundary guard: "enters untapped creatures" (hypothetical) must not
// accidentally match — the combinator requires a terminator after the
// state word.
#[test]
fn trigger_etb_untapped_rider_requires_word_boundary() {
    assert!(parse_enters_tapped_state_rider("s untappedness").is_none());
    assert!(parse_enters_tapped_state_rider("s tappedly").is_none());
    // Valid terminators:
    assert_eq!(
        parse_enters_tapped_state_rider("s untapped"),
        Some(TriggerCondition::Not {
            condition: Box::new(TriggerCondition::ZoneChangeObjectIsTapped)
        })
    );
    assert_eq!(
        parse_enters_tapped_state_rider("s untapped "),
        Some(TriggerCondition::Not {
            condition: Box::new(TriggerCondition::ZoneChangeObjectIsTapped)
        })
    );
    assert_eq!(
        parse_enters_tapped_state_rider("s tapped,"),
        Some(TriggerCondition::ZoneChangeObjectIsTapped)
    );
}

#[test]
fn trigger_dies() {
    let def = parse_trigger_line(
        "When this creature dies, create a 1/1 white Spirit creature token.",
        "Some Card",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.origin, Some(Zone::Battlefield));
    assert_eq!(def.destination, Some(Zone::Graveyard));
}

#[test]
fn trigger_combat_damage_to_player() {
    let def = parse_trigger_line(
        "Whenever Eye Collector deals combat damage to a player, each player mills a card.",
        "Eye Collector",
    );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    assert_eq!(def.valid_target, Some(TargetFilter::Player));
}

#[test]
fn that_creature_subject_resolves_to_parent_target() {
    let mut ctx = ParseContext::default();
    let (_, def) = parse_trigger_condition(
        "that creature deals combat damage to a player or planeswalker",
        &mut ctx,
    );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    assert_eq!(def.valid_source, Some(TargetFilter::ParentTarget));
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Or {
            filters: vec![
                TargetFilter::Player,
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Planeswalker)),
            ],
        })
    );
}

#[test]
fn that_permanent_attacks_subject_resolves_to_parent_target() {
    let mut ctx = ParseContext::default();
    let (_, def) = parse_trigger_condition("that permanent attacks", &mut ctx);
    assert_eq!(def.mode, TriggerMode::Attacks);
    assert_eq!(def.valid_card, Some(TargetFilter::ParentTarget));
}

#[test]
fn creature_subject_still_returns_typed_filter() {
    let mut ctx = ParseContext::default();
    let (_, def) = parse_trigger_condition("a creature deals combat damage to a player", &mut ctx);
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(
        def.valid_source,
        Some(TargetFilter::Typed(TypedFilter::creature()))
    );
}

#[test]
fn impostor_syndrome_copy_it_binds_to_damage_source() {
    let def = parse_trigger_line(
        "Whenever a nontoken creature you control deals combat damage to a player, \
             create a token that's a copy of it, except it isn't legendary.",
        "Impostor Syndrome",
    );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    match def.execute.as_ref().unwrap().effect.as_ref() {
        Effect::CopyTokenOf {
            target,
            additional_modifications,
            ..
        } => {
            assert_eq!(*target, TargetFilter::TriggeringSource);
            assert!(additional_modifications.iter().any(|m| matches!(
                m,
                ContinuousModification::RemoveSupertype {
                    supertype: crate::types::card_type::Supertype::Legendary
                }
            )));
        }
        other => panic!("expected CopyTokenOf, got {other:?}"),
    }
}

#[test]
fn renowned_creature_damage_subject_keeps_designation_filter() {
    let def = parse_trigger_line(
            "Whenever a renowned creature you control deals combat damage to a player, double the number of +1/+1 counters on it.",
            "Aragorn, Hornburg Hero",
        );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    assert_eq!(def.valid_target, Some(TargetFilter::Player));
    assert_eq!(
        def.valid_source,
        Some(TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::Renowned])
        ))
    );
    let exec = def.execute.as_deref().expect("execute should be set");
    assert!(matches!(
        exec.effect.as_ref(),
        Effect::MultiplyCounter {
            target: TargetFilter::TriggeringSource,
            ..
        }
    ));
}

/// MSH Wave 2 (Molten Lavamancer): the batched "one or more of your opponents"
/// recipient must parse as a noncombat damage trigger whose recipient is an
/// opponent. Without the new `parse_opponent_player_recipient` arm, the
/// recipient is unmatched, `try_parse_source_deals_damage_trigger` bails on the
/// `valid_target.is_none()` guard, and `mode != DamageDone`.
#[test]
fn molten_lavamancer_one_or_more_opponents_recipient_parses() {
    let def = parse_trigger_line(
        "Whenever a source you control deals noncombat damage to one or more of your \
             opponents during your turn, you create a 1/1 red Elemental creature token. \
             This ability triggers only once each turn.",
        "Molten Lavamancer",
    );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(def.damage_kind, DamageKindFilter::NoncombatOnly);
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        )),
        "recipient must be an opponent-controlled player filter"
    );
}

#[test]
fn hunters_insight_class_builds_whenever_event_delayed_trigger() {
    use crate::parser::oracle::parse_oracle_text;

    let parsed = parse_oracle_text(
            "Choose target creature you control. Whenever that creature deals combat damage to a player or planeswalker this turn, draw that many cards.",
            "Hunter's Insight",
            &[],
            &["Instant".to_string()],
            &[],
        );
    assert_eq!(parsed.abilities.len(), 1);

    let delayed = parsed.abilities[0]
        .sub_ability
        .as_deref()
        .expect("target choice should chain into delayed trigger");
    let Effect::CreateDelayedTrigger {
        condition, effect, ..
    } = delayed.effect.as_ref()
    else {
        panic!("expected CreateDelayedTrigger, got {:?}", delayed.effect);
    };
    let DelayedTriggerCondition::WheneverEvent { trigger } = condition else {
        panic!("expected WheneverEvent, got {condition:?}");
    };
    assert_eq!(trigger.mode, TriggerMode::DamageDone);
    assert_eq!(trigger.damage_kind, DamageKindFilter::CombatOnly);
    assert_eq!(trigger.valid_source, Some(TargetFilter::ParentTarget));

    let Effect::Draw { count, target } = effect.effect.as_ref() else {
        panic!("expected Draw, got {:?}", effect.effect);
    };
    assert_eq!(
        *count,
        QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount
        }
    );
    assert_eq!(*target, TargetFilter::Controller);
}

// Thrummingbird: pins the trigger event shape AND the parsed execute body
// (Effect::Proliferate) — siblings above only assert the event shape.
// CR 603.2 + CR 701.34a.
#[test]
fn trigger_thrummingbird_combat_damage_proliferate() {
    let def = parse_trigger_line(
        "Whenever ~ deals combat damage to a player, proliferate.",
        "Thrummingbird",
    );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    assert_eq!(def.valid_target, Some(TargetFilter::Player));
    let exec = def.execute.as_deref().expect("execute should be set");
    assert!(
        matches!(exec.effect.as_ref(), Effect::Proliferate),
        "execute body should be Effect::Proliferate, got {:?}",
        exec.effect,
    );
}

#[test]
fn trigger_combat_damage_installs_until_next_turn_damage_doubler() {
    let def = parse_trigger_line(
            "Whenever Lightning deals combat damage to a player, until your next turn, if a source would deal damage to that player or a permanent that player controls, it deals double that damage instead.",
            "Lightning, Army of One",
        );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    assert_eq!(def.valid_source, Some(TargetFilter::SelfRef));
    assert_eq!(def.valid_target, Some(TargetFilter::Player));

    let exec = def.execute.as_deref().expect("execute should be set");
    assert_eq!(
        exec.duration,
        Some(Duration::UntilNextTurnOf {
            player: PlayerScope::Controller
        })
    );
    let Effect::AddTargetReplacement {
        replacement,
        target,
    } = exec.effect.as_ref()
    else {
        panic!("expected AddTargetReplacement, got {:?}", exec.effect);
    };
    assert_eq!(*target, TargetFilter::TriggeringPlayer);
    assert_eq!(replacement.event, ReplacementEvent::DamageDone);
    assert_eq!(
        replacement.damage_modification,
        Some(DamageModification::Double)
    );
    assert!(replacement.damage_target_filter.is_none());
}

#[test]
fn trigger_combat_damage_to_opponent() {
    let def = parse_trigger_line(
        "Whenever ~ deals combat damage to an opponent, draw a card.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        ))
    );
}

#[test]
fn trigger_subject_warns_on_any_fallback() {
    let mut ctx = ParseContext::default();
    let (filter, rest) = parse_single_subject("xyzzy", &mut ctx);
    assert_eq!(filter, TargetFilter::Any);
    assert_eq!(rest, "xyzzy");
    assert!(ctx.diagnostics.iter().any(
        |d| matches!(d, OracleDiagnostic::TargetFallback { context, text, .. }
                if context == "trigger subject parse fell back to Any" && text == "xyzzy")
    ));
}

#[test]
fn trigger_combat_damage_no_qualifier() {
    // "deals combat damage" with no "to X" — fires for any target
    let def = parse_trigger_line(
        "Whenever ~ deals combat damage, put a +1/+1 counter on ~.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    assert_eq!(def.valid_target, None);
}

#[test]
fn trigger_one_or_more_creatures_you_control_deal_combat_damage_to_player() {
    let def = parse_trigger_line(
            "Whenever one or more creatures you control deal combat damage to a player, create a Treasure token.",
            "Professional Face-Breaker",
        );
    assert_eq!(def.mode, TriggerMode::DamageDoneOnceByController);
    assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    assert_eq!(
        def.valid_source,
        Some(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You)
        ))
    );
    assert_eq!(def.valid_target, Some(TargetFilter::Player));
    assert!(
        def.batched,
        "one-or-more combat damage triggers are batched"
    );
}

#[test]
fn grim_hireling_combat_damage_trigger_is_batched() {
    let def = parse_trigger_line(
            "Whenever one or more creatures you control deal combat damage to a player, create two Treasure tokens.",
            "Grim Hireling",
        );
    assert_eq!(def.mode, TriggerMode::DamageDoneOnceByController);
    assert!(def.batched);
}

#[test]
fn primo_the_unbounded_combat_damage_trigger() {
    // Primo, the Unbounded (#1361): the triggering creatures' base power is 0,
    // and the Fractal token enters with +1/+1 counters equal to the combat
    // damage dealt (EventContextAmount). Verifies the full trigger:
    //   - DamageDoneOnceByController mode
    //   - valid_source carries a base-power-0 check (CR 208.4b)
    //   - the token-creation body has no Unimplemented effects
    let def = parse_trigger_line(
            "Whenever one or more creatures you control with base power 0 deal combat damage to a player, create a 0/0 green and blue Fractal creature token. Put a number of +1/+1 counters on it equal to the damage dealt.",
            "Primo, the Unbounded",
        );
    assert_eq!(def.mode, TriggerMode::DamageDoneOnceByController);
    assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    assert_eq!(def.valid_target, Some(TargetFilter::Player));

    // CR 208.4b: source filter must include a base-power-0 comparison.
    let TargetFilter::Typed(tf) = def
        .valid_source
        .as_ref()
        .expect("valid_source should be present")
    else {
        panic!(
            "valid_source should be a typed filter, got {:?}",
            def.valid_source
        );
    };
    assert!(
        tf.properties.iter().any(|p| matches!(
            p,
            FilterProp::PtComparison {
                stat: PtStat::Power,
                scope: PtValueScope::Base,
                comparator: Comparator::EQ,
                value: QuantityExpr::Fixed { value: 0 },
            }
        )),
        "expected base-power-0 PtComparison in source filter, got: {:?}",
        tf.properties
    );

    // No Unimplemented effects anywhere in the trigger body.
    let execute: &AbilityDefinition = def.execute.as_deref().expect("trigger should have execute");
    let mut current: Option<&AbilityDefinition> = Some(execute);
    while let Some(ability) = current {
        assert!(
            !matches!(*ability.effect, Effect::Unimplemented { .. }),
            "trigger body must not contain Unimplemented, got: {:?}",
            ability.effect
        );
        current = ability.sub_ability.as_deref();
    }

    // The Fractal token must enter with +1/+1 counters equal to the combat
    // damage dealt (EventContextAmount), not a fixed count. Walk the
    // sub-ability chain to find the Token effect and verify its
    // enter_with_counters payload.
    let mut current: Option<&AbilityDefinition> = Some(execute);
    let mut enter_with_counters: Option<&Vec<(CounterType, QuantityExpr)>> = None;
    while let Some(ability) = current {
        if let Effect::Token {
            enter_with_counters: counters,
            ..
        } = &*ability.effect
        {
            enter_with_counters = Some(counters);
            break;
        }
        current = ability.sub_ability.as_deref();
    }
    let counters = enter_with_counters.expect("trigger body should contain a Token effect");
    let expected: Vec<(CounterType, QuantityExpr)> = vec![(
        CounterType::Plus1Plus1,
        QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        },
    )];
    assert_eq!(
        counters, &expected,
        "Fractal token must enter with +1/+1 counters equal to damage dealt, got: {:?}",
        counters
    );
}

#[test]
fn trigger_combat_damage_create_treasure_and_manifest_that_players_library() {
    let def = parse_trigger_line(
            "Whenever one or more creatures you control deal combat damage to a player, create a Treasure token and manifest the top card of that player's library.",
            "Orochi Soul-Reaver",
        );
    assert_eq!(def.mode, TriggerMode::DamageDoneOnceByController);

    let execute = def.execute.as_ref().expect("trigger should have execute");
    assert!(matches!(*execute.effect, Effect::Token { .. }));

    let sub = execute
        .sub_ability
        .as_ref()
        .expect("manifest should be chained after Treasure creation");
    assert!(
        matches!(
            *sub.effect,
            Effect::Manifest {
                target: TargetFilter::TriggeringPlayer,
                count: QuantityExpr::Fixed { value: 1 },
                ..
            }
        ),
        "expected Manifest {{ TriggeringPlayer, count: 1 }}, got: {:?}",
        sub.effect
    );
}

// --- CR 120.1 + CR 120.1a: "or battle" damage-recipient triggers ---

#[test]
fn archpriest_of_shadows_player_or_battle_trigger() {
    // CR 120.1a: "deals combat damage to a player or battle" must include
    // Battle in the trigger's valid_target (Archpriest of Shadows class).
    let def = parse_trigger_line(
            "Whenever Archpriest of Shadows deals combat damage to a player or battle, return target creature card from your graveyard to the battlefield.",
            "Archpriest of Shadows",
        );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    match &def.valid_target {
        Some(TargetFilter::Or { filters }) => {
            assert!(filters.iter().any(|f| matches!(f, TargetFilter::Player)));
            assert!(filters.iter().any(|f| matches!(
                f,
                TargetFilter::Typed(TypedFilter { type_filters, .. })
                if type_filters.contains(&TypeFilter::Battle)
            )));
        }
        other => panic!("expected Or {{ Player, Battle }}, got {other:?}"),
    }
}

#[test]
fn bloodfeather_phoenix_opponent_or_battle_trigger() {
    // CR 120.1a: "deals damage to an opponent or battle" — opponent disjunction.
    let def = parse_trigger_line(
            "Whenever an instant or sorcery spell you control deals damage to an opponent or battle, you may pay {R}. If you do, return Bloodfeather Phoenix from your graveyard to the battlefield.",
            "Bloodfeather Phoenix",
        );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    match &def.valid_target {
        Some(TargetFilter::Or { filters }) => {
            assert!(filters.iter().any(|f| matches!(
                f,
                TargetFilter::Typed(TypedFilter {
                    controller: Some(ControllerRef::Opponent),
                    ..
                })
            )));
            assert!(filters.iter().any(|f| matches!(
                f,
                TargetFilter::Typed(TypedFilter { type_filters, .. })
                if type_filters.contains(&TypeFilter::Battle)
            )));
        }
        other => panic!("expected Or {{ Opponent, Battle }}, got {other:?}"),
    }
}

#[test]
fn farseeing_flockmate_player_planeswalker_or_battle_trigger() {
    // CR 120.1a: Three-way "a player, planeswalker, or battle" disjunction.
    let def = parse_trigger_line(
            "Whenever Farseeing Flockmate deals combat damage to a player, planeswalker, or battle, surveil 1.",
            "Farseeing Flockmate",
        );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    match &def.valid_target {
        Some(TargetFilter::Or { filters }) => {
            assert_eq!(filters.len(), 3);
            assert!(filters.iter().any(|f| matches!(f, TargetFilter::Player)));
            assert!(filters.iter().any(|f| matches!(
                f,
                TargetFilter::Typed(TypedFilter { type_filters, .. })
                if type_filters.contains(&TypeFilter::Planeswalker)
            )));
            assert!(filters.iter().any(|f| matches!(
                f,
                TargetFilter::Typed(TypedFilter { type_filters, .. })
                if type_filters.contains(&TypeFilter::Battle)
            )));
        }
        other => panic!("expected Or {{ Player, Planeswalker, Battle }}, got {other:?}"),
    }
}

#[test]
fn zurgo_and_ojutai_one_or_more_player_or_battle_trigger() {
    // CR 120.1a + CR 603.10a: "one or more dragons you control deal combat
    // damage to a player or battle" — batched damage with battle recipient.
    let def = parse_trigger_line(
            "Whenever one or more Dragons you control deal combat damage to a player or battle, look at the top three cards of your library. Put one of them into your hand and the rest on the bottom of your library in a random order.",
            "Zurgo and Ojutai",
        );
    assert_eq!(def.mode, TriggerMode::DamageDoneOnceByController);
    assert!(def.batched);
    assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    match &def.valid_target {
        Some(TargetFilter::Or { filters }) => {
            assert!(filters.iter().any(|f| matches!(f, TargetFilter::Player)));
            assert!(filters.iter().any(|f| matches!(
                f,
                TargetFilter::Typed(TypedFilter { type_filters, .. })
                if type_filters.contains(&TypeFilter::Battle)
            )));
        }
        other => panic!("expected Or {{ Player, Battle }}, got {other:?}"),
    }
}

/// CR 406.3 + CR 701.16a + CR 400.7i: Gonti, Canny Acquisitor. "look at the
/// top card of that player's library, then exile it face down" must rewrite
/// the private `Dig` look step into a face-down `ExileTop` (issue #1316: the
/// card was looked at but never left the library), and the follow-on "You may
/// play that card for as long as it remains exiled, and mana of any type can
/// be spent" grant must bind to the exiled card via the tracked set.
#[test]
fn trigger_combat_damage_look_then_exile_face_down_grants_impulse_play() {
    use crate::types::identifiers::TrackedSetId;

    let def = parse_trigger_line(
            "Whenever one or more creatures you control deal combat damage to a player, look at the top card of that player's library, then exile it face down. You may play that card for as long as it remains exiled, and mana of any type can be spent to cast that spell.",
            "Gonti, Canny Acquisitor",
        );
    assert_eq!(def.mode, TriggerMode::DamageDoneOnceByController);
    assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);

    let execute = def.execute.as_ref().expect("trigger should have execute");
    assert!(
        matches!(
            *execute.effect,
            Effect::ExileTop {
                player: TargetFilter::TriggeringPlayer,
                count: QuantityExpr::Fixed { value: 1 },
                face_down: true,
            }
        ),
        "expected face-down ExileTop from the triggering player's library, got: {:?}",
        execute.effect
    );

    let grant = execute
        .sub_ability
        .as_ref()
        .expect("the impulse-play grant must chain after the exile");
    assert!(
        matches!(
            &*grant.effect,
            Effect::GrantCastingPermission {
                permission: CastingPermission::PlayFromExile {
                    duration: Duration::Permanent,
                    mana_spend_permission: Some(ManaSpendPermission::AnyTypeOrColor),
                    ..
                },
                target: TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
                ..
            }
        ),
        "expected PlayFromExile grant bound to the tracked exiled card, got: {:?}",
        grant.effect
    );
}

#[test]
fn trigger_upkeep() {
    let def = parse_trigger_line(
        "At the beginning of your upkeep, look at the top card of your library.",
        "Delver of Secrets",
    );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::Upkeep));
}

#[test]
fn trigger_optional_you_may() {
    let def = parse_trigger_line(
        "When this creature enters, you may draw a card.",
        "Some Card",
    );
    assert!(def.optional);
}

#[test]
fn trigger_attacks() {
    let def = parse_trigger_line(
        "Whenever Goblin Guide attacks, defending player reveals the top card of their library.",
        "Goblin Guide",
    );
    assert_eq!(def.mode, TriggerMode::Attacks);
}

/// CR 107.1 + CR 701.17a: Attack triggers can use the shared fractional
/// quantity parser to mill the targeted player's library.
#[test]
fn trigger_attacks_target_player_mills_half_their_library_rounded_up() {
    use crate::types::ability::{RoundingMode, ZoneRef};

    let def = parse_trigger_line(
        "Whenever this creature attacks, target player mills half their library, rounded up.",
        "Fleet Swallower",
    );
    assert_eq!(def.mode, TriggerMode::Attacks);
    assert_eq!(def.valid_target, Some(TargetFilter::Player));

    let execute = def.execute.as_ref().expect("trigger should have effect");
    match execute.effect.as_ref() {
        Effect::Mill {
            count,
            target,
            destination,
        } => {
            assert_eq!(*target, TargetFilter::Player);
            assert_eq!(*destination, Zone::Graveyard);
            assert_eq!(
                *count,
                QuantityExpr::DivideRounded {
                    inner: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::TargetZoneCardCount {
                            zone: ZoneRef::Library,
                        },
                    }),
                    divisor: 2,
                    rounding: RoundingMode::Up,
                },
            );
        }
        other => panic!("Expected Mill, got {other:?}"),
    }
}

/// CR 107.1 + CR 701.17a: Sibling coverage — "rounded down" variant
/// exercises the same fractional quantity parser with the opposite rounding
/// mode, ensuring both arms of the `RoundingMode` axis are verified.
#[test]
fn trigger_attacks_target_player_mills_half_their_library_rounded_down() {
    use crate::types::ability::{RoundingMode, ZoneRef};

    let def = parse_trigger_line(
        "Whenever this creature attacks, target player mills half their library, rounded down.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::Attacks);

    let execute = def.execute.as_ref().expect("trigger should have effect");
    match execute.effect.as_ref() {
        Effect::Mill {
            count,
            target,
            destination,
        } => {
            assert_eq!(*target, TargetFilter::Player);
            assert_eq!(*destination, Zone::Graveyard);
            assert_eq!(
                *count,
                QuantityExpr::DivideRounded {
                    inner: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::TargetZoneCardCount {
                            zone: ZoneRef::Library,
                        },
                    }),
                    divisor: 2,
                    rounding: RoundingMode::Down,
                },
            );
        }
        other => panic!("Expected Mill, got {other:?}"),
    }
}

/// Issue #1354 — full Coveted Jewel Oracle must retain the unblocked-attack trigger.
#[test]
fn coveted_jewel_full_oracle_parses_you_attack_unblocked_trigger() {
    let parsed = crate::parser::oracle::parse_oracle_text(
        "When this artifact enters, draw three cards.\n\
             {T}: Add three mana of any one color.\n\
             Whenever one or more creatures an opponent controls attack you and aren't blocked, \
             that player draws three cards and gains control of this artifact. Untap it.",
        "Coveted Jewel",
        &[],
        &["Artifact".to_string()],
        &[],
    );
    assert!(
        parsed
            .triggers
            .iter()
            .any(|trigger| { matches!(trigger.mode, TriggerMode::YouAttackUnblocked) }),
        "expected YouAttackUnblocked among {:?}",
        parsed
            .triggers
            .iter()
            .map(|trigger| format!("{:?}", trigger.mode))
            .collect::<Vec<_>>()
    );
}

/// Issue #1354 — Coveted Jewel: plural "aren't blocked" on a batched opponent-
/// creature attack must fire after blockers are declared, not at attack declaration.
#[test]
fn trigger_opponent_creatures_attack_you_and_arent_blocked_uses_you_attack_unblocked() {
    let def = parse_trigger_line(
            "Whenever one or more creatures an opponent controls attack you and aren't blocked, that player draws three cards and gains control of this artifact. Untap it.",
            "Coveted Jewel",
        );
    assert_eq!(def.mode, TriggerMode::YouAttackUnblocked);
    assert!(def.batched);
    assert_eq!(def.attack_target_filter, Some(AttackTargetFilter::Player));
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    let execute = def.execute.as_ref().expect("trigger should have effect");
    fn effect_chain_contains_give_control_self(ability: &AbilityDefinition) -> bool {
        match ability.effect.as_ref() {
            Effect::GiveControl {
                target: TargetFilter::SelfRef,
                ..
            } => true,
            _ => ability
                .sub_ability
                .as_ref()
                .is_some_and(|sub| effect_chain_contains_give_control_self(sub)),
        }
    }
    fn effect_chain_contains_untap_self(ability: &AbilityDefinition) -> bool {
        match ability.effect.as_ref() {
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Untap,
            } => true,
            _ => ability
                .sub_ability
                .as_ref()
                .is_some_and(|sub| effect_chain_contains_untap_self(sub)),
        }
    }
    assert!(
        effect_chain_contains_give_control_self(execute),
        "expected GiveControl of SelfRef in effect chain, got {:?}",
        execute.effect
    );
    assert!(
        effect_chain_contains_untap_self(execute),
        "expected Untap of SelfRef in effect chain, got {:?}",
        execute.effect
    );
}

#[test]
fn trigger_attacks_and_isnt_blocked_uses_unblocked_mode_and_combat_duration() {
    let def = parse_trigger_line(
        "Whenever Murk Dwellers attacks and isn't blocked, it gets +2/+0 until end of combat.",
        "Murk Dwellers",
    );
    assert_eq!(def.mode, TriggerMode::AttackerUnblocked);
    let execute = def.execute.as_ref().expect("trigger should have effect");
    assert_eq!(execute.duration, Some(Duration::UntilEndOfCombat));
    assert!(
        matches!(
            execute.effect.as_ref(),
            Effect::Pump {
                power: PtValue::Fixed(2),
                toughness: PtValue::Fixed(0),
                ..
            }
        ),
        "expected self pump, got {:?}",
        execute.effect
    );
}

#[test]
fn trigger_attacks_you_or_planeswalker_you_control() {
    let def = parse_trigger_line(
            "Whenever a creature attacks you or a planeswalker you control, that creature's controller loses 1 life.",
            "Marchesa's Decree",
        );
    assert_eq!(def.mode, TriggerMode::Attacks);
    assert_eq!(
        def.attack_target_filter,
        Some(AttackTargetFilter::PlayerOrPlaneswalker)
    );
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn opponent_attacks_that_player_library_binds_to_triggering_player() {
    let def = parse_trigger_line(
            "Whenever an opponent attacks you and/or one or more planeswalkers you control, exile the top card of that player's library.",
            "Cunning Rhetoric",
        );
    assert_eq!(def.mode, TriggerMode::AttackersDeclared);
    assert_eq!(
        def.attack_target_filter,
        Some(AttackTargetFilter::PlayerOrPlaneswalker)
    );
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    let execute = def.execute.as_ref().expect("trigger should have effect");
    assert!(
        matches!(
            execute.effect.as_ref(),
            Effect::ExileTop {
                player: TargetFilter::TriggeringPlayer,
                count: QuantityExpr::Fixed { value: 1 },
                face_down: false,
            }
        ),
        "expected ExileTop to bind to TriggeringPlayer, got {:?}",
        execute.effect
    );
}

#[test]
fn opponent_attacks_you_uses_attackers_declared() {
    let def = parse_trigger_line(
            "Whenever an opponent attacks you, choose target creature attacking you. Put a stun counter on that creature.",
            "Lulu, Stern Guardian",
        );
    assert_eq!(def.mode, TriggerMode::AttackersDeclared);
    assert_eq!(def.attack_target_filter, Some(AttackTargetFilter::Player));
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    let execute = def.execute.as_ref().expect("trigger should have effect");
    assert!(
        matches!(
            execute.effect.as_ref(),
            Effect::TargetOnly {
                target: TargetFilter::Typed(typed),
                ..
            } if typed.type_filters.contains(&TypeFilter::Creature)
                && typed.properties.contains(&FilterProp::Attacking {
                    defender: Some(ControllerRef::You),
                })
        ),
        "expected TargetOnly(creature attacking you), got {:?}",
        execute.effect
    );
}

/// Issue #594 — Maralen, Fae Ascendant's ETB trigger: the full Oracle
/// "Whenever Maralen or another Elf or Faerie you control enters, exile
/// the top two cards of target opponent's library." Verifies the
/// trigger-half end-to-end: mode is `ChangesZone` (ETB) with battlefield
/// destination, and the execute step lowers to `Effect::ExileTop` with
/// count = 2 (not silently dropped) and the target-opponent typed filter
/// (not the generic library-zone fallback).
///
/// CR 603.2a + CR 400.12 + CR 115.1.
#[test]
fn trigger_maralen_etb_exile_top_two_of_target_opponents_library() {
    let def = parse_trigger_line(
        "Whenever Maralen or another Elf or Faerie you control enters, \
             exile the top two cards of target opponent's library.",
        "Maralen, Fae Ascendant",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));

    let execute = def
        .execute
        .as_ref()
        .expect("trigger should have execute step");
    match execute.effect.as_ref() {
        Effect::ExileTop {
            player,
            count,
            face_down,
        } => {
            assert_eq!(
                *count,
                QuantityExpr::Fixed { value: 2 },
                "count must survive the targeted-library lowering"
            );
            assert!(!*face_down);
            assert_eq!(
                *player,
                TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
                "expected target-opponent filter, got {:?}",
                player
            );
        }
        other => panic!("Expected ExileTop, got {other:?}"),
    }
}

/// Issue #1499 — Arabella, Abandoned Doll: "Whenever Arabella attacks, it
/// deals X damage to each opponent and you gain X life, where X is the
/// number of creatures you control with power 2 or less." The attack trigger
/// must bind X to a dynamic creature count (a `QuantityRef::ObjectCount`),
/// not a `Variable("X")` placeholder that resolves to 0 at runtime.
#[test]
fn arabella_attack_trigger_binds_x_to_creature_count() {
    let def = parse_trigger_line(
        "Whenever Arabella, Abandoned Doll attacks, it deals X damage to each \
             opponent and you gain X life, where X is the number of creatures you \
             control with power 2 or less.",
        "Arabella, Abandoned Doll",
    );
    assert_eq!(def.mode, TriggerMode::Attacks);
    let execute = def
        .execute
        .as_ref()
        .expect("Arabella attack trigger must have an execute");
    match &*execute.effect {
        Effect::DamageEachPlayer { amount, .. } => assert!(
            matches!(
                amount,
                QuantityExpr::Ref {
                    qty: crate::types::ability::QuantityRef::ObjectCount { .. }
                }
            ),
            "X must bind to a dynamic creature count, got {amount:?}"
        ),
        other => panic!("expected DamageEachPlayer top effect, got {other:?}"),
    }
}

/// Issue #1585 — Pantlaza, Sun-Favored: "Whenever Pantlaza or another
/// Dinosaur you control enters, you may discover X, where X is that
/// creature's toughness. Do this only once each turn." The trigger must be
/// an ETB (`ChangesZone` -> Battlefield), constrained to once per turn, and
/// its execute must be a `Discover` whose limit binds to the *entering*
/// creature's toughness - NOT a `Variable("X")` placeholder, which resolves
/// to 0 at runtime and makes discover a silent no-op ("did not discover").
#[test]
fn trigger_pantlaza_etb_discover_x_is_entering_creature_toughness() {
    let def = parse_trigger_line(
        "Whenever Pantlaza or another Dinosaur you control enters, you may \
             discover X, where X is that creature's toughness. Do this only \
             once each turn. (Exile cards from the top of your library until \
             you exile a nonland card with that mana value or less. Cast it \
             without paying its mana cost or put it into your hand. Put the \
             rest on the bottom in a random order.)",
        "Pantlaza, Sun-Favored",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    assert_eq!(
        def.constraint,
        Some(crate::types::ability::TriggerConstraint::OncePerTurn),
        "\"Do this only once each turn\" must map to OncePerTurn",
    );

    let execute = def
        .execute
        .as_ref()
        .expect("trigger should have execute step");
    match execute.effect.as_ref() {
        Effect::Discover {
            mana_value_limit, ..
        } => {
            assert!(
                matches!(
                    mana_value_limit,
                    QuantityExpr::Ref {
                        qty: crate::types::ability::QuantityRef::Toughness { .. }
                    }
                ),
                "discover X must bind to the entering creature's toughness, \
                     not a Variable placeholder (got {mana_value_limit:?})",
            );
        }
        other => panic!("Expected Discover, got {other:?}"),
    }
}

#[test]
fn trigger_battalion() {
    let def = parse_trigger_line(
            "Whenever Boros Elite and at least two other creatures attack, Boros Elite gets +2/+2 until end of turn.",
            "Boros Elite",
        );
    assert_eq!(def.mode, TriggerMode::Attacks);
    assert!(def.condition.is_some());
    if let Some(TriggerCondition::MinCoAttackers { minimum, filter }) = &def.condition {
        assert_eq!(*minimum, 2);
        assert_eq!(*filter, None);
    } else {
        panic!("Expected MinCoAttackers");
    }
}

#[test]
fn trigger_pack_tactics() {
    let def = parse_trigger_line(
            "Whenever Werewolf Pack Leader attacks, if the total power of creatures you control is 6 or greater, draw a card.",
            "Werewolf Pack Leader",
        );
    // Pack tactics is a different pattern (if-condition), not battalion
    assert_eq!(def.mode, TriggerMode::Attacks);
}

#[test]
fn trigger_exploits_a_creature() {
    let def = parse_trigger_line(
        "When Sidisi's Faithful exploits a creature, return target creature to its owner's hand.",
        "Sidisi's Faithful",
    );
    assert_eq!(def.mode, TriggerMode::Exploited);
}

#[test]
fn trigger_creature_you_control_explores() {
    let def = parse_trigger_line(
            "Whenever a creature you control explores, put a +1/+1 counter on Wildgrowth Walker and you gain 3 life.",
            "Wildgrowth Walker",
        );
    assert_eq!(def.mode, TriggerMode::Explored);
    assert!(def.valid_card.is_some());
}

#[test]
fn trigger_self_explores() {
    let def = parse_trigger_line("Whenever this creature explores, draw a card.", "Test Card");
    assert_eq!(def.mode, TriggerMode::Explored);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

#[test]
fn trigger_explores_card_quality_remains_unknown() {
    let def = parse_trigger_line(
            "Whenever a creature you control explores a land card, you may put a land card from your hand onto the battlefield tapped.",
            "Nicanzil, Current Conductor",
        );
    assert!(
        matches!(def.mode, TriggerMode::Unknown(_)),
        "explore-card-quality trigger needs event payload support, got {:?}",
        def.mode
    );
}

#[test]
fn trigger_evolves_self() {
    let def = parse_trigger_line(
            "Whenever ~ evolves, put a +1/+1 counter on each other creature you control with a +1/+1 counter on it.",
            "Renegade Krasis",
        );
    assert_eq!(def.mode, TriggerMode::Evolved);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}
#[test]
fn trigger_evolves_creature_you_control() {
    let def = parse_trigger_line(
        "Whenever a creature you control evolves, draw a card.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::Evolved);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::creature().controller(crate::types::ability::ControllerRef::You)
        ))
    );
}
#[test]
fn trigger_evolve_plural() {
    let def = parse_trigger_line(
        "Whenever one or more creatures you control evolve, draw a card.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::Evolved);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::creature().controller(crate::types::ability::ControllerRef::You)
        ))
    );
}
// --- Subject decomposition tests ---
#[test]
fn trigger_another_creature_you_control_enters() {
    let def = parse_trigger_line(
            "Whenever another creature you control enters, put a +1/+1 counter on Hinterland Sanctifier.",
            "Hinterland Sanctifier",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::creature()
                .controller(crate::types::ability::ControllerRef::You)
                .properties(vec![FilterProp::Another])
        ))
    );
}

#[test]
fn trigger_another_creature_enters_no_controller() {
    let def = parse_trigger_line(
        "Whenever another creature enters, draw a card.",
        "Some Card",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    match &def.valid_card {
        Some(TargetFilter::Typed(TypedFilter { properties, .. })) => {
            assert!(properties.contains(&FilterProp::Another));
        }
        other => panic!("Expected Typed filter with Another, got {:?}", other),
    }
}

#[test]
fn trigger_a_creature_enters() {
    let def = parse_trigger_line(
        "Whenever a creature enters, you gain 1 life.",
        "Soul Warden",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(TypedFilter::creature()))
    );
}

#[test]
fn trigger_gain_life_and_get_energy_chains_both_effects() {
    let def = parse_trigger_line(
            "Whenever another creature you control enters, you gain 1 life and get {E} (an energy counter).",
            "Guide of Souls",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::Another])
        ))
    );

    let execute = def.execute.expect("trigger should have execute ability");
    assert!(matches!(*execute.effect, Effect::GainLife { .. }));
    let sub_ability = execute
        .sub_ability
        .expect("energy gain should be chained after life gain");
    assert_eq!(
        *sub_ability.effect,
        Effect::GainEnergy {
            amount: QuantityExpr::Fixed { value: 1 }
        }
    );
}

#[test]
fn trigger_jocasta_commander_attack_return_tapped_and_attacking() {
    use crate::parser::oracle::parse_oracle_text;

    let oracle = "Flying\n\
            Whenever your commander deals combat damage to a player, put a +1/+1 counter on Jocasta.\n\
            Whenever you attack with your commander, if this card is in your graveyard, you may return it to the battlefield tapped and attacking.";

    let parsed = parse_oracle_text(
        oracle,
        "Jocasta, Automaton Avenger",
        &[],
        &["Artifact".to_string(), "Creature".to_string()],
        &["Robot".to_string(), "Hero".to_string()],
    );
    let graveyard_return = parsed
        .triggers
        .iter()
        .find(|t| {
            t.execute
                .as_ref()
                .is_some_and(|a| matches!(a.effect.as_ref(), Effect::ChangeZone { .. }))
        })
        .expect("graveyard return trigger must exist in full pipeline");
    assert!(
        graveyard_return.trigger_zones.contains(&Zone::Graveyard),
        "graveyard intervening-if must set trigger_zones, got {:?}",
        graveyard_return.trigger_zones
    );
    assert_eq!(graveyard_return.mode, TriggerMode::YouAttack);
    match &graveyard_return.valid_card {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::IsCommander)));
        }
        other => {
            panic!("YouAttack with your commander must set IsCommander valid_card, got {other:?}")
        }
    }
    let execute = graveyard_return
        .execute
        .as_ref()
        .expect("trigger should have execute ability");
    match execute.effect.as_ref() {
        Effect::ChangeZone {
            origin,
            destination,
            enter_tapped,
            enters_attacking,
            target,
            ..
        } => {
            assert_eq!(*destination, Zone::Battlefield);
            assert_eq!(
                origin,
                &Some(Zone::Graveyard),
                "graveyard intervening-if must stamp ChangeZone origin"
            );
            assert!(
                enter_tapped.is_tapped(),
                "enter_tapped must be set, got {enter_tapped:?}"
            );
            assert!(enters_attacking, "enters_attacking must be true");
            assert!(matches!(target, TargetFilter::SelfRef));
        }
        other => panic!("expected Effect::ChangeZone, got {other:?}"),
    }
}

#[test]
fn stamp_self_return_origin_skips_nested_parent_target_return() {
    use crate::types::ability::{AbilityDefinition, AbilityKind};
    use crate::types::zones::EtbTapState;

    let mut trigger = TriggerDefinition::new(TriggerMode::YouAttack);
    trigger.condition = Some(TriggerCondition::SourceInZone {
        zone: Zone::Graveyard,
    });
    let mut head = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChangeZone {
            origin: None,
            destination: Zone::Battlefield,
            target: TargetFilter::SelfRef,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: EtbTapState::Tapped,
            enters_attacking: true,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
    );
    head.sub_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChangeZone {
            origin: None,
            destination: Zone::Hand,
            target: TargetFilter::ParentTarget,
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
    )));
    trigger.execute = Some(Box::new(head));

    stamp_self_return_origin_from_trigger_condition(&mut trigger);

    let execute = trigger.execute.as_ref().expect("execute");
    match execute.effect.as_ref() {
        Effect::ChangeZone { origin, target, .. } => {
            assert_eq!(origin, &Some(Zone::Graveyard));
            assert_eq!(*target, TargetFilter::SelfRef);
        }
        other => panic!("expected head ChangeZone, got {other:?}"),
    }
    let sub = execute.sub_ability.as_ref().expect("nested return");
    match sub.effect.as_ref() {
        Effect::ChangeZone { origin, target, .. } => {
            assert_eq!(origin, &None);
            assert_eq!(*target, TargetFilter::ParentTarget);
        }
        other => panic!("expected nested ParentTarget ChangeZone, got {other:?}"),
    }
}

#[test]
fn trigger_your_commander_enters_or_attacks() {
    let def = parse_trigger_line(
        "Whenever your commander enters or attacks, put a page counter on ~.",
        "Tome of Legends",
    );
    assert_eq!(def.mode, TriggerMode::EntersOrAttacks);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(TypedFilter {
            properties: vec![
                FilterProp::Owned {
                    controller: ControllerRef::You,
                },
                FilterProp::IsCommander,
            ],
            ..Default::default()
        }))
    );
}

/// CR 702.55c: haunt creature payoff — "~ enters or the creature it haunts dies"
/// must stay one compound trigger, not split into ETB + HauntedCreatureDies.
#[test]
fn trigger_enters_or_creature_it_haunts_dies_stays_compound() {
    let triggers = parse_trigger_lines(
        "When this creature enters or the creature it haunts dies, return target creature \
             card from your graveyard to your hand.",
        "Exhumer Thrull",
    );
    assert_eq!(
        triggers.len(),
        1,
        "haunt creature payoff must not split into two triggers: {:?}",
        triggers.iter().map(|t| &t.mode).collect::<Vec<_>>()
    );
    assert_eq!(triggers[0].mode, TriggerMode::EntersOrHauntedCreatureDies);
    assert_eq!(triggers[0].destination, Some(Zone::Battlefield));
    assert_eq!(triggers[0].valid_card, Some(TargetFilter::SelfRef));
    assert!(triggers[0]
        .execute
        .as_ref()
        .is_some_and(|a| matches!(a.effect.as_ref(), Effect::Bounce { .. })));
}

#[test]
fn trigger_cross_subject_player_casts_or_creature_attacks() {
    // Norin the Wary: "Whenever a player casts a spell or a creature attacks, exile ~,
    // then return it to the battlefield under its owner's control at the beginning of
    // the next end step."
    // This should split into two separate triggers: one for spell cast, one for attack.
    let triggers = parse_trigger_lines(
            "Whenever a player casts a spell or a creature attacks, exile ~, then return it to the battlefield under its owner's control at the beginning of the next end step.",
            "Norin the Wary",
        );

    assert_eq!(triggers.len(), 2, "should split into two triggers");

    // First trigger: "Whenever a player casts a spell"
    let spell_trigger = &triggers[0];
    assert_eq!(spell_trigger.mode, TriggerMode::SpellCast);
    // "a player casts a spell" fires for any player's spell, so valid_target is None
    assert_eq!(spell_trigger.valid_target, None);

    // Second trigger: "Whenever a creature attacks"
    let attack_trigger = &triggers[1];
    assert_eq!(attack_trigger.mode, TriggerMode::Attacks);
    assert!(matches!(
        attack_trigger.valid_card,
        Some(TargetFilter::Typed(_))
    ));
}

/// CR 508.3a: "attacks you or a planeswalker you control" is a single
/// attack-target scope (PlayerOrPlaneswalker), NOT a cross-subject compound.
/// The production path (`parse_trigger_lines`) must produce exactly one
/// trigger — not mis-split at " or ".
#[test]
fn trigger_attacks_you_or_planeswalker_not_split() {
    let triggers = parse_trigger_lines(
            "Whenever a creature attacks you or a planeswalker you control, that creature's controller loses 1 life.",
            "Revenge of Ravens",
        );
    assert_eq!(
        triggers.len(),
        1,
        "attack-target scope extension must not be split into two triggers"
    );
    assert_eq!(triggers[0].mode, TriggerMode::Attacks);
    assert_eq!(
        triggers[0].attack_target_filter,
        Some(AttackTargetFilter::PlayerOrPlaneswalker)
    );
}

#[test]
fn trigger_counter_put_on_self() {
    let def = parse_trigger_line(
        "Whenever a +1/+1 counter is put on ~, draw a card.",
        "Fathom Mage",
    );
    assert_eq!(def.mode, TriggerMode::CounterAdded);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

#[test]
fn trigger_ordinal_counter_threshold_on_self() {
    let def = parse_trigger_line(
        "When the twelfth hour counter is put on ~, draw seven cards.",
        "Midnight Clock",
    );
    assert_eq!(def.mode, TriggerMode::CounterAdded);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert_eq!(
        def.counter_filter,
        Some(crate::types::ability::CounterTriggerFilter {
            counter_type: crate::types::counter::CounterType::Generic("hour".to_string()),
            threshold: Some(12),
        })
    );
}

#[test]
fn trigger_ordinal_counter_threshold_does_not_emit_subject_fallback() {
    let mut ctx = ParseContext::default();
    let def = parse_trigger_line_with_index(
        "When the fifth plan counter is put on ~, sacrifice it.",
        "Doom Reigns Supreme",
        None,
        &mut ctx,
    );
    assert_eq!(def.mode, TriggerMode::CounterAdded);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert!(
        ctx.diagnostics.iter().all(|diagnostic| !matches!(
            diagnostic,
            OracleDiagnostic::TargetFallback { context, text, .. }
                if context == "trigger subject parse fell back to Any"
                    && text == "the fifth plan counter is put on ~"
        )),
        "ordinal counter trigger should parse before generic subject fallback, got {:?}",
        ctx.diagnostics
    );
}

#[test]
fn trigger_one_or_more_counters_on_self() {
    let def = parse_trigger_line(
        "Whenever one or more counters are put on ~, you gain 1 life.",
        "Some Card",
    );
    assert_eq!(def.mode, TriggerMode::CounterAdded);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

// --- Constraint parsing tests ---

#[test]
fn trigger_once_each_turn_constraint() {
    let def = parse_trigger_line(
            "Whenever you gain life, put a +1/+1 counter on Exemplar of Light. This ability triggers only once each turn.",
            "Exemplar of Light",
        );
    assert_eq!(def.mode, TriggerMode::LifeGained);
    assert_eq!(
        def.constraint,
        Some(crate::types::ability::TriggerConstraint::OncePerTurn)
    );
}

#[test]
fn trigger_no_constraint_by_default() {
    let def = parse_trigger_line(
        "Whenever you gain life, put a +1/+1 counter on this creature.",
        "Ajani's Pridemate",
    );
    assert_eq!(def.mode, TriggerMode::LifeGained);
    assert_eq!(def.constraint, None);
}

// CR 119.3 + CR 603.2: "Whenever you gain life" must restrict the trigger to
// the source's controller. Regression for Vito, Thorn of the Dusk Rose and
// every other "you gain life" trigger that previously fired on opponent
// life-gain because `valid_target` was None.
#[test]
fn trigger_you_gain_life_scopes_to_controller() {
    let def = parse_trigger_line(
        "Whenever you gain life, target opponent loses that much life.",
        "Vito, Thorn of the Dusk Rose",
    );
    assert_eq!(def.mode, TriggerMode::LifeGained);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn trigger_you_gain_life_pridemate_scopes_to_controller() {
    let def = parse_trigger_line(
        "Whenever you gain life, put a +1/+1 counter on this creature.",
        "Ajani's Pridemate",
    );
    assert_eq!(def.mode, TriggerMode::LifeGained);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

// Negative test: "an opponent gains life" must remain opponent-scoped, not
// pick up `Controller` from the "you gain life" fast-path.
#[test]
fn trigger_opponent_gains_life_scopes_to_opponent() {
    let def = parse_trigger_line(
        "Whenever an opponent gains life, you gain that much life.",
        "Some Card",
    );
    assert_eq!(def.mode, TriggerMode::LifeGained);
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        ))
    );
}

// Negative test: "a player gains life" (no scope qualifier) must accept any
// player. The subject-bearing handler stores the parsed subject filter, which
// for "a player" is the unscoped player filter.
#[test]
fn trigger_a_player_gains_life_unscoped() {
    let def = parse_trigger_line("Whenever a player gains life, draw a card.", "Some Card");
    assert_eq!(def.mode, TriggerMode::LifeGained);
    // Whatever filter the subject parser produces for "a player", the key
    // invariant is that it is NOT scoped to Controller (which would silently
    // restrict to the source's controller).
    assert_ne!(def.valid_target, Some(TargetFilter::Controller));
}

// CR 725.1: "become the monarch" trigger tests.
#[test]
fn trigger_you_become_the_monarch_scopes_to_controller() {
    // Custodi Lich: "Whenever you become the monarch, target player
    // sacrifices a creature of their choice."
    let def = parse_trigger_line(
        "Whenever you become the monarch, target player sacrifices a creature of their choice.",
        "Custodi Lich",
    );
    assert_eq!(def.mode, TriggerMode::BecomeMonarch);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}
#[test]
fn trigger_opponent_becomes_the_monarch() {
    // Knights of the Black Rose: "Whenever an opponent becomes the monarch,
    // if you were the monarch as the turn began, that player loses 2 life."
    let def = parse_trigger_line(
        "Whenever an opponent becomes the monarch, that player loses 2 life.",
        "Knights of the Black Rose",
    );
    assert_eq!(def.mode, TriggerMode::BecomeMonarch);
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        ))
    );
}

#[test]
fn trigger_monarch_turn_began_condition_stays_unimplemented() {
    let def = parse_trigger_line(
            "Whenever an opponent becomes the monarch, if you were the monarch as the turn began, that player loses 2 life and you gain 2 life.",
            "Knights of the Black Rose",
        );
    assert_eq!(def.mode, TriggerMode::BecomeMonarch);
    let execute = def.execute.expect("trigger should keep an explicit body");
    assert!(
        matches!(
            execute.effect.as_ref(),
            Effect::Unimplemented { name, .. }
                if name == "Unsupported monarch turn-began condition"
        ),
        "unsupported monarch turn-began guard must not parse as unconditional, got {:?}",
        execute.effect
    );
}

#[test]
fn trigger_a_player_becomes_the_monarch() {
    let def = parse_trigger_line(
        "Whenever a player becomes the monarch, draw a card.",
        "Some Card",
    );
    assert_eq!(def.mode, TriggerMode::BecomeMonarch);
    assert_eq!(def.valid_target, Some(TargetFilter::Player));
}

#[test]
fn trigger_become_the_monarch_rejects_partial_suffix() {
    let def = parse_trigger_line("Whenever you become the monarchy, draw a card.", "Test");
    assert!(matches!(def.mode, TriggerMode::Unknown(_)));
}

// CR 309.4c + CR 701.49: venture-into-dungeon trigger tests.
#[test]
fn trigger_you_venture_into_the_dungeon_scopes_to_controller() {
    let def = parse_trigger_line(
        "Whenever you venture into the dungeon, create a 5/5 black Zombie Giant creature token.",
        "Acererak, the Archlich",
    );
    assert_eq!(def.mode, TriggerMode::RoomEntered);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    let execute = def.execute.expect("venture trigger must parse an effect");
    assert!(
        !matches!(execute.effect.as_ref(), Effect::Unimplemented { .. }),
        "venture trigger body must be implemented, got {:?}",
        execute.effect
    );
}

#[test]
fn trigger_you_venture_into_the_undercity_maps_to_room_entered() {
    let def = parse_trigger_line(
        "Whenever you venture into the Undercity, draw a card.",
        "Test",
    );
    assert_eq!(def.mode, TriggerMode::RoomEntered);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

// CR 726.2: take-the-initiative trigger tests.
#[test]
fn trigger_you_take_the_initiative_scopes_to_controller() {
    let def = parse_trigger_line("Whenever you take the initiative, draw a card.", "Test");
    assert_eq!(def.mode, TriggerMode::TakesInitiative);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn trigger_a_player_takes_the_initiative() {
    let def = parse_trigger_line(
        "Whenever a player takes the initiative, each opponent loses 1 life.",
        "Test",
    );
    assert_eq!(def.mode, TriggerMode::TakesInitiative);
    assert_eq!(def.valid_target, Some(TargetFilter::Player));
}

#[test]
fn trigger_attack_with_initiative_intervening_if() {
    let def = parse_trigger_line(
        "Whenever this creature attacks, if you have the initiative, draw a card.",
        "Test",
    );
    assert_eq!(def.mode, TriggerMode::Attacks);
    assert!(matches!(
        def.condition,
        Some(TriggerCondition::IsInitiative)
    ));
}

#[test]
fn trigger_only_during_your_turn() {
    let def = parse_trigger_line(
        "Whenever a creature enters, draw a card. This ability triggers only during your turn.",
        "Some Card",
    );
    assert_eq!(
        def.constraint,
        Some(crate::types::ability::TriggerConstraint::OnlyDuringYourTurn)
    );
}

// --- Compound subject tests ---

#[test]
fn trigger_self_or_another_creature_or_artifact_you_control() {
    use crate::types::ability::{ControllerRef, TypeFilter};
    let def = parse_trigger_line(
        "Whenever Haliya or another creature or artifact you control enters, you gain 1 life.",
        "Haliya, Guided by Light",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    match &def.valid_card {
        Some(TargetFilter::Or { filters }) => {
            assert_eq!(filters.len(), 3);
            assert_eq!(filters[0], TargetFilter::SelfRef);
            // Both branches should have Another + You controller
            assert_eq!(
                filters[1],
                TargetFilter::Typed(
                    TypedFilter::creature()
                        .controller(ControllerRef::You)
                        .properties(vec![FilterProp::Another])
                )
            );
            assert_eq!(
                filters[2],
                TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Artifact)
                        .controller(ControllerRef::You)
                        .properties(vec![FilterProp::Another])
                )
            );
        }
        other => panic!("Expected Or filter with 3 branches, got {:?}", other),
    }
}

#[test]
fn normalize_legendary_short_name() {
    let result = normalize_self_refs(
        "Whenever Haliya or another creature enters",
        "Haliya, Guided by Light",
    );
    assert_eq!(result, "Whenever ~ or another creature enters");
}

/// CR 700.6: Arbaaz Mir's "Whenever ~ or another nontoken historic
/// permanent you control enters" must parse cleanly into a ChangesZone
/// trigger whose `valid_card` is an `Or { SelfRef, Typed[Permanent,
/// controller=You, [NonToken, Historic, Another]] }`. Regression for
/// the previously-Unknown trigger phrase.
#[test]
fn trigger_self_or_another_nontoken_historic_permanent_arbaaz() {
    use crate::types::ability::{FilterProp, TypeFilter};
    let def = parse_trigger_line(
            "Whenever Arbaaz Mir or another nontoken historic permanent you control enters, Arbaaz Mir deals 1 damage to each opponent and you gain 1 life.",
            "Arbaaz Mir",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    let TargetFilter::Or { ref filters } = def.valid_card.as_ref().expect("valid_card present")
    else {
        panic!("Expected Or filter, got {:?}", def.valid_card);
    };
    assert_eq!(filters.len(), 2, "expected 2-leg Or, got {filters:#?}");
    assert_eq!(filters[0], TargetFilter::SelfRef);
    let TargetFilter::Typed(ref tf) = filters[1] else {
        panic!("Expected Typed second leg, got {:?}", filters[1]);
    };
    assert_eq!(tf.controller, Some(ControllerRef::You));
    assert!(
        tf.type_filters.contains(&TypeFilter::Permanent),
        "expected Permanent in {:?}",
        tf.type_filters,
    );
    assert!(
        tf.properties.contains(&FilterProp::NonToken),
        "expected NonToken in {:?}",
        tf.properties,
    );
    assert!(
        tf.properties.contains(&FilterProp::Historic),
        "expected Historic in {:?}",
        tf.properties,
    );
    assert!(
        tf.properties.contains(&FilterProp::Another),
        "expected Another in {:?}",
        tf.properties,
    );
}

#[test]
fn trigger_first_word_short_name_enters() {
    let def = parse_trigger_line(
            "When Sharuum enters, you may return target artifact card from your graveyard to the battlefield.",
            "Sharuum the Hegemon",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert!(def.optional);
}

#[test]
fn trigger_a_prefix_card_enters() {
    let def = parse_trigger_line(
            "When Sprouting Goblin enters, search your library for a land card with a basic land type, reveal it, put it into your hand, then shuffle.",
            "A-Sprouting Goblin",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
}

#[test]
fn trigger_self_or_another_creature_enters() {
    let def = parse_trigger_line(
        "Whenever Some Card or another creature enters, draw a card.",
        "Some Card",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    match &def.valid_card {
        Some(TargetFilter::Or { filters }) => {
            assert_eq!(filters.len(), 2);
            assert_eq!(filters[0], TargetFilter::SelfRef);
            match &filters[1] {
                TargetFilter::Typed(TypedFilter { properties, .. }) => {
                    assert!(properties.contains(&FilterProp::Another));
                }
                other => panic!("Expected Typed with Another, got {:?}", other),
            }
        }
        other => panic!("Expected Or filter, got {:?}", other),
    }
}

/// CR 700.4 + CR 603.10a: Jackdaw Savior (issue #887) — "Whenever this
/// creature or another creature you control with flying dies, return another
/// target creature card with lesser mana value from your graveyard to the
/// battlefield."
///
/// The `valid_card` must be `Or[SelfRef, Typed{Creature, You,
/// [WithKeyword(Flying), Another]}]` — not `Or[SelfRef, Typed{..., [Another]}]`
/// (missing flying) or `Typed{..., [Flying, Another]}` (missing SelfRef arm).
#[test]
fn trigger_self_or_another_creature_with_flying_dies() {
    let def = parse_trigger_line(
        "Whenever this creature or another creature you control with flying dies, \
             return another target creature card with lesser mana value from your graveyard \
             to the battlefield.",
        "Jackdaw Savior",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    // CR 700.4: "dies" = leaves battlefield → graveyard.
    assert_eq!(def.origin, Some(Zone::Battlefield));
    assert_eq!(def.destination, Some(Zone::Graveyard));
    // The valid_card filter distinguishes Jackdaw Savior itself (SelfRef) from
    // other flying creatures you control (Typed + WithKeyword + Another).
    match &def.valid_card {
        Some(TargetFilter::Or { filters }) => {
            assert_eq!(filters.len(), 2, "Or must have exactly two arms");
            assert_eq!(
                filters[0],
                TargetFilter::SelfRef,
                "first arm must be SelfRef ('this creature')"
            );
            match &filters[1] {
                TargetFilter::Typed(TypedFilter {
                    type_filters,
                    controller,
                    properties,
                }) => {
                    assert!(
                        type_filters.contains(&TypeFilter::Creature),
                        "second arm must require Creature type"
                    );
                    assert_eq!(
                        *controller,
                        Some(ControllerRef::You),
                        "second arm must require 'you control'"
                    );
                    assert!(
                        properties.iter().any(|p| matches!(
                            p,
                            FilterProp::WithKeyword { value: kw }
                            if *kw == crate::types::keywords::Keyword::Flying
                        )),
                        "second arm must check WithKeyword(Flying); properties={:?}",
                        properties
                    );
                    assert!(
                        properties.contains(&FilterProp::Another),
                        "second arm must check Another (not Jackdaw itself); \
                             properties={:?}",
                        properties
                    );
                }
                other => panic!("second Or arm must be Typed filter, got {other:?}"),
            }
        }
        other => panic!(
            "valid_card must be Or[SelfRef, Typed{{Creature, You, [Flying, Another]}}], \
                 got {other:?}"
        ),
    }
}

// --- Intervening-if condition tests ---

/// CR 608.2h: Haliya, Guided by Light — "draw a card if you've gained 3 or
/// more life this turn" is a post-effect `if`. It is NOT a CR 603.4
/// intervening-`if` (which must immediately follow the trigger condition),
/// so it is re-homed onto the clause-level `execute.condition`, checked
/// once at resolution — not hoisted to `TriggerDefinition.condition`.
#[test]
fn trigger_haliya_end_step_with_life_condition() {
    let def = parse_trigger_line(
        "At the beginning of your end step, draw a card if you've gained 3 or more life this turn.",
        "Haliya, Guided by Light",
    );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::End));
    assert!(
        def.condition.is_none(),
        "a post-effect `if` is not a CR 603.4 intervening-if and must not \
             hoist to the trigger condition, got {:?}",
        def.condition,
    );
    let execute = def.execute.expect("Haliya trigger must have an execute");
    assert_eq!(
        execute.condition,
        Some(AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeGainedThisTurn {
                    player: PlayerScope::Controller,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 3 },
        }),
        "the post-effect `if` must re-home onto the clause-level condition",
    );
}

/// CR 608.2h: A post-effect `if` with no explicit count ("if you gained
/// life this turn") re-homes onto `execute.condition` as a `QuantityCheck`
/// defaulting to `>= 1`, not onto the trigger-level condition.
#[test]
fn trigger_if_gained_life_no_number() {
    let def = parse_trigger_line(
        "At the beginning of your end step, create a Blood token if you gained life this turn.",
        "Some Card",
    );
    assert!(
        def.condition.is_none(),
        "a post-effect `if` must not hoist to the trigger condition, got {:?}",
        def.condition,
    );
    let execute = def.execute.expect("trigger must have an execute");
    assert_eq!(
        execute.condition,
        Some(AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeGainedThisTurn {
                    player: PlayerScope::Controller,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        }),
        "the post-effect `if` must re-home onto the clause-level condition",
    );
}

#[test]
fn trigger_if_descended_this_turn() {
    let def = parse_trigger_line(
        "At the beginning of your end step, if you descended this turn, scry 1.",
        "Ruin-Lurker Bat",
    );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::End));
    assert_eq!(
        def.condition,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::DescendedThisTurn,
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        })
    );
    assert!(def.execute.is_some());
}

#[test]
fn trigger_if_gained_5_or_more_life() {
    let def = parse_trigger_line(
            "At the beginning of each end step, if you gained 5 or more life this turn, create a 4/4 white Angel creature token with flying and vigilance.",
            "Resplendent Angel",
        );
    assert_eq!(
        def.condition,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeGainedThisTurn {
                    player: PlayerScope::Controller,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 5 },
        })
    );
    // Regression: execute must not be None — the effect text after the condition
    // must be preserved and parsed (previously the condition clause consumed the
    // entire text, leaving execute as None).
    assert!(
            def.execute.is_some(),
            "execute must be Some — effect text after 'if you gained N or more life this turn' was dropped"
        );
}

#[test]
fn trigger_if_gained_4_or_more_life_angelic_accord() {
    // Angelic Accord: condition at start of effect text
    let def = parse_trigger_line(
            "At the beginning of each end step, if you gained 4 or more life this turn, create a 4/4 white Angel creature token with flying.",
            "Angelic Accord",
        );
    assert_eq!(
        def.condition,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeGainedThisTurn {
                    player: PlayerScope::Controller,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 4 },
        })
    );
    assert!(
        def.execute.is_some(),
        "execute must be Some — token creation effect was dropped"
    );
}

#[test]
fn trigger_if_gained_life_this_turn_no_minimum() {
    // Ocelot Pride: "if you gained life this turn" (no number)
    let def = parse_trigger_line(
            "At the beginning of your end step, if you gained life this turn, create a 1/1 white Cat creature token.",
            "Ocelot Pride",
        );
    assert_eq!(
        def.condition,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeGainedThisTurn {
                    player: PlayerScope::Controller,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        })
    );
    assert!(
        def.execute.is_some(),
        "execute must be Some — token creation effect was dropped"
    );
}

/// CR 119.3 + CR 603.4: The Book of Vile Darkness — the controller-scoped
/// "you lost N or more life this turn" intervening-if mirrors the gained
/// sibling. Previously the condition was silently dropped (left `None`).
#[test]
fn trigger_if_you_lost_2_or_more_life() {
    let def = parse_trigger_line(
            "At the beginning of your end step, if you lost 2 or more life this turn, create a 2/2 black Zombie creature token.",
            "The Book of Vile Darkness",
        );
    assert_eq!(
        def.condition,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeLostThisTurn {
                    player: PlayerScope::Controller,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 2 },
        })
    );
    assert!(
        def.execute.is_some(),
        "execute must be Some — token creation effect after the condition was dropped"
    );
}

/// "if you lost life this turn" (no minimum) → controller LifeLostThisTurn ≥ 1.
#[test]
fn trigger_if_you_lost_life_this_turn_no_minimum() {
    let def = parse_trigger_line(
        "At the beginning of your end step, if you lost life this turn, draw a card.",
        "Test Lost Life",
    );
    assert_eq!(
        def.condition,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeLostThisTurn {
                    player: PlayerScope::Controller,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        })
    );
    assert!(def.execute.is_some(), "execute must be Some");
}

#[test]
fn trigger_ocelot_pride_copy_each_entered_token_not_source() {
    let def = parse_trigger_line(
            "At the beginning of your end step, if you gained life this turn, create a 1/1 white Cat creature token. Then if you have the city's blessing, for each token you control that entered the battlefield this turn, create a token that's a copy of it.",
            "Ocelot Pride",
        );
    let create_cat = def.execute.expect("token creation execute");
    let copy_each = create_cat.sub_ability.expect("city blessing copy clause");
    assert!(matches!(
        copy_each.condition,
        Some(AbilityCondition::HasCityBlessing)
    ));
    assert!(copy_each.repeat_for.is_none());
    match &*copy_each.effect {
        Effect::CopyTokenOf {
            target,
            source_filter: Some(TargetFilter::Typed(filter)),
            ..
        } => {
            assert_eq!(*target, TargetFilter::None);
            assert_eq!(filter.controller, Some(ControllerRef::You));
            assert!(filter
                .properties
                .iter()
                .any(|prop| matches!(prop, FilterProp::Token)));
            assert!(filter
                .properties
                .iter()
                .any(|prop| matches!(prop, FilterProp::EnteredThisTurn)));
        }
        other => panic!("expected source-filtered CopyTokenOf, got {other:?}"),
    }
}

/// CR 603.4 + CR 608.2c: Wick — post-effect "if you don't control a Snail"
/// on the Token clause must re-home to `execute.condition`, not hoist to
/// `def.condition`, so the trigger still fires when a Snail is present and
/// the Otherwise PutCounter runs via `else_ability`.
#[test]
fn wick_etb_post_effect_if_stays_on_execute_with_otherwise() {
    use crate::types::ability::{AbilityCondition, Effect};

    const WICK: &str = "Whenever Wick or another Rat you control enters, create a 1/1 black Snail creature token if you don't control a Snail. Otherwise, put a +1/+1 counter on a Snail you control.";
    let def = parse_trigger_line(WICK, "Wick, the Whorled Mind");
    assert!(
        def.condition.is_none(),
        "post-effect Snail gate must not hoist to trigger condition, got {:?}",
        def.condition
    );
    let execute = def.execute.as_ref().expect("Wick ETB must have execute");
    assert!(
        matches!(
            execute.condition,
            Some(AbilityCondition::QuantityCheck { .. })
        ),
        "Token clause must carry the Snail control gate on execute.condition, got {:?}",
        execute.condition
    );
    let else_branch = execute
        .else_ability
        .as_ref()
        .expect("Otherwise must attach PutCounter as else_ability");
    assert!(
        matches!(*else_branch.effect, Effect::PutCounter { .. }),
        "else branch must PutCounter, got {:?}",
        else_branch.effect
    );
}

/// CR 603.4: A post-effect `if` ("draw a card if Y") is NOT an
/// intervening-`if` — that rule applies only to an `if` that immediately
/// follows the trigger condition ("When X, if Y, Z"). Such a trailing `if`
/// must be left in the effect text so per-clause parsing
/// (`strip_suffix_conditional`) attaches it as a clause-level
/// `AbilityCondition`, rather than being hoisted to the trigger level.
#[test]
fn extract_if_does_not_hoist_post_effect_condition() {
    let (cleaned, cond) =
        extract_if_condition("draw a card if you've gained 3 or more life this turn.");
    assert_eq!(
        cond, None,
        "a post-effect `if` is not an intervening-if (CR 603.4) and must not hoist",
    );
    assert_eq!(
        cleaned, "draw a card if you've gained 3 or more life this turn.",
        "effect text must be unchanged when the `if` is not a true intervening-if",
    );
}

/// CR 603.4: "effect. Then if Y, effect2" — the `if` is introduced by "then"
/// and scopes only to the second clause's sub_ability. `extract_if_condition`
/// must NOT hoist this to the trigger-level condition.
#[test]
fn extract_if_skips_then_if_clause() {
    let (cleaned, cond) = extract_if_condition(
            "create a 1/1 black ninja creature token. then if you control five or more ninjas, that player loses half their life, rounded up.",
        );
    assert_eq!(
        cond, None,
        "then-if conditions must not be hoisted to trigger level",
    );
    assert_eq!(
            cleaned,
            "create a 1/1 black ninja creature token. then if you control five or more ninjas, that player loses half their life, rounded up.",
            "effect text must be returned unchanged when the if belongs to a then-clause",
        );
}

/// CR 603.4: Genuine leading intervening-if ("When X, if Y, Z" — here
/// `extract_if_condition` receives only the effect portion "if Y, Z") must
/// still be hoisted even if a later "then if" appears.
#[test]
fn extract_if_preserves_leading_intervening_if_with_later_then() {
    // Only the FIRST `if ` is considered for the then-clause guard; a
    // leading intervening-if (no preceding "then") is correctly hoisted.
    let (_, cond) = extract_if_condition("if you control a creature, draw a card");
    assert!(
        cond.is_some(),
        "leading intervening-if must still be hoisted, got {cond:?}",
    );
}

/// CR 603.4 + CR 603.6a: "if it wasn't put onto the battlefield with this
/// ability" — Kodama of the East Tree anti-recursion guard. Must hoist to
/// `Not(PlacedByAbilitySource)` and excise the clause so no `Condition_If`
/// SwallowedClause warning is emitted.
#[test]
fn extract_if_condition_wasnt_put_with_this_ability() {
    let (cleaned, cond) = extract_if_condition(
            "if it wasn't put onto the battlefield with this ability, you may put a permanent card with equal or lesser mana value from your hand onto the battlefield.",
        );
    assert!(
        matches!(
            cond,
            Some(TriggerCondition::Not { ref condition }) if matches!(**condition, TriggerCondition::PlacedByAbilitySource)
        ),
        "expected Not(PlacedByAbilitySource), got {cond:?}",
    );
    assert!(
        // allow-noncombinator: test assertion verifying clause excision, not parsing dispatch
        !cleaned.contains("put onto the battlefield with this ability"),
        "intervening-if clause must be excised, got: {cleaned}",
    );
}

/// CR 603.4 + CR 603.6a: positive "if it was put onto the battlefield with
/// this ability" → `PlacedByAbilitySource` (no negation wrapper).
#[test]
fn extract_if_condition_was_put_with_this_ability() {
    let (cleaned, cond) =
        extract_if_condition("if it was put onto the battlefield with this ability, draw a card.");
    assert!(
        matches!(cond, Some(TriggerCondition::PlacedByAbilitySource)),
        "expected PlacedByAbilitySource, got {cond:?}",
    );
    assert!(
        // allow-noncombinator: test assertion verifying clause excision, not parsing dispatch
        !cleaned.contains("put onto the battlefield with this ability"),
        "intervening-if clause must be excised, got: {cleaned}",
    );
}

/// CR 603.4 + CR 601.2 + CR 404.1: "if you didn't cast it from your hand" —
/// negated zone-specific cast check (Chainer, Nightmare Adept; Phage the
/// Untouchable). Must hoist as `Not(WasCast { zone: Hand, caster=you,
/// owner=you })` — both the caster and owner-specific axes are scoped.
#[test]
fn extract_if_condition_negated_cast_from_hand() {
    let (cleaned, cond) = extract_if_condition(
        "if you didn't cast it from your hand, it gains haste until your next turn.",
    );
    assert!(
        matches!(
            &cond,
            Some(TriggerCondition::Not {
                condition
            }) if matches!(
                condition.as_ref(),
                TriggerCondition::WasCast {
                    zone: Some(Zone::Hand),
                    controller: Some(ControllerRef::You),
                    owner: Some(ControllerRef::You),
                }
            )
        ),
        "expected Not(WasCast {{ zone: Hand, you/you }}), got {cond:?}",
    );
    assert!(
        // allow-noncombinator: test assertion verifying clause excision
        !cleaned.contains("didn't cast"),
        "intervening-if clause must be excised, got: {cleaned}",
    );
}

/// CR 603.4 + CR 601.2 + CR 404.1: "if you cast it from your hand" — positive
/// zone-specific cast check hoisted as `WasCast { zone: Hand, caster=you,
/// owner=you }`. The owner-specific zone scopes both axes.
#[test]
fn extract_if_condition_cast_from_hand() {
    let (cleaned, cond) =
        extract_if_condition("if you cast it from your hand, put a counter on it.");
    assert!(
        matches!(
            cond,
            Some(TriggerCondition::WasCast {
                zone: Some(Zone::Hand),
                controller: Some(ControllerRef::You),
                owner: Some(ControllerRef::You),
            })
        ),
        "expected WasCast {{ zone: Hand, you/you }}, got {cond:?}",
    );
    assert!(
        // allow-noncombinator: test assertion verifying clause excision
        !cleaned.contains("cast it from"),
        "intervening-if clause must be excised, got: {cleaned}",
    );
}

/// CR 601.2 + CR 404.1: "if you cast it from exile" scopes the CASTER axis
/// to you but leaves the ORIGIN-ZONE-OWNER unscoped — exile is a shared
/// zone ("from exile" has no possessive), unlike hand/graveyard.
#[test]
fn extract_if_condition_cast_from_exile_owner_unscoped() {
    let (_, cond) = extract_if_condition("if you cast it from exile, draw a card.");
    assert!(
        matches!(
            cond,
            Some(TriggerCondition::WasCast {
                zone: Some(Zone::Exile),
                controller: Some(ControllerRef::You),
                owner: None,
            })
        ),
        "expected WasCast {{ zone: Exile, caster=you, owner=None }}, got {cond:?}",
    );
}

/// CR 702.112: "if it's renowned" (event subject, 702.112b) and "if ~ is
/// renowned" (source, 702.112a) map to distinct `RenownSubject` axes.
#[test]
fn extract_if_condition_renowned_disambiguates_subject() {
    let (_, it_cond) = extract_if_condition("if it's renowned, draw a card.");
    assert!(
        matches!(
            it_cond,
            Some(TriggerCondition::IsRenowned {
                subject: RenownSubject::EventSubject
            })
        ),
        "expected IsRenowned {{ EventSubject }}, got {it_cond:?}",
    );

    let (_, self_cond) = extract_if_condition("if ~ is renowned, draw a card.");
    assert!(
        matches!(
            self_cond,
            Some(TriggerCondition::IsRenowned {
                subject: RenownSubject::Source
            })
        ),
        "expected IsRenowned {{ Source }}, got {self_cond:?}",
    );
}

/// CR 603.4: Inline "then if" without a sentence boundary ("X then if Y,
/// Z") — the condition still scopes to the then-clause sub_ability and
/// must not be hoisted. Covers punctuation-free variants of the pattern.
#[test]
fn extract_if_skips_inline_then_if_clause() {
    let (_, cond) = extract_if_condition("draw a card then if you control a creature, gain 1 life");
    assert_eq!(
        cond, None,
        "inline `then if` (no sentence boundary) must not be hoisted",
    );
}

/// CR 603.4: "effect. Then, if Y, ..." (with comma after "Then") — the
/// condition still belongs to the "then" clause and must not be hoisted.
/// Regression: A Good Thing ("double your life total. Then, if you have
/// 1,000 or more life, you lose the game.").
#[test]
fn extract_if_skips_then_comma_if_clause() {
    let (_, cond) = extract_if_condition(
        "double your life total. then, if you have 1,000 or more life, you lose the game.",
    );
    assert_eq!(
        cond, None,
        "\"then, if\" conditions must not be hoisted to trigger level",
    );
}

/// CR 608.2k + CR 603.4: Full Dark Leo & Shredder parse — the if-condition
/// must attach to the sub_ability (not the trigger), the sub_ability target
/// must be TriggeringPlayer (not a new Player target), and the sub_ability
/// amount must resolve "half their life, rounded up".
#[test]
fn parse_dark_leo_trigger_structure() {
    use crate::types::ability::{AbilityCondition, Effect, RoundingMode};

    let def = parse_trigger_line(
            "Whenever ~ deals combat damage to a player, create a 1/1 black Ninja creature token. Then if you control five or more Ninjas, that player loses half their life, rounded up.",
            "Dark Leo & Shredder",
        );

    // Trigger-level condition must be None — the `if you control five or more`
    // scopes only to the sub_ability.
    assert_eq!(def.condition, None, "trigger.condition must be None");

    // Outer effect is the token creation.
    let execute = def.execute.as_ref().expect("execute must be Some");
    assert!(
        matches!(*execute.effect, Effect::Token { .. }),
        "outer execute must be Token, got {:?}",
        execute.effect,
    );

    // Sub-ability holds the conditional life-loss.
    let sub = execute
        .sub_ability
        .as_deref()
        .expect("sub_ability must be Some");
    // Sub-ability condition is the Ninja count check.
    assert!(
        matches!(
            &sub.condition,
            Some(AbilityCondition::QuantityCheck {
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 5 },
                ..
            })
        ),
        "sub_ability.condition must be QuantityCheck ≥ 5, got {:?}",
        sub.condition,
    );
    // Sub-ability effect is LoseLife targeting TriggeringPlayer with DivideRounded amount.
    match &*sub.effect {
        Effect::LoseLife { amount, target } => {
            assert_eq!(
                target.as_ref(),
                Some(&TargetFilter::TriggeringPlayer),
                "sub_ability LoseLife.target must be TriggeringPlayer",
            );
            assert!(
                matches!(
                    amount,
                    QuantityExpr::DivideRounded {
                        rounding: RoundingMode::Up,
                        ..
                    }
                ),
                "amount must be DivideRounded(Up), got {amount:?}",
            );
        }
        other => panic!("sub_ability effect must be LoseLife, got {other:?}"),
    }
}

/// CR 104.3e + CR 119 + CR 603.4 + CR 603.7c + CR 603.12: Ezio Auditore
/// da Firenze — "Whenever ~ deals combat damage to a player, you may pay
/// {W}{U}{B}{R}{G} if that player has 10 or less life. When you do, that
/// player loses the game."
///
/// Three-defect regression for issue #1962:
///
/// 1. The intervening-if "if that player has 10 or less life" (CR 603.4)
///    used to be dropped — the trigger fired unconditionally. The new
///    `parse_life_predicate` combinator in `oracle_nom::condition` must
///    lift this into `TriggerCondition::QuantityComparison` on
///    `LifeTotal[ScopedPlayer]`.
/// 2. "That player loses the game" used to lower to a bare
///    `Effect::LoseTheGame` with no target — the resolver routed
///    elimination to the ability controller (Ezio's controller), so the
///    Ezio player eliminated *themselves*. The new
///    `Effect::LoseTheGame.target` field must be
///    `Some(TargetFilter::TriggeringPlayer)` (CR 603.7c — "that player"
///    anaphora binds to the player named by the damage event).
/// 3. The reflexive "When you do" gate (CR 603.12) on the directed-loss
///    sub-ability must be preserved so the loss only fires after the
///    mana payment occurs.
#[test]
fn parse_ezio_damage_trigger_full_structure() {
    use crate::types::ability::{
        AbilityCondition, AbilityCost, Effect, PlayerScope, QuantityExpr, QuantityRef,
    };
    use crate::types::mana::{ManaCost, ManaCostShard};

    let def = parse_trigger_line(
            "Whenever ~ deals combat damage to a player, if that player has 10 or less life, you may pay {W}{U}{B}{R}{G}. When you do, that player loses the game.",
            "Ezio Auditore da Firenze",
        );

    // (a) Mode + damage kind + valid_target — CR 120.3 + CR 603.7c.
    assert!(
        matches!(def.mode, TriggerMode::DamageDone),
        "mode must be DamageDone, got {:?}",
        def.mode,
    );
    assert!(
        matches!(def.damage_kind, DamageKindFilter::CombatOnly),
        "damage_kind must be CombatOnly, got {:?}",
        def.damage_kind,
    );
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Player),
        "valid_target must be Player (the recipient of combat damage)",
    );

    // (b) Intervening-if condition — CR 603.4 + CR 119: LifeTotal[ScopedPlayer] LE 10.
    assert_eq!(
        def.condition,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal {
                    player: PlayerScope::ScopedPlayer,
                },
            },
            comparator: Comparator::LE,
            rhs: QuantityExpr::Fixed { value: 10 },
        }),
        "intervening-if must lift to QuantityComparison(LifeTotal[ScopedPlayer] LE 10), got {:?}",
        def.condition,
    );

    // Outer execute is the "you may pay {WUBRG}" cost.
    let execute = def.execute.as_ref().expect("execute must be Some");

    // (c) Optional flag — the "you may" prefix on the cost (CR 609.3).
    assert!(
        execute.optional,
        "execute.optional must be true (the 'you may pay' wording)",
    );

    // (d) Cost effect — PayCost { Mana { WUBRG }, payer: Controller }.
    match &*execute.effect {
        Effect::PayCost { cost, payer, .. } => {
            match cost {
                AbilityCost::Mana {
                    cost: ManaCost::Cost { shards, generic },
                } => {
                    assert_eq!(*generic, 0, "WUBRG cost has no generic component");
                    let expected = [
                        ManaCostShard::White,
                        ManaCostShard::Blue,
                        ManaCostShard::Black,
                        ManaCostShard::Red,
                        ManaCostShard::Green,
                    ];
                    for shard in expected {
                        assert!(
                            shards.contains(&shard),
                            "cost shards must include {shard:?}, got {shards:?}",
                        );
                    }
                    assert_eq!(
                        shards.len(),
                        5,
                        "cost must be exactly WUBRG (5 shards), got {shards:?}",
                    );
                }
                other => panic!("PayCost.cost must be Mana(WUBRG), got {other:?}"),
            }
            assert_eq!(
                *payer,
                TargetFilter::Controller,
                "PayCost.payer must be Controller (the trigger controller pays)",
            );
        }
        other => panic!("execute.effect must be PayCost, got {other:?}"),
    }

    // (e) Reflexive sub_ability: "When you do, that player loses the game."
    let sub = execute
        .sub_ability
        .as_deref()
        .expect("sub_ability must be Some — the 'When you do' clause");
    assert_eq!(
        sub.condition,
        Some(AbilityCondition::WhenYouDo),
        "sub_ability.condition must be WhenYouDo (CR 603.12), got {:?}",
        sub.condition,
    );
    assert!(
        matches!(
            &*sub.effect,
            Effect::LoseTheGame { target: Some(f) } if *f == TargetFilter::TriggeringPlayer
        ),
        "sub_ability.effect must be LoseTheGame {{ target: Some(TriggeringPlayer) }}, got {:?}",
        sub.effect,
    );
}

/// CR 104.3e + CR 119 + CR 603.4 + CR 603.7c + CR 603.12: Ezio Auditore
/// da Firenze — VERBATIM printed Oracle text (post-effect `if` form):
/// "Whenever ~ deals combat damage to a player, you may pay
/// {W}{U}{B}{R}{G} if that player has 10 or less life. When you do,
/// that player loses the game."
///
/// The companion test `parse_ezio_damage_trigger_full_structure` uses
/// the *normalized* leading-`if` form ("...if that player has 10 or
/// less life, you may pay...") which hoists the predicate to
/// `def.condition` (CR 603.4 intervening-if, detection-time gate). The
/// verbatim card text uses the *post-effect* `if` form, which re-homes
/// the predicate onto `execute.condition` (CR 608.2c, resolution-time
/// gate via `strip_suffix_conditional` →
/// `try_nom_condition_as_ability_condition`) instead.
///
/// Issue #1962 hardening (TEST-ONLY): the real printed card text was
/// previously untested — a regression in the post-effect re-homer would
/// silently strand the life predicate (allowing the loss to fire at
/// any life total), but the normalized-form regression test would
/// continue to pass. This test locks the load-bearing invariant that
/// the life-total gate exists *somewhere* on the lowered ability tree,
/// regardless of which path the parser uses to lift it.
#[test]
fn parse_ezio_damage_trigger_verbatim_oracle_text() {
    use crate::types::ability::{
        AbilityCondition, AbilityCost, Effect, PlayerScope, QuantityExpr, QuantityRef,
    };
    use crate::types::mana::{ManaCost, ManaCostShard};

    let def = parse_trigger_line(
            "Whenever ~ deals combat damage to a player, you may pay {W}{U}{B}{R}{G} if that player has 10 or less life. When you do, that player loses the game.",
            "Ezio Auditore da Firenze",
        );

    // (a) Mode + damage kind + valid_target — CR 120.3 + CR 603.7c.
    // These are unchanged from the normalized form: the trigger shape
    // itself doesn't depend on which side of the comma the `if` clause
    // lives on.
    assert!(
        matches!(def.mode, TriggerMode::DamageDone),
        "mode must be DamageDone, got {:?}",
        def.mode,
    );
    assert!(
        matches!(def.damage_kind, DamageKindFilter::CombatOnly),
        "damage_kind must be CombatOnly, got {:?}",
        def.damage_kind,
    );
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Player),
        "valid_target must be Player (the recipient of combat damage)",
    );

    let execute = def.execute.as_ref().expect("execute must be Some");

    // (b) Optional flag — the "you may pay" wording (CR 609.3).
    assert!(
        execute.optional,
        "execute.optional must be true (the 'you may pay' wording)",
    );

    // (c) Cost effect — PayCost { Mana { WUBRG }, payer: Controller }.
    // The cost shape must be identical to the normalized form.
    match &*execute.effect {
        Effect::PayCost { cost, payer, .. } => {
            match cost {
                AbilityCost::Mana {
                    cost: ManaCost::Cost { shards, generic },
                } => {
                    assert_eq!(*generic, 0, "WUBRG cost has no generic component");
                    let expected = [
                        ManaCostShard::White,
                        ManaCostShard::Blue,
                        ManaCostShard::Black,
                        ManaCostShard::Red,
                        ManaCostShard::Green,
                    ];
                    for shard in expected {
                        assert!(
                            shards.contains(&shard),
                            "cost shards must include {shard:?}, got {shards:?}",
                        );
                    }
                    assert_eq!(
                        shards.len(),
                        5,
                        "cost must be exactly WUBRG (5 shards), got {shards:?}",
                    );
                }
                other => panic!("PayCost.cost must be Mana(WUBRG), got {other:?}"),
            }
            assert_eq!(
                *payer,
                TargetFilter::Controller,
                "PayCost.payer must be Controller (the trigger controller pays)",
            );
        }
        other => panic!("execute.effect must be PayCost, got {other:?}"),
    }

    // (d) THE KEY INVARIANT: the life-total predicate must exist
    // *somewhere* on the lowered ability tree. The post-effect `if`
    // form lifts it to `execute.condition` (CR 608.2c), not
    // `def.condition` (CR 603.4) — but a regression in either path
    // would drop the gate entirely. Accept whichever path the parser
    // actually uses, but assert at least one is populated with the
    // correct shape.
    let expected_predicate = (
        QuantityExpr::Ref {
            qty: QuantityRef::LifeTotal {
                player: PlayerScope::ScopedPlayer,
            },
        },
        Comparator::LE,
        QuantityExpr::Fixed { value: 10 },
    );
    let trigger_condition_matches = matches!(
        &def.condition,
        Some(TriggerCondition::QuantityComparison { lhs, comparator, rhs })
            if *lhs == expected_predicate.0
                && *comparator == expected_predicate.1
                && *rhs == expected_predicate.2
    );
    let execute_condition_matches = matches!(
        &execute.condition,
        Some(AbilityCondition::QuantityCheck { lhs, comparator, rhs })
            if *lhs == expected_predicate.0
                && *comparator == expected_predicate.1
                && *rhs == expected_predicate.2
    );
    assert!(
        trigger_condition_matches || execute_condition_matches,
        "life-total gate (LifeTotal[ScopedPlayer] LE 10) must be present on either \
             def.condition (CR 603.4 intervening-if) or execute.condition (CR 608.2c \
             post-effect re-homer); got def.condition={:?}, execute.condition={:?}",
        def.condition,
        execute.condition,
    );

    // (e) Reflexive sub_ability: "When you do, that player loses the
    // game." This is independent of which side of the comma the `if`
    // appears on — the WhenYouDo gate + directed LoseTheGame target
    // must always be wired through.
    let sub = execute
        .sub_ability
        .as_deref()
        .expect("sub_ability must be Some — the 'When you do' clause");
    assert_eq!(
        sub.condition,
        Some(AbilityCondition::WhenYouDo),
        "sub_ability.condition must be WhenYouDo (CR 603.12), got {:?}",
        sub.condition,
    );
    assert!(
        matches!(
            &*sub.effect,
            Effect::LoseTheGame { target: Some(f) } if *f == TargetFilter::TriggeringPlayer
        ),
        "sub_ability.effect must be LoseTheGame {{ target: Some(TriggeringPlayer) }}, got {:?}",
        sub.effect,
    );
}

/// CR 603.7c + CR 120.3 + CR 119.3: Unstoppable Slasher — "Whenever this
/// creature deals combat damage to a player, they lose half their life,
/// rounded up." is an event-bound (non-targeted) trigger per CR 603.6f.
/// "they" must resolve to `TriggeringPlayer` (the damaged player), and the
/// half-life amount must read `PlayerScope::ScopedPlayer`, NOT the
/// targeting `PlayerScope::Target` (which has no chosen target on an
/// event-bound trigger and resolves to 0 — the reported silent no-op).
#[test]
fn parse_unstoppable_slasher_combat_damage_half_life() {
    use crate::types::ability::{Effect, PlayerScope, QuantityExpr, QuantityRef, RoundingMode};

    let def = parse_trigger_line(
            "Whenever this creature deals combat damage to a player, they lose half their life, rounded up.",
            "Unstoppable Slasher",
        );

    let execute = def.execute.as_ref().expect("execute must be Some");
    match &*execute.effect {
        Effect::LoseLife { amount, target } => {
            assert_eq!(
                target.as_ref(),
                Some(&TargetFilter::TriggeringPlayer),
                "LoseLife.target must be TriggeringPlayer (the damaged player), not ParentTarget",
            );
            match amount {
                QuantityExpr::DivideRounded {
                    inner,
                    divisor,
                    rounding,
                } => {
                    assert_eq!(*divisor, 2, "half ⇒ divisor 2");
                    assert_eq!(*rounding, RoundingMode::Up, "rounded up");
                    assert_eq!(
                            **inner,
                            QuantityExpr::Ref {
                                qty: QuantityRef::LifeTotal {
                                    player: PlayerScope::ScopedPlayer,
                                },
                            },
                            "inner amount must read the event player's life (ScopedPlayer), got {inner:?}",
                        );
                }
                other => panic!("amount must be DivideRounded, got {other:?}"),
            }
        }
        other => panic!("effect must be LoseLife, got {other:?}"),
    }
}

/// CR 603.4 + CR 608.2c + CR 119.3 + CR 107.1a: Cecil, Dark Knight —
/// damage-done trigger with a "Then if your life total is less than or
/// equal to half your starting life total, untap ~ and transform it"
/// sub-ability gate. The "Then if" gate must NOT be hoisted as a
/// trigger-level intervening-if; it scopes to the per-clause Untap
/// sub_ability (with Transform chained sequentially). The condition is a
/// `QuantityCheck` comparing `LifeTotal{Controller}` to
/// `DivideRounded(StartingLifeTotal, 2, Down)` — fractional rounding
/// follows the engine convention when Oracle text does not specify
/// direction (CR 107.1a). The Untap clause is a `SequentialSibling` after
/// the life-loss instruction, and Transform is a `ContinuationStep` under
/// Untap, so the QuantityCheck on Untap gates both actions in the "then if"
/// clause.
#[test]
fn parse_cecil_dark_knight_then_if_life_threshold_gate_structure() {
    use crate::types::ability::{AbilityCondition, Effect, RoundingMode, SubAbilityLink};

    let def = parse_trigger_line(
            "Whenever ~ deals damage, you lose that much life. Then if your life total is less than or equal to half your starting life total, untap ~ and transform it.",
            "Cecil, Dark Knight",
        );

    // CR 603.4 — "Then if" attaches as per-clause QuantityCheck, NOT
    // trigger-level intervening-if. The gate scopes only to the
    // sub_ability chain.
    assert_eq!(
            def.condition, None,
            "trigger.condition must be None (the 'Then if' gate scopes to sub_ability, not the trigger as a whole)",
        );

    let execute = def.execute.as_ref().expect("execute must be Some");

    // Outer effect: LoseLife { amount: Ref(EventContextAmount), target: Controller }.
    // CR 119.3 — life-loss adjusts the player's life total.
    match &*execute.effect {
        Effect::LoseLife { amount, target } => {
            assert_eq!(
                    *amount,
                    QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount,
                    },
                    "LoseLife.amount must be Ref(EventContextAmount) — 'you lose that much life' tracks the damage amount",
                );
            assert_eq!(
                *target,
                Some(TargetFilter::Controller),
                "LoseLife.target must be Controller — 'you' is the trigger source's controller",
            );
        }
        other => panic!("outer execute effect must be LoseLife, got {other:?}"),
    }

    // First sub_ability: Untap { target: SelfRef } with QuantityCheck.
    // CR 608.2c — per-clause QuantityCheck condition.
    // CR 119.3 + CR 107.1a — LifeTotal{Controller} compared to half StartingLifeTotal rounded Down.
    let untap_sub = execute
        .sub_ability
        .as_deref()
        .expect("first sub_ability (Untap) must be Some");
    match &untap_sub.condition {
            Some(AbilityCondition::QuantityCheck {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::LifeTotal {
                                player: PlayerScope::Controller,
                            },
                    },
                comparator: Comparator::LE,
                rhs:
                    QuantityExpr::DivideRounded {
                        inner,
                        divisor: 2,
                        rounding: RoundingMode::Down,
                    },
            }) => {
                assert_eq!(
                    **inner,
                    QuantityExpr::Ref {
                        qty: QuantityRef::StartingLifeTotal,
                    },
                    "DivideRounded.inner must be Ref(StartingLifeTotal), got {inner:?}",
                );
            }
            other => panic!(
                "Untap sub_ability.condition must be QuantityCheck LifeTotal{{Controller}} LE DivideRounded(StartingLifeTotal, 2, Down), got {other:?}",
            ),
        }
    match &*untap_sub.effect {
        Effect::SetTapState {
            target,
            scope: EffectScope::Single,
            state: TapStateChange::Untap,
        } => {
            assert_eq!(
                *target,
                TargetFilter::SelfRef,
                "Untap.target must be SelfRef — 'untap ~' refers to the trigger source",
            );
        }
        other => panic!("first sub_ability effect must be Untap, got {other:?}"),
    }
    assert_eq!(
            untap_sub.sub_link,
            SubAbilityLink::SequentialSibling,
            "Untap sub_link must be SequentialSibling (independent following instruction after LoseLife)",
        );

    // Nested sub_ability: Transform { target: ParentTarget } with no
    // duplicated condition. The QuantityCheck sits on the Untap sub_ability
    // above and gates this ContinuationStep.
    let transform_sub = untap_sub
        .sub_ability
        .as_deref()
        .expect("nested sub_ability (Transform) must be Some");
    assert!(
            transform_sub.condition.is_none(),
            "Transform sub_ability.condition must be None (the 'Then if' gate sits on the Untap sub_ability), got {:?}",
            transform_sub.condition,
        );
    match &*transform_sub.effect {
        Effect::Transform { target } => {
            // The parser today emits `ParentTarget` here — "transform it"
            // refers back to the Untap target (the trigger source).
            assert_eq!(
                *target,
                TargetFilter::ParentTarget,
                "Transform.target must be ParentTarget — 'transform it' inherits the Untap target",
            );
        }
        other => panic!("nested sub_ability effect must be Transform, got {other:?}"),
    }
    // The Transform clause is the inner-most sub_ability and inherits the
    // default `ContinuationStep` link — the chain shape is
    // LoseLife (outer) → Untap (SequentialSibling under the gate)
    // → Transform (ContinuationStep extension of the Untap clause). The
    // QuantityCheck on Untap still gates Transform via chain dependency.
    assert_eq!(
        transform_sub.sub_link,
        SubAbilityLink::ContinuationStep,
        "Transform sub_link must be ContinuationStep (chain extension of the Untap clause)",
    );
}

/// CR 104.3e + CR 603.4 + CR 508.6 + CR 119: Angel of Destiny (issue #1599).
/// "At the beginning of your end step, if you have at least 15 life more than
/// your starting life total, each player this creature attacked this turn
/// loses the game."
///
/// Two clauses were silently dropped before this fix:
///   1. The intervening-if (life ≥ starting life + 15) parsed as `None`, so
///      the loss fired every end step regardless of life total.
///   2. The subject "each player this creature attacked this turn" was
///      stripped, leaving `LoseTheGame` with no targets and no `player_scope`
///      — which `win_lose::resolve_lose` routes to the controller (CR 104.3a),
///      making the Angel eliminate its own controller.
#[test]
fn parse_angel_of_destiny_end_step_loss_issue_1599() {
    let def = parse_trigger_line(
            "At the beginning of your end step, if you have at least 15 life more than your starting life total, each player this creature attacked this turn loses the game.",
            "Angel of Destiny",
        );

    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::End));

    // Clause 1 — intervening-if (CR 603.4): LifeAboveStarting ≥ 15, i.e.
    // current life is at least 15 above the starting life total (CR 119).
    // Reuses the `LifeAboveStarting` building block (life − starting life).
    assert_eq!(
        def.condition,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeAboveStarting,
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 15 },
        }),
        "condition must be QuantityComparison LifeAboveStarting GE Fixed(15), got {:?}",
        def.condition,
    );

    // Clause 2 — effect + player scope: LoseTheGame fanned out over each
    // player the source creature attacked this turn (CR 508.6). The
    // controller is excluded by `OpponentAttacked { Source, ThisTurn }`, so the
    // Angel never eliminates itself — directly fixing the "my own Angel
    // killed me" report.
    let execute = def.execute.as_ref().expect("execute must be Some");
    assert!(
        matches!(*execute.effect, Effect::LoseTheGame { target: None }),
        "execute effect must be LoseTheGame with no explicit target, got {:?}",
        execute.effect,
    );
    assert_eq!(
        execute.player_scope,
        Some(PlayerFilter::OpponentAttacked {
            subject: AttackSubject::Source,
            scope: AttackScope::ThisTurn,
        }),
        "LoseTheGame must scope to players the source attacked this turn (issue #1599), got {:?}",
        execute.player_scope,
    );
}

/// CR 208.1 + CR 603.4: Cloud, Ex-SOLDIER — attack trigger with a "Then if
/// ~ has power 7 or greater, …" sub-ability gate. Before the `~ has power N`
/// grammar branch was added to `parse_source_power_toughness_condition`,
/// the condition silently dropped and the Treasure sub-ability fired on
/// every attack regardless of Cloud's power.
///
/// Asserted shape:
/// - Trigger-level `condition` is None (the gate scopes to the sub_ability).
/// - Outer effect is the draw clause.
/// - Sub-ability effect is Token creation, and its `condition` is a
///   `QuantityCheck` on `Power { scope: Source } >= 7`.
#[test]
fn parse_cloud_ex_soldier_attack_trigger_structure() {
    use crate::types::ability::{AbilityCondition, Effect};

    let def = parse_trigger_line(
            "Whenever ~ attacks, draw a card for each equipped attacking creature you control. Then if ~ has power 7 or greater, create two Treasure tokens.",
            "Cloud, Ex-SOLDIER",
        );

    // Trigger-level condition must be None — the "Then if ~ has power 7 or
    // greater" gate scopes only to the sub_ability.
    assert_eq!(
        def.condition, None,
        "trigger.condition must be None (gate scopes to sub_ability)"
    );

    let execute = def.execute.as_ref().expect("execute must be Some");

    // Outer effect: draw clause.
    assert!(
        matches!(*execute.effect, Effect::Draw { .. }),
        "outer execute must be Draw, got {:?}",
        execute.effect,
    );

    // Sub-ability: Treasure token creation gated on Power >= 7.
    let sub = execute
        .sub_ability
        .as_deref()
        .expect("sub_ability must be Some (Treasure clause)");
    match &sub.condition {
        Some(AbilityCondition::QuantityCheck {
            lhs:
                QuantityExpr::Ref {
                    qty:
                        QuantityRef::Power {
                            scope: crate::types::ability::ObjectScope::Source,
                        },
                },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 7 },
        }) => {}
        other => {
            panic!("sub_ability.condition must be QuantityCheck Power(Source) >= 7, got {other:?}",)
        }
    }
    assert!(
        matches!(*sub.effect, Effect::Token { .. }),
        "sub_ability effect must be Token, got {:?}",
        sub.effect,
    );
}

/// CR 208.1 + CR 107.3e + CR 603.4: Betor, Kin to All — three-tier end-step
/// trigger where each tier is gated on a "creatures you control have total
/// toughness N or greater" predicate. Before this regression test was
/// added, all three gates dropped silently because no parser path
/// recognized `Aggregate{Sum, Toughness, creatures-you-control}` in
/// condition position; the trigger fired unconditionally and the third
/// clause's "each opponent" subject collapsed onto the controller (so
/// "each opponent loses half their life, rounded up" hit the source
/// player instead).
///
/// Asserted shape:
/// - Trigger-level intervening-if at `total toughness ≥ 10` (CR 603.4 —
///   checked at trigger creation AND at resolution per the published
///   ruling).
/// - First effect (Draw) is unconditional under the trigger gate.
/// - Second sub_ability (UntapAll) carries `QuantityCheck ≥ 20`.
/// - Third sub_ability (LoseLife) carries `QuantityCheck ≥ 40` and its
///   target is NOT the controller — it must address each opponent.
#[test]
fn parse_betor_kin_to_all_trigger_structure() {
    use crate::types::ability::{
        AbilityCondition, AggregateFunction, Effect, ObjectProperty, RoundingMode,
    };

    let def = parse_trigger_line(
            "At the beginning of your end step, if creatures you control have total toughness 10 or greater, draw a card. Then if creatures you control have total toughness 20 or greater, untap each creature you control. Then if creatures you control have total toughness 40 or greater, each opponent loses half their life, rounded up.",
            "Betor, Kin to All",
        );

    // -- Trigger-level: intervening-if `Aggregate{Sum, Toughness, creatures-you-control} >= 10`.
    let trigger_cond = def
        .condition
        .as_ref()
        .expect("trigger.condition must be Some (intervening-if hoisted)");
    match trigger_cond {
        TriggerCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        } => {
            assert_eq!(*comparator, Comparator::GE);
            assert_eq!(*rhs, QuantityExpr::Fixed { value: 10 });
            match lhs {
                QuantityExpr::Ref {
                    qty:
                        QuantityRef::Aggregate {
                            function,
                            property,
                            filter,
                        },
                } => {
                    assert_eq!(*function, AggregateFunction::Sum);
                    assert_eq!(*property, ObjectProperty::Toughness);
                    match filter {
                        TargetFilter::Typed(t) => {
                            assert_eq!(t.controller, Some(ControllerRef::You));
                            assert!(t.type_filters.contains(&TypeFilter::Creature));
                        }
                        other => panic!("expected Typed(Creature, you) filter, got {other:?}"),
                    }
                }
                other => panic!("trigger lhs must be Aggregate Ref, got {other:?}"),
            }
        }
        other => panic!("trigger.condition must be QuantityComparison, got {other:?}"),
    }

    // -- Walk the sub_ability chain.
    let execute = def.execute.as_ref().expect("execute must be Some");

    // First effect: Draw (no per-clause condition; gate is the trigger-level intervening-if).
    assert!(
        matches!(*execute.effect, Effect::Draw { .. }),
        "first effect must be Draw, got {:?}",
        execute.effect,
    );

    // Second tier: UntapAll under "Then if ... ≥ 20".
    let untap_sub = execute
        .sub_ability
        .as_deref()
        .expect("first sub_ability (UntapAll) must be Some");
    match &untap_sub.condition {
            Some(AbilityCondition::QuantityCheck {
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 20 },
                lhs,
            }) => {
                assert!(
                    matches!(
                        lhs,
                        QuantityExpr::Ref {
                            qty: QuantityRef::Aggregate {
                                function: AggregateFunction::Sum,
                                property: ObjectProperty::Toughness,
                                ..
                            },
                        }
                    ),
                    "untap sub_ability lhs must be Aggregate Sum/Toughness, got {lhs:?}",
                );
            }
            other => panic!(
                "untap sub_ability.condition must be QuantityCheck >= 20 over Aggregate Sum/Toughness, got {other:?}",
            ),
        }
    assert!(
        matches!(
            *untap_sub.effect,
            Effect::SetTapState {
                state: TapStateChange::Untap,
                ..
            }
        ),
        "second tier effect must be UntapAll/Untap, got {:?}",
        untap_sub.effect,
    );

    // Third tier: LoseLife under "Then if ... ≥ 40", targeting each opponent.
    let lose_sub = untap_sub
        .sub_ability
        .as_deref()
        .expect("second sub_ability (LoseLife) must be Some");
    match &lose_sub.condition {
            Some(AbilityCondition::QuantityCheck {
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 40 },
                lhs,
            }) => {
                assert!(
                    matches!(
                        lhs,
                        QuantityExpr::Ref {
                            qty: QuantityRef::Aggregate {
                                function: AggregateFunction::Sum,
                                property: ObjectProperty::Toughness,
                                ..
                            },
                        }
                    ),
                    "lose-life sub_ability lhs must be Aggregate Sum/Toughness, got {lhs:?}",
                );
            }
            other => panic!(
                "lose-life sub_ability.condition must be QuantityCheck >= 40 over Aggregate Sum/Toughness, got {other:?}",
            ),
        }
    match &*lose_sub.effect {
        Effect::LoseLife { amount, target } => {
            // CR 107.1a: amount must be DivideRounded by 2, rounding Up
            // ("half ... rounded up"). The inner ref resolves per the
            // active player binding (CR 608.2c) when the outer
            // player_scope iterates over each opponent.
            assert!(
                matches!(
                    amount,
                    QuantityExpr::DivideRounded {
                        divisor: 2,
                        rounding: RoundingMode::Up,
                        ..
                    }
                ),
                "amount must be DivideRounded(2, Up), got {amount:?}",
            );

            // Critical regression: the third clause's "each opponent" subject
            // must NOT collapse onto the controller. The bug before the fix
            // produced a degenerate empty Typed filter (`type_filters: []`,
            // `controller: null`, `properties: []`) that the runtime resolved
            // to the controller, draining the source player's own life. Any
            // such empty filter must be rejected outright.
            if let Some(TargetFilter::Typed(t)) = target {
                let degenerate_empty =
                    t.type_filters.is_empty() && t.controller.is_none() && t.properties.is_empty();
                assert!(
                        !degenerate_empty,
                        "regression: LoseLife.target collapsed to the bug-shape degenerate Typed filter (would drain the controller): {t:?}",
                    );
            }
        }
        other => {
            panic!("third tier effect must be LoseLife addressing each opponent, got {other:?}",)
        }
    }

    // The each-opponent dispatch lives on the sub_ability's `player_scope`,
    // not on `LoseLife.target`. The parser's `strip_each_player_subject`
    // strips the "each opponent " prefix and lifts `PlayerFilter::Opponent`
    // onto the surrounding spell-execute wrapper; the runtime iterates the
    // life-loss across opponents (CR 608.2c). This is the same encoding
    // emitted today by Night Market Lookout's "each opponent loses 1 life"
    // trigger.
    assert_eq!(
            lose_sub.player_scope,
            Some(PlayerFilter::Opponent),
            "third tier sub_ability.player_scope must be Some(Opponent) — each-opponent dispatch must be lifted to the wrapper, not collapsed onto the controller",
        );
}

/// CR 608.2k: "that player discards a card" in a trigger effect must target
/// the triggering player (damaged player), not surface a fresh target prompt.
/// Abyssal-Specter-class regression test.
#[test]
fn parse_abyssal_specter_that_player_discard() {
    use crate::types::ability::Effect;

    let def = parse_trigger_line(
        "Whenever ~ deals damage to a player, that player discards a card.",
        "Abyssal Specter",
    );
    let execute = def.execute.as_ref().expect("execute must be Some");
    match &*execute.effect {
        Effect::Discard { target, .. } => {
            assert_eq!(
                target,
                &TargetFilter::TriggeringPlayer,
                "Discard.target must be TriggeringPlayer",
            );
        }
        other => panic!("execute effect must be Discard, got {other:?}"),
    }
}

#[test]
fn trigger_if_gained_and_lost_life_compound() {
    // CR 119: "you gained and lost life this turn" is a compound-verb condition
    // with shared object — two event verbs joined by "and" sharing "life this turn".
    let def = parse_trigger_line(
            "At the beginning of your end step, if you gained and lost life this turn, create a 1/1 black Bat creature token with flying.",
            "Some Card",
        );
    assert!(
        matches!(
            &def.condition,
            Some(TriggerCondition::And { conditions }) if conditions.len() == 2
        ),
        "Expected And with 2 conditions, got {:?}",
        def.condition
    );
    assert!(def.execute.is_some());
}

#[test]
fn trigger_if_gained_or_lost_life_compound() {
    // CR 119: "you gained or lost life this turn" (Star Charter, Starseer
    // Mentor, Starlit Soothsayer) is the disjunctive sibling of the "and"
    // compound — either life change satisfies it.
    let def = parse_trigger_line(
        "At the beginning of your end step, if you gained or lost life this turn, surveil 1.",
        "Starlit Soothsayer",
    );
    match &def.condition {
        Some(TriggerCondition::Or { conditions }) if conditions.len() == 2 => {
            assert!(conditions.iter().any(|c| matches!(
                c,
                TriggerCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::LifeGainedThisTurn { .. }
                    },
                    ..
                }
            )));
            assert!(conditions.iter().any(|c| matches!(
                c,
                TriggerCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::LifeLostThisTurn { .. }
                    },
                    ..
                }
            )));
        }
        other => panic!("Expected Or with LifeGained+LifeLost conditions, got {other:?}"),
    }
    assert!(def.execute.is_some(), "surveil effect must not be dropped");
}

#[test]
fn shared_animosity_attack_pump_for_each_other_attacker_sharing_type() {
    let def = parse_trigger_line(
            "Whenever a creature you control attacks, it gets +1/+0 until end of turn for each other attacking creature that shares a creature type with it.",
            "Shared Animosity",
        );
    assert_eq!(def.mode, TriggerMode::Attacks);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You)
        ))
    );
    let exec = def.execute.as_ref().expect("execute");
    match &*exec.effect {
        Effect::Pump {
            power,
            toughness,
            target,
            ..
        } => {
            assert_eq!(
                *target,
                TargetFilter::TriggeringSource,
                "attacker anaphor should be TriggeringSource"
            );
            assert_eq!(*toughness, PtValue::Fixed(0));
            let PtValue::Quantity(QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { filter },
            }) = power
            else {
                panic!("power should be for-each count, got {power:?}");
            };
            let TargetFilter::Typed(tf) = filter else {
                panic!("filter should be typed, got {filter:?}");
            };
            assert!(tf.properties.contains(&FilterProp::Another));
            assert!(tf
                .properties
                .contains(&FilterProp::Attacking { defender: None }));
            let shares = tf.properties.iter().find(|p| {
                matches!(
                    p,
                    FilterProp::SharesQuality {
                        quality: SharedQuality::CreatureType,
                        ..
                    }
                )
            });
            let Some(FilterProp::SharesQuality { reference, .. }) = shares else {
                panic!(
                    "for-each filter must include SharesQuality, properties: {:?}",
                    tf.properties
                );
            };
            assert_eq!(
                reference.as_deref(),
                Some(&TargetFilter::TriggeringSource),
                "shares-type reference should bind to the attacking creature"
            );
        }
        other => panic!("expected Pump, got {other:?}"),
    }
}

#[test]
fn trigger_attacker_it_gets_is_single_target_pump() {
    // CR 608.2c: "Whenever a creature you control attacks, it gets +2/+0 until end of turn."
    // "it" refers to the triggering attacker → single-object TriggeringSource,
    // which must lower to Effect::Pump (single target), NOT Effect::PumpAll.
    let def = parse_trigger_line(
        "Whenever a creature you control attacks, it gets +2/+2 until end of turn.",
        "Fervent Charge",
    );
    assert_eq!(def.mode, TriggerMode::Attacks);
    let exec = def.execute.as_ref().expect("execute must be Some");
    match &*exec.effect {
        Effect::Pump { target, .. } => {
            assert_eq!(*target, TargetFilter::TriggeringSource);
        }
        other => panic!(
            "expected Effect::Pump with TriggeringSource, got {:?}",
            other
        ),
    }
}

#[test]
fn necroduality_copies_entering_zombie_not_source() {
    // CR 608.2k: "Whenever a nontoken Zombie you control enters, create a
    // token that's a copy of that creature." "That creature" is the
    // entering Zombie (the event source), NOT Necroduality itself. The
    // untargeted ChangesZone trigger lifts CopyTokenOf's ParentTarget to
    // TriggeringSource. (#596)
    let def = parse_trigger_line(
            "Whenever a nontoken Zombie you control enters, create a token that's a copy of that creature.",
            "Necroduality",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    let exec = def.execute.as_ref().expect("execute must be Some");
    match &*exec.effect {
        Effect::CopyTokenOf { target, .. } => {
            assert_eq!(
                *target,
                TargetFilter::TriggeringSource,
                "copy source must be the entering Zombie, not the trigger's own source"
            );
        }
        other => panic!("expected Effect::CopyTokenOf, got {other:?}"),
    }
}

#[test]
fn etb_token_copier_exile_anaphor_binds_created_token() {
    // CR 603.7c + CR 608.2c: in an ETB-triggered token-copier, the trigger
    // sets the effect subject to the *entering* creature, so the bare-"it"
    // pronoun in "Exile it at the beginning of the next end step" lowers to
    // `TriggeringSource`. But the antecedent is the newly created TOKEN, so
    // the delayed-exile must bind `LastCreated`. The `CopyTokenOf` copy
    // source stays `TriggeringSource` (the token IS a copy of the entering
    // creature — locked by `necroduality_copies_entering_zombie_not_source`).
    fn collect<'a>(def: &'a AbilityDefinition, out: &mut Vec<&'a Effect>) {
        out.push(&def.effect);
        if let Effect::CreateDelayedTrigger { effect: inner, .. } = &*def.effect {
            collect(inner, out);
        }
        if let Some(sub) = def.sub_ability.as_deref() {
            collect(sub, out);
        }
        if let Some(els) = def.else_ability.as_deref() {
            collect(els, out);
        }
    }
    fn copy_source(effs: &[&Effect]) -> Option<TargetFilter> {
        effs.iter().find_map(|e| match e {
            Effect::CopyTokenOf { target, .. } => Some(target.clone()),
            _ => None,
        })
    }
    fn exile_target(effs: &[&Effect]) -> Option<TargetFilter> {
        effs.iter().find_map(|e| match e {
            Effect::ChangeZone {
                destination: Zone::Exile,
                target,
                ..
            } => Some(target.clone()),
            _ => None,
        })
    }

    for (name, text) in [
            (
                "Flameshadow Conjuring",
                "Whenever a nontoken creature you control enters, you may pay {R}. If you do, create a token that's a copy of that creature. That token gains haste. Exile it at the beginning of the next end step.",
            ),
            (
                "Inalla, Archmage Ritualist",
                "Whenever another nontoken Wizard you control enters, you may pay {1}. If you do, create a token that's a copy of that Wizard. The token gains haste. Exile it at the beginning of the next end step.",
            ),
        ] {
            let def = parse_trigger_line(text, name);
            let exec = def.execute.as_ref().expect("execute must be Some");
            let mut effs = Vec::new();
            collect(exec, &mut effs);
            assert_eq!(
                copy_source(&effs),
                Some(TargetFilter::TriggeringSource),
                "{name}: CopyTokenOf source must stay TriggeringSource (copy the entering creature)"
            );
            assert_eq!(
                exile_target(&effs),
                Some(TargetFilter::LastCreated),
                "{name}: delayed exile must bind the created token (LastCreated), not the entering creature"
            );
        }
}

/// CR 603.7a + CR 118.12a (issue #4369): Ashling, the Limitless — "Whenever
/// you sacrifice a nontoken Elemental, create a token that's a copy of it.
/// The token gains haste until end of turn. At the beginning of your next
/// end step, sacrifice it unless you pay {W}{U}{B}{R}{G}." The "unless you
/// pay {W}{U}{B}{R}{G}" alternative cost belongs to the DELAYED end-step
/// sacrifice, not to the parent "Whenever you sacrifice" trigger. Before the
/// fix `extract_unless_pay_modifier` scanned the whole multi-sentence effect,
/// found the "unless" in the delayed sentence, and hoisted the cost onto the
/// parent trigger — so the engine demanded the 5-color payment when the token
/// copy was created (wrong time), letting the player keep the token without
/// the mana. The cost must ride the `CreateDelayedTrigger`'s inner sacrifice.
#[test]
fn ashling_delayed_end_step_unless_pay_rides_delayed_sacrifice() {
    use crate::types::ability::{AbilityCost, Effect};

    // Walk the execute chain into the CreateDelayedTrigger's inner def.
    fn find_delayed(def: &AbilityDefinition) -> Option<&AbilityDefinition> {
        if let Effect::CreateDelayedTrigger { effect: inner, .. } = &*def.effect {
            return Some(inner);
        }
        def.sub_ability
            .as_deref()
            .and_then(find_delayed)
            .or_else(|| def.else_ability.as_deref().and_then(find_delayed))
    }

    let def = parse_trigger_line(
        "Whenever you sacrifice a nontoken Elemental, create a token that's a copy of it. \
             The token gains haste until end of turn. At the beginning of your next end step, \
             sacrifice it unless you pay {W}{U}{B}{R}{G}.",
        "Ashling, the Limitless",
    );

    // (1) The parent sacrifice trigger must NOT carry the delayed unless-cost.
    assert_eq!(
        def.unless_pay, None,
        "the delayed end-step unless-cost must not be hoisted onto the parent trigger"
    );

    // (2) The delayed trigger's inner sacrifice carries the {W}{U}{B}{R}{G} cost.
    let exec = def
        .execute
        .as_deref()
        .expect("Ashling trigger execute body");
    let delayed = find_delayed(exec).expect("CreateDelayedTrigger in the effect chain");
    let unless = delayed
        .unless_pay
        .as_ref()
        .expect("the delayed sacrifice must carry the unless-cost");
    match &unless.cost {
        AbilityCost::Mana { cost } => assert_eq!(
            cost.mana_value(),
            5,
            "the delayed unless-cost must be the 5-color {{W}}{{U}}{{B}}{{R}}{{G}}, got {cost:?}"
        ),
        other => panic!("delayed unless-cost must be AbilityCost::Mana, got {other:?}"),
    }
}

/// CR 603.7a + CR 118.12a + CR 107.14 (issue #4369): Satya, Aetherflux Genius
/// — the energy-cost sibling of the same class: "Whenever Satya attacks, ...
/// At the beginning of the next end step, sacrifice that token unless you pay
/// an amount of {E} equal to its mana value." The dynamic-energy unless-cost
/// must ride the delayed sacrifice (not the parent attack trigger), exactly
/// like Ashling's mana cost — proving the fix is class-general across cost
/// types, not a one-off for Ashling.
#[test]
fn satya_delayed_end_step_energy_unless_rides_delayed_sacrifice() {
    use crate::types::ability::{AbilityCost, Effect};

    fn find_delayed(def: &AbilityDefinition) -> Option<&AbilityDefinition> {
        if let Effect::CreateDelayedTrigger { effect: inner, .. } = &*def.effect {
            return Some(inner);
        }
        def.sub_ability
            .as_deref()
            .and_then(find_delayed)
            .or_else(|| def.else_ability.as_deref().and_then(find_delayed))
    }

    let def = parse_trigger_line(
        "Whenever Satya attacks, create a tapped and attacking token that's a copy of up to \
             one other target nontoken creature you control. You get {E}{E}. At the beginning of \
             the next end step, sacrifice that token unless you pay an amount of {E} equal to its \
             mana value.",
        "Satya, Aetherflux Genius",
    );

    assert_eq!(
        def.unless_pay, None,
        "the delayed end-step energy unless-cost must not be hoisted onto the parent trigger"
    );
    let exec = def.execute.as_deref().expect("Satya trigger execute body");
    let delayed = find_delayed(exec).expect("CreateDelayedTrigger in the effect chain");
    let unless = delayed
        .unless_pay
        .as_ref()
        .expect("the delayed sacrifice must carry the energy unless-cost");
    assert!(
        matches!(unless.cost, AbilityCost::PayEnergy { .. }),
        "delayed unless-cost must be AbilityCost::PayEnergy, got {:?}",
        unless.cost
    );
}

/// CR 115.1 + CR 118.12a + CR 603.3d: Athreos, God of Passage — "Whenever
/// another creature you control dies, return it to its owner's hand unless
/// target opponent pays 3 life." The unless-payer is declared as a target
/// inside the unless clause, distinct from the anaphoric "they"/"that
/// opponent" forms.
#[test]
fn athreos_god_of_passage_targeted_opponent_unless_pay() {
    let def = parse_trigger_line(
        "Whenever another creature you control dies, return it to its owner's hand \
             unless target opponent pays 3 life.",
        "Athreos, God of Passage",
    );

    let unless = def
        .unless_pay
        .as_ref()
        .expect("Athreos trigger must carry the unless-pay modifier");
    assert_eq!(
        unless.payer,
        TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
        "payer must be a declared-target opponent Typed, not the anaphoric forms"
    );
    assert_eq!(
        unless.cost,
        AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 3 }
        },
        "cost must be PayLife 3, got {:?}",
        unless.cost
    );

    let exec = def.execute.as_deref().expect("Athreos execute body");
    assert!(
        !matches!(&*exec.effect, Effect::Unimplemented { .. }),
        "body effect must be the parsed return-to-hand, got {:?}",
        exec.effect
    );
    assert!(
        matches!(
            &*exec.effect,
            Effect::ChangeZone {
                destination: Zone::Hand,
                ..
            } | Effect::Bounce {
                destination: Some(Zone::Hand) | None,
                ..
            }
        ),
        "body effect must return the creature to its owner's hand, got {:?}",
        exec.effect
    );
}

/// CR 118.12a: Sibling guard for the declared-target arm. The anaphoric
/// pronoun forms must not collapse to the declared-target `Typed` payer.
#[test]
fn unless_pay_anaphoric_payers_distinct_from_declared_target() {
    let they = parse_trigger_line(
        "Whenever you attack, target opponent loses 3 life unless they pay 3 life.",
        "They Pay Test",
    );
    let they_unless = they
        .unless_pay
        .as_ref()
        .expect("\"they pay\" must yield an unless-pay modifier");
    assert_eq!(
        they_unless.payer,
        TargetFilter::Player,
        "\"they pay\" stays anaphoric Player, not the declared-target Typed"
    );

    let that = parse_trigger_line(
        "Whenever a player casts a spell, that player draws a card \
             unless that opponent pays 3 life.",
        "That Opponent Pay Test",
    );
    let that_unless = that
        .unless_pay
        .as_ref()
        .expect("\"that opponent pays\" must yield an unless-pay modifier");
    assert_eq!(
        that_unless.payer,
        TargetFilter::TriggeringPlayer,
        "\"that opponent pays\" stays anaphoric TriggeringPlayer"
    );
    assert!(
        matches!(that_unless.cost, AbilityCost::PayLife { .. }),
        "cost must be PayLife, got {:?}",
        that_unless.cost
    );
}

#[test]
fn non_token_enters_exile_it_stays_triggering_source() {
    // No-regression: a non-token ETB trigger "exile it" has NO token creator
    // in the chain, so the populate-anaphor repair pass is never entered and
    // "it" correctly stays `TriggeringSource` (the entering creature).
    let def = parse_trigger_line(
        "Whenever a creature you control enters, exile it at the beginning of the next end step.",
        "Test Nontoken Exiler",
    );
    let exec = def.execute.as_ref().expect("execute must be Some");
    fn find_exile(def: &AbilityDefinition) -> Option<TargetFilter> {
        if let Effect::CreateDelayedTrigger { effect: inner, .. } = &*def.effect {
            if let Effect::ChangeZone {
                destination: Zone::Exile,
                target,
                ..
            } = &*inner.effect
            {
                return Some(target.clone());
            }
        }
        if let Effect::ChangeZone {
            destination: Zone::Exile,
            target,
            ..
        } = &*def.effect
        {
            return Some(target.clone());
        }
        def.sub_ability.as_deref().and_then(find_exile)
    }
    assert_eq!(
        find_exile(exec),
        Some(TargetFilter::TriggeringSource),
        "non-token 'exile it' must stay TriggeringSource (no token creator → pass not entered)"
    );
}

/// CR 608.2c + CR 603.7c (issue #4601): a PHASE-triggered token-copier —
/// Mishra, Eminent One: "At the beginning of combat on your turn, create a
/// token that's a copy of target noncreature artifact you control, … It
/// gains haste until end of turn. Sacrifice it at the beginning of the next
/// end step." Unlike the ETB copiers above, a Phase trigger has no
/// triggering object, so the bare-"it" in "Sacrifice it" lowers to `SelfRef`
/// (the source — Mishra) rather than `TriggeringSource`. The antecedent is
/// still the newly created TOKEN, so the delayed sacrifice must bind
/// `LastCreated` — otherwise Mishra sacrifices ITSELF at the end step.
#[test]
fn phase_trigger_token_copier_sacrifice_anaphor_binds_created_token() {
    fn collect<'a>(def: &'a AbilityDefinition, out: &mut Vec<&'a Effect>) {
        out.push(&def.effect);
        if let Effect::CreateDelayedTrigger { effect: inner, .. } = &*def.effect {
            collect(inner, out);
        }
        if let Some(sub) = def.sub_ability.as_deref() {
            collect(sub, out);
        }
        if let Some(els) = def.else_ability.as_deref() {
            collect(els, out);
        }
    }
    let def = parse_trigger_line(
            "At the beginning of combat on your turn, create a token that's a copy of target noncreature artifact you control, except its name is Mishra's Warform and it's a 4/4 Construct artifact creature in addition to its other types. It gains haste until end of turn. Sacrifice it at the beginning of the next end step.",
            "Mishra, Eminent One",
        );
    let exec = def.execute.as_ref().expect("execute must be Some");
    let mut effs = Vec::new();
    collect(exec, &mut effs);
    let sac_target = effs.iter().find_map(|e| match e {
        Effect::Sacrifice { target, .. } => Some(target.clone()),
        _ => None,
    });
    assert_eq!(
        sac_target,
        Some(TargetFilter::LastCreated),
        "the delayed 'Sacrifice it' must bind the created token (LastCreated), not \
             Mishra itself (SelfRef) — otherwise Mishra sacrifices itself at the end step",
    );
}

/// CR 603.7c + CR 608.2c (issue #4601 review): the same Phase-triggered
/// token-copier anaphor, but with a LIBRARY-POSITION cleanup instead of a
/// sacrifice — "… create a token that's a copy of target creature you
/// control. It gains haste until end of turn. Put it on the bottom of its
/// owner's library at the beginning of the next end step." A Phase trigger
/// has no triggering object, so the bare-"it" in the delayed "Put it …"
/// lowers to `SelfRef`; `Effect::PutAtLibraryPosition` is a token-cleanup
/// move just like the sacrifice form, so it must rebind to `LastCreated`.
#[test]
fn phase_trigger_token_copier_library_cleanup_anaphor_binds_created_token() {
    fn collect<'a>(def: &'a AbilityDefinition, out: &mut Vec<&'a Effect>) {
        out.push(&def.effect);
        if let Effect::CreateDelayedTrigger { effect: inner, .. } = &*def.effect {
            collect(inner, out);
        }
        if let Some(sub) = def.sub_ability.as_deref() {
            collect(sub, out);
        }
        if let Some(els) = def.else_ability.as_deref() {
            collect(els, out);
        }
    }
    let def = parse_trigger_line(
            "At the beginning of combat on your turn, create a token that's a copy of target creature you control. It gains haste until end of turn. Put it on the bottom of its owner's library at the beginning of the next end step.",
            "Test Phase Library Copier",
        );
    let exec = def.execute.as_ref().expect("execute must be Some");
    let mut effs = Vec::new();
    collect(exec, &mut effs);
    let lib_target = effs.iter().find_map(|e| match e {
        Effect::PutAtLibraryPosition { target, .. } => Some(target.clone()),
        _ => None,
    });
    assert_eq!(
        lib_target,
        Some(TargetFilter::LastCreated),
        "the delayed 'Put it on the bottom of its owner's library' must bind the \
             created token (LastCreated), not the source (SelfRef)",
    );
}

#[test]
fn flicker_enters_trigger_keeps_chosen_target_anaphor() {
    // CR 608.2c: "When ~ enters, you may exile another target permanent you
    // control, then return that card …" — "that card" refers to the CHOSEN
    // exiled permanent (ParentTarget), NOT the trigger event. The enters
    // event-source lift must STOP at the first chosen target so the
    // sub-ability stays ParentTarget (regression guard for #596 — Felidar
    // Guardian / Restoration Angel flicker class).
    let def = parse_trigger_line(
            "When this creature enters, you may exile another target permanent you control, then return that card to the battlefield under your control.",
            "Felidar Guardian",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    let exec = def.execute.as_ref().expect("execute must be Some");
    // Top link targets the chosen permanent.
    assert!(
        matches!(exec.effect.target_filter(), Some(TargetFilter::Typed(_))),
        "top-level exile must keep its chosen Typed target, got {:?}",
        exec.effect.target_filter()
    );
    // Sub-ability "that card" must remain ParentTarget, not be lifted.
    let sub = exec
        .sub_ability
        .as_ref()
        .expect("flicker chains to a return");
    assert_eq!(
        sub.effect.target_filter(),
        Some(&TargetFilter::ParentTarget),
        "return-that-card must bind to the chosen permanent (ParentTarget), not TriggeringSource"
    );
}

#[test]
fn counter_added_trigger_captures_explicit_type() {
    // CR 122.1: Hapatra — "Whenever you put one or more -1/-1 counters on a
    // creature, create a Snake" must fire ONLY on -1/-1 counters, not any
    // counter. The type-prefix extractor sets counter_filter. (#589)
    let def = parse_trigger_line(
            "Whenever you put one or more -1/-1 counters on a creature, create a 1/1 green Snake creature token with deathtouch.",
            "Hapatra, Vizier of Poisons",
        );
    assert_eq!(def.mode, TriggerMode::CounterAdded);
    let filter = def
        .counter_filter
        .as_ref()
        .expect("explicit -1/-1 counter type must be captured");
    assert_eq!(
        filter.counter_type,
        crate::types::counter::CounterType::Minus1Minus1
    );
    assert_eq!(filter.threshold, None, "no threshold form here");
}

#[test]
fn counter_added_trigger_generic_has_no_type_filter() {
    // "one or more counters" (no type word) keeps firing on any counter
    // kind — the extractor must leave counter_filter unset.
    let def = parse_trigger_line(
        "Whenever you put one or more counters on a creature, draw a card.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::CounterAdded);
    assert!(
        def.counter_filter.is_none(),
        "generic counter trigger must not set a type filter"
    );
}

#[test]
fn counter_added_trigger_rejects_non_type_remainder() {
    // Regression (#589 review): the prefix strip is loose, so a non-"you"
    // subject ("an opponent puts one or more counters") leaves leftover
    // subject text in the type slot. try_parse_counter_type must reject that
    // multi-word junk → counter_filter stays None → the any-counter trigger
    // (Bold Plagiarist class) keeps firing on every counter, not none.
    let def = parse_trigger_line(
            "Whenever an opponent puts one or more counters on a creature they control, create a 1/1 colorless Thopter artifact creature token with flying.",
            "Test Card",
        );
    assert_eq!(def.mode, TriggerMode::CounterAdded);
    assert!(
        def.counter_filter.is_none(),
        "loose prefix strip must not manufacture a bogus typed filter from subject text"
    );
}

#[test]
fn doran_attack_block_pump_resolves_pt_difference() {
    // Issue #417 — Doran, Besieged by Time.
    // "Whenever a creature you control attacks or blocks, it gets +X/+X
    //  until end of turn, where X is the difference between its power and
    //  toughness." The `where X is` tail must lower to a typed
    //  QuantityExpr::Difference, NOT leak the clause text into a
    //  PtValue::Variable (the `unwrap_or_else` fallback in
    //  oracle_effect/mod.rs). CR 208.1 / CR 611.2d. ("The difference
    //  between A and B" is an unsigned Oracle templating convention with
    //  no dedicated CR number — the resolver takes `.abs()`.)
    let def = parse_trigger_line(
            "Whenever a creature you control attacks or blocks, it gets +X/+X until end of turn, where X is the difference between its power and toughness.",
            "Doran, Besieged by Time",
        );
    let exec = def.execute.as_ref().expect("execute must be Some");
    let expected = QuantityExpr::Difference {
        left: Box::new(QuantityExpr::Ref {
            qty: QuantityRef::Power {
                scope: ObjectScope::Recipient,
            },
        }),
        right: Box::new(QuantityExpr::Ref {
            qty: QuantityRef::Toughness {
                scope: ObjectScope::Recipient,
            },
        }),
    };
    match &*exec.effect {
        Effect::Pump {
            power,
            toughness,
            target,
        } => {
            assert_eq!(
                *power,
                PtValue::Quantity(expected.clone()),
                "power must be a typed Difference, not a Variable leak"
            );
            assert_eq!(
                *toughness,
                PtValue::Quantity(expected),
                "toughness must be a typed Difference, not a Variable leak"
            );
            assert_eq!(*target, TargetFilter::TriggeringSource);
        }
        other => panic!("expected Effect::Pump, got {other:?}"),
    }
}

#[test]
fn trigger_execute_pump_all_creatures() {
    // Regression: trigger bodies with "creatures you control get +1/+1 until end of turn"
    // must produce a PumpAll execute effect, not null.
    let def = parse_trigger_line(
            "Whenever another creature you control enters, creatures you control get +1/+1 until end of turn.",
            "Goldnight Commander",
        );
    assert!(
        def.execute.is_some(),
        "execute should be Some (PumpAll), got None"
    );
    let exec = def.execute.as_ref().unwrap();
    assert!(
        matches!(*exec.effect, Effect::PumpAll { .. }),
        "execute effect should be PumpAll, got {:?}",
        exec.effect
    );
}

#[test]
fn extract_if_graveyard_threshold() {
    let (cleaned, cond) = extract_if_condition(
            "if there are seven or more cards in your graveyard, exile a card at random from your graveyard.",
        );
    assert!(
        matches!(cond, Some(TriggerCondition::QuantityComparison { .. })),
        "Expected QuantityComparison, got {:?}",
        cond
    );
    assert!(
        cleaned.contains("exile"),
        "Effect text should remain: {cleaned}"
    );
}

#[test]
fn trigger_graveyard_threshold_tersa() {
    let def = parse_trigger_line(
            "Whenever ~ attacks, if there are seven or more cards in your graveyard, exile a card at random from your graveyard. You may play that card this turn.",
            "Tersa Lightshatter",
        );
    assert_eq!(def.mode, TriggerMode::Attacks);
    assert!(
        matches!(
            def.condition,
            Some(TriggerCondition::QuantityComparison { .. })
        ),
        "Expected graveyard threshold condition, got {:?}",
        def.condition
    );
}

// --- Counter placement with "you put" pattern ---

#[test]
fn trigger_you_put_counters_on_self() {
    let def = parse_trigger_line(
            "Whenever you put one or more +1/+1 counters on this creature, draw a card. This ability triggers only once each turn.",
            "Exemplar of Light",
        );
    assert_eq!(def.mode, TriggerMode::CounterAdded);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert_eq!(
        def.constraint,
        Some(crate::types::ability::TriggerConstraint::OncePerTurn)
    );
    // Constraint sentence should NOT leak as a sub-ability
    if let Some(ref exec) = def.execute {
        assert!(
            !matches!(
                *exec.effect,
                crate::types::ability::Effect::Unimplemented { .. }
            ),
            "Effect should be Draw, not Unimplemented"
        );
        assert!(
            exec.sub_ability.is_none(),
            "No spurious sub-ability from constraint text"
        );
    }
}

#[test]
fn trigger_counters_put_on_another_creature_you_control() {
    use crate::types::ability::ControllerRef;
    let def = parse_trigger_line(
            "Whenever one or more +1/+1 counters are put on another creature you control, put a +1/+1 counter on this creature.",
            "Enduring Scalelord",
        );
    assert_eq!(def.mode, TriggerMode::CounterAdded);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::Another])
        ))
    );
}

#[test]
fn trigger_you_put_counters_on_creature_you_control() {
    use crate::types::ability::ControllerRef;
    let def = parse_trigger_line(
        "Whenever you put one or more +1/+1 counters on a creature you control, draw a card.",
        "The Powerful Dragon",
    );
    assert_eq!(def.mode, TriggerMode::CounterAdded);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You)
        ))
    );
}

#[test]
fn strip_constraint_does_not_affect_effect() {
    let result =
        strip_constraint_sentences("draw a card. this ability triggers only once each turn.");
    assert_eq!(result, "draw a card");
}

#[test]
fn strip_constraint_preserves_plain_effect() {
    let result = strip_constraint_sentences("put a +1/+1 counter on ~");
    assert_eq!(result, "put a +1/+1 counter on ~");
}

// --- Color-filtered trigger subjects ---

#[test]
fn trigger_white_creature_you_control_attacks() {
    let def = parse_trigger_line(
        "Whenever a white creature you control attacks, you gain 1 life.",
        "Linden, the Steadfast Queen",
    );
    assert_eq!(def.mode, TriggerMode::Attacks);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::creature()
                .controller(crate::types::ability::ControllerRef::You)
                .properties(vec![FilterProp::HasColor {
                    color: crate::types::mana::ManaColor::White
                }])
        ))
    );
}

// --- New trigger mode tests ---

#[test]
fn trigger_land_enters() {
    let def = parse_trigger_line("When this land enters, you gain 1 life.", "Bloodfell Caves");
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

#[test]
fn trigger_evelyn_exiles_each_library_with_collection_counter_and_permission() {
    let def = parse_trigger_line(
            "Whenever Evelyn or another Vampire you control enters, exile the top card of each player's library with a collection counter on it.",
            "Evelyn, the Covetous",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));

    let execute = def.execute.as_deref().expect("execute ability");
    assert_eq!(execute.player_scope, Some(PlayerFilter::All));
    assert!(matches!(
        execute.effect.as_ref(),
        Effect::ExileTop {
            player: TargetFilter::Controller,
            count: QuantityExpr::Fixed { value: 1 },
            face_down: false,
        }
    ));

    let counter = execute.sub_ability.as_deref().expect("counter rider");
    assert!(matches!(
        counter.effect.as_ref(),
        Effect::PutCounterAll {
            counter_type: CounterType::Generic(name),
            target: TargetFilter::TrackedSet { .. },
            ..
        } if name == "collection"
    ));

    let grant = counter.sub_ability.as_deref().expect("permission rider");
    let Effect::GrantCastingPermission {
        permission, target, ..
    } = grant.effect.as_ref()
    else {
        panic!("expected GrantCastingPermission, got {:?}", grant.effect);
    };
    assert!(matches!(target, TargetFilter::TrackedSet { .. }));
    assert!(matches!(
        permission,
        CastingPermission::PlayFromExile {
            frequency: CastFrequency::OncePerTurn,
            mana_spend_permission: Some(ManaSpendPermission::AnyTypeOrColor),
            ..
        }
    ));
}

#[test]
fn trigger_aura_enters() {
    let def = parse_trigger_line(
        "When this Aura enters, tap target creature an opponent controls.",
        "Glaring Aegis",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

#[test]
fn trigger_equipment_enters() {
    let def = parse_trigger_line(
        "When this Equipment enters, attach it to target creature you control.",
        "Shining Armor",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

#[test]
fn trigger_vehicle_enters() {
    let def = parse_trigger_line(
        "When this Vehicle enters, create a 1/1 white Pilot creature token.",
        "Some Vehicle",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

#[test]
fn trigger_leaves_battlefield() {
    let def = parse_trigger_line(
        "When Oblivion Ring leaves the battlefield, return the exiled card to the battlefield.",
        "Oblivion Ring",
    );
    assert_eq!(def.mode, TriggerMode::LeavesBattlefield);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert!(def.trigger_zones.contains(&Zone::Graveyard));
    assert!(def.trigger_zones.contains(&Zone::Exile));
}

#[test]
fn trigger_leaves_battlefield_without_dying_excludes_graveyard_destination() {
    let def = parse_trigger_line(
            "Whenever this creature or another creature you control leaves the battlefield without dying, put a +1/+1 counter on target creature you control.",
            "Three Tree Scribe",
        );
    assert_eq!(def.mode, TriggerMode::LeavesBattlefield);
    assert_eq!(
        def.destination_constraint,
        DestinationConstraint::NotEquals(Zone::Graveyard)
    );
}

#[test]
fn trigger_skyclave_apparition_leaves_battlefield_uses_linked_exile_owner_scope() {
    let def = parse_trigger_line(
            "When this creature leaves the battlefield, the exiled card's owner creates an X/X blue Illusion creature token, where X is the mana value of the exiled card.",
            "Skyclave Apparition",
        );
    assert_eq!(def.mode, TriggerMode::LeavesBattlefield);

    let execute = def.execute.as_deref().expect("execute ability");
    assert_eq!(
        execute.player_scope,
        Some(PlayerFilter::OwnersOfCardsExiledBySource)
    );

    match execute.effect.as_ref() {
        Effect::Token {
            name,
            power,
            toughness,
            ..
        } => {
            assert_eq!(name, "Illusion");
            let expected = QuantityExpr::Ref {
                qty: QuantityRef::Aggregate {
                    function: crate::types::ability::AggregateFunction::Sum,
                    property: crate::types::ability::ObjectProperty::ManaValue,
                    filter: TargetFilter::And {
                        filters: vec![
                            TargetFilter::ExiledBySource,
                            TargetFilter::Typed(TypedFilter::default().properties(vec![
                                FilterProp::Owned {
                                    controller: ControllerRef::You,
                                },
                            ])),
                        ],
                    },
                },
            };
            assert_eq!(power, &PtValue::Quantity(expected.clone()));
            assert_eq!(toughness, &PtValue::Quantity(expected));
        }
        other => panic!("expected Skyclave leaves trigger to create a token, got {other:?}"),
    }
}

/// CR 113.6k: A non-self-referential LTB trigger (source stays on the
/// battlefield while some other object leaves) must NOT extend its
/// `trigger_zones` into graveyard/exile — otherwise the trigger would
/// continue to fire even after its source permanent was removed.
#[test]
fn trigger_leaves_battlefield_non_self_ref_keeps_default_zones() {
    let def = parse_trigger_line(
        "Whenever a creature you control leaves the battlefield, each opponent loses 1 life.",
        "Ninja Teen",
    );
    assert_eq!(def.mode, TriggerMode::LeavesBattlefield);
    assert!(
        !def.trigger_zones.contains(&Zone::Graveyard),
        "non-self-ref LTB must not extend to graveyard"
    );
    assert!(
        !def.trigger_zones.contains(&Zone::Exile),
        "non-self-ref LTB must not extend to exile"
    );
}

/// CR 603.10a: "Whenever an instant or sorcery card leaves your graveyard"
/// (Murktide Regent). The single-subject leaves-a-graveyard form routes
/// through the general ChangesZone matcher via one zone-change clause with
/// an unconstrained destination (the card may move to any zone). The trigger
/// is non-self-referential -- it lives on the battlefield permanent -- so
/// trigger_zones must stay at the battlefield default. The owner scope
/// ("your graveyard") on the shared `parse_zone_change_clause` building block
/// is covered by `parse_syr_konrad_disjunctive_zone_change`.
#[test]
fn trigger_murktide_leaves_your_graveyard() {
    let def = parse_trigger_line(
        "Whenever an instant or sorcery card leaves your graveyard, \
             put a +1/+1 counter on this creature.",
        "Murktide Regent",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(
        def.zone_change_clauses.len(),
        1,
        "expected one zone-change clause, got {:?}",
        def.zone_change_clauses
    );
    let clause = &def.zone_change_clauses[0];
    assert_eq!(clause.origin, OriginConstraint::Equals(Zone::Graveyard));
    assert_eq!(clause.destination, None);
    assert!(
        clause.valid_card.is_some(),
        "clause must carry a card filter"
    );
    // Non-self-ref: the ability lives on the battlefield permanent and must
    // not extend trigger_zones into graveyard/exile.
    assert!(!def.trigger_zones.contains(&Zone::Graveyard));
    assert!(!def.trigger_zones.contains(&Zone::Exile));
    // The effect chain must resolve (no Unimplemented leakage).
    let execute = def.execute.as_ref().expect("execute ability");
    fn has_unimplemented(ability: &AbilityDefinition) -> bool {
        matches!(*ability.effect, Effect::Unimplemented { .. })
            || ability
                .sub_ability
                .as_ref()
                .is_some_and(|s| has_unimplemented(s))
    }
    assert!(
        !has_unimplemented(execute),
        "effect chain leaked Unimplemented: {:?}",
        execute
    );
}

/// CR 603.2 + CR 613.3 + #1522: "When you lose control of ~" — Sigil of
/// Corruption ability on Khârn the Betrayer. Maps to ChangesController with
/// valid_card = SelfRef so the trigger fires only when this specific permanent
/// changes controller. Execute draws 2 for the previous controller (the trigger
/// fires before layer re-evaluation, so the previous controller still holds Khârn
/// at trigger-scan time).
#[test]
fn trigger_when_you_lose_control_maps_to_changes_controller() {
    let def = parse_trigger_line(
        "When you lose control of ~, draw two cards.",
        "Khârn the Betrayer",
    );
    assert_eq!(def.mode, TriggerMode::ChangesController);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

/// CR 603.2c: Batched "one or more permanents … leave the battlefield" uses the plural
/// verb form. The parser must accept "leave" alongside "leaves" for LTB triggers.
#[test]
fn trigger_one_or_more_permanents_leave_the_battlefield() {
    let def = parse_trigger_line(
        "Whenever one or more permanents you control leave the battlefield, scry 1.",
        "Nefarious Imp",
    );
    assert_eq!(def.mode, TriggerMode::LeavesBattlefield);
    assert!(def.batched, "one or more must set batched flag");
    assert!(
        !def.trigger_zones.contains(&Zone::Graveyard),
        "non-self-ref LTB must not extend to graveyard"
    );
}

/// CR 603.2c + CR 603.6c: Batched "one or more artifacts you control leave the
/// battlefield during your turn" — Oni-Cult Anvil. CR 603.2c covers the
/// batched (one-or-more) trigger; CR 603.6c covers the LTB event; CR 603.4
/// covers the "during your turn" intervening-if condition. The trailing
/// "only once each turn" becomes an OncePerTurn constraint.
#[test]
fn trigger_one_or_more_artifacts_leave_battlefield_during_your_turn() {
    let def = parse_trigger_line(
            "Whenever one or more artifacts you control leave the battlefield during your turn, create a 1/1 colorless Construct artifact creature token. This ability triggers only once each turn.",
            "Oni-Cult Anvil",
        );
    assert_eq!(def.mode, TriggerMode::LeavesBattlefield);
    assert!(def.batched, "one or more must set batched flag");
    assert_eq!(
        def.condition,
        Some(TriggerCondition::DuringPlayersTurn {
            player: PlayerFilter::Controller,
        }),
        "during your turn must become DuringPlayersTurn condition"
    );
    assert_eq!(
        def.constraint,
        Some(TriggerConstraint::OncePerTurn),
        "'only once each turn' must set OncePerTurn constraint"
    );
    assert!(
        !def.trigger_zones.contains(&Zone::Graveyard),
        "non-self-ref LTB must not extend to graveyard"
    );
}

/// CR 603.6c + CR 603.4: Singular "another permanent you control leaves the
/// battlefield during your turn" — Suki, Courageous Rescuer. CR 603.6c
/// covers the LTB event; CR 603.4 covers the "during your turn"
/// intervening-if condition. The trailing "only once each turn" becomes an
/// OncePerTurn constraint.
#[test]
fn trigger_another_permanent_leaves_battlefield_during_your_turn() {
    let def = parse_trigger_line(
            "Whenever another permanent you control leaves the battlefield during your turn, create a 1/1 white Ally creature token. This ability triggers only once each turn.",
            "Suki, Courageous Rescuer",
        );
    assert_eq!(def.mode, TriggerMode::LeavesBattlefield);
    assert!(!def.batched, "singular form must not set batched flag");
    assert_eq!(
        def.condition,
        Some(TriggerCondition::DuringPlayersTurn {
            player: PlayerFilter::Controller,
        }),
        "during your turn must become DuringPlayersTurn condition"
    );
    assert_eq!(
        def.constraint,
        Some(TriggerConstraint::OncePerTurn),
        "'only once each turn' must set OncePerTurn constraint"
    );
}

/// CR 603.6c + CR 603.4: Synthetic test for an LTB trigger with "during an
/// opponent's turn" tail. Proves the opponent-turn branch maps to
/// `DuringPlayersTurn { player: Opponent }`.
#[test]
fn trigger_leaves_battlefield_during_opponents_turn() {
    let def = parse_trigger_line(
            "Whenever a creature you control leaves the battlefield during an opponent's turn, draw a card.",
            "Synthetic Opponent Turn LTB",
        );
    assert_eq!(def.mode, TriggerMode::LeavesBattlefield);
    assert_eq!(
        def.condition,
        Some(TriggerCondition::DuringPlayersTurn {
            player: PlayerFilter::Opponent,
        }),
        "during an opponent's turn must become DuringPlayersTurn with Opponent"
    );
}
/// CR 603.6c: "Whenever a permanent is returned to a player's hand" — Warped Devotion.
#[test]
fn trigger_returned_to_a_players_hand() {
    let def = parse_trigger_line(
        "Whenever a permanent is returned to a player's hand, that player discards a card.",
        "Warped Devotion",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.origin, Some(Zone::Battlefield));
    assert_eq!(def.destination, Some(Zone::Hand));
    // "a permanent" — any permanent, not self-referential.
    assert!(def.valid_card.is_some());
    // "a player's hand" — no owner constraint.
    assert!(def.valid_target.is_none());
}

/// CR 603.6c: "Whenever a permanent is returned to your hand" — Azorius Aethermage.
#[test]
fn trigger_returned_to_your_hand() {
    let def = parse_trigger_line(
        "Whenever a permanent is returned to your hand, you may pay {1}. If you do, draw a card.",
        "Azorius Aethermage",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.origin, Some(Zone::Battlefield));
    assert_eq!(def.destination, Some(Zone::Hand));
    // "your hand" — controller constraint merged into valid_card.
    let vc = def.valid_card.as_ref().expect("valid_card should be set");
    match vc {
        TargetFilter::Typed(tf) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
        }
        _ => panic!("expected Typed filter with controller"),
    }
    // Owner constraint is on valid_card, not valid_target.
    assert!(def.valid_target.is_none());
}

#[test]
fn trigger_becomes_blocked() {
    let def = parse_trigger_line(
        "Whenever Gustcloak Cavalier becomes blocked, you may untap it and remove it from combat.",
        "Gustcloak Cavalier",
    );
    assert_eq!(def.mode, TriggerMode::BecomesBlocked);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    // CR 509.3c: bare "becomes blocked" has no blocker qualifier, so
    // `valid_target` stays None — the runtime reads this as once-per-combat.
    assert!(def.valid_target.is_none());
}

/// CR 509.3d: "becomes blocked by a creature" carries a blocker qualifier and
/// triggers once per matching blocker. The parser must populate `valid_target`
/// with the (creature) blocker filter so the runtime matcher distinguishes it
/// from the bare CR 509.3c form (which fires once per combat). Regression guard
/// for the ~29 plain "by a creature" cards (Quagmire Lamprey, Order of the
/// Alabaster Host, Cave Tiger, …) that would otherwise collapse to once-per-combat.
#[test]
fn trigger_becomes_blocked_by_a_creature_sets_blocker_filter() {
    let def = parse_trigger_line(
            "Whenever Quagmire Lamprey becomes blocked by a creature, that creature gets a -1/-1 counter.",
            "Quagmire Lamprey",
        );
    assert_eq!(def.mode, TriggerMode::BecomesBlocked);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    match def.valid_target {
        Some(TargetFilter::Typed(ref tf)) => {
            assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
        }
        other => panic!("expected a creature blocker filter, got {other:?}"),
    }
}

#[test]
fn trigger_becomes_blocked_by_a_qualified_creature_preserves_blocker_filter() {
    let def = parse_trigger_line(
            "Whenever Quagmire Lamprey becomes blocked by a creature without flying, that creature gets a -1/-1 counter.",
            "Quagmire Lamprey",
        );

    assert_eq!(def.mode, TriggerMode::BecomesBlocked);
    match def.valid_target {
        Some(TargetFilter::Typed(ref tf)) => {
            assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
            assert!(
                    tf.properties
                        .iter()
                        .any(|p| matches!(p, FilterProp::WithoutKeyword { value } if *value == Keyword::Flying)),
                    "expected WithoutKeyword(Flying) in {:?}",
                    tf.properties
                );
        }
        other => panic!("expected a qualified creature blocker filter, got {other:?}"),
    }
}

#[test]
fn trigger_becomes_blocked_by_two_or_more_creatures_stays_bare() {
    let def = parse_trigger_line(
        "Whenever Vicious Battlerager becomes blocked by two or more creatures, draw a card.",
        "Vicious Battlerager",
    );

    assert_eq!(def.mode, TriggerMode::BecomesBlocked);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert!(
        def.valid_target.is_none(),
        "threshold wording must not be treated as a per-blocker qualifier"
    );
}

#[test]
fn trigger_becomes_blocked_pump_scales_with_creatures_blocking_it() {
    let def = parse_trigger_line(
            "Whenever this creature becomes blocked, it gets +2/+2 until end of turn for each creature blocking it.",
            "Gang of Elk",
        );

    assert_eq!(def.mode, TriggerMode::BecomesBlocked);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    let execute = def.execute.as_ref().expect("trigger should have effect");
    assert_eq!(execute.duration, Some(Duration::UntilEndOfTurn));
    match execute.effect.as_ref() {
        Effect::Pump {
            power,
            toughness,
            target,
        } => {
            assert_eq!(target, &TargetFilter::SelfRef);
            for value in [power, toughness] {
                match value {
                    PtValue::Quantity(QuantityExpr::Multiply { factor, inner }) => {
                        assert_eq!(*factor, 2);
                        match inner.as_ref() {
                            QuantityExpr::Ref {
                                qty: QuantityRef::ObjectCount { filter },
                            } => match filter {
                                TargetFilter::Typed(tf) => {
                                    assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
                                    assert_eq!(tf.properties, vec![FilterProp::BlockingSource]);
                                }
                                other => panic!("expected Typed filter, got {other:?}"),
                            },
                            other => panic!("expected ObjectCount ref, got {other:?}"),
                        }
                    }
                    other => panic!("expected scaled dynamic P/T value, got {other:?}"),
                }
            }
        }
        other => panic!("expected Pump, got {other:?}"),
    }
}

#[test]
fn trigger_becomes_blocked_pump_scales_with_blockers_beyond_first() {
    let def = parse_trigger_line(
            "Whenever this creature becomes blocked, it gets -2/-1 until end of turn for each creature blocking it beyond the first.",
            "Johtull Wurm",
        );

    assert_eq!(def.mode, TriggerMode::BecomesBlocked);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    let execute = def.execute.as_ref().expect("trigger should have effect");
    assert_eq!(execute.duration, Some(Duration::UntilEndOfTurn));
    match execute.effect.as_ref() {
        Effect::Pump {
            power,
            toughness,
            target,
        } => {
            assert_eq!(target, &TargetFilter::SelfRef);
            assert_eq!(
                power,
                &PtValue::Quantity(QuantityExpr::Multiply {
                    factor: -2,
                    inner: Box::new(blocking_source_beyond_first_expr()),
                })
            );
            assert_eq!(
                toughness,
                &PtValue::Quantity(QuantityExpr::Multiply {
                    factor: -1,
                    inner: Box::new(blocking_source_beyond_first_expr()),
                })
            );
        }
        other => panic!("expected Pump, got {other:?}"),
    }
}

#[test]
fn trigger_one_or_more_creatures_become_blocked() {
    // Hezrou (CLB): "Whenever one or more creatures you control become blocked,
    // each blocking creature gets -1/-1 until end of turn."
    let def = parse_trigger_line(
            "Whenever one or more creatures you control become blocked, each blocking creature gets -1/-1 until end of turn.",
            "Hezrou",
        );
    assert_eq!(def.mode, TriggerMode::BecomesBlocked);
    assert!(def.batched, "one or more triggers must be batched");
    match &def.valid_card {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
            assert_eq!(tf.controller, Some(ControllerRef::You));
        }
        other => panic!("expected Typed creature filter with controller You, got {other:?}"),
    }
    let execute = def.execute.as_ref().expect("trigger should have effect");
    assert_eq!(execute.duration, Some(Duration::UntilEndOfTurn));
    match execute.effect.as_ref() {
        Effect::PumpAll {
            power,
            toughness,
            target,
        } => {
            assert_eq!(*power, PtValue::Fixed(-1));
            assert_eq!(*toughness, PtValue::Fixed(-1));
            match target {
                TargetFilter::Typed(tf) => {
                    assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
                    assert!(tf.properties.contains(&FilterProp::Blocking));
                }
                other => panic!("expected Typed blocking creature filter, got {other:?}"),
            }
        }
        other => panic!("expected PumpAll, got {other:?}"),
    }
}

#[test]
fn trigger_is_dealt_damage() {
    let def = parse_trigger_line(
        "Whenever Spitemare is dealt damage, it deals that much damage to any target.",
        "Spitemare",
    );
    assert_eq!(def.mode, TriggerMode::DamageReceived);
    assert_eq!(def.damage_kind, DamageKindFilter::Any);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert_eq!(def.valid_target, None);
}

#[test]
fn trigger_is_dealt_combat_damage() {
    let def = parse_trigger_line(
        "Whenever ~ is dealt combat damage, draw a card.",
        "Some Card",
    );
    assert_eq!(def.mode, TriggerMode::DamageReceived);
    assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
}

#[test]
fn trigger_source_deals_damage_to_self() {
    // Phyrexian Obliterator (NPH/DMR): "Whenever a source deals damage to this creature,
    // that source's controller sacrifices that many permanents."
    let def = parse_trigger_line(
            "Whenever a source deals damage to this creature, that source's controller sacrifices that many permanents of their choice.",
            "Phyrexian Obliterator",
        );
    assert_eq!(def.mode, TriggerMode::DamageReceived);
    assert_eq!(def.damage_kind, DamageKindFilter::Any);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

#[test]
fn trigger_you_attack() {
    let def = parse_trigger_line(
        "Whenever you attack, create a 1/1 white Soldier creature token.",
        "Some Card",
    );
    assert_eq!(def.mode, TriggerMode::YouAttack);
}

// CR 508.1 + CR 603.7c: a delayed "Whenever you attack this turn" trigger is
// prefix-stripped to the bare condition "you attack" before reaching
// `parse_trigger_condition`. Bare "you attack" must resolve to YouAttack, not
// Unknown — the #433 root cause (Dalkovan Encampment).
#[test]
fn trigger_condition_bare_you_attack() {
    let mut ctx = ParseContext::default();
    let (mode, _def) = parse_trigger_condition("you attack", &mut ctx);
    assert_eq!(mode, TriggerMode::YouAttack);
}

// The word-boundary guard: "you attacked this turn" is a condition, not a
// trigger, and must NOT be recognized as a YouAttack trigger.
#[test]
fn trigger_condition_you_attacked_is_not_a_trigger() {
    let mut ctx = ParseContext::default();
    let (mode, _def) = parse_trigger_condition("you attacked this turn", &mut ctx);
    assert_ne!(mode, TriggerMode::YouAttack);
}

// CR 603.7c: the full Dalkovan Encampment activated ability — the inner
// "Whenever you attack this turn, ..." clause is an effect-body delayed
// trigger, so it builds a CreateDelayedTrigger whose WheneverEvent trigger
// has mode YouAttack (previously Unknown — the #433 bug).
#[test]
fn trigger_dalkovan_encampment_delayed_you_attack() {
    use crate::parser::oracle::parse_oracle_text;

    let parsed = parse_oracle_text(
        "{2}{W}, {T}: Whenever you attack this turn, create two 1/1 red \
             Warrior creature tokens that are tapped and attacking. Sacrifice \
             them at the beginning of the next end step.",
        "Dalkovan Encampment",
        &[],
        &["Land".to_string()],
        &[],
    );
    assert_eq!(parsed.abilities.len(), 1);

    let delayed_effect = parsed.abilities[0].effect.as_ref();
    let Effect::CreateDelayedTrigger { condition, .. } = delayed_effect else {
        panic!("expected CreateDelayedTrigger, got {delayed_effect:?}");
    };
    let DelayedTriggerCondition::WheneverEvent { trigger } = condition else {
        panic!("expected WheneverEvent, got {condition:?}");
    };
    assert_eq!(trigger.mode, TriggerMode::YouAttack);

    // CR 603.7c + CR 513.1: the sacrifice cleanup must nest under the token
    // creator inside the WheneverEvent delayed trigger, not as a sibling
    // activated sub registered at ability activation time (issue #2433).
    let Effect::CreateDelayedTrigger { effect: inner, .. } = delayed_effect else {
        unreachable!();
    };
    assert!(
        matches!(&*inner.effect, Effect::Token { .. }),
        "WheneverEvent inner effect must be Token, got {:?}",
        inner.effect
    );
    let sacrifice_delayed = inner
        .sub_ability
        .as_deref()
        .expect("token creator must chain to end-step sacrifice delayed trigger");
    let Effect::CreateDelayedTrigger {
        condition: cleanup_condition,
        effect: cleanup_effect,
        ..
    } = sacrifice_delayed.effect.as_ref()
    else {
        panic!(
            "expected nested CreateDelayedTrigger sacrifice, got {:?}",
            sacrifice_delayed.effect
        );
    };
    assert_eq!(
        *cleanup_condition,
        DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
    );
    assert!(
        matches!(
            &*cleanup_effect.effect,
            Effect::Sacrifice {
                target: TargetFilter::LastCreated,
                ..
            }
        ),
        "sacrifice them must rewrite to LastCreated, got {:?}",
        cleanup_effect.effect
    );
    assert!(
        parsed.abilities[0].sub_ability.is_none(),
        "sacrifice cleanup must not remain a sibling sub of the outer delayed trigger"
    );
}

#[test]
fn trigger_becomes_tapped() {
    let def = parse_trigger_line(
            "Whenever Night Market Lookout becomes tapped, each opponent loses 1 life and you gain 1 life.",
            "Night Market Lookout",
        );
    assert_eq!(def.mode, TriggerMode::Taps);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

/// CR 701.26 + CR 603.2c: Batched plural form "become tapped" for
/// "Whenever one or more nontoken Merfolk you control become tapped" —
/// Deeproot Pilgrimage.
#[test]
fn trigger_one_or_more_become_tapped() {
    let def = parse_trigger_line(
            "Whenever one or more nontoken Merfolk you control become tapped, create a 1/1 blue Merfolk creature token with hexproof.",
            "Deeproot Pilgrimage",
        );
    assert_eq!(def.mode, TriggerMode::Taps);
    assert!(def.batched);
    // valid_card should be a Typed filter for nontoken Merfolk you control
    match &def.valid_card {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            // type_filters should contain the Merfolk subtype (no core type needed —
            // "Merfolk" alone identifies the creature subtype per CR 205.3m).
            assert!(
                tf.type_filters
                    .contains(&TypeFilter::Subtype("Merfolk".to_string())),
                "expected Subtype(Merfolk) in type_filters, got {:?}",
                tf.type_filters
            );
            // Should have NonToken property
            assert!(
                tf.properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::NonToken)),
                "expected NonToken property, got {:?}",
                tf.properties
            );
        }
        other => panic!("expected Typed filter, got {other:?}"),
    }
}

#[test]
fn trigger_you_cast_this_spell() {
    let def = parse_trigger_line(
            "When you cast this spell, draw cards equal to the greatest power among creatures you control.",
            "Hydroid Krasis",
        );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert!(def.trigger_zones.contains(&Zone::Stack));
}

/// CR 601.2i + CR 603.2: A self-cast trigger phrased as
/// "When you cast <CARDNAME>" must produce the same `TriggerMode::SpellCast`
/// shape (with `valid_card = SelfRef`) as the canonical "When you cast this
/// spell" templating. After `normalize_card_name_refs` (CR 201.5), the card
/// name becomes `~`, so the parser sees "when you cast ~" — a class-level
/// pattern covering every aura/permanent whose cast trigger references
/// itself by name.
#[test]
fn trigger_you_cast_self_by_name() {
    let def = parse_trigger_line(
        "When you cast Taught by Surrak, draw a card.",
        "Taught by Surrak",
    );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert!(def.trigger_zones.contains(&Zone::Stack));
}

/// CR 601.2i + CR 603.2: Mirror of `trigger_you_cast_self_by_name` for the
/// "Whenever" prefix. Both keywords are valid cast-time trigger phrasings
/// (CR 603.1) and must produce identical SpellCast definitions.
#[test]
fn trigger_whenever_you_cast_self_by_name() {
    let def = parse_trigger_line("Whenever you cast Test Card, draw a card.", "Test Card");
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert!(def.trigger_zones.contains(&Zone::Stack));
}

/// CR 601.2i + CR 603.2: Passive "Whenever ~ is cast" phrasing — the third
/// cell of the {when, whenever} × {you cast this spell, you cast ~, ~ is
/// cast} cross-product. Exercises the composed `alt × alt` combinator path.
#[test]
fn trigger_whenever_self_is_cast() {
    let def = parse_trigger_line("Whenever Test Card is cast, draw a card.", "Test Card");
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert!(def.trigger_zones.contains(&Zone::Stack));
}

#[test]
fn trigger_opponent_casts_multicolored_spell() {
    let def = parse_trigger_line(
        "Whenever an opponent casts a multicolored spell, you gain 1 life.",
        "Soldier of the Pantheon",
    );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        ))
    );
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(TypedFilter::default().properties(
            vec![FilterProp::ColorCount {
                comparator: Comparator::GE,
                count: 2,
            }]
        )))
    );
}

#[test]
fn trigger_mana_cannons_uses_triggering_spell_color_count() {
    let def = parse_trigger_line(
            "Whenever you cast a multicolored spell, this enchantment deals X damage to any target, where X is the number of colors that spell is.",
            "Mana Cannons",
        );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Card).properties(vec![FilterProp::ColorCount {
                comparator: Comparator::GE,
                count: 2,
            }])
        ))
    );
    let execute = def.execute.as_deref().expect("trigger should execute");
    match execute.effect.as_ref() {
        Effect::DealDamage { amount, target, .. } => {
            assert_eq!(*target, TargetFilter::Any);
            assert_eq!(
                *amount,
                QuantityExpr::Ref {
                    qty: QuantityRef::ObjectColorCount {
                        scope: ObjectScope::EventSource
                    }
                }
            );
        }
        other => panic!("expected DealDamage, got {other:?}"),
    }
}

#[test]
fn trigger_you_cast_aura_spell() {
    let def = parse_trigger_line(
        "Whenever you cast an Aura spell, you may draw a card.",
        "Kor Spiritdancer",
    );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    // Must restrict to Aura subtype
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::default().subtype("Aura".to_string())
        ))
    );
    // Must restrict to controller's spells
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

/// CR 601.2 + CR 702.33d: "Whenever you cast a kicked spell" is a
/// SpellCast trigger whose `valid_card` gates on the cast-time kicked
/// snapshot, not an unrestricted any-spell trigger.
#[test]
fn trigger_you_cast_kicked_spell_filters_valid_card() {
    let parsed = parse_oracle_text(
        "Flying\nWhenever you cast a kicked spell, scry 2.",
        "Merfolk Falconer",
        &[],
        &["Creature".to_string()],
        &["Merfolk".to_string(), "Wizard".to_string()],
    );

    let def = parsed
        .triggers
        .first()
        .expect("Merfolk Falconer should have a SpellCast trigger");
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert!(matches!(
        def.valid_card,
        Some(TargetFilter::Typed(TypedFilter { ref properties, .. }))
            if properties.contains(&FilterProp::WasKicked)
    ));

    let execute = def.execute.as_deref().expect("trigger should execute");
    match execute.effect.as_ref() {
        Effect::Scry { count, target } => {
            assert_eq!(*count, QuantityExpr::Fixed { value: 2 });
            assert_eq!(*target, TargetFilter::Controller);
        }
        other => panic!("expected Scry 2, got {other:?}"),
    }
}

#[test]
fn trigger_a_player_casts_spell_they_dont_own() {
    let def = parse_trigger_line(
        "Whenever a player casts a spell they don't own, that player creates a Treasure token.",
        "Gonti, Night Minister",
    );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(def.valid_target, Some(TargetFilter::TriggeringPlayer));
    assert_owned_by_opponent(
        def.valid_card
            .as_ref()
            .expect("spell they don't own must carry valid_card"),
    );
}

#[test]
fn trigger_you_cast_spell_you_dont_own() {
    let def = parse_trigger_line(
            "Whenever you cast a spell you don't own, put a +1/+1 counter on each creature you control.",
            "Nita, Forum Conciliator",
        );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert_owned_by_opponent(
        def.valid_card
            .as_ref()
            .expect("spell you don't own must carry valid_card"),
    );
}

#[test]
fn trigger_you_cast_instant_or_sorcery_spell_you_dont_own() {
    let def = parse_trigger_line(
        "Whenever you cast an instant or sorcery spell you don't own, draw a card.",
        "Nita, Forum Conciliator",
    );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    let valid_card = def
        .valid_card
        .as_ref()
        .expect("instant or sorcery spell you don't own must carry valid_card");
    assert_owned_by_opponent(valid_card);
}

#[test]
fn trigger_you_cast_creature_spell() {
    let def = parse_trigger_line(
        "Whenever you cast a creature spell, draw a card.",
        "Beast Whisperer",
    );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)))
    );
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn trigger_you_cast_a_spell_no_type() {
    let def = parse_trigger_line("Whenever you cast a spell, add {C}.", "Conduit of Ruin");
    assert_eq!(def.mode, TriggerMode::SpellCast);
    // No type restriction
    assert!(def.valid_card.is_none());
    // But still restricted to controller
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    // No origin clause → no zone restriction (CR 601.2a).
    assert_eq!(def.spell_cast_origin, OriginConstraint::Any);
}

#[test]
fn trigger_you_cast_spell_during_opponents_turn() {
    let def = parse_trigger_line(
            "Whenever you cast a spell during an opponent's turn, put a -1/-1 counter on up to one target creature.",
            "Nightmare Sower",
        );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert!(def.valid_card.is_none());
    assert_eq!(
        def.constraint,
        Some(TriggerConstraint::OnlyDuringOpponentsTurn)
    );
}

#[test]
fn spell_cast_turn_constraint_peel_rejects_phase_or_step_tails() {
    let (payload, constraint) =
        peel_trailing_turn_constraint("spell during an opponent's end step");
    assert_eq!(payload, "spell during an opponent's end step");
    assert!(constraint.is_none());
}

// CR 601.1a + CR 701.18b: "Whenever you play a card" fires on playing a land
// or casting a spell. Recycle and Null Profusion both read "Whenever you play
// a card, draw a card." — classify as the unified `PlayCard` mode.
#[test]
fn trigger_you_play_a_card_draw() {
    let def = parse_trigger_line("Whenever you play a card, draw a card.", "Recycle");
    assert_eq!(def.mode, TriggerMode::PlayCard);
    // "you" → controller-scoped.
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    // No card-type restriction — any card played.
    assert!(def.valid_card.is_none());
    let execute = def.execute.as_ref().expect("trigger should have an effect");
    assert!(
        matches!(*execute.effect, Effect::Draw { .. }),
        "expected Draw effect, got {:?}",
        execute.effect
    );
}

#[test]
fn trigger_you_plays_a_card_does_not_match_play_card() {
    let def = parse_trigger_line("Whenever you plays a card, draw a card.", "Malformed");
    assert_ne!(def.mode, TriggerMode::PlayCard);
}

#[test]
fn trigger_you_cast_target_player_mill_instead_keeps_chosen_player() {
    let def = parse_trigger_line(
            "Whenever you cast an instant or sorcery spell, target player mills three cards. If five or more mana was spent to cast that spell, that player mills ten cards instead.",
            "Exhibition Tidecaller",
        );

    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));

    let execute = def.execute.as_ref().expect("trigger should have an effect");
    match &*execute.effect {
        Effect::Mill { target, .. } => assert_eq!(*target, TargetFilter::Player),
        other => panic!("expected base target-player Mill, got {other:?}"),
    }

    let instead = execute
        .sub_ability
        .as_ref()
        .expect("mv>=5 clause should lower as an instead sub-ability");
    assert!(
        matches!(
            instead.condition,
            Some(AbilityCondition::ConditionInstead { .. })
        ),
        "mv>=5 clause should be a ConditionInstead override"
    );
    match &*instead.effect {
        Effect::Mill { target, .. } => assert_eq!(*target, TargetFilter::ParentTarget),
        other => panic!("expected instead target-player Mill, got {other:?}"),
    }
}

/// CR 601.2h + CR 603.4: "at least N mana was spent to cast it" is an
/// intervening-if on a spell-cast trigger. Must hoist as a trigger-level
/// QuantityComparison so the trigger only fires when the threshold is met.
/// Regression: The Emperor of Palamecia (#1490).
#[test]
fn trigger_at_least_mana_spent_intervening_if() {
    let def = parse_trigger_line(
            "Whenever you cast a noncreature spell, if at least four mana was spent to cast it, put a +1/+1 counter on ~. Then if it has three or more +1/+1 counters on it, transform it.",
            "The Emperor of Palamecia",
        );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    // The intervening-if must hoist to a trigger-level condition.
    let cond = def
        .condition
        .as_ref()
        .expect("at-least-four mana threshold must hoist as trigger condition");
    match cond {
            TriggerCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ManaSpentToCast {
                                // CR 400.7d: "it" on a spell-cast trigger denotes
                                // the triggering spell, NOT the source permanent.
                                scope: CastManaObjectScope::TriggeringSpell,
                                metric: crate::types::ability::CastManaSpentMetric::Total,
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 4 },
            } => {}
            other => panic!(
                "expected ManaSpentToCast {{ TriggeringSpell, Total }} >= 4 trigger condition, got {other:?}"
            ),
        }
    // The effect text should have the condition stripped.
    let execute = def.execute.as_ref().unwrap();
    match &*execute.effect {
        Effect::PutCounter { .. } => {}
        other => panic!("expected PutCounter effect, got {other:?}"),
    }
}

/// CR 601.2a + #538: Ghostly Pilferer. The cast-origin discriminator
/// "from anywhere other than their hand" must survive parsing as
/// `NotEquals(Hand)`; without it the trigger fires on every opponent
/// hand cast.
#[test]
fn trigger_opponent_casts_from_anywhere_other_than_hand() {
    let def = parse_trigger_line(
        "Whenever an opponent casts a spell from anywhere other than their hand, draw a card.",
        "Ghostly Pilferer",
    );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        ))
    );
    assert_eq!(
        def.spell_cast_origin,
        OriginConstraint::NotEquals(Zone::Hand)
    );
}

/// CR 601.2a — positive cast-origin form. Snapcaster-class triggers
/// must parse as `Equals(Graveyard)`.
#[test]
fn trigger_you_cast_a_spell_from_your_graveyard() {
    let def = parse_trigger_line(
        "Whenever you cast a spell from your graveyard, draw a card.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert_eq!(
        def.spell_cast_origin,
        OriginConstraint::Equals(Zone::Graveyard)
    );
}

/// CR 205.2a + CR 601.2: "whenever you cast an artifact creature spell" must
/// AND both core types into `valid_card`, so a non-creature artifact spell
/// does NOT fire the trigger. Regression for Lux Artillery, whose spell-cast
/// trigger incorrectly accepted any artifact spell.
#[test]
fn trigger_you_cast_artifact_creature_spell() {
    let def = parse_trigger_line(
        "Whenever you cast an artifact creature spell, it gains sunburst.",
        "Lux Artillery",
    );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    let Some(TargetFilter::Typed(tf)) = &def.valid_card else {
        panic!("expected Typed valid_card, got {:?}", def.valid_card);
    };
    assert!(
        tf.type_filters.contains(&TypeFilter::Artifact),
        "expected Artifact in {:?}",
        tf.type_filters
    );
    assert!(
        tf.type_filters.contains(&TypeFilter::Creature),
        "expected Creature in {:?}",
        tf.type_filters
    );
}

/// CR 205.2a + CR 205.4a + CR 601.2: "whenever you cast a legendary creature
/// spell" — supertype lives in properties, not type_filters.
#[test]
fn trigger_you_cast_legendary_creature_spell() {
    let def = parse_trigger_line(
        "Whenever you cast a legendary creature spell, draw a card.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    let Some(TargetFilter::Typed(tf)) = &def.valid_card else {
        panic!("expected Typed valid_card, got {:?}", def.valid_card);
    };
    assert!(tf.type_filters.contains(&TypeFilter::Creature));
    assert!(
        tf.properties.iter().any(|prop| matches!(
            prop,
            FilterProp::HasSupertype {
                value: crate::types::card_type::Supertype::Legendary
            }
        )),
        "expected HasSupertype(Legendary) in {:?}",
        tf.properties
    );
}

/// CR 205.2a + CR 205.4b + CR 601.2: "whenever you cast a noncreature
/// artifact spell" — Non(Creature) + Artifact conjunction.
#[test]
fn trigger_you_cast_noncreature_artifact_spell() {
    let def = parse_trigger_line(
        "Whenever you cast a noncreature artifact spell, draw a card.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    let Some(TargetFilter::Typed(tf)) = &def.valid_card else {
        panic!("expected Typed valid_card, got {:?}", def.valid_card);
    };
    assert!(tf.type_filters.contains(&TypeFilter::Artifact));
    assert!(
        tf.type_filters
            .contains(&TypeFilter::Non(Box::new(TypeFilter::Creature))),
        "expected Non(Creature) in {:?}",
        tf.type_filters
    );
}

/// CR 603.4: "Whenever you cast another Vampire spell" — the "another"
/// prefix must produce `FilterProp::Another` on `valid_card` so the trigger
/// does not fire on the source spell itself (Edgar Markov).
#[test]
fn trigger_you_cast_another_vampire_spell() {
    let def = parse_trigger_line(
            "Whenever you cast another Vampire spell, if ~ is in the command zone or on the battlefield, create a 1/1 black Vampire creature token.",
            "Edgar Markov",
        );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    let Some(TargetFilter::Typed(tf)) = &def.valid_card else {
        panic!("expected Typed valid_card, got {:?}", def.valid_card);
    };
    assert!(
        tf.properties.contains(&FilterProp::Another),
        "expected Another in {:?}",
        tf.properties
    );
    assert!(
        tf.type_filters
            .iter()
            .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "Vampire")),
        "expected Subtype(Vampire) in {:?}",
        tf.type_filters
    );
}

/// Issue #817 — CR 113.6b + CR 207.2c: Eminence (ability word) triggers
/// function in both the command zone and on the battlefield. The "if ~ is
/// in the command zone or on the battlefield" intervening-if parses to
/// `TriggerCondition::Or { [SourceInZone(Command), SourceInZone(Battlefield)] }`,
/// and the trigger's `trigger_zones` MUST list both — otherwise the
/// runtime trigger scanner only walks the battlefield and silently
/// skips a command-zone Edgar Markov (the original bug).
#[test]
fn trigger_eminence_command_zone_or_battlefield_extends_trigger_zones() {
    let def = parse_trigger_line(
            "Whenever you cast another Vampire spell, if ~ is in the command zone or on the battlefield, create a 1/1 black Vampire creature token.",
            "Edgar Markov",
        );
    assert!(
        def.trigger_zones.contains(&Zone::Command),
        "Eminence trigger must scan the command zone, got {:?}",
        def.trigger_zones,
    );
    assert!(
        def.trigger_zones.contains(&Zone::Battlefield),
        "Eminence trigger must also scan the battlefield, got {:?}",
        def.trigger_zones,
    );
}

/// Issue #817 follow-up: Oloro, Ageless Ascetic exercises the same
/// Eminence intervening-if from a different mode path (BeginningOfPhase,
/// not SpellCast). Confirms the trigger-zones derivation block runs
/// regardless of which `try_parse_*` arm produced the trigger.
#[test]
fn trigger_eminence_oloro_upkeep_command_zone_or_battlefield() {
    let def = parse_trigger_line(
            "At the beginning of your upkeep, if ~ is in the command zone or on the battlefield, you gain 1 life.",
            "Oloro, Ageless Ascetic",
        );
    assert!(
        def.trigger_zones.contains(&Zone::Command),
        "Oloro Eminence upkeep trigger must scan the command zone, got {:?}",
        def.trigger_zones,
    );
    assert!(
        def.trigger_zones.contains(&Zone::Battlefield),
        "Oloro Eminence upkeep trigger must also scan the battlefield, got {:?}",
        def.trigger_zones,
    );
}

/// CR 113.6b: A trigger whose intervening-if mentions no zone clause
/// at all leaves the `trigger_zones` derivation to fall through to the
/// `make_base()` `[Battlefield]` default — Eminence's `Or`-of-zones is
/// the only path that adds non-battlefield zones. Locks in that the
/// multi-zone refactor doesn't widen the scan for unrelated triggers.
#[test]
fn trigger_no_zone_condition_defaults_to_battlefield_only() {
    let def = parse_trigger_line("Whenever you cast a spell, draw a card.", "Test No Zone");
    assert!(
        def.trigger_zones.contains(&Zone::Battlefield),
        "no-zone trigger must default-scan battlefield, got {:?}",
        def.trigger_zones,
    );
    assert!(
        !def.trigger_zones.contains(&Zone::Command),
        "no-zone trigger must NOT pick up Command, got {:?}",
        def.trigger_zones,
    );
    assert!(
        !def.trigger_zones.contains(&Zone::Graveyard),
        "no-zone trigger must NOT pick up Graveyard, got {:?}",
        def.trigger_zones,
    );
}

#[test]
fn trigger_you_cast_another_spell_keeps_another_filter() {
    let def = parse_trigger_line("Whenever you cast another spell, draw a card.", "Test");
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    let Some(TargetFilter::Typed(tf)) = &def.valid_card else {
        panic!("expected Typed valid_card, got {:?}", def.valid_card);
    };
    assert!(
        tf.properties.contains(&FilterProp::Another),
        "expected Another in {:?}",
        tf.properties
    );
}

/// CR 603.4 + CR 122.1: "at the beginning of your end step, if there are
/// thirty or more counters among artifacts and creatures you control, ..."
/// — intervening-if with counter-count condition that sums across every
/// counter type on the matching permanents. Regression for Lux Artillery's
/// second trigger, which previously produced `condition: null` and fired
/// every end step unconditionally.
#[test]
fn trigger_intervening_if_counters_among_filter() {
    let def = parse_trigger_line(
            "At the beginning of your end step, if there are thirty or more counters among artifacts and creatures you control, this artifact deals 10 damage to each opponent.",
            "Lux Artillery",
        );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::End));
    let Some(TriggerCondition::QuantityComparison {
        lhs,
        comparator,
        rhs,
    }) = &def.condition
    else {
        panic!(
            "expected QuantityComparison intervening-if, got {:?}",
            def.condition
        );
    };
    assert_eq!(*comparator, Comparator::GE);
    assert_eq!(*rhs, QuantityExpr::Fixed { value: 30 });
    let QuantityExpr::Ref {
        qty:
            QuantityRef::CountersOnObjects {
                counter_type,
                filter,
            },
    } = lhs
    else {
        panic!("expected CountersOnObjects lhs, got {lhs:?}");
    };
    assert!(
        counter_type.is_none(),
        "expected any-counter-type (None), got {counter_type:?}"
    );
    // Filter should be an Or of (artifact you control) ∪ (creature you control).
    let TargetFilter::Or { filters } = filter else {
        panic!("expected Or filter for 'artifacts and creatures you control', got {filter:?}");
    };
    assert_eq!(filters.len(), 2);
    assert!(filters.iter().any(|f| matches!(
        f,
        TargetFilter::Typed(tf)
            if tf.type_filters.contains(&TypeFilter::Artifact)
                && tf.controller == Some(ControllerRef::You)
    )));
    assert!(filters.iter().any(|f| matches!(
        f,
        TargetFilter::Typed(tf)
            if tf.type_filters.contains(&TypeFilter::Creature)
                && tf.controller == Some(ControllerRef::You)
    )));
}

/// CR 603.4 + CR 701.9: "at the beginning of each end step, if an opponent
/// discarded a card this turn, ..." — intervening-if must be hoisted as a
/// scoped `CardsDiscardedThisTurn` quantity comparison. Regression
/// for Tinybones, Trinket Thief (previously `condition: null`).
#[test]
fn trigger_intervening_if_opponent_discarded_this_turn() {
    let def = parse_trigger_line(
            "At the beginning of each end step, if an opponent discarded a card this turn, you draw a card and you lose 1 life.",
            "Tinybones, Trinket Thief",
        );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::End));
    let Some(TriggerCondition::QuantityComparison {
        lhs,
        comparator,
        rhs,
    }) = &def.condition
    else {
        panic!(
            "expected QuantityComparison intervening-if, got {:?}",
            def.condition
        );
    };
    assert_eq!(*comparator, Comparator::GE);
    assert_eq!(*rhs, QuantityExpr::Fixed { value: 1 });
    assert_eq!(
        *lhs,
        QuantityExpr::Ref {
            qty: QuantityRef::CardsDiscardedThisTurn {
                player: PlayerScope::Opponent {
                    aggregate: AggregateFunction::Sum,
                },
            },
        }
    );
}

/// Issue #5143 — Anje Falkenrath: "Whenever you discard a card, if it has
/// madness, untap Anje Falkenrath." The madness intervening-if must gate on
/// the discarded card, not fire on every discard.
#[test]
fn trigger_intervening_if_discarded_card_has_madness() {
    use crate::types::ability::FilterProp;
    use crate::types::keywords::KeywordKind;

    let def = parse_trigger_line(
        "Whenever you discard a card, if it has madness, untap Anje Falkenrath.",
        "Anje Falkenrath",
    );
    assert_eq!(def.mode, TriggerMode::Discarded);
    let Some(TriggerCondition::EventObjectMatchesFilter { filter }) = &def.condition else {
        panic!(
            "expected EventObjectMatchesFilter intervening-if, got {:?}",
            def.condition
        );
    };
    let TargetFilter::Typed(tf) = filter else {
        panic!("expected typed madness filter, got {filter:?}");
    };
    assert!(
        tf.properties.iter().any(|prop| matches!(
            prop,
            FilterProp::HasKeywordKind {
                value: KeywordKind::Madness
            }
        )),
        "expected madness keyword filter, got {:?}",
        tf.properties
    );
}

/// Issue #551 — The Raven Man: "At the beginning of each end step, if a
/// player discarded a card this turn, create a 1/1 black Bird ...". The
/// "a player" (any player) intervening-if must be hoisted as an all-players
/// `CardsDiscardedThisTurn` comparison; before this fix the condition was
/// dropped and the bird was created every end step regardless of discards.
#[test]
fn trigger_intervening_if_a_player_discarded_this_turn() {
    let def = parse_trigger_line(
            "At the beginning of each end step, if a player discarded a card this turn, create a 1/1 black Bird creature token with flying.",
            "The Raven Man",
        );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::End));
    let Some(TriggerCondition::QuantityComparison {
        lhs,
        comparator,
        rhs,
    }) = &def.condition
    else {
        panic!(
            "expected QuantityComparison intervening-if, got {:?}",
            def.condition
        );
    };
    assert_eq!(*comparator, Comparator::GE);
    assert_eq!(*rhs, QuantityExpr::Fixed { value: 1 });
    assert_eq!(
        *lhs,
        QuantityExpr::Ref {
            qty: QuantityRef::CardsDiscardedThisTurn {
                player: PlayerScope::AllPlayers {
                    aggregate: AggregateFunction::Sum,
                    exclude: None,
                },
            },
        }
    );
}

#[test]
fn trigger_intervening_if_card_left_your_graveyard_this_turn() {
    let def = parse_trigger_line(
        "At the beginning of your end step, if a card left your graveyard this turn, draw a card.",
        "Primary Research",
    );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::End));
    let Some(TriggerCondition::QuantityComparison {
        lhs,
        comparator,
        rhs,
    }) = &def.condition
    else {
        panic!(
            "expected QuantityComparison intervening-if, got {:?}",
            def.condition
        );
    };
    assert_eq!(*comparator, Comparator::GE);
    assert_eq!(*rhs, QuantityExpr::Fixed { value: 1 });
    assert!(matches!(
        lhs,
        QuantityExpr::Ref {
            qty: QuantityRef::ZoneChangeCountThisTurn {
                from: Some(Zone::Graveyard),
                to: None,
                ..
            }
        }
    ));
}

#[test]
fn trigger_intervening_if_permanent_was_put_into_your_hand_from_battlefield() {
    let def = parse_trigger_line(
            "At the beginning of your end step, if a permanent was put into your hand from the battlefield this turn, draw a card.",
            "Barrin, Tolarian Archmage",
        );
    let Some(TriggerCondition::QuantityComparison {
        lhs,
        comparator,
        rhs,
    }) = &def.condition
    else {
        panic!(
            "expected QuantityComparison intervening-if, got {:?}",
            def.condition
        );
    };
    assert_eq!(*comparator, Comparator::GE);
    assert_eq!(*rhs, QuantityExpr::Fixed { value: 1 });
    assert!(matches!(
        lhs,
        QuantityExpr::Ref {
            qty: QuantityRef::ZoneChangeCountThisTurn {
                from: Some(Zone::Battlefield),
                to: Some(Zone::Hand),
                ..
            }
        }
    ));
}

#[test]
fn trigger_intervening_if_artifact_or_creature_was_put_into_graveyard_from_battlefield() {
    let def = parse_trigger_line(
            "At the beginning of your end step, if an artifact or creature was put into a graveyard from the battlefield this turn, put a +1/+1 counter on this creature.",
            "Ichor Shade",
        );
    assert!(matches!(
        def.condition,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ZoneChangeCountThisTurn {
                    from: Some(Zone::Battlefield),
                    to: Some(Zone::Graveyard),
                    filter: TargetFilter::Or { .. },
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        })
    ));
}

#[test]
fn trigger_intervening_if_source_dealt_damage_to_opponent_this_turn() {
    let def = parse_trigger_line(
            "At the beginning of each end step, if this creature dealt damage to an opponent this turn, put a +1/+1 counter on it.",
            "Dunerider Outlaw",
        );
    let Some(TriggerCondition::QuantityComparison {
        lhs,
        comparator,
        rhs,
    }) = &def.condition
    else {
        panic!(
            "expected QuantityComparison intervening-if, got {:?}",
            def.condition
        );
    };
    assert_eq!(*comparator, Comparator::GE);
    assert_eq!(*rhs, QuantityExpr::Fixed { value: 1 });
    assert!(matches!(
        lhs,
        QuantityExpr::Ref {
            qty: QuantityRef::DamageDealtThisTurn {
                source,
                ..
            }
        } if **source == TargetFilter::SelfRef
    ));
}

#[test]
fn trigger_intervening_if_source_was_dealt_damage_this_turn() {
    let def = parse_trigger_line(
            "At the beginning of each end step, if this creature was dealt damage this turn, put a +0/+1 counter on it.",
            "Wall of Resistance",
        );
    assert!(matches!(
        def.condition,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::DamageDealtThisTurn {
                    source,
                    target,
                    ..
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        }) if *source == TargetFilter::Any && *target == TargetFilter::SelfRef
    ));
}

/// CR 120.10 + CR 603.4: Maarika, Brutal Gladiator's "Whenever ~ deals
/// damage to a creature, if that creature was dealt excess damage this turn"
/// must hoist the excess-damage clause as a `TriggerCondition::QuantityComparison`
/// with `channel: Excess`, not silently drop it (condition: null).
#[test]
fn trigger_intervening_if_that_creature_was_dealt_excess_damage_this_turn() {
    let def = parse_trigger_line(
            "Whenever Maarika deals damage to a creature, if that creature was dealt excess damage this turn, that creature's controller sacrifices a noncreature, nonland permanent.",
            "Maarika, Brutal Gladiator",
        );
    assert!(
        matches!(
            def.condition,
            Some(TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::DamageDealtThisTurn {
                        channel: DamageChannel::Excess,
                        ..
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            })
        ),
        "expected excess-damage intervening-if, got: {:?}",
        def.condition
    );
}

/// CR 120.10 + CR 603.4: Rith, Liberated Primeval's phase trigger with an
/// opponent-scoped excess-damage intervening-if must set `channel: Excess`
/// and produce a non-trivial target filter. `parse_type_phrase` emits
/// `TargetFilter::Or` for compound types, so we check the channel and
/// that the condition is a QuantityComparison with DamageDealtThisTurn.
#[test]
fn trigger_intervening_if_opponent_creature_or_planeswalker_excess_damage_this_turn() {
    let def = parse_trigger_line(
            "At the beginning of your end step, if a creature or planeswalker an opponent controlled was dealt excess damage this turn, create a 4/4 red Dragon creature token with flying.",
            "Rith, Liberated Primeval",
        );
    let Some(TriggerCondition::QuantityComparison {
        lhs:
            QuantityExpr::Ref {
                qty:
                    QuantityRef::DamageDealtThisTurn {
                        ref target,
                        channel,
                        ..
                    },
            },
        comparator: Comparator::GE,
        rhs: QuantityExpr::Fixed { value: 1 },
    }) = def.condition
    else {
        panic!(
            "expected QuantityComparison(DamageDealtThisTurn), got: {:?}",
            def.condition
        );
    };
    assert_eq!(
        channel,
        DamageChannel::Excess,
        "channel must be Excess for Rith's trigger"
    );
    assert!(
        !matches!(target.as_ref(), TargetFilter::Any),
        "target filter must be non-Any, got: {target:?}"
    );
}

#[test]
fn trigger_intervening_if_you_were_dealt_damage_threshold_this_turn() {
    let def = parse_trigger_line(
            "At the beginning of each end step, if you were dealt 4 or more damage this turn, exile this artifact.",
            "Boarded Window",
        );
    assert!(matches!(
        def.condition,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::DamageDealtThisTurn {
                    source,
                    target,
                    ..
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 4 },
        }) if *source == TargetFilter::Any
            && matches!(
                &*target,
                TargetFilter::Typed(ref typed)
                    if typed.controller == Some(ControllerRef::You)
            )
    ));
}

#[test]
fn trigger_intervening_if_counter_was_put_on_owned_permanent_this_turn() {
    let def = parse_trigger_line(
            "At the beginning of each end step, if a +1/+1 counter was put on a permanent under your control this turn, put a +1/+1 counter on this creature.",
            "Fairgrounds Trumpeter",
        );
    assert!(matches!(
        def.condition,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::CounterAddedThisTurn {
                    actor: CountScope::All,
                    counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                    ..
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        })
    ));
}

// --- ControlCount condition tests ---

#[test]
fn trigger_leonin_vanguard_control_creature_count() {
    let def = parse_trigger_line(
            "At the beginning of combat on your turn, if you control three or more creatures, this creature gets +1/+1 until end of turn and you gain 1 life.",
            "Leonin Vanguard",
        );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::BeginCombat));
    assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
    assert!(
        matches!(
            def.condition,
            Some(TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount { .. },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            })
        ),
        "Expected QuantityComparison with ObjectCount >= 3, got {:?}",
        def.condition
    );
    // Effect: pump self +1/+1 with life gain sub_ability
    let exec = def.execute.as_ref().expect("should have execute");
    assert!(matches!(
        *exec.effect,
        Effect::Pump {
            power: PtValue::Fixed(1),
            toughness: PtValue::Fixed(1),
            target: TargetFilter::SelfRef,
        }
    ));
    assert_eq!(exec.duration, Some(Duration::UntilEndOfTurn));
    // Sub-ability: gain 1 life
    let sub = exec.sub_ability.as_ref().expect("should have sub_ability");
    assert!(matches!(
        *sub.effect,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            ..
        }
    ));
}

#[test]
fn extract_if_control_creature_count() {
    let (cleaned, cond) = extract_if_condition(
        "if you control three or more creatures, ~ gets +1/+1 until end of turn",
    );
    assert_eq!(cleaned, "~ gets +1/+1 until end of turn");
    // The canonical combinator produces QuantityComparison with ObjectCount.
    let cond = cond.expect("should have condition");
    assert!(
        matches!(
            cond,
            TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount { .. },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            }
        ),
        "Expected QuantityComparison with ObjectCount >= 3, got {cond:?}"
    );
}

// --- Equipment / Aura subject filter tests ---

#[test]
fn trigger_equipped_creature_attacks() {
    let def = parse_trigger_line(
        "Whenever equipped creature attacks, put a +1/+1 counter on it.",
        "Blackblade Reforged",
    );
    assert_eq!(def.mode, TriggerMode::Attacks);
    assert_eq!(def.valid_card, Some(TargetFilter::AttachedTo));
}

#[test]
fn trigger_equipped_creature_deals_combat_damage() {
    let def = parse_trigger_line(
        "Whenever equipped creature deals combat damage to a player, draw a card.",
        "Shadowspear",
    );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    assert_eq!(def.valid_source, Some(TargetFilter::AttachedTo));
    assert_eq!(def.valid_target, Some(TargetFilter::Player));
}

#[test]
fn trigger_equipped_creature_dies() {
    let def = parse_trigger_line(
        "Whenever equipped creature dies, you gain 2 life.",
        "Strider Harness",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.origin, Some(Zone::Battlefield));
    assert_eq!(def.destination, Some(Zone::Graveyard));
    assert_eq!(def.valid_card, Some(TargetFilter::AttachedTo));
}

#[test]
fn trigger_heirloom_blade_reveal_until_shares_creature_type() {
    let def = parse_trigger_line(
            "Whenever equipped creature dies, reveal cards from the top of your library until you reveal a creature card that shares a creature type with it, then you may put that card into your hand and the rest on the bottom of your library in a random order.",
            "Heirloom Blade",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    let Effect::RevealUntil { filter, .. } = def.execute.as_ref().unwrap().effect.as_ref() else {
        panic!("expected RevealUntil, got {:?}", def.execute);
    };
    let TargetFilter::Typed(tf) = filter else {
        panic!("expected Typed filter, got {filter:?}");
    };
    assert!(tf.type_filters.contains(&TypeFilter::Creature));
    assert!(tf.properties.iter().any(|p| matches!(
        p,
        FilterProp::SharesQuality {
            quality: SharedQuality::CreatureType,
            reference: Some(reference),
            ..
        } if matches!(reference.as_ref(), TargetFilter::TriggeringSource)
    )));
}

#[test]
fn trigger_enchanted_creature_attacks() {
    let def = parse_trigger_line(
        "Whenever enchanted creature attacks, draw a card.",
        "Curiosity",
    );
    assert_eq!(def.mode, TriggerMode::Attacks);
    assert_eq!(def.valid_card, Some(TargetFilter::AttachedTo));
}

#[test]
fn trigger_enchanted_creature_dies() {
    let def = parse_trigger_line(
        "Whenever enchanted creature dies, return ~ to its owner's hand.",
        "Angelic Destiny",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.origin, Some(Zone::Battlefield));
    assert_eq!(def.destination, Some(Zone::Graveyard));
    assert_eq!(def.valid_card, Some(TargetFilter::AttachedTo));
}

// CR 303.4 + CR 603.10a: "Whenever an enchanted creature dies" with the
// indefinite article is NON-source-relative (the source isn't the Aura).
// The subject filter must be a typed creature with `EnchantedBy` so runtime
// interprets it as "has any Aura attached" (Hateful Eidolon).
#[test]
fn trigger_an_enchanted_creature_dies_hateful_eidolon() {
    let def = parse_trigger_line(
            "Whenever an enchanted creature dies, draw a card for each Aura you controlled that was attached to it.",
            "Hateful Eidolon",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.origin, Some(Zone::Battlefield));
    assert_eq!(def.destination, Some(Zone::Graveyard));
    let expected =
        TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]));
    assert_eq!(def.valid_card, Some(expected));
}

// CR 701.60b: "suspected creatures" as a type-phrase subject in a batched attack
// trigger. Covers Clandestine Meddler, Frantic Scapegoat, and any card that
// watches for one or more suspected creatures attacking.
#[test]
fn trigger_one_or_more_suspected_creatures_attack_clandestine_meddler() {
    let def = parse_trigger_line(
        "Whenever one or more suspected creatures you control attack, surveil 1.",
        "Clandestine Meddler",
    );
    assert_eq!(def.mode, TriggerMode::YouAttack);
    assert!(def.batched, "expected batched trigger");
    let filter = def.valid_card.as_ref().expect("valid_card set");
    match filter {
        TargetFilter::Typed(tf) => {
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "expected Creature type filter; got {:?}",
                tf.type_filters
            );
            assert!(
                tf.properties.contains(&FilterProp::Suspected),
                "expected Suspected property; got {:?}",
                tf.properties
            );
        }
        other => panic!("expected Typed filter, got {other:?}"),
    }
}

// CR 303.4: "Creatures that are enchanted by an Aura you control" subject filter.
#[test]
fn trigger_one_or_more_enchanted_creatures_attack_killian() {
    let def = parse_trigger_line(
            "Whenever one or more creatures that are enchanted by an Aura you control attack, draw a card.",
            "Killian, Decisive Mentor",
        );
    assert_eq!(def.mode, TriggerMode::YouAttack);
    // CR 303.4e + CR 506.2: the attachment-relation clause ("enchanted by an
    // Aura you control") makes the attacking-player gate pass-through
    // (`valid_target == Player`) — the enchanted attacker may be
    // opponent-controlled — WITHOUT any attacked-target narrowing
    // (`attack_target_filter == None`), since this text has no "attack a
    // player" restriction. (#3314)
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Player),
        "attachment-relation subject ⇒ permissive attacking-player pass-through"
    );
    assert_eq!(
        def.attack_target_filter, None,
        "no \"attack a player\" clause ⇒ no attacked-target narrowing"
    );
    let filter = def.valid_card.as_ref().expect("valid_card set");
    match filter {
        TargetFilter::Typed(tf) => {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            let has_attachment = tf.properties.iter().any(|p| {
                matches!(
                    p,
                    FilterProp::HasAttachment {
                        kind: AttachmentKind::Aura,
                        controller: Some(ControllerRef::You),
                        ..
                    }
                )
            });
            assert!(
                has_attachment,
                "expected HasAttachment(Aura, You); got {:?}",
                tf.properties
            );
        }
        other => panic!("expected Typed filter, got {other:?}"),
    }
}

#[test]
fn trigger_cycle_this_card() {
    let def = parse_trigger_line(
        "When you cycle this card, draw a card.",
        "Decree of Justice",
    );
    assert_eq!(def.mode, TriggerMode::Cycled);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert!(def.trigger_zones.contains(&Zone::Graveyard));
}

#[test]
fn trigger_cycle_self_ref() {
    let def = parse_trigger_line(
        "When you cycle ~, you may draw a card.",
        "Decree of Justice",
    );
    assert_eq!(def.mode, TriggerMode::Cycled);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert!(def.trigger_zones.contains(&Zone::Graveyard));
    assert!(def.optional);
}

#[test]
fn trigger_cycle_another_card() {
    // CR 702.29: "Whenever you cycle another card" — Drannith Stinger
    let def = parse_trigger_line(
        "Whenever you cycle another card, this creature deals 1 damage to each opponent.",
        "Drannith Stinger",
    );
    assert_eq!(def.mode, TriggerMode::Cycled);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert!(matches!(
        &def.valid_card,
        Some(TargetFilter::Typed(tf)) if tf.properties.contains(&FilterProp::Another)
    ));
}

#[test]
fn trigger_cycle_or_discard_a_card() {
    // CR 702.29d: "Whenever you cycle or discard a card" — Drake Haven
    let def = parse_trigger_line(
            "Whenever you cycle or discard a card, you may pay {1}. If you do, create a 2/2 blue Drake creature token with flying.",
            "Drake Haven",
        );
    assert_eq!(def.mode, TriggerMode::CycledOrDiscarded);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn trigger_cycle_or_discard_another_card() {
    // CR 702.29d: "Whenever you cycle or discard another card" — Horror of the Broken Lands
    let def = parse_trigger_line(
        "Whenever you cycle or discard another card, this creature gets +2/+1 until end of turn.",
        "Horror of the Broken Lands",
    );
    assert_eq!(def.mode, TriggerMode::CycledOrDiscarded);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert!(matches!(
        &def.valid_card,
        Some(TargetFilter::Typed(tf)) if tf.properties.contains(&FilterProp::Another)
    ));
}

#[test]
fn trigger_when_you_cast_this_spell_if_youve_cast_another_spell_this_turn() {
    let def = parse_trigger_line(
        "When you cast this spell, if you've cast another spell this turn, copy it.",
        "Sage of the Skies",
    );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert_eq!(def.trigger_zones, vec![Zone::Stack]);
    assert_eq!(
        def.condition,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::SpellsCastThisTurn {
                    scope: crate::types::ability::CountScope::Controller,
                    filter: None
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 2 },
        })
    );
}

#[test]
fn trigger_attack_if_cast_spell_with_mana_value_this_turn() {
    let def = parse_trigger_line(
            "Whenever this creature attacks, if you've cast a spell with mana value 4 or greater this turn, draw a card.",
            "Rhino, Barreling Brute",
        );
    assert_eq!(def.mode, TriggerMode::Attacks);
    assert_eq!(
        def.condition,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::SpellsCastThisTurn {
                    scope: crate::types::ability::CountScope::Controller,
                    filter: Some(TargetFilter::Typed(TypedFilter::card().properties(vec![
                        FilterProp::Cmc {
                            comparator: Comparator::GE,
                            value: QuantityExpr::Fixed { value: 4 },
                        }
                    ]))),
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        })
    );
}

#[test]
fn trigger_end_step_if_cast_both_creature_and_noncreature_this_turn() {
    let def = parse_trigger_line(
            "At the beginning of each end step, if you've cast both a creature spell and a noncreature spell this turn, create a Clue token.",
            "Fae Offering",
        );
    assert_eq!(def.mode, TriggerMode::Phase);
    match def.condition {
        Some(TriggerCondition::And { conditions }) => {
            assert_eq!(conditions.len(), 2);
            assert!(conditions.iter().any(|condition| matches!(
                condition,
                TriggerCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::SpellsCastThisTurn {
                            scope: crate::types::ability::CountScope::Controller,
                            filter: Some(TargetFilter::Typed(TypedFilter { type_filters, .. })),
                        },
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 1 },
                } if type_filters == &vec![TypeFilter::Creature]
            )));
            assert!(conditions.iter().any(|condition| matches!(
                condition,
                TriggerCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::SpellsCastThisTurn {
                            scope: crate::types::ability::CountScope::Controller,
                            filter: Some(TargetFilter::Typed(TypedFilter { type_filters, .. })),
                        },
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 1 },
                } if type_filters == &vec![TypeFilter::Non(Box::new(TypeFilter::Creature))]
            )));
        }
        other => panic!("expected compound trigger condition, got {other:?}"),
    }
}

#[test]
fn trigger_opponent_draws_a_card() {
    let def = parse_trigger_line(
        "Whenever an opponent draws a card, you gain 1 life.",
        "Underworld Dreams",
    );
    assert_eq!(def.mode, TriggerMode::Drawn);
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        ))
    );
}

/// CR 702.5a + CR 303.4 + CR 121.1: "Whenever enchanted player draws a card"
/// (Curse of Fool's Wisdom) lowers to a Drawn trigger scoped to the enchanted
/// player via `AttachedTo` — NOT `TriggerMode::Unknown` (which never fires).
#[test]
fn trigger_enchanted_player_draws_is_attached_to() {
    let def = parse_trigger_line(
        "Whenever enchanted player draws a card, they lose 2 life and you gain 2 life.",
        "Curse of Fool's Wisdom",
    );
    assert_eq!(def.mode, TriggerMode::Drawn);
    assert_eq!(def.valid_target, Some(TargetFilter::AttachedTo));
}

/// CR 702.5a: "Whenever enchanted opponent draws a card" (Psychic Possession).
/// "Enchant opponent" already restricts the attachment, so bare `AttachedTo`
/// resolves the enchanted opponent at runtime.
#[test]
fn trigger_enchanted_opponent_draws_is_attached_to() {
    let def = parse_trigger_line(
        "Whenever enchanted opponent draws a card, you may draw a card.",
        "Psychic Possession",
    );
    assert_eq!(def.mode, TriggerMode::Drawn);
    assert_eq!(def.valid_target, Some(TargetFilter::AttachedTo));
}

/// Containment guard: the "enchanted player/opponent" recognition lives ONLY
/// in the draws path, so "Whenever enchanted player is dealt damage" (Grievous
/// Wound) stays honestly `Unknown` rather than flipping to a parses-but-dead
/// `DamageReceived` trigger with `valid_card = AttachedTo` (which the runtime
/// rejects for player-recipient damage, so it would never fire).
#[test]
fn trigger_enchanted_player_is_dealt_damage_stays_unknown() {
    let def = parse_trigger_line(
        "Whenever enchanted player is dealt damage, they lose 2 life.",
        "Grievous Wound",
    );
    assert!(
        matches!(def.mode, TriggerMode::Unknown(_)),
        "expected Unknown, got {:?}",
        def.mode
    );
}

/// CR 603.4 + CR 102.1: "draws a card during their turn" — the trailing
/// timing tail restricts the trigger to the drawing player's own turn
/// (issue #403 defect 2a).
#[test]
fn trigger_draws_a_card_during_their_turn_attaches_actors_turn_timing() {
    let def = parse_trigger_line(
        "Whenever a player draws a card during their turn, you draw a card.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::Drawn);
    assert_eq!(
        def.condition,
        Some(TriggerCondition::DuringPlayersTurn {
            player: PlayerFilter::TriggeringPlayer,
        }),
        "the 'during their turn' tail must become a DuringPlayersTurn intervening-if"
    );
}

/// CR 603.4 + CR 102.1: "gains life during their turn" — same timing-tail
/// composition for the life-gain predicate (issue #403 defect 2a).
#[test]
fn trigger_gains_life_during_their_turn_attaches_actors_turn_timing() {
    let def = parse_trigger_line(
        "Whenever a player gains life during their turn, you draw a card.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::LifeGained);
    assert_eq!(
        def.condition,
        Some(TriggerCondition::DuringPlayersTurn {
            player: PlayerFilter::TriggeringPlayer,
        })
    );
}

/// Regression guard: a plain draw trigger with no timing tail must remain
/// unconditional.
#[test]
fn trigger_draws_a_card_without_timing_tail_has_no_condition() {
    let def = parse_trigger_line(
        "Whenever a player draws a card, you draw a card.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::Drawn);
    assert_eq!(def.condition, None);
}

#[test]
fn trigger_you_draw_a_card_scopes_to_controller() {
    // CR 121.1 + CR 603.2: Sheoldred's first trigger must scope to the
    // controller so it does not fire on opponent draws.
    let def = parse_trigger_line(
        "Whenever you draw a card, you gain 2 life.",
        "Sheoldred, the Apocalypse",
    );
    assert_eq!(def.mode, TriggerMode::Drawn);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn trigger_opponent_loses_life_exquisite_blood() {
    // CR 119.3 + CR 603.2 + CR 603.7c: Exquisite Blood — opponent-scoped
    // life-loss trigger whose effect reads "that much" from the event.
    let def = parse_trigger_line(
        "Whenever an opponent loses life, you gain that much life.",
        "Exquisite Blood",
    );
    assert_eq!(def.mode, TriggerMode::LifeLost);
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        ))
    );
    // Effect amount should be the triggering event's amount, not Fixed 1.
    let execute = def.execute.as_ref().expect("execute ability present");
    match &*execute.effect {
        crate::types::ability::Effect::GainLife { amount, player } => {
            assert_eq!(
                amount,
                &QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                "Exquisite Blood: gain amount must reference event's life-loss amount"
            );
            // CR 608.2k: "you gain" — effect-level subject is the ability
            // controller. Must NOT be rerouted to the trigger's opponent
            // subject by a post-hoc `player_scope` override.
            assert_eq!(
                player,
                &crate::types::ability::TargetFilter::Controller,
                "Exquisite Blood: 'you gain' recipient must be the ability controller"
            );
        }
        other => panic!("expected GainLife effect, got {other:?}"),
    }
    // CR 113.3c + CR 608.2k: The effect's subject ("you") is the single
    // authority for who gains life. There must be NO `player_scope` on the
    // execute ability — that field was historically (mis)used by a
    // post-hoc rewire to redirect "you gain" effects to the triggering
    // opponent, which is exactly the regression this guards against.
    assert!(
        execute.player_scope.is_none(),
        "Exquisite Blood: execute.player_scope must be None — the effect-level \
             subject is authoritative. Found {:?}",
        execute.player_scope,
    );
}

#[test]
fn trigger_you_cycle_a_card() {
    let def = parse_trigger_line("Whenever you cycle a card, draw a card.", "Drake Haven");
    assert_eq!(def.mode, TriggerMode::Cycled);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn trigger_you_lose_life() {
    let def = parse_trigger_line(
        "Whenever you lose life, create a 1/1 token.",
        "Unholy Annex",
    );
    assert_eq!(def.mode, TriggerMode::LifeLost);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn trigger_you_lose_life_during_your_turn() {
    let def = parse_trigger_line(
        "Whenever you lose life during your turn, draw a card.",
        "Bloodtracker",
    );
    assert_eq!(def.mode, TriggerMode::LifeLost);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
}

#[test]
fn trigger_one_or_more_opponents_each_lose_exactly_one_life() {
    // CR 119.3 + CR 603.2c: Ob Nixilis, Captive Kingpin — batched
    // "one or more opponents each" subject plus an "exactly 1" magnitude
    // constraint on the life-loss event.
    let def = parse_trigger_line(
            "Whenever one or more opponents each lose exactly 1 life, put a +1/+1 counter on this creature.",
            "Ob Nixilis, Captive Kingpin",
        );
    assert_eq!(def.mode, TriggerMode::LifeLost);
    assert!(
        def.batched,
        "CR 603.2c: 'one or more … each' is a batched trigger"
    );
    assert_eq!(
        def.life_amount,
        Some((Comparator::EQ, 1)),
        "the 'exactly 1' qualifier must constrain the life-loss magnitude"
    );
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        ))
    );
}

#[test]
fn trigger_one_or_more_opponents_each_draw_generalizes() {
    // CR 603.2c: Prove the "each" seam-move generalizes to non-life verbs.
    // The distributive "each" is stripped at the subject seam (parse_single_subject),
    // so parse_draws_card sees "draw a card" directly without any "each " prefix.
    let def = parse_trigger_line(
        "Whenever one or more opponents each draw a card, you gain 1 life.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::Drawn);
    assert!(
        def.batched,
        "CR 603.2c: 'one or more … each' is a batched trigger"
    );
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        ))
    );
}

#[test]
fn trigger_life_loss_or_more_threshold() {
    // CR 119.3: the magnitude qualifier composes with "N or more" too — the
    // same building block that powers "exactly N", proving the class is
    // generalized rather than special-cased to one card.
    let def = parse_trigger_line(
        "Whenever an opponent loses 3 or more life, draw a card.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::LifeLost);
    assert_eq!(def.life_amount, Some((Comparator::GE, 3)));
}

#[test]
fn trigger_plain_life_loss_has_no_amount_constraint() {
    // Regression: an unqualified life-loss trigger must leave `life_amount`
    // None so every loss fires it.
    let def = parse_trigger_line(
        "Whenever an opponent loses life, you gain that much life.",
        "Exquisite Blood",
    );
    assert_eq!(def.mode, TriggerMode::LifeLost);
    assert_eq!(def.life_amount, None);
}

#[test]
fn trigger_you_gain_or_lose_life_during_your_turn() {
    let def = parse_trigger_line(
            "Whenever you gain or lose life during your turn, this creature gets +1/+0 until end of turn.",
            "Wax-Wane Witness",
        );
    assert_eq!(def.mode, TriggerMode::LifeChanged);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    // CR 603.4 + CR 102.1: "during your turn" becomes an intervening-if
    // condition so it composes with a separate rate-limit constraint.
    assert_eq!(
        def.condition,
        Some(TriggerCondition::DuringPlayersTurn {
            player: PlayerFilter::Controller,
        })
    );
}

/// CR 603.4: Moonstone Harbinger — "during your turn" + "only once each turn"
/// must both survive lowering. The turn restriction is a condition; the rate
/// limit is a constraint.
#[test]
fn trigger_you_gain_or_lose_life_during_your_turn_once_each_turn() {
    let def = parse_trigger_line(
            "Whenever you gain or lose life during your turn, Bats you control get +1/+0 and gain deathtouch until end of turn. This ability triggers only once each turn.",
            "Moonstone Harbinger",
        );
    assert_eq!(def.mode, TriggerMode::LifeChanged);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert_eq!(
        def.condition,
        Some(TriggerCondition::DuringPlayersTurn {
            player: PlayerFilter::Controller,
        })
    );
    assert_eq!(def.constraint, Some(TriggerConstraint::OncePerTurn));
}

#[test]
fn trigger_you_gain_or_lose_life_no_turn_constraint() {
    let def = parse_trigger_line(
        "Whenever you gain or lose life, put a +1/+1 counter on this creature.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::LifeChanged);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert_eq!(def.condition, None);
    assert_eq!(def.constraint, None);
}

#[test]
fn trigger_opponent_gains_or_loses_life_scopes_to_opponent() {
    let def = parse_trigger_line(
        "Whenever an opponent gains or loses life during their turn, draw a card.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::LifeChanged);
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        ))
    );
    assert_eq!(
        def.condition,
        Some(TriggerCondition::DuringPlayersTurn {
            player: PlayerFilter::TriggeringPlayer,
        })
    );
}

#[test]
fn trigger_a_player_gains_or_loses_life_is_unscoped() {
    let def = parse_trigger_line(
        "Whenever a player gains or loses life, each opponent loses 1 life.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::LifeChanged);
    assert_eq!(def.valid_target, Some(TargetFilter::Player));
    assert_eq!(def.condition, None);
}

#[test]
fn trigger_you_get_one_or_more_energy() {
    let def = parse_trigger_line(
        "Whenever you get one or more {E}, you get an additional {E}.",
        "Fabrication Module",
    );
    assert_eq!(def.mode, TriggerMode::CounterPlayerAddedAll);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert!(def.batched);
}

#[test]
fn trigger_spell_or_ability_you_control_counters_a_spell() {
    let def = parse_trigger_line(
            "Whenever a spell or ability you control counters a spell, you may tap or untap target permanent.",
            "Lullmage Mentor",
        );
    assert_eq!(def.mode, TriggerMode::Countered);
    assert_eq!(
        def.valid_source,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You)
        ))
    );
}

#[test]
fn trigger_spell_or_ability_opponent_controls_counters_a_spell() {
    let def = parse_trigger_line(
        "Whenever a spell or ability an opponent controls counters a spell, draw a card.",
        "Hypothetical Counter Watcher",
    );
    assert_eq!(def.mode, TriggerMode::Countered);
    assert_eq!(
        def.valid_source,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        ))
    );
}

#[test]
fn trigger_a_spell_youve_cast_is_countered() {
    // CR 701.6a + CR 108.4: Multani's Presence -- the passive dual of the
    // countering-side arm. The trigger fires when a spell YOU control leaves
    // the stack via a counter, so it gates the *countered* spell through
    // `valid_card` (the `SpellCountered` event's `object_id`), not the
    // countering source through `valid_source`.
    let def = parse_trigger_line(
        "Whenever a spell you've cast is countered, draw a card.",
        "Multani's Presence",
    );
    assert_eq!(def.mode, TriggerMode::Countered);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You)
        ))
    );
    // The passive form must NOT populate `valid_source` (that gates the
    // countering source, the active-side semantics).
    assert_eq!(def.valid_source, None);
}

#[test]
fn trigger_a_spell_you_control_is_countered() {
    // CR 108.4: the plain "you control" possessive is synonymous with
    // "you've cast" for a spell (a spell's controller is its caster), so it
    // routes to the same `ControllerRef::You` `valid_card` filter.
    let def = parse_trigger_line(
        "Whenever a spell you control is countered, draw a card.",
        "Hypothetical Own-Spell Countered Watcher",
    );
    assert_eq!(def.mode, TriggerMode::Countered);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You)
        ))
    );
}

#[test]
fn trigger_you_sacrifice_a_creature() {
    let def = parse_trigger_line(
        "Whenever you sacrifice a creature, draw a card.",
        "Morbid Opportunist",
    );
    assert_eq!(def.mode, TriggerMode::Sacrificed);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You)
        ))
    );
}

#[test]
fn trigger_a_player_sacrifices_a_permanent() {
    // CR 603 + CR 701.21: "a player sacrifices" → any-player scope (no controller filter).
    let def = parse_trigger_line(
        "Whenever a player sacrifices a permanent, put a +1/+1 counter on this creature.",
        "Merchant of Venom",
    );
    assert_eq!(def.mode, TriggerMode::Sacrificed);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Permanent)))
    );
}

#[test]
fn trigger_a_player_sacrifices_another_permanent() {
    // CR 603 + CR 701.21: Mazirek — "another permanent" carries FilterProp::Another,
    // which excludes the trigger source from matching its own sacrifice.
    let def = parse_trigger_line(
            "Whenever a player sacrifices another permanent, put a +1/+1 counter on each creature you control.",
            "Mazirek, Kraul Death Priest",
        );
    assert_eq!(def.mode, TriggerMode::Sacrificed);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Permanent).properties(vec![FilterProp::Another])
        ))
    );
}

#[test]
fn trigger_an_opponent_sacrifices_a_creature() {
    // CR 603 + CR 701.21: opponent-actor sacrifice dispatch.
    let def = parse_trigger_line(
        "Whenever an opponent sacrifices a creature, you gain 1 life.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::Sacrificed);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::Opponent)
        ))
    );
}

#[test]
fn trigger_sacrifice_with_during_your_turn_constraint() {
    // CR 603.2 + CR 603.7: Szarel, Genesis Shepherd — sacrifice trigger
    // with a trailing turn constraint. The parser must extract the
    // constraint rather than reject the whole line because the subject
    // wasn't the final token.
    use crate::types::ability::TriggerConstraint;
    let def = parse_trigger_line(
        "Whenever you sacrifice another nontoken permanent during your turn, you gain 1 life.",
        "Szarel, Genesis Shepherd",
    );
    assert_eq!(def.mode, TriggerMode::Sacrificed);
    assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
}

#[test]
fn trigger_you_tap_a_land_for_mana() {
    let def = parse_trigger_line("Whenever you tap a land for mana, add {G}.", "Mana Flare");
    assert_eq!(def.mode, TriggerMode::TapsForMana);
    // CR 603.2 + CR 106.12a: "you tap a land" scopes the source filter to
    // the trigger source's controller.
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Land).controller(ControllerRef::You)
        ))
    );
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert_eq!(def.taps_for_mana_produced, None);
}

#[test]
fn trigger_you_tap_a_land_for_colorless_mana() {
    let def = parse_trigger_line(
        "Whenever you tap a land for {C}, add an additional {C}.",
        "Ultima, Origin of Oblivion",
    );
    assert_eq!(def.mode, TriggerMode::TapsForMana);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Land).controller(ControllerRef::You)
        ))
    );
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert_eq!(def.taps_for_mana_produced, Some(vec![ManaType::Colorless]));
    let execute = def.execute.as_deref().expect("trigger should execute");
    assert!(matches!(
        execute.effect.as_ref(),
        Effect::Mana {
            produced: crate::types::ability::ManaProduction::Colorless { count },
            ..
        } if matches!(count, QuantityExpr::Fixed { value: 1 })
    ));
}

#[test]
fn trigger_forbidden_orchard_targets_opponent_token_owner() {
    let def = parse_trigger_line(
            "Whenever you tap Forbidden Orchard for mana, target opponent creates a 1/1 colorless Spirit creature token.",
            "Forbidden Orchard",
        );
    assert_eq!(def.mode, TriggerMode::TapsForMana);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));

    let execute = def.execute.as_deref().expect("trigger should execute");
    let Effect::Token { owner, .. } = execute.effect.as_ref() else {
        panic!("expected Token effect, got {:?}", execute.effect);
    };
    assert_eq!(
        *owner,
        TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
    );
}

#[test]
fn trigger_player_taps_land_for_mana_adds_to_that_player() {
    let def = parse_trigger_line(
            "Whenever a player taps a land for mana, that player adds one mana of any type that land produced.",
            "Mana Flare",
        );
    assert_eq!(def.mode, TriggerMode::TapsForMana);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)))
    );
    let execute = def.execute.as_deref().unwrap();
    assert_eq!(execute.player_scope, Some(PlayerFilter::TriggeringPlayer));
    assert!(matches!(
        execute.effect.as_ref(),
        crate::types::ability::Effect::Mana {
            produced: crate::types::ability::ManaProduction::TriggerEventManaType,
            ..
        }
    ));
}

#[test]
fn fastbond_trigger_parses_land_played_with_controller_scope() {
    // CR 305.1 + CR 603.2 + CR 305.3 + CR 603.4: "whenever you play a land"
    // uses the second-person form — must map to LandPlayed scoped to Controller.
    // The intervening-if condition "if it wasn't the first land you played this
    // turn" must be hoisted as a QuantityComparison (lands_played_this_turn >= 2).
    let t = parse_trigger_line(
            "Whenever you play a land, if it wasn't the first land you played this turn, ~ deals 1 damage to you.",
            "Fastbond",
        );
    assert_eq!(t.mode, TriggerMode::LandPlayed);
    assert_eq!(t.valid_target, Some(TargetFilter::Controller));
    assert_eq!(t.valid_card, None);
    assert!(
        t.condition.is_some(),
        "intervening-if condition must be extracted"
    );
    assert!(
        matches!(
            t.condition.as_ref().unwrap(),
            TriggerCondition::QuantityComparison { .. }
        ),
        "condition must be QuantityComparison, got {:?}",
        t.condition
    );
    let execute = t.execute.expect("trigger must have an execute effect");
    assert!(
        matches!(*execute.effect, Effect::DealDamage { .. }),
        "expected DealDamage effect, got {:?}",
        execute.effect
    );
}

#[test]
fn opponent_land_play_trigger_scopes_to_opponent() {
    let t = parse_trigger_line(
        "Whenever an opponent plays a land, draw a card.",
        "Test Card",
    );
    assert_eq!(t.mode, TriggerMode::LandPlayed);
    assert!(matches!(
        t.valid_target,
        Some(TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::Opponent),
            ..
        }))
    ));
}

#[test]
fn trigger_city_of_traitors_play_another_land() {
    // CR 305.1: "When you play another land, sacrifice ~."
    // The "another" qualifier must produce FilterProp::Another on valid_card
    // so the trigger does not fire when City of Traitors itself enters.
    let t = parse_trigger_line(
        "When you play another land, sacrifice this land.",
        "City of Traitors",
    );
    assert_eq!(t.mode, TriggerMode::LandPlayed);
    assert_eq!(t.valid_target, Some(TargetFilter::Controller));
    // valid_card must contain Another
    let filter = t
        .valid_card
        .expect("valid_card must be set for 'another' qualifier");
    match &filter {
        TargetFilter::Typed(tf) => {
            assert!(
                tf.properties.contains(&FilterProp::Another),
                "expected FilterProp::Another in properties, got {:?}",
                tf.properties
            );
        }
        other => panic!("expected Typed filter with Another, got {:?}", other),
    }
}

#[test]
fn trigger_shanid_play_a_legendary_land() {
    // CR 305.1 + CR 205.4a: "Whenever you play a legendary land" must
    // produce a LandPlayed trigger with valid_card filtering for the
    // Legendary supertype on a Land type.
    let t = parse_trigger_line(
        "Whenever you play a legendary land, you draw a card and you lose 1 life.",
        "Shanid, Sleepers' Scourge",
    );
    assert_eq!(t.mode, TriggerMode::LandPlayed);
    assert_eq!(t.valid_target, Some(TargetFilter::Controller));
    let filter = t
        .valid_card
        .expect("valid_card must be set for legendary land qualifier");
    match &filter {
        TargetFilter::Typed(tf) => {
            assert!(
                tf.type_filters.contains(&TypeFilter::Land),
                "expected Land type filter, got {:?}",
                tf.type_filters
            );
            assert!(
                tf.properties.contains(&FilterProp::HasSupertype {
                    value: crate::types::card_type::Supertype::Legendary
                }),
                "expected HasSupertype(Legendary) in {:?}",
                tf.properties
            );
        }
        other => panic!("expected Typed filter, got {:?}", other),
    }
}

#[test]
fn trigger_land_play_object_phrase_uses_type_parser() {
    let island = parse_trigger_line("Whenever you play an Island, draw a card.", "Test Card");
    assert_eq!(island.mode, TriggerMode::LandPlayed);
    let filter = island
        .valid_card
        .expect("land subtype qualifier must set valid_card");
    match &filter {
        TargetFilter::Typed(tf) => {
            assert!(tf
                .type_filters
                .contains(&TypeFilter::Subtype("Island".to_string())));
        }
        other => panic!("expected Typed filter, got {:?}", other),
    }

    let from_exile = parse_trigger_line(
        "Whenever you play a land from exile, draw a card.",
        "Test Card",
    );
    assert_eq!(from_exile.mode, TriggerMode::LandPlayed);
    let filter = from_exile
        .valid_card
        .expect("from-zone qualifier must set valid_card");
    match &filter {
        TargetFilter::Typed(tf) => {
            assert!(tf.type_filters.contains(&TypeFilter::Land));
            assert!(tf
                .properties
                .contains(&FilterProp::InZone { zone: Zone::Exile }));
        }
        other => panic!("expected Typed filter, got {:?}", other),
    }
}

#[test]
fn extract_if_condition_first_land_pattern() {
    // CR 305.3 + CR 603.4: Verify the condition is stripped from the effect text.
    let (cleaned, cond) = super::extract_if_condition(
        "if it wasn't the first land you played this turn, ~ deals 1 damage to you.",
    );
    assert!(cond.is_some(), "condition must be extracted");
    assert_eq!("~ deals 1 damage to you", cleaned);
}

/// CR 701.26 + CR 603.4: Captain America, Living Legend — the first-tap
/// intervening-if lowers to `TriggerCondition::FirstTimeObjectTappedThisTurn`
/// and is stripped from the effect text, leaving the bare "untap it" effect.
#[test]
fn extract_if_condition_first_time_tapped_pattern() {
    let (cleaned, cond) = super::extract_if_condition(
        "if it's the first time that creature has become tapped this turn, untap it.",
    );
    assert_eq!(
        cond,
        Some(TriggerCondition::FirstTimeObjectTappedThisTurn),
        "first-tap intervening-if must extract FirstTimeObjectTappedThisTurn"
    );
    assert_eq!("untap it", cleaned);
}

#[test]
fn trigger_enchanted_land_is_tapped_for_mana() {
    let def = parse_trigger_line(
        "Whenever enchanted land is tapped for mana, its controller adds an additional {G}.",
        "Wild Growth",
    );
    assert_eq!(def.mode, TriggerMode::TapsForMana);
    assert_eq!(def.valid_card, Some(TargetFilter::AttachedTo));
    // CR 109.5 + CR 605.1b: "its controller" antecedent is the enchanted
    // land — the player who tapped it for mana. `PlayerFilter::TriggeringPlayer`
    // rebinds the resolving ability's controller to the ManaAdded event's
    // `player_id` so the bonus mana routes to the land's controller even
    // when the Aura is opponent-controlled.
    let execute = def.execute.as_deref().unwrap();
    assert_eq!(execute.player_scope, Some(PlayerFilter::TriggeringPlayer));
}

#[test]
fn trigger_enchanted_forest_is_tapped_for_mana_utopia_sprawl() {
    // CR 205.3i + CR 605.1b: "Whenever enchanted Forest is tapped for mana …"
    // The basic land type token ("Forest") must resolve to `AttachedTo`; the
    // Enchant keyword already constrains the aura's attach target to Forests.
    let def = parse_trigger_line(
            "Whenever enchanted Forest is tapped for mana, its controller adds an additional one mana of the chosen color.",
            "Utopia Sprawl",
        );
    assert_eq!(def.mode, TriggerMode::TapsForMana);
    assert_eq!(def.valid_card, Some(TargetFilter::AttachedTo));
    let execute = def.execute.as_deref().unwrap();
    assert_eq!(execute.player_scope, Some(PlayerFilter::TriggeringPlayer));
}

#[test]
fn trigger_fertile_ground_its_controller_adds_routes_to_triggering_player() {
    // CR 109.5 + CR 605.1b regression for the "its controller adds …"
    // Aura class (Fertile Ground, Wild Growth, Utopia Sprawl, Verdant Haven,
    // Trace of Abundance, Market Festival, Weirding Wood, Overgrowth):
    // bonus mana must go to the enchanted land's controller, not the
    // aura's controller, when an opponent ends up attaching the aura.
    let def = parse_trigger_line(
            "Whenever enchanted land is tapped for mana, its controller adds an additional one mana of any color.",
            "Fertile Ground",
        );
    assert_eq!(def.mode, TriggerMode::TapsForMana);
    assert_eq!(def.valid_card, Some(TargetFilter::AttachedTo));
    let execute = def.execute.as_deref().unwrap();
    assert_eq!(execute.player_scope, Some(PlayerFilter::TriggeringPlayer));
    assert!(matches!(
        execute.effect.as_ref(),
        crate::types::ability::Effect::Mana { .. }
    ));
}

#[test]
fn trigger_opponent_taps_land_for_mana_vorinclex() {
    // CR 603.2 + CR 605.1a: "Whenever an opponent taps <subject> for mana" —
    // source must be opponent-controlled.
    let def = parse_trigger_line(
            "Whenever an opponent taps a land for mana, that land doesn't untap during its controller's next untap step.",
            "Vorinclex, Voice of Hunger",
        );
    assert_eq!(def.mode, TriggerMode::TapsForMana);
    match def.valid_card {
        Some(TargetFilter::Typed(ref tf)) => {
            assert_eq!(tf.type_filters, vec![TypeFilter::Land]);
            assert_eq!(tf.controller, Some(ControllerRef::Opponent));
        }
        other => panic!("expected Typed(Land) with Opponent controller, got {other:?}"),
    }
}

#[test]
fn trigger_opponent_taps_land_for_mana_wars_toll_tap_all_lands() {
    let def = parse_trigger_line(
        "Whenever an opponent taps a land for mana, tap all lands that player controls.",
        "War's Toll",
    );
    assert_eq!(def.mode, TriggerMode::TapsForMana);
    let execute = def.execute.expect("War's Toll must have tap-all execute");
    match &*execute.effect {
        Effect::SetTapState {
            scope: EffectScope::All,
            state: TapStateChange::Tap,
            target: TargetFilter::Typed(ref tf),
        } => {
            assert_eq!(tf.type_filters, vec![TypeFilter::Land]);
            assert_eq!(
                tf.controller,
                Some(ControllerRef::TriggeringPlayer),
                "that player controls must bind to the tapping opponent"
            );
        }
        other => panic!("expected TapAll effect, got {other:?}"),
    }
}

#[test]
fn trigger_player_taps_land_for_mana_that_player_scope() {
    let def = parse_trigger_line(
        "Whenever a player taps a land for mana, tap all lands that player controls.",
        "Test",
    );
    assert_eq!(def.mode, TriggerMode::TapsForMana);
    let execute = def.execute.expect("must have tap-all execute");
    match &*execute.effect {
        Effect::SetTapState {
            target: TargetFilter::Typed(ref tf),
            ..
        } => {
            assert_eq!(
                tf.controller,
                Some(ControllerRef::TriggeringPlayer),
                "a player taps: that player must bind to TriggeringPlayer"
            );
        }
        other => panic!("expected TapAll effect, got {other:?}"),
    }
}

#[test]
fn trigger_you_tap_land_for_mana_that_player_scope() {
    let def = parse_trigger_line(
        "Whenever you tap a land for mana, tap all lands that player controls.",
        "Test",
    );
    assert_eq!(def.mode, TriggerMode::TapsForMana);
    let execute = def.execute.expect("must have tap-all execute");
    match &*execute.effect {
        Effect::SetTapState {
            target: TargetFilter::Typed(ref tf),
            ..
        } => {
            assert_eq!(
                tf.controller,
                Some(ControllerRef::TriggeringPlayer),
                "you tap: that player must bind to TriggeringPlayer"
            );
        }
        other => panic!("expected TapAll effect, got {other:?}"),
    }
}

#[test]
fn high_tide_delayed_trigger_taps_for_mana_mode_and_filter() {
    // CR 603.7b + CR 106.12a (issue #4673): High Tide's real Oracle text is an
    // instant that creates an until-end-of-turn delayed trigger. The inner
    // trigger condition is keyword-stripped by `try_parse_whenever_this_turn`
    // before `parse_trigger_condition` runs, so the taps-for-mana recognizer
    // must accept the missing "whenever " keyword. Reverting the `opt()` on the
    // leading keyword in `parse_taps_for_mana_actor_line` reproduces
    // `TriggerMode::Unknown`, which has no matcher and never fires.
    let parsed = parse_oracle_text(
        "Until end of turn, whenever a player taps an Island for mana, that player adds an additional {U}.",
        "High Tide",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let ability = parsed
        .abilities
        .iter()
        .find(|a| matches!(*a.effect, Effect::CreateDelayedTrigger { .. }))
        .expect("High Tide must parse a CreateDelayedTrigger");

    // Outer window is the until-end-of-turn cleanup purge (CR 603.7b).
    assert_eq!(ability.duration, Some(Duration::UntilEndOfTurn));

    let Effect::CreateDelayedTrigger {
        condition: DelayedTriggerCondition::WheneverEvent { trigger },
        effect,
        ..
    } = &*ability.effect
    else {
        panic!(
            "expected WheneverEvent delayed trigger, got {:?}",
            ability.effect
        );
    };

    // Inner trigger must be a real TapsForMana matcher, NOT the Unknown fallback.
    assert_eq!(trigger.mode, TriggerMode::TapsForMana);
    assert!(
        !matches!(trigger.mode, TriggerMode::Unknown(_)),
        "inner trigger must not be Unknown (Unknown has no matcher and never fires)"
    );

    // valid_card is the Island subtype filter with no controller constraint
    // ("a player" = any player).
    match &trigger.valid_card {
        Some(TargetFilter::Typed(tf)) => {
            assert!(
                tf.type_filters
                    .contains(&TypeFilter::Subtype("Island".to_string())),
                "valid_card must filter on Subtype(Island), got {:?}",
                tf.type_filters
            );
            assert_eq!(
                tf.controller, None,
                "\"a player\" imposes no controller constraint on the tapped land"
            );
        }
        other => panic!("expected Typed Island filter, got {other:?}"),
    }

    // Inner rider adds an ADDITIONAL blue mana.
    assert!(
        matches!(
            &*effect.effect,
            Effect::Mana {
                produced: ManaProduction::Fixed { colors, contribution: ManaContribution::Additional },
                ..
            } if colors == &vec![ManaColor::Blue]
        ),
        "inner effect must be additional blue mana, got {:?}",
        effect.effect
    );
}

#[test]
fn high_tide_delayed_trigger_that_player_binds_triggering_player() {
    // CR 608.2c + CR 106.12a (issue #4673): "that player adds an additional {U}"
    // must bind the recipient to the player who tapped the land
    // (TriggeringPlayer), not the caster. Both symptoms share the root cause:
    // `relative_player_scope_for_condition` runs on the keyword-stripped
    // condition and must recognize the taps-for-mana event to yield
    // TriggeringPlayer scope, and the subject-application must honor that scope
    // rather than defaulting to ParentTargetController. Reverting either fix
    // reproduces the wrong recipient.
    let parsed = parse_oracle_text(
        "Until end of turn, whenever a player taps an Island for mana, that player adds an additional {U}.",
        "High Tide",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let ability = parsed
        .abilities
        .iter()
        .find(|a| matches!(*a.effect, Effect::CreateDelayedTrigger { .. }))
        .expect("High Tide must parse a CreateDelayedTrigger");
    let Effect::CreateDelayedTrigger { effect, .. } = &*ability.effect else {
        unreachable!("guarded by find() above");
    };
    match &*effect.effect {
        Effect::Mana {
            target: Some(recipient),
            ..
        } => assert_eq!(
            *recipient,
            TargetFilter::TriggeringPlayer,
            "\"that player\" must bind to the triggering (tapping) player, not the caster"
        ),
        other => panic!("expected Mana with an explicit recipient, got {other:?}"),
    }
}

#[test]
fn bubbling_muck_delayed_trigger_taps_for_mana_class_general() {
    // CR 603.7b + CR 106.12a (issue #4673): Bubbling Muck is the Swamp/{B}
    // sibling of High Tide — proves the fix is class-general across permanent
    // type and mana color, not a High Tide special case.
    let parsed = parse_oracle_text(
        "Until end of turn, whenever a player taps a Swamp for mana, that player adds an additional {B}.",
        "Bubbling Muck",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    let ability = parsed
        .abilities
        .iter()
        .find(|a| matches!(*a.effect, Effect::CreateDelayedTrigger { .. }))
        .expect("Bubbling Muck must parse a CreateDelayedTrigger");
    let Effect::CreateDelayedTrigger {
        condition: DelayedTriggerCondition::WheneverEvent { trigger },
        effect,
        ..
    } = &*ability.effect
    else {
        panic!("expected WheneverEvent delayed trigger");
    };
    assert_eq!(trigger.mode, TriggerMode::TapsForMana);
    match &trigger.valid_card {
        Some(TargetFilter::Typed(tf)) => assert!(tf
            .type_filters
            .contains(&TypeFilter::Subtype("Swamp".to_string()))),
        other => panic!("expected Typed Swamp filter, got {other:?}"),
    }
    assert!(matches!(
        &*effect.effect,
        Effect::Mana {
            produced: ManaProduction::Fixed { colors, contribution: ManaContribution::Additional },
            target: Some(TargetFilter::TriggeringPlayer),
            ..
        } if colors == &vec![ManaColor::Black]
    ));
}

#[test]
fn trigger_you_tap_land_for_mana_vorinclex_first_half() {
    // CR 603.2 + CR 106.12a: "you tap a land" arm scopes the source filter to
    // ControllerRef::You — the trigger fires only on lands the controller taps.
    let def = parse_trigger_line(
        "Whenever you tap a land for mana, add one mana of any type that land produced.",
        "Vorinclex, Voice of Hunger",
    );
    assert_eq!(def.mode, TriggerMode::TapsForMana);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    match def.valid_card {
        Some(TargetFilter::Typed(ref tf)) => {
            assert_eq!(tf.type_filters, vec![TypeFilter::Land]);
            assert_eq!(tf.controller, Some(ControllerRef::You));
        }
        other => panic!("expected Typed(Land) with You controller, got {other:?}"),
    }
}

#[test]
fn trigger_nth_spell_second() {
    let def = parse_trigger_line(
        "Whenever you cast your second spell each turn, draw a card.",
        "Spectral Sailor",
    );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(
        def.constraint,
        Some(TriggerConstraint::NthSpellThisTurn { n: 2, filter: None })
    );
}

#[test]
fn trigger_nth_spell_with_filter_constrains_triggering_spell() {
    let def = parse_trigger_line(
        "Whenever you cast your second creature spell each turn, draw a card.",
        "Some Card",
    );
    let filter = TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature));
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(def.valid_card, Some(filter.clone()));
    assert_eq!(
        def.constraint,
        Some(TriggerConstraint::NthSpellThisTurn {
            n: 2,
            filter: Some(filter),
        })
    );
}

#[test]
fn trigger_vengevine_intervening_if_maps_to_nth_creature_spell_constraint() {
    let def = parse_trigger_line(
            "Whenever you cast a spell, if it's the second creature spell you cast this turn, you may return this card from your graveyard to the battlefield.",
            "Vengevine",
        );
    let filter = TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature));
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(def.valid_card, Some(filter.clone()));
    assert_eq!(
        def.constraint,
        Some(TriggerConstraint::NthSpellThisTurn {
            n: 2,
            filter: Some(filter),
        })
    );
    assert_eq!(def.trigger_zones, vec![Zone::Graveyard]);
    assert!(def.optional);
}

/// CR 601.2a + CR 603.4: Alania's disjunctive "first-of-type this turn"
/// intervening-if lowers to `Or` of composed `And(TriggeringSpellMatchesFilter,
/// SpellsCastThisTurn == n)` disjuncts — NOT a bundled ordinal variant — and the
/// residual "you may have target opponent draw a card. If you do, copy that
/// spell" parses for free (optional Draw + preserved gated CopySpell).
///
/// DISCRIMINATION: reverting the `parse_disjunctive_first_spell_intervening_if`
/// wire-in restores `Effect::Unimplemented { name: "the", .. }` at the top of the
/// chain and drops `def.condition` to `None`, flipping both the condition-equality
/// assertion and the zero-Unimplemented reach-guard.
#[test]
fn trigger_alania_disjunctive_first_of_type_intervening_if() {
    let def = parse_trigger_line(
        "Whenever you cast a spell, if it's the first instant spell, the first sorcery spell, or the first Otter spell other than Alania you've cast this turn, you may have target opponent draw a card. If you do, copy that spell. You may choose new targets for the copy.",
        "Alania, Divergent Storm",
    );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    // Fires on ANY spell you cast; the disjunctive Or gates it — valid_card stays None.
    assert_eq!(def.valid_card, None);

    let instant = type_only_filter("instant").expect("instant type filter");
    let sorcery = type_only_filter("sorcery").expect("sorcery type filter");
    let otter = type_only_filter("otter").expect("otter subtype filter");
    let otter_excl = TargetFilter::And {
        filters: vec![
            otter,
            TargetFilter::Not {
                filter: Box::new(TargetFilter::Named {
                    name: "Alania, Divergent Storm".to_string(),
                }),
            },
        ],
    };
    let disjunct = |filter: TargetFilter| TriggerCondition::And {
        conditions: vec![
            TriggerCondition::TriggeringSpellMatchesFilter {
                filter: filter.clone(),
            },
            TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::SpellsCastThisTurn {
                        scope: CountScope::Controller,
                        filter: Some(filter),
                    },
                },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 1 },
            },
        ],
    };
    assert_eq!(
        def.condition,
        Some(TriggerCondition::Or {
            conditions: vec![disjunct(instant), disjunct(sorcery), disjunct(otter_excl)],
        }),
        "Alania condition must be Or-of-And(anchor, count); got {:?}",
        def.condition
    );

    // Zero Unimplemented leakage (reach-guard for the reverted recognizer).
    let execute = def.execute.as_ref().expect("Alania trigger execute");
    fn has_unimplemented(ability: &AbilityDefinition) -> bool {
        matches!(*ability.effect, Effect::Unimplemented { .. })
            || ability
                .sub_ability
                .as_ref()
                .is_some_and(|s| has_unimplemented(s))
    }
    assert!(
        !has_unimplemented(execute),
        "effect chain leaked Unimplemented: {execute:?}"
    );

    // The preserved CopySpell sub is still present and gated on the optional draw.
    fn find_copyspell(ability: &AbilityDefinition) -> Option<&AbilityDefinition> {
        if matches!(*ability.effect, Effect::CopySpell { .. }) {
            return Some(ability);
        }
        ability.sub_ability.as_deref().and_then(find_copyspell)
    }
    let copy = find_copyspell(execute).expect("CopySpell sub preserved");
    assert_eq!(
        copy.condition,
        Some(AbilityCondition::EffectOutcome {
            signal: crate::types::ability::EffectOutcomeSignal::OptionalEffectPerformed,
        }),
        "CopySpell must stay gated on the optional draw (if you do)"
    );
}

/// CR 109.4: "other than this card" in an exile target must add
/// `FilterProp::Another` so the ability source (Ichorid) cannot be used
/// to pay its own recursion cost.
#[test]
fn trigger_ichorid_exile_target_excludes_self() {
    let def = parse_trigger_line(
            "At the beginning of your upkeep, if this card is in your graveyard, you may exile a black creature card other than this card from your graveyard. If you do, return this card to the battlefield.",
            "Ichorid",
        );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::Upkeep));
    assert_eq!(def.trigger_zones, vec![Zone::Graveyard]);
    let exec = def
        .execute
        .expect("Ichorid upkeep trigger must have execute")
        .effect;
    let Effect::ChangeZone {
        ref target,
        origin,
        destination,
        ..
    } = *exec
    else {
        panic!("expected ChangeZone exile, got {exec:?}")
    };
    assert_eq!(origin, Some(Zone::Graveyard));
    assert_eq!(destination, Zone::Exile);
    let TargetFilter::Typed(ref tf) = *target else {
        panic!("expected Typed filter, got {target:?}")
    };
    assert_eq!(tf.controller, Some(ControllerRef::You));
    assert!(
        tf.properties.contains(&FilterProp::Another),
        "exile target must carry FilterProp::Another for 'other than this card'; got {:?}",
        tf.properties
    );
    assert!(
        tf.properties.contains(&FilterProp::InZone {
            zone: Zone::Graveyard
        }),
        "exile target must be scoped to your graveyard; got {:?}",
        tf.properties
    );
    assert!(
        tf.properties.iter().any(|p| matches!(
            p,
            FilterProp::HasColor {
                color: ManaColor::Black
            }
        )),
        "exile target must require black; got {:?}",
        tf.properties
    );
}

#[test]
fn trigger_nth_spell_third() {
    let def = parse_trigger_line(
        "Whenever you cast your third spell each turn, create a 1/1 token.",
        "Some Card",
    );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(
        def.constraint,
        Some(TriggerConstraint::NthSpellThisTurn { n: 3, filter: None })
    );
}

/// CR 603.2: "you cast your Nth spell" must be gated on caster=you so the
/// trigger does not fire for opponents' casts. Mirrors the symmetric
/// `controller(Opponent)` scoping on the "an opponent casts their Nth"
/// branch (Alphinaud Leveilleur class).
#[test]
fn trigger_nth_spell_you_scopes_to_controller() {
    let def = parse_trigger_line(
        "Whenever you cast your second spell each turn, scry 1.",
        "Alphinaud Leveilleur",
    );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You),
        ))
    );
}

#[test]
fn trigger_nth_draw_second() {
    let def = parse_trigger_line(
        "Whenever you draw your second card each turn, you gain 1 life.",
        "Some Card",
    );
    assert_eq!(def.mode, TriggerMode::Drawn);
    // CR 603.2: "you draw" scopes the trigger to the controller's draws.
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You),
        ))
    );
    assert_eq!(
        def.constraint,
        Some(TriggerConstraint::NthDrawThisTurn { n: 2 })
    );
}

#[test]
fn trigger_nth_draw_you_in_a_turn_phrasing() {
    // CR 603.2: "When you draw your Nth card in a turn" (Sneaky Snacker phrasing)
    // must scope to the controller's draws, not any player's.
    let def = parse_trigger_line(
            "When you draw your third card in a turn, return this card from your graveyard to the battlefield tapped.",
            "Sneaky Snacker",
        );
    assert_eq!(def.mode, TriggerMode::Drawn);
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You),
        ))
    );
    assert_eq!(
        def.constraint,
        Some(TriggerConstraint::NthDrawThisTurn { n: 3 })
    );
}

#[test]
fn trigger_nth_draw_opponent_second() {
    let def = parse_trigger_line(
        "Whenever an opponent draws their second card each turn, you draw two cards.",
        "The Unagi of Kyoshi Island",
    );
    assert_eq!(def.mode, TriggerMode::Drawn);
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent),
        ))
    );
    assert_eq!(
        def.constraint,
        Some(TriggerConstraint::NthDrawThisTurn { n: 2 })
    );
}

#[test]
fn trigger_nth_draw_any_player() {
    let def = parse_trigger_line(
        "Whenever a player draws their third card each turn, you gain 1 life.",
        "Some Card",
    );
    assert_eq!(def.mode, TriggerMode::Drawn);
    assert_eq!(def.valid_target, None);
    assert_eq!(
        def.constraint,
        Some(TriggerConstraint::NthDrawThisTurn { n: 3 })
    );
}

/// SHAPE test (#413): The Council of Four's draw trigger. "during their
/// turn" restricts the trigger to draws on the drawer's own turn, mapped
/// to a `DuringPlayersTurn { TriggeringPlayer }` intervening-if. Does not
/// substitute for the runtime test in `triggers.rs`.
#[test]
fn trigger_nth_draw_any_player_during_their_turn() {
    let def = parse_trigger_line(
        "Whenever a player draws their second card during their turn, you draw a card.",
        "The Council of Four",
    );
    assert_eq!(def.mode, TriggerMode::Drawn);
    assert_eq!(def.valid_target, None);
    assert_eq!(
        def.constraint,
        Some(TriggerConstraint::NthDrawThisTurn { n: 2 })
    );
    assert_eq!(
        def.condition,
        Some(TriggerCondition::DuringPlayersTurn {
            player: PlayerFilter::TriggeringPlayer,
        })
    );
    assert!(def.execute.is_some());
}

/// SHAPE test (#413): The Council of Four's spell trigger. "casts their
/// second spell during their turn" → `NthSpellThisTurn { n: 2 }` constraint
/// plus a `DuringPlayersTurn { TriggeringPlayer }` intervening-if.
#[test]
fn trigger_nth_spell_any_player_during_their_turn() {
    let def = parse_trigger_line(
            "Whenever a player casts their second spell during their turn, you create a 2/2 white Knight creature token.",
            "The Council of Four",
        );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(def.valid_target, None);
    assert_eq!(
        def.constraint,
        Some(TriggerConstraint::NthSpellThisTurn { n: 2, filter: None })
    );
    assert_eq!(
        def.condition,
        Some(TriggerCondition::DuringPlayersTurn {
            player: PlayerFilter::TriggeringPlayer,
        })
    );
    assert!(def.execute.is_some());
}

/// SHAPE test (#413): Ledger Shredder — cluster spot-check. "casts their
/// second spell each turn" (no turn restriction) must still parse to the
/// unrestricted form with no condition.
#[test]
fn trigger_nth_spell_any_player_each_turn_no_condition() {
    let def = parse_trigger_line(
        "Whenever a player casts their second spell each turn, ~ explores.",
        "Ledger Shredder",
    );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(
        def.constraint,
        Some(TriggerConstraint::NthSpellThisTurn { n: 2, filter: None })
    );
    assert_eq!(def.condition, None);
}

#[test]
fn trigger_you_search_your_library() {
    let def = parse_trigger_line(
        "Whenever you search your library, scry 1.",
        "Search Elemental",
    );
    assert_eq!(def.mode, TriggerMode::SearchedLibrary);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn trigger_opponent_searches_their_library() {
    let def = parse_trigger_line(
        "Whenever an opponent searches their library, you gain 1 life and draw a card.",
        "Archivist of Oghma",
    );
    assert_eq!(def.mode, TriggerMode::SearchedLibrary);
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent),
        ))
    );
}

#[test]
fn trigger_you_scry() {
    let def = parse_trigger_line(
        "Whenever you scry, put a +1/+1 counter on this creature.",
        "Thoughtbound Phantasm",
    );
    assert_eq!(def.mode, TriggerMode::Scry);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn trigger_you_surveil() {
    let def = parse_trigger_line(
        "Whenever you surveil, put a +1/+1 counter on Mirko.",
        "Mirko, Obsessive Theorist",
    );
    assert_eq!(def.mode, TriggerMode::Surveil);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn trigger_you_investigate() {
    // Erdwal Illuminator (SOI): "Whenever you investigate for the first time
    // each turn, investigate an additional time." The "for the first time
    // each turn" qualifier becomes OncePerTurn; the trigger itself must be
    // Investigated (not the inert Unknown that never fires).
    let def = parse_trigger_line(
        "Whenever you investigate for the first time each turn, investigate an additional time.",
        "Erdwal Illuminator",
    );
    assert_eq!(def.mode, TriggerMode::Investigated);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert_eq!(def.constraint, Some(TriggerConstraint::OncePerTurn));
}

#[test]
fn trigger_you_investigate_bare() {
    let def = parse_trigger_line(
        "Whenever you investigate, draw a card.",
        "Test Investigator",
    );
    assert_eq!(def.mode, TriggerMode::Investigated);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn trigger_you_collect_evidence() {
    // Surveillance Monitor (MKM): "Whenever you collect evidence, create a 1/1 colorless
    // Thopter artifact creature token with flying."
    let def = parse_trigger_line(
            "Whenever you collect evidence, create a 1/1 colorless Thopter artifact creature token with flying.",
            "Surveillance Monitor",
        );
    assert_eq!(def.mode, TriggerMode::CollectEvidence);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn trigger_you_proliferate() {
    // Scheming Aspirant (ONE): "Whenever you proliferate, each opponent loses 2 life
    // and you gain 2 life."
    let def = parse_trigger_line(
        "Whenever you proliferate, each opponent loses 2 life and you gain 2 life.",
        "Scheming Aspirant",
    );
    assert_eq!(def.mode, TriggerMode::PlayerPerformedAction);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert_eq!(
        def.player_actions,
        Some(vec![PlayerActionKind::Proliferate])
    );
}

#[test]
fn trigger_you_scry_or_surveil() {
    let def = parse_trigger_line(
        "Whenever you scry or surveil, draw a card.",
        "Matoya, Archon Elder",
    );
    assert_eq!(def.mode, TriggerMode::PlayerPerformedAction);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert_eq!(
        def.player_actions,
        Some(vec![PlayerActionKind::Scry, PlayerActionKind::Surveil])
    );
}

#[test]
fn trigger_opponent_scries_surveils_or_searches() {
    let def = parse_trigger_line(
            "Whenever an opponent scries, surveils, or searches their library, put a +1/+1 counter on River Song. Then River Song deals damage to that player equal to its power.",
            "River Song",
        );
    assert_eq!(def.mode, TriggerMode::PlayerPerformedAction);
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent),
        ))
    );
    assert_eq!(
        def.player_actions,
        Some(vec![
            PlayerActionKind::Scry,
            PlayerActionKind::Surveil,
            PlayerActionKind::SearchedLibrary,
        ])
    );
}

#[test]
fn trigger_nth_spell_opponent_noncreature() {
    let def = parse_trigger_line(
        "Whenever an opponent casts their first noncreature spell each turn, draw a card.",
        "Esper Sentinel",
    );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    // parse_type_phrase("noncreature") produces [Non(Creature)] without a redundant
    // Card base type — Non(Creature) alone is sufficient for spell-history filtering.
    assert_eq!(
        def.constraint,
        Some(TriggerConstraint::NthSpellThisTurn {
            n: 1,
            filter: Some(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Non(Box::new(TypeFilter::Creature))],
                controller: None,
                properties: vec![],
            })),
        })
    );
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        ))
    );
}

#[test]
fn trigger_esper_sentinel_unless_pay() {
    let def = parse_trigger_line(
            "Whenever an opponent casts their first noncreature spell each turn, draw a card unless that player pays {X}, where X is this creature's power.",
            "Esper Sentinel",
        );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    // Effect should be Draw, not Unimplemented
    let execute = def.execute.as_ref().expect("should have execute");
    assert!(
        matches!(*execute.effect, Effect::Draw { .. }),
        "execute effect should be Draw, got {:?}",
        execute.effect
    );
    // Unless pay should be DynamicGeneric with SelfPower
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    assert_eq!(
        unless_pay.cost,
        AbilityCost::ManaDynamic {
            quantity: QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: crate::types::ability::ObjectScope::Source
                }
            }
        }
    );
    assert_eq!(unless_pay.payer, TargetFilter::TriggeringPlayer);
}

#[test]
fn trigger_unless_you_pay_mana() {
    // "sacrifice this creature unless you pay {G}{G}" — "you pay" payer variant
    let def = parse_trigger_line(
        "At the beginning of your upkeep, sacrifice this creature unless you pay {G}{G}.",
        "Test Card",
    );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    assert_eq!(unless_pay.payer, TargetFilter::Controller);
    assert!(
        matches!(unless_pay.cost, AbilityCost::Mana { .. }),
        "cost should be Fixed mana, got {:?}",
        unless_pay.cost
    );
    // The effect text should be stripped of the unless clause
    let execute = def.execute.as_ref().expect("should have execute");
    assert!(
        matches!(*execute.effect, Effect::Sacrifice { .. }),
        "execute should be Sacrifice, got {:?}",
        execute.effect
    );
}

#[test]
fn trigger_unless_you_pay_energy() {
    let def = parse_trigger_line(
        "At the beginning of your end step, sacrifice this creature unless you pay {E}{E}.",
        "Lathnu Hellion",
    );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    assert_eq!(unless_pay.payer, TargetFilter::Controller);
    assert_eq!(
        unless_pay.cost,
        AbilityCost::PayEnergy {
            amount: QuantityExpr::Fixed { value: 2 }
        }
    );
    let execute = def.execute.as_ref().expect("should have execute");
    assert!(
        matches!(*execute.effect, Effect::Sacrifice { .. }),
        "execute should be Sacrifice, got {:?}",
        execute.effect
    );
}

/// CR 107.14 + CR 202.3: Volatile Stormdrake's dynamic energy unless-cost —
/// "sacrifice that creature unless you pay an amount of {E} equal to its
/// mana value" must surface a `PayEnergy` whose amount references the
/// taxed object's mana value (`Recipient` scope).
#[test]
fn trigger_unless_you_pay_dynamic_energy() {
    let def = parse_trigger_line(
            "When this creature enters, sacrifice that creature unless you pay an amount of {E} equal to its mana value.",
            "Volatile Stormdrake",
        );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    assert_eq!(unless_pay.payer, TargetFilter::Controller);
    assert_eq!(
        unless_pay.cost,
        AbilityCost::PayEnergy {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: crate::types::ability::ObjectScope::Recipient,
                },
            },
        }
    );
    let execute = def.execute.as_ref().expect("should have execute");
    assert!(
        matches!(*execute.effect, Effect::Sacrifice { .. }),
        "execute should be Sacrifice, got {:?}",
        execute.effect
    );
}

/// CR 608.2k + CR 603.7c: Self-ETB "sacrifice it" anaphor — Azorius
/// Herald, Balduvian Horde, Glint Hawk, Faerie Impostor, Phlage. The
/// bare object pronoun "it" in a `SelfRef`-subject trigger sub-effect
/// must resolve to `TargetFilter::SelfRef` (the source itself), NOT to
/// `ParentTarget` — the trigger introduces no parent-target chain.
///
/// This locks in the divergent behavior between `resolve_it_pronoun`
/// (which returns `SelfRef` for `Some(SelfRef)` subjects) and
/// `resolve_pronoun_target` (which returns `ParentTarget` for the same
/// subject case to support parent-target chains like
/// "tap target creature. exile it"). The two helpers serve different
/// anaphor contexts; the sacrifice imperative path delegates to
/// `resolve_it_pronoun` because no parent target is in scope for the
/// self-ETB trigger sub-effect.
#[test]
fn self_etb_sacrifice_it_anaphor_binds_to_self_ref() {
    let def = parse_trigger_line(
        "When ~ enters, sacrifice it unless {U} was spent to cast it.",
        "Azorius Herald",
    );
    let execute = def.execute.as_ref().expect("should have execute");
    let Effect::Sacrifice { target, .. } = &*execute.effect else {
        panic!("expected Sacrifice, got {:?}", execute.effect);
    };
    assert_eq!(
        *target,
        TargetFilter::SelfRef,
        "sacrifice target must bind to the self (entering source), not ParentTarget"
    );
}

#[test]
fn trigger_unless_you_discard_a_card() {
    // CR 608.2c: Balduvian Horde — "sacrifice it unless you discard a card at random".
    // The "at random" suffix is currently sub-fidelity (player-chosen via WardDiscardChoice);
    // the cost-gate itself is captured.
    let def = parse_trigger_line(
        "When ~ enters, sacrifice it unless you discard a card at random.",
        "Balduvian Horde",
    );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    assert_eq!(unless_pay.payer, TargetFilter::Controller);
    assert!(
        matches!(
            unless_pay.cost,
            AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                filter: None,
                selection: CardSelectionMode::Chosen,
                self_scope: DiscardSelfScope::FromHand
            }
        ),
        "cost should be DiscardCard, got {:?}",
        unless_pay.cost
    );
    let execute = def.execute.as_ref().expect("should have execute");
    assert!(
        matches!(*execute.effect, Effect::Sacrifice { .. }),
        "execute should be Sacrifice, got {:?}",
        execute.effect
    );
}

#[test]
fn trigger_unless_you_sacrifice_filter() {
    // Bog Elemental — "sacrifice this creature unless you sacrifice a land".
    let def = parse_trigger_line(
        "At the beginning of your upkeep, sacrifice this creature unless you sacrifice a land.",
        "Bog Elemental",
    );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    assert_eq!(unless_pay.payer, TargetFilter::Controller);
    match &unless_pay.cost {
        AbilityCost::Sacrifice(cost) => {
            assert_eq!(cost.requirement, SacrificeRequirement::count(1));
            match &cost.target {
                TargetFilter::Typed(typed) => {
                    assert!(
                        typed
                            .type_filters
                            .iter()
                            .any(|t| matches!(t, TypeFilter::Land)),
                        "filter should include Land, got {:?}",
                        typed.type_filters,
                    );
                }
                other => panic!("expected Typed filter, got {:?}", other),
            }
        }
        other => panic!("cost should be Sacrifice, got {:?}", other),
    }
}

#[test]
fn trigger_unless_you_sacrifice_power_threshold() {
    let def = parse_trigger_line(
            "When this creature enters, sacrifice it unless you sacrifice any number of creatures with total power 12 or greater.",
            "Phyrexian Dreadnought",
        );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    assert_eq!(unless_pay.payer, TargetFilter::Controller);
    match &unless_pay.cost {
        AbilityCost::Sacrifice(cost) => match &cost.requirement {
            SacrificeRequirement::Aggregate {
                stat: SacrificeAggregateStat::TotalPower,
                comparator: Comparator::GE,
                value: 12,
            } => match &cost.target {
                TargetFilter::Typed(typed) => {
                    assert!(
                        typed
                            .type_filters
                            .iter()
                            .any(|t| matches!(t, TypeFilter::Creature)),
                        "filter should include Creature, got {:?}",
                        typed.type_filters,
                    );
                }
                other => panic!("expected Typed filter, got {:?}", other),
            },
            other => panic!("expected aggregate sacrifice requirement, got {:?}", other),
        },
        other => panic!("cost should be Sacrifice, got {:?}", other),
    }
    match def.execute.as_ref().expect("execute").effect.as_ref() {
        Effect::Sacrifice { target, .. } => {
            assert_eq!(*target, TargetFilter::SelfRef);
        }
        other => panic!("primary ETB effect should sacrifice self, got {:?}", other),
    }
}

#[test]
fn trigger_unless_you_pay_n_life() {
    // Carnophage — "tap this creature unless you pay 1 life".
    let def = parse_trigger_line(
        "At the beginning of your upkeep, tap this creature unless you pay 1 life.",
        "Carnophage",
    );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    assert_eq!(unless_pay.payer, TargetFilter::Controller);
    assert!(
        matches!(
            unless_pay.cost,
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 1 }
            }
        ),
        "cost should be PayLife {{ amount: 1 }}, got {:?}",
        unless_pay.cost
    );
}

#[test]
fn trigger_unless_they_pay_life_binds_phase_that_player_to_scoped_player() {
    // Blood Clock — phase-scoped "that player" is the active player whose
    // upkeep began, not an event player from a spell/action trigger.
    let def = parse_trigger_line(
            "At the beginning of each player's upkeep, that player returns a permanent they control to its owner's hand unless they pay 2 life.",
            "Blood Clock",
        );

    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    assert_eq!(unless_pay.payer, TargetFilter::Controller);
    assert!(
        matches!(
            unless_pay.cost,
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 2 }
            }
        ),
        "cost should be PayLife {{ amount: 2 }}, got {:?}",
        unless_pay.cost
    );
    let execute = def.execute.as_ref().expect("should have execute");
    match execute.effect.as_ref() {
        Effect::Bounce { target, .. } => match target {
            TargetFilter::Typed(filter) => {
                assert_eq!(filter.controller, Some(ControllerRef::ScopedPlayer));
                assert!(
                    filter.type_filters.contains(&TypeFilter::Permanent),
                    "filter should include Permanent, got {:?}",
                    filter.type_filters
                );
            }
            other => panic!("expected typed bounce filter, got {other:?}"),
        },
        other => panic!("expected Bounce effect, got {other:?}"),
    }
}

#[test]
fn continues_spell_quality_disjunction_talion_segments() {
    assert!(continues_spell_quality_disjunction(
        "power, or toughness equal to the chosen number, that player loses 2 life",
    ));
    assert!(continues_spell_quality_disjunction(
        "or toughness equal to the chosen number, that player loses 2 life",
    ));
    assert!(!continues_spell_quality_disjunction("you draw a card"));
}

#[test]
fn find_effect_boundary_does_not_skip_unrelated_power_or_comma() {
    let line = "Whenever an opponent casts a red spell, you draw a card";
    let lower = line.to_lowercase();
    let boundary = find_effect_boundary(&lower).expect("effect boundary");
    let suffix = &lower[boundary..];
    let expected = ", you draw";
    assert_eq!(
        &suffix[..expected.len()],
        expected,
        "comma after spell type should split condition/effect, got {suffix:?}"
    );
}

#[test]
fn find_effect_boundary_skips_spell_quality_comma() {
    let line = "Whenever an opponent casts a spell with mana value, power, or toughness equal to the chosen number, that player loses 2 life";
    let lower = line.to_lowercase();
    let boundary = find_effect_boundary(&lower).expect("effect boundary");
    let suffix = &lower[boundary..];
    let expected = ", that player";
    assert_eq!(
        &suffix[..expected.len()],
        expected,
        "boundary should follow the spell-quality clause, got {suffix:?}"
    );
}

#[test]
fn parse_spell_chosen_number_quality_talion_suffix() {
    let tf = parse_spell_chosen_number_quality(
        "spell with mana value, power, or toughness equal to the chosen number",
    )
    .expect("suffix should parse");
    assert!(tf
        .properties
        .iter()
        .any(|p| matches!(p, FilterProp::AnyOf { .. })));
}

#[test]
fn talion_opponent_spell_cast_chosen_number_or_filter() {
    let def = parse_trigger_line(
            "Whenever an opponent casts a spell with mana value, power, or toughness equal to the chosen number, that player loses 2 life and you draw a card.",
            "Talion, the Kindly Lord",
        );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        ))
    );
    let valid_card = def.valid_card.as_ref().expect("spell quality filter");
    let TargetFilter::Typed(tf) = valid_card else {
        panic!("expected typed spell filter, got {valid_card:?}");
    };
    let props = tf
        .properties
        .iter()
        .find_map(|p| match p {
            FilterProp::AnyOf { props } => Some(props.as_slice()),
            _ => None,
        })
        .expect("expected AnyOf cmc/pt props");
    assert_eq!(props.len(), 3);
    assert!(props.iter().any(|p| {
        matches!(
            p,
            FilterProp::Cmc {
                comparator: Comparator::EQ,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::ChosenNumber
                }
            }
        )
    }));
    assert!(props.iter().any(|p| {
        matches!(
            p,
            FilterProp::PtComparison {
                stat: PtStat::Power,
                comparator: Comparator::EQ,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::ChosenNumber
                },
                ..
            }
        )
    }));
    assert!(props.iter().any(|p| {
        matches!(
            p,
            FilterProp::PtComparison {
                stat: PtStat::Toughness,
                comparator: Comparator::EQ,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::ChosenNumber
                },
                ..
            }
        )
    }));
    let execute = def.execute.as_ref().expect("execute");
    assert!(
        matches!(
            execute.effect.as_ref(),
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 2 },
                target: Some(TargetFilter::TriggeringPlayer),
            }
        ),
        "head effect: {:?}",
        execute.effect
    );
    let draw = execute
        .sub_ability
        .as_ref()
        .expect("draw chained after life loss");
    assert!(matches!(draw.effect.as_ref(), Effect::Draw { .. }));
}

#[test]
fn talion_kindly_lord_full_oracle_no_swallowed_clause() {
    let text = "Flying\nAs Talion, the Kindly Lord enters the battlefield, choose a number between 1 and 10.\nWhenever an opponent casts a spell with mana value, power, or toughness equal to the chosen number, that player loses 2 life and you draw a card.";
    let parsed = parse_oracle_text(
        text,
        "Talion, the Kindly Lord",
        &["Flying".to_string()],
        &["Creature".to_string()],
        &["Faerie".to_string(), "Noble".to_string()],
    );
    let swallowed: Vec<_> = parsed
        .parse_warnings
        .iter()
        .filter(|w| w.category_name() == "swallowed-clause")
        .collect();
    assert!(
        swallowed.is_empty(),
        "Talion should parse without swallowed-clause warnings: {swallowed:?}"
    );
}

#[test]
fn talion_runtime_triggers_only_for_matching_spell_quality() {
    let run = |name: &str, creature_pt: Option<(i32, i32)>, mana_value: u32| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Drawn Card"]);
        let talion = scenario
                .add_creature_from_oracle(
                    P0,
                    "Talion, the Kindly Lord",
                    3,
                    4,
                    "Flying\nAs Talion, the Kindly Lord enters the battlefield, choose a number between 1 and 10.\nWhenever an opponent casts a spell with mana value, power, or toughness equal to the chosen number, that player loses 2 life and you draw a card.",
                )
                .id();
        let spell = if let Some((power, toughness)) = creature_pt {
            scenario
                .add_creature_to_hand_from_oracle(P1, name, power, toughness, "")
                .with_mana_cost(ManaCost::generic(mana_value))
                .id()
        } else {
            scenario
                .add_spell_to_hand_from_oracle(P1, name, true, "")
                .with_mana_cost(ManaCost::generic(mana_value))
                .id()
        };
        scenario.with_mana_pool(
            P1,
            (0..mana_value)
                .map(|_| ManaUnit::new(ManaType::Colorless, talion, false, Vec::new()))
                .collect(),
        );
        let mut runner = scenario.build();
        {
            let state = runner.state_mut();
            state
                .objects
                .get_mut(&talion)
                .expect("Talion source exists")
                .chosen_attributes
                .push(ChosenAttribute::Number(3));
            state.active_player = P1;
            state.priority_player = P1;
            state.waiting_for = WaitingFor::Priority { player: P1 };
        }

        let outcome = runner.cast(spell).resolve();
        (outcome.life_delta(P1), outcome.hand_drawn(P0))
    };

    assert_eq!(run("Three-Mana Instant", None, 3), (-2, 1));
    assert_eq!(run("Three-Power Creature", Some((3, 1)), 1), (-2, 1));
    assert_eq!(run("Three-Toughness Creature", Some((1, 3)), 1), (-2, 1));
    assert_eq!(run("One-One Creature", Some((1, 1)), 1), (0, 0));
}

#[test]
fn trigger_unless_they_pay_life_keeps_event_that_player_as_triggering_player() {
    let def = parse_trigger_line(
        "Whenever an opponent casts a spell, that player loses 5 life unless they pay 2 life.",
        "Test Card",
    );

    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    assert_eq!(unless_pay.payer, TargetFilter::TriggeringPlayer);
    assert!(
        matches!(
            unless_pay.cost,
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 2 }
            }
        ),
        "cost should be PayLife {{ amount: 2 }}, got {:?}",
        unless_pay.cost
    );
    let execute = def.execute.as_ref().expect("should have execute");
    match execute.effect.as_ref() {
        Effect::LoseLife { target, amount } => {
            assert_eq!(*target, Some(TargetFilter::TriggeringPlayer));
            assert_eq!(*amount, QuantityExpr::Fixed { value: 5 });
        }
        other => panic!("expected LoseLife effect, got {other:?}"),
    }
}

#[test]
fn trigger_unless_you_discard_typed_preserves_filter() {
    // Drekavac — "unless you discard a noncreature card".
    let def = parse_trigger_line(
        "When ~ enters, sacrifice it unless you discard a noncreature card.",
        "Drekavac",
    );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    match &unless_pay.cost {
        AbilityCost::Discard {
            filter: Some(TargetFilter::Typed(typed)),
            ..
        } => assert!(
            typed.type_filters.iter().any(
                |t| matches!(t, TypeFilter::Non(inner) if matches!(**inner, TypeFilter::Creature))
            ),
            "filter should include noncreature, got {:?}",
            typed.type_filters
        ),
        other => panic!("cost should be filtered Discard, got {:?}", other),
    }
}

#[test]
fn trigger_unless_you_return_artifact_to_hand() {
    // CR 118.12: Glint Hawk — "sacrifice it unless you return an artifact
    // you control to its owner's hand."
    let def = parse_trigger_line(
            "When ~ enters, sacrifice it unless you return an artifact you control to its owner's hand.",
            "Glint Hawk",
        );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    assert_eq!(unless_pay.payer, TargetFilter::Controller);
    match &unless_pay.cost {
        AbilityCost::ReturnToHand {
            count,
            filter: Some(filter),
            from_zone,
        } => {
            assert_eq!(*count, 1);
            assert!(
                from_zone.is_none(),
                "battlefield source should have no from_zone"
            );
            let has_artifact = match filter {
                TargetFilter::And { filters } => filters.iter().any(|f| {
                    matches!(f,
                        TargetFilter::Typed(tf) if tf.type_filters.contains(&TypeFilter::Artifact)
                    )
                }),
                TargetFilter::Typed(tf) => tf.type_filters.contains(&TypeFilter::Artifact),
                _ => false,
            };
            assert!(
                has_artifact,
                "filter should include Artifact, got {:?}",
                filter
            );
        }
        other => panic!("cost should be ReturnToHand, got {:?}", other),
    }
    let execute = def.execute.as_ref().expect("should have execute");
    assert!(
        matches!(*execute.effect, Effect::Sacrifice { .. }),
        "execute should be Sacrifice, got {:?}",
        execute.effect
    );
}

#[test]
fn trigger_unless_you_return_another_creature_to_hand() {
    // CR 118.12: Faerie Impostor / Quickling — "sacrifice it unless you
    // return another creature you control to its owner's hand."
    let def = parse_trigger_line(
            "When ~ enters, sacrifice it unless you return another creature you control to its owner's hand.",
            "Faerie Impostor",
        );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    match &unless_pay.cost {
        AbilityCost::ReturnToHand {
            count,
            filter: Some(filter),
            ..
        } => {
            assert_eq!(*count, 1);
            let has_another_creature = match filter {
                TargetFilter::And { filters } => filters.iter().any(|f| {
                    matches!(f,
                        TargetFilter::Typed(tf) if tf.properties.contains(&FilterProp::Another)
                            && tf.type_filters.contains(&TypeFilter::Creature)
                    )
                }),
                TargetFilter::Typed(tf) => {
                    tf.properties.contains(&FilterProp::Another)
                        && tf.type_filters.contains(&TypeFilter::Creature)
                }
                _ => false,
            };
            assert!(
                has_another_creature,
                "filter should include Another+Creature, got {:?}",
                filter
            );
        }
        other => panic!("cost should be ReturnToHand, got {:?}", other),
    }
}

#[test]
fn trigger_unless_you_return_two_forests_to_hand() {
    // CR 118.12: Bull Elephant — "sacrifice it unless you return two
    // Forests you control to their owner's hand."
    let def = parse_trigger_line(
            "When ~ enters, sacrifice it unless you return two Forests you control to their owner's hand.",
            "Bull Elephant",
        );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    match &unless_pay.cost {
        AbilityCost::ReturnToHand {
            count, from_zone, ..
        } => {
            assert_eq!(*count, 2);
            assert!(from_zone.is_none());
        }
        other => panic!("cost should be ReturnToHand, got {:?}", other),
    }
}

#[test]
fn trigger_unless_you_return_non_lair_land_to_hand() {
    // CR 118.12: Crosis's Catacombs — "sacrifice it unless you return a
    // non-Lair land you control to its owner's hand."
    let def = parse_trigger_line(
            "When ~ enters, sacrifice it unless you return a non-Lair land you control to its owner's hand.",
            "Crosis's Catacombs",
        );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    match &unless_pay.cost {
        AbilityCost::ReturnToHand {
            count, from_zone, ..
        } => {
            assert_eq!(*count, 1);
            assert!(from_zone.is_none());
        }
        other => panic!("cost should be ReturnToHand, got {:?}", other),
    }
}

#[test]
fn trigger_unless_you_return_from_graveyard() {
    // CR 118.12: Harvest Wurm — "sacrifice it unless you return a basic
    // land card from your graveyard to your hand."
    let def = parse_trigger_line(
            "When ~ enters, sacrifice it unless you return a basic land card from your graveyard to your hand.",
            "Harvest Wurm",
        );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    match &unless_pay.cost {
        AbilityCost::ReturnToHand {
            count,
            from_zone,
            filter: Some(filter),
        } => {
            assert_eq!(*count, 1);
            assert_eq!(
                *from_zone,
                Some(crate::types::zones::Zone::Graveyard),
                "should be from graveyard"
            );
            let has_land = match filter {
                TargetFilter::And { filters } => filters.iter().any(|f| {
                    matches!(f,
                        TargetFilter::Typed(tf) if tf.type_filters.contains(&TypeFilter::Land)
                    )
                }),
                TargetFilter::Typed(tf) => tf.type_filters.contains(&TypeFilter::Land),
                _ => false,
            };
            assert!(has_land, "filter should include Land, got {:?}", filter);
        }
        other => panic!("cost should be ReturnToHand, got {:?}", other),
    }
}

#[test]
fn trigger_unless_you_tap_untapped_creature() {
    // CR 118.12 + CR 701.20a: Koskun Falls — "sacrifice this enchantment
    // unless you tap an untapped creature you control."
    let def = parse_trigger_line(
            "At the beginning of your upkeep, sacrifice this enchantment unless you tap an untapped creature you control.",
            "Koskun Falls",
        );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    match &unless_pay.cost {
        AbilityCost::TapCreatures {
            requirement,
            filter,
        } => {
            assert_eq!(requirement.fixed_count(), Some(1));
            let is_creature = match filter {
                    TargetFilter::Typed(tf) => {
                        tf.type_filters.contains(&TypeFilter::Creature)
                    }
                    TargetFilter::And { filters } => filters.iter().any(|f| {
                        matches!(f, TargetFilter::Typed(tf) if tf.type_filters.contains(&TypeFilter::Creature))
                    }),
                    _ => false,
                };
            assert!(
                is_creature,
                "filter should include Creature, got {:?}",
                filter
            );
        }
        other => panic!("cost should be TapCreatures, got {:?}", other),
    }
    assert_eq!(
        unless_pay.payer,
        TargetFilter::Controller,
        "payer should be Controller"
    );
}

#[test]
fn trigger_unless_you_tap_untapped_permanent() {
    // CR 118.12 + CR 701.20a: Command Bridge — "sacrifice it unless you
    // tap an untapped permanent you control."
    let def = parse_trigger_line(
        "When this land enters, sacrifice it unless you tap an untapped permanent you control.",
        "Command Bridge",
    );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    match &unless_pay.cost {
        AbilityCost::TapCreatures {
            requirement,
            filter: _,
        } => {
            assert_eq!(requirement.fixed_count(), Some(1));
        }
        other => panic!("cost should be TapCreatures, got {:?}", other),
    }
    assert_eq!(
        unless_pay.payer,
        TargetFilter::Controller,
        "payer should be Controller"
    );
}

#[test]
fn trigger_unless_you_tap_two_untapped_creatures() {
    let def = parse_trigger_line(
            "At the beginning of your upkeep, sacrifice this enchantment unless you tap two untapped creatures you control.",
            "Test Card",
        );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    match &unless_pay.cost {
        AbilityCost::TapCreatures {
            requirement,
            filter,
        } => {
            assert_eq!(requirement.fixed_count(), Some(2));
            let has_creature = match filter {
                    TargetFilter::Typed(tf) => tf.type_filters.contains(&TypeFilter::Creature),
                    TargetFilter::And { filters } => filters.iter().any(|f| {
                        matches!(f, TargetFilter::Typed(tf) if tf.type_filters.contains(&TypeFilter::Creature))
                    }),
                    _ => false,
                };
            assert!(
                has_creature,
                "filter should include Creature, got {:?}",
                filter
            );
        }
        other => panic!("cost should be TapCreatures, got {:?}", other),
    }
}

#[test]
fn trigger_unless_you_exile_card_from_graveyard() {
    // CR 118.12 + CR 701.7: Rotting Giant — "sacrifice it unless you
    // exile a card from your graveyard."
    let def = parse_trigger_line(
            "Whenever this creature attacks or blocks, sacrifice it unless you exile a card from your graveyard.",
            "Rotting Giant",
        );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    match &unless_pay.cost {
        AbilityCost::Exile {
            count,
            zone,
            filter,
        } => {
            assert_eq!(*count, 1);
            assert_eq!(*zone, Some(crate::types::zones::Zone::Graveyard));
            assert!(filter.is_some(), "filter should be present");
        }
        other => panic!("cost should be Exile, got {:?}", other),
    }
    assert_eq!(
        unless_pay.payer,
        TargetFilter::Controller,
        "payer should be Controller"
    );
}

#[test]
fn trigger_unless_you_exile_two_cards_from_graveyard() {
    let def = parse_trigger_line(
            "Whenever this creature attacks, sacrifice it unless you exile two cards from your graveyard.",
            "Test Card",
        );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    match &unless_pay.cost {
        AbilityCost::Exile { count, zone, .. } => {
            assert_eq!(*count, 2);
            assert_eq!(*zone, Some(crate::types::zones::Zone::Graveyard));
        }
        other => panic!("cost should be Exile, got {:?}", other),
    }
}

#[test]
fn trigger_unless_intervening_attacked_this_turn() {
    // Bellowing Saddlebrute (Raid) — "When this creature enters, you lose 4
    // life unless you attacked this turn." The trailing intervening-unless
    // wraps `AttackedThisTurn >= 1` in `Not`, leaving "you lose 4 life" as
    // the effect.
    let def = parse_trigger_line(
        "When ~ enters, you lose 4 life unless you attacked this turn.",
        "Bellowing Saddlebrute",
    );
    let cond = def
        .condition
        .expect("intervening unless should set condition");
    match cond {
        TriggerCondition::Not { condition } => match *condition {
            TriggerCondition::QuantityComparison {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::AttackedThisTurn {
                                scope: CountScope::Controller,
                                filter: None,
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {}
            other => panic!("inner should be AttackedThisTurn >= 1, got {:?}", other),
        },
        other => panic!("expected TriggerCondition::Not, got {:?}", other),
    }
}

#[test]
fn trigger_unless_intervening_controls_creature() {
    // Generic shape: "deals 3 damage to target player unless you control a
    // creature" — IsPresent(Creature) wrapped in Not.
    let def = parse_trigger_line(
        "When ~ enters, ~ deals 3 damage to target player unless you control a creature.",
        "Test",
    );
    match def.condition {
        Some(TriggerCondition::Not { condition }) => match *condition {
            TriggerCondition::ControlsType { .. } => {}
            other => panic!("inner should be ControlsType, got {:?}", other),
        },
        other => panic!("expected Some(Not), got {:?}", other),
    }
}

#[test]
fn trigger_unless_intervening_does_not_swallow_unless_pay() {
    // Sanity: "unless you pay {2}" must remain captured by
    // extract_unless_pay_modifier — NOT routed through the new
    // intervening-condition combinator. If parse_inner_condition somehow
    // matched a cost phrase, the unless_pay slot would be lost.
    let def = parse_trigger_line("When ~ enters, draw a card unless you pay {2}.", "Test");
    assert!(
        def.unless_pay.is_some(),
        "unless-pay must be captured as alt-cost, not as intervening condition"
    );
    assert!(
        def.condition.is_none(),
        "no intervening condition should be set when unless-pay handled it"
    );
}

#[test]
fn trigger_unless_they_pay_binds_that_player_to_triggering_player() {
    let def = parse_trigger_line(
            "Whenever an opponent casts a creature spell, that player loses 2 life unless they pay {2}.",
            "Isolation Cell",
        );

    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    assert_eq!(unless_pay.payer, TargetFilter::TriggeringPlayer);
    assert!(
        matches!(unless_pay.cost, AbilityCost::Mana { .. }),
        "cost should be Fixed mana, got {:?}",
        unless_pay.cost
    );
}

#[test]
fn trigger_unless_they_pay_disjunctive_mana_builds_one_of() {
    let def = parse_trigger_line(
            "At the beginning of your upkeep, for each player, this enchantment deals 1 damage to that player unless they pay {B} or {3}.",
            "Lim-Dul's Hex",
        );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    assert_eq!(unless_pay.payer, TargetFilter::TriggeringPlayer);
    let AbilityCost::OneOf { costs } = &unless_pay.cost else {
        panic!("cost should be OneOf, got {:?}", unless_pay.cost);
    };
    assert_eq!(
        costs.len(),
        2,
        "OneOf should have two mana branches: {costs:?}"
    );
    assert!(
        matches!(&costs[0], AbilityCost::Mana { .. }),
        "first branch should be Mana, got {:?}",
        costs[0]
    );
    assert!(
        matches!(&costs[1], AbilityCost::Mana { .. }),
        "second branch should be Mana, got {:?}",
        costs[1]
    );
}

#[test]
fn trigger_unless_they_pay_binds_to_that_player_damage_target() {
    let def = parse_trigger_line(
            "Whenever an opponent casts a creature spell, this enchantment deals 2 damage to that player unless they pay {2}.",
            "Soul Barrier",
        );

    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    assert_eq!(unless_pay.payer, TargetFilter::TriggeringPlayer);
    assert!(
        matches!(unless_pay.cost, AbilityCost::Mana { .. }),
        "cost should be Fixed mana, got {:?}",
        unless_pay.cost
    );
}

#[test]
fn trigger_unless_they_pay_binds_each_opponent_to_scoped_player() {
    let def = parse_trigger_line(
            "When this creature enters, each opponent sacrifices a permanent of their choice unless they pay {2}.",
            "Rishadan Footpad",
        );

    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    // CR 608.2f: the per-iteration scoped opponent pays, resolved via
    // `ability.scoped_player` (not `state.active_player` as `Controller`
    // would yield on a non-active opponent's behalf).
    assert_eq!(unless_pay.payer, TargetFilter::ScopedPlayer);
    let execute = def.execute.as_ref().expect("should have execute");
    assert_eq!(execute.player_scope, Some(PlayerFilter::Opponent));
}

// CR 118.12a: Trigger-side delegation to `parse_unless_they_alt_cost_chain`
// for the disjunctive non-mana unless-cost shape. The payer-pronoun axis
// {they, that player, that opponent} x cost {sacrifice <filter>, discard a
// card, pay N life}. Asserts the typed `UnlessPayModifier` and the cleaned
// effect text — never card names.
#[test]
fn trigger_unless_they_sacrifice_filter_binds_triggering_player() {
    let def = parse_trigger_line(
            "Whenever an opponent casts a spell, that player loses 3 life unless they sacrifice a creature.",
            "Test Punisher",
        );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    assert_eq!(unless_pay.payer, TargetFilter::TriggeringPlayer);
    assert!(
        matches!(
            &unless_pay.cost,
            AbilityCost::Sacrifice(cost)
                if cost.requirement == SacrificeRequirement::count(1)
        ),
        "cost should be Sacrifice, got {:?}",
        unless_pay.cost
    );
    let AbilityCost::Sacrifice(cost) = &unless_pay.cost else {
        unreachable!("checked sacrifice cost above");
    };
    let TargetFilter::Typed(tf) = &cost.target else {
        panic!("sacrifice target should be typed, got {:?}", cost.target);
    };
    assert_eq!(tf.controller, Some(ControllerRef::You));
}

#[test]
fn demanding_dragon_etb_unless_sacrifice_binds_target_player_payer() {
    let def = parse_trigger_line(
            "When this creature enters, it deals 5 damage to target opponent unless that player sacrifices a creature of their choice.",
            "Demanding Dragon",
        );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    assert_eq!(
        unless_pay.payer,
        TargetFilter::Player,
        "target-opponent punishers bind unless payer to the chosen player target (#2422)"
    );
    assert!(
        matches!(
            &unless_pay.cost,
            AbilityCost::Sacrifice(cost)
                if cost.requirement == SacrificeRequirement::count(1)
        ),
        "cost should be Sacrifice, got {:?}",
        unless_pay.cost
    );
}

#[test]
fn trigger_unless_that_player_discards_a_card() {
    let def = parse_trigger_line(
            "Whenever an opponent casts a spell, that player loses 3 life unless that player discards a card.",
            "Test Punisher",
        );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    assert_eq!(unless_pay.payer, TargetFilter::TriggeringPlayer);
    assert!(
        matches!(
            unless_pay.cost,
            AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                filter: None,
                selection: CardSelectionMode::Chosen,
                self_scope: DiscardSelfScope::FromHand
            }
        ),
        "cost should be DiscardCard, got {:?}",
        unless_pay.cost
    );
}

#[test]
fn trigger_unless_that_opponent_pays_life() {
    let def = parse_trigger_line(
            "Whenever an opponent casts a spell, that opponent loses 3 life unless that opponent pays 4 life.",
            "Test Punisher",
        );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    assert_eq!(unless_pay.payer, TargetFilter::TriggeringPlayer);
    assert!(
        matches!(
            unless_pay.cost,
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 4 }
            }
        ),
        "cost should be PayLife(4), got {:?}",
        unless_pay.cost
    );
}

#[test]
fn trigger_unless_they_disjunctive_sacrifice_or_discard_builds_one_of() {
    let def = parse_trigger_line(
            "Whenever an opponent casts a spell, that player loses 3 life unless they sacrifice a nonland permanent or discard a card.",
            "Test Punisher",
        );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    assert_eq!(unless_pay.payer, TargetFilter::TriggeringPlayer);
    let AbilityCost::OneOf { costs } = &unless_pay.cost else {
        panic!("cost should be OneOf, got {:?}", unless_pay.cost);
    };
    assert_eq!(costs.len(), 2, "OneOf should have two branches: {costs:?}");
    assert!(
        matches!(costs[0], AbilityCost::Sacrifice(_)),
        "first branch should be Sacrifice, got {:?}",
        costs[0]
    );
    let AbilityCost::Sacrifice(cost) = &costs[0] else {
        unreachable!("checked sacrifice branch above");
    };
    let TargetFilter::Typed(tf) = &cost.target else {
        panic!("sacrifice target should be typed, got {:?}", cost.target);
    };
    assert_eq!(tf.controller, Some(ControllerRef::You));
    assert!(
        matches!(costs[1], AbilityCost::Discard { .. }),
        "second branch should be Discard, got {:?}",
        costs[1]
    );
}

#[test]
fn trigger_unless_each_opponent_sacrifice_binds_scoped_player() {
    let def = parse_trigger_line(
        "When this creature enters, each opponent loses 3 life unless they sacrifice a creature.",
        "Test Scoped Punisher",
    );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    // CR 608.2f: scoped opponent pays via per-iteration `scoped_player`.
    assert_eq!(unless_pay.payer, TargetFilter::ScopedPlayer);
    assert!(
        matches!(
            &unless_pay.cost,
            AbilityCost::Sacrifice(cost)
                if cost.requirement == SacrificeRequirement::count(1)
        ),
        "cost should be Sacrifice, got {:?}",
        unless_pay.cost
    );
    let AbilityCost::Sacrifice(cost) = &unless_pay.cost else {
        unreachable!("checked sacrifice cost above");
    };
    let TargetFilter::Typed(tf) = &cost.target else {
        panic!("sacrifice target should be typed, got {:?}", cost.target);
    };
    assert_eq!(tf.controller, Some(ControllerRef::You));
    let execute = def.execute.as_ref().expect("should have execute");
    assert_eq!(execute.player_scope, Some(PlayerFilter::Opponent));
}

// NEGATIVE: bare "mill N" without "cards" suffix is NOT recognized as an
// unless-cost — it could be an effect-mill clause. Only "mill N cards" is.
#[test]
fn trigger_unless_you_mill_is_not_unless_pay() {
    let def = parse_trigger_line(
        "When this creature enters, draw a card unless you mill 3.",
        "Test Card",
    );
    assert!(
        def.unless_pay.is_none(),
        "bare mill without 'cards' suffix must not be extracted, got {:?}",
        def.unless_pay
    );
}

// CR 118.12 + CR 701.17: "sacrifice ~ unless you mill two cards" — Deep
// Spawn. Mill word-count form recognized as unless-pay.
#[test]
fn trigger_unless_you_mill_two_cards() {
    let def = parse_trigger_line(
        "At the beginning of your upkeep, sacrifice ~ unless you mill two cards.",
        "Deep Spawn",
    );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    assert!(
        matches!(unless_pay.cost, AbilityCost::Mill { count: 2 }),
        "expected Mill {{ count: 2 }}, got {:?}",
        unless_pay.cost
    );
}

// CR 118.12 + CR 701.9: "sacrifice it unless you discard two cards" —
// Avatar of Discord. Numeric discard count > 1.
#[test]
fn trigger_unless_you_discard_two_cards() {
    let def = parse_trigger_line(
        "When ~ enters, sacrifice it unless you discard two cards.",
        "Avatar of Discord",
    );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    assert!(
        matches!(
            &unless_pay.cost,
            AbilityCost::Discard { count, .. }
                if matches!(count, QuantityExpr::Fixed { value: 2 })
        ),
        "expected Discard count=2, got {:?}",
        unless_pay.cost
    );
}

// CR 122.6 + CR 118.12: "sacrifice ~ unless you remove a counter from a
// permanent you control" — Chisei, Heart of Oceans. The TARGETED
// remove-counter form has no runtime payment path (handle_unless_payment
// leaves `RemoveCounter { target: Some(_) }` unsupported), so the parser
// must NOT extract it — leaving the clause cleanly unsupported instead of
// emitting an unpayable cost. Self-reference ("from it") remains supported.
#[test]
fn trigger_unless_you_remove_targeted_counter_is_not_extracted() {
    let def = parse_trigger_line(
            "At the beginning of your upkeep, sacrifice ~ unless you remove a counter from a permanent you control.",
            "Chisei, Heart of Oceans",
        );
    assert!(
        def.unless_pay.is_none(),
        "targeted remove-counter unless-cost must not be extracted (unpayable), got {:?}",
        def.unless_pay
    );
}

// CR 118.12 + CR 122.1: "sacrifice ~ unless you remove a +1/+1 counter
// from it" — Junk Golem. Typed counter, self target.
#[test]
fn trigger_unless_you_remove_plus_counter() {
    let def = parse_trigger_line(
        "At the beginning of your upkeep, sacrifice ~ unless you remove a +1/+1 counter from it.",
        "Junk Golem",
    );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    assert!(
        matches!(
            &unless_pay.cost,
            AbilityCost::RemoveCounter {
                count: 1,
                counter_type: CounterMatch::OfType(CounterType::Plus1Plus1),
                target: None,
                ..
            }
        ),
        "expected RemoveCounter +1/+1, got {:?}",
        unless_pay.cost
    );
}

// CR 118.12 + CR 122.1: "return ~ to its owner's hand unless you remove
// two oil counters from it" — Magmatic Sprinter. Numeric count, typed
// counter, self target.
#[test]
fn trigger_unless_you_remove_two_oil_counters() {
    let def = parse_trigger_line(
            "At the beginning of your end step, return ~ to its owner's hand unless you remove two oil counters from it.",
            "Magmatic Sprinter",
        );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    match &unless_pay.cost {
        AbilityCost::RemoveCounter {
            count,
            counter_type,
            target,
            ..
        } => {
            assert_eq!(*count, 2);
            assert!(
                matches!(counter_type, CounterMatch::OfType(ct) if ct == &crate::types::counter::parse_counter_type("oil")),
                "expected oil counter type, got {:?}",
                counter_type
            );
            assert_eq!(*target, None, "self target should be None");
        }
        other => panic!("expected RemoveCounter, got {:?}", other),
    }
}

#[test]
fn trigger_unless_you_pay_its_mana_cost_is_self_mana_cost() {
    // #3791: "unless you pay its mana cost" is now recognized — the cost is
    // the ability source's own printed mana cost, represented as
    // `ManaCost::SelfManaCost` and materialized to the source's mana_cost at
    // resolution time (Pendrell Flux, Disruption Aura, Pendrell Mists).
    let def = parse_trigger_line(
        "When this creature enters, draw a card unless you pay its mana cost.",
        "Test Card",
    );
    let unless = def
        .unless_pay
        .as_ref()
        .expect("\"pay its mana cost\" must be recognized as an unless-cost");
    assert_eq!(
        unless.cost,
        AbilityCost::Mana {
            cost: crate::types::mana::ManaCost::SelfManaCost,
        },
        "\"pay its mana cost\" must lower to Mana{{SelfManaCost}}, got {:?}",
        unless.cost
    );
}

// NO-REGRESSION: bare "unless you pay {2}" still routes through the
// existing mana block (the "you" pronoun is excluded from the explicit-
// pronoun chain), not the new delegation.
#[test]
fn trigger_unless_you_pay_mana_still_routes_to_mana_block() {
    let def = parse_trigger_line(
        "When this creature enters, draw a card unless you pay {2}.",
        "Test Card",
    );
    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    assert_eq!(unless_pay.payer, TargetFilter::Controller);
    assert!(
        matches!(unless_pay.cost, AbilityCost::Mana { .. }),
        "cost should be Mana, got {:?}",
        unless_pay.cost
    );
}

#[test]
fn trigger_unless_they_pay_binds_creature_controller_to_parent_target_controller() {
    let def = parse_trigger_line(
            "Whenever this creature deals combat damage to a creature, that creature's controller loses 2 life unless they pay {2}.",
            "Death Charmer",
        );

    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    assert_eq!(unless_pay.payer, TargetFilter::ParentTargetController);
}

#[test]
fn trigger_unless_they_discard_binds_that_player_to_triggering_player() {
    let def = parse_trigger_line(
        "Whenever an opponent casts a spell, that player loses 5 life unless they discard a card.",
        "Painful Quandary",
    );

    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    assert_eq!(unless_pay.payer, TargetFilter::TriggeringPlayer);
    assert!(
        matches!(
            unless_pay.cost,
            AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                filter: None,
                selection: CardSelectionMode::Chosen,
                self_scope: DiscardSelfScope::FromHand
            }
        ),
        "cost should be DiscardCard, got {:?}",
        unless_pay.cost
    );
}

#[test]
fn trigger_unless_they_discard_multi_sentence_branch_not_terminal_cost() {
    let def = parse_trigger_line(
            "At the beginning of your upkeep, each opponent loses 3 life unless they discard a card. If you're the monarch, instead each opponent loses 6 life unless they discard two cards.",
            "Court of Ambition",
        );

    assert!(
        def.unless_pay.is_none(),
        "multi-sentence branch should not be stripped as one terminal unless cost, got {:?}",
        def.unless_pay
    );
    let execute = def.execute.as_ref().expect("should have execute");
    assert!(
        execute.sub_ability.is_some(),
        "monarch branch should remain available for downstream parsing"
    );
}

#[test]
fn trigger_unless_pay_for_each_uses_dynamic_generic_cost() {
    let def = parse_trigger_line(
            "At the beginning of your upkeep, sacrifice this creature unless you pay {1} for each card in your hand.",
            "Extravagant Spirit",
        );

    let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
    assert_eq!(unless_pay.payer, TargetFilter::Controller);
    assert!(
        matches!(
            unless_pay.cost,
            AbilityCost::ManaDynamic {
                quantity: QuantityExpr::Ref { .. }
            }
        ),
        "unless payment should use dynamic generic cost, got {:?}",
        unless_pay.cost
    );
}

#[test]
fn trigger_unrecognized_unless_payment_preserves_clause_as_gap() {
    let def = parse_trigger_line(
        "When this creature enters, draw a card unless the active player compliments your hat.",
        "Test Card",
    );

    assert!(def.unless_pay.is_none());
    let execute = def.execute.as_ref().expect("should have execute");
    assert!(
        matches!(*execute.effect, Effect::Unimplemented { .. }),
        "unrecognized unless clause must remain visible as Unimplemented, got {:?}",
        execute.effect
    );
}

#[test]
fn trigger_put_into_graveyard_from_battlefield_self() {
    // CR 700.4: "Is put into a graveyard from the battlefield" is a synonym for "dies."
    let def = parse_trigger_line(
        "When ~ is put into a graveyard from the battlefield, return ~ to its owner's hand.",
        "Rancor",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.origin, Some(Zone::Battlefield));
    assert_eq!(def.destination, Some(Zone::Graveyard));
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

#[test]
fn trigger_put_into_graveyard_from_battlefield_another_creature() {
    // plural "are put into a graveyard from the battlefield"
    let def = parse_trigger_line(
            "Whenever a creature you control is put into a graveyard from the battlefield, you gain 1 life.",
            "Some Card",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.origin, Some(Zone::Battlefield));
    assert_eq!(def.destination, Some(Zone::Graveyard));
}

#[test]
fn trigger_blocks_self() {
    let def = parse_trigger_line(
        "Whenever Sustainer of the Realm blocks, it gains +0/+2 until end of turn.",
        "Sustainer of the Realm",
    );
    assert_eq!(def.mode, TriggerMode::Blocks);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

#[test]
fn trigger_blocks_when_prefix() {
    let def = parse_trigger_line(
        "When Stoic Ephemera blocks, it deals 5 damage to each creature blocking or blocked by it.",
        "Stoic Ephemera",
    );
    assert_eq!(def.mode, TriggerMode::Blocks);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

#[test]
fn trigger_blocks_a_creature() {
    let def = parse_trigger_line(
            "Whenever Wall of Frost blocks a creature, that creature doesn't untap during its controller's next untap step.",
            "Wall of Frost",
        );
    assert_eq!(def.mode, TriggerMode::Blocks);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

#[test]
fn trigger_blocks_or_becomes_blocked() {
    // "blocks or becomes blocked" — parsed as Blocks (blocker side)
    let def = parse_trigger_line(
        "Whenever Karn, Silver Golem blocks or becomes blocked, it gets -4/+4 until end of turn.",
        "Karn, Silver Golem",
    );
    assert_eq!(def.mode, TriggerMode::Blocks);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

#[test]
fn trigger_creature_you_control_blocks() {
    let def = parse_trigger_line(
        "Whenever a creature you control blocks, draw a card.",
        "Some Card",
    );
    assert_eq!(def.mode, TriggerMode::Blocks);
}

#[test]
fn trigger_chaos_ensues_mode() {
    let def = parse_trigger_line("Whenever chaos ensues, draw a card.", "Plane");
    assert_eq!(def.mode, TriggerMode::ChaosEnsues);
    // CR 311.7: self-referential — fires for its own plane.
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

#[test]
fn trigger_planeswalk_away_from_mode() {
    // CR 701.31d: "Whenever you planeswalk away from [this plane]".
    let def = parse_trigger_line(
        "Whenever you planeswalk away from Test Plane, draw a card.",
        "Test Plane",
    );
    assert_eq!(def.mode, TriggerMode::PlaneswalkedFrom);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn trigger_planeswalk_to_mode() {
    // CR 701.31d: "Whenever you planeswalk to [this plane]".
    let def = parse_trigger_line(
        "Whenever you planeswalk to Test Plane, draw a card.",
        "Test Plane",
    );
    assert_eq!(def.mode, TriggerMode::PlaneswalkedTo);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn trigger_encounter_maps_to_planeswalked_to() {
    // CR 312.5: "When you encounter [this phenomenon]" is the face-up
    // (planeswalked-to) endpoint.
    let def = parse_trigger_line(
        "When you encounter Test Phenomenon, draw a card.",
        "Test Phenomenon",
    );
    assert_eq!(def.mode, TriggerMode::PlaneswalkedTo);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

/// DEFERRED GAP (documented, not fixed): Caught in a Parallel Universe is a
/// Planechase phenomenon whose encounter effect is a per-player, left-neighbor,
/// many-to-many choose-and-copy — "each player chooses a creature controlled by
/// the player to their left. Each player creates a token that's a copy of the
/// creature they chose, except it has menace."
///
/// Modeling this correctly needs infrastructure the engine does not yet have:
///   * a left-neighbor `ControllerRef` — CR 103.1 fixes turn order (starting
///     player, proceeding clockwise) and thus "the player to their left", but no
///     filter controller ref resolves it (`ControllerRef` has no
///     `PlayerToTheLeft`/left-neighbor variant);
///   * a per-player PARALLEL selection where every player is simultaneously a
///     chooser binding their own creature — `Effect::ChooseObjectsIntoTrackedSet`
///     has a single `chooser` and one tracked set (CR 608.2c), not one binding
///     per player; and
///   * a per-player token copy keyed to each chooser's own binding —
///     `CopyTokenOf { target: ParentTarget }` inherits ONE parent target, not a
///     per-player selection (CR 707.2).
///
/// This is a single Planechase phenomenon, not a card class, so the per-player
/// left-neighbor choose head is deliberately left as a strict-failure
/// `Unimplemented { name: "choose" }` gap rather than mis-modeled with the
/// single-chooser machinery. This test LOCKS that documented state so the card
/// is not silently counted as fixed and a future change can't quietly alter the
/// shape. When per-player parallel-selection infrastructure lands, replace this
/// with a positive end-to-end test.
#[test]
fn caught_in_a_parallel_universe_per_player_left_neighbor_choose_is_deferred_gap() {
    let def = parse_trigger_line(
        "When you encounter Caught in a Parallel Universe, each player chooses a \
         creature controlled by the player to their left. Each player creates a \
         token that's a copy of the creature they chose, except it has menace. \
         (Then planeswalk away from this phenomenon.)",
        "Caught in a Parallel Universe",
    );
    // CR 312.5: the encounter maps to the face-up (planeswalked-to) endpoint.
    assert_eq!(def.mode, TriggerMode::PlaneswalkedTo);
    let execute = def
        .execute
        .as_deref()
        .expect("the encounter trigger must carry an execute body");
    // The per-player left-neighbor selection head is unsupported: it must remain
    // a documented `Unimplemented { name: "choose" }` strict-failure, NOT be
    // mis-converted into a single-chooser `ChooseObjectsIntoTrackedSet`.
    match &*execute.effect {
        Effect::Unimplemented { name, .. } => assert_eq!(
            name, "choose",
            "the deferred per-player choose head must stay an Unimplemented choose gap"
        ),
        other => panic!(
            "Caught in a Parallel Universe's per-player left-neighbor choose is a \
             deferred gap and must remain Unimplemented, got {other:?}"
        ),
    }
}

#[test]
fn fixed_point_in_time_full_trigger_parses_replacement_with_duration() {
    // CR 312.5 + CR 614.1a + CR 901.9c: the full production parser must carry
    // the encounter trigger, duration shell, and planar-die replacement payload
    // together for Fixed Point in Time.
    let parsed = parse_oracle_text(
        "When you encounter Fixed Point in Time, until your next turn, if a player would planeswalk as a result of rolling the planar die, chaos ensues instead.",
        "Fixed Point in Time",
        &[],
        &["Phenomenon".to_string()],
        &[],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|trigger| trigger.mode == TriggerMode::PlaneswalkedTo)
        .expect("Fixed Point in Time encounter trigger must parse");

    assert_eq!(trigger.valid_card, Some(TargetFilter::SelfRef));
    let execute = trigger
        .execute
        .as_ref()
        .expect("Fixed Point in Time trigger must execute");
    assert_eq!(
        execute.duration,
        Some(Duration::UntilNextTurnOf {
            player: PlayerScope::Controller
        })
    );
    match execute.effect.as_ref() {
        Effect::CreatePlaneswalkReplacement { replacement_effect } => {
            assert!(matches!(replacement_effect.as_ref(), Effect::ChaosEnsues));
        }
        other => panic!("expected CreatePlaneswalkReplacement, got {other:?}"),
    }
}

#[test]
fn trigger_arrival_phrase_axis_all_map_to_planeswalked_to() {
    // CR 312.5 / CR 701.31d: every arrival/encounter phrasing in the class
    // maps to PlaneswalkedTo with the controller target filter — including
    // the "here" and literal "this plane"/"this phenomenon" forms that do
    // NOT normalize to ~ (so they exercise the literal arms of the axis).
    for (oracle, name) in [
        // "planeswalk here" — Ghirapur Grand Prix's arrival trigger.
        (
            "When you planeswalk here, draw a card.",
            "Ghirapur Grand Prix",
        ),
        ("Whenever you planeswalk here, draw a card.", "Some Plane"),
        // literal "this plane" (not a SELF_REF_TYPE_PHRASE → stays literal).
        (
            "When you planeswalk to this plane, draw a card.",
            "Some Plane",
        ),
        // literal "this phenomenon".
        (
            "When you encounter this phenomenon, draw a card.",
            "Some Phenomenon",
        ),
    ] {
        let def = parse_trigger_line(oracle, name);
        assert_eq!(
            def.mode,
            TriggerMode::PlaneswalkedTo,
            "`{oracle}` should map to PlaneswalkedTo",
        );
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::SelfRef),
            "`{oracle}` arrival trigger is self-referential",
        );
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Controller),
            "`{oracle}` resolves the arrival for the planar controller",
        );
    }
}

#[test]
fn trigger_chaos_no_zone_stamped_by_parser() {
    // Synthesis stamps trigger_zones=[Command]; the parser must NOT (preserves
    // the trigger_no_zone test-lock). Confirm the parser leaves it default.
    let def = parse_trigger_line("Whenever chaos ensues, draw a card.", "Plane");
    assert!(
        !def.trigger_zones.contains(&Zone::Command),
        "parser must not stamp Zone::Command — synthesis owns that, got {:?}",
        def.trigger_zones
    );
}

#[test]
fn trigger_set_in_motion_mode() {
    let def = parse_trigger_line("When you set this scheme in motion, draw a card.", "Scheme");
    assert_eq!(def.mode, TriggerMode::SetInMotion);
}

#[test]
fn trigger_crank_contraption_mode() {
    let def = parse_trigger_line(
        "Whenever you crank this Contraption, create a token.",
        "Contraption",
    );
    assert_eq!(def.mode, TriggerMode::CrankContraption);
}

#[test]
fn trigger_dungeon_completed_whenever() {
    let def = parse_trigger_line(
        "Whenever you complete a dungeon, create a 2/2 green Wolf creature token.",
        "Varis, Silverymoon Ranger",
    );
    assert_eq!(def.mode, TriggerMode::DungeonCompleted);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}
#[test]
fn trigger_dungeon_completed_when() {
    let def = parse_trigger_line(
        "When you complete a dungeon, create a 5/5 red Dragon creature token with flying.",
        "Loot Dispute",
    );
    assert_eq!(def.mode, TriggerMode::DungeonCompleted);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn trigger_you_win_a_coin_flip() {
    // CR 705.2: "Whenever you win a coin flip" fires only on won flips.
    let def = parse_trigger_line(
        "Whenever you win a coin flip, draw a card.",
        "Krark's Thumb",
    );
    assert_eq!(def.mode, TriggerMode::FlippedCoin);
    assert_eq!(def.coin_flip_result, Some(CoinFlipResult::Won));
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn trigger_you_lose_a_coin_flip() {
    let def = parse_trigger_line(
        "Whenever you lose a coin flip, target opponent gains 1 life.",
        "Bad Luck Charm",
    );
    assert_eq!(def.mode, TriggerMode::FlippedCoin);
    assert_eq!(def.coin_flip_result, Some(CoinFlipResult::Lost));
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn trigger_a_player_loses_a_coin_flip() {
    let def = parse_trigger_line(
        "When a player loses a coin flip, you gain 1 life.",
        "Tavern Swindler",
    );
    assert_eq!(def.mode, TriggerMode::FlippedCoin);
    assert_eq!(def.coin_flip_result, Some(CoinFlipResult::Lost));
    assert_eq!(def.valid_target, Some(TargetFilter::Player));
}

#[test]
fn trigger_opponent_wins_a_coin_flip() {
    let def = parse_trigger_line(
        "Whenever an opponent wins a coin flip, you gain 1 life.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::FlippedCoin);
    assert_eq!(def.coin_flip_result, Some(CoinFlipResult::Won));
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        ))
    );
}

#[test]
fn trigger_coin_flip_rejects_partial_suffix() {
    let def = parse_trigger_line("Whenever you win a coin flipper, draw a card.", "Test Card");
    assert!(matches!(def.mode, TriggerMode::Unknown(_)));
}

#[test]
fn trigger_ring_tempts_you_whenever() {
    let def = parse_trigger_line(
        "Whenever the Ring tempts you, you may discard your hand.",
        "Sauron, the Dark Lord",
    );
    assert_eq!(def.mode, TriggerMode::RingTemptsYou);
}

#[test]
fn trigger_ring_tempts_you_draws_card() {
    let def = parse_trigger_line("Whenever the Ring tempts you, draw a card.", "Ring Watcher");
    assert_eq!(def.mode, TriggerMode::RingTemptsYou);
    assert!(
        def.execute.is_some(),
        "draw effect must lower onto the trigger execute slot"
    );
    assert!(matches!(
        def.execute.as_ref().unwrap().effect.as_ref(),
        Effect::Draw { .. }
    ));
}

#[test]
fn trigger_ring_tempts_you_when() {
    let def = parse_trigger_line(
        "When the Ring tempts you, return this card from your graveyard to your hand.",
        "Ringwraiths",
    );
    assert_eq!(def.mode, TriggerMode::RingTemptsYou);
}

#[test]
fn trigger_ring_tempts_you_target_opponent_reveal_sets_valid_target() {
    let def = parse_trigger_line(
            "Whenever the Ring tempts you, target opponent reveals cards from the top of their library until they reveal a land card. Put that card onto the battlefield tapped under your control and the rest into their graveyard.",
            "Sméagol, Helpful Guide",
        );
    assert_eq!(def.mode, TriggerMode::RingTemptsYou);
    assert_eq!(def.valid_target, Some(TargetFilter::Player));
    let Effect::RevealUntil {
        player,
        filter,
        kept_destination,
        rest_destination,
        enter_tapped,
        enters_under,
        ..
    } = def.execute.as_ref().unwrap().effect.as_ref()
    else {
        panic!("expected RevealUntil");
    };
    assert!(
        matches!(
            player,
            TargetFilter::Typed(tf) if tf.controller == Some(ControllerRef::Opponent)
        ),
        "reveal player must be opponent, got {player:?}"
    );
    assert!(
        matches!(filter, TargetFilter::Typed(tf) if tf.type_filters.contains(&TypeFilter::Land)),
        "filter must be land, got {filter:?}"
    );
    assert_eq!(*kept_destination, Zone::Battlefield);
    assert_eq!(*rest_destination, Zone::Graveyard);
    assert_eq!(
        *enter_tapped,
        crate::types::zones::EtbTapState::Tapped,
        "land must enter tapped"
    );
    assert_eq!(
        *enters_under,
        Some(ControllerRef::You),
        "stolen land must enter under ability controller"
    );
}

#[test]
fn trigger_rolled_die_batch() {
    let def = parse_trigger_line(
        "Whenever you roll one or more dice, put a +1/+1 counter on ~.",
        "Vrondiss, Rage of Ancients",
    );
    assert_eq!(def.mode, TriggerMode::RolledDie);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert!(def.batched);
    assert_eq!(def.die_sides, None);
}

#[test]
fn trigger_rolled_die_single() {
    let def = parse_trigger_line(
        "Whenever you roll a die, put a +1/+1 counter on ~.",
        "The Space Family Goblinson",
    );
    assert_eq!(def.mode, TriggerMode::RolledDieOnce);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert!(!def.batched);
    assert_eq!(def.die_sides, None);
}

#[test]
fn trigger_rolled_die_opponent_scope() {
    let def = parse_trigger_line(
        "Whenever an opponent rolls a die, draw a card.",
        "Barbarian Class",
    );
    assert_eq!(def.mode, TriggerMode::RolledDieOnce);
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        ))
    );
    assert_eq!(def.die_sides, None);
}

#[test]
fn trigger_rolled_d20_filters_sides() {
    let def = parse_trigger_line(
        "Whenever you roll a d20, put a +1/+1 counter on ~.",
        "Pixie Guide",
    );
    assert_eq!(def.mode, TriggerMode::RolledDieOnce);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert_eq!(def.die_sides, Some(20));
}

#[test]
fn trigger_rolled_die_result_exact_single() {
    let def = parse_trigger_line(
        "Whenever you roll a 1, create a Treasure token.",
        "Complaints Clerk",
    );
    assert_eq!(def.mode, TriggerMode::RolledDieOnce);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert_eq!(def.die_sides, None);
    assert_eq!(def.die_result, Some(DieResultFilter::Exact(vec![1])));
}

#[test]
fn trigger_rolled_die_result_exact_disjunction() {
    // Atomwheel Acrobats: "Whenever you roll a 1 or 2, ...".
    let def = parse_trigger_line(
        "Whenever you roll a 1 or 2, put that many +1/+1 counters on this creature.",
        "Atomwheel Acrobats",
    );
    assert_eq!(def.mode, TriggerMode::RolledDieOnce);
    assert_eq!(def.die_result, Some(DieResultFilter::Exact(vec![1, 2])));
}

#[test]
fn trigger_rolled_die_result_at_least_higher() {
    // Monoxa, Midway Manager: "Whenever you roll a 3 or higher, ...".
    let def = parse_trigger_line(
        "Whenever you roll a 3 or higher, Monoxa gains first strike until end of turn.",
        "Monoxa, Midway Manager",
    );
    assert_eq!(def.mode, TriggerMode::RolledDieOnce);
    assert_eq!(def.die_result, Some(DieResultFilter::AtLeast(3)));
}

#[test]
fn trigger_rolled_die_result_at_least_more() {
    // "N or more" is the alternate GE phrasing; folds to AtLeast like "N or higher".
    let def = parse_trigger_line(
        "Whenever you roll a 4 or more, draw a card.",
        "Die Roll GE Phrasing",
    );
    assert_eq!(def.mode, TriggerMode::RolledDieOnce);
    assert_eq!(def.die_result, Some(DieResultFilter::AtLeast(4)));
}

#[test]
fn trigger_rolled_die_no_result_filter_is_none() {
    // Regression: the bare "a die" form classifies as RolledDieOnce with no
    // result filter, and the batched "one or more dice" form is RolledDie.
    let single = parse_trigger_line(
        "Whenever you roll a die, put a +1/+1 counter on ~.",
        "The Space Family Goblinson",
    );
    assert_eq!(single.mode, TriggerMode::RolledDieOnce);
    assert_eq!(single.die_result, None);

    let batch = parse_trigger_line(
        "Whenever you roll one or more dice, put a +1/+1 counter on ~.",
        "Vrondiss, Rage of Ancients",
    );
    assert_eq!(batch.mode, TriggerMode::RolledDie);
    assert_eq!(batch.die_result, None);
}

#[test]
fn trigger_turn_face_up_mode() {
    let def = parse_trigger_line(
        "When this creature is turned face up, draw a card.",
        "Morphling",
    );
    assert_eq!(def.mode, TriggerMode::TurnFaceUp);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

#[test]
fn trigger_commit_crime_mode() {
    // CR 700.13: "you commit a crime" scopes the trigger to the controller.
    let def = parse_trigger_line("Whenever you commit a crime, draw a card.", "At Knifepoint");
    assert_eq!(def.mode, TriggerMode::CommitCrime);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn trigger_commit_crime_opponent_scope() {
    // CR 700.13: "an opponent commits a crime" → Patrolling Peacemaker / similar.
    let def = parse_trigger_line(
        "Whenever an opponent commits a crime, proliferate.",
        "Patrolling Peacemaker",
    );
    assert_eq!(def.mode, TriggerMode::CommitCrime);
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        ))
    );
    assert_eq!(def.constraint, None);
}

#[test]
fn trigger_commit_crime_any_player_scope() {
    // CR 700.13: "a player commits a crime" → fires for any player (Tarnation / similar).
    let def = parse_trigger_line(
        "Whenever a player commits a crime, they may draw a card.",
        "Tarnation",
    );
    assert_eq!(def.mode, TriggerMode::CommitCrime);
    assert_eq!(def.valid_target, Some(TargetFilter::Player));
    assert_eq!(def.constraint, None);
}

#[test]
fn trigger_commit_crime_during_your_turn() {
    // CR 700.13 + CR 500.6: "during your turn" restricts the trigger to the controller's turn.
    let def = parse_trigger_line(
            "Whenever you commit a crime during your turn, this creature gains indestructible until end of turn.",
            "Overzealous Muscle",
        );
    assert_eq!(def.mode, TriggerMode::CommitCrime);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
}

// CR 701.62: "Whenever you manifest dread" — actor-side Manifest Dread
// trigger, gated on controller via TargetFilter::Controller.
#[test]
fn trigger_manifest_dread_actor_side() {
    let def = parse_trigger_line(
            "Whenever you manifest dread, put a card you put into your graveyard this way into your hand.",
            "Paranormal Analyst",
        );
    assert_eq!(def.mode, TriggerMode::ManifestDread);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

// CR 708 + CR 701.40b: "Whenever you turn a permanent face up" — actor-side
// TurnFaceUp trigger. `valid_card` records the subject, `valid_target`
// gates on the turning player being the trigger controller.
#[test]
fn trigger_turn_permanent_face_up_actor_side() {
    let def = parse_trigger_line(
        "Whenever you turn a permanent face up, put a +1/+1 counter on it.",
        "Growing Dread",
    );
    assert_eq!(def.mode, TriggerMode::TurnFaceUp);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Permanent)))
    );
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

// CR 708 + CR 701.40b: creature-subject variant of the actor-side trigger.
#[test]
fn trigger_turn_creature_face_up_actor_side() {
    let def = parse_trigger_line(
        "Whenever you turn a creature face up, draw a card.",
        "Hypothetical Morph Payoff",
    );
    assert_eq!(def.mode, TriggerMode::TurnFaceUp);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)))
    );
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn trigger_commit_crime_returns_this_card_from_graveyard_sets_graveyard_zone() {
    let def = parse_trigger_line(
            "Whenever you commit a crime, you may pay {B}. If you do, return this card from your graveyard to the battlefield.",
            "Forsaken Miner",
        );
    assert_eq!(def.mode, TriggerMode::CommitCrime);
    assert_eq!(def.trigger_zones, vec![Zone::Graveyard]);
    let execute = def.execute.expect("should have execute");
    let if_you_do = execute
        .sub_ability
        .expect("should have if-you-do sub ability");
    assert!(matches!(
        *if_you_do.effect,
        crate::types::ability::Effect::ChangeZone {
            origin: Some(Zone::Graveyard),
            destination: Zone::Battlefield,
            target: TargetFilter::SelfRef,
            ..
        }
    ));
}

#[test]
fn trigger_day_night_changes_mode() {
    let def = parse_trigger_line(
        "Whenever day becomes night or night becomes day, draw a card.",
        "Firmament Sage",
    );
    assert_eq!(def.mode, TriggerMode::DayTimeChanges);
}

#[test]
fn trigger_end_of_combat_phase() {
    let def = parse_trigger_line(
        "At end of combat, sacrifice this creature.",
        "Ball Lightning",
    );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::EndCombat));
}

#[test]
fn trigger_becomes_target_mode() {
    let def = parse_trigger_line(
        "When this creature becomes the target of a spell or ability, sacrifice it.",
        "Frost Walker",
    );
    assert_eq!(def.mode, TriggerMode::BecomesTarget);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert_eq!(def.valid_source, None); // spell OR ability — no source filter
}

#[test]
fn trigger_becomes_target_of_spell_only() {
    let def = parse_trigger_line(
            "Whenever this creature becomes the target of a spell, this creature deals 2 damage to that spell's controller.",
            "Bonecrusher Giant",
        );
    assert_eq!(def.mode, TriggerMode::BecomesTarget);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert_eq!(def.valid_source, Some(TargetFilter::StackSpell));
}

#[test]
fn trigger_becomes_target_you_control_sets_controller_source() {
    // Valiant (#1378): "a spell or ability you control" must restrict the
    // targeting source's controller, not fire for any source.
    let def = parse_trigger_line(
            "Whenever this creature becomes the target of a spell or ability you control for the first time each turn, put a +1/+1 counter on it.",
            "Heartfire Hero",
        );
    assert_eq!(def.mode, TriggerMode::BecomesTarget);
    assert_eq!(
        def.valid_source,
        Some(becomes_target_source_filter(ControllerRef::You))
    );
}

#[test]
fn trigger_becomes_target_opponent_controls_sets_controller_source() {
    // CR 115.1: the same spell-or-ability controller grammar supports
    // opponent-scoped source restrictions.
    let def = parse_trigger_line(
            "Whenever this creature becomes the target of a spell or ability an opponent controls, draw a card.",
            "Opponent-Scoped Observer",
        );
    assert_eq!(def.mode, TriggerMode::BecomesTarget);
    assert_eq!(
        def.valid_source,
        Some(becomes_target_source_filter(ControllerRef::Opponent))
    );
}

#[test]
fn trigger_becomes_target_pawpatch_recruit_pattern() {
    // Pawpatch Recruit pattern: "whenever a creature you control becomes the target
    // of a spell or ability an opponent controls"
    // This test verifies both valid_card (creature you control) and valid_source
    // (opponent controls) are set correctly — the exact pattern from bug #1569.
    let def = parse_trigger_line(
            "Whenever a creature you control becomes the target of a spell or ability an opponent controls, put a +1/+1 counter on target creature you control other than that creature.",
            "Pawpatch Recruit",
        );
    assert_eq!(def.mode, TriggerMode::BecomesTarget);
    // "a creature you control" → valid_card with ControllerRef::You
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You),
        ))
    );
    // "an opponent controls" → valid_source with ControllerRef::Opponent
    assert_eq!(
        def.valid_source,
        Some(becomes_target_source_filter(ControllerRef::Opponent))
    );
}

#[test]
fn trigger_becomes_target_of_aura_spell_only() {
    let def = parse_trigger_line(
        "Whenever this creature becomes the target of an Aura spell, you draw a card.",
        "Fugitive Druid",
    );
    assert_eq!(def.mode, TriggerMode::BecomesTarget);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert_eq!(
        def.valid_source,
        Some(TargetFilter::And {
            filters: vec![
                TargetFilter::StackSpell,
                TargetFilter::Typed(TypedFilter::default().subtype("Aura".to_string())),
            ],
        })
    );
}

#[test]
fn trigger_becomes_target_of_instant_or_sorcery_spell() {
    let def = parse_trigger_line(
            "Whenever a creature you control becomes the target of an instant or sorcery spell, that creature gets +3/+3 until end of turn.",
            "Wild Defiance",
        );
    assert_eq!(def.mode, TriggerMode::BecomesTarget);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Creature).controller(ControllerRef::You),
        ))
    );
    assert_eq!(
        def.valid_source,
        Some(TargetFilter::And {
            filters: vec![
                TargetFilter::StackSpell,
                TargetFilter::Or {
                    filters: vec![
                        TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant)),
                        TargetFilter::Typed(TypedFilter::new(TypeFilter::Sorcery)),
                    ],
                },
            ],
        })
    );
}

#[test]
fn trigger_becomes_target_of_backup_ability() {
    // CR 702.165a: Huge Truck pattern — "becomes the target of a backup
    // ability" parses to a `BecomesTarget` trigger whose `valid_source` is a
    // stack ability tagged `Backup`, with the subject ("another creature you
    // control") routed to `valid_card`.
    let def = parse_trigger_line(
            "Whenever another creature you control becomes the target of a backup ability, draw a card.",
            "Huge Truck",
        );
    assert_eq!(def.mode, TriggerMode::BecomesTarget);
    // valid_source is the new backup-tag stack-ability filter — this is the
    // assertion that flips if the converter arm is reverted.
    assert_eq!(
        def.valid_source,
        Some(TargetFilter::StackAbility {
            controller: None,
            tag: Some(AbilityTag::Backup),
            kind: None,
        })
    );
    // Subject: "another creature you control" → Typed creature / You / Another.
    let TargetFilter::Typed(TypedFilter {
        type_filters,
        controller,
        properties,
    }) = def.valid_card.expect("backup trigger has a subject filter")
    else {
        panic!("expected a Typed subject filter for the backup trigger");
    };
    assert_eq!(type_filters, vec![TypeFilter::Creature]);
    assert_eq!(controller, Some(ControllerRef::You));
    assert!(properties.contains(&FilterProp::Another));
}

#[test]
fn trigger_loki_becomes_target_of_ability_you_control() {
    // §8.0 — Loki, God of Mischief. CR 115.1a + CR 602.2b (ability-only source),
    // CR 110.1 (battlefield-scoped permanent leaf), CR 603.2h (once each turn).
    let def = parse_trigger_line(
            "Whenever a player or permanent becomes the target of an ability you control, draw a card. This ability triggers only once each turn.",
            "Loki, God of Mischief",
        );
    assert_eq!(def.mode, TriggerMode::BecomesTarget);
    // Player leaf → valid_subject_player (the SUBJECT player axis), NOT
    // valid_target. valid_target is the effect-target slot and stays None here
    // because "draw a card" is untargeted — this separation is what stops a
    // player-targeting effect (Venerated Rotpriest) from over-firing.
    assert_eq!(def.valid_subject_player, Some(TargetFilter::Player));
    assert_eq!(def.valid_target, None);
    // Permanent leaf → valid_card, battlefield-scoped (CR 110.1). Asserting the
    // InZone prop is present is what flips if the §3c zone gate is reverted.
    let TargetFilter::Typed(TypedFilter {
        type_filters,
        controller,
        properties,
    }) = def
        .valid_card
        .clone()
        .expect("permanent leaf must populate valid_card")
    else {
        panic!(
            "expected a Typed permanent valid_card, got {:?}",
            def.valid_card
        );
    };
    assert_eq!(type_filters, vec![TypeFilter::Permanent]);
    assert_eq!(controller, None);
    assert!(
        properties.contains(&FilterProp::InZone {
            zone: Zone::Battlefield
        }),
        "permanent leaf must be battlefield-scoped (CR 110.1); got {properties:?}"
    );
    // Ability-only source (NOT the spell-or-ability Or), you-controlled. This is
    // the assertion that flips if the arm reused becomes_target_source_filter.
    assert_eq!(
        def.valid_source,
        Some(TargetFilter::StackAbility {
            controller: Some(ControllerRef::You),
            tag: None,
            kind: None,
        })
    );
    // "This ability triggers only once each turn." → OncePerTurn (auto-wired).
    assert_eq!(def.constraint, Some(TriggerConstraint::OncePerTurn));
    // Effect body: untargeted single-card draw.
    let execute = def.execute.as_ref().expect("Loki has an execute body");
    match &*execute.effect {
        Effect::Draw { count, .. } => {
            assert_eq!(*count, QuantityExpr::Fixed { value: 1 });
        }
        other => panic!("expected a Draw effect body, got {other:?}"),
    }
}

#[test]
fn trigger_skophos_maze_warden_of_an_ability_stays_unknown() {
    // §8.0-neg (F1 prefix-collision guard). "becomes the target of an ability
    // OF A LAND you control named ..." carries a source restriction this arm
    // cannot model. The optional controller clause does not match the leading
    // "of a land..." (the embedded "you control" is mid-phrase), so the
    // non-empty remainder trips the remaining-empty guard → fall through to
    // Unknown rather than over-firing as a bare BecomesTargetAbility.
    let def = parse_trigger_line(
            "Whenever another creature becomes the target of an ability of a land you control named Labyrinth of Skophos, you may have this creature fight that creature.",
            "Skophos Maze-Warden",
        );
    assert!(
        matches!(def.mode, TriggerMode::Unknown(_)),
        "Skophos Maze-Warden must NOT parse to a BecomesTarget trigger; got {:?}",
        def.mode
    );
}

#[test]
fn trigger_agrus_kos_of_an_ability_stays_unknown() {
    // §8.0-neg (F1). "...of an ability THAT TARGETS ONLY IT" has no controller
    // clause, so the non-empty remainder trips the guard → Unknown.
    let def = parse_trigger_line(
            "Whenever Agrus Kos, Eternal Soldier becomes the target of an ability that targets only it, you may pay {1}{R/W}. If you do, copy that ability. You may choose new targets for the copy.",
            "Agrus Kos, Eternal Soldier",
        );
    assert!(
        matches!(def.mode, TriggerMode::Unknown(_)),
        "Agrus Kos, Eternal Soldier must NOT parse to a BecomesTarget trigger; got {:?}",
        def.mode
    );
}

#[test]
fn trigger_valkmira_mixed_subject_splits_player_and_object_axes_full_pipeline() {
    // §8.0-mixed (LOW-2). CR 115.1: a mixed "you or <permanent>" subject routes
    // the player leaf → valid_subject_player and the object leaf → valid_card so
    // the becomes-target matcher's Player arm can fire on either kind.
    //
    // Asserts against the FULL card pipeline (parse_oracle_text), not just
    // parse_trigger_line, so it reflects what actually ships. IMPORTANT: of the
    // five corpus cards with a mixed player+object becomes-target subject, only
    // Valkmira ("you or ANOTHER permanent you control") reaches this Or-split in
    // the full pipeline. The other four (Leovold, Parnesse, Rayne, Unsettled
    // Mariner) use "you or A permanent you control", which an upstream line-split
    // breaks into a separate `Unknown("Whenever you")` + a permanent-only
    // BecomesTarget, so THEIR player halves remain unfired. That upstream gap is
    // pre-existing and out of scope here.
    //
    // TODO(parser-gap): the "Whenever you or a permanent you control …" upstream
    // split (vs. "you or another permanent …") drops the player leaf for the four
    // leading-"you" cards before set_trigger_subject ever sees the Or. Fixing the
    // line-splitter to keep that subject intact would route their player halves
    // through this same Or-split.
    use crate::parser::oracle::parse_oracle_text;
    let parsed = parse_oracle_text(
            "If a source an opponent controls would deal damage to you or a permanent you control, prevent 1 of that damage.\n\
             Whenever you or another permanent you control becomes the target of a spell or ability an opponent controls, counter that spell or ability unless its controller pays {1}.",
            "Valkmira, Protector's Shield",
            &[],
            &["Artifact".to_string()],
            &[],
        );
    let bt = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::BecomesTarget)
        .expect("Valkmira's becomes-target trigger must survive the full pipeline");
    // Player leaf "you" → valid_subject_player (NOT valid_target, which is the
    // effect-target slot and is None here — the counter effect is untargeted).
    assert_eq!(bt.valid_subject_player, Some(TargetFilter::Controller));
    assert_eq!(bt.valid_target, None);
    // Object leaf "another permanent you control" → valid_card.
    assert!(
        bt.valid_card.is_some(),
        "mixed subject must populate valid_card (permanent half); got None"
    );
    // The opponent-controlled spell-or-ability source axis is preserved.
    assert_eq!(
        bt.valid_source,
        Some(becomes_target_source_filter(ControllerRef::Opponent))
    );
}

#[test]
fn trigger_batched_become_target_of_instant_or_sorcery_spell() {
    let def = parse_trigger_line(
            "Whenever one or more creatures you control become the target of an instant or sorcery spell, draw a card.",
            "Hypothetical Wild Defiance Payoff",
        );
    assert_eq!(def.mode, TriggerMode::BecomesTarget);
    assert!(def.batched);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Creature).controller(ControllerRef::You),
        ))
    );
    assert_eq!(
        def.valid_source,
        Some(TargetFilter::And {
            filters: vec![
                TargetFilter::StackSpell,
                TargetFilter::Or {
                    filters: vec![
                        TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant)),
                        TargetFilter::Typed(TypedFilter::new(TypeFilter::Sorcery)),
                    ],
                },
            ],
        })
    );
}

#[test]
fn trigger_you_become_target_uses_valid_target() {
    let def = parse_trigger_line(
        "Whenever you become the target of a spell or ability, draw a card.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::BecomesTarget);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert_eq!(def.valid_card, None);
    assert_eq!(def.valid_source, None);
}

#[test]
fn trigger_opponents_are_dealt_combat_damage_uses_valid_target() {
    let def = parse_trigger_line(
        "Whenever one or more opponents are dealt combat damage, draw a card.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::DamageReceived);
    assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    assert!(def.batched);
    assert_eq!(def.valid_card, None);
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        ))
    );
}

#[test]
fn mindblade_render_intervening_if_warrior_source_parsed() {
    // CR 603.4 + CR 120.1: the "if any of that damage was dealt by a
    // Warrior" intervening-if must be hoisted to the trigger condition, not
    // dropped (issue #2867).
    let def = parse_trigger_line(
            "Whenever your opponents are dealt combat damage, if any of that damage was dealt by a Warrior, you draw a card and you lose 1 life.",
            "Mindblade Render",
        );
    assert_eq!(
        def.condition,
        Some(TriggerCondition::EventDamageSourceMatchesFilter {
            filter: TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Subtype("Warrior".to_string())],
                controller: None,
                properties: vec![],
            }),
        }),
    );
}

#[test]
fn event_damage_source_intervening_if_handles_disjunction_and_combat_word() {
    // The class generalizes past one card: a "~ or a Dragon" source
    // disjunction and the optional "combat" qualifier both compose via the
    // shared `parse_target` + `merge_or_filters` chain (CR 120.1 + CR 608.2i).
    let def = parse_trigger_line(
            "Whenever your opponents are dealt combat damage, if any of that combat damage was dealt by a Dragon, draw a card.",
            "Test Card",
        );
    assert_eq!(
        def.condition,
        Some(TriggerCondition::EventDamageSourceMatchesFilter {
            filter: TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Subtype("Dragon".to_string())],
                controller: None,
                properties: vec![],
            }),
        }),
    );
}

#[test]
fn trigger_opponents_creatures_dealt_excess_noncombat_damage() {
    let def = parse_trigger_line(
            "Whenever one or more creatures your opponents control are dealt excess noncombat damage, create a Treasure token.",
            "Become Brutes",
        );
    assert_eq!(def.mode, TriggerMode::ExcessDamageAll);
    assert_eq!(def.damage_kind, DamageKindFilter::NoncombatOnly);
    assert!(def.batched);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::Opponent)
        ))
    );
    assert_eq!(def.valid_target, None);
}

#[test]
fn trigger_becomes_monstrous_mode() {
    let def = parse_trigger_line(
            "When this creature becomes monstrous, creatures without flying your opponents control can't block this turn.",
            "Stoneshock Giant",
        );
    assert_eq!(def.mode, TriggerMode::BecomeMonstrous);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

#[test]
fn trigger_becomes_renowned_mode_with_subject_filter() {
    let def = parse_trigger_line(
        "Whenever a creature you control becomes renowned, draw a card.",
        "Valeron Wardens",
    );
    assert_eq!(def.mode, TriggerMode::BecomeRenowned);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Creature).controller(ControllerRef::You)
        ))
    );
}

#[test]
fn trigger_put_into_graveyard_from_anywhere() {
    let def = parse_trigger_line(
        "When this card is put into a graveyard from anywhere, draw a card.",
        "Dread",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Graveyard));
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert_eq!(def.origin, None);
}

#[test]
fn trigger_you_discard_a_card() {
    let def = parse_trigger_line(
        "Whenever you discard a card, draw a card.",
        "Bag of Holding",
    );
    assert_eq!(def.mode, TriggerMode::Discarded);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Card).controller(ControllerRef::You)
        ))
    );
}

#[test]
fn trigger_opponent_discards_a_card() {
    let def = parse_trigger_line(
        "Whenever an opponent discards a card, draw a card.",
        "Geth's Grimoire",
    );
    assert_eq!(def.mode, TriggerMode::Discarded);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Card).controller(ControllerRef::Opponent)
        ))
    );
}

/// CR 109.5 + CR 603.2: "When a spell or ability an opponent controls causes
/// you to discard this card, [effect]" (Guerrilla Tactics, Sand Golem) — a
/// self-discard trigger gated by the `EventSourceControlledBy { Opponent }`
/// constraint. Issue #3109-style. (issue67)
#[test]
fn trigger_opponent_causes_you_to_discard_this_card() {
    let def = parse_trigger_line(
            "When a spell or ability an opponent controls causes you to discard this card, this card deals 4 damage to any target.",
            "Guerrilla Tactics",
        );
    assert_eq!(def.mode, TriggerMode::Discarded);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert_eq!(
        def.constraint,
        Some(
            crate::types::ability::TriggerConstraint::EventSourceControlledBy {
                controller: ControllerRef::Opponent
            }
        )
    );
    // The discarded card has already moved hand->graveyard (or exile under a
    // madness/RIP redirect) by the time the Discarded event is scanned, so the
    // trigger must function off the battlefield to fire at all.
    assert_eq!(def.trigger_zones, vec![Zone::Graveyard, Zone::Exile]);
}

/// CR 701.9 + CR 603.7c + CR 406.1: Necropotence's on-discard trigger
/// exiles the just-discarded card from the graveyard. The "that card"
/// anaphor must lift from `ParentTarget` to `TriggeringSource` so the
/// `ChangeZone { origin: Some(Graveyard), destination: Exile }` resolves
/// against the discarded object, and the controller filter must be
/// `You`. Locks the parse output for the Necropotence punisher-class
/// (Necropotence, Yawgmoth's Bargain-ish self-discard punishers).
#[test]
fn trigger_necropotence_you_discard_exile_from_graveyard() {
    let def = parse_trigger_line(
        "Whenever you discard a card, exile that card from your graveyard.",
        "Necropotence",
    );
    assert_eq!(def.mode, TriggerMode::Discarded);
    match def.valid_card.as_ref().expect("valid_card must be set") {
        TargetFilter::Typed(tf) => {
            assert_eq!(tf.controller, Some(ControllerRef::You));
        }
        other => panic!("expected Typed filter, got {other:?}"),
    }
    let execute = def
        .execute
        .as_deref()
        .expect("on-discard trigger must have an execute body");
    match execute.effect.as_ref() {
        Effect::ChangeZone {
            origin,
            destination,
            target,
            ..
        } => {
            assert_eq!(*origin, Some(Zone::Graveyard));
            assert_eq!(*destination, Zone::Exile);
            assert!(
                matches!(target, TargetFilter::TriggeringSource),
                "expected TriggeringSource after lift, got {target:?}",
            );
        }
        other => panic!("expected ChangeZone (graveyard → exile), got {other:?}"),
    }
}

/// CR 701.9a + CR 603.2c: type qualifier on the discarded card must be
/// preserved so a "discards a land card" trigger fires only on land
/// discards (Aclazotz, Deepest Betrayal class).
#[test]
fn trigger_opponent_discards_a_land_card() {
    let def = parse_trigger_line(
            "Whenever an opponent discards a land card, create a 1/1 black Bat creature token with flying.",
            "Aclazotz, Deepest Betrayal",
        );
    assert_eq!(def.mode, TriggerMode::Discarded);
    match def.valid_card.as_ref().expect("valid_card must be set") {
        TargetFilter::Typed(tf) => {
            assert!(
                tf.type_filters.contains(&TypeFilter::Land),
                "expected Land in type_filters, got {:?}",
                tf.type_filters
            );
            assert_eq!(tf.controller, Some(ControllerRef::Opponent));
        }
        other => panic!("expected Typed filter, got {other:?}"),
    }
}

#[test]
fn trigger_you_sacrifice_another_permanent() {
    let def = parse_trigger_line(
        "Whenever you sacrifice another permanent, draw a card.",
        "Furnace Celebration",
    );
    assert_eq!(def.mode, TriggerMode::Sacrificed);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Permanent)
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::Another])
        ))
    );
}

#[test]
fn trigger_player_cycles_a_card() {
    let def = parse_trigger_line(
        "Whenever a player cycles a card, draw a card.",
        "Astral Slide",
    );
    assert_eq!(def.mode, TriggerMode::Cycled);
}

#[test]
fn trigger_spell_cast_or_copy_mode() {
    let def = parse_trigger_line(
        "Whenever you cast or copy an instant or sorcery spell, create a Treasure token.",
        "Storm-Kiln Artist",
    );
    assert_eq!(def.mode, TriggerMode::SpellCastOrCopy);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

// CR 601.2a + CR 707.10: try_parse_casts_or_copies_trigger — all actor variants.

#[test]
fn trigger_opponent_casts_or_copies_instant_or_sorcery() {
    // Mage Hunter: "Whenever an opponent casts or copies an instant or sorcery spell,
    // they lose 1 life." — SpellCastOrCopy with opponent-controller restriction.
    let def = parse_trigger_line(
            "Whenever an opponent casts or copies an instant or sorcery spell, that player loses 1 life.",
            "Mage Hunter",
        );
    assert_eq!(def.mode, TriggerMode::SpellCastOrCopy);
    assert!(
        matches!(
            def.valid_target,
            Some(TargetFilter::Typed(ref t)) if matches!(t.controller, Some(ControllerRef::Opponent))
        ),
        "expected Opponent-controller TypedFilter in valid_target, got {:?}",
        def.valid_target
    );
    assert!(
        matches!(def.valid_card, Some(TargetFilter::Or { .. })),
        "expected Or{{Instant, Sorcery}} in valid_card, got {:?}",
        def.valid_card
    );
}

#[test]
fn trigger_opponent_casts_or_copies_any_spell() {
    let def = parse_trigger_line(
        "Whenever an opponent casts or copies a spell, draw a card.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::SpellCastOrCopy);
    assert!(
        matches!(
            def.valid_target,
            Some(TargetFilter::Typed(ref t)) if matches!(t.controller, Some(ControllerRef::Opponent))
        ),
        "expected Opponent-controller TypedFilter, got {:?}",
        def.valid_target
    );
    assert_eq!(def.valid_card, None);
}

#[test]
fn trigger_a_player_casts_or_copies_a_spell() {
    let def = parse_trigger_line(
        "Whenever a player casts or copies a spell, you gain 1 life.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::SpellCastOrCopy);
    assert_eq!(def.valid_target, Some(TargetFilter::Player));
    assert_eq!(def.valid_card, None);
}

#[test]
fn trigger_unlock_door_mode() {
    let def = parse_trigger_line("When you unlock this door, draw a card.", "Door");
    assert_eq!(def.mode, TriggerMode::UnlockDoor);
}

#[test]
fn trigger_you_fully_unlock_a_room_mode() {
    let def = parse_trigger_line(
        "Whenever you fully unlock a Room, draw a card.",
        "Entity Tracker",
    );
    assert_eq!(def.mode, TriggerMode::FullyUnlock);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::default().subtype("Room".to_string())
        ))
    );
    assert!(matches!(
        def.execute
            .as_deref()
            .map(|ability| ability.effect.as_ref()),
        Some(Effect::Draw { .. })
    ));
}

#[test]
fn trigger_you_fully_unlock_room_self_return_uses_graveyard_zone() {
    let def = parse_trigger_line(
            "Whenever you fully unlock a Room, you may return this card from your graveyard to your hand.",
            "Fear of Infinity",
        );
    assert_eq!(def.mode, TriggerMode::FullyUnlock);
    assert_eq!(def.trigger_zones, vec![Zone::Graveyard]);
    assert!(def.optional);
    assert!(matches!(
        def.execute
            .as_deref()
            .map(|ability| ability.effect.as_ref()),
        Some(Effect::Bounce {
            target: TargetFilter::SelfRef,
            destination: None,
            selection: BounceSelection::Targeted,
        })
    ));
}

#[test]
fn trigger_card_name_self_return_uses_graveyard_zone() {
    let def = parse_trigger_line(
            "Whenever you fully unlock a Room, you may return Fear of Infinity from your graveyard to your hand.",
            "Fear of Infinity",
        );
    assert_eq!(def.mode, TriggerMode::FullyUnlock);
    assert_eq!(def.trigger_zones, vec![Zone::Graveyard]);
    assert!(matches!(
        def.execute
            .as_deref()
            .map(|ability| ability.effect.as_ref()),
        Some(Effect::Bounce {
            target: TargetFilter::SelfRef,
            destination: None,
            selection: BounceSelection::Targeted,
        })
    ));
}

#[test]
fn phase_trigger_self_bounce_stays_battlefield_hosted() {
    let def = parse_trigger_line(
            "At the beginning of your upkeep, if you control no Thopters other than this creature, return ~ to its owner's hand and create five 1/1 colorless Thopter artifact creature tokens with flying.",
            "Thopter Assembly",
        );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::Upkeep));
    assert!(
        def.trigger_zones.is_empty() || def.trigger_zones == vec![Zone::Battlefield],
        "ordinary phase trigger must not be hosted from graveyard: {:?}",
        def.trigger_zones
    );
    let execute = def.execute.as_deref().expect("should have execute");
    assert!(matches!(
        execute.effect.as_ref(),
        Effect::Bounce {
            target: TargetFilter::SelfRef,
            destination: None,
            selection: BounceSelection::Targeted,
        }
    ));
    assert!(matches!(
        execute
            .sub_ability
            .as_deref()
            .map(|ability| ability.effect.as_ref()),
        Some(Effect::Token { name, .. }) if name == "Thopter"
    ));
}

#[test]
fn trigger_this_card_becomes_plotted_uses_exile_zone() {
    let def = parse_trigger_line(
        "When this card becomes plotted, it deals 2 damage to any target.",
        "Aloe Alchemist",
    );
    assert_eq!(def.mode, TriggerMode::BecomesPlotted);
    assert_eq!(def.trigger_zones, vec![Zone::Exile]);
    assert!(matches!(
        def.execute
            .as_deref()
            .map(|ability| ability.effect.as_ref()),
        Some(Effect::DealDamage { .. })
    ));
}

#[test]
fn trigger_mutates_mode() {
    let def = parse_trigger_line("Whenever this creature mutates, draw a card.", "Gemrazer");
    assert_eq!(def.mode, TriggerMode::Mutates);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

#[test]
fn trigger_becomes_untapped_mode() {
    let def = parse_trigger_line(
        "Whenever this creature becomes untapped, draw a card.",
        "Arbiter of the Ideal",
    );
    assert_eq!(def.mode, TriggerMode::Untaps);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

#[test]
fn trigger_self_or_another_ally_enters() {
    let def = parse_trigger_line(
        "Whenever this creature or another Ally you control enters, you gain 1 life.",
        "Hada Freeblade",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert!(matches!(def.valid_card, Some(TargetFilter::Or { .. })));
    assert_eq!(def.destination, Some(Zone::Battlefield));
}

#[test]
fn trigger_may_have_self_become_named_equipment_if_you_do() {
    let def = parse_trigger_line(
            "Whenever a legendary creature you control enters, you may have The Irencrag become a legendary Equipment artifact named Everflame, Heroes' Legacy. If you do, it gains equip {3} and \"Equipped creature gets +3/+3\" and loses all other abilities.",
            "The Irencrag",
        );

    assert!(def.optional);
    let execute = def.execute.as_ref().expect("trigger execute ability");
    assert!(execute.optional);

    let Effect::GenericEffect {
        static_abilities, ..
    } = execute.effect.as_ref()
    else {
        panic!("expected GenericEffect, got {:?}", execute.effect);
    };
    let modifications = &static_abilities[0].modifications;
    assert!(
        modifications.iter().any(|modification| matches!(
            modification,
            ContinuousModification::SetName { name } if name == "Everflame, Heroes' Legacy"
        )),
        "expected SetName in {modifications:?}",
    );
    assert!(
        modifications.iter().any(|modification| matches!(
            modification,
            ContinuousModification::AddSubtype { subtype } if subtype == "Equipment"
        )),
        "expected AddSubtype(Equipment) in {modifications:?}",
    );

    let sub_ability = execute.sub_ability.as_ref().expect("If you do sub-ability");
    assert!(sub_ability
        .condition
        .as_ref()
        .is_some_and(AbilityCondition::is_optional_effect_performed));
    let Effect::GenericEffect {
        static_abilities, ..
    } = sub_ability.effect.as_ref()
    else {
        panic!(
            "expected GenericEffect sub-ability, got {:?}",
            sub_ability.effect
        );
    };
    assert!(matches!(
        static_abilities[0].affected,
        Some(TargetFilter::SelfRef)
    ));
    let sub_modifications = &static_abilities[0].modifications;
    assert!(
        sub_modifications
            .iter()
            .any(|modification| matches!(modification, ContinuousModification::RemoveAllAbilities)),
        "expected RemoveAllAbilities in {sub_modifications:?}",
    );
}

#[test]
fn trigger_another_human_you_control_enters() {
    let def = parse_trigger_line(
        "Whenever another Human you control enters, draw a card.",
        "Welcoming Vampire",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::default()
                .subtype("Human".to_string())
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::Another])
        ))
    );
}

#[test]
fn trigger_dragon_you_control_attacks() {
    let def = parse_trigger_line(
        "Whenever a Dragon you control attacks, create a Treasure token.",
        "Ganax, Astral Hunter",
    );
    assert_eq!(def.mode, TriggerMode::Attacks);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::default()
                .subtype("Dragon".to_string())
                .controller(ControllerRef::You)
        ))
    );
}

#[test]
fn trigger_samurai_or_warrior_attacks_alone() {
    let def = parse_trigger_line(
        "Whenever a Samurai or Warrior you control attacks alone, draw a card.",
        "Raiyuu, Storm's Edge",
    );
    // Now that parse_type_phrase recognizes subtypes ("Samurai", "Warrior"),
    // the trigger parser correctly identifies this as an Attacks trigger.
    assert!(matches!(def.mode, TriggerMode::Attacks));
}

#[test]
fn trigger_this_siege_enters_is_self_etb() {
    let def = parse_trigger_line("When this Siege enters, draw a card.", "Invasion");
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

// --- Phase trigger possessive qualifier tests ---

#[test]
fn phase_trigger_your_upkeep() {
    let def = parse_trigger_line("At the beginning of your upkeep, draw a card.", "Test Card");
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::Upkeep));
    assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
}

#[test]
fn hope_estheim_end_step_mill_keeps_you_quantity_controller_scoped() {
    let def = parse_trigger_line(
            "At the beginning of your end step, each opponent mills X cards, where X is the amount of life you gained this turn.",
            "Hope Estheim",
        );
    let execute = def.execute.as_ref().expect("trigger execute ability");

    assert_eq!(execute.player_scope, Some(PlayerFilter::Opponent));
    match &*execute.effect {
        Effect::Mill { target, count, .. } => {
            assert_eq!(*target, TargetFilter::Controller);
            assert!(matches!(
                count,
                QuantityExpr::Ref {
                    qty: QuantityRef::LifeGainedThisTurn {
                        player: PlayerScope::Controller
                    }
                }
            ));
        }
        other => panic!("expected Mill effect, got {other:?}"),
    }
}

/// Issue #1307: Moseo, Vein's New Dean's Infusion ability — an end-step
/// trigger gated by an intervening-if condition whose effect's
/// `ChangeZone` target filter bears a `Cmc` bound defined by a trailing
/// "where X is …" clause. `apply_where_x_effect_expression` previously had
/// no `Effect::ChangeZone` arm, so the filter's bound stayed an unresolved
/// bare `Variable("X")` (resolves to 0 at runtime via
/// `QuantityRef::Variable`), making the reanimation target only mana value
/// 0 or less and the ability appear to never fire for any real graveyard
/// card. The fix threads the where-X rewrite into `ChangeZone`'s target
/// filter the same way `SearchLibrary`/`Seek` already do.
#[test]
fn moseo_infusion_end_step_reanimate_binds_cmc_to_life_gained_this_turn() {
    let def = parse_trigger_line(
            "At the beginning of your end step, if you gained life this turn, return up to one target creature card with mana value X or less from your graveyard to the battlefield, where X is the amount of life you gained this turn.",
            "Moseo, Vein's New Dean",
        );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::End));
    let execute = def.execute.as_ref().expect("trigger execute ability");
    match execute.effect.as_ref() {
        Effect::ChangeZone {
            origin: Some(Zone::Graveyard),
            destination: Zone::Battlefield,
            target,
            ..
        } => match target {
            TargetFilter::Typed(typed) => {
                assert!(
                    typed.properties.iter().any(|prop| matches!(
                        prop,
                        FilterProp::Cmc {
                            comparator: Comparator::LE,
                            value: QuantityExpr::Ref {
                                qty: QuantityRef::LifeGainedThisTurn {
                                    player: PlayerScope::Controller
                                }
                            },
                        }
                    )),
                    "expected Cmc{{LE, LifeGainedThisTurn{{Controller}}}} bound, got {:?}",
                    typed.properties
                );
            }
            other => panic!("expected Typed target filter, got {other:?}"),
        },
        other => panic!("expected ChangeZone effect, got {other:?}"),
    }
}

#[test]
fn phase_trigger_combat_on_your_turn() {
    let def = parse_trigger_line(
        "At the beginning of combat on your turn, target creature gets +1/+1 until end of turn.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::BeginCombat));
    assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
}

/// Issue #1993: Halana and Alena, Partners — X in the counter clause must bind
/// to source power, not an unresolved Variable name.
#[test]
fn halana_alena_partners_combat_trigger_puts_source_power_counters() {
    let def = parse_trigger_line(
            "At the beginning of combat on your turn, put X +1/+1 counters on another target creature you control, where X is Halana and Alena's power. That creature gains haste until end of turn.",
            "Halana and Alena, Partners",
        );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::BeginCombat));
    let execute = def.execute.as_ref().expect("execute");
    match execute.effect.as_ref() {
        Effect::PutCounter {
            count,
            counter_type,
            target,
        } => {
            assert_eq!(
                counter_type,
                &crate::types::counter::CounterType::Plus1Plus1
            );
            assert_eq!(
                count,
                &QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: ObjectScope::Source,
                    },
                },
                "where X is printed name's power must bind to Source, got {count:?}"
            );
            assert!(
                matches!(target, TargetFilter::Typed(_)),
                "expected typed creature target, got {target:?}"
            );
        }
        other => panic!("expected PutCounter, got {other:?}"),
    }
}

#[test]
fn phase_trigger_each_players_upkeep_no_constraint() {
    let def = parse_trigger_line(
        "At the beginning of each player's upkeep, that player draws a card.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::Upkeep));
    assert_eq!(def.constraint, None);
}

/// CR 603.2b + CR 608.2c: Roiling Vortex — "At the beginning of each player's
/// upkeep, this enchantment deals 1 damage to them." The bare player anaphor
/// "them" is the player whose upkeep it is — the same referent "that player"
/// resolves to in this trigger class (`ScopedPlayer`, which the runtime binds
/// to the active player at fire time). It must NOT fall back to the object
/// `ParentTarget` anaphor, which has no referent so the damage hits no one
/// (issue #2891).
#[test]
fn phase_trigger_each_players_upkeep_deals_damage_to_them() {
    let def = parse_trigger_line(
        "At the beginning of each player's upkeep, this enchantment deals 1 damage to them.",
        "Roiling Vortex",
    );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::Upkeep));
    match def.execute.as_ref().map(|ability| ability.effect.as_ref()) {
        Some(Effect::DealDamage { target, amount, .. }) => {
            assert_eq!(target, &TargetFilter::ScopedPlayer);
            assert_eq!(amount, &QuantityExpr::Fixed { value: 1 });
        }
        other => panic!("expected DealDamage to ScopedPlayer, got {other:?}"),
    }
}

/// CR 603.2 + CR 608.2c: Razorkin Needlehead — "Whenever an opponent draws a
/// card, this creature deals 1 damage to them." The player-actor trigger
/// subject ("an opponent") makes "them" the triggering player; with no
/// explicit player scope, the bare "them" damage recipient must fall back to
/// `TriggeringPlayer` rather than the object anaphor `TriggeringSource`,
/// which has no player referent so the damage hits no one (issue #2869).
#[test]
fn opponent_draws_trigger_deals_damage_to_them_binds_triggering_player() {
    let def = parse_trigger_line(
        "Whenever an opponent draws a card, this creature deals 1 damage to them.",
        "Razorkin Needlehead",
    );
    match def.execute.as_ref().map(|ability| ability.effect.as_ref()) {
        Some(Effect::DealDamage { target, amount, .. }) => {
            assert_eq!(target, &TargetFilter::TriggeringPlayer);
            assert_eq!(amount, &QuantityExpr::Fixed { value: 1 });
        }
        other => panic!("expected DealDamage to TriggeringPlayer, got {other:?}"),
    }
}

/// CR 603.7c + CR 608.2c: God-Pharaoh's Gift — "create a token that's a copy
/// of that card … It gains haste." The "It gains haste" grant, nested as the
/// token creator's own sub-ability, must apply to the newly created token
/// (`LastCreated`), not the source artifact (`SelfRef`). Issue #2356.
#[test]
fn token_creator_it_gains_keyword_binds_to_created_token() {
    fn first_grant_affected(
        def: &crate::types::ability::AbilityDefinition,
    ) -> Option<TargetFilter> {
        if let Effect::GenericEffect {
            static_abilities, ..
        } = &*def.effect
        {
            if let Some(s) = static_abilities.first() {
                return s.affected.clone();
            }
        }
        def.sub_ability.as_deref().and_then(first_grant_affected)
    }

    let def = parse_trigger_line(
            "At the beginning of combat on your turn, you may exile a creature card from your graveyard. If you do, create a token that's a copy of that card, except it's a 4/4 black Zombie. It gains haste until end of turn.",
            "God-Pharaoh's Gift",
        );
    let affected = first_grant_affected(def.execute.as_deref().expect("execute ability"));
    assert_eq!(
        affected,
        Some(TargetFilter::LastCreated),
        "the 'It gains haste' grant must apply to the created token, not SelfRef"
    );
}

/// CR 603.2 + CR 608.2c + CR 115.10a: Kederekt Parasite — "Whenever an opponent draws a
/// card, ... you may have this creature deal 1 damage to that player." The
/// optional "you may have ~ deal N damage to that player" causative frame
/// must bind "that player" to the triggering (drawing) player, not to a
/// chooseable `Player` target. The damaged player is fixed by the trigger
/// event (CR 603.2), so no target selection is offered (issue #2893).
#[test]
fn opponent_draws_may_have_deal_damage_to_that_player_binds_triggering_player() {
    let def = parse_trigger_line(
            "Whenever an opponent draws a card, if you control a red permanent, you may have this creature deal 1 damage to that player.",
            "Kederekt Parasite",
        );
    let execute = def.execute.as_ref().expect("execute must be Some");
    // The "you may" wording makes the resolution optional (yes/no), not a
    // forced effect.
    assert!(
        execute.optional,
        "execute.optional must be true ('you may')"
    );
    match execute.effect.as_ref() {
        Effect::DealDamage { target, amount, .. } => {
            assert_eq!(
                target,
                &TargetFilter::TriggeringPlayer,
                "'that player' must bind to the drawing opponent (TriggeringPlayer), \
                     not a chooseable Player target",
            );
            assert_eq!(amount, &QuantityExpr::Fixed { value: 1 });
        }
        other => panic!("expected DealDamage to TriggeringPlayer, got {other:?}"),
    }
}

/// CR 613.1 + CR 503.1a: The Rack — "the chosen player's upkeep" must
/// scope the phase trigger to `SourceChosenPlayer`, not every active player.
#[test]
fn phase_trigger_chosen_players_upkeep_scopes_to_source_chosen_player() {
    let def = parse_trigger_line(
            "At the beginning of the chosen player's upkeep, this artifact deals X damage to that player, where X is 3 minus the number of cards in their hand.",
            "The Rack",
        );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::Upkeep));
    assert_eq!(def.valid_target, Some(TargetFilter::SourceChosenPlayer));
    match def.execute.as_ref().map(|ability| ability.effect.as_ref()) {
        Some(Effect::DealDamage { target, amount, .. }) => {
            assert_eq!(target, &TargetFilter::SourceChosenPlayer);
            assert!(matches!(amount, QuantityExpr::ClampMin { minimum: 0, .. }));
        }
        other => panic!("expected DealDamage, got {other:?}"),
    }
}

/// CR 613.1 + CR 503.1a: Curly-apostrophe "chosen player's upkeep" must
/// scope the phase trigger to the source's chosen player.
#[test]
fn phase_trigger_chosen_player_curly_apostrophe_scopes_to_source_chosen_player() {
    let def = parse_trigger_line(
            "At the beginning of the chosen player\u{2019}s upkeep, this artifact deals X damage to that player, where X is 3 minus the number of cards in their hand.",
            "The Rack",
        );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::Upkeep));
    assert_eq!(def.valid_target, Some(TargetFilter::SourceChosenPlayer));
    match def.execute.as_ref().map(|ability| ability.effect.as_ref()) {
        Some(Effect::DealDamage { target, amount, .. }) => {
            assert_eq!(target, &TargetFilter::SourceChosenPlayer);
            assert!(matches!(amount, QuantityExpr::ClampMin { minimum: 0, .. }));
        }
        other => panic!("expected DealDamage, got {other:?}"),
    }
}

/// CR 603.2b + CR 102.1: Dictate of Kruphix / Kami of the Crescent Moon —
/// "At the beginning of each player's draw step, that player draws an
/// additional card." `target` must be `ScopedPlayer`; the runtime then
/// binds it to the active player at trigger fire time
/// (`triggers::build_triggered_ability` for Phase triggers).
#[test]
fn phase_trigger_each_players_draw_step_that_player_draws() {
    let def = parse_trigger_line(
        "At the beginning of each player's draw step, that player draws an additional card.",
        "Dictate of Kruphix",
    );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::Draw));
    assert_eq!(def.constraint, None);
    match def.execute.as_ref().map(|ability| ability.effect.as_ref()) {
        Some(Effect::Draw { target, .. }) => {
            assert_eq!(target, &TargetFilter::ScopedPlayer);
        }
        other => panic!("expected Draw effect with ScopedPlayer target, got {other:?}"),
    }
}

/// CR 603.4 + CR 104.2b: Triskaidekaphile — "if you have exactly thirteen
/// cards in your hand" must hoist as HandSize EQ 13, not fire WinTheGame
/// unconditionally every upkeep (issue #2371).
#[test]
fn phase_trigger_exactly_thirteen_cards_in_hand_win_the_game() {
    let def = parse_trigger_line(
            "At the beginning of your upkeep, if you have exactly thirteen cards in your hand, you win the game.",
            "Triskaidekaphile",
        );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::Upkeep));
    match def.condition.as_ref() {
        Some(TriggerCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        }) => {
            assert_eq!(
                lhs,
                &QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::Controller,
                    },
                }
            );
            assert_eq!(*comparator, Comparator::EQ);
            assert_eq!(rhs, &QuantityExpr::Fixed { value: 13 });
        }
        other => panic!("expected HandSize EQ 13 intervening-if, got {other:?}"),
    }
    match def.execute.as_ref().map(|ability| ability.effect.as_ref()) {
        Some(Effect::WinTheGame { .. }) => {}
        other => panic!("expected WinTheGame effect, got {other:?}"),
    }
}

/// CR 603.2b + CR 603.4 + CR 102.1: Ghirapur Orrery — the intervening-if
/// "if that player has no cards in hand" must hoist onto the trigger
/// definition as a `QuantityComparison` against `HandSize { ScopedPlayer }`,
/// not be silently dropped. Without this, every player would draw three
/// cards on every upkeep regardless of hand size.
#[test]
fn phase_trigger_intervening_if_that_player_no_cards_in_hand() {
    let def = parse_trigger_line(
            "At the beginning of each player's upkeep, if that player has no cards in hand, that player draws three cards.",
            "Ghirapur Orrery",
        );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::Upkeep));
    match def.condition.as_ref() {
        Some(TriggerCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        }) => {
            assert_eq!(
                lhs,
                &QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::ScopedPlayer,
                    },
                }
            );
            assert_eq!(*comparator, Comparator::EQ);
            assert_eq!(rhs, &QuantityExpr::Fixed { value: 0 });
        }
        other => panic!("expected QuantityComparison condition, got {other:?}"),
    }
    match def.execute.as_ref().map(|ability| ability.effect.as_ref()) {
        Some(Effect::Draw { target, .. }) => {
            assert_eq!(target, &TargetFilter::ScopedPlayer);
        }
        other => panic!("expected Draw effect with ScopedPlayer target, got {other:?}"),
    }
}

#[test]
fn phase_trigger_braids_sacrifices_artifact_creature_or_land() {
    let def = parse_trigger_line(
            "At the beginning of each player's upkeep, that player sacrifices an artifact, a creature, or a land.",
            "Braids, Cabal Minion",
        );

    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::Upkeep));
    match def.execute.as_ref().map(|ability| ability.effect.as_ref()) {
        Some(Effect::Sacrifice { target, .. }) => match target {
            TargetFilter::Or { filters } => {
                assert_eq!(filters.len(), 3);
                assert!(filters.iter().any(|f| matches!(
                    f,
                    TargetFilter::Typed(tf)
                        if tf.type_filters == [TypeFilter::Artifact]
                )));
                assert!(filters.iter().any(|f| matches!(
                    f,
                    TargetFilter::Typed(tf) if tf.type_filters == [TypeFilter::Creature]
                )));
                assert!(filters.iter().any(|f| matches!(
                    f,
                    TargetFilter::Typed(tf) if tf.type_filters == [TypeFilter::Land]
                )));
                assert!(filters.iter().all(|f| matches!(
                    f,
                    TargetFilter::Typed(tf)
                        if tf.controller == Some(ControllerRef::ScopedPlayer)
                )));
            }
            other => panic!("expected Or sacrifice filter, got {other:?}"),
        },
        other => panic!("expected Sacrifice effect, got {other:?}"),
    }
}

#[test]
fn phase_trigger_that_player_sacrifices_uses_scoped_player_not_target_player() {
    let def = parse_trigger_line(
            "At the beginning of each player's upkeep, that player sacrifices a non-Elf creature of their choice.",
            "Ruthless Winnower",
        );

    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::Upkeep));
    assert_eq!(def.valid_target, None);
    match def.execute.as_ref().map(|ability| ability.effect.as_ref()) {
        Some(Effect::Sacrifice { target, .. }) => match target {
            TargetFilter::Typed(filter) => {
                assert_eq!(filter.controller, Some(ControllerRef::ScopedPlayer));
            }
            other => panic!("expected typed sacrifice filter, got {other:?}"),
        },
        other => panic!("expected Sacrifice effect, got {other:?}"),
    }
}

#[test]
fn phase_trigger_enchanted_players_first_upkeep() {
    let def = parse_trigger_line(
            "At the beginning of enchanted player's first upkeep each turn, that player gets an additional upkeep step after this step.",
            "Paradox Haze",
        );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::Upkeep));
    assert_eq!(def.valid_target, Some(TargetFilter::AttachedTo));
    assert_eq!(
        def.constraint,
        Some(TriggerConstraint::MaxTimesPerTurn { max: 1 })
    );
    assert!(matches!(
        def.execute.as_ref().map(|ability| ability.effect.as_ref()),
        Some(Effect::AdditionalPhase {
            target: TargetFilter::TriggeringPlayer,
            phase: Phase::Upkeep,
            after: Phase::Upkeep,
            followed_by,
            ..
        }) if followed_by.is_empty()
    ));
}

#[test]
fn phase_trigger_each_opponents_upkeep() {
    let def = parse_trigger_line(
        "At the beginning of each opponent's upkeep, this creature deals 1 damage to that player.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::Upkeep));
    assert_eq!(
        def.constraint,
        Some(TriggerConstraint::OnlyDuringOpponentsTurn)
    );
}

/// CR 513.1 + CR 603.4: Keeper of the Accord — opponent end-step token trigger
/// with intervening-if comparing that player's creatures to yours.
#[test]
fn keeper_of_the_accord_opponent_end_step_creature_token() {
    let def = parse_trigger_line(
            "At the beginning of each opponent's end step, if that player controls more creatures than you, create a 1/1 white Soldier creature token.",
            "Keeper of the Accord",
        );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::End));
    assert_eq!(
        def.constraint,
        Some(TriggerConstraint::OnlyDuringOpponentsTurn)
    );
    match def.condition.as_ref() {
        Some(TriggerCondition::QuantityComparison {
            lhs,
            comparator: Comparator::GT,
            rhs,
        }) => {
            match lhs {
                QuantityExpr::Ref {
                    qty:
                        QuantityRef::ObjectCount {
                            filter: TargetFilter::Typed(tf),
                        },
                } => {
                    assert_eq!(tf.controller, Some(ControllerRef::ScopedPlayer));
                }
                other => panic!("expected scoped ObjectCount lhs, got {other:?}"),
            }
            match rhs {
                QuantityExpr::Ref {
                    qty:
                        QuantityRef::ObjectCount {
                            filter: TargetFilter::Typed(tf),
                        },
                } => {
                    assert_eq!(tf.controller, Some(ControllerRef::You));
                }
                other => panic!("expected you-scoped ObjectCount rhs, got {other:?}"),
            }
        }
        other => panic!("expected QuantityComparison intervening-if, got {other:?}"),
    }
    assert!(matches!(
        def.execute.as_ref().map(|a| a.effect.as_ref()),
        Some(Effect::Token { .. })
    ));
}

/// CR 513.1 + CR 603.4: Keeper of the Accord — optional Plains search when
/// that player controls more lands.
#[test]
fn keeper_of_the_accord_opponent_end_step_plains_search() {
    let def = parse_trigger_line(
            "At the beginning of each opponent's end step, if that player controls more lands than you, you may search your library for a basic Plains card, put it onto the battlefield tapped, then shuffle.",
            "Keeper of the Accord",
        );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::End));
    assert!(def.optional);
    assert!(def.condition.is_some());
}

#[test]
fn phase_trigger_each_combat_no_constraint() {
    let def = parse_trigger_line(
        "At the beginning of each combat, create a 1/1 white Soldier creature token.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::BeginCombat));
    assert_eq!(def.constraint, None);
}

#[test]
fn trigger_optional_sub_ability_not_optional() {
    // "you may" applies to the first sentence only; the sub-ability
    // should not inherit optional.
    let def = parse_trigger_line(
            "When this creature enters, you may draw a card. Create a 1/1 white Soldier creature token.",
            "Some Card",
        );
    assert!(def.optional);
    let execute = def.execute.as_ref().unwrap();
    assert!(execute.optional, "root ability should be optional");
    let sub = execute
        .sub_ability
        .as_ref()
        .expect("should have sub_ability");
    assert!(!sub.optional, "sub-ability should NOT be optional");
}

#[test]
fn trigger_you_may_mid_chain_not_trigger_optional() {
    // "you may" is in the second sentence — trigger-level optional is false,
    // but the second sentence's ability should have optional = true.
    let def = parse_trigger_line(
        "When this creature enters, draw a card. You may discard a card.",
        "Some Card",
    );
    assert!(!def.optional, "trigger-level optional should be false");
    let execute = def.execute.as_ref().unwrap();
    assert!(!execute.optional, "root ability should NOT be optional");
    let sub = execute
        .sub_ability
        .as_ref()
        .expect("should have sub_ability");
    assert!(sub.optional, "second sentence ability should be optional");
}

#[test]
fn trigger_you_may_cast_target_instant_from_graveyard_is_not_trigger_optional() {
    let def = parse_trigger_line(
            "When this creature enters, you may cast target instant card from your graveyard without paying its mana cost. If that spell would be put into your graveyard, exile it instead.",
            "Torrential Gearhulk",
        );
    assert!(
        !def.optional,
        "CR 603.3d: targeted cast triggers must go on the stack when legal targets exist"
    );
    let execute = def.execute.as_ref().expect("ETB execute body");
    assert!(
        execute.optional,
        "CR 608.2c: the cast itself is optional at resolution"
    );
    assert_eq!(
        execute.target_choice_timing,
        crate::types::ability::TargetChoiceTiming::Stack,
        "graveyard instant target must be chosen when the trigger goes on the stack"
    );
    assert!(
        matches!(execute.effect.as_ref(), Effect::CastFromZone { .. }),
        "expected CastFromZone root effect, got {:?}",
        execute.effect
    );
    if let Effect::CastFromZone {
        without_paying_mana_cost,
        duration,
        ..
    } = execute.effect.as_ref()
    {
        assert!(
            *without_paying_mana_cost,
            "Gearhulk cast must be without paying mana cost"
        );
        assert!(
            duration.is_none(),
            "immediate graveyard cast must not carry a standing duration: {duration:?}"
        );
    }
}

// ── Work Item 1: Leaves-Graveyard Batch Triggers ──────────────

#[test]
fn trigger_one_or_more_creature_cards_leave_graveyard() {
    let def = parse_trigger_line(
            "Whenever one or more creature cards leave your graveyard, create a 1/1 green and black Insect creature token.",
            "Insidious Roots",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZoneAll);
    assert_eq!(def.origin, Some(Zone::Graveyard));
    assert!(def.batched);
    assert_owned_by_you(def.valid_card.as_ref().expect("valid_card"));
    // CR 113.6 / CR 113.6b: Insidious Roots is a battlefield permanent whose
    // "leave your graveyard" trigger references other cards, not itself, so it
    // functions only from the battlefield (make_base() default). CR 603.10a's
    // graveyard/exile look-back applies only to self-referential leaves triggers.
    assert_eq!(def.trigger_zones, vec![Zone::Battlefield]);
}

#[test]
fn trigger_one_or_more_cards_leave_graveyard() {
    let def = parse_trigger_line(
        "Whenever one or more cards leave your graveyard, put a +1/+1 counter on this creature.",
        "Chalk Outline",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZoneAll);
    assert_eq!(def.origin, Some(Zone::Graveyard));
    assert!(def.batched);
    let filter = def.valid_card.as_ref().expect("valid_card");
    assert_owned_by_you(filter);
    assert!(
        matches!(filter, TargetFilter::Typed(typed) if typed.type_filters == vec![TypeFilter::Card]),
        "expected card filter for unqualified cards, got {filter:?}"
    );
    // CR 113.6 / CR 113.6b: Chalk Outline's trigger references other cards leaving
    // its owner's graveyard, not itself — battlefield-only (make_base() default).
    assert_eq!(def.trigger_zones, vec![Zone::Battlefield]);
}

#[test]
fn trigger_one_or_more_cards_leave_graveyard_during_your_turn() {
    let def = parse_trigger_line(
        "Whenever one or more cards leave your graveyard during your turn, you gain 1 life.",
        "Soul Enervation",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZoneAll);
    assert_eq!(def.origin, Some(Zone::Graveyard));
    assert!(def.batched);
    assert_owned_by_you(def.valid_card.as_ref().expect("valid_card"));
    assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
    // CR 113.6 / CR 113.6b: Soul Enervation is a battlefield permanent; its
    // non-self "leave your graveyard" trigger stays battlefield-only.
    assert_eq!(def.trigger_zones, vec![Zone::Battlefield]);
}

#[test]
fn trigger_one_or_more_cards_put_into_exile_from_library_or_graveyard() {
    // CR 603.2c + CR 603.10a: Laelia, the Blade Reforged — batched
    // zone-change trigger with disjunctive source zones.
    let def = parse_trigger_line(
            "Whenever one or more cards are put into exile from your library and/or your graveyard, put a +1/+1 counter on Laelia.",
            "Laelia, the Blade Reforged",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZoneAll);
    assert_eq!(def.destination, Some(Zone::Exile));
    assert_eq!(def.origin_zones, vec![Zone::Library, Zone::Graveyard]);
    assert!(def.batched);
    // CR 113.6 / CR 113.6b: Laelia is a battlefield permanent whose "cards are put
    // into exile from library/graveyard" trigger has no self-referential subject —
    // it functions only from the battlefield (make_base() default).
    assert_eq!(def.trigger_zones, vec![Zone::Battlefield]);
}

#[test]
fn trigger_one_or_more_cards_put_into_exile_from_library_only() {
    // Single-zone source variant should still parse.
    let def = parse_trigger_line(
            "Whenever one or more cards are put into exile from your library, put a +1/+1 counter on this creature.",
            "Test Card",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZoneAll);
    assert_eq!(def.destination, Some(Zone::Exile));
    assert_eq!(def.origin_zones, vec![Zone::Library]);
    assert!(def.batched);
    // CR 113.6 / CR 113.6b: battlefield-only for the single-source variant too.
    assert_eq!(def.trigger_zones, vec![Zone::Battlefield]);
}

#[test]
fn trigger_one_or_more_artifact_or_creature_cards_leave_graveyard() {
    let def = parse_trigger_line(
            "Whenever one or more artifact and/or creature cards leave your graveyard, put a +1/+1 counter on this creature.",
            "Attuned Hunter",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZoneAll);
    assert_eq!(def.origin, Some(Zone::Graveyard));
    assert!(def.batched);
    let filter = def.valid_card.as_ref().expect("valid_card");
    assert!(matches!(filter, TargetFilter::Or { .. }));
    assert_owned_by_you(filter);
    // CR 113.6 / CR 113.6b: Attuned Hunter's disjunctive "leave your graveyard"
    // trigger references other cards, not itself — battlefield-only.
    assert_eq!(def.trigger_zones, vec![Zone::Battlefield]);
}

// ── Work Item 2: Discard Batch Triggers ───────────────────────

#[test]
fn trigger_you_discard_one_or_more_cards() {
    let def = parse_trigger_line(
        "Whenever you discard one or more cards, this creature gets +1/+0 until end of turn.",
        "Magmakin Artillerist",
    );
    assert_eq!(def.mode, TriggerMode::DiscardedAll);
    assert!(def.batched);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn trigger_one_or_more_players_discard() {
    let def = parse_trigger_line(
            "Whenever one or more players discard one or more cards, put a +1/+1 counter on this creature.",
            "Waste Not",
        );
    assert_eq!(def.mode, TriggerMode::DiscardedAll);
    assert!(def.batched);
    assert_eq!(def.valid_target, None); // any player
}

// ── Work Item 3: Noncombat Damage to Opponent ─────────────────

#[test]
fn trigger_noncombat_damage_to_opponent() {
    let def = parse_trigger_line(
            "Whenever a source you control deals noncombat damage to an opponent, create a 1/1 red Elemental creature token.",
            "Virtue of Courage",
        );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(def.damage_kind, DamageKindFilter::NoncombatOnly);
    assert!(matches!(
        def.valid_source,
        Some(TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::You),
            ..
        }))
    ));
    assert!(matches!(
        def.valid_target,
        Some(TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::Opponent),
            ..
        }))
    ));
    // No amount threshold on Virtue of Courage's trigger.
    assert_eq!(def.damage_amount, None);
}

#[test]
fn trigger_source_you_control_deals_damage_to_another_player() {
    let def = parse_trigger_line(
            "Whenever a source you control deals damage to another player, put that many theft counters on ~.",
            "Night Dealings",
        );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(def.damage_kind, DamageKindFilter::Any);
    assert_eq!(def.damage_amount, None);
    assert!(matches!(
        def.valid_source,
        Some(TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::You),
            ..
        }))
    ));
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        ))
    );
}

// CR 603.2 + CR 120.1: "Whenever a source you control deals N or more
// damage to <recipient>" — exercises the amount-threshold axis added for
// Dragonborn Champion. Building-block test: it verifies the parser emits
// `damage_amount = Some((GE, N))` together with the source/recipient
// filters, regardless of the specific card.
#[test]
fn trigger_source_deals_n_or_more_damage_to_player() {
    let def = parse_trigger_line(
        "Whenever a source you control deals 5 or more damage to a player, draw a card.",
        "Dragonborn Champion",
    );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(def.damage_kind, DamageKindFilter::Any);
    assert_eq!(def.damage_amount, Some((Comparator::GE, 5)));
    assert!(matches!(
        def.valid_source,
        Some(TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::You),
            ..
        }))
    ));
    assert_eq!(def.valid_target, Some(TargetFilter::Player));
}

#[test]
fn trigger_source_deals_n_or_more_damage_without_recipient() {
    let def = parse_trigger_line(
        "Whenever a source you control deals 5 or more damage, draw a card.",
        "Test",
    );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(def.damage_amount, Some((Comparator::GE, 5)));
    assert!(matches!(
        def.valid_source,
        Some(TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::You),
            ..
        }))
    ));
    assert_eq!(def.valid_target, None);
}

#[test]
fn trigger_creature_source_deals_n_or_more_damage_to_player() {
    let def = parse_trigger_line(
        "Whenever a creature you control deals 5 or more damage to a player, draw a card.",
        "Test",
    );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(def.damage_amount, Some((Comparator::GE, 5)));
    match def.valid_source {
        Some(TargetFilter::Typed(TypedFilter {
            type_filters,
            controller: Some(ControllerRef::You),
            ..
        })) => assert_eq!(type_filters, vec![TypeFilter::Creature]),
        other => panic!("expected controlled creature source filter, got {other:?}"),
    }
    assert_eq!(def.valid_target, Some(TargetFilter::Player));
}

#[test]
fn trigger_source_deals_exactly_n_damage_to_player() {
    let def = parse_trigger_line(
        "Whenever a source you control deals exactly 5 damage to a player, draw a card.",
        "Test",
    );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(def.damage_amount, Some((Comparator::EQ, 5)));
    assert_eq!(def.valid_target, Some(TargetFilter::Player));
}

#[test]
fn ghyrson_damage_trigger_parses_mixed_permanent_or_player_recipient() {
    let parsed = parse_oracle_text(
        "Ward {2}\nWhenever another source you control deals exactly 1 damage to a permanent or player, Ghyrson Starn, Kelermorph deals 2 damage to that permanent or player.",
        "Ghyrson Starn, Kelermorph",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &[],
    );
    assert!(
        parsed
            .abilities
            .iter()
            .all(|ability| !matches!(ability.effect.as_ref(), Effect::Unimplemented { .. })),
        "Ghyrson must parse without unimplemented abilities: {:?}",
        parsed.abilities
    );
    let trigger = parsed.triggers.first().expect("Ghyrson trigger parses");
    assert_eq!(trigger.mode, TriggerMode::DamageDone);
    assert_eq!(trigger.damage_amount, Some((Comparator::EQ, 1)));
    assert_eq!(trigger.valid_target, None);
    match trigger.valid_source.as_ref() {
        Some(TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::You),
            properties,
            ..
        })) => assert!(
            properties
                .iter()
                .any(|prop| matches!(prop, FilterProp::Another)),
            "source filter must require another controlled source: {properties:?}"
        ),
        other => panic!("expected another source you control filter, got {other:?}"),
    }
    let execute = trigger.execute.as_ref().expect("trigger has an effect");
    assert!(
        matches!(
            execute.effect.as_ref(),
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::EventTarget,
                damage_source: None,
                ..
            }
        ),
        "Ghyrson effect must damage the event target, got {:?}",
        execute.effect
    );
}

#[test]
fn damage_trigger_mixed_permanent_or_player_requires_exact_qualifier() {
    let def = parse_trigger_line(
        "Whenever another source you control deals exactly 1 damage to a permanent or player this turn, draw a card.",
        "Test",
    );
    assert_ne!(
        def.mode,
        TriggerMode::DamageDone,
        "mixed permanent/player recipient must not accept trailing qualifier text"
    );
}

#[test]
fn damage_trigger_creature_or_player_is_not_promoted_to_mixed_recipient() {
    let def = parse_trigger_line(
        "Whenever another source you control deals exactly 1 damage to a creature or player, draw a card.",
        "Test",
    );
    assert_ne!(def.mode, TriggerMode::DamageDone);
}

// Same general parser must also accept the no-threshold + noncombat-kind
// form (Virtue of Courage style) — proves the threshold axis is optional
// and composes orthogonally with the damage-kind axis.
#[test]
fn trigger_source_deals_noncombat_damage_to_player_no_threshold() {
    let def = parse_trigger_line(
        "Whenever a source you control deals noncombat damage to a player, draw a card.",
        "Test",
    );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(def.damage_kind, DamageKindFilter::NoncombatOnly);
    assert_eq!(def.damage_amount, None);
    assert_eq!(def.valid_target, Some(TargetFilter::Player));
}

#[test]
fn trigger_source_opponent_controls_deals_damage_to_you() {
    let def = parse_trigger_line(
            "Whenever a source an opponent controls deals damage to you, you may put that many +1/+1 counters on ~.",
            "Retaliator Griffin",
        );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(def.damage_kind, DamageKindFilter::Any);
    assert_eq!(def.damage_amount, None);
    assert_eq!(
        def.valid_source,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        ))
    );
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn trigger_noncreature_source_you_control_deals_damage() {
    let def = parse_trigger_line(
        "Whenever a noncreature source you control deals damage, you gain that much life.",
        "Tamanoa",
    );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(def.damage_kind, DamageKindFilter::Any);
    assert_eq!(def.damage_amount, None);
    assert_eq!(
        def.valid_source,
        Some(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Non(Box::new(TypeFilter::Creature)))
                .controller(ControllerRef::You)
        ))
    );
    assert_eq!(def.valid_target, None);
}

#[test]
fn trigger_source_opponent_controls_deals_damage_to_you_michiko() {
    let def = parse_trigger_line(
            "Whenever a source an opponent controls deals damage to you, that player sacrifices a permanent.",
            "Michiko Konda, Truth Seeker",
        );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(
        def.valid_source,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        ))
    );
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    let execute = def.execute.as_ref().expect("trigger execute");
    match execute.effect.as_ref() {
        Effect::Sacrifice { target, .. } => match target {
            TargetFilter::Typed(TypedFilter {
                type_filters,
                controller: Some(ControllerRef::ParentTargetController),
                ..
            }) => assert_eq!(type_filters.as_slice(), [TypeFilter::Permanent]),
            other => panic!("expected source-controller sacrifice filter, got {other:?}"),
        },
        other => panic!("expected Sacrifice, got {other:?}"),
    }
}

#[test]
fn trigger_noncreature_source_deals_damage_to_player() {
    let def = parse_trigger_line(
        "Whenever a noncreature source deals damage to a player, draw a card.",
        "Test",
    );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(
        def.valid_source,
        Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Non(
            Box::new(TypeFilter::Creature)
        ))))
    );
    assert_eq!(def.valid_target, Some(TargetFilter::Player));
}
// ── Work Item 4: Transforms Into Self ─────────────────────────

#[test]
fn trigger_transforms_into_self() {
    let def = parse_trigger_line(
        "When this creature transforms into Trystan, Penitent Culler, you gain 3 life.",
        "Trystan, Penitent Culler",
    );
    assert_eq!(def.mode, TriggerMode::Transformed);
    assert_eq!(def.valid_source, Some(TargetFilter::SelfRef));
}

// ── Work Item 5: Tap Opponent's Creature ──────────────────────

#[test]
fn trigger_you_tap_opponent_creature() {
    let def = parse_trigger_line(
        "Whenever you tap an untapped creature an opponent controls, you gain 1 life.",
        "Hylda of the Icy Crown",
    );
    assert_eq!(def.mode, TriggerMode::Taps);
    assert!(matches!(
        def.valid_card,
        Some(TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::Opponent),
            ..
        }))
    ));
}

// ── Work Item 6: Expend Triggers ──────────────────────────────

#[test]
fn trigger_expend_4() {
    let def = parse_trigger_line(
        "Whenever you expend 4, put a +1/+1 counter on this creature.",
        "Roughshod Duo",
    );
    assert_eq!(def.mode, TriggerMode::ManaExpend);
    assert_eq!(def.expend_threshold, Some(4));
}

#[test]
fn trigger_expend_8() {
    let def = parse_trigger_line("Whenever you expend 8, draw a card.", "Wandertale Mentor");
    assert_eq!(def.mode, TriggerMode::ManaExpend);
    assert_eq!(def.expend_threshold, Some(8));
}

#[test]
fn trigger_plural_deal_combat_damage() {
    // CR 120.1: Plural "deal" for &-names after ~ normalization
    let def = parse_trigger_line(
            "Whenever Dark Leo & Shredder deal combat damage to a player, create a 1/1 black Ninja creature token.",
            "Dark Leo & Shredder",
        );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
}

#[test]
fn trigger_singular_deals_combat_damage_regression() {
    // Ensure singular "deals" still works
    let def = parse_trigger_line(
        "Whenever Ninja of the Deep Hours deals combat damage to a player, you may draw a card.",
        "Ninja of the Deep Hours",
    );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    assert_eq!(def.valid_target, Some(TargetFilter::Player));
}

#[test]
fn trigger_one_or_more_ninja_or_rogue_combat_damage() {
    // CR 205.3m + CR 603.2c: Compound subtype in "one or more" batched damage trigger
    let result = try_parse_one_or_more_combat_damage_to_player(
        "whenever one or more ninja or rogue creatures you control deal combat damage to a player",
    );
    assert!(
        result.is_some(),
        "should parse one-or-more compound trigger"
    );
    let (mode, def) = result.unwrap();
    assert_eq!(mode, TriggerMode::DamageDoneOnceByController);
    assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    assert!(matches!(&def.valid_source, Some(TargetFilter::Or { filters }) if filters.len() == 2));
}

#[test]
fn trigger_one_or_more_comma_and_or_subtypes_combat_damage() {
    let def = parse_trigger_line(
            "Whenever one or more Mutants, Ninjas, and/or Turtles you control deal combat damage to a player, put a +1/+1 counter on each of those creatures and draw a card.",
            "Heroes in a Half Shell",
        );

    assert_eq!(def.mode, TriggerMode::DamageDoneOnceByController);
    assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    assert_eq!(def.valid_target, Some(TargetFilter::Player));
    assert!(matches!(
        &def.valid_source,
        Some(TargetFilter::Or { filters }) if filters.len() == 3
    ));

    let execute = def.execute.as_ref().expect("trigger should have execute");
    assert!(matches!(
        *execute.effect,
        Effect::PutCounterAll {
            target: TargetFilter::TrackedSet { .. },
            ..
        }
    ));
    let sub = execute
        .sub_ability
        .as_ref()
        .expect("draw should be chained");
    assert!(matches!(*sub.effect, Effect::Draw { .. }));
}

#[test]
fn trigger_etb_from_hand_if_attacking() {
    // Thousand-Faced Shadow: "When this creature enters from your hand, if it's attacking, ..."
    let def = parse_trigger_line(
            "When this creature enters from your hand, if it's attacking, create a token that's a copy of another target attacking creature. The token enters tapped and attacking.",
            "Thousand-Faced Shadow",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    assert_eq!(def.origin, Some(Zone::Hand));
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert_eq!(def.condition, Some(TriggerCondition::SourceIsAttacking));
    // Effect should be CopyTokenOf
    assert!(def.execute.is_some());
    let exec = def.execute.as_ref().unwrap();
    assert!(matches!(*exec.effect, Effect::CopyTokenOf { .. }));
}

/// CR 603.6 + CR 603.6a — Flayer of the Hatebound: "enters from your
/// graveyard" must set `origin = Some(Graveyard)` on the ChangesZone
/// trigger. Issue #396 — previously the origin-zone qualifier was
/// dropped, so the trigger fired on any creature entering from any zone
/// (including commanders cast from the command zone).
#[test]
fn trigger_etb_from_graveyard_flayer() {
    let def = parse_trigger_line(
            "Whenever this creature or another creature enters from your graveyard, that creature deals damage equal to its power to any target.",
            "Flayer of the Hatebound",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    assert_eq!(def.origin, Some(Zone::Graveyard));
    let execute = def.execute.as_ref().expect("trigger should have execute");
    match &*execute.effect {
        Effect::DealDamage {
            amount,
            target,
            damage_source,
            excess: _,
        } => {
            assert_eq!(*target, TargetFilter::Any);
            assert_eq!(*damage_source, Some(DamageSource::TriggeringSource));
            assert!(matches!(
                amount,
                QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: ObjectScope::EventSource,
                    },
                }
            ));
        }
        other => panic!("expected DealDamage, got {other:?}"),
    }
}

/// CR 120.1 + CR 603.6: Pyrogoyf's "that creature deals damage equal to
/// its power" trigger must use the entering Lhurgoyf as both the damage
/// source and power source, while keeping "any target" as the chosen damage
/// recipient.
#[test]
fn pyrogoyf_etb_damage_uses_entering_lhurgoyf_as_damage_source() {
    let def = parse_trigger_line(
            "Whenever this creature or another Lhurgoyf creature you control enters, that creature deals damage equal to its power to any target.",
            "Pyrogoyf",
        );

    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    let execute = def.execute.as_ref().expect("trigger should have execute");
    match &*execute.effect {
        Effect::DealDamage {
            amount,
            target,
            damage_source,
            excess: _,
        } => {
            assert_eq!(*target, TargetFilter::Any);
            assert_eq!(*damage_source, Some(DamageSource::TriggeringSource));
            assert!(matches!(
                amount,
                QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: ObjectScope::EventSource,
                    },
                }
            ));
        }
        other => panic!("expected DealDamage, got {other:?}"),
    }
}

/// CR 603.6 + CR 603.6a — origin extraction for "enters from exile"
/// triggers (e.g. cards that fire when a creature comes back from exile).
#[test]
fn trigger_etb_from_exile_origin() {
    let def = parse_trigger_line(
        "Whenever another creature enters from exile, draw a card.",
        "Test Source",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    assert_eq!(def.origin, Some(Zone::Exile));
}

/// CR 508.4 + CR 614.1 — Kaalia of the Vast: the inline-tail patcher in
/// `try_parse_put_zone_change` must lift "tapped and attacking that
/// opponent" onto the produced `Effect::ChangeZone`, setting both
/// `enter_tapped` and `enters_attacking`.
#[test]
fn trigger_attacks_inline_tail_kaalia_tapped_and_attacking() {
    let def = parse_trigger_line(
            "Whenever Kaalia attacks an opponent, you may put an Angel, Demon, or Dragon creature card from your hand onto the battlefield tapped and attacking that opponent.",
            "Kaalia of the Vast",
        );
    assert_eq!(def.mode, TriggerMode::Attacks);
    let exec = def.execute.as_ref().expect("expected execute");
    match &*exec.effect {
        Effect::ChangeZone {
            destination,
            enter_tapped,
            enters_attacking,
            ..
        } => {
            assert_eq!(*destination, Zone::Battlefield);
            assert!(
                enter_tapped.is_tapped(),
                "expected enter_tapped.is_tapped()"
            );
            assert!(*enters_attacking, "expected enters_attacking");
        }
        other => panic!("expected ChangeZone, got {other:?}"),
    }
}

/// CR 508.4 — Ilharg / Preeminent Captain bare form: tail without a
/// trailing player phrase. Both flags must still be set.
#[test]
fn trigger_attacks_inline_tail_ilharg_bare_tapped_and_attacking() {
    let def = parse_trigger_line(
            "Whenever Ilharg attacks, you may put a creature card from your hand onto the battlefield tapped and attacking.",
            "Ilharg, the Raze-Boar",
        );
    assert_eq!(def.mode, TriggerMode::Attacks);
    let exec = def.execute.as_ref().expect("expected execute");
    match &*exec.effect {
        Effect::ChangeZone {
            destination,
            enter_tapped,
            enters_attacking,
            ..
        } => {
            assert_eq!(*destination, Zone::Battlefield);
            assert!(enter_tapped.is_tapped());
            assert!(*enters_attacking);
        }
        other => panic!("expected ChangeZone, got {other:?}"),
    }
}

/// CR 508.4 — Negative regression for the existing separate-sentence
/// patcher (`ContinuationAst::EntersTappedAttacking`). Stangg / Shark
/// Shredder / Thousand-Faced Shadow style "It enters tapped and attacking"
/// in a follow-on sentence must continue to set both flags on the prior
/// effect — the inline-tail patcher must not interfere.
#[test]
fn trigger_separate_sentence_patcher_still_sets_both_flags() {
    // Synthetic Stangg-style: a token effect followed by a separate
    // "It enters tapped and attacking" sentence patcher.
    let def = parse_trigger_line(
            "When this creature enters, create a 3/3 red Cat creature token. It enters tapped and attacking.",
            "Stangg-Style Test",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    let exec = def.execute.as_ref().expect("expected execute");
    match &*exec.effect {
        Effect::Token {
            tapped,
            enters_attacking,
            ..
        } => {
            assert!(*tapped, "separate-sentence patcher must set tapped");
            assert!(
                *enters_attacking,
                "separate-sentence patcher must set enters_attacking"
            );
        }
        other => panic!("expected Token, got {other:?}"),
    }
}

#[test]
fn cast_variant_paid_sneak_condition() {
    // CR 702.190a: "if its sneak cost was paid" → CastVariantPaid { variant: Sneak }
    let def = parse_trigger_line(
        "When this creature enters, if its sneak cost was paid, draw a card.",
        "Test Ninja",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    assert_eq!(
        def.condition,
        Some(TriggerCondition::CastVariantPaid {
            variant: CastVariantPaid::Sneak,
        })
    );
}

#[test]
fn cast_variant_paid_ninjutsu_condition() {
    // CR 702.49: "if its ninjutsu cost was paid" → CastVariantPaid { variant: Ninjutsu }
    let def = parse_trigger_line(
            "When this creature enters, if its ninjutsu cost was paid, target opponent discards a card.",
            "Test Ninja",
        );
    assert_eq!(
        def.condition,
        Some(TriggerCondition::CastVariantPaid {
            variant: CastVariantPaid::Ninjutsu,
        })
    );
}

#[test]
fn cast_variant_paid_surge_condition() {
    // CR 702.117a + CR 603.4: "if its surge cost was paid" intervening-if
    // (Reckless Bushwhacker class) → CastVariantPaid { variant: Surge }.
    let def = parse_trigger_line(
            "When this creature enters, if its surge cost was paid, creatures you control get +1/+1 until end of turn.",
            "Test Surge",
        );
    assert_eq!(
        def.condition,
        Some(TriggerCondition::CastVariantPaid {
            variant: CastVariantPaid::Surge,
        })
    );
}

#[test]
fn cast_variant_paid_spectacle_condition() {
    // CR 702.137a + CR 603.4: "if its spectacle cost was paid" intervening-if
    // (Rafter Demon) → CastVariantPaid { variant: Spectacle }.
    let def = parse_trigger_line(
        "When this creature enters, if its spectacle cost was paid, each opponent discards a card.",
        "Test Spectacle",
    );
    assert_eq!(
        def.condition,
        Some(TriggerCondition::CastVariantPaid {
            variant: CastVariantPaid::Spectacle,
        })
    );
}

#[test]
fn cast_variant_paid_prowl_condition() {
    // CR 702.76a + CR 603.4: "if its prowl cost was paid" intervening-if
    // (Latchkey Faerie) → CastVariantPaid { variant: Prowl }.
    let def = parse_trigger_line(
        "When this creature enters, if its prowl cost was paid, draw a card.",
        "Test Prowl",
    );
    assert_eq!(
        def.condition,
        Some(TriggerCondition::CastVariantPaid {
            variant: CastVariantPaid::Prowl,
        })
    );
}

// CR 702.138c + CR 603.11: Pharika's Spawn — the linked triggered ability of
// an "[this permanent] escapes with [counters]" replacement effect. "When it
// enters this way" must (a) resolve the pronoun "it" to SelfRef, (b) lower to
// an ETB ChangesZone→Battlefield trigger, and (c) attach a
// CastVariantPaid { Escape } intervening-if so the linked effect fires only
// when the permanent escaped.
#[test]
fn pharikas_spawn_enters_this_way_gated_on_escape() {
    let def = parse_trigger_line(
        "When it enters this way, each opponent sacrifices a non-Gorgon creature of their choice.",
        "Pharika's Spawn",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert_eq!(
        def.condition,
        Some(TriggerCondition::CastVariantPaid {
            variant: CastVariantPaid::Escape,
        })
    );
}

// CR 702.138b + CR 603.4: Phlage, Titan of Fire's Fury — "sacrifice it
// unless it escaped" must (a) resolve "it" to SelfRef via pronoun context,
// not ParentTarget, and (b) attach a negated CastVariantPaid { Escape }
// intervening-if so the sacrifice fires on hard-casts and reanimation but
// not on escape casts.
#[test]
fn phlage_unless_it_escaped_attaches_negated_escape_condition() {
    let def = parse_trigger_line(
        "When ~ enters, sacrifice it unless it escaped.",
        "Phlage, Titan of Fire's Fury",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert_eq!(
        def.condition,
        Some(TriggerCondition::Not {
            condition: Box::new(TriggerCondition::CastVariantPaid {
                variant: CastVariantPaid::Escape,
            }),
        })
    );
    let execute = def.execute.as_deref().expect("execute ability");
    match &*execute.effect {
        Effect::Sacrifice { target, .. } => {
            assert_eq!(
                *target,
                TargetFilter::SelfRef,
                "`sacrifice it` in an ETB-self trigger should resolve to SelfRef"
            );
        }
        other => panic!("expected Sacrifice, got {other:?}"),
    }
}

#[test]
fn grave_pact_scopes_sacrifice_to_other_players() {
    let def = parse_trigger_line(
            "Whenever a creature you control dies, each other player sacrifices a creature of their choice.",
            "Grave Pact",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.origin, Some(Zone::Battlefield));
    assert_eq!(def.destination, Some(Zone::Graveyard));
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You)
        ))
    );

    let execute = def.execute.as_deref().expect("execute ability");
    match &*execute.effect {
        Effect::Sacrifice { target, count, .. } => {
            assert_eq!(*count, QuantityExpr::Fixed { value: 1 });
            assert_eq!(
                *target,
                TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
            );
        }
        other => panic!("expected Sacrifice, got {other:?}"),
    }
}

#[test]
fn portal_to_phyrexia_etb_scopes_each_opponent_sacrifice_to_scoped_player() {
    let def = parse_trigger_line(
        "When this artifact enters, each opponent sacrifices three creatures of their choice.",
        "Portal to Phyrexia",
    );
    let execute = def.execute.as_deref().expect("execute ability");
    assert_eq!(execute.player_scope, Some(PlayerFilter::Opponent));
    match &*execute.effect {
        Effect::Sacrifice { target, count, .. } => {
            assert_eq!(*count, QuantityExpr::Fixed { value: 3 });
            assert_eq!(
                *target,
                TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
            );
        }
        other => panic!("expected Sacrifice, got {other:?}"),
    }
}

#[test]
fn balefire_dragon_damages_creatures_controlled_by_damaged_player() {
    let def = parse_trigger_line(
            "Whenever this creature deals combat damage to a player, it deals that much damage to each creature controlled by that player.",
            "Balefire Dragon",
        );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    assert_eq!(def.valid_target, Some(TargetFilter::Player));

    let execute = def.execute.as_deref().expect("execute ability");
    match &*execute.effect {
        Effect::DamageAll { amount, target, .. } => {
            assert_eq!(
                *amount,
                QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                }
            );
            assert_eq!(
                *target,
                TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::TargetPlayer)
                )
            );
        }
        other => panic!("expected DamageAll, got {other:?}"),
    }
}

#[test]
fn ninjutsu_activation_trigger() {
    // CR 702.49a: "Whenever you activate a ninjutsu ability" → NinjutsuActivated
    let def = parse_trigger_line(
        "Whenever you activate a ninjutsu ability, look at the top three cards of your library.",
        "Satoru Umezawa",
    );
    assert_eq!(def.mode, TriggerMode::NinjutsuActivated);
}

#[test]
fn ninjutsu_activation_trigger_with_once_per_turn() {
    // CR 702.49a: Ninjutsu activation with once-per-turn constraint
    let triggers = parse_trigger_lines(
            "Whenever you activate a ninjutsu ability, look at the top three cards of your library. Put one of them into your hand and the rest on the bottom of your library in any order. This ability triggers only once each turn.",
            "Satoru Umezawa",
        );
    assert_eq!(triggers.len(), 1);
    assert_eq!(triggers[0].mode, TriggerMode::NinjutsuActivated);
    assert_eq!(triggers[0].constraint, Some(TriggerConstraint::OncePerTurn));
}

#[test]
fn exhaust_activation_trigger() {
    let def = parse_trigger_line(
        "Whenever you activate an exhaust ability, draw a card.",
        "Rangers' Aetherhive",
    );
    assert_eq!(
        def.mode,
        TriggerMode::KeywordAbilityActivated(AbilityTag::Exhaust)
    );
    assert_eq!(def.condition, None);
}

#[test]
fn exhaust_activation_trigger_non_mana_qualifier() {
    let def = parse_trigger_line(
        "Whenever you activate an exhaust ability that isn't a mana ability, draw a card.",
        "Sala, Deck Boss",
    );
    assert_eq!(
        def.mode,
        TriggerMode::KeywordAbilityActivated(AbilityTag::Exhaust)
    );
    assert_eq!(
        def.condition,
        Some(TriggerCondition::ActivatedAbilityIsNonMana)
    );
}

#[test]
fn outlast_activation_trigger_self_ref_scopes_valid_card() {
    let def = parse_trigger_line(
            "Whenever you activate this creature's outlast ability, create a 1/1 white Warrior creature token.",
            "Herald of Anafenza",
        );
    assert_eq!(
        def.mode,
        TriggerMode::KeywordAbilityActivated(AbilityTag::Outlast)
    );
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

#[test]
fn outlast_activation_trigger_subject_siblings() {
    let cases = [
        (
            "Whenever you activate an outlast ability, draw a card.",
            None,
        ),
        (
            "Whenever you activate your outlast ability, draw a card.",
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
        ),
        (
            "Whenever you activate an opponent's outlast ability, draw a card.",
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            )),
        ),
    ];

    for (text, valid_card) in cases {
        let def = parse_trigger_line(text, "Outlast Test");
        assert_eq!(
            def.mode,
            TriggerMode::KeywordAbilityActivated(AbilityTag::Outlast),
            "{text}"
        );
        assert_eq!(def.valid_card, valid_card, "{text}");
    }
}

// --- CR 602.1 + CR 605.1a: generic non-mana ability activation trigger ---

#[test]
fn ability_activation_trigger_a_player() {
    // Burning-Tree Shaman: "Whenever a player activates an ability that
    // isn't a mana ability, this creature deals 1 damage to that player."
    let def = parse_trigger_line(
            "Whenever a player activates an ability that isn't a mana ability, this creature deals 1 damage to that player.",
            "Burning-Tree Shaman",
        );
    assert_eq!(def.mode, TriggerMode::AbilityActivated);
    // "a player" — every player matches, so no valid_target filter.
    assert_eq!(def.valid_target, None);
    assert_eq!(
        def.condition,
        Some(TriggerCondition::ActivatedAbilityIsNonMana)
    );
}

#[test]
fn ability_activation_trigger_an_opponent() {
    // Flamescroll Celebrant: "Whenever an opponent activates an ability
    // that isn't a mana ability, this creature deals 1 damage to that
    // player."
    let def = parse_trigger_line(
            "Whenever an opponent activates an ability that isn't a mana ability, this creature deals 1 damage to that player.",
            "Flamescroll Celebrant",
        );
    assert_eq!(def.mode, TriggerMode::AbilityActivated);
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent),
        ))
    );
    assert_eq!(
        def.condition,
        Some(TriggerCondition::ActivatedAbilityIsNonMana)
    );
}

#[test]
fn ability_activation_trigger_you_form() {
    // Symmetric "you activate" form — verb conjugates to second-person.
    let def = parse_trigger_line(
        "Whenever you activate an ability that isn't a mana ability, draw a card.",
        "Hypothetical You-Activate",
    );
    assert_eq!(def.mode, TriggerMode::AbilityActivated);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn ability_activation_trigger_accepts_activated_modifier() {
    let def = parse_trigger_line(
        "Whenever you activate an activated ability that isn't a mana ability, draw a card.",
        "Hypothetical Activated-Modifier",
    );
    assert_eq!(def.mode, TriggerMode::AbilityActivated);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert_eq!(
        def.condition,
        Some(TriggerCondition::ActivatedAbilityIsNonMana)
    );
}

// --- CR 606.2: "Whenever you activate a loyalty ability of [pw]" ---

/// CR 606.2 + CR 205.3j: Chandra's Regulator — "a Chandra planeswalker"
/// parses to a typed Planeswalker + Subtype("Chandra") filter on
/// `valid_card`, mode `LoyaltyAbilityActivated`, and the effect is not
/// `Unimplemented`.
#[test]
fn loyalty_ability_trigger_chandra_subtype_regulator() {
    let def = parse_trigger_line(
            "Whenever you activate a loyalty ability of a Chandra planeswalker, you may pay {1}. If you do, copy that ability. You may choose new targets for the copy.",
            "Chandra's Regulator",
        );
    assert_eq!(def.mode, TriggerMode::LoyaltyAbilityActivated);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Planeswalker).subtype("Chandra".to_string()),
        ))
    );
    let execute = def.execute.as_ref().expect("execute ability present");
    assert!(
        !matches!(*execute.effect, Effect::Unimplemented { .. }),
        "Chandra's Regulator effect should parse, got {:?}",
        execute.effect
    );
}

/// CR 606.2: Keral Keep Disciples — same "a Chandra planeswalker" filter,
/// different (damage) effect.
#[test]
fn loyalty_ability_trigger_chandra_subtype_keral_keep() {
    let def = parse_trigger_line(
            "Whenever you activate a loyalty ability of a Chandra planeswalker, this creature deals 1 damage to each opponent.",
            "Keral Keep Disciples",
        );
    assert_eq!(def.mode, TriggerMode::LoyaltyAbilityActivated);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Planeswalker).subtype("Chandra".to_string()),
        ))
    );
    let execute = def.execute.as_ref().expect("execute ability present");
    assert!(
        !matches!(*execute.effect, Effect::Unimplemented { .. }),
        "Keral Keep Disciples effect should parse, got {:?}",
        execute.effect
    );
}

/// CR 606.2 + CR 303.4m: Elspeth's Talent — "enchanted planeswalker" parses
/// to `TargetFilter::AttachedTo` (the Aura host) on `valid_card`.
#[test]
fn loyalty_ability_trigger_enchanted_elspeth() {
    let def = parse_trigger_line(
            "Whenever you activate a loyalty ability of enchanted planeswalker, creatures you control get +2/+2 and gain vigilance until end of turn.",
            "Elspeth's Talent",
        );
    assert_eq!(def.mode, TriggerMode::LoyaltyAbilityActivated);
    assert_eq!(def.valid_card, Some(TargetFilter::AttachedTo));
    let execute = def.execute.as_ref().expect("execute ability present");
    assert!(
        !matches!(*execute.effect, Effect::Unimplemented { .. }),
        "Elspeth's Talent effect should parse, got {:?}",
        execute.effect
    );
}

/// CR 606.2 + CR 303.4m: Rowan's Talent — "enchanted planeswalker" + the
/// copy effect.
#[test]
fn loyalty_ability_trigger_enchanted_rowan() {
    let def = parse_trigger_line(
            "Whenever you activate a loyalty ability of enchanted planeswalker, copy that ability. You may choose new targets for the copy.",
            "Rowan's Talent",
        );
    assert_eq!(def.mode, TriggerMode::LoyaltyAbilityActivated);
    assert_eq!(def.valid_card, Some(TargetFilter::AttachedTo));
    let execute = def.execute.as_ref().expect("execute ability present");
    assert!(
        !matches!(*execute.effect, Effect::Unimplemented { .. }),
        "Rowan's Talent effect should parse, got {:?}",
        execute.effect
    );
}

/// Negative: the additive "planeswalker" host noun in `parse_attached_to_subject`
/// must not change the generic non-loyalty activation trigger class.
#[test]
fn loyalty_ability_trigger_does_not_capture_generic_activation() {
    let def = parse_trigger_line(
        "Whenever you activate an ability that isn't a mana ability, draw a card.",
        "Generic Activation",
    );
    assert_eq!(def.mode, TriggerMode::AbilityActivated);
}

// --- CR 115.9c: "that targets only [X]" trigger tests ---

#[test]
fn trigger_zada_targets_only_self() {
    let def = parse_trigger_line(
            "Whenever you cast an instant or sorcery spell that targets only Zada, copy that spell for each other creature you control.",
            "Zada, Hedron Grinder",
        );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    // valid_card should be Or(Instant, Sorcery) with TargetsOnly { SelfRef } on each
    let valid_card = def.valid_card.expect("should have valid_card");
    if let TargetFilter::Or { filters } = &valid_card {
        assert_eq!(filters.len(), 2, "expected 2 branches for instant/sorcery");
        for f in filters {
            if let TargetFilter::Typed(tf) = f {
                assert!(
                        tf.properties.iter().any(|p| matches!(p, FilterProp::TargetsOnly { filter } if **filter == TargetFilter::SelfRef)),
                        "expected TargetsOnly(SelfRef) in {tf:?}"
                    );
            } else {
                panic!("expected Typed filter, got {f:?}");
            }
        }
    } else {
        panic!("expected Or filter, got {valid_card:?}");
    }
}

#[test]
fn trigger_leyline_of_resonance_targets_only_single_creature_you_control() {
    let def = parse_trigger_line(
            "Whenever you cast an instant or sorcery spell that targets only a single creature you control, copy that spell.",
            "Leyline of Resonance",
        );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    let valid_card = def.valid_card.expect("should have valid_card");
    if let TargetFilter::Or { filters } = &valid_card {
        assert_eq!(filters.len(), 2);
        for f in filters {
            if let TargetFilter::Typed(tf) = f {
                assert!(
                    tf.properties
                        .iter()
                        .any(|p| matches!(p, FilterProp::TargetsOnly { .. })),
                    "expected TargetsOnly in {tf:?}"
                );
                assert!(
                    tf.properties
                        .iter()
                        .any(|p| matches!(p, FilterProp::HasSingleTarget)),
                    "expected HasSingleTarget in {tf:?}"
                );
            } else {
                panic!("expected Typed filter, got {f:?}");
            }
        }
    } else {
        panic!("expected Or filter, got {valid_card:?}");
    }
}

#[test]
fn enters_tapped_and_attacking_patches_change_zone() {
    // CR 508.4: Shark Shredder — "put ... onto the battlefield under your control.
    // It enters tapped and attacking that player."
    let def = parse_trigger_line(
            "Whenever Shark Shredder deals combat damage to a player, put up to one target creature card from that player's graveyard onto the battlefield under your control. It enters tapped and attacking that player.",
            "Shark Shredder, Killer Clone",
        );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    let exec = def.execute.as_ref().unwrap();
    // The primary effect should be ChangeZone with enter_tapped + enters_attacking.
    match &*exec.effect {
        Effect::ChangeZone {
            destination: Zone::Battlefield,
            enters_under: Some(ControllerRef::You),
            enter_tapped: crate::types::zones::EtbTapState::Tapped,
            enters_attacking: true,
            ..
        } => {} // expected
        other => panic!(
            "expected ChangeZone with enter_tapped + enters_attacking, got {:?}",
            other
        ),
    }
    // The sub_ability should NOT be Unimplemented.
    if let Some(sub) = &exec.sub_ability {
        assert!(
            !matches!(*sub.effect, Effect::Unimplemented { .. }),
            "sub_ability should not be Unimplemented, got {:?}",
            sub.effect,
        );
    }
}

#[test]
fn enters_tapped_and_attacking_patches_token() {
    // CR 508.4: Stangg — "create ... token. It enters tapped and attacking."
    let def = parse_trigger_line(
            "Whenever Stangg attacks, create Stangg Twin, a legendary 3/4 red and green Human Warrior creature token. It enters tapped and attacking.",
            "Stangg, Echo Warrior",
        );
    assert_eq!(def.mode, TriggerMode::Attacks);
    let exec = def.execute.as_ref().unwrap();
    match &*exec.effect {
        Effect::Token {
            tapped: true,
            enters_attacking: true,
            ..
        } => {} // expected
        other => panic!(
            "expected Token with tapped + enters_attacking, got {:?}",
            other
        ),
    }
}

// -----------------------------------------------------------------------
// ChangesZone "put into graveyard" sub-pattern tests (Phase 35-01)
// -----------------------------------------------------------------------

#[test]
fn trigger_put_into_graveyard_from_battlefield() {
    // CR 700.4: "is put into a graveyard from the battlefield" == "dies"
    let def = parse_trigger_line(
        "Whenever a creature is put into a graveyard from the battlefield, draw a card.",
        "Some Card",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.origin, Some(Zone::Battlefield));
    assert_eq!(def.destination, Some(Zone::Graveyard));
    assert!(def.valid_card.is_some());
    assert!(def.execute.is_some());
}

#[test]
fn trigger_creature_card_put_into_graveyard_from_anywhere() {
    // "from anywhere" means no origin restriction (typed subject)
    let def = parse_trigger_line(
        "Whenever a creature card is put into a graveyard from anywhere, draw a card.",
        "Some Card",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.origin, None);
    assert_eq!(def.destination, Some(Zone::Graveyard));
    assert!(def.valid_card.is_some());
}

#[test]
fn trigger_put_into_opponents_graveyard() {
    let def = parse_trigger_line(
        "Whenever a card is put into an opponent's graveyard from anywhere, draw a card.",
        "Some Card",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.origin, None);
    assert_eq!(def.destination, Some(Zone::Graveyard));
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        ))
    );
}

// -----------------------------------------------------------------------
// Phase trigger variant tests (35-02)
// -----------------------------------------------------------------------

#[test]
fn trigger_end_of_combat_your_turn() {
    // CR 511.2: "At end of combat on your turn" restricts to controller's turn.
    let def = parse_trigger_line(
            "At end of combat on your turn, exile target creature you control, then return it to the battlefield under your control.",
            "Thassa, Deep-Dwelling",
        );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::EndCombat));
    assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
}

#[test]
fn trigger_the_end_of_combat_your_turn() {
    // CR 511.2: Alternate phrasing "at the end of combat on your turn".
    let def = parse_trigger_line(
            "At the end of combat on your turn, put a +1/+1 counter on each creature that attacked this turn.",
            "Test Card",
        );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::EndCombat));
    assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
}

#[test]
fn trigger_end_of_combat_no_constraint() {
    // CR 511.2: Bare "at end of combat" with no turn qualifier has no constraint.
    let def = parse_trigger_line(
        "At end of combat, sacrifice this creature.",
        "Ball Lightning",
    );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::EndCombat));
    assert_eq!(def.constraint, None);
}

#[test]
fn trigger_each_end_step() {
    // CR 513.1: "each end step" fires every turn with no controller constraint.
    let def = parse_trigger_line(
        "At the beginning of each end step, each player draws a card.",
        "Howling Mine",
    );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::End));
    assert_eq!(def.constraint, None);
}

#[test]
fn trigger_the_end_step() {
    // CR 513.1: "the end step" with no possessive — fires each turn.
    let def = parse_trigger_line(
        "At the beginning of the end step, sacrifice this creature.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::End));
    assert_eq!(def.constraint, None);
}

#[test]
fn trigger_each_upkeep() {
    // CR 503.1a: "each upkeep" fires every turn with no controller constraint.
    let def = parse_trigger_line(
        "At the beginning of each upkeep, each player loses 1 life.",
        "Sulfuric Vortex",
    );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::Upkeep));
    assert_eq!(def.constraint, None);
}

#[test]
fn trigger_phase_with_if_condition() {
    // Intervening-if condition is extracted by extract_if_condition upstream.
    let def = parse_trigger_line(
        "At the beginning of your end step, if you gained life this turn, draw a card.",
        "Dawn of Hope",
    );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::End));
    assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
    assert_eq!(
        def.condition,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeGainedThisTurn {
                    player: PlayerScope::Controller,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        })
    );
}

#[test]
fn trigger_oversold_cemetery_upkeep_creature_graveyard_gate() {
    let def = parse_trigger_line(
            "At the beginning of your upkeep, if you have four or more creature cards in your graveyard, you may return target creature card from your graveyard to your hand.",
            "Oversold Cemetery",
        );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::Upkeep));
    assert_eq!(
        def.condition,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ZoneCardCount {
                    zone: ZoneRef::Graveyard,
                    card_types: vec![TypeFilter::Creature],
                    filter: None,
                    scope: CountScope::Controller,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 4 },
        })
    );
    assert!(def.optional);
}

#[test]
fn trigger_put_into_your_graveyard_from_library() {
    let def = parse_trigger_line(
        "Whenever a creature card is put into your graveyard from your library, draw a card.",
        "Some Card",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.origin, Some(Zone::Library));
    assert_eq!(def.destination, Some(Zone::Graveyard));
    // CR 109.5: "your graveyard" narrows valid_card.controller (the field
    // match_changes_zone actually consults) — not just valid_target.
    if let Some(TargetFilter::Typed(tf)) = &def.valid_card {
        assert_eq!(tf.controller, Some(ControllerRef::You));
    } else {
        panic!(
            "Expected Typed valid_card with controller=You, got {:?}",
            def.valid_card
        );
    }
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You)
        ))
    );
}

/// Regression for issue #311: Undead Alchemist class. "Whenever a creature
/// card is put into an opponent's graveyard from their library" must:
///   - set origin = Library (CR 603.6c: from-library zone constraint)
///   - set valid_card.controller = Opponent (CR 109.5: "their" anaphor
///     binds back to the previously named opponent; valid_card is the field
///     match_changes_zone consults).
///     Without these, the trigger fires when the controller's own creatures go
///     from battlefield to graveyard — the user-reported softlock chain.
#[test]
fn trigger_put_into_opponent_graveyard_from_their_library() {
    let def = parse_trigger_line(
            "Whenever a creature card is put into an opponent's graveyard from their library, exile that card.",
            "Undead Alchemist",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.origin, Some(Zone::Library));
    assert_eq!(def.destination, Some(Zone::Graveyard));
    if let Some(TargetFilter::Typed(tf)) = &def.valid_card {
        assert_eq!(
            tf.controller,
            Some(ControllerRef::Opponent),
            "valid_card.controller must be Opponent so the trigger does not \
                 fire on the source's own death"
        );
        assert!(
            tf.type_filters.contains(&TypeFilter::Creature),
            "Expected Creature type filter, got {:?}",
            tf.type_filters
        );
    } else {
        panic!(
            "Expected Typed valid_card with controller=Opponent, got {:?}",
            def.valid_card
        );
    }
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        ))
    );
}

/// CR 109.5 + CR 700.4: Bloodchief Ascension class — "an opponent's
/// graveyard from anywhere". `origin = None` (no zone restriction) but
/// `valid_card.controller = Opponent` so the trigger only fires for cards
/// owned by an opponent.
#[test]
fn trigger_put_into_opponent_graveyard_from_anywhere() {
    let def = parse_trigger_line(
        "Whenever a card is put into an opponent's graveyard from anywhere, you gain 1 life.",
        "Bloodchief Ascension",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.origin, None);
    assert_eq!(def.destination, Some(Zone::Graveyard));
    if let Some(TargetFilter::Typed(tf)) = &def.valid_card {
        assert_eq!(tf.controller, Some(ControllerRef::Opponent));
    } else {
        panic!(
            "Expected Typed valid_card with controller=Opponent, got {:?}",
            def.valid_card
        );
    }
}

/// CR 109.5: "a player's graveyard from any library" — owner-unscoped form.
/// Both possessives leave `valid_card.controller` and `origin` constrained
/// only on the side that matters: origin=Library (CR 603.6c) but no
/// controller filter on the card (since "a player" / "any" cover all).
#[test]
fn trigger_put_into_any_player_graveyard_from_any_library() {
    let def = parse_trigger_line(
        "Whenever a creature card is put into a player's graveyard from any library, draw a card.",
        "Spelltracker",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.origin, Some(Zone::Library));
    assert_eq!(def.destination, Some(Zone::Graveyard));
    if let Some(TargetFilter::Typed(tf)) = &def.valid_card {
        assert_eq!(tf.controller, None, "Expected no controller scope");
        assert!(
            tf.type_filters.contains(&TypeFilter::Creature),
            "Expected Creature type filter, got {:?}",
            tf.type_filters
        );
    } else {
        panic!(
            "Expected Typed valid_card with controller=None, got {:?}",
            def.valid_card
        );
    }
}

#[test]
fn trigger_one_or_more_creature_cards_put_into_graveyard_from_library() {
    // CR 603.2c: "One or more" triggers fire once per batch
    let def = parse_trigger_line(
            "Whenever one or more creature cards are put into your graveyard from your library, draw a card.",
            "Some Card",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZoneAll);
    assert_eq!(def.origin, Some(Zone::Library));
    assert_eq!(def.destination, Some(Zone::Graveyard));
    assert!(def.batched);
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You)
        ))
    );
    // CR 109.5: Subject filter must carry both the creature type AND the
    // owner-narrowed controller scope so the matcher fires only for the
    // controller's cards.
    if let Some(TargetFilter::Typed(tf)) = &def.valid_card {
        assert!(
            tf.type_filters.contains(&TypeFilter::Creature),
            "Expected Creature in type_filters, got {:?}",
            tf.type_filters
        );
        assert_eq!(
            tf.controller,
            Some(ControllerRef::You),
            "Expected controller=You merged from 'your graveyard', got {:?}",
            tf.controller
        );
    } else {
        panic!("Expected Typed creature filter, got {:?}", def.valid_card);
    }
}

#[test]
fn trigger_nontoken_creature_put_into_graveyard() {
    let def = parse_trigger_line(
        "Whenever a nontoken creature is put into your graveyard, draw a card.",
        "Some Card",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Graveyard));
    // CR 111.1: "nontoken" is token identity, not a fake subtype.
    if let Some(TargetFilter::Typed(tf)) = &def.valid_card {
        assert!(
            tf.properties.contains(&FilterProp::NonToken),
            "Expected NonToken in properties, got {:?}",
            tf.properties
        );
    } else {
        panic!(
            "Expected Typed filter with NonToken, got {:?}",
            def.valid_card
        );
    }
}

#[test]
fn trigger_creature_with_power_4_or_greater_enters() {
    let def = parse_trigger_line(
            "Whenever a creature with power 4 or greater enters the battlefield under your control, draw a card.",
            "Some Card",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    // Should have PtComparison(Power, GE, 4) in the filter props
    if let Some(TargetFilter::Typed(tf)) = &def.valid_card {
        assert!(
            tf.properties.iter().any(|p| matches!(
                p,
                FilterProp::PtComparison {
                    stat: PtStat::Power,
                    scope: PtValueScope::Current,
                    comparator: Comparator::GE,
                    value: QuantityExpr::Fixed { value: 4 }
                }
            )),
            "Expected PtComparison(Power, GE, 4) in properties, got {:?}",
            tf.properties
        );
    } else {
        panic!(
            "Expected Typed filter with PtComparison, got {:?}",
            def.valid_card
        );
    }
}

#[test]
fn trigger_face_down_creature_dies() {
    let def = parse_trigger_line(
        "Whenever a face-down creature you control dies, draw a card.",
        "Some Card",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.origin, Some(Zone::Battlefield));
    assert_eq!(def.destination, Some(Zone::Graveyard));
    // Should have FaceDown in the filter props
    if let Some(TargetFilter::Typed(tf)) = &def.valid_card {
        assert!(
            tf.properties
                .iter()
                .any(|p| matches!(p, FilterProp::FaceDown)),
            "Expected FaceDown in properties, got {:?}",
            tf.properties
        );
        assert_eq!(tf.controller, Some(ControllerRef::You));
    } else {
        panic!(
            "Expected Typed filter with FaceDown, got {:?}",
            def.valid_card
        );
    }
}

#[test]
fn trigger_put_into_your_graveyard_no_origin() {
    // "is put into your graveyard" without "from" clause
    let def = parse_trigger_line(
        "Whenever a creature is put into your graveyard, draw a card.",
        "Some Card",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.origin, None);
    assert_eq!(def.destination, Some(Zone::Graveyard));
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You)
        ))
    );
}

#[test]
fn trigger_one_or_more_cards_put_into_graveyard_from_anywhere() {
    let def = parse_trigger_line(
        "Whenever one or more cards are put into your graveyard from anywhere, draw a card.",
        "Some Card",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZoneAll);
    assert_eq!(def.origin, None);
    assert_eq!(def.destination, Some(Zone::Graveyard));
    assert!(def.batched);
    // "your graveyard" scopes the affected cards to the controller even
    // when "cards" carries no additional type restriction.
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You)
        ))
    );
}

#[test]
fn trigger_precombat_main_phase() {
    // CR 505.1: "precombat main phase" maps to PreCombatMain.
    let def = parse_trigger_line(
        "At the beginning of your precombat main phase, add one mana of any color.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::PreCombatMain));
    assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
}

#[test]
fn trigger_postcombat_main_phase() {
    // CR 505.1: "postcombat main phase" maps to PostCombatMain.
    let def = parse_trigger_line(
        "At the beginning of each player's postcombat main phase, that player may cast a spell.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::PostCombatMain));
    // "each player's" has no "your" or "opponent" → no constraint
    assert_eq!(def.constraint, None);
}

/// CR 107.3i + CR 120.1 + CR 510.1: Tymna the Weaver — the trigger body
/// must bind `X` from the trailing "where X is …" clause across BOTH the
/// "pay X life" cost AND the "draw X cards" sub-ability. The bound
/// expression must resolve to
/// `PlayerCount { OpponentDealtCombatDamage }`, not to an empty-shaped
/// `ObjectCount` (which previously matched every battlefield permanent
/// and made Tymna draw all 12 of the player's permanents instead of 1).
#[test]
fn trigger_tymna_the_weaver_pays_and_draws_bound_x() {
    use crate::types::ability::{AbilityCost, Effect, PlayerFilter, QuantityExpr, QuantityRef};

    let def = parse_trigger_line(
        "At the beginning of each of your postcombat main phases, you may pay X life, \
             where X is the number of opponents that were dealt combat damage this turn. \
             If you do, draw X cards.",
        "Tymna the Weaver",
    );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::PostCombatMain));
    assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
    assert!(def.optional, "trigger should be optional ('you may')");

    let bound_qty = QuantityExpr::Ref {
        qty: QuantityRef::PlayerCount {
            filter: PlayerFilter::OpponentDealtCombatDamage { source: None },
        },
    };

    let execute = def
        .execute
        .as_ref()
        .expect("trigger must have an execute body");

    // CR 118.8 + CR 107.3i: pay-life cost amount carries the bound X.
    match execute.effect.as_ref() {
        Effect::PayCost {
            cost: AbilityCost::PayLife { amount },
            ..
        } => assert_eq!(*amount, bound_qty, "pay-life cost amount must be bound X"),
        other => panic!("expected PayCost::Life, got {:?}", other),
    }

    // CR 121.1 + CR 107.3i: the conditional "if you do, draw X cards"
    // sub-ability count carries the SAME bound X.
    let sub = execute
        .sub_ability
        .as_ref()
        .expect("trigger must have draw sub-ability");
    match sub.effect.as_ref() {
        Effect::Draw { count, .. } => {
            assert_eq!(*count, bound_qty, "draw count must be bound X");
        }
        other => panic!("expected Effect::Draw, got {:?}", other),
    }
}

#[test]
fn trigger_first_main_phase() {
    // CR 505.1: "first main phase" is an alias for precombat main phase.
    let def = parse_trigger_line(
        "At the beginning of your first main phase, add one mana of any color.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::PreCombatMain));
    assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
}

/// Issue #2900: Blinkmoth Urn — "that player adds {C} for each artifact they
/// control" must route mana to `ScopedPlayer` and count the scoped player's
/// artifacts, not the source controller's.
#[test]
fn phase_trigger_blinkmoth_urn_that_player_adds_mana_for_their_artifacts() {
    let def = parse_trigger_line(
            "At the beginning of each player's first main phase, if this artifact is untapped, that player adds {C} for each artifact they control.",
            "Blinkmoth Urn",
        );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::PreCombatMain));
    assert_eq!(def.constraint, None);
    let exec = def
        .execute
        .as_ref()
        .expect("Blinkmoth Urn must have execute");
    match exec.effect.as_ref() {
        Effect::Mana {
            produced: ManaProduction::Colorless { count },
            target,
            ..
        } => {
            assert_eq!(
                *target,
                Some(TargetFilter::ScopedPlayer),
                "mana recipient must be the active player (ScopedPlayer)"
            );
            let QuantityExpr::Ref {
                qty:
                    QuantityRef::ObjectCount {
                        filter: TargetFilter::Typed(tf),
                    },
            } = count
            else {
                panic!("expected ObjectCount artifact filter, got {count:?}");
            };
            assert!(
                tf.type_filters.contains(&TypeFilter::Artifact),
                "count must be artifacts"
            );
            assert_eq!(
                tf.controller,
                Some(ControllerRef::ScopedPlayer),
                "\"they control\" must bind to the scoped player"
            );
        }
        other => panic!("expected Effect::Mana, got {other:?}"),
    }
}

#[test]
fn trigger_each_of_your_main_phases_uses_main_phase_constraint() {
    let def = parse_trigger_line(
            "At the beginning of each of your main phases, if you haven't added mana with this ability this turn, you may add X mana of any one color, where X is the number of Islands target opponent controls.",
            "Carpet of Flowers",
        );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, None);
    assert_eq!(
        def.constraint,
        Some(TriggerConstraint::OnlyDuringYourMainPhase)
    );
    assert!(def.optional, "trigger should be optional ('you may')");
}

/// Coalition Relic, third ability — Future Sight artifact, issue #130.
///
/// Oracle: "At the beginning of your precombat main phase, you may remove
/// all charge counters from ~. If you do, add one mana of any color for
/// each charge counter removed this way."
///
/// This trigger composes four primitives that must all wire together:
///
/// 1. CR 505.1: "precombat main phase" → `Phase::PreCombatMain`.
/// 2. CR 603.5 + CR 118.12: "you may" → `optional: true` on both the
///    trigger def AND its execute ability, routing through
///    `WaitingFor::OptionalEffectChoice` at resolution; "If you do" checks
///    whether the player chose to pay the optional cost.
/// 3. CR 122.1: "remove all charge counters from ~" →
///    `Effect::RemoveCounter { counter_type: "charge", count: -1, target:
///    SelfRef }` (count=-1 is the "remove all" sentinel).
/// 4. CR 608.2c + CR 106.1 + CR 122.1: "If you do, add one mana of any
///    color for each charge counter removed this way" →
///    sub_ability with `condition: Some(IfYouDo)` and effect
///    `Effect::Mana { produced: AnyOneColor { count:
///    QuantityExpr::Ref { qty: PreviousEffectAmount }, color_options: <all
///    five>, .. }, .. }`. The runtime parent-effect-aware scan in
///    `effects/mod.rs` reads `GameEvent::CounterRemoved` for RemoveCounter
///    parents to populate `last_effect_amount`, which
///    `PreviousEffectAmount` reads.
#[test]
fn trigger_coalition_relic_charge_counter_drain() {
    use crate::types::ability::AbilityCondition;
    use crate::types::ability::ManaProduction;

    let def = parse_trigger_line(
            "At the beginning of your precombat main phase, you may remove all charge counters from ~. If you do, add one mana of any color for each charge counter removed this way.",
            "Coalition Relic",
        );

    // (1) Phase shape.
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::PreCombatMain));
    assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));

    // (2) Optional flag on the trigger def itself.
    assert!(def.optional, "trigger must carry optional: true");

    let execute = def
        .execute
        .as_ref()
        .expect("trigger must have an execute body");

    // (2 cont.) Optional propagated to the execute root so the resolver
    // routes through WaitingFor::OptionalEffectChoice.
    assert!(
        execute.optional,
        "execute root must carry optional: true so OptionalEffectChoice fires"
    );

    // (3) Root effect: remove all charge counters from self.
    match &*execute.effect {
        Effect::RemoveCounter {
            counter_type,
            count,
            target,
        } => {
            assert_eq!(
                counter_type,
                &Some(crate::types::counter::CounterType::Generic(
                    "charge".to_string()
                ))
            );
            assert_eq!(
                *count,
                crate::types::ability::QuantityExpr::Fixed { value: -1 },
                "Fixed(-1) is the remove-all sentinel"
            );
            assert!(matches!(target, TargetFilter::SelfRef));
        }
        other => panic!("expected Effect::RemoveCounter, got {other:?}"),
    }

    // (4) Sub-ability: gated by IfYouDo, produces dynamic-count any-color
    // mana from the count of counters removed by the parent.
    let sub = execute
        .sub_ability
        .as_ref()
        .expect("execute must have an If-you-do sub-ability");

    assert_eq!(
        sub.condition,
        Some(AbilityCondition::effect_performed()),
        "sub-ability must be gated by IfYouDo so it only fires when the player accepted"
    );
    assert!(
        !sub.optional,
        "sub-ability is not its own optional choice — only the root prompts the player"
    );

    match &*sub.effect {
        Effect::Mana {
            produced,
            target: mana_target,
            ..
        } => {
            assert!(
                mana_target.is_none(),
                "no player target on this mana production"
            );
            match produced {
                ManaProduction::AnyOneColor {
                    count,
                    color_options,
                    ..
                } => {
                    assert_eq!(
                        *count,
                        QuantityExpr::Ref {
                            qty: QuantityRef::PreviousEffectAmount
                        },
                        "for-each tail must dispatch to PreviousEffectAmount"
                    );
                    assert_eq!(
                        color_options.len(),
                        5,
                        "any-color must offer all five colors"
                    );
                }
                other => panic!("expected AnyOneColor, got {other:?}"),
            }
        }
        other => panic!("expected Effect::Mana on sub-ability, got {other:?}"),
    }
}

#[test]
fn trigger_second_main_phase() {
    // CR 505.1: "second main phase" is an alias for postcombat main phase.
    let def = parse_trigger_line(
        "At the beginning of each player's second main phase, that player draws a card.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::PostCombatMain));
    assert_eq!(def.constraint, None);
}

// --- Plan 03: Attacks trigger sub-patterns ---

#[test]
fn trigger_enchanted_player_attacked() {
    // CR 508.1a: "enchanted player is attacked" — AttachedTo as defending player.
    let def = parse_trigger_line(
        "Whenever enchanted player is attacked, create a 1/1 white Soldier creature token.",
        "Curse of the Forsaken",
    );
    assert_eq!(def.mode, TriggerMode::Attacks);
    assert_eq!(def.valid_target, Some(TargetFilter::AttachedTo));
    // CR 508.3b: only fires when the player themselves is attacked.
    assert_eq!(def.attack_target_filter, Some(AttackTargetFilter::Player),);
    assert!(def.execute.is_some());
}

#[test]
fn trigger_two_or_more_creatures_attack() {
    // CR 508.1a + CR 603.2c: head-noun counts use the full attackers-declared
    // batch, not source-relative co-attacker counting.
    let def = parse_trigger_line(
        "Whenever two or more creatures you control attack a player, draw a card.",
        "Edric, Spymaster of Trest",
    );
    assert_eq!(def.mode, TriggerMode::YouAttack);
    assert_eq!(
        def.condition,
        Some(TriggerCondition::AttackersDeclaredCount {
            subject: AttackersDeclaredCountSubject::Controller {
                scope: ControllerRef::You,
                filter: None,
            },
            comparator: Comparator::GE,
            count: 2,
        })
    );
    // CR 508.3a: the lexical "attack a player" restriction is an
    // attacked-target narrowing carried by `attack_target_filter`, NOT the
    // `valid_target` overload. With no attachment-relation clause, the
    // attacking-player gate stays at the controller-scoped default
    // (`valid_target == None`).
    assert_eq!(
        def.attack_target_filter,
        Some(AttackTargetFilter::Player),
        "\"attack a player\" sets the purpose-built attack_target_filter"
    );
    assert_eq!(
        def.valid_target, None,
        "no attachment clause ⇒ controller-scoped default, not the Player overload"
    );
    assert!(def.execute.is_some());
}

#[test]
fn trigger_two_or_more_typed_creatures_attack() {
    let def = parse_trigger_line(
        "Whenever two or more Dinosaurs attack, draw a card.",
        "Test Dinosaur Lord",
    );
    assert_eq!(def.mode, TriggerMode::YouAttack);
    assert!(def.batched);
    match &def.valid_card {
        Some(TargetFilter::Typed(tf)) => assert!(
            tf.type_filters
                .iter()
                .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "Dinosaur")),
            "expected Dinosaur subtype in valid_card, got {:?}",
            tf.type_filters,
        ),
        other => panic!("expected Typed valid_card with Dinosaur, got {other:?}"),
    }
    match &def.condition {
        Some(TriggerCondition::AttackersDeclaredCount {
            subject:
                AttackersDeclaredCountSubject::Controller {
                    scope: ControllerRef::You,
                    filter: Some(TargetFilter::Typed(tf)),
                },
            comparator: Comparator::GE,
            count: 2,
        }) => assert!(
            tf.type_filters
                .iter()
                .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "Dinosaur")),
            "expected Dinosaur subtype in condition filter, got {:?}",
            tf.type_filters,
        ),
        other => {
            panic!("expected AttackersDeclaredCount {{ Controller {{ You, Some(Dinosaur) }}, GE, 2 }}, got {other:?}")
        }
    }
}

// --- Plan 03: SpellCast trigger sub-patterns ---

#[test]
fn trigger_first_spell_opponents_turn() {
    // CR 601.2: "first spell during each opponent's turn"
    let def = parse_trigger_line(
        "Whenever you cast your first spell during each opponent's turn, draw a card.",
        "Faerie Mastermind",
    );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(
        def.constraint,
        Some(TriggerConstraint::NthSpellThisTurn { n: 1, filter: None })
    );
    assert_eq!(
        def.condition,
        Some(TriggerCondition::DuringPlayersTurn {
            player: PlayerFilter::Opponent,
        })
    );
}

/// CR 603.4 + CR 102.1: "whenever you cast your first spell during each of
/// your turns" — restricted to the controller's own turn. Target card:
/// Rashmi and Ragavan.
#[test]
fn trigger_first_spell_during_each_of_your_turns() {
    let def = parse_trigger_line(
            "Whenever you cast your first spell during each of your turns, exile the top card of target opponent's library and create a Treasure token.",
            "Rashmi and Ragavan",
        );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(
        def.constraint,
        Some(TriggerConstraint::NthSpellThisTurn { n: 1, filter: None })
    );
    assert_eq!(
        def.condition,
        Some(TriggerCondition::DuringPlayersTurn {
            player: PlayerFilter::Controller,
        })
    );

    let def = parse_trigger_line(
        "Whenever you cast your first spell during your turn, draw a card.",
        "Timing Tail Fixture",
    );
    assert_eq!(
        def.condition,
        Some(TriggerCondition::DuringPlayersTurn {
            player: PlayerFilter::Controller,
        })
    );
}

/// CR 107.3 + CR 202.1: "whenever you cast your first spell with {X} in its
/// mana cost each turn" — the "with {X}" qualifier lives AFTER "spell"
/// (post-spell modifier), not before. Verifies `HasXInManaCost` filter
/// emission on the per-turn SpellCast trigger. Target cards: Lattice
/// Library, Nev the Practical Dean, Owlin Spiralmancer, Zimone Infinite
/// Analyst.
#[test]
fn trigger_first_spell_with_x_in_cost() {
    use crate::types::ability::{FilterProp, TypedFilter};
    let def = parse_trigger_line(
        "Whenever you cast your first spell with {X} in its mana cost each turn, draw a card.",
        "Nev, the Practical Dean",
    );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    let expected_filter =
        TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::HasXInManaCost]));
    assert_eq!(
        def.constraint,
        Some(TriggerConstraint::NthSpellThisTurn {
            n: 1,
            filter: Some(expected_filter),
        }),
        "first-spell-with-X trigger must carry HasXInManaCost filter"
    );
    assert!(def.execute.is_some());
}

/// CR 107.3 + CR 202.1: Combined type phrase + X-in-cost qualifier.
/// "your first creature spell with {X} in its mana cost each turn" should
/// produce an And-composed filter of (Creature) AND (HasXInManaCost).
#[test]
fn trigger_first_creature_spell_with_x_in_cost() {
    use crate::types::ability::{FilterProp, TypeFilter, TypedFilter};
    let def = parse_trigger_line(
            "Whenever you cast your first creature spell with {X} in its mana cost each turn, draw a card.",
            "Hypothetical",
        );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    let TriggerConstraint::NthSpellThisTurn { n, ref filter } = def.constraint.unwrap() else {
        panic!("expected NthSpellThisTurn");
    };
    assert_eq!(n, 1);
    let filter = filter.as_ref().expect("filter must be set");
    // Shape: And { filters: [Creature typed, HasXInManaCost typed] }
    match filter {
        TargetFilter::And { filters } => {
            assert_eq!(filters.len(), 2, "expected 2-part AND filter");
            assert!(
                    filters
                        .iter()
                        .any(|f| matches!(f, TargetFilter::Typed(tf) if tf.type_filters.contains(&TypeFilter::Creature))),
                    "must include Creature type filter: {filters:?}"
                );
            assert!(
                filters.iter().any(|f| matches!(
                    f,
                    TargetFilter::Typed(TypedFilter { properties, .. })
                        if properties.contains(&FilterProp::HasXInManaCost)
                )),
                "must include HasXInManaCost filter: {filters:?}"
            );
        }
        other => panic!("expected AND filter, got {other:?}"),
    }
}

/// Ensure the existing "first spell each turn" behavior (no qualifier) is
/// preserved by the refactor — filter remains `None`.
#[test]
fn trigger_first_spell_no_qualifier_remains_none() {
    let def = parse_trigger_line(
        "Whenever you cast your first spell each turn, draw a card.",
        "Archmage Emeritus",
    );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(
        def.constraint,
        Some(TriggerConstraint::NthSpellThisTurn { n: 1, filter: None })
    );
}

#[test]
fn trigger_copy_spell() {
    // CR 707.10: "you copy a spell" maps to SpellCopy.
    let def = parse_trigger_line(
        "Whenever you copy a spell, put a +1/+1 counter on ~.",
        "Ivy, Gleeful Spellthief",
    );
    assert_eq!(def.mode, TriggerMode::SpellCopy);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert!(def.execute.is_some());
}

/// CR 603.4 + CR 508.1 (issue #1487, Fire Lord Azula): a `"while ~ is
/// attacking"` gate appended to a cast-trigger event restricts the trigger
/// to combat. Before the fix the clause was dropped, so the copy fired on
/// every spell the controller cast (regardless of whether the source was
/// attacking). The gate must surface as a `SourceIsAttacking` condition.
#[test]
fn trigger_cast_spell_while_attacking_gates_on_combat() {
    let def = parse_trigger_line(
        "Whenever you cast a spell while Fire Lord Azula is attacking, copy that spell. \
             You may choose new targets for the copy.",
        "Fire Lord Azula",
    );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert_eq!(
        def.condition,
        Some(TriggerCondition::SourceIsAttacking),
        "the `while ~ is attacking` gate must become a SourceIsAttacking condition"
    );
    // The remaining event clause still parses to the copy effect.
    assert!(matches!(
        def.execute.as_deref().map(|a| a.effect.as_ref()),
        Some(crate::types::ability::Effect::CopySpell { .. })
    ));
}

/// CR 603.4 + CR 508.1: the `"while ~ is attacking"` gate composes with an
/// existing intervening-if rather than overwriting it — the trigger fires
/// only when both predicates hold.
#[test]
fn trigger_while_attacking_composes_with_existing_condition() {
    let def = parse_trigger_line(
            "Whenever you cast a creature spell while ~ is attacking, if you control three or more creatures, draw a card.",
            "Test Card",
        );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    // The `while` gate and the intervening-if are AND-composed.
    match def.condition {
        Some(TriggerCondition::And { conditions }) => {
            assert!(
                conditions.contains(&TriggerCondition::SourceIsAttacking),
                "expected SourceIsAttacking among AND conditions, got {conditions:?}"
            );
            assert!(
                conditions
                    .iter()
                    .any(|c| !matches!(c, TriggerCondition::SourceIsAttacking)),
                "expected the intervening-if to also be present, got {conditions:?}"
            );
        }
        other => panic!("expected And(SourceIsAttacking, <if>), got {other:?}"),
    }
}

/// CR 603.4 + CR 122.1 (issue #2376, Pyromancer's Ascension): the
/// `"while this enchantment has two or more quest counters on it"` gate on
/// a cast trigger must surface as a `HasCounters` condition, not fire on
/// every instant/sorcery cast.
#[test]
fn trigger_cast_instant_sorcery_while_two_or_more_quest_counters() {
    let def = parse_trigger_line(
            "Whenever you cast an instant or sorcery spell while this enchantment has two or more quest counters on it, you may copy that spell. You may choose new targets for the copy.",
            "Pyromancer's Ascension",
        );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert_eq!(
        def.condition,
        Some(TriggerCondition::HasCounters {
            counters: CounterMatch::OfType(CounterType::Generic("quest".to_string())),
            minimum: 2,
            maximum: None,
        }),
        "the quest-counter gate must become a HasCounters condition"
    );
    assert!(matches!(
        def.execute.as_deref().map(|a| a.effect.as_ref()),
        Some(Effect::CopySpell { .. })
    ));
}

/// CR 603.2 + CR 608.2b (issue #2376): instant/sorcery casts that share a
/// graveyard card name must be encoded on `valid_card`, not left unfiltered.
#[test]
fn trigger_cast_instant_sorcery_same_name_as_graveyard_card() {
    use crate::types::ability::SharedQualityRelation;

    let def = parse_trigger_line(
            "Whenever you cast an instant or sorcery spell that has the same name as a card in your graveyard, you may put a quest counter on this enchantment.",
            "Pyromancer's Ascension",
        );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert!(def.condition.is_none());

    let valid_card = def.valid_card.expect("spell qualifier must set valid_card");
    let TargetFilter::And { filters } = valid_card else {
        panic!("expected And(instant/sorcery, graveyard name match), got {valid_card:?}");
    };
    assert_eq!(filters.len(), 2);
    assert!(filters.iter().any(|filter| matches!(
        filter,
        TargetFilter::Or { filters: branches }
            if branches.len() == 2
                && branches.iter().all(|branch| matches!(
                    branch,
                    TargetFilter::Typed(TypedFilter { type_filters, .. })
                        if type_filters.iter().any(|t| matches!(t, TypeFilter::Instant))
                            || type_filters.iter().any(|t| matches!(t, TypeFilter::Sorcery))
                ))
    )));
    assert!(filters.iter().any(|filter| matches!(
        filter,
        TargetFilter::Typed(TypedFilter { properties, .. })
            if properties.iter().any(|property| matches!(
                property,
                FilterProp::SharesQuality {
                    quality: SharedQuality::Name,
                    reference: Some(reference),
                    relation: SharedQualityRelation::Shares,
                } if matches!(
                    reference.as_ref(),
                    TargetFilter::Typed(TypedFilter { properties, .. })
                        if properties.iter().any(|p| matches!(
                            p,
                            FilterProp::Owned { controller: ControllerRef::You }
                        )) && properties.iter().any(|p| matches!(
                            p,
                            FilterProp::InZone { zone: Zone::Graveyard }
                        ))
                )
            ))
    )));
    assert!(matches!(
        def.execute.as_deref().map(|a| a.effect.as_ref()),
        Some(Effect::PutCounter { .. })
    ));
}

// --- Plan 03: DamageDone trigger sub-patterns ---

#[test]
fn trigger_dealt_damage_by_source_dies() {
    // CR 700.4 + CR 120.1: "a creature dealt damage by ~ this turn dies"
    let def = parse_trigger_line(
            "Whenever a creature dealt damage by Syr Konrad, the Grim this turn dies, each opponent loses 1 life.",
            "Syr Konrad, the Grim",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.origin, Some(Zone::Battlefield));
    assert_eq!(def.destination, Some(Zone::Graveyard));
    assert_eq!(
        def.condition,
        Some(TriggerCondition::DealtDamageBySourceThisTurn)
    );
}

#[test]
fn trigger_another_creature_damaged_by_spider_you_controlled_dies() {
    // Issue #1206 — Shelob, Child of Ungoliant
    let def = parse_trigger_line(
            "Whenever another creature dealt damage this turn by a Spider you controlled dies, create a token that's a copy of that creature, except it's a Food artifact with \"{2}, {T}, Sacrifice ~: You gain 3 life,\" and it loses all other card types.",
            "Shelob, Child of Ungoliant",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.origin, Some(Zone::Battlefield));
    assert_eq!(def.destination, Some(Zone::Graveyard));
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::Another])
        ))
    );
    assert_eq!(
        def.condition,
        Some(TriggerCondition::DealtDamageThisTurnBySource {
            source: TargetFilter::Typed(
                TypedFilter::default()
                    .subtype("Spider".to_string())
                    .controller(ControllerRef::You)
            )
        })
    );
}

#[test]
fn trigger_you_dealt_damage() {
    // CR 120.1: "whenever you're dealt damage" — player damage received.
    let def = parse_trigger_line(
        "Whenever you're dealt damage, put that many charge counters on ~.",
        "Stuffy Doll",
    );
    assert_eq!(def.mode, TriggerMode::DamageReceived);
    assert_eq!(def.damage_kind, DamageKindFilter::Any);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn trigger_you_dealt_combat_damage() {
    // CR 120.2a: "whenever you're dealt combat damage" — combat-only variant.
    let def = parse_trigger_line(
        "Whenever you're dealt combat damage, draw a card.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::DamageReceived);
    assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn trigger_combat_damage_dealt_to_you_passive() {
    // CR 120.2a: passive voice "combat damage is dealt to you" is the same
    // event as the active "you're dealt combat damage" (Risona, Asari
    // Commander; I Am Untouchable).
    let def = parse_trigger_line(
        "Whenever combat damage is dealt to you, draw a card.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::DamageReceived);
    assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn trigger_combat_damage_dealt_to_you_passive_when() {
    // CR 120.2a: "When" variant of the passive combat-damage form.
    let def = parse_trigger_line(
        "When combat damage is dealt to you, create a 4/4 Scarecrow.",
        "I Am Untouchable",
    );
    assert_eq!(def.mode, TriggerMode::DamageReceived);
    assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn trigger_combat_damage_dealt_to_you_compound_unsupported() {
    // GUARD: the compound "to you or a planeswalker you control" form
    // (Vengeful Pharaoh) is not the bare literal; it stays Unknown — an
    // honest gap, since the compound splitter is out of scope here.
    let def = parse_trigger_line(
            "Whenever combat damage is dealt to you or a planeswalker you control, return ~ from your graveyard to the battlefield.",
            "Vengeful Pharaoh",
        );
    assert!(matches!(def.mode, TriggerMode::Unknown(_)));
}

#[test]
fn trigger_you_deal_combat_damage_to_player_not_intercepted() {
    // NO-REGRESSION: the active deal-form ("deals combat damage to a
    // player") is DamageDone, not DamageReceived — the new passive
    // receive-literals must not capture it.
    let def = parse_trigger_line(
        "Whenever Risona deals combat damage to a player, you draw a card.",
        "Risona, Asari Commander",
    );
    assert_eq!(def.mode, TriggerMode::DamageDone);
}

#[test]
fn trigger_opponent_dealt_noncombat_damage() {
    // CR 120.2b: "whenever an opponent is dealt noncombat damage"
    let def = parse_trigger_line(
        "Whenever an opponent is dealt noncombat damage, you gain 1 life.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::DamageReceived);
    assert_eq!(def.damage_kind, DamageKindFilter::NoncombatOnly);
}

// --- Plan 03: CounterRemoved trigger sub-patterns ---

#[test]
fn trigger_time_counter_removed_exile() {
    // CR 122.1: "a time counter is removed from ~ while it's exiled"
    let def = parse_trigger_line(
            "Whenever a time counter is removed from ~ while it's exiled, you may cast a copy of ~ without paying its mana cost.",
            "Rift Bolt",
        );
    assert_eq!(def.mode, TriggerMode::CounterRemoved);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    assert_eq!(def.trigger_zones, vec![Zone::Exile]);
}

#[test]
fn trigger_counter_removed_no_zone_constraint() {
    // CR 122.1: "a time counter is removed from ~" without zone constraint.
    let def = parse_trigger_line(
        "Whenever a time counter is removed from ~, deal 1 damage to any target.",
        "Test Suspend Card",
    );
    assert_eq!(def.mode, TriggerMode::CounterRemoved);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    // No zone constraint — fires from default zones
    assert_eq!(def.trigger_zones, vec![Zone::Battlefield]);
}

// -----------------------------------------------------------------------
// CR 608.2k: Trigger pronoun resolution — "it"/"its" context-dependent
// -----------------------------------------------------------------------

#[test]
fn trigger_it_resolves_to_triggering_source_for_non_self_subject() {
    // "it" refers to the entering creature, not the enchantment
    let def = parse_trigger_line(
        "Whenever a creature you control enters, put a +1/+1 counter on it",
        "Test Enchantment",
    );
    let exec = def.execute.as_ref().expect("should have execute");
    match &*exec.effect {
        Effect::PutCounter { target, .. } => {
            assert_eq!(
                *target,
                TargetFilter::TriggeringSource,
                "non-self trigger 'it' should resolve to TriggeringSource"
            );
        }
        other => panic!("Expected PutCounter, got {:?}", other),
    }
}

#[test]
fn trigger_it_stays_self_ref_for_self_subject() {
    // "it" refers to ~ (the card itself entering)
    let def = parse_trigger_line(
        "When Test Card enters, put a +1/+1 counter on it",
        "Test Card",
    );
    let exec = def.execute.as_ref().expect("should have execute");
    match &*exec.effect {
        Effect::PutCounter { target, .. } => {
            assert_eq!(
                *target,
                TargetFilter::SelfRef,
                "self-trigger 'it' should stay SelfRef"
            );
        }
        other => panic!("Expected PutCounter, got {:?}", other),
    }
}

#[test]
fn trigger_tilde_stays_self_ref_with_non_self_subject() {
    // "~" always refers to the source permanent, even in non-self trigger
    let def = parse_trigger_line(
        "Whenever a creature you control enters, sacrifice ~",
        "Test Enchantment",
    );
    let exec = def.execute.as_ref().expect("should have execute");
    match &*exec.effect {
        Effect::Sacrifice { target, .. } => {
            assert_eq!(*target, TargetFilter::SelfRef, "~ should always be SelfRef");
        }
        other => panic!("Expected Sacrifice, got {:?}", other),
    }
}

#[test]
fn trigger_otherwise_branch_preserves_context() {
    // Tribute to the World Tree pattern: else_ability "it" = triggering creature
    let def = parse_trigger_line(
            "Whenever a creature you control enters, draw a card if its power is 3 or greater. Otherwise, put two +1/+1 counters on it.",
            "Tribute to the World Tree",
        );
    let exec = def.execute.as_ref().expect("should have execute");
    let else_ab = exec
        .else_ability
        .as_ref()
        .expect("should have else_ability");
    match &*else_ab.effect {
        Effect::PutCounter { target, count, .. } => {
            assert_eq!(
                *target,
                TargetFilter::TriggeringSource,
                "else_ability 'it' should be TriggeringSource"
            );
            assert_eq!(*count, QuantityExpr::Fixed { value: 2 });
        }
        other => panic!("Expected PutCounter in else_ability, got {:?}", other),
    }
}

#[test]
fn trigger_copy_token_suffix_condition_attaches_otherwise() {
    let (effect_without_if, trigger_condition) = extract_if_condition(
            "create a tapped token that's a copy of ~ if seven or more land cards are in your graveyard. otherwise, create a tapped 1/1 black insect creature token with flying.",
        );
    assert!(
        trigger_condition.is_none(),
        "suffix condition before otherwise must stay in effect chain, got {trigger_condition:?}"
    );
    assert!(
        crate::parser::oracle_nom::primitives::scan_contains(
            &effect_without_if,
            "if seven or more land cards are in your graveyard"
        ),
        "effect text should preserve suffix condition, got {effect_without_if:?}"
    );
    let mut effect_ctx = ParseContext {
        subject: Some(TargetFilter::SelfRef),
        card_name: Some("Scouring Swarm".to_string()),
        ..Default::default()
    };
    let chain_ir = parse_effect_chain_ir(&effect_without_if, AbilityKind::Spell, &mut effect_ctx);
    let chain = lower_effect_chain_ir(&chain_ir);
    assert!(
        chain.condition.is_some(),
        "effect chain should carry suffix condition, got {chain:?}"
    );

    let def = parse_trigger_line(
            "Whenever you sacrifice a land, create a tapped token that's a copy of this creature if seven or more land cards are in your graveyard. Otherwise, create a tapped 1/1 black Insect creature token with flying.",
            "Scouring Swarm",
        );
    let exec = def.execute.as_ref().expect("should have execute");
    assert!(
        matches!(
            &*exec.effect,
            Effect::CopyTokenOf {
                tapped: true,
                enters_attacking: false,
                ..
            }
        ),
        "expected tapped CopyTokenOf, got {:?}",
        exec.effect
    );
    assert!(
        exec.condition.is_some(),
        "copy branch should carry suffix condition"
    );
    let else_ab = exec
        .else_ability
        .as_ref()
        .expect("should have else_ability");
    assert!(
        matches!(&*else_ab.effect, Effect::Token { tapped: true, .. }),
        "expected tapped token in else_ability, got {:?}",
        else_ab.effect
    );
}

/// Issue #466 — CR 608.2c: a special (rider) clause occupies a chunk slot
/// and carries its own trailing boundary. A normal clause that follows a
/// special clause must stamp its `sub_link` from the boundary AFTER the
/// special clause, not from the stale boundary that preceded it.
///
/// Direct-IR construction tests the boundary-advance building block in
/// isolation: clause 0 normal (`boundary: Comma`), clause 1
/// `SpecialClause::Otherwise` (`boundary: Sentence`), clause 2 normal.
/// With the bug, clause 2 inherits clause 0's `Comma` → `ContinuationStep`;
/// with the fix it inherits clause 1's `Sentence` → `SequentialSibling`.
#[test]
fn lower_effect_chain_ir_advances_boundary_past_special_clause() {
    use crate::parser::oracle_ir::ast::{parsed_clause, ClauseBoundary};
    use crate::parser::oracle_ir::effect_chain::{ClauseIr, EffectChainIr, SpecialClause};
    use crate::types::ability::SubAbilityLink;

    fn make_clause(
        effect: Effect,
        boundary: Option<ClauseBoundary>,
        special: Option<SpecialClause>,
    ) -> ClauseIr {
        ClauseIr {
            parsed: parsed_clause(effect),
            boundary,
            condition: None,
            is_optional: false,
            opponent_may_scope: None,
            repeat_for: None,
            player_scope: None,
            starting_with: None,
            delayed_condition: None,
            prefix_delayed_condition: None,
            intrinsic_continuation: None,
            followup_continuation: None,
            absorbed_by_followup: false,
            multi_target: None,
            where_x_expression: None,
            is_otherwise: false,
            unless_pay: None,
            special,
            source_text: String::new(),
            target_selection_mode: Default::default(),
            target_chooser: None,
        }
    }

    let draw_one = || Effect::Draw {
        count: QuantityExpr::Fixed { value: 1 },
        target: TargetFilter::Controller,
    };
    // `SpecialClause::Otherwise` is a silent no-op when no prior conditional
    // def exists, so clause 0 needs no condition.
    let otherwise_def = Box::new(crate::types::ability::AbilityDefinition::new(
        AbilityKind::Spell,
        draw_one(),
    ));

    let ir = EffectChainIr {
        clauses: vec![
            // clause 0: normal, trailing boundary = Comma
            make_clause(draw_one(), Some(ClauseBoundary::Comma), None),
            // clause 1: SpecialClause::Otherwise, trailing boundary = Sentence
            make_clause(
                draw_one(),
                Some(ClauseBoundary::Sentence),
                Some(SpecialClause::Otherwise(otherwise_def)),
            ),
            // clause 2: normal — must stamp sub_link from clause 1's Sentence
            make_clause(draw_one(), None, None),
        ],
        kind: AbilityKind::Spell,
        chain_rounding: None,
        actor: None,
        repeat_until: None,
    };

    let root = lower_effect_chain_ir(&ir);
    // clause 0 → root def; clause 1 special (no-op); clause 2 → trailing sibling.
    let trailing = root
        .sub_ability
        .as_ref()
        .expect("clause 2 should lower to a trailing sub_ability def");
    assert_eq!(
        trailing.sub_link,
        SubAbilityLink::SequentialSibling,
        "clause after a special clause must inherit the boundary AFTER the \
             special clause (Sentence → SequentialSibling)"
    );
    assert_ne!(
        trailing.sub_link,
        SubAbilityLink::ContinuationStep,
        "regression guard: must NOT inherit clause 0's stale Comma boundary"
    );
}

#[test]
fn trigger_subject_predicate_it_gains() {
    // "it gains haste" — subject-predicate with "it" as subject.
    // The subject "it" resolves to TriggeringSource and lands in the
    // static_abilities[0].affected field (not the top-level `target`).
    let def = parse_trigger_line(
        "Whenever a creature you control enters, it gains haste until end of turn",
        "Test Enchantment",
    );
    let exec = def.execute.as_ref().expect("should have execute");
    match &*exec.effect {
        Effect::GenericEffect {
            static_abilities, ..
        } => {
            assert_eq!(
                static_abilities[0].affected,
                Some(TargetFilter::TriggeringSource),
                "subject-predicate 'it' should produce TriggeringSource in affected"
            );
        }
        other => panic!("Expected GenericEffect, got {:?}", other),
    }
}

#[test]
fn trigger_pronoun_after_self_ref_clause_stays_self_ref_for_generic_effect() {
    // In "put a counter on this creature. It can't be blocked", the
    // trailing pronoun refers to the object named by the prior effect
    // clause, not the artifact/creature whose entry triggered the ability.
    let def = parse_trigger_line(
            "Whenever this creature or another artifact you control enters, put a +1/+1 counter on this creature. It can't be blocked this turn.",
            "Kappa Cannoneer",
        );
    let exec = def.execute.as_ref().expect("should have execute");
    match &*exec.effect {
        Effect::PutCounter { target, .. } => {
            assert_eq!(
                *target,
                TargetFilter::SelfRef,
                "the counter clause should target the trigger source itself"
            );
        }
        other => panic!("Expected PutCounter, got {:?}", other),
    }
    let sub = exec
        .sub_ability
        .as_ref()
        .expect("should have evasion rider");
    match &*sub.effect {
        Effect::GenericEffect {
            static_abilities, ..
        } => {
            assert_eq!(
                static_abilities[0].affected,
                Some(TargetFilter::SelfRef),
                "the evasion rider should stay bound to the prior SelfRef clause"
            );
        }
        other => panic!("Expected GenericEffect, got {:?}", other),
    }
}

#[test]
fn trigger_equipped_creature_it_resolves_to_triggering_source() {
    // "it" = equipped creature (AttachedTo subject → TriggeringSource)
    let def = parse_trigger_line(
        "Whenever equipped creature attacks, put a +1/+1 counter on it",
        "Test Equipment",
    );
    let exec = def.execute.as_ref().expect("should have execute");
    match &*exec.effect {
        Effect::PutCounter { target, .. } => {
            assert_eq!(
                *target,
                TargetFilter::TriggeringSource,
                "equipped creature 'it' should be TriggeringSource"
            );
        }
        other => panic!("Expected PutCounter, got {:?}", other),
    }
}

// --- CR 115.9b: "that targets" trigger integration tests ---

#[test]
fn trigger_heroic_that_targets_self() {
    let def = parse_trigger_line(
            "Heroic — Whenever you cast a spell that targets this creature, put a +1/+1 counter on each creature you control.",
            "Phalanx Leader",
        );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    // valid_card should have Targets { SelfRef } property
    let valid_card = def.valid_card.expect("should have valid_card");
    if let TargetFilter::Typed(tf) = &valid_card {
        assert!(
            tf.properties.iter().any(
                |p| matches!(p, FilterProp::Targets { filter } if **filter == TargetFilter::SelfRef)
            ),
            "expected Targets {{ SelfRef }} in properties: {:?}",
            tf.properties
        );
    } else {
        panic!("expected Typed filter, got {valid_card:?}");
    }
}

#[test]
fn trigger_floodpits_etb_keeps_stun_counter_on_parent_target() {
    let def = parse_trigger_line(
            "When this creature enters, tap target creature an opponent controls and put a stun counter on it.",
            "Floodpits Drowner",
        );
    let exec = def.execute.as_ref().expect("should have execute ability");
    let sub = exec
        .sub_ability
        .as_ref()
        .expect("tap effect should chain into stun counter effect");
    match &*sub.effect {
        Effect::PutCounter { target, .. } => {
            assert!(
                matches!(target, TargetFilter::ParentTarget),
                "expected ParentTarget, got {target:?}"
            );
        }
        other => panic!("expected PutCounter sub-ability, got {other:?}"),
    }
}

/// CR 603.4 + CR 608.2h: A post-effect re-homeable `if` ("draw a card if
/// you have 40 or more life") is NOT hoisted to the trigger-level condition
/// by `extract_if_condition` — it is left intact for `strip_suffix_conditional`
/// to re-home onto a clause-level `AbilityCondition` (`execute.condition`).
#[test]
fn extract_if_you_have_n_or_more_life() {
    let (cleaned, cond) = extract_if_condition("draw a card if you have 40 or more life");
    assert_eq!(
        cond, None,
        "a re-homeable post-effect `if` must not hoist to the trigger condition",
    );
    assert_eq!(
        cleaned, "draw a card if you have 40 or more life",
        "the post-effect `if` must be left in the effect text for re-homing",
    );
}

/// CR 603.4 + CR 608.2h: "you win the game if you have 40 or more life" —
/// the `if` is post-effect (after the effect verb) and re-homeable, so
/// `extract_if_condition` leaves it intact rather than hoisting it.
#[test]
fn extract_if_you_have_n_or_more_life_win() {
    let (cleaned, cond) = extract_if_condition("you win the game if you have 40 or more life");
    assert_eq!(
        cond, None,
        "a re-homeable post-effect `if` must not hoist to the trigger condition",
    );
    assert_eq!(
        cleaned, "you win the game if you have 40 or more life",
        "the post-effect `if` must be left in the effect text for re-homing",
    );
}

/// CR 603.4 + CR 608.2h: A post-effect re-homeable `if` is left intact by
/// `extract_if_condition` — `strip_suffix_conditional` re-homes it later.
#[test]
fn extract_if_gained_life_regression() {
    let (cleaned, cond) = extract_if_condition("draw a card if you've gained life this turn");
    assert_eq!(
        cond, None,
        "a re-homeable post-effect `if` must not hoist to the trigger condition",
    );
    assert_eq!(
        cleaned, "draw a card if you've gained life this turn",
        "the post-effect `if` must be left in the effect text for re-homing",
    );
}

// --- Fix 1: find_effect_boundary comma splitter respects type-phrase lists ---

#[test]
fn split_trigger_compound_type_subject() {
    // "a creature, planeswalker, or battle enters" — comma is part of the subject
    let tp = TextPair::new(
        "whenever a creature, planeswalker, or battle enters the battlefield, draw a card",
        "whenever a creature, planeswalker, or battle enters the battlefield, draw a card",
    );
    let (condition, effect) = split_trigger(tp);
    assert!(
        condition.contains("enters"),
        "Condition should contain 'enters', got: '{condition}'"
    );
    assert_eq!(effect, "draw a card");
}

#[test]
fn split_trigger_two_type_subject() {
    // "a creature or enchantment" — no comma in subject but "artifact, creature, or enchantment" has
    let tp = TextPair::new(
        "whenever an artifact, creature, or enchantment enters the battlefield, you gain 1 life",
        "whenever an artifact, creature, or enchantment enters the battlefield, you gain 1 life",
    );
    let (condition, effect) = split_trigger(tp);
    assert!(
        condition.contains("enchantment"),
        "Condition should contain full type list, got: '{condition}'"
    );
    assert_eq!(effect, "you gain 1 life");
}

#[test]
fn continues_player_action_list_type_word() {
    // Bare type word after comma: "planeswalker, or battle enters"
    assert!(continues_player_action_list(
        "planeswalker, or battle enters"
    ));
    assert!(continues_player_action_list("or battle enters"));
    assert!(continues_player_action_list(
        "creature, or enchantment enters"
    ));
    // Non-type word should not match
    assert!(!continues_player_action_list("draw a card"));
    assert!(!continues_player_action_list("you gain 1 life"));
}

#[test]
fn continues_player_action_list_rejects_negated_auxiliary_effect_sentences() {
    assert!(!continues_player_action_list(
        "creatures you control can't be the targets of spells or abilities"
    ));
    assert!(!continues_player_action_list(
        "creatures you control don't untap during their controllers' untap steps"
    ));
    assert!(!continues_player_action_list(
        "creature doesn't untap during its controller's next untap step"
    ));
    assert!(!continues_player_action_list("creatures won't untap"));
    assert!(!continues_player_action_list("creature isn't tapped"));
    assert!(!continues_player_action_list("creatures aren't attacking"));
}

// --- Fix 2: missing event verbs ---

#[test]
fn trigger_is_exiled() {
    let def = parse_trigger_line(
        "Whenever a creature you control is exiled, draw a card.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::Exiled);
    assert!(def.valid_card.is_some());
}

#[test]
fn trigger_is_sacrificed() {
    let def = parse_trigger_line(
        "Whenever a creature is sacrificed, you gain 1 life.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::Sacrificed);
    assert!(def.valid_card.is_some());
}

#[test]
fn trigger_is_destroyed() {
    let def = parse_trigger_line(
        "Whenever a permanent you control is destroyed, draw a card.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::Destroyed);
    assert!(def.valid_card.is_some());
}

// --- Milled triggers (CR 701.17a) ---

#[test]
fn trigger_passive_milled_batched() {
    // The Wise Mothman / Mirelurk Queen / Screeching Scorchbeast shape.
    let def = parse_trigger_line(
        "Whenever one or more nonland cards are milled, put a +1/+1 counter on it.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::Milled);
    assert!(def.batched, "\"one or more\" subject must stamp batched");
    match def.valid_card {
        Some(TargetFilter::Typed(ref f)) => {
            assert!(
                f.type_filters.iter().any(|t| matches!(t, TypeFilter::Card)),
                "valid_card should be a card filter, got {:?}",
                f.type_filters
            );
            assert!(
                f.type_filters.iter().any(|t| matches!(
                    t,
                    TypeFilter::Non(inner) if matches!(**inner, TypeFilter::Land)
                )),
                "valid_card should carry Non(Land), got {:?}",
                f.type_filters
            );
        }
        other => panic!("expected Typed nonland-card filter, got {other:?}"),
    }
}

#[test]
fn trigger_passive_milled_single_card() {
    // Single-card passive form — no "one or more", so not batched.
    let def = parse_trigger_line("Whenever a card is milled, you gain 1 life.", "Test Card");
    assert_eq!(def.mode, TriggerMode::Milled);
    assert!(!def.batched);
    assert!(def.valid_card.is_some());
}

#[test]
fn trigger_active_milled_a_player() {
    // Glowing One shape: "Whenever a player mills a nonland card, …"
    let def = parse_trigger_line(
        "Whenever a player mills a nonland card, you gain 1 life.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::Milled);
    match def.valid_card {
        Some(TargetFilter::Typed(ref f)) => {
            assert!(
                f.type_filters.iter().any(|t| matches!(t, TypeFilter::Card)),
                "valid_card should be a card filter"
            );
            assert!(
                f.controller.is_none(),
                "\"a player\" milling has no controller scope, got {:?}",
                f.controller
            );
        }
        other => panic!("expected Typed nonland-card filter, got {other:?}"),
    }
}

#[test]
fn trigger_active_milled_an_opponent_scopes_controller() {
    // Infesting Radroach shape: the milled card lives in the opponent's library.
    let def = parse_trigger_line(
        "Whenever an opponent mills a nonland card, you gain 1 life.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::Milled);
    match def.valid_card {
        Some(TargetFilter::Typed(ref f)) => assert_eq!(
            f.controller,
            Some(ControllerRef::Opponent),
            "opponent milling must scope the milled-card filter to Opponent"
        ),
        other => panic!("expected Typed opponent-scoped filter, got {other:?}"),
    }
}

#[test]
fn trigger_active_milled_one_or_more() {
    // Zellix shape: "Whenever a player mills one or more creature cards, …"
    let def = parse_trigger_line(
            "Whenever a player mills one or more creature cards, create a 1/1 black Insect creature token.",
            "Test Card",
        );
    assert_eq!(def.mode, TriggerMode::Milled);
    assert!(def.batched, "\"one or more\" subject must stamp batched");
    match def.valid_card {
        Some(TargetFilter::Typed(ref f)) => assert!(
            f.type_filters
                .iter()
                .any(|t| matches!(t, TypeFilter::Creature)),
            "valid_card should be a creature-card filter, got {:?}",
            f.type_filters
        ),
        other => panic!("expected Typed creature-card filter, got {other:?}"),
    }
}

#[test]
fn trigger_fights() {
    let def = parse_trigger_line(
        "Whenever a creature you control fights, put a +1/+1 counter on it.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::Fight);
    assert!(def.valid_card.is_some());
}

#[test]
fn trigger_player_shuffles_library_scopes_actor_as_valid_target() {
    let def = parse_trigger_line(
        "Whenever an opponent shuffles their library, put a +1/+1 counter on this creature.",
        "Cosi's Trickster",
    );
    assert_eq!(def.mode, TriggerMode::Shuffled);
    assert_eq!(def.valid_card, None);
    assert!(matches!(
        def.valid_target,
        Some(TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::Opponent),
            ..
        }))
    ));
}

#[test]
fn trigger_player_shuffles_library_sibling_phrases() {
    let cases = [
        "Whenever a player shuffles their library, draw a card.",
        "Whenever each opponent shuffle his or her library, draw a card.",
        "Whenever you shuffle your library, draw a card.",
        "Whenever a player shuffles a library, draw a card.",
    ];

    for text in cases {
        let def = parse_trigger_line(text, "Test Card");
        assert_eq!(def.mode, TriggerMode::Shuffled, "{text}");
        assert!(
            def.valid_target.is_some(),
            "shuffle actor must be represented as valid_target for: {text}"
        );
    }
}

// -- StaticCondition → TriggerCondition bridge tests --

#[test]
fn bridge_quantity_comparison() {
    let sc = StaticCondition::QuantityComparison {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::HandSize {
                player: crate::types::ability::PlayerScope::Controller,
            },
        },
        comparator: Comparator::EQ,
        rhs: QuantityExpr::Fixed { value: 0 },
    };
    let tc = static_condition_to_trigger_condition(&sc).unwrap();
    assert!(matches!(
        tc,
        TriggerCondition::QuantityComparison {
            comparator: Comparator::EQ,
            ..
        }
    ));
}

#[test]
fn bridge_is_present_to_controls_type() {
    let sc = StaticCondition::IsPresent {
        filter: Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You),
        )),
    };
    let tc = static_condition_to_trigger_condition(&sc).unwrap();
    assert!(matches!(tc, TriggerCondition::ControlsType { .. }));
}

#[test]
fn bridge_is_present_none_filter_returns_none() {
    let sc = StaticCondition::IsPresent { filter: None };
    assert!(static_condition_to_trigger_condition(&sc).is_none());
}

#[test]
fn bridge_source_matches_filter() {
    let filter = TargetFilter::Typed(TypedFilter::creature());
    assert_eq!(
        static_condition_to_trigger_condition(&StaticCondition::SourceMatchesFilter {
            filter: filter.clone(),
        }),
        Some(TriggerCondition::SourceMatchesFilter { filter }),
    );
}

/// CR 603.4 + CR 301.5a: the equipped intervening-if must bridge to a
/// SourceMatchesFilter carrying a creature HasAttachment(Equipment) predicate.
/// Fail-before: `SourceIsEquipped` was in the `=> None` arm, so `.expect`
/// would panic on `None`.
#[test]
fn bridge_source_is_equipped_to_has_attachment() {
    let tc = static_condition_to_trigger_condition(&StaticCondition::SourceIsEquipped)
        .expect("SourceIsEquipped should bridge to a TriggerCondition");
    let TriggerCondition::SourceMatchesFilter { filter } = tc else {
        panic!("expected SourceMatchesFilter, got {tc:?}");
    };
    let TargetFilter::Typed(typed) = filter else {
        panic!("expected Typed filter, got {filter:?}");
    };
    assert!(
        typed.properties.iter().any(|p| matches!(
            p,
            FilterProp::HasAttachment {
                kind: AttachmentKind::Equipment,
                controller: None,
                exclude_source: crate::types::ability::SourceExclusion::Include,
            }
        )),
        "expected HasAttachment(Equipment, None, Include); got {:?}",
        typed.properties
    );
}

/// CR 603.4 + CR 303.4: the enchanted intervening-if bridges to an Aura
/// HasAttachment predicate, and crucially must NOT carry the Equipment
/// variant (discriminates attachment kind).
#[test]
fn bridge_source_is_enchanted_to_has_attachment() {
    let tc = static_condition_to_trigger_condition(&StaticCondition::SourceIsEnchanted)
        .expect("SourceIsEnchanted should bridge to a TriggerCondition");
    let TriggerCondition::SourceMatchesFilter { filter } = tc else {
        panic!("expected SourceMatchesFilter, got {tc:?}");
    };
    let TargetFilter::Typed(typed) = filter else {
        panic!("expected Typed filter, got {filter:?}");
    };
    assert!(
        typed.properties.iter().any(|p| matches!(
            p,
            FilterProp::HasAttachment {
                kind: AttachmentKind::Aura,
                controller: None,
                exclude_source: crate::types::ability::SourceExclusion::Include,
            }
        )),
        "expected HasAttachment(Aura, None, Include); got {:?}",
        typed.properties
    );
    assert!(
        !typed.properties.iter().any(|p| matches!(
            p,
            FilterProp::HasAttachment {
                kind: AttachmentKind::Equipment,
                ..
            }
        )),
        "enchanted bridge must not carry the Equipment HasAttachment; got {:?}",
        typed.properties
    );
}

/// CR 603.4 + CR 303.4: the enchanted intervening-if is source-object-wide and
/// must NOT impose a creature card-type gate (CR 303.4 Auras may enchant any
/// object or player; the host is not restricted to creatures). Fail-before:
/// the bridge used `TypedFilter::creature()`, so `type_filters == [Creature]`
/// and BOTH assertions below would fail. The HasAttachment{Aura} property is
/// reasserted as a sibling guard so the swap to `default()` can't silently
/// drop the attachment predicate.
#[test]
fn bridge_source_is_enchanted_no_creature_type() {
    let tc = static_condition_to_trigger_condition(&StaticCondition::SourceIsEnchanted)
        .expect("SourceIsEnchanted should bridge to a TriggerCondition");
    let TriggerCondition::SourceMatchesFilter { filter } = tc else {
        panic!("expected SourceMatchesFilter, got {tc:?}");
    };
    let TargetFilter::Typed(typed) = filter else {
        panic!("expected Typed filter, got {filter:?}");
    };
    assert!(
        typed.type_filters.is_empty(),
        "enchanted bridge must carry no card-type constraint; got {:?}",
        typed.type_filters
    );
    assert!(
        !typed.type_filters.contains(&TypeFilter::Creature),
        "enchanted bridge must not gate on Creature; got {:?}",
        typed.type_filters
    );
    assert!(
        typed.properties.iter().any(|p| matches!(
            p,
            FilterProp::HasAttachment {
                kind: AttachmentKind::Aura,
                controller: None,
                exclude_source: crate::types::ability::SourceExclusion::Include,
            }
        )),
        "expected HasAttachment(Aura, None, Include); got {:?}",
        typed.properties
    );
}

/// CR 603.4 + CR 301.5a/301.5c: the equipped intervening-if is source-object-wide.
/// The HasAttachment{Equipment} subtype predicate already implies a legal
/// creature host, so a redundant `creature()` card-type gate would diverge from
/// the layer evaluator (game/layers.rs). Fail-before: the bridge used
/// `TypedFilter::creature()`, so `type_filters == [Creature]` and the empty
/// assertion would fail. HasAttachment{Equipment} reasserted as a sibling guard.
#[test]
fn bridge_source_is_equipped_no_creature_type() {
    let tc = static_condition_to_trigger_condition(&StaticCondition::SourceIsEquipped)
        .expect("SourceIsEquipped should bridge to a TriggerCondition");
    let TriggerCondition::SourceMatchesFilter { filter } = tc else {
        panic!("expected SourceMatchesFilter, got {tc:?}");
    };
    let TargetFilter::Typed(typed) = filter else {
        panic!("expected Typed filter, got {filter:?}");
    };
    assert!(
        typed.type_filters.is_empty(),
        "equipped bridge must carry no card-type constraint; got {:?}",
        typed.type_filters
    );
    assert!(
        typed.properties.iter().any(|p| matches!(
            p,
            FilterProp::HasAttachment {
                kind: AttachmentKind::Equipment,
                controller: None,
                exclude_source: crate::types::ability::SourceExclusion::Include,
            }
        )),
        "expected HasAttachment(Equipment, None, Include); got {:?}",
        typed.properties
    );
}

#[test]
fn bridge_not_during_your_turn() {
    let sc = StaticCondition::Not {
        condition: Box::new(StaticCondition::DuringYourTurn),
    };
    let tc = static_condition_to_trigger_condition(&sc).unwrap();
    assert_eq!(
        tc,
        TriggerCondition::Not {
            condition: Box::new(TriggerCondition::DuringPlayersTurn {
                player: PlayerFilter::Controller,
            }),
        }
    );
}

#[test]
fn bridge_during_your_turn_maps_to_trigger() {
    assert_eq!(
        static_condition_to_trigger_condition(&StaticCondition::DuringYourTurn),
        Some(TriggerCondition::DuringPlayersTurn {
            player: PlayerFilter::Controller,
        }),
    );
}

#[test]
fn bridge_not_is_present_to_quantity_eq_zero() {
    let sc = StaticCondition::Not {
        condition: Box::new(StaticCondition::IsPresent {
            filter: Some(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                controller: Some(ControllerRef::You),
                properties: Vec::new(),
            })),
        }),
    };
    let tc = static_condition_to_trigger_condition(&sc).unwrap();
    match tc {
        TriggerCondition::QuantityComparison {
            comparator,
            rhs: QuantityExpr::Fixed { value: 0 },
            ..
        } => assert_eq!(comparator, Comparator::EQ),
        other => panic!("expected QuantityComparison EQ 0, got {other:?}"),
    }
}

#[test]
fn bridge_negated_quantity_comparison() {
    let sc = StaticCondition::Not {
        condition: Box::new(StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal {
                    player: crate::types::ability::PlayerScope::Controller,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 5 },
        }),
    };
    let tc = static_condition_to_trigger_condition(&sc).unwrap();
    match tc {
        TriggerCondition::QuantityComparison {
            comparator: Comparator::LT,
            ..
        } => {}
        other => panic!("expected negated GE→LT, got {other:?}"),
    }
}

#[test]
fn bridge_has_max_speed() {
    let tc = static_condition_to_trigger_condition(&StaticCondition::HasMaxSpeed).unwrap();
    assert_eq!(tc, TriggerCondition::HasMaxSpeed);
}

#[test]
fn bridge_class_level_ge() {
    let sc = StaticCondition::ClassLevelGE { level: 2 };
    let tc = static_condition_to_trigger_condition(&sc).unwrap();
    assert_eq!(tc, TriggerCondition::ClassLevelGE { level: 2 });
}

#[test]
fn bridge_and_recursive() {
    let sc = StaticCondition::And {
        conditions: vec![
            StaticCondition::HasMaxSpeed,
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: crate::types::ability::PlayerScope::Controller,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            },
        ],
    };
    let tc = static_condition_to_trigger_condition(&sc).unwrap();
    match tc {
        TriggerCondition::And { conditions } => assert_eq!(conditions.len(), 2),
        other => panic!("expected And, got {other:?}"),
    }
}

#[test]
fn bridge_and_with_unmappable_returns_none() {
    let sc = StaticCondition::And {
        conditions: vec![
            StaticCondition::HasMaxSpeed,
            StaticCondition::IsRingBearer, // unmappable
        ],
    };
    assert!(static_condition_to_trigger_condition(&sc).is_none());
}

#[test]
fn bridge_unmappable_variants_return_none() {
    assert!(static_condition_to_trigger_condition(&StaticCondition::IsRingBearer).is_none());
    assert!(
        static_condition_to_trigger_condition(&StaticCondition::SourceHasDealtDamage).is_none()
    );
}

#[test]
fn bridge_source_entered_this_turn() {
    // CR 400.7 + CR 603.4: "if ~ entered this turn" intervening-if (Hixus, Prison
    // Warden) bridges to the trigger-side source-entered check instead of being
    // silently dropped.
    assert_eq!(
        static_condition_to_trigger_condition(&StaticCondition::SourceEnteredThisTurn),
        Some(TriggerCondition::SourceEnteredThisTurn)
    );
}

/// Hixus, Prison Warden: "Whenever a creature deals combat damage to you, if Hixus
/// entered this turn, exile that creature until Hixus leaves the battlefield." The
/// intervening-if "if Hixus entered this turn" must be retained as the trigger
/// condition. It was previously dropped to `None` (the static→trigger bridge had no
/// arm for `SourceEnteredThisTurn`), so the exile fired on every later turn rather
/// than only the turn Hixus flashed in (CR 400.7). The condition is evaluated at
/// trigger fire-time and rechecked at resolution (CR 603.4) against
/// `GameObject.entered_battlefield_turn`.
#[test]
fn parse_hixus_keeps_entered_this_turn_intervening_if() {
    let defs = parse_trigger_lines(
        "Whenever a creature deals combat damage to you, if Hixus entered this turn, \
             exile that creature until Hixus leaves the battlefield.",
        "Hixus, Prison Warden",
    );
    let def = defs
        .iter()
        .find(|d| d.condition == Some(TriggerCondition::SourceEnteredThisTurn))
        .unwrap_or_else(|| {
            panic!(
                "expected a trigger with SourceEnteredThisTurn condition, got {:?}",
                defs.iter()
                    .map(|d| (&d.mode, &d.condition))
                    .collect::<Vec<_>>()
            )
        });
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(def.condition, Some(TriggerCondition::SourceEnteredThisTurn));
}

#[test]
fn bridge_monarch() {
    assert_eq!(
        static_condition_to_trigger_condition(&StaticCondition::IsMonarch),
        Some(TriggerCondition::IsMonarch),
    );
}

#[test]
fn bridge_opponent_is_monarch_intervening_if() {
    let sc = StaticCondition::And {
        conditions: vec![
            StaticCondition::Not {
                condition: Box::new(StaticCondition::IsMonarch),
            },
            StaticCondition::Not {
                condition: Box::new(StaticCondition::NoMonarch),
            },
        ],
    };
    assert_eq!(
        static_condition_to_trigger_condition(&sc),
        Some(TriggerCondition::And {
            conditions: vec![
                TriggerCondition::Not {
                    condition: Box::new(TriggerCondition::IsMonarch),
                },
                TriggerCondition::Not {
                    condition: Box::new(TriggerCondition::NoMonarch),
                },
            ],
        })
    );
}

#[test]
fn queen_marchesa_upkeep_attaches_opponent_monarch_intervening_if() {
    let oracle = "At the beginning of your upkeep, if an opponent is the monarch, create a 1/1 black Assassin creature token with haste.";
    let def = parse_trigger_line(oracle, "Queen Marchesa");
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::Upkeep));
    let condition = def.condition.as_ref().expect("intervening-if must parse");
    assert!(matches!(
        condition,
        TriggerCondition::And {
            conditions,
        } if conditions.len() == 2
            && matches!(
                conditions[0],
                TriggerCondition::Not {
                    condition: ref inner,
                } if matches!(inner.as_ref(), TriggerCondition::IsMonarch)
            )
            && matches!(
                conditions[1],
                TriggerCondition::Not {
                    condition: ref inner,
                } if matches!(inner.as_ref(), TriggerCondition::NoMonarch)
            )
    ));
}

#[test]
fn bridge_initiative() {
    assert_eq!(
        static_condition_to_trigger_condition(&StaticCondition::IsInitiative),
        Some(TriggerCondition::IsInitiative),
    );
}

#[test]
fn bridge_no_monarch() {
    assert_eq!(
        static_condition_to_trigger_condition(&StaticCondition::NoMonarch),
        Some(TriggerCondition::NoMonarch),
    );
}

#[test]
fn bridge_city_blessing() {
    assert_eq!(
        static_condition_to_trigger_condition(&StaticCondition::HasCityBlessing),
        Some(TriggerCondition::HasCityBlessing),
    );
}

#[test]
fn bridge_source_is_tapped() {
    assert_eq!(
        static_condition_to_trigger_condition(&StaticCondition::SourceIsTapped),
        Some(TriggerCondition::SourceIsTapped),
    );
}

#[test]
fn bridge_source_in_zone() {
    use crate::types::zones::Zone;
    assert_eq!(
        static_condition_to_trigger_condition(&StaticCondition::SourceInZone {
            zone: Zone::Graveyard,
        }),
        Some(TriggerCondition::SourceInZone {
            zone: Zone::Graveyard,
        }),
    );
}

#[test]
fn bridge_has_counters() {
    assert_eq!(
        static_condition_to_trigger_condition(&StaticCondition::HasCounters {
            counters: CounterMatch::OfType(CounterType::Time),
            minimum: 1,
            maximum: None,
        }),
        Some(TriggerCondition::HasCounters {
            counters: CounterMatch::OfType(CounterType::Time),
            minimum: 1,
            maximum: None,
        }),
    );
}

#[test]
fn trigger_intervening_if_source_is_exiled_sets_trigger_zone() {
    let def = parse_trigger_line(
        "Whenever a land you control enters, if ~ is exiled, you may put a voyage counter on it.",
        "Cosima, God of the Voyage",
    );

    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.trigger_zones, vec![Zone::Exile]);
    assert_eq!(
        def.condition,
        Some(TriggerCondition::SourceInZone { zone: Zone::Exile }),
    );
}

#[test]
fn trigger_intervening_if_this_card_is_suspended() {
    let def = parse_trigger_line(
        "Whenever you cast a spell, if this card is suspended, remove a time counter from it.",
        "17-Year Cicadas",
    );

    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(def.trigger_zones, vec![Zone::Exile]);
    match def.condition {
        Some(TriggerCondition::And { conditions }) => {
            assert!(conditions.iter().any(|condition| matches!(
                condition,
                TriggerCondition::SourceInZone { zone: Zone::Exile }
            )));
            assert!(conditions.iter().any(|condition| matches!(
                condition,
                TriggerCondition::HasCounters {
                    counters: CounterMatch::OfType(CounterType::Time),
                    minimum: 1,
                    maximum: None,
                }
            )));
        }
        other => panic!("expected suspended And condition, got {other:?}"),
    }
}

#[test]
fn trigger_intervening_if_this_card_is_in_your_graveyard_sets_trigger_zone() {
    let def = parse_trigger_line(
        "At the beginning of your upkeep, if this card is in your graveyard, you gain 1 life.",
        "Graveyard Source",
    );

    assert_eq!(def.trigger_zones, vec![Zone::Graveyard]);
    assert_eq!(
        def.condition,
        Some(TriggerCondition::SourceInZone {
            zone: Zone::Graveyard,
        }),
    );
}

// -- Nom bridge fallback integration tests --

// CR 603.4 + CR 102.1: "that player's turn" patterns bind to
// the trigger event's player (drawer / tapper / damaged player / etc.), not
// the trigger controller. Both the affirmative ("if it's") and negation
// ("if it isn't" / "if it's not") forms are parsed via the source-referential
// simple-pattern branch in `extract_if_condition`.
#[test]
fn extract_if_isnt_that_players_turn_yields_triggering_player_negation() {
    let (cleaned, cond) =
        extract_if_condition("if it isn't that player's turn, create a tapped Treasure token");
    assert_eq!(cleaned, "create a tapped Treasure token");
    assert_eq!(
        cond.unwrap(),
        TriggerCondition::Not {
            condition: Box::new(TriggerCondition::DuringPlayersTurn {
                player: PlayerFilter::TriggeringPlayer,
            }),
        }
    );
}

#[test]
fn extract_if_its_not_that_players_turn_yields_triggering_player_negation() {
    let (cleaned, cond) = extract_if_condition("if it's not that player's turn, destroy that land");
    assert_eq!(cleaned, "destroy that land");
    assert_eq!(
        cond.unwrap(),
        TriggerCondition::Not {
            condition: Box::new(TriggerCondition::DuringPlayersTurn {
                player: PlayerFilter::TriggeringPlayer,
            }),
        }
    );
}

#[test]
fn extract_if_its_that_players_turn_yields_triggering_player_affirmative() {
    let (cleaned, cond) = extract_if_condition("if it's that player's turn, you gain 1 life");
    assert_eq!(cleaned, "you gain 1 life");
    assert_eq!(
        cond.unwrap(),
        TriggerCondition::DuringPlayersTurn {
            player: PlayerFilter::TriggeringPlayer,
        }
    );
}

#[test]
fn tataru_taru_scions_secretary_attaches_triggering_player_negation() {
    // CR 603.4 + CR 102.1: "Whenever an opponent draws a card, if it isn't
    // that player's turn, create a tapped Treasure token. This ability
    // triggers only once each turn."
    let def = parse_trigger_line(
            "Whenever an opponent draws a card, if it isn't that player's turn, create a tapped Treasure token. This ability triggers only once each turn.",
            "Tataru Taru",
        );
    assert_eq!(def.mode, TriggerMode::Drawn);
    assert_eq!(
        def.condition,
        Some(TriggerCondition::Not {
            condition: Box::new(TriggerCondition::DuringPlayersTurn {
                player: PlayerFilter::TriggeringPlayer,
            }),
        }),
        "Tataru Taru must gate on it NOT being the drawing player's turn"
    );
}

#[test]
fn price_of_glory_attaches_triggering_player_negation() {
    // CR 603.4 + CR 102.1: "Whenever a player taps a land for mana, if it's
    // not that player's turn, destroy that land."
    let def = parse_trigger_line(
            "Whenever a player taps a land for mana, if it's not that player's turn, destroy that land.",
            "Price of Glory",
        );
    assert_eq!(def.mode, TriggerMode::TapsForMana);
    assert_eq!(
        def.condition,
        Some(TriggerCondition::Not {
            condition: Box::new(TriggerCondition::DuringPlayersTurn {
                player: PlayerFilter::TriggeringPlayer,
            }),
        }),
        "Price of Glory must gate on it NOT being the tapping player's turn"
    );
}

#[test]
fn breena_attaches_defending_life_intervening_if() {
    // Issue #865: attack trigger gated on defending opponent life.
    let def = parse_trigger_line(
            "Whenever a player attacks one of your opponents, if that opponent has more life than another of your opponents, that attacking player draws a card and you put two +1/+1 counters on a creature you control.",
            "Breena, the Demagogue",
        );
    assert_eq!(def.mode, TriggerMode::Attacks);
    assert_eq!(def.valid_card, None);
    assert_eq!(def.valid_source, Some(TargetFilter::Player));
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        ))
    );
    match def.condition {
        Some(TriggerCondition::QuantityComparison {
            comparator: Comparator::GT,
            ..
        }) => {}
        other => panic!(
            "Breena must gate on defending player life exceeding another opponent, got {other:?}"
        ),
    }
    let execute = def.execute.as_deref().expect("Breena must have execute");
    let Effect::Draw { target, .. } = execute.effect.as_ref() else {
        panic!(
            "Breena draw clause must lower to Draw, got {:?}",
            execute.effect
        );
    };
    assert_eq!(
        *target,
        TargetFilter::TriggeringPlayer,
        "that attacking player draws must bind to TriggeringPlayer"
    );
}

#[test]
fn ellie_brick_master_attack_token_trigger() {
    // Issue #1325: attack trigger creates Cordyceps Infected for the attacking player.
    let def = parse_trigger_line(
            "Whenever a player attacks one of your opponents, that attacking player creates a tapped 1/1 black Fungus Zombie creature token named Cordyceps Infected that's attacking that opponent.",
            "Ellie, Brick Master",
        );
    assert_eq!(def.mode, TriggerMode::Attacks);
    assert_eq!(def.valid_source, Some(TargetFilter::Player));
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        ))
    );
    let execute = def.execute.as_deref().expect("Ellie must have execute");
    let Effect::Token {
        owner,
        name,
        tapped,
        enters_attacking,
        types,
        colors,
        power,
        toughness,
        ..
    } = execute.effect.as_ref()
    else {
        panic!("Ellie must lower to Token, got {:?}", execute.effect);
    };
    assert_eq!(
        *owner,
        TargetFilter::TriggeringPlayer,
        "that attacking player creates must bind token owner to TriggeringPlayer"
    );
    assert_eq!(name, "Cordyceps Infected");
    assert!(*tapped, "Cordyceps Infected must enter tapped");
    assert!(
        *enters_attacking,
        "Cordyceps Infected must enter attacking that opponent"
    );
    assert!(
        types.iter().any(|t| t.eq_ignore_ascii_case("Fungus"))
            && types.iter().any(|t| t.eq_ignore_ascii_case("Zombie")),
        "Cordyceps Infected must be Fungus Zombie, got {types:?}"
    );
    assert!(
        colors.contains(&crate::types::mana::ManaColor::Black),
        "Cordyceps Infected must be black"
    );
    assert_eq!(power, &crate::types::ability::PtValue::Fixed(1));
    assert_eq!(toughness, &crate::types::ability::PtValue::Fixed(1));
}

#[test]
fn glademuse_attaches_not_their_turn_intervening_if() {
    // Issue #873: "their" refers to the casting player, same as "that player's".
    let def = parse_trigger_line(
        "Whenever a player casts a spell, if it's not their turn, that player draws a card.",
        "Glademuse",
    );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(
        def.condition,
        Some(TriggerCondition::Not {
            condition: Box::new(TriggerCondition::DuringPlayersTurn {
                player: PlayerFilter::TriggeringPlayer,
            }),
        }),
        "Glademuse must only trigger off-turn for the casting player"
    );
}

#[test]
fn fallback_if_you_control_a_creature() {
    // "if you control a creature" is handled by the nom bridge fallback
    let (cleaned, cond) = extract_if_condition("if you control a creature, draw a card");
    assert_eq!(cleaned, "draw a card");
    assert!(cond.is_some());
    assert!(matches!(
        cond.unwrap(),
        TriggerCondition::ControlsType { .. }
    ));
}

#[test]
fn fallback_if_hand_empty() {
    let (cleaned, cond) = extract_if_condition("if you have no cards in hand, draw a card");
    assert_eq!(cleaned, "draw a card");
    match cond.unwrap() {
        TriggerCondition::QuantityComparison {
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: 0 },
            ..
        } => {}
        other => panic!("expected QuantityComparison EQ 0, got {other:?}"),
    }
}

#[test]
fn combinator_handles_gained_life() {
    // "if you gained life this turn" routes through the nom combinator,
    // producing QuantityComparison with LifeGainedThisTurn.
    let (_, cond) = extract_if_condition("if you gained life this turn, draw a card");
    assert_eq!(
        cond.unwrap(),
        TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeGainedThisTurn {
                    player: PlayerScope::Controller,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        }
    );
}

#[test]
fn combinator_handles_played_land_or_cast_spell_from_outside_hand_this_turn() {
    let (cleaned, cond) = extract_if_condition(
            "if you've played a land or cast a spell this turn from anywhere other than your hand, ~ deals damage equal to its power to any target",
        );
    assert_eq!(cleaned, "~ deals damage equal to its power to any target");

    let TriggerCondition::Or { conditions } = cond.unwrap() else {
        panic!("expected Or trigger condition");
    };
    assert_eq!(conditions.len(), 2);
    assert!(matches!(
        &conditions[0],
        TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LandsPlayedThisTurn {
                    player: PlayerScope::Controller,
                    from_zones: Some(_),
                }
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        }
    ));

    let TriggerCondition::QuantityComparison {
        lhs:
            QuantityExpr::Ref {
                qty:
                    QuantityRef::SpellsCastThisTurn {
                        filter: Some(TargetFilter::Typed(TypedFilter { properties, .. })),
                        ..
                    },
            },
        ..
    } = &conditions[1]
    else {
        panic!(
            "expected spell-history quantity condition, got {:?}",
            conditions[1]
        );
    };
    let zones = properties.iter().find_map(|prop| match prop {
        FilterProp::InAnyZone { zones } => Some(zones),
        _ => None,
    });
    let zones = zones.expect("expected InAnyZone origin qualifier");
    assert!(!zones.contains(&crate::types::zones::Zone::Hand));
    assert!(zones.contains(&crate::types::zones::Zone::Exile));
}

#[test]
fn fallback_does_not_shadow_specific_not_your_turn() {
    let (_, cond) = extract_if_condition("if it's not your turn, draw a card");
    assert_eq!(
        cond.unwrap(),
        TriggerCondition::Not {
            condition: Box::new(TriggerCondition::DuringPlayersTurn {
                player: PlayerFilter::Controller,
            }),
        }
    );
}

#[test]
fn combinator_handles_controls_count() {
    // "if you control 3 or more creatures" routes through the nom combinator,
    // producing QuantityComparison with ObjectCount.
    let (_, cond) = extract_if_condition("if you control three or more creatures, draw a card");
    assert!(
        matches!(
            cond.unwrap(),
            TriggerCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount { .. },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            }
        ),
        "Expected QuantityComparison with ObjectCount >= 3"
    );
}

#[test]
fn combinator_handles_life_total() {
    // "if you have 5 or more life" routes through the nom combinator,
    // producing QuantityComparison with LifeTotal.
    let (_, cond) = extract_if_condition("if you have five or more life, draw a card");
    assert_eq!(
        cond.unwrap(),
        TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal {
                    player: crate::types::ability::PlayerScope::Controller
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 5 },
        }
    );
}

// -- Source-referential condition extraction tests --

#[test]
fn extract_tribute_not_paid() {
    let (cleaned, cond) =
        extract_if_condition("put two +1/+1 counters on it if tribute wasn't paid");
    assert_eq!(cleaned, "put two +1/+1 counters on it");
    assert_eq!(cond.unwrap(), TriggerCondition::TributeNotPaid);
}

#[test]
fn extract_addendum_main_phase() {
    let (cleaned, cond) =
        extract_if_condition("draw a card if you cast this spell during your main phase");
    assert_eq!(cleaned, "draw a card");
    assert_eq!(
        cond.unwrap(),
        TriggerCondition::CastDuringPhase {
            phases: vec![Phase::PreCombatMain, Phase::PostCombatMain],
        }
    );
}

#[test]
fn extract_adamant_three_red() {
    let (cleaned, cond) = extract_if_condition(
        "it deals 4 damage instead if at least three red mana was spent to cast this spell",
    );
    assert_eq!(cleaned, "it deals 4 damage instead");
    assert_eq!(
        cond.unwrap(),
        TriggerCondition::ManaColorSpent {
            color: crate::types::mana::ManaColor::Red,
            minimum: 3,
        }
    );
}

// CR 400.7d + CR 601.2h: Incarnation / hybrid-ETB cycle — symbolic-form
// spent-mana condition "if {C}{C} was spent to cast it".
#[test]
fn extract_symbolic_mana_spent_two_green() {
    let (cleaned, cond) = extract_if_condition(
        "if {G}{G} was spent to cast it, exile target artifact or enchantment an opponent controls",
    );
    assert_eq!(
        cleaned,
        "exile target artifact or enchantment an opponent controls"
    );
    assert_eq!(
        cond.unwrap(),
        TriggerCondition::ManaColorSpent {
            color: crate::types::mana::ManaColor::Green,
            minimum: 2,
        }
    );
}

#[test]
fn extract_symbolic_mana_spent_two_blue_with_trailing_effect() {
    let (cleaned, cond) =
        extract_if_condition("if {U}{U} was spent to cast it, draw two cards, then discard a card");
    assert_eq!(cleaned, "draw two cards, then discard a card");
    assert_eq!(
        cond.unwrap(),
        TriggerCondition::ManaColorSpent {
            color: crate::types::mana::ManaColor::Blue,
            minimum: 2,
        }
    );
}

#[test]
fn extract_symbolic_mana_spent_single_red_this_spell() {
    let (cleaned, cond) = extract_if_condition("draw a card if {R} was spent to cast this spell");
    assert_eq!(cleaned, "draw a card");
    assert_eq!(
        cond.unwrap(),
        TriggerCondition::ManaColorSpent {
            color: crate::types::mana::ManaColor::Red,
            minimum: 1,
        }
    );
}

#[test]
fn extract_symbolic_unless_mana_spent_single_blue() {
    let (cleaned, cond) = extract_if_condition("sacrifice it unless {U} was spent to cast it");
    assert_eq!(cleaned, "sacrifice it");
    assert_eq!(
        cond.unwrap(),
        TriggerCondition::Not {
            condition: Box::new(TriggerCondition::ManaColorSpent {
                color: crate::types::mana::ManaColor::Blue,
                minimum: 1,
            }),
        }
    );
}

#[test]
fn extract_symbolic_unless_mana_spent_two_black() {
    let (cleaned, cond) = extract_if_condition("sacrifice it unless {B}{B} was spent to cast it");
    assert_eq!(cleaned, "sacrifice it");
    assert_eq!(
        cond.unwrap(),
        TriggerCondition::Not {
            condition: Box::new(TriggerCondition::ManaColorSpent {
                color: crate::types::mana::ManaColor::Black,
                minimum: 2,
            }),
        }
    );
}

#[test]
fn extract_no_mana_spent_condition() {
    let (cleaned, cond) =
        extract_if_condition("if no mana was spent to cast it, counter that spell");
    assert_eq!(cleaned, "counter that spell");
    assert_eq!(
        cond,
        Some(TriggerCondition::ManaSpentCondition {
            text: "no mana was spent to cast it".to_string(),
        })
    );
}

#[test]
fn extract_mana_spent_comparison_condition_less_than() {
    let (cleaned, cond) = extract_if_condition(
            "if the amount of mana spent to cast it was less than its mana value, ~ deal 3 damage to that player",
        );
    assert_eq!(cleaned, "~ deal 3 damage to that player");
    assert_eq!(
        cond,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope: crate::types::ability::CastManaObjectScope::TriggeringSpell,
                    metric: crate::types::ability::CastManaSpentMetric::Total,
                },
            },
            comparator: Comparator::LT,
            rhs: QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::EventSource,
                },
            },
        })
    );
}

#[test]
fn extract_mana_spent_comparison_condition_greater_than() {
    let (cleaned, cond) = extract_if_condition(
            "if the amount of mana spent to cast that spell was greater than its mana value, put a +1/+1 counter on ~",
        );
    assert_eq!(cleaned, "put a +1/+1 counter on ~");
    assert_eq!(
        cond,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope: crate::types::ability::CastManaObjectScope::TriggeringSpell,
                    metric: crate::types::ability::CastManaSpentMetric::Total,
                },
            },
            comparator: Comparator::GT,
            rhs: QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::EventSource,
                },
            },
        })
    );
}

// The extractor uses `scan_split_at_phrase`, so the clause doesn't have to
// be at the start of the text. Covers the same positional flexibility the
// word-form Adamant extractor already relies on.
#[test]
fn extract_symbolic_mana_spent_mid_sentence() {
    let (cleaned, cond) =
        extract_if_condition("it deals 4 damage instead if {R}{R}{R} was spent to cast this spell");
    assert_eq!(cleaned, "it deals 4 damage instead");
    assert_eq!(
        cond.unwrap(),
        TriggerCondition::ManaColorSpent {
            color: crate::types::mana::ManaColor::Red,
            minimum: 3,
        }
    );
}

// Production path pre-lowercases effect text; verify the extractor matches
// lowercase `{g}{g}` equivalently to uppercase. This is the case actually
// exercised by Wistfulness/Vibrance/Deceit at card-data-export time.
#[test]
fn extract_symbolic_mana_spent_lowercase_input() {
    let (cleaned, cond) = extract_if_condition(
        "if {g}{g} was spent to cast it, exile target artifact or enchantment an opponent controls",
    );
    assert_eq!(
        cleaned,
        "exile target artifact or enchantment an opponent controls"
    );
    assert_eq!(
        cond.unwrap(),
        TriggerCondition::ManaColorSpent {
            color: crate::types::mana::ManaColor::Green,
            minimum: 2,
        }
    );
}

#[test]
fn extract_symbolic_mana_spent_mixed_colors() {
    let (cleaned, cond) = extract_if_condition("do something if {G}{U} was spent to cast it");
    assert_eq!(cleaned, "do something");
    let TriggerCondition::And { conditions } =
        cond.expect("mixed color run should produce an And condition")
    else {
        panic!("expected And condition");
    };
    assert_eq!(
        conditions,
        vec![
            TriggerCondition::ManaColorSpent {
                color: crate::types::mana::ManaColor::Green,
                minimum: 1,
            },
            TriggerCondition::ManaColorSpent {
                color: crate::types::mana::ManaColor::Blue,
                minimum: 1,
            },
        ]
    );
}

// Hybrid pips should not match — CR 601.2h tracks the color actually paid,
// not the hybrid pip symbol itself.
#[test]
fn extract_symbolic_mana_spent_rejects_hybrid() {
    let (_cleaned, cond) = extract_if_condition("do something if {G/U}{G/U} was spent to cast it");
    assert!(cond.is_none(), "hybrid pips should not match");
}

#[test]
fn extract_had_counter_typed() {
    let (cleaned, cond) =
        extract_if_condition("return it to the battlefield if it had a +1/+1 counter on it");
    assert_eq!(cleaned, "return it to the battlefield");
    assert_eq!(
        cond.unwrap(),
        TriggerCondition::HadCounters {
            counter_type: Some(crate::types::counter::CounterType::Plus1Plus1),
        }
    );
}

/// CR 400.7 + CR 603.4 + issue #1498: the negated untyped form
/// ("if it had no counters on it") — Unstoppable Slasher's dies-return gate.
#[test]
fn extract_had_no_counters_negates() {
    let (cleaned, cond) =
        extract_if_condition("return it to the battlefield if it had no counters on it");
    assert_eq!(cleaned, "return it to the battlefield");
    assert_eq!(
        cond.unwrap(),
        TriggerCondition::Not {
            condition: Box::new(TriggerCondition::HadCounters { counter_type: None }),
        }
    );
}

/// CR 400.7: the negated typed form composes the negation and type axes.
#[test]
fn extract_had_no_typed_counters_negates() {
    let (cleaned, cond) = extract_if_condition("draw a card if it had no +1/+1 counters on it");
    assert_eq!(cleaned, "draw a card");
    assert_eq!(
        cond.unwrap(),
        TriggerCondition::Not {
            condition: Box::new(TriggerCondition::HadCounters {
                counter_type: Some(crate::types::counter::CounterType::Plus1Plus1),
            }),
        }
    );
}

/// CR 702.188a + CR 603.4 (issue: Spiders-Man, Heroic Horde): the ETB
/// intervening-if "if they were cast using web-slinging" must gate the
/// life-gain + token effects. Before the fix `extract_if_condition` had no
/// arm for "cast using <variant>", so the clause was dropped and the
/// trigger fired unconditionally. Drives the full lowering pipeline
/// (`parse_trigger_line` → `lower_trigger_ir`) and asserts the composed
/// `def.condition` carries the web-slinging gate — this assertion flips to
/// `None` if the parser arm is reverted.
#[test]
fn extract_cast_using_web_slinging_plural_pronoun() {
    let (cleaned, cond) = extract_if_condition(
            "if they were cast using web-slinging, you gain 3 life and create two 2/1 green Spider creature tokens with reach",
        );
    assert_eq!(
        cleaned,
        "you gain 3 life and create two 2/1 green Spider creature tokens with reach"
    );
    assert_eq!(
        cond.unwrap(),
        TriggerCondition::CastVariantPaidPersistent {
            variant: CastVariantPaid::WebSlinging,
        }
    );
}

/// Singular-pronoun variant of the same class — "if it was cast using
/// web-slinging" — covers single-permanent web-slinging cards.
#[test]
fn extract_cast_using_web_slinging_singular_pronoun() {
    let (_cleaned, cond) = extract_if_condition("if it was cast using web-slinging, draw a card");
    assert_eq!(
        cond.unwrap(),
        TriggerCondition::CastVariantPaidPersistent {
            variant: CastVariantPaid::WebSlinging,
        }
    );
}

/// End-to-end: the full Spiders-Man ETB trigger lowers with the
/// web-slinging intervening-if as its `def.condition` (NOT `None`).
/// This is the discriminating assertion: revert the parser arm and
/// `def.condition` becomes `None`, so the gated effects would fire
/// unconditionally.
#[test]
fn spiders_man_etb_carries_web_slinging_condition() {
    let def = parse_trigger_line(
            "When Spiders-Man enters, if they were cast using web-slinging, you gain 3 life and create two 2/1 green Spider creature tokens with reach.",
            "Spiders-Man, Heroic Horde",
        );
    assert_eq!(
        def.condition,
        Some(TriggerCondition::CastVariantPaidPersistent {
            variant: CastVariantPaid::WebSlinging,
        }),
        "Spiders-Man ETB must be gated on web-slinging, not fire unconditionally"
    );
}

/// Control: an ETB trigger with no cast-variant clause must remain
/// unconditional (`def.condition == None`) — guards against the new arm
/// over-matching.
#[test]
fn plain_etb_has_no_cast_variant_condition() {
    let def = parse_trigger_line("When Test Card enters, draw a card.", "Test Card");
    assert_eq!(def.condition, None);
}

#[test]
fn extract_if_it_wasnt_blocking_as_zone_change_lookback() {
    let (cleaned, cond) = extract_if_condition("if it wasn't blocking, draw a card");
    assert_eq!(cleaned, "draw a card");
    assert_eq!(
        cond.unwrap(),
        TriggerCondition::Not {
            condition: Box::new(TriggerCondition::ZoneChangeObjectMatchesFilter {
                origin: Some(Zone::Battlefield),
                destination: Zone::Graveyard,
                filter: TargetFilter::Typed(
                    TypedFilter::creature().properties(vec![FilterProp::Blocking])
                ),
            }),
        }
    );
}

/// CR 506.5: the disjunctive "attacking or blocking alone" intervening-if
/// (Thijarian Witness "Bear Witness") becomes a zone-change look-back over
/// an `AnyOf([AttackingAlone, BlockingAlone])` creature filter, and the
/// clause is stripped from the residual effect text.
#[test]
fn extract_if_it_was_attacking_or_blocking_alone_as_zone_change_lookback() {
    let (cleaned, cond) =
        extract_if_condition("if it was attacking or blocking alone, exile it and investigate");
    assert_eq!(cleaned, "exile it and investigate");
    assert_eq!(
        cond.unwrap(),
        TriggerCondition::ZoneChangeObjectMatchesFilter {
            origin: Some(Zone::Battlefield),
            destination: Zone::Graveyard,
            filter: TargetFilter::Typed(TypedFilter::creature().properties(vec![
                FilterProp::AnyOf {
                    props: vec![FilterProp::AttackingAlone, FilterProp::BlockingAlone],
                },
            ])),
        }
    );
}

/// CR 506.5: the single-phrase "attacking alone" form (building-block
/// coverage — the class, not just the disjunctive card text).
#[test]
fn extract_if_it_was_attacking_alone_as_zone_change_lookback() {
    let (cleaned, cond) = extract_if_condition("if it was attacking alone, draw a card");
    assert_eq!(cleaned, "draw a card");
    assert_eq!(
        cond.unwrap(),
        TriggerCondition::ZoneChangeObjectMatchesFilter {
            origin: Some(Zone::Battlefield),
            destination: Zone::Graveyard,
            filter: TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::AttackingAlone])
            ),
        }
    );
}

/// CR 506.5: the single-phrase "blocking alone" form.
#[test]
fn extract_if_it_was_blocking_alone_as_zone_change_lookback() {
    let (cleaned, cond) = extract_if_condition("if it was blocking alone, draw a card");
    assert_eq!(cleaned, "draw a card");
    assert_eq!(
        cond.unwrap(),
        TriggerCondition::ZoneChangeObjectMatchesFilter {
            origin: Some(Zone::Battlefield),
            destination: Zone::Graveyard,
            filter: TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::BlockingAlone])
            ),
        }
    );
}

/// CR 506.5 + CR 603.4: the negated polarity composes through the existing
/// negation axis ("if it wasn't attacking alone" → `Not(...)`).
#[test]
fn extract_if_it_wasnt_attacking_alone_negates_zone_change_lookback() {
    let (cleaned, cond) = extract_if_condition("if it wasn't attacking alone, draw a card");
    assert_eq!(cleaned, "draw a card");
    assert_eq!(
        cond.unwrap(),
        TriggerCondition::Not {
            condition: Box::new(TriggerCondition::ZoneChangeObjectMatchesFilter {
                origin: Some(Zone::Battlefield),
                destination: Zone::Graveyard,
                filter: TargetFilter::Typed(
                    TypedFilter::creature().properties(vec![FilterProp::AttackingAlone])
                ),
            }),
        }
    );
}

#[test]
fn extract_if_it_was_enchanted_or_equipped_as_zone_change_lookback() {
    let (cleaned, cond) =
        extract_if_condition("if it was enchanted or equipped, return it to its owner's hand");
    assert_eq!(cleaned, "return it to its owner's hand");
    assert_eq!(
        cond.unwrap(),
        TriggerCondition::ZoneChangeObjectMatchesFilter {
            origin: Some(Zone::Battlefield),
            destination: Zone::Graveyard,
            filter: TargetFilter::Typed(TypedFilter::creature().properties(vec![
                FilterProp::HasAnyAttachmentOf {
                    kinds: vec![AttachmentKind::Aura, AttachmentKind::Equipment],
                    controller: None,
                },
            ])),
        }
    );
}

#[test]
fn extract_had_counters_untyped() {
    let (cleaned, cond) = extract_if_condition("draw a card if it had counters on it");
    assert_eq!(cleaned, "draw a card");
    assert_eq!(
        cond.unwrap(),
        TriggerCondition::HadCounters { counter_type: None },
    );
}

/// CR 603.4 + CR 608.2h: "you're the monarch" is a re-homeable post-effect
/// `if` — `extract_if_condition` leaves it for `strip_suffix_conditional`
/// to attach as a clause-level `AbilityCondition` rather than hoisting it.
#[test]
fn bridge_monarch_from_trigger_text() {
    let (cleaned, cond) = extract_if_condition("draw a card if you're the monarch");
    assert_eq!(
        cond, None,
        "a re-homeable post-effect `if` must not hoist to the trigger condition",
    );
    assert_eq!(cleaned, "draw a card if you're the monarch");
}

/// CR 603.4 + CR 608.2h: "this land is tapped" is a re-homeable post-effect
/// `if` — left intact by `extract_if_condition` for downstream re-homing.
#[test]
fn bridge_source_tapped_from_trigger_text() {
    let (cleaned, cond) =
        extract_if_condition("put a storage counter on it if this land is tapped");
    assert_eq!(
        cond, None,
        "a re-homeable post-effect `if` must not hoist to the trigger condition",
    );
    assert_eq!(
        cleaned,
        "put a storage counter on it if this land is tapped"
    );
}

#[test]
fn cast_trigger_lowers_to_control_next_turn_effect() {
    let def = parse_trigger_line(
            "When you cast this spell, you gain control of target opponent during that player's next turn. After that turn, that player takes an extra turn.",
            "Emrakul, the Promised End",
        );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    let execute = def.execute.expect("expected execute ability");
    match execute.effect.as_ref() {
        Effect::ControlNextTurn {
            target,
            grant_extra_turn_after,
            window: _,
        } => {
            assert!(*grant_extra_turn_after);
            assert_eq!(
                target,
                &TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
            );
        }
        other => panic!("expected ControlNextTurn effect, got {other:?}"),
    }
}

#[test]
fn state_trigger_control_no_islands() {
    let def = parse_trigger_line(
        "When you control no Islands, sacrifice this creature.",
        "Dandân",
    );
    assert_eq!(def.mode, TriggerMode::StateCondition);
    if let Some(TriggerCondition::ControlsNone { filter }) = &def.condition {
        if let TargetFilter::Typed(tf) = filter {
            assert!(
                tf.type_filters
                    .iter()
                    .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "Island")),
                "expected Island subtype in {:?}",
                tf.type_filters,
            );
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
    } else {
        panic!("expected ControlsNone condition, got {:?}", def.condition,);
    }
    // Effect should be sacrifice self
    let execute = def.execute.as_ref().expect("should have execute");
    assert!(
        matches!(*execute.effect, Effect::Sacrifice { .. }),
        "expected Sacrifice, got {:?}",
        execute.effect,
    );
}

#[test]
fn state_trigger_control_no_other_creatures() {
    let def = parse_trigger_line(
        "When you control no other creatures, sacrifice this creature.",
        "Emperor Crocodile",
    );
    assert_eq!(def.mode, TriggerMode::StateCondition);
    if let Some(TriggerCondition::ControlsNone { filter }) = &def.condition {
        if let TargetFilter::Typed(tf) = filter {
            assert!(tf.properties.contains(&FilterProp::Another));
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert_eq!(tf.controller, Some(ControllerRef::You));
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
    } else {
        panic!("expected ControlsNone condition, got {:?}", def.condition);
    }
}

#[test]
fn state_trigger_control_no_artifacts() {
    let def = parse_trigger_line(
        "When you control no artifacts, sacrifice this creature.",
        "Covetous Dragon",
    );
    assert_eq!(def.mode, TriggerMode::StateCondition);
    if let Some(TriggerCondition::ControlsNone { filter }) = &def.condition {
        if let TargetFilter::Typed(tf) = filter {
            assert!(tf.type_filters.contains(&TypeFilter::Artifact));
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
    } else {
        panic!("expected ControlsNone condition, got {:?}", def.condition);
    }
}

#[test]
fn state_trigger_control_a_creature_with_toughness() {
    // CR 603.8: Endangered Armodon — the positive-existence sibling of the
    // "control no [type]" state triggers above. Discriminating shape check:
    // reverting the new parser arm yields no StateCondition/ControlsType
    // trigger (the text falls through to Unimplemented), failing every
    // assertion here.
    let def = parse_trigger_line(
        "When you control a creature with toughness 2 or less, sacrifice this creature.",
        "Endangered Armodon",
    );
    assert_eq!(def.mode, TriggerMode::StateCondition);
    if let Some(TriggerCondition::ControlsType { filter }) = &def.condition {
        if let TargetFilter::Typed(tf) = filter {
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "expected Creature type filter in {:?}",
                tf.type_filters,
            );
            assert!(
                tf.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::PtComparison {
                        stat: PtStat::Toughness,
                        comparator: Comparator::LE,
                        ..
                    }
                )),
                "expected `toughness N or less` PtComparison in {:?}",
                tf.properties,
            );
            assert_eq!(tf.controller, Some(ControllerRef::You));
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
    } else {
        panic!("expected ControlsType condition, got {:?}", def.condition);
    }
    // Effect should be sacrifice self.
    let execute = def.execute.as_ref().expect("should have execute");
    assert!(
        matches!(*execute.effect, Effect::Sacrifice { .. }),
        "expected Sacrifice, got {:?}",
        execute.effect,
    );
}

/// CR 603.8: Endangered Armodon's state trigger fires at runtime when its
/// controller controls a creature with toughness 2 or less, sacrificing
/// Endangered Armodon. Drives the real `check_state_triggers` →
/// `GameRunner` resolution path (not just the parsed AST): the
/// `ControlsType` condition is evaluated against the live battlefield and
/// the `Sacrifice` effect resolves. Discriminates against reverting the new
/// parser arm — without it the text produces no StateCondition trigger, so
/// the pre-assertion fails and Endangered Armodon is never sacrificed.
#[test]
fn endangered_armodon_state_trigger_fires_and_sacrifices_self() {
    use crate::game::scenario::GameRunner;
    use crate::game::triggers::check_state_triggers;
    use crate::game::zones::create_object;
    use crate::parser::oracle::parse_oracle_text;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::phase::Phase;
    use crate::types::zones::Zone;
    use crate::types::PlayerId;
    use std::sync::Arc;

    const ORACLE: &str =
        "When you control a creature with toughness 2 or less, sacrifice this creature.";

    let parsed = parse_oracle_text(
        ORACLE,
        "Endangered Armodon",
        &[],
        &["Creature".to_string()],
        &[],
    );

    // Confirm the state trigger was parsed — shape check before the runtime test.
    assert!(
        parsed
            .triggers
            .iter()
            .any(|t| t.mode == TriggerMode::StateCondition),
        "Endangered Armodon must parse a StateCondition trigger; got {:?}",
        parsed.triggers,
    );

    let mut state = crate::types::game_state::GameState::new_two_player(7);
    state.phase = Phase::PreCombatMain;
    state.turn_number = 2;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    // Endangered Armodon (3/3) — its own toughness (3) does NOT satisfy the
    // "toughness 2 or less" filter, so the trigger fires only because of the
    // separate small creature below, not self-reference.
    let armodon_id = create_object(
        &mut state,
        CardId(7),
        PlayerId(0),
        "Endangered Armodon".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&armodon_id).unwrap();
        obj.controller = PlayerId(0);
        obj.power = Some(3);
        obj.toughness = Some(3);
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types.core_types.push(CoreType::Creature);
        obj.base_trigger_definitions = Arc::new(parsed.triggers.clone());
        obj.trigger_definitions = parsed.triggers.clone().into();
    }

    // A 1/1 creature under the same controller satisfies "a creature with
    // toughness 2 or less", making the state condition true.
    let token_id = create_object(
        &mut state,
        CardId(8),
        PlayerId(0),
        "Goblin".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&token_id).unwrap();
        obj.controller = PlayerId(0);
        obj.power = Some(1);
        obj.toughness = Some(1);
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types.core_types.push(CoreType::Creature);
    }

    // CR 603.8: the state trigger detects the small creature and enqueues the
    // sacrifice.
    check_state_triggers(&mut state);
    assert!(
        state.pending_trigger.is_some() || !state.stack.is_empty(),
        "state trigger must be pending or on the stack after check_state_triggers",
    );

    // Resolve the sacrifice and confirm Endangered Armodon left the battlefield.
    let mut runner = GameRunner::from_state(state);
    runner.advance_until_stack_empty();
    let state = runner.state();
    assert!(
        !state.battlefield.iter().any(|id| *id == armodon_id),
        "Endangered Armodon must be sacrificed once a toughness-≤2 creature is controlled",
    );
    assert!(
        state.battlefield.iter().any(|id| *id == token_id),
        "the toughness-≤2 creature is unaffected by the sacrifice",
    );
}

#[test]
fn state_trigger_has_no_ice_counters() {
    // Dark Depths: "When Dark Depths has no ice counters on it, sacrifice it."
    let def = parse_trigger_line(
        "When Dark Depths has no ice counters on it, sacrifice it.",
        "Dark Depths",
    );
    assert_eq!(def.mode, TriggerMode::StateCondition);
    if let Some(TriggerCondition::HasCounters {
        counters,
        minimum,
        maximum,
    }) = &def.condition
    {
        assert_eq!(
            *counters,
            CounterMatch::OfType(CounterType::Generic("ice".to_string()))
        );
        assert_eq!(*minimum, 0);
        assert_eq!(*maximum, Some(0));
    } else {
        panic!("expected HasCounters condition, got {:?}", def.condition);
    }
    let execute = def.execute.as_ref().expect("should have execute");
    assert!(
        matches!(*execute.effect, Effect::Sacrifice { .. }),
        "expected Sacrifice, got {:?}",
        execute.effect,
    );
}

#[test]
fn state_trigger_has_no_plus1_counters() {
    // Afiya Grove: "When Afiya Grove has no +1/+1 counters on it, sacrifice it."
    let def = parse_trigger_line(
        "When Afiya Grove has no +1/+1 counters on it, sacrifice it.",
        "Afiya Grove",
    );
    assert_eq!(def.mode, TriggerMode::StateCondition);
    if let Some(TriggerCondition::HasCounters {
        counters,
        minimum,
        maximum,
    }) = &def.condition
    {
        assert_eq!(*counters, CounterMatch::OfType(CounterType::Plus1Plus1));
        assert_eq!(*minimum, 0);
        assert_eq!(*maximum, Some(0));
    } else {
        panic!("expected HasCounters condition, got {:?}", def.condition);
    }
}

#[test]
fn state_trigger_has_no_counters_bare() {
    // Hypothetical: "When ~ has no counters on it, sacrifice it."
    let def = parse_trigger_line(
        "When TestCard has no counters on it, sacrifice it.",
        "TestCard",
    );
    assert_eq!(def.mode, TriggerMode::StateCondition);
    if let Some(TriggerCondition::HasCounters {
        counters,
        minimum,
        maximum,
    }) = &def.condition
    {
        assert_eq!(*counters, CounterMatch::Any);
        assert_eq!(*minimum, 0);
        assert_eq!(*maximum, Some(0));
    } else {
        panic!("expected HasCounters condition, got {:?}", def.condition);
    }
}

#[test]
fn state_trigger_has_twenty_or_more_charge_counters() {
    // Darksteel Reactor: "When Darksteel Reactor has twenty or more charge counters on it,
    // you win the game."
    let def = parse_trigger_line(
        "When Darksteel Reactor has twenty or more charge counters on it, you win the game.",
        "Darksteel Reactor",
    );
    assert_eq!(def.mode, TriggerMode::StateCondition);
    if let Some(TriggerCondition::HasCounters {
        counters,
        minimum,
        maximum,
    }) = &def.condition
    {
        assert_eq!(
            *counters,
            CounterMatch::OfType(CounterType::Generic("charge".to_string()))
        );
        assert_eq!(*minimum, 20);
        assert_eq!(*maximum, None);
    } else {
        panic!("expected HasCounters condition, got {:?}", def.condition);
    }
    let execute = def.execute.as_ref().expect("should have execute");
    assert!(
        matches!(*execute.effect, Effect::WinTheGame { .. }),
        "expected WinTheGame, got {:?}",
        execute.effect,
    );
}

#[test]
fn upkeep_trigger_may_put_charge_counter() {
    // Darksteel Reactor: "At the beginning of your upkeep, you may put a charge counter
    // on Darksteel Reactor."
    let def = parse_trigger_line(
        "At the beginning of your upkeep, you may put a charge counter on Darksteel Reactor.",
        "Darksteel Reactor",
    );
    assert_eq!(def.phase, Some(Phase::Upkeep));
    assert!(def.optional, "upkeep trigger should be optional (you may)");
    let execute = def.execute.as_ref().expect("should have execute");
    assert!(
        matches!(
            *execute.effect,
            Effect::PutCounter {
                counter_type: CounterType::Generic(ref s),
                target: TargetFilter::SelfRef,
                ..
            } if s == "charge"
        ),
        "expected PutCounter(charge) on SelfRef, got {:?}",
        execute.effect,
    );
}

/// CR 603.8: Darksteel Reactor's state trigger fires when the reactor reaches
/// 20 charge counters and resolves the WinTheGame effect, ending the game.
/// Discriminates against guard-reversion regressions: reverting the
/// `minimum: 1..` arm causes the pre-assertion (StateCondition trigger exists)
/// to fail because the oracle text no longer produces a StateCondition trigger.
#[test]
fn darksteel_reactor_state_trigger_fires_and_wins_game_at_twenty_counters() {
    use crate::game::scenario::GameRunner;
    use crate::game::triggers::check_state_triggers;
    use crate::game::zones::create_object;
    use crate::parser::oracle::parse_oracle_text;
    use crate::types::game_state::WaitingFor;
    use crate::types::identifiers::CardId;
    use crate::types::phase::Phase;
    use crate::types::zones::Zone;
    use crate::types::PlayerId;
    use std::sync::Arc;

    const ORACLE: &str = "Indestructible\n\
            At the beginning of your upkeep, you may put a charge counter on Darksteel Reactor.\n\
            When Darksteel Reactor has twenty or more charge counters on it, you win the game.";

    let parsed = parse_oracle_text(
        ORACLE,
        "Darksteel Reactor",
        &[],
        &["Artifact".to_string()],
        &[],
    );

    // Confirm the state trigger was parsed — shape check before the runtime test.
    assert!(
        parsed
            .triggers
            .iter()
            .any(|t| t.mode == TriggerMode::StateCondition),
        "Darksteel Reactor must parse a StateCondition trigger; got {:?}",
        parsed.triggers,
    );

    let mut state = crate::types::game_state::GameState::new_two_player(42);
    state.phase = Phase::PreCombatMain;
    state.turn_number = 2;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    // Place Darksteel Reactor on the battlefield for player 0.
    let reactor_id = create_object(
        &mut state,
        CardId(42),
        PlayerId(0),
        "Darksteel Reactor".to_string(),
        Zone::Battlefield,
    );

    // Install the parsed trigger definitions and add 20 charge counters.
    {
        let obj = state.objects.get_mut(&reactor_id).unwrap();
        obj.base_trigger_definitions = Arc::new(parsed.triggers.clone());
        obj.trigger_definitions = parsed.triggers.clone().into();
        obj.controller = PlayerId(0);
        obj.counters
            .insert(CounterType::Generic("charge".to_string()), 20);
    }

    // CR 603.8: call check_state_triggers, which sees the reactor has 20 charge
    // counters and enqueues the WinTheGame trigger.
    check_state_triggers(&mut state);

    assert!(
        state.pending_trigger.is_some() || !state.stack.is_empty(),
        "state trigger must be pending or on the stack after check_state_triggers",
    );

    // Drain the stack: the WinTheGame effect resolves, eliminating all opponents.
    let mut runner = GameRunner::from_state(state);
    runner.advance_until_stack_empty();
    let state = runner.state();

    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(0))
            }
        ),
        "game must end with player 0 as winner after Darksteel Reactor fires; got {:?}",
        state.waiting_for,
    );
}

// --- Compound trigger tests ---

#[test]
fn compound_and_when_cycle_and_dies() {
    // Jund Sojourners: "When you cycle ~ and when ~ dies, you may have it deal 1 damage to any target."
    let triggers = parse_trigger_lines(
            "When you cycle this card and when this creature dies, you may have it deal 1 damage to any target.",
            "Jund Sojourners",
        );
    assert_eq!(triggers.len(), 2);
    assert_eq!(triggers[0].mode, TriggerMode::Cycled);
    assert_eq!(triggers[1].mode, TriggerMode::ChangesZone);
    assert_eq!(triggers[1].origin, Some(Zone::Battlefield));
    assert_eq!(triggers[1].destination, Some(Zone::Graveyard));
    // Both should have the same execute effect
    assert!(triggers[0].execute.is_some());
    assert!(triggers[1].execute.is_some());
}

#[test]
fn compound_and_when_enters_and_sacrifice() {
    // Heaped Harvest: "When this artifact enters and when you sacrifice it, ..."
    let triggers = parse_trigger_lines(
            "When this artifact enters and when you sacrifice it, you may search your library for a basic land card, put it onto the battlefield tapped, then shuffle.",
            "Heaped Harvest",
        );
    assert_eq!(triggers.len(), 2);
    assert_eq!(triggers[0].mode, TriggerMode::ChangesZone);
    assert_eq!(triggers[0].destination, Some(Zone::Battlefield));
    assert_eq!(triggers[1].mode, TriggerMode::Sacrificed);
}

#[test]
fn compound_or_enters_or_deals_combat_damage() {
    // Aerial Extortionist: "Whenever this creature enters or deals combat damage to a player, ..."
    let triggers = parse_trigger_lines(
            "Whenever this creature enters or deals combat damage to a player, exile up to one target nonland permanent.",
            "Aerial Extortionist",
        );
    assert_eq!(triggers.len(), 2);
    assert_eq!(triggers[0].mode, TriggerMode::ChangesZone);
    assert_eq!(triggers[0].destination, Some(Zone::Battlefield));
    assert_eq!(triggers[1].mode, TriggerMode::DamageDone);
    assert_eq!(triggers[1].damage_kind, DamageKindFilter::CombatOnly);
}

#[test]
fn compound_or_deals_combat_damage_or_dies() {
    // Park Heights Maverick: "Whenever this creature deals combat damage to a player or dies, proliferate."
    let triggers = parse_trigger_lines(
        "Whenever this creature deals combat damage to a player or dies, proliferate.",
        "Park Heights Maverick",
    );
    assert_eq!(triggers.len(), 2);
    assert_eq!(triggers[0].mode, TriggerMode::DamageDone);
    assert_eq!(triggers[0].damage_kind, DamageKindFilter::CombatOnly);
    assert_eq!(triggers[1].mode, TriggerMode::ChangesZone);
    assert_eq!(triggers[1].origin, Some(Zone::Battlefield));
    assert_eq!(triggers[1].destination, Some(Zone::Graveyard));
}

#[test]
fn compound_and_whenever_enters_and_cast_spell() {
    // Salacinder and Soot: "When ~ enters and whenever you cast an Elemental spell, ..."
    let triggers = parse_trigger_lines(
        "When Salacinder and Soot enters and whenever you cast an Elemental spell, choose one —",
        "Salacinder and Soot",
    );
    assert_eq!(triggers.len(), 2);
    assert_eq!(triggers[0].mode, TriggerMode::ChangesZone);
    assert_eq!(triggers[1].mode, TriggerMode::SpellCast);
}

#[test]
fn compound_or_create_or_sacrifice_token() {
    // CR 701.7 + CR 701.21: Mirkwood Bats — "Whenever you create or sacrifice a token"
    let triggers = parse_trigger_lines(
        "Whenever you create or sacrifice a token, each opponent loses 1 life.",
        "Mirkwood Bats",
    );
    assert_eq!(triggers.len(), 2);
    assert_eq!(triggers[0].mode, TriggerMode::TokenCreated);
    assert_eq!(triggers[1].mode, TriggerMode::Sacrificed);
}

#[test]
fn compound_or_create_subject_boundary_rejects_created_prefix() {
    // CR 701.7: "create" is an event verb; "created" is not the verb head.
    assert_eq!(extract_subject_text("you create"), "you");
    assert_eq!(
        extract_subject_text("you created a token"),
        "you created a token"
    );
}

#[test]
fn compound_or_cast_or_cycle_self() {
    // CR 702.29c + CR 601.2: Warped Tusker — "When you cast or cycle ~"
    // splits into a SpellCast self-trigger and a Cycled self-trigger.
    let triggers = parse_trigger_lines(
            "When you cast or cycle Warped Tusker, search your library for a basic land card, reveal it, put it into your hand, then shuffle.",
            "Warped Tusker",
        );
    assert_eq!(triggers.len(), 2);
    assert_eq!(triggers[0].mode, TriggerMode::SpellCast);
    assert_eq!(triggers[0].valid_card, Some(TargetFilter::SelfRef));
    assert_eq!(triggers[0].trigger_zones, vec![Zone::Stack]);
    assert_eq!(triggers[1].mode, TriggerMode::Cycled);
    assert_eq!(triggers[1].valid_card, Some(TargetFilter::SelfRef));
    assert!(triggers[1].trigger_zones.contains(&Zone::Graveyard));
}

#[test]
fn non_compound_trigger_returns_single() {
    // Normal trigger should produce exactly 1 result
    let triggers = parse_trigger_lines("When this creature enters, draw a card.", "Test Card");
    assert_eq!(triggers.len(), 1);
    assert_eq!(triggers[0].mode, TriggerMode::ChangesZone);
}

// ── "and/or" compound subject triggers ──

#[test]
fn trigger_self_and_or_other_nontoken_creatures_enter() {
    // CR 603.4 + CR 601.2: Satoru-style "~ and/or one or more other nontoken
    // creatures you control enter, if none of them were cast ..."
    let def = parse_trigger_line(
            "Whenever ~ and/or one or more other nontoken creatures you control enter, if none of them were cast or no mana was spent to cast them, draw a card.",
            "Satoru, the Infiltrator",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    assert!(def.batched);

    // Subject should be Or { SelfRef, Typed(nontoken creature you control, Another) }
    match &def.valid_card {
        Some(TargetFilter::Or { filters }) => {
            assert_eq!(
                filters.len(),
                2,
                "Expected 2 filters in Or, got {filters:?}"
            );
            assert_eq!(filters[0], TargetFilter::SelfRef);
            // Second filter: nontoken creature you control with Another
            if let TargetFilter::Typed(tf) = &filters[1] {
                assert!(
                    tf.properties.contains(&FilterProp::Another),
                    "Expected Another property, got {:?}",
                    tf.properties
                );
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(
                    tf.type_filters.contains(&TypeFilter::Creature),
                    "Expected Creature type, got {:?}",
                    tf.type_filters
                );
            } else {
                panic!("Expected Typed filter, got {:?}", filters[1]);
            }
        }
        other => panic!("Expected Or filter, got {other:?}"),
    }

    // Condition: Or { Not(WasCast), ManaSpentCondition }
    match &def.condition {
        Some(TriggerCondition::Or { conditions }) => {
            assert_eq!(conditions.len(), 2);
            assert_eq!(
                conditions[0],
                TriggerCondition::Not {
                    condition: Box::new(TriggerCondition::WasCast {
                        zone: None,
                        controller: None,
                        owner: None,
                    }),
                }
            );
            assert!(
                matches!(&conditions[1], TriggerCondition::ManaSpentCondition { .. }),
                "Expected ManaSpentCondition, got {:?}",
                conditions[1]
            );
        }
        other => panic!("Expected Or condition, got {other:?}"),
    }
}

#[test]
fn trigger_if_it_wasnt_cast() {
    // CR 603.4 + CR 601.2: "if it wasn't cast" — negation of WasCast.
    let def = parse_trigger_line(
        "Whenever a creature enters under your control, if it wasn't cast, draw a card.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You)
        ))
    );
    assert_eq!(
        def.condition,
        Some(TriggerCondition::Not {
            condition: Box::new(TriggerCondition::WasCast {
                zone: None,
                controller: None,
                owner: None,
            }),
        })
    );
}

#[test]
fn trigger_subject_extracts_opponent_as_player() {
    // CR 608.2k: "an opponent" should be recognized as a player-type subject,
    // not fall through to parse_type_phrase returning Any.
    let (filter, rest) =
        parse_single_subject("an opponent draws a card", &mut ParseContext::default());
    assert!(
        matches!(
            &filter,
            TargetFilter::Typed(tf) if tf.type_filters.is_empty()
                && tf.controller == Some(ControllerRef::Opponent)
        ),
        "expected opponent player filter, got: {filter:?}"
    );
    assert!(
        rest.starts_with("draws"),
        "rest should start with verb: {rest}"
    );
}

#[test]
fn trigger_subject_extracts_player() {
    let (filter, rest) =
        parse_single_subject("a player casts a spell", &mut ParseContext::default());
    assert_eq!(filter, TargetFilter::Player);
    assert!(
        rest.starts_with("casts"),
        "rest should start with verb: {rest}"
    );
}

#[test]
fn sheoldred_they_lose_life_has_triggering_player() {
    // Sheoldred: "Whenever an opponent draws a card, they lose 2 life."
    // The LoseLife effect should target TriggeringPlayer (the opponent who drew).
    let def = parse_trigger_line(
        "Whenever an opponent draws a card, they lose 2 life.",
        "Sheoldred, the Apocalypse",
    );
    assert_eq!(def.mode, TriggerMode::Drawn);
    let execute = def.execute.as_ref().expect("should have execute");
    match &*execute.effect {
        Effect::LoseLife { target, .. } => {
            assert_eq!(
                *target,
                Some(TargetFilter::TriggeringPlayer),
                "LoseLife should target TriggeringPlayer"
            );
        }
        other => panic!("expected LoseLife, got: {other:?}"),
    }
}

#[test]
fn smothering_tithe_that_player_pays_as_triggering_player() {
    let def = parse_trigger_line(
            "Whenever an opponent draws a card, that player may pay {2}. If the player doesn't, you create a Treasure token.",
            "Smothering Tithe",
        );

    assert_eq!(def.mode, TriggerMode::Drawn);
    let execute = def.execute.as_ref().expect("should have execute");
    match &*execute.effect {
        Effect::PayCost {
            payer,
            cost: AbilityCost::Mana { cost },
            ..
        } => {
            assert_eq!(payer, &TargetFilter::TriggeringPlayer);
            assert_eq!(cost, &crate::types::mana::ManaCost::generic(2));
        }
        other => panic!("expected PayCost, got: {other:?}"),
    }
    assert!(execute.optional, "that player may pay should be optional");

    let sub = execute
        .sub_ability
        .as_ref()
        .expect("Treasure creation should remain chained");
    assert_eq!(
        sub.condition,
        Some(AbilityCondition::Not {
            condition: Box::new(AbilityCondition::effect_performed())
        })
    );
    match &*sub.effect {
        Effect::Token { name, owner, .. } => {
            assert_eq!(name, "Treasure");
            assert_eq!(owner, &TargetFilter::Controller);
        }
        other => panic!("expected Treasure Token sub_ability, got: {other:?}"),
    }
}

/// CR 603.4: Wedding Ring — "an opponent who controls F draws
/// a card" parses the relative clause into an `ObjectCount >= 1`
/// intervening-if scoped to the triggering player, ANDed with the
/// during-turn timing condition.
fn assert_wedding_ring_clause_condition(condition: &Option<TriggerCondition>) {
    let TriggerCondition::And { conditions } = condition
        .as_ref()
        .expect("trigger should carry a condition")
    else {
        panic!("expected And condition, got: {condition:?}");
    };
    assert_eq!(conditions.len(), 2, "during-turn AND clause-presence");
    // First conjunct: the "during their turn" timing restriction.
    assert_eq!(
        conditions[0],
        TriggerCondition::DuringPlayersTurn {
            player: PlayerFilter::TriggeringPlayer,
        },
    );
    // Second conjunct: the "who controls F" relative-clause intervening-if.
    match &conditions[1] {
        TriggerCondition::QuantityComparison {
            lhs:
                QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount { filter },
                },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        } => {
            let TargetFilter::Typed(tf) = filter else {
                panic!("expected Typed clause filter, got: {filter:?}");
            };
            assert_eq!(
                tf.controller,
                Some(ControllerRef::TriggeringPlayer),
                "clause filter controller must be TriggeringPlayer",
            );
            assert!(
                tf.type_filters.contains(&TypeFilter::Artifact),
                "clause filter should be an artifact: {tf:?}",
            );
            // CR 201.2a: name comparison is case-insensitive at evaluation;
            // `parse_trigger_condition` lowercases the condition text, so the
            // parsed `Named` value is lowercase here.
            assert!(
                tf.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::Named { name } if name.eq_ignore_ascii_case("Wedding Ring")
                )),
                "clause filter should carry Named \"Wedding Ring\": {tf:?}",
            );
        }
        other => panic!("expected QuantityComparison clause, got: {other:?}"),
    }
}

#[test]
fn wedding_ring_who_controls_clause_drawn() {
    // `parse_trigger_condition` receives the trigger CONDITION clause only
    // (the effect has already been split off by the IR pipeline at the
    // first comma — see `parse_trigger_line_with_index_ir`).
    let mut ctx = ParseContext::default();
    let (mode, def) = parse_trigger_condition(
            "Whenever an opponent who controls an artifact named Wedding Ring draws a card during their turn",
            &mut ctx,
        );
    assert_eq!(mode, TriggerMode::Drawn);
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        )),
    );
    assert_wedding_ring_clause_condition(&def.condition);
}

#[test]
fn wedding_ring_who_controls_clause_gains_life() {
    let mut ctx = ParseContext::default();
    let (mode, def) = parse_trigger_condition(
            "Whenever an opponent who controls an artifact named Wedding Ring gains life during their turn",
            &mut ctx,
        );
    assert_eq!(mode, TriggerMode::LifeGained);
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        )),
    );
    assert_wedding_ring_clause_condition(&def.condition);
}

#[test]
fn plain_opponent_draws_has_no_clause_condition() {
    // CR 603.4: A trigger with no "who controls" clause must not carry a
    // QuantityComparison intervening-if (unregressed baseline).
    let mut ctx = ParseContext::default();
    let (mode, def) = parse_trigger_condition("an opponent draws a card", &mut ctx);
    assert_eq!(mode, TriggerMode::Drawn);
    assert_eq!(def.condition, None);
}

#[test]
fn parse_trigger_condition_entry_resets_stale_subject_clause() {
    // ROUND-3 CORRECTION #1: `parse_trigger_condition` unconditionally
    // resets `ctx.pending_trigger_subject_clause` at entry. This guards
    // against a clause set on a prior parse path (subject decomposition,
    // compound-`or` recursion, or a discarded-remainder caller such as
    // `extract_trigger_subject_for_context`) leaking into the next trigger
    // line — those paths set the clause but never reach the consuming
    // `.take()` in `parse_trigger_condition`.
    //
    // This test discriminates the entry-reset directly: a stale clause is
    // pre-seeded on the context, then a CLEAN trigger (no "who controls"
    // clause of its own) is parsed. With the entry-reset the stale clause
    // is discarded and the clean trigger carries NO condition. Without the
    // entry-reset the stale clause survives to the post-`try_parse_event`
    // `.take()` and is wrongly ANDed into the clean trigger's condition.
    let mut ctx = ParseContext {
        pending_trigger_subject_clause: Some(TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Artifact],
            controller: None,
            properties: vec![FilterProp::Named {
                name: "wedding ring".to_string(),
            }],
        })),
        ..ParseContext::default()
    };

    let (mode, def) = parse_trigger_condition("an opponent draws a card", &mut ctx);
    assert_eq!(mode, TriggerMode::Drawn);
    assert_eq!(
        def.condition, None,
        "entry-reset must discard the pre-seeded stale clause; \
             a clean trigger must not inherit a leaked QuantityComparison",
    );
    // The reset must also clear the field itself, not just ignore it.
    assert_eq!(ctx.pending_trigger_subject_clause, None);
}

#[test]
fn who_controls_clause_does_not_leak_to_next_trigger_line() {
    // Sequential end-to-end check: a "who controls F" trigger followed by a
    // plain opponent-draw trigger on the SAME context. The first parse
    // consumes its own clause via `.take()`; the entry-reset additionally
    // guarantees the second line starts clean.
    let mut ctx = ParseContext::default();
    let (_, first) = parse_trigger_condition(
            "Whenever an opponent who controls an artifact named Wedding Ring draws a card during their turn",
            &mut ctx,
        );
    assert_wedding_ring_clause_condition(&first.condition);
    let (_, second) = parse_trigger_condition("an opponent draws a card", &mut ctx);
    assert_eq!(
        second.condition, None,
        "second trigger line must not inherit the first line's clause",
    );
}

/// CR 603.4: Assert `condition` is exactly the who-controls `ObjectCount
/// >= 1` intervening-if (no during-turn AND-wrapper), with the clause
/// filter's controller rewritten to `TriggeringPlayer` and carrying a
/// type-filter matching `expect_type`.
fn assert_who_controls_clause_only(condition: &Option<TriggerCondition>, expect_type: TypeFilter) {
    match condition
        .as_ref()
        .expect("trigger should carry a condition")
    {
        TriggerCondition::QuantityComparison {
            lhs:
                QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount { filter },
                },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        } => {
            let TargetFilter::Typed(tf) = filter else {
                panic!("expected Typed clause filter, got: {filter:?}");
            };
            assert_eq!(
                tf.controller,
                Some(ControllerRef::TriggeringPlayer),
                "clause filter controller must be TriggeringPlayer",
            );
            assert!(
                tf.type_filters.contains(&expect_type),
                "clause filter should be {expect_type:?}: {tf:?}",
            );
        }
        other => panic!("expected QuantityComparison clause, got: {other:?}"),
    }
}

#[test]
fn who_controls_clause_cast_lifts_condition() {
    // CR 603.4 + CR 601.2: BEFORE the cast-scan guard, `split_once_on`
    // matched " casts a" anywhere, so this line parsed to a clause-LESS
    // SpellCast (the who-controls clause silently dropped). AFTER, the
    // cast-scan guard declines, subject decomposition runs, and the
    // who-controls clause is lifted into `def.condition`.
    let mut ctx = ParseContext::default();
    let (mode, def) = parse_trigger_condition(
        "Whenever an opponent who controls a creature casts a spell",
        &mut ctx,
    );
    assert_eq!(mode, TriggerMode::SpellCast);
    assert_who_controls_clause_only(&def.condition, TypeFilter::Creature);
}

#[test]
fn who_controls_clause_sacrifice_lifts_condition() {
    // CR 603.4 + CR 701.21: who-controls sacrifice line is purely additive
    // (today falls through to Unknown). Mode must be `Sacrificed`.
    let mut ctx = ParseContext::default();
    let (mode, def) = parse_trigger_condition(
        "Whenever an opponent who controls an artifact sacrifices a permanent",
        &mut ctx,
    );
    assert_eq!(mode, TriggerMode::Sacrificed);
    assert_who_controls_clause_only(&def.condition, TypeFilter::Artifact);
}

#[test]
fn who_controls_clause_discard_lifts_condition() {
    // CR 603.4 + CR 701.9: who-controls discard line, purely additive.
    let mut ctx = ParseContext::default();
    let (mode, def) = parse_trigger_condition(
        "Whenever an opponent who controls a creature discards a card",
        &mut ctx,
    );
    assert_eq!(mode, TriggerMode::Discarded);
    assert_who_controls_clause_only(&def.condition, TypeFilter::Creature);
}

#[test]
fn who_controls_clause_mills_lifts_condition() {
    // CR 603.4 + CR 701.17a: who-controls mill line — parsed by
    // `try_parse_event` (ViaEvent), purely additive.
    let mut ctx = ParseContext::default();
    let (mode, def) = parse_trigger_condition(
        "Whenever a player who controls a creature mills a nonland card",
        &mut ctx,
    );
    assert_eq!(mode, TriggerMode::Milled);
    assert_who_controls_clause_only(&def.condition, TypeFilter::Creature);
}

#[test]
fn plain_opponent_cast_unchanged_by_guard() {
    // CR 603.4: Regression — the cast-scan guard must NOT over-fire on a
    // plain (no who-controls) cast line. `valid_target` stays `Opponent`,
    // no who-controls condition is added.
    let mut ctx = ParseContext::default();
    let (mode, def) = parse_trigger_condition("Whenever an opponent casts a spell", &mut ctx);
    assert_eq!(mode, TriggerMode::SpellCast);
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        )),
    );
    assert_eq!(def.condition, None);
}

#[test]
fn who_controls_cast_clause_does_not_leak_to_next_trigger_line() {
    // CR 603.4: a who-controls cast line followed by a plain trigger parse
    // independently on the same context — the clause does not leak.
    let mut ctx = ParseContext::default();
    let (_, first) = parse_trigger_condition(
        "Whenever an opponent who controls a creature casts a spell",
        &mut ctx,
    );
    assert_who_controls_clause_only(&first.condition, TypeFilter::Creature);
    let (_, second) = parse_trigger_condition("an opponent draws a card", &mut ctx);
    assert_eq!(
        second.condition, None,
        "second trigger line must not inherit the first line's clause",
    );
}

#[test]
fn trigger_you_may_pay_remains_controller() {
    let def = parse_trigger_line(
        "Whenever you attack, you may pay {2}. If you do, draw a card.",
        "Test Card",
    );

    assert_eq!(def.mode, TriggerMode::YouAttack);
    let execute = def.execute.as_ref().expect("should have execute");
    match &*execute.effect {
        Effect::PayCost {
            payer,
            cost: AbilityCost::Mana { cost },
            ..
        } => {
            assert_eq!(payer, &TargetFilter::Controller);
            assert_eq!(cost, &crate::types::mana::ManaCost::generic(2));
        }
        other => panic!("expected PayCost, got: {other:?}"),
    }
    assert!(execute.optional, "you may pay should remain optional");
}

#[test]
fn spellcast_you_may_pay_if_you_do_create_token() {
    let def = parse_trigger_line(
            "Whenever you cast an artifact spell, you may pay {1}. If you do, create a 1/1 colorless Myr artifact creature token.",
            "Myrsmith",
        );

    assert_eq!(def.mode, TriggerMode::SpellCast);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert!(matches!(
        &def.valid_card,
        Some(TargetFilter::Typed(filter))
            if filter.type_filters == vec![TypeFilter::Artifact]
    ));

    let execute = def.execute.as_ref().expect("should have execute");
    assert!(execute.optional, "you may pay should be optional");
    match &*execute.effect {
        Effect::PayCost {
            payer,
            cost: AbilityCost::Mana { cost },
            ..
        } => {
            assert_eq!(payer, &TargetFilter::Controller);
            assert_eq!(cost, &crate::types::mana::ManaCost::generic(1));
        }
        other => panic!("expected PayCost, got: {other:?}"),
    }

    let token = execute
        .sub_ability
        .as_ref()
        .expect("pay-cost trigger should have an if-you-do token");
    assert_eq!(token.condition, Some(AbilityCondition::effect_performed()));
    match &*token.effect {
        Effect::Token {
            name,
            power,
            toughness,
            types,
            owner,
            ..
        } => {
            assert_eq!(name, "Myr");
            assert_eq!(power, &PtValue::Fixed(1));
            assert_eq!(toughness, &PtValue::Fixed(1));
            assert_eq!(
                types,
                &vec![
                    "Artifact".to_string(),
                    "Creature".to_string(),
                    "Myr".to_string()
                ]
            );
            assert_eq!(owner, &TargetFilter::Controller);
        }
        other => panic!("expected token, got: {other:?}"),
    }
}

// -----------------------------------------------------------------------
// Parts A–E: Station / Saddle / Crew triggers + OnlyDuringYourMainPhase
// + condition-scoped OncePerTurn sweep.
// -----------------------------------------------------------------------

#[test]
fn monoist_gravliner_stations_trigger_parses() {
    // CR 702.184a: "Whenever a creature stations this Spacecraft, ..."
    let def = parse_trigger_line(
            "Whenever a creature stations this Spacecraft, that creature perpetually gains deathtouch and lifelink.",
            "Monoist Gravliner",
        );
    assert_eq!(def.mode, TriggerMode::Stationed);
}

#[test]
fn another_creature_stations_subject_threading() {
    // valid_source carries the actor subject (pronoun context).
    let def = parse_trigger_line(
        "Whenever another creature stations ~, draw a card.",
        "Test Spacecraft",
    );
    assert_eq!(def.mode, TriggerMode::Stationed);
    // Subject is a Typed(Creature) with FilterProp::Another.
    match &def.valid_source {
        Some(TargetFilter::Typed(tf)) => {
            use crate::types::ability::FilterProp;
            assert!(
                tf.properties.contains(&FilterProp::Another),
                "expected FilterProp::Another in subject, got {:?}",
                tf.properties
            );
        }
        other => panic!("expected Typed subject, got {other:?}"),
    }
}

#[test]
fn burrowfiend_becomes_saddled_parses_with_once_per_turn() {
    // CR 702.171b + Part D: BecomesSaddled mode + OncePerTurn from condition-scoped scan.
    let def = parse_trigger_line(
        "Whenever this creature becomes saddled for the first time each turn, mill two cards.",
        "Stubborn Burrowfiend",
    );
    assert_eq!(def.mode, TriggerMode::BecomesSaddled);
    assert_eq!(def.constraint, Some(TriggerConstraint::OncePerTurn));
}

#[test]
fn gearshift_ace_crews_trigger_parses() {
    // CR 702.122: "Whenever ~ crews a Vehicle, ..."
    let def = parse_trigger_line(
        "Whenever Gearshift Ace crews a Vehicle, that Vehicle gains flying until end of turn.",
        "Gearshift Ace",
    );
    assert_eq!(def.mode, TriggerMode::Crews);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

#[test]
fn canyon_vaulter_compound_trigger_parses() {
    // CR 702.122 + CR 702.171c + CR 505.1: SaddlesOrCrews + OnlyDuringYourMainPhase.
    let def = parse_trigger_line(
            "Whenever Canyon Vaulter saddles a Mount or crews a Vehicle during your main phase, that Mount or Vehicle gains flying until end of turn.",
            "Canyon Vaulter",
        );
    assert_eq!(def.mode, TriggerMode::SaddlesOrCrews);
    assert_eq!(
        def.constraint,
        Some(TriggerConstraint::OnlyDuringYourMainPhase)
    );
}

#[test]
fn saddles_a_mount_singular_parses() {
    // Pre-stage: no card prints this today without the compound; the arm must still fire.
    let def = parse_trigger_line(
        "Whenever ~ saddles a Mount, draw a card.",
        "Hypothetical Saddler",
    );
    assert_eq!(def.mode, TriggerMode::Saddles);
    assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
}

#[test]
fn first_time_each_turn_in_condition_sets_once_per_turn() {
    // Part D: condition-scoped constraint assignment.
    let def = parse_trigger_line(
        "Whenever ~ attacks for the first time each turn, draw a card.",
        "Godo, Bandit Warlord",
    );
    assert_eq!(def.mode, TriggerMode::Attacks);
    assert_eq!(def.constraint, Some(TriggerConstraint::OncePerTurn));
}

#[test]
fn first_time_during_each_opponent_turn_sets_per_opponent_constraint() {
    // CR 603.2: the "first time during each of their turns" text is part
    // of the trigger event, not a generic once-per-turn limit.
    let def = parse_trigger_line(
            "Whenever an opponent loses life for the first time during each of their turns, put a +1/+1 counter on ~.",
            "Valgavoth, Harrower of Souls",
        );
    assert_eq!(def.mode, TriggerMode::LifeLost);
    assert_eq!(
        def.constraint,
        Some(TriggerConstraint::OncePerOpponentPerTurn)
    );
}

#[test]
fn first_time_each_turn_in_effect_only_does_not_set_constraint() {
    // Part D scope guard: the phrase in EFFECT text alone must not set the constraint.
    // Contrived input — no real card prints this, but the guard is important.
    let def = parse_trigger_line(
        "Whenever ~ attacks, for the first time each turn create a token.",
        "Contrived Card",
    );
    assert_eq!(def.mode, TriggerMode::Attacks);
    assert_eq!(def.constraint, None);
}

#[test]
fn valiant_rescuer_regression() {
    // Part D: the removed hardcoded handler must be replaced by the generic path
    // + condition-scoped OncePerTurn. FilterProp::Another must still be present,
    // and `secondary` must NOT be set (the removed hack is the only writer).
    use crate::types::ability::FilterProp;
    let def = parse_trigger_line(
            "Whenever you cycle another card for the first time each turn, create a 2/2 red Dinosaur creature token.",
            "Valiant Rescuer",
        );
    assert_eq!(def.mode, TriggerMode::Cycled);
    assert_eq!(def.constraint, Some(TriggerConstraint::OncePerTurn));
    assert!(!def.secondary, "removed hack should not set secondary");
    match &def.valid_card {
        Some(TargetFilter::Typed(tf)) => {
            assert!(tf.properties.contains(&FilterProp::Another));
        }
        other => panic!("expected Typed filter with Another prop, got {other:?}"),
    }
}

#[test]
fn aurelia_attacks_first_time_has_constraint() {
    // Regression guard: Aurelia was previously parsed as TriggerMode::Attacks
    // but without the OncePerTurn constraint (latent multi-card bug).
    let def = parse_trigger_line(
            "Whenever Aurelia, the Warleader attacks for the first time each turn, untap all attacking creatures.",
            "Aurelia, the Warleader",
        );
    assert_eq!(def.mode, TriggerMode::Attacks);
    assert_eq!(def.constraint, Some(TriggerConstraint::OncePerTurn));
}

#[test]
fn during_your_main_phase_parser_arm_unit_test() {
    // Isolated: parse_trigger_constraint arm.
    assert_eq!(
        parse_trigger_constraint("whenever ~ attacks during your main phase"),
        Some(TriggerConstraint::OnlyDuringYourMainPhase)
    );
}

#[test]
fn tiana_when_keyword_and_compound_subject_parses() {
    // M9 + M10 guard: Tiana uses "When" (not "Whenever") AND a compound subject
    // "Tiana, Angelic Mechanic or another legendary creature you control".
    // normalize_card_name_refs must replace the full name → ~, and the compound
    // subject parser must produce Or { SelfRef, Typed(Creature, Legendary, You, Another) }.
    let def = parse_trigger_line(
            "When Tiana, Angelic Mechanic or another legendary creature you control crews a Vehicle, that Vehicle perpetually gets +1/+0.",
            "Tiana, Angelic Mechanic",
        );
    assert_eq!(def.mode, TriggerMode::Crews);
    // valid_card must be an Or with both SelfRef and the Typed branch.
    match &def.valid_card {
        Some(TargetFilter::Or { filters }) => {
            let has_self = filters.iter().any(|f| matches!(f, TargetFilter::SelfRef));
            let has_typed_legendary = filters.iter().any(|f| {
                matches!(
                    f,
                    TargetFilter::Typed(tf)
                    if tf.controller == Some(ControllerRef::You)
                        && tf.properties.contains(&FilterProp::HasSupertype {
                            value: crate::types::card_type::Supertype::Legendary,
                        })
                        && tf.properties.contains(&FilterProp::Another)
                )
            });
            assert!(
                has_self && has_typed_legendary,
                "expected Or{{SelfRef, Typed(Legendary, You, Another)}}, got {filters:?}"
            );
        }
        other => panic!("expected Or filter, got {other:?}"),
    }
}

#[test]
fn mighty_servant_becomes_crewed_parses_with_once_per_turn() {
    // M3 regression: "becomes crewed" was never recognized by parse_simple_event,
    // so Mighty Servant of Leuk-O and Mindlink Mech silently parsed as Unknown
    // despite carrying the OncePerTurn constraint. Part M3 adds the arm.
    let def = parse_trigger_line(
        "Whenever this Vehicle becomes crewed for the first time each turn, draw two cards.",
        "Mighty Servant of Leuk-O",
    );
    assert_eq!(def.mode, TriggerMode::BecomesCrewed);
    assert_eq!(def.constraint, Some(TriggerConstraint::OncePerTurn));
}

#[test]
fn gourmand_talent_two_triggers_both_constrained() {
    // D5 #4: Gourmand's Talent has two separate life-gain triggers. Each must
    // carry OncePerTurn independently; runtime trig_idx (ordinal in the trigger
    // list) keys the OncePerTurn state distinctly, so independent parse →
    // independent runtime tracking.
    let first = parse_trigger_line(
        "Whenever you gain life for the first time each turn, draw a card.",
        "Gourmand's Talent",
    );
    let second = parse_trigger_line(
        "Whenever you gain life for the first time each turn, create a Food token.",
        "Gourmand's Talent",
    );
    assert_eq!(first.constraint, Some(TriggerConstraint::OncePerTurn));
    assert_eq!(second.constraint, Some(TriggerConstraint::OncePerTurn));
}

#[test]
fn stensia_generic_damage_trigger_constrained() {
    // D5 #5 / M8: Stensia's "a creature deals damage to one or more players for
    // the first time each turn" — phrase modifies the EVENT, not per-creature
    // frequency. OncePerTurn keyed on Stensia's (obj_id, trig_idx) is
    // source-level — one firing per turn regardless of which creature triggered.
    let def = parse_trigger_line(
            "Whenever a creature deals damage to one or more players for the first time each turn, put a +1/+1 counter on it.",
            "Stensia, Condemner's Keep",
        );
    assert_eq!(def.constraint, Some(TriggerConstraint::OncePerTurn));
}

// SOC Tier 2.6: "Whenever you create one or more creature tokens" —
// batched token-creation trigger with type + controller filters.
#[test]
fn trigger_one_or_more_creature_tokens_created() {
    let def = parse_trigger_line(
        "Whenever you create one or more creature tokens, put a story counter on this artifact.",
        "Staff of the Storyteller",
    );
    assert_eq!(def.mode, TriggerMode::TokenCreated);
    assert!(def.batched);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    assert!(
        def.valid_card.is_some(),
        "creature-type filter should be captured on valid_card"
    );
    assert!(def.execute.is_some());
}

#[test]
fn trigger_one_or_more_tokens_created_bare() {
    let def = parse_trigger_line(
        "Whenever you create one or more tokens, draw a card.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::TokenCreated);
    assert!(def.batched);
    assert_eq!(def.valid_card, None);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn trigger_simple_token_created_is_not_batched() {
    let def = parse_trigger_line(
        "Whenever you create a token, each opponent loses 1 life.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::TokenCreated);
    assert!(!def.batched);
    assert_eq!(def.valid_card, None);
    assert_eq!(def.valid_target, Some(TargetFilter::Controller));
}

#[test]
fn trigger_one_or_more_artifact_tokens_created() {
    let def = parse_trigger_line(
        "Whenever you create one or more artifact tokens, you gain 1 life.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::TokenCreated);
    assert!(def.batched);
    assert!(def.valid_card.is_some());
}

// CR 508.1 + CR 603.2c + CR 603.4: Attacks-with-N-creatures trigger family
// (Firemane Commando and analogous cards).

/// Issue #610 — Kratos, Stoic Father. The actor-led count==1 "attack with one
/// or more <TYPE>" form must populate `valid_card` with the attacker-type
/// filter (and NO count condition; the matcher's "≥1 matching attacker" IS
/// "one or more"). Pre-fix this text fell through to the bare "you attack"
/// fallback, dropping the God filter entirely.
#[test]
fn you_attack_with_one_or_more_gods_populates_filter() {
    let def = parse_trigger_line(
        "Whenever you attack with one or more Gods, you get an experience counter.",
        "Kratos, Stoic Father",
    );
    assert_eq!(def.mode, TriggerMode::YouAttack);
    assert!(def.batched);
    // No count condition for the count==1 form.
    assert_eq!(def.condition, None);
    // valid_target carries the controller (You) axis.
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You)
        ))
    );
    // valid_card carries the God attacker-type filter (load-bearing fix).
    match &def.valid_card {
        Some(TargetFilter::Typed(tf)) => assert!(
            tf.type_filters
                .iter()
                .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "God")),
            "expected God subtype in valid_card, got {:?}",
            tf.type_filters,
        ),
        other => panic!("expected Typed valid_card with God subtype, got {other:?}"),
    }
}

/// Issue #610 (Anim Pakal class) — negated subtype head noun. "non-Gnome
/// creatures" must yield a negated-Gnome filter on `valid_card`. `parse_type_phrase`
/// already emits the negation; verify it survives onto `valid_card`.
#[test]
fn you_attack_with_one_or_more_non_gnome_creatures() {
    let def = parse_trigger_line(
        "Whenever you attack with one or more non-Gnome creatures, create a 1/1 Gnome.",
        "Anim Pakal, Thousandth Moon",
    );
    assert_eq!(def.mode, TriggerMode::YouAttack);
    assert_eq!(def.condition, None);
    match &def.valid_card {
        Some(TargetFilter::Typed(tf)) => assert!(
            tf.type_filters.iter().any(|t| matches!(
                t,
                TypeFilter::Non(inner) if matches!(&**inner, TypeFilter::Subtype(s) if s == "Gnome")
            )),
            "expected negated Gnome subtype in valid_card, got {:?}",
            tf.type_filters,
        ),
        other => panic!("expected Typed valid_card with negated Gnome, got {other:?}"),
    }
}

/// CR 508.1a + CR 603.2c: the UNTYPED actor-led count>1 form
/// ("two or more creatures") stays byte-identical to pre-fix behavior:
/// `valid_card` UNSET and the count condition's `Controller` subject carries
/// `filter: None`. (Formerly the deferral lock — the typed count>1 form is
/// now implemented; see `trigger_you_attack_with_two_or_more_typed_creatures`.)
#[test]
fn you_attack_with_two_or_more_creatures_untyped_no_filter() {
    let def = parse_trigger_line(
        "Whenever you attack with two or more creatures, draw a card.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::YouAttack);
    assert_eq!(
        def.valid_card, None,
        "untyped count>1 must NOT set valid_card"
    );
    assert_eq!(
        def.condition,
        Some(TriggerCondition::AttackersDeclaredCount {
            subject: AttackersDeclaredCountSubject::Controller {
                scope: ControllerRef::You,
                filter: None,
            },
            comparator: Comparator::GE,
            count: 2,
        })
    );
}

/// CR 508.1a + CR 603.2c: the TYPED actor-led count>1 form
/// ("two or more Dinosaurs") now parses the type phrase into BOTH the
/// matcher's `valid_card` gate AND the condition-level type axis
/// (`AttackersDeclaredCount`'s `Controller` subject `filter`), so the count
/// enforces ≥N *Dinosaurs* rather than ≥N *any* attackers. This is the
/// over-fire guard.
#[test]
fn trigger_you_attack_with_two_or_more_typed_creatures() {
    let def = parse_trigger_line(
        "Whenever you attack with two or more Dinosaurs, draw a card.",
        "Test Dinosaur Lord",
    );
    assert_eq!(def.mode, TriggerMode::YouAttack);
    assert!(def.batched);
    // valid_card carries the Dinosaur subtype so the matcher's
    // "≥1 matching attacker" gate aligns with the typed minimum.
    match &def.valid_card {
        Some(TargetFilter::Typed(tf)) => assert!(
            tf.type_filters
                .iter()
                .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "Dinosaur")),
            "expected Dinosaur subtype in valid_card, got {:?}",
            tf.type_filters,
        ),
        other => panic!("expected Typed valid_card with Dinosaur, got {other:?}"),
    }
    // The count condition carries the SAME filter as the type axis.
    match &def.condition {
        Some(TriggerCondition::AttackersDeclaredCount {
            subject:
                AttackersDeclaredCountSubject::Controller {
                    scope: ControllerRef::You,
                    filter: Some(TargetFilter::Typed(tf)),
                },
            comparator: Comparator::GE,
            count: 2,
        }) => assert!(
            tf.type_filters
                .iter()
                .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "Dinosaur")),
            "expected Dinosaur subtype in condition filter, got {:?}",
            tf.type_filters,
        ),
        other => {
            panic!("expected AttackersDeclaredCount {{ Controller {{ You, Some(Dinosaur) }}, GE, 2 }}, got {other:?}")
        }
    }
}

#[test]
fn trigger_you_attack_with_two_or_more_creatures() {
    let def = parse_trigger_line(
        "Whenever you attack with two or more creatures, draw a card.",
        "Firemane Commando",
    );
    assert_eq!(def.mode, TriggerMode::YouAttack);
    assert!(def.batched);
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You)
        ))
    );
    assert!(matches!(
        def.condition,
        Some(TriggerCondition::AttackersDeclaredCount {
            subject: AttackersDeclaredCountSubject::Controller {
                scope: ControllerRef::You,
                filter: None,
            },
            comparator: Comparator::GE,
            count: 2,
        })
    ));
}

#[test]
fn trigger_another_player_attacks_with_two_or_more_creatures_intervening_if() {
    let def = parse_trigger_line(
            "Whenever another player attacks with two or more creatures, they draw a card if none of those creatures attacked you.",
            "Firemane Commando",
        );
    assert_eq!(def.mode, TriggerMode::YouAttack);
    assert!(def.batched);
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        ))
    );
    // Composed: batch-size AND none-of-those-attacked-you.
    match &def.condition {
            Some(TriggerCondition::And { conditions }) => {
                assert_eq!(conditions.len(), 2);
                assert!(matches!(
                    &conditions[0],
                    TriggerCondition::AttackersDeclaredCount {
                        subject: AttackersDeclaredCountSubject::Controller {
                            scope: ControllerRef::Opponent,
                            filter: None,
                        },
                        comparator: Comparator::GE,
                        count: 2,
                    }
                ));
                assert!(matches!(
                    &conditions[1],
                    TriggerCondition::AttackersDeclaredCount {
                        subject: AttackersDeclaredCountSubject::AttackTarget {
                            controller: ControllerRef::You,
                            attacked: AttackTargetFilter::Player,
                            filter: None,
                        },
                        comparator: Comparator::EQ,
                        count: 0,
                    }
                ));
            }
            other => panic!(
                "expected And(controller AttackersDeclaredCount, target AttackersDeclaredCount), got {other:?}"
            ),
        }
    // CR 121.1 + CR 603.7c + CR 608.2k: "they draw a card" — the effect-level
    // subject ("they") must be encoded directly on the Draw target as
    // `TriggeringPlayer`, not via a post-hoc `player_scope` override on the
    // execute ability. The runtime auto-binds `target: TriggeringPlayer`
    // from `state.current_trigger_event` at resolution time
    // (`extract_event_context_filter` in `effects/mod.rs`).
    let execute = def.execute.as_ref().expect("execute");
    match &*execute.effect {
        crate::types::ability::Effect::Draw { target, .. } => {
            assert_eq!(
                target,
                &TargetFilter::TriggeringPlayer,
                "Draw target must be TriggeringPlayer, not Controller"
            );
        }
        other => panic!("expected Draw effect, got {other:?}"),
    }
    assert!(
        execute.player_scope.is_none(),
        "player_scope must be None — the effect-level subject is the single \
             authority for routing. Found {:?}",
        execute.player_scope,
    );
}

#[test]
fn trigger_an_opponent_attacks_with_two_or_more_creatures() {
    let def = parse_trigger_line(
        "Whenever an opponent attacks with two or more creatures, you gain 1 life.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::YouAttack);
    assert!(matches!(
        def.condition,
        Some(TriggerCondition::AttackersDeclaredCount {
            subject: AttackersDeclaredCountSubject::Controller {
                scope: ControllerRef::Opponent,
                filter: None,
            },
            comparator: Comparator::GE,
            count: 2,
        })
    ));
    assert!(def.batched);
}

/// CR 508.1 + CR 603.2c: Aurelia, the Law Above — any-player attack batch
/// count uses `TriggeringPlayer` scope and `Attacks` mode (not the
/// opponent/you-scoped `YouAttack` siblings above).
#[test]
fn trigger_a_player_attacks_with_three_or_more_creatures() {
    let def = parse_trigger_line(
        "Whenever a player attacks with three or more creatures, you draw a card.",
        "Aurelia, the Law Above",
    );
    assert_eq!(def.mode, TriggerMode::Attacks);
    assert!(def.batched);
    assert_eq!(def.valid_source, Some(TargetFilter::Player));
    assert!(matches!(
        def.condition,
        Some(TriggerCondition::AttackersDeclaredCount {
            subject: AttackersDeclaredCountSubject::Controller {
                scope: ControllerRef::TriggeringPlayer,
                filter: None,
            },
            comparator: Comparator::GE,
            count: 3,
        })
    ));
}

#[test]
fn mangara_attack_batch_intervening_if_counts_attacking_you_or_planeswalkers() {
    let def = parse_trigger_line(
            "Whenever an opponent attacks with creatures, if two or more of those creatures are attacking you and/or planeswalkers you control, draw a card.",
            "Mangara, the Diplomat",
        );
    assert_eq!(def.mode, TriggerMode::Attacks);
    assert_eq!(
        def.condition,
        Some(TriggerCondition::AttackersDeclaredCount {
            subject: AttackersDeclaredCountSubject::AttackTarget {
                controller: ControllerRef::You,
                attacked: AttackTargetFilter::PlayerOrPlaneswalker,
                filter: None,
            },
            comparator: Comparator::GE,
            count: 2,
        })
    );
}

/// CR 109.4 + CR 115.1 + CR 506.2: Karazikar's first trigger introduces
/// the attacked player in the condition; "that player controls" inside the
/// effect must resolve to `ControllerRef::DefendingPlayer` so the runtime
/// reads the defending player from the combat state, not from a TargetPlayer
/// target slot. Regression test for #1667.
#[test]
fn karazikar_attack_a_player_uses_defending_player_controller() {
    use crate::types::ability::Effect;

    let def = parse_trigger_line(
        "Whenever you attack a player, tap target creature that player controls and goad it.",
        "Karazikar, the Eye Tyrant",
    );
    assert_eq!(def.mode, TriggerMode::YouAttack);
    let execute = def.execute.as_deref().expect("execute ability");
    match execute.effect.as_ref() {
        Effect::SetTapState {
            target,
            scope: EffectScope::Single,
            state: TapStateChange::Tap,
        } => match target {
            TargetFilter::Typed(t) => assert_eq!(
                t.controller,
                Some(ControllerRef::DefendingPlayer),
                "tap target should reference the defending player via DefendingPlayer",
            ),
            other => panic!("expected Typed filter, got {other:?}"),
        },
        other => panic!("expected Tap effect, got {other:?}"),
    }
}

/// CR 109.4 + CR 115.1 + CR 506.2: Gornog's "Whenever one or more Warriors
/// you control attack a player, target creature that player controls
/// becomes a Coward" introduces the attacked player through a relative-
/// clause subject ("Warriors you control") rather than a bare actor. The
/// effect's "that player controls" must resolve to
/// `ControllerRef::DefendingPlayer` so the runtime reads the defending
/// player from the combat state, not from a TargetPlayer target slot.
/// Regression test for #1667 — the parser now emits DefendingPlayer for
/// attack triggers instead of TargetPlayer.
#[test]
fn gornog_one_or_more_warriors_attack_uses_defending_player_controller() {
    use crate::types::ability::Effect;

    let def = parse_trigger_line(
            "Whenever one or more Warriors you control attack a player, target creature that player controls becomes a Coward.",
            "Gornog, the Red Reaper",
        );
    assert_eq!(def.mode, TriggerMode::YouAttack);
    let execute = def.execute.as_deref().expect("execute ability");
    match execute.effect.as_ref() {
        Effect::GenericEffect { target, .. } => match target {
            Some(TargetFilter::Typed(t)) => assert_eq!(
                t.controller,
                Some(ControllerRef::DefendingPlayer),
                "GenericEffect target should reference the defending player via DefendingPlayer",
            ),
            other => panic!("expected Some(Typed) target filter, got {other:?}"),
        },
        other => panic!("expected GenericEffect, got {other:?}"),
    }
}

/// CR 506.2 + CR 508.1a + CR 608.2c: "attack an opponent" introduces the
/// attacked player just like "attack a player"; the effect's "that player"
/// anaphor must still bind to `DefendingPlayer`.
#[test]
fn attack_an_opponent_uses_defending_player_controller() {
    let def = parse_trigger_line(
            "Whenever a creature you control attacks an opponent, tap target creature that player controls.",
            "Test Card",
        );
    let execute = def.execute.as_deref().expect("execute ability");
    match execute.effect.as_ref() {
        Effect::SetTapState {
            target,
            scope: EffectScope::Single,
            state: TapStateChange::Tap,
        } => match target {
            TargetFilter::Typed(t) => assert_eq!(
                t.controller,
                Some(ControllerRef::DefendingPlayer),
                "tap target should reference the defending player via DefendingPlayer",
            ),
            other => panic!("expected Typed filter, got {other:?}"),
        },
        other => panic!("expected Tap effect, got {other:?}"),
    }
}

/// CR 506.2 + CR 508.1a + CR 608.2c: plural opponent phrases are the same
/// attacked-player anaphor class and must not fall back to TargetPlayer.
#[test]
fn attack_one_or_more_opponents_uses_defending_player_controller() {
    let def = parse_trigger_line(
            "Whenever one or more creatures you control attack one or more of your opponents, tap target creature that player controls.",
            "Test Card",
        );
    let execute = def.execute.as_deref().expect("execute ability");
    match execute.effect.as_ref() {
        Effect::SetTapState {
            target,
            scope: EffectScope::Single,
            state: TapStateChange::Tap,
        } => match target {
            TargetFilter::Typed(t) => assert_eq!(
                t.controller,
                Some(ControllerRef::DefendingPlayer),
                "tap target should reference the defending player via DefendingPlayer",
            ),
            other => panic!("expected Typed filter, got {other:?}"),
        },
        other => panic!("expected Tap effect, got {other:?}"),
    }
}

/// CR 508.5 / CR 508.5a: an attack-trigger effect whose target carries the
/// explicit "defending player controls" qualifier must scope to the
/// defending player. This is the bug class (Kogla, The Tarrasque, ~42
/// cards) — distinct from the "attack a player ... that player controls"
/// anaphor path covered above. Drives the full trigger→effect→target
/// pipeline on the real Oracle text; the assertion flips to `None` if the
/// `parse_zone_controller` arm is reverted.
#[test]
fn attack_trigger_destroy_or_target_defending_player_controls() {
    use crate::types::ability::Effect;

    // Kogla, the Titan Ape: Or-target destroy, scope must fan onto each leg.
    let def = parse_trigger_line(
            "Whenever Kogla, the Titan Ape attacks, destroy target artifact or enchantment defending player controls.",
            "Kogla, the Titan Ape",
        );
    assert_eq!(def.mode, TriggerMode::Attacks);
    let execute = def.execute.as_deref().expect("execute ability");
    match execute.effect.as_ref() {
        Effect::Destroy { target, .. } => match target {
            TargetFilter::Or { filters } => {
                assert_eq!(filters.len(), 2, "expected 2-way OR, got {filters:#?}");
                for (i, leg) in filters.iter().enumerate() {
                    match leg {
                        TargetFilter::Typed(t) => assert_eq!(
                            t.controller,
                            Some(ControllerRef::DefendingPlayer),
                            "leg {i} must scope to the defending player, not null",
                        ),
                        other => panic!("leg {i} expected Typed, got {other:?}"),
                    }
                }
            }
            other => panic!("expected Or target, got {other:?}"),
        },
        other => panic!("expected Destroy effect, got {other:?}"),
    }
}

/// CR 508.5 / CR 508.5a + CR 701.14a: The Tarrasque — "it fights target
/// creature defending player controls". The fight target must scope to the
/// defending player. Covers the Fight verb of the bug class.
#[test]
fn attack_trigger_fight_defending_player_controls() {
    use crate::types::ability::Effect;

    let def = parse_trigger_line(
        "Whenever The Tarrasque attacks, it fights target creature defending player controls.",
        "The Tarrasque",
    );
    assert_eq!(def.mode, TriggerMode::Attacks);
    let execute = def.execute.as_deref().expect("execute ability");
    match execute.effect.as_ref() {
        Effect::Fight { target, .. } => match target {
            TargetFilter::Typed(t) => assert_eq!(
                t.controller,
                Some(ControllerRef::DefendingPlayer),
                "fight target should scope to the defending player, not null",
            ),
            other => panic!("expected Typed target, got {other:?}"),
        },
        other => panic!("expected Fight effect, got {other:?}"),
    }
}

/// CR 115.6 + CR 508.5 / CR 508.5a + CR 701.14a: Ace, Fearless Rebel —
/// "…then it fights up to one target creature defending player controls."
/// The Fight is the tail of a `then`-sequence sub-ability chain, so it lands
/// on a nested `sub_ability`. Both axes must survive: the "up to one" target
/// cardinality (`multi_target == up_to(1)`, min=0) AND the defending-player
/// scope (`ControllerRef::DefendingPlayer`). Regression guard for the
/// dropped optionality (cluster #17) and the landed controller-scope fix.
#[test]
fn attack_trigger_fight_up_to_one_defending_player_controls() {
    use crate::types::ability::{Effect, MultiTargetSpec};

    let def = parse_trigger_line(
            "Whenever Ace attacks, put a +1/+1 counter on Ace, then it fights up to one target creature defending player controls.",
            "Ace, Fearless Rebel",
        );
    assert_eq!(def.mode, TriggerMode::Attacks);
    let execute = def.execute.as_deref().expect("execute ability");
    // Walk the `then`-sequence chain to the Fight sub-ability.
    let fight = walk_to_fight_sub_ability(execute);
    assert_eq!(
        fight.multi_target,
        Some(MultiTargetSpec::up_to(QuantityExpr::Fixed { value: 1 })),
        "fight leg must carry up-to-one multi_target (min=0)",
    );
    match fight.effect.as_ref() {
        Effect::Fight { target, .. } => match target {
            TargetFilter::Typed(t) => assert_eq!(
                t.controller,
                Some(ControllerRef::DefendingPlayer),
                "fight target must scope to the defending player, not null",
            ),
            other => panic!("expected Typed target, got {other:?}"),
        },
        other => panic!("expected Fight effect, got {other:?}"),
    }
}

/// CR 115.6 + CR 508.5 / CR 508.5a: the "up to one" optionality and the
/// `DefendingPlayer` scope are orthogonal axes — an Or-target Fight must fan
/// the scope onto each `Or` disjunct AND retain `up_to(1)`.
#[test]
fn attack_trigger_fight_up_to_one_or_target_defending_player_controls() {
    use crate::types::ability::{Effect, MultiTargetSpec};

    let def = parse_trigger_line(
            "Whenever Ace attacks, put a +1/+1 counter on Ace, then it fights up to one target artifact or enchantment defending player controls.",
            "Ace, Fearless Rebel",
        );
    assert_eq!(def.mode, TriggerMode::Attacks);
    let execute = def.execute.as_deref().expect("execute ability");
    let fight = walk_to_fight_sub_ability(execute);
    assert_eq!(
        fight.multi_target,
        Some(MultiTargetSpec::up_to(QuantityExpr::Fixed { value: 1 })),
        "or-target fight leg must carry up-to-one multi_target (min=0)",
    );
    match fight.effect.as_ref() {
        Effect::Fight { target, .. } => match target {
            TargetFilter::Or { filters } => {
                assert_eq!(filters.len(), 2, "expected 2-way OR, got {filters:#?}");
                for (i, leg) in filters.iter().enumerate() {
                    match leg {
                        TargetFilter::Typed(t) => assert_eq!(
                            t.controller,
                            Some(ControllerRef::DefendingPlayer),
                            "or-leg {i} must scope to the defending player, not null",
                        ),
                        other => panic!("or-leg {i} expected Typed, got {other:?}"),
                    }
                }
            }
            other => panic!("expected Or target, got {other:?}"),
        },
        other => panic!("expected Fight effect, got {other:?}"),
    }
}

/// Walk a `then`-sequence sub-ability chain to the `Effect::Fight` link.
fn walk_to_fight_sub_ability(
    execute: &crate::types::ability::AbilityDefinition,
) -> &crate::types::ability::AbilityDefinition {
    use crate::types::ability::Effect;
    let mut current = execute;
    loop {
        if matches!(current.effect.as_ref(), Effect::Fight { .. }) {
            return current;
        }
        current = current
            .sub_ability
            .as_deref()
            .expect("fight link must exist in the sub_ability chain");
    }
}

/// CR 120.3: Damage-to-player triggers (e.g., "Whenever ~ deals combat
/// damage to a player, destroy target creature that player controls") must
/// continue using `ControllerRef::TargetPlayer`, not `DefendingPlayer`,
/// because the damaged player is not necessarily the defending player in
/// combat (e.g., trample damage to a planeswalker's controller). Regression
/// test for #1667 — ensures the DefendingPlayer fix doesn't break
/// damage-to-player triggers.
#[test]
fn damage_to_player_trigger_uses_target_player() {
    use crate::types::ability::Effect;

    let def = parse_trigger_line(
        "Whenever ~ deals combat damage to a player, destroy target creature that player controls.",
        "Test Card",
    );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    let execute = def.execute.as_deref().expect("execute ability");
    match execute.effect.as_ref() {
        Effect::Destroy { target, .. } => match target {
            TargetFilter::Typed(t) => assert_eq!(
                t.controller,
                Some(ControllerRef::TargetPlayer),
                "Damage-to-player trigger should use TargetPlayer, not DefendingPlayer",
            ),
            other => panic!("expected Typed target filter, got {other:?}"),
        },
        other => panic!("expected Destroy effect, got {other:?}"),
    }
}

/// CR 120.3: Damage-to-opponent triggers introduce the damaged player,
/// which remains TargetPlayer even though attack-to-opponent triggers use
/// DefendingPlayer.
#[test]
fn damage_to_opponent_trigger_uses_target_player() {
    let def = parse_trigger_line(
            "Whenever ~ deals combat damage to an opponent, destroy target creature that player controls.",
            "Test Card",
        );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    let execute = def.execute.as_deref().expect("execute ability");
    match execute.effect.as_ref() {
        Effect::Destroy { target, .. } => match target {
            TargetFilter::Typed(t) => assert_eq!(
                t.controller,
                Some(ControllerRef::TargetPlayer),
                "Damage-to-opponent trigger should use TargetPlayer, not DefendingPlayer",
            ),
            other => panic!("expected Typed target filter, got {other:?}"),
        },
        other => panic!("expected Destroy effect, got {other:?}"),
    }
}

/// Negative scope test — a non-attack-player trigger ("Whenever you draw a
/// card") MUST NOT push the relative-player scope, so "that player controls"
/// inside the effect (synthetic but exercising the parser) still defaults to
/// `ControllerRef::You`. Guards against accidental scope leakage.
#[test]
fn non_attack_player_trigger_does_not_emit_target_player() {
    use crate::types::ability::Effect;

    let def = parse_trigger_line(
        "Whenever you draw a card, tap target creature that player controls.",
        "Test Card",
    );
    let execute = def.execute.as_deref().expect("execute ability");
    // If the parser doesn't classify the synthetic effect, the negative
    // assertion is vacuously satisfied — the karazikar test covers the
    // positive case. If it DOES classify, the controller must remain `You`.
    if let Effect::SetTapState {
        target: TargetFilter::Typed(t),
        scope: EffectScope::Single,
        state: TapStateChange::Tap,
    } = execute.effect.as_ref()
    {
        assert_eq!(
            t.controller,
            Some(ControllerRef::You),
            "non-attack-player trigger should not emit TargetPlayer",
        );
    }
}

/// CR 107.3a + CR 601.2f: Wan Shi Tong's ETB trigger pays `{X}` at cast
/// and must put X +1/+1 counters on himself. Verify the pronoun "him"
/// routes through `resolve_it_pronoun` → `SelfRef` (not `ParentTarget`),
/// and that `Variable{name:"X"}` is rewritten to `CostXPaid` on both the
/// primary PutCounter count and the chained Draw's `DivideRounded` inner.
#[test]
fn wan_shi_tong_etb_cost_x_and_self_pronoun() {
    let def = parse_trigger_line(
            "When Wan Shi Tong enters, put X +1/+1 counters on him. Then draw half X cards, rounded down.",
            "Wan Shi Tong, Librarian",
        );
    let execute = def.execute.as_ref().expect("execute should exist");
    match execute.effect.as_ref() {
        Effect::PutCounter {
            count,
            target,
            counter_type,
        } => {
            assert_eq!(
                counter_type,
                &crate::types::counter::CounterType::Plus1Plus1
            );
            assert_eq!(
                count,
                &QuantityExpr::Ref {
                    qty: QuantityRef::CostXPaid
                },
                "PutCounter count should be CostXPaid, got {count:?}"
            );
            assert_eq!(
                target,
                &TargetFilter::SelfRef,
                "'on him' should resolve to SelfRef for ETB-self trigger, got {target:?}"
            );
        }
        other => panic!("expected PutCounter, got {other:?}"),
    }
    let sub = execute.sub_ability.as_ref().expect("sub ability");
    match sub.effect.as_ref() {
        Effect::Draw { count, .. } => match count {
            QuantityExpr::DivideRounded { inner, .. } => {
                assert_eq!(
                    **inner,
                    QuantityExpr::Ref {
                        qty: QuantityRef::CostXPaid
                    },
                    "DivideRounded inner should be CostXPaid, got {inner:?}"
                );
            }
            other => panic!("expected DivideRounded, got {other:?}"),
        },
        other => panic!("expected Draw, got {other:?}"),
    }
}

/// CR 107.3a + CR 601.2f: The Meathook Massacre's ETB trigger applies
/// -X/-X to each creature. Verify `PtValue::Variable("-X")` is rewritten
/// to `PtValue::Quantity(Multiply{factor:-1, inner:Ref(CostXPaid)})`
/// so the runtime pump handler's `PtValue::Quantity` branch evaluates
/// the X paid at cast time instead of short-circuiting to zero.
#[test]
fn meathook_massacre_etb_cost_x_pumpall() {
    let def = parse_trigger_line(
        "When The Meathook Massacre enters, each creature gets -X/-X until end of turn.",
        "The Meathook Massacre",
    );
    let execute = def.execute.as_ref().expect("execute should exist");
    let expected = PtValue::Quantity(QuantityExpr::Multiply {
        factor: -1,
        inner: Box::new(QuantityExpr::Ref {
            qty: QuantityRef::CostXPaid,
        }),
    });
    match execute.effect.as_ref() {
        Effect::PumpAll {
            power, toughness, ..
        } => {
            assert_eq!(
                power, &expected,
                "PumpAll power should be Multiply(-1, CostXPaid), got {power:?}"
            );
            assert_eq!(
                toughness, &expected,
                "PumpAll toughness should be Multiply(-1, CostXPaid), got {toughness:?}"
            );
        }
        other => panic!("expected PumpAll, got {other:?}"),
    }
}

/// Regression: the cost-X rewrite must be scoped to ETB-self triggers.
/// A dies / attacks / activated-ability trigger that mentions `X` must
/// NOT be rewritten, because `cost_x_paid` on the dying permanent refers
/// to a historical cast and the modern trigger body's X has a different
/// meaning (or no meaning, which falls back to the existing Variable
/// resolver). We spot-check a non-ETB dies trigger to ensure the
/// Variable name survives.
#[test]
fn cost_x_rewrite_skips_non_etb_triggers() {
    // Pure dies trigger — not ETB, must not be rewritten.
    let def = parse_trigger_line(
        "When ~ dies, put X +1/+1 counters on target creature you control.",
        "Fake Card",
    );
    assert_ne!(
        def.destination,
        Some(Zone::Battlefield),
        "sanity: dies trigger is Graveyard destination, not Battlefield"
    );
    // `trigger_should_rewrite_cost_x` must return false.
    assert!(
        !trigger_should_rewrite_cost_x(&def),
        "dies trigger should NOT have X rewritten to CostXPaid"
    );
}

// ── BecomeCopy via triggered ability — CR 707.9 + CR 707.9a ───────────
//
// Class: "[Self] becomes a copy of <target>, except <body>" inside a
// triggered ability body. Building blocks under test: shared
// `parse_except_clause` (oracle_effect::become_copy_except), the new
// `RetainPrintedTriggerFromSource` continuous modification (CR 707.9a),
// and trigger-index threading via `ParseContext::current_trigger_index`.
//
// The tests intentionally span multiple cards / phrasings (Irma, plus
// a synthetic gendered variant) so the building block is exercised
// across the class — not just the named card.

#[test]
fn trigger_become_copy_with_set_name_and_retain_this_ability() {
    // Irma, Part-Time Mutant — the canonical card driving this work.
    // Use the indexed entry point to simulate `parse_oracle_text`'s
    // wiring of `current_trigger_index` (the trigger is the card's first
    // and only printed trigger → index 0).
    let def = parse_trigger_line_with_index(
            "At the beginning of combat on your turn, ~ becomes a copy of up to one other target creature you control, except her name is ~ and she has this ability. Then put a +1/+1 counter on her.",
            "Irma, Part-Time Mutant",
            Some(0),
            &mut ParseContext::default(),
        );
    // Phase + constraint: BoC on your turn.
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::BeginCombat));
    assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));

    let execute = def.execute.as_deref().expect("execute body must parse");
    // Primary effect: BecomeCopy with target = creature you control + Another.
    match execute.effect.as_ref() {
        Effect::BecomeCopy {
            target,
            additional_modifications,
            ..
        } => {
            match target {
                TargetFilter::Typed(tf) => {
                    assert_eq!(tf.controller, Some(ControllerRef::You));
                    assert!(
                        tf.properties.contains(&FilterProp::Another),
                        "expected Another (other-target) property, got {:?}",
                        tf.properties
                    );
                }
                other => panic!("expected Typed creature target, got {other:?}"),
            }
            // Modifications must include SetName and RetainPrintedTriggerFromSource.
            assert!(
                additional_modifications.iter().any(|m| matches!(
                    m,
                    crate::types::ability::ContinuousModification::SetName { name }
                        if name == "Irma, Part-Time Mutant"
                )),
                "expected SetName(Irma, Part-Time Mutant), got {additional_modifications:?}"
            );
            assert!(
                additional_modifications.iter().any(|m| matches!(
                    m,
                    crate::types::ability::ContinuousModification::RetainPrintedTriggerFromSource {
                        source_trigger_index: 0,
                    }
                )),
                "expected RetainPrintedTriggerFromSource(0), got {additional_modifications:?}"
            );
        }
        other => panic!("expected BecomeCopy primary effect, got {other:?}"),
    }
    // Reflexive sub_ability: PutCounter on her (= SelfRef via is_it_pronoun).
    let sub = execute
        .sub_ability
        .as_deref()
        .expect("Then put a +1/+1 counter on her — sub_ability must chain");
    match sub.effect.as_ref() {
        Effect::PutCounter {
            counter_type,
            target,
            ..
        } => {
            assert_eq!(
                counter_type,
                &crate::types::counter::CounterType::Plus1Plus1
            );
            assert!(
                matches!(target, TargetFilter::SelfRef),
                "+1/+1 counter must land on the post-copy self, got {target:?}"
            );
        }
        other => panic!("expected PutCounter sub_ability, got {other:?}"),
    }
    // CR 115.1d: This singleton "up to one" copy target still uses the
    // legacy optional-targeting flag; multi-target specs are required for
    // variable-count attach choices such as "any number of target Equipment."
    assert!(execute.multi_target.is_none());
    assert!(
        execute.optional_targeting,
        "up to one target must remain optional on the parsed ability"
    );
}

#[test]
fn trigger_become_copy_he_has_this_ability() {
    // Same class, gendered "he" variant — the building block must accept
    // every pronoun without per-card branching.
    let def = parse_trigger_line_with_index(
            "At the beginning of your upkeep, ~ becomes a copy of target creature you control, except his name is ~ and he has this ability.",
            "Test Mutant",
            Some(0),
            &mut ParseContext::default(),
        );
    let execute = def.execute.as_deref().unwrap();
    match execute.effect.as_ref() {
        Effect::BecomeCopy {
            additional_modifications,
            ..
        } => {
            assert!(additional_modifications.iter().any(|m| matches!(
                m,
                crate::types::ability::ContinuousModification::SetName { name }
                    if name == "Test Mutant"
            )));
            assert!(additional_modifications.iter().any(|m| matches!(
                m,
                crate::types::ability::ContinuousModification::RetainPrintedTriggerFromSource {
                    source_trigger_index: 0
                }
            )));
        }
        other => panic!("expected BecomeCopy, got {other:?}"),
    }
}

#[test]
fn trigger_become_copy_it_has_this_ability_neuter() {
    // Class: neuter "it" pronoun in the except clause. The trigger frame
    // here is the simple "[Self] becomes ..." form (Irma family); the
    // alternative "you may have ~ become ..." frame (Cryptoplasm proper)
    // is a distinct grammatical pattern handled by the replacement parser
    // — the building block under test is the except-clause arm regardless
    // of which trigger frame produced the BecomeCopy.
    let def = parse_trigger_line_with_index(
            "At the beginning of your upkeep, ~ becomes a copy of another target creature, except it has this ability.",
            "Test Cloner",
            Some(0),
            &mut ParseContext::default(),
        );
    let execute = def.execute.as_deref().unwrap();
    match execute.effect.as_ref() {
        Effect::BecomeCopy {
            additional_modifications,
            ..
        } => {
            assert!(
                additional_modifications.iter().any(|m| matches!(
                    m,
                    crate::types::ability::ContinuousModification::RetainPrintedTriggerFromSource {
                        source_trigger_index: 0
                    }
                )),
                "trigger with 'except it has this ability' must emit \
                     RetainPrintedTriggerFromSource(0); got {additional_modifications:?}"
            );
        }
        other => panic!("expected BecomeCopy, got {other:?}"),
    }
}

/// CR 109.5 + CR 115.1 + CR 608.2c + CR 611.2c: Gogo, Mysterious Mime —
/// end-to-end trigger parse. The "you may have ~ become a copy ..." frame
/// produces a BecomeCopy execute with a SetName(Gogo) modification, and the
/// "If you do, Gogo and that creature each get +2/+0 and gain haste ... and
/// attack this turn if able" follow-up chains as a 4-link distribution
/// (SelfRef links then ParentTarget links) with NO Unimplemented anywhere.
///
/// This test asserts the FULL desired spec, including that the pump links
/// carry the Haste keyword. The subjectless pump+keyword coalescing path
/// (`coalesce_pump_with_modifications` in `oracle_effect::imperative`) now
/// folds "get +2/+0 and gain haste until end of turn" into a single
/// `GenericEffect { AddPower, AddToughness, AddKeyword(Haste) }`, matching the
/// subject-bound form, so the Haste keyword survives distribution. The
/// "If you do," reflexive frame no longer hides the compound subject from the
/// chunk splitter (sticky-detection strips the leading reflexive connector
/// via the shared `parse_reflexive_conditional_connector` combinator).
#[test]
fn trigger_gogo_distributes_pump_haste_must_attack_to_both() {
    let def = parse_trigger_line_with_index(
            "At the beginning of combat on your turn, you may have ~ become a copy of another target creature you control until end of turn, except its name is ~. If you do, ~ and that creature each get +2/+0 and gain haste until end of turn and attack this turn if able.",
            "Gogo, Mysterious Mime",
            Some(0),
            &mut ParseContext::default(),
        );
    assert_eq!(def.mode, TriggerMode::Phase);
    assert_eq!(def.phase, Some(Phase::BeginCombat));
    assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));

    let execute = def.execute.as_deref().expect("execute body must parse");

    // Walk the entire ability tree and assert NO link is Unimplemented.
    fn assert_no_unimplemented(node: &AbilityDefinition) {
        assert!(
            !matches!(*node.effect, Effect::Unimplemented { .. }),
            "found Unimplemented link in Gogo chain: {:?}",
            node.effect
        );
        if let Some(sub) = node.sub_ability.as_deref() {
            assert_no_unimplemented(sub);
        }
    }
    assert_no_unimplemented(execute);

    // Primary effect: BecomeCopy with SetName(Gogo, Mysterious Mime).
    match execute.effect.as_ref() {
        Effect::BecomeCopy {
            additional_modifications,
            ..
        } => {
            assert!(
                additional_modifications.iter().any(|m| matches!(
                    m,
                    crate::types::ability::ContinuousModification::SetName { name }
                        if name == "Gogo, Mysterious Mime"
                )),
                "expected SetName(Gogo, Mysterious Mime), got {additional_modifications:?}"
            );
        }
        other => panic!("expected BecomeCopy primary effect, got {other:?}"),
    }

    // Collect every distribution-link recipient hanging off the BecomeCopy,
    // and detect whether the Haste keyword survived on any link.
    let mut recipients: Vec<TargetFilter> = Vec::new();
    let mut haste_present = false;
    let mut cursor = execute.sub_ability.as_deref();
    while let Some(link) = cursor {
        match link.effect.as_ref() {
            Effect::Pump { target, .. } => recipients.push(target.clone()),
            Effect::GenericEffect {
                target,
                static_abilities,
                ..
            } => {
                recipients.push(target.clone().expect("GenericEffect needs a recipient"));
                for s in static_abilities {
                    if s.modifications.iter().any(|m| {
                        matches!(
                            m,
                            crate::types::ability::ContinuousModification::AddKeyword {
                                keyword: crate::types::keywords::Keyword::Haste
                            }
                        )
                    }) {
                        haste_present = true;
                    }
                }
            }
            _ => {}
        }
        cursor = link.sub_ability.as_deref();
    }
    // 4-link distribution: first half SelfRef, second half ParentTarget.
    assert_eq!(
        recipients,
        vec![
            TargetFilter::SelfRef,
            TargetFilter::SelfRef,
            TargetFilter::ParentTarget,
            TargetFilter::ParentTarget,
        ],
        "expected SelfRef,SelfRef,ParentTarget,ParentTarget recipient order"
    );
    // The currently-blocking assertion: Haste must be retained on the pump.
    assert!(
        haste_present,
        "Haste keyword must survive distribution across the coalesced \
             pump+keyword GenericEffect links"
    );
}

#[test]
fn trigger_become_copy_without_this_ability_clause_emits_no_retain() {
    // The retain modification must only be emitted when the body contains
    // "<pronoun> has this ability". A copy trigger without that clause
    // must NOT spuriously retain the trigger.
    let def = parse_trigger_line(
        "When ~ enters, ~ becomes a copy of target creature you control.",
        "Vanilla Cloner",
    );
    let execute = def.execute.as_deref().unwrap();
    match execute.effect.as_ref() {
        Effect::BecomeCopy {
            additional_modifications,
            ..
        } => {
            assert!(
                    !additional_modifications.iter().any(|m| matches!(
                        m,
                        crate::types::ability::ContinuousModification::RetainPrintedTriggerFromSource { .. }
                    )),
                    "no 'has this ability' clause → no RetainPrintedTriggerFromSource"
                );
        }
        other => panic!("expected BecomeCopy, got {other:?}"),
    }
}

/// CR 208.1 + CR 603.4 + CR 109.3: Selvala, Heart of the Wilds — the ETB
/// trigger fires unconditionally for every other creature, but the "may
/// draw a card" effect is gated on the triggering creature's power being
/// strictly greater than the max power of every other creature.
/// Regression for the silently-dropped intervening-if condition (#333).
#[test]
fn trigger_intervening_if_selvala_power_greater_than_each_other() {
    let def = parse_trigger_line(
            "Whenever another creature enters, its controller may draw a card if its power is greater than each other creature's power.",
            "Selvala, Heart of the Wilds",
        );
    let Some(TriggerCondition::QuantityComparison {
        lhs,
        comparator,
        rhs,
    }) = &def.condition
    else {
        panic!(
            "expected QuantityComparison intervening-if, got {:?}",
            def.condition
        );
    };
    // LHS: the triggering creature's power.
    assert_eq!(*comparator, Comparator::GT);
    assert_eq!(
        *lhs,
        QuantityExpr::Ref {
            qty: QuantityRef::Power {
                scope: ObjectScope::EventSource,
            },
        }
    );
    // RHS: Max(power) across creatures excluding the triggering object.
    let QuantityExpr::Ref {
        qty:
            QuantityRef::Aggregate {
                function,
                property,
                filter,
            },
    } = rhs
    else {
        panic!("expected Aggregate Max Power rhs, got {rhs:?}");
    };
    assert_eq!(*function, AggregateFunction::Max);
    assert_eq!(*property, crate::types::ability::ObjectProperty::Power);
    let TargetFilter::Typed(tf) = filter else {
        panic!("expected Typed creature filter, got {filter:?}");
    };
    assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
    assert!(
        tf.properties.contains(&FilterProp::OtherThanTriggerObject),
        "expected OtherThanTriggerObject in aggregate filter, got {:?}",
        tf.properties
    );
    // CR 603.4: the source-referential condition is non-re-homeable, so it
    // hoists to the trigger-level condition — it must NOT also re-home onto
    // the clause-level `execute.condition`.
    let execute = def.execute.expect("Selvala trigger must have an execute");
    assert!(
        execute.condition.is_none(),
        "a hoisted non-re-homeable `if` must not also re-home onto the \
             clause-level condition, got {:?}",
        execute.condition,
    );
}

#[test]
fn substitute_another_rewrites_shared_quality_count_filter() {
    let expr = QuantityExpr::Ref {
        qty: QuantityRef::ObjectCountBySharedQuality {
            filter: TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                controller: Some(ControllerRef::You),
                properties: vec![FilterProp::Another],
            }),
            quality: SharedQuality::CreatureType,
            aggregate: AggregateFunction::Max,
        },
    };

    let rewritten = substitute_another_in_expr(&expr);
    let QuantityExpr::Ref {
        qty: QuantityRef::ObjectCountBySharedQuality { filter, .. },
    } = rewritten
    else {
        panic!("expected ObjectCountBySharedQuality, got {rewritten:?}");
    };
    let TargetFilter::Typed(tf) = filter else {
        panic!("expected Typed filter, got {filter:?}");
    };
    assert!(tf.properties.contains(&FilterProp::OtherThanTriggerObject));
    assert!(!tf.properties.contains(&FilterProp::Another));
}

/// Issue #444 — Odric, Lunarch Marshal. The full trigger parses to a
/// single `GenericEffect` carrying one conditioned `StaticDefinition` per
/// keyword (first strike + the 11 from the "the same is true for"
/// sentence). Each `StaticDefinition` grants exactly that keyword and is
/// gated on `IsPresent` of a creature you control with that keyword.
#[test]
fn parse_odric_lunarch_marshal_conditional_keyword_grants() {
    use crate::types::keywords::Keyword;

    let def = parse_trigger_line(
        "At the beginning of combat on your turn, creatures you control gain first \
             strike until end of turn if a creature you control has first strike. The \
             same is true for flying, double strike, deathtouch, haste, hexproof, \
             indestructible, lifelink, menace, reach, trample, and vigilance.",
        "Odric, Lunarch Marshal",
    );
    let execute = def.execute.expect("Odric trigger must have an execute");
    let Effect::GenericEffect {
        static_abilities, ..
    } = &*execute.effect
    else {
        panic!("expected GenericEffect, got {:?}", execute.effect);
    };
    // First strike + 11 "same is true for" keywords = 12 definitions.
    assert_eq!(
        static_abilities.len(),
        12,
        "expected 12 conditioned StaticDefinitions, got {}",
        static_abilities.len()
    );
    // The continuation must NOT remain a separate Unimplemented sub_ability.
    assert!(
        execute.sub_ability.is_none(),
        "the 'same is true for' sentence must fold into static_abilities"
    );

    let expected = [
        Keyword::FirstStrike,
        Keyword::Flying,
        Keyword::DoubleStrike,
        Keyword::Deathtouch,
        Keyword::Haste,
        Keyword::Hexproof,
        Keyword::Indestructible,
        Keyword::Lifelink,
        Keyword::Menace,
        Keyword::Reach,
        Keyword::Trample,
        Keyword::Vigilance,
    ];
    for (sdef, keyword) in static_abilities.iter().zip(expected.iter()) {
        // Grant: one AddKeyword for this keyword.
        assert!(
            sdef.modifications.iter().any(|m| matches!(
                m,
                ContinuousModification::AddKeyword { keyword: k } if k == keyword
            )),
            "static def must grant {keyword:?}, mods: {:?}",
            sdef.modifications
        );
        // Gate: IsPresent of a creature with the SAME keyword.
        let Some(crate::types::ability::StaticCondition::IsPresent {
            filter: Some(TargetFilter::Typed(tf)),
        }) = &sdef.condition
        else {
            panic!(
                "static def for {keyword:?} must be IsPresent-gated, got {:?}",
                sdef.condition
            );
        };
        assert!(
            tf.properties.iter().any(|p| matches!(
                p,
                FilterProp::WithKeyword { value } if value == keyword
            )),
            "gate for {keyword:?} must check WithKeyword({keyword:?}), props: {:?}",
            tf.properties
        );
    }
}

/// Issue #1592 — Odric, Lunarch Marshal's CURRENT (post-errata) Oracle text:
/// the trigger is now "At the beginning of each combat" and the
/// "same is true for" list gained `skulk` (13 keywords total). Each grant
/// must still be an `IsPresent`-gated `StaticDefinition` (gated on a creature
/// you control having that keyword) — NOT an unconditional grant of every
/// keyword. This guards the runtime symptom reported in #1592 ("it's getting
/// all of the abilities") against the current card text.
#[test]
fn parse_odric_current_errata_text_gates_all_thirteen_keywords() {
    use crate::types::keywords::Keyword;

    let def = parse_trigger_line(
        "At the beginning of each combat, creatures you control gain first \
             strike until end of turn if a creature you control has first strike. \
             The same is true for flying, deathtouch, double strike, haste, \
             hexproof, indestructible, lifelink, menace, reach, skulk, trample, \
             and vigilance.",
        "Odric, Lunarch Marshal",
    );
    let execute = def.execute.expect("Odric trigger must have an execute");
    let Effect::GenericEffect {
        static_abilities, ..
    } = &*execute.effect
    else {
        panic!("expected GenericEffect, got {:?}", execute.effect);
    };
    // First strike + 12 "same is true for" keywords = 13 definitions.
    assert_eq!(
        static_abilities.len(),
        13,
        "expected 13 conditioned StaticDefinitions, got {}",
        static_abilities.len()
    );
    assert!(
        execute.sub_ability.is_none(),
        "the 'same is true for' sentence must fold into static_abilities"
    );

    let expected = [
        Keyword::FirstStrike,
        Keyword::Flying,
        Keyword::Deathtouch,
        Keyword::DoubleStrike,
        Keyword::Haste,
        Keyword::Hexproof,
        Keyword::Indestructible,
        Keyword::Lifelink,
        Keyword::Menace,
        Keyword::Reach,
        Keyword::Skulk,
        Keyword::Trample,
        Keyword::Vigilance,
    ];
    for (sdef, keyword) in static_abilities.iter().zip(expected.iter()) {
        // Grant: one AddKeyword for this keyword.
        assert!(
            sdef.modifications.iter().any(|m| matches!(
                m,
                ContinuousModification::AddKeyword { keyword: k } if k == keyword
            )),
            "static def must grant {keyword:?}, mods: {:?}",
            sdef.modifications
        );
        // Gate: IsPresent of a creature YOU CONTROL with the SAME keyword.
        let Some(crate::types::ability::StaticCondition::IsPresent {
            filter: Some(TargetFilter::Typed(tf)),
        }) = &sdef.condition
        else {
            panic!(
                "static def for {keyword:?} must be IsPresent-gated, got {:?}",
                sdef.condition
            );
        };
        assert!(
            tf.properties.iter().any(|p| matches!(
                p,
                FilterProp::WithKeyword { value } if value == keyword
            )),
            "gate for {keyword:?} must check WithKeyword({keyword:?}), props: {:?}",
            tf.properties
        );
        assert_eq!(
            tf.controller,
            Some(crate::types::ability::ControllerRef::You),
            "gate for {keyword:?} must be scoped to creatures you control"
        );
    }
}

/// CR 608.2d + CR 113.3 + CR 611.2: Angelic Skirmisher — "At the beginning
/// of each combat, choose first strike, vigilance, or lifelink. Creatures
/// you control gain that ability until end of turn." The trigger execute
/// chain must (a) prompt a typed `Effect::Choose { ChoiceType::Keyword }`
/// with `persist: true`, then (b) grant `AddChosenKeyword` to creatures you
/// control — never `Effect::Unimplemented`.
#[test]
fn parse_angelic_skirmisher_choose_then_grant_chosen_keyword() {
    use crate::types::ability::ChoiceType;
    use crate::types::keywords::Keyword;

    let def = parse_trigger_line(
        "At the beginning of each combat, choose first strike, vigilance, or \
             lifelink. Creatures you control gain that ability until end of turn.",
        "Angelic Skirmisher",
    );
    let execute = def
        .execute
        .expect("Angelic Skirmisher trigger must have an execute");
    let chain = ability_chain(&execute);

    // (a) The choose clause is a persisting typed keyword choice.
    let choose = chain
        .iter()
        .find_map(|node| match &*node.effect {
            Effect::Choose {
                choice_type,
                persist,
                ..
            } => Some((choice_type.clone(), *persist)),
            _ => None,
        })
        .expect("expected an Effect::Choose in the chain");
    assert_eq!(
        choose.0,
        ChoiceType::Keyword {
            options: vec![Keyword::FirstStrike, Keyword::Vigilance, Keyword::Lifelink],
            count: 1,
        },
        "choose clause must be a typed keyword choice"
    );
    assert!(
        choose.1,
        "keyword choice must persist for the grant to read"
    );

    // (b) The grant clause adds the chosen keyword to creatures you control.
    let granted_chosen = chain.iter().any(|node| match &*node.effect {
        Effect::GenericEffect {
            static_abilities, ..
        } => static_abilities.iter().any(|sdef| {
            sdef.modifications
                .contains(&ContinuousModification::AddChosenKeyword)
        }),
        _ => false,
    });
    assert!(
        granted_chosen,
        "expected an AddChosenKeyword grant, chain: {:?}",
        chain.iter().map(|n| &n.effect).collect::<Vec<_>>()
    );

    // Nothing in the chain may be Unimplemented.
    for node in &chain {
        assert!(
            !matches!(&*node.effect, Effect::Unimplemented { .. }),
            "no clause may be Unimplemented, got {:?}",
            node.effect
        );
    }
}

/// Walk an ability chain (effect + every `sub_ability`) collecting a
/// reference to each `AbilityDefinition` node so tests can inspect both the
/// effect and the per-node `condition`.
fn ability_chain(def: &AbilityDefinition) -> Vec<&AbilityDefinition> {
    let mut out = Vec::new();
    let mut node = Some(def);
    while let Some(d) = node {
        out.push(d);
        node = d.sub_ability.as_deref();
    }
    out
}

/// Issue #3659 — Gix, Yawgmoth Praetor: damage trigger with "its controller may
/// pay … if they do, they draw" must route the draw to the creature's
/// controller (ParentTargetController), not the damaged opponent
/// (TriggeringPlayer from the damage condition scope).
#[test]
fn gix_optional_draw_binds_creature_controller_not_damaged_player() {
    let def = parse_trigger_line(
            "Whenever a creature deals combat damage to one of your opponents, its controller may pay 1 life. If they do, they draw a card.",
            "Gix, Yawgmoth Praetor",
        );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    let execute = def.execute.as_ref().expect("trigger should have execute");
    let chain = ability_chain(execute);
    let draw_node = chain
        .iter()
        .find(|d| matches!(d.effect.as_ref(), Effect::Draw { .. }))
        .expect("chain must contain a Draw node");
    match draw_node.effect.as_ref() {
        Effect::Draw { target, count, .. } => {
            assert_eq!(
                *target,
                TargetFilter::ParentTargetController,
                "Gix draw must go to the creature's controller who paid, not the damaged opponent"
            );
            assert_eq!(*count, QuantityExpr::Fixed { value: 1 });
        }
        other => panic!("expected Draw, got {other:?}"),
    }
    assert_eq!(
        draw_node.condition,
        Some(AbilityCondition::effect_performed()),
        "draw must be gated on optional cost payment"
    );
}

/// Issue #3659 (leak guard) — Gix's "its controller may pay … if they do,
/// they draw" antecedent must not leak into a later independent "that
/// player" clause, which must bind to the damaged opponent (TriggeringPlayer).
#[test]
fn gix_its_controller_antecedent_does_not_leak_to_later_that_player() {
    let def = parse_trigger_line(
            "Whenever a creature deals combat damage to one of your opponents, \
             its controller may pay 1 life. If they do, they draw a card. That player discards a card.",
            "Gix, Yawgmoth Praetor",
        );
    let execute = def.execute.as_ref().expect("trigger should have execute");
    let chain = ability_chain(execute);
    let discard_node = chain
        .iter()
        .find(|d| matches!(d.effect.as_ref(), Effect::Discard { .. }))
        .expect("chain must contain a Discard node");
    match discard_node.effect.as_ref() {
        Effect::Discard { target, .. } => {
            assert_eq!(
                *target,
                TargetFilter::TriggeringPlayer,
                "later 'that player' must be the damaged opponent, not the creature's controller"
            );
        }
        other => panic!("expected Discard, got {other:?}"),
    }
}

/// Issue #1670 — Star Athlete: "Whenever this creature attacks, choose up to
/// one target nonland permanent. Its controller may sacrifice it. If they
/// don't, this creature deals 5 damage to that player." The "that player"
/// recipient of the DealDamage is the chosen permanent's controller (the
/// body "its controller may" antecedent), NOT the attacker's own controller
/// (TriggeringPlayer). And the "If they don't" decline gate must lower to a
/// negated optional-effect-performed condition, not be swallowed.
/// CR 608.2c (read the whole text) + CR 109.4 (controller) + CR 603.12
/// (reflexive "if [a player] doesn't" trigger).
#[test]
fn star_athlete_decline_damage_binds_parent_target_controller() {
    let def = parse_trigger_line(
            "Whenever this creature attacks, choose up to one target nonland permanent. \
             Its controller may sacrifice it. If they don't, this creature deals 5 damage to that player.",
            "Star Athlete",
        );
    assert_eq!(def.mode, TriggerMode::Attacks);
    let execute = def.execute.as_ref().expect("trigger should have execute");
    let chain = ability_chain(execute);
    let damage_node = chain
        .iter()
        .find(|d| matches!(d.effect.as_ref(), Effect::DealDamage { .. }))
        .expect("chain must contain a DealDamage node");
    match damage_node.effect.as_ref() {
        Effect::DealDamage { target, amount, .. } => {
            assert_eq!(
                *target,
                TargetFilter::ParentTargetController,
                "'that player' must bind to the chosen permanent's controller, not the attacker"
            );
            assert_eq!(*amount, QuantityExpr::Fixed { value: 5 });
        }
        other => panic!("expected DealDamage, got {other:?}"),
    }
    assert_eq!(
        damage_node.condition,
        Some(AbilityCondition::Not {
            condition: Box::new(AbilityCondition::effect_performed()),
        }),
        "'If they don't' must lower to a negated optional-effect-performed gate"
    );
}

/// Issue #1670 (leak guard) — an "its controller may" body antecedent must
/// NOT leak into an unrelated later sentence's "that player". After the
/// DealDamage sentence binds (and consumes) the antecedent, the following
/// "That player discards a card." has no live antecedent and falls back to
/// the default TriggeringPlayer recipient.
/// CR 608.2c + CR 109.4.
#[test]
fn star_athlete_its_controller_antecedent_does_not_leak_to_later_that_player() {
    let def = parse_trigger_line(
            "Whenever this creature attacks, choose up to one target nonland permanent. \
             Its controller may sacrifice it. If they don't, this creature deals 5 damage to that player. \
             That player discards a card.",
            "Star Athlete",
        );
    let execute = def.execute.as_ref().expect("trigger should have execute");
    let chain = ability_chain(execute);
    let damage_node = chain
        .iter()
        .find(|d| matches!(d.effect.as_ref(), Effect::DealDamage { .. }))
        .expect("chain must contain a DealDamage node");
    match damage_node.effect.as_ref() {
        Effect::DealDamage { target, .. } => assert_eq!(
            *target,
            TargetFilter::ParentTargetController,
            "DealDamage 'that player' binds to the chosen permanent's controller"
        ),
        other => panic!("expected DealDamage, got {other:?}"),
    }
    let discard_node = chain
        .iter()
        .find(|d| matches!(d.effect.as_ref(), Effect::Discard { .. }))
        .expect("chain must contain a Discard node");
    match discard_node.effect.as_ref() {
        Effect::Discard { target, .. } => assert_eq!(
            *target,
            TargetFilter::TriggeringPlayer,
            "the later 'That player' must NOT leak the consumed antecedent — \
                 it falls back to the default TriggeringPlayer recipient"
        ),
        other => panic!("expected Discard, got {other:?}"),
    }
}

// -----------------------------------------------------------------------
// Taii Wakeen, Perfect Shot — "deals … damage to a creature equal to that
// creature's toughness" damage==recipient-P/T gate, and the anti-over-match
// regression that distinguishes it from "deals damage equal to its power to
// X" (amount = source's power, NOT a recipient-P/T gate).
// -----------------------------------------------------------------------

/// CR 120.1 + CR 208.1 + CR 603.4: the recipient-P/T gate fires only on a
/// genuine object recipient parsed on the success path, producing
/// `QuantityComparison { EventContextAmount EQ Toughness{EventTarget} }`.
#[test]
fn taii_damage_equals_recipient_toughness_gate() {
    let def = parse_trigger_line(
        "Whenever a source you control deals noncombat damage to a creature \
             equal to that creature's toughness, draw a card.",
        "Taii Wakeen, Perfect Shot",
    );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(def.damage_kind, DamageKindFilter::NoncombatOnly);
    assert_eq!(
        def.condition,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Ref {
                qty: QuantityRef::Toughness {
                    scope: ObjectScope::EventTarget,
                },
            },
        }),
        "the damage==toughness intervening-if must compare the dealt amount \
             against the damaged creature's toughness (EventTarget)"
    );
}

/// The "power" variant resolves to `Power{EventTarget}`.
#[test]
fn taii_variant_damage_equals_recipient_power() {
    let def = parse_trigger_line(
        "Whenever a source you control deals noncombat damage to a creature \
             equal to that creature's power, draw a card.",
        "Taii Variant",
    );
    assert_eq!(
        def.condition,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::EventTarget,
                },
            },
        })
    );
}

/// A permanent recipient parses on the object axis and still anchors the gate.
#[test]
fn taii_variant_permanent_recipient() {
    let def = parse_trigger_line(
        "Whenever a source you control deals noncombat damage to a permanent \
             equal to that permanent's toughness, draw a card.",
        "Taii Permanent Variant",
    );
    assert_eq!(def.mode, TriggerMode::DamageDone);
    assert_eq!(
        def.valid_target,
        Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Permanent)))
    );
    assert_eq!(
        def.condition,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Ref {
                qty: QuantityRef::Toughness {
                    scope: ObjectScope::EventTarget,
                },
            },
        })
    );
}

/// Anti-over-match regression (Bionic Blow's clause). "it deals damage equal
/// to its power to up to one other target creature" defines the damage
/// AMOUNT as the SOURCE's power (CR 120.3) — the "equal to" precedes the
/// recipient, so no object recipient parses on the success path and the
/// `EventTarget` gate must NOT be set. This locks the over-match fix.
#[test]
fn deals_damage_equal_to_its_power_does_not_set_event_target_gate() {
    // The subject-led ("it deals …") and source-led ("a source you control
    // deals …") forms both route through the recipient/gate logic; assert
    // neither produces an EventTarget condition for the "equal to its power
    // to X" shape.
    for line in [
        "Whenever a creature you control deals damage equal to its power to \
             another target creature, draw a card.",
        "When a source you control deals damage equal to its power to each \
             other creature, draw a card.",
    ] {
        let def = parse_trigger_line(line, "Over-Match Probe");
        let sets_event_target = matches!(
            &def.condition,
            Some(TriggerCondition::QuantityComparison { rhs, .. })
                if matches!(
                    rhs,
                    QuantityExpr::Ref {
                        qty: QuantityRef::Toughness { scope: ObjectScope::EventTarget }
                            | QuantityRef::Power { scope: ObjectScope::EventTarget },
                    }
                )
        );
        assert!(
            !sets_event_target,
            "'deals damage equal to its power to X' must NOT set an \
                 EventTarget recipient-P/T gate (amount is the source's power, \
                 not a damage==recipient-P/T condition): {line}"
        );
    }
}

/// End-to-end: the full card parses into both abilities (the damage==
/// toughness trigger and the {X}{T} damage-boost activated ability) with no
/// `Effect::Unimplemented` and no swallowed clause.
#[test]
fn taii_wakeen_full_card_parses_both_abilities() {
    let parsed = parse_oracle_text(
        "Whenever a source you control deals noncombat damage to a creature \
             equal to that creature's toughness, draw a card.\n\
             {X}, {T}: If a source you control would deal noncombat damage to a \
             permanent or player this turn, it deals that much damage plus X instead.",
        "Taii Wakeen, Perfect Shot",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &["Human".to_string(), "Mercenary".to_string()],
    );

    // No clause may be swallowed into an Unimplemented effect.
    let has_unimplemented = parsed
        .abilities
        .iter()
        .any(|a| matches!(a.effect.as_ref(), Effect::Unimplemented { .. }))
        || parsed.triggers.iter().any(|t| {
            t.execute
                .as_ref()
                .is_some_and(|e| matches!(e.effect.as_ref(), Effect::Unimplemented { .. }))
        });
    assert!(
        !has_unimplemented,
        "Taii must parse with no Unimplemented effect: {parsed:#?}"
    );

    // Ability 1: the damage==toughness draw trigger.
    assert_eq!(parsed.triggers.len(), 1, "exactly one trigger (ability 1)");
    let trigger = &parsed.triggers[0];
    assert_eq!(trigger.mode, TriggerMode::DamageDone);
    assert_eq!(trigger.damage_kind, DamageKindFilter::NoncombatOnly);
    assert_eq!(
        trigger.condition,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Ref {
                qty: QuantityRef::Toughness {
                    scope: ObjectScope::EventTarget,
                },
            },
        })
    );
    let exec = trigger.execute.as_deref().expect("trigger execute body");
    assert!(
        matches!(exec.effect.as_ref(), Effect::Draw { .. }),
        "ability 1 draws a card, got {:?}",
        exec.effect
    );

    // Ability 2: the {X}{T} damage-boost activated ability installs an
    // AddTargetReplacement with the "plus X" placeholder, NoncombatOnly,
    // end-of-turn expiry.
    assert_eq!(parsed.abilities.len(), 1, "exactly one activated ability");
    let activated = &parsed.abilities[0];
    let Effect::AddTargetReplacement { replacement, .. } = activated.effect.as_ref() else {
        panic!(
            "ability 2 must be AddTargetReplacement, got {:?}",
            activated.effect
        );
    };
    assert_eq!(
        replacement.damage_modification,
        Some(DamageModification::Plus {
            value: crate::types::ability::QuantityExpr::Fixed { value: 0 }
        }),
        "the 'plus X' placeholder is frozen at activation, not parse time"
    );
    assert_eq!(
        replacement.combat_scope,
        Some(crate::types::ability::CombatDamageScope::NoncombatOnly)
    );
    assert_eq!(
        replacement.expiry,
        Some(crate::types::ability::RestrictionExpiry::EndOfTurn),
        "the boost lasts until end of turn"
    );
}

/// CR 108.3 + CR 109.4 + CR 603.4: Agent of Treachery's end-step draw is
/// gated by an intervening-if — "if you control three or more permanents you
/// don't own". The bare "you don't own" negated-ownership suffix must be
/// consumed by the type-phrase parser so the count condition reaches a word
/// boundary and the intervening-if is hoisted onto the trigger. Before the
/// fix the suffix was unconsumed, the condition was discarded
/// (`condition: None`), and Agent drew three cards unconditionally every end
/// step (#3304).
#[test]
fn agent_of_treachery_end_step_draw_is_gated_by_not_owned_count() {
    let parsed = parse_oracle_text(
        "When this creature enters, gain control of target permanent.\n\
             At the beginning of your end step, if you control three or more \
             permanents you don't own, draw three cards.",
        "Agent of Treachery",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Rogue".to_string()],
    );

    assert!(
        parsed.parse_warnings.is_empty(),
        "no parse warnings expected, got {:?}",
        parsed.parse_warnings
    );

    let end_step = parsed
        .triggers
        .iter()
        .find(|t| t.phase == Some(Phase::End))
        .expect("an end-step (Phase::End) trigger must be parsed");

    // The intervening-if must survive as a QuantityComparison, NOT be dropped.
    let Some(TriggerCondition::QuantityComparison {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        },
        comparator: Comparator::GE,
        rhs: QuantityExpr::Fixed { value: 3 },
    }) = end_step.condition.clone()
    else {
        panic!(
            "end-step trigger must carry ObjectCount >= 3 intervening-if, got {:?}",
            end_step.condition
        );
    };

    let TargetFilter::Typed(tf) = filter else {
        panic!("count filter must be Typed, got {filter:?}");
    };
    assert_eq!(
        tf.controller,
        Some(ControllerRef::You),
        "the counted permanents are ones YOU control (CR 109.4)"
    );
    assert!(
        tf.properties.contains(&FilterProp::Owned {
            controller: ControllerRef::Opponent,
        }),
        "the counted permanents are ones you DON'T own — Owned{{Opponent}} \
             (runtime: owner != controller); got {:?}",
        tf.properties
    );

    // The execute body still draws three cards.
    let exec = end_step.execute.as_deref().expect("end-step execute body");
    assert!(
        matches!(exec.effect.as_ref(), Effect::Draw { .. }),
        "end-step trigger draws cards, got {:?}",
        exec.effect
    );
}

/// CR 702.11a + CR 603.1: "creatures you control can't be the targets of
/// spells or abilities your opponents control this turn" — the effect body
/// starts with a type word ("creatures") but the negated modal "can't"
/// indicates a new subject-predicate sentence, not a type-list continuation.
/// Verify the trigger boundary splits correctly and the Hexproof grant parses.
#[test]
fn trigger_veilstone_amulet_cant_be_targets() {
    let def = parse_trigger_line(
            "Whenever you cast a spell, creatures you control can't be the targets of spells or abilities your opponents control this turn.",
            "Veilstone Amulet",
        );
    assert_eq!(def.mode, TriggerMode::SpellCast);
    let execute = def.execute.as_deref().expect(
        "Veilstone Amulet trigger execute must not be None — \
             the boundary splitter should classify 'creatures you control can't ...' \
             as a new sentence (negated modal 'can't' is a predicate verb)",
    );
    assert!(
        matches!(execute.effect.as_ref(), Effect::GenericEffect { .. }),
        "expected GenericEffect (hexproof grant), got {:?}",
        execute.effect
    );
}

/// CR 508.1c + CR 611.2c: Bumi, Unleashed — a triggered additional combat
/// phase whose "Only land creatures can attack during that combat phase"
/// rider must fold onto the `AdditionalPhase` (Typed restriction,
/// re-evaluated continuously) rather than surfacing as an Unimplemented gap.
#[test]
fn triggered_additional_combat_folds_land_creature_attacker_restriction() {
    let def = parse_trigger_line(
        "Whenever Bumi deals combat damage to a player, untap all lands you control. \
         After this phase, there is an additional combat phase. Only land creatures \
         can attack during that combat phase.",
        "Bumi, Unleashed",
    );

    let mut node = def.execute.as_deref();
    let mut saw_restriction = false;
    while let Some(ability) = node {
        assert!(
            !matches!(ability.effect.as_ref(), Effect::Unimplemented { .. }),
            "no Unimplemented node may remain after the fold"
        );
        if let Effect::AdditionalPhase {
            phase: Phase::BeginCombat,
            attacker_restriction: Some(TargetFilter::Typed(tf)),
            ..
        } = ability.effect.as_ref()
        {
            // "land creatures" -> a typed land+creature filter (Bumi class).
            assert!(
                tf.type_filters.contains(&TypeFilter::Land)
                    && tf.type_filters.contains(&TypeFilter::Creature),
                "restriction must be the land-creature typed filter, got {tf:?}"
            );
            saw_restriction = true;
        }
        node = ability.sub_ability.as_deref();
    }
    assert!(
        saw_restriction,
        "the additional combat phase must carry a Typed land-creature restriction"
    );
}

#[test]
fn high_tide_runtime_bonus_mana_routes_to_triggering_player_and_expires_at_eot() {
    // CR 603.7b + CR 106.12a + CR 605.1a (issue #4673): End-to-end runtime proof
    // for High Tide's real Oracle text. The instant creates an until-end-of-turn
    // multi-fire delayed trigger. Whenever ANY player taps an Island for mana,
    // THAT player (TriggeringPlayer, not the caster) gets an additional {U}.
    // The delayed trigger is purged at end-of-turn cleanup (CR 603.7b).
    use crate::game::scenario::GameScenario;
    use crate::game::triggers::check_delayed_triggers;
    use crate::game::turns::execute_cleanup;
    use crate::types::events::{GameEvent, ManaTapState};
    use crate::types::identifiers::ObjectId;
    use crate::types::phase::Phase;
    use crate::types::player::PlayerId;

    fn blue(runner: &crate::game::scenario::GameRunner, p: PlayerId) -> usize {
        runner
            .state()
            .players
            .iter()
            .find(|ps| ps.id == p)
            .unwrap()
            .mana_pool
            .count_color(ManaType::Blue)
    }

    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let hi = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "High Tide",
            true,
            "Until end of turn, whenever a player taps an Island for mana, that player adds an additional {U}.",
        )
        .id();
    let isl0 = scenario.add_basic_land(P0, ManaColor::Blue);
    let isl1 = scenario.add_basic_land(P1, ManaColor::Blue);
    // Fund P0's {U} to cast High Tide through the real pipeline (registers the
    // delayed trigger authentically).
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Blue, ObjectId(9999), false, vec![])],
    );
    let mut runner = scenario.build();
    runner.cast(hi).resolve();

    // The instant registered exactly one until-end-of-turn delayed trigger.
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "High Tide must register its delayed trigger on resolution"
    );
    // Clear any residual pool mana from the cast so bonus deltas read cleanly.
    for p in [P0, P1] {
        runner
            .state_mut()
            .players
            .iter_mut()
            .find(|ps| ps.id == p)
            .unwrap()
            .mana_pool
            .clear();
    }

    // (1) P0 taps its own Island for mana. Simulate the base {U} the land
    // produced (CR 106.12a) and fire the delayed trigger; P0 must gain the
    // ADDITIONAL {U} on top (net +2 blue in the pool: base + bonus).
    runner
        .state_mut()
        .add_mana_to_pool(P0, ManaUnit::new(ManaType::Blue, isl0, false, vec![]));
    check_delayed_triggers(
        runner.state_mut(),
        &[GameEvent::TappedForMana {
            player_id: PlayerId(0),
            source_id: isl0,
            produced: vec![ManaType::Blue],
            tap_state: ManaTapState::FromTap,
        }],
    );
    assert_eq!(
        blue(&runner, P0),
        2,
        "P0 taps its own Island: base {{U}} + additional {{U}} = 2 blue"
    );
    assert_eq!(blue(&runner, P1), 0, "P1 gets nothing when P0 taps");

    // (2) P1 (a DIFFERENT player, not the caster) taps THEIR Island. The bonus
    // must go to P1's pool — proving the recipient is TriggeringPlayer, not the
    // caster P0.
    runner
        .state_mut()
        .add_mana_to_pool(P1, ManaUnit::new(ManaType::Blue, isl1, false, vec![]));
    check_delayed_triggers(
        runner.state_mut(),
        &[GameEvent::TappedForMana {
            player_id: PlayerId(1),
            source_id: isl1,
            produced: vec![ManaType::Blue],
            tap_state: ManaTapState::FromTap,
        }],
    );
    assert_eq!(
        blue(&runner, P1),
        2,
        "P1 taps their Island: bonus routes to P1 (TriggeringPlayer), not caster P0"
    );
    assert_eq!(
        blue(&runner, P0),
        2,
        "P0's pool is unchanged when P1 taps — bonus is NOT the caster's"
    );

    // (3) End-of-turn cleanup purges the multi-fire delayed trigger (CR 603.7b).
    let mut cleanup_events = Vec::new();
    execute_cleanup(runner.state_mut(), &mut cleanup_events);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        0,
        "the until-end-of-turn delayed trigger must be purged at cleanup"
    );

    // A subsequent Island tap yields NO bonus (the WheneverEvent is gone).
    for p in [P0, P1] {
        runner
            .state_mut()
            .players
            .iter_mut()
            .find(|ps| ps.id == p)
            .unwrap()
            .mana_pool
            .clear();
    }
    runner
        .state_mut()
        .add_mana_to_pool(P0, ManaUnit::new(ManaType::Blue, isl0, false, vec![]));
    check_delayed_triggers(
        runner.state_mut(),
        &[GameEvent::TappedForMana {
            player_id: PlayerId(0),
            source_id: isl0,
            produced: vec![ManaType::Blue],
            tap_state: ManaTapState::FromTap,
        }],
    );
    assert_eq!(
        blue(&runner, P0),
        1,
        "after cleanup: only the base {{U}}, no bonus — the delayed trigger expired"
    );
}

/// CR 614.12: Summoner's Grimoire's granted ability — the leading
/// "if that card is an enchantment card" must materialize an
/// `enters_modified_if` gate on the absorbed ChangeZone (via `parse_type_phrase`),
/// not be silently dropped while applying the riders unconditionally.
#[test]
fn grimoire_granted_trigger_gates_enters_on_moved_object_type() {
    let def = parse_trigger_line(
            "Whenever this creature attacks, you may put a creature card from your hand onto the battlefield. If that card is an enchantment card, it enters tapped and attacking.",
            "Summoner's Grimoire",
        );
    let exec = def.execute.as_ref().expect("expected execute");
    match &*exec.effect {
        Effect::ChangeZone {
            enter_tapped,
            enters_attacking,
            enters_modified_if,
            ..
        } => {
            assert!(enter_tapped.is_tapped(), "enter_tapped must be Tapped");
            assert!(*enters_attacking, "enters_attacking must be set");
            match enters_modified_if {
                Some(TargetFilter::Typed(tf)) => assert!(
                    tf.type_filters.contains(&TypeFilter::Enchantment),
                    "gate must materialize an Enchantment card-type filter, got {tf:?}"
                ),
                other => {
                    panic!("expected enters_modified_if = Some(Typed(Enchantment)), got {other:?}")
                }
            }
        }
        other => panic!("expected ChangeZone execute, got {other:?}"),
    }
}

/// CR 508.4 — negative: a follow-on "It enters tapped and attacking" with NO
/// leading moved-object condition (Stangg / Shark Shredder) keeps both flags
/// AND leaves `enters_modified_if` as `None` (unconditional).
#[test]
fn grimoire_unconditional_enters_leaves_gate_none() {
    let def = parse_trigger_line(
            "When this creature enters, put a creature card from your hand onto the battlefield. It enters tapped and attacking.",
            "Unconditional Put",
        );
    let exec = def.execute.as_ref().expect("expected execute");
    match &*exec.effect {
        Effect::ChangeZone {
            enter_tapped,
            enters_attacking,
            enters_modified_if,
            ..
        } => {
            assert!(enter_tapped.is_tapped());
            assert!(*enters_attacking);
            assert!(
                enters_modified_if.is_none(),
                "no leading condition -> gate must stay None, got {enters_modified_if:?}"
            );
        }
        other => panic!("expected ChangeZone execute, got {other:?}"),
    }
}

/// Issue #4356 — Trouble in Pairs disjunctive trigger must split into three
/// independent triggers instead of misparsing the draw/cast clauses as effect text.
#[test]
fn trouble_in_pairs_disjunctive_trigger_splits_into_three_triggers() {
    let text = "Whenever an opponent attacks you with two or more creatures, draws their second card each turn, or casts their second spell each turn, you draw a card.";
    let triggers = parse_trigger_lines(text, "Trouble in Pairs");
    assert_eq!(
        triggers.len(),
        3,
        "expected three triggers, got {triggers:?}"
    );

    assert_eq!(triggers[0].mode, TriggerMode::YouAttack);
    assert!(matches!(
        triggers[0].condition,
        Some(TriggerCondition::AttackersDeclaredCount {
            subject: AttackersDeclaredCountSubject::AttackTarget {
                controller: ControllerRef::You,
                attacked: AttackTargetFilter::Player,
                filter: None,
            },
            comparator: Comparator::GE,
            count: 2,
        })
    ));

    assert_eq!(triggers[1].mode, TriggerMode::Drawn);
    assert!(matches!(
        triggers[1].constraint,
        Some(TriggerConstraint::NthDrawThisTurn { n: 2, .. })
    ));

    assert_eq!(triggers[2].mode, TriggerMode::SpellCast);
    assert!(matches!(
        triggers[2].constraint,
        Some(TriggerConstraint::NthSpellThisTurn { n: 2, .. })
    ));

    for trigger in &triggers {
        assert!(
            !matches!(
                trigger.execute.as_ref().map(|e| e.effect.as_ref()),
                Some(Effect::Unimplemented { .. })
            ),
            "trigger execute must not be Unimplemented: {trigger:?}"
        );
    }
}

#[test]
fn trouble_in_pairs_opponent_attacks_you_with_two_or_more_creatures() {
    let def = parse_trigger_line(
        "Whenever an opponent attacks you with two or more creatures, you draw a card.",
        "Trouble in Pairs",
    );
    assert_eq!(def.mode, TriggerMode::YouAttack);
    assert_eq!(def.attack_target_filter, Some(AttackTargetFilter::Player));
    assert!(matches!(
        def.condition,
        Some(TriggerCondition::AttackersDeclaredCount {
            subject: AttackersDeclaredCountSubject::AttackTarget {
                controller: ControllerRef::You,
                attacked: AttackTargetFilter::Player,
                filter: None,
            },
            comparator: Comparator::GE,
            count: 2,
        })
    ));
}

#[test]
fn opponent_attacks_you_with_two_or_more_dinosaurs_carries_type_filter() {
    let def = parse_trigger_line(
        "Whenever an opponent attacks you with two or more Dinosaurs, you draw a card.",
        "Trouble in Pairs",
    );
    assert_eq!(def.mode, TriggerMode::YouAttack);
    assert_eq!(def.attack_target_filter, Some(AttackTargetFilter::Player));
    match &def.valid_card {
        Some(TargetFilter::Typed(tf)) => assert!(
            tf.type_filters
                .iter()
                .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "Dinosaur")),
            "expected Dinosaur subtype in valid_card, got {:?}",
            tf.type_filters,
        ),
        other => panic!("expected Typed valid_card with Dinosaur, got {other:?}"),
    }
    match &def.condition {
        Some(TriggerCondition::AttackersDeclaredCount {
            subject:
                AttackersDeclaredCountSubject::AttackTarget {
                    controller: ControllerRef::You,
                    attacked: AttackTargetFilter::Player,
                    filter: Some(TargetFilter::Typed(tf)),
                },
            comparator: Comparator::GE,
            count: 2,
        }) => assert!(
            tf.type_filters
                .iter()
                .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "Dinosaur")),
            "expected Dinosaur subtype in attack-target count filter, got {:?}",
            tf.type_filters,
        ),
        other => panic!(
            "expected AttackersDeclaredCount {{ AttackTarget {{ You, Player, Some(Dinosaur) }}, GE, 2 }}, got {other:?}"
        ),
    }
}
