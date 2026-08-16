//! MSH Wave 6: Willie Lumpkin, Postman — "Whenever Willie Lumpkin deals combat
//! damage to an opponent, you draw a card and that player may draw a card. If
//! they do, that player can't attack you or permanents you control during their
//! next turn."
//!
//! The trailing "that player can't attack you or permanents you control during
//! their next turn" was a parser `Effect::Unimplemented { name: "can't" }` leaf.
//! This suite locks in:
//!   1. PARSE: the trigger sub-chain leaf lowers to
//!      `Effect::AddRestriction { ProhibitActivity { Attack { PlayerOrPermanents },
//!      ParentTargetedPlayer } }`, NOT `Unimplemented`.
//!   2. RUNTIME: the new player-scoped declare-attackers gate rejects the
//!      restricted player's attacks against the protected player, their
//!      planeswalker, AND their battle (the `PlayerOrPermanents` Battle arm),
//!      while a third (unrestricted) player may attack freely.
//!   3. EXPIRY: the restriction anchors on the RESTRICTED player's next turn.
//!
//! CR 508.1c: attack restrictions checked at declare-attackers. CR 508.5: the
//! defended planeswalker/battle compares on controller. CR 310.5: battles are
//! attackable permanents (the `PlayerOrPermanents` distinction from
//! `PlayerOrPlaneswalker`). CR 514.2 + CR 500.7: next-turn expiry.

use engine::game::combat::{declare_attackers, AttackTarget};
use engine::game::effects::add_restriction;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::triggers::process_triggers;
use engine::game::zones::{create_object, move_to_zone};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, Duration, Effect, GameRestriction, PlayerScope, ProhibitedActivity,
    ResolvedAbility, RestrictionExpiry, RestrictionPlayerScope, TargetRef,
};
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::triggers::AttackTargetFilter;
use engine::types::zones::Zone;

const WILLIE_ORACLE: &str = "Willie Lumpkin can't be blocked.\nWhenever Willie Lumpkin deals combat damage to an opponent, you draw a card and that player may draw a card. If they do, that player can't attack you or permanents you control during their next turn.";
const PLANESWALKER_ONLY_WILLIE_ORACLE: &str = "Whenever this creature deals combat damage to an opponent, you draw a card and that player may draw a card. If they do, that player can't attack planeswalkers you control during their next turn.";

const PROTECTED: PlayerId = PlayerId(0); // Willie's controller ("you")
const RESTRICTED: PlayerId = PlayerId(1); // the opponent dealt damage
const THIRD: PlayerId = PlayerId(2); // an unrestricted third player

/// Walk the trigger sub-chain to the deepest sub_ability (the "that player can't
/// attack ..." leaf).
fn deepest_sub_effect(mut ability: &AbilityDefinition) -> &Effect {
    while let Some(sub) = ability.sub_ability.as_deref() {
        ability = sub;
    }
    &ability.effect
}

