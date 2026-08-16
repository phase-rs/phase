//! Regression for Mauhur, Uruk-hai Captain with Swarming of Moria.
//!
//! CR 701.47a: amass Orcs puts counters on the chosen Army. Mauhur does not
//! move those counters to itself; it increases the number put on that Army.

use engine::game::effects::counters::add_counter_with_replacement;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    ControllerRef, Effect, ObjectScope, QuantityExpr, QuantityModification, QuantityRef,
    ReplacementDefinition, TargetFilter,
};
use engine::types::card_type::CoreType;
use engine::types::counter::{CounterMatch, CounterType};
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::proposed_event::{TokenCharacteristics, TokenSpec};
use engine::types::replacements::ReplacementEvent;
use engine::types::PlayerId;

const MAUHUR: &str = "Menace\n\
If one or more +1/+1 counters would be put on an Army, Goblin, or Orc you control, \
that many plus one +1/+1 counters are put on it instead.";

const SWARMING_OF_MORIA: &str = "Create a Treasure token.\nAmass Orcs 2.";

const FORAY_OF_ORCS: &str = "Amass Orcs 2. When you do, Foray of Orcs deals X damage \
to target creature an opponent controls, where X is the amassed Army's power.";

const GRISHNAKH: &str = "When Grishnákh, Brash Instigator enters the battlefield, \
amass Orcs 2. When you do, until end of turn, gain control of target nonlegendary \
creature an opponent controls with power less than or equal to the amassed Army's \
power. Untap that creature. It gains haste until end of turn.";

const SURROUNDED_BY_ORCS: &str =
    "Amass Orcs 3, then target player mills X cards, where X is the amassed Army's power.";

fn plus_one_counters(runner: &GameRunner, object_id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&object_id)
        .and_then(|obj| obj.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

fn controlled_army(runner: &GameRunner) -> Option<ObjectId> {
    runner.state().battlefield.iter().copied().find(|id| {
        runner.state().objects.get(id).is_some_and(|obj| {
            obj.controller == P0 && obj.card_types.subtypes.iter().any(|s| s == "Army")
        })
    })
}

fn damage_marked(runner: &GameRunner, object_id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&object_id)
        .map(|obj| obj.damage_marked)
        .unwrap_or(0)
}

fn library_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.library.len())
        .unwrap_or(0)
}

fn graveyard_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.graveyard.len())
        .unwrap_or(0)
}

fn add_token_count_replacement(
    scenario: &mut GameScenario,
    name: &str,
    modification: QuantityModification,
) -> ObjectId {
    scenario
        .add_creature(P0, name, 0, 4)
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::CreateToken)
                .quantity_modification(modification),
        )
        .id()
}

fn add_counter_count_replacement(
    scenario: &mut GameScenario,
    name: &str,
    modification: QuantityModification,
) -> ObjectId {
    scenario
        .add_creature(P0, name, 0, 4)
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::AddCounter)
                .quantity_modification(modification)
                .counter_match(CounterMatch::OfType(CounterType::Plus1Plus1)),
        )
        .id()
}

fn squirrel_spec() -> TokenSpec {
    TokenSpec {
        characteristics: TokenCharacteristics {
            display_name: "Squirrel".to_string(),
            power: Some(1),
            toughness: Some(1),
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Squirrel".to_string()],
            supertypes: Vec::new(),
            colors: vec![ManaColor::Green],
            keywords: Vec::new(),
        },
        script_name: "Squirrel".to_string(),
        static_abilities: Vec::new(),
        enter_with_counters: Vec::new(),
        tapped: false,
        enters_attacking: false,
        sacrifice_at: None,
        source_id: ObjectId(0),
        controller: P0,
        attach_to: None,
    }
}

fn army_amassed_event_for(events: &[GameEvent], army: ObjectId) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            GameEvent::ArmyAmassed { object_id, .. } if *object_id == army
        )
    })
}

