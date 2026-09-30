//! Production-pipeline witnesses for the parser velocity sprint 1 classes. Each test
//! builds the card from its VERBATIM Oracle text and drives the real engine, so a
//! revert of the parser change it covers turns it red:
//!
//! * `SourceAttackingAlone` vs an attached-subject gate (Gutter Shortcut, Dream Prowler)
//! * a targeted `payer: Player` unless-payment repeated by "Repeat this process once"
//!   (Remorseless Punishment)
//! * the revealed-or-controlled-as-cast gate (Draconic Roar)

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::{GameAction, UnlessCostBranch};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

use engine::game::game_object::AttachTarget;

const GUTTER_SHORTCUT: &str =
    "Enchant creature\nEnchanted creature can't be blocked as long as it's attacking alone.";
const DREAM_PROWLER: &str = "This creature can't be blocked as long as it's attacking alone.";

/// Attack P1 with `attackers` and report whether P1's creature `blocker` may legally
/// block `attackers[0]` (CR 509.1b). An attacker no creature can block skips the
/// declare-blockers prompt entirely, so a missing prompt reads as "may not block".
fn blocker_may_block_first_attacker(
    runner: &mut GameRunner,
    attackers: &[ObjectId],
    blocker: ObjectId,
) -> bool {
    runner.pass_both_players();
    let attacks: Vec<_> = attackers
        .iter()
        .map(|&id| (id, AttackTarget::Player(P1)))
        .collect();
    runner
        .declare_attackers(&attacks)
        .expect("declare attackers");
    assert_eq!(
        runner.state().combat.as_ref().map(|c| c.attackers.len()),
        Some(attackers.len()),
        "reach guard: every attacker must be declared"
    );
    runner.pass_both_players();
    matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) && runner.declare_blockers(&[(blocker, attackers[0])]).is_ok()
}

/// Control for the two tests below: an ordinary lone attacker IS blockable, so the
/// "no declare-blockers prompt" observation is caused by the unblockable static.
#[test]
fn control_a_plain_lone_attacker_is_blockable() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Plain Attacker", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Blocker", 2, 2).id();
    let mut runner = scenario.build();
    assert!(blocker_may_block_first_attacker(
        &mut runner,
        &[attacker],
        blocker
    ));
}

/// CR 506.5 + CR 509.1b + CR 611.3a: Gutter Shortcut's "it" is the ENCHANTED creature.
/// The Aura is never an attacker, so a gate on the Aura itself never opens and the
/// creature stays blockable forever (the pre-fix "supported but never works").
#[test]
fn gutter_shortcut_makes_the_enchanted_lone_attacker_unblockable() {
    for (extra_attacker, lone) in [(false, true), (true, false)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let host = scenario.add_creature(P0, "Host", 2, 2).id();
        let mut attackers = vec![host];
        if extra_attacker {
            attackers.push(scenario.add_creature(P0, "Second Attacker", 2, 2).id());
        }
        let blocker = scenario.add_creature(P1, "Blocker", 2, 2).id();
        let aura = scenario
            .add_enchantment_from_oracle(P0, "Gutter Shortcut", GUTTER_SHORTCUT)
            .with_subtypes(vec!["Aura"])
            .id();
        let mut runner = scenario.build();
        runner
            .state_mut()
            .objects
            .get_mut(&aura)
            .unwrap()
            .attached_to = Some(AttachTarget::Object(host));
        runner
            .state_mut()
            .objects
            .get_mut(&host)
            .unwrap()
            .attachments
            .push(aura);
        runner.state_mut().layers_dirty.mark_full();

        let may_block = blocker_may_block_first_attacker(&mut runner, &attackers, blocker);
        assert_eq!(
            may_block, !lone,
            "extra attacker = {extra_attacker}: the enchanted creature is unblockable only \
             while it attacks alone (CR 506.5)"
        );
    }
}

/// The self-source shape (Dream Prowler) keeps gating on its own source and is
/// unaffected by the attached-subject rebinding.
#[test]
fn dream_prowler_is_unblockable_only_while_attacking_alone() {
    for (extra_attacker, lone) in [(false, true), (true, false)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let prowler = scenario
            .add_creature_from_oracle(P0, "Dream Prowler", 1, 1, DREAM_PROWLER)
            .id();
        let mut attackers = vec![prowler];
        if extra_attacker {
            attackers.push(scenario.add_creature(P0, "Second Attacker", 2, 2).id());
        }
        let blocker = scenario.add_creature(P1, "Blocker", 2, 2).id();
        let mut runner = scenario.build();

        let may_block = blocker_may_block_first_attacker(&mut runner, &attackers, blocker);
        assert_eq!(may_block, !lone, "extra attacker = {extra_attacker}");
    }
}

