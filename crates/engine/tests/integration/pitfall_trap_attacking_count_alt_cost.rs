//! Pitfall Trap — "If exactly one creature is attacking, you may pay {W} rather
//! than pay this spell's mana cost. Destroy target attacking creature without
//! flying."
//!
//! The leading "if exactly one creature is attacking" gate is an EQ 1 count over
//! every attacking creature, any controller (CR 508.1k), and it gates the {W}
//! alternative cost (CR 118.9). These tests drive the real cast pipeline: with
//! exactly one attacker the {W} cost is offered and only one Plains is tapped;
//! with two attackers it is not offered and the full {2}{W} is paid. The gate is
//! pinned in both directions — the opponent attacking the caster (the card's
//! printed use) and the caster's own attack.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

// Verbatim Oracle text (Scryfall).
const PITFALL_TRAP: &str = "If exactly one creature is attacking, you may pay {W} rather than pay this spell's mana cost.\n\
Destroy target attacking creature without flying.";

struct Board {
    runner: GameRunner,
    trap: ObjectId,
    attackers: Vec<ObjectId>,
    plains: Vec<ObjectId>,
}

/// Put Pitfall Trap ({2}{W}) into P0's hand and `plains` Plains onto P0's
/// battlefield; `attackers` creatures (with `flying` when set) for `attacker_owner`.
fn setup(
    attacker_owner: PlayerId,
    attackers: usize,
    flying: bool,
    plains: usize,
) -> (GameScenario, ObjectId, Vec<ObjectId>, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_ids = (0..attackers)
        .map(|i| {
            let mut b = scenario.add_creature(attacker_owner, &format!("Attacker {i}"), 2, 2);
            if flying {
                b.with_keyword(Keyword::Flying);
            }
            b.id()
        })
        .collect();
    let plains_ids = (0..plains)
        .map(|_| scenario.add_basic_land(P0, ManaColor::White))
        .collect();
    let trap = scenario
        .add_spell_to_hand_from_oracle(P0, "Pitfall Trap", true, PITFALL_TRAP)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 2,
        })
        .id();
    (scenario, trap, attacker_ids, plains_ids)
}

/// P1 attacks P0 (the card's printed use): P1 is made the active player in its
/// pre-combat main, declares `attackers` at P0, then passes priority so P0 (the
/// Trap's controller) holds priority in the declare-attackers step.
fn opponent_attacks(attackers: usize, flying: bool, plains: usize) -> Board {
    let (scenario, trap, attackers, plains) = setup(P1, attackers, flying, plains);
    let mut runner = scenario.build();
    {
        let st = runner.state_mut();
        st.active_player = P1;
        st.priority_player = P1;
        st.waiting_for = WaitingFor::Priority { player: P1 };
    }
    runner.advance_to_combat();
    let attacks: Vec<_> = attackers
        .iter()
        .map(|&a| (a, AttackTarget::Player(P0)))
        .collect();
    runner
        .declare_attackers(&attacks)
        .expect("P1 declares its attackers");
    // CR 508.2: after attackers are declared, the active player gets priority.
    assert_eq!(runner.state().phase, Phase::DeclareAttackers);
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    );
    // CR 117.3d: P1 passes; P0 receives priority with an empty stack.
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes priority");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
    assert!(runner.state().stack.is_empty());
    Board {
        runner,
        trap,
        attackers,
        plains,
    }
}

/// P0 attacks P1 with its own creatures and keeps priority in the
/// declare-attackers step (CR 508.2).
fn own_attack(attackers: usize, flying: bool, plains: usize) -> Board {
    let (scenario, trap, attackers, plains) = setup(P0, attackers, flying, plains);
    let mut runner = scenario.build();
    runner.advance_to_combat();
    let attacks: Vec<_> = attackers
        .iter()
        .map(|&a| (a, AttackTarget::Player(P1)))
        .collect();
    runner
        .declare_attackers(&attacks)
        .expect("P0 declares its attackers");
    assert_eq!(runner.state().phase, Phase::DeclareAttackers);
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
    Board {
        runner,
        trap,
        attackers,
        plains,
    }
}

fn tapped_plains(b: &Board) -> usize {
    b.plains
        .iter()
        .filter(|p| b.runner.state().objects[*p].tapped)
        .count()
}

fn cast_and_resolve(b: &mut Board, target: ObjectId) {
    b.runner
        .cast(b.trap)
        .target_objects(&[target])
        .accept_optional()
        .resolve();
}

