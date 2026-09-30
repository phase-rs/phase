//! GitHub issue #3466 — Counter spells with non-mana "unless" costs must not
//! silently parse as unconditional counters.
//!
//! Dash Hopes ("Counter target spell unless its controller pays 5 life.") and
//! similar cards must carry `unless_pay` with `PayLife` / `Sacrifice` / `Discard`
//! (CR 118.12 / CR 119.4 / CR 608.2c), and deck validation must not mark them
//! fully supported when the unless clause is dropped.

use engine::game::coverage::card_face_gaps;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityCost, AbilityKind, Effect, QuantityExpr, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::game_state::{StackEntry, StackEntryKind, WaitingFor};
use engine::types::identifiers::CardId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DASH_HOPES: &str = "Counter target spell unless its controller pays 5 life.";
const MANA_LEAK: &str = "Counter target spell unless its controller pays {3}.";
const COUNTER_SACRIFICE: &str = "Counter target spell unless its controller sacrifices a creature.";
const COUNTER_DISCARD: &str = "Counter target spell unless its controller discards a card.";

fn card_face(name: &str, oracle: &str) -> CardFace {
    let parsed = parse_oracle_text(oracle, name, &[], &["Instant".to_string()], &[]);
    CardFace {
        name: name.to_string(),
        oracle_text: Some(oracle.to_string()),
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        ..Default::default()
    }
}

fn spell_ability(name: &str, oracle: &str) -> engine::types::ability::AbilityDefinition {
    let parsed = parse_oracle_text(oracle, name, &[], &["Instant".to_string()], &[]);
    parsed
        .abilities
        .into_iter()
        .find(|a| a.kind == AbilityKind::Spell)
        .expect("spell ability")
}

fn put_instant_on_stack(
    runner: &mut engine::game::scenario::GameRunner,
    controller: engine::types::player::PlayerId,
) -> engine::types::identifiers::ObjectId {
    let spell = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(901),
        controller,
        "Shock".to_string(),
        Zone::Stack,
    );
    if let Some(obj) = runner.state_mut().objects.get_mut(&spell) {
        obj.card_types.core_types = vec![CoreType::Instant];
    }
    runner.state_mut().stack.push_back(StackEntry {
        id: spell,
        source_id: spell,
        controller,
        kind: StackEntryKind::Spell {
            card_id: CardId(901),
            ability: None,
            casting_variant: engine::types::game_state::CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    spell
}

#[test]
fn dash_hopes_parses_conditional_counter_with_pay_life_unless() {
    let ability = spell_ability("Dash Hopes", DASH_HOPES);
    assert!(
        matches!(ability.effect.as_ref(), Effect::Counter { .. }),
        "expected Counter, got {:?}",
        ability.effect
    );
    let unless_pay = ability
        .unless_pay
        .as_ref()
        .expect("Dash Hopes must carry unless_pay, not an unconditional counter");
    assert_eq!(unless_pay.payer, TargetFilter::ParentTargetController);
    assert_eq!(
        unless_pay.cost,
        AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 5 }
        }
    );
}

#[test]
fn counter_unless_non_mana_costs_have_no_coverage_gaps() {
    for (name, oracle) in [
        ("Dash Hopes", DASH_HOPES),
        ("Counter-Sacrifice", COUNTER_SACRIFICE),
        ("Counter-Discard", COUNTER_DISCARD),
    ] {
        let gaps = card_face_gaps(&card_face(name, oracle));
        assert!(
            gaps.is_empty(),
            "{name} should report no face gaps when unless_pay is present, got {gaps:?}"
        );
    }
}

#[test]
fn mana_leak_control_has_unless_pay_for_regression() {
    let ability = spell_ability("Mana Leak", MANA_LEAK);
    assert!(
        ability.unless_pay.is_some(),
        "Mana Leak control must keep unless_pay"
    );
}

#[test]
fn dash_hopes_prompts_life_payment_then_counters_on_decline() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P1, 20);
    let mut dash = scenario.add_spell_to_hand_from_oracle(P0, "Dash Hopes", true, DASH_HOPES);
    dash.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Black, ManaCostShard::Black],
    });
    let dash_hopes = dash.id();
    scenario.add_basic_land(P0, ManaColor::Black);
    scenario.add_basic_land(P0, ManaColor::Black);

    let mut runner = scenario.build();
    let opponent_spell = put_instant_on_stack(&mut runner, P1);

    runner
        .cast(dash_hopes)
        .target_objects(&[opponent_spell])
        .resolve();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::UnlessPayment { player: P1, .. }
        ),
        "Dash Hopes must prompt P1 to pay 5 life, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("P1 declines to pay life");

    assert!(
        runner.state().stack.is_empty(),
        "declining life payment must counter the targeted spell"
    );
    assert_eq!(
        runner.state().objects.get(&opponent_spell).map(|o| o.zone),
        Some(Zone::Graveyard)
    );
}

