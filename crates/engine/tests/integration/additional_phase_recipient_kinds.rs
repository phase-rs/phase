//! CR 500.10a + CR 115.1 + CR 608.2b: a player the text names as getting an
//! added phase is a real target when the text says "target player": the target
//! is chosen when the spell is cast, re-checked when it resolves, and the phase
//! goes only to that player's own turn. A player target lost before
//! resolution in the spell's first clause is not replaced by the spell's
//! controller or a surviving player from another instruction.
//!
//! No printed card says "target player gets an additional … phase"; the
//! grammar reaches it (charter C8c-a), so these rows use synthetic text.
//!
//! The Paradox Haze row runs the triggering-player kind through a real phase
//! trigger, whose event names the player who gets the step.

use engine::game::effects::attach::attach_to_player;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::static_abilities::player_has_hexproof;
use engine::game::trigger_index::reindex_object_triggers;
use engine::types::ability::{
    ControllerRef, Effect, EffectKind, StaticDefinition, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, LayersDirty, StackEntryKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::phase::{Phase, TurnSegment};
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const TARGET_PLAYER_COMBAT: &str =
    "Target player gets an additional combat phase after this phase.";

const TARGET_PLAYER_COMBAT_AND_PUMP: &str = "Target player gets an additional combat phase after this phase. Target creature gets +1/+1 until end of turn.";

const EACH_PLAYER_COMBAT: &str = "Each player gets an additional combat phase after this phase.";

const PARADOX_HAZE: &str = "Enchant player\nAt the beginning of enchanted player's first upkeep each turn, that player gets an additional upkeep step after this step.";

fn resolved_additional_phase(events: &[GameEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::AdditionalPhase,
                ..
            }
        )
    })
}

/// "You have hexproof" (the Leyline of Sanctity shape), on a permanent `player`
/// controls. A new continuous-effect source needs a layer pass to be seen,
/// which an ETB would have requested, so the pass is marked `Full`.
fn grant_hexproof(state: &mut GameState, player: PlayerId) {
    let grantor = engine::game::zones::create_object(
        state,
        CardId(951),
        player,
        "You Have Hexproof Source".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&grantor)
        .expect("the grantor was just created")
        .static_definitions =
        vec![
            StaticDefinition::new(StaticMode::Hexproof).affected(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
        ]
        .into();
    state.layers_dirty = LayersDirty::Full;
    engine::game::layers::flush_layers(state);
}

/// P0's precombat main phase, with an instant `text` in `caster`'s hand and a
/// P0 creature. When P1 casts, P0 passes first so P1 holds priority.
fn board(caster: PlayerId, text: &str) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_creature(P0, "Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(caster, "Recipient Probe", true, text)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    if caster == P1 {
        runner.act(GameAction::PassPriority).expect("P0 passes");
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
            "P1 has priority in P0's precombat main phase"
        );
    }
    (runner, spell, creature)
}

/// CR 115.1 + CR 601.2c + CR 500.10a: "target player gets" declares a player
/// target when the spell is cast, and the phase goes to that player's turn
/// only. Each row is cast on P0's turn; the targeted player is on the stack
/// before resolution (the cast-time slot), and the effect resolves in every
/// row (reach guard) before the discriminating count.
#[test]
fn targeted_player_is_declared_at_cast_and_gets_the_phase_on_their_own_turn() {
    for (caster, target, added) in [(P0, P0, 1), (P1, P0, 1), (P0, P1, 0), (P1, P1, 0)] {
        let (mut runner, spell, _) = board(caster, TARGET_PLAYER_COMBAT);
        let committed = runner.cast(spell).target_player(target).commit();
        let declared = committed
            .state()
            .stack
            .last()
            .and_then(|entry| entry.ability())
            .map(|ability| ability.targets.clone());
        assert_eq!(
            declared,
            Some(vec![TargetRef::Player(target)]),
            "caster {caster:?}: the player target is declared at cast time"
        );
        let outcome = committed.resolve();
        assert!(
            resolved_additional_phase(outcome.events()),
            "caster {caster:?}, target {target:?}: reach guard, the effect resolved"
        );
        assert_eq!(
            outcome.state().extra_phases.len(),
            added,
            "caster {caster:?}, target {target:?} on P0's turn"
        );
    }
}

