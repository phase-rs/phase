//! Regression for issue #1108: Plumb the Forbidden must copy itself once for
//! EACH creature sacrificed as its optional additional cost, not zero or one
//! time regardless of how many creatures were sacrificed.
//!
//! Oracle text (verified against Scryfall):
//! "As an additional cost to cast this spell, you may sacrifice one or more
//! creatures. When you do, copy this spell for each creature sacrificed this
//! way.\nYou draw a card and lose 1 life."
//!
//! Root cause (two parser bugs, both fixed here):
//!   1. `oracle_cost.rs`'s sacrifice-cost parser recognized "sacrifice any
//!      number of X" but had no case for "sacrifice one or more X" — it fell
//!      through to a numeral-fallback path that mis-parsed the cost as a
//!      fixed, filter-less "sacrifice exactly 1" cost.
//!   2. The "As an additional cost..., you may X. When you do, [effect]"
//!      reflexive-trigger shape (CR 603.2b — the unnamed-keyword sibling of
//!      Casualty/Replicate/Squad) was never wired into a `SpellCast` trigger
//!      at all; the whole "When you do, copy this spell for each creature
//!      sacrificed this way" sentence was silently dropped.
//!
//! https://github.com/phase-rs/phase/issues/1108

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const PLUMB_ORACLE: &str = "As an additional cost to cast this spell, you may sacrifice one or more creatures. When you do, copy this spell for each creature sacrificed this way.\nYou draw a card and lose 1 life.";

/// Two generic mana (colorless) — enough for a `{2}` spell regardless of which
/// two units get auto-tapped.
fn generic_mana(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn plumb_cost() -> ManaCost {
    ManaCost::Cost {
        generic: 2,
        shards: Vec::<ManaCostShard>::new(),
    }
}

/// Drive the pipeline to stack-empty. Handles the additional-cost decision,
/// the ranged sacrifice selection, trigger ordering, and priority passes.
/// `sacrifice` lists which creatures to select when the ranged sacrifice
/// prompt appears; an empty slice declines the optional additional cost.
fn cast_and_resolve(
    runner: &mut engine::game::scenario::GameRunner,
    spell: ObjectId,
    sacrifice: &[ObjectId],
) {
    let card_id = runner.state().objects.get(&spell).unwrap().card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: Default::default(),
        })
        .expect("cast Plumb the Forbidden");

    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost {
                        pay: !sacrifice.is_empty(),
                    })
                    .expect("decide the optional sacrifice cost");
            }
            // CR 601.2b: the ranged "sacrifice one or more creatures" cost
            // reuses the "any number of" X-announcement sentinel, so X is
            // announced up front before the sacrifice-selection prompt.
            WaitingFor::ChooseXValue { min, max, .. } => {
                let x = (sacrifice.len() as u32).clamp(min, max);
                runner
                    .act(GameAction::ChooseX { value: x })
                    .expect("announce X for the ranged sacrifice cost");
            }
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: sacrifice.to_vec(),
                    })
                    .expect("select the ranged sacrifice choice");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() || runner.act(GameAction::PassPriority).is_err()
                {
                    break;
                }
            }
            other => panic!("unexpected waiting state driving Plumb the Forbidden: {other:?}"),
        }
    }
}

/// CR 603.2b + CR 707.10: sacrificing 2 creatures as the additional cost must
/// copy the spell twice — 1 original resolution + 2 copies = 3 total draws
/// and 3 total life losses, and both sacrificed creatures leave the
/// battlefield.
#[test]
fn plumb_the_forbidden_copies_once_per_creature_sacrificed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_mana_pool(P0, generic_mana(2));
    scenario.with_library_top(P0, &["Card One", "Card Two", "Card Three", "Card Four"]);

    let creature_a = scenario.add_creature(P0, "Fodder A", 1, 1).id();
    let creature_b = scenario.add_creature(P0, "Fodder B", 1, 1).id();
    let plumb = scenario
        .add_spell_to_hand_from_oracle(P0, "Plumb the Forbidden", true, PLUMB_ORACLE)
        .with_mana_cost(plumb_cost())
        .id();

    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;

    cast_and_resolve(&mut runner, plumb, &[creature_a, creature_b]);

    assert_eq!(
        runner.state().objects[&creature_a].zone,
        Zone::Graveyard,
        "creature_a must have been sacrificed as the additional cost"
    );
    assert_eq!(
        runner.state().objects[&creature_b].zone,
        Zone::Graveyard,
        "creature_b must have been sacrificed as the additional cost"
    );

    let hand_names: Vec<&str> = runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Hand && o.owner == P0)
        .map(|o| o.name.as_str())
        .collect();
    for expected in ["Card One", "Card Two", "Card Three"] {
        assert!(
            hand_names.contains(&expected),
            "expected {expected} to have been drawn; hand contains {hand_names:?}"
        );
    }
    assert_eq!(
        hand_names.len(),
        3,
        "sacrificing 2 creatures must draw exactly 3 cards total \
         (1 original resolution + 2 copies), got {hand_names:?}"
    );

    let life_after = runner.state().players[P0.0 as usize].life;
    assert_eq!(
        life_after,
        life_before - 3,
        "sacrificing 2 creatures must lose 3 total life (1 original + 2 copies)"
    );
}

/// CR 603.2b: declining the optional additional cost must leave the spell
/// resolving exactly once — no reflexive trigger, no copies.
#[test]
fn plumb_the_forbidden_declined_cost_resolves_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_mana_pool(P0, generic_mana(2));
    scenario.with_library_top(P0, &["Card One", "Card Two", "Card Three"]);

    let creature_a = scenario.add_creature(P0, "Fodder A", 1, 1).id();
    let plumb = scenario
        .add_spell_to_hand_from_oracle(P0, "Plumb the Forbidden", true, PLUMB_ORACLE)
        .with_mana_cost(plumb_cost())
        .id();

    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;

    cast_and_resolve(&mut runner, plumb, &[]);

    assert_eq!(
        runner.state().objects[&creature_a].zone,
        Zone::Battlefield,
        "declining the additional cost must leave the creature on the battlefield"
    );

    let hand_names: Vec<&str> = runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Hand && o.owner == P0)
        .map(|o| o.name.as_str())
        .collect();
    assert_eq!(
        hand_names,
        vec!["Card One"],
        "declining the additional cost must draw exactly 1 card, got {hand_names:?}"
    );

    let life_after = runner.state().players[P0.0 as usize].life;
    assert_eq!(
        life_after,
        life_before - 1,
        "declining the additional cost must lose exactly 1 life"
    );
}
