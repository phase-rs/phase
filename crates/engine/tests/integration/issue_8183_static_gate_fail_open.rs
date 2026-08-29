//! Issue #8183 — a static ability's gate condition that the parser could not
//! type lands as `StaticCondition::Unrecognized`, which the layer system
//! evaluates as ALWAYS TRUE (fail-open). Three shapes of that defect are fixed
//! in the PARSER (no `layers.rs` change), and this file is the runtime proof
//! that the fixed AST actually changes what the game allows.
//!
//! CR 604.1: a static ability is "simply true" — its gate decides whether it
//! currently applies, so a gate the parser drops or mis-polarizes silently
//! turns the restriction on (or off) for every board state.
//!
//!  1. **Graxiplon** — "This creature can't be blocked unless defending player
//!     controls three or more creatures that share a creature type."
//!     The `unless` NEGATION was dropped at the `can't be blocked` fallback
//!     branch, leaving a bare `Unrecognized` that evaluates TRUE, so
//!     `CantBeBlocked` applied unconditionally: Graxiplon was permanently
//!     unblockable. With the fix the gate is `Not(Unrecognized)`, which
//!     evaluates FALSE, so the restriction does not apply and the block is
//!     legal (CR 509.1b — the defending player checks each creature for
//!     blocking restrictions, and an evasion ability creates one).
//!
//!  2. **Training Drone** — "This creature can't attack or block unless it's
//!     equipped." The anaphoric "it" in the gate was never resolved to the
//!     source, so the gate stayed `Not(Unrecognized)` = FALSE and the
//!     restriction NEVER applied: an unequipped Training Drone could attack.
//!     With the fix the gate is `Not(SourceIsEquipped)` (CR 301.5a — the
//!     creature an Equipment is attached to is the "equipped creature"), so an
//!     unequipped Drone cannot be declared as an attacker (CR 508.1d) and an
//!     equipped one can.
//!
//!  3. **Ancestral Katana** — `Equipped creature gets +2/+2 and has "This
//!     creature has first strike as long as it's attacking."` The predicate was
//!     split on the ` as long as ` INSIDE the quoted granted ability, so the
//!     granted first strike was destroyed and its gate was mis-attached to the
//!     +2/+2. With the fix the quoted ability survives as a granted static whose
//!     own gate is evaluated against the EQUIPPED CREATURE (CR 611.3a — a
//!     continuous effect from a static ability applies at any given moment to
//!     whatever its text indicates; CR 508.1k — an attacking creature).
//!
//! Every card here is built from VERBATIM Oracle text through
//! `GameScenario`/`GameRunner`, so the whole production route runs: Oracle text
//! → static parser → `StaticDefinition.condition` → `evaluate_condition*` →
//! combat legality / layer evaluation. No test in this file asserts an AST
//! shape as its primary claim; the two AST assertions present are explicitly
//! labelled reach-guards.

use engine::game::combat::{AttackTarget, AttackerInfo, CombatState};
use engine::game::game_object::AttachTarget;
use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;

/// Verbatim Graxiplon Oracle text (MTGJSON AtomicCards).
const GRAXIPLON: &str = "This creature can't be blocked unless defending player controls three or more creatures that share a creature type.";

/// Verbatim Training Drone Oracle text (MTGJSON AtomicCards).
const TRAINING_DRONE: &str = "This creature can't attack or block unless it's equipped.";

/// Verbatim Ancestral Katana Oracle text — the Alchemy rebalanced printing,
/// which MTGJSON keys as `A-Ancestral Katana` and whose printed card name is
/// "Ancestral Katana". The PAPER Ancestral Katana reads
/// "Equipped creature gets +2/+1." and carries no quoted granted ability at
/// all, so a fixture built from the paper text could not exercise this defect.
const ANCESTRAL_KATANA: &str = "Whenever a Samurai or Warrior you control attacks alone, you may pay {1}. When you do, attach Ancestral Katana to it.\nEquipped creature gets +2/+2 and has \"This creature has first strike as long as it's attacking.\"\nEquip {2}";

/// Wire `equipment` onto `host` the way the equip action does (CR 301.5).
/// Mirrors the local `attach` helper in `mjolnir_hammer_double_damage.rs`.
fn attach(runner: &mut GameRunner, equipment: ObjectId, host: ObjectId) {
    let state = runner.state_mut();
    state.objects.get_mut(&equipment).unwrap().attached_to = Some(AttachTarget::Object(host));
    state
        .objects
        .get_mut(&host)
        .unwrap()
        .attachments
        .push(equipment);
}

