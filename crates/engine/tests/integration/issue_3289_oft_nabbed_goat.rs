//! Regression for GitHub issue #3289 — Oft-Nabbed Goat opponent-only sorcery activation.
//!
//! Oracle: "{1}: Draw a card. Gain control of this creature and put a -1/-1 counter
//! on it. Only your opponents may activate this ability and only as a sorcery."

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{AbilityKind, ActivationRestriction, PlayerFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastingVariant, StackEntry, StackEntryKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const GOAT_ORACLE: &str = "{1}: Draw a card. Gain control of this creature and put a -1/-1 counter on it. Only your opponents may activate this ability and only as a sorcery.\n\
When this creature dies, if it had one or more -1/-1 counters on it, its owner draws that many cards and each other player loses that much life.";
const VINDICATE_ORACLE: &str = "Destroy target permanent.";

fn fund_generic(
    runner: &mut engine::game::scenario::GameRunner,
    player: engine::types::player::PlayerId,
    count: u32,
) {
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
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

#[test]
fn oft_nabbed_goat_parses_opponent_only_sorcery_activation() {
    let mut scenario = GameScenario::new();
    let goat = scenario
        .add_creature_from_oracle(P0, "Oft-Nabbed Goat", 1, 1, GOAT_ORACLE)
        .id();
    let runner = scenario.build();
    let ability = runner
        .state()
        .objects
        .get(&goat)
        .and_then(|obj| {
            obj.abilities
                .iter()
                .find(|a| a.kind == AbilityKind::Activated)
        })
        .expect("activated ability");
    assert_eq!(ability.activator_filter, Some(PlayerFilter::Opponent));
    assert!(
        ability
            .activation_restrictions
            .contains(&ActivationRestriction::AsSorcery),
        "must carry AsSorcery restriction"
    );
}

#[test]
fn oft_nabbed_goat_controller_cannot_activate() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let goat = scenario
        .add_creature_from_oracle(P0, "Oft-Nabbed Goat", 1, 1, GOAT_ORACLE)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    fund_generic(&mut runner, P0, 1);

    let err = runner.act(GameAction::ActivateAbility {
        source_id: goat,
        ability_index: 0,
    });
    assert!(
        err.is_err(),
        "controller must not activate opponent-only ability (CR 602.2a)"
    );
}

#[test]
fn oft_nabbed_goat_opponent_activates_at_sorcery_speed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Toughness 2 so the ability's self-inflicted -1/-1 counter (below) leaves the
    // goat a 1/1 survivor rather than a 0/0 that dies as an SBA (CR 704.5f) — the
    // test asserts the surviving goat is under the opponent's control.
    let goat = scenario
        .add_creature_from_oracle(P0, "Oft-Nabbed Goat", 2, 2, GOAT_ORACLE)
        .id();
    // The ability's "Draw a card" is resolved by the activating opponent P1. Give
    // P1 a library card so that draw does not empty their (default-empty) library
    // and lose them the game (CR 704.5b) — an eliminated P1 would have its gained
    // control of the goat end (CR 800.4a), which is not what this test exercises.
    scenario.add_card_to_library_top(P1, "Plains");

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    fund_generic(&mut runner, P1, 1);

    runner.activate(goat, 0).resolve();

    assert_eq!(
        runner.state().objects[&goat].controller,
        P1,
        "opponent must gain control"
    );
    assert!(
        runner.state().objects.get(&goat).is_some_and(|obj| obj
            .counters
            .get(&CounterType::Minus1Minus1)
            .copied()
            == Some(1)),
        "activation must put a -1/-1 counter on the goat"
    );
}

