//! Issue #8760 — Appa, Steadfast Guardian airbends any number of other
//! target nonland permanents you control (including zero).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, ControllerRef, Effect, EffectKind, FilterProp, MultiTargetSpec,
    TargetFilter, TargetRef, TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::{BendingType, GameEvent};
use engine::types::game_state::{StackEntryKind, TargetSelectionSlot, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const APPA_ORACLE: &str = "Flash\nFlying\nWhen Appa enters, airbend any number of other target nonland permanents you control. (Exile them. While each one is exiled, its owner may cast it for {2} rather than its mana cost.)\nWhenever you cast a spell from exile, create a 1/1 white Ally creature token.";

const AVATAR_AANG_ORACLE: &str = "Flying, firebending 2\nWhenever you waterbend, earthbend, \
     firebend, or airbend, draw a card. Then if you've done all four this turn, transform \
     Avatar Aang.";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

fn grant_priority(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

fn ability_contains_unimplemented(definition: &AbilityDefinition) -> bool {
    matches!(definition.effect.as_ref(), Effect::Unimplemented { .. })
        || definition
            .sub_ability
            .as_deref()
            .is_some_and(ability_contains_unimplemented)
        || definition
            .else_ability
            .as_deref()
            .is_some_and(ability_contains_unimplemented)
}

/// Stage Appa in P0's hand with exactly the mana to cast it.
fn add_castable_appa(scenario: &mut GameScenario) -> ObjectId {
    let appa = scenario
        .add_creature_to_hand_from_oracle(P0, "Appa, Steadfast Guardian", 3, 4, APPA_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::White, ManaCostShard::White],
        })
        .id();
    scenario.with_mana_pool(P0, floating_mana(4, ManaType::White));
    appa
}

fn appa_board() -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_basic_land(P0, ManaColor::White);
    let mine_a = scenario.add_creature(P0, "Grizzly Bears A", 2, 2).id();
    let mine_b = scenario.add_creature(P0, "Grizzly Bears B", 2, 2).id();
    let theirs = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();
    let appa = add_castable_appa(&mut scenario);
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    (runner, land, mine_a, mine_b, theirs, appa)
}

/// Appa in hand beside Avatar Aang, whose "whenever you … airbend, draw a card"
/// trigger observes whether Appa's ETB counted as an airbend (CR 701.65b).
fn appa_with_avatar_aang_board() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    scenario
        .add_creature(P0, "Avatar Aang", 4, 4)
        .from_oracle_text_with_keywords(&["Flying", "firebending"], AVATAR_AANG_ORACLE);
    scenario.add_card_to_library_top(P0, "Library Card");
    let appa = add_castable_appa(&mut scenario);
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    (runner, bear, appa)
}

/// Pass priority until the stack is empty, returning every emitted event.
fn drain_stack_collecting_events(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    while !runner.state().stack.is_empty() {
        let result = runner
            .act(GameAction::PassPriority)
            .expect("priority pass must advance resolution");
        events.extend(result.events);
    }
    events
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

/// CR 117.4 + CR 603.3d: cast Appa through the real pipeline, let both players
/// pass so it resolves and enters, and stop at its ETB airbend's stack-time
/// target prompt — the production boundary the controller answers. Returns
/// that prompt's target slots for the caller to assert on.
fn resolve_appa_to_etb_target_prompt(
    runner: &mut GameRunner,
    appa: ObjectId,
) -> Vec<TargetSelectionSlot> {
    runner.cast(appa).commit();
    while runner.state().objects[&appa].zone == Zone::Stack {
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass must advance Appa's resolution");
    }
    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection {
            player,
            source_id,
            target_slots,
            ..
        } => {
            assert_eq!(*player, P0, "Appa's controller chooses the ETB targets");
            assert_eq!(
                *source_id,
                Some(appa),
                "the target prompt must belong to Appa's ETB"
            );
            target_slots.clone()
        }
        other => panic!("Appa's ETB must stop at its stack-time target prompt, got {other:?}"),
    }
}

