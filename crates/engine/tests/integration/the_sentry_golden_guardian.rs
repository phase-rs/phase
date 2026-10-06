//! The Sentry, Golden Guardian — opponent-created The Void token regression.
//!
//! The parser unit regression covers the catalog-independent token grammar. This
//! integration test drives the real source card through casting, its targeted
//! ETB trigger, the opponent's next combat, and declaration validation.

use engine::game::combat::{AttackTarget, CombatRequirement};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;

const SENTRY_ORACLE: &str = "Flying, vigilance, indestructible\nWhen The Sentry enters, target opponent creates The Void, a legendary 5/5 black Horror Villain creature token with flying, indestructible, and \"The Void attacks each combat if able.\"";

#[test]
fn the_sentry_creates_the_void_with_full_token_body_and_forced_attack() {
    let mut scenario = GameScenario::new();
    // Cast in postcombat main so `advance_to_upkeep()` crosses cleanup into
    // P1's turn rather than stopping at P0's declare-attackers step.
    scenario.at_phase(Phase::PostCombatMain);
    // Supply P1's sole draw before declare attackers to avoid an unrelated
    // empty-library game loss during the natural turn transition.
    scenario.add_card_to_library_top(P1, "P1 Draw-Step Filler");
    let sentry = scenario
        .add_creature_to_hand(P0, "The Sentry, Golden Guardian", 5, 5)
        .as_legendary()
        .with_subtypes(vec!["Human", "Hero"])
        .from_oracle_text_with_keywords(&["Flying", "Vigilance", "Indestructible"], SENTRY_ORACLE)
        .id();

    let mut runner = scenario.build();
    runner.cast(sentry).target_player(P1).resolve();

    let voids: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|object| {
            object.is_token
                && object.zone == engine::types::zones::Zone::Battlefield
                && object.name == "The Void"
        })
        .collect();
    assert_eq!(
        voids.len(),
        1,
        "The Sentry must create exactly one The Void"
    );
    let void = voids[0];
    assert_eq!(
        void.owner, P1,
        "the chosen opponent creates and owns The Void"
    );
    assert_eq!(
        void.controller, P1,
        "the chosen opponent controls the created token"
    );
    // These materialized characteristics are catalog-backed for this named
    // token; the parser unit test above independently proves their provenance.
    assert!(void.has_keyword(&Keyword::Flying));
    assert!(void.has_keyword(&Keyword::Indestructible));
    assert!(
        void.static_definitions
            .as_slice()
            .iter()
            .any(|definition| definition.mode == engine::types::statics::StaticMode::MustAttack),
        "The Void's quoted rule must materialize as a MustAttack static"
    );
    let void_id = void.id;

    // Complete P0's remaining turn through the real turn driver, so P1 begins
    // their turn naturally and The Void no longer has summoning sickness.
    runner.advance_to_upkeep();
    assert_eq!(
        runner.state().active_player,
        P1,
        "P1 must begin the next turn"
    );
    runner.advance_to_combat();

    let engine::types::game_state::WaitingFor::DeclareAttackers {
        player,
        valid_attacker_ids,
        valid_attack_targets,
        attacker_constraints,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "advance_to_combat must produce the engine declare-attackers prompt, got {:?}",
            runner.state().waiting_for.variant_name()
        );
    };
    assert_eq!(*player, P1, "P1 must declare attackers on their turn");
    assert!(
        valid_attacker_ids.contains(&void_id),
        "The Void must be an eligible attacker in the production prompt"
    );
    assert!(
        valid_attack_targets.contains(&AttackTarget::Player(P0)),
        "P0 must be an attackable defender for P1"
    );
    assert_eq!(
        attacker_constraints.get(&void_id),
        Some(&CombatRequirement::MustAttack {
            defenders: vec![],
            sources: vec![void_id],
        }),
        "The Void must carry its intrinsic generic MustAttack requirement"
    );

    assert!(
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            })
            .is_err(),
        "an empty declaration must be rejected while The Void can attack"
    );
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(void_id, AttackTarget::Player(P0))],
            bands: vec![],
        })
        .expect("The Void attacking P0 must satisfy its requirement");
    assert!(
        runner.state().combat.as_ref().is_some_and(|combat| combat
            .attackers
            .iter()
            .any(|attacker| attacker.object_id == void_id)),
        "the accepted declaration must commit The Void as an attacker"
    );
}
