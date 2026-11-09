//! CR 608.2k + CR 701.26a: "Whenever this creature deals combat
//! damage to a creature, tap that creature and it doesn't untap during its
//! controller's next untap step." (Kashi-Tribe Warriors, Kashi-Tribe Elite).
//!
//! "That creature" is the damage recipient, an untargeted event referent
//! (`TargetFilter::EventTarget`), so the trigger has no chosen target. The tap
//! resolver read only `ability.targets`, so the trigger resolved and tapped
//! nothing. It now reads the event-subject anaphors from the trigger event, the
//! way Destroy and Prepare already do.

use super::rules::{GameScenario, Phase, P0, P1};
use engine::analysis::resource::loop_states_equal_modulo_resources;
use engine::game::combat::AttackTarget;
use engine::game::scenario::GameRunner;
use engine::game::zones::move_to_zone;
use engine::types::ability::{ContinuousModification, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const KASHI_TRIBE_WARRIORS: &str = "Whenever this creature deals combat damage to a creature, tap that creature and it doesn't untap during its controller's next untap step.";

fn pass_to_declare_blockers(runner: &mut GameRunner) {
    for _ in 0..8 {
        if runner.waiting_for_kind() == "DeclareBlockers" {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority into the declare-blockers step");
    }
}

/// Advance turn by turn to `player`'s next upkeep, declaring no attackers when
/// a declare-attackers step asks for them.
fn advance_to_upkeep_of(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..8 {
        runner.advance_to_upkeep();
        if runner.state().phase == Phase::Upkeep && runner.state().active_player == player {
            return;
        }
        if runner.waiting_for_kind() == "DeclareAttackers" {
            runner.declare_attackers(&[]).expect("declare no attackers");
        } else {
            runner.advance_to_end_step();
        }
    }
    panic!("did not reach {player:?}'s upkeep");
}

fn is_tapped(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].tapped
}

/// CR 701.26a + CR 502.3: Kashi-Tribe Warriors attacks and is blocked. The
/// blocker it damaged becomes tapped, stays tapped through its controller's
/// next untap step, and untaps normally in the untap step after that.
#[test]
fn damaged_blocker_is_tapped_and_skips_its_controllers_next_untap() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    scenario.with_library_top(P1, &["Island", "Island", "Island"]);
    let warriors = {
        let mut b = scenario.add_creature(P0, "Kashi-Tribe Warriors", 2, 4);
        b.from_oracle_text(KASHI_TRIBE_WARRIORS);
        b.id()
    };
    let wall = scenario.add_creature(P1, "Wall of Wood", 0, 5).id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(warriors, AttackTarget::Player(P1))])
        .expect("declare attackers");
    pass_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(wall, warriors)])
        .expect("declare blockers");
    runner.combat_damage();

    assert!(
        is_tapped(&runner, wall),
        "CR 701.26a: the creature dealt combat damage must be tapped"
    );

    // CR 502.3: P1's next untap step — the wall must stay tapped.
    advance_to_upkeep_of(&mut runner, P1);
    assert!(
        is_tapped(&runner, wall),
        "the damaged creature must not untap during its controller's next untap step"
    );

    // The restriction covers only that one untap step.
    runner.advance_to_end_step();
    runner.advance_to_upkeep();
    assert_eq!(runner.state().active_player, P0);
    runner.advance_to_end_step();
    advance_to_upkeep_of(&mut runner, P1);
    assert!(
        !is_tapped(&runner, wall),
        "the creature untaps normally in its controller's following untap step"
    );
}

