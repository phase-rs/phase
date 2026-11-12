//! CR 614.6 + CR 120.8: combat bookkeeping records only damage that was
//! actually dealt to the player that actually received it.
//!
//! The Phase C fold used to key the combat-hit record (the Ring level-four
//! trigger's "dealt combat damage to a player" aggregate) and the CR 903.10a
//! commander-damage tally on the ASSIGNED target, so a hit that Szadek
//! replaced (never happened) or that Ancient Adamantoise redirected away from
//! the player still counted for that player.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::ReplacementMode;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const SZADEK_ORACLE: &str = "If Szadek would deal combat damage to a player, instead put \
that many +1/+1 counters on Szadek and that player mills that many cards.";
const ADAMANTOISE: &str = "Vigilance, ward {3}\nDamage isn't removed from this creature during cleanup steps.\nAll damage that would be dealt to you and other permanents you control is dealt to this creature instead.\nWhen this creature dies, exile it and create ten tapped Treasure tokens.";

/// Declare `attacker` at P1, make it P0's Ring-bearer at Ring level four (set
/// after the attack so the level-two loot trigger stays out of the way), and
/// play combat through to the end, resolving the Ring trigger if it fires.
/// `answer` is applied to every replacement choice raised on the way.
fn attack_as_level_four_ring_bearer(
    runner: &mut GameRunner,
    attacker: ObjectId,
    answer: Option<usize>,
) {
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare attacker");
    runner.state_mut().ring_level.insert(P0, 4);
    runner.state_mut().ring_bearer.insert(P0, Some(attacker));
    for _ in 0..64 {
        let past_combat = matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        );
        if past_combat && runner.state().stack.is_empty() {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::ReplacementChoice { .. } => {
                runner
                    .act(GameAction::ChooseReplacement {
                        index: answer.expect("no replacement choice expected"),
                    })
                    .expect("answer replacement choice");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    panic!("combat did not settle: {:?}", runner.state().waiting_for);
}

fn commander_damage_to(runner: &GameRunner, player: PlayerId) -> u32 {
    runner
        .state()
        .commander_damage
        .iter()
        .filter(|entry| entry.player == player)
        .map(|entry| entry.damage)
        .sum()
}

/// CR 701.54c: the Ring-bearer's level-four trigger needs combat damage dealt
/// to a player. Control: a plain 3/3 bearer connects and each opponent loses 3
/// on top of the 3 damage.
#[test]
fn ring_level_four_triggers_for_an_unreplaced_hit() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Bear", 3, 3).id();
    let mut runner = scenario.build();
    attack_as_level_four_ring_bearer(&mut runner, bear, None);
    assert_eq!(runner.life(P1), 20 - 3 - 3, "3 damage, then 3 life loss");
}

/// CR 614.6 + CR 120.8: Szadek's replaced hit never happens, so the Ring
/// bearer dealt no combat damage and the level-four trigger does not fire.
#[test]
fn ring_level_four_does_not_fire_for_a_replaced_hit() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let szadek = scenario
        .add_creature_from_oracle(P0, "Szadek, Lord of Secrets", 5, 5, SZADEK_ORACLE)
        .id();
    scenario.with_library_top(P1, &["C1", "C2", "C3", "C4", "C5", "C6"]);
    let mut runner = scenario.build();
    attack_as_level_four_ring_bearer(&mut runner, szadek, None);
    assert_eq!(runner.life(P1), 20, "no damage and no Ring life loss");
}

/// The same board through the replacement-choice resume: an optional
/// substitution, once declined, deals the damage (the Ring fires); once
/// accepted, the hit is replaced (it does not).
#[test]
fn ring_level_four_follows_an_answered_optional_substitution() {
    for accept in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let szadek = scenario
            .add_creature_from_oracle(P0, "Szadek, Lord of Secrets", 5, 5, SZADEK_ORACLE)
            .id();
        scenario.with_library_top(P1, &["C1", "C2", "C3", "C4", "C5", "C6"]);
        let mut runner = scenario.build();
        {
            let optional = ReplacementMode::Optional { decline: None };
            let obj = runner.state_mut().objects.get_mut(&szadek).expect("Szadek");
            for index in 0..obj.replacement_definitions.len() {
                obj.replacement_definitions[index].mode = optional.clone();
            }
            let mut base = (*obj.base_replacement_definitions).clone();
            for definition in &mut base {
                definition.mode = optional.clone();
            }
            obj.base_replacement_definitions = std::sync::Arc::new(base);
        }
        attack_as_level_four_ring_bearer(&mut runner, szadek, Some(if accept { 0 } else { 1 }));
        let expected = if accept { 20 } else { 20 - 5 - 3 };
        assert_eq!(runner.life(P1), expected, "accept={accept}");
    }
}

