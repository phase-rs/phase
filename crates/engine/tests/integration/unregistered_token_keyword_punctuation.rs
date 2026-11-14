//! Unregistered named-token punctuation regression.
//!
//! This deliberately uses an unregistered token name. Unlike The Void, the
//! token registry cannot fill in missing characteristics, so the full cast →
//! trigger → token creation → combat path proves that the parser preserved the
//! keyword list before a following quoted rules sentence.

use engine::game::combat::{AttackTarget, CombatRequirement};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;

const REGRESSION_SENTRY_ORACLE: &str = "When Regression Sentry enters, target opponent creates Regression Nullwatch, a legendary 5/5 black Horror Villain creature token with flying, indestructible, and \"Regression Nullwatch attacks each combat if able.\"";

#[test]
fn unregistered_named_token_keeps_keywords_and_forced_attack_through_combat() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario.add_card_to_library_top(P1, "P1 Draw-Step Filler");
    let sentry = scenario
        .add_creature_to_hand(P0, "Regression Sentry", 5, 5)
        .as_legendary()
        .with_subtypes(vec!["Human", "Hero"])
        .from_oracle_text_with_keywords(&[], REGRESSION_SENTRY_ORACLE)
        .id();

    let mut runner = scenario.build();
    runner.cast(sentry).target_player(P1).resolve();

    let tokens: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|object| {
            object.is_token
                && object.zone == engine::types::zones::Zone::Battlefield
                && object.name == "Regression Nullwatch"
        })
        .collect();
    assert_eq!(tokens.len(), 1, "the trigger must create one token");
    let token = tokens[0];
    assert_eq!(token.owner, P1);
    assert_eq!(token.controller, P1);
    assert!(
        token.has_keyword(&Keyword::Flying),
        "Flying must come from the parsed token clause"
    );
    assert!(
        token.has_keyword(&Keyword::Indestructible),
        "Indestructible must survive the comma before `and`"
    );
    assert!(
        token
            .static_definitions
            .as_slice()
            .iter()
            .any(|definition| definition.mode == engine::types::statics::StaticMode::MustAttack),
        "the quoted rule must materialize as a MustAttack static"
    );
    let token_id = token.id;

    runner.advance_to_upkeep();
    assert_eq!(runner.state().active_player, P1);
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
            "advance_to_combat must produce DeclareAttackers, got {:?}",
            runner.state().waiting_for.variant_name()
        );
    };
    assert_eq!(*player, P1);
    assert!(valid_attacker_ids.contains(&token_id));
    assert!(valid_attack_targets.contains(&AttackTarget::Player(P0)));
    assert_eq!(
        attacker_constraints.get(&token_id),
        Some(&CombatRequirement::MustAttack {
            defenders: vec![],
            sources: vec![token_id],
        })
    );

    assert!(
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            })
            .is_err(),
        "the engine must reject an empty declaration while the token can attack"
    );
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(token_id, AttackTarget::Player(P0))],
            bands: vec![],
        })
        .expect("attacking P0 must satisfy the token's requirement");
}