#[test]
fn dash_hopes_paying_life_leaves_target_spell_on_stack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P1, 20);
    let mut dash = scenario.add_spell_to_hand_from_oracle(P0, "Dash Hopes", true, DASH_HOPES);
    dash.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Black, ManaCostShard::Black],
    });
    let dash_hopes = dash.id();
    scenario.add_basic_land(P0, ManaColor::Black);
    scenario.add_basic_land(P0, ManaColor::Black);

    let mut runner = scenario.build();
    let opponent_spell = put_instant_on_stack(&mut runner, P1);
    let life_before = runner.state().players[P1.0 as usize].life;

    runner
        .cast(dash_hopes)
        .target_objects(&[opponent_spell])
        .resolve();

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("P1 pays 5 life");

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        life_before - 5,
        "paying the unless cost must deduct 5 life (CR 119.4)"
    );
    assert!(
        runner.state().stack.iter().any(|e| e.id == opponent_spell),
        "target spell must remain on stack when unless cost is paid"
    );
    assert_eq!(
        runner.state().objects.get(&opponent_spell).map(|o| o.zone),
        Some(Zone::Stack)
    );
}

// --- Perplex: "unless its controller discards their hand" (CR 118.12a + CR 701.9a) ---

const PERPLEX: &str = "Counter target spell unless its controller discards their hand.";
const DISCARD_OR_PAY: &str =
    "Counter target spell unless its controller discards a card or pays {2}.";

/// P0 casts a counterspell (verbatim `oracle`) at a spell controlled by P1.
/// P0 holds `p0_hand` extra cards and P1 holds `p1_hand` cards; P1 also gets
/// `p1_lands` untapped Islands. Returns the runner (waiting at P1's unless
/// prompt) and the targeted spell.
fn cast_counter_at_p1_spell(
    oracle: &str,
    p0_hand: usize,
    p1_hand: usize,
    p1_lands: usize,
) -> (
    engine::game::scenario::GameRunner,
    engine::types::identifiers::ObjectId,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut counter = scenario.add_spell_to_hand_from_oracle(P0, "Perplex", true, oracle);
    counter.with_mana_cost(ManaCost::Cost {
        generic: 1,
        shards: vec![ManaCostShard::Blue, ManaCostShard::Black],
    });
    let counter_id = counter.id();
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Black);
    scenario.add_basic_land(P0, ManaColor::Blue);
    for i in 0..p0_hand {
        scenario.add_card_to_hand(P0, &format!("P0 Filler {i}"));
    }
    for i in 0..p1_hand {
        scenario.add_card_to_hand(P1, &format!("P1 Filler {i}"));
    }
    for _ in 0..p1_lands {
        scenario.add_basic_land(P1, ManaColor::Blue);
    }
    let mut runner = scenario.build();
    let target = put_instant_on_stack(&mut runner, P1);
    runner.cast(counter_id).target_objects(&[target]).resolve();
    (runner, target)
}

fn hand_len(
    runner: &engine::game::scenario::GameRunner,
    player: engine::types::player::PlayerId,
) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn graveyard_len(
    runner: &engine::game::scenario::GameRunner,
    player: engine::types::player::PlayerId,
) -> usize {
    runner.state().players[player.0 as usize].graveyard.len()
}

/// Discard whatever `WardDiscardChoice` rounds are pending, one card each.
fn drain_discard_rounds(runner: &mut engine::game::scenario::GameRunner) -> usize {
    let mut rounds = 0;
    while let WaitingFor::WardDiscardChoice { cards, .. } = runner.state().waiting_for.clone() {
        runner
            .act(GameAction::SelectCards {
                cards: vec![cards[0]],
            })
            .expect("discard selection accepted");
        rounds += 1;
        assert!(rounds < 20, "discard loop must terminate");
    }
    rounds
}