/// CR 608.2b: the only target became illegal (P0 gained hexproof after P1
/// targeted it), so the spell doesn't resolve and nothing is added. Reach
/// guards: P0 has hexproof at resolution, and the spell left the stack. The
/// paired positive is the (P1, P0) row above.
#[test]
fn targeted_player_is_rechecked_at_resolution() {
    let (mut runner, spell, _) = board(P1, TARGET_PLAYER_COMBAT);
    let mut committed = runner.cast(spell).target_player(P0).commit();
    grant_hexproof(committed.state_mut(), P0);
    assert!(player_has_hexproof(committed.state(), P0));
    let outcome = committed.resolve();
    assert!(outcome.state().stack.is_empty(), "the spell left the stack");
    assert_eq!(outcome.zone_of(spell), Zone::Graveyard);
    assert!(!resolved_additional_phase(outcome.events()));
    assert!(outcome.state().extra_phases.is_empty());
}

/// CR 608.2b + CR 500.10a: with two targets, losing the player target leaves
/// the spell resolving for its creature target, and the grant that needed the
/// player adds nothing, even though the spell's controller (P0) is the active
/// player. Reach guards: P1 has hexproof at resolution, the creature got
/// +1/+1, and the additional-phase effect resolved. Paired positive: the same
/// spell targeting P0 adds the phase.
#[test]
fn a_lost_player_target_is_not_replaced_by_the_controller() {
    for (target, hexproof, added) in [(P0, false, 1), (P1, true, 0)] {
        let (mut runner, spell, creature) = board(P0, TARGET_PLAYER_COMBAT_AND_PUMP);
        let mut committed = runner
            .cast(spell)
            .target_player(target)
            .target_object(creature)
            .commit();
        if hexproof {
            grant_hexproof(committed.state_mut(), target);
            assert!(player_has_hexproof(committed.state(), target));
        }
        let outcome = committed.resolve();
        assert_eq!(
            outcome.state().objects[&creature].power,
            Some(3),
            "target {target:?}: the creature target was pumped"
        );
        assert!(
            resolved_additional_phase(outcome.events()),
            "target {target:?}: reach guard, the effect resolved"
        );
        assert_eq!(
            outcome.state().extra_phases.len(),
            added,
            "target {target:?}, hexproof {hexproof}"
        );
    }
}

/// CR 115.1 + CR 608.2b + CR 500.10a: identical player filters announce
/// distinct slots. The grant uses its own surviving target, never a preceding
/// life instruction's target. The text is synthetic grammar coverage.
#[test]
fn a_later_player_grant_preserves_its_own_slot_and_illegal_targets_stay_empty() {
    const TEXT: &str = "Target player gains 3 life. Target player gets an additional combat phase after this phase.";
    for (caster, earlier, recipient, make_illegal, added) in [
        (P0, P0, P0, false, 1),
        (P0, P1, P0, false, 1),
        (P0, P0, P1, false, 0),
        (P0, P0, P1, true, 0),
        (P1, P1, P0, false, 1),
        (P1, P1, P0, true, 0),
    ] {
        let (mut runner, spell, _) = board(caster, TEXT);
        let mut committed = runner
            .cast(spell)
            .target_players(&[earlier, recipient])
            .commit();
        let root = committed
            .state()
            .stack
            .last()
            .and_then(|entry| entry.ability())
            .unwrap();
        assert!(
            std::iter::successors(Some(root), |node| node.sub_ability.as_deref())
                .all(|node| !matches!(node.effect, Effect::Unimplemented { .. })),
            "reach guard: no instruction is unsupported"
        );
        assert!(matches!(root.effect, Effect::GainLife { .. }));
        assert_eq!(root.targets, vec![TargetRef::Player(earlier)]);
        let grant = root.sub_ability.as_deref().unwrap();
        assert!(matches!(grant.effect, Effect::AdditionalPhase { .. }));
        assert_eq!(grant.targets, vec![TargetRef::Player(recipient)]);
        assert!(
            grant.sub_ability.is_none(),
            "both clauses reached their supported effects"
        );
        if make_illegal {
            grant_hexproof(committed.state_mut(), recipient);
            assert!(player_has_hexproof(committed.state(), recipient));
        }
        let outcome = committed.resolve();
        outcome.assert_life_delta(earlier, 3);
        assert!(
            resolved_additional_phase(outcome.events()),
            "the legal earlier target keeps the chain resolving"
        );
        assert_eq!(outcome.state().extra_phases.len(), added,
            "caster {caster:?}, earlier {earlier:?}, recipient {recipient:?}, illegal {make_illegal}");
    }
}

