//! Foil — "You may discard an Island card and another card rather than pay this
//! spell's mana cost." (CR 118.9). The alternative cost is TWO chosen hand
//! discards: the first an Island card, the second any other card. One physical
//! card can never serve both legs, so the cost is offered only when the hand
//! holds an Island plus a different card besides Foil itself (CR 118.3 +
//! CR 601.2h), and the second leg's prompt excludes the card already discarded.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityCost, AdditionalCost, TargetRef};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::card_type::CoreType;
use engine::types::game_state::{
    CastPaymentMode, CastingVariant, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// Verbatim Oracle text (Scryfall).
const FOIL: &str =
    "You may discard an Island card and another card rather than pay this spell's mana cost.\n\
Counter target spell.";

#[derive(Clone, Copy, PartialEq)]
enum Card {
    Island,
    Forest,
}

struct Board {
    runner: GameRunner,
    foil: ObjectId,
    hand: Vec<ObjectId>,
    opponent_spell: ObjectId,
}

/// P0 holds Foil plus `hand`; an opposing spell is on the stack. When `lands` is
/// true, P0 controls enough untapped lands to pay Foil's printed {1}{U}{U}.
fn board(hand: &[Card], lands: bool) -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let foil = scenario
        .add_spell_to_hand_from_oracle(P0, "Foil", true, FOIL)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 1,
        })
        .id();
    let mut ids = Vec::new();
    for (i, card) in hand.iter().enumerate() {
        let id = match card {
            Card::Island => {
                let mut b = scenario.add_land_to_hand(P0, &format!("Island {i}"));
                b.with_subtypes(vec!["Island"]);
                b.id()
            }
            Card::Forest => {
                let mut b = scenario.add_land_to_hand(P0, &format!("Forest {i}"));
                b.with_subtypes(vec!["Forest"]);
                b.id()
            }
        };
        ids.push(id);
    }
    if lands {
        for _ in 0..3 {
            scenario.add_basic_land(P0, ManaColor::Blue);
        }
    }
    let mut runner = scenario.build();
    let spell = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(9001),
        P1,
        "Opponent Instant".to_string(),
        Zone::Stack,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&spell)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Instant];
    runner.state_mut().stack.push_back(StackEntry {
        id: spell,
        source_id: spell,
        controller: P1,
        kind: StackEntryKind::Spell {
            card_id: CardId(9001),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    Board {
        runner,
        foil,
        hand: ids,
        opponent_spell: spell,
    }
}

/// Cast Foil choosing its alternative cost, recording the `choices` of every
/// discard prompt and answering each with `picks` in order.
fn cast_with_alternative(b: &mut Board, picks: &[ObjectId]) -> Result<Vec<Vec<ObjectId>>, String> {
    let card_id = b.runner.state().objects[&b.foil].card_id;
    let mut waiting = b
        .runner
        .act(GameAction::CastSpell {
            object_id: b.foil,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .map_err(|e| format!("cast rejected: {e:?}"))?
        .waiting_for;
    let mut prompts = Vec::new();
    let mut picks = picks.iter();
    for _ in 0..12 {
        let action = match &waiting {
            WaitingFor::Priority { .. } => break,
            WaitingFor::AlternativeCastChoice { .. } => GameAction::ChooseAlternativeCast {
                choice: AlternativeCastDecision::Alternative,
            },
            WaitingFor::OptionalCostChoice { .. } => GameAction::DecideOptionalCost { pay: true },
            WaitingFor::TargetSelection { .. } => GameAction::ChooseTarget {
                target: Some(TargetRef::Object(b.opponent_spell)),
            },
            WaitingFor::PayCost { choices, .. } => {
                prompts.push(choices.clone());
                GameAction::SelectCards {
                    cards: vec![*picks.next().expect("a pick for every discard prompt")],
                }
            }
            other => return Err(format!("unexpected prompt: {other:?}")),
        };
        waiting = b
            .runner
            .act(action)
            .map_err(|e| format!("action rejected: {e:?}"))?
            .waiting_for;
    }
    assert!(
        matches!(waiting, WaitingFor::Priority { .. }),
        "the cast must reach Priority"
    );
    Ok(prompts)
}

/// Positive: Island + Forest pays both legs; prompts are typed, then exclude the
/// discarded card, and Foil itself is never offered.
#[test]
fn foil_discards_island_then_another_card_and_counters() {
    let mut b = board(&[Card::Island, Card::Forest], false);
    let (island, forest) = (b.hand[0], b.hand[1]);
    let prompts = cast_with_alternative(&mut b, &[island, forest]).expect("castable");
    assert_eq!(prompts.len(), 2, "two discard legs, two prompts");
    assert_eq!(prompts[0], vec![island], "first leg: only the Island card");
    assert_eq!(
        prompts[1],
        vec![forest],
        "second leg: another card, not the discarded Island and not Foil"
    );
    assert!(!prompts.iter().any(|p| p.contains(&b.foil)));
    assert_eq!(b.runner.state().objects[&island].zone, Zone::Graveyard);
    assert_eq!(b.runner.state().objects[&forest].zone, Zone::Graveyard);
    // Foil resolves and counters the opposing spell.
    b.runner.resolve_top();
    assert_eq!(
        b.runner.state().objects[&b.opponent_spell].zone,
        Zone::Graveyard
    );
    assert_eq!(b.runner.state().objects[&b.foil].zone, Zone::Graveyard);
}

/// A second Island legally serves as "another card".
#[test]
fn foil_second_island_serves_as_another_card() {
    let mut b = board(&[Card::Island, Card::Island], false);
    let (a, c) = (b.hand[0], b.hand[1]);
    let prompts = cast_with_alternative(&mut b, &[a, c]).expect("castable");
    assert_eq!(prompts[0].len(), 2);
    assert_eq!(
        prompts[1],
        vec![c],
        "the other Island is the only other card"
    );
    assert_eq!(b.runner.state().objects[&a].zone, Zone::Graveyard);
    assert_eq!(b.runner.state().objects[&c].zone, Zone::Graveyard);
}

/// Negatives (each reaches the same cast entry point as the positives above):
/// the alternative cost is not offered, so with no mana the cast is rejected up
/// front rather than dead-ending mid-payment.
#[test]
fn foil_alternative_cost_not_offered_without_two_distinct_cards() {
    // Reach guard: the positive shape casts.
    let mut ok = board(&[Card::Island, Card::Forest], false);
    let (i, f) = (ok.hand[0], ok.hand[1]);
    assert!(cast_with_alternative(&mut ok, &[i, f]).is_ok());

    for (name, hand) in [
        (
            "lone Island (one card cannot serve both legs)",
            vec![Card::Island],
        ),
        ("no Island", vec![Card::Forest, Card::Forest]),
        ("only Foil", vec![]),
        ("one Forest", vec![Card::Forest]),
    ] {
        let mut b = board(&hand, false);
        let err = cast_with_alternative(&mut b, &[]).expect_err(name);
        assert!(err.starts_with("cast rejected"), "{name}: {err}");
        assert!(
            b.hand
                .iter()
                .all(|id| b.runner.state().objects[id].zone == Zone::Hand),
            "{name}: nothing was discarded"
        );
    }
}

/// CR 118.9b: the alternative cost is optional — with the printed cost affordable
/// the player is offered the choice between both.
#[test]
fn foil_with_affordable_mana_offers_both_costs() {
    let mut b = board(&[Card::Island, Card::Forest], true);
    let card_id = b.runner.state().objects[&b.foil].card_id;
    let waiting = b
        .runner
        .act(GameAction::CastSpell {
            object_id: b.foil,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast")
        .waiting_for;
    // The cast surfaces the optional-cost choice between the two-discard
    // alternative cost and the printed mana cost (CR 118.9b).
    let WaitingFor::OptionalCostChoice {
        cost: AdditionalCost::Choice(alternative, printed),
        ..
    } = &waiting
    else {
        panic!("expected the alternative/printed cost choice, got {waiting:?}");
    };
    assert!(matches!(
        alternative,
        AbilityCost::Composite { costs } if costs.len() == 2
    ));
    assert!(matches!(printed, AbilityCost::Mana { .. }));
}

/// B1 guard: an ACTIVATED cost with two chosen hand-discard legs goes through the
/// activation payment path (not the spell alternative-cost path). It must pay
/// BOTH legs, and must not be offered when the hand cannot cover both.
#[test]
fn activated_two_discard_leg_cost_pays_both_legs_or_is_not_offered() {
    let build = |hand: usize| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_artifact_from_oracle(
                P0,
                "Pitch Engine",
                "Discard a card and another card: You gain 1 life.",
            )
            .id();
        let cards: Vec<ObjectId> = (0..hand)
            .map(|i| scenario.add_card_to_hand(P0, &format!("Card {i}")))
            .collect();
        (scenario.build(), source, cards)
    };

    // Positive: three cards in hand, both legs are paid, 2 cards discarded.
    let (mut runner, source, cards) = build(3);
    let mut waiting = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("activation with enough cards is offered")
        .waiting_for;
    let mut prompts = 0;
    for _ in 0..6 {
        match &waiting {
            WaitingFor::PayCost { choices, .. } => {
                prompts += 1;
                let pick = choices[0];
                waiting = runner
                    .act(GameAction::SelectCards { cards: vec![pick] })
                    .expect("select")
                    .waiting_for;
            }
            _ => break,
        }
    }
    let discarded = cards
        .iter()
        .filter(|id| runner.state().objects[id].zone == Zone::Graveyard)
        .count();
    assert_eq!(prompts, 2, "one prompt per discard leg");
    assert_eq!(discarded, 2, "both legs discard a card");

    // Hostile: one card cannot pay two legs.
    let (mut runner, source, _) = build(1);
    assert!(runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0
        })
        .is_err());
}