/// CR 701.26a + CR 702.20b: blocking side. A vigilant attacker isn't tapped by
/// attacking, so being tapped after combat can only come from the trigger.
#[test]
fn damaged_attacker_is_tapped_when_warriors_blocks() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest"]);
    scenario.with_library_top(P1, &["Island", "Island"]);
    let warriors = {
        let mut b = scenario.add_creature(P0, "Kashi-Tribe Warriors", 2, 4);
        b.from_oracle_text(KASHI_TRIBE_WARRIORS);
        b.id()
    };
    let attacker = {
        let mut b = scenario.add_creature(P1, "Vigilant Giant", 2, 5);
        b.vigilance();
        b.id()
    };

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P0))])
        .expect("declare attackers");
    assert!(
        !is_tapped(&runner, attacker),
        "CR 702.20b: vigilance keeps the attacker untapped"
    );
    pass_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(warriors, attacker)])
        .expect("declare blockers");
    runner.combat_damage();

    assert!(
        is_tapped(&runner, attacker),
        "CR 701.26a: the attacker dealt combat damage must be tapped"
    );
    assert!(
        !is_tapped(&runner, warriors),
        "the trigger source is not its own referent"
    );

    // CR 502.3: the freeze is keyed to the controller of the affected creature
    // (P1), not the controller of the ability (P0). It is P1's turn, so P1's
    // untap step has already passed; P0's untap step does not untap P1's
    // permanents.
    runner.advance_to_end_step();
    advance_to_upkeep_of(&mut runner, P0);
    assert!(
        is_tapped(&runner, attacker),
        "P0's untap step does not untap P1's permanent"
    );

    // P1's next untap step: the attacker must stay tapped.
    advance_to_upkeep_of(&mut runner, P1);
    assert!(
        is_tapped(&runner, attacker),
        "the damaged attacker must not untap during its controller's next untap step"
    );

    // P1's following untap step (after P0's turn): it untaps normally.
    runner.advance_to_end_step();
    advance_to_upkeep_of(&mut runner, P0);
    runner.advance_to_end_step();
    advance_to_upkeep_of(&mut runner, P1);
    assert!(
        !is_tapped(&runner, attacker),
        "the attacker untaps normally in its controller's following untap step"
    );
}

/// CR 701.26a: only a permanent can be tapped. In one combat two Warriors each
/// deal combat damage to a different blocker: the survivor is tapped, the one
/// that died from the damage is no longer on the battlefield and is not
/// tapped, and neither trigger falls back to its source.
#[test]
fn recipient_that_died_is_not_tapped_while_survivor_in_same_combat_is() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let warriors_a = {
        let mut b = scenario.add_creature(P0, "Kashi-Tribe Warriors", 2, 4);
        b.from_oracle_text(KASHI_TRIBE_WARRIORS);
        b.vigilance();
        b.id()
    };
    let warriors_b = {
        let mut b = scenario.add_creature(P0, "Kashi-Tribe Warriors", 2, 4);
        b.from_oracle_text(KASHI_TRIBE_WARRIORS);
        b.vigilance();
        b.id()
    };
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let wall = scenario.add_creature(P1, "Wall of Wood", 0, 5).id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (warriors_a, AttackTarget::Player(P1)),
            (warriors_b, AttackTarget::Player(P1)),
        ])
        .expect("declare attackers");
    pass_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(bear, warriors_a), (wall, warriors_b)])
        .expect("declare blockers");
    let outcome = runner.combat_damage();

    // Positive reach-guards: both damage triggers resolved in this combat and
    // the surviving recipient was tapped exactly once.
    assert!(
        is_tapped(&runner, wall),
        "CR 701.26a: the surviving creature dealt combat damage must be tapped"
    );
    let tapped_wall = outcome
        .events()
        .iter()
        .filter(|e| matches!(e, GameEvent::PermanentTapped { object_id, .. } if *object_id == wall))
        .count();
    assert_eq!(tapped_wall, 1, "events: {:?}", outcome.events());
    let resolved = outcome
        .events()
        .iter()
        .filter(|e| matches!(e, GameEvent::StackResolved { .. }))
        .count();
    assert!(
        resolved >= 2,
        "both damage triggers must resolve; events: {:?}",
        outcome.events()
    );

    // Negatives.
    assert_eq!(outcome.zone_of(bear), Zone::Graveyard);
    assert!(
        !runner.state().objects[&bear].tapped,
        "a creature card in the graveyard is not a permanent and is not tapped"
    );
    assert!(
        !outcome.events().iter().any(
            |e| matches!(e, GameEvent::PermanentTapped { object_id, .. } if *object_id == bear)
        ),
        "no tap event for the dead recipient"
    );
    assert!(
        !is_tapped(&runner, warriors_a) && !is_tapped(&runner, warriors_b),
        "the trigger must not fall back to tapping its source"
    );
}

