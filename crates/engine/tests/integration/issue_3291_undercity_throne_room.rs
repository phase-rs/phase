//! Regression for GitHub issue #3291 — Undercity room 8 (Throne of the Dead Three).
//!
//! Oracle: "Reveal the top ten cards of your library. Put a creature card from
//! among them onto the battlefield with three +1/+1 counters on it. It gains
//! hexproof until your next turn. Then shuffle."
//!
//! Bug: room effect was still `Effect::Unimplemented`, so venturing into the
//! final room did nothing.

use engine::game::dungeon::{dungeon_sentinel_id, room_effects, DungeonId};
use engine::game::effects::resolve_ability_chain;
use engine::game::engine::apply_as_current;
use engine::game::keywords::has_keyword;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{
    ContinuousModification, Effect, QuantityExpr, ResolvedAbility, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::zones::Zone;

fn resolved_chain_contains(
    ability: &ResolvedAbility,
    mut pred: impl FnMut(&Effect) -> bool,
) -> bool {
    if pred(&ability.effect) {
        return true;
    }
    ability
        .sub_ability
        .as_ref()
        .is_some_and(|sub| resolved_chain_contains(sub, pred))
}

fn assert_no_unimplemented_resolved(ability: &ResolvedAbility) {
    assert!(
        !matches!(ability.effect, Effect::Unimplemented { .. }),
        "unexpected Unimplemented effect: {:?}",
        ability.effect
    );
    if let Some(sub) = ability.sub_ability.as_ref() {
        assert_no_unimplemented_resolved(sub);
    }
}

fn assert_throne_room_chain(ability: &ResolvedAbility) {
    assert_no_unimplemented_resolved(ability);
    assert!(
        matches!(
            ability.effect,
            Effect::Dig {
                reveal: true,
                count: QuantityExpr::Fixed { value: 10 },
                ..
            }
        ),
        "Throne must reveal top ten, got {:?}",
        ability.effect
    );
    assert!(
        resolved_chain_contains(ability, |effect| matches!(
            effect,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::ParentTarget,
            }
        )),
        "Throne must put three +1/+1 counters on the dug creature"
    );
    assert!(
        resolved_chain_contains(ability, |effect| {
            matches!(effect, Effect::GenericEffect { static_abilities, .. }
            if static_abilities.iter().any(|st| {
                matches!(st.affected, Some(TargetFilter::ParentTarget))
                    && st.modifications.iter().any(|m| matches!(
                        m,
                        ContinuousModification::AddKeyword {
                            keyword: Keyword::Hexproof
                        }
                    ))
            }))
        }),
        "Throne must grant hexproof to the dug creature"
    );
    assert!(
        resolved_chain_contains(ability, |effect| matches!(
            effect,
            Effect::Shuffle {
                target: TargetFilter::Controller,
                ..
            }
        )),
        "Throne must shuffle the controller's library"
    );
}

#[test]
fn undercity_throne_room_effect_chain_is_fully_parsed() {
    let (ability, _) = room_effects(DungeonId::Undercity, 8, ObjectId(1), P0);
    assert_throne_room_chain(&ability);
}

#[test]
fn undercity_throne_room_resolves_creature_counters_hexproof_and_shuffle() {
    let mut scenario = GameScenario::new();
    for i in 0..9 {
        scenario.add_card_to_library_top(P0, &format!("Noncreature {i}"));
    }
    let creature = scenario.add_card_to_library_top(P0, "Undead Servant");

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        let obj = state.objects.get_mut(&creature).expect("library creature");
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
    }

    let library_before = runner.state().players[0].library.clone();
    let (ability, _) = room_effects(DungeonId::Undercity, 8, dungeon_sentinel_id(P0), P0);

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("Throne room begins resolving");

    let apply_result = match runner.state().waiting_for.clone() {
        WaitingFor::DigChoice { cards, .. } => {
            assert!(
                cards.contains(&creature),
                "looked-at cards must include the library creature, got {cards:?}"
            );
            apply_as_current(
                runner.state_mut(),
                GameAction::SelectCards {
                    cards: vec![creature],
                },
            )
            .expect("keep the creature from the dig")
        }
        other => panic!("expected DigChoice after reveal-dig, got {other:?}"),
    };
    events.extend(apply_result.events);

    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Battlefield,
        "chosen creature must enter the battlefield"
    );
    assert_eq!(
        runner.state().objects[&creature]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        Some(3),
        "creature must enter with three +1/+1 counters"
    );
    assert!(
        has_keyword(&runner.state().objects[&creature], &Keyword::Hexproof),
        "creature must gain hexproof until next turn"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerPerformedAction {
                action: PlayerActionKind::ShuffledLibrary,
                ..
            }
        )),
        "library must be shuffled after the room resolves"
    );
    assert_ne!(
        runner.state().players[0].library,
        library_before,
        "library order must change after shuffle"
    );
}
