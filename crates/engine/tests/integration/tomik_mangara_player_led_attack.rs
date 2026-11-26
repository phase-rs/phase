//! Tomik, Wielder of Law and Mangara, the Diplomat: "Whenever an opponent
//! attacks with creatures, if two or more of those creatures are attacking you
//! and/or planeswalkers you control, ..." fires once for the attacking
//! opponent's declaration (CR 508.3c), counts that opponent's whole attack, and
//! Tomik's "that opponent" is the attacking player.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::Zone;

const TOMIK: &str = "Affinity for planeswalkers (This spell costs {1} less to cast for each planeswalker you control.)\nFlying, vigilance\nWhenever an opponent attacks with creatures, if two or more of those creatures are attacking you and/or planeswalkers you control, that opponent loses 3 life and you draw a card.";
const MANGARA: &str = "Lifelink\nWhenever an opponent attacks with creatures, if two or more of those creatures are attacking you and/or planeswalkers you control, draw a card.\nWhenever an opponent casts their second spell each turn, draw a card.";
const JACE_BELEREN: &str = "+2: Each player draws a card.\n\u{2212}1: Target player draws a card.\n\u{2212}10: Target player mills twenty cards.";
const ACT_OF_AGGRESSION: &str = "({R/P} can be paid with either {R} or 2 life.)\nGain control of target creature an opponent controls until end of turn. Untap that creature. It gains haste until end of turn.";

struct Board {
    runner: GameRunner,
    observer: ObjectId,
    planeswalker: ObjectId,
    attackers: Vec<ObjectId>,
    draw: ObjectId,
    steal: ObjectId,
}

/// P0 attacks; `defender` controls the observer and a planeswalker.
fn board(players: u8, defender: PlayerId, name: &str, oracle: &str, attackers: usize) -> Board {
    let mut scenario = GameScenario::new_n_player(players, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let observer = scenario
        .add_creature_from_oracle(defender, name, 2, 4, oracle)
        .id();
    let planeswalker = scenario
        .add_planeswalker_from_oracle(defender, "Jace Beleren", "Jace", 3, JACE_BELEREN)
        .id();
    let draw = scenario.add_card_to_library_top(defender, "Island");
    let steal = scenario
        .add_spell_to_hand_from_oracle(defender, "Act of Aggression", true, ACT_OF_AGGRESSION)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let attackers = (0..attackers)
        .map(|i| {
            scenario
                .add_creature(P0, &format!("Grizzly Bears {i}"), 2, 2)
                .id()
        })
        .collect();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    Board {
        runner,
        observer,
        planeswalker,
        attackers,
        draw,
        steal,
    }
}

fn observer_triggers(runner: &GameRunner, observer: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == observer)
        .count()
}

/// CR 508.3c + CR 603.4: one attacker at you and one at your planeswalker
/// meet the threshold; the attacking opponent loses 3 and you draw once.
#[test]
fn tomik_drains_the_attacking_opponent_for_an_attack_on_you_and_your_planeswalker() {
    let mut b = board(2, P1, "Tomik, Wielder of Law", TOMIK, 2);
    let (p0_life, p1_life) = (b.runner.life(P0), b.runner.life(P1));
    b.runner
        .declare_attackers(&[
            (b.attackers[0], AttackTarget::Player(P1)),
            (b.attackers[1], AttackTarget::Planeswalker(b.planeswalker)),
        ])
        .expect("attack you and your planeswalker");
    assert_eq!(observer_triggers(&b.runner, b.observer), 1);
    b.runner.advance_until_stack_empty();
    assert_eq!(b.runner.life(P0), p0_life - 3);
    assert_eq!(b.runner.life(P1), p1_life);
    assert_eq!(b.runner.state().objects[&b.draw].zone, Zone::Hand);
}

/// CR 603.4: one qualifying attacker is below the threshold; three still
/// make one trigger and one draw.
#[test]
fn tomik_threshold_counts_the_whole_declaration_once() {
    let mut one = board(2, P1, "Tomik, Wielder of Law", TOMIK, 1);
    let p0_life = one.runner.life(P0);
    one.runner
        .declare_attackers(&[(one.attackers[0], AttackTarget::Player(P1))])
        .expect("attack with one creature");
    assert_eq!(observer_triggers(&one.runner, one.observer), 0);
    one.runner.advance_until_stack_empty();
    assert_eq!(one.runner.life(P0), p0_life);

    let mut three = board(2, P1, "Tomik, Wielder of Law", TOMIK, 3);
    let p0_life = three.runner.life(P0);
    let hand_before = three.runner.state().players[usize::from(P1.0)].hand.len();
    let attacks: Vec<_> = three
        .attackers
        .iter()
        .map(|&id| (id, AttackTarget::Player(P1)))
        .collect();
    three
        .runner
        .declare_attackers(&attacks)
        .expect("attack with three creatures");
    assert_eq!(observer_triggers(&three.runner, three.observer), 1);
    three.runner.advance_until_stack_empty();
    assert_eq!(three.runner.life(P0), p0_life - 3);
    assert_eq!(
        three.runner.state().players[usize::from(P1.0)].hand.len(),
        hand_before + 1
    );
}

