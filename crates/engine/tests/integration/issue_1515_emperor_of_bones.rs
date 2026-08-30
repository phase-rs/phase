//! Issue #1515 — Emperor of Bones must grant haste to, and later sacrifice,
//! the creature returned from its linked exile set.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::Duration;
use engine::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, ChoiceType, ChosenAttribute,
    ContinuousModification, ControllerRef, DelayedTriggerCondition, Effect, EffectScope,
    FilterProp, QuantityExpr, QuantityRef, ReplacementDefinition, ResolvedAbility,
    StaticDefinition, SubAbilityLink, TapStateChange, TargetChoiceTiming, TargetFilter, TypeFilter,
    TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, ExileLink, ExileLinkKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

const EMPEROR_COUNTER_TRIGGER_EFFECT: &str = "put a creature card exiled with this creature onto \
the battlefield under your control with a finality counter on it. it gains haste. sacrifice it at \
the beginning of the next end step.";
const EMPEROR_ORACLE: &str =
    "At the beginning of combat on your turn, exile up to one target card from a graveyard.\n\
{1}{B}: Adapt 2.\n\
Whenever one or more +1/+1 counters are put on this creature, put a creature card exiled with this \
creature onto the battlefield under your control with a finality counter on it. It gains haste. \
Sacrifice it at the beginning of the next end step.";
const PUT_COUNTER_ORACLE: &str = "Put a +1/+1 counter on target creature.";
const YAWGMOTHS_VILE_OFFERING_ORACLE: &str = "Put up to one target creature or planeswalker card from a graveyard onto the battlefield under your control. Destroy up to one target creature or planeswalker. Exile Yawgmoth's Vile Offering.";
const REANIMATION_RESPONSE_ORACLE: &str =
    "Return target creature card from a graveyard to the battlefield under your control.";

const ANOINTED_PEACEKEEPER: &str = "Vigilance\n\
As this creature enters, look at an opponent's hand, then choose any card name.\n\
Spells your opponents cast with the chosen name cost {2} more to cast.\n\
Activated abilities of sources with the chosen name cost {2} more to activate unless they're mana abilities.";

const P1: PlayerId = PlayerId(1);
const NAMED_CARD: &str = "Llanowar Elves";

fn creature_has_haste_from_transient_effects(
    state: &engine::types::game_state::GameState,
    creature: ObjectId,
) -> bool {
    state.transient_continuous_effects.iter().any(|effect| {
        effect.affected == TargetFilter::SpecificObject { id: creature }
            && effect.modifications.iter().any(|modification| {
                matches!(
                    modification,
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Haste
                    }
                )
            })
    })
}

#[test]
fn issue_1515_emperor_of_bones_binds_haste_and_delayed_sacrifice_to_returned_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let emperor = scenario.add_creature(P0, "Emperor of Bones", 2, 2).id();
    let returned = scenario
        .add_creature_to_exile(P0, "Linked Gravebeast", 3, 3)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().exile_links.push(ExileLink {
        exiled_id: returned,
        source_id: emperor,
        kind: ExileLinkKind::TrackedBySource,
    });

    let def = parse_effect_chain(EMPEROR_COUNTER_TRIGGER_EFFECT, AbilityKind::Spell);
    let ability = build_resolved_from_def(&def, emperor, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("Emperor of Bones counter-trigger effect must resolve");

    let state = runner.state();
    assert_eq!(
        state.objects[&returned].zone,
        Zone::Battlefield,
        "linked creature card must be returned to the battlefield"
    );
    assert_eq!(
        state.objects[&emperor].zone,
        Zone::Battlefield,
        "Emperor must remain on the battlefield after returning the linked creature"
    );
    assert_eq!(
        state.objects[&returned]
            .counters
            .get(&CounterType::Finality)
            .copied()
            .unwrap_or(0),
        1,
        "returned creature must enter with a finality counter"
    );
    assert!(
        creature_has_haste_from_transient_effects(state, returned),
        "haste grant must bind to the returned creature, not Emperor"
    );
    assert!(
        !creature_has_haste_from_transient_effects(state, emperor),
        "Emperor itself must not receive the returned creature's haste grant"
    );
    assert_eq!(
        state.delayed_triggers.len(),
        1,
        "resolution must install exactly one delayed sacrifice trigger"
    );
    assert!(matches!(
        state.delayed_triggers[0].condition,
        DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
    ));
    assert_eq!(
        state.delayed_triggers[0].ability.targets,
        vec![engine::types::ability::TargetRef::Object(returned)],
        "delayed sacrifice trigger must snapshot the returned creature"
    );
    assert!(
        matches!(
            &state.delayed_triggers[0].ability.effect,
            Effect::Sacrifice {
                target: TargetFilter::ParentTarget,
                ..
            }
        ),
        "delayed trigger effect must sacrifice the snapshotted returned creature"
    );

    let mut guard = 0;
    while !runner.state().delayed_triggers.is_empty() || !runner.state().stack.is_empty() {
        guard += 1;
        assert!(
            guard < 256,
            "delayed sacrifice trigger never fired; phase = {:?}, waiting_for = {:?}, \
             delayed_triggers = {}, stack = {}",
            runner.state().phase,
            runner.state().waiting_for,
            runner.state().delayed_triggers.len(),
            runner.state().stack.len(),
        );
        match runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => runner
                .act(engine::types::actions::GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                })
                .expect("declare no attackers while advancing to end step"),
            WaitingFor::DeclareBlockers { .. } => runner
                .act(engine::types::actions::GameAction::DeclareBlockers {
                    assignments: vec![],
                })
                .expect("declare no blockers while advancing to end step"),
            _ => runner
                .act(engine::types::actions::GameAction::PassPriority)
                .expect("priority pass while waiting for delayed sacrifice"),
        };
    }

    assert_eq!(
        runner.state().objects[&returned].zone,
        Zone::Exile,
        "returned creature must be sacrificed at the beginning of the next end step; \
         its finality counter sends it to exile"
    );
    assert_eq!(
        runner.state().objects[&emperor].zone,
        Zone::Battlefield,
        "the delayed sacrifice must not sacrifice Emperor"
    );
}

