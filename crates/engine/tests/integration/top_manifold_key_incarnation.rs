//! CR 113.7a + CR 400.7 + CR 608.2c regression coverage for source-referential
//! activated abilities that remain on the stack across a zone change.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

#[test]
fn top_ability_does_not_follow_new_object_after_key_untaps_it() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(
        P0,
        &["Prepared Card A", "Prepared Card B", "Prepared Card C"],
    );
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(9_999),
            false,
            vec![],
        )],
    );

    let top = scenario
        .add_enchantment_from_oracle(
            P0,
            "Sensei's Divining Top",
            "{1}: Look at the top three cards of your library, then put them back in any order.\n{T}: Draw a card, then put this artifact on top of its owner's library.",
        )
        .as_artifact()
        .id();
    let key = scenario
        .add_enchantment_from_oracle(
            P0,
            "Manifold Key",
            "{1}, {T}: Untap another target artifact.\n{3}, {T}: Target creature can't be blocked this turn.",
        )
        .as_artifact()
        .id();

    let mut runner = scenario.build();
    let initial_hand_size = runner.state().players[0].hand.len();
    let next_library_card = runner.state().players[0].library[1];

    // CR 602.2b: Put the first Top activation on the stack and capture its
    // source incarnation before the intervening Key activation resolves.
    runner
        .act(GameAction::ActivateAbility {
            source_id: top,
            ability_index: 1,
        })
        .expect("Top's first draw activation must be accepted");
    let first_incarnation = runner.state().objects[&top].incarnation;
    let first_ability_incarnation = runner.state().stack[0]
        .ability()
        .and_then(|ability| ability.source_incarnation);
    assert_eq!(
        first_ability_incarnation,
        Some(first_incarnation),
        "ordinary artifacts must capture their source incarnation on the stack"
    );

    // CR 602.2b: Activate Key in response, choose Top, pay {1}, and resolve
    // only Key. The first Top activation remains underneath it.
    runner
        .act(GameAction::ActivateAbility {
            source_id: key,
            ability_index: 0,
        })
        .expect("Key's untap activation must be accepted");
    if matches!(
        runner.state().waiting_for,
        engine::types::game_state::WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(top)),
            })
            .expect("Key must be able to target Top");
    }
    assert!(
        runner.state().stack.back().is_some_and(|entry| {
            entry
                .ability()
                .is_some_and(|ability| ability.targets.contains(&TargetRef::Object(top)))
        }),
        "Key's stacked ability must target Top"
    );
    runner
        .act(GameAction::PassPriority)
        .expect("Key's mana payment or priority pass must be accepted");
    assert_eq!(
        runner.state().stack.len(),
        2,
        "Key must remain above Top's ability"
    );
    runner.resolve_top();
    assert_eq!(
        runner.state().stack.len(),
        1,
        "resolving Key must leave the first Top ability on the stack"
    );
    assert!(!runner.state().objects[&top].tapped, "Key must untap Top");

    // CR 405.1 + CR 608.2c: Activate Top again, then resolve the newer
    // activation first. It draws a card and puts the current Top object on top
    // of the library, creating a new object incarnation.
    runner
        .act(GameAction::ActivateAbility {
            source_id: top,
            ability_index: 1,
        })
        .expect("Top's second draw activation must be accepted");
    assert_eq!(
        runner.state().stack.len(),
        2,
        "both Top abilities must be stacked"
    );
    runner.resolve_top();
    assert_eq!(
        runner.state().players[0].hand.len(),
        initial_hand_size + 1,
        "the newer Top ability must draw one prepared card"
    );
    assert_eq!(runner.state().objects[&top].zone, Zone::Library);
    assert_eq!(runner.state().players[0].library[0], top);
    assert_ne!(
        runner.state().objects[&top].incarnation,
        first_incarnation,
        "moving Top to the library must create a new object"
    );
    assert_eq!(
        runner.state().stack[0]
            .ability()
            .and_then(|ability| ability.source_incarnation),
        Some(first_incarnation),
        "the older Top ability must retain its original source incarnation"
    );
    // CR 400.7 + CR 113.7a: The older ability draws Top as a new object. Its
    // stale SelfRef placement is a legal no-op, so the stack still settles.
    runner.resolve_top();
    assert!(
        runner.state().stack.is_empty(),
        "both Top abilities must resolve"
    );
    assert_eq!(
        runner.state().players[0].hand.len(),
        initial_hand_size + 2,
        "the older Top ability must draw Top"
    );
    assert_eq!(runner.state().objects[&top].zone, Zone::Hand);
    assert_eq!(runner.state().players[0].library[0], next_library_card);
}
