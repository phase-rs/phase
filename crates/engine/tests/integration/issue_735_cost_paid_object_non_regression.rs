//! Non-regression for issue #735 — distinguish chosen-target power from a
//! genuine entering-object referent. Ent's Fury's own announced target binds
//! `ObjectScope::Target`; Tribute's entering creature keeps
//! `ObjectScope::CostPaidObject`. Genuine source/phase-subject references
//! (Amalia, Lily) retain `Source`.
//!
//! Cards covered:
//!   - Tribute to the World Tree: "Whenever a creature you control enters, draw
//!     a card if its power is 3 or greater. …" — "its" = the ENTERING creature.
//!   - Ent's Fury: "Put a +1/+1 counter on target creature you control if its
//!     power is 4 or greater. …" — "its" = the TARGET creature.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 115.1a + CR 601.2c: Ent's Fury's targets are chosen as it is cast.
//!   - CR 608.2h: the power comparison reads the referenced object's current
//!     info once, when the effect is applied.
//!   - CR 608.2k: an effect referring to an untargeted object previously named
//!     by its cost or trigger condition still affects that object.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::{drain_order_triggers_with_identity, process_triggers};
use engine::game::zones::{create_object, move_to_zone};
use engine::types::ability::EffectKind;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const TRIBUTE_ORACLE: &str = "Whenever a creature you control enters, draw a card if its power is 3 or greater. Otherwise, put two +1/+1 counters on it.";

const ENTS_FURY_ORACLE: &str = "Put a +1/+1 counter on target creature you control if its power is 4 or greater. Then that creature gets +1/+1 until end of turn and fights target creature you don't control.";

/// CR 608.2c + CR 109.5: the declared or carried grammatical player
/// performs a conjugated continuation. A following imperative starts with "you".
#[test]
fn synthetic_declared_and_carried_owner_library_subjects_keep_their_counter_actor() {
    use engine::types::ability::{ControllerRef, Effect};
    use engine::types::player::PlayerId;
    let p2 = PlayerId(2);
    let p3 = PlayerId(3);
    for owner_subject in [false, true] {
        let text = if owner_subject {
            "Target creature's owner shuffles it into their library, then puts a +1/+1 counter on target creature. Put a +1/+1 counter on target creature."
        } else {
            "Target player shuffles their library, then puts a +1/+1 counter on target creature. Put a +1/+1 counter on target creature."
        };
        let mut scenario = GameScenario::new_n_player(4, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let borrowed = scenario
            .add_creature(P1, "Borrowed library subject", 2, 5)
            .controlled_by(p2)
            .id();
        let recipient = scenario
            .add_creature(p3, "Independent counter recipient", 2, 5)
            .controlled_by(p2)
            .id();
        let independent = scenario
            .add_creature(p2, "Imperative counter recipient", 2, 5)
            .controlled_by(p3)
            .id();
        scenario.with_library_top(P1, &["Subject first", "Subject second"]);
        scenario.with_library_top(P0, &["Caster library decoy"]);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Synthetic carried subject", false, text)
            .id();
        let mut runner = scenario.build();
        assert!(
            runner.state().objects[&spell].parse_warnings.is_empty(),
            "{text}: {:?}",
            runner.state().objects[&spell].parse_warnings
        );
        let definition = &runner.state().objects[&spell].abilities[0];
        let mut node = Some(definition);
        let mut actors = Vec::new();
        while let Some(current) = node {
            assert!(!matches!(*current.effect, Effect::Unimplemented { .. }));
            if matches!(*current.effect, Effect::PutCounter { .. }) {
                actors.push(
                    current
                        .instruction_actor
                        .clone()
                        .unwrap_or_else(|| ControllerRef::You.into()),
                );
            }
            node = current.sub_ability.as_deref();
        }
        assert_eq!(actors.len(), 2);
        assert_ne!(actors[0], ControllerRef::You.into(), "{text}");
        assert_eq!(actors[1], ControllerRef::You.into());
        let cast = runner.cast(spell);
        let outcome = if owner_subject {
            cast.target_objects(&[borrowed, recipient, independent])
                .resolve()
        } else {
            cast.target_player(P1)
                .target_objects(&[recipient, independent])
                .resolve()
        };
        for (actor, object) in [(P1, recipient), (P0, independent)] {
            assert_eq!(outcome.events().iter().filter(|event| matches!(event, GameEvent::CounterAdded { actor: actual, object_id, counter_type: CounterType::Plus1Plus1, count: 1 } if *actual == actor && *object_id == object)).count(), 1, "{text}");
        }
        assert_eq!(outcome.events().iter().filter(|event| matches!(event, GameEvent::EffectResolved { kind: EffectKind::PutCounter, source_id, .. } if *source_id == spell)).count(), 2);
        assert_eq!(
            outcome.state().objects[&borrowed].zone,
            if owner_subject {
                Zone::Library
            } else {
                Zone::Battlefield
            }
        );
        if owner_subject {
            assert!(outcome.state().players[1].library.contains(&borrowed));
            assert!(!outcome.state().players[p2.0 as usize]
                .library
                .contains(&borrowed));
        }
        assert!(outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::PlayerPerformedAction {
                player_id: P1,
                action: engine::types::events::PlayerActionKind::ShuffledLibrary,
                ..
            }
        )));
        outcome.assert_zone(&[spell], Zone::Graveyard);
        assert!(outcome.state().stack.is_empty());
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
    }
}

