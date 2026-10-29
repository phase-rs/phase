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

use engine::game::game_object::ObjectIncarnationRefExt;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{DelayedTriggerCondition, Effect, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
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
                target: TargetFilter::Or { .. },
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

const FULL_DALKOVAN: &str="This land enters tapped unless you control a Swamp or a Mountain.\n{T}: Add {W}.\n{2}{W}, {T}: Whenever you attack this turn, create two 1/1 red Warrior creature tokens that are tapped and attacking. Sacrifice them at the beginning of the next end step.";

fn full_dalkovan_replacement_case(kind: usize, reload: bool) {
    use super::jev_token_followup::{fund_creation_response, reach_cleanup_stack};
    use engine::game::scenario::GameRunner;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        scenario.with_life(player, 100);
        scenario.with_library_top(player, &["Plains", "Plains", "Plains", "Plains"]);
    }
    let source = scenario
        .add_land_from_oracle(P0, "Dalkovan Encampment", FULL_DALKOVAN)
        .id();
    let attacker = scenario.add_creature(P0, "Declared attacker", 2, 3).id();
    let other = scenario.add_creature(P0, "Unrelated creature", 4, 5).id();
    if matches!(kind, 0 | 6 | 7) {
        scenario.add_creature_from_oracle(P0,"Chatterfang, Squirrel General",3,3,"Forestwalk (This creature can't be blocked as long as defending player controls a Forest.)\nIf one or more tokens would be created under your control, those tokens plus that many 1/1 green Squirrel creature tokens are created instead.\n{B}, Sacrifice X Squirrels: Target creature gets +X/-X until end of turn.");
    }
    if matches!(kind, 1 | 3 | 4) {
        scenario.add_creature_from_oracle(P0,"Jinnie Fay, Jetmir's Second",3,3,"If you would create one or more tokens, you may instead create that many 2/2 green Cat creature tokens with haste or that many 3/1 green Dog creature tokens with vigilance.");
    }
    if kind == 5 {
        scenario.add_enchantment_from_oracle(P0,"Divine Visitation","If one or more creature tokens would be created under your control, that many 4/4 white Angel creature tokens with flying and vigilance are created instead.");
    }
    if kind == 7 {
        scenario.add_enchantment_from_oracle(P0, "Root Maze", "Artifacts and lands enter tapped.");
    }
    let cutpurse=scenario.add_creature_to_hand(P1,"Crafty Cutpurse",2,2).from_oracle_text_with_keywords(&["Flash"],"Flash\nWhen this creature enters, each token that would be created under an opponent's control this turn is created under your control instead.").id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic source removal",
            true,
            "Exile target permanent.",
        )
        .id();
    let prior_maker = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic prior Squirrel",
            false,
            "Create a 1/1 green Squirrel creature token.",
        )
        .id();
    let later_maker = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic later Squirrel",
            true,
            "Create a 1/1 green Squirrel creature token.",
        )
        .id();
    let regain=scenario.add_spell_to_hand_from_oracle(P0,"Ray of Command",true,"Untap target creature an opponent controls and gain control of it until end of turn. That creature gains haste until end of turn. When you lose control of the creature, tap it.").id();
    let mut runner = scenario.build();
    if kind == 6 {
        super::jev_token_followup::install_creation_pause_isolation(&mut runner, 1);
    }
    fund_creation_response(&mut runner, P0);
    let mut independent_squirrels = Vec::new();
    if kind == 0 {
        let prior = runner.cast(prior_maker).resolve();
        independent_squirrels.extend(prior.events().iter().filter_map(|event| {
            if let engine::types::events::GameEvent::TokenCreated { object_id, .. } = event {
                Some(*object_id)
            } else {
                None
            }
        }));
        assert_eq!(independent_squirrels.len(), 2);
    }

    if matches!(kind, 2 | 8) {
        runner.act(GameAction::PassPriority).unwrap();
        runner.cast(cutpurse).resolve();
        super::jev_token_followup::creation_priority_to_caster(&mut runner);
    }
    let index = runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| matches!(ability.effect.as_ref(), Effect::CreateDelayedTrigger { .. }))
        .unwrap();
    runner.activate(source, index).resolve();
    if kind == 7 {
        super::jev_token_followup::creation_priority_to_caster(&mut runner);
        runner.cast(removal).target_object(source).resolve();
        assert_eq!(runner.state().objects[&source].zone, Zone::Exile);
    }

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .unwrap();
    let mut events = Vec::new();
    for _ in 0..80 {
        if runner.state().delayed_triggers.iter().any(|trigger| {
            trigger.source_id == source
                && matches!(trigger.ability.effect, Effect::Sacrifice { .. })
        }) {
            break;
        }
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { .. } => {
                if reload {
                    runner = GameRunner::from_state(
                        serde_json::from_value::<engine::types::resolution::ResolutionStateWire>(
                            serde_json::to_value(
                                engine::types::resolution::ResolutionStateWire::from_game_state(
                                    runner.state().clone(),
                                ),
                            )
                            .unwrap(),
                        )
                        .unwrap()
                        .into_game_state(),
                    );
                }
                GameAction::ChooseReplacement {
                    index: if kind == 4 { 1 } else { 0 },
                }
            }
            WaitingFor::ChooseOneOfBranch { .. } => GameAction::ChooseBranch {
                index: if kind == 3 { 0 } else { 1 },
            },
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected Dalkovan replacement: {other:?}"),
        };
        events.extend(runner.act(action).unwrap().events);
    }
    let delayed = runner
        .state()
        .delayed_triggers
        .iter()
        .find(|trigger| {
            trigger.source_id == source
                && matches!(trigger.ability.effect, Effect::Sacrifice { .. })
        })
        .unwrap();
    assert_eq!(delayed.controller, P0);
    let births = delayed.ability.target_incarnations.clone();
    assert_eq!(births.len(), if matches!(kind, 0 | 6 | 7) { 4 } else { 2 });
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, engine::types::events::GameEvent::TokenCreated { .. }))
            .count(),
        births.len()
    );
    for member in &births {
        let object = &runner.state().objects[&member.object_id];
        assert!(object.tapped);
        assert_eq!(
            object.controller,
            if matches!(kind, 2 | 8) { P1 } else { P0 }
        );
        assert_eq!(object.owner, object.controller);
        let attacking = runner
            .state()
            .combat
            .as_ref()
            .unwrap()
            .attackers
            .iter()
            .any(|attacker| attacker.object_id == member.object_id);
        assert_eq!(attacking, !matches!(kind, 2 | 8));
        if kind == 3 {
            assert_eq!(
                (
                    object.name.as_str(),
                    object.base_power,
                    object.base_toughness
                ),
                ("Cat", Some(2), Some(2))
            );
            assert!(object
                .keywords
                .contains(&engine::types::keywords::Keyword::Haste));
        }
        if kind == 4 {
            assert_eq!(object.name, "Warrior");
        }
        if kind == 5 {
            assert_eq!(
                (
                    object.name.as_str(),
                    object.base_power,
                    object.base_toughness
                ),
                ("Angel", Some(4), Some(4))
            );
            assert!(object
                .keywords
                .contains(&engine::types::keywords::Keyword::Flying));
            assert!(object
                .keywords
                .contains(&engine::types::keywords::Keyword::Vigilance));
        }
        if kind == 6 {
            assert!(object.name == "Warrior" || object.name == "Dog");
        }
        if kind == 1 {
            assert_eq!(
                (
                    object.name.as_str(),
                    object.base_power,
                    object.base_toughness
                ),
                ("Dog", Some(3), Some(1))
            );
        }
        if kind == 0 {
            assert_eq!(
                (object.base_power, object.base_toughness),
                (Some(1), Some(1))
            );
            assert!(object.name == "Warrior" || object.name == "Squirrel");
        }
    }
    // Source departure is a public response after installation. The normal
    // independent trigger must keep its installing player and exact birth set.
    super::jev_token_followup::creation_priority_to_caster(&mut runner);
    fund_creation_response(&mut runner, P0);
    if kind == 0 {
        let later = runner.cast(later_maker).resolve();
        let latest = later
            .events()
            .iter()
            .filter_map(|event| {
                if let engine::types::events::GameEvent::TokenCreated { object_id, .. } = event {
                    Some(*object_id)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(latest.len(), 2);
        independent_squirrels.extend(latest);
        super::jev_token_followup::creation_priority_to_caster(&mut runner);
    }
    if runner.state().objects[&source].zone == Zone::Battlefield {
        runner.cast(removal).target_object(source).resolve();
    }
    assert_eq!(runner.state().objects[&source].zone, Zone::Exile);
    let regained = if kind == 8 {
        let member = births[0];
        super::jev_token_followup::creation_priority_to_caster(&mut runner);
        runner
            .cast(regain)
            .target_object(member.object_id)
            .resolve();
        assert_eq!(runner.state().objects[&member.object_id].controller, P0);
        assert_eq!(
            engine::types::identifiers::ObjectIncarnationRef::from_object(
                &runner.state().objects[&member.object_id]
            ),
            member
        );
        Some(member.object_id)
    } else {
        None
    };
    reach_cleanup_stack(&mut runner, source);
    runner.advance_until_stack_empty();
    for member in births {
        assert_eq!(
            runner.state().objects[&member.object_id].zone == Zone::Battlefield,
            kind == 2 || (kind == 8 && regained != Some(member.object_id))
        );
    }
    assert_eq!(runner.state().objects[&other].zone, Zone::Battlefield);
    for id in independent_squirrels {
        assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&id].name, "Squirrel");
    }
    assert!(!runner
        .state()
        .delayed_triggers
        .iter()
        .any(|trigger| trigger.source_id == source
            && matches!(trigger.ability.effect, Effect::Sacrifice { .. })));
    // The ordinary no-redirect sister above is the positive legal attacker.
}

#[test]
fn full_dalkovan_chatterfang_inherits_tapped_attacking_and_complete_cleanup() {
    full_dalkovan_replacement_case(0, false);
}
#[test]
fn full_dalkovan_jinnie_substitution_pause_reload_keeps_entry_and_cleanup() {
    full_dalkovan_replacement_case(1, true);
}
#[test]
fn redirected_creation_preserves_instruction_seed_and_cleanup_controller() {
    full_dalkovan_replacement_case(2, false);
    full_dalkovan_replacement_case(8, false);
}

#[test]
fn dalkovan_substitution_retains_entry_instructions_and_body() {
    for kind in [1, 3, 4, 5] {
        full_dalkovan_replacement_case(kind, false);
    }
    full_dalkovan_replacement_case(6, true);
    full_dalkovan_replacement_case(7, false);
}