fn assert_destroyed(b: &Board, target: ObjectId) {
    // CR 701.8a: destroying a permanent moves it to its owner's graveyard.
    assert_eq!(
        b.runner.state().objects[&target].zone,
        Zone::Graveyard,
        "Pitfall Trap destroys the targeted attacking creature"
    );
}

/// PRIMARY: the opponent attacks with exactly one creature, so the {W}
/// alternative cost is offered and paid — one of P0's three Plains is tapped.
/// Reverting the comparator-aware gate leaves the option unparsed, so the full
/// {2}{W} is paid (three Plains tapped) and the tap-count assertion fails.
#[test]
fn opponent_single_attacker_offers_w_alternative_cost() {
    let mut b = opponent_attacks(1, false, 3);
    let attacker = b.attackers[0];
    cast_and_resolve(&mut b, attacker);
    assert_destroyed(&b, attacker);
    // CR 118.9 + CR 118.9a: the {W} alternative cost replaces the {2}{W} mana cost.
    assert_eq!(
        tapped_plains(&b),
        1,
        "exactly one creature is attacking, so only {{W}} is paid"
    );
}

/// The opponent attacks with two creatures: the EQ 1 gate is false, the
/// alternative cost is not offered, and the full {2}{W} is paid.
#[test]
fn opponent_two_attackers_pays_full_mana_cost() {
    let mut b = opponent_attacks(2, false, 3);
    let target = b.attackers[0];
    cast_and_resolve(&mut b, target);
    assert_destroyed(&b, target);
    // CR 601.2f + CR 601.2h: no alternative cost applies; the mana cost is paid.
    assert_eq!(
        tapped_plains(&b),
        3,
        "two creatures are attacking, so the full {{2}}{{W}} is paid"
    );
}

/// The caster's own single attacker also satisfies the controller-agnostic
/// gate (CR 508.1k counts every attacking creature).
#[test]
fn own_single_attacker_offers_w_alternative_cost() {
    let mut b = own_attack(1, false, 3);
    let attacker = b.attackers[0];
    cast_and_resolve(&mut b, attacker);
    assert_destroyed(&b, attacker);
    // CR 118.9: {W} paid instead of {2}{W}.
    assert_eq!(tapped_plains(&b), 1);
}

/// The caster's own two attackers: gate false, full {2}{W} paid.
#[test]
fn own_two_attackers_pays_full_mana_cost() {
    let mut b = own_attack(2, false, 3);
    let target = b.attackers[0];
    cast_and_resolve(&mut b, target);
    assert_destroyed(&b, target);
    // CR 601.2f + CR 601.2h: the mana cost is paid in full.
    assert_eq!(tapped_plains(&b), 3);
}

/// With a single Plains, Pitfall Trap is castable only through the {W}
/// alternative cost: one attacker → resolves (reach-guard), two attackers →
/// the {2}{W} mana cost cannot be paid and the cast is rejected.
#[test]
fn single_plains_castable_only_with_exactly_one_attacker() {
    let mut one = own_attack(1, false, 1);
    let attacker = one.attackers[0];
    cast_and_resolve(&mut one, attacker);
    assert_destroyed(&one, attacker);

    let mut two = own_attack(2, false, 1);
    let target = two.attackers[0];
    // CR 601.2f + CR 601.2h: without the alternative cost, {2}{W} is unpayable
    // from one Plains.
    let result = two
        .runner
        .cast(two.trap)
        .target_objects(&[target])
        .accept_optional()
        .try_resolve();
    assert!(
        result.is_err(),
        "two attackers: the {{W}} alternative cost is unavailable and {{2}}{{W}} is unpayable"
    );
    assert_eq!(two.runner.state().objects[&target].zone, Zone::Battlefield);
}

/// A lone attacker with flying is not a legal target ("without flying",
/// CR 702.9a), so the spell has no legal target and cannot be cast
/// (CR 115.1 + CR 601.2c). Reach-guard: the non-flying single-attacker case
/// above resolves through the same fixture.
#[test]
fn lone_flying_attacker_is_not_a_legal_target() {
    let mut b = own_attack(1, true, 3);
    let attacker = b.attackers[0];
    let result = b
        .runner
        .cast(b.trap)
        .target_objects(&[attacker])
        .accept_optional()
        .try_resolve();
    assert!(
        result.is_err(),
        "a flying attacker is not a legal Pitfall Trap target"
    );
    assert_eq!(b.runner.state().objects[&attacker].zone, Zone::Battlefield);
}
