//! "Cast this spell only if you've cast another [<filter>] spell this turn."
//!
//! CR 601.3: the restriction is checked when the spell would be cast — before
//! it is recorded in this turn's spell history. The condition used to be
//! `SpellsCastThisTurn >= 2` ("counting this spell"), so with one earlier spell
//! the count was 1 and the card was never castable.
//!
//! Field report (2026-10-03, Mesa): Talara's Battalion stayed uncastable after
//! Pugnacious Hammerskull (green) had been cast that turn. Illusory Angel
//! ("another spell", no filter) had the same defect.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// Verbatim Oracle text (AtomicCards.json).
const TALARA: &str = "Cast this spell only if you've cast another green spell this turn.\nTrample";
const ILLUSORY_ANGEL: &str = "Cast this spell only if you've cast another spell this turn.\nFlying";

fn cast_action(runner: &GameRunner, id: ObjectId) -> GameAction {
    GameAction::CastSpell {
        object_id: id,
        card_id: runner.state().objects[&id].card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    }
}

fn creature_in_hand(
    scenario: &mut GameScenario,
    name: &str,
    color: ManaColor,
    shard: ManaCostShard,
) -> ObjectId {
    let mut b = scenario.add_creature_to_hand(P0, name, 2, 2);
    b.with_mana_cost(ManaCost::Cost {
        shards: vec![shard],
        generic: 1,
    });
    b.with_color(vec![color]);
    b.id()
}

/// Lands for both colours, a green and a blue two-drop, and the restricted card.
fn setup(
    restricted_name: &str,
    restricted_oracle: &str,
    restricted_power: i32,
    restricted_toughness: i32,
    restricted_generic: u32,
    restricted_color: ManaColor,
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::Green);
        scenario.add_basic_land(P0, ManaColor::Blue);
    }
    let shard = if restricted_color == ManaColor::Green {
        ManaCostShard::Green
    } else {
        ManaCostShard::Blue
    };
    let restricted = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            restricted_name,
            restricted_power,
            restricted_toughness,
            restricted_oracle,
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![shard],
            generic: restricted_generic,
        })
        .id();
    let green = creature_in_hand(
        &mut scenario,
        "Green Bear",
        ManaColor::Green,
        ManaCostShard::Green,
    );
    let blue = creature_in_hand(
        &mut scenario,
        "Blue Bear",
        ManaColor::Blue,
        ManaCostShard::Blue,
    );
    (scenario.build(), restricted, green, blue)
}

#[test]
fn talara_is_not_castable_before_any_spell() {
    let (mut runner, talara, _, _) = setup("Talara's Battalion", TALARA, 4, 3, 1, ManaColor::Green);
    let action = cast_action(&runner, talara);
    assert!(runner.act(action).is_err(), "no spell cast yet this turn");
    assert_eq!(runner.state().objects[&talara].zone, Zone::Hand);
}

#[test]
fn talara_is_castable_after_another_green_spell() {
    let (mut runner, talara, green, _) =
        setup("Talara's Battalion", TALARA, 4, 3, 1, ManaColor::Green);
    runner.cast(green).resolve();
    let action = cast_action(&runner, talara);
    runner
        .act(action)
        .expect("CR 601.3: one earlier green spell satisfies \"another green spell\"");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&talara].zone, Zone::Battlefield);
}

#[test]
fn talara_is_not_castable_after_only_a_blue_spell() {
    let (mut runner, talara, _, blue) =
        setup("Talara's Battalion", TALARA, 4, 3, 1, ManaColor::Green);
    runner.cast(blue).resolve();
    let action = cast_action(&runner, talara);
    assert!(
        runner.act(action).is_err(),
        "a blue spell is not \"another green spell\""
    );
    assert_eq!(runner.state().objects[&talara].zone, Zone::Hand);
}

#[test]
fn illusory_angel_is_castable_after_any_other_spell() {
    let (mut runner, angel, _, blue) =
        setup("Illusory Angel", ILLUSORY_ANGEL, 4, 4, 2, ManaColor::Blue);
    let refused = cast_action(&runner, angel);
    assert!(runner.act(refused).is_err(), "no spell cast yet this turn");
    runner.cast(blue).resolve();
    let action = cast_action(&runner, angel);
    runner
        .act(action)
        .expect("CR 601.3: one earlier spell satisfies \"another spell\"");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&angel].zone, Zone::Battlefield);
}
