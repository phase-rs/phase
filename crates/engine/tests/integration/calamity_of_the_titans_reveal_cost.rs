//! Issue #6003: Calamity of the Titans never prompts to show a card from hand.
//!
//! Oracle text (verified via Scryfall, CMM #713): "As an additional cost to
//! cast this spell, reveal a colorless creature card from your hand. Exile
//! each creature and planeswalker with mana value less than the revealed
//! card's mana value."
//!
//! `AbilityCost::Reveal { filter: Some(_), .. }` parsed correctly already, but
//! `pay_additional_cost_with_source` had no arm for it — it silently fell
//! through to the no-op `_` branch, so the cast never opened a
//! `WaitingFor::PayCost` prompt and no card was ever bound as the cost-paid
//! object the exile clause reads "the revealed card's mana value" from.
//!
//! This test drives the real cast pipeline: cast the spell, reveal a
//! colorless creature from hand, resolve, and assert the exile clause used
//! the REVEALED card's mana value (4) — not 0 (dropped referent).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;

const CALAMITY_OF_THE_TITANS_ORACLE: &str = "As an additional cost to cast this spell, reveal a \
colorless creature card from your hand.\nExile each creature and planeswalker with mana value \
less than the revealed card's mana value.";

/// Drive the reveal-cost prompt then pass priority until the spell resolves.
fn pay_reveal_and_resolve(runner: &mut GameRunner, revealed: ObjectId) {
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost {
                kind: PayCostKind::Reveal,
                choices,
                ..
            } => {
                assert_eq!(
                    choices,
                    vec![revealed],
                    "only the colorless creature card must be an eligible reveal choice"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![revealed],
                    })
                    .expect("revealing the colorless creature must succeed");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            other => panic!("unexpected prompt while casting Calamity of the Titans: {other:?}"),
        }
    }
    panic!("cast pipeline did not settle within the prompt budget");
}

#[test]
fn reveal_cost_prompts_and_exiles_by_revealed_mana_value() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The colorless creature card to reveal — mana value 4.
    let revealed = scenario
        .add_creature_to_hand(P0, "Eldrazi Husk", 4, 4)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    // A colored creature card in hand — must NOT be an eligible reveal choice.
    scenario
        .add_creature_to_hand(P0, "Loyal Warhound", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 1,
        })
        .id();

    // Below the revealed card's mana value (4) — must be exiled.
    let low_mv = scenario
        .add_creature(P1, "Low MV Guy", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    // Equal to the revealed card's mana value — "less than" excludes it.
    let equal_mv = scenario
        .add_creature(P1, "Equal MV Guy", 4, 4)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    // Above the revealed card's mana value — must survive.
    let high_mv = scenario
        .add_creature(P0, "High MV Guy", 6, 6)
        .with_mana_cost(ManaCost::generic(6))
        .id();

    let mut builder = scenario.add_spell_to_hand_from_oracle(
        P0,
        "Calamity of the Titans",
        false,
        CALAMITY_OF_THE_TITANS_ORACLE,
    );
    builder.with_mana_cost(ManaCost::generic(0));
    let spell = builder.id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Calamity of the Titans must be accepted");

    pay_reveal_and_resolve(&mut runner, revealed);

    assert_eq!(
        runner.state().objects[&low_mv].zone,
        engine::types::zones::Zone::Exile,
        "a creature with mana value less than the revealed card's (4) must be exiled"
    );
    assert_eq!(
        runner.state().objects[&equal_mv].zone,
        engine::types::zones::Zone::Battlefield,
        "a creature with mana value EQUAL to the revealed card's must survive (less than, not less-or-equal)"
    );
    assert_eq!(
        runner.state().objects[&high_mv].zone,
        engine::types::zones::Zone::Battlefield,
        "a creature with mana value greater than the revealed card's must survive"
    );
}
