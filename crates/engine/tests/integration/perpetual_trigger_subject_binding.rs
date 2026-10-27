//! PR #8494 regression: a bare `it` in a perpetual-grant body must keep the
//! trigger condition's object referent (CR 608.2k).
//!
//! Effluence Devourer's trigger is grammatically headed by `you`, so the parser
//! context's subject is a player filter even though the trigger's valid card is
//! the sacrificed creature. `resolve_it_pronoun` binds that antecedent to
//! `TriggeringSource`, whose runtime resolver understands
//! `GameEvent::PermanentSacrificed`; `ParentTarget` does not.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::PerpetualModification;
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::ObjectId;

#[test]
fn volatile_rift_modifies_the_library_to_hand_event_object_and_counts_only_own_colors() {
    use engine::types::ability::QuantityExpr;
    use engine::types::game_state::WaitingFor;
    use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
    const RIFT: &str = "Vivid — Whenever a creature card is put into your hand from your library, it perpetually gets +X/+0, where X is the number of colors among permanents you control.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])],
    );
    let rift = scenario
        .add_enchantment_from_oracle(P0, "Volatile Rift", RIFT)
        .with_color(vec![ManaColor::Red, ManaColor::Green])
        .id();
    let own_decoy = scenario
        .add_creature(P0, "Own green decoy", 2, 2)
        .with_color(vec![ManaColor::Green])
        .id();
    let opponent = scenario
        .add_creature(
            engine::game::scenario::P1,
            "Opponent white black decoy",
            2,
            2,
        )
        .with_color(vec![ManaColor::White, ManaColor::Black])
        .id();
    let existing_hand = scenario
        .add_creature_to_hand(P0, "Earlier hand creature", 2, 2)
        .id();
    let drawn = scenario
        .add_spell_to_library_top(P0, "Library creature", false)
        .as_creature()
        .id();
    let draw = scenario
        .add_spell_to_hand_from_oracle(P0, "Reach Through Mists", true, "Draw a card.")
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    let mut runner = scenario.build();
    runner.cast(draw).resolve();
    assert_eq!(runner.state().objects[&drawn].zone, Zone::Hand);
    assert!(runner.state().objects[&drawn]
        .perpetual_mods
        .iter()
        .any(|modification| matches!(
            modification,
            PerpetualModification::ModifyPowerToughness {
                power: QuantityExpr::Fixed { value: 2 },
                toughness: QuantityExpr::Fixed { value: 0 },
                ..
            }
        )));
    for decoy in [rift, own_decoy, opponent, existing_hand] {
        assert!(runner.state().objects[&decoy].perpetual_mods.is_empty());
    }
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty());
}

const EFFLUENCE_DEVOURER: &str = "Whenever you sacrifice Effluence Devourer or another creature, it perpetually gains \"{2}, Exile this card from your graveyard: Create an X/X green Ooze creature token, where X is this card's power. Activate only as a sorcery.\"";

fn has_granted_ability(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id]
        .perpetual_mods
        .iter()
        .any(|modification| matches!(modification, PerpetualModification::GrantAbility { .. }))
}

#[test]
fn effluence_devourer_grants_the_sacrificed_creature_not_the_trigger_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let devourer = scenario
        .add_creature_from_oracle(P0, "Effluence Devourer", 3, 4, EFFLUENCE_DEVOURER)
        .id();
    let victim = scenario.add_creature(P0, "Sacrifice Witness", 2, 2).id();
    let sacrifice = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic Sacrifice", true, "Sacrifice a creature.")
        .id();

    let mut runner = scenario.build();
    runner.cast(sacrifice).effect_zone(&[victim]).resolve();

    assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
    assert!(
        has_granted_ability(&runner, victim),
        "the sacrificed creature is the trigger antecedent and receives the perpetual grant"
    );
    assert!(
        !has_granted_ability(&runner, devourer),
        "the trigger source must not receive a grant meant for the sacrificed creature"
    );
}
