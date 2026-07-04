//! Regression for issue #3258: Show and Tell must let each player decline
//! putting a permanent from hand onto the battlefield.
//!
//! https://github.com/phase-rs/phase/issues/3258
//!
//! Oracle: "Each player may put an artifact, creature, enchantment, or land
//! card from their hand onto the battlefield."

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SHOW_AND_TELL_ORACLE: &str = "Each player may put an artifact, creature, enchantment, or land card from their hand onto the battlefield.";

fn hand_size(state: &GameState, player: engine::types::player::PlayerId) -> usize {
    state.players[player.0 as usize].hand.len()
}

fn battlefield_count(state: &GameState) -> usize {
    state
        .objects
        .values()
        .filter(|o| o.zone == Zone::Battlefield)
        .count()
}

#[test]
fn show_and_tell_each_player_may_decline_optional_put() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_land_to_hand(P0, "Island");
    scenario.add_creature_to_hand(P0, "Grizzly Bears", 2, 2);
    scenario.add_land_to_hand(P1, "Plains");
    scenario.add_creature_to_hand(P1, "Elvish Mystic", 1, 1);

    let show_and_tell = scenario
        .add_spell_to_hand_from_oracle(P0, "Show and Tell", false, SHOW_AND_TELL_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();
    let hand_p1_before = hand_size(runner.state(), P1);
    let bf_before = battlefield_count(runner.state());

    let outcome = runner.cast(show_and_tell).decline_optional().resolve();

    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Show and Tell must finish after both players decline, got {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(
        hand_size(outcome.state(), P0),
        2,
        "P0 keeps Island + Grizzly after declining (only the cast spell left hand)"
    );
    assert_eq!(
        hand_size(outcome.state(), P1),
        hand_p1_before,
        "P1 hand must be unchanged when declining Show and Tell"
    );
    assert_eq!(
        battlefield_count(outcome.state()),
        bf_before,
        "battlefield must be unchanged when all players decline"
    );
}

#[test]
fn show_and_tell_parsed_as_optional_per_player_put() {
    use engine::parser::oracle::parse_oracle_text;
    use engine::types::ability::{Effect, PlayerFilter};

    let parsed = parse_oracle_text(
        SHOW_AND_TELL_ORACLE,
        "Show and Tell",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    let ability = parsed.abilities.first().expect("spell ability");
    assert!(
        ability.optional,
        "Show and Tell must be optional, got {:?}",
        ability.effect
    );
    assert_eq!(ability.player_scope, Some(PlayerFilter::All));
    assert!(
        matches!(&*ability.effect, Effect::ChangeZone { .. }),
        "expected ChangeZone, got {:?}",
        ability.effect
    );
}

#[test]
fn show_and_tell_prompts_optional_before_zone_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_land_to_hand(P0, "Island");
    scenario.add_land_to_hand(P1, "Plains");

    let show_and_tell = scenario
        .add_spell_to_hand_from_oracle(P0, "Show and Tell", false, SHOW_AND_TELL_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();
    runner.cast(show_and_tell).commit().resolve();

    assert_eq!(
        hand_size(runner.state(), P0),
        1,
        "P0 should keep only Island after declining (spell left hand on cast)"
    );
    assert_eq!(
        hand_size(runner.state(), P1),
        1,
        "P1 should keep Plains after declining"
    );
    assert_eq!(
        battlefield_count(runner.state()),
        0,
        "declining must not put permanents onto the battlefield"
    );
}

#[test]
fn show_and_tell_acceptor_puts_one_card_from_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let island = scenario.add_land_to_hand(P0, "Island").id();
    scenario.add_land_to_hand(P1, "Plains");

    let show_and_tell = scenario
        .add_spell_to_hand_from_oracle(P0, "Show and Tell", false, SHOW_AND_TELL_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();
    let bf_before = battlefield_count(runner.state());

    runner.cast(show_and_tell).commit();

    while matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
            | WaitingFor::EffectZoneChoice { .. }
            | WaitingFor::Priority { .. }
    ) {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                let accept = *player == P0;
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("optional decision");
            }
            WaitingFor::EffectZoneChoice { cards, .. } => {
                let pick = cards
                    .iter()
                    .find(|id| **id == island)
                    .copied()
                    .or_else(|| cards.first().copied())
                    .expect("legal card to put");
                runner
                    .act(GameAction::SelectCards { cards: vec![pick] })
                    .expect("zone choice");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }

    assert_eq!(
        hand_size(runner.state(), P0),
        0,
        "P0 should put Island from hand when accepting"
    );
    assert_eq!(
        battlefield_count(runner.state()),
        bf_before + 1,
        "exactly one permanent should enter from P0's accept"
    );
    assert_eq!(
        runner.state().objects[&island].zone,
        Zone::Battlefield,
        "accepted card should be on the battlefield"
    );
}

#[test]
fn show_and_tell_change_zone_filter_uses_scoped_player() {
    use engine::parser::oracle::parse_oracle_text;
    use engine::types::ability::{Effect, FilterProp, TargetFilter};
    use engine::types::zones::Zone;
    use engine::types::ControllerRef;

    let parsed = parse_oracle_text(
        SHOW_AND_TELL_ORACLE,
        "Show and Tell",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    let ability = parsed.abilities.first().expect("spell ability");
    let Effect::ChangeZone { origin, target, .. } = &*ability.effect else {
        panic!("expected ChangeZone, got {:?}", ability.effect);
    };
    assert_eq!(origin.as_ref(), Some(&Zone::Hand));
    let TargetFilter::Or { filters } = target else {
        panic!("expected or-filter, got {target:?}");
    };
    assert!(
        filters.iter().any(|filter| {
            matches!(
                filter,
                TargetFilter::Typed(tf) if tf.properties.iter().any(|prop| {
                    matches!(
                        prop,
                        FilterProp::Owned {
                            controller: ControllerRef::ScopedPlayer
                        }
                    )
                })
            )
        }),
        "their hand must bind to the iterating scoped player"
    );
}

#[test]
fn show_and_tell_p0_declines_p1_accepts_from_own_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let island = scenario.add_land_to_hand(P0, "Island").id();
    let plains = scenario.add_land_to_hand(P1, "Plains").id();
    scenario.add_creature_to_hand(P1, "Elvish Mystic", 1, 1);

    let show_and_tell = scenario
        .add_spell_to_hand_from_oracle(P0, "Show and Tell", false, SHOW_AND_TELL_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();
    let bf_before = battlefield_count(runner.state());
    let mut p1_optional_prompted = false;
    let mut p1_zone_choice_prompted = false;

    runner.cast(show_and_tell).commit();

    while matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
            | WaitingFor::EffectZoneChoice { .. }
            | WaitingFor::Priority { .. }
    ) {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                let accept = *player == P1;
                if *player == P1 {
                    p1_optional_prompted = true;
                }
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("optional decision");
            }
            WaitingFor::EffectZoneChoice { cards, .. } => {
                assert!(
                    cards.contains(&plains),
                    "P1 zone choice must include P1's Plains, got {cards:?}"
                );
                p1_zone_choice_prompted = true;
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![plains],
                    })
                    .expect("zone choice");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }

    assert!(
        p1_optional_prompted,
        "P1 must receive its own OptionalEffectChoice after P0 declines"
    );
    assert!(
        p1_zone_choice_prompted,
        "P1 must receive an EffectZoneChoice scoped to its hand"
    );
    assert_eq!(
        hand_size(runner.state(), P0),
        1,
        "P0's Island must stay in hand when P0 declines"
    );
    assert_eq!(
        runner.state().objects[&island].zone,
        Zone::Hand,
        "P0's Island must not move to the battlefield"
    );
    assert_eq!(
        hand_size(runner.state(), P1),
        1,
        "P1 should put Plains from hand when accepting and keep the other card"
    );
    assert_eq!(
        battlefield_count(runner.state()),
        bf_before + 1,
        "exactly one permanent should enter from P1's accept"
    );
    assert_eq!(
        runner.state().objects[&plains].zone,
        Zone::Battlefield,
        "P1's Plains should be on the battlefield"
    );
}