/// True iff `id` has `keyword` after a fresh layer evaluation (CR 613).
/// Same idiom as `knighthood_first_strike_grant.rs`.
fn has_kw(runner: &mut GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], keyword)
}

/// From `Phase::PreCombatMain`, pass to the declare-attackers step and assert we
/// actually arrived — so a harness change surfaces as a clear failure rather
/// than silently making every legality assertion below vacuous.
fn advance_to_declare_attackers(runner: &mut GameRunner) {
    runner.pass_both_players();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ),
        "fixture must reach the declare-attackers step; got {:?}",
        runner.state().waiting_for
    );
}

// ---------------------------------------------------------------------------
// 1. Graxiplon — the dropped `unless` negation
// ---------------------------------------------------------------------------

/// REACH-GUARD for the Graxiplon runtime test below.
///
/// `graxiplon_can_be_blocked_when_gate_unmet` asserts a block is LEGAL. That
/// assertion is satisfied for the wrong reason if Graxiplon parsed no
/// `CantBeBlocked` static at all, or parsed one with no gate. This pins that
/// the static exists AND carries a condition, so the legal block below is
/// evidence about the GATE and not about an absent static.
#[test]
fn graxiplon_parses_a_condition_gated_cant_be_blocked_static() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let graxiplon = scenario
        .add_creature_from_oracle(P0, "Graxiplon", 4, 4, GRAXIPLON)
        .id();
    let runner = scenario.build();

    let statics = &runner.state().objects[&graxiplon].static_definitions;
    let gated: Vec<_> = statics
        .iter_unchecked()
        .filter(|d| matches!(d.mode, StaticMode::CantBeBlocked))
        .collect();
    assert_eq!(
        gated.len(),
        1,
        "Graxiplon must parse to exactly one CantBeBlocked static: {statics:#?}"
    );
    assert!(
        gated[0].condition.is_some(),
        "the CantBeBlocked static must carry the `unless` gate as a condition, \
         not be unconditional: {statics:#?}"
    );
}

/// CR 509.1b + CR 604.1: with the `unless` gate UNMET (the defending player
/// controls a single creature, not three sharing a type), the evasion
/// restriction does NOT apply and the block is legal.
///
/// Discriminating: revert U2 (restore the bare
/// `nom_condition::parse_condition(after_blocked)` fallback) and the gate
/// becomes a bare `Unrecognized`, which the layer system evaluates as TRUE —
/// `CantBeBlocked` applies unconditionally and `declare_blockers` returns Err.
/// The `is_ok()` assertion is what flips.
#[test]
fn graxiplon_can_be_blocked_when_gate_unmet() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let graxiplon = scenario
        .add_creature_from_oracle(P0, "Graxiplon", 4, 4, GRAXIPLON)
        .id();
    // ONE creature for the defender: the printed gate ("three or more creatures
    // that share a creature type") is unmet by construction.
    let blocker = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();

    let mut runner = scenario.build();

    advance_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[(graxiplon, AttackTarget::Player(P1))])
        .expect("Graxiplon must be a legal attacker");

    // CR 508.2 + CR 117.1c: the active player gets priority after attackers are
    // declared; pass through it to reach the declare-blockers step.
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ),
        "fixture must reach the declare-blockers step; got {:?}",
        runner.state().waiting_for
    );

    assert!(
        runner.declare_blockers(&[(blocker, graxiplon)]).is_ok(),
        "with the `unless` gate unmet the CantBeBlocked restriction must NOT \
         apply, so the block is legal (CR 509.1b)"
    );
}

// ---------------------------------------------------------------------------
// 2. Training Drone — the unresolved source anaphor
// ---------------------------------------------------------------------------

