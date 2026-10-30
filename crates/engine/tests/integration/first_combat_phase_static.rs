//! A static gated on the first combat phase follows every step boundary.

use super::rules::{GameAction, GameRunner, GameScenario, ObjectId, Phase, WaitingFor, P0, P1};
use engine::game::game_object::DisplayIdentity;
use engine::game::layers::flush_layers;
use engine::game::perf_counters;
use engine::game::printed_cards::intrinsic_copiable_values;
use engine::types::ability::{
    ContinuousModification, ControllerRef, Duration, EffectKind, StaticCondition, StaticDefinition,
    TargetFilter, TypedFilter,
};
use engine::types::events::GameEvent;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::stickers::{AppliedSticker, StickerLocator};
use engine::types::zones::Zone;

const GATED: &str = "As long as it's the first combat phase of the turn, Gated Bear gets +2/+0.";
const GATED_SELF_REMOVING: &str =
    "As long as it's the first combat phase of the turn, Gated Bear gets +2/+0 and loses all abilities.";
const GATED_LORD: &str = "Other creatures you control have \"As long as it's the first combat phase of the turn, this creature gets +2/+0.\"";
const MANDATE: &str = "Cast this spell only during combat.\nYour opponents can't cast spells this turn.\nEnd the combat phase. (Remove all attackers and blockers from combat. Exile all spells and abilities from the stack, including this spell.)";
const TIME_STOP: &str = "End the turn. (Exile all spells and abilities, including this spell. The player whose turn it is discards down to their maximum hand size. Damage heals and \"this turn\" and \"until end of turn\" effects end.)";
const SAVAGE_BEATING: &str = "Cast this spell only during combat on your turn.\nChoose one —\n• Creatures you control gain double strike until end of turn.\n• Untap all creatures you control. After this phase, there is an additional combat phase.\nEntwine {1}{R} (Choose both if you pay the entwine cost.)";

fn scenario() -> GameScenario {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_library_top(P0, &["Filler A", "Filler B"]);
    s.with_library_top(P1, &["Filler C", "Filler D"]);
    s
}

fn power(r: &GameRunner, id: ObjectId) -> i32 {
    r.state().objects[&id].power.unwrap()
}

fn fund(r: &mut GameRunner, color: ManaType, count: usize) {
    for _ in 0..count {
        r.state_mut().players[0]
            .mana_pool
            .add(ManaUnit::new(color, ObjectId(0), false, vec![]));
    }
}

/// Enters the first combat, declares no attackers, and checks the gate is on.
fn empty_first_combat(r: &mut GameRunner, id: ObjectId) {
    r.advance_to_combat();
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ));
    assert_eq!(power(r, id), 4, "CR 611.3a: on as the first combat begins");
    r.declare_attackers(&[]).unwrap();
    assert_eq!(r.state().phase, Phase::EndCombat);
    assert_eq!(
        r.state().combat.as_ref(),
        Some(&Default::default()),
        "the combat stays untouched"
    );
    assert_eq!(power(r, id), 4);
}

/// Leaves combat for the postcombat main phase and checks the cached power against a forced full pass.
fn assert_postcombat_power_is_fresh(r: &mut GameRunner, id: ObjectId) {
    for _ in 0..2 {
        r.act(GameAction::PassPriority).unwrap();
    }
    assert_eq!(r.state().phase, Phase::PostCombatMain);
    assert_priority(r);
    let cached = power(r, id);
    r.state_mut().layers_dirty.mark_full();
    flush_layers(r.state_mut());
    assert_eq!(
        cached,
        power(r, id),
        "cached power matches a full re-evaluation"
    );
    assert_eq!(cached, 2);
}

fn assert_priority(r: &GameRunner) {
    assert!(r.state().stack.is_empty());
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
}

// CR 511.3 + CR 611.3a: the natural End Combat exit.
#[test]
fn first_combat_static_ends_with_an_untouched_natural_combat() {
    let mut s = scenario();
    let id = s
        .add_creature_from_oracle(P0, "Gated Bear", 2, 2, GATED)
        .id();
    let mut r = s.build();
    empty_first_combat(&mut r, id);
    for _ in 0..2 {
        r.act(GameAction::PassPriority).unwrap();
    }
    assert_eq!(r.state().phase, Phase::PostCombatMain);
    assert!(r.state().combat.is_none());
    assert_priority(&r);
    assert_eq!(power(&r, id), 2);
}

