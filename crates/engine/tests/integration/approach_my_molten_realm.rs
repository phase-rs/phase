//! Approach My Molten Realm: damage doubles until the archenemy's next turn.
//!
//! Oracle: "When you set this scheme in motion, until your next turn, if a
//! source would deal damage, it deals double that damage instead."
//!
//! CR 701.32a + CR 904.9: the set-in-motion trigger installs the doubler.
//! CR 614.1a: the "instead" clause is a damage-modification replacement.
//! CR 611.2a: the "until your next turn" window rides on the installing
//! ability's duration and becomes `UntilPlayerNextTurn` expiry on the
//! floating shield — so it still applies on the opponents' intervening turn
//! and is gone when the archenemy's next turn begins.
//!
//! Revert-to-red: drop the duration-shell lift — the trigger installs no
//! shield (install assertions fail). Stamp `EndOfTurn` instead of deriving
//! the window — the shield is gone before the extra checks (same). Break the
//! untap-step prune — the second Bolt still deals 6 (expiry assertions fail).

use engine::database::synthesis::synthesize_archenemy;
use engine::game::deck_loading::create_scheme_deck_card;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::parser::parse_oracle_text;
use engine::types::ability::{DamageModification, RestrictionExpiry, ShieldKind};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;

const APPROACH_MY_MOLTEN_REALM: &str = "When you set this scheme in motion, until your next turn, if a source would deal damage, it deals double that damage instead.";
const BOLT: &str = "Lightning Bolt deals 3 damage to any target.";

fn scheme_face(name: &str, oracle: &str) -> CardFace {
    let parsed = parse_oracle_text(oracle, name, &[], &["Scheme".to_string()], &[]);
    assert!(
        !format!("{parsed:?}").contains("Unimplemented"),
        "{name}: document is gapped"
    );
    let mut face = CardFace {
        name: name.to_string(),
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        ..Default::default()
    };
    face.card_type.core_types.push(CoreType::Scheme);
    synthesize_archenemy(&mut face);
    let gaps = engine::game::coverage::card_face_gaps(&face);
    assert!(gaps.is_empty(), "{name}: gaps: {gaps:?}");
    face
}

/// Resolve every pending trigger with no choices (the install is prompt-free).
fn drain_triggers(runner: &mut GameRunner) {
    for _ in 0..64 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt while draining triggers: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "all triggers must have resolved"
    );
}

/// Drive to `target`, declaring no attackers/blockers through combat and
/// passing priority. Panics on any other prompt.
fn drive_to_phase(runner: &mut GameRunner, target: Phase) {
    for _ in 0..64 {
        runner.advance_to_phase(target);
        if runner.state().phase == target {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner.declare_attackers(&[]).expect("declare no attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).expect("declare no blockers");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            other => panic!("unexpected prompt driving to {target:?}: {other:?}"),
        }
    }
    panic!("never reached {target:?}");
}

fn life_of(runner: &GameRunner, player: engine::types::player::PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player must exist")
        .life
}

#[test]
fn approach_my_molten_realm_doubles_damage_until_next_turn() {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 2, 42);
    scenario.at_phase(Phase::Draw);
    for i in 0..10 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
    }
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );
    let bolt_one = scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, BOLT)
        .id();
    let bolt_two = scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, BOLT)
        .id();
    let mut runner = scenario.build();

    let face = scheme_face("Approach My Molten Realm", APPROACH_MY_MOLTEN_REALM);
    let scheme = create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    // Turn 1: the turn-based action sets the scheme in motion; its trigger
    // installs the floating doubler.
    drive_to_phase(&mut runner, Phase::PreCombatMain);
    assert!(
        runner.state().command_zone.contains(&scheme) && !runner.state().objects[&scheme].face_down,
        "reach guard: turn 1 must set the scheme in motion"
    );
    drain_triggers(&mut runner);
    let shields: Vec<_> = runner
        .state()
        .pending_damage_replacements
        .iter()
        .filter(|shield| shield.event == ReplacementEvent::DamageDone)
        .collect();
    assert_eq!(
        shields.len(),
        1,
        "the trigger must install exactly one damage shield"
    );
    let shield = shields[0];
    assert_eq!(
        shield.damage_modification,
        Some(DamageModification::Double),
        "the shield must double damage"
    );
    assert_eq!(
        shield.shield_kind,
        ShieldKind::None,
        "the continuous doubler must re-apply, not consume"
    );
    assert!(
        !shield.consume_on_apply,
        "the continuous doubler must not consume on apply"
    );
    assert_eq!(
        shield.expiry,
        Some(RestrictionExpiry::UntilPlayerNextTurn { player: P0 }),
        "the shield must expire at the archenemy's next turn"
    );

    // A Bolt under the doubler deals 6.
    let before = life_of(&runner, P1);
    runner.cast(bolt_one).target_players(&[P1]).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        life_of(&runner, P1),
        before - 6,
        "damage under the doubler must be doubled"
    );

    // The window covers the opponents' intervening turn, then ends: on the
    // archenemy's next turn (before precombat main re-fires the scheme) the
    // shield is gone and damage is normal again.
    drive_to_phase(&mut runner, Phase::End);
    drive_to_phase(&mut runner, Phase::PreCombatMain);
    assert_eq!(
        runner.state().active_player,
        P1,
        "reach guard: the opponent's turn must intervene"
    );
    assert_eq!(
        runner
            .state()
            .pending_damage_replacements
            .iter()
            .filter(|shield| shield.event == ReplacementEvent::DamageDone)
            .count(),
        1,
        "the window must still cover the intervening turn"
    );
    drive_to_phase(&mut runner, Phase::End);
    drive_to_phase(&mut runner, Phase::Upkeep);
    assert_eq!(
        runner.state().active_player,
        P0,
        "reach guard: back to the archenemy's turn"
    );
    assert!(
        runner
            .state()
            .pending_damage_replacements
            .iter()
            .all(|shield| shield.event != ReplacementEvent::DamageDone),
        "the shield must expire at the archenemy's next turn"
    );
    runner
        .state_mut()
        .add_mana_to_pool(P0, ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]));
    let before = life_of(&runner, P1);
    runner.cast(bolt_two).target_players(&[P1]).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        life_of(&runner, P1),
        before - 3,
        "damage after expiry must be normal"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}
