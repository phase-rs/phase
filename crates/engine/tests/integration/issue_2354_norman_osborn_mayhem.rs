//! Regression for issue #2354: Norman Osborn // Green Goblin grants Mayhem to
//! cards in your graveyard, and Mayhem enables casting from the graveyard after
//! discard.
//!
//! https://github.com/phase-rs/phase/issues/2354

use engine::game::layers::evaluate_layers;
use engine::game::restrictions::{record_card_discarded, record_discard};
use engine::game::scenario::{GameScenario, P0};
use engine::game::{casting::can_cast_object_now, casting::spell_objects_available_to_cast};
use engine::parser::oracle::parse_oracle_text;
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::{CastingVariant, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const GREEN_GOBLIN_GY_STATIC: &str =
    "Each nonland card in your graveyard has mayhem. The mayhem cost is equal to its mana cost.";

fn floating_mana(generic: usize, red: usize) -> Vec<ManaUnit> {
    let mut pool = Vec::new();
    for _ in 0..generic {
        pool.push(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    for _ in 0..red {
        pool.push(ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]));
    }
    pool
}

#[test]
fn green_goblin_static_parses_graveyard_mayhem_grant() {
    let parsed = parse_oracle_text(
        GREEN_GOBLIN_GY_STATIC,
        "Green Goblin",
        &["Legendary".to_string()],
        &["Creature".to_string()],
        &[
            "Goblin".to_string(),
            "Human".to_string(),
            "Villain".to_string(),
        ],
    );
    assert!(
        parsed.statics.iter().any(|s| {
            s.modifications.iter().any(|m| {
                matches!(
                    m,
                    engine::types::ability::ContinuousModification::AddKeyword {
                        keyword: Keyword::Mayhem(ManaCost::SelfManaCost)
                    }
                )
            })
        }),
        "Green Goblin must grant Mayhem to nonland cards in your graveyard"
    );
}

#[test]
fn mayhem_allows_cast_from_graveyard_after_discard_this_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Green Goblin", 4, 4, GREEN_GOBLIN_GY_STATIC);
    let spell = scenario
        .add_spell_to_hand(P0, "Mayhem Bolt", true)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Red],
        })
        .id();

    scenario.with_mana_pool(P0, floating_mana(1, 1));

    let mut runner = scenario.build();
    evaluate_layers(runner.state_mut());

    {
        let state = runner.state_mut();
        engine::game::zones::move_to_zone(state, spell, Zone::Graveyard, &mut Vec::new());
    }
    evaluate_layers(runner.state_mut());

    let card_id = runner.state().objects[&spell].card_id;
    assert!(
        runner.state().objects[&spell]
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::Mayhem(_))),
        "nonland spell must gain Mayhem from Green Goblin in the graveyard"
    );
    assert!(
        !can_cast_object_now(runner.state(), P0, spell),
        "Mayhem must not make the spell castable before it was discarded this turn"
    );
    assert!(
        !spell_objects_available_to_cast(runner.state(), P0).contains(&spell),
        "legal actions must not expose Mayhem before the discarded-this-turn gate"
    );
    assert!(
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .is_err(),
        "direct CastSpell must reject a Mayhem card that was not discarded this turn"
    );

    record_discard(runner.state_mut(), P0);
    record_card_discarded(runner.state_mut(), spell);
    evaluate_layers(runner.state_mut());

    assert!(
        runner.state().objects[&spell]
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::Mayhem(_))),
        "discarded nonland spell must gain Mayhem from Green Goblin"
    );
    assert!(
        can_cast_object_now(runner.state(), P0, spell),
        "Mayhem must make the spell castable after it was discarded this turn"
    );
    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&spell),
        "legal actions must expose Mayhem after the discarded-this-turn gate"
    );

    let result = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("begin casting via Mayhem");

    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "Expected Priority after Mayhem cast auto-pays mana, got {:?}",
        result.waiting_for
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Mayhem spell should be on the stack"
    );
    match &runner.state().stack[0].kind {
        StackEntryKind::Spell {
            casting_variant, ..
        } => {
            assert_eq!(
                *casting_variant,
                CastingVariant::Mayhem,
                "Stack entry should use CastingVariant::Mayhem"
            );
        }
        other => panic!("Expected Spell on stack, got {other:?}"),
    }
}