/// CR 400.7 + CR 506.4: the damage recipient leaves the battlefield and
/// returns while the trigger is on the stack. The returned permanent is a new
/// object: the trigger neither taps it nor stops it from untapping.
#[test]
fn blinked_recipient_is_neither_tapped_nor_frozen() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    scenario.with_library_top(P1, &["Island", "Island", "Island"]);
    let warriors = {
        let mut b = scenario.add_creature(P0, "Kashi-Tribe Warriors", 2, 4);
        b.from_oracle_text(KASHI_TRIBE_WARRIORS);
        b.id()
    };
    let wall = scenario.add_creature(P1, "Wall of Wood", 0, 5).id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(warriors, AttackTarget::Player(P1))])
        .expect("declare attackers");
    pass_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(wall, warriors)])
        .expect("declare blockers");
    for _ in 0..8 {
        if !runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass into combat damage");
    }

    // CR 510.2 + CR 510.3a: combat damage was dealt and the trigger was put on
    // the stack.
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().phase, Phase::CombatDamage);
    assert!(
        runner
            .state()
            .damage_dealt_this_turn
            .iter()
            .any(|record| record.target == TargetRef::Object(wall) && record.is_combat),
        "the wall must have been dealt combat damage"
    );

    // Blink the wall: CR 400.7 + CR 506.4, a new object removed from combat.
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), wall, Zone::Exile, &mut events);
    move_to_zone(runner.state_mut(), wall, Zone::Battlefield, &mut events);
    assert_eq!(runner.state().objects[&wall].zone, Zone::Battlefield);

    runner.resolve_top();
    assert!(
        runner.state().stack.is_empty(),
        "the trigger must have resolved"
    );
    assert!(
        !is_tapped(&runner, wall),
        "CR 400.7: the returned permanent is a new object and is not tapped"
    );
    assert!(
        !runner
            .state()
            .transient_continuous_effects
            .iter()
            .any(|tce| {
                tce.affected == TargetFilter::SpecificObject { id: wall }
                    && tce
                        .modifications
                        .contains(&ContinuousModification::AddStaticMode {
                            mode: StaticMode::CantUntap,
                        })
            }),
        "CR 400.7: the returned permanent gets no untap restriction; TCEs: {:?}",
        runner.state().transient_continuous_effects
    );

    // Stronger check: tapped by other means, the new object untaps in its
    // controller's next untap step (the paired positive is
    // `damaged_blocker_is_tapped_and_skips_its_controllers_next_untap`).
    runner.state_mut().objects.get_mut(&wall).unwrap().tapped = true;
    advance_to_upkeep_of(&mut runner, P1);
    assert!(
        !is_tapped(&runner, wall),
        "the returned permanent untaps during its controller's next untap step"
    );
}

const MATSU_TRIBE_SNIPER: &str = "{T}: This creature deals 1 damage to target creature with flying.\nWhenever this creature deals damage to a creature, tap that creature and it doesn't untap during its controller's next untap step.";
const FLICKER_OF_FATE: &str =
    "Exile target creature or enchantment, then return it to the battlefield under its owner's control.";

