use std::ops::ControlFlow;
use std::sync::Arc;

use engine::game::combat::AttackTarget;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::static_abilities::object_has_static_other;
use engine::game::zones::move_to_zone;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, Effect, QuantityExpr, QuantityRef, ResolvedAbility, SacrificeSelection,
    StaticDefinition, TargetFilter, TargetRef,
};
use engine::types::ability_visit::visit_ability_def;
use engine::types::actions::{DebugAction, GameAction};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{ObjectId, TrackedSetId};
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const FORCE: &str = "Create two 3/1 red Elemental creature tokens with trample and haste. Sacrifice those tokens at the beginning of your next upkeep.";
const THATCHER: &str = "Create three 1/1 red Human creature tokens with haste. Sacrifice those tokens at the beginning of the next end step.";
const CHANDRA: &str = "0: Create two 1/1 red Elemental creature tokens. They gain haste. Sacrifice them at the beginning of the next end step.";
const ORTHION: &str = "{1}{R}, {T}: Create a token that's a copy of another target creature you control. It gains haste. Sacrifice it at the beginning of the next end step. Activate only as a sorcery.\n{6}{R}{R}{R}, {T}: Create five tokens that are copies of another target creature you control. They gain haste. Sacrifice them at the beginning of the next end step. Activate only as a sorcery.";
const DEPTHSHAKER: &str = "When this creature enters, any number of target noncreature artifacts you control become 3/3 artifact creatures. Sacrifice them at the beginning of the next end step.\nEach artifact creature you control has melee, trample, and haste. (Whenever a creature with melee attacks, it gets +1/+1 until end of turn for each opponent you attacked this combat.)";
const MALIK: &str = "When Malik, Grim Manipulator enters, you and target opponent each secretly choose a creature that player controls. Then those choices are revealed, and that player sacrifices those creatures.\nWhenever an opponent sacrifices a creature, you create a Treasure token.";
const AVENGER: &str = "Deathtouch\nMobilize X, where X is the number of creature cards in your graveyard. (Whenever this creature attacks, create X tapped and attacking 1/1 red Warrior creature tokens. Sacrifice them at the beginning of the next end step.)";
const ZURGO: &str = "Mobilize 2 (Whenever this creature attacks, create two tapped and attacking 1/1 red Warrior creature tokens. Sacrifice them at the beginning of the next end step.)\nDuring your end step, Warrior tokens you control have \"This token can't be sacrificed.\"";

fn sacrifice_leaves(def: &AbilityDefinition) -> Vec<(TargetFilter, SacrificeSelection)> {
    let mut leaves = Vec::new();
    let _ = visit_ability_def(def, &mut |effect| {
        if let Effect::Sacrifice { target, count, .. } = effect {
            leaves.push((target.clone(), count.clone()));
        }
        ControlFlow::Continue(())
    });
    leaves
}

fn parsed_sacrifices(
    name: &str,
    oracle: &str,
    card_type: &str,
) -> Vec<(TargetFilter, SacrificeSelection)> {
    let parsed = parse_oracle_text(oracle, name, &[], &[card_type.into()], &[]);
    parsed
        .abilities
        .iter()
        .chain(
            parsed
                .triggers
                .iter()
                .filter_map(|trigger| trigger.execute.as_deref()),
        )
        .flat_map(sacrifice_leaves)
        .collect()
}

/// SHAPE. CR 608.2c: a plural token-batch reference names the batch.
#[test]
fn sacrifice_parser_distinguishes_plural_token_batches() {
    use SacrificeSelection::{Count, EntireReferent};
    let one = Count(QuantityExpr::Fixed { value: 1 });
    for (name, oracle, kind) in [
        ("Force of Rage", FORCE, "Instant"),
        ("Thatcher Revolt", THATCHER, "Sorcery"),
    ] {
        assert_eq!(
            parsed_sacrifices(name, oracle, kind),
            [(
                TargetFilter::TrackedSet {
                    id: TrackedSetId(0)
                },
                EntireReferent
            )],
            "{name}"
        );
    }
    assert_eq!(
        parsed_sacrifices("Chandra, Acolyte of Flame", CHANDRA, "Planeswalker"),
        [(TargetFilter::LastCreated, EntireReferent)]
    );
    assert_eq!(
        parsed_sacrifices("Orthion, Hero of Lavabrink", ORTHION, "Creature"),
        [
            (TargetFilter::LastCreated, one.clone()),
            (TargetFilter::LastCreated, EntireReferent),
        ]
    );
    assert_eq!(
        parsed_sacrifices("Depthshaker Titan", DEPTHSHAKER, "Creature"),
        [(TargetFilter::ParentTarget, one.clone())]
    );
    let malik = parsed_sacrifices("Malik, Grim Manipulator", MALIK, "Creature");
    assert!(!malik.is_empty());
    assert!(
        malik.iter().all(|(_, selection)| *selection == one),
        "{malik:?}"
    );
}

