//! Regressions for issue #3248 (Steel Hellkite) and the Hellkite half of #7885.
//!
//! https://github.com/phase-rs/phase/issues/3248
//! https://github.com/phase-rs/phase/issues/7885
//!
//! "{X}: Destroy each nonland permanent with mana value X whose controller was
//! dealt combat damage by this creature this turn." The controller damage
//! look-back was silently dropped, so the ability destroyed every MV-X nonland
//! without any combat damage. It must destroy ONLY permanents whose controller
//! was dealt combat damage by the Hellkite itself.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::AbilityCost;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const STEEL_HELLKITE_ORACLE: &str = "Flying\n{2}: This creature gets +1/+0 until end of turn.\n{X}: Destroy each nonland permanent with mana value X whose controller was dealt combat damage by this creature this turn. Activate only once each turn.";

// Verbatim Ray of Command rules text (mirrors departed_recipient_control_exchange.rs).
const RAY_OF_COMMAND: &str = "Untap target creature an opponent controls and gain control of it \
until end of turn. That creature gains haste until end of turn. When you lose control of the \
creature, tap it.";

/// The `{X}` destroy ability: the Hellkite ability whose mana cost carries X.
fn x_ability_index(runner: &GameRunner, hellkite: ObjectId) -> usize {
    runner.state().objects[&hellkite]
        .abilities
        .iter()
        .position(|a| {
            matches!(
                &a.cost,
                Some(AbilityCost::Mana { cost })
                    if matches!(cost, ManaCost::Cost { shards, .. }
                        if shards.contains(&ManaCostShard::X))
            )
        })
        .expect("Hellkite has an {X} activated ability")
}

/// A vanilla creature with the given mana value (generic-only cost).
fn add_mv_creature(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    mana_value: u32,
) -> ObjectId {
    scenario
        .add_creature(player, name, 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: mana_value,
        })
        .id()
}

/// Fund mana on the LIVE state (post-build). Scenario-time funding would empty
/// across combat steps (CR 106.4), so combat tests fund after damage.
fn fund_mana_live(runner: &mut GameRunner, player: PlayerId, colorless: usize) {
    for _ in 0..colorless {
        runner.state_mut().add_mana_to_pool(
            player,
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        );
    }
}

/// Drive the REAL combat pipeline: `attacker` attacks `defender` unblocked, deals
/// combat damage, and the game reaches post-combat priority. Priority passes cannot
/// cross DeclareAttackers, so the harness advances phase-to-phase explicitly. The
/// DeclareBlockers prompt is answered with no blocks when it appears; boards with
/// no legal block (flying vs non-reach) skip the prompt and the loop passes on.
fn deal_unblocked_combat_damage(runner: &mut GameRunner, attacker: ObjectId, defender: PlayerId) {
    runner.advance_to_combat();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ),
        "expected the DeclareAttackers prompt, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(defender))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");
    // Attackers-declared priority, blockers prompt (if any), blockers-declared
    // priority, combat damage (CR 510), end of combat: drive until post-combat
    // priority. Bounded; works for any player count.
    for _ in 0..24 {
        if matches!(runner.state().phase, Phase::PostCombatMain)
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass should succeed");
            }
            // CR 509.1: the defending player declares no blockers.
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("empty DeclareBlockers should succeed");
            }
            other => panic!("unexpected prompt driving through combat: {other:?}"),
        }
    }
    assert!(
        matches!(runner.state().phase, Phase::PostCombatMain)
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "expected PostCombatMain priority after combat damage, got {:?} / {:?}",
        runner.state().phase,
        runner.state().waiting_for
    );
}

