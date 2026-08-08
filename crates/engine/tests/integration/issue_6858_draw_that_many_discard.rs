//! GitHub issue #6858 — a chained "discard that many" that follows a draw
//! always resolved to 0.
//!
//! CR 608.2c + CR 121.2: `QuantityRef::PreviousEffectAmount { channel: Total }`
//! reads one resolution-local slot, `GameState::last_effect_amount`. Every
//! non-damage producer is contracted to stamp it (see the type doc on
//! `QuantityRef::PreviousEffectAmount`, which names "cards drawn" explicitly),
//! but `Effect::Draw` committed its instruction total only to
//! `state.last_effect_count`, so the chained consumer read `None` → 0 and the
//! discard silently short-circuited before `WaitingFor::DiscardChoice`.
//!
//! These tests pin the CHANNEL CONTRACT (`Draw` → `last_effect_amount`), not one
//! card: the first drives a synthetic "draw N, then discard that many" chain, the
//! second drives Varina, Lich Queen's real attack trigger end to end, and the
//! third pins the zero-draw case that keeps a preceding chain step's amount from
//! leaking into the discard count (Last Stand with no Islands).

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, CardSelectionMode, DamageChannel, Effect, QuantityExpr,
    QuantityRef, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use super::rules::run_combat;

/// Verified against Scryfall (`/cards/named?exact=Varina, Lich Queen`).
const VARINA_ORACLE: &str = "Whenever you attack with one or more Zombies, draw that many cards, \
then discard that many cards. You gain that much life.\n\
{2}, Exile two cards from your graveyard: Create a tapped 2/2 black Zombie creature token.";

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.hand.len())
        .unwrap_or(0)
}

fn library_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.library.len())
        .unwrap_or(0)
}

/// "Draw `draw_count` cards, then discard that many cards" as a bare chain —
/// the building block the four affected Oracle shapes all compile down to.
fn draw_then_discard_that_many(draw: Effect) -> AbilityDefinition {
    let mut ability = AbilityDefinition::new(AbilityKind::Activated, draw);
    ability.sub_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Discard {
            count: QuantityExpr::Ref {
                qty: QuantityRef::PreviousEffectAmount {
                    channel: DamageChannel::Total,
                },
            },
            target: TargetFilter::Controller,
            selection: CardSelectionMode::Chosen,
            unless_filter: None,
            filter: None,
        },
    )));
    ability
}

fn stock_library(scenario: &mut GameScenario, player: PlayerId, count: usize) {
    for i in 0..count {
        scenario.add_card_to_library_top(player, &format!("Library Card {i}"));
    }
}

fn stock_hand(scenario: &mut GameScenario, player: PlayerId, count: usize) {
    for i in 0..count {
        scenario.add_creature_to_hand(player, &format!("Hand Card {i}"), 1, 1);
    }
}

fn activate(runner: &mut GameRunner, source: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("costless activation must succeed");
    runner.advance_until_stack_empty();
}