/// CR 122.1 + CR 603.2 + CR 608.2c: Drive the printed counter trigger through
/// the reducer so the returned creature, rather than the trigger source, owns
/// both anaphoric riders.
#[test]
fn emperor_of_bones_counter_trigger_uses_returned_creature_in_cast_pipeline() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let emperor = scenario
        .add_creature_from_oracle(P0, "Emperor of Bones", 2, 2, EMPEROR_ORACLE)
        .id();
    let returned = scenario
        .add_creature_to_exile(P0, "Linked Gravebeast", 3, 3)
        .id();
    let counter_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Counter Placement", false, PUT_COUNTER_ORACLE)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().exile_links.push(ExileLink {
        exiled_id: returned,
        source_id: emperor,
        kind: ExileLinkKind::TrackedBySource,
    });

    runner.cast(counter_spell).target_object(emperor).resolve();

    let state = runner.state();
    assert_eq!(
        state.objects[&returned].zone,
        Zone::Battlefield,
        "the counter trigger must return the linked creature through apply()"
    );
    assert_eq!(
        state.objects[&returned]
            .counters
            .get(&CounterType::Finality)
            .copied()
            .unwrap_or(0),
        1,
        "the returned creature must receive Emperor's finality entry modifier"
    );
    assert_eq!(
        state.objects[&emperor].zone,
        Zone::Battlefield,
        "the counter trigger must not sacrifice Emperor while resolving"
    );
    assert!(
        creature_has_haste_from_transient_effects(state, returned),
        "the printed haste rider must bind to the returned creature"
    );
    assert!(
        !creature_has_haste_from_transient_effects(state, emperor),
        "the printed haste rider must not bind to Emperor"
    );
    assert_eq!(state.delayed_triggers.len(), 1);
    assert_eq!(
        state.delayed_triggers[0].ability.targets,
        vec![engine::types::ability::TargetRef::Object(returned)],
        "the delayed sacrifice must snapshot the returned creature"
    );
}

#[test]
fn emperor_of_bones_adapt_pipeline_binds_delayed_sacrifice_to_returned_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let emperor = scenario
        .add_creature_from_oracle(P0, "Emperor of Bones", 2, 2, EMPEROR_ORACLE)
        .id();
    let returned = scenario
        .add_creature_to_exile(P0, "Linked Gravebeast", 3, 3)
        .id();
    let swamp_a = scenario.add_basic_land(P0, engine::types::mana::ManaColor::Black);
    let swamp_b = scenario.add_basic_land(P0, engine::types::mana::ManaColor::Black);

    let mut runner = scenario.build();
    runner.state_mut().exile_links.push(ExileLink {
        exiled_id: returned,
        source_id: emperor,
        kind: ExileLinkKind::TrackedBySource,
    });

    runner
        .activate(emperor, 0)
        .pay_with(&[swamp_a, swamp_b])
        .resolve();

    let state = runner.state();
    assert_eq!(
        state.objects[&returned].zone,
        Zone::Battlefield,
        "Adapt must resolve Emperor's counter trigger and return the linked creature"
    );
    assert_eq!(
        state.delayed_triggers.len(),
        1,
        "the counter trigger must install one delayed sacrifice"
    );
    assert_eq!(
        state.delayed_triggers[0].ability.targets,
        vec![engine::types::ability::TargetRef::Object(returned)],
        "the Adapt-triggered delayed sacrifice must snapshot the returned creature"
    );
    assert_eq!(
        state.objects[&emperor].zone,
        Zone::Battlefield,
        "Emperor must remain on the battlefield until its own ability is removed"
    );
}

#[test]
fn emperor_of_bones_adapt_without_linked_exile_has_no_riders_to_apply() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let emperor = scenario
        .add_creature_from_oracle(P0, "Emperor of Bones", 2, 2, EMPEROR_ORACLE)
        .id();
    let swamp_a = scenario.add_basic_land(P0, engine::types::mana::ManaColor::Black);
    let swamp_b = scenario.add_basic_land(P0, engine::types::mana::ManaColor::Black);

    let mut runner = scenario.build();
    runner
        .activate(emperor, 0)
        .pay_with(&[swamp_a, swamp_b])
        .resolve();

    let state = runner.state();
    assert_eq!(
        state.objects[&emperor]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        2,
        "Adapt must still put its counters on Emperor"
    );
    assert_eq!(
        state.delayed_triggers.len(),
        0,
        "no returned creature means Emperor's haste and delayed Sacrifice riders must not run"
    );
    assert!(
        !creature_has_haste_from_transient_effects(state, emperor),
        "Emperor must not receive the returned creature's haste rider"
    );
    assert_eq!(
        state.objects[&emperor].zone,
        Zone::Battlefield,
        "Emperor must remain on the battlefield when no linked creature was exiled"
    );
}