#[test]
fn perplex_pay_discards_target_controllers_entire_hand() {
    // P0 (caster) has 1 extra card, P1 (payer) has 3: the count must be P1's.
    let (mut runner, target) = cast_counter_at_p1_spell(PERPLEX, 1, 3, 0);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::UnlessPayment { player: P1, .. }
        ),
        "P1 must be prompted, got {:?}",
        runner.state().waiting_for
    );
    let p0_hand_before = hand_len(&runner, P0);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("P1 pays by discarding");
    assert_eq!(
        drain_discard_rounds(&mut runner),
        3,
        "one round per card in P1's hand"
    );
    assert_eq!(hand_len(&runner, P1), 0);
    assert_eq!(graveyard_len(&runner, P1), 3);
    assert_eq!(
        hand_len(&runner, P0),
        p0_hand_before,
        "caster's hand is untouched"
    );
    assert_eq!(
        runner.state().objects.get(&target).map(|o| o.zone),
        Some(Zone::Stack),
        "paid cost: target spell is not countered"
    );
}

#[test]
fn perplex_pay_with_single_card_hand_takes_one_round() {
    let (mut runner, target) = cast_counter_at_p1_spell(PERPLEX, 0, 1, 0);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("P1 pays");
    assert_eq!(drain_discard_rounds(&mut runner), 1);
    assert_eq!(hand_len(&runner, P1), 0);
    assert_eq!(
        runner.state().objects.get(&target).map(|o| o.zone),
        Some(Zone::Stack)
    );
}

/// CR 118.3 + card ruling 2005-10-01: with no cards in hand the controller can
/// still choose to discard their hand and prevent the counter.
#[test]
fn perplex_pay_with_empty_hand_terminates_as_paid() {
    let (mut runner, target) = cast_counter_at_p1_spell(PERPLEX, 1, 0, 0);
    // Reach-guard: the prompt was raised (the Discard arm is what runs next).
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::UnlessPayment { player: P1, .. }
    ));
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("empty-hand payer may still pay");
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::WardDiscardChoice { .. }
        ),
        "must not soft-lock on an empty discard prompt, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects.get(&target).map(|o| o.zone),
        Some(Zone::Stack),
        "empty-hand payment still prevents the counter"
    );
}

#[test]
fn perplex_decline_counters_the_spell_and_keeps_hand() {
    for p1_hand in [0usize, 2] {
        let (mut runner, target) = cast_counter_at_p1_spell(PERPLEX, 0, p1_hand, 0);
        runner
            .act(GameAction::PayUnlessCost { pay: false })
            .expect("P1 declines");
        assert_eq!(
            runner.state().objects.get(&target).map(|o| o.zone),
            Some(Zone::Graveyard),
            "declining counters the spell (hand {p1_hand})"
        );
        assert_eq!(hand_len(&runner, P1), p1_hand);
    }
}

#[test]
fn perplex_has_no_coverage_gaps() {
    let gaps = card_face_gaps(&card_face("Perplex", PERPLEX));
    assert!(
        gaps.is_empty(),
        "Perplex must be fully supported, got {gaps:?}"
    );
}

/// CR 118.12a: "discards a card or pays {2}" is a real disjunction, no longer
/// the bare `Counter unless {2}` the unanchored "pays " scan produced.
#[test]
fn counter_unless_discard_or_pays_disjunction_end_to_end() {
    // Pay via the discard sub-cost.
    let (mut runner, target) = cast_counter_at_p1_spell(DISCARD_OR_PAY, 0, 2, 2);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::UnlessPaymentChooseCost { player: P1, .. }
        ),
        "expected sub-cost choice, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseUnlessCostBranch {
            choice: engine::types::actions::UnlessCostBranch::Pay { index: 0 },
        })
        .expect("choose discard branch");
    assert_eq!(drain_discard_rounds(&mut runner), 1);
    assert_eq!(hand_len(&runner, P1), 1);
    assert_eq!(
        runner.state().objects.get(&target).map(|o| o.zone),
        Some(Zone::Stack)
    );

    // Pay via the mana sub-cost.
    let (mut runner, target) = cast_counter_at_p1_spell(DISCARD_OR_PAY, 0, 2, 2);
    runner
        .act(GameAction::ChooseUnlessCostBranch {
            choice: engine::types::actions::UnlessCostBranch::Pay { index: 1 },
        })
        .expect("choose mana branch");
    assert_eq!(hand_len(&runner, P1), 2, "mana payment discards nothing");
    assert_eq!(
        runner.state().objects.get(&target).map(|o| o.zone),
        Some(Zone::Stack)
    );

    // Decline.
    let (mut runner, target) = cast_counter_at_p1_spell(DISCARD_OR_PAY, 0, 2, 2);
    runner
        .act(GameAction::ChooseUnlessCostBranch {
            choice: engine::types::actions::UnlessCostBranch::Decline,
        })
        .expect("decline");
    assert_eq!(
        runner.state().objects.get(&target).map(|o| o.zone),
        Some(Zone::Graveyard)
    );
}