/// CR 608.2c + CR 120.2a: with combat damage dealt, X=2 destroys the
/// damaged opponent's MV-2 nonlands — and nothing else. Pre-fix the filter was
/// absent, so the controller's own MV-2 nonland was destroyed too.
#[test]
fn hellkite_destroys_only_damaged_opponents_mana_value_x_nonlands() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let hellkite = scenario
        .add_creature_from_oracle(P0, "Steel Hellkite", 5, 5, STEEL_HELLKITE_ORACLE)
        .id();
    // THE discriminating survivor: ours, MV 2 — destroyed pre-fix.
    let own_two = add_mv_creature(&mut scenario, P0, "Own Martyr", 2);
    let victim_two = add_mv_creature(&mut scenario, P1, "Doomed Martyr", 2);
    let survivor_three = add_mv_creature(&mut scenario, P1, "Big Martyr", 3);

    let mut runner = scenario.build();
    deal_unblocked_combat_damage(&mut runner, hellkite, P1);

    // Reach-guard: the Hellkite really did deal combat damage to P1 (5/5).
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        15,
        "Hellkite must deal 5 combat damage to P1"
    );
    fund_mana_live(&mut runner, P0, 4);

    let index = x_ability_index(&runner, hellkite);
    let outcome = runner.activate(hellkite, index).x(2).resolve();

    outcome.assert_zone(&[victim_two], Zone::Graveyard);
    outcome.assert_zone(&[own_two], Zone::Battlefield);
    outcome.assert_zone(&[survivor_three], Zone::Battlefield);
    outcome.assert_zone(&[hellkite], Zone::Battlefield);
    assert_eq!(
        outcome.mana_pool_total(P0),
        2,
        "X=2 must be paid from the 4-mana pool"
    );
    assert!(
        outcome.state().stack.is_empty(),
        "the activation must have finished resolving"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "expected Priority after the activation resolves, got {:?}",
        outcome.final_waiting_for()
    );
}

/// No combat damage ⇒ the activation resolves but destroys nothing. The restriction
/// is on the TARGET SET ("destroy each … whose …"), not on activation: paying X
/// with no qualifying permanents is legal and simply does nothing.
#[test]
fn hellkite_without_combat_damage_destroys_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let hellkite = scenario
        .add_creature_from_oracle(P0, "Steel Hellkite", 5, 5, STEEL_HELLKITE_ORACLE)
        .id();
    let own_two = add_mv_creature(&mut scenario, P0, "Own Martyr", 2);
    let victim_two = add_mv_creature(&mut scenario, P1, "Doomed Martyr", 2);

    let mut runner = scenario.build();
    // No combat at all — fund and activate straight from pre-combat priority.
    fund_mana_live(&mut runner, P0, 4);
    let index = x_ability_index(&runner, hellkite);
    let outcome = runner.activate(hellkite, index).x(2).resolve();

    // Reach-guards: the activation RAN (X paid, resolved to Priority) and the
    // would-be victims prove it destroyed nothing (pre-fix both were destroyed).
    assert_eq!(
        outcome.mana_pool_total(P0),
        2,
        "X=2 must be paid even when nothing qualifies"
    );
    outcome.assert_zone(&[own_two], Zone::Battlefield);
    outcome.assert_zone(&[victim_two], Zone::Battlefield);
    assert!(
        outcome.state().stack.is_empty(),
        "the activation must have finished resolving"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "expected Priority after the activation resolves, got {:?}",
        outcome.final_waiting_for()
    );
}

/// Source discrimination: combat damage dealt ONLY by another creature
/// does not qualify anyone. The look-back names damage "by this creature".
#[test]
fn hellkite_ignores_combat_damage_dealt_by_other_sources() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let hellkite = scenario
        .add_creature_from_oracle(P0, "Steel Hellkite", 5, 5, STEEL_HELLKITE_ORACLE)
        .id();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let victim_two = add_mv_creature(&mut scenario, P1, "Doomed Martyr", 2);

    let mut runner = scenario.build();
    // Only the Bears attack — the Hellkite deals no damage this turn.
    deal_unblocked_combat_damage(&mut runner, bears, P1);

    // Reach-guard: the damage ledger is NON-empty (Bears dealt 2), so "destroys
    // nothing" proves source discrimination, not an empty ledger.
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        18,
        "Bears must deal 2 combat damage to P1"
    );
    fund_mana_live(&mut runner, P0, 4);

    let index = x_ability_index(&runner, hellkite);
    let outcome = runner.activate(hellkite, index).x(2).resolve();

    outcome.assert_zone(&[victim_two], Zone::Battlefield);
    assert_eq!(
        outcome.mana_pool_total(P0),
        2,
        "X=2 must be paid even when nothing qualifies"
    );
    assert!(
        outcome.state().stack.is_empty(),
        "the activation must have finished resolving"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "expected Priority after the activation resolves, got {:?}",
        outcome.final_waiting_for()
    );
}