/// CR 608.2b: legality is checked before instructions resolve, so hexproof
/// gained from an earlier instruction does not invalidate the later grant.
#[test]
fn a_player_becoming_hexproof_mid_resolution_keeps_the_announced_phase_grant() {
    const TEXT: &str = "Return target creature card from a graveyard to the battlefield. Target player gets an additional combat phase after this phase.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let grantor = scenario
        .add_creature_to_graveyard(P0, "Hexproof Grantor", 2, 2)
        .with_static_definition(StaticDefinition::new(StaticMode::Hexproof).affected(
            TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::SpecificPlayer { id: P0 }),
            ),
        ))
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Mid-resolution Recipient Probe", true, TEXT)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    assert!(!player_has_hexproof(runner.state(), P0));
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority to the caster");
    let committed = runner
        .cast(spell)
        .target_object(grantor)
        .target_player(P0)
        .commit();
    let root = committed
        .state()
        .stack
        .last()
        .and_then(|entry| entry.ability())
        .unwrap();
    assert!(
        std::iter::successors(Some(root), |node| node.sub_ability.as_deref())
            .all(|node| !matches!(node.effect, Effect::Unimplemented { .. }))
    );
    assert_eq!(root.targets, vec![TargetRef::Object(grantor)]);
    assert_eq!(
        root.sub_ability.as_deref().unwrap().targets,
        vec![TargetRef::Player(P0)]
    );
    assert!(matches!(root.effect, Effect::ChangeZone { .. }));
    assert!(matches!(
        root.sub_ability.as_deref().unwrap().effect,
        Effect::AdditionalPhase { .. }
    ));
    let outcome = committed.resolve();
    outcome.assert_zone(&[grantor], Zone::Battlefield);
    assert!(
        player_has_hexproof(outcome.state(), P0),
        "the preceding instruction granted hexproof"
    );
    assert!(resolved_additional_phase(outcome.events()));
    assert_eq!(outcome.state().extra_phases.len(), 1);
}

