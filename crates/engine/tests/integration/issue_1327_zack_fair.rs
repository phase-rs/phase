//! Regression for issue #1327 — Zack Fair.
//!
//! Oracle:
//!   Zack Fair enters with a +1/+1 counter on it.
//!   {1}, Sacrifice Zack Fair: Target creature you control gains indestructible
//!   until end of turn. Put Zack Fair's counters on that creature and attach an
//!   Equipment that was attached to Zack Fair to that creature.
//!
//! https://github.com/phase-rs/phase/issues/1327

use engine::game::effects::attach::attach_to;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityDefinition, Effect, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ZACK_FAIR_ORACLE: &str = "Zack Fair enters with a +1/+1 counter on it.\n{1}, Sacrifice Zack Fair: Target creature you control gains indestructible until end of turn. Put Zack Fair's counters on that creature and attach an Equipment that was attached to Zack Fair to that creature.";

fn p1p1(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|obj| obj.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

fn add_colorless_mana(runner: &mut engine::game::scenario::GameRunner, count: u32) {
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .expect("P0")
        .mana_pool;
    for _ in 0..count {
        pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
}

fn chain_contains_effect(def: &AbilityDefinition, predicate: &dyn Fn(&Effect) -> bool) -> bool {
    if predicate(&def.effect) {
        return true;
    }
    def.sub_ability
        .as_ref()
        .is_some_and(|sub| chain_contains_effect(sub, predicate))
}

#[test]
fn zack_fair_activated_parses_counter_move_and_equipment_attach() {
    let parsed = parse_oracle_text(
        ZACK_FAIR_ORACLE,
        "Zack Fair",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Soldier".to_string()],
    );

    let activated = parsed
        .abilities
        .iter()
        .find(|a| matches!(a.kind, engine::types::ability::AbilityKind::Activated))
        .expect("Zack Fair must parse an activated ability");

    assert!(
        chain_contains_effect(activated, &|effect| {
            matches!(
                effect,
                Effect::MoveCounters {
                    source: TargetFilter::SelfRef,
                    ..
                }
            )
        }),
        "activated chain must include MoveCounters from SelfRef, got root {:?}",
        activated.effect
    );
    assert!(
        chain_contains_effect(activated, &|effect| matches!(effect, Effect::Attach { .. })),
        "activated chain must include Attach, got root {:?}",
        activated.effect
    );
    assert!(
        !chain_contains_effect(activated, &|effect| {
            matches!(effect, Effect::Unimplemented { .. })
        }),
        "activated chain must not contain Unimplemented nodes"
    );
}

#[test]
fn zack_fair_sacrifice_moves_counters_and_reattaches_equipment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let zack = scenario
        .add_creature(P0, "Zack Fair", 0, 1)
        .from_oracle_text(ZACK_FAIR_ORACLE)
        .id();
    let bearer = scenario.add_creature(P0, "Bearer", 2, 2).id();
    let equipment = scenario
        .add_creature(P0, "Hero's Sword", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .id();

    let mut runner = scenario.build();
    add_colorless_mana(&mut runner, 1);
    attach_to(runner.state_mut(), equipment, zack);
    evaluate_layers(runner.state_mut());

    runner
        .state_mut()
        .objects
        .get_mut(&zack)
        .expect("Zack on battlefield")
        .counters
        .insert(CounterType::Plus1Plus1, 1);

    runner
        .act(GameAction::ActivateAbility {
            source_id: zack,
            ability_index: 0,
        })
        .expect("activating Zack Fair must succeed");

    let targets = match &runner.state().waiting_for {
        WaitingFor::TargetSelection { target_slots, .. } => target_slots
            .iter()
            .map(|slot| {
                slot.legal_targets
                    .iter()
                    .find(|t| matches!(t, TargetRef::Object(id) if *id == bearer))
                    .or_else(|| {
                        slot.legal_targets
                            .iter()
                            .find(|t| matches!(t, TargetRef::Object(id) if *id == equipment))
                    })
                    .or(slot.legal_targets.first())
                    .cloned()
                    .expect("each target slot must have a legal choice")
            })
            .collect::<Vec<_>>(),
        other => panic!("expected target selection for bearer creature, got {other:?}"),
    };

    runner
        .act(GameAction::SelectTargets { targets })
        .expect("choosing targets must succeed");

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&zack].zone,
        Zone::Graveyard,
        "Zack must be sacrificed as part of the cost"
    );
    assert_eq!(
        p1p1(&runner, bearer),
        1,
        "bearer must receive Zack's +1/+1 counter (got {})",
        p1p1(&runner, bearer)
    );
    assert_eq!(p1p1(&runner, zack), 0, "Zack's counters must have moved");
    assert!(
        runner.state().objects[&bearer].has_keyword(&Keyword::Indestructible),
        "bearer must gain indestructible until end of turn"
    );
    assert_eq!(
        runner.state().objects[&equipment].attached_to,
        Some(engine::game::game_object::AttachTarget::Object(bearer)),
        "equipment must re-attach to the bearer"
    );
}