#[test]
fn willie_cant_attack_clause_parses_to_player_scoped_prohibition() {
    let parsed = parse_oracle_text(
        WILLIE_ORACLE,
        "Willie Lumpkin, Postman",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Citizen".to_string()],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find_map(|t| t.execute.as_deref())
        .expect("Willie has a DamageDone trigger with an execute chain");

    let leaf = deepest_sub_effect(trigger);
    match leaf {
        Effect::AddRestriction {
            restriction:
                GameRestriction::ProhibitActivity {
                    affected_players,
                    activity: ProhibitedActivity::Attack { defended, .. },
                    ..
                },
        } => {
            // REVERT-FAIL: reverting the Willie parser leaves `Effect::Unimplemented`.
            assert_eq!(
                *defended,
                AttackTargetFilter::PlayerOrPermanents,
                "Willie defends 'you or permanents you control' (battles included)"
            );
            // NEGATIVE: must NOT be the planeswalker-only scope (proves the
            // permanents-vs-planeswalkers distinction).
            assert_ne!(*defended, AttackTargetFilter::PlayerOrPlaneswalker);
            assert_eq!(
                *affected_players,
                RestrictionPlayerScope::ParentTargetedPlayer,
                "'that player' binds to the parent draw-trigger target"
            );
        }
        other => panic!("expected AddRestriction(Attack), got {other:?}"),
    }
}

/// Build a 3-player board with Willie's restriction in force against RESTRICTED,
/// defended scope `PlayerOrPermanents`, and a creature each of RESTRICTED and
/// THIRD control. Returns the state with active_player = RESTRICTED.
fn board_with_restriction() -> (GameState, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut state = GameState::new(FormatConfig::standard(), 3, 42);
    state.active_player = RESTRICTED;
    state.turn_number = 2;

    // Willie (the protected player's permanent — the restriction source).
    let willie_card = CardId(state.next_object_id);
    let willie = create_object(
        &mut state,
        willie_card,
        PROTECTED,
        "Willie Lumpkin, Postman".to_string(),
        Zone::Battlefield,
    );

    // RESTRICTED's attacker.
    let restricted_attacker = make_creature(&mut state, RESTRICTED, "Restricted Bear");
    // THIRD's attacker.
    let third_attacker = make_creature(&mut state, THIRD, "Third Bear");

    // PROTECTED's planeswalker (a defended permanent).
    let pw_card = CardId(state.next_object_id);
    let pw = create_object(
        &mut state,
        pw_card,
        PROTECTED,
        "Jace".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&pw)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Planeswalker);

    // Install the restriction via the real resolver so affected_players and the
    // expiry anchor are lowered exactly as in production.
    let ability = ResolvedAbility::new(
        Effect::AddRestriction {
            restriction: GameRestriction::ProhibitActivity {
                source: ObjectId(0),
                affected_players: RestrictionPlayerScope::ParentTargetedPlayer,
                expiry: RestrictionExpiry::EndOfTurn,
                activity: ProhibitedActivity::Attack {
                    defended: AttackTargetFilter::PlayerOrPermanents,
                    protected_player: None,
                },
            },
        },
        vec![TargetRef::Player(RESTRICTED)],
        willie,
        PROTECTED,
    )
    .duration(Duration::UntilEndOfNextTurnOf {
        player: PlayerScope::Controller,
    });
    let mut events = Vec::new();
    add_restriction::resolve(&mut state, &ability, &mut events).unwrap();

    (state, restricted_attacker, third_attacker, pw, willie)
}

fn make_creature(state: &mut GameState, controller: PlayerId, name: &str) -> ObjectId {
    let card = CardId(state.next_object_id);
    let id = create_object(state, card, controller, name.to_string(), Zone::Battlefield);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Creature];
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(2);
    obj.toughness = Some(2);
    obj.base_power = Some(2);
    obj.base_toughness = Some(2);
    id
}

#[test]
fn willie_restricted_player_cannot_attack_protected_third_player_can() {
    let (state, restricted_attacker, third_attacker, _pw, _willie) = board_with_restriction();

    // REVERT-FAIL (combat gate): the restricted player's attack on PROTECTED is rejected.
    let mut s = state.clone();
    let mut events = Vec::new();
    assert!(
        declare_attackers(
            &mut s,
            &[(restricted_attacker, AttackTarget::Player(PROTECTED))],
            &mut events,
        )
        .is_err(),
        "CR 508.1c: restricted player can't attack the protected player"
    );

    // SIBLING: a THIRD (unrestricted) player attacking PROTECTED is allowed.
    let mut s = state.clone();
    s.active_player = THIRD;
    let mut events = Vec::new();
    assert!(
        declare_attackers(
            &mut s,
            &[(third_attacker, AttackTarget::Player(PROTECTED))],
            &mut events,
        )
        .is_ok(),
        "CR 101.2: only the restricted player is prohibited; THIRD may attack"
    );
}

