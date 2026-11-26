//! Regression tests for Dalkovan Encampment:
//! - Issue #433 — delayed "Whenever you attack this turn" trigger does nothing.
//! - Issue #2433 — warrior tokens are created but not sacrificed at the next
//!   end step.
//!
//! Dalkovan Encampment's activated ability is:
//!   `{2}{W}, {T}: Whenever you attack this turn, create two 1/1 red Warrior
//!    creature tokens that are tapped and attacking. ...`
//!
//! The inner clause is a delayed triggered ability (CR 603.7b) whose condition
//! is prefix-stripped to the bare string `"you attack"`. Before the #433 fix the
//! trigger parser only recognized the prefixed forms `"whenever you attack"` /
//! `"when you attack"`, so the bare condition fell through to
//! `TriggerMode::Unknown` and the delayed trigger never fired — the Warrior
//! tokens were never created.
//!
//! This test drives the full pipeline through `apply`: activate the ability,
//! declare an attacker, and assert two tapped+attacking Warrior tokens appear.
//! There is no synthetic `process_triggers` call.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{DelayedTriggerCondition, Effect, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const DALKOVAN_ATTACK_ABILITY: &str =
    "{2}{W}, {T}: Whenever you attack this turn, create two 1/1 red \
     Warrior creature tokens that are tapped and attacking. Sacrifice \
     them at the beginning of the next end step.";

/// CR 508.1 + CR 603.7a + CR 603.7b: activating Dalkovan Encampment's ability creates a
/// "Whenever you attack this turn" delayed trigger; declaring an attacker fires
/// it and creates two 1/1 red Warrior tokens that are tapped and attacking.
#[test]
fn dalkovan_encampment_delayed_trigger_creates_warrior_tokens() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Dalkovan Encampment's third ability only. `add_creature_from_oracle`
    // parses the activated ability from Oracle text; card type is irrelevant
    // to activated-ability resolution.
    let encampment = scenario
        .add_creature_from_oracle(P0, "Dalkovan Encampment", 0, 1, DALKOVAN_ATTACK_ABILITY)
        .id();

    // A separate creature to attack with.
    let attacker = scenario.add_creature(P0, "Grizzly Bear", 2, 2).id();

    // Pay {2}{W} from the pool so activation does not require land taps.
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();

    // Locate the activated ability index whose effect is the delayed trigger.
    let ability_index = runner
        .state()
        .objects
        .get(&encampment)
        .expect("encampment exists")
        .abilities
        .iter()
        .position(|a| matches!(a.effect.as_ref(), Effect::CreateDelayedTrigger { .. }))
        .expect("Dalkovan Encampment must have a CreateDelayedTrigger ability");

    // Activate the {2}{W},{T} ability through the activation pipeline, paying the
    // mana cost from the funded pool; resolution creates the "Whenever you attack
    // this turn" delayed trigger (CR 603.7b).
    runner.activate(encampment, ability_index).resolve();

    // Declare the attacker — this is the event the delayed trigger watches.
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");
    runner.advance_until_stack_empty();

    // Two 1/1 red Warrior tokens, tapped and attacking, must now exist on P0's
    // battlefield.
    let warriors: Vec<ObjectId> = runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.controller == P0
                && o.zone == Zone::Battlefield
                && o.card_types
                    .subtypes
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case("warrior"))
        })
        .map(|o| o.id)
        .collect();

    assert_eq!(
        warriors.len(),
        2,
        "the delayed 'you attack' trigger must create exactly two Warrior tokens"
    );

    for &w in &warriors {
        let obj = runner.state().objects.get(&w).expect("token exists");
        assert!(obj.tapped, "Warrior tokens must enter tapped (CR 508.4)");
    }

    // CR 508.4: the tokens enter attacking — they must appear in the combat
    // attacker list (not declared as attackers, but attacking creatures).
    let combat = runner
        .state()
        .combat
        .as_ref()
        .expect("combat state present during DeclareAttackers");
    for &w in &warriors {
        let info = combat
            .attackers
            .iter()
            .find(|a| a.object_id == w)
            .expect("Warrior token must be an attacking creature");
        assert_eq!(
            info.defending_player, P1,
            "Warrior token must attack the opponent declared this combat, not its controller"
        );
    }
}

fn warrior_ids_on_battlefield(runner: &super::rules::GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.controller == P0
                && o.zone == Zone::Battlefield
                && o.card_types
                    .subtypes
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case("warrior"))
        })
        .map(|o| o.id)
        .collect()
}