fn hand_size(runner: &GameRunner, player: engine::types::player::PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn p1p1(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// Create a creature in `owner`'s hand with the given power and move it to the
/// battlefield, firing "creature you control enters" triggers.
fn enter_creature(runner: &mut GameRunner, owner: engine::types::player::PlayerId, power: i32) {
    let cid = {
        let state = runner.state_mut();
        let card_id = CardId(state.next_object_id);
        let id = create_object(state, card_id, owner, "Bystander".to_string(), Zone::Hand);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(power);
        obj.toughness = Some(power.max(1));
        obj.base_power = Some(power);
        obj.base_toughness = Some(power.max(1));
        id
    };
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), cid, Zone::Battlefield, &mut events);
    process_triggers(runner.state_mut(), &events);
    drain_order_triggers_with_identity(runner.state_mut());
    runner.advance_until_stack_empty();
}

fn setup_tribute() -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut builder = scenario.add_creature(P0, "Tribute to the World Tree", 0, 0);
    builder.as_enchantment();
    builder.from_oracle_text(TRIBUTE_ORACLE);
    // Library padding so a draw does not deck P0.
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Forest");
    }
    scenario.build()
}

/// NON-REGRESSION (Tribute): an entering creature with power ≥ 3 draws a card —
/// "its power" binds the ENTERING creature (CostPaidObject), so a 3-power
/// creature satisfies `Power{CostPaidObject} GE 3` and P0 draws.
#[test]
fn tribute_entering_power_3_draws_a_card() {
    let mut runner = setup_tribute();
    let before = hand_size(&runner, P0);

    enter_creature(&mut runner, P0, 3);

    assert_eq!(
        hand_size(&runner, P0),
        before + 1,
        "an entering 3-power creature must make Tribute draw a card (its = entering creature)"
    );
}

/// NON-REGRESSION (Tribute): an entering creature with power < 3 does NOT draw;
/// the else branch puts two +1/+1 counters on the entering creature instead.
#[test]
fn tribute_entering_power_2_puts_counters_not_draw() {
    let mut runner = setup_tribute();
    let before = hand_size(&runner, P0);

    enter_creature(&mut runner, P0, 2);

    assert_eq!(
        hand_size(&runner, P0),
        before,
        "an entering 2-power creature must NOT draw (its power 2 < 3)"
    );
    // The entering creature ("Bystander") received two +1/+1 counters.
    let bystander = runner
        .state()
        .battlefield
        .iter()
        .find(|id| runner.state().objects[id].name == "Bystander")
        .copied()
        .expect("the entering creature is on the battlefield");
    assert_eq!(
        p1p1(&runner, bystander),
        2,
        "the else branch must put two +1/+1 counters on the entering creature"
    );
}

