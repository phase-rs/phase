//! Issue #9841 — Thought Rattle: "Target opponent reveals each nonland card in
//! their hand. You choose one of those cards. Exile that card."
//!
//! CR 701.20a: the revealed cards are already shown to every player, so the
//! controller's only decision is the `ChooseFromZoneChoice` — no separate
//! `RevealChoice` acknowledgement may precede it.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const THOUGHT_RATTLE: &str = "Target opponent reveals each nonland card in their hand. You choose one of those cards. Exile that card.\nThreshold — If seven or more cards are in your graveyard, seek a Rat card. It perpetually gains \"This spell costs {1} less to cast.\"\nYou gain 1 life for each Rat card in your hand.";

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
    runner
        .act(GameAction::CastSpell {
            object_id: rattle,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Thought Rattle");

    let mut saw_choose = false;
    for _ in 0..12 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ChooseFromZoneChoice { cards, player, .. } => {
                assert_eq!(player, P0);
                assert!(cards.contains(&bolt) && cards.contains(&bear));
                assert!(!cards.contains(&land), "lands are not revealed");
                saw_choose = true;
                runner
                    .act(GameAction::SelectCards { cards: vec![bear] })
                    .expect("choose the bear");
            }
            WaitingFor::RevealChoice { .. } => {
                panic!("redundant RevealChoice before ChooseFromZoneChoice")
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.pass_both_players();
            }
            WaitingFor::Priority { .. } => break,
            _ => {
                let _ = runner.act(GameAction::PassPriority);
            }
        }
    }
    assert!(saw_choose, "ChooseFromZoneChoice must be reached directly");
    assert_eq!(runner.state().objects[&bear].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&bolt].zone, Zone::Hand);
    assert!(runner.state().stack.is_empty());
}