/// CR 608.2b: a child player target pruned before an interactive Scry pause
/// remains empty after its continuation resumes; the legal earlier target still
/// resolves. The positive row proves an unstamped child still receives its own
/// announced recipient.
#[test]
fn scry_parked_continuation_preserves_pruned_child_target_evidence() {
    const TEXT: &str =
        "Target player gains 3 life. Scry 1. Target player gets an additional combat phase after this phase.";

    for (hexproof, expected_phases) in [(false, 1), (true, 0)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_card_to_library_top(P1, "Plains");
        let spell = scenario
            .add_spell_to_hand_from_oracle(P1, "Scry Recipient Probe", true, TEXT)
            .with_mana_cost(ManaCost::generic(0))
            .id();
        let mut runner = scenario.build();
        runner.act(GameAction::PassPriority).expect("P0 passes");
        let mut committed = runner.cast(spell).target_players(&[P1, P0]).commit();
        if hexproof {
            grant_hexproof(committed.state_mut(), P0);
            assert!(player_has_hexproof(committed.state(), P0));
        }

        let mut events = committed
            .act(GameAction::PassPriority)
            .expect("P1 passes")
            .events;
        events.extend(
            committed
                .act(GameAction::PassPriority)
                .expect("P0 resolves the spell")
                .events,
        );
        let cards = match &committed.state().waiting_for {
            WaitingFor::ScryChoice { cards, .. } => cards.clone(),
            other => panic!("expected ScryChoice, got {other:?}"),
        };
        assert!(
            committed
                .state()
                .active_ability_continuation()
                .is_some_and(|continuation| {
                    matches!(&continuation.chain.effect, Effect::AdditionalPhase { .. })
                        && (hexproof == !continuation.chain.illegal_local_target_slots.is_empty())
                }),
            "the parked child preserves its validation evidence"
        );
        events.extend(
            committed
                .act(GameAction::SelectCards { cards })
                .expect("answer Scry through apply")
                .events,
        );

        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::LifeChanged { player_id, amount, .. }
                    if *player_id == P1 && *amount == 3
            )),
            "reach guard: the earlier legal instruction resolved"
        );
        assert!(resolved_additional_phase(&events));
        assert_eq!(committed.state().extra_phases.len(), expected_phases);
    }
}

/// CR 500.10a: "each player gets" grants the phase to each player in turn,
/// and each grant is gated on that player's own turn. On P0's and P1's turns
/// exactly one combat is added. Reach guard: the effect resolves once per player.
#[test]
fn each_player_gets_adds_only_the_active_players_phase() {
    for active in [P0, P1] {
        let (mut runner, spell, _) = board(P0, EACH_PLAYER_COMBAT);
        runner.state_mut().active_player = active;
        let outcome = runner.cast(spell).resolve();
        assert_eq!(outcome.state().active_player, active);
        assert_eq!(
            outcome
                .events()
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: EffectKind::AdditionalPhase,
                        ..
                    }
                ))
                .count(),
            2,
            "reach guard: the grant resolves for each player on {active:?}'s turn"
        );
        assert_eq!(outcome.state().extra_phases.len(), 1);
    }
}

/// CR 500.10a + CR 603.2: Paradox Haze's "that player gets" names the player
/// its phase trigger's event names. P0 controls the Aura and P1 is enchanted,
/// so the controller is not that player: on P1's upkeep the trigger resolves
/// through the stack with its event, and the upkeep step is added to P1's
/// turn. Reach guards: the trigger went on the stack, and the effect resolved.
#[test]
fn paradox_haze_adds_the_upkeep_step_to_the_enchanted_players_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let haze = {
        let mut builder = scenario.add_enchantment_from_oracle(P0, "Paradox Haze", PARADOX_HAZE);
        builder.with_subtypes(vec!["Aura"]);
        builder.id()
    };
    for _ in 0..5 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    attach_to_player(runner.state_mut(), haze, P1);
    reindex_object_triggers(runner.state_mut(), haze);

    runner.advance_to_upkeep();
    assert!(
        runner.state().stack.iter().any(|entry| matches!(
            &entry.kind,
            StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == haze
        )),
        "reach guard: Paradox Haze triggered at P1's upkeep"
    );
    let mut events = Vec::new();
    while !runner.state().stack.is_empty() {
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("each player passes")
                .events,
        );
    }
    assert!(
        resolved_additional_phase(&events),
        "reach guard: the effect resolved"
    );
    assert_eq!(runner.state().active_player, P1);
    assert_eq!(
        runner
            .state()
            .extra_phases
            .iter()
            .map(|phase| phase.segment)
            .collect::<Vec<_>>(),
        vec![TurnSegment::Step(Phase::Upkeep)],
        "P1 gets the added upkeep step"
    );
}