/// REGRESSION (Ent's Fury): "its power" reads the own announced target via
/// `Power{Target}`, while genuine cost/entering references retain CostPaidObject.
/// CR 608.2c + CR 608.2h: the first instruction reads the ally's current power 4,
/// so it receives one counter before the independent pump instruction.
/// CR 122.1a + CR 611.2a + CR 613.4c: counter +1/+1 and EOT pump +1/+1 make it 6/6.
/// CR 701.14a + CR 701.14d: it then fights the 3/3 with current powers as noncombat
/// damage. CostPaidObject remains a cost/trigger/effect referent, with no target
/// fallback.
#[test]
fn ents_fury_own_target_gets_counter_then_pump() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // P0's own creature (the counter/pump/fight subject) at power 4.
    let ally = scenario.add_creature(P0, "Strong Ally", 4, 4).id();
    // An opponent creature to satisfy "fights target creature you don't control".
    let enemy = scenario.add_creature(P1, "Enemy", 3, 3).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Ent's Fury", false, ENTS_FURY_ORACLE)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    engine::game::layers::evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&ally].power,
        Some(4),
        "precondition: the ally target has power 4"
    );

    // Two target slots: the subject (a creature you control) and the fight
    // opponent (a creature you don't control).
    let outcome = runner.cast(spell).target_objects(&[ally, enemy]).resolve();

    // CR 608.2c + CR 608.2h: the own target's initial power 4 satisfies the gate.
    assert_eq!(
        p1p1(&runner, ally),
        1,
        "Ent's Fury must put one +1/+1 counter on its own 4-power target"
    );
    // CR 122.1a + CR 611.2a + CR 613.4c: the counter and separate pump each add 1.
    assert_eq!(
        outcome.state().objects[&ally].power,
        Some(6),
        "Ent's Fury's counter and independent +1/+1 pump must both apply (4 -> 6)"
    );
    // CR 701.14a + CR 704.5g: the 6/6 target survives the 3/3's fight damage.
    assert_eq!(
        outcome.state().objects[&ally].zone,
        Zone::Battlefield,
        "the countered and pumped 6/6 target survives fighting a 3/3"
    );
}

/// CR 608.2c + CR 608.2h: Ent's independent pump and fight still run when its
/// own target starts below the counter threshold.
#[test]
fn ents_fury_below_threshold_still_pumps_and_fights() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ally = scenario.add_creature(P0, "Small Ally", 3, 5).id();
    let enemy = scenario.add_creature(P1, "Fight Enemy", 2, 4).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Ent's Fury", false, ENTS_FURY_ORACLE)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_objects(&[ally, enemy]).resolve();
    assert_eq!(p1p1(&runner, ally), 0);
    assert_eq!(outcome.state().objects[&ally].power, Some(4));
    outcome.assert_zone(&[ally], Zone::Battlefield);
    // CR 701.14a + CR 704.5g: the pumped ally's four damage kills the 2/4.
    outcome.assert_zone(&[enemy, spell], Zone::Graveyard);
    assert!(outcome.events().iter().any(|event| matches!(event,
        GameEvent::EffectResolved { kind: EffectKind::Fight, source_id, .. } if *source_id == spell)));
}

