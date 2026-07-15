//! Regression coverage for non-mana flashback cost payability.

use engine::ai_support::legal_actions;
use engine::game::casting::{can_cast_object_now, spell_objects_available_to_cast};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const GROUP_PROJECT_ORACLE: &str = "Create a 2/2 red and white Spirit creature token.\n\
Flashback—Tap three untapped creatures you control. (You may cast this card from your graveyard \
for its flashback cost. Then exile it.)";

const FESTIVAL_OF_EMBERS_ORACLE: &str = "During your turn, you may cast instant and sorcery \
spells from your graveyard by paying 1 life in addition to their other costs.\n\
If a card or token would be put into your graveyard from anywhere, exile it instead.\n\
{1}{R}: Sacrifice this enchantment.";

fn has_cast_action(runner: &GameRunner, spell: ObjectId) -> bool {
    legal_actions(runner.state()).iter().any(|action| {
        matches!(
            action,
            GameAction::CastSpell { object_id, .. } if *object_id == spell
        )
    })
}

fn group_project_with_helpers(eligible_count: usize) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_graveyard(P0, "Group Project", false)
        .from_oracle_text(GROUP_PROJECT_ORACLE)
        .id();
    let eligible: Vec<ObjectId> = (0..eligible_count)
        .map(|index| {
            scenario
                .add_creature(P0, &format!("Eligible helper {index}"), 1, 1)
                .id()
        })
        .collect();
    let tapped = scenario.add_creature(P0, "Tapped helper", 1, 1).id();
    scenario.add_creature(P1, "Opponent's untapped helper", 1, 1);

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&tapped)
        .expect("tapped helper exists")
        .tapped = true;
    (runner, spell, eligible)
}

fn spirit_token_count(state: &GameState) -> usize {
    state
        .objects
        .values()
        .filter(|object| {
            object.controller == P0
                && object.zone == Zone::Battlefield
                && object.is_token
                && object.name == "Spirit"
        })
        .count()
}

fn pool_units(colors: &[ManaType]) -> Vec<ManaUnit> {
    colors
        .iter()
        .map(|&color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect()
}

/// CR 702.34a + CR 118.3 + CR 601.2h: a card may be cast via flashback only
/// when its complete alternative cost can be paid. Tapped creatures and
/// creatures controlled by another player cannot satisfy this tap cost.
#[test]
fn group_project_flashback_requires_three_eligible_creatures() {
    let (runner, spell, _) = group_project_with_helpers(2);

    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&spell),
        "the coarse graveyard-keyword scan must reach the flashback candidate"
    );
    assert!(!can_cast_object_now(runner.state(), P0, spell));
    assert!(!has_cast_action(&runner, spell));

    let (runner, spell, eligible) = group_project_with_helpers(3);
    assert_eq!(eligible.len(), 3);
    assert!(can_cast_object_now(runner.state(), P0, spell));
    assert!(has_cast_action(&runner, spell));
}

/// CR 702.34a + CR 601.2h: the chosen creatures are tapped as the flashback
/// cost is paid, the spell resolves, and flashback exiles it from the stack.
#[test]
fn group_project_flashback_pays_and_resolves_through_cast_pipeline() {
    let (mut runner, spell, helpers) = group_project_with_helpers(3);

    let outcome = runner.cast(spell).pay_cost_with(&helpers).resolve();

    for helper in helpers {
        assert!(
            outcome.state().objects[&helper].tapped,
            "selected helper must be tapped to pay the flashback cost"
        );
    }
    outcome.assert_zone(&[spell], Zone::Exile);
    assert_eq!(
        spirit_token_count(outcome.state()),
        1,
        "Group Project must create one Spirit token"
    );
}

/// CR 601.2f + CR 702.34a: an unpayable flashback option must not suppress an
/// independently payable graveyard-cast permission. With no creatures to tap,
/// the cast uses Festival's printed-cost-plus-one-life path without requiring a
/// variant choice.
#[test]
fn festival_permission_remains_available_when_flashback_is_unpayable() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    scenario
        .add_creature(P0, "Festival of Embers", 0, 0)
        .as_enchantment()
        .from_oracle_text(FESTIVAL_OF_EMBERS_ORACLE);
    let spell = scenario
        .add_spell_to_graveyard(P0, "Group Project", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 1,
        })
        .from_oracle_text(GROUP_PROJECT_ORACLE)
        .id();
    scenario.with_mana_pool(P0, pool_units(&[ManaType::Colorless, ManaType::White]));
    let mut runner = scenario.build();

    assert!(can_cast_object_now(runner.state(), P0, spell));
    let outcome = runner.cast(spell).resolve();

    outcome.assert_life_delta(P0, -1);
    assert_eq!(
        spirit_token_count(outcome.state()),
        1,
        "the Festival cast must resolve Group Project normally"
    );
}
