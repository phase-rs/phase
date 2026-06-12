//! Regression for issue #2414: Semblance Anvil must reduce costs only for
//! spells sharing a card type with the imprinted card.

use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::parser::oracle_static::parse_static_line;
use engine::types::card_type::CoreType;
use engine::types::identifiers::CardId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

#[test]
fn semblance_anvil_reduces_only_matching_card_type() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();
    let state = runner.state_mut();

    let anvil = create_object(
        state,
        CardId(1),
        P0,
        "Semblance Anvil".to_string(),
        Zone::Battlefield,
    );
    let reduction = parse_static_line(
        "Spells you cast that share a card type with the exiled card cost {2} less to cast.",
    )
    .expect("Semblance Anvil cost-reduction static should parse");
    state
        .objects
        .get_mut(&anvil)
        .unwrap()
        .static_definitions
        .push(reduction);

    let exiled_creature = create_object(
        state,
        CardId(2),
        P0,
        "Imprinted Creature".to_string(),
        Zone::Exile,
    );
    {
        let obj = state.objects.get_mut(&exiled_creature).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
    }
    state
        .exile_links
        .push(engine::types::game_state::ExileLink {
            source_id: anvil,
            exiled_id: exiled_creature,
            kind: engine::types::game_state::ExileLinkKind::TrackedBySource,
        });

    let matching_creature = create_object(
        state,
        CardId(3),
        P0,
        "Matching Creature Spell".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&matching_creature).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.mana_cost = ManaCost::generic(4);
    }

    let nonmatching_artifact = create_object(
        state,
        CardId(4),
        P0,
        "Nonmatching Artifact".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&nonmatching_artifact).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.mana_cost = ManaCost::generic(4);
    }

    let matching_cost =
        engine::game::casting::display_spell_cost(state, P0, matching_creature).unwrap();
    let nonmatching_cost =
        engine::game::casting::display_spell_cost(state, P0, nonmatching_artifact).unwrap();

    let ManaCost::Cost {
        generic: matching_generic,
        ..
    } = matching_cost
    else {
        panic!("expected cost for matching spell");
    };
    let ManaCost::Cost {
        generic: nonmatching_generic,
        ..
    } = nonmatching_cost
    else {
        panic!("expected cost for nonmatching spell");
    };

    assert_eq!(
        matching_generic, 2,
        "creature spell sharing imprinted creature type should be reduced by 2"
    );
    assert_eq!(
        nonmatching_generic, 4,
        "artifact spell must not receive Semblance Anvil's reduction"
    );
}