#[test]
fn synthetic_decline_actor_survives_a_real_saved_optional_prompt() {
    const ORACLE: &str = "Each opponent may draw a card. For each opponent who doesn't, put a +1/+1 counter on target creature you control if its power is 4 or greater.";
    for (power, accept, counters) in [(4, true, 0), (4, false, 1), (3, true, 0), (3, false, 0)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let ally = scenario.add_creature(P0, "Saved Target", power, 6).id();
        let decoy = scenario.add_creature(P0, "Saved Decoy", 8, 8).id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Synthetic Saved Decline", false, ORACLE)
            .with_mana_cost(engine::types::mana::ManaCost::zero())
            .id();
        for _ in 0..4 {
            scenario.add_card_to_library_top(P1, "Forest");
        }
        let mut runner = scenario.build();
        let mut committed = runner.cast(spell).target_object(ally).commit();
        for _ in 0..4 {
            if !matches!(committed.state().waiting_for, WaitingFor::Priority { .. }) {
                break;
            }
            committed.act(GameAction::PassPriority).unwrap();
        }
        assert!(matches!(committed.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: P1, source_id, .. } if source_id == spell));
        let frame = committed
            .state()
            .active_optional_effect_frame()
            .expect("real optional frame");
        assert_eq!(frame.ability.original_controller, Some(P0));
        assert_eq!(frame.ability.scoped_player, Some(P1));
        let saved = serde_json::to_vec(committed.state()).unwrap();
        drop(committed);
        let mut runner = GameRunner::from_state(serde_json::from_slice(&saved).unwrap());
        let hand_before = hand_size(&runner, P1);
        let result = runner
            .act(GameAction::DecideOptionalEffect { accept })
            .expect("restore resumes the real owned decision");
        assert_eq!(hand_size(&runner, P1), hand_before + usize::from(accept));
        assert_eq!(p1p1(&runner, ally), counters);
        assert_eq!(p1p1(&runner, decoy), 0);
        assert_eq!(result.events.iter().filter(|event| matches!(event,
            GameEvent::CounterAdded { object_id, counter_type: CounterType::Plus1Plus1, count: 1, actor: P0 }
                if *object_id == ally)).count(), counters as usize);
        assert_eq!(result.events.iter().filter(|event| matches!(event,
            GameEvent::EffectResolved { kind: EffectKind::PutCounter, source_id, .. } if *source_id == spell)).count(), counters as usize);
        if accept {
            assert!(result.events.iter().any(|event| matches!(event,
            GameEvent::EffectResolved { kind: EffectKind::Draw, source_id, .. } if *source_id == spell)));
        }
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        assert!(runner.state().stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

/// Synthetic admitted list body: both intrinsic counter placements keep the
/// controller imperative actor through the real declined optional instruction.
#[test]
fn synthetic_decline_compound_counter_children_keep_their_actor() {
    const ORACLE: &str = "Each opponent may draw a card. For each opponent who doesn't, put a +1/+1 counter and a lifelink counter on target creature you control.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ally = scenario.add_creature(P0, "Compound Target", 4, 6).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic Compound Actor", false, ORACLE)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    scenario.add_card_to_library_top(P1, "Forest");
    let mut runner = scenario.build();
    assert!(
        std::iter::successors(runner.state().objects[&spell].abilities.first(), |d| d
            .sub_ability
            .as_deref())
        .all(|d| d.effect.unimplemented_description().is_none()),
        "the full synthetic list must be admitted"
    );
    let outcome = runner
        .cast(spell)
        .target_object(ally)
        .decline_optional()
        .resolve();
    assert_eq!(p1p1(&runner, ally), 1);
    assert_eq!(
        outcome
            .events()
            .iter()
            .filter(|event| matches!(event,
        GameEvent::CounterAdded { object_id, count: 1, actor: P0, .. } if *object_id == ally))
            .count(),
        2
    );
    assert_eq!(outcome.events().iter().filter(|event| matches!(event,
        GameEvent::EffectResolved { kind: EffectKind::PutCounter, source_id, .. } if *source_id == spell)).count(), 2);
    outcome.assert_hand_drawn(P1, 0);
    outcome.assert_zone(&[spell], Zone::Graveyard);
    assert!(outcome.state().stack.is_empty());
}

#[test]
fn synthetic_decline_owned_body_preserves_a_later_explicit_player_actor() {
    const ORACLE: &str = "Each opponent may draw a card. For each opponent who doesn't, put a +1/+1 counter and a lifelink counter on target creature you control, then that player puts a +1/+1 counter on a creature they control.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ally = scenario
        .add_creature(P0, "Printed Controller Target", 3, 6)
        .id();
    let other = scenario.add_creature(P1, "Scoped Actor Target", 3, 6).id();
    let decoy = scenario.add_creature(P1, "Scoped Actor Decoy", 3, 6).id();
    scenario.add_card_to_library_top(P1, "Forest");
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic Distinct Subjects", false, ORACLE)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    assert!(
        std::iter::successors(runner.state().objects[&spell].abilities.first(), |d| d
            .sub_ability
            .as_deref())
        .all(|d| d.effect.unimplemented_description().is_none()),
        "both printed subjects must parse"
    );
    let outcome = runner
        .cast(spell)
        .target_object(ally)
        .decline_optional()
        .resolve();
    let mut events = outcome.events().to_vec();
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::ChooseFromZoneChoice { player: P1, cards, .. }
        if cards.contains(&other) && cards.contains(&decoy)),
        "explicit player's counter choice: {:?}",
        runner.state().waiting_for
    );
    events.extend(
        runner
            .act(GameAction::SelectCards { cards: vec![other] })
            .unwrap()
            .events,
    );
    assert_eq!(p1p1(&runner, ally), 1);
    assert_eq!(p1p1(&runner, other), 1);
    assert_eq!(p1p1(&runner, decoy), 0);
    assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::CounterAdded { object_id, count: 1, actor: P0, .. } if *object_id == ally)).count(), 2);
    assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::CounterAdded { object_id, counter_type: CounterType::Plus1Plus1, count: 1, actor: P1 } if *object_id == other)).count(), 1);
    assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::EffectResolved { kind: EffectKind::PutCounter, source_id, .. } if *source_id == spell)).count(), 3);
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
}

