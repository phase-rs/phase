//! Brain Pry — "Choose a nonland card name. Target player reveals their hand.
//! That player discards a card with that name. If they can't, you draw a card."
//!
//! CR 201.4: the chosen name is public information, so the game log must state
//! it. CR 608.2c: the discard is limited to cards with that name (CR 201.2), and
//! the "if they can't" rider draws only when no such card was discarded.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCondition, AbilityDefinition, ChoiceType, Effect, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::log::LogSegment;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const BRAIN_PRY: &str = "Choose a nonland card name. Target player reveals their hand. That \
player discards a card with that name. If they can't, you draw a card.";

struct Setup {
    runner: GameRunner,
    hand: Vec<ObjectId>,
}

fn setup(opponent_hand: &[&str]) -> (Setup, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 11);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Library Card"]);
    let hand = opponent_hand
        .iter()
        .map(|name| scenario.add_card_to_hand(P1, name))
        .collect();
    let mut builder = scenario.add_spell_to_hand_from_oracle(P0, "Brain Pry", false, BRAIN_PRY);
    builder.with_mana_cost(ManaCost::Cost {
        generic: 1,
        shards: vec![ManaCostShard::Black],
    });
    let spell = builder.id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, spell, false, vec![]),
            ManaUnit::new(ManaType::Black, spell, false, vec![]),
        ],
    );
    let mut runner = scenario.build();
    runner.state_mut().all_card_names = ["Opt", "Shock", "Forest"].map(String::from).into();
    (Setup { runner, hand }, spell)
}

/// Casts Brain Pry at P1 naming `named`, answering every prompt, and returns
/// every event raised.
fn cast_naming(setup: &mut Setup, spell: ObjectId, named: &str) -> Vec<GameEvent> {
    let card_id = setup.runner.state().objects[&spell].card_id;
    let mut events = Vec::new();
    let mut result = setup
        .runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast");
    for _ in 0..32 {
        events.extend(result.events.clone());
        let action = match &result.waiting_for {
            WaitingFor::TargetSelection { .. } => GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            },
            WaitingFor::NamedChoice { .. } => GameAction::ChooseOption {
                choice: named.to_string(),
            },
            WaitingFor::DiscardChoice { cards, .. } => GameAction::SelectCards {
                cards: cards.iter().take(1).copied().collect(),
            },
            WaitingFor::Priority { .. } if setup.runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            _ => GameAction::PassPriority,
        };
        result = setup.runner.act(action).expect("prompt answer");
    }
    events
}

fn walk<'a>(def: &'a AbilityDefinition, out: &mut Vec<&'a AbilityDefinition>) {
    out.push(def);
    if let Some(sub) = def.sub_ability.as_deref() {
        walk(sub, out);
    }
}

#[test]
fn parses_discard_filter_and_if_they_cant_rider() {
    let parsed = engine::parser::oracle::parse_oracle_text(
        BRAIN_PRY,
        "Brain Pry",
        &[],
        &["Sorcery".to_owned()],
        &[],
    );
    let mut chain = Vec::new();
    walk(&parsed.abilities[0], &mut chain);
    assert!(matches!(
        &*chain[0].effect,
        Effect::Choose {
            choice_type: ChoiceType::CardName,
            ..
        }
    ));
    let Effect::Discard { filter, .. } = &*chain[2].effect else {
        panic!("expected Discard, got {:?}", chain[2].effect);
    };
    assert_eq!(filter.as_ref(), Some(&TargetFilter::HasChosenName));
    assert!(matches!(&*chain[3].effect, Effect::Draw { .. }));
    assert!(matches!(
        chain[3].condition,
        Some(AbilityCondition::Not { .. })
    ));
}

#[test]
fn naming_a_card_is_logged_and_only_that_card_is_discarded() {
    let (mut setup, spell) = setup(&["Opt", "Shock"]);
    let events = cast_naming(&mut setup, spell, "Shock");
    let state = setup.runner.state();

    assert!(events.iter().any(|event| matches!(
        event,
        GameEvent::CardNamed { player_id, name, .. } if *player_id == P0 && name == "Shock"
    )));
    let entries = engine::game::log::resolve_log_entries(&events, state, state);
    assert!(
        entries.iter().any(|entry| entry
            .segments
            .iter()
            .any(|segment| matches!(segment, LogSegment::Text(text) if text == "Shock"))),
        "the log must state the named card"
    );
    assert_eq!(state.objects[&setup.hand[1]].zone, Zone::Graveyard);
    assert_eq!(state.objects[&setup.hand[0]].zone, Zone::Hand);
    assert_eq!(
        state.players[0].hand.len(),
        0,
        "a discard happened: no draw"
    );
}

#[test]
fn no_matching_card_discards_nothing_and_controller_draws() {
    let (mut setup, spell) = setup(&["Opt", "Shock"]);
    cast_naming(&mut setup, spell, "Forest");
    let state = setup.runner.state();

    for id in &setup.hand {
        assert_eq!(
            state.objects[id].zone,
            Zone::Hand,
            "nothing may be discarded"
        );
    }
    assert_eq!(
        state.players[0].hand.len(),
        1,
        "\"if they can't\" draws a card"
    );
}
