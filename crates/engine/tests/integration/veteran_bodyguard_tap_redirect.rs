//! GameRunner integration regression for Veteran Bodyguard's tap-gated damage
//! redirection (CR 604.2 + CR 614.9 + CR 509.1h).
//!
//! Oracle text under test (verbatim, Scryfall-verified):
//!   "As long as this creature is untapped, all damage that would be dealt to
//!    you by unblocked creatures is dealt to this creature instead."
//!
//! Two cases:
//!  1. Untapped — the redirect fires: an unblocked attacker's damage lands on
//!     Veteran Bodyguard instead of the defending player.
//!  2. Tapped — the redirect does NOT fire: the same unblocked attacker's
//!     damage lands on the defending player normally.
//!
//! The Bodyguard is controlled by the defending player (P1); P0 (the active
//! player) attacks P1 with a 3/3, so "you" in the Oracle text — the Bodyguard's
//! controller, P1 — is the damaged player whose damage redirects to itself.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;

use super::rules::run_combat;

/// Verbatim Veteran Bodyguard redirection text (Scryfall-verified).
const VETERAN_BODYGUARD_TEXT: &str = "As long as this creature is untapped, all damage that \
    would be dealt to you by unblocked creatures is dealt to this creature instead.";

/// Drive the game from the current state (at or before DeclareAttackers) through
/// the end-of-combat step. `attacker_player` declares `attacker` against
/// `defend_player` unblocked; every other prompt is auto-passed / no-op. This is
/// a simplified `weeping_angel_combat_prevention::run_combat` with no blocker,
/// since both cases exercise the "unblocked" axis.
fn run_combat_unblocked(
    runner: &mut engine::game::scenario::GameRunner,
    attacker_player: engine::types::player::PlayerId,
    attacker: engine::types::identifiers::ObjectId,
    defend_player: engine::types::player::PlayerId,
) {
    let mut attacked = false;

    for _ in 0..400 {
        match runner.state().phase {
            Phase::EndCombat | Phase::PostCombatMain => break,
            _ => {}
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::OrderTriggers { .. } => {
                if runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .is_err()
                {
                    break;
                }
            }
            WaitingFor::DeclareAttackers { player, .. } if !attacked => {
                attacked = true;
                let attacks = if player == attacker_player {
                    vec![(attacker, AttackTarget::Player(defend_player))]
                } else {
                    vec![]
                };
                if runner.declare_attackers(&attacks).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareAttackers { .. } => {
                if runner.declare_attackers(&[]).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareBlockers { .. } => {
                if runner.declare_blockers(&[]).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
}

/// CR 604.2 + CR 614.9 + CR 509.1h: while Veteran Bodyguard is untapped, all
/// damage an unblocked creature would deal to its controller is dealt to the
/// Bodyguard instead — so the defending player (the Bodyguard's controller)
/// takes no life loss from the unblocked attacker.
///
/// This asserts the in-scope behavior of this fix: while UNTAPPED, the shield
/// FIRES and the controller is protected. It is the positive reach-guard that
/// makes the tapped negative below non-vacuous (it proves the shield actually
/// reaches the redirect arm for this fixture).
///
/// CR 614.9: per the card, the redirected damage is *dealt to the Bodyguard*
/// (marking damage on it). `parse_replacement_line` emits `shield_kind =
/// ShieldKind::Prevention { amount: PreventionAmount::All }` with
/// `redirect_target: SelfRef`, and `game::replacement::damage_done_applier`'s
/// Branch 2 now reads `redirect_target` and routes the whole
/// "damage is dealt to X instead" class (Palisade Giant, Veteran Bodyguard,
/// Weathered Bodyguards) through the shared CR 614.9 redirection mechanics —
/// so the damage is redirected onto the Bodyguard, not merely prevented. This
/// test therefore asserts BOTH halves: the controller is protected AND the
/// unblocked attacker's damage lands on the Bodyguard.
#[test]
fn veteran_bodyguard_untapped_protects_controller_from_unblocked_damage() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // P1 (the defending player) controls Veteran Bodyguard, a 2/5 with the
    // tap-gated redirection static ability. Untapped by builder default.
    let bodyguard = scenario
        .add_creature_from_oracle(P1, "Veteran Bodyguard", 2, 5, VETERAN_BODYGUARD_TEXT)
        .id();

    // P0 (the active player) controls a 3/3 vanilla attacker.
    let attacker = scenario.add_creature(P0, "Charging Bear", 3, 3).id();

    let mut runner = scenario.build();
    let p1_life_before = runner.life(P1);

    runner.advance_to_combat();
    run_combat_unblocked(&mut runner, P0, attacker, P1);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        p1_life_before,
        "P1 must take NO life loss — while the Veteran Bodyguard is untapped, the \
         unblocked attacker's damage to P1 is redirected away (CR 604.2 + CR 614.9)"
    );
    // CR 614.9: the redirected damage is DEALT to the Bodyguard, not prevented.
    // Revert guard for Branch 2's redirect check — without it this is 0.
    assert_eq!(
        runner.state().objects[&bodyguard].damage_marked,
        3,
        "the unblocked attacker's 3 damage must be redirected onto the untapped Veteran Bodyguard"
    );
}

/// CR 604.2: while Veteran Bodyguard is TAPPED, its "as long as this creature is
/// untapped" gate is false, so the redirect does NOT fire. The unblocked
/// attacker's damage lands on the defending player normally.
///
/// Discriminating: this test fails if the leading "as long as ... untapped" gate
/// is dropped (the redirect would fire unconditionally, P1 would take 0 damage
/// and the Bodyguard 3). It is the revert-guard for the condition half of the fix.
#[test]
fn veteran_bodyguard_tapped_does_not_redirect_unblocked_damage() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let bodyguard = scenario
        .add_creature_from_oracle(P1, "Veteran Bodyguard", 2, 5, VETERAN_BODYGUARD_TEXT)
        .id();
    let attacker = scenario.add_creature(P0, "Charging Bear", 3, 3).id();

    let mut runner = scenario.build();
    let p1_life_before = runner.life(P1);

    runner.advance_to_combat();

    // Tap the Bodyguard so its untapped-gate is false. No untap step occurs
    // between here and combat damage (same turn, past the untap step), so the
    // tapped state persists. Direct state mutation mirrors the precondition
    // setup pattern in weeping_angel_combat_prevention.rs.
    runner
        .state_mut()
        .objects
        .get_mut(&bodyguard)
        .unwrap()
        .tapped = true;

    run_combat_unblocked(&mut runner, P0, attacker, P1);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        p1_life_before - 3,
        "P1 must take the full 3 combat damage — the redirect is gated on the \
         Bodyguard being untapped and must NOT fire while it is tapped (CR 604.2)"
    );
    assert_eq!(
        runner.state().objects[&bodyguard].damage_marked,
        0,
        "no damage may be redirected to the tapped Veteran Bodyguard"
    );
}

/// CR 509.1h + CR 510.1c + CR 702.19b + CR 604.2: the redirect covers only damage
/// dealt "by unblocked creatures" — the source filter (`FilterProp::Unblocked`).
/// A BLOCKED attacker with trample is a *blocked* creature (CR 509.1h), so even
/// though its trample excess is dealt to the defending player (CR 510.1c +
/// CR 702.19b), that damage is NOT dealt "by an unblocked creature" and must NOT
/// be caught by the shield. The Bodyguard is kept UNTAPPED here so the tap-gate is
/// satisfied and the ONLY variable under test is the unblocked-source restriction.
///
/// Discriminating: this test FAILS if the "unblocked creatures" source filter is
/// dropped or inverted. Without the filter the shield would match ALL damage to
/// P1 and (per the same prevention-only runtime path exercised by the untapped
/// test above) prevent the trample excess, so P1 would lose 0 and the -3
/// assertion would fail. With the correct `FilterProp::Unblocked` restriction the
/// blocked trampler's damage is untouched and P1 takes the full 3.
///
/// The 5/5 trampler blocked by a 2/2 assigns lethal 2 to the blocker and 3
/// trample excess to P1 (CR 702.19b) — the shared trample-assignment harness
/// (`super::rules::run_combat`, as used by `power_fist_combat_damage_regression`)
/// drives the `AssignCombatDamage` prompt.
#[test]
fn veteran_bodyguard_untapped_does_not_redirect_blocked_trample_damage() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // P1 (defending player, "you") controls an UNTAPPED Veteran Bodyguard so the
    // tap-gate is satisfied — the source-filter is the only variable under test.
    let bodyguard = scenario
        .add_creature_from_oracle(P1, "Veteran Bodyguard", 2, 5, VETERAN_BODYGUARD_TEXT)
        .id();

    // P1 also controls a 2/2 blocker so P0's attacker becomes a *blocked* creature
    // (CR 509.1h) rather than unblocked.
    let blocker = scenario.add_creature(P1, "Blocking Bear", 2, 2).id();

    // P0 (active player) attacks with a 5/5; grant it trample so its excess damage
    // spills over to P1 even though it is blocked.
    let attacker = scenario.add_creature(P0, "Trampling Rhino", 5, 5).id();

    let mut runner = scenario.build();
    {
        // CR 702.19: grant trample directly (mirrors the keyword-push precondition
        // in `power_fist_combat_damage_regression::trample_splits_damage_when_blocked_single_blocker`).
        let obj = runner.state_mut().objects.get_mut(&attacker).unwrap();
        obj.base_keywords.push(Keyword::Trample);
        obj.keywords.push(Keyword::Trample);
    }
    let p1_life_before = runner.life(P1);

    // 5/5 trampler blocked by the 2/2: 2 lethal to the blocker, 3 trample excess
    // to P1 (CR 702.19b + CR 510.1c). `run_combat` declares P0's attack on P1 and
    // resolves the trample `AssignCombatDamage` prompt automatically.
    run_combat(&mut runner, vec![attacker], vec![(blocker, attacker)]);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        p1_life_before - 3,
        "P1 must take the full 3 trample damage — the blocked attacker is NOT an \
         'unblocked creature' (CR 509.1h), so the redirect's source filter excludes \
         it and the damage is dealt to P1 normally (CR 510.1c + CR 702.19b)"
    );
    assert_eq!(
        runner.state().objects[&bodyguard].damage_marked,
        0,
        "the untapped Bodyguard must take NO damage from the blocked trampler — its \
         shield only covers unblocked creatures"
    );
}