#[test]
fn willie_restricted_player_cannot_attack_protected_planeswalker_or_battle() {
    let (mut state, restricted_attacker, _third, pw, _willie) = board_with_restriction();

    // Planeswalker arm.
    let mut s = state.clone();
    let mut events = Vec::new();
    assert!(
        declare_attackers(
            &mut s,
            &[(restricted_attacker, AttackTarget::Planeswalker(pw))],
            &mut events,
        )
        .is_err(),
        "CR 508.5: restricted player can't attack the protected player's planeswalker"
    );

    // Battle arm — the PlayerOrPermanents distinctive case (CR 310.5).
    let battle_card = CardId(state.next_object_id);
    let battle = create_object(
        &mut state,
        battle_card,
        THIRD, // a battle the restricted player is NOT the protector of
        "Invasion Battle".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&battle).unwrap();
        obj.card_types.core_types = vec![CoreType::Battle];
        obj.base_card_types = obj.card_types.clone();
        // CR 508.5: the matcher compares the battle's CONTROLLER against the
        // protected player, so set the battle's controller to PROTECTED.
        obj.controller = PROTECTED;
    }
    let mut events = Vec::new();
    // REVERT-FAIL (Battle matcher arm): removing the (PlayerOrPermanents, Battle)
    // arm lets this attack through.
    assert!(
        declare_attackers(
            &mut state,
            &[(restricted_attacker, AttackTarget::Battle(battle))],
            &mut events,
        )
        .is_err(),
        "CR 310.5 + CR 508.5: 'permanents you control' defends the protected player's battle"
    );
}

#[test]
fn willie_expiry_anchors_on_restricted_player_not_controller() {
    let (state, _ra, _ta, _pw, _willie) = board_with_restriction();
    // REVERT-FAIL (Step 5 expiry arm): the round-1 bug anchored on ability.controller
    // (PROTECTED). The fix anchors on the resolved SpecificPlayer (RESTRICTED).
    let restriction = state
        .restrictions
        .iter()
        .find(|r| {
            matches!(
                r,
                GameRestriction::ProhibitActivity {
                    activity: ProhibitedActivity::Attack { .. },
                    ..
                }
            )
        })
        .expect("the Attack prohibition is in state.restrictions");
    match restriction {
        GameRestriction::ProhibitActivity {
            affected_players,
            expiry,
            ..
        } => {
            assert_eq!(
                *affected_players,
                RestrictionPlayerScope::SpecificPlayer(RESTRICTED),
                "affected player resolves to the restricted opponent"
            );
            assert_eq!(
                *expiry,
                RestrictionExpiry::UntilEndOfNextTurnOf { player: RESTRICTED },
                "CR 514.2: expiry anchors on the RESTRICTED player's next turn, not the controller"
            );
        }
        _ => unreachable!(),
    }
}

