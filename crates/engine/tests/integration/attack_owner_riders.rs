//! CR 108.3 + CR 508.1d + CR 611.2: the attack-owner rider class, end to end.
//!
//! "Each of them attacks its owner" (My Crushing Masterstroke) and "Each of
//! those creatures attacks its owner each combat if able" (The Dead Shall
//! Serve) lower to `ForceAttack` with the per-member `AffectedObjectOwner`
//! anchor. These drive the production pipeline — verbatim Oracle text →
//! parser → set in motion → resolution (which publishes the tracked set the
//! rider reads) → the `DeclareAttackers` payload authority and legality
//! validator — with creatures owned by different opponents, so a
//! single-defender snapshot cannot pass.
//!
//! Rows, both in 3-player Archenemy (P0 archenemy, P1/P2 heroes):
//! * Dead Shall Serve: P1-owned and P2-owned creatures reanimated under P0's
//!   control each graft `MustAttackDefender` pinned to their own owner with
//!   `Permanent` duration; each must attack its owner at P0's combat
//!   (cross-declaration and a full pass are refused).
//! * Masterstroke: the heroes' creatures seized until end of turn each graft
//!   the owner-pinned requirement with `UntilEndOfTurn`; enforced at P0's
//!   combat, then expired at cleanup (grafts gone, control reverted).
//!
//! Revert-to-red: pin the graft defender to a single player instead of each
//! member's owner — the cross-owner assertions fail. Starve the tracked-set
//! publication — the Dead Shall Serve grafts never install.

use engine::database::synthesis::synthesize_archenemy;
use engine::game::combat::AttackTarget;
use engine::game::deck_loading::create_scheme_deck_card;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::parser::parse_oracle_text;
use engine::types::ability::{ContinuousModification, Duration, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{RequiredDefender, StaticMode};
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const THE_DEAD_SHALL_SERVE: &str = "When you set this scheme in motion, for each opponent, put up to one target creature card from that player's graveyard onto the battlefield under your control. Each of those creatures attacks its owner each combat if able.";
const MY_CRUSHING_MASTERSTROKE: &str = "When you set this scheme in motion, gain control of all nonland permanents your opponents control until end of turn. Untap those permanents. They gain haste until end of turn. Each of them attacks its owner this turn if able.";

fn scheme_face(name: &str, oracle: &str) -> CardFace {
    let parsed = parse_oracle_text(oracle, name, &[], &["Scheme".to_string()], &[]);
    assert_eq!(
        parsed.triggers.len(),
        1,
        "{name} must parse to exactly one trigger"
    );
    assert!(
        !format!("{:?}", parsed.triggers).contains("Unimplemented"),
        "{name}: trigger body is gapped"
    );
    let mut face = CardFace {
        name: name.to_string(),
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        ..Default::default()
    };
    face.card_type.core_types.push(CoreType::Scheme);
    synthesize_archenemy(&mut face);
    face
}

/// Read back one member's installed rider: the required-defender player and
/// the graft window. Panics unless the member carries exactly the
/// per-object `MustAttackDefender` graft the resolver installs.
fn owner_graft(state: &GameState, member: ObjectId) -> (PlayerId, Duration) {
    let effect = state
        .transient_continuous_effects
        .iter()
        .find(|ce| {
            ce.affected == TargetFilter::SpecificObject { id: member }
                && ce.modifications.iter().any(|m| {
                    matches!(
                        m,
                        ContinuousModification::AddStaticMode {
                            mode: StaticMode::MustAttackDefender { .. },
                        }
                    )
                })
        })
        .unwrap_or_else(|| panic!("member {member:?} gets its own graft"));
    let defender = effect
        .modifications
        .iter()
        .find_map(|m| match m {
            ContinuousModification::AddStaticMode {
                mode: StaticMode::MustAttackDefender { defender },
            } => Some(defender.clone()),
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "member {member:?} graft must be MustAttackDefender, got {:?}",
                effect.modifications
            )
        });
    let RequiredDefender::Fixed { player } = defender else {
        panic!("member {member:?} defender must be its owner player, got {defender:?}");
    };
    (player, effect.duration.clone())
}

fn declare(runner: &mut GameRunner, attacks: Vec<(ObjectId, AttackTarget)>) -> Result<(), String> {
    runner
        .act(GameAction::DeclareAttackers {
            attacks,
            bands: vec![],
        })
        .map(|_| ())
        .map_err(|e| format!("{e:?}"))
}

/// Declare no blocks when the engine opens the declare-blockers step; a
/// side with nothing to block with may skip the prompt entirely.
fn maybe_declare_no_blocks(runner: &mut GameRunner) {
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner
            .act(GameAction::DeclareBlockers {
                assignments: vec![],
            })
            .expect("no blocks must be accepted");
    }
}

