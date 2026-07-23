//! GameRunner integration regression for Weeping Angel's combat-damage
//! prevention pipeline.
//!
//! Oracle text under test (representative of the class):
//!   "If this creature would deal combat damage to a creature, prevent that
//!    damage and that creature's owner shuffles it into their library."
//!
//! Three complementary cases in a single file:
//!
//!  1. **Basic — creature is shuffled into owner's library.** Weeping Angel
//!     attacks; the defending player blocks with a creature; the prevention
//!     fires (CR 615.1a), and the blocked creature is moved to its owner's
//!     library and that library is shuffled (CR 615.5 follow-up).
//!     Discriminating: without the `replacement.rs` fix that allows static
//!     `Prevention::All` object-hosted shields to fire their `execute` follow-up
//!     per-event inline (instead of being suppressed by `batched_combat_all_shield`),
//!     the ChangeZone + Shuffle chain never fires and the creature stays on
//!     the battlefield.
//!
//!  2. **Owner ≠ controller — shuffled into owner's library, not controller's.**
//!     The blocking creature is owned by P0 but currently controlled by P1
//!     (simulating a steal effect). After prevention, the creature must go to
//!     P0's library (CR 108.3: owner is the player who started with the card in
//!     their deck; CR 400.3: a card that would go to any library goes to its
//!     owner's library). A controller-projection would wrongly place it in P1's
//!     library, failing the assertion.
//!
//!  3. **Negative — unblocked player damage is NOT prevented.** Weeping Angel
//!     attacks unblocked; the shield is "to a creature" scoped (CR 615.1a,
//!     `DamageTargetFilter::CreatureOnly`), so it does NOT apply when the
//!     target is a player (CR 510.1b: unblocked attacker assigns damage to
//!     the player or planeswalker it's attacking). The defending player must
//!     lose life equal to Weeping Angel's power.
//!
//! CR 510.1b + CR 510.2 + CR 615.1a + CR 615.5 + CR 108.3 + CR 400.3.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Synthetic oracle text for the Weeping Angel prevention shield — the same
/// text verified by `weeping_angel_prevention_scopes_to_creature_and_rewrites_anaphors`
/// in `oracle_replacement.rs`. Using the same text exercises the full real
/// parse → combat pipeline without hardcoding an in-band card database.
const WEEPING_ANGEL_TEXT: &str =
    "If this creature would deal combat damage to a creature, prevent that \
     damage and that creature's owner shuffles it into their library.";