const REMORSELESS_PUNISHMENT: &str = "Target opponent loses 5 life unless that player discards two cards or sacrifices a creature or planeswalker of their choice. Repeat this process once.";

/// CR 608.2c + CR 118.12a: "Repeat this process once" re-follows the whole process, so
/// the TARGETED opponent (a `payer: Player`, no `player_scope`) is offered the unless
/// choice on EACH of the two repetitions, and declining both costs 10 life.
#[test]
fn remorseless_punishment_offers_the_targeted_opponent_the_choice_twice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Remorseless Punishment", false, REMORSELESS_PUNISHMENT)
        .id();
    scenario.add_creature(P1, "Sacrificial Lamb", 1, 1);
    scenario.add_card_to_hand(P1, "Card A");
    scenario.add_card_to_hand(P1, "Card B");
    let mut runner = scenario.build();
    let life_before = runner.state().players[P1.0 as usize].life;

    let outcome = runner.cast(spell).target_player(P1).resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::UnlessPaymentChooseCost { player: P1, .. }
        ),
        "first repetition must ask P1, got {:?}",
        outcome.final_waiting_for()
    );
    for repetition in 1..=2 {
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::UnlessPaymentChooseCost { player: P1, .. }
            ),
            "repetition {repetition} must offer P1 the unless choice, got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::ChooseUnlessCostBranch {
                choice: UnlessCostBranch::Decline,
            })
            .expect("decline the unless cost");
    }
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        life_before - 10,
        "declining both repetitions loses 5 life each"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

const DRACONIC_ROAR: &str = "As an additional cost to cast this spell, you may reveal a Dragon card from your hand.\nDraconic Roar deals 3 damage to target creature. If you revealed a Dragon card or controlled a Dragon as you cast this spell, Draconic Roar deals 3 damage to that creature's controller.";

/// (dragon_in_hand, dragon_on_battlefield, reveal_it) -> P1 life delta.
fn draconic_roar_life_delta(dragon_in_hand: bool, dragon_controlled: bool, reveal: bool) -> i32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Draconic Roar", true, DRACONIC_ROAR)
        .id();
    let victim = scenario.add_creature(P1, "Victim", 0, 20).id();
    if dragon_controlled {
        scenario
            .add_creature(P0, "Shivan Dragon", 5, 5)
            .with_subtypes(vec!["Dragon"]);
    }
    let hand_dragon = dragon_in_hand.then(|| {
        scenario
            .add_creature_to_hand_from_oracle(P0, "Hand Dragon", 5, 5, "")
            .with_subtypes(vec!["Dragon"])
            .id()
    });
    let mut runner = scenario.build();
    let cast = runner.cast(spell).target_object(victim);
    let outcome = match (reveal, hand_dragon) {
        (true, Some(dragon)) => cast.accept_optional().pay_cost_with(&[dragon]).resolve(),
        _ => cast.decline_optional().resolve(),
    };
    outcome.life_delta(P1)
}

/// CR 601.2b + CR 601.2h + CR 608.2c: the extra 3 damage happens when a Dragon was
/// revealed as the optional additional cost OR a Dragon was controlled as the spell was
/// cast, and does not happen with neither. Both halves of the `Or` are exercised.
#[test]
fn draconic_roar_extra_damage_needs_a_revealed_or_controlled_dragon() {
    assert_eq!(
        draconic_roar_life_delta(false, true, false),
        -3,
        "a controlled Dragon at cast time opens the gate"
    );
    assert_eq!(
        draconic_roar_life_delta(true, false, true),
        -3,
        "a revealed Dragon card opens the gate"
    );
    assert_eq!(
        draconic_roar_life_delta(false, false, false),
        0,
        "with neither, only the creature takes damage"
    );
    assert_eq!(
        draconic_roar_life_delta(true, false, false),
        0,
        "a Dragon in hand that is NOT revealed does not open the gate"
    );
}
