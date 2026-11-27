//! "That player" / "that opponent" after a trigger head names the player the
//! head introduces: Calculating Lich's attacked opponent (CR 508.5) and Zhang
//! Liao, Hero of Hefei's damaged opponent (CR 120.3), not the attacking player
//! or a combat defending player.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::Zone;

const CALCULATING_LICH: &str =
    "Menace\nWhenever a creature attacks one of your opponents, that player loses 1 life.";
const ZHANG_LIAO: &str =
    "Whenever Zhang Liao deals damage to an opponent, that opponent discards a card.";
const SOULS_FIRE: &str =
    "Target creature you control deals damage equal to its power to any target.";
const P2: PlayerId = PlayerId(2);

/// CR 508.5: "that player" is the opponent being attacked, not the player
/// who declared the attack.
#[test]
fn calculating_lich_drains_the_attacked_opponent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Calculating Lich", 5, 5, CALCULATING_LICH);
    let attacker = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    let (p0_life, p1_life) = (runner.life(P0), runner.life(P1));
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("attack the opponent");
    assert_eq!(runner.state().stack.len(), 1);
    runner.resolve_top();
    assert_eq!(runner.life(P0), p0_life);
    assert_eq!(runner.life(P1), p1_life - 1);
}

/// CR 508.5 + CR 508.5a: Each firing names the defender of its own attacker.
#[test]
fn calculating_lich_drains_each_attacked_opponent_while_attacking() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let lich = scenario
        .add_creature_from_oracle(P0, "Calculating Lich", 5, 5, CALCULATING_LICH)
        .id();
    let other = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    let initial_life = [P0, P1, P2].map(|player| runner.life(player));
    runner
        .declare_attackers(&[
            (lich, AttackTarget::Player(P1)),
            (other, AttackTarget::Player(P2)),
        ])
        .expect("attack two different opponents");

    if let WaitingFor::OrderTriggers { player, triggers } = &runner.state().waiting_for {
        assert_eq!(*player, P0);
        assert_eq!(triggers.len(), 2);
        runner
            .act(GameAction::OrderTriggers { order: vec![1, 0] })
            .expect("order both firings");
    }
    assert_eq!(runner.state().stack.len(), 2);
    assert!(runner
        .state()
        .stack
        .iter()
        .all(|entry| entry.source_id == lich));
    runner.advance_until_stack_empty();

    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(
        [P0, P1, P2].map(|player| runner.life(player)),
        [initial_life[0], initial_life[1] - 1, initial_life[2] - 1]
    );
}

/// CR 120.3 + CR 701.9a: noncombat damage from Zhang Liao makes the damaged
/// opponent discard; no combat defending player exists to stand in for it.
#[test]
fn zhang_liao_noncombat_damage_makes_the_damaged_opponent_discard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let zhang = scenario
        .add_creature_from_oracle(P0, "Zhang Liao, Hero of Hefei", 3, 3, ZHANG_LIAO)
        .id();
    let souls_fire = scenario
        .add_spell_to_hand_from_oracle(P0, "Soul's Fire", false, SOULS_FIRE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let discard = scenario.add_card_to_hand(P1, "Island");
    let mut runner = scenario.build();
    let p1_life = runner.life(P1);

    runner
        .cast(souls_fire)
        .target_object(zhang)
        .target_player(P1)
        .commit();
    runner.advance_until_stack_empty();

    assert_eq!(runner.life(P1), p1_life - 3);
    assert_eq!(runner.state().objects[&discard].zone, Zone::Graveyard);
}
