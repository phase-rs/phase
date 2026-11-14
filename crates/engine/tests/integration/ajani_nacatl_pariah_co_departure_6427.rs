//! Regression for GitHub issue #6427 — Ajani, Nacatl Pariah's transform trigger
//! when Ajani co-departs with the dying Cat.
//!
//! Oracle (Ajani): "Whenever one or more other Cats you control die, you may
//! exile Ajani, then return him to the battlefield transformed under his
//! owner's control."
//!
//! When Ajani and another Cat die in the same simultaneous event (mass removal),
//! the trigger must NOT exile/return/transform Ajani — he stays in the
//! graveyard, untransformed. The co-departure mis-latch of `expected_zone` to
//! Graveyard must not let `SelfRef` early-bind.
//!
//! CR 400.7e: co-departure must not let SelfRef bind Ajani in the graveyard when
//! the trigger event is the Cat's departure — only his own departure successor may bind.
//! CR 603.10a: Ajani's trigger source retains battlefield last-known information.
//! CR 400.7 + CR 608.2d: the card in the graveyard is a new object, so "exile
//! Ajani" is impossible and the "you may" is not offered at all.
//!
//! Harness note: use `cast(...).commit()` (not `.resolve()`) and drive priority
//! by hand, so no cast driver auto-answers an `OptionalEffectChoice` before the
//! test can observe whether one was offered.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const WRATH: &str = "Destroy all creatures.";
const MURDER: &str = "Destroy target creature.";

/// Pass priority until the committed `spell` has left the stack, stopping with
/// any triggers its resolution caused still on the stack.
fn resolve_spell_only(runner: &mut GameRunner, spell: ObjectId) {
    for _ in 0..12 {
        if !runner.state().stack.iter().any(|entry| entry.id == spell) {
            return;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "the spell resolves through priority passes, got {:?}",
            runner.state().waiting_for
        );
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
    panic!("the spell never resolved");
}

/// Ajani's triggered abilities currently on the stack.
fn stacked_ajani_triggers(runner: &GameRunner, ajani: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            entry.source_id == ajani
                && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
        })
        .count()
}

fn assert_settled_priority(runner: &GameRunner, why: &str) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{why}: expected Priority with no prompt, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "{why}: the stack must be empty"
    );
}

/// Ajani + Savannah Lions die together via Wrath of God (simultaneous batch).
/// The Cat's death triggers Ajani, but the Ajani card in the graveyard is a new
/// object (CR 400.7), so exiling him is impossible and the trigger resolves
/// without offering its "you may" (CR 608.2d), leaving Ajani in the graveyard,
/// untransformed.
#[test]
fn ajani_stays_in_graveyard_when_co_departing_with_dying_cat() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ajani = scenario.add_real_card(P0, "Ajani, Nacatl Pariah", Zone::Battlefield, db);
    let _lion = scenario.add_real_card(P0, "Savannah Lions", Zone::Battlefield, db);
    let wrath = scenario
        .add_spell_to_hand_from_oracle(P0, "Wrath of God", false, WRATH)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    {
        let obj = runner.state().objects.get(&ajani).expect("Ajani exists");
        assert!(
            !obj.transformed,
            "precondition: Ajani starts front-face on the battlefield"
        );
    }

    runner.cast(wrath).commit();
    resolve_spell_only(&mut runner, wrath);
    assert_eq!(
        runner.state().objects[&ajani].zone,
        Zone::Graveyard,
        "reach: Wrath destroyed Ajani alongside the Cat"
    );
    assert_eq!(
        stacked_ajani_triggers(&runner, ajani),
        1,
        "reach: the Cat's death put Ajani's trigger on the stack"
    );

    // Only priority is passed: an offered "you may" would stop the driver
    // with the trigger unresolved.
    runner.advance_until_stack_empty();
    assert_settled_priority(
        &runner,
        "co-departure: exiling the Ajani card in the graveyard is impossible, so the trigger \
         resolves without offering its optional",
    );

    let ajani_obj = runner
        .state()
        .objects
        .get(&ajani)
        .expect("Ajani object still exists");
    assert_eq!(
        ajani_obj.zone,
        Zone::Graveyard,
        "co-departure: Ajani must stay in the graveyard when the SelfRef mis-latch gate fails"
    );
    assert!(
        !ajani_obj.transformed,
        "co-departure: Ajani must remain untransformed"
    );
}

/// Feasible twin of the co-departure case: when only the Cat dies, Ajani is
/// still on the battlefield, so the same trigger still offers its optional
/// (CR 608.2d forbids only impossible choices). Declining it leaves Ajani on
/// the battlefield as the same object, untransformed.
#[test]
fn ajani_trigger_is_still_offered_and_declinable_when_only_the_cat_dies() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ajani = scenario.add_real_card(P0, "Ajani, Nacatl Pariah", Zone::Battlefield, db);
    let lion = scenario.add_real_card(P0, "Savannah Lions", Zone::Battlefield, db);
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", false, MURDER)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    let incarnation = runner.state().objects[&ajani].incarnation;

    runner.cast(murder).target_object(lion).commit();
    resolve_spell_only(&mut runner, murder);
    assert_eq!(
        runner.state().objects[&lion].zone,
        Zone::Graveyard,
        "reach: the Cat died"
    );
    assert_eq!(
        stacked_ajani_triggers(&runner, ajani),
        1,
        "reach: the Cat's death put Ajani's trigger on the stack"
    );

    runner.advance_until_stack_empty();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: P0, source_id, .. } if source_id == ajani
        ),
        "Ajani on the battlefield can be exiled, so the optional is offered, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline Ajani's optional exile/return trigger");
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "after declining Ajani's optional");

    let ajani_obj = runner
        .state()
        .objects
        .get(&ajani)
        .expect("Ajani object still exists");
    assert_eq!(
        ajani_obj.zone,
        Zone::Battlefield,
        "declined trigger: Ajani stays on the battlefield"
    );
    assert_eq!(
        ajani_obj.incarnation, incarnation,
        "declined trigger: Ajani never left the battlefield"
    );
    assert!(
        !ajani_obj.transformed,
        "declined trigger: Ajani stays untransformed"
    );
}
