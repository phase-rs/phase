//! Intrinsic permanent statics that previously mis-routed into a kind:"Spell"
//! GenericEffect (never registered with the layer system) now land as
//! top-level `static_abilities` and resolve through the real continuous-effect
//! layer pipeline.
//!
//! Gap A — "All <subtype> have <keyword>" universal-quantifier grant
//! (Crystalline Sliver: "All Slivers have shroud."). CR 205.3 (subtypes),
//! CR 604.1 (static abilities are continuously true), CR 702.18a (Shroud).
//!
//! Gap B — self/typed "can't be blocked except by <filter>" evasion static.
//! CR 509.1b (the defending player checks each creature for block
//! restrictions). The restriction must be live in declare-blockers, so it has
//! to be a top-level continuous static, not a spell-resolution effect.
//!
//! These tests drive the REAL pipeline through `apply()` (priority passing /
//! declare-attackers-blockers) — they never hand-construct expected state. The
//! assertions read the post-layer keyword set / the engine's own
//! `find_legal_targets` and block-legality validation.

use engine::game::layers::{flush_layers, mark_layers_full};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::targeting::find_legal_targets;
use engine::types::ability::{TargetFilter, TargetRef, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

/// Crystalline Sliver's printed static (reminder text stripped by the parser).
const CRYSTALLINE_SLIVER: &str = "All Slivers have shroud.";

/// CR 205.3 + CR 604.1 + CR 702.18a: Crystalline Sliver's "All Slivers have
/// shroud" must grant Shroud to OTHER Slivers on the battlefield through the
/// continuous layer system, and that Shroud must make the affected Sliver
/// untargetable by an opponent's spell.
#[test]
fn crystalline_sliver_grants_shroud_to_other_slivers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _crystalline = scenario
        .add_creature_from_oracle(P0, "Crystalline Sliver", 1, 1, CRYSTALLINE_SLIVER)
        .with_subtypes(vec!["Sliver"])
        .id();
    let other_sliver = scenario
        .add_creature(P0, "Muscle Sliver", 2, 2)
        .with_subtypes(vec!["Sliver"])
        .id();
    // A non-Sliver control creature proves the grant is subtype-selective.
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    // Run the engine's own continuous-effect layer pipeline (the same
    // `evaluate_layers` path `apply()` invokes) so the continuous Shroud grant
    // is written onto the effective keyword set. We mark layers full + flush
    // rather than hand-construct the result.
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());

    let other = &runner.state().objects[&other_sliver];
    assert!(
        other.has_keyword(&Keyword::Shroud),
        "the other Sliver should have Shroud granted by Crystalline Sliver, \
         got keywords {:?}",
        other.keywords
    );
    assert!(
        !runner.state().objects[&bear].has_keyword(&Keyword::Shroud),
        "a non-Sliver creature must NOT receive the Sliver-only Shroud grant"
    );

    // Discriminating: an opponent (P1) targeting a creature on the battlefield
    // must not be able to select the shrouded Sliver. Source id is irrelevant
    // here (any opponent source is barred by Shroud — CR 702.18a).
    let creature_filter = TargetFilter::Typed(TypedFilter::creature());
    let legal_for_opponent =
        find_legal_targets(runner.state(), &creature_filter, P1, ObjectId(9999));
    assert!(
        !legal_for_opponent.contains(&TargetRef::Object(other_sliver)),
        "the shrouded Sliver must not be a legal target for an opponent's spell"
    );
    // Sanity: the non-Sliver bear (no Shroud) IS still targetable by the opponent.
    assert!(
        legal_for_opponent.contains(&TargetRef::Object(bear)),
        "the un-shrouded bear should remain a legal opponent target"
    );
}

/// Synthetic self-referential evasion static: "~ can't be blocked except by
/// creatures with flying." Mirrors the Crystalline-class root cause for Gap B.
const EXCEPT_BY_FLYING: &str = "~ can't be blocked except by creatures with flying.";

/// CR 509.1b + CR 604.1: An attacker with "can't be blocked except by creatures
/// with flying" must reject a non-flying blocker in declare-blockers and accept
/// a flying one — driven through the real combat pipeline via `apply()`.
#[test]
fn cant_be_blocked_except_by_flying_rejects_nonflying_blocker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let attacker = scenario
        .add_creature_from_oracle(P0, "Evasive Striker", 2, 2, EXCEPT_BY_FLYING)
        .id();
    let ground_blocker = scenario.add_creature(P1, "Ground Wall", 0, 4).id();
    let flying_blocker = {
        let mut b = scenario.add_creature(P1, "Air Wall", 0, 4);
        b.flying();
        b.id()
    };

    let mut runner = scenario.build();
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
        })
        .expect("attacker should be able to attack");

    // Advance to the declare-blockers window (drain any attack-trigger stack).
    for _ in 0..20 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ) {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ),
        "expected to reach declare-blockers, got {:?}",
        runner.state().waiting_for
    );

    // CR 509.1b: a non-flying blocker is an illegal block — the declaration
    // must be rejected by the engine's own validation.
    let illegal = runner.act(GameAction::DeclareBlockers {
        assignments: vec![(ground_blocker, attacker)],
    });
    assert!(
        illegal.is_err(),
        "a non-flying creature must NOT be allowed to block a \
         'can't be blocked except by creatures with flying' attacker"
    );
    // The attacker is still attacking (declaration was rejected, not committed).
    assert_eq!(runner.state().objects[&attacker].zone, Zone::Battlefield);

    // A flying blocker IS a legal block.
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(flying_blocker, attacker)],
        })
        .expect("a flying creature should be a legal blocker for the evasion attacker");
}