/// Answer the current target slot with a graveyard creature card when one is
/// legal, else decline ("up to one").
fn answer_graveyard_slot(runner: &mut GameRunner) {
    let (slot, has_slot) = match runner.state().waiting_for.clone() {
        WaitingFor::TriggerTargetSelection {
            target_slots,
            selection,
            ..
        }
        | WaitingFor::TargetSelection {
            target_slots,
            selection,
            ..
        } => (target_slots[selection.current_slot].clone(), true),
        _ => panic!("expected a target selection prompt"),
    };
    assert!(has_slot);
    let pick = slot.legal_targets.iter().find_map(|target| match target {
        TargetRef::Object(id)
            if runner.state().objects.get(id).is_some_and(|obj| {
                obj.zone == Zone::Graveyard
                    && obj.card_types.core_types.contains(&CoreType::Creature)
            }) =>
        {
            Some(TargetRef::Object(*id))
        }
        _ => None,
    });
    runner
        .act(GameAction::ChooseTarget { target: pick })
        .expect("answering the graveyard slot must be accepted");
}

/// CR 108.3 + CR 508.1d + CR 611.2: The Dead Shall Serve, production path.
/// "For each opponent" fans out into one single-target iteration per
/// opponent: P1's and P2's creatures reanimate under P0's control, each
/// grafting `MustAttackDefender` pinned to their OWN owner (not one
/// snapshot player) with `Permanent` duration; at P0's combat each must
/// attack its owner.
#[test]
fn dead_shall_serve_reanimated_creatures_attack_their_owners() {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 3, 42);
    scenario.at_phase(Phase::Draw);
    let owned_by_p1 = scenario
        .add_creature_to_graveyard(P1, "Fallen Hero", 2, 2)
        .id();
    let owned_by_p2 = scenario
        .add_creature_to_graveyard(P2, "Fallen Villain", 3, 3)
        .id();
    for i in 0..7 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
        scenario.add_card_to_library_top(P2, &format!("P2 Library {i}"));
    }
    let mut runner = scenario.build();

    let face = scheme_face("The Dead Shall Serve", THE_DEAD_SHALL_SERVE);
    create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    // CR 904.9: entering the archenemy's precombat main sets the scheme in
    // motion; the trigger announces one optional graveyard target.
    runner.advance_to_phase(Phase::PreCombatMain);
    assert!(
        runner.state().scheme_deck.is_empty(),
        "the scheme must have left the scheme deck"
    );
    // "For each opponent" fans out into one single-target iteration per
    // opponent, announced and resolved sequentially (the stack is
    // momentarily empty between iterations), so the loop breaks only once
    // both iterations have resolved.
    let mut iterations = 0;
    for _ in 0..128 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                iterations += 1;
                answer_graveyard_slot(&mut runner);
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() && iterations == 2 {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            other => panic!("unexpected prompt while resolving Dead Shall Serve: {other:?}"),
        }
    }
    assert_eq!(
        iterations, 2,
        "one iteration per opponent must announce, got {iterations}"
    );
    assert!(
        runner.state().stack.is_empty(),
        "both iterations must have resolved"
    );

    // Production tracked-set binding: both reanimated under P0's control
    // with ownership intact, each grafted to its own owner with the "each
    // combat" window.
    for (id, owner) in [(owned_by_p1, P1), (owned_by_p2, P2)] {
        let obj = &runner.state().objects[&id];
        assert_eq!(obj.zone, Zone::Battlefield, "{id:?} must be reanimated");
        assert_eq!(obj.controller, P0, "{id:?} must be under P0's control");
        assert_eq!(obj.owner, owner, "{id:?} ownership must be intact");
        assert_eq!(
            owner_graft(runner.state(), id),
            (owner, Duration::Permanent),
            "{id:?} must attack its owner every combat"
        );
    }

    // CR 302.6: test-accelerated control tenure — the rider binds at
    // resolution, not at attack time, so tenure is orthogonal setup (the
    // same acceleration the Gideon production test uses).
    for id in [owned_by_p1, owned_by_p2] {
        runner
            .state_mut()
            .objects
            .get_mut(&id)
            .unwrap()
            .summoning_sick = false;
    }
    runner.advance_to_combat();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { player, .. } if player == P0
        ),
        "P0 must declare attackers, got {:?}",
        runner.state().waiting_for
    );

    // Cross-declarations violate the owner-pinned requirements. Both
    // creatures attack so the refusal isolates DEFENDER enforcement: with
    // only the wrong-defender creature named, the missing must-attack
    // attacker would refuse the declaration on its own.
    assert!(
        declare(
            &mut runner,
            vec![
                (owned_by_p1, AttackTarget::Player(P2)),
                (owned_by_p2, AttackTarget::Player(P2)),
            ]
        )
        .is_err(),
        "P1's creature must not attack P2"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ),
        "a refused declaration must leave the prompt in place"
    );
    assert!(
        declare(
            &mut runner,
            vec![
                (owned_by_p1, AttackTarget::Player(P1)),
                (owned_by_p2, AttackTarget::Player(P1)),
            ]
        )
        .is_err(),
        "P2's creature must not attack P1"
    );
    // A full pass violates both requirements (both are able).
    assert!(
        declare(&mut runner, vec![]).is_err(),
        "able creatures with must-attack requirements cannot all hold back"
    );
    // Each attacks its owner: accepted.
    declare(
        &mut runner,
        vec![
            (owned_by_p1, AttackTarget::Player(P1)),
            (owned_by_p2, AttackTarget::Player(P2)),
        ],
    )
    .expect("each creature attacking its owner must be accepted");
}

