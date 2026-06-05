//! Regression test for GitHub issue #503 — Ajani, Nacatl Pariah's transform
//! trigger.
//!
//! Ajani, Nacatl Pariah's second trigger is "Whenever one or more other Cats
//! you control die, you may exile Ajani, then return him to the battlefield
//! transformed under his owner's control."
//!
//! The reported bug: when another Cat died, Ajani exiled itself but never
//! returned transformed. Root cause was a parser anaphor mis-binding — the
//! pronoun "him" in clause 2 ("return him ... transformed") bound to the
//! triggering source (the dying Cat) instead of to Ajani, the object named by
//! `~` in clause 1 ("exile ~"). At resolution the engine then moved/transform-
//! no-op'd the dead Cat and left Ajani stranded in exile.
//!
//! The fix is parser-only: a guarded post-clause anaphoric-rewrite arm in
//! `parse_effect_chain_ir` rebinds the pronoun to `SelfRef` when a preceding
//! clause named the source via `~`. This test drives the real `apply`
//! pipeline (kill a Cat → trigger fires → resolve) and asserts Ajani returns
//! to the battlefield transformed to his Avenger (planeswalker) back face.
//!
//! CR 608.2c: "read the whole text ... apply the rules of English" — the
//! anaphor "him" binds to the named antecedent in the preceding clause.
//! CR 712.14 / 712.14a: a double-faced card put onto the battlefield
//! transformed enters with its back face up.

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// Drive the full pipeline: a Cat dies → Ajani's trigger fires → the player
/// accepts the optional "you may" → Ajani returns to the battlefield
/// transformed to his planeswalker back face.
#[test]
fn ajani_nacatl_pariah_returns_transformed_when_another_cat_dies() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Ajani, Nacatl Pariah on P0's battlefield (front face — a 1/2 Cat
    // creature). Placed directly, so its own ETB token trigger does not fire.
    let ajani = scenario.add_real_card(P0, "Ajani, Nacatl Pariah", Zone::Battlefield, db);
    // A second Cat under P0 — Savannah Lions is a 2/1 Cat. Killing it satisfies
    // "one or more other Cats you control die".
    let lion = scenario.add_real_card(P0, "Savannah Lions", Zone::Battlefield, db);
    // P0 holds a Lightning Bolt to kill its own Cat at sorcery speed.
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    scenario.with_life(P0, 20);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Precondition: Ajani is a front-face creature (not yet transformed).
    {
        let obj = runner.state().objects.get(&ajani).expect("Ajani exists");
        assert!(!obj.transformed, "precondition: Ajani starts front-face");
        assert!(
            obj.card_types.core_types.contains(&CoreType::Creature),
            "precondition: Ajani's front face is a creature"
        );
        assert!(
            obj.back_face.is_some(),
            "precondition: Ajani's DFC back face must be hydrated"
        );
    }

    // Fund P0's pool with {R} for Lightning Bolt.
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool
        .add(ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]));

    // P0 bolts its own Savannah Lions — 3 damage kills the 2/1 via SBA, which
    // fires Ajani's "other Cats die" trigger.
    let bolt_card_id = runner.state().objects[&bolt].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            object_id: bolt,
            card_id: bolt_card_id,
            targets: vec![],
        })
        .expect("cast Lightning Bolt");
    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(lion)],
            })
            .expect("target Savannah Lions");
    }

    // Resolve until the stack settles, accepting any optional "you may"
    // decision (Ajani's trigger is `optional: true`).
    loop {
        runner.advance_until_stack_empty();
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept Ajani's optional exile/return trigger");
            }
            _ => break,
        }
    }

    // The Cat died.
    assert_eq!(
        runner.state().objects.get(&lion).unwrap().zone,
        Zone::Graveyard,
        "Savannah Lions must be destroyed by lethal damage"
    );

    // The fix's payload: Ajani is back on the battlefield, transformed to his
    // planeswalker back face (Ajani, Nacatl Avenger). With the parser fix
    // reverted, clause 2 targets the dead Cat and Ajani stays in exile —
    // this assertion fails. (See mutation-check in the report.)
    let ajani_obj = runner
        .state()
        .objects
        .get(&ajani)
        .expect("Ajani object still exists");
    assert_eq!(
        ajani_obj.zone,
        Zone::Battlefield,
        "Ajani must return to the battlefield (CR 608.2c: 'him' binds to Ajani, \
         the source named by clause 1's `exile ~`)"
    );
    assert!(
        ajani_obj.transformed,
        "Ajani must enter transformed (CR 712.14)"
    );
    assert!(
        ajani_obj
            .card_types
            .core_types
            .contains(&CoreType::Planeswalker),
        "transformed Ajani must show his Avenger back face (a planeswalker), \
         got core types {:?}",
        ajani_obj.card_types.core_types
    );
    assert!(
        ajani_obj.loyalty.is_some(),
        "the planeswalker back face must carry loyalty"
    );
}
