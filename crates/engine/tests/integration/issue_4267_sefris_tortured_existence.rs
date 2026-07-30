//! Issue #4267 — Sefris of the Hidden Ways must trigger when Tortured
//! Existence's discard cost puts a creature card into your graveyard.
//!
//! https://github.com/phase-rs/phase/issues/4267

use engine::game::mana_sources::activatable_mana_actions_for_player;
use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{PayCostKind, WaitingFor};
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;

const SEFRIS_ORACLE: &str = "Whenever one or more creature cards are put into your graveyard from anywhere, venture into the dungeon. This ability triggers only once each turn.";
const TORTURED_EXISTENCE_ORACLE: &str =
    "{B}, Discard a creature card: Return target creature card from your graveyard to your hand.";

#[test]
fn tortured_existence_parses_mana_and_discard_activation_cost() {
    use engine::parser::oracle::parse_oracle_text;
    use engine::types::ability::AbilityCost;

    let doc = parse_oracle_text(
        TORTURED_EXISTENCE_ORACLE,
        "Tortured Existence",
        &[],
        &[],
        &[],
    );
    let ability = doc
        .abilities
        .iter()
        .find(|a| matches!(a.kind, engine::types::ability::AbilityKind::Activated))
        .expect("activated ability");
    match ability.cost.as_ref().expect("activation cost") {
        AbilityCost::Composite { costs } => {
            assert!(
                costs.iter().any(|c| matches!(c, AbilityCost::Mana { .. })),
                "expected mana leg: {costs:?}"
            );
            assert!(
                costs
                    .iter()
                    .any(|c| matches!(c, AbilityCost::Discard { .. })),
                "expected discard leg: {costs:?}"
            );
        }
        other => panic!("expected composite cost, got {other:?}"),
    }
}

#[test]
fn sefris_ventures_when_tortured_existence_discards_creature_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Sefris of the Hidden Ways", 3, 4, SEFRIS_ORACLE)
        .id();

    let gy_creature = scenario
        .add_creature_to_graveyard(P0, "Graveyard Bear", 2, 2)
        .id();
    let discard_creature = scenario.add_creature_to_hand(P0, "Hand Bear", 2, 2).id();

    let tortured = scenario
        .add_creature(P0, "Tortured Existence", 0, 0)
        .as_enchantment()
        .from_oracle_text(TORTURED_EXISTENCE_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    // Play a Swamp so {B} is paid in a separate action after the discard cost
    // (no pre-funded pool). Regression for issue #4267: discard-for-cost zone
    // events must survive a ManaPayment pause.
    let black_mana_source = scenario.add_basic_land(P0, ManaColor::Black);

    let mut runner = scenario.build();
    assert!(
        runner
            .state()
            .dungeon_progress
            .get(&P0)
            .and_then(|p| p.current_dungeon)
            .is_none(),
        "precondition: no dungeon marker before the trigger"
    );

    runner
        .act(GameAction::ActivateAbility {
            source_id: tortured,
            ability_index: 0,
        })
        .expect("announce Tortured Existence");

    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(engine::types::ability::TargetRef::Object(gy_creature)),
                    })
                    .expect("choose graveyard return target");
            }
            WaitingFor::PayCost {
                kind: PayCostKind::Discard,
                ..
            } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![discard_creature],
                    })
                    .expect("discard creature card to pay activation cost");
            }
            WaitingFor::ManaPayment { .. } => {
                let action = activatable_mana_actions_for_player(
                    runner.state(),
                    runner
                        .state()
                        .waiting_for
                        .acting_player()
                        .expect("acting player"),
                )
                .into_iter()
                .find(|action| {
                    matches!(action, GameAction::TapLandForMana { selection }
                        if selection.source.object_id == black_mana_source)
                })
                .expect("Swamp must expose semantic mana action");
                runner.act(action).expect("tap Swamp for {B}");
                runner
                    .act(GameAction::PassPriority)
                    .expect("pay {B} from tapped land");
            }
            WaitingFor::ChooseDungeon { options, .. } => {
                runner
                    .act(GameAction::ChooseDungeon {
                        dungeon: options[0],
                    })
                    .expect("choose first available dungeon");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            other if runner.state().stack.is_empty() => {
                panic!("unexpected waiting state: {other:?}");
            }
            _ => {
                runner.act(GameAction::PassPriority).unwrap();
            }
        }
    }

    assert!(
        runner
            .state()
            .dungeon_progress
            .get(&P0)
            .and_then(|p| p.current_dungeon)
            .is_some(),
        "Sefris must venture into the dungeon when a creature card is discarded to pay \
         Tortured Existence's activation cost"
    );
}