fn created_orc_army_event_count(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::TokenCreated { name, .. } if name == "Orc Army"
            )
        })
        .count()
}

#[test]
fn swarming_of_moria_puts_mauhurs_extra_counter_on_the_army() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mauhur = scenario
        .add_creature_from_oracle(P0, "Mauhur, Uruk-hai Captain", 2, 2, MAUHUR)
        .with_subtypes(vec!["Orc", "Soldier"])
        .id();
    let swarming = scenario
        .add_spell_to_hand_from_oracle(P0, "Swarming of Moria", false, SWARMING_OF_MORIA)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    runner.cast(swarming).resolve();

    let army = controlled_army(&runner).expect("Swarming of Moria should create an Army");
    assert_eq!(
        plus_one_counters(&runner, army),
        3,
        "amass Orcs 2 should put 3 counters on the Army while Mauhur is controlled"
    );
    assert_eq!(
        plus_one_counters(&runner, mauhur),
        0,
        "Mauhur modifies the Army's counter event; it does not receive those counters"
    );
}

#[test]
fn mauhur_does_not_add_counters_to_unlisted_creature_types() {
    let mut scenario = GameScenario::new();
    let _mauhur = scenario
        .add_creature_from_oracle(P0, "Mauhur, Uruk-hai Captain", 2, 2, MAUHUR)
        .with_subtypes(vec!["Orc", "Soldier"])
        .id();
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let mut runner = scenario.build();
    let mut events = Vec::new();

    assert!(add_counter_with_replacement(
        runner.state_mut(),
        P0,
        bear,
        CounterType::Plus1Plus1,
        1,
        &mut events,
    ));

    assert_eq!(
        plus_one_counters(&runner, bear),
        1,
        "Mauhur only modifies counters put on Armies, Goblins, and Orcs you control"
    );
}

#[test]
fn foray_of_orcs_damages_target_from_the_army_amassed_after_swarming_of_moria() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let swarming = scenario
        .add_spell_to_hand_from_oracle(P0, "Swarming of Moria", false, SWARMING_OF_MORIA)
        .with_mana_cost(ManaCost::zero())
        .id();
    let foray = scenario
        .add_spell_to_hand_from_oracle(P0, "Foray of Orcs", false, FORAY_OF_ORCS)
        .with_mana_cost(ManaCost::zero())
        .id();
    let target = scenario.add_creature(P1, "Target Dummy", 0, 10).id();
    let mut runner = scenario.build();

    runner.cast(swarming).resolve();
    let army = controlled_army(&runner).expect("Swarming of Moria should create an Army");

    let outcome = runner.cast(foray).target_object(target).resolve();

    assert_eq!(
        plus_one_counters(&runner, army),
        4,
        "Foray should continue amassing onto the Army created earlier this turn"
    );
    assert_eq!(
        damage_marked(&runner, target),
        4,
        "Foray damage must read the Army selected by its own amass instruction"
    );
    assert!(
        army_amassed_event_for(outcome.events(), army),
        "Foray should emit an ArmyAmassed event for the selected Army"
    );
}

#[test]
fn grishnakh_target_eligibility_uses_the_amassed_armys_current_power() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let grishnakh = scenario
        .add_creature_to_hand_from_oracle(P0, "Grishnákh, Brash Instigator", 1, 1, GRISHNAKH)
        .with_mana_cost(ManaCost::zero())
        .id();
    let legal_target = scenario.add_creature(P1, "Legal Target", 2, 2).id();
    let too_large_target = scenario.add_creature(P1, "Too Large Target", 3, 3).id();
    let mut runner = scenario.build();

    let outcome = runner.cast(grishnakh).target_object(legal_target).resolve();
    let army = controlled_army(&runner).expect("Grishnákh should create an Army");

    assert_eq!(
        plus_one_counters(&runner, army),
        2,
        "Grishnákh's amass instruction should create a 2-power Army"
    );
    assert_eq!(
        runner.state().objects[&legal_target].controller,
        P0,
        "the 2-power creature should be legal because the amassed Army is 2/2 and should be gained"
    );
    assert_eq!(
        runner.state().objects[&too_large_target].controller,
        P1,
        "the 3-power sibling must not be selected or gained while the amassed Army is 2/2"
    );
    assert!(
        army_amassed_event_for(outcome.events(), army),
        "Grishnákh should bind the Army created by its own amass instruction"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Grishnákh should resolve its reflexive trigger and return priority"
    );
}