/// Build a spell-shaped forward-result chain and drive it through the public
/// cast/apply pipeline. Its optional graveyard move selects nothing, exercising
/// the same empty-forward-result branch as an illegal target at resolution.
fn empty_forward_result_generic_spell(
    static_abilities: Vec<StaticDefinition>,
    target: Option<TargetFilter>,
) -> AbilityDefinition {
    let mut independent_draw = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    );
    independent_draw.sub_link = SubAbilityLink::SequentialSibling;
    let dependent_continuation = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    )
    .sub_ability(independent_draw);
    let generic = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GenericEffect {
            static_abilities,
            duration: Some(Duration::UntilEndOfTurn),
            target,
            end_cost: None,
        },
    )
    .sub_ability(dependent_continuation);
    let mut root = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChangeZone {
            origin: Some(Zone::Graveyard),
            destination: Zone::Battlefield,
            target: TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                controller: None,
                properties: vec![FilterProp::InZone {
                    zone: Zone::Graveyard,
                }],
            }),
            owner_library: false,
            enter_transformed: false,
            enters_under: Some(ControllerRef::You),
            enter_tapped: engine::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: true,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
    )
    .sub_ability(generic);
    root.forward_result = true;
    root.target_choice_timing = TargetChoiceTiming::Resolution;
    root
}

fn resolve_empty_forward_result_generic_spell(
    static_abilities: Vec<StaticDefinition>,
    target: Option<TargetFilter>,
) -> engine::types::game_state::GameState {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["independent sibling draw"]);
    let spell = scenario
        .add_spell_to_hand(P0, "Empty Forward Generic", false)
        .with_ability_definition(empty_forward_result_generic_spell(static_abilities, target))
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: runner.state().objects[&spell].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("synthetic spell announcement must be accepted");
    runner.advance_until_stack_empty();
    runner.state().clone()
}

/// The exact Princess Yue regression shape: a no-op forwarded move followed by
/// an all-effective-SelfRef GenericEffect must skip that grant while preserving
/// an independent sequential sibling in the normal cast/apply pipeline.
#[test]
fn empty_forward_result_skips_all_self_ref_generic_effect_but_runs_independent_sibling() {
    let state = resolve_empty_forward_result_generic_spell(
        vec![StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Haste,
            }])],
        None,
    );
    assert_eq!(state.players[P0.0 as usize].hand.len(), 1);
    assert_eq!(state.players[P0.0 as usize].life, 20);
    assert!(
        state.transient_continuous_effects.is_empty(),
        "the missing forwarded object must suppress the all-SelfRef grant"
    );
}

/// `affected: TriggeringSource` overrides an outer SelfRef descriptor. The
/// GenericEffect must remain executable after an empty forwarded move, so its
/// dependent continuation runs before the independent sibling.
#[test]
fn empty_forward_result_preserves_triggering_source_generic_effect() {
    let state = resolve_empty_forward_result_generic_spell(
        vec![StaticDefinition::continuous()
            .affected(TargetFilter::TriggeringSource)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Haste,
            }])],
        Some(TargetFilter::SelfRef),
    );
    assert_eq!(state.players[P0.0 as usize].hand.len(), 1);
    assert_eq!(state.players[P0.0 as usize].life, 21);
    assert!(
        state.transient_continuous_effects.is_empty(),
        "the inner TriggeringSource application filter must override the outer SelfRef; \
         without an event-context source, no transient may bind to the spell source"
    );
}

/// `affected: CostPaidObject` likewise overrides outer SelfRef. It must not be
/// pruned merely because the outer descriptor is SelfRef.
#[test]
fn empty_forward_result_preserves_cost_paid_object_generic_effect() {
    let state = resolve_empty_forward_result_generic_spell(
        vec![StaticDefinition::continuous()
            .affected(TargetFilter::CostPaidObject)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Haste,
            }])],
        Some(TargetFilter::SelfRef),
    );
    assert_eq!(state.players[P0.0 as usize].hand.len(), 1);
    assert_eq!(state.players[P0.0 as usize].life, 21);
    assert!(
        state.transient_continuous_effects.is_empty(),
        "the inner CostPaidObject application filter must override the outer SelfRef; \
         without a cost-paid object, no transient may bind to the spell source"
    );
}

/// `affected: ParentTarget` is the third inherited-target reference and behaves
/// exactly like its `TriggeringSource` / `CostPaidObject` siblings: the outer
/// SelfRef descriptor must not pin the grant to the spell source. With no
/// chosen target, no cost-paid object, and an empty tracked set, the dedicated
/// resolution-local arm binds nothing at all.
#[test]
fn empty_forward_result_preserves_parent_target_generic_effect() {
    let state = resolve_empty_forward_result_generic_spell(
        vec![StaticDefinition::continuous()
            .affected(TargetFilter::ParentTarget)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Haste,
            }])],
        Some(TargetFilter::SelfRef),
    );
    assert_eq!(state.players[P0.0 as usize].hand.len(), 1);
    assert_eq!(state.players[P0.0 as usize].life, 21);
    assert!(
        state.transient_continuous_effects.is_empty(),
        "the inner ParentTarget application filter must override the outer SelfRef; \
         without a parent binding, no transient may bind to the spell source"
    );
}