/// Produce the damage and pending untargeted trigger through the activation
/// pipeline, not an injected event or a hand-built stack entry.
fn matsu_pending_damage() -> (GameRunner, ObjectId, ObjectId, [ObjectId; 2], usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest", "Forest"]);
    scenario.with_library_top(P1, &["Island", "Island", "Island", "Island"]);
    let sniper = scenario
        .add_creature_from_oracle(P0, "Matsu-Tribe Sniper", 1, 1, MATSU_TRIBE_SNIPER)
        .id();
    let crow = scenario
        .add_creature(P1, "Storm Crow", 1, 2)
        .from_oracle_text_with_keywords(
            &["Flying"],
            "Flying (This creature can't be blocked except by creatures with flying or reach.)",
        )
        .id();
    // Two legal flyers require an actual target choice, not an optional prompt
    // silently ignored because the only legal object was auto-selected.
    let other_crow = scenario
        .add_creature(P1, "Storm Crow", 1, 2)
        .from_oracle_text_with_keywords(
            &["Flying"],
            "Flying (This creature can't be blocked except by creatures with flying or reach.)",
        )
        .id();
    let responses = std::array::from_fn(|_| {
        scenario
            .add_spell_to_hand_from_oracle(P0, "Flicker of Fate", true, FLICKER_OF_FATE)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 1,
            })
            .id()
    });
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::White);
    }
    let mut runner = scenario.build();
    let recipient_epoch = runner.state().objects[&crow].incarnation;
    runner
        .act(GameAction::ActivateAbility {
            source_id: sniper,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("announce Matsu's printed tap activation");
    let WaitingFor::TargetSelection {
        player,
        target_slots,
        selection,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("Matsu must ask which of the two legal flyers to damage");
    };
    assert_eq!(*player, P0);
    let legal = &target_slots[selection.current_slot].legal_targets;
    assert!(legal.contains(&TargetRef::Object(crow)));
    assert!(legal.contains(&TargetRef::Object(other_crow)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(crow)),
        })
        .expect("choose the surviving Storm Crow");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(is_tapped(&runner, sniper), "the printed tap cost was paid");
    assert_eq!(runner.state().stack.len(), 1);
    let activation = &runner.state().stack[0];
    assert!(matches!(
        activation.kind,
        StackEntryKind::ActivatedAbility { .. }
    ));
    assert_eq!(
        activation.ability().unwrap().targets,
        vec![TargetRef::Object(crow)]
    );

    let mut emitted = Vec::new();
    for _ in 0..8 {
        if matches!(
            runner.state().stack.back().map(|entry| &entry.kind),
            Some(StackEntryKind::TriggeredAbility { .. })
        ) {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        emitted.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("resolve only the damage activation")
                .events,
        );
    }
    assert_eq!(runner.state().stack.len(), 1);
    let entry = &runner.state().stack[0];
    assert_eq!(entry.source_id, sniper);
    assert_eq!(entry.controller, P0);
    assert!(
        entry.ability().unwrap().targets.is_empty(),
        "the trigger is untargeted"
    );
    let StackEntryKind::TriggeredAbility {
        trigger_event:
            Some(
                event @ GameEvent::DamageDealt {
                    source_id,
                    source_incarnation,
                    target,
                    amount,
                    is_combat,
                    ..
                },
            ),
        ..
    } = &entry.kind
    else {
        panic!("the real Matsu trigger must retain its damage event");
    };
    assert_eq!(*source_id, sniper);
    assert_eq!(
        *source_incarnation,
        Some(runner.state().objects[&sniper].incarnation)
    );
    assert_eq!(*target, TargetRef::Object(crow));
    assert_eq!(*amount, 1);
    assert!(!is_combat);
    assert!(
        emitted.contains(event),
        "the activation actually emitted this event"
    );
    let matching: Vec<_> = runner
        .state()
        .damage_dealt_this_turn
        .iter()
        .enumerate()
        .filter(|(_, record)| {
            record.source_id == *source_id
                && record.source_incarnation == *source_incarnation
                && record.target == *target
                && record.amount == *amount
                && record.is_combat == *is_combat
        })
        .collect();
    assert_eq!(matching.len(), 1, "capture the exact event/journal join");
    let record_index = matching[0].0;
    assert_eq!(matching[0].1.target_incarnation, Some(recipient_epoch));
    assert_eq!(runner.state().objects[&crow].zone, Zone::Battlefield);
    assert!(
        !is_tapped(&runner, crow),
        "damage has not resolved the freeze trigger"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    (runner, sniper, crow, responses, record_index)
}

/// Cast and pay for a real response, stopping before the underlying trigger.
fn flicker_matsu_recipient(runner: &mut GameRunner, response: ObjectId, crow: ObjectId) {
    for _ in 0..2 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("give P0 response priority");
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().stack.len(), 1);
    let pending = runner.state().stack[0].clone();
    let old_epoch = runner.state().objects[&crow].incarnation;
    let card_id = runner.state().objects[&response].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: response,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast funded Flicker of Fate in response");
    let WaitingFor::TargetSelection {
        player,
        target_slots,
        selection,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("Flicker must ask which of the several creatures to blink");
    };
    assert_eq!(*player, P0);
    assert!(target_slots[selection.current_slot]
        .legal_targets
        .contains(&TargetRef::Object(crow)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(crow)),
        })
        .expect("choose the actual damage recipient for Flicker");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().stack.len(), 2);
    let spell = runner.state().stack.back().unwrap();
    assert_eq!(spell.source_id, response);
    assert_eq!(
        spell.ability().unwrap().targets,
        vec![TargetRef::Object(crow)]
    );
    assert!(matches!(
        spell.kind,
        StackEntryKind::Spell {
            actual_mana_spent: 2,
            ..
        }
    ));
    for _ in 0..8 {
        if runner.state().objects[&response].zone == Zone::Graveyard {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("resolve the paid Flicker only");
    }
    assert_eq!(runner.state().objects[&response].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&crow].zone, Zone::Battlefield);
    // CR 400.7: the paid blink produced a different object.
    assert_ne!(runner.state().objects[&crow].incarnation, old_epoch);
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(
        runner.state().stack[0],
        pending,
        "Matsu's original event is still pending"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

fn resolve_matsu_future(state: GameState, crow: ObjectId, freezes: bool) {
    // Only raw captured gameplay states are resumed, never normalized comparands.
    let mut future = GameRunner::from_state(state);
    future.advance_until_stack_empty();
    assert!(future.state().stack.is_empty());
    assert!(matches!(
        future.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    // CR 608.2k + CR 400.7: the previously identified recipient is not a
    // returned object, unless this labelled control pins the current object.
    assert_eq!(is_tapped(&future, crow), freezes);
    // Controlled tap setup distinguishes the freeze from the immediate tap.
    future.state_mut().objects.get_mut(&crow).unwrap().tapped = true;
    advance_to_upkeep_of(&mut future, P1);
    // CR 502.3: only the bound recipient skips this untap.
    assert_eq!(is_tapped(&future, crow), freezes);
}

#[test]
fn loop_relational_matsu_pending_recipient_relation_survives_resource_projection() {
    let (mut runner, _, crow, responses, record_index) = matsu_pending_damage();
    flicker_matsu_recipient(&mut runner, responses[0], crow);
    let stale = runner.state().clone();
    assert_ne!(
        stale.damage_dealt_this_turn[record_index].target_incarnation,
        Some(stale.objects[&crow].incarnation)
    );

    // Controlled raw comparison mutation, NOT an independently played history:
    // change ONLY the selected journal recipient relation; dealer, carried event,
    // current board, damage payload and latest snapshots remain identical.
    let mut live = stale.clone();
    live.damage_dealt_this_turn[record_index].target_incarnation =
        Some(live.objects[&crow].incarnation);

    // Positive future first, before the refusal: a lost/unsupported trigger
    // cannot make the subsequent negative or comparator assertion vacuous.
    resolve_matsu_future(live.clone(), crow, true);
    resolve_matsu_future(stale.clone(), crow, false);
    assert!(loop_states_equal_modulo_resources(&stale, &stale.clone()));
    assert!(loop_states_equal_modulo_resources(&live, &live.clone()));
    assert!(
        !loop_states_equal_modulo_resources(&stale, &live),
        "pending Matsu triggers with stale versus live recipients have different futures"
    );
}

#[test]
fn loop_relational_matsu_legacy_unknown_and_live_pin_diverge_after_another_paid_blink() {
    let (mut runner, _, crow, responses, record_index) = matsu_pending_damage();
    flicker_matsu_recipient(&mut runner, responses[0], crow);
    let mut pinned = runner.state().clone();
    pinned.damage_dealt_this_turn[record_index].target_incarnation =
        Some(pinned.objects[&crow].incarnation);
    // Labelled compatibility control: None is genuinely unknown legacy history,
    // not another spelling of a pin on today's live recipient.
    let mut unknown = pinned.clone();
    unknown.damage_dealt_this_turn[record_index].target_incarnation = None;
    resolve_matsu_future(pinned.clone(), crow, true);
    resolve_matsu_future(unknown.clone(), crow, true);

    let mut unknown_future = GameRunner::from_state(unknown.clone());
    let mut pinned_future = GameRunner::from_state(pinned.clone());
    flicker_matsu_recipient(&mut unknown_future, responses[1], crow);
    flicker_matsu_recipient(&mut pinned_future, responses[1], crow);
    assert_eq!(
        unknown_future.state().objects[&crow].incarnation,
        pinned_future.state().objects[&crow].incarnation
    );
    // The unchanged runtime's legacy fallback accepts the returned object;
    // the formerly live exact pin is now stale and must reject it.
    resolve_matsu_future(unknown_future.state().clone(), crow, true);
    resolve_matsu_future(pinned_future.state().clone(), crow, false);
    assert!(loop_states_equal_modulo_resources(
        &unknown,
        &unknown.clone()
    ));
    assert!(
        !loop_states_equal_modulo_resources(&unknown, &pinned),
        "same immediate acceptance does not imply the same future after a response"
    );
}
