//! Issue #4384 — Aang, Swift Savior must airbend a spell on the stack, not only
//! battlefield creatures.

use engine::game::casting::spell_objects_available_to_cast;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastingVariant, StackEntry, StackEntryKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const AANG_ORACLE: &str = "Flash\nFlying\nWhen Aang enters, airbend up to one other target creature or spell. (Exile it. While it's exiled, its owner may cast it for {2} rather than its mana cost.)\nWaterbend {8}: Transform Aang.";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

fn grant_priority(runner: &mut engine::game::scenario::GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

fn put_instant_on_stack(
    runner: &mut engine::game::scenario::GameRunner,
    controller: PlayerId,
) -> ObjectId {
    let spell = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(801),
        controller,
        "Shock".to_string(),
        Zone::Stack,
    );
    if let Some(obj) = runner.state_mut().objects.get_mut(&spell) {
        obj.card_types.core_types = vec![CoreType::Instant];
    }
    runner.state_mut().stack.push_back(StackEntry {
        id: spell,
        source_id: spell,
        controller,
        kind: StackEntryKind::Spell {
            card_id: CardId(801),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    spell
}

#[test]
fn airbend_exiles_opponent_spell_on_stack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P1, ManaColor::White);
    scenario.add_basic_land(P1, ManaColor::Blue);
    let aang = scenario
        .add_creature_to_hand_from_oracle(P1, "Aang, Swift Savior", 2, 3, AANG_ORACLE)
        .id();
    scenario.with_mana_pool(P1, floating_mana(3, ManaType::Blue));

    let mut runner = scenario.build();
    let stack_spell = put_instant_on_stack(&mut runner, P0);
    grant_priority(&mut runner, P1);

    runner.cast(aang).target_object(stack_spell).resolve();

    assert_eq!(
        runner.state().objects[&stack_spell].zone,
        Zone::Exile,
        "airbended stack spell must be exiled"
    );
    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&stack_spell),
        "stack spell owner must be able to cast the airbended card for {{2}}"
    );
}
