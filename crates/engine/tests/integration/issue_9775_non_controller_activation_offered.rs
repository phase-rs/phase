//! Regression for GitHub issue #9775: an ability its source says a non-controller
//! may activate ("Only your opponents may activate this ability", "Any player may
//! activate this ability") must be offered by `legal_actions`, which is what the
//! client's per-object action list (`legal_actions_by_object`) is built from.
//!
//! CR 602.2: "Only an object's controller (or its owner, if it doesn't have a
//! controller) can activate its activated ability unless the object specifically
//! says otherwise." CR 602.5d: "as a sorcery" timing still applies to the
//! activating player.

use engine::ai_support::{legal_actions, legal_actions_full};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::PlayerFilter;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const GOAT: &str = "{1}: Draw a card. Gain control of this creature and put a -1/-1 counter on it. Only your opponents may activate this ability and only as a sorcery.\nWhen this creature dies, if it had one or more -1/-1 counters on it, its owner draws that many cards and each other player loses that much life.";
const MERCENARIES: &str = "{3}: The next time this creature would deal damage to you this turn, prevent that damage. Any player may activate this ability.";

fn colorless(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn activate(source_id: ObjectId) -> GameAction {
    GameAction::ActivateAbility {
        source_id,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    }
}

fn priority_holder(runner: &engine::game::scenario::GameRunner) -> PlayerId {
    match runner.state().waiting_for {
        WaitingFor::Priority { player } => player,
        ref other => panic!("expected a priority window, got {other:?}"),
    }
}

/// The triage repro: P1 controls the Goat, P0 has priority in its own precombat
/// main phase with an empty stack and {1} floating. P0 is offered the ability
/// (flat list and the per-object bucket the client renders), and activating it
/// draws P0 a card and moves the Goat to P0 with a -1/-1 counter.
#[test]
fn oft_nabbed_goat_is_offered_to_its_controllers_opponent_and_resolves() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let goat = s
        .add_creature_from_oracle(P1, "Oft-Nabbed Goat", 0, 5, GOAT)
        .id();
    s.with_library_top(P0, &["Card A", "Card B"]);
    s.with_mana_pool(P0, colorless(1));
    let mut r = s.build();
    assert_eq!(priority_holder(&r), P0, "reach guard: P0 holds priority");
    assert_eq!(
        r.state().objects[&goat].abilities[0].activator_filter,
        Some(PlayerFilter::Opponent),
        "reach guard: the Goat's ability is opponents-only"
    );

    let action = activate(goat);
    assert!(
        legal_actions(r.state()).contains(&action),
        "CR 602.2: legal_actions must offer an opponents-only ability to the controller's opponent"
    );
    let (_, _, by_object) = legal_actions_full(r.state());
    assert!(
        by_object
            .get(&goat)
            .is_some_and(|bucket| bucket.contains(&action)),
        "the client's per-object bucket for the Goat must carry the activation"
    );

    let hand_before = r.state().players[0].hand.len();
    r.act(action).expect("the offered activation is accepted");
    r.advance_until_stack_empty();
    let goat_obj = &r.state().objects[&goat];
    assert_eq!(goat_obj.controller, P0, "P0 gains control of the Goat");
    assert_eq!(
        goat_obj.counters.get(&CounterType::Minus1Minus1).copied(),
        Some(1),
        "the Goat gets a -1/-1 counter"
    );
    assert_eq!(r.state().players[0].hand.len() - hand_before, 1, "P0 draws");
}

/// Control change (hostile): once P0 has taken the Goat, P0 is its controller, so
/// the opponents-only ability is no longer P0's to activate (CR 602.2 reads the
/// CURRENT controller), even with mana and sorcery timing available.
#[test]
fn oft_nabbed_goat_is_not_offered_again_after_its_activator_gains_control() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let goat = s
        .add_creature_from_oracle(P1, "Oft-Nabbed Goat", 0, 5, GOAT)
        .id();
    s.with_library_top(P0, &["Card A", "Card B"]);
    s.with_mana_pool(P0, colorless(2));
    let mut r = s.build();

    r.act(activate(goat)).expect("first activation is legal");
    r.advance_until_stack_empty();
    assert_eq!(
        r.state().objects[&goat].controller,
        P0,
        "reach guard: P0 now controls the Goat"
    );
    assert_eq!(priority_holder(&r), P0, "reach guard: P0 holds priority");
    assert_eq!(
        r.state().players[0].mana_pool.total(),
        1,
        "reach guard: P0 still has {{1}} floating"
    );
    assert!(
        !legal_actions(r.state()).contains(&activate(goat)),
        "CR 602.2: the Goat's new controller may not activate its opponents-only ability"
    );
}

