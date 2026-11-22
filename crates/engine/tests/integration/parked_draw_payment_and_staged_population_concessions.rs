//! CR 800.4a + CR 117.2e + CR 616.1 + CR 101.4: one game in which two
//! concessions meet two restructurings of the same code. #10742 keeps a parked
//! draw payment alive across a departure (the payer's remaining draws are still
//! performed); #10637 settles a replacement the leaver chose through the
//! departure authority and schedules a population's choices in APNAP order.
//!
//! Four players, so the game goes on after both concessions. P0 pays Psychic Vortex's cumulative upkeep; P1's Notion Thief turns the
//! first of the two draws into a draw by P1, which parks on P1's Obstinate
//! Familiar. P1 concedes at that prompt (a replacement the leaver chose, inside
//! a surviving payment). P0 then casts Fated Retribution, a destruction
//! population in which P2 and P3 each hold an Umbra-armor choice; P2 concedes
//! at the Umbra-armor choice they were asked (a replacement the leaver chose,
//! with no payment left) and P3's choice, held behind it, is asked next. Printed Oracle text is quoted from the card data; the
//! board is synthetic only in that no printed card makes a payment whose draw
//! is replaced by an opponent's skip effect, as the Vortex tests it reuses.

use engine::game::engine::apply;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{PersistedGameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const PSYCHIC_VORTEX: &str = "Cumulative upkeep\u{2014}Draw a card. (At the beginning of your upkeep, put an age counter on this permanent, then sacrifice it unless you pay its upkeep cost for each age counter on it.)\nAt the beginning of your end step, sacrifice a land and discard your hand.";
const NOTION_THIEF: &str = "Flash\nIf an opponent would draw a card except the first one they draw in each of their draw steps, instead that player skips that draw and you draw a card.";
const OBSTINATE_FAMILIAR: &str = "If you would draw a card, you may skip that draw instead.";
const HYENA_UMBRA: &str = "Enchant creature\nEnchanted creature gets +1/+1 and has first strike.\nUmbra armor (If enchanted creature would be destroyed, instead remove all damage from it and destroy this Aura.)";
const SPIDER_UMBRA: &str = "Enchant creature\nEnchanted creature gets +1/+1 and has reach. (It can block creatures with flying.)\nUmbra armor (If enchanted creature would be destroyed, instead remove all damage from it and destroy this Aura.)";
const FATED_RETRIBUTION: &str =
    "Destroy all creatures and planeswalkers. If it's your turn, scry 2.";
const LIBRARY: [&str; 6] = ["A", "B", "C", "D", "E", "F"];
const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

/// Save the state, restore it through checked restore and keep playing on it.
fn round_trip(runner: &mut GameRunner) {
    runner
        .state()
        .resolution_stack
        .validate_at_rest(&runner.state().waiting_for)
        .expect("the state validates before the save");
    let saved = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("the state serializes");
    let restored = serde_json::from_str::<PersistedGameState>(&saved)
        .expect("the state decodes")
        .into_game_state()
        .expect("the state restores through checked restore");
    restored
        .resolution_stack
        .validate_at_rest(&restored.waiting_for)
        .expect("the reopened state validates");
    *runner.state_mut() = restored;
}

fn concede(runner: &mut GameRunner, player: PlayerId) -> Vec<GameEvent> {
    apply(
        runner.state_mut(),
        player,
        GameAction::Concede { player_id: player },
    )
    .expect("a player can concede at any time")
    .events
}

fn eliminations_of(events: &[GameEvent], player: PlayerId) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, GameEvent::PlayerEliminated { player_id } if *player_id == player))
        .count()
}

fn cards_drawn_by(events: &[GameEvent], player: PlayerId) -> usize {
    events
        .iter()
        .filter(
            |event| matches!(event, GameEvent::CardDrawn { player_id, .. } if *player_id == player),
        )
        .count()
}

fn zone(runner: &GameRunner, object: ObjectId) -> Zone {
    runner.state().objects[&object].zone
}

fn armored(
    scenario: &mut GameScenario,
    owner: PlayerId,
    name: &str,
) -> (ObjectId, ObjectId, ObjectId) {
    let creature = scenario.add_creature(owner, name, 2, 2).id();
    let hyena = scenario
        .add_enchantment_from_oracle(owner, "Hyena Umbra", HYENA_UMBRA)
        .with_subtypes(vec!["Aura"])
        .id();
    let spider = scenario
        .add_enchantment_from_oracle(owner, "Spider Umbra", SPIDER_UMBRA)
        .with_subtypes(vec!["Aura"])
        .id();
    (creature, hyena, spider)
}