// CR 613.6 + CR 611.3a: a step-gated static that removes its own ability still ends with the step.
#[test]
fn first_combat_static_that_removes_its_own_ability_ends_with_an_untouched_natural_combat() {
    let mut s = scenario();
    let id = s
        .add_creature_from_oracle(P0, "Gated Bear", 2, 2, GATED_SELF_REMOVING)
        .id();
    let mut r = s.build();
    empty_first_combat(&mut r, id);
    assert!(
        r.state().objects[&id].static_definitions.is_empty(),
        "the static removed itself from the live set"
    );
    for _ in 0..2 {
        r.act(GameAction::PassPriority).unwrap();
    }
    assert_eq!(r.state().phase, Phase::PostCombatMain);
    assert_priority(&r);
    let cached = power(&r, id);
    r.state_mut().layers_dirty.mark_full();
    flush_layers(r.state_mut());
    assert_eq!(
        cached,
        power(&r, id),
        "cached power matches a full re-evaluation"
    );
    assert_eq!(cached, 2);
}

// CR 724.2d + CR 611.3a: Mandate of Peace ends an untouched combat.
#[test]
fn first_combat_static_ends_when_an_untouched_combat_phase_is_ended() {
    let mut s = scenario();
    let id = s
        .add_creature_from_oracle(P0, "Gated Bear", 2, 2, GATED)
        .id();
    let mandate = s
        .add_spell_to_hand_from_oracle(P0, "Mandate of Peace", true, MANDATE)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::White],
        })
        .id();
    let mut r = s.build();
    empty_first_combat(&mut r, id);
    fund(&mut r, ManaType::White, 2);
    let outcome = r.cast(mandate).resolve();
    assert!(outcome.events().iter().any(|event| matches!(event,
        GameEvent::EffectResolved { kind: EffectKind::EndCombatPhase, source_id, .. }
            if *source_id == mandate)));
    assert_eq!(r.state().phase, Phase::PostCombatMain);
    assert!(r.state().combat.is_none());
    assert_priority(&r);
    assert_eq!(power(&r, id), 2);
}

// CR 724.1d + CR 611.3a: ending the turn during an untouched combat.
#[test]
fn first_combat_static_ends_when_the_turn_ends_during_an_untouched_combat() {
    let mut s = scenario();
    let id = s
        .add_creature_from_oracle(P0, "Gated Bear", 2, 2, GATED)
        .id();
    let time_stop = s
        .add_spell_to_hand_from_oracle(P0, "Time Stop", true, TIME_STOP)
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::Blue; 2],
        })
        .id();
    let mut r = s.build();
    let turn = r.state().turn_number;
    empty_first_combat(&mut r, id);
    fund(&mut r, ManaType::Blue, 6);
    let outcome = r.cast(time_stop).resolve();
    assert!(outcome.events().iter().any(|event| matches!(event,
        GameEvent::EffectResolved { kind: EffectKind::EndTheTurn, source_id, .. }
            if *source_id == time_stop)));
    assert_eq!(r.state().phase, Phase::Cleanup);
    assert_eq!(r.state().turn_number, turn);
    assert!(r.state().combat.is_none());
    assert_priority(&r);
    assert_eq!(power(&r, id), 2);
}

// CR 500.8 + CR 611.3a: an added combat phase directly after an untouched one is not the first.
#[test]
fn first_combat_static_ends_when_an_added_combat_follows_an_untouched_one() {
    let mut s = scenario();
    let id = s
        .add_creature_from_oracle(P0, "Gated Bear", 2, 2, GATED)
        .id();
    let beating = s
        .add_spell_to_hand_from_oracle(P0, "Savage Beating", true, SAVAGE_BEATING)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Red; 2],
        })
        .id();
    let mut r = s.build();
    empty_first_combat(&mut r, id);
    fund(&mut r, ManaType::Red, 5);
    r.cast(beating).modes(&[1]).resolve();
    assert_priority(&r);
    assert_eq!(r.state().phase, Phase::EndCombat);
    for _ in 0..2 {
        r.act(GameAction::PassPriority).unwrap();
    }
    assert_eq!(r.state().phase, Phase::BeginCombat);
    assert_eq!(
        r.state().steps_started_this_turn.count(Phase::BeginCombat),
        2
    );
    assert_priority(&r);
    assert_eq!(power(&r, id), 2);
}