#[test]
fn synthetic_decline_remaining_seat_and_independent_tail_keep_distinct_authorities() {
    const ORACLE: &str = "Each opponent may draw a card. For each opponent who doesn't, put a +1/+1 counter on target creature you control if its power is 4 or greater. You gain 1 life.";
    let p2 = engine::types::player::PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let ally = scenario
        .add_creature(P0, "Remaining Seat Target", 4, 6)
        .id();
    for player in [P1, p2] {
        for _ in 0..3 {
            scenario.add_card_to_library_top(player, "Forest");
        }
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic Remaining Actor", false, ORACLE)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let mut committed = runner.cast(spell).target_object(ally).commit();
    for _ in 0..6 {
        if !matches!(committed.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        committed.act(GameAction::PassPriority).unwrap();
    }
    assert!(
        matches!(committed.state().waiting_for, WaitingFor::OptionalEffectChoice { player: P1, source_id, .. } if source_id == spell)
    );
    let first = committed
        .act(GameAction::DecideOptionalEffect { accept: true })
        .unwrap();
    assert!(
        matches!(committed.state().waiting_for, WaitingFor::OptionalEffectChoice { player, source_id, .. } if player == p2 && source_id == spell)
    );
    let frame = committed.state().active_optional_effect_frame().unwrap();
    assert_eq!(frame.ability.original_controller, Some(P0));
    assert_eq!(frame.ability.scoped_player, Some(p2));
    let saved = serde_json::to_vec(committed.state()).unwrap();
    drop(committed);
    let mut runner = GameRunner::from_state(serde_json::from_slice(&saved).unwrap());
    let second = runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .unwrap();
    let events: Vec<_> = first.events.into_iter().chain(second.events).collect();
    assert_eq!(hand_size(&runner, P1), 1);
    assert_eq!(hand_size(&runner, p2), 0);
    assert_eq!(p1p1(&runner, ally), 1);
    assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::CounterAdded { object_id, counter_type: CounterType::Plus1Plus1, count: 1, actor: P0 } if *object_id == ally)).count(), 1);
    assert_eq!(runner.life(P0), 21);
    assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::EffectResolved { kind: EffectKind::GainLife, source_id, .. } if *source_id == spell)).count(), 1);
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
}

#[test]
fn synthetic_decline_printed_controller_survives_source_exit_or_control_change() {
    const ABILITY: &str = "{T}: Each opponent may draw a card. For each opponent who doesn't, put a +1/+1 counter on target creature you control if its power is 4 or greater.";
    for change_control in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let ally = scenario.add_creature(P0, "Captured Target", 4, 6).id();
        let source = scenario
            .add_creature_from_oracle(P0, "Synthetic Captured Actor", 0, 3, ABILITY)
            .id();
        scenario.add_card_to_library_top(P1, "Forest");
        let response = if change_control {
            let def = engine::types::ability::AbilityDefinition::new(
                engine::types::ability::AbilityKind::Spell,
                engine::types::ability::Effect::GainControl {
                    target: engine::types::ability::TargetFilter::Typed(
                        engine::types::ability::TypedFilter::creature(),
                    ),
                },
            );
            scenario
                .add_spell_to_hand(P1, "Synthetic Transfer Response", true)
                .with_ability_definition(def)
                .with_mana_cost(engine::types::mana::ManaCost::zero())
                .id()
        } else {
            scenario
                .add_spell_to_hand_from_oracle(
                    P0,
                    "Unsummon",
                    true,
                    "Return target creature to its owner's hand.",
                )
                .with_mana_cost(engine::types::mana::ManaCost::zero())
                .id()
        };
        let mut runner = scenario.build();
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            })
            .unwrap();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ));
        runner
            .act(GameAction::ChooseTarget {
                target: Some(engine::types::ability::TargetRef::Object(ally)),
            })
            .unwrap();
        assert!(
            !runner.state().stack.is_empty(),
            "the P0 ability was queued before the response"
        );
        if change_control {
            runner.act(GameAction::PassPriority).unwrap();
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { player: P1 }
            ));
        }
        let mut committed = runner.cast(response).target_object(source).commit();
        for _ in 0..20 {
            if !matches!(committed.state().waiting_for, WaitingFor::Priority { .. }) {
                break;
            }
            committed.act(GameAction::PassPriority).unwrap();
        }
        drop(committed);
        if change_control {
            assert_eq!(runner.state().objects[&source].controller, P1);
        } else {
            assert_eq!(runner.state().objects[&source].zone, Zone::Hand);
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::OptionalEffectChoice { player: P1, source_id, .. } if source_id == source)
        );
        let result = runner
            .act(GameAction::DecideOptionalEffect { accept: false })
            .unwrap();
        assert_eq!(p1p1(&runner, ally), 1);
        assert!(result.events.iter().any(|event| matches!(event, GameEvent::CounterAdded { object_id, counter_type: CounterType::Plus1Plus1, count: 1, actor: P0 } if *object_id == ally)));
        assert!(result.events.iter().any(|event| matches!(event, GameEvent::EffectResolved { kind: EffectKind::PutCounter, source_id, .. } if *source_id == source)));
        assert!(runner.state().stack.is_empty());
    }
}

