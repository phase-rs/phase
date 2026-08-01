//! Issue #6446: Relic of Progenitus first ability — verbatim Oracle text:
//!   "{T}: Target player exiles a card from their graveyard.
//!    {1}, Exile this artifact: Exile all graveyards. Draw a card."
//!
//! Rules-correct behavior (CR 115.1c + CR 115.10a + CR 608.2d):
//! the ability targets ONLY the player. That player then CHOOSES — does not
//! TARGET — a card in THEIR graveyard to exile as the ability resolves.
//!
//! The bug had two halves:
//!   (A) Stack timing + Owned{TargetPlayer} rebind produced THREE stack slots
//!       (TargetOnly player + companion TargetPlayer + ChangeZone card), and
//!   (B) the activator chose the card at announcement instead of the targeted
//!       player at resolution.
//!
//! These tests are per-mechanism revert-sensitive; each notes which revert flips
//! it. Mirror of `strategic_betrayal_6505.rs` for the off-battlefield twin.

use engine::game::ability_utils::{build_resolved_from_def, build_target_slots};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, ControllerRef, Effect, FilterProp, TargetChoiceTiming, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const RELIC_ORACLE: &str = "{T}: Target player exiles a card from their graveyard.\n\
{1}, Exile this artifact: Exile all graveyards. Draw a card.";

fn chain_has_unimplemented(def: &AbilityDefinition) -> bool {
    fn effect_unimplemented(effect: &Effect) -> bool {
        matches!(effect, Effect::Unimplemented { .. })
    }
    effect_unimplemented(&def.effect)
        || def
            .sub_ability
            .as_deref()
            .is_some_and(chain_has_unimplemented)
        || def
            .else_ability
            .as_deref()
            .is_some_and(chain_has_unimplemented)
}

fn parse_relic_first_ability() -> AbilityDefinition {
    let parsed = parse_oracle_text(RELIC_ORACLE, "Relic of Progenitus", &[], &[], &[]);
    let first = parsed
        .abilities
        .into_iter()
        .find(|a| matches!(&*a.effect, Effect::TargetOnly { .. }))
        .expect("Relic first ability must be TargetOnly player wrap");
    assert!(
        !chain_has_unimplemented(&first),
        "Relic first ability must parse with no Unimplemented; got {first:#?}"
    );
    first
}

fn find_change_zone(def: &AbilityDefinition) -> Option<&AbilityDefinition> {
    let mut cursor = Some(def);
    while let Some(node) = cursor {
        if matches!(*node.effect, Effect::ChangeZone { .. }) {
            return Some(node);
        }
        cursor = node.sub_ability.as_deref();
    }
    None
}

fn graveyard_ids(runner: &GameRunner, player: engine::types::player::PlayerId) -> Vec<ObjectId> {
    runner.state().players[player.0 as usize]
        .graveyard
        .iter()
        .copied()
        .collect()
}

fn first_exile_ability_index(runner: &GameRunner, relic: ObjectId) -> usize {
    runner.state().objects[&relic]
        .abilities
        .iter()
        .position(|def| matches!(&*def.effect, Effect::TargetOnly { .. }))
        .expect("Relic must expose the TargetOnly first ability")
}

/// SHAPE: first ability is TargetOnly{Player} → Resolution ChangeZone with
/// Owned{ScopedPlayer}. Exactly one stack target slot.
#[test]
fn shape_one_player_target_resolution_scoped_owned() {
    let first = parse_relic_first_ability();
    let Effect::TargetOnly { target } = &*first.effect else {
        panic!("expected TargetOnly, got {:?}", first.effect);
    };
    assert_eq!(*target, TargetFilter::Player);

    let cz = find_change_zone(&first).expect("ChangeZone exile leg");
    assert_eq!(
        cz.target_choice_timing,
        TargetChoiceTiming::Resolution,
        "card is chosen at resolution (CR 608.2d), not a stack target"
    );
    let Effect::ChangeZone {
        origin,
        destination,
        target,
        ..
    } = cz.effect.as_ref()
    else {
        unreachable!()
    };
    assert_eq!(*origin, Some(Zone::Graveyard));
    assert_eq!(*destination, Zone::Exile);
    let TargetFilter::Typed(typed) = target else {
        panic!("expected typed GY card filter, got {target:?}");
    };
    assert!(
        typed.properties.contains(&FilterProp::Owned {
            controller: ControllerRef::ScopedPlayer
        }),
        "must keep ScopedPlayer for resolution chooser stamp; got {typed:?}"
    );
    assert!(
        !typed.properties.contains(&FilterProp::Owned {
            controller: ControllerRef::TargetPlayer
        }),
        "must not rebind to TargetPlayer; got {typed:?}"
    );

    let state = engine::types::game_state::GameState::new_two_player(42);
    let resolved = build_resolved_from_def(&first, ObjectId(1), engine::types::player::PlayerId(0));
    let slots = build_target_slots(&state, &resolved).expect("slots");
    assert_eq!(
        slots.len(),
        1,
        "exactly one stack target (the player); companion + card must not appear — got {slots:?}"
    );
}