/// Full layer passes run while passing from End Combat to the postcombat main phase.
fn untouched_combat_exit_full_passes(oracle: Option<&str>) -> u64 {
    let mut s = scenario();
    let id = match oracle {
        Some(text) => s
            .add_creature_from_oracle(P0, "Gated Bear", 2, 2, text)
            .id(),
        None => s.add_creature(P0, "Plain Bear", 2, 2).id(),
    };
    let mut r = s.build();
    r.advance_to_combat();
    r.declare_attackers(&[]).unwrap();
    r.act(GameAction::PassPriority).unwrap();
    assert_eq!(r.state().phase, Phase::EndCombat);
    perf_counters::reset();
    r.act(GameAction::PassPriority).unwrap();
    let passes = perf_counters::snapshot().layers_full_eval;
    assert_eq!(r.state().phase, Phase::PostCombatMain);
    assert_eq!(power(&r, id), 2);
    passes
}

// CR 611.3a: only a live step-gated static makes a step begin re-run the layers.
#[test]
fn untouched_combat_exit_reruns_layers_only_for_a_step_gated_static() {
    assert_eq!(untouched_combat_exit_full_passes(None), 0);
    assert_eq!(untouched_combat_exit_full_passes(Some(GATED)), 1);
}

// CR 707.2 + CR 613.6 + CR 611.3a: a copied step-gated static that removes itself, after its donor left.
#[test]
fn copied_first_combat_static_ends_after_its_donor_left() {
    let mut s = scenario();
    let donor = s
        .add_creature_from_oracle(P0, "Gated Bear", 2, 2, GATED_SELF_REMOVING)
        .id();
    let copier = s.add_creature(P0, "Copier", 1, 1).id();
    let mut r = s.build();
    let values = intrinsic_copiable_values(&r.state().objects[&donor]);
    assert!(r
        .state_mut()
        .add_transient_continuous_effect(
            copier,
            P0,
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: copier },
            vec![ContinuousModification::CopyValues {
                values: Box::new(values),
                display: DisplayIdentity::default(),
            }],
            None,
        )
        .is_some());
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(r.state_mut(), donor, Zone::Graveyard, &mut events);
    flush_layers(r.state_mut());
    assert_eq!(r.state().objects[&donor].zone, Zone::Graveyard);
    empty_first_combat(&mut r, copier);
    assert_postcombat_power_is_fresh(&mut r, copier);
}

// CR 123.7 + CR 613.6 + CR 611.3a: a sticker-granted step-gated static that removes itself.
#[test]
fn stickered_first_combat_static_ends_with_an_untouched_natural_combat() {
    let mut s = scenario();
    let id = s.add_creature(P0, "Sticker Bear", 2, 2).id();
    let mut r = s.build();
    r.state_mut()
        .objects
        .get_mut(&id)
        .unwrap()
        .stickers
        .push(AppliedSticker::Ability {
            locator: StickerLocator {
                sheet: "Probe Sheet".into(),
                index: 0,
            },
            ticket_cost: 0,
            text: "As long as it's the first combat phase of the turn, this creature gets +2/+0 and loses all abilities.".into(),
            timestamp: 1,
        });
    r.state_mut().layers_dirty.mark_full();
    flush_layers(r.state_mut());
    assert!(r.state().objects[&id].base_static_definitions.is_empty());
    empty_first_combat(&mut r, id);
    assert!(
        r.state().objects[&id].static_definitions.is_empty(),
        "the sticker static removed itself from the live set"
    );
    assert_postcombat_power_is_fresh(&mut r, id);
}

/// The record a forced full pass leaves in the precombat main phase.
fn record_after_full_pass(r: &mut GameRunner) -> bool {
    r.state_mut().layers_dirty.mark_full();
    flush_layers(r.state_mut());
    r.state().layers_read_current_step
}

