//! CR 508.1d + CR 604.1 / CR 604.2: Galactus, Devourer of Worlds — a printed
//! STATIC forced-attack requirement whose required defender is a LIVE-evaluated
//! player class ("an opponent with the most life among your opponents"), gated by
//! "unless you control a creature named Silver Surfer, Galactus's Herald".
//!
//! These drive the REAL pipeline end-to-end: verbatim Oracle text →
//! `normalize_self_refs_for_static` → parser
//! (`parse_forced_attack_defender_static`) → `MustAttackPlayer { Matching }` →
//! `must_attack_player_directives_for_creature` (the changed runtime seam,
//! re-evaluated each declare-attackers step) → `attacker_constraints_for_active_player`
//! (the DeclareAttackers waiting payload authority) AND the `declare_attackers`
//! legality validator via the `GameAction::DeclareAttackers` route.

use engine::game::combat::{
    attacker_constraints_for_active_player, get_valid_attacker_ids, AttackTarget, CombatRequirement,
};
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);

/// Galactus, Devourer of Worlds — verbatim Oracle text (Scryfall). The
/// forced-attack line carries the CR 207.2d Universes-Beyond flavor label
/// "Insatiable Hunger — " and the embedded "Galactus's" inside the herald's name.
const GALACTUS_ORACLE: &str = concat!(
    "Flying, trample, indestructible\n",
    "When Galactus enters, exile target permanent.\n",
    "Insatiable Hunger — Galactus attacks an opponent with the most life among ",
    "your opponents each combat if able unless you control a creature named ",
    "Silver Surfer, Galactus's Herald.",
);

/// Build a 3-player game with Galactus (parsed from verbatim Oracle text) on P0's
/// battlefield, opponents at the given life totals, optionally with a creature of
/// `herald` name under P0's control, parked at declare-attackers with the
/// materialized statics live.
fn parked_galactus(p1_life: i32, p2_life: i32, herald: Option<&str>) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::DeclareAttackers);
    let galactus = scenario
        .add_creature_from_oracle(P0, "Galactus, Devourer of Worlds", 12, 12, GALACTUS_ORACLE)
        .id();
    if let Some(name) = herald {
        scenario.add_creature(P0, name, 4, 4);
    }
    scenario.with_life(P1, p1_life);
    scenario.with_life(P2, p2_life);
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.phase = Phase::DeclareAttackers;
        state.turn_number = 2;
        state.layers_dirty.mark_full();
    }
    evaluate_layers(runner.state_mut());
    let valid = get_valid_attacker_ids(runner.state());
    runner.state_mut().waiting_for = WaitingFor::DeclareAttackers {
        player: P0,
        valid_attacker_ids: valid,
        valid_attack_targets: vec![AttackTarget::Player(P1), AttackTarget::Player(P2)],
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };
    (runner, galactus)
}

fn declare(runner: &mut GameRunner, galactus: ObjectId, defender: PlayerId) -> Result<(), String> {
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(galactus, AttackTarget::Player(defender))],
            bands: vec![],
        })
        .map(|_| ())
        .map_err(|e| format!("{e:?}"))
}

/// The changed seam surfaces through the production requirement authority: the
/// required defender resolves to the most-life opponent (CR 604.1 live class).
/// REVERT-FAIL: if the `RequiredDefender::Matching` arm of
/// `must_attack_player_directives_for_creature` returned nothing, `players` would
/// be empty and this equality fails.
#[test]
fn galactus_requirement_surfaces_most_life_opponent() {
    let (runner, galactus) = parked_galactus(30, 20, None);
    let valid = get_valid_attacker_ids(runner.state());
    assert!(
        valid.contains(&galactus),
        "reach-guard: Galactus is a valid attacker (the requirement is non-vacuous)"
    );
    let constraints = attacker_constraints_for_active_player(runner.state(), &valid);
    let Some(CombatRequirement::MustAttack { players, .. }) = constraints.get(&galactus) else {
        panic!(
            "expected a MustAttack requirement for Galactus, got {:?}",
            constraints.get(&galactus)
        );
    };
    assert_eq!(
        players,
        &vec![P1],
        "the live-evaluated required defender is the single most-life opponent"
    );
}