/// SHAPE: Appa's ETB airbend is `MultiTargetSpec::unlimited(0)`, not
/// `ChangeZone.up_to`. Both triggers lower with zero Unimplemented.
#[test]
fn appa_etb_airbend_any_number_shape() {
    let parsed = parse_oracle_text(
        APPA_ORACLE,
        "Appa, Steadfast Guardian",
        &[],
        &["Creature".to_string()],
        &[],
    );
    assert_eq!(
        parsed.triggers.len(),
        2,
        "Appa must parse ETB + spell-cast triggers, got {:?}",
        parsed.triggers
    );

    let etb = parsed
        .triggers
        .iter()
        .find(|trigger| trigger.mode == TriggerMode::ChangesZone)
        .expect("Appa must parse an ETB trigger");
    let execute = etb
        .execute
        .as_deref()
        .expect("ETB trigger must carry an executed ability");
    assert_eq!(
        execute.multi_target,
        Some(MultiTargetSpec::unlimited(0)),
        "ETB airbend optionality lives on MultiTargetSpec"
    );
    match execute.effect.as_ref() {
        Effect::ChangeZone {
            origin,
            target: TargetFilter::Typed(tf),
            up_to,
            ..
        } => {
            assert_eq!(origin, &None);
            assert!(
                !*up_to,
                "optionality must not be encoded as ChangeZone.up_to"
            );
            assert!(
                tf.type_filters.contains(&TypeFilter::Permanent),
                "expected Permanent, got {:?}",
                tf.type_filters
            );
            assert!(
                tf.type_filters
                    .iter()
                    .any(|ty| matches!(ty, TypeFilter::Non(inner) if **inner == TypeFilter::Land)),
                "expected Non(Land), got {:?}",
                tf.type_filters
            );
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(
                tf.properties.contains(&FilterProp::Another),
                "expected Another, got {:?}",
                tf.properties
            );
        }
        other => panic!("expected ChangeZone with typed nonland permanent, got {other:?}"),
    }

    let spell_cast = parsed
        .triggers
        .iter()
        .find(|trigger| trigger.mode == TriggerMode::SpellCast)
        .expect("Appa must parse a SpellCast trigger");
    let spell_execute = spell_cast
        .execute
        .as_deref()
        .expect("SpellCast trigger must carry an executed ability");
    assert!(
        matches!(spell_execute.effect.as_ref(), Effect::Token { .. }),
        "second trigger must still be SpellCast→Token, got {:?}",
        spell_execute.effect
    );

    for trigger in &parsed.triggers {
        if let Some(execute) = trigger.execute.as_deref() {
            assert!(
                !ability_contains_unimplemented(execute),
                "both triggers must lower with zero Unimplemented, got {execute:#?}"
            );
        }
    }
}

/// CR 107.1c + CR 115.6 + CR 603.3d: "any number" includes zero. Appa's ETB
/// prompt has no required slot, the empty selection is accepted, and the
/// trigger still goes on the stack and resolves without exiling anything.
#[test]
fn appa_etb_airbend_zero_selected() {
    let (mut runner, land, mine_a, mine_b, theirs, appa) = appa_board();

    let slots = resolve_appa_to_etb_target_prompt(&mut runner, appa);
    // CR 115.3: one slot per distinct legal permanent (the two Bears).
    assert_eq!(
        slots.len(),
        2,
        "any-number airbend offers one slot per legal permanent, got {slots:?}"
    );
    assert!(
        slots.iter().all(|slot| slot.optional),
        "CR 107.1c: any-number targeting has a zero minimum — no slot may be required, \
         got {slots:?}"
    );

    runner
        .act(GameAction::SelectTargets { targets: vec![] })
        .expect("CR 115.6: the empty target selection must be accepted");
    let top = runner
        .state()
        .stack
        .back()
        .expect("Appa's ETB must be on the stack after its targets are chosen");
    assert!(
        matches!(
            &top.kind,
            StackEntryKind::TriggeredAbility { source_id, ability, .. }
                if *source_id == appa && ability.targets.is_empty()
        ),
        "Appa's ETB must go on the stack with zero targets, got {top:?}"
    );

    runner.advance_until_stack_empty();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "zero-target airbend must return to priority, got {:?}",
        runner.state().waiting_for
    );
    for (id, name) in [
        (mine_a, "mine_a"),
        (mine_b, "mine_b"),
        (land, "land"),
        (theirs, "theirs"),
        (appa, "appa"),
    ] {
        assert_eq!(
            runner.state().objects[&id].zone,
            Zone::Battlefield,
            "{name} must remain on the battlefield when zero targets are chosen"
        );
    }
}