// CR 611.3a: a full pass records a step-reading condition wherever it evaluates one, and only there.
#[test]
fn full_pass_records_step_reading_conditions_by_carrier() {
    for (oracle, transient, expected) in [
        (None, None, false),
        (Some("As long as it's your turn, Gated Bear gets +2/+0."), None, false),
        (
            Some("As long as it's the first combat phase of your turn, Gated Bear gets +2/+0."),
            None,
            true,
        ),
        (
            Some("Creatures you control have \"As long as it's the first combat phase of the turn, this creature gets +2/+0.\""),
            None,
            true,
        ),
        (None, Some(StaticCondition::DuringYourTurn), false),
        (
            None,
            Some(StaticCondition::Not {
                condition: Box::new(StaticCondition::FirstCombatPhaseOfTurn),
            }),
            true,
        ),
    ] {
        let mut s = scenario();
        let id = match oracle {
            Some(text) => s.add_creature_from_oracle(P0, "Gated Bear", 2, 2, text),
            None => s.add_creature(P0, "Gated Bear", 2, 2),
        }
        .id();
        let mut r = s.build();
        if let Some(condition) = transient.clone() {
            assert!(r
                .state_mut()
                .add_transient_continuous_effect(
                    id,
                    P0,
                    Duration::UntilEndOfTurn,
                    TargetFilter::SpecificObject { id },
                    vec![ContinuousModification::AddPower { value: 2 }],
                    Some(condition),
                )
                .is_some());
        }
        assert_eq!(record_after_full_pass(&mut r), expected, "{oracle:?} / {transient:?}");
    }
}

// CR 611.3a: a full pass replaces the record, so a step-gated source that left stops marking.
#[test]
fn full_pass_clears_the_record_once_the_step_gated_source_leaves() {
    let mut s = scenario();
    let id = s
        .add_creature_from_oracle(P0, "Gated Bear", 2, 2, GATED)
        .id();
    let mut r = s.build();
    assert!(record_after_full_pass(&mut r));
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(r.state_mut(), id, Zone::Graveyard, &mut events);
    assert!(!record_after_full_pass(&mut r));
}

// CR 613.1f + CR 611.3a: an incremental pass that evaluates a step-reading condition adds it to the record.
#[test]
fn incremental_pass_adds_the_step_reads_it_makes() {
    let mut s = scenario();
    s.add_creature_from_oracle(P0, "Gated Lord", 1, 1, GATED_LORD);
    let entrant = s.add_creature(P0, "Plain Bear", 2, 2).id();
    let mut r = s.build();
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(r.state_mut(), entrant, Zone::Graveyard, &mut events);
    assert!(
        !record_after_full_pass(&mut r),
        "no recipient, so nothing read the step"
    );
    engine::game::zones::move_to_zone(r.state_mut(), entrant, Zone::Battlefield, &mut events);
    perf_counters::reset();
    flush_layers(r.state_mut());
    let passes = perf_counters::snapshot();
    assert_eq!((passes.layers_incremental, passes.layers_full_eval), (1, 0));
    empty_first_combat(&mut r, entrant);
    assert_postcombat_power_is_fresh(&mut r, entrant);
}

// CR 611.3a: an incremental pass keeps the step reads of cached results it does not re-derive.
#[test]
fn incremental_pass_keeps_step_reads_it_does_not_remake() {
    let lord_static = StaticDefinition::continuous()
        .affected(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You),
        ))
        .modifications(vec![ContinuousModification::AddPower { value: 2 }])
        .condition(StaticCondition::And {
            conditions: vec![
                StaticCondition::RecipientMatchesFilter {
                    filter: TargetFilter::Typed(TypedFilter::creature()),
                },
                StaticCondition::FirstCombatPhaseOfTurn,
            ],
        });
    let mut s = scenario();
    let lord = s
        .add_creature(P0, "Recipient Lord", 2, 2)
        .with_static_definition(lord_static)
        .id();
    let entrant = s.add_creature(P1, "Opponent Bear", 2, 2).id();
    let mut r = s.build();
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(r.state_mut(), entrant, Zone::Graveyard, &mut events);
    assert!(record_after_full_pass(&mut r));
    engine::game::zones::move_to_zone(r.state_mut(), entrant, Zone::Battlefield, &mut events);
    perf_counters::reset();
    flush_layers(r.state_mut());
    let passes = perf_counters::snapshot();
    assert_eq!((passes.layers_incremental, passes.layers_full_eval), (1, 0));
    r.advance_to_combat();
    assert_eq!(r.state().phase, Phase::DeclareAttackers);
    let cached = power(&r, lord);
    r.state_mut().layers_dirty.mark_full();
    flush_layers(r.state_mut());
    assert_eq!(
        cached,
        power(&r, lord),
        "cached power matches a full re-evaluation"
    );
    assert_eq!(cached, 4);
}
