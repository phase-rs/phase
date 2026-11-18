//! Issue #9841 — Thought Rattle: "Target opponent reveals each nonland card in
//! their hand. You choose one of those cards. Exile that card."
//!
//! CR 701.20a: the revealed cards are already shown to every player, so the
//! controller's only decision is the `ChooseFromZoneChoice` — no separate
//! `RevealChoice` acknowledgement may precede it.
//!
//! CR 608.2c: genuine card partitions (e.g. Karn, Scion of Urza's +1) continue
//! to forward the unchosen complement to downstream instructions.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::visibility::filter_state_for_viewer;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const THOUGHT_RATTLE: &str = "Target opponent reveals each nonland card in their hand. You choose one of those cards. Exile that card.\nThreshold — If seven or more cards are in your graveyard, seek a Rat card. It perpetually gains \"This spell costs {1} less to cast.\"\nYou gain 1 life for each Rat card in your hand.";

const KARN_SCION_OF_URZA: &str = "+1: Reveal the top two cards of your library. An opponent chooses one of them. Put that card into your hand and exile the other with a silver counter on it.\n−1: Put a card you own with a silver counter on it from exile into your hand.\n−2: Create a 0/0 colorless Construct artifact creature token with \"This token gets +1/+1 for each artifact you control.\"";

fn drain_priority_collect_events(
    runner: &mut engine::game::scenario::GameRunner,
    all_events: &mut Vec<GameEvent>,
) {
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if let Ok(result) = runner.act(GameAction::PassPriority) {
                    all_events.extend(result.events);
                } else {
                    return;
                }
            }
            _ => return,
        }
    }
}

#[test]
fn thought_rattle_single_choose_prompt_then_exile() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let land = scenario.add_land_to_hand(P1, "Opp Land").id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P1, "Opp Bolt", true, "")
        .id();
    let bear = scenario.add_creature_to_hand(P1, "Opp Bear", 2, 2).id();
    let rattle = {
        let mut b =
            scenario.add_spell_to_hand_from_oracle(P0, "Thought Rattle", false, THOUGHT_RATTLE);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        });
        b.id()
    };
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Black);

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&rattle].card_id;
    let mut all_events = Vec::new();

    // Cast the spell
    let cast_result = runner
        .act(GameAction::CastSpell {
            object_id: rattle,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Thought Rattle");
    all_events.extend(cast_result.events);

    // Pass priority to let the spell resolve
    drain_priority_collect_events(&mut runner, &mut all_events);

    // The resolution should have emitted CardsRevealed with the two nonlands.
    let revealed_event = all_events
        .iter()
        .find_map(|e| match e {
            GameEvent::CardsRevealed { card_ids, .. } => Some(card_ids.clone()),
            _ => None,
        })
        .expect("CardsRevealed event must be emitted");
    assert_eq!(
        revealed_event.len(),
        2,
        "exactly two nonland cards revealed"
    );
    assert!(revealed_event.contains(&bolt) && revealed_event.contains(&bear));
    assert!(
        !revealed_event.contains(&land),
        "land must not be in CardsRevealed"
    );

    // Now we should be at ChooseFromZoneChoice
    match runner.state().waiting_for.clone() {
        WaitingFor::ChooseFromZoneChoice { cards, player, .. } => {
            assert_eq!(player, P0);
            assert!(cards.contains(&bolt) && cards.contains(&bear));
            assert!(
                !cards.contains(&land),
                "lands are not offered to ChooseFromZoneChoice"
            );
        }
        other => panic!("expected ChooseFromZoneChoice, got {other:?}"),
    }

    // While the choice is open, the controller's filtered view must not reveal the land.
    let controller_view = filter_state_for_viewer(runner.state(), P0);
    let land_obj = controller_view
        .objects
        .get(&land)
        .expect("land exists in view");
    assert_eq!(
        land_obj.name, "Hidden Card",
        "viewer-filtered state must conceal the land while choice is open"
    );

    // Make the choice
    let choose_result = runner
        .act(GameAction::SelectCards { cards: vec![bear] })
        .expect("choose the bear");
    all_events.extend(choose_result.events);

    // Pass priority to finish resolution
    for _ in 0..10 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if let Ok(result) = runner.act(GameAction::PassPriority) {
                    all_events.extend(result.events);
                } else {
                    break;
                }
            }
            WaitingFor::ChooseFromZoneChoice { .. } => {
                panic!("ChooseFromZoneChoice still pending after selection");
            }
            WaitingFor::RevealChoice { .. } => {
                panic!("Unexpected RevealChoice after ChooseFromZoneChoice");
            }
            other => panic!("unexpected waiting_for state: {other:?}"),
        }
    }

    // Verify final state
    let bear_zone = runner.state().objects[&bear].zone;
    let bolt_zone = runner.state().objects[&bolt].zone;
    assert_eq!(bear_zone, Zone::Exile);
    assert_eq!(bolt_zone, Zone::Hand);
    assert!(runner.state().stack.is_empty());
}