#[test]
fn draw_stamps_the_total_channel_a_chained_that_many_discard_reads() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Draw Then Discard That Many", 1, 1)
        .with_ability_definition(draw_then_discard_that_many(Effect::Draw {
            count: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Controller,
        }))
        .id();
    stock_library(&mut scenario, P0, 5);
    stock_hand(&mut scenario, P0, 2);

    let mut runner = scenario.build();
    let hand_before = hand_len(&runner, P0);
    let library_before = library_len(&runner, P0);

    activate(&mut runner, source);

    // The draw itself must have happened — without this the discard assertions
    // below could pass for the wrong reason (a chain that never ran at all).
    assert_eq!(
        library_len(&runner, P0),
        library_before - 3,
        "the three-card draw must have left the library"
    );
    assert_eq!(
        hand_len(&runner, P0),
        hand_before + 3,
        "the three drawn cards must be in hand while the discard choice is open"
    );

    // CR 608.2c: at the moment the chain pauses on the discard prompt, BOTH
    // resolution-local channels must still carry the draw instruction's
    // committed total. `last_effect_count` is the slot `draw::resume_draw_
    // sequence` commits to; `last_effect_amount` is the slot
    // `PreviousEffectAmount { channel: Total }` actually reads, and the gap
    // between them was the defect. Both are observed here rather than only the
    // second, so a future producer change that fills one and drops the other
    // cannot pass.
    assert_eq!(
        runner.state().last_effect_count,
        Some(3),
        "the draw sequence commits its total to last_effect_count"
    );
    assert_eq!(
        runner.state().last_effect_amount,
        Some(3),
        "the draw must also stamp the slot that \
         PreviousEffectAmount {{ channel: Total }} reads"
    );

    // The discard must actually reach the interactive branch with count 3 — a
    // count of 0 short-circuits to a no-op before `DiscardChoice` is raised.
    let WaitingFor::DiscardChoice { player, count, .. } = &runner.state().waiting_for else {
        panic!(
            "chained \"discard that many\" must raise DiscardChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*count, 3, "discard that many == the three cards drawn");
}

#[test]
fn varina_attack_trigger_discards_as_many_as_it_drew() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Varina, Lich Queen", 3, 4, VARINA_ORACLE)
        .with_subtypes(vec!["Zombie", "Wizard"]);
    let zombie_a = scenario
        .add_creature(P0, "Zombie Attacker A", 2, 2)
        .with_subtypes(vec!["Zombie"])
        .id();
    let zombie_b = scenario
        .add_creature(P0, "Zombie Attacker B", 2, 2)
        .with_subtypes(vec!["Zombie"])
        .id();
    stock_library(&mut scenario, P0, 5);
    stock_hand(&mut scenario, P0, 2);

    let mut runner = scenario.build();
    let hand_before = hand_len(&runner, P0);
    let library_before = library_len(&runner, P0);
    let life_before = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .map(|p| p.life)
        .expect("P0 exists");

    run_combat(&mut runner, vec![zombie_a, zombie_b], vec![]);

    assert_eq!(
        library_len(&runner, P0),
        library_before - 2,
        "two attacking Zombies draw two cards"
    );
    assert_eq!(
        runner.state().last_effect_amount,
        Some(2),
        "the draw must stamp the total channel the discard reads"
    );

    let WaitingFor::DiscardChoice { count, cards, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "Varina's \"then discard that many cards\" must prompt a discard, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(count, 2, "discard as many as were drawn");

    let chosen: Vec<ObjectId> = cards.into_iter().take(2).collect();
    runner
        .act(GameAction::SelectCards { cards: chosen })
        .expect("submitting the discard selection must succeed");
    runner.advance_until_stack_empty();

    assert_eq!(
        hand_len(&runner, P0),
        hand_before,
        "drew two and discarded two — net hand size unchanged"
    );
    assert_eq!(
        runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P0)
            .map(|p| p.life)
            .expect("P0 exists"),
        life_before + 2,
        "\"You gain that much life\" still reads the attacking-Zombie count"
    );
    assert_eq!(
        runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P0)
            .map(|p| p.graveyard.len())
            .expect("P0 exists"),
        2,
        "the two discarded cards reached the graveyard"
    );
}

#[test]
fn a_zero_card_draw_stamps_zero_instead_of_inheriting_the_previous_step() {
    // Last Stand's tail: "You gain 2 life for each Plains you control. Draw a
    // card for each Island you control, then discard that many cards." With no
    // Islands the draw delivers 0 and the discard must be 0 — never the life
    // total the preceding chain step left in `last_effect_amount`.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mut ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 4 },
            player: TargetFilter::Controller,
        },
    );
    ability.sub_ability = Some(Box::new(draw_then_discard_that_many(Effect::Draw {
        count: QuantityExpr::Fixed { value: 0 },
        target: TargetFilter::Controller,
    })));
    let source = scenario
        .add_creature(P0, "Gain Then Draw Zero", 1, 1)
        .with_ability_definition(ability)
        .id();
    stock_library(&mut scenario, P0, 5);
    stock_hand(&mut scenario, P0, 4);

    let mut runner = scenario.build();
    let hand_before = hand_len(&runner, P0);

    activate(&mut runner, source);

    assert_eq!(
        runner.state().last_effect_amount,
        Some(0),
        "a zero-card draw is a real zero result — it must overwrite the \
         preceding GainLife stamp, not leave it standing"
    );
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::DiscardChoice { .. }),
        "discarding zero cards must not prompt, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        hand_len(&runner, P0),
        hand_before,
        "no cards drawn and none discarded"
    );
}