#[test]
fn a_concession_inside_a_parked_draw_payment_then_one_inside_a_population_each_stand() {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::Untap);
    scenario.with_library_top(P0, &LIBRARY);
    scenario.with_library_top(P1, &LIBRARY);
    scenario.with_library_top(P2, &LIBRARY);
    scenario.with_library_top(P3, &LIBRARY);
    let vortex = scenario
        .add_enchantment_from_oracle(P0, "Psychic Vortex", PSYCHIC_VORTEX)
        .id();
    scenario.add_creature_from_oracle(P1, "Notion Thief", 3, 1, NOTION_THIEF);
    scenario.add_creature_from_oracle(P1, "Obstinate Familiar", 1, 1, OBSTINATE_FAMILIAR);
    let p0_creature = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let (p2_creature, p2_hyena, p2_spider) = armored(&mut scenario, P2, "Runeclaw Bear");
    let (p3_creature, p3_hyena, p3_spider) = armored(&mut scenario, P3, "Bear Cub");
    let retribution = scenario
        .add_spell_to_hand_from_oracle(P0, "Fated Retribution", true, FATED_RETRIBUTION)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&vortex)
        .expect("Psychic Vortex exists")
        .counters
        .insert(CounterType::Age, 1);
    for (aura, host) in [
        (p2_hyena, p2_creature),
        (p2_spider, p2_creature),
        (p3_hyena, p3_creature),
        (p3_spider, p3_creature),
    ] {
        engine::game::effects::attach::attach_to(runner.state_mut(), aura, host);
    }
    engine::game::layers::flush_layers(runner.state_mut());

    // The upkeep draw payment: two instructions, the first replaced by P1.
    runner.auto_advance_to_main_phase();
    runner.advance_until_stack_empty();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::UnlessPayment { player: P0, .. }
        ),
        "reach guard: P0 is asked to pay Psychic Vortex's upkeep: {:?}",
        runner.state().waiting_for
    );
    let hand_before = runner.state().players[0].hand.len();
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("choosing the draw cost is legal");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ReplacementChoice { player: P1, .. }),
        "reach guard: P1's Obstinate Familiar offers the skip of the draw Notion Thief made P1's: {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().pending_replacement.is_some()
            && runner.state().pending_cost_move_resume.is_some(),
        "reach guard: a replacement the leaver chose is parked inside P0's surviving payment"
    );

    // First concession: P1 leaves at that prompt.
    let first = concede(&mut runner, P1);
    assert_eq!(
        eliminations_of(&first, P1),
        1,
        "the departure is published once"
    );
    assert_eq!(
        cards_drawn_by(&first, P1),
        0,
        "the leaver's draw does not happen"
    );
    assert_eq!(
        cards_drawn_by(&first, P0),
        1,
        "the instruction still owed is performed for the surviving payer"
    );
    assert_eq!(runner.state().players[0].hand.len(), hand_before + 1);
    assert!(
        runner.state().pending_cost_move_resume.is_none(),
        "the payment is over"
    );
    assert!(runner.state().pending_replacement.is_none());
    assert_eq!(
        zone(&runner, vortex),
        Zone::Battlefield,
        "the upkeep was paid"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "priority only once the payment is over: {:?}",
        runner.state().waiting_for
    );
    round_trip(&mut runner);

    // The population: P0's plain creature is destroyed; P2, next in turn order,
    // is asked about their armor and P3's choice is held behind it.
    runner.cast(retribution).commit();
    for _ in 0..8 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority passes");
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player: P2, .. }
        ),
        "reach guard: P2 is the first seat with a choice: {:?}",
        runner.state().waiting_for
    );
    round_trip(&mut runner);

    // Second concession: P2 leaves at the armor choice they were asked.
    let second = concede(&mut runner, P2);
    assert_eq!(
        eliminations_of(&second, P2),
        1,
        "the second departure is published once"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player: P3, .. }
        ),
        "the choice held behind the leaver's is asked next: {:?}",
        runner.state().waiting_for
    );
    round_trip(&mut runner);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("P3 answers their armor choice");
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            // "If it's your turn, scry 2": keep both cards on top.
            WaitingFor::ScryChoice { .. } => {
                runner
                    .act(GameAction::SelectCards { cards: vec![] })
                    .expect("the scry is answered");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority passes");
            }
        }
    }
    assert_eq!(
        zone(&runner, p3_creature),
        Zone::Battlefield,
        "the armor saved P3's creature"
    );
    assert_eq!(
        zone(&runner, p0_creature),
        Zone::Graveyard,
        "P0's plain creature was destroyed"
    );
    assert_eq!(
        zone(&runner, retribution),
        Zone::Graveyard,
        "stack {:?} waiting {:?}",
        runner.state().stack,
        runner.state().waiting_for
    );
    assert_ne!(
        zone(&runner, p2_creature),
        Zone::Battlefield,
        "the leaver's creature left with them"
    );
    assert!(runner.state().pending_replacement.is_none());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));

    // A checked save and reopen takes a successful next action.
    round_trip(&mut runner);
    let WaitingFor::Priority { player } = runner.state().waiting_for.clone() else {
        panic!("a living player has priority");
    };
    assert!(runner.state().players[1].is_eliminated && runner.state().players[2].is_eliminated);
    apply(runner.state_mut(), player, GameAction::PassPriority)
        .expect("the next ordinary action starts from the two departures");
}