/// `affected: AmassedArmy` is the fourth inherited-target reference (CR 701.47c)
/// and must reach its `amassed_army_object` arm rather than the outer SelfRef
/// short-circuit. With no Army stamped on the resolution, nothing binds.
#[test]
fn empty_forward_result_preserves_amassed_army_generic_effect() {
    let state = resolve_empty_forward_result_generic_spell(
        vec![StaticDefinition::continuous()
            .affected(TargetFilter::AmassedArmy)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Haste,
            }])],
        Some(TargetFilter::SelfRef),
    );
    assert_eq!(state.players[P0.0 as usize].hand.len(), 1);
    assert_eq!(state.players[P0.0 as usize].life, 21);
    assert!(
        state.transient_continuous_effects.is_empty(),
        "the inner AmassedArmy application filter must override the outer SelfRef; \
         without an amassed Army, no transient may bind to the spell source"
    );
}

/// Creature filter used as the innocuous second member of the combinator cases
/// below — present only so the combinator has something to combine `SelfRef`
/// with.
fn any_creature_filter() -> TargetFilter {
    TargetFilter::Typed(TypedFilter {
        type_filters: vec![TypeFilter::Creature],
        controller: None,
        properties: vec![],
    })
}

/// CR 608.2c: `And` is satisfied only when EVERY member matches, so an effective
/// `And { [SelfRef, ...] }` still names the object the preceding instruction
/// failed to produce. It must prune exactly like a bare `SelfRef`: the grant is
/// dropped, the dependent continuation is skipped, and only the independent
/// sequential sibling runs.
#[test]
fn empty_forward_result_prunes_conjunctive_self_ref_static() {
    let state = resolve_empty_forward_result_generic_spell(
        vec![StaticDefinition::continuous()
            .affected(TargetFilter::And {
                filters: vec![TargetFilter::SelfRef, any_creature_filter()],
            })
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Haste,
            }])],
        None,
    );
    assert_eq!(
        state.players[P0.0 as usize].hand.len(),
        1,
        "the independent sequential sibling must still run"
    );
    assert_eq!(
        state.players[P0.0 as usize].life, 20,
        "a conjunctive SelfRef static cannot be satisfied without the forwarded \
         object, so its dependent continuation must be skipped"
    );
    assert!(
        state.transient_continuous_effects.is_empty(),
        "no transient may bind for a static that requires the absent object"
    );
}

/// The other half of the conjunctive rule: `Or` and `Not` must NOT be pruned.
/// `Or { [SelfRef, X] }` is still satisfiable through `X`, and `Not { SelfRef }`
/// is satisfied by everything the anaphor is not — neither requires the absent
/// object, so pruning either would drop a grant the game still owes.
///
/// This guards against a later over-broadening of
/// `filter_requires_missing_forward_result` into the remaining combinators.
#[test]
fn empty_forward_result_keeps_disjunctive_and_negated_self_ref_statics() {
    let cases = vec![
        (
            "or",
            TargetFilter::Or {
                filters: vec![TargetFilter::SelfRef, any_creature_filter()],
            },
        ),
        (
            "not",
            TargetFilter::Not {
                filter: Box::new(TargetFilter::SelfRef),
            },
        ),
    ];

    for (label, affected) in cases {
        let state = resolve_empty_forward_result_generic_spell(
            vec![StaticDefinition::continuous()
                .affected(affected)
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Haste,
                }])],
            None,
        );
        assert_eq!(
            state.players[P0.0 as usize].life, 21,
            "{label} does not require the forwarded object, so the node and its \
             dependent continuation must survive"
        );
    }
}

/// The arm where the retain pass and the dependency check must agree: an outer
/// `ParentTarget` node carrying BOTH a dependent `SelfRef` static and an
/// independent `TriggeringSource` one. The pruner must drop only the first and
/// keep the node alive for the second, which then binds the event-context object.
///
/// This is the case that would regress if the two predicates ever stopped
/// sharing `generic_static_depends_on_missing_forward_result`.
#[test]
fn empty_forward_result_prunes_only_the_dependent_static_under_outer_parent_target() {
    let (state, trigger_source) = resolve_empty_forward_result_with_trigger_source(
        vec![
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Vigilance,
                }]),
            StaticDefinition::continuous()
                .affected(TargetFilter::TriggeringSource)
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Haste,
                }]),
        ],
        Some(TargetFilter::ParentTarget),
    );
    assert_eq!(
        state.transient_continuous_effects.len(),
        1,
        "exactly the independent TriggeringSource static may survive"
    );
    assert_eq!(
        state.transient_continuous_effects[0].affected,
        TargetFilter::SpecificObject { id: trigger_source },
        "the surviving static must bind the triggering source"
    );
    assert!(
        state.transient_continuous_effects[0]
            .modifications
            .iter()
            .any(|m| matches!(
                m,
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Haste
                }
            )),
        "the SelfRef static (vigilance) must have been pruned, not the TriggeringSource one"
    );
}

/// Same empty-forward-result spell as `resolve_empty_forward_result_generic_spell`,
/// but with a real event-context source staged so an `affected: TriggeringSource`
/// static has a referent to bind. `PermanentUntapped` is the smallest event
/// `targeting::extract_source_from_event` accepts. Returns the resolved state
/// and the staged source.
fn resolve_empty_forward_result_with_trigger_source(
    static_abilities: Vec<StaticDefinition>,
    target: Option<TargetFilter>,
) -> (engine::types::game_state::GameState, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["independent sibling draw"]);
    let trigger_source = scenario.add_vanilla(P0, 2, 2);
    let spell = scenario
        .add_spell_to_hand(P0, "Empty Forward Generic", false)
        .with_ability_definition(empty_forward_result_generic_spell(static_abilities, target))
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.state_mut().current_trigger_event = Some(GameEvent::PermanentUntapped {
        object_id: trigger_source,
    });
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: runner.state().objects[&spell].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("synthetic spell announcement must be accepted");
    runner.advance_until_stack_empty();
    (runner.state().clone(), trigger_source)
}

