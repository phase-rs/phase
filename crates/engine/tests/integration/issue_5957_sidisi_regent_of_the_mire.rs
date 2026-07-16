//! Regression for issue #5957: Sidisi, Regent of the Mire must reanimate a
//! creature card whose mana value is exactly one greater than the sacrificed
//! creature's mana value, not equal to it.
//!
//! https://github.com/phase-rs/phase/issues/5957
//!
//! Oracle text (verified via Scryfall): "{T}, Sacrifice a creature you
//! control with mana value X other than Sidisi: Return target creature card
//! with mana value X plus 1 from your graveyard to the battlefield. Activate
//! only as a sorcery." The parser previously dropped the "plus 1" and emitted
//! a target filter requiring mana value == X, matching the sacrificed
//! creature's own mana value instead of one greater (CR 202.3). Separately,
//! the "with mana value X" sacrifice-cost filter (X bound by the chosen
//! permanent, not a prior announcement — the Shoal pattern) was never
//! relaxed for activation-cost eligibility, so the ability could not even be
//! activated; this test also covers that fix.
//!
//! Loads real Sidisi card data (parser → cast pipeline) and asserts that,
//! after sacrificing a mana-value-2 creature, only mana-value-3 graveyard
//! creatures are offered as legal targets — not the mana-value-2 one.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{AbilityKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{PayCostKind, WaitingFor};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn load_db() -> Option<&'static CardDatabase> {
    static DB: std::sync::OnceLock<Option<CardDatabase>> = std::sync::OnceLock::new();
    DB.get_or_init(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/issue_5957_cards.json");
        CardDatabase::from_export(&path)
            .expect("issue_5957_cards.json fixture must load")
            .into()
    })
    .as_ref()
}

#[test]
fn sidisi_reanimates_creature_one_mana_value_greater_than_sacrificed() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let sidisi = scenario.add_real_card(P0, "Sidisi, Regent of the Mire", Zone::Battlefield, db);

    // Sacrifice fodder with mana value 2 — this defines X = 2.
    let fodder = scenario
        .add_creature(P0, "Fodder", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();

    // Mana value 3 — a correct target (X plus 1 = 3).
    let correct_target = scenario
        .add_creature_to_graveyard(P0, "Correct Target", 3, 3)
        .with_mana_cost(ManaCost::generic(3))
        .id();

    // Mana value 2 — same as the sacrificed creature. Under the pre-fix
    // parse (mana value == X) this was wrongly offered as a legal target.
    let wrong_target = scenario
        .add_creature_to_graveyard(P0, "Wrong Target", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();

    // A second mana-value-3 creature keeps target selection genuinely
    // interactive: with only one legal target the engine auto-selects it and
    // skips the `TargetSelection` round trip this test exercises.
    let _other_correct_target = scenario
        .add_creature_to_graveyard(P0, "Other Correct Target", 3, 3)
        .with_mana_cost(ManaCost::generic(3))
        .id();

    let mut runner = scenario.build();
    let ability_index = runner.state().objects[&sidisi]
        .abilities
        .iter()
        .position(|ability| matches!(ability.kind, AbilityKind::Activated))
        .expect("activated ability");

    runner
        .act(GameAction::ActivateAbility {
            source_id: sidisi,
            ability_index,
        })
        .expect("begin activation");

    let mut saw_sacrifice = false;
    let mut saw_target = false;

    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![fodder],
                    })
                    .expect("sacrifice mana-value-2 fodder");
                saw_sacrifice = true;
            }
            WaitingFor::TargetSelection { ref selection, .. } => {
                assert!(
                    selection
                        .current_legal_targets
                        .contains(&TargetRef::Object(correct_target)),
                    "the mana-value-3 graveyard creature must be a legal target"
                );
                assert!(
                    !selection
                        .current_legal_targets
                        .contains(&TargetRef::Object(wrong_target)),
                    "the mana-value-2 graveyard creature (same as the sacrificed \
                     creature) must NOT be a legal target — the ability requires \
                     mana value X plus 1, not X"
                );
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(correct_target)],
                    })
                    .expect("select mana-value-3 graveyard creature");
                saw_target = true;
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(correct_target)],
                    })
                    .expect("select mana-value-3 graveyard creature");
                saw_target = true;
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to resolve ability");
            }
            other => panic!("unexpected waiting state during activation: {other:?}"),
        }
    }

    assert!(
        saw_sacrifice,
        "activation must require sacrificing a creature"
    );
    assert!(
        saw_target,
        "activation must require choosing a graveyard creature"
    );
    assert_eq!(
        runner.state().objects[&correct_target].zone,
        Zone::Battlefield,
        "the mana-value-3 creature must be reanimated"
    );
    assert_eq!(
        runner.state().objects[&wrong_target].zone,
        Zone::Graveyard,
        "the mana-value-2 creature must remain in the graveyard"
    );
    assert_eq!(
        runner.state().objects[&fodder].zone,
        Zone::Graveyard,
        "the sacrificed creature must be in the graveyard"
    );
}