fn assert_priority(runner: &GameRunner) {
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

fn on_battlefield(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().battlefield.contains(&id)
}

fn cast_cleanup_spell(
    oracle: &str,
    name: &str,
    instant: bool,
) -> (GameRunner, Vec<ObjectId>, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, name, instant, oracle)
        .id();
    let unrelated_spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Raise the Alarm",
            true,
            "Create two 1/1 white Soldier creature tokens.",
        )
        .id();
    for _ in 0..3 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }
    let mut runner = scenario.build();
    runner.cast(spell).resolve();
    assert_priority(&runner);
    let created = runner.state().last_created_token_ids.clone();
    let installed = &runner.state().delayed_triggers[0].ability.effect;
    assert!(
        matches!(
            installed,
            Effect::Sacrifice { target: TargetFilter::TrackedSet { id }, .. } if id.0 != 0
        ),
        "cleanup must bind a concrete set at installation: {installed:?}"
    );
    (runner, created, unrelated_spell)
}

fn publish_unrelated_tokens(runner: &mut GameRunner, spell: ObjectId) -> Vec<ObjectId> {
    runner.cast(spell).resolve();
    assert_priority(runner);
    let unrelated = runner.state().last_created_token_ids.clone();
    assert_eq!(unrelated.len(), 2);
    // Raise the Alarm has no reader of its publication. Seed that independent
    // tracked set explicitly so the sentinel's firing-time fallback has a decoy.
    let state = runner.state_mut();
    let unrelated_set = TrackedSetId(state.next_tracked_set_id);
    state.next_tracked_set_id += 1;
    state
        .tracked_object_sets
        .insert(unrelated_set, unrelated.clone());
    unrelated
}

/// CR 603.7c + CR 701.21a: the delayed effect reads only the batch it named.
#[test]
fn force_of_rage_sacrifices_both_original_tokens_on_its_controllers_upkeep() {
    let (mut runner, created, unrelated_spell) = cast_cleanup_spell(FORCE, "Force of Rage", true);
    assert_eq!(created.len(), 2);
    let unrelated = publish_unrelated_tokens(&mut runner, unrelated_spell);
    runner.advance_to_combat();
    runner.declare_attackers(&[]).unwrap();
    runner.advance_to_end_step();
    runner.advance_to_upkeep();
    assert_eq!(runner.state().phase, Phase::Upkeep);
    assert!(created.iter().all(|id| on_battlefield(&runner, *id)));
    runner.advance_to_end_step();
    runner.advance_to_upkeep();
    assert_eq!(runner.state().phase, Phase::Upkeep);
    runner.advance_until_stack_empty();
    assert_priority(&runner);
    assert!(created.iter().all(|id| !on_battlefield(&runner, *id)));
    assert!(unrelated.iter().all(|id| on_battlefield(&runner, *id)));
}

/// CR 603.7c + CR 701.21a: only original, still-controlled permanents can be sacrificed.
#[test]
fn thatcher_revolt_sacrifices_only_its_remaining_controlled_token() {
    let (mut runner, created, unrelated_spell) =
        cast_cleanup_spell(THATCHER, "Thatcher Revolt", false);
    let [departed, stolen, eligible] = created[..] else {
        panic!("expected three Humans, got {created:?}");
    };
    let unrelated = publish_unrelated_tokens(&mut runner, unrelated_spell);
    move_to_zone(runner.state_mut(), departed, Zone::Exile, &mut Vec::new());
    runner.state_mut().debug_mode = true;
    runner
        .act(GameAction::Debug(DebugAction::SetController {
            object_id: stolen,
            controller: P1,
        }))
        .unwrap();
    assert_eq!(runner.state().objects[&stolen].controller, P1);
    assert!(on_battlefield(&runner, eligible));
    runner.advance_to_combat();
    runner.declare_attackers(&[]).unwrap();
    runner.advance_to_end_step();
    assert_eq!(runner.state().phase, Phase::End);
    runner.advance_until_stack_empty();
    assert_priority(&runner);
    assert!(!on_battlefield(&runner, eligible));
    assert!(on_battlefield(&runner, stolen));
    assert!(unrelated.iter().all(|id| on_battlefield(&runner, *id)));
}