/// CR 608.2c: an outer `target: ParentTarget` must not condemn a static whose
/// effective application filter is the resolution-local `TriggeringSource`.
/// `generic_effect_application_filter` makes the inner `affected` the authority,
/// so the static is independent of the absent forwarded object and the node —
/// and its dependent continuation — must survive the pruner.
///
/// Pre-fix, `effect_chain_depends_on_missing_forward_result` re-read the raw
/// outer slot after `without_missing_forward_result_dependencies` had already
/// retained this static, and discarded the whole node: life stopped at 20.
#[test]
fn empty_forward_result_keeps_triggering_source_static_under_outer_parent_target() {
    let state = resolve_empty_forward_result_generic_spell(
        vec![StaticDefinition::continuous()
            .affected(TargetFilter::TriggeringSource)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Haste,
            }])],
        Some(TargetFilter::ParentTarget),
    );
    assert_eq!(state.players[P0.0 as usize].hand.len(), 1);
    assert_eq!(
        state.players[P0.0 as usize].life, 21,
        "the retained TriggeringSource static is independent of the missing forward \
         result, so the node and its dependent continuation must both survive"
    );
}

/// The positive half: with a real event context staged, the static the pruner
/// retained must actually install its transient, bound to the triggering source.
/// Asserting `SpecificObject` identity means a regression that drops the static
/// fails on a MISSING transient and one that mis-binds fails on the WRONG object.
#[test]
fn empty_forward_result_binds_retained_triggering_source_static_under_outer_parent_target() {
    let (state, trigger_source) = resolve_empty_forward_result_with_trigger_source(
        vec![StaticDefinition::continuous()
            .affected(TargetFilter::TriggeringSource)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Haste,
            }])],
        Some(TargetFilter::ParentTarget),
    );
    assert_eq!(
        state.transient_continuous_effects.len(),
        1,
        "the retained TriggeringSource static must install its transient; the pruner \
         must not have discarded the node that carries it"
    );
    assert_eq!(
        state.transient_continuous_effects[0].affected,
        TargetFilter::SpecificObject { id: trigger_source },
        "the event-context transient must bind the triggering source"
    );
    assert_eq!(
        state.players[P0.0 as usize].life, 21,
        "the node carrying the independent static must survive the pruner"
    );
    assert!(creature_has_haste_from_transient_effects(
        &state,
        trigger_source
    ));
}

/// The `GenericEffect` continuation both positive pairings share: it declares
/// `target: SelfRef` but binds through an inherited-reference `affected`, which
/// `generic_effect_application_filter` gives precedence (CR 608.2c).
fn self_ref_generic_grant(affected: TargetFilter) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(affected)
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Haste,
                }])],
            duration: Some(Duration::UntilEndOfTurn),
            target: Some(TargetFilter::SelfRef),
            end_cost: None,
        },
    )
}

/// Find the single Army token the `Effect::Amass` parent created (CR 701.47a).
fn amassed_army_on_battlefield(state: &engine::types::game_state::GameState) -> ObjectId {
    let armies: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state.objects[id]
                .card_types
                .subtypes
                .iter()
                .any(|s| s.eq_ignore_ascii_case("Army"))
        })
        .collect();
    assert_eq!(armies.len(), 1, "amass must have produced exactly one Army");
    armies[0]
}

/// Positive pairing for `empty_forward_result_preserves_parent_target_generic_effect`.
///
/// The negative case above only proves nothing bound to the spell. This one
/// proves the `ParentTarget` binding path actually executes: a targeted parent
/// (`SetTapState`) propagates its chosen creature into the continuation's
/// `targets`, and the continuation must bind the transient to THAT creature.
/// Reverting the shared-classifier fix pins it to the spell's own id instead,
/// so the `SpecificObject` identity assertion — not just a count — fails.
#[test]
fn parent_target_generic_effect_binds_the_chosen_object_under_outer_self_ref() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let chosen = scenario.add_vanilla(P0, 2, 2);
    let spell = scenario
        .add_spell_to_hand(P0, "Parent Target Generic", false)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::SetTapState {
                    target: TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        controller: None,
                        properties: vec![],
                    }),
                    scope: EffectScope::Single,
                    state: TapStateChange::Tap,
                },
            )
            .sub_ability(self_ref_generic_grant(TargetFilter::ParentTarget)),
        )
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_objects(&[chosen]).resolve();
    let state = outcome.state();

    assert_eq!(
        state.transient_continuous_effects.len(),
        1,
        "the ParentTarget continuation must install exactly one transient"
    );
    assert_eq!(
        state.transient_continuous_effects[0].affected,
        TargetFilter::SpecificObject { id: chosen },
        "the inner ParentTarget must bind the chosen creature, not the spell source \
         ({chosen:?} expected, spell source is {spell:?})"
    );
    assert!(creature_has_haste_from_transient_effects(state, chosen));
}

