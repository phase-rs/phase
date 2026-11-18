//! Issue #9841 — Thought Rattle: "Target opponent reveals each nonland card in
//! their hand. You choose one of those cards. Exile that card."
//!
//! CR 701.20a: the revealed cards are already shown to every player, so the
//! controller's only decision is the `ChooseFromZoneChoice` — no separate
//! `RevealChoice` acknowledgement may precede it.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::visibility::filter_state_for_viewer;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const THOUGHT_RATTLE: &str = "Target opponent reveals each nonland card in their hand. You choose one of those cards. Exile that card.\nThreshold — If seven or more cards are in your graveyard, seek a Rat card. It perpetually gains \"This spell costs {1} less to cast.\"\nYou gain 1 life for each Rat card in your hand.";

fn print_ability_chain(def: &engine::types::ability::AbilityDefinition, indent: usize) {
    let prefix = "  ".repeat(indent);
    eprintln!("{}effect={:?}, sub_ability={}", prefix, def.effect, def.sub_ability.is_some());
    if let Some(sub) = &def.sub_ability {
        print_ability_chain(sub, indent + 1);
    }
    if let Some(else_ability) = &def.else_ability {
        eprintln!("{}else_ability:", prefix);
        print_ability_chain(else_ability, indent + 1);
    }
    for mode_def in &def.mode_abilities {
        eprintln!("{}mode_ability:", prefix);
        print_ability_chain(mode_def, indent + 1);
    }
}

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
    eprintln!("DEBUG: rattle object id = {:?}", rattle);
    eprintln!("DEBUG: rattle card id = {:?}", card_id);
    if let Some(obj) = runner.state().objects.get(&rattle) {
        eprintln!("DEBUG: rattle abilities = {:?}", obj.abilities.len());
        for (i, def) in obj.abilities.iter().enumerate() {
            eprintln!("DEBUG: ability {}: kind={:?}, effect={:?}", i, def.kind, def.effect);
            print_ability_chain(def, 0);
        }
    }
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
    assert_eq!(revealed_event.len(), 2, "exactly two nonland cards revealed");
    assert!(revealed_event.contains(&bolt) && revealed_event.contains(&bear));
    assert!(!revealed_event.contains(&land), "land must not be in CardsRevealed");

    // Now we should be at ChooseFromZoneChoice
    match runner.state().waiting_for.clone() {
        WaitingFor::ChooseFromZoneChoice { cards, player, .. } => {
            assert_eq!(player, P0);
            assert!(cards.contains(&bolt) && cards.contains(&bear));
            assert!(!cards.contains(&land), "lands are not offered to ChooseFromZoneChoice");
        }
        other => panic!("expected ChooseFromZoneChoice, got {other:?}"),
    }

    // While the choice is open, the controller's filtered view must not reveal the land.
    let controller_view = filter_state_for_viewer(runner.state(), P0);
    let land_obj = controller_view.objects.get(&land).expect("land exists in view");
    assert_eq!(
        land_obj.name, "Hidden Card",
        "viewer-filtered state must conceal the land while choice is open"
    );

    // Make the choice
    let choose_result = runner
        .act(GameAction::SelectCards { cards: vec![bear] })
        .expect("choose the bear");
    all_events.extend(choose_result.events);
    
    // Check zones immediately after choice
    eprintln!("DEBUG after choice: bear zone={:?}, bolt zone={:?}, land zone={:?}", 
        runner.state().objects[&bear].zone,
        runner.state().objects[&bolt].zone,
        runner.state().objects[&land].zone);

    // Pass priority to finish resolution
    for i in 0..10 {
        eprintln!("DEBUG priority pass {}: waiting_for={:?}", i, runner.state().waiting_for);
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if let Ok(result) = runner.act(GameAction::PassPriority) {
                    all_events.extend(result.events);
                    eprintln!("DEBUG after pass {}: bear zone={:?}, bolt zone={:?}", i, runner.state().objects[&bear].zone, runner.state().objects[&bolt].zone);
                } else {
                    break;
                }
            }
            WaitingFor::ChooseFromZoneChoice { .. } => {
                // Choice still pending (should not happen after we made our selection)
                panic!("ChooseFromZoneChoice still pending after selection");
            }
            WaitingFor::RevealChoice { .. } => {
                // Redundant reveal prompt should not appear
                panic!("Unexpected RevealChoice after ChooseFromZoneChoice");
            }
            other => panic!("unexpected waiting_for state: {other:?}"),
        }
    }

// Verify final state
    let bear_zone = runner.state().objects[&bear].zone;
    let bolt_zone = runner.state().objects[&bolt].zone;
    eprintln!("bear zone: {:?}, bolt zone: {:?}", bear_zone, bolt_zone);
    assert_eq!(bear_zone, Zone::Exile);
    assert_eq!(bolt_zone, Zone::Hand);
    assert!(runner.state().stack.is_empty());
}