fn mobilize(runner: &mut GameRunner, attacker: ObjectId) -> Vec<ObjectId> {
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .unwrap();
    runner.advance_until_stack_empty();
    assert_priority(runner);
    let installed = &runner.state().delayed_triggers[0].ability.effect;
    assert!(
        matches!(
            installed,
            Effect::Sacrifice {
                target: TargetFilter::LastCreated,
                count: SacrificeSelection::EntireReferent,
                ..
            }
        ),
        "{installed:?}"
    );
    runner.state().last_created_token_ids.clone()
}

#[test]
fn mobilize_x_sacrifices_created_warriors_after_graveyard_is_emptied() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario
        .add_creature(P0, "Avenger of the Fallen", 2, 4)
        .from_oracle_text_with_keywords(&["deathtouch", "mobilize"], AVENGER)
        .id();
    let graveyard = [
        scenario
            .add_creature_to_graveyard(P0, "Runeclaw Bear", 2, 2)
            .id(),
        scenario
            .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
            .id(),
    ];
    let unrelated = scenario.add_creature(P0, "Forest Bear", 2, 2).id();
    let mut runner = scenario.build();
    let created = mobilize(&mut runner, attacker);
    assert_eq!(created.len(), 2);
    for id in graveyard {
        move_to_zone(runner.state_mut(), id, Zone::Exile, &mut Vec::new());
    }
    assert!(runner.state().players[0].graveyard.is_empty());
    runner.advance_until_stack_empty();
    runner.combat_damage();
    runner.advance_to_end_step();
    assert_eq!(runner.state().phase, Phase::End);
    runner.advance_until_stack_empty();
    assert_priority(&runner);
    assert!(created.iter().all(|id| !on_battlefield(&runner, *id)));
    assert!(on_battlefield(&runner, attacker));
    assert!(on_battlefield(&runner, unrelated));
}

/// CR 701.21a + CR 609.3: the whole batch is subject to a sacrifice prohibition.
#[test]
fn mobilize_respects_zurgos_warrior_prohibition() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let zurgo = scenario
        .add_creature_from_oracle(P0, "Zurgo, Thunder's Decree", 2, 4, ZURGO)
        .id();
    let mut runner = scenario.build();
    let created = mobilize(&mut runner, zurgo);
    assert_eq!(created.len(), 2);
    runner.combat_damage();
    runner.advance_to_end_step();
    assert_eq!(runner.state().phase, Phase::End);
    // The printed grant currently misparses as a prohibition on Zurgo.
    // Stage the granted prohibition on one Warrior to isolate cleanup and
    // prove that it still sacrifices the unprotected member.
    let [protected, unprotected] = created[..] else {
        panic!("expected two Warriors, got {created:?}");
    };
    let warrior = runner.state_mut().objects.get_mut(&protected).unwrap();
    Arc::make_mut(&mut warrior.base_static_definitions).push(
        StaticDefinition::new(StaticMode::Other("CantBeSacrificed".into()))
            .affected(TargetFilter::SelfRef),
    );
    evaluate_layers(runner.state_mut());
    assert!(object_has_static_other(
        runner.state(),
        protected,
        "CantBeSacrificed"
    ));
    assert!(!object_has_static_other(
        runner.state(),
        unprotected,
        "CantBeSacrificed"
    ));
    runner.advance_until_stack_empty();
    assert_priority(&runner);
    assert!(runner.state().delayed_triggers.is_empty());
    assert!(on_battlefield(&runner, protected));
    assert!(!on_battlefield(&runner, unprotected));
}