/// Synthetic admitted possessive subjects: the wrapper's object, its owner,
/// its controller and the printed spell controller are separate authorities.
/// CR 608.2c: both intrinsic placements belong to the explicit player subject.
#[test]
fn synthetic_explicit_subject_wrapper_defaults_its_owned_counter_body() {
    use engine::types::player::PlayerId;
    let p2 = PlayerId(2);
    for (subject, actor) in [("controller", P1), ("owner", p2)] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let recipient = scenario
            .add_creature(p2, "Borrowed Counter Recipient", 3, 6)
            .controlled_by(P1)
            .id();
        let oracle = format!(
            "Target creature's {subject} puts a +1/+1 counter and a lifelink counter on it."
        );
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Synthetic Possessive Counter Actor", false, &oracle)
            .with_mana_cost(engine::types::mana::ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        let outcome = runner.cast(spell).target_object(recipient).resolve();
        assert_eq!(outcome.state().objects[&recipient].owner, p2);
        assert_eq!(outcome.state().objects[&recipient].controller, P1);
        for counter_type in [
            CounterType::Plus1Plus1,
            CounterType::Keyword(engine::types::keywords::KeywordKind::Lifelink),
        ] {
            assert_eq!(
                outcome.state().objects[&recipient]
                    .counters
                    .get(&counter_type),
                Some(&1)
            );
            assert_eq!(outcome.events().iter().filter(|event| matches!(event,
                GameEvent::CounterAdded { object_id, counter_type: actual_type, count: 1, actor: actual_actor }
                if *object_id == recipient && *actual_type == counter_type && *actual_actor == actor)).count(), 1);
        }
        assert_eq!(outcome.events().iter().filter(|event| matches!(event,
            GameEvent::EffectResolved { kind: EffectKind::PutCounter, source_id, .. } if *source_id == spell)).count(), 2);
        outcome.assert_zone(&[spell], Zone::Graveyard);
        assert!(outcome.state().stack.is_empty());
    }
}

/// Synthetic possessive subject with a real resolution-time chooser.
/// CR 608.2d: the named actor chooses the legal creature, independently of P0.
#[test]
fn synthetic_explicit_controller_subject_owns_its_counter_choice() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let owner = engine::types::player::PlayerId(2);
    let anchor = scenario
        .add_creature(owner, "Possessive Target", 3, 6)
        .controlled_by(P1)
        .id();
    let recipient = scenario.add_creature(P1, "Chosen Recipient", 3, 6).id();
    let decoy = scenario
        .add_creature(P0, "Printed Controller Decoy", 3, 6)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic Possessive Chooser",
            false,
            "Target creature's controller puts a +1/+1 counter on a creature they control.",
        )
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let mut events = runner
        .cast(spell)
        .target_object(anchor)
        .resolve()
        .events()
        .to_vec();
    assert!(
        matches!(&runner.state().waiting_for,
        WaitingFor::ChooseFromZoneChoice { player: P1, cards, .. }
        if cards.contains(&anchor) && cards.contains(&recipient) && !cards.contains(&decoy)),
        "possessive controller's counter choice: {:?}",
        runner.state().waiting_for
    );
    events.extend(
        runner
            .act(GameAction::SelectCards {
                cards: vec![recipient],
            })
            .unwrap()
            .events,
    );
    assert_eq!(p1p1(&runner, recipient), 1);
    assert_eq!(p1p1(&runner, anchor), 0);
    assert_eq!(p1p1(&runner, decoy), 0);
    assert_eq!(events.iter().filter(|event| matches!(event,
        GameEvent::CounterAdded { object_id, counter_type: CounterType::Plus1Plus1, count: 1, actor: P1 }
        if *object_id == recipient)).count(), 1);
    assert_eq!(events.iter().filter(|event| matches!(event,
        GameEvent::EffectResolved { kind: EffectKind::PutCounter, source_id, .. } if *source_id == spell)).count(), 1);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert!(runner.state().stack.is_empty());
}