/// CR 508.1d enforcement: Galactus must attack the most-life opponent (P1@30) —
/// attacking the lower-life opponent (P2@20) is rejected, attacking P1 commits,
/// and declaring no attacker is rejected (it is able to attack).
#[test]
fn galactus_forced_to_attack_most_life_opponent() {
    // Wrong opponent (P2, not most life) → rejected.
    let (mut wrong, galactus) = parked_galactus(30, 20, None);
    assert!(
        declare(&mut wrong, galactus, P2).is_err(),
        "attacking the lower-life opponent leaves the most-life requirement unmet"
    );

    // Correct opponent (P1, most life) → accepted and committed.
    let (mut right, galactus) = parked_galactus(30, 20, None);
    declare(&mut right, galactus, P1)
        .expect("attacking the most-life opponent satisfies CR 508.1d");
    assert!(
        right.state().combat.is_some(),
        "the satisfying declaration must commit"
    );

    // Declining entirely → rejected (Galactus is able to attack).
    let (mut none, _galactus) = parked_galactus(30, 20, None);
    let empty = none.act(GameAction::DeclareAttackers {
        attacks: vec![],
        bands: vec![],
    });
    assert!(
        empty.is_err(),
        "declaring no attacker leaves the requirement unmet — it still binds"
    );
}

/// Reach-guard proving the defender is re-evaluated LIVE, not snapshotted at setup:
/// build ONE fixture with P1 as the most-life opponent (30 vs 20), then swap the
/// life totals IN PLACE before declaring. Because the requirement is recomputed
/// from live state at declare-attackers time, attacking P1 (now the lower-life
/// opponent) is rejected and attacking P2 (now the most-life opponent) commits —
/// both through the production `GameAction::DeclareAttackers` path in the SAME game
/// state. A setup-time snapshot of the required defender would still name P1 and
/// wrongly accept the P1 declaration, so this fails on any snapshot regression.
#[test]
fn galactus_required_defender_reevaluated_live() {
    let (mut runner, galactus) = parked_galactus(30, 20, None);
    // Swap the life totals in place: P2 becomes the most-life opponent. (A rejected
    // declaration commits nothing — CR 508.1a–e validate before any tap/commit — so
    // the reject-then-accept below runs against one continuous game state.)
    runner.state_mut().players[P1.0 as usize].life = 20;
    runner.state_mut().players[P2.0 as usize].life = 30;

    assert!(
        declare(&mut runner, galactus, P1).is_err(),
        "after the in-place life swap P1 is no longer the most-life opponent"
    );
    declare(&mut runner, galactus, P2)
        .expect("P2 became the most-life opponent after the live swap — attacking it is legal");
    assert!(
        runner.state().combat.is_some(),
        "the satisfying declaration commits in the same fixture"
    );
}

/// CR 508.1d tie: when opponents are tied for the most life, attacking EITHER
/// satisfies the requirement (the tied set both surface), but declining does not.
#[test]
fn galactus_tie_allows_either_most_life_opponent() {
    let (mut p1, galactus) = parked_galactus(25, 25, None);
    declare(&mut p1, galactus, P1).expect("attacking one tied most-life opponent is legal");
    assert!(p1.state().combat.is_some());

    let (mut p2, galactus) = parked_galactus(25, 25, None);
    declare(&mut p2, galactus, P2)
        .expect("attacking the other tied most-life opponent is equally legal");
    assert!(p2.state().combat.is_some());

    // Vacuity guard: the tie must still BIND — declining is rejected.
    let (mut none, _galactus) = parked_galactus(25, 25, None);
    let empty = none.act(GameAction::DeclareAttackers {
        attacks: vec![],
        bands: vec![],
    });
    assert!(
        empty.is_err(),
        "a tie expands the required set — it does not drop the requirement"
    );
}

/// CR 604.1 gate: controlling a creature named exactly "Silver Surfer, Galactus's
/// Herald" suppresses the static (its `condition` is false), so Galactus is free
/// to decline. Paired reach-guard: a creature with a DIFFERENT name does NOT
/// suppress — proving the named-control predicate, not a bare presence check.
#[test]
fn galactus_herald_suppresses_requirement() {
    let (mut suppressed, _galactus) =
        parked_galactus(30, 20, Some("Silver Surfer, Galactus's Herald"));
    suppressed
        .act(GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        })
        .expect("the herald suppresses the forced-attack requirement — declining is legal");

    // Wrong name → requirement still binds → declining is rejected.
    let (mut still_forced, _galactus) = parked_galactus(30, 20, Some("Silver Surfer"));
    let empty = still_forced.act(GameAction::DeclareAttackers {
        attacks: vec![],
        bands: vec![],
    });
    assert!(
        empty.is_err(),
        "a differently-named creature must NOT suppress the requirement"
    );
}