/// Positive pairing for `empty_forward_result_preserves_amassed_army_generic_effect`.
///
/// CR 701.47c: "the amassed Army" names the creature amass chose. `Effect::Amass`
/// stamps `amassed_army_object` recursively onto its chain, so the continuation
/// must bind that Army rather than the spell that amassed it.
#[test]
fn amassed_army_generic_effect_binds_the_stamped_army_under_outer_self_ref() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Amass Generic", false)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Amass {
                    subtype: "Zombie".to_string(),
                    count: QuantityExpr::Fixed { value: 2 },
                },
            )
            .sub_ability(self_ref_generic_grant(TargetFilter::AmassedArmy)),
        )
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();
    let state = outcome.state();
    let army = amassed_army_on_battlefield(state);

    assert_eq!(
        state.transient_continuous_effects.len(),
        1,
        "the AmassedArmy continuation must install exactly one transient"
    );
    assert_eq!(
        state.transient_continuous_effects[0].affected,
        TargetFilter::SpecificObject { id: army },
        "the inner AmassedArmy must bind the stamped Army, not the spell source \
         ({army:?} expected, spell source is {spell:?})"
    );
    assert!(creature_has_haste_from_transient_effects(state, army));
}

/// A mixed GenericEffect is retained after an empty forwarded move, but its
/// individual SelfRef definition still depends on the missing object. Prune
/// that definition while preserving the independent player-bound definition
/// and the node's continuation edges.
#[test]
fn empty_forward_result_prunes_self_ref_static_from_mixed_generic_effect() {
    let state = resolve_empty_forward_result_generic_spell(
        vec![
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Haste,
                }]),
            StaticDefinition::continuous()
                .affected(TargetFilter::Controller)
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Vigilance,
                }]),
        ],
        None,
    );

    assert_eq!(state.players[P0.0 as usize].hand.len(), 1);
    assert_eq!(
        state.players[P0.0 as usize].life, 21,
        "retaining the mixed node must preserve its dependent continuation"
    );
    assert_eq!(
        state.transient_continuous_effects.len(),
        1,
        "only the independent static definition may survive the missing forward result"
    );
    let effect = &state.transient_continuous_effects[0];
    assert_eq!(
        effect.affected,
        TargetFilter::SpecificPlayer { id: P0 },
        "the surviving Controller definition must bind to the ability controller"
    );
    assert!(effect.modifications.iter().any(|modification| matches!(
        modification,
        ContinuousModification::AddKeyword {
            keyword: Keyword::Vigilance
        }
    )));
}

/// Only an all-effective-SelfRef GenericEffect depends on the missing result.
/// Mixed, broadcast, empty, and no-application-filter forms keep both their
/// continuation and independent sibling in the production cast/apply path.
#[test]
fn empty_forward_result_preserves_non_self_ref_generic_effect_forms() {
    let cases = vec![
        (
            "broadcast",
            vec![StaticDefinition::continuous().affected(TargetFilter::Controller)],
            None,
        ),
        ("empty", vec![], Some(TargetFilter::SelfRef)),
        ("none", vec![StaticDefinition::continuous()], None),
        // A statics-less node has nothing that can need the forwarded object,
        // whatever the outer descriptor says, so it stays executable exactly
        // like the `SelfRef` row above.
        (
            "empty-outer-parent-target",
            vec![],
            Some(TargetFilter::ParentTarget),
        ),
    ];

    for (label, static_abilities, target) in cases {
        let state = resolve_empty_forward_result_generic_spell(static_abilities, target);
        assert_eq!(
            state.players[P0.0 as usize].life, 21,
            "{label} GenericEffect must remain executable after an empty forward result"
        );
    }
}

#[test]
fn empty_forward_result_preserves_independent_sequential_siblings() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let graveyard_creature = scenario
        .add_creature_to_graveyard(P0, "Unreturned Creature", 2, 2)
        .id();
    let destroy_target = scenario.add_creature(P1, "Destroy Target", 2, 2).id();
    let offering = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Yawgmoth's Vile Offering",
            true,
            YAWGMOTHS_VILE_OFFERING_ORACLE,
        )
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let response = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Reanimation Response",
            true,
            REANIMATION_RESPONSE_ORACLE,
        )
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&offering].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: offering,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Yawgmoth's Vile Offering must be castable for the regression");

    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { selection, .. } => {
                let target = if selection.current_slot == 0 {
                    Some(engine::types::ability::TargetRef::Object(
                        graveyard_creature,
                    ))
                } else {
                    Some(engine::types::ability::TargetRef::Object(destroy_target))
                };
                runner
                    .act(GameAction::ChooseTarget { target })
                    .expect("target choice must be accepted");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => break,
            _ => break,
        }
    }

    // CR 608.2b: Make the first selected target illegal between announcement
    // and resolution, so its forward-result move returns no object while the
    // independently targeted Destroy sibling remains legal.
    runner
        .cast(response)
        .target_object(graveyard_creature)
        .commit();
    runner.pass_both_players();
    assert_eq!(
        runner.state().objects[&graveyard_creature].zone,
        Zone::Battlefield,
        "the production cast/resolution pipeline must move the reanimation target first"
    );
    assert!(
        !runner.state().stack.is_empty(),
        "Yawgmoth's Vile Offering must remain on the stack after the response resolves"
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&graveyard_creature].zone,
        Zone::Battlefield,
        "the pre-resolution move must invalidate the selected reanimation target"
    );
    assert_ne!(
        runner.state().objects[&destroy_target].zone,
        Zone::Battlefield,
        "the independent Destroy sibling must still resolve"
    );
    assert_eq!(
        runner.state().objects[&offering].zone,
        Zone::Exile,
        "the later self-exile sibling must still resolve"
    );
}