/// CR 608.2c: Karn, Scion of Urza +1 partitions revealed cards:
/// "Reveal the top two cards of your library. An opponent chooses one of them.
/// Put that card into your hand and exile the other with a silver counter on it."
///
/// The chosen card goes to the player's hand, while the unchosen complement
/// is forwarded to the exile instruction with the silver counter.
#[test]
fn karn_scion_of_urza_plus_one_partitions_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let karn = scenario
        .add_planeswalker_from_oracle(P0, "Karn, Scion of Urza", "Karn", 5, KARN_SCION_OF_URZA)
        .id();
    let card_b = scenario.add_spell_to_library_top(P0, "Card B", true).id();
    let card_a = scenario.add_spell_to_library_top(P0, "Card A", true).id();
    let mut runner = scenario.build();

    let mut all_events = Vec::new();

    // P0 activates Karn's +1 (ability index 0).
    let act_result = runner
        .act(GameAction::ActivateAbility {
            source_id: karn,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Karn +1");
    all_events.extend(act_result.events);

    // Pass priority to let Karn's +1 resolve from the stack.
    drain_priority_collect_events(&mut runner, &mut all_events);

    // Resolution prompts the opponent (P1) to choose one of the revealed cards.
    match runner.state().waiting_for.clone() {
        WaitingFor::ChooseFromZoneChoice { cards, player, .. } => {
            assert_eq!(player, P1, "opponent makes the partition choice");
            assert!(
                cards.contains(&card_a) && cards.contains(&card_b),
                "both library-top cards offered to ChooseFromZoneChoice: {cards:?}"
            );
        }
        other => panic!("expected ChooseFromZoneChoice for P1, got {other:?}"),
    }

    // Opponent (P1) chooses card_a.
    let choose_result = runner
        .act(GameAction::SelectCards {
            cards: vec![card_a],
        })
        .expect("P1 selects card_a");
    all_events.extend(choose_result.events);

    // Pass priority to finish resolving downstream instructions.
    drain_priority_collect_events(&mut runner, &mut all_events);

    // CR 608.2c: The chosen card goes to P0's hand, and the unchosen complement
    // goes to exile with a silver counter on it.
    let obj_a = &runner.state().objects[&card_a];
    let obj_b = &runner.state().objects[&card_b];

    assert_eq!(obj_a.zone, Zone::Hand, "chosen card goes to hand");
    assert!(
        obj_a.counters.is_empty(),
        "chosen card in hand must have no counters"
    );

    assert_eq!(obj_b.zone, Zone::Exile, "unchosen card goes to exile");
    let silver_counter = CounterType::Generic("silver".to_string());
    assert_eq!(
        obj_b.counters.get(&silver_counter).copied().unwrap_or(0),
        1,
        "unchosen card in exile must have exactly one silver counter"
    );

    assert!(runner.state().stack.is_empty(), "stack must be empty");
}
