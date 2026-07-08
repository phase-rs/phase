//! Regression for issue #3257: Macabre Waltz must return chosen graveyard
//! creatures to hand, then discard a card.
//!
//! https://github.com/phase-rs/phase/issues/3257

use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, Effect};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MACABRE_WALTZ_ORACLE: &str =
    "Return up to two target creature cards from your graveyard to your hand, then discard a card.";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

#[test]
fn macabre_waltz_returns_creature_from_graveyard_to_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Macabre Waltz", false, MACABRE_WALTZ_ORACLE)
        .id();
    let gy_creature = scenario
        .add_creature_to_graveyard(P0, "Graveyard Bear", 2, 2)
        .id();
    let hand_discard = scenario.add_creature_to_hand(P0, "Hand Rat", 1, 1).id();
    scenario.with_mana_pool(
        P0,
        floating_mana(1, ManaType::Colorless)
            .into_iter()
            .chain(floating_mana(1, ManaType::Black))
            .collect(),
    );

    let mut runner = scenario.build();

    runner
        .cast(spell)
        .target_object(gy_creature)
        .commit()
        .resolve();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::DiscardChoice { .. }),
        "discard rider must pause for a hand choice, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects[&gy_creature].zone,
        Zone::Hand,
        "returned creature must reach hand before the discard choice"
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![hand_discard],
        })
        .expect("discard choice");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&gy_creature].zone,
        Zone::Hand,
        "chosen graveyard creature must stay in hand"
    );
    assert_eq!(
        runner.state().objects[&hand_discard].zone,
        Zone::Graveyard,
        "player must discard a different card after the return"
    );
}

#[test]
fn macabre_waltz_parses_return_then_discard_chain() {
    let def = parse_effect_chain(MACABRE_WALTZ_ORACLE, AbilityKind::Spell);
    assert!(
        matches!(
            &*def.effect,
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Hand,
                ..
            }
        ),
        "expected graveyard-to-hand ChangeZone head, got {:?}",
        def.effect
    );
    let spec = def
        .multi_target
        .as_ref()
        .expect("up-to-two must carry multi_target");
    assert!(
        spec.min_is_fixed_zero(),
        "up-to-two must allow zero targets"
    );
    let discard = def
        .sub_ability
        .as_ref()
        .expect("bounce must chain into discard");
    assert!(
        matches!(discard.effect.as_ref(), Effect::Discard { .. }),
        "expected discard rider, got {:?}",
        discard.effect
    );
}
