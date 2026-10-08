//! Issue #5972: Saheeli / Twinflame / Mysterio class — "exile those tokens at
//! the next end step" must bind the full tracked token set with battlefield
//! origin, not force exile-origin scanning that misses battlefield tokens.

use crate::support::shared_card_db;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, Effect, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::identifiers::TrackedSetId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SAHEELI_MINUS_SEVEN_FRAGMENT: &str = "for each artifact you control, create a token that's a copy of it. those tokens gain haste. exile those tokens at the beginning of the next end step";

fn twinflame_mana_pool(spell: engine::types::identifiers::ObjectId) -> Vec<ManaUnit> {
    // {1}{R} + {2}{R} strive for a second target = {3}{R}{R}
    vec![
        ManaUnit::new(ManaType::Red, spell, false, vec![]),
        ManaUnit::new(ManaType::Red, spell, false, vec![]),
        ManaUnit::new(ManaType::Colorless, spell, false, vec![]),
        ManaUnit::new(ManaType::Colorless, spell, false, vec![]),
        ManaUnit::new(ManaType::Colorless, spell, false, vec![]),
    ]
}

fn find_delayed_tracked_exile(
    def: &engine::types::ability::AbilityDefinition,
) -> Option<(bool, TargetFilter, Option<Zone>)> {
    if let Effect::CreateDelayedTrigger {
        uses_tracked_set,
        effect: inner,
        ..
    } = &*def.effect
    {
        if let Effect::ChangeZone {
            target,
            destination: Zone::Exile,
            origin,
            ..
        } = &*inner.effect
        {
            return Some((*uses_tracked_set, target.clone(), *origin));
        }
    }
    def.sub_ability
        .as_deref()
        .and_then(find_delayed_tracked_exile)
}

/// Parser chain shape for Saheeli's -7 tail.
#[test]
fn saheeli_minus_seven_those_tokens_cleanup_uses_tracked_set() {
    let ability = parse_effect_chain(SAHEELI_MINUS_SEVEN_FRAGMENT, AbilityKind::Activated);
    let (uses_tracked_set, target, origin) =
        find_delayed_tracked_exile(&ability).expect("Saheeli -7 tail must include delayed exile");
    assert!(uses_tracked_set);
    assert_eq!(
        target,
        TargetFilter::TrackedSet {
            id: TrackedSetId(0)
        }
    );
    assert_eq!(origin, Some(Zone::Battlefield));
}

/// Production cast pipeline: Twinflame creates two copy tokens and exiles both
/// at the next end step when they remain on the battlefield.
#[test]
fn twinflame_tracked_set_cleanup_exiles_all_copy_tokens() {
    let Some(db) = shared_card_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature_a = scenario.add_creature(P0, "Goblin A", 2, 2).id();
    let creature_b = scenario.add_creature(P0, "Goblin B", 2, 2).id();
    let twinflame = scenario.add_real_card(P0, "Twinflame", Zone::Hand, db);
    scenario.with_mana_pool(P0, twinflame_mana_pool(twinflame));

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let pre_battlefield: std::collections::HashSet<_> =
        runner.state().battlefield.iter().copied().collect();
    runner
        .cast(twinflame)
        .target_objects(&[creature_a, creature_b])
        .resolve();

    let tokens: Vec<_> = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| !pre_battlefield.contains(id))
        .collect();
    assert_eq!(
        tokens.len(),
        2,
        "Twinflame must create two copy tokens on the battlefield"
    );

    runner.advance_to_combat();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        })
        .expect("declare no attackers to cross combat");
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    let tokens_remaining_on_battlefield = tokens
        .iter()
        .filter(|id| runner.state().battlefield.contains(id))
        .count();
    assert_eq!(
        tokens_remaining_on_battlefield, 0,
        "both tracked-set copy tokens must leave the battlefield at end-step cleanup; \
         remaining on battlefield: {tokens_remaining_on_battlefield}"
    );
    assert_eq!(
        runner.state().objects[&creature_a].zone,
        Zone::Battlefield,
        "Twinflame cleanup must not exile the copied source creatures"
    );
    assert_eq!(
        runner.state().objects[&creature_b].zone,
        Zone::Battlefield,
        "Twinflame cleanup must not exile the copied source creatures"
    );
}