/// CR 603.3 + CR 608.2c: the legacy anaphor route resolves through the normal
/// trigger/action pipeline and snapshots the damaged player, protected player,
/// and that player's next-turn expiry. Unlike the real Willie card's broader
/// permanent scope, this focused grammar fixture makes its planeswalker-only
/// target shape observable at runtime.
#[test]
fn legacy_willie_planeswalker_only_restriction_snapshots_selected_player() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let source = scenario
        .add_creature_from_oracle(
            PROTECTED,
            "Legacy Willie Fixture",
            1,
            1,
            PLANESWALKER_ONLY_WILLIE_ORACLE,
        )
        .id();
    let restricted_attacker = scenario
        .add_creature(RESTRICTED, "Restricted Bear", 2, 2)
        .id();
    let protected_walker = scenario
        .add_creature(PROTECTED, "Protected Jace", 0, 0)
        .as_planeswalker_with_loyalty("Jace", 4)
        .id();
    let other_walker = scenario
        .add_creature(THIRD, "Other Chandra", 0, 0)
        .as_planeswalker_with_loyalty("Chandra", 4)
        .id();
    scenario.add_card_to_library_top(PROTECTED, "P0 draw fixture");
    scenario.add_card_to_library_top(RESTRICTED, "P1 draw fixture");
    let mut runner = scenario.build();
    process_triggers(
        runner.state_mut(),
        &[engine::types::events::GameEvent::DamageDealt {
            source_id: source,
            target: TargetRef::Player(RESTRICTED),
            amount: 1,
            is_combat: true,
            excess: 0,
        }],
    );
    for _ in 0..24 {
        match runner.state().waiting_for.clone() {
            engine::types::game_state::WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(engine::types::actions::GameAction::DecideOptionalEffect { accept: true })
                    .expect("the damaged player accepts the legacy draw offer");
            }
            engine::types::game_state::WaitingFor::Priority { .. }
                if runner.state().stack.is_empty() =>
            {
                break
            }
            engine::types::game_state::WaitingFor::Priority { .. } => {
                runner
                    .act(engine::types::actions::GameAction::PassPriority)
                    .expect("priority resolves the trigger");
            }
            other => panic!("unexpected legacy Willie prompt: {other:?}"),
        }
    }

    assert!(matches!(
        runner.state().restrictions.as_slice(),
        [GameRestriction::ProhibitActivity {
            source: stored_source,
            affected_players: RestrictionPlayerScope::SpecificPlayer(RESTRICTED),
            expiry: RestrictionExpiry::UntilEndOfNextTurnOf { player: RESTRICTED },
            activity: ProhibitedActivity::Attack {
                defended: AttackTargetFilter::Planeswalker,
                protected_player: Some(PROTECTED),
            },
        }] if *stored_source == source
    ));
    assert!(!attack_legal_from_runner(
        &runner,
        RESTRICTED,
        restricted_attacker,
        AttackTarget::Planeswalker(protected_walker)
    ));
    assert!(attack_legal_from_runner(
        &runner,
        RESTRICTED,
        restricted_attacker,
        AttackTarget::Player(PROTECTED)
    ));
    assert!(attack_legal_from_runner(
        &runner,
        RESTRICTED,
        restricted_attacker,
        AttackTarget::Planeswalker(other_walker)
    ));

    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .unwrap()
        .controller = THIRD;
    move_to_zone(runner.state_mut(), source, Zone::Graveyard, &mut Vec::new());
    assert!(
        !attack_legal_from_runner(
            &runner,
            RESTRICTED,
            restricted_attacker,
            AttackTarget::Planeswalker(protected_walker)
        ),
        "source change/removal cannot mutate the resolved legacy snapshot"
    );
}

fn attack_legal_from_runner(
    runner: &GameRunner,
    player: PlayerId,
    attacker: ObjectId,
    target: AttackTarget,
) -> bool {
    let mut state = runner.state().clone();
    state.active_player = player;
    let mut events = Vec::new();
    declare_attackers(&mut state, &[(attacker, target)], &mut events).is_ok()
}

/// MANDATORY no-over-application regression: an UNSCOPED `CantAttack` static
/// (`attack_defended: None` — Propaganda/Pacifism family) must still reject the
/// creature from attacking ANY target. This guards the `attack_defended.is_none()`
/// fast path (combat.rs) against accidental narrowing by the scoped paths added
/// for Willie/Promise. CR 508.1c.
#[test]
fn unscoped_cant_attack_still_rejects_all_targets() {
    use engine::game::layers::evaluate_layers;
    use engine::types::ability::{ContinuousModification, StaticDefinition, TargetFilter};
    use engine::types::statics::StaticMode;

    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    state.active_player = RESTRICTED;
    state.turn_number = 2;

    let attacker = make_creature(&mut state, RESTRICTED, "Pacified Bear");
    // Intrinsic unscoped CantAttack (attack_defended: None) on the creature.
    {
        let def = StaticDefinition::new(StaticMode::CantAttack)
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::CantAttack,
            }]);
        let obj = state.objects.get_mut(&attacker).unwrap();
        obj.static_definitions = vec![def.clone()].into();
        obj.base_static_definitions = std::sync::Arc::new(vec![def]);
    }
    evaluate_layers(&mut state);

    // A potential defender player + their planeswalker.
    let pw_card = CardId(state.next_object_id);
    let pw = create_object(
        &mut state,
        pw_card,
        PROTECTED,
        "Jace".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&pw)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Planeswalker);

    for target in [
        AttackTarget::Player(PROTECTED),
        AttackTarget::Planeswalker(pw),
    ] {
        let mut s = state.clone();
        let mut events = Vec::new();
        assert!(
            declare_attackers(&mut s, &[(attacker, target)], &mut events).is_err(),
            "CR 508.1c: an unscoped CantAttack must reject attacking {target:?}"
        );
    }
}