#[test]
fn empty_forward_result_resolves_independent_sibling_before_dependent_tail() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Forward Result Source", 2, 2)
        .id();
    let destroy_target = scenario.add_creature(P1, "Independent Target", 2, 2).id();

    let dependent_tail = ResolvedAbility::new(
        Effect::CreateDelayedTrigger {
            condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
            effect: Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Sacrifice {
                    target: TargetFilter::ParentTarget,
                    count: QuantityExpr::Fixed { value: 1 },
                    min_count: 0,
                },
            )),
            uses_tracked_set: false,
        },
        vec![],
        source,
        P0,
    );
    let mut independent_sibling = ResolvedAbility::new(
        Effect::Destroy {
            target: TargetFilter::SpecificObject { id: destroy_target },
            cant_regenerate: false,
        },
        vec![engine::types::ability::TargetRef::Object(destroy_target)],
        source,
        P0,
    );
    independent_sibling.sub_link = engine::types::ability::SubAbilityLink::SequentialSibling;
    independent_sibling.sub_ability = Some(Box::new({
        let mut tail = dependent_tail;
        tail.sub_link = engine::types::ability::SubAbilityLink::SequentialSibling;
        tail
    }));

    let mut forward_result = ResolvedAbility::new(
        Effect::ChangeZone {
            origin: Some(Zone::Graveyard),
            destination: Zone::Battlefield,
            target: TargetFilter::Typed(TypedFilter {
                type_filters: vec![engine::types::ability::TypeFilter::Creature],
                controller: None,
                properties: vec![FilterProp::InZone {
                    zone: Zone::Graveyard,
                }],
            }),
            owner_library: false,
            enter_transformed: false,
            enters_under: Some(ControllerRef::You),
            enter_tapped: engine::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: true,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
        vec![],
        source,
        P0,
    )
    .sub_ability(independent_sibling);
    forward_result.target_choice_timing = TargetChoiceTiming::Resolution;
    forward_result.forward_result = true;

    let mut runner = scenario.build();
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &forward_result, &mut events, 0)
        .expect("empty forward-result chain must resolve");

    assert_ne!(
        runner.state().objects[&destroy_target].zone,
        Zone::Battlefield,
        "the independent sibling must resolve even when a later dependent tail is skipped"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        0,
        "the dependent ParentTarget tail must remain a no-op without a moved object"
    );
}

#[test]
fn empty_forward_result_suppresses_dependent_else_and_resumes_later_sibling() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Untapped Source", 2, 2).id();
    let destroy_target = scenario.add_creature(P1, "Reach Guard Target", 2, 2).id();

    let dependent_else = ResolvedAbility::new(
        Effect::CreateDelayedTrigger {
            condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
            effect: Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Sacrifice {
                    target: TargetFilter::ParentTarget,
                    count: QuantityExpr::Fixed { value: 1 },
                    min_count: 0,
                },
            )),
            uses_tracked_set: false,
        },
        vec![],
        source,
        P0,
    );
    let mut later_sibling = ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 2 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    later_sibling.sub_link = engine::types::ability::SubAbilityLink::SequentialSibling;

    let mut false_condition_sibling = ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    false_condition_sibling.condition = Some(AbilityCondition::SourceIsTapped);
    false_condition_sibling.sub_link = engine::types::ability::SubAbilityLink::SequentialSibling;
    false_condition_sibling.else_ability = Some(Box::new(dependent_else));
    false_condition_sibling.sub_ability = Some(Box::new(later_sibling));

    let mut first_independent_sibling = ResolvedAbility::new(
        Effect::Destroy {
            target: TargetFilter::SpecificObject { id: destroy_target },
            cant_regenerate: false,
        },
        vec![engine::types::ability::TargetRef::Object(destroy_target)],
        source,
        P0,
    );
    first_independent_sibling.sub_link = engine::types::ability::SubAbilityLink::SequentialSibling;
    first_independent_sibling.sub_ability = Some(Box::new(false_condition_sibling));

    let mut forward_result = ResolvedAbility::new(
        Effect::ChangeZone {
            origin: Some(Zone::Graveyard),
            destination: Zone::Battlefield,
            target: TargetFilter::Typed(TypedFilter {
                type_filters: vec![engine::types::ability::TypeFilter::Creature],
                controller: None,
                properties: vec![FilterProp::InZone {
                    zone: Zone::Graveyard,
                }],
            }),
            owner_library: false,
            enter_transformed: false,
            enters_under: Some(ControllerRef::You),
            enter_tapped: engine::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: true,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
        vec![],
        source,
        P0,
    )
    .sub_ability(first_independent_sibling);
    forward_result.target_choice_timing = TargetChoiceTiming::Resolution;
    forward_result.forward_result = true;

    let mut runner = scenario.build();
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &forward_result, &mut events, 0)
        .expect("conditional empty forward-result chain must resolve");

    assert_eq!(
        runner.state().players[usize::from(P0.0)].life,
        22,
        "the false sibling must skip its own effect and resume the later independent sibling"
    );
    assert_ne!(
        runner.state().objects[&destroy_target].zone,
        Zone::Battlefield,
        "the first independent sibling must resolve and prove the handoff was reached"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        0,
        "the dependent ParentTarget else branch must remain a no-op"
    );
}

