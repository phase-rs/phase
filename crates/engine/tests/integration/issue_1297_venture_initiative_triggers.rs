//! GitHub issue #1297 — venture-into-dungeon and initiative triggers must
//! parse, match their events, and resolve through the standard trigger pipeline.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 309.4c: room abilities trigger on dungeon room entry.
//!   - CR 603.2: triggered abilities trigger when their event occurs.
//!   - CR 701.49: venture into the dungeon moves the venture marker.
//!   - CR 726.2 / CR 726.3: taking and holding the initiative.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
const VENTURE_TRIGGER: &str =
    "Whenever you venture into the dungeon, create a 5/5 black Zombie Giant creature token.";
const TAKE_INITIATIVE_ETB: &str = "When this creature enters, you take the initiative.";
const INITIATIVE_ATTACK: &str =
    "Whenever this creature attacks, if you have the initiative, draw a card.";

fn zombie_giant_tokens(state: &engine::types::game_state::GameState) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| {
            state.objects.get(id).is_some_and(|o| {
                o.is_token
                    && o.card_types.subtypes.iter().any(|s| s == "Zombie")
                    && o.card_types.subtypes.iter().any(|s| s == "Giant")
                    && o.power == Some(5)
                    && o.toughness == Some(5)
            })
        })
        .count()
}

/// Acererak-class venture trigger: taking the initiative auto-ventures into
/// Undercity (CR 726.2), emitting `RoomEntered`, which must fire the venture
/// trigger and create the Zombie Giant token.
#[test]
fn venture_into_dungeon_trigger_resolves_after_initiative_venture() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Plains");
    scenario.add_card_to_library_top(P0, "Forest");

    scenario
        .add_creature_from_oracle(P0, "Acererak Probe", 5, 5, VENTURE_TRIGGER)
        .id();
    let seasoned = scenario
        .add_creature_to_hand_from_oracle(P0, "Seasoned Dungeoneer", 3, 4, TAKE_INITIATIVE_ETB)
        .id();

    let mut runner = scenario.build();
    let tokens_before = zombie_giant_tokens(runner.state());

    runner.cast(seasoned).search_first_legal().resolve();

    let tokens_after = zombie_giant_tokens(runner.state());
    assert!(
        tokens_after > tokens_before,
        "issue #1297: venture trigger must resolve and create a 5/5 Zombie Giant; \
         before={tokens_before} after={tokens_after}, waiting_for={:?}",
        runner.state().waiting_for
    );
}

/// "Whenever you take the initiative" must fire when initiative is taken.
#[test]
fn takes_initiative_trigger_resolves_on_initiative_taken() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Plains");
    scenario.add_card_to_library_top(P0, "Forest");

    scenario
        .add_creature_from_oracle(
            P0,
            "Initiative Draw Probe",
            2,
            2,
            "Whenever you take the initiative, draw a card.",
        )
        .id();
    let seasoned = scenario
        .add_creature_to_hand_from_oracle(P0, "Seasoned Dungeoneer", 3, 4, TAKE_INITIATIVE_ETB)
        .id();

    let mut runner = scenario.build();
    let hand_before = runner.state().players[0].hand.len();

    runner.cast(seasoned).search_first_legal().resolve();

    let hand_after = runner.state().players[0].hand.len();
    assert!(
        hand_after > hand_before,
        "issue #1297: take-the-initiative trigger must resolve (draw); \
         hand before={hand_before} after={hand_after}, waiting_for={:?}",
        runner.state().waiting_for
    );
}

#[test]
fn initiative_attack_trigger_skips_without_initiative() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let attacker = scenario
        .add_creature_from_oracle(P0, "Initiative Attacker", 3, 3, INITIATIVE_ATTACK)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().initiative = None;
    let hand_before = runner.state().players[0].hand.len();

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare attackers");
    drain_to_priority(&mut runner);

    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before,
        "attack without initiative must not draw; waiting_for={:?}",
        runner.state().waiting_for
    );
}

#[test]
fn initiative_attack_trigger_draws_with_initiative() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Island");

    let attacker = scenario
        .add_creature_from_oracle(P0, "Initiative Attacker", 3, 3, INITIATIVE_ATTACK)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().initiative = Some(P0);
    let hand_before = runner.state().players[0].hand.len();

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare attackers with initiative");
    drain_to_priority(&mut runner);

    assert!(
        runner.state().players[0].hand.len() > hand_before,
        "attack with initiative must draw a card; hand before={hand_before} after={}, waiting_for={:?}",
        runner.state().players[0].hand.len(),
        runner.state().waiting_for
    );
}

fn drain_to_priority(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..200 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::SearchChoice { cards, count, .. } => {
                let pick: Vec<_> = cards.iter().take(*count).copied().collect();
                runner
                    .act(GameAction::SelectCards { cards: pick })
                    .expect("search choice");
            }
            _ => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority during drain");
            }
        }
    }
}
