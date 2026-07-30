//! Issue #5336 — Kodama of the East Tree deck interaction (Nature's Lore ramp chain).
//!
//! https://github.com/phase-rs/phase/issues/5336
//!
//! Kodama's optional put-from-hand sub-ability must honor the entering permanent's
//! mana value (`with equal or lesser mana value`) and must not re-trigger on
//! permanents it placed itself (CR 603.4 anti-recursion guard).
//!
//! The existing `kodama_anti_recursion_intervening_if` tests omit the MV qualifier;
//! this file exercises the full Oracle line in runtime scenarios matching the
//! reported deck (Kodama + Nature's Lore ramp).

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const KODAMA_ORACLE: &str = "Reach\nWhenever another permanent you control enters, if it wasn't \
     put onto the battlefield with this ability, you may put a permanent card with equal or lesser \
     mana value from your hand onto the battlefield.";

const NATURES_LORE_ORACLE: &str =
    "Search your library for a Forest card, put that card onto the battlefield, then shuffle.";

fn floating_generic(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn cast_from_hand(runner: &mut engine::game::scenario::GameRunner, id: ObjectId) {
    let card_id = runner.state().objects[&id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast from hand");
}

fn advance_to_optional_choice(runner: &mut engine::game::scenario::GameRunner) -> bool {
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => return true,
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return false;
                }
                if runner.state().stack.is_empty()
                    && runner.state().deferred_triggers.is_empty()
                    && runner.state().pending_trigger.is_none()
                    && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                {
                    return false;
                }
            }
            _ => return false,
        }
    }
    false
}

fn resolve_optional_and_stack(runner: &mut engine::game::scenario::GameRunner, accept: bool) {
    runner
        .act(GameAction::DecideOptionalEffect { accept })
        .expect("optional Kodama decision");
    runner.advance_until_stack_empty();
}

/// Seed a basic Forest with the correct land type + subtype on the library top.
fn seed_forest_on_library_top(runner: &mut engine::game::scenario::GameRunner) -> ObjectId {
    use engine::types::card_type::CoreType;
    let card_id = engine::types::identifiers::CardId(runner.state().next_object_id);
    let id = engine::game::zones::create_object(
        runner.state_mut(),
        card_id,
        P0,
        "Forest".to_string(),
        Zone::Library,
    );
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Land);
    obj.base_card_types = obj.card_types.clone();
    obj.card_types.subtypes.push("Forest".to_string());
    runner.state_mut().players[P0.0 as usize]
        .library
        .insert(0, id);
    id
}

/// CR 603.6a: a Forest entering from the library must trigger Kodama when
/// `process_triggers` runs on the emitted `ZoneChanged` event.
#[test]
fn kodama_triggers_on_forest_etb_from_library() {
    use engine::game::triggers::process_triggers;
    use engine::game::zones::move_to_zone;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Kodama of the East Tree", 6, 4, KODAMA_ORACLE)
        .id();

    let mut runner = scenario.build();
    let forest = seed_forest_on_library_top(&mut runner);

    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), forest, Zone::Battlefield, &mut events);
    process_triggers(runner.state_mut(), &events);

    assert!(
        advance_to_optional_choice(&mut runner),
        "Kodama must trigger on Forest ETB; waiting={:?}, stack={}, deferred={}",
        runner.state().waiting_for,
        runner.state().stack.len(),
        runner.state().deferred_triggers.len(),
    );
}

/// CR 603.4: ability-placement provenance from *another* ability must not
/// suppress Kodama — only placements by Kodama itself are excluded.
#[test]
fn kodama_triggers_when_entering_permanent_has_foreign_ability_provenance() {
    use engine::game::triggers::process_triggers;
    use engine::game::zones::move_to_zone;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Kodama of the East Tree", 6, 4, KODAMA_ORACLE)
        .id();
    let foreign_placer = scenario.add_creature(P0, "Nature's Lore", 0, 0).id();

    let mut runner = scenario.build();
    let forest = seed_forest_on_library_top(&mut runner);

    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), forest, Zone::Battlefield, &mut events);
    runner
        .state_mut()
        .objects
        .get_mut(&forest)
        .unwrap()
        .entered_via_ability_source = Some(foreign_placer);
    process_triggers(runner.state_mut(), &events);

    assert!(
        advance_to_optional_choice(&mut runner),
        "Kodama must trigger even when the entering permanent was placed by another ability"
    );
}