/// CR 614.12a + CR 400.7j: An as-enters choice on the returned permanent must
/// complete without losing later instructions that refer to that permanent.
#[test]
fn emperor_of_bones_resumes_riders_after_anointed_peacekeepers_as_enters_choices() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let emperor = scenario.add_creature(P0, "Emperor of Bones", 2, 2).id();
    let _opponent_card = scenario.add_card_to_hand(P1, "Opponent Secret");
    let peacekeeper = {
        let mut builder = scenario.add_creature_to_exile(P0, "Anointed Peacekeeper", 3, 3);
        builder.from_oracle_text(ANOINTED_PEACEKEEPER);
        builder.id()
    };

    let mut runner = scenario.build();
    runner.state_mut().all_card_names = std::sync::Arc::from([NAMED_CARD.to_string()]);
    runner.state_mut().exile_links.push(ExileLink {
        exiled_id: peacekeeper,
        source_id: emperor,
        kind: ExileLinkKind::TrackedBySource,
    });

    let definition = parse_effect_chain(EMPEROR_COUNTER_TRIGGER_EFFECT, AbilityKind::Spell);
    let ability = build_resolved_from_def(&definition, emperor, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("Emperor of Bones return must reach Peacekeeper's as-enters choice");

    let WaitingFor::NamedChoice {
        choice_type,
        options,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "Peacekeeper must ask which opponent to look at, got {}",
            runner.waiting_for_kind()
        );
    };
    assert!(matches!(choice_type, ChoiceType::Opponent { .. }));
    assert_eq!(options, vec![P1.0.to_string()]);
    runner
        .act(GameAction::ChooseOption {
            choice: P1.0.to_string(),
        })
        .expect("choose the opponent whose hand Peacekeeper looks at");

    let WaitingFor::NamedChoice { choice_type, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "Peacekeeper must ask for a card name after looking, got {}",
            runner.waiting_for_kind()
        );
    };
    assert!(matches!(choice_type, ChoiceType::CardName));
    runner
        .act(GameAction::ChooseOption {
            choice: NAMED_CARD.to_string(),
        })
        .expect("choose the card name for Peacekeeper");

    let state = runner.state();
    let returned = &state.objects[&peacekeeper];
    assert_eq!(returned.zone, Zone::Battlefield);
    assert!(returned.chosen_attributes.iter().any(
        |attribute| matches!(attribute, ChosenAttribute::CardName(name) if name == NAMED_CARD)
    ));
    assert_eq!(
        returned
            .counters
            .get(&CounterType::Finality)
            .copied()
            .unwrap_or(0),
        1,
        "Peacekeeper must retain Emperor's finality entry modifier"
    );
    assert!(
        creature_has_haste_from_transient_effects(state, peacekeeper),
        "Emperor's forwarded haste rider must resume after both as-enters choices"
    );
    assert_eq!(
        state.delayed_triggers.len(),
        1,
        "Emperor's delayed sacrifice rider must resume after both as-enters choices"
    );
    assert_eq!(
        state.delayed_triggers[0].ability.targets,
        vec![engine::types::ability::TargetRef::Object(peacekeeper)]
    );
}

/// A synthetic as-enters replacement that opens a PayAmountChoice before the
/// returning permanent finishes entering. This mirrors the shape of a printed
/// Moved replacement while keeping the regression independent of card data.
fn pay_amount_choice_replacement() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Battlefield)
        .valid_card(TargetFilter::SelfRef)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PayCost {
                cost: AbilityCost::PayLife {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
        ))
}

/// CR 614.12a + CR 400.7j: A previously unlisted resolution-owned prompt must
/// survive the replacement pause and resume the Emperor continuation.
#[test]
fn emperor_of_bones_preserves_pay_amount_choice_through_replacement_pipeline() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let emperor = scenario.add_creature(P0, "Emperor of Bones", 2, 2).id();
    let peacekeeper = {
        let mut builder = scenario.add_creature_to_exile(P0, "Anointed Peacekeeper", 3, 3);
        builder.from_oracle_text(ANOINTED_PEACEKEEPER);
        builder.id()
    };

    let mut runner = scenario.build();
    runner.state_mut().exile_links.push(ExileLink {
        exiled_id: peacekeeper,
        source_id: emperor,
        kind: ExileLinkKind::TrackedBySource,
    });
    runner
        .state_mut()
        .objects
        .get_mut(&peacekeeper)
        .unwrap()
        .replacement_definitions = vec![pay_amount_choice_replacement()].into();

    let definition = parse_effect_chain(EMPEROR_COUNTER_TRIGGER_EFFECT, AbilityKind::Spell);
    let ability = build_resolved_from_def(&definition, emperor, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("Emperor of Bones return must reach the replacement PayAmountChoice");

    let WaitingFor::PayAmountChoice {
        player, min, max, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the replacement must preserve its PayAmountChoice, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(player, P0);
    assert_eq!(min, 0);
    assert!(max > 0);

    runner
        .act(GameAction::SubmitPayAmount { amount: 0 })
        .expect("answer the replacement PayAmountChoice through GameRunner::act");

    let state = runner.state();
    assert_eq!(state.objects[&peacekeeper].zone, Zone::Battlefield);
    assert!(matches!(
        state.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert!(state.stack.is_empty());
    assert_eq!(
        state.delayed_triggers.len(),
        1,
        "the Emperor delayed sacrifice rider must resume after the replacement choice"
    );
}