/// The controller is never offered its own Goat (CR 602.2 "only your opponents").
/// One fixture: while P1 controls the Goat it IS offered to P0 (reach guard:
/// parse, timing and mana all admit P0), then control moves to P0 with timing and
/// resources unchanged and the offer disappears.
#[test]
fn oft_nabbed_goat_is_not_offered_to_its_controller() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let goat = s
        .add_creature_from_oracle(P0, "Oft-Nabbed Goat", 0, 5, GOAT)
        .controlled_by(P1)
        .id();
    s.with_mana_pool(P0, colorless(1));
    let mut r = s.build();
    assert_eq!(priority_holder(&r), P0, "reach guard: P0 holds priority");
    assert!(
        legal_actions(r.state()).contains(&activate(goat)),
        "reach guard: offered to P0 while P1 controls the Goat"
    );

    // Same layer-safe control assignment `CardBuilder::controlled_by` uses.
    let obj = r.state_mut().objects.get_mut(&goat).unwrap();
    obj.base_controller = Some(P0);
    obj.controller = P0;
    assert_eq!(
        priority_holder(&r),
        P0,
        "reach guard: P0 still holds priority"
    );
    assert_eq!(
        r.state().players[0].mana_pool.total(),
        1,
        "reach guard: P0 still has {{1}} floating"
    );
    assert!(
        !legal_actions(r.state()).contains(&activate(goat)),
        "CR 602.2: the Goat's controller may not activate its opponents-only ability"
    );
}

/// CR 602.5d: the opponent may activate it only as a sorcery. On the
/// controller's turn the opponent is not offered it.
#[test]
fn oft_nabbed_goat_is_not_offered_outside_the_opponents_sorcery_timing() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let goat = s
        .add_creature_from_oracle(P1, "Oft-Nabbed Goat", 0, 5, GOAT)
        .id();
    s.with_mana_pool(P0, colorless(1));
    let mut r = s.build();
    assert!(
        legal_actions(r.state()).contains(&activate(goat)),
        "reach guard: offered to P0 in its own main phase with an empty stack"
    );
    // P1's turn; P0 holds priority (not the active player).
    r.state_mut().active_player = P1;
    r.state_mut().priority_player = P0;
    r.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    assert!(
        !legal_actions(r.state()).contains(&activate(goat)),
        "CR 602.5d: not offered when P0 could not cast a sorcery"
    );
}

/// "Any player may activate this ability" (PlayerFilter::All): the controller's
/// opponent is offered it at instant speed, and so is the controller.
#[test]
fn any_player_may_activate_is_offered_to_both_controller_and_opponent() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let mercs = s
        .add_creature_from_oracle(P1, "Mercenaries", 3, 3, MERCENARIES)
        .id();
    s.with_mana_pool(P0, colorless(3));
    s.with_mana_pool(P1, colorless(3));
    let mut r = s.build();
    assert_eq!(
        r.state().objects[&mercs].abilities[0].activator_filter,
        Some(PlayerFilter::All),
        "reach guard: Mercenaries is any-player"
    );
    assert_eq!(priority_holder(&r), P0, "reach guard: P0 holds priority");
    assert!(
        legal_actions(r.state()).contains(&activate(mercs)),
        "CR 602.2: the opponent is offered an any-player ability"
    );

    r.state_mut().priority_player = P1;
    r.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    assert!(
        legal_actions(r.state()).contains(&activate(mercs)),
        "the controller is still offered its own any-player ability at instant speed"
    );
}