/// CR 108.3 + CR 508.1d + CR 611.2a + CR 514.2: My Crushing Masterstroke,
/// production path. The seized creatures graft owner-pinned requirements
/// with the "this turn" window, must attack their owners at P0's combat,
/// and are freed at cleanup (grafts gone, control reverted).
#[test]
fn masterstroke_seized_creatures_attack_owners_then_expire() {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 3, 42);
    scenario.at_phase(Phase::Draw);
    let owned_by_p1 = scenario.add_creature(P1, "Seized Bear", 2, 2).id();
    let owned_by_p2 = scenario.add_creature(P2, "Seized Spider", 1, 3).id();
    for i in 0..7 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
        scenario.add_card_to_library_top(P2, &format!("P2 Library {i}"));
    }
    let mut runner = scenario.build();

    let face = scheme_face("My Crushing Masterstroke", MY_CRUSHING_MASTERSTROKE);
    create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    runner.advance_to_phase(Phase::PreCombatMain);
    assert!(
        runner.state().scheme_deck.is_empty(),
        "the scheme must have left the scheme deck"
    );
    for _ in 0..128 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            other => panic!("unexpected prompt while resolving Masterstroke: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "the set-in-motion trigger must have resolved"
    );

    // Seized under P0's control with ownership intact, each grafted to its
    // own owner for "this turn".
    for (id, owner) in [(owned_by_p1, P1), (owned_by_p2, P2)] {
        let obj = &runner.state().objects[&id];
        assert_eq!(obj.controller, P0, "{id:?} must be seized by P0");
        assert_eq!(obj.owner, owner, "{id:?} ownership must be intact");
    }
    assert_eq!(
        owner_graft(runner.state(), owned_by_p1),
        (P1, Duration::UntilEndOfTurn),
        "P1's creature must attack P1 this turn"
    );
    assert_eq!(
        owner_graft(runner.state(), owned_by_p2),
        (P2, Duration::UntilEndOfTurn),
        "P2's creature must attack P2 this turn"
    );

    // Haste is live, so both are able: cross-declaration refused, correct
    // declaration accepted. Both creatures attack so the refusal isolates
    // DEFENDER enforcement: with only the wrong-defender creature named,
    // the missing must-attack attacker would refuse the declaration on its
    // own.
    runner.advance_to_combat();
    assert!(
        declare(
            &mut runner,
            vec![
                (owned_by_p1, AttackTarget::Player(P2)),
                (owned_by_p2, AttackTarget::Player(P2)),
            ]
        )
        .is_err(),
        "P1's creature must not attack P2"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ),
        "a refused declaration must leave the prompt in place"
    );
    declare(
        &mut runner,
        vec![
            (owned_by_p1, AttackTarget::Player(P1)),
            (owned_by_p2, AttackTarget::Player(P2)),
        ],
    )
    .expect("each creature attacking its owner must be accepted");

    // Run combat to completion (no blocks when the step opens), then drive
    // P0's turn into the next turn so cleanup expires the "this turn" effects.
    maybe_declare_no_blocks(&mut runner);
    let start_turn = runner.state().turn_number;
    for _ in 0..128 {
        if runner.state().turn_number != start_turn {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .unwrap();
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            other => panic!("unexpected prompt while finishing P0's turn: {other:?}"),
        }
    }
    assert_ne!(
        runner.state().turn_number,
        start_turn,
        "P0's turn must have ended"
    );
    assert!(
        !runner
            .state()
            .transient_continuous_effects
            .iter()
            .any(|ce| ce.modifications.iter().any(|m| matches!(
                m,
                ContinuousModification::AddStaticMode {
                    mode: StaticMode::MustAttackDefender { .. },
                }
            ))),
        "the this-turn requirements must have expired at cleanup"
    );
    assert_eq!(
        runner.state().objects[&owned_by_p1].controller,
        P1,
        "control of P1's creature must revert at cleanup"
    );
    assert_eq!(
        runner.state().objects[&owned_by_p2].controller,
        P2,
        "control of P2's creature must revert at cleanup"
    );
}
