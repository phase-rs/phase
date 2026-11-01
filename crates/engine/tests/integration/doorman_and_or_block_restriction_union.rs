//! CR 509.1b — a blocking restriction whose filter is an "and/or" union must
//! enforce EVERY disjunct.
//!
//! Doorman (4ed): "{T}: Until end of turn, target creature can't be blocked by
//! creatures with power 2 or less and/or Walls."
//!
//! CR 509.1b is the authorizing rule: "A restriction may be created by an
//! evasion ability (a static ability an attacking creature has that restricts
//! what can block it)." CR 608.2c supplies the reading of the filter text
//! itself ("read the whole text and apply the rules of English to the text") —
//! the "power 2 or less" suffix binds to the noun it follows, and "Walls" is a
//! second, unconstrained disjunct.
//!
//! Before the fix (issue #10364) the filter grammar's union loop ran BEFORE its
//! trailing-suffix scan, so a separator that followed a leg's own suffix was
//! never folded: "creatures with power 2 or less and/or Walls" closed on the
//! power suffix and handed " and/or Walls" back as an unconsumed remainder,
//! which every `can't be blocked by` call site discarded. The published
//! restriction was therefore HALF the printed one — a 3/5 Wall could block
//! freely — behind a green `supported: true`.
//!
//! These tests drive the real public enforcement entry point,
//! `combat::validate_blockers_for_player` (CR 509.1b/509.1c), against cards
//! built from VERBATIM Oracle text, so the whole production path runs: Oracle
//! text -> parser -> `StaticMode::CantBeBlockedBy` / `CantBeBlockedExceptBy`
//! -> ability activation and resolution -> Layer 6 `AddStaticMode`
//! -> `matches_target_filter` -> blocker legality.

use engine::game::combat::{validate_blockers_for_player, AttackerInfo, CombatState};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;

/// Verbatim Doorman Oracle text (MTGJSON AtomicCards).
const DOORMAN: &str = "{T}: Until end of turn, target creature can't be blocked by creatures with power 2 or less and/or Walls.";

/// Verbatim Elven Riders Oracle text (MTGJSON AtomicCards) — the "except by"
/// half of the same union grammar.
const ELVEN_RIDERS: &str =
    "This creature can't be blocked except by Walls and/or creatures with flying.";

struct DoormanSetup {
    runner: GameRunner,
    doorman: ObjectId,
    /// P0's attacker, the creature Doorman's ability targets.
    attacker: ObjectId,
    /// P1's power-2 creature — rejected by the "power 2 or less" disjunct.
    small: ObjectId,
    /// P1's 3/5 Wall. Power 3 is deliberately ABOVE the power disjunct's
    /// threshold, so only the "and/or Walls" disjunct can reject it. A 0/4 Wall
    /// would be rejected by the power leg alone and would make this file pass
    /// with the union half-parsed.
    wall: ObjectId,
    /// P1's 3/3 non-Wall — matched by NEITHER disjunct, so it must still be
    /// able to block. This is the positive reach-guard that keeps the two
    /// negative assertions from being vacuous.
    ordinary: ObjectId,
}

/// P0 attacks P1 with a 4/4 while P0 also controls an untapped Doorman. P1 has
/// three candidate blockers. The scenario starts at `DeclareBlockers` with the
/// attack already declared, so the ability can be activated with the attack in
/// progress — exactly the window Doorman is played in.
fn setup_doorman() -> DoormanSetup {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareBlockers);
    let doorman = scenario
        .add_creature_from_oracle(P0, "Doorman", 0, 4, DOORMAN)
        .id();
    let attacker = scenario.add_creature(P0, "Hill Giant", 4, 4).id();
    let small = scenario.add_creature(P1, "Small Blocker", 2, 2).id();
    let wall = scenario
        .add_creature(P1, "Big Wall", 3, 5)
        .with_subtypes(vec!["Wall"])
        .id();
    let ordinary = scenario.add_creature(P1, "Ordinary Blocker", 3, 3).id();

    let mut runner = scenario.build();
    runner.state_mut().combat = Some(CombatState {
        attackers: vec![AttackerInfo::attacking_player(attacker, P1)],
        ..Default::default()
    });
    DoormanSetup {
        runner,
        doorman,
        attacker,
        small,
        wall,
        ordinary,
    }
}

fn block_is_legal(setup: &DoormanSetup, blocker: ObjectId) -> bool {
    validate_blockers_for_player(setup.runner.state(), P1, &[(blocker, setup.attacker)]).is_ok()
}

/// Reach-guard #1 — before Doorman's ability resolves, every one of the three
/// candidate blockers can legally block. Without this, the two negative
/// assertions below could both pass for reasons unrelated to the restriction
/// (a tapped blocker, a wrong phase, an unbuilt combat state).
#[test]
fn every_candidate_blocker_is_legal_before_the_ability_resolves() {
    let setup = setup_doorman();
    assert!(
        block_is_legal(&setup, setup.small),
        "reach guard: the power-2 creature can block before the restriction exists"
    );
    assert!(
        block_is_legal(&setup, setup.wall),
        "reach guard: the 3/5 Wall can block before the restriction exists"
    );
    assert!(
        block_is_legal(&setup, setup.ordinary),
        "reach guard: the 3/3 non-Wall can block before the restriction exists"
    );
}