/// E2E: activator targets P1; P1 chooses from P1's GY; P0's GY cards are not
/// offered. Revert Resolution/ScopedPlayer → wrong chooser or card targets.
#[test]
fn end_to_end_targeted_player_chooses_from_own_graveyard() {
    let _ = parse_relic_first_ability();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_graveyard(P1, &["Opp GY A", "Opp GY B"]);
    // Multi-authority hostile: activator also has GY cards — must not be offered.
    scenario.with_graveyard(P0, &["Activator GY Card"]);

    let relic = scenario
        .add_creature(P0, "Relic of Progenitus", 0, 0)
        .as_artifact()
        .from_oracle_text(RELIC_ORACLE)
        .id();

    let mut runner = scenario.build();
    let p1_gy = graveyard_ids(&runner, P1);
    let p0_gy = graveyard_ids(&runner, P0);
    assert_eq!(p1_gy.len(), 2);
    assert_eq!(p0_gy.len(), 1);

    let idx = first_exile_ability_index(&runner, relic);
    let outcome = runner.activate(relic, idx).target_player(P1).resolve();

    match outcome.final_waiting_for() {
        WaitingFor::EffectZoneChoice { player, cards, .. } => {
            assert_eq!(*player, P1, "targeted player chooses the exiled card");
            for id in &p1_gy {
                assert!(
                    cards.contains(id),
                    "P1 GY cards must be offered; got {cards:?}"
                );
            }
            for id in &p0_gy {
                assert!(
                    !cards.contains(id),
                    "activator GY cards must not be offered; got {cards:?}"
                );
            }
        }
        other => panic!("expected EffectZoneChoice for P1; got {other:?}"),
    }

    let chosen = p1_gy[0];
    let unchosen = p1_gy[1];
    runner
        .act(GameAction::SelectCards {
            cards: vec![chosen],
        })
        .expect("P1 selects a GY card to exile");

    assert_eq!(
        runner.state().objects[&chosen].zone,
        Zone::Exile,
        "chosen card is exiled"
    );
    assert_eq!(
        runner.state().objects[&unchosen].zone,
        Zone::Graveyard,
        "unchosen card stays in graveyard"
    );
    assert_eq!(
        runner.state().objects[&p0_gy[0]].zone,
        Zone::Graveyard,
        "activator GY card must remain untouched"
    );
}

/// Self-target: activator chooses from their own graveyard.
#[test]
fn self_target_chooser_is_activator() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_graveyard(P0, &["Own GY A", "Own GY B"]);
    scenario.with_graveyard(P1, &["Opp GY"]);

    let relic = scenario
        .add_creature(P0, "Relic of Progenitus", 0, 0)
        .as_artifact()
        .from_oracle_text(RELIC_ORACLE)
        .id();

    let mut runner = scenario.build();
    let p0_gy = graveyard_ids(&runner, P0);
    let p1_gy = graveyard_ids(&runner, P1);
    let idx = first_exile_ability_index(&runner, relic);
    let outcome = runner.activate(relic, idx).target_player(P0).resolve();

    match outcome.final_waiting_for() {
        WaitingFor::EffectZoneChoice { player, cards, .. } => {
            assert_eq!(*player, P0);
            for id in &p0_gy {
                assert!(cards.contains(id), "own GY must be offered; got {cards:?}");
            }
            for id in &p1_gy {
                assert!(
                    !cards.contains(id),
                    "opponent GY must not be offered; got {cards:?}"
                );
            }
        }
        other => panic!("expected EffectZoneChoice for P0; got {other:?}"),
    }
}