/// CR 508.1d + CR 301.5a: an UNEQUIPPED Training Drone cannot be declared as an
/// attacker.
///
/// Discriminating: revert U1 (make the helper's SelfRef arm unreachable) and the
/// gate stays `Not(Unrecognized)` = FALSE, the restriction never applies, and
/// `declare_attackers` returns Ok. The `is_err()` assertion is what flips.
///
/// Paired positive reach-guard: `training_drone_can_attack_while_equipped`
/// below. Without it this negative could be satisfied by summoning sickness or
/// a tapped state rather than by the restriction under test.
#[test]
fn training_drone_cannot_attack_while_unequipped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let drone = scenario
        .add_creature_from_oracle(P0, "Training Drone", 1, 1, TRAINING_DRONE)
        .id();
    let mut runner = scenario.build();

    advance_to_declare_attackers(&mut runner);
    assert!(
        runner
            .declare_attackers(&[(drone, AttackTarget::Player(P1))])
            .is_err(),
        "an unequipped Training Drone must not be a legal attacker \
         (CR 508.1d; the `unless it's equipped` gate is unmet)"
    );
}

/// Paired positive for the negative above: attach an Equipment and the SAME
/// declaration becomes legal. This is what proves the negative is caused by the
/// gate and not by an unrelated attack-legality failure.
#[test]
fn training_drone_can_attack_while_equipped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let drone = scenario
        .add_creature_from_oracle(P0, "Training Drone", 1, 1, TRAINING_DRONE)
        .id();
    // CR 301.5 + CR 704.5p: a bare attached noncreature permanent that is
    // neither Aura, Equipment, nor Fortification is unattached by SBAs, so the
    // Equipment subtype is load-bearing for the fixture.
    let equipment = scenario
        .add_creature(P0, "Bone Saw", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .id();

    let mut runner = scenario.build();
    attach(&mut runner, equipment, drone);

    advance_to_declare_attackers(&mut runner);
    assert!(
        runner
            .declare_attackers(&[(drone, AttackTarget::Player(P1))])
            .is_ok(),
        "an EQUIPPED Training Drone must be a legal attacker — the \
         `unless it's equipped` gate is met, so the restriction does not apply"
    );
}

// ---------------------------------------------------------------------------
// 3. Ancestral Katana — the quoted granted ability and its own gate
// ---------------------------------------------------------------------------

/// CR 611.3a + CR 508.1k: the ability written in quotation marks is a separate
/// static granted to the equipped creature, and ITS gate ("as long as it's
/// attacking") is evaluated against the EQUIPPED CREATURE, not against the
/// Equipment and not against the +2/+2 grant.
///
/// Discriminating: revert any one of U3's three ` as long as ` quote guards and
/// the predicate is split inside the quotation marks — the granted first strike
/// is destroyed entirely and its gate is mis-attached to the +2/+2 — so the
/// attacking case has no first strike to find. The `has_kw(... FirstStrike)`
/// assertion under `state.combat` is what flips.
///
/// The not-attacking case is the paired reach-guard: a grant that applied
/// unconditionally (or never applied at all) would make one of the two halves
/// vacuous, and the pair rules both out.
#[test]
fn ancestral_katana_granted_first_strike_binds_to_equipped_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Deliberately NOT a Samurai or Warrior: the Equipment's own attack trigger
    // ("Whenever a Samurai or Warrior you control attacks alone") must not fire
    // and add an unrelated prompt to this fixture.
    let bearer = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
    let katana = scenario
        .add_creature(P0, "Ancestral Katana", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(ANCESTRAL_KATANA)
        .id();

    let mut runner = scenario.build();
    attach(&mut runner, katana, bearer);

    // Not attacking: the granted static's own gate is FALSE, so no first strike.
    assert!(
        !has_kw(&mut runner, bearer, &Keyword::FirstStrike),
        "the granted first strike is gated on `as long as it's attacking`; a \
         non-attacking equipped creature must NOT have it"
    );
    // The +2/+2 half of the same line must apply unconditionally — the gate
    // belongs to the QUOTED ability, not to the P/T grant. Without this the
    // not-attacking assertion above is also satisfied by a line that parsed to
    // nothing at all.
    assert_eq!(
        runner.state().objects[&bearer].power,
        Some(4),
        "the +2/+2 grant is ungated and must apply to the equipped creature \
         whether or not it is attacking"
    );

    // Attacking: CR 508.1k — the equipped creature is now an attacking
    // creature, so the granted static's gate is TRUE.
    runner.state_mut().combat = Some(CombatState {
        attackers: vec![AttackerInfo::attacking_player(bearer, P1)],
        ..Default::default()
    });
    assert!(
        has_kw(&mut runner, bearer, &Keyword::FirstStrike),
        "an ATTACKING equipped creature must have the granted first strike \
         (CR 611.3a: the granted static's `it` names the equipped creature)"
    );
}