/// CR 603.2: ETB observers must still fire while a sorcery's `resolving_stack_entry`
/// is stashed mid-resolution (Nature's Lore search-put path).
#[test]
fn kodama_triggers_on_forest_etb_with_resolving_stack_entry_stashed() {
    use engine::game::triggers::process_triggers;
    use engine::game::zones::move_to_zone;
    use engine::types::game_state::{CastingVariant, StackEntry, StackEntryKind};

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kodama = scenario
        .add_creature_from_oracle(P0, "Kodama of the East Tree", 6, 4, KODAMA_ORACLE)
        .id();

    let mut runner = scenario.build();
    let forest = seed_forest_on_library_top(&mut runner);

    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), forest, Zone::Battlefield, &mut events);
    runner.state_mut().resolving_stack_entry = Some(StackEntry {
        id: ObjectId(9999),
        source_id: ObjectId(9998),
        controller: P0,
        kind: StackEntryKind::Spell {
            card_id: engine::types::identifiers::CardId(9998),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    runner
        .state_mut()
        .objects
        .get_mut(&forest)
        .unwrap()
        .entered_via_ability_source = Some(ObjectId(9998));

    process_triggers(runner.state_mut(), &events);

    assert!(
        advance_to_optional_choice(&mut runner),
        "Kodama must trigger with stashed resolving_stack_entry; kodama={kodama:?}, waiting={:?}, stack={}",
        runner.state().waiting_for,
        runner.state().stack.len(),
    );
}

/// CR 202.3 + CR 603.4: when a 2-MV creature enters normally, Kodama's optional
/// put-from-hand must not cheat in a higher-MV permanent.
#[test]
fn kodama_hand_choice_respects_entering_permanent_mana_value() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Kodama of the East Tree", 6, 4, KODAMA_ORACLE)
        .id();

    let trigger_creature = scenario
        .add_creature_to_hand(P0, "Two Drop", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let eligible = scenario
        .add_creature_to_hand(P0, "Also Two", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let too_big = scenario
        .add_creature_to_hand(P0, "Five Drop", 5, 5)
        .with_mana_cost(ManaCost::generic(5))
        .id();

    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize].mana_pool.mana = floating_generic(2);
    cast_from_hand(&mut runner, trigger_creature);

    assert!(
        advance_to_optional_choice(&mut runner),
        "Kodama must offer its optional put-from-hand ability"
    );
    resolve_optional_and_stack(&mut runner, true);

    assert_eq!(
        runner.state().objects[&eligible].zone,
        Zone::Battlefield,
        "the eligible 2-MV permanent must be put onto the battlefield"
    );
    assert_eq!(
        runner.state().objects[&too_big].zone,
        Zone::Hand,
        "the 5-MV permanent must stay in hand when a 2-MV creature triggered Kodama"
    );
}

/// CR 202.3 + CR 603.6a: Nature's Lore putting a Forest (MV 0) triggers Kodama,
/// and only 0-MV permanents may be put from hand.
#[test]
fn kodama_natures_lore_forest_limits_hand_to_zero_mana_value() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Green);

    scenario
        .add_creature_from_oracle(P0, "Kodama of the East Tree", 6, 4, KODAMA_ORACLE)
        .id();

    let zero_drop = scenario
        .add_creature_to_hand(P0, "Free Creature", 1, 1)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let three_drop = scenario
        .add_creature_to_hand(P0, "Three Drop", 3, 3)
        .with_mana_cost(ManaCost::generic(3))
        .id();

    let natures_lore = scenario
        .add_spell_to_hand_from_oracle(P0, "Nature's Lore", false, NATURES_LORE_ORACLE)
        .id();

    let mut runner = scenario.build();
    let forest = seed_forest_on_library_top(&mut runner);
    runner.cast(natures_lore).search_first_legal().resolve();

    assert_eq!(
        runner.state().objects[&forest].zone,
        Zone::Battlefield,
        "Nature's Lore must put the searched Forest onto the battlefield"
    );

    let kodama_id = runner
        .state()
        .battlefield
        .iter()
        .find(|id| runner.state().objects[id].name == "Kodama of the East Tree")
        .copied()
        .expect("Kodama on battlefield");
    assert_eq!(
        runner.state().objects[&kodama_id].trigger_definitions.len(),
        1,
        "Kodama must have exactly one parsed ETB trigger"
    );
    let cond = runner.state().objects[&kodama_id].trigger_definitions[0]
        .definition
        .condition
        .as_ref()
        .expect("Kodama trigger must carry intervening-if");
    assert!(
        matches!(
            cond,
            engine::types::ability::TriggerCondition::Not { condition }
                if matches!(
                    condition.as_ref(),
                    engine::types::ability::TriggerCondition::PlacedByAbilitySource
                )
        ),
        "expected Not(PlacedByAbilitySource), got {cond:?}"
    );
    assert!(
        runner.state().objects[&forest]
            .entered_via_ability_source
            .is_some(),
        "Forest from Nature's Lore must record ability-placement provenance"
    );

    assert!(
        advance_to_optional_choice(&mut runner),
        "Forest ETB from Nature's Lore must trigger Kodama's optional ability; \
         waiting_for={:?}, deferred={}, stack={}, pending_cont={:?}",
        runner.state().waiting_for,
        runner.state().deferred_triggers.len(),
        runner.state().stack.len(),
        runner.state().active_ability_continuation().is_some(),
    );
    resolve_optional_and_stack(&mut runner, true);

    assert_eq!(
        runner.state().objects[&zero_drop].zone,
        Zone::Battlefield,
        "0-MV hand permanent must be eligible when a Forest (MV 0) triggered Kodama"
    );
    assert_eq!(
        runner.state().objects[&three_drop].zone,
        Zone::Hand,
        "3-MV hand permanent must stay in hand when a Forest (MV 0) triggered Kodama"
    );
}

/// CR 609.3: declining Kodama's optional ability with no eligible hand cards must
/// resolve cleanly (no stall on an empty EffectZoneChoice).
#[test]
fn kodama_decline_with_no_eligible_hand_cards_resolves_cleanly() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Kodama of the East Tree", 6, 4, KODAMA_ORACLE)
        .id();

    let trigger_creature = scenario
        .add_creature_to_hand(P0, "Two Drop", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    scenario
        .add_creature_to_hand(P0, "Five Drop", 5, 5)
        .with_mana_cost(ManaCost::generic(5))
        .id();

    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize].mana_pool.mana = floating_generic(2);
    cast_from_hand(&mut runner, trigger_creature);

    assert!(advance_to_optional_choice(&mut runner));
    resolve_optional_and_stack(&mut runner, false);

    assert!(
        runner.state().stack.is_empty(),
        "stack must settle after declining Kodama with no eligible picks"
    );
    assert_eq!(
        runner.state().objects[&trigger_creature].zone,
        Zone::Battlefield,
        "the triggering creature must remain on the battlefield"
    );
}
