//! Isildur's Fateful Strike: the object-controller antecedent binds through
//! `ParentTargetController`, never `TargetPlayer` (R2).
//!
//! Oracle: "Destroy target creature. If its controller has more than four
//! cards in hand, they exile cards from their hand equal to the difference."
//!
//! The spell declares a CREATURE target and no player target. "Its
//! controller" / "they" / "their" all denote the targeted creature's
//! controller (CR 109.4 + CR 608.2c). `TargetPlayer` cannot express that:
//! filter.rs resolves it through DECLARED player targets, of which there
//! are none — so the old promotion exported a silently empty continuation
//! (and lost the previously working self-target case). The typed
//! `ParentTargetController` ref carries the object anchor the resolver
//! needs, including its LKI fallback for the already-destroyed target.
//!
//! Two behaviors this file deliberately does NOT pin: the "more than four
//! cards in hand" gate and the "equal to the difference" count are both
//! unlowered (pre-existing gaps, out of R2's scope). Both real-cast tests
//! hold six cards so the gate-true case is observed; the count asserted is
//! the engine's honest current behavior, documented at the assertion.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{AbilityDefinition, ControllerRef, Effect, FilterProp, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ISILDUR: &str = "Destroy target creature. If its controller has more than four cards in hand, they exile cards from their hand equal to the difference.";

fn spell_head() -> AbilityDefinition {
    let parsed = parse_oracle_text(
        ISILDUR,
        "Isildur's Fateful Strike",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    assert_eq!(
        parsed.abilities.len(),
        1,
        "one spell ability, got {}",
        parsed.abilities.len()
    );
    assert!(
        parsed.triggers.is_empty(),
        "no triggers expected, got {:?}",
        parsed.triggers.len()
    );
    parsed.abilities.into_iter().next().expect("one ability")
}

/// The exile clause, reached through the destroy head's sequential sibling.
fn exile_clause(head: &AbilityDefinition) -> &AbilityDefinition {
    head.sub_ability
        .as_deref()
        .expect("the exile follows the destroy as a sub-ability")
}

#[test]
fn isildur_exile_subject_is_the_targeted_creatures_controller() {
    let head = spell_head();
    assert!(
        matches!(&*head.effect, Effect::Destroy { .. }),
        "head must destroy, got {:?}",
        head.effect
    );
    let exile = exile_clause(&head);
    let Effect::ChangeZone {
        origin,
        destination,
        target,
        ..
    } = &*exile.effect
    else {
        panic!("exile clause must be a ChangeZone, got {:?}", exile.effect);
    };
    assert_eq!(*origin, Some(Zone::Hand));
    assert_eq!(*destination, Zone::Exile);
    // R2: the exiling player is the PARENT (creature) target's controller.
    // A TargetPlayer here would resolve through declared player targets —
    // of which this spell has none — and exile nothing.
    let TargetFilter::Typed(filter) = target else {
        panic!("exile target must be a typed filter, got {target:?}");
    };
    assert_eq!(
        filter.controller,
        Some(ControllerRef::ParentTargetController),
        "exile scope must be the targeted creature's controller, got {:?}",
        filter.controller
    );
    assert!(
        filter.properties.iter().any(|p| matches!(
            p,
            FilterProp::Owned {
                controller: ControllerRef::ParentTargetController
            }
        )),
        "exiled cards must be owned by the targeted creature's controller, got {:?}",
        filter.properties
    );
    assert!(
        !format!("{head:?}").contains("TargetPlayer"),
        "no player-target ref may appear: the spell declares no player target"
    );
}

/// P0 holds Isildur; `victim` holds the creature and a six-card hand.
/// Returns the runner, the spell, the creature, and the hand ids.
fn six_card_setup(
    victim: engine::types::player::PlayerId,
) -> (GameRunner, ObjectId, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_vanilla(victim, 2, 2);
    let mut hand = Vec::new();
    for i in 0..6 {
        hand.push(scenario.add_card_to_hand(victim, &format!("Victim card {i}")));
    }
    for i in 0..7 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Isildur's Fateful Strike", false, ISILDUR)
        .id();
    (scenario.build(), spell, creature, hand)
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects.get(&id).expect("object exists").zone
}

/// Drive the cast to the exile prompt; answer it; drain to completion.
/// Returns the ids the prompted player exiled.
fn cast_and_answer_exile(
    runner: &mut GameRunner,
    spell: ObjectId,
    target: ObjectId,
    expected_chooser: engine::types::player::PlayerId,
    hand: &[ObjectId],
) -> Vec<ObjectId> {
    runner.cast(spell).target_object(target).resolve();
    let mut exiled = Vec::new();
    let mut saw_prompt = false;
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                count,
                ..
            } => {
                assert_eq!(
                    player, expected_chooser,
                    "the targeted creature's controller must choose the exiles"
                );
                assert!(
                    !cards.is_empty() && count >= 1,
                    "the continuation must exile something (the old empty \
                     continuation offered no prompt at all)"
                );
                assert!(
                    cards.iter().all(|c| hand.contains(c)),
                    "only the chooser's hand cards may be offered, got {cards:?}"
                );
                // The "equal to the difference" count is unlowered
                // (pre-existing gap): answer the engine's honest count.
                let chosen: Vec<ObjectId> = cards.into_iter().take(count).collect();
                exiled.clone_from(&chosen);
                runner
                    .act(GameAction::SelectCards { cards: chosen })
                    .expect("exile choice must be accepted");
                saw_prompt = true;
            }
            other => panic!("unexpected prompt while resolving Isildur: {other:?}"),
        }
    }
    assert!(saw_prompt, "the exile prompt must have been offered");
    assert!(
        runner.state().stack.is_empty(),
        "the spell must fully resolve"
    );
    exiled
}