/// Synthetic decline grammar retains the independent possessive subject;
/// contextual "that player" is covered by the existing scoped-player positive.
/// CR 608.2c: the decline's seat is not the controller/owner of its object.
#[test]
fn synthetic_decline_preserves_explicit_controller_and_owner_actors() {
    use engine::types::player::PlayerId;
    let p2 = PlayerId(2);
    let p3 = PlayerId(3);
    for (subject, actor) in [("controller", p2), ("owner", p3)] {
        let mut scenario = GameScenario::new_n_player(4, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let recipient = scenario
            .add_creature(p3, "Independent Possessive Recipient", 3, 6)
            .controlled_by(p2)
            .id();
        for player in [P1, p2, p3] {
            scenario.add_card_to_library_top(player, "Forest");
        }
        let oracle = format!("Each opponent may draw a card. For each opponent who doesn't, target creature's {subject} puts a +1/+1 counter on it.");
        let spell = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Synthetic Independent Decline Subject",
                false,
                &oracle,
            )
            .with_mana_cost(engine::types::mana::ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        let outcome = runner
            .cast(spell)
            .target_object(recipient)
            .decline_optional()
            .resolve();
        assert_eq!(
            outcome.state().objects[&recipient]
                .counters
                .get(&CounterType::Plus1Plus1),
            Some(&3)
        );
        assert_eq!(outcome.events().iter().filter(|event| matches!(event,
            GameEvent::CounterAdded { object_id, counter_type: CounterType::Plus1Plus1, count: 1, actor: actual }
            if *object_id == recipient && *actual == actor)).count(), 3);
        assert_eq!(outcome.events().iter().filter(|event| matches!(event,
            GameEvent::EffectResolved { kind: EffectKind::PutCounter, source_id, .. } if *source_id == spell)).count(), 3);
        outcome.assert_zone(&[spell], Zone::Graveyard);
        assert!(outcome.state().stack.is_empty());
    }
}

/// Synthetic attack trigger: the event player P3 differs from scope P1,
/// printed controller P0 and the counter recipient's controller P2.
/// CR 608.2c: an explicit event subject is independent of the decline anaphor.
#[test]
fn synthetic_decline_preserves_an_explicit_attacking_player_actor() {
    use engine::types::ability::TargetRef;
    use engine::types::player::PlayerId;
    let p2 = PlayerId(2);
    let p3 = PlayerId(3);
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature_from_oracle(P0, "Synthetic Event Subject", 1, 6,
        "Whenever a creature attacks, each opponent may draw a card. For each opponent who doesn't, that attacking player puts a +1/+1 counter on target creature.").id();
    let recipient = scenario
        .add_creature(p2, "Independent Event Recipient", 3, 6)
        .id();
    let attacker = scenario.add_creature(p3, "Event Actor Attacker", 1, 6).id();
    for player in [P1, p2, p3] {
        scenario.add_card_to_library_top(player, "Forest");
    }
    let mut runner = scenario.build();
    runner.state_mut().active_player = p3;
    runner
        .state_mut()
        .objects
        .get_mut(&attacker)
        .unwrap()
        .summoning_sick = false;
    runner.advance_to_combat();
    let mut events = runner
        .declare_attackers(&[(attacker, engine::game::combat::AttackTarget::Player(P1))])
        .unwrap()
        .events;
    let mut offers = Vec::new();
    for _ in 0..96 {
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(recipient)),
                }
            }
            WaitingFor::OptionalEffectChoice {
                player, source_id, ..
            } => {
                assert_eq!(source_id, source);
                offers.push(player);
                GameAction::DecideOptionalEffect { accept: false }
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected synthetic event-subject prompt: {other:?}"),
        };
        events.extend(runner.act(action).unwrap().events);
    }
    assert_eq!(offers.len(), 3);
    assert!(offers.contains(&P1) && offers.contains(&p2) && offers.contains(&p3));
    assert_eq!(p1p1(&runner, recipient), 3);
    assert_eq!(events.iter().filter(|event| matches!(event,
        GameEvent::CounterAdded { object_id, counter_type: CounterType::Plus1Plus1, count: 1, actor: actual }
        if *object_id == recipient && *actual == p3)).count(), 3);
    assert_eq!(events.iter().filter(|event| matches!(event,
        GameEvent::EffectResolved { kind: EffectKind::PutCounter, source_id, .. } if *source_id == source)).count(), 3);
    assert!(runner.state().stack.is_empty());
}

