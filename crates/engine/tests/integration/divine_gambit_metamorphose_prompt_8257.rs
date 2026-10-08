//! Issue #8257 — resolution-time private-zone choices controlled by the
//! controller of an earlier target must not inherit that target object.
//!
//! Divine Gambit exiles the target, while Metamorphose moves it to the hidden
//! Library. In both cases the opponent, not the caster, owns the optional hand
//! choice. These tests drive the real cast/resolve pipeline with two eligible
//! cards so accepting must surface an `EffectZoneChoice`; declining must leave
//! both cards in hand.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::{ObjectId, PlayerId};

const P2: PlayerId = PlayerId(2);

const DIVINE_GAMBIT: &str = "Exile target artifact, creature, or enchantment an opponent controls. That player may put a permanent card from their hand onto the battlefield.";
const METAMORPHOSE: &str = "Put target permanent an opponent controls on top of its owner's library. That opponent may put an artifact, creature, enchantment, or land card from their hand onto the battlefield.";

fn setup(oracle: &str, name: &str) -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, name, false, oracle)
        .id();
    let target = scenario
        .add_artifact_from_oracle(P1, "Opponent Target", "")
        .id();
    let creature = scenario
        .add_creature_to_hand(P1, "Opponent Bear", 2, 2)
        .id();
    let land = scenario.add_land_to_hand(P1, "Opponent Forest").id();
    (scenario.build(), spell, target, creature, land)
}

#[test]
fn divine_gambit_accept_prompts_opponent_and_puts_chosen_permanent() {
    let (mut runner, spell, target, creature, land) = setup(DIVINE_GAMBIT, "Divine Gambit");

    let outcome = runner
        .cast(spell)
        .target_objects(&[target])
        .accept_optional()
        .effect_zone(&[creature])
        .resolve();

    assert_eq!(outcome.zone_of(target), Zone::Exile);
    assert_eq!(outcome.zone_of(creature), Zone::Battlefield);
    assert_eq!(outcome.zone_of(land), Zone::Hand);
}

#[test]
fn divine_gambit_decline_leaves_opponents_hand_untouched() {
    let (mut runner, spell, target, creature, land) = setup(DIVINE_GAMBIT, "Divine Gambit");

    let outcome = runner
        .cast(spell)
        .target_objects(&[target])
        .decline_optional()
        .resolve();

    assert_eq!(outcome.zone_of(target), Zone::Exile);
    assert_eq!(outcome.zone_of(creature), Zone::Hand);
    assert_eq!(outcome.zone_of(land), Zone::Hand);
}

#[test]
fn metamorphose_accept_prompts_opponent_after_hidden_library_move() {
    let (mut runner, spell, target, creature, land) = setup(METAMORPHOSE, "Metamorphose");

    let outcome = runner
        .cast(spell)
        .target_objects(&[target])
        .accept_optional()
        .effect_zone(&[land])
        .resolve();

    assert_eq!(outcome.zone_of(target), Zone::Library);
    assert_eq!(outcome.zone_of(land), Zone::Battlefield);
    assert_eq!(outcome.zone_of(creature), Zone::Hand);
}

#[test]
fn metamorphose_decline_leaves_opponents_hand_untouched() {
    let (mut runner, spell, target, creature, land) = setup(METAMORPHOSE, "Metamorphose");

    let outcome = runner
        .cast(spell)
        .target_objects(&[target])
        .decline_optional()
        .resolve();

    assert_eq!(outcome.zone_of(target), Zone::Library);
    assert_eq!(outcome.zone_of(creature), Zone::Hand);
    assert_eq!(outcome.zone_of(land), Zone::Hand);
}

#[test]
fn metamorphose_prompts_targets_controller_not_owner_after_library_move() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Metamorphose", false, METAMORPHOSE)
        .id();
    let target = {
        let mut target = scenario.add_artifact_from_oracle(P1, "P1-Owned P2 Target", "");
        target.controlled_by(P2);
        target.id()
    };
    let p1_card = scenario.add_land_to_hand(P1, "P1 Forest").id();
    let p2_card = scenario.add_land_to_hand(P2, "P2 Island").id();
    let p2_other = scenario.add_creature_to_hand(P2, "P2 Bear", 2, 2).id();
    let mut runner = scenario.build();

    let mut cast = runner.cast(spell).target_object(target).commit();
    while matches!(cast.state().waiting_for, WaitingFor::Priority { .. }) {
        cast.act(GameAction::PassPriority)
            .expect("priority pass must advance Metamorphose resolution");
    }

    match &cast.state().waiting_for {
        WaitingFor::OptionalEffectChoice { player, .. } => assert_eq!(
            *player, P2,
            "the optional hand-put choice belongs to the target's controller"
        ),
        other => panic!("expected Metamorphose optional choice for P2, got {other:?}"),
    }
    cast.act(GameAction::DecideOptionalEffect { accept: true })
        .expect("P2 must be able to accept Metamorphose's optional effect");

    match &cast.state().waiting_for {
        WaitingFor::EffectZoneChoice { player, cards, .. } => {
            assert_eq!(
                *player, P2,
                "the hand-card prompt must remain assigned to P2"
            );
            assert!(
                cards.contains(&p2_card),
                "P2's hand card must be selectable"
            );
            assert!(
                cards.contains(&p2_other),
                "P2's second permanent forces the interactive choice path"
            );
            assert!(
                !cards.contains(&p1_card),
                "the target owner's hand must not replace its controller's hand"
            );
        }
        other => panic!("expected Metamorphose hand choice for P2, got {other:?}"),
    }
    cast.act(GameAction::SelectCards {
        cards: vec![p2_card],
    })
    .expect("P2 must be able to put the selected permanent onto the battlefield");

    assert_eq!(cast.state().objects[&target].zone, Zone::Library);
    assert_eq!(cast.state().objects[&p2_card].zone, Zone::Battlefield);
    assert_eq!(cast.state().objects[&p2_other].zone, Zone::Hand);
    assert_eq!(cast.state().objects[&p1_card].zone, Zone::Hand);
}