/// Reach-guard #2 — the parsed restriction really is a TWO-disjunct union.
/// An AST shape alone is not a regression test, but it localizes a failure of
/// the runtime tests below to the parser rather than to combat.
#[test]
fn doorman_grants_a_two_disjunct_blocker_restriction() {
    let mut setup = setup_doorman();
    setup
        .runner
        .activate(setup.doorman, 0)
        .target_object(setup.attacker)
        .resolve();

    let attacker = &setup.runner.state().objects[&setup.attacker];
    let filters: Vec<_> = attacker
        .static_definitions
        .iter_unchecked()
        .filter_map(|def| match &def.mode {
            StaticMode::CantBeBlockedBy { filter } => Some(filter.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        filters.len(),
        1,
        "exactly one CantBeBlockedBy restriction landed on the attacker: {:#?}",
        attacker.static_definitions
    );
    let disjuncts = match &filters[0] {
        engine::types::ability::TargetFilter::Or { filters } => filters.len(),
        other => panic!(
            "CR 509.1b + CR 608.2c: the printed filter is a union of two disjuncts, got {other:?}"
        ),
    };
    assert_eq!(
        disjuncts, 2,
        "both printed disjuncts (power 2 or less; Walls) must survive the parse: {:#?}",
        filters[0]
    );
}

/// CR 509.1b — the FIRST disjunct: a power-2 creature can't block.
#[test]
fn doorman_blocks_a_power_two_creature_from_blocking() {
    let mut setup = setup_doorman();
    setup
        .runner
        .activate(setup.doorman, 0)
        .target_object(setup.attacker)
        .resolve();

    assert!(
        !block_is_legal(&setup, setup.small),
        "CR 509.1b: a creature with power 2 is named by the first disjunct"
    );
    assert!(
        block_is_legal(&setup, setup.ordinary),
        "positive reach-guard: the restriction is a filter, not blanket unblockability"
    );
}

/// CR 509.1b — the SECOND disjunct, the one the bug dropped: a Wall whose power
/// is ABOVE the first disjunct's threshold still can't block.
///
/// This is the revert detector for issue #10364. With the fold removed, the
/// published filter is `Typed{Creature, power <= 2}` alone, the 3/5 Wall matches
/// nothing, and this assertion goes red.
#[test]
fn doorman_blocks_a_high_power_wall_from_blocking() {
    let mut setup = setup_doorman();
    setup
        .runner
        .activate(setup.doorman, 0)
        .target_object(setup.attacker)
        .resolve();

    assert!(
        !block_is_legal(&setup, setup.wall),
        "CR 509.1b + CR 608.2c: \"and/or Walls\" is a second disjunct with no \
         power constraint, so a 3/5 Wall can't block either"
    );
    assert!(
        block_is_legal(&setup, setup.ordinary),
        "positive reach-guard: a 3/3 non-Wall matches neither disjunct and can \
         still block, so the Err above is the Wall disjunct and not a blanket ban"
    );
}

/// CR 509.1b — the "except by" half of the same union grammar, on a printed
/// static rather than a granted one. Elven Riders admits BOTH disjuncts
/// (Walls, and creatures with flying) and nothing else.
#[test]
fn elven_riders_admits_both_exception_disjuncts_and_nothing_else() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareBlockers);
    let riders = scenario
        .add_creature_from_oracle(P0, "Elven Riders", 3, 3, ELVEN_RIDERS)
        .id();
    let wall = scenario
        .add_creature(P1, "Big Wall", 3, 5)
        .with_subtypes(vec!["Wall"])
        .id();
    let flyer = scenario.add_creature(P1, "Flyer", 2, 2).flying().id();
    let ground = scenario.add_creature(P1, "Ground Pounder", 3, 3).id();

    let mut runner = scenario.build();
    runner.state_mut().combat = Some(CombatState {
        attackers: vec![AttackerInfo::attacking_player(riders, P1)],
        ..Default::default()
    });

    let legal = |blocker: ObjectId| {
        validate_blockers_for_player(runner.state(), P1, &[(blocker, riders)]).is_ok()
    };
    assert!(
        legal(wall),
        "CR 509.1b: the first exception disjunct (Walls) must be admitted"
    );
    assert!(
        legal(flyer),
        "CR 509.1b: the second exception disjunct (creatures with flying) must \
         be admitted — if the union truncated to its first leg this goes red"
    );
    assert!(
        !legal(ground),
        "negative discriminator: a ground non-Wall matches neither exception, \
         so the restriction is really being enforced"
    );
}