#[test]
fn sacrifice_count_legacy_serde() {
    for (json, expected) in [
        (r#"{"type":"Sacrifice","count":2}"#, 2),
        (r#"{"type":"Sacrifice"}"#, 1),
        (
            r#"{"type":"Sacrifice","count":{"type":"Fixed","value":3}}"#,
            3,
        ),
    ] {
        let effect: Effect = serde_json::from_str(json).unwrap();
        assert_eq!(
            effect.count_expr(),
            Some(&QuantityExpr::Fixed { value: expected })
        );
        assert_eq!(
            serde_json::to_value(effect).unwrap()["count"],
            serde_json::json!({"type":"Fixed", "value": expected})
        );
    }
    let entire = Effect::Sacrifice {
        target: TargetFilter::LastCreated,
        count: SacrificeSelection::EntireReferent,
        min_count: 0,
    };
    assert_eq!(
        serde_json::from_value::<Effect>(serde_json::to_value(&entire).unwrap()).unwrap(),
        entire
    );
}

fn counted_by(filter: TargetFilter) -> SacrificeSelection {
    QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount { filter },
    }
    .into()
}

fn assert_choose_one(runner: &mut GameRunner, a: ObjectId, b: ObjectId) {
    let WaitingFor::EffectZoneChoice {
        player,
        count,
        cards,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected a counted sacrifice choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*count, 1);
    assert_eq!(cards.len(), 2);
    assert!(cards.contains(&a) && cards.contains(&b));
    runner
        .act(GameAction::SelectCards { cards: vec![b] })
        .unwrap();
    assert_priority(runner);
    assert!(on_battlefield(runner, a));
    assert!(!on_battlefield(runner, b));
}

// These synthetic definitions isolate mismatched bindings that the parser
// need not emit. Descendants' Fury is the printed counted-set control.
#[test]
fn sacrifice_tracked_set_count_can_read_a_different_set() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let (victims, quantity) = (TrackedSetId(1), TrackedSetId(2));
    let a = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let b = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
    let c = scenario.add_creature(P0, "Balduvian Bears", 2, 2).id();
    let target = TargetFilter::TrackedSet { id: victims };
    let spell = scenario
        .add_spell_to_hand(P0, "Counted sacrifice binder probe", true)
        .with_ability(Effect::Sacrifice {
            target: target.clone(),
            count: counted_by(TargetFilter::TrackedSet { id: quantity }),
            min_count: 0,
        })
        .id();
    let entire = scenario
        .add_spell_to_hand(P0, "Whole sacrifice binder probe", true)
        .with_ability(Effect::Sacrifice {
            target,
            count: SacrificeSelection::EntireReferent,
            min_count: 0,
        })
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .tracked_object_sets
        .insert(victims, vec![a, b]);
    runner
        .state_mut()
        .tracked_object_sets
        .insert(quantity, vec![c]);
    runner.state_mut().next_tracked_set_id = 3;

    let mut whole_runner = GameRunner::from_state(runner.state().clone());
    whole_runner.cast(entire).resolve();
    assert_priority(&whole_runner);
    assert!(!on_battlefield(&whole_runner, a));
    assert!(!on_battlefield(&whole_runner, b));
    assert!(on_battlefield(&whole_runner, c));

    runner.cast(spell).resolve();
    assert_choose_one(&mut runner, a, b);
    assert!(on_battlefield(&runner, c));
}

#[test]
fn sacrifice_last_created_count_can_read_a_different_population() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let producer = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Raise the Alarm",
            true,
            "Create two 1/1 white Soldier creature tokens.",
        )
        .id();
    let quantity_object = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    runner.cast(producer).resolve();
    assert_priority(&runner);
    let [a, b] = runner.state().last_created_token_ids[..] else {
        panic!("expected two Soldiers");
    };

    // The legacy count path uses the LastCreated population as its choice
    // pool when there are no explicit objects on the resolving ability.
    let counted = ResolvedAbility::new(
        Effect::Sacrifice {
            target: TargetFilter::LastCreated,
            count: counted_by(TargetFilter::SpecificObject {
                id: quantity_object,
            }),
            min_count: 0,
        },
        vec![],
        quantity_object,
        P0,
    );
    let mut whole_runner = GameRunner::from_state(runner.state().clone());
    // A delayed LastCreated cleanup carries the installed cohort in targets.
    let entire = ResolvedAbility::new(
        Effect::Sacrifice {
            target: TargetFilter::LastCreated,
            count: SacrificeSelection::EntireReferent,
            min_count: 0,
        },
        vec![TargetRef::Object(a), TargetRef::Object(b)],
        quantity_object,
        P0,
    );
    engine::game::effects::sacrifice::resolve(whole_runner.state_mut(), &entire, &mut Vec::new())
        .expect("resolve whole sacrifice");
    assert_priority(&whole_runner);
    assert!(!on_battlefield(&whole_runner, a));
    assert!(!on_battlefield(&whole_runner, b));
    assert!(on_battlefield(&whole_runner, quantity_object));

    engine::game::effects::sacrifice::resolve(runner.state_mut(), &counted, &mut Vec::new())
        .expect("resolve counted sacrifice");
    assert_choose_one(&mut runner, a, b);
    assert!(on_battlefield(&runner, quantity_object));
}