/// Drive the game from the current state (expected to be at or before
/// DeclareAttackers) through the end-of-combat step, answering combat prompts:
///   - P0 (`attacker_player`) declares `attacker` against `defend_player`.
///   - `blocker` (if Some) is declared to block `attacker` by the defending
///     player; otherwise no blocks are declared.
///   - All other priority windows are auto-passed.
///
/// Stops when the phase reaches EndCombat / PostCombatMain or when a prompt
/// the driver cannot auto-handle surfaces.
fn run_combat(
    runner: &mut engine::game::scenario::GameRunner,
    attacker_player: engine::types::player::PlayerId,
    attacker: engine::types::identifiers::ObjectId,
    defend_player: engine::types::player::PlayerId,
    blocker: Option<engine::types::identifiers::ObjectId>,
) {
    let mut attacked = false;
    let mut blocked = false;

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
            WaitingFor::DeclareBlockers { .. } if !blocked => {
                blocked = true;
                let blocks = if let Some(blk) = blocker {
                    vec![(blk, attacker)]
                } else {
                    vec![]
                };
                if runner.declare_blockers(&blocks).is_err() {
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

/// CR 615.1a + CR 615.5: When Weeping Angel deals combat damage to a creature,
/// the prevention shield fires, the damage is prevented (the blocker takes no
/// marked damage), and the CR 615.5 follow-up moves the blocker to its owner's
/// library (ChangeZone) and shuffles that library (Shuffle).
///
/// Discriminating: without the fix in `replacement.rs` that allows static
/// `Prevention::All` object-hosted shields to fire per-event inline during the
/// combat batch (rather than being suppressed as a `batched_combat_all_shield`),
/// the ChangeZone + Shuffle follow-up never fires and the blocker stays on the
/// battlefield — the `Zone::Library` assertion fails.
#[test]
fn weeping_angel_combat_damage_to_blocker_shuffles_into_owners_library() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for &pid in &[P0, P1] {
        // Give each player some library cards so the shuffle has something to
        // operate on (prevents a shuffle-of-empty-library edge case).
        scenario.with_library_top(pid, &["Card A", "Card B", "Card C"]);
    }

    // P0 controls Weeping Angel (4/4 with the prevention static ability).
    // 4/4 ensures Weeping Angel survives the blocker's combat damage.
    let angel = scenario
        .add_creature_from_oracle(P0, "Weeping Angel", 4, 4, WEEPING_ANGEL_TEXT)
        .id();

    // P1 controls a 1/1 creature that blocks Weeping Angel. After prevention,
    // it must be in Zone::Library (P1's library).
    let victim = scenario.add_creature(P1, "Potential Victim", 1, 1).id();

    let mut runner = scenario.build();

    runner.advance_to_combat();
    run_combat(&mut runner, P0, angel, P1, Some(victim));
    // Drain any remaining post-combat priority windows and triggered abilities.
    runner.advance_until_stack_empty();

    // CR 615.1a + CR 615.5: the prevention fired and the follow-up moved the
    // victim creature to its owner's library.
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Library,
        "the blocked creature must be in Zone::Library after Weeping Angel's \
         prevention follow-up fires (CR 615.5); it should not remain on the \
         battlefield or go to the graveyard"
    );

    // CR 108.3 + CR 400.3: the creature went to P1's library (its owner).
    let victim_in_p1_library = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .map(|p| p.library.contains(&victim))
        .unwrap_or(false);
    assert!(
        victim_in_p1_library,
        "the creature must be in P1's library (its owner), not P0's"
    );

    // Weeping Angel is still on the battlefield: the prevention shields itself
    // from dealing any damage, but the 1/1 blocker deals 1 damage to Weeping
    // Angel — with 4 toughness, that is sub-lethal and it survives.
    assert_eq!(
        runner.state().objects[&angel].zone,
        Zone::Battlefield,
        "Weeping Angel must remain on the battlefield after the combat"
    );
}

/// CR 108.3 + CR 400.3 + CR 615.5: when the blocked creature is owned by P0
/// but currently controlled by P1 (simulating a steal effect), the CR 615.5
/// follow-up must route the creature to P0's library — the owner's library —
/// not P1's (the controller's).
///
/// Discriminating: a controller-projection in `PostReplacementDamageTargetOwner`
/// resolution would route the creature to P1's library instead, failing the
/// P0-library assertion.
#[test]
fn weeping_angel_prevention_uses_owner_not_controller_for_library() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for &pid in &[P0, P1] {
        scenario.with_library_top(pid, &["Card A", "Card B", "Card C"]);
    }

    let angel = scenario
        .add_creature_from_oracle(P0, "Weeping Angel", 4, 4, WEEPING_ANGEL_TEXT)
        .id();

    // The "stolen" creature is owned by P0 but will be controlled by P1 so that
    // P1 can declare it as a blocker. After prevention, it must go to P0's
    // library (the owner's), not P1's.
    let stolen = scenario.add_creature(P0, "Stolen Bear", 1, 1).id();

    let mut runner = scenario.build();

    // Transfer control to P1 so P1 can block with it. Owner remains P0.
    // CR 109.4: the player who controls an object is its controller; CR 108.3:
    // the owner is always the player who started with the card in their deck.
    //
    // Both `controller` and `base_controller` must be updated: `evaluate_layers`
    // resets `controller` to `base_controller.unwrap_or(owner)` on every game
    // action. Without the `base_controller` update the controller reverts to P0
    // before the DeclareBlockers prompt, making `stolen` invisible to P1.
    {
        let obj = runner.state_mut().objects.get_mut(&stolen).unwrap();
        obj.base_controller = Some(P1);
        obj.controller = P1;
    }

    runner.advance_to_combat();
    run_combat(&mut runner, P0, angel, P1, Some(stolen));
    runner.advance_until_stack_empty();

    // CR 400.3: the creature must go to its owner's (P0's) library, NOT
    // to P1's library even though P1 was its controller.
    let in_p0_library = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .map(|p| p.library.contains(&stolen))
        .unwrap_or(false);
    assert!(
        in_p0_library,
        "the creature (owned by P0, controlled by P1) must be shuffled into \
         P0's library (owner, CR 108.3 + CR 400.3); controller-projection \
         would route it to P1's library"
    );

    let in_p1_library = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .map(|p| p.library.contains(&stolen))
        .unwrap_or(false);
    assert!(
        !in_p1_library,
        "the creature must NOT be in P1's library (P1 was only its controller, \
         not its owner)"
    );
}

/// CR 510.1b + CR 615.1a: Weeping Angel's shield is "to a creature" scoped
/// (damage_target_filter = DamageTargetFilter::CreatureOnly). When Weeping Angel
/// attacks unblocked, it deals combat damage directly to the defending player —
/// a player, not a creature — so the prevention effect MUST NOT apply.
/// The defending player must lose life equal to Weeping Angel's power (4).
///
/// Discriminating: if the prevention shield incorrectly fired for player targets,
/// the defending player would take 0 damage and the life assertion would fail.
#[test]
fn weeping_angel_prevention_does_not_apply_to_unblocked_player_damage() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for &pid in &[P0, P1] {
        scenario.with_library_top(pid, &["Card A", "Card B"]);
    }

    // P0 controls a 4/4 Weeping Angel. P1 has no blockers.
    let angel = scenario
        .add_creature_from_oracle(P0, "Weeping Angel", 4, 4, WEEPING_ANGEL_TEXT)
        .id();

    let mut runner = scenario.build();
    let p1_life_before = runner.life(P1);

    // Attack unblocked — P1 declares no blockers.
    runner.advance_to_combat();
    run_combat(&mut runner, P0, angel, P1, None);
    runner.advance_until_stack_empty();

    // CR 510.1b: the unblocked attacker assigned its 4 damage to P1.
    // CR 615.1a: the shield is "to a creature" scoped so it does NOT apply.
    assert_eq!(
        runner.life(P1),
        p1_life_before - 4,
        "P1 must take 4 combat damage from the unblocked Weeping Angel — the \
         prevention shield is creature-scoped and must not intercept player damage \
         (CR 510.1b + CR 615.1a)"
    );
}
