//! Regression for issue #2376: Pyromancer's Ascension copy and quest-counter
//! triggers must gate on quest-counter count and graveyard name match.
//!
//! https://github.com/phase-rs/phase/issues/2376

use engine::game::scenario::{GameScenario, P0};
use engine::game::triggers::{process_triggers, trigger_matcher};
use engine::game::zones::create_object;
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    Effect, QuantityExpr, ResolvedAbility, TargetFilter, TriggerDefinition,
};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastingVariant, GameState, StackEntry, StackEntryKind};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const COUNTER_TRIGGER: &str = "Whenever you cast an instant or sorcery spell that has the same name as a card in your graveyard, you may put a quest counter on this enchantment.";
const COPY_TRIGGER: &str = "Whenever you cast an instant or sorcery spell while this enchantment has two or more quest counters on it, you may copy that spell. You may choose new targets for the copy.";

fn main_phase_state() -> GameState {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.build().state().clone()
}

fn install_ascension(state: &mut GameState, quest_counters: u32) -> ObjectId {
    let ascension = create_object(
        state,
        CardId(2376),
        P0,
        "Pyromancer's Ascension".to_string(),
        Zone::Battlefield,
    );
    let parsed = parse_oracle_text(
        &format!("{COUNTER_TRIGGER}\n{COPY_TRIGGER}"),
        "Pyromancer's Ascension",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    let triggers: Vec<TriggerDefinition> = parsed.triggers;
    assert_eq!(
        triggers.len(),
        2,
        "Ascension oracle must parse to two triggers"
    );
    assert_eq!(triggers[0].mode, TriggerMode::SpellCast);
    assert_eq!(triggers[1].mode, TriggerMode::SpellCast);
    assert!(
        triggers[0].condition.is_none(),
        "graveyard-name gate belongs on valid_card, not condition"
    );
    assert!(
        triggers[1].condition.is_some(),
        "copy trigger must carry the quest-counter intervening-if"
    );

    let obj = state.objects.get_mut(&ascension).unwrap();
    obj.card_types.core_types.push(CoreType::Enchantment);
    if quest_counters > 0 {
        obj.counters
            .insert(CounterType::Generic("quest".to_string()), quest_counters);
    }
    obj.trigger_definitions = triggers.into();
    ascension
}

fn push_instant_spell(state: &mut GameState, name: &str) -> ObjectId {
    let spell_id = create_object(
        state,
        CardId(state.next_object_id + 1000),
        P0,
        name.to_string(),
        Zone::Stack,
    );
    {
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.card_types.core_types.push(CoreType::Instant);
    }

    let mut ability = ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        vec![],
        spell_id,
        P0,
    );
    ability.context.cast_from_zone = Some(Zone::Hand);

    state.stack.push_back(StackEntry {
        id: spell_id,
        source_id: spell_id,
        controller: P0,
        kind: StackEntryKind::Spell {
            card_id: CardId(spell_id.0),
            ability: Some(ability),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    spell_id
}

fn spell_cast_event(spell_id: ObjectId) -> GameEvent {
    GameEvent::SpellCast {
        card_id: CardId(spell_id.0),
        controller: P0,
        object_id: spell_id,
    }
}

fn spell_cast_matcher_accepts(
    state: &GameState,
    ascension: ObjectId,
    trigger_idx: usize,
    spell_id: ObjectId,
) -> bool {
    let trigger = &state.objects.get(&ascension).unwrap().trigger_definitions[trigger_idx];
    let matcher = trigger_matcher(trigger.definition.mode.clone()).expect("SpellCast matcher");
    matcher(
        &spell_cast_event(spell_id),
        trigger.definition(),
        ascension,
        state,
    )
}

fn ascension_triggers_on_stack(state: &GameState, ascension: ObjectId) -> usize {
    state
        .stack
        .iter()
        .filter(|entry| entry.source_id == ascension)
        .count()
}

#[test]
fn pyromancers_ascension_graveyard_name_match_filters_counter_trigger() {
    let mut state = main_phase_state();
    let ascension = install_ascension(&mut state, 0);

    let _graveyard_shock = create_object(
        &mut state,
        CardId(2377),
        P0,
        "Shock".to_string(),
        Zone::Graveyard,
    );

    let shock_id = push_instant_spell(&mut state, "Shock");
    assert!(
        spell_cast_matcher_accepts(&state, ascension, 0, shock_id),
        "matching graveyard name must pass the counter trigger's valid_card filter"
    );

    let bolt_id = push_instant_spell(&mut state, "Lightning Bolt");
    assert!(
        !spell_cast_matcher_accepts(&state, ascension, 0, bolt_id),
        "non-matching graveyard name must fail the counter trigger's valid_card filter"
    );
}

#[test]
fn pyromancers_ascension_copy_trigger_requires_two_quest_counters() {
    let mut state = main_phase_state();
    let ascension = install_ascension(&mut state, 0);
    let spell_id = push_instant_spell(&mut state, "Shock");

    process_triggers(&mut state, &[spell_cast_event(spell_id)]);
    assert_eq!(
        ascension_triggers_on_stack(&state, ascension),
        0,
        "copy trigger must not enqueue with zero quest counters"
    );

    state
        .objects
        .get_mut(&ascension)
        .unwrap()
        .counters
        .insert(CounterType::Generic("quest".to_string()), 2);

    process_triggers(&mut state, &[spell_cast_event(spell_id)]);
    assert_eq!(
        ascension_triggers_on_stack(&state, ascension),
        1,
        "two quest counters must enqueue the copy trigger"
    );
}