#[test]
fn surrounded_by_orcs_mills_target_player_using_the_selected_armys_current_power() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(
        P1,
        &[
            "Library One",
            "Library Two",
            "Library Three",
            "Library Four",
            "Library Five",
            "Library Six",
        ],
    );
    let swarming = scenario
        .add_spell_to_hand_from_oracle(P0, "Swarming of Moria", false, SWARMING_OF_MORIA)
        .with_mana_cost(ManaCost::zero())
        .id();
    let surrounded = scenario
        .add_spell_to_hand_from_oracle(P0, "Surrounded by Orcs", false, SURROUNDED_BY_ORCS)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    runner.cast(swarming).resolve();
    let army = controlled_army(&runner).expect("Swarming of Moria should create an Army");
    assert_eq!(plus_one_counters(&runner, army), 2);
    let p0_library_before = library_count(&runner, P0);
    let p1_library_before = library_count(&runner, P1);

    let outcome = runner.cast(surrounded).target_player(P1).resolve();

    assert_eq!(
        plus_one_counters(&runner, army),
        5,
        "Surrounded by Orcs should amass onto the existing Army before milling"
    );
    assert!(
        army_amassed_event_for(outcome.events(), army),
        "Surrounded by Orcs should bind the same selected Army used by its mill clause"
    );
    assert_eq!(
        p1_library_before - library_count(&runner, P1),
        5,
        "target player should mill cards equal to the selected Army's current power"
    );
    assert_eq!(
        graveyard_count(&runner, P1),
        5,
        "milled cards should move from the target player's library to graveyard"
    );
    assert_eq!(
        library_count(&runner, P0),
        p0_library_before,
        "non-target controller library should not be milled"
    );
}

#[test]
fn amass_token_replacement_pause_resumes_and_binds_amassed_army() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_token_count_replacement(&mut scenario, "Token Doubler", QuantityModification::DOUBLE);
    add_token_count_replacement(
        &mut scenario,
        "Token Incrementer",
        QuantityModification::Plus { value: 1 },
    );
    let foray = scenario
        .add_spell_to_hand_from_oracle(P0, "Foray of Orcs", false, FORAY_OF_ORCS)
        .with_mana_cost(ManaCost::zero())
        .id();
    let target = scenario.add_creature(P1, "Target Dummy", 0, 10).id();
    let mut runner = scenario.build();

    let outcome = runner
        .cast(foray)
        .replacement_choice(0)
        .target_object(target)
        .resolve();
    let army = controlled_army(&runner).expect("Foray should create at least one Army");

    assert_eq!(
        plus_one_counters(&runner, army),
        2,
        "the amass-selected Army should still receive Foray's two counters after token replacement"
    );
    assert_eq!(
        damage_marked(&runner, target),
        2,
        "Foray damage must use the selected Army after the token replacement choice resumes"
    );
    assert!(
        created_orc_army_event_count(outcome.events()) > 1,
        "the token replacement setup should prove multiple Army tokens were created before one was selected for amass"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the replacement pause should drain through Foray's reflexive target and return priority"
    );
    assert!(
        army_amassed_event_for(outcome.events(), army),
        "Foray should emit ArmyAmassed after token replacement resumes"
    );
}