/// CR 115.1d + CR 701.65a: the ETB's legal set is exactly the other nonland
/// permanents Appa's controller controls; airbending the chosen ones exiles
/// them and leaves everything else in place.
#[test]
fn appa_etb_airbend_multiple_selected() {
    let (mut runner, land, mine_a, mine_b, theirs, appa) = appa_board();

    let slots = resolve_appa_to_etb_target_prompt(&mut runner, appa);
    assert_eq!(
        slots.len(),
        2,
        "any-number airbend offers one slot per legal permanent, got {slots:?}"
    );
    for slot in &slots {
        for (id, name) in [(mine_a, "mine_a"), (mine_b, "mine_b")] {
            assert!(
                slot.legal_targets.contains(&TargetRef::Object(id)),
                "{name} (another nonland permanent you control) must be a legal target, \
                 got {:?}",
                slot.legal_targets
            );
        }
        for (id, reason) in [
            (land, "the land (nonland)"),
            (theirs, "the opponent's permanent (you control)"),
            (appa, "Appa itself (other)"),
        ] {
            assert!(
                !slot.legal_targets.contains(&TargetRef::Object(id)),
                "{reason} must not be a legal target, got {:?}",
                slot.legal_targets
            );
        }
    }

    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(mine_a), TargetRef::Object(mine_b)],
        })
        .expect("selecting both Bears must be accepted");
    runner.advance_until_stack_empty();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "multi-target airbend must return to priority, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().objects[&mine_a].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&mine_b].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&appa].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&land].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&theirs].zone, Zone::Battlefield);
}

/// CR 701.65b: an airbend triggers "whenever you airbend" abilities only when it
/// exiles one or more objects. Appa's ETB with zero targets reaches its bend
/// registration but exiles nothing, so no Airbend event is emitted, no airbend
/// is recorded this turn, and Avatar Aang does not draw.
#[test]
fn appa_zero_target_airbend_does_not_trigger_airbend_abilities() {
    let (mut runner, bear, appa) = appa_with_avatar_aang_board();

    resolve_appa_to_etb_target_prompt(&mut runner, appa);
    let hand_before = hand_size(&runner, P0);
    runner
        .act(GameAction::SelectTargets { targets: vec![] })
        .expect("CR 115.6: the empty target selection must be accepted");
    let events = drain_stack_collecting_events(&mut runner);

    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::RegisterBending,
                ..
            }
        )),
        "reach guard: the zero-target airbend must still resolve its bend registration, \
         got {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, GameEvent::Airbend { .. })),
        "CR 701.65b: an airbend that exiled nothing must not emit Airbend, got {events:?}"
    );
    assert!(
        !runner.state().players[P0.0 as usize]
            .bending_types_this_turn
            .contains(&BendingType::Air),
        "an airbend that exiled nothing must not count as airbending this turn"
    );
    assert_eq!(
        hand_size(&runner, P0),
        hand_before,
        "Avatar Aang's airbend trigger must not fire"
    );
    assert_eq!(runner.state().objects[&bear].zone, Zone::Battlefield);
}

/// CR 701.65b positive control: exiling one object is an airbend — the Airbend
/// event fires, the airbend is recorded this turn, and Avatar Aang draws.
#[test]
fn appa_airbend_exiling_one_object_triggers_airbend_abilities() {
    let (mut runner, bear, appa) = appa_with_avatar_aang_board();

    resolve_appa_to_etb_target_prompt(&mut runner, appa);
    let hand_before = hand_size(&runner, P0);
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(bear)],
        })
        .expect("selecting the Bear must be accepted");
    let events = drain_stack_collecting_events(&mut runner);

    assert_eq!(runner.state().objects[&bear].zone, Zone::Exile);
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::Airbend { controller, .. } if *controller == P0
        )),
        "CR 701.65b: exiling one object must emit Airbend, got {events:?}"
    );
    assert!(
        runner.state().players[P0.0 as usize]
            .bending_types_this_turn
            .contains(&BendingType::Air),
        "exiling one object must count as airbending this turn"
    );
    assert_eq!(
        hand_size(&runner, P0),
        hand_before + 1,
        "Avatar Aang's airbend trigger must draw a card"
    );
}