#[test]
fn isildur_real_cast_opponents_creature_controller_exiles() {
    let (mut runner, spell, creature, hand) = six_card_setup(P1);
    let p0_hand_before: Vec<ObjectId> = runner
        .state()
        .objects
        .iter()
        .filter(|(id, o)| o.zone == Zone::Hand && o.owner == P0 && **id != spell)
        .map(|(id, _)| *id)
        .collect();

    let exiled = cast_and_answer_exile(&mut runner, spell, creature, P1, &hand);

    assert_eq!(
        zone_of(&runner, creature),
        Zone::Graveyard,
        "the targeted creature must be destroyed"
    );
    assert_eq!(
        exiled.len(),
        1,
        "engine's honest count for the unlowered difference (6 in hand); \
         Oracle demands 2 — the difference-count gap, not R2's subject"
    );
    for id in &exiled {
        assert_eq!(zone_of(&runner, *id), Zone::Exile, "chosen cards exiled");
    }
    for id in hand.iter().filter(|id| !exiled.contains(id)) {
        assert_eq!(zone_of(&runner, *id), Zone::Hand, "unchosen cards stay");
    }
    // The caster pays nothing: no P0 hand card may move.
    for id in &p0_hand_before {
        assert_eq!(zone_of(&runner, *id), Zone::Hand, "caster hand untouched");
    }
}

#[test]
fn isildur_real_cast_own_creature_caster_exiles() {
    // The self-target case the old promotion lost: targeting your own
    // creature makes YOU "its controller", so you exile from your hand.
    let (mut runner, spell, creature, hand) = six_card_setup(P0);
    let p1_hand_before: Vec<ObjectId> = runner
        .state()
        .objects
        .iter()
        .filter(|(_, o)| o.zone == Zone::Hand && o.owner == P1)
        .map(|(id, _)| *id)
        .collect();

    let exiled = cast_and_answer_exile(&mut runner, spell, creature, P0, &hand);

    assert_eq!(
        zone_of(&runner, creature),
        Zone::Graveyard,
        "the targeted creature must be destroyed"
    );
    assert!(!exiled.is_empty(), "the caster must exile something");
    for id in &exiled {
        assert_eq!(zone_of(&runner, *id), Zone::Exile, "chosen cards exiled");
    }
    for id in p1_hand_before {
        assert_eq!(zone_of(&runner, id), Zone::Hand, "opponent hand untouched");
    }
}
