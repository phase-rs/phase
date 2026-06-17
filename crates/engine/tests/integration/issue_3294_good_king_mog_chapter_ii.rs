//! Regression for GitHub issue #3294 — Summon: Good King Mog XII, chapter II/III.
//!
//! Oracle (chapters II–III): "Whenever you cast a noncreature spell this turn,
//! create a token that's a copy of a non-Saga token you control."
//!
//! Reported bug: the delayed trigger never fires when casting a noncreature spell
//! after the chapter ability resolves.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityKind, ControllerRef, DelayedTriggerCondition, Effect, FilterProp, TargetFilter,
    TypeFilter,
};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;

const CHAPTER_II: &str = "Whenever you cast a noncreature spell this turn, create a token that's a copy of a non-Saga token you control.";

const MOG_ORACLE: &str = "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after IV.)\n\
I — Create two 1/2 white Moogle creature tokens with lifelink.\n\
II, III — Whenever you cast a noncreature spell this turn, create a token that's a copy of a non-Saga token you control.\n\
IV — Put two +1/+1 counters on each other Moogle you control.\n\
Flying, lifelink";

fn token_count(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.is_token)
        .count()
}

#[test]
fn chapter_ii_copy_token_parses_non_saga_source_filter() {
    let def = parse_effect_chain(CHAPTER_II, AbilityKind::Spell);
    let Effect::CreateDelayedTrigger { effect, .. } = &*def.effect else {
        panic!("expected CreateDelayedTrigger, got {:?}", def.effect);
    };
    let Effect::CopyTokenOf {
        target,
        source_filter,
        ..
    } = &*effect.effect
    else {
        panic!("expected CopyTokenOf inner effect, got {:?}", effect.effect);
    };
    assert!(source_filter.is_none());
    let TargetFilter::Typed(tf) = target else {
        panic!("expected Typed copy source filter, got {target:?}");
    };
    assert!(
        tf.type_filters
            .contains(&TypeFilter::Non(Box::new(TypeFilter::Subtype(
                "Saga".to_string()
            )))),
        "expected Non(Saga), got {:?}",
        tf.type_filters
    );
    assert!(tf.properties.contains(&FilterProp::Token));
    assert_eq!(tf.controller, Some(ControllerRef::You));
}

#[test]
fn chapter_ii_parses_to_delayed_spell_cast_trigger() {
    let def = parse_effect_chain(CHAPTER_II, AbilityKind::Spell);
    let Effect::CreateDelayedTrigger { condition, .. } = &*def.effect else {
        panic!("expected CreateDelayedTrigger, got {:?}", def.effect);
    };
    let DelayedTriggerCondition::WheneverEvent { trigger } = condition else {
        panic!("expected WheneverEvent, got {condition:?}");
    };
    assert_eq!(
        trigger.mode,
        TriggerMode::SpellCast,
        "chapter II delayed trigger must listen for SpellCast"
    );
}

#[test]
fn chapter_ii_delayed_trigger_fires_when_spell_commits_to_stack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mog = scenario
        .add_creature_from_oracle(P0, "Summon: Good King Mog XII", 4, 4, MOG_ORACLE)
        .with_subtypes(vec!["Moogle", "Saga"])
        .id();

    let moogle_id = scenario.add_creature(P0, "Moogle Token", 1, 2).id();
    let draw = scenario
        .add_spell_to_hand_from_oracle(P0, "Opt", true, "Scry 1.\nDraw a card.")
        .id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&moogle_id)
        .unwrap()
        .is_token = true;

    let chapter_ii = parse_effect_chain(CHAPTER_II, AbilityKind::Spell);
    let resolved = build_resolved_from_def(&chapter_ii, mog, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("chapter II delayed trigger registers");

    let stack_before = runner.state().stack.len();
    runner.cast(draw).commit();
    let stack_after_commit = runner.state().stack.len();

    assert!(
        stack_after_commit > stack_before,
        "Opt must commit to the stack (stack {stack_before}→{stack_after_commit})"
    );
    assert!(
        runner.state().stack.len() >= 2,
        "chapter II delayed trigger must be on stack when noncreature spell is cast \
         (stack={}, delayed_triggers={})",
        runner.state().stack.len(),
        runner.state().delayed_triggers.len()
    );
}

#[test]
fn chapter_ii_delayed_trigger_fires_on_noncreature_spell_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mog = scenario
        .add_creature_from_oracle(P0, "Summon: Good King Mog XII", 4, 4, MOG_ORACLE)
        .with_subtypes(vec!["Moogle", "Saga"])
        .id();

    let moogle_id = scenario.add_creature(P0, "Moogle Token", 1, 2).id();

    let draw = scenario
        .add_spell_to_hand_from_oracle(P0, "Opt", true, "Scry 1.\nDraw a card.")
        .id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&moogle_id)
        .unwrap()
        .is_token = true;

    // Resolve chapter II: register the delayed trigger.
    let chapter_ii = parse_effect_chain(CHAPTER_II, AbilityKind::Spell);
    let resolved = build_resolved_from_def(&chapter_ii, mog, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("chapter II delayed trigger registers");

    assert!(
        !runner.state().delayed_triggers.is_empty(),
        "chapter II must register a delayed trigger"
    );

    let stack_before = runner.state().stack.len();
    let tokens_before = token_count(&runner);

    runner.cast(draw).target_object(moogle_id).resolve();

    let stack_after = runner.state().stack.len();
    let tokens_after = token_count(&runner);

    assert!(
        stack_after > stack_before || tokens_after > tokens_before,
        "chapter II delayed trigger must fire when casting a noncreature spell \
         (stack {stack_before}→{stack_after}, tokens {tokens_before}→{tokens_after}, \
         delayed_triggers={})",
        runner.state().delayed_triggers.len()
    );
}

#[test]
fn check_delayed_triggers_matches_noncreature_spell_cast_directly() {
    use engine::game::triggers::check_delayed_triggers;
    use engine::game::zones::create_object;
    use engine::types::card_type::CoreType;
    use engine::types::events::GameEvent;
    use engine::types::identifiers::CardId;
    use engine::types::zones::Zone;

    let mut scenario = GameScenario::new();
    let mog = scenario
        .add_creature_from_oracle(P0, "Summon: Good King Mog XII", 4, 4, MOG_ORACLE)
        .id();
    let mut runner = scenario.build();

    let chapter_ii = parse_effect_chain(CHAPTER_II, AbilityKind::Spell);
    let resolved = build_resolved_from_def(&chapter_ii, mog, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("chapter II registers");

    let state = runner.state_mut();
    let spell = create_object(state, CardId(99), P0, "Opt".to_string(), Zone::Stack);
    state
        .objects
        .get_mut(&spell)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Instant);

    let spell_cast = GameEvent::SpellCast {
        card_id: CardId(99),
        controller: P0,
        object_id: spell,
    };
    let stacked = check_delayed_triggers(state, &[spell_cast]);
    assert!(
        !stacked.is_empty() || !state.stack.is_empty(),
        "check_delayed_triggers must queue chapter II trigger on SpellCast \
         (stacked_events={}, stack={})",
        stacked.len(),
        state.stack.len()
    );
}
