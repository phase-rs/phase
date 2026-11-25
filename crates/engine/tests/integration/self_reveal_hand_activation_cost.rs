//! CR 701.20a + CR 701.20b: activation costs in which a card reveals itself from hand (augment,
//! forecast, self-name reveals) are payable, reveal the source, and leave it in hand.

use engine::ai_support::legal_actions;
use engine::game::casting::can_cast_object_now;
use engine::game::engine::EngineError;
use engine::game::game_object::MergeKind;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{AbilityCost, AbilityTag};
use engine::types::actions::GameAction;
use engine::types::card_type::Supertype;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim Oracle text (MTGJSON).
const MONKEY: &str = "Whenever a nontoken creature you control dies,\nAugment {2}{G} ({2}{G}, Reveal this card from your hand: Combine it with target host. Augment only as a sorcery.)";
const ADORABLE_KITTEN: &str =
    "When this creature enters, roll a six-sided die. You gain life equal to the result.";
const PRIDE_OF_THE_CLOUDS: &str = "Flying\nThis creature gets +1/+1 for each other creature on the battlefield with flying.\nForecast — {2}{W}{U}, Reveal this creature from your hand: Create a 1/1 white and blue Bird creature token with flying. (Activate only during your upkeep and only once each turn.)";
const HEIR_TO_DRAGONFIRE: &str = "{R}: Heir to Dragonfire gets +1/+0 until end of turn.\n{2}{R}, Reveal Heir to Dragonfire from your hand: Heir to Dragonfire perpetually becomes a Dragon, gets +3/+3, and gains flying.";
const SADISTIC_SKYMARCHER: &str = "As an additional cost to cast this spell, reveal a Vampire card from your hand or pay {1}.\nFlying, lifelink";

fn pool(types: &[ManaType]) -> Vec<ManaUnit> {
    types
        .iter()
        .map(|t| ManaUnit::new(*t, ObjectId(0), false, vec![]))
        .collect()
}

fn offered(runner: &GameRunner, source: ObjectId, index: usize) -> bool {
    legal_actions(runner.state()).iter().any(|a| {
        matches!(a, GameAction::ActivateAbility { source_id, ability_index, .. }
            if *source_id == source && *ability_index == index)
    })
}

fn activate(
    runner: &mut GameRunner,
    source: ObjectId,
    index: usize,
) -> Result<Vec<GameEvent>, EngineError> {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .map(|r| r.events)
}

fn revealed(events: &[GameEvent]) -> Vec<Vec<ObjectId>> {
    events
        .iter()
        .filter_map(|e| match e {
            GameEvent::CardsRevealed { card_ids, .. } => Some(card_ids.clone()),
            _ => None,
        })
        .collect()
}

fn reveal_ability_index(runner: &GameRunner, source: ObjectId) -> usize {
    let is_reveal = |c: &AbilityCost| matches!(c, AbilityCost::Reveal { .. });
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|a| match &a.cost {
            Some(AbilityCost::Composite { costs }) => costs.iter().any(is_reveal),
            Some(c) => is_reveal(c),
            None => false,
        })
        .expect("activated ability with a reveal cost")
}

/// Monkey- in hand and Adorable Kitten on P0's battlefield, optionally a Host.
fn monkey_board(kitten_is_host: bool) -> (GameRunner, ObjectId, ObjectId, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kitten = scenario
        .add_creature_from_oracle(P0, "Adorable Kitten", 1, 1, ADORABLE_KITTEN)
        .id();
    let monkey = scenario
        .add_creature_to_hand(P0, "Monkey-", 2, 2)
        .from_oracle_text_with_keywords(&["Augment"], MONKEY)
        .id();
    scenario.with_mana_pool(
        P0,
        pool(&[ManaType::Green, ManaType::Colorless, ManaType::Colorless]),
    );
    let mut runner = scenario.build();
    if kitten_is_host {
        let obj = runner.state_mut().objects.get_mut(&kitten).unwrap();
        obj.card_types.supertypes.push(Supertype::Host);
        obj.base_card_types = obj.card_types.clone();
    }
    let augment = runner.state().objects[&monkey]
        .abilities
        .iter()
        .position(|a| a.ability_tag == Some(AbilityTag::Augment))
        .expect("synthesized augment ability");
    assert_eq!(
        runner.state().objects[&monkey].abilities[augment].activation_zone,
        Some(Zone::Hand)
    );
    (runner, monkey, kitten, augment)
}