#[test]
fn oft_nabbed_goat_opponent_rejected_at_instant_speed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let goat = scenario
        .add_creature_from_oracle(P0, "Oft-Nabbed Goat", 1, 1, GOAT_ORACLE)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    let spell = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(501),
        P1,
        "Shock".to_string(),
        Zone::Stack,
    );
    if let Some(obj) = runner.state_mut().objects.get_mut(&spell) {
        obj.card_types.core_types = vec![CoreType::Instant];
    }
    runner.state_mut().stack.push_back(StackEntry {
        id: spell,
        source_id: spell,
        controller: P1,
        kind: StackEntryKind::Spell {
            card_id: CardId(501),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    let err = runner.act(GameAction::ActivateAbility {
        source_id: goat,
        ability_index: 0,
    });
    assert!(
        err.is_err(),
        "opponent-only activation must be rejected while stack is non-empty (CR 602.5d)"
    );
}

#[test]
fn opponents_only_activation_parses_turn_timing_composition() {
    const ORACLE: &str = "{T}: Draw a card. Only your opponents may activate this ability and only during your turn.";
    let mut scenario = GameScenario::new();
    let source = scenario
        .add_creature_from_oracle(P0, "Test Permanent", 1, 1, ORACLE)
        .id();
    let runner = scenario.build();
    let ability = runner
        .state()
        .objects
        .get(&source)
        .and_then(|obj| {
            obj.abilities
                .iter()
                .find(|a| a.kind == AbilityKind::Activated)
        })
        .expect("activated ability");
    assert_eq!(ability.activator_filter, Some(PlayerFilter::Opponent));
    assert!(
        ability
            .activation_restrictions
            .contains(&ActivationRestriction::DuringYourTurn),
        "opponent permission must compose with DuringYourTurn"
    );
}

#[test]
fn oft_nabbed_goat_owner_draws_and_each_other_player_loses_from_lki_counters() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 3289);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    scenario.add_card_to_library_top(P0, "Plains");
    scenario.add_card_to_library_top(P0, "Island");

    let goat = scenario
        .add_creature_from_oracle(P0, "Oft-Nabbed Goat", 3, 3, GOAT_ORACLE)
        .controlled_by(P1)
        .id();
    scenario.with_counter(goat, CounterType::Minus1Minus1, 2);
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Vindicate", false, VINDICATE_ORACLE)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(destroy).target_object(goat).resolve();

    assert_eq!(
        outcome.zone_of(goat),
        Zone::Graveyard,
        "reach-guard: Vindicate must destroy the Goat"
    );
    outcome.assert_hand_drawn(P0, 2);
    outcome.assert_life_delta(P0, 0);
    outcome.assert_life_delta(P1, -2);
    outcome.assert_life_delta(p2, -2);
}

/// Positive: CR 108.3 keeps the Goat's owner as the exclusion anchor, while CR
/// 800.4a removes only stack objects controlled by the leaving player. The
/// Goat's controller still controls the dies trigger after the owner concedes.
#[test]
fn oft_nabbed_goat_owner_concedes_each_other_remaining_player_loses_life() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 3289);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );

    let goat = scenario
        .add_creature_from_oracle(P0, "Oft-Nabbed Goat", 3, 3, GOAT_ORACLE)
        .controlled_by(P1)
        .id();
    scenario.with_counter(goat, CounterType::Minus1Minus1, 2);
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Vindicate", false, VINDICATE_ORACLE)
        .id();

    let mut runner = scenario.build();
    runner.cast(destroy).target_object(goat).commit();
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. }
                if runner.state().objects[&goat].zone == Zone::Graveyard
                    && runner
                        .state()
                        .stack
                        .iter()
                        .any(|entry| entry.source_id == goat) =>
            {
                break;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must advance to the Goat trigger");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("ordering the Goat trigger must succeed");
            }
            other => panic!("unexpected prompt before Goat trigger resolved: {other:?}"),
        }
    }
    assert_eq!(
        runner.state().objects[&goat].zone,
        Zone::Graveyard,
        "reach-guard: the Goat must die before its owner concedes"
    );
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == goat),
        "reach-guard: the Goat's dies trigger must be waiting on the stack"
    );

    runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("the Goat's owner can concede before the trigger resolves");
    assert!(
        runner
            .state()
            .players
            .iter()
            .any(|player| player.id == P0 && player.is_eliminated),
        "reach-guard: P0 must have left the game"
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        18,
        "the remaining player who controlled the Goat still loses two life"
    );
    assert_eq!(
        runner.life(p2),
        18,
        "the other remaining player still loses two life"
    );
}