#[test]
fn amass_counter_replacement_pause_finalizes_before_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_counter_count_replacement(
        &mut scenario,
        "Counter Doubler",
        QuantityModification::DOUBLE,
    );
    add_counter_count_replacement(
        &mut scenario,
        "Counter Incrementer",
        QuantityModification::Plus { value: 1 },
    );
    let swarming = scenario
        .add_spell_to_hand_from_oracle(P0, "Swarming of Moria", false, SWARMING_OF_MORIA)
        .with_mana_cost(ManaCost::zero())
        .id();
    let foray = scenario
        .add_spell_to_hand_from_oracle(P0, "Foray of Orcs", false, FORAY_OF_ORCS)
        .with_mana_cost(ManaCost::zero())
        .id();
    let target = scenario.add_creature(P1, "Target Dummy", 0, 20).id();
    let mut runner = scenario.build();

    runner.cast(swarming).replacement_choice(0).resolve();
    let army = controlled_army(&runner).expect("Swarming should create an Army");
    let counters_before_foray = plus_one_counters(&runner, army);

    let outcome = runner
        .cast(foray)
        .replacement_choice(0)
        .target_object(target)
        .resolve();
    let final_counters = plus_one_counters(&runner, army);

    assert!(
        final_counters > counters_before_foray,
        "Foray should finish replacement-processed counter placement before resolving damage"
    );
    assert_eq!(
        damage_marked(&runner, target),
        final_counters,
        "Foray damage must see the finalized post-replacement Army power"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the nested counter replacement pause should fully resolve back to priority"
    );
    assert!(
        army_amassed_event_for(outcome.events(), army),
        "ArmyAmassed should be emitted only after counter replacement finalization"
    );
}

#[test]
fn chatterfang_style_token_replacement_does_not_become_amassed_army() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Squirrel Helper", 3, 3)
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::CreateToken)
                .token_owner_scope(ControllerRef::You)
                .additional_token_spec(squirrel_spec()),
        );
    let foray = scenario
        .add_spell_to_hand_from_oracle(P0, "Foray of Orcs", false, FORAY_OF_ORCS)
        .with_mana_cost(ManaCost::zero())
        .id();
    let target = scenario.add_creature(P1, "Target Dummy", 0, 10).id();
    let mut runner = scenario.build();

    runner.cast(foray).target_object(target).resolve();
    let army = controlled_army(&runner).expect("Foray should create an Army");
    let squirrel = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| {
            runner.state().objects.get(id).is_some_and(|obj| {
                obj.controller == P0 && obj.card_types.subtypes.iter().any(|s| s == "Squirrel")
            })
        })
        .expect("replacement should create a Squirrel token");

    assert_eq!(plus_one_counters(&runner, army), 2);
    assert_eq!(
        plus_one_counters(&runner, squirrel),
        0,
        "the additional token is created but is not the amass-selected Army"
    );
    assert_eq!(
        damage_marked(&runner, target),
        2,
        "Foray damage should still be based on the Army, not the extra token"
    );
}

#[test]
fn generic_demonstrative_does_not_read_previous_amassed_army() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let swarming = scenario
        .add_spell_to_hand_from_oracle(P0, "Swarming of Moria", false, SWARMING_OF_MORIA)
        .with_mana_cost(ManaCost::zero())
        .id();
    let demonstrative_damage = scenario
        .add_spell_to_hand(P0, "Generic Demonstrative Damage", false)
        .with_mana_cost(ManaCost::zero())
        .with_ability(Effect::DealDamage {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Demonstrative,
                },
            },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        })
        .id();
    let target = scenario.add_creature(P1, "Target Dummy", 0, 10).id();
    let mut runner = scenario.build();

    runner.cast(swarming).resolve();
    let army = controlled_army(&runner).expect("Swarming should create an Army");
    assert_eq!(plus_one_counters(&runner, army), 2);

    runner
        .cast(demonstrative_damage)
        .target_object(target)
        .resolve();

    assert_eq!(
        damage_marked(&runner, target),
        0,
        "generic demonstrative scope must not read an unrelated previous amassed Army"
    );
}