/// CR 603.7c + CR 513.1: after the attack-delayed trigger creates Warrior
/// tokens, the chained "sacrifice them at the beginning of the next end step"
/// delayed trigger must fire and move those tokens to the graveyard.
#[test]
fn dalkovan_encampment_warriors_sacrificed_at_next_end_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let encampment = scenario
        .add_creature_from_oracle(P0, "Dalkovan Encampment", 0, 1, DALKOVAN_ATTACK_ABILITY)
        .id();
    let attacker = scenario.add_creature(P0, "Grizzly Bear", 2, 2).id();

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();

    let ability_index = runner
        .state()
        .objects
        .get(&encampment)
        .expect("encampment exists")
        .abilities
        .iter()
        .position(|a| matches!(a.effect.as_ref(), Effect::CreateDelayedTrigger { .. }))
        .expect("Dalkovan Encampment must have a CreateDelayedTrigger ability");

    runner.activate(encampment, ability_index).resolve();

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");
    runner.advance_until_stack_empty();

    let warriors = warrior_ids_on_battlefield(&runner);
    assert_eq!(
        warriors.len(),
        2,
        "attack must create exactly two Warrior tokens before the end step"
    );

    let sacrifice_delayed = runner.state().delayed_triggers.iter().find(|dt| {
        matches!(
            dt.condition,
            DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
        ) && matches!(
            &dt.ability.effect,
            Effect::Sacrifice {
                target: TargetFilter::LastCreated,
                ..
            }
        )
    });
    assert!(
        sacrifice_delayed.is_some(),
        "attack resolution must install an AtNextPhase{{End}} Sacrifice delayed trigger; \
         got {:?}",
        runner.state().delayed_triggers
    );
    let sacrifice_delayed = sacrifice_delayed.unwrap();
    for &w in &warriors {
        assert!(
            sacrifice_delayed
                .ability
                .targets
                .contains(&TargetRef::Object(w)),
            "sacrifice delayed trigger must snapshot warrior token {w:?}"
        );
    }

    runner.pass_both_players();
    runner.advance_until_stack_empty();

    let mut guard = 0;
    while runner.state().delayed_triggers.iter().any(|dt| {
        matches!(
            dt.condition,
            DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
        )
    }) || !runner.state().stack.is_empty()
    {
        guard += 1;
        assert!(
            guard < 256,
            "sacrifice delayed trigger never fired; phase = {:?}, waiting_for = {:?}, dt = {}, stack = {}",
            runner.state().phase,
            runner.state().waiting_for,
            runner.state().delayed_triggers.len(),
            runner.state().stack.len(),
        );
        match &runner.state().waiting_for {
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("empty block declaration should succeed");
            }
            _ => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass while advancing to end step should succeed");
            }
        }
    }

    assert!(
        warrior_ids_on_battlefield(&runner).is_empty(),
        "Warrior tokens must be sacrificed at the beginning of the next end step"
    );
    assert!(
        !runner.state().delayed_triggers.iter().any(|dt| {
            matches!(
                dt.condition,
                DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
            ) && matches!(&dt.ability.effect, Effect::Sacrifice { .. })
        }),
        "the end-step sacrifice delayed trigger must be consumed after firing"
    );
}

fn advance_dalkovan_combat(
    runner: &mut super::rules::GameRunner,
    stop: impl Fn(&super::rules::GameRunner) -> bool,
) {
    for _ in 0..256 {
        if stop(runner) {
            return;
        }
        let action = match runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            ref other => panic!("unexpected combat advancement prompt: {other:?}"),
        };
        runner.act(action).expect("advance combat or turn");
    }
    panic!("combat advancement did not reach its stop");
}

