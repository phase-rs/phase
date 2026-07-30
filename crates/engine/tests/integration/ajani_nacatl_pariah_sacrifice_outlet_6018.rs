//! Regression for GitHub issue #6018 — Ajani, Nacatl Pariah's transform trigger
//! when another Cat dies via a sacrifice outlet.
//!
//! Discord repro board: Ajani, Nacatl Pariah + Cat Warrior token + Goblin
//! Bombardment + Guide of Souls.
//!
//! Oracle (Ajani): "Whenever one or more other Cats you control die, you may
//! exile Ajani, then return him to the battlefield transformed under his
//! owner's control."
//!
//! The #503 regression covered a Lightning Bolt kill of a nontoken Cat; this
//! test drives the sacrifice-outlet path (Goblin Bombardment: "Sacrifice a
//! creature: This enchantment deals 1 damage to any target") so dies-triggers
//! collected during the activation's sacrifice cost still reach the stack and
//! Ajani's optional exile/return chain resolves with `him` bound to Ajani.
//!
//! CR 608.2c: the anaphor "him" in clause 2 binds to Ajani (`exile ~` in
//! clause 1), not the sacrificed Cat.
//! CR 712.14a: a double-faced card put onto the battlefield transformed
//! enters with its back face up.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{PayCostKind, WaitingFor};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;
const GOBLIN_BOMBARDMENT: &str =
    "Sacrifice a creature: This enchantment deals 1 damage to any target.";

/// Sacrificing Ajani's Cat Warrior token to Goblin Bombardment must still flip
/// Ajani to his planeswalker back face when the optional trigger is accepted.
#[test]
fn ajani_returns_transformed_when_cat_token_sacrificed_to_outlet() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ajani = scenario.add_real_card(P0, "Ajani, Nacatl Pariah", Zone::Battlefield, db);
    let _guide = scenario.add_real_card(P0, "Guide of Souls", Zone::Battlefield, db);
    let bombardment = scenario
        .add_creature(P0, "Goblin Bombardment", 0, 0)
        .as_enchantment()
        .from_oracle_text(GOBLIN_BOMBARDMENT)
        .id();

    // Ajani's 2/1 white Cat Warrior token from his ETB ability.
    let cat_token = scenario
        .add_creature(P0, "Cat Warrior", 2, 1)
        .with_subtypes(vec!["Cat", "Warrior"])
        .id();

    scenario.add_basic_land(P0, ManaColor::Red);

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&cat_token)
        .unwrap()
        .is_token = true;
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Precondition: Ajani is front-face on the battlefield.
    {
        let obj = runner.state().objects.get(&ajani).expect("Ajani exists");
        assert!(!obj.transformed, "precondition: Ajani starts front-face");
        assert!(
            obj.back_face.is_some(),
            "precondition: Ajani's DFC back face must be hydrated"
        );
    }

    runner
        .act(GameAction::ActivateAbility {
            source_id: bombardment,
            ability_index: 0,
        })
        .expect("activating Goblin Bombardment must succeed");

    let mut sacrificed = false;
    let mut targeted = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                choices,
                ..
            } => {
                assert!(
                    choices.contains(&cat_token),
                    "the Cat Warrior token must be a legal sacrifice"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![cat_token],
                    })
                    .expect("sacrificing the Cat Warrior token must succeed");
                sacrificed = true;
            }
            WaitingFor::TargetSelection { target_slots, .. } => {
                assert!(
                    target_slots[0]
                        .legal_targets
                        .contains(&TargetRef::Player(P1)),
                    "the opponent must be a legal damage target"
                );
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Player(P1)],
                    })
                    .expect("targeting the opponent must succeed");
                targeted = true;
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept Ajani's optional exile/return trigger");
            }
            _ => {
                runner.advance_until_stack_empty();
                if !runner.state().stack.is_empty() {
                    continue;
                }
                if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
                    break;
                }
            }
        }
    }

    assert!(
        sacrificed,
        "must have paid Goblin Bombardment's sacrifice cost"
    );
    assert!(targeted, "must have selected a damage target");

    // CR 111.2: tokens cease to exist in the graveyard — the object is removed
    // from `state.objects` rather than lingering with `zone == Graveyard`.
    assert!(
        runner.state().objects.get(&cat_token).is_none()
            || runner
                .state()
                .objects
                .get(&cat_token)
                .is_some_and(|o| o.zone != Zone::Battlefield),
        "the Cat Warrior token must no longer be on the battlefield"
    );

    let ajani_obj = runner
        .state()
        .objects
        .get(&ajani)
        .expect("Ajani object still exists");
    assert_eq!(
        ajani_obj.zone,
        Zone::Battlefield,
        "Ajani must return to the battlefield (CR 608.2c: 'him' binds to Ajani)"
    );
    assert!(
        ajani_obj.transformed,
        "Ajani must enter transformed (CR 712.14a)"
    );
    assert!(
        ajani_obj
            .card_types
            .core_types
            .contains(&CoreType::Planeswalker),
        "transformed Ajani must show his Avenger back face, got {:?}",
        ajani_obj.card_types.core_types
    );
    assert!(
        ajani_obj.loyalty.is_some(),
        "the planeswalker back face must carry loyalty"
    );
}