/// CR 508.3c + CR 508.5: in a three-player game the attacker splitting its
/// attack still makes one trigger, and only attackers aimed at Tomik's
/// controller or their planeswalkers count.
#[test]
fn tomik_in_multiplayer_counts_only_attackers_aimed_at_its_controller() {
    let p2 = PlayerId(2);
    let mut split = board(3, P1, "Tomik, Wielder of Law", TOMIK, 3);
    let lives = [
        split.runner.life(P0),
        split.runner.life(P1),
        split.runner.life(p2),
    ];
    split
        .runner
        .declare_attackers(&[
            (split.attackers[0], AttackTarget::Player(P1)),
            (split.attackers[1], AttackTarget::Player(P1)),
            (split.attackers[2], AttackTarget::Player(p2)),
        ])
        .expect("split the attack between both opponents");
    assert_eq!(observer_triggers(&split.runner, split.observer), 1);
    split.runner.advance_until_stack_empty();
    assert_eq!(
        [
            split.runner.life(P0),
            split.runner.life(P1),
            split.runner.life(p2)
        ],
        [lives[0] - 3, lives[1], lives[2]]
    );

    let mut elsewhere = board(3, P1, "Tomik, Wielder of Law", TOMIK, 2);
    let p0_life = elsewhere.runner.life(P0);
    let attacks: Vec<_> = elsewhere
        .attackers
        .iter()
        .map(|&id| (id, AttackTarget::Player(p2)))
        .collect();
    elsewhere
        .runner
        .declare_attackers(&attacks)
        .expect("attack the other opponent");
    assert_eq!(observer_triggers(&elsewhere.runner, elsewhere.observer), 0);
    elsewhere.runner.advance_until_stack_empty();
    assert_eq!(elsewhere.runner.life(P0), p0_life);
}

/// CR 508.3c: Mangara shares Tomik's head and draws exactly once.
#[test]
fn mangara_draws_once_for_an_attack_on_you_and_your_planeswalker() {
    let mut b = board(2, P1, "Mangara, the Diplomat", MANGARA, 3);
    let hand_before = b.runner.state().players[usize::from(P1.0)].hand.len();
    b.runner
        .declare_attackers(&[
            (b.attackers[0], AttackTarget::Player(P1)),
            (b.attackers[1], AttackTarget::Planeswalker(b.planeswalker)),
            (b.attackers[2], AttackTarget::Player(P1)),
        ])
        .expect("attack you and your planeswalker");
    assert_eq!(observer_triggers(&b.runner, b.observer), 1);
    b.runner.advance_until_stack_empty();
    assert_eq!(
        b.runner.state().players[usize::from(P1.0)].hand.len(),
        hand_before + 1
    );
    assert_eq!(b.runner.state().objects[&b.draw].zone, Zone::Hand);
}

/// CR 508.3c + CR 506.4: "that opponent" is the player who declared the
/// attack. Tomik's controller stealing an attacker in response removes it from
/// combat, but the declaring player still loses the life.
#[test]
fn tomik_drains_the_declaring_player_after_an_attacker_changes_control() {
    let mut b = board(2, P1, "Tomik, Wielder of Law", TOMIK, 3);
    let (p0_life, p1_life) = (b.runner.life(P0), b.runner.life(P1));
    let attacks: Vec<_> = b
        .attackers
        .iter()
        .map(|&id| (id, AttackTarget::Player(P1)))
        .collect();
    b.runner
        .declare_attackers(&attacks)
        .expect("attack with three creatures");
    assert_eq!(observer_triggers(&b.runner, b.observer), 1);

    // The first declared attacker is the one taken, so a live
    // first-attacker lookup would name Tomik's controller.
    b.runner
        .act(GameAction::PassPriority)
        .expect("the attacking player passes");
    assert!(matches!(
        b.runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    b.runner
        .cast(b.steal)
        .target_object(b.attackers[0])
        .commit();
    b.runner.resolve_top();
    assert_eq!(b.runner.state().objects[&b.attackers[0]].controller, P1);

    b.runner.advance_until_stack_empty();
    assert_eq!(b.runner.life(P0), p0_life - 3);
    assert_eq!(b.runner.life(P1), p1_life);
}