#[test]
fn augment_reveals_itself_from_hand_and_combines_with_the_host() {
    let (mut runner, monkey, host, augment) = monkey_board(true);

    assert!(offered(&runner, monkey, augment));
    let events = activate(&mut runner, monkey, augment).expect("augment activates");
    assert_eq!(revealed(&events), vec![vec![monkey]]);
    assert_eq!(runner.state().objects[&monkey].zone, Zone::Hand);
    assert_eq!(runner.state().stack.len(), 1);

    runner.advance_until_stack_empty();
    let host_obj = &runner.state().objects[&host];
    assert!(host_obj.merged_components.contains(&monkey));
    assert_eq!(host_obj.merge_kind, Some(MergeKind::Augment));
    assert!(!runner.state().players[0].hand.contains(&monkey));
}

#[test]
fn augment_without_a_host_is_not_activatable() {
    let (mut runner, monkey, _, augment) = monkey_board(false);

    assert!(!offered(&runner, monkey, augment));
    assert!(matches!(
        activate(&mut runner, monkey, augment),
        Err(EngineError::ActionNotAllowed(_))
    ));
}

fn pride_board(with_other_card: bool) -> (GameRunner, ObjectId, Option<ObjectId>, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);
    let pride = scenario
        .add_creature_to_hand_from_oracle(P0, "Pride of the Clouds", 1, 1, PRIDE_OF_THE_CLOUDS)
        .id();
    let other = with_other_card.then(|| {
        scenario
            .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
            .id()
    });
    scenario.with_mana_pool(
        P0,
        pool(&[
            ManaType::White,
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    let runner = scenario.build();
    let index = reveal_ability_index(&runner, pride);
    assert_eq!(
        runner.state().objects[&pride].abilities[index].activation_zone,
        Some(Zone::Hand)
    );
    (runner, pride, other, index)
}

fn birds(runner: &GameRunner) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| runner.state().objects[id].name == "Bird")
        .count()
}

#[test]
fn forecast_this_creature_reveals_itself_from_hand() {
    let (mut runner, pride, _, index) = pride_board(false);

    assert!(offered(&runner, pride, index));
    let events = activate(&mut runner, pride, index).expect("forecast activates");
    assert_eq!(revealed(&events), vec![vec![pride]]);
    assert_eq!(runner.state().objects[&pride].zone, Zone::Hand);

    runner.advance_until_stack_empty();
    assert_eq!(birds(&runner), 1);
}

#[test]
fn self_reveal_never_reveals_another_hand_card() {
    let (mut runner, pride, other, index) = pride_board(true);
    let bears = other.unwrap();

    assert!(offered(&runner, pride, index));
    let events = activate(&mut runner, pride, index).expect("forecast activates");
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::PayCost { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    assert_eq!(revealed(&events), vec![vec![pride]]);
    assert!(!revealed(&events).iter().flatten().any(|id| *id == bears));
    assert_eq!(runner.state().objects[&pride].zone, Zone::Hand);
}

#[test]
fn self_name_reveal_functions_from_hand() {
    let mut scenario = GameScenario::new();
    let heir = scenario
        .add_creature_to_hand_from_oracle(P0, "Heir to Dragonfire", 2, 2, HEIR_TO_DRAGONFIRE)
        .id();
    let runner = scenario.build();
    let ability = &runner.state().objects[&heir].abilities[reveal_ability_index(&runner, heir)];

    assert_eq!(ability.activation_zone, Some(Zone::Hand));
    let Some(AbilityCost::Composite { costs }) = &ability.cost else {
        panic!("composite cost: {:?}", ability.cost);
    };
    assert!(costs
        .iter()
        .any(|c| matches!(c, AbilityCost::Reveal { filter: None, .. })));
}

/// CR 601.2a: a spell is on the stack while its costs are paid, so its own "reveal a card from
/// your hand" cost cannot be paid by revealing itself.
#[test]
fn spell_reveal_cost_still_excludes_the_spell_itself() {
    let castable = |with_second_vampire: bool| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let skymarcher = scenario
            .add_creature_to_hand_from_oracle(P0, "Sadistic Skymarcher", 3, 2, SADISTIC_SKYMARCHER)
            .with_mana_cost(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Black],
            })
            .with_subtypes(vec!["Vampire", "Soldier"])
            .id();
        if with_second_vampire {
            scenario
                .add_creature_to_hand(P0, "Vampire Nighthawk", 2, 3)
                .with_subtypes(vec!["Vampire", "Shaman"]);
        }
        scenario.with_mana_pool(
            P0,
            pool(&[ManaType::Black, ManaType::Colorless, ManaType::Colorless]),
        );
        let runner = scenario.build();
        can_cast_object_now(runner.state(), P0, skymarcher)
    };

    assert!(castable(true), "another Vampire pays the reveal");
    assert!(!castable(false), "the spell cannot reveal itself");
}
