//! Tests for Fire Prophecy (and the "You may put a card from your hand on the bottom of your library. If you do, draw a card" class).

use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityCondition, AbilityKind, Effect, LibraryPosition};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const FIRE_PROPHECY: &str = "Fire Prophecy deals 3 damage to target creature. You may put a card from your hand on the bottom of your library. If you do, draw a card.";

const GRIZZLY_BEARS: &str = "";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

#[test]
fn fire_prophecy_empty_hand_does_not_prompt_and_draws_nothing() {
    let parsed = parse_effect_chain(FIRE_PROPHECY, AbilityKind::Spell);
    assert!(matches!(parsed.effect.as_ref(), Effect::DealDamage { .. }));
    let bottom = parsed
        .sub_ability
        .as_ref()
        .expect("implemented bottoming rider");
    assert!(bottom.optional);
    assert!(matches!(
        bottom.effect.as_ref(),
        Effect::PutAtLibraryPosition {
            position: LibraryPosition::Bottom,
            ..
        }
    ));
    let draw = bottom.sub_ability.as_ref().expect("conditional draw rider");
    assert!(matches!(draw.effect.as_ref(), Effect::Draw { .. }));
    assert!(draw
        .condition
        .as_ref()
        .is_some_and(AbilityCondition::is_optional_effect_performed));

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bears = scenario
        .add_creature_from_oracle(P0, "Grizzly Bears", 2, 2, GRIZZLY_BEARS)
        .id();
    let prophecy = scenario
        .add_spell_to_hand_from_oracle(P0, "Fire Prophecy", true, FIRE_PROPHECY)
        .id();
    scenario.add_card_to_library_top(P0, "Deck Card");
    scenario.with_mana_pool(P0, floating_mana(2, ManaType::Red));

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&prophecy].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: prophecy,
            card_id,
            targets: vec![bears],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Fire Prophecy must be castable");

    // Hand is now empty (Fire Prophecy is on the stack).
    assert!(runner.state().players[P0.0 as usize].hand.is_empty());

    let initial_library_len = runner.state().players[P0.0 as usize].library.len();

    runner.advance_until_stack_empty();

    assert!(runner.state().stack.is_empty(), "resolution must complete");
    assert_eq!(runner.state().objects[&prophecy].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Graveyard,
        "the damage instruction must execute"
    );

    // CR 608.2d: Infeasible optional effect (empty hand) must auto-decline without prompting.
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. } | WaitingFor::EffectZoneChoice { .. }
        ),
        "Must not prompt when hand is empty, got {:?}",
        runner.state().waiting_for
    );

    // No cards should have been drawn
    assert_eq!(
        runner.state().players[P0.0 as usize].library.len(),
        initial_library_len,
        "No cards should have been drawn"
    );
    assert!(
        runner.state().players[P0.0 as usize].hand.is_empty(),
        "Hand must still be empty"
    );
}

#[test]
fn fire_prophecy_decline_does_not_bottom_or_draw() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bears = scenario
        .add_creature_from_oracle(P0, "Grizzly Bears", 2, 2, GRIZZLY_BEARS)
        .id();
    let prophecy = scenario
        .add_spell_to_hand_from_oracle(P0, "Fire Prophecy", true, FIRE_PROPHECY)
        .id();
    let filler = scenario.add_spell_to_hand(P0, "Hand Filler", true).id();
    scenario.add_card_to_library_top(P0, "Deck Card");
    scenario.with_mana_pool(P0, floating_mana(2, ManaType::Red));

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&prophecy].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: prophecy,
            card_id,
            targets: vec![bears],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Fire Prophecy must be castable");

    runner.advance_until_stack_empty();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player, .. } if player == P0
        ),
        "Fire Prophecy must prompt to bottom a card, got {:?}",
        runner.state().waiting_for
    );

    let initial_library_len = runner.state().players[P0.0 as usize].library.len();

    // Decline the optional effect
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("Decline must succeed");

    runner.advance_until_stack_empty();

    // Hand Filler remains in hand, 0 drawn, library length unchanged
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .hand
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![filler]
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].library.len(),
        initial_library_len
    );
}

#[test]
fn fire_prophecy_accept_requires_bottom_card_and_draws() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bears = scenario
        .add_creature_from_oracle(P0, "Grizzly Bears", 2, 2, GRIZZLY_BEARS)
        .id();
    let prophecy = scenario
        .add_spell_to_hand_from_oracle(P0, "Fire Prophecy", true, FIRE_PROPHECY)
        .id();
    let filler = scenario.add_spell_to_hand(P0, "Hand Filler", true).id();
    let top_deck = scenario.add_card_to_library_top(P0, "Top of Library");
    scenario.with_mana_pool(P0, floating_mana(2, ManaType::Red));

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&prophecy].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: prophecy,
            card_id,
            targets: vec![bears],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Fire Prophecy must be castable");

    runner.advance_until_stack_empty();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player, .. } if player == P0
        ),
        "Fire Prophecy must prompt to bottom a card, got {:?}",
        runner.state().waiting_for
    );

    // Accept the optional effect
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("Accept must succeed");

    // Must now be waiting for card selection to put on bottom
    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice {
            player,
            cards,
            count,
            min_count,
            up_to,
            zone,
            ..
        } => {
            assert_eq!(*player, P0);
            assert_eq!(*count, 1);
            assert_eq!(*min_count, 1);
            assert!(!*up_to);
            assert_eq!(*zone, Zone::Hand);
            assert_eq!(cards, &vec![filler]);
        }
        other => panic!("Expected EffectZoneChoice, got {:?}", other),
    }

    // Selecting 0 cards must fail because up_to is false / min_count is 1
    let empty_selection_err = runner.act(GameAction::SelectCards { cards: vec![] });
    assert!(
        empty_selection_err.is_err(),
        "Selecting 0 cards when 1 is required must fail"
    );

    // Select the filler card from hand
    runner
        .act(GameAction::SelectCards {
            cards: vec![filler],
        })
        .expect("Card selection must succeed");

    runner.advance_until_stack_empty();

    // Hand Filler should now be at the bottom of the library (back)
    assert_eq!(
        runner.state().players[P0.0 as usize].library.back(),
        Some(&filler)
    );

    // Top deck card should now be in hand
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .hand
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![top_deck]
    );
}