/// Synthetic admitted player-target zone wrapper, distinct from the possessive
/// object wrapper above. CR 608.2c/d: P1 both chooses and performs its exile.
#[test]
fn synthetic_target_player_wrapper_owns_its_zone_choice_and_move() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, ReplacementMode,
    };
    use engine::types::player::PlayerId;
    use engine::types::proposed_event::ProposedEvent;
    use engine::types::replacements::ReplacementEvent;
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let recipient = scenario
        .add_creature(p2, "Borrowed Exile Recipient", 3, 6)
        .controlled_by(P1)
        .id();
    let decoy = scenario.add_creature(P1, "Exile Choice Decoy", 3, 6).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic Player Zone Actor",
            false,
            "Target player exiles a creature they control.",
        )
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    scenario
        .add_enchantment_from_oracle(P0, "Synthetic Optional Exile Observer", "")
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .mode(ReplacementMode::Optional { decline: None })
                .destination_zone(Zone::Exile)
                .execute(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp)),
        );
    let mut runner = scenario.build();
    let mut events = runner
        .cast(spell)
        .target_player(P1)
        .resolve()
        .events()
        .to_vec();
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::EffectZoneChoice { player: P1, cards, .. }
        if cards.contains(&recipient) && cards.contains(&decoy))
    );
    events.extend(
        runner
            .act(GameAction::SelectCards {
                cards: vec![recipient],
            })
            .unwrap()
            .events,
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(
        matches!(&runner.state().pending_replacement.as_ref().unwrap().proposed,
        ProposedEvent::ZoneChange { object_id, performed_by: Some(P1), from: Zone::Battlefield, to: Zone::Exile, .. }
        if *object_id == recipient)
    );
    events.extend(
        runner
            .act(GameAction::ChooseReplacement { index: 1 })
            .unwrap()
            .events,
    );
    assert_eq!(runner.state().objects[&recipient].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&recipient].owner, p2);
    assert_eq!(runner.state().objects[&decoy].zone, Zone::Battlefield);
    assert_eq!(events.iter().filter(|event| matches!(event,
        GameEvent::EffectResolved { kind: EffectKind::ChangeZone, source_id, .. } if *source_id == spell)).count(), 1);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert!(runner.state().stack.is_empty());
}

#[test]
fn synthetic_optional_decline_preserves_target_power_gate_when_cast() {
    const ORACLE: &str = "Each opponent may draw a card. For each opponent who doesn't, put a +1/+1 counter on target creature you control if its power is 4 or greater.";
    for (power, accept, expected_counters) in
        [(4, true, 0), (4, false, 1), (3, true, 0), (3, false, 0)]
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let ally = scenario.add_creature(P0, "Chosen Ally", power, 6).id();
        let decoy = scenario.add_creature(P0, "Unchosen Ally", 6, 6).id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Synthetic Decline Grammar", false, ORACLE)
            .with_mana_cost(engine::types::mana::ManaCost::zero())
            .id();
        for _ in 0..4 {
            scenario.add_card_to_library_top(P1, "Forest");
        }
        let mut runner = scenario.build();
        let source = &runner.state().objects[&spell];
        assert!(
            source.power.is_none(),
            "the spell cannot satisfy the power gate"
        );
        assert!(
            std::iter::successors(source.abilities.first(), |def| def.sub_ability.as_deref())
                .all(|def| def.effect.unimplemented_description().is_none()),
            "the complete synthetic chain must parse"
        );

        let mut committed = runner.cast(spell).target_object(ally).commit();
        // Keep the harness's cast baseline and observe the actual decision
        // boundary before its ordinary accept/decline driver answers it.
        for _ in 0..4 {
            if !matches!(committed.state().waiting_for, WaitingFor::Priority { .. }) {
                break;
            }
            committed
                .act(GameAction::PassPriority)
                .expect("pass to optional draw");
        }
        assert!(
            matches!(committed.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: P1, source_id, .. } if source_id == spell),
            "P1 must actually receive this spell's optional draw decision: {:?}",
            committed.state().waiting_for
        );
        let outcome = if accept {
            committed.accept_optional().resolve()
        } else {
            committed.decline_optional().resolve()
        };
        outcome.assert_hand_drawn(P1, i64::from(accept));
        outcome.assert_zone(&[spell], Zone::Graveyard);
        assert!(outcome.state().stack.is_empty());
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
        assert_eq!(
            outcome.state().objects[&ally]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            expected_counters,
            "power {power}, accept {accept}: both independent gates must hold"
        );
        assert_eq!(
            outcome.state().objects[&decoy]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            0,
            "only the announced target can receive the counter"
        );
        assert_eq!(outcome.events().iter().filter(|event| matches!(event,
            GameEvent::CounterAdded { object_id, counter_type: CounterType::Plus1Plus1, count: 1, actor: P0 } if *object_id == ally)).count(),
            expected_counters as usize, "the counter event must name the target and spell controller");
        assert_eq!(outcome.events().iter().filter(|event| matches!(event,
            GameEvent::EffectResolved { kind: EffectKind::PutCounter, source_id, .. } if *source_id == spell)).count(),
            expected_counters as usize, "the counter instruction must resolve from this spell");
        if accept {
            assert!(outcome.events().iter().any(|event| matches!(event,
                GameEvent::EffectResolved { kind: EffectKind::Draw, source_id, .. } if *source_id == spell)),
                "accepted draw must resolve from the same spell");
        }
    }
}