/// F1 (PR #10015 review): the damaged player is NOT excluded after gaining
/// control of Steel Hellkite. CR 608.2c applies "the rules of English to the
/// text", and the text names the damaged controller with no opponent
/// restriction. P0's Hellkite damages P1; P1 steals it with a production-path
/// Ray of Command cast, then activates its unused ability with X=6. The
/// Hellkite (MV 6, controlled by damaged P1) must destroy itself. Pre-fix the
/// opponent-relative predicate excluded P1 and the Hellkite survived.
#[test]
fn hellkite_destroys_itself_after_damaged_player_gains_control() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // MV 6, like the printed card, so X=6 can destroy the Hellkite itself.
    let hellkite = scenario
        .add_creature_from_oracle(P0, "Steel Hellkite", 5, 5, STEEL_HELLKITE_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 6,
        })
        .id();
    let ray = scenario
        .add_spell_to_hand_from_oracle(P1, "Ray of Command", true, RAY_OF_COMMAND)
        .id();
    let doomed_titan = add_mv_creature(&mut scenario, P1, "Doomed Titan", 6);
    let own_titan = add_mv_creature(&mut scenario, P0, "Own Titan", 6);
    let wrong_mv = add_mv_creature(&mut scenario, P1, "Small Martyr", 5);

    let mut runner = scenario.build();
    deal_unblocked_combat_damage(&mut runner, hellkite, P1);

    // Reach-guard: the Hellkite really did deal combat damage to P1 (5/5).
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        15,
        "Hellkite must deal 5 combat damage to P1"
    );

    // P1's turn to act: pass priority until P1 has it, then cast Ray
    // production-path targeting the Hellkite. The cast is uncharged in this
    // harness (mirrors departed_recipient_control_exchange, which funds
    // nothing for it); only the X activation below is paid.
    for _ in 0..8 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass should succeed");
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "expected P1 priority to cast Ray, got {:?}",
        runner.state().waiting_for
    );
    let _ = runner.cast(ray).target_objects(&[hellkite]).commit();
    for _ in 0..10 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass should succeed");
    }
    // Reach-guard: the steal really happened through the cast.
    assert_eq!(
        runner.state().objects[&hellkite].controller,
        P1,
        "Ray of Command must give P1 control of the Hellkite"
    );

    // P1 activates the unused ability with X=6. Priority again to P1 first.
    for _ in 0..8 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass should succeed");
    }
    fund_mana_live(&mut runner, P1, 6);
    let index = x_ability_index(&runner, hellkite);
    let outcome = runner.activate(hellkite, index).x(6).resolve();

    // THE discriminating assert: the Hellkite destroys itself.
    outcome.assert_zone(&[hellkite], Zone::Graveyard);
    outcome.assert_zone(&[doomed_titan], Zone::Graveyard);
    outcome.assert_zone(&[own_titan], Zone::Battlefield);
    outcome.assert_zone(&[wrong_mv], Zone::Battlefield);
    assert_eq!(
        outcome.mana_pool_total(P1),
        0,
        "X=6 must be paid from P1's 6-mana pool"
    );
    assert!(
        outcome.state().stack.is_empty(),
        "the activation must have finished resolving"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "expected Priority after the activation resolves, got {:?}",
        outcome.final_waiting_for()
    );
}

/// Multiplayer: only the damaged opponent's permanents qualify. P1 was dealt
/// combat damage by the Hellkite; P2 was not.
#[test]
fn hellkite_destroys_only_the_damaged_opponents_permanents_in_multiplayer() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let hellkite = scenario
        .add_creature_from_oracle(P0, "Steel Hellkite", 5, 5, STEEL_HELLKITE_ORACLE)
        .id();
    let victim_two = add_mv_creature(&mut scenario, P1, "Doomed Martyr", 2);
    let survivor_two = add_mv_creature(&mut scenario, P2, "Spared Martyr", 2);

    let mut runner = scenario.build();
    deal_unblocked_combat_damage(&mut runner, hellkite, P1);

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        15,
        "Hellkite must deal 5 combat damage to P1"
    );
    assert_eq!(
        runner.state().players[P2.0 as usize].life,
        20,
        "P2 must be untouched"
    );
    fund_mana_live(&mut runner, P0, 4);

    let index = x_ability_index(&runner, hellkite);
    let outcome = runner.activate(hellkite, index).x(2).resolve();

    outcome.assert_zone(&[victim_two], Zone::Graveyard);
    outcome.assert_zone(&[survivor_two], Zone::Battlefield);
    assert!(
        outcome.state().stack.is_empty(),
        "the activation must have finished resolving"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "expected Priority after the activation resolves, got {:?}",
        outcome.final_waiting_for()
    );
}