/// CR 903.10a + CR 614.6: Ancient Adamantoise redirects the commander's hit
/// onto itself, so P1 was dealt no combat damage by the commander.
#[test]
fn redirected_commander_hit_is_not_commander_damage_to_the_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let commander = {
        let mut builder = scenario.add_creature(P0, "Commander", 5, 5);
        builder.commander();
        builder.id()
    };
    let turtle = scenario
        .add_creature_from_oracle(P1, "Ancient Adamantoise", 8, 20, ADAMANTOISE)
        .id();
    let mut runner = scenario.build();
    attack_as_level_four_ring_bearer(&mut runner, commander, None);
    assert_eq!(runner.life(P1), 20, "the hit never reached the player");
    assert_eq!(runner.state().objects[&turtle].damage_marked, 5);
    assert_eq!(
        commander_damage_to(&runner, P1),
        0,
        "redirected damage is not commander damage to the player"
    );
}

/// Control: with no redirect, the same commander hit is booked.
#[test]
fn unredirected_commander_hit_is_commander_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let commander = {
        let mut builder = scenario.add_creature(P0, "Commander", 5, 5);
        builder.commander();
        builder.id()
    };
    let mut runner = scenario.build();
    attack_as_level_four_ring_bearer(&mut runner, commander, None);
    assert_eq!(commander_damage_to(&runner, P1), 5);
}

const RHOX_ORACLE: &str = "Lifelink (Damage dealt by this creature also causes you to gain that much life.)\nIf you would gain life, you gain twice that much life instead.";
const ANGEL_OF_VITALITY_ORACLE: &str = "Flying\nIf you would gain life, you gain that much life plus 1 instead.\nThis creature gets +2/+2 as long as you have 25 or more life.";

/// CR 702.15e + CR 616.1 + CR 101.4: two lifelink creatures trade blows in a
/// blocked combat. The blocker's damage to the ACTIVE player's creature would
/// sort first by damaged seat, but each lifelink gain is its own event whose
/// replacement order belongs to the GAINING player, so the active player is
/// asked first, then the nonactive player. Rhox Faithmender (x2) and Angel of
/// Vitality (+1) make the order material: 2 life gains 6 (Angel first) or 5
/// (Rhox first). Each seat's life reflects its own answer, each gain once.
#[test]
fn lifelink_gain_choices_follow_the_gaining_seats_apnap_order() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario
        .add_creature_from_oracle(P0, "Rhox Faithmender", 2, 5, RHOX_ORACLE)
        .lifelink()
        .id();
    let blocker = scenario
        .add_creature_from_oracle(P1, "Rhox Faithmender", 2, 5, RHOX_ORACLE)
        .lifelink()
        .id();
    let p0_angel = scenario
        .add_creature_from_oracle(P0, "Angel of Vitality", 2, 2, ANGEL_OF_VITALITY_ORACLE)
        .id();
    let p1_angel = scenario
        .add_creature_from_oracle(P1, "Angel of Vitality", 2, 2, ANGEL_OF_VITALITY_ORACLE)
        .id();
    let p0_rhox = attacker;
    let p1_rhox = blocker;
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare attacker");
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ) {
            break;
        }
        runner.act(GameAction::PassPriority).expect("to blockers");
    }
    runner
        .declare_blockers(&[(blocker, attacker)])
        .expect("declare blockers");
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            break;
        }
        runner.act(GameAction::PassPriority).expect("to damage");
    }

    let mut answers = vec![(P0, p0_angel), (P1, p1_rhox)];
    let _ = (p0_rhox, p1_angel);
    for (chooser, source) in answers.drain(..) {
        let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = runner.state().waiting_for.clone()
        else {
            panic!(
                "expected {chooser:?} gain choice, got {:?} phase {:?} life {} {} dmg {} {} lifelink {}",
                runner.state().waiting_for,
                runner.state().phase,
                runner.life(P0),
                runner.life(P1),
                runner.state().objects[&p0_rhox].damage_marked,
                runner.state().objects[&p1_rhox].damage_marked,
                runner.state().objects[&p0_rhox].has_keyword(&engine::types::keywords::Keyword::Lifelink)
            );
        };
        assert_eq!(
            player, chooser,
            "CR 616.1 + CR 101.4: APNAP of the gaining seat"
        );
        let index = candidates
            .iter()
            .position(|candidate| candidate.source_id == source)
            .expect("candidate present");
        runner
            .act(GameAction::ChooseReplacement { index })
            .expect("answer gain order");
    }
    for _ in 0..16 {
        if matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ) {
            break;
        }
        runner.act(GameAction::PassPriority).expect("finish combat");
    }
    // P0: Angel first (2 + 1) x 2 = 6. P1: Rhox first 2 x 2 + 1 = 5.
    assert_eq!(runner.life(P0), 26, "P0's own answer, gained once");
    assert_eq!(runner.life(P1), 25, "P1's own answer, gained once");
}