#[test]
fn dalkovan_encampment_installs_a_distinct_token_batch_per_declaration() {
    use engine::types::mana::{ManaColor, ManaCost};
    use std::collections::BTreeSet;

    const DALKOVAN: &str = "This land enters tapped unless you control a Swamp or a Mountain.\n{T}: Add {W}.\n{2}{W}, {T}: Whenever you attack this turn, create two 1/1 red Warrior creature tokens that are tapped and attacking. Sacrifice them at the beginning of the next end step.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Mountain"]);
    scenario.with_library_top(P1, &["Mountain"]);
    let encampment = scenario
        .add_land_from_oracle(P0, "Dalkovan Encampment", DALKOVAN)
        .id();
    let mine = scenario
        .add_land_from_oracle(
            P0,
            "Strip Mine",
            "{T}: Add {C}.\n{T}, Sacrifice this land: Destroy target land.",
        )
        .id();
    let prior_producer = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Raise the Alarm",
            true,
            "Create two 1/1 white Soldier creature tokens.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();
    let extra_combats = scenario.add_spell_to_hand_from_oracle(
        P0, "Full Throttle", false, "After this main phase, there are two additional combat phases.\nAt the beginning of each combat this turn, untap all creatures that attacked this turn.",
    ).with_mana_cost(ManaCost::zero()).id();
    let attackers = [
        scenario.add_creature(P0, "First attacker", 2, 2).id(),
        scenario.add_creature(P0, "Second attacker", 2, 2).id(),
    ];
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let mut runner = scenario.build();
    // CR 508.4: P1 is the sole legal attack destination. This is a two-player
    // board with no planeswalkers or battles.
    let prior = runner.cast(prior_producer).resolve();
    assert_eq!(prior.zone_of(prior_producer), Zone::Graveyard);
    let soldiers = |runner: &super::rules::GameRunner| {
        runner
            .state()
            .objects
            .values()
            .filter(|obj| obj.zone == Zone::Battlefield && obj.name == "Soldier")
            .map(|obj| obj.id)
            .collect::<BTreeSet<_>>()
    };
    let prior_ids = soldiers(&runner);
    assert_eq!(prior_ids.len(), 2);
    runner.cast(extra_combats).resolve();
    assert_eq!(runner.state().extra_phases.len(), 2);
    let activate = runner.state().objects[&encampment]
        .abilities
        .iter()
        .position(|ability| matches!(ability.effect.as_ref(), Effect::CreateDelayedTrigger { .. }))
        .expect("Dalkovan's attack watcher ability");
    let paid = runner.activate(encampment, activate).resolve();
    assert_eq!(paid.mana_pool_total(P0), 0);
    assert!(runner.state().objects[&encampment].tapped);
    let watches = |runner: &super::rules::GameRunner| {
        runner
            .state()
            .delayed_triggers
            .iter()
            .filter(|watch| watch.source_id == encampment)
            .count()
    };
    assert_eq!(watches(&runner), 1);
    assert!(warrior_ids_on_battlefield(&runner).is_empty());

    // Remove the source via its opponent-independent land destruction ability
    // while still in main-phase priority.
    let destroy = runner.state().objects[&mine]
        .abilities
        .iter()
        .position(|ability| matches!(ability.effect.as_ref(), Effect::Destroy { .. }))
        .expect("Strip Mine's land destruction ability");
    let removed = runner
        .activate(mine, destroy)
        .target_objects(&[encampment])
        .resolve();
    assert_eq!(removed.zone_of(encampment), Zone::Graveyard);

    let at_declaration = |r: &super::rules::GameRunner| {
        matches!(r.state().waiting_for, WaitingFor::DeclareAttackers { .. })
    };
    let declare = |runner: &mut super::rules::GameRunner, ids: &[ObjectId]| {
        runner
            .act(GameAction::DeclareAttackers {
                attacks: ids
                    .iter()
                    .map(|&id| (id, AttackTarget::Player(P1)))
                    .collect(),
                bands: vec![],
            })
            .expect("declare attackers");
        runner.advance_until_stack_empty();
        assert!(runner.state().stack.is_empty());
    };
    advance_dalkovan_combat(&mut runner, at_declaration);
    assert_eq!(
        soldiers(&runner),
        prior_ids,
        "the earlier producer's tokens are present before declaration"
    );
    declare(&mut runner, &[]);
    assert_eq!(watches(&runner), 1);
    assert!(warrior_ids_on_battlefield(&runner).is_empty());

    let mut all_warriors = BTreeSet::new();
    let mut expected_sets = Vec::new();
    let old_turn = runner.state().turn_number;
    // CR 603.7b + CR 508.4: each declaration this turn creates its own attacking batch.
    for attackers in [&attackers[..], &attackers[..1]] {
        advance_dalkovan_combat(&mut runner, at_declaration);
        assert_eq!(runner.state().turn_number, old_turn);
        declare(&mut runner, attackers);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        let present: BTreeSet<_> = warrior_ids_on_battlefield(&runner).into_iter().collect();
        let batch: BTreeSet<_> = present.difference(&all_warriors).copied().collect();
        assert_eq!(batch.len(), 2);
        assert!(batch.is_disjoint(&prior_ids));
        all_warriors.extend(&batch);
        for id in &batch {
            let token = &runner.state().objects[id];
            assert!(token.is_token && token.tapped);
            assert_eq!((token.power, token.toughness), (Some(1), Some(1)));
            assert_eq!(token.color, [ManaColor::Red]);
            assert!(runner
                .state()
                .combat
                .as_ref()
                .unwrap()
                .attackers
                .iter()
                .any(|attack| attack.object_id == *id && attack.defending_player == P1));
        }

        expected_sets.push(batch);
        let mut installed_sets: Vec<BTreeSet<_>> = runner
            .state()
            .delayed_triggers
            .iter()
            .filter(|watch| {
                watch.source_id == encampment
                    && matches!(
                        watch.condition,
                        DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
                    )
            })
            .map(|watch| {
                assert!(matches!(
                    watch.ability.effect,
                    Effect::Sacrifice {
                        target: TargetFilter::LastCreated,
                        ..
                    }
                ));
                watch
                    .ability
                    .targets
                    .iter()
                    .map(|target| match target {
                        TargetRef::Object(id) => *id,
                        other => panic!("unexpected sacrifice target: {other:?}"),
                    })
                    .collect()
            })
            .collect();
        installed_sets.sort();
        let mut expected = expected_sets.clone();
        expected.sort();
        assert_eq!(
            installed_sets, expected,
            "installation snapshots exactly the distinct per-firing sets"
        );
    }

    advance_dalkovan_combat(&mut runner, |r| {
        matches!(r.state().waiting_for, WaitingFor::OrderTriggers { .. })
    });
    assert_eq!(runner.state().phase, Phase::End);
    assert_eq!(runner.state().turn_number, old_turn);
    assert_eq!(
        warrior_ids_on_battlefield(&runner)
            .into_iter()
            .collect::<BTreeSet<_>>(),
        all_warriors,
        "both batches must survive until the end-step triggers resolve"
    );
    let WaitingFor::OrderTriggers { player, triggers } = &runner.state().waiting_for else {
        panic!("the two end-step installers must reach trigger ordering");
    };
    assert_eq!(*player, P0);
    assert_eq!(triggers.len(), 2);
    assert!(triggers
        .iter()
        .all(|trigger| trigger.source_id == encampment));
    runner
        .act(GameAction::OrderTriggers { order: vec![0, 1] })
        .expect("order the two end-step triggers");

    assert_eq!(runner.state().stack.len(), expected_sets.len());
    for remaining in (0..expected_sets.len()).rev() {
        let entry = runner.state().stack.last().expect("end-step sacrifice");
        let engine::types::game_state::StackEntryKind::TriggeredAbility { ability, .. } =
            &entry.kind
        else {
            panic!("expected a triggered ability");
        };
        assert_eq!(entry.source_id, encampment);
        let batch: BTreeSet<_> = ability
            .targets
            .iter()
            .map(|target| {
                let TargetRef::Object(id) = target else {
                    panic!("unexpected sacrifice target: {target:?}");
                };
                *id
            })
            .collect();
        let index = expected_sets
            .iter()
            .position(|expected| *expected == batch)
            .expect("the trigger must bind a distinct installed batch");
        expected_sets.remove(index);

        // CR 603.7c + CR 701.21a: resolve one trigger at a time.
        let mut sacrificed = Vec::new();
        for _ in 0..16 {
            assert_eq!(runner.state().phase, Phase::End);
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            let result = runner
                .act(GameAction::PassPriority)
                .expect("resolve end-step sacrifice");
            for event in result.events {
                if let GameEvent::PermanentSacrificed {
                    object_id,
                    player_id,
                } = event
                {
                    assert_eq!(player_id, P0);
                    sacrificed.push(object_id);
                }
            }
            if runner.state().stack.len() != remaining + 1 {
                break;
            }
        }
        assert_eq!(runner.state().stack.len(), remaining);
        sacrificed.sort_unstable();
        assert_eq!(sacrificed, batch.into_iter().collect::<Vec<_>>());
        assert_eq!(
            warrior_ids_on_battlefield(&runner)
                .into_iter()
                .collect::<BTreeSet<_>>(),
            expected_sets
                .iter()
                .flatten()
                .copied()
                .collect::<BTreeSet<_>>(),
            "Warriors bound to a pending trigger must survive"
        );
    }
    assert!(warrior_ids_on_battlefield(&runner).is_empty());
    assert_eq!(
        soldiers(&runner),
        prior_ids,
        "unrelated tokens must survive"
    );
    // CR 514.2: the attack watcher expires at cleanup.
    advance_dalkovan_combat(&mut runner, |r| {
        r.state().turn_number > old_turn && r.state().active_player == P0 && at_declaration(r)
    });
    assert_eq!(watches(&runner), 0);
    let before = warrior_ids_on_battlefield(&runner);
    declare(&mut runner, &attackers[..1]);
    assert!(runner
        .state()
        .combat
        .as_ref()
        .unwrap()
        .attackers
        .iter()
        .any(|attack| attack.object_id == attackers[0] && attack.defending_player == P1));
    assert_eq!(warrior_ids_on_battlefield(&runner), before);
}
