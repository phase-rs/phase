//! CR733 coverage for the effect-driven combat-membership family.
//!
//! Five authorities put an object into or out of combat by effect rather than by
//! declaration — `combat::enter_attacking`, `combat::place_attacking_alongside`,
//! `combat::place_blocking`, `combat::mark_attacker_blocked`, and
//! `remove_from_combat::remove_object_from_combat` — and every one of them wrote
//! its mutation raw. A retained-prefix replay therefore had no record that a
//! creature was ever attacking, blocking, or blocked by effect.
//!
//! They share ONE parameterized command rather than five sibling variants. The
//! axis stays inside a single CR section, as the categorical boundary rule
//! requires: CR 506.3a-g govern putting a permanent onto the battlefield
//! "attacking or blocking" in one breath, CR 506.4 governs removal, and the
//! declaration-side rules delegate back to it (CR 509.1g ends "See rule 506.4.").
//!
//! The load-bearing reason the defender must be RECORDED rather than re-derived
//! is CR 508.4: "its controller chooses which defending player, planeswalker a
//! defending player controls, or battle a defending player protects it's
//! attacking." That is a choice the rules assign to a player. The resolve-time
//! authority approximates it from ambient state —
//! `defending_player_for_enters_attacking` reads the source's own attacker
//! entry, then `state.current_trigger_event`, then a controller-scan of the live
//! attacker list, then falls back to the first opponent. None of that is
//! reconstructible at replay time, so re-deriving would silently seat the
//! creature against a different defender. `replay_installs_recorded_defender_
//! when_ambient_derivation_diverges` is the test that pins it, and it carries a
//! probe proving the ambient path really would answer differently.

use engine::game::combat::{
    apply_resolved_combat_membership, AttackTarget, BlockHistoryPair, CombatParticipation,
    DamageAssignment, DamageTarget,
};
use engine::game::engine::EngineError;
use engine::game::game_object::ObjectIncarnationRefExt;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{ChosenAttribute, EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::game_state::{CastPaymentMode, LayersDirty, WaitingFor};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef, LEGACY_INCARNATION};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolved_commands::{
    ResolvedCombatMembershipCommand, ResolvedCombatMembershipEdit,
    ResolvedCombatMembershipReplayInvariantError, ResolvedRulesCommand, ResolvedRulesJournal,
};
use engine::types::zones::Zone;

/// Verbatim Oracle text (Scryfall, 2026-07). A paraphrase risks taking a
/// different parser branch than the real card.
const KAALIA_ORACLE: &str = "Flying\nWhenever Kaalia attacks an opponent, you may put an Angel, Demon, or Dragon creature card from your hand onto the battlefield tapped and attacking that opponent.";

/// Every combat-membership command recorded after `journal_start`, in journal
/// (execution) order.
fn membership_commands(
    state: &GameState,
    journal_start: usize,
) -> Vec<ResolvedCombatMembershipCommand> {
    state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter_map(|entry| entry.command.clone())
        .filter_map(|command| match command {
            ResolvedRulesCommand::CombatMembership(command) => Some(command),
            _ => None,
        })
        .collect()
}

fn attacker_entry(state: &GameState, oid: ObjectId) -> Option<(PlayerId, AttackTarget, bool)> {
    state.combat.as_ref().and_then(|combat| {
        combat
            .attackers
            .iter()
            .find(|a| a.object_id == oid)
            .map(|a| (a.defending_player, a.attack_target, a.blocked))
    })
}

/// Advance from a main-phase priority to the declare-attackers step.
fn advance_to_declare_attackers(runner: &mut GameRunner, attacker: PlayerId) {
    runner.state_mut().active_player = attacker;
    runner.state_mut().priority_player = attacker;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: attacker };

    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass should advance toward declare attackers");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("ordering combat triggers should succeed");
            }
            other => panic!("unexpected waiting_for advancing to declare attackers: {other:?}"),
        }
    }
    panic!("expected DeclareAttackers");
}

/// A Kaalia attack that has resolved its "put a creature onto the battlefield
/// tapped and attacking that opponent" trigger.
struct KaaliaAttack {
    runner: GameRunner,
    kaalia: ObjectId,
    angel: ObjectId,
    /// A bystander creature used by the L1 probe to observe what the LIVE
    /// ambient derivation answers in the divergent replay state.
    control: ObjectId,
    journal_start: usize,
}

/// Drives the REAL pipeline: declare attackers, then let Kaalia's attack trigger
/// resolve through the production stack so `change_zone` reaches
/// `combat::enter_attacking` exactly as it does in a game.
fn kaalia_attack() -> KaaliaAttack {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let kaalia = scenario
        .add_creature_from_oracle(P0, "Kaalia of the Vast", 2, 2, KAALIA_ORACLE)
        .id();
    let angel = scenario
        .add_creature_to_hand(P0, "Serra Angel", 4, 4)
        .with_subtypes(vec!["Angel"])
        .id();
    let control = scenario.add_vanilla(P0, 1, 1);

    let mut runner = scenario.build();
    advance_to_declare_attackers(&mut runner, P0);

    let journal_start = runner.state().resolved_rules_journal.entries().len();

    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(kaalia, AttackTarget::Player(P1))],
            bands: Vec::new(),
        })
        .expect("Kaalia can attack the only opponent");

    // Resolve the attack trigger, answering the "you may" and the card choice.
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("ordering the attack trigger should succeed");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accepting Kaalia's may-clause should succeed");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass should resolve the attack trigger");
            }
            other => panic!("unexpected waiting_for resolving Kaalia's trigger: {other:?}"),
        }
    }

    KaaliaAttack {
        runner,
        kaalia,
        angel,
        control,
        journal_start,
    }
}

/// Headline: a creature put onto the battlefield attacking is journaled as an
/// exact resolved command, and replaying that command reproduces the attacking
/// entry against the RECORDED defender.
#[test]
fn enters_attacking_journals_and_replays_its_recorded_defender() {
    let attack = kaalia_attack();
    let state = attack.runner.state();

    // Reach guards. Without these the journal assertion below could pass
    // vacuously on a trigger that never resolved, or on an Angel that entered
    // the battlefield without ever becoming an attacking creature.
    assert_eq!(
        state.objects[&attack.angel].zone,
        engine::types::zones::Zone::Battlefield,
        "the trigger must have put the Angel onto the battlefield"
    );
    let (defender, target, _) = attacker_entry(state, attack.angel)
        .expect("CR 508.4: the Angel must be an attacking creature");
    assert_eq!(
        defender, P1,
        "CR 508.4: the Angel attacks the opponent Kaalia attacked"
    );
    assert_eq!(target, AttackTarget::Player(P1));

    // Discriminating assertion: the membership edit is journaled. A raw
    // mutation records nothing here.
    let commands: Vec<_> = membership_commands(state, attack.journal_start)
        .into_iter()
        .filter(|command| command.object.object_id == attack.angel)
        .collect();
    assert_eq!(
        commands.len(),
        1,
        "the attacking authority must journal exactly one resolved edit for the Angel"
    );
    let command = &commands[0];
    assert_eq!(
        command.edit,
        ResolvedCombatMembershipEdit::Attack {
            resulting_defending_player: P1,
            resulting_attack_target: AttackTarget::Player(P1),
            resulting_attack_target_identity: None,
        },
        "the recorded pair is the CR 508.4 choice the authority settled"
    );

    // Replay exactness. Fields are compared individually: `CombatState`'s
    // hand-written `PartialEq` skips `damage_assignments`, `damage_step_index`,
    // `pending_damage`, and `regular_damage_done`, so a whole-struct equality
    // assertion would be blind to divergence in exactly those fields.
    //
    // The predecessor is the post-resolution state with ONLY this family's edit
    // undone. It cannot be the pre-declaration state: the Angel moved from hand
    // to battlefield, and CR 400.7 makes that a NEW object, so the recorded
    // incarnation would legitimately not match (the applier rejects that with
    // `StaleObject`, which the fail-closed test relies on).
    let mut replay = state.clone();
    if let Some(combat) = replay.combat.as_mut() {
        combat.attackers.retain(|a| a.object_id != attack.angel);
    }
    apply_resolved_combat_membership(&mut replay, command)
        .expect("the recorded edit must replay against a state without the Angel attacking");
    let (replay_defender, replay_target, replay_blocked) = attacker_entry(&replay, attack.angel)
        .expect("replay must reinstate the Angel as an attacking creature");
    assert_eq!(
        replay_defender, P1,
        "replay installs the recorded defending player"
    );
    assert_eq!(
        replay_target,
        AttackTarget::Player(P1),
        "replay installs the recorded attack target"
    );
    assert!(
        !replay_blocked,
        "CR 509.1h: a creature entering attacking is not blocked"
    );
}

/// Verbatim pinned MTGJSON Oracle text. The destination siblings below are
/// typed combat fixtures; neither adds a new Oracle clause to this card.
const NINJA_ORACLE: &str = "Ninjutsu {1}{U} ({1}{U}, Return an unblocked attacker you control to hand: Put this card onto the battlefield from your hand tapped and attacking.)\nWhenever this creature deals combat damage to a player, you may draw a card.";

/// Produce the permanent Attack command through the real activation action,
/// sharing `place_attacking_alongside` / `push_attacker_and_journal` with entry.
fn permanent_ninjutsu_attack(battle: bool) -> (GameRunner, ObjectId, ObjectId, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let returned = scenario.add_vanilla(P0, 2, 2);
    let other = scenario.add_vanilla(P0, 2, 2);
    let blocker = scenario.add_vanilla(P1, 1, 4);
    let ninja = scenario
        .add_creature_to_hand_from_oracle(P0, "Ninja of the Deep Hours", 2, 2, NINJA_ORACLE)
        .with_subtypes(vec!["Human", "Ninja"])
        .id();
    let recipient = if battle {
        scenario.add_vanilla(P0, 0, 1)
    } else {
        scenario
            .add_planeswalker_from_oracle(P1, "Combat recipient", "Test", 8, "")
            .id()
    };
    let mut runner = scenario.build();
    if battle {
        let object = runner.state_mut().objects.get_mut(&recipient).unwrap();
        object.card_types.core_types = vec![CoreType::Battle];
        object.card_types.subtypes = vec!["Siege".to_string()];
        object.base_card_types = object.card_types.clone();
        object.power = None;
        object.toughness = None;
        object.base_power = None;
        object.base_toughness = None;
        object.defense = Some(8);
        object.base_defense = Some(8);
        object.counters.insert(CounterType::Defense, 8);
        object.chosen_attributes.push(ChosenAttribute::Player(P1));
    }
    let target = if battle {
        AttackTarget::Battle(recipient)
    } else {
        AttackTarget::Planeswalker(recipient)
    };
    advance_to_declare_attackers(&mut runner, P0);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(returned, target), (other, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare the permanent attack and unrelated player attack");
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ));
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![],
        })
        .expect("the returned attacker is genuinely unblocked");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(
        attacker_entry(runner.state(), returned),
        Some((P1, target, false))
    );
    let pin = ObjectIncarnationRef::from_object(&runner.state().objects[&recipient]);
    assert_eq!(
        runner
            .state()
            .combat
            .as_ref()
            .unwrap()
            .attacked_permanents
            .get(&returned),
        Some(&pin)
    );
    for mana in [ManaType::Blue, ManaType::Colorless] {
        let _ = runner
            .state_mut()
            .add_mana_to_pool(P0, ManaUnit::new(mana, ninja, false, vec![]));
    }
    let journal_start = runner.state().resolved_rules_journal.entries().len();
    runner
        .act(GameAction::ActivateNinjutsu {
            ninjutsu_object_id: ninja,
            creature_to_return: returned,
        })
        .expect("activate the printed Ninjutsu ability through apply");
    assert_eq!(runner.state().objects[&returned].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&ninja].zone, Zone::Battlefield);
    assert!(runner.state().objects[&ninja].tapped);
    assert_eq!(
        attacker_entry(runner.state(), ninja),
        Some((P1, target, false))
    );

    // Nonempty unrelated bookkeeping makes partial mutation on rejection visible.
    // This is a typed replay fixture, after the production command was captured.
    let combat = runner.state_mut().combat.as_mut().unwrap();
    combat
        .attackers
        .iter_mut()
        .find(|info| info.object_id == other)
        .unwrap()
        .blocked = true;
    combat.blocker_assignments.insert(other, vec![blocker]);
    combat.blocker_to_attacker.insert(blocker, vec![other]);
    combat.damage_assignments.insert(
        other,
        vec![DamageAssignment {
            target: DamageTarget::Object(blocker),
            amount: 2,
        }],
    );
    combat.pending_damage.push((
        other,
        DamageAssignment {
            target: DamageTarget::Player(P1),
            amount: 1,
        },
    ));
    combat.damage_step_index = Some(1);
    combat.first_strike_done = true;
    combat.first_strike_participants = Some([other].into_iter().collect());
    combat.regular_damage_done = true;
    runner.state_mut().layers_dirty = LayersDirty::Clean;
    (runner, ninja, recipient, journal_start)
}

fn assert_atomic_attack_rejection(
    state: &mut GameState,
    command: &ResolvedCombatMembershipCommand,
    expected: ResolvedCombatMembershipReplayInvariantError,
) {
    // Serialize the ENTIRE state, including all CombatState fields omitted by
    // PartialEq, block histories, journal, waiting state and event carriers.
    let before = serde_json::to_value(&*state).unwrap();
    let dirty = state.layers_dirty.clone();
    assert_eq!(
        apply_resolved_combat_membership(state, command),
        Err(expected)
    );
    assert_eq!(serde_json::to_value(&*state).unwrap(), before);
    assert_eq!(state.layers_dirty, dirty);
}

/// CR 702.49c: Ninjutsu preserves the returned creature's exact destination,
/// including both permanent kinds. Replay must install the recorded active pin.
#[test]
fn permanent_attack_producer_journal_roundtrip_and_replay_keep_exact_recipient() {
    for battle in [false, true] {
        let (runner, ninja, recipient, journal_start) = permanent_ninjutsu_attack(battle);
        let state = runner.state();
        let pin = ObjectIncarnationRef::from_object(&state.objects[&recipient]);
        let target = if battle {
            AttackTarget::Battle(recipient)
        } else {
            AttackTarget::Planeswalker(recipient)
        };
        let commands: Vec<_> = membership_commands(state, journal_start)
            .into_iter()
            .filter(|command| command.object.object_id == ninja)
            .collect();
        assert_eq!(
            commands.len(),
            1,
            "the production activation records one Attack"
        );
        let command = &commands[0];
        assert_eq!(
            command.object,
            ObjectIncarnationRef::from_object(&state.objects[&ninja])
        );
        assert_eq!(
            command.edit,
            ResolvedCombatMembershipEdit::Attack {
                resulting_defending_player: P1,
                resulting_attack_target: target,
                resulting_attack_target_identity: Some(pin),
            },
            "the producer must thread the exact permanent pin, kind, ID and original defender"
        );
        assert_eq!(
            state
                .combat
                .as_ref()
                .unwrap()
                .attacked_permanents
                .get(&ninja),
            Some(&pin)
        );

        let json = serde_json::to_string(&state.resolved_rules_journal).unwrap();
        let roundtrip: ResolvedRulesJournal = serde_json::from_str(&json)
            .expect("the real journal decoder validates the populated Attack authority");
        assert_eq!(
            serde_json::to_value(&roundtrip).unwrap(),
            serde_json::to_value(&state.resolved_rules_journal).unwrap()
        );
        let mut replay = state.clone();
        replay.resolved_rules_journal = roundtrip;
        let decoded = membership_commands(&replay, journal_start)
            .into_iter()
            .find(|record| record.object.object_id == ninja)
            .unwrap();
        assert_eq!(&decoded, command);
        let combat = replay.combat.as_mut().unwrap();
        combat.attackers.retain(|info| info.object_id != ninja);
        combat.attacked_permanents.remove(&ninja);
        assert!(attacker_entry(&replay, ninja).is_none());
        assert!(!replay
            .combat
            .as_ref()
            .unwrap()
            .attacked_permanents
            .contains_key(&ninja));
        apply_resolved_combat_membership(&mut replay, &decoded)
            .expect("replay against the post-entry predecessor installs membership and pin");
        assert_eq!(attacker_entry(&replay, ninja), Some((P1, target, false)));
        let combat = replay.combat.as_ref().unwrap();
        assert_eq!(
            combat.attacked_permanents.get(&ninja),
            Some(&pin),
            "replay must install the absent outgoing edge"
        );
        let member = combat
            .attackers
            .iter()
            .find(|info| info.object_id == ninja)
            .unwrap();
        assert_eq!(
            member.resolve_damage_target(&replay, false),
            Some(DamageTarget::Object(recipient)),
            "the shared current-recipient reader must consume the installed pin"
        );
        assert_eq!(
            serde_json::to_value(&replay.combat).unwrap(),
            serde_json::to_value(&state.combat).unwrap(),
            "replay preserves the complete graph and damage bookkeeping"
        );
        assert_eq!(replay.layers_dirty, LayersDirty::Full);
        assert_atomic_attack_rejection(
            &mut replay,
            &decoded,
            ResolvedCombatMembershipReplayInvariantError::AlreadyAttacking(ninja),
        );
    }
}

/// CR 400.7: a new recipient incarnation cannot satisfy recorded authority.
#[test]
fn permanent_attack_stale_and_missing_legacy_authority_reject_atomically() {
    for battle in [false, true] {
        let (runner, ninja, recipient, journal_start) = permanent_ninjutsu_attack(battle);
        let state = runner.state();
        let command = membership_commands(state, journal_start)
            .into_iter()
            .find(|record| record.object.object_id == ninja)
            .unwrap();
        let pin = ObjectIncarnationRef::from_object(&state.objects[&recipient]);
        let mut predecessor = state.clone();
        let combat = predecessor.combat.as_mut().unwrap();
        combat.attackers.retain(|info| info.object_id != ninja);
        combat.attacked_permanents.remove(&ninja);
        let mut stale = predecessor.clone();
        stale.objects.get_mut(&recipient).unwrap().incarnation += 1;
        let found = ObjectIncarnationRef::from_object(&stale.objects[&recipient]);
        assert_ne!(pin, found);
        assert_atomic_attack_rejection(
            &mut stale,
            &command,
            ResolvedCombatMembershipReplayInvariantError::StaleObject {
                expected: pin,
                found,
            },
        );

        let mut legacy_json = serde_json::to_value(&state.resolved_rules_journal).unwrap();
        let mut removed = 0;
        for entry in legacy_json["entries"].as_array_mut().unwrap() {
            let Some(record) = entry
                .get_mut("command")
                .and_then(|command| command.get_mut("CombatMembership"))
            else {
                continue;
            };
            if record["object"] == serde_json::to_value(command.object).unwrap() {
                let attack = record["edit"]["Attack"]
                    .as_object_mut()
                    .expect("the captured command is Attack");
                assert!(attack.remove("resulting_attack_target_identity").is_some());
                removed += 1;
            }
        }
        assert_eq!(
            removed, 1,
            "remove the field from the actual permanent Attack wire record"
        );
        let legacy: ResolvedRulesJournal = serde_json::from_value(legacy_json)
            .expect("unused legacy permanent Attack history remains decodable");
        let mut history = state.clone();
        history.resolved_rules_journal = legacy;
        let unknown = membership_commands(&history, journal_start)
            .into_iter()
            .find(|record| record.object.object_id == ninja)
            .unwrap();
        let ResolvedCombatMembershipEdit::Attack {
            resulting_attack_target_identity,
            ..
        } = &unknown.edit
        else {
            unreachable!()
        };
        assert_eq!(
            *resulting_attack_target_identity, None,
            "historical decoding must not invent a pin"
        );
        let redecoded: ResolvedRulesJournal =
            serde_json::from_str(&serde_json::to_string(&history.resolved_rules_journal).unwrap())
                .unwrap();
        history.resolved_rules_journal = redecoded;
        assert_eq!(
            membership_commands(&history, journal_start)
                .into_iter()
                .find(|record| record.object.object_id == ninja),
            Some(unknown.clone())
        );
        assert_atomic_attack_rejection(
            &mut predecessor,
            &unknown,
            ResolvedCombatMembershipReplayInvariantError::StaleObject {
                expected: ObjectIncarnationRef::of(recipient, LEGACY_INCARNATION),
                found: pin,
            },
        );
    }
}

#[test]
fn permanent_attack_serialized_guard_refuses_incoherent_populated_authority() {
    for battle in [false, true] {
        let (runner, ninja, recipient, journal_start) = permanent_ninjutsu_attack(battle);
        let state = runner.state();
        let command = membership_commands(state, journal_start)
            .into_iter()
            .find(|record| record.object.object_id == ninja)
            .unwrap();
        let mut wrong_id = command.clone();
        let mut legacy_pin = command.clone();
        let mut player_pin = command.clone();
        if let ResolvedCombatMembershipEdit::Attack {
            resulting_attack_target_identity,
            ..
        } = &mut wrong_id.edit
        {
            *resulting_attack_target_identity =
                Some(ObjectIncarnationRef::from_object(&state.objects[&ninja]));
        }
        if let ResolvedCombatMembershipEdit::Attack {
            resulting_attack_target_identity,
            ..
        } = &mut legacy_pin.edit
        {
            *resulting_attack_target_identity =
                Some(ObjectIncarnationRef::of(recipient, LEGACY_INCARNATION));
        }
        if let ResolvedCombatMembershipEdit::Attack {
            resulting_attack_target,
            ..
        } = &mut player_pin.edit
        {
            *resulting_attack_target = AttackTarget::Player(P1);
        }
        for bad in [wrong_id, legacy_pin, player_pin] {
            let mut journal = state.resolved_rules_journal.clone();
            journal.record_combat_membership(bad).unwrap();
            let error = serde_json::from_value::<ResolvedRulesJournal>(
                serde_json::to_value(journal).unwrap(),
            )
            .expect_err("the Attack serialized-authority guard must reject populated incoherence");
            assert!(
                error
                    .to_string()
                    .contains("combat-membership attack has incoherent permanent identity"),
                "wrong validator branch: {error}"
            );
        }
    }
}

#[test]
fn field_missing_legacy_player_attack_replays_without_a_permanent_pin() {
    let attack = kaalia_attack();
    let command = membership_commands(attack.runner.state(), attack.journal_start)
        .into_iter()
        .find(|record| record.object.object_id == attack.angel)
        .unwrap();
    let mut wire = serde_json::to_value(&command).unwrap();
    assert!(wire["edit"]["Attack"]
        .as_object_mut()
        .unwrap()
        .remove("resulting_attack_target_identity")
        .is_some());
    let legacy: ResolvedCombatMembershipCommand = serde_json::from_value(wire).unwrap();
    assert_eq!(
        legacy, command,
        "Player/None retains its safe legacy meaning"
    );
    let mut replay = attack.runner.state().clone();
    let combat = replay.combat.as_mut().unwrap();
    combat
        .attackers
        .retain(|info| info.object_id != attack.angel);
    assert!(!combat.attacked_permanents.contains_key(&attack.angel));
    apply_resolved_combat_membership(&mut replay, &legacy)
        .expect("legacy Player Attack remains replayable");
    assert_eq!(
        attacker_entry(&replay, attack.angel),
        Some((P1, AttackTarget::Player(P1), false))
    );
    let combat = replay.combat.as_ref().unwrap();
    assert!(!combat.attacked_permanents.contains_key(&attack.angel));
    assert_eq!(
        combat
            .attackers
            .iter()
            .find(|info| info.object_id == attack.angel)
            .unwrap()
            .resolve_damage_target(&replay, false),
        Some(DamageTarget::Player(P1))
    );
}

/// The point of the unit (landmine L1). `enter_attacking` picks its defender
/// from AMBIENT state, so the applier must never call it. This drives replay
/// into a state whose ambient derivation demonstrably answers DIFFERENTLY, and
/// proves the recorded defender still wins.
///
/// CR 508.4 makes this a rules question, not just a determinism question: the
/// defender is a choice the controller already made, and re-deriving re-decides
/// a settled choice.
#[test]
fn replay_installs_recorded_defender_when_ambient_derivation_diverges() {
    let attack = kaalia_attack();
    let state = attack.runner.state();
    let command = membership_commands(state, attack.journal_start)
        .into_iter()
        .find(|command| command.object.object_id == attack.angel)
        .expect("the Angel's attacking entry must be journaled");

    // Build a replay state where every ambient source disagrees with the
    // record: Kaalia is no longer an attacker (defeating both the source lookup
    // and the controller-scan), and the current trigger event names P0.
    let mut replay = state.clone();
    if let Some(combat) = replay.combat.as_mut() {
        combat
            .attackers
            .retain(|a| a.object_id != attack.angel && a.object_id != attack.kaalia);
    }
    replay.current_trigger_event = Some(GameEvent::DamageDealt {
        source_id: attack.kaalia,
        target: TargetRef::Player(P0),
        amount: 1,
        is_combat: false,
        excess: 0,
        source_incarnation: None,
    });

    // Non-vacuity probe, VERIFIED APPLIED: run the live authority in this exact
    // state on a control creature. If it still derived P1 the test below would
    // be vacuous — a re-deriving applier would coincidentally look correct.
    let mut probe = replay.clone();
    engine::game::combat::enter_attacking(&mut probe, attack.control, attack.kaalia, P0);
    let (probe_defender, _, _) = attacker_entry(&probe, attack.control)
        .expect("the probe creature must be seated as an attacker");
    assert_eq!(
        probe_defender, P0,
        "probe: ambient derivation in this state must answer P0, otherwise the \
         divergence assertion below is vacuous"
    );

    // The discriminating assertion: replay ignores ambient state entirely.
    apply_resolved_combat_membership(&mut replay, &command)
        .expect("the recorded edit must replay regardless of ambient combat state");
    let (replay_defender, replay_target, _) = attacker_entry(&replay, attack.angel)
        .expect("replay must reinstate the Angel as an attacking creature");
    assert_eq!(
        replay_defender, P1,
        "CR 508.4: replay installs the RECORDED defender, not the ambient one"
    );
    assert_eq!(
        replay_target,
        AttackTarget::Player(P1),
        "replay installs the recorded attack target, not the ambient one"
    );
}

/// Fail-closed: an `expected_*` that no longer describes live state must return
/// a typed error instead of installing the `resulting_*` anyway. CR 506.3c and
/// CR 508.4a are the rules reason — a creature whose recorded defender is gone
/// is never an attacking creature, so silently seating it elsewhere is wrong.
#[test]
fn replay_fails_closed_when_the_recorded_precondition_no_longer_holds() {
    let attack = kaalia_attack();
    let state = attack.runner.state();
    let command = membership_commands(state, attack.journal_start)
        .into_iter()
        .find(|command| command.object.object_id == attack.angel)
        .expect("the Angel's attacking entry must be journaled");

    // The Angel is ALREADY attacking in this state, so the command's
    // "not yet an attacker" precondition is violated.
    let mut replay = state.clone();
    assert!(
        attacker_entry(&replay, attack.angel).is_some(),
        "reach guard: the Angel must already be attacking for this to be a real conflict"
    );
    let error = apply_resolved_combat_membership(&mut replay, &command)
        .expect_err("replaying onto an already-attacking creature must fail closed");
    assert_eq!(
        error,
        ResolvedCombatMembershipReplayInvariantError::AlreadyAttacking(attack.angel),
        "the failure is typed, not a silent duplicate attacker"
    );
    // The rejection left no partial edit.
    assert_eq!(
        replay.combat.as_ref().map(|combat| combat
            .attackers
            .iter()
            .filter(|a| a.object_id == attack.angel)
            .count()),
        Some(1),
        "a rejected command must not have pushed a second attacker entry"
    );
}

/// CR 506.4 removal: the authority records the exact roles it pruned, replay
/// reproduces the prune, and a participation mismatch fails closed.
///
/// `damage_assignments` is carried in the receipt precisely because
/// `CombatState`'s `PartialEq` does not compare it.
#[test]
fn removal_journals_exact_participation_and_replays_it() {
    let attack = kaalia_attack();
    let mut runner = attack.runner;

    let before_removal = runner.state().clone();
    let journal_start = before_removal.resolved_rules_journal.entries().len();
    let expected = CombatParticipation::capture(&before_removal, attack.angel);

    // Reach guard: the object really is in combat, so the removal below prunes
    // something and the journal assertion cannot pass vacuously.
    assert!(
        !expected.is_empty(),
        "reach guard: the Angel must hold a combat role before removal"
    );
    assert!(
        expected.attacking.is_some(),
        "reach guard: the Angel is the attacking creature being pruned"
    );

    engine::game::effects::remove_from_combat::remove_object_from_combat(
        runner.state_mut(),
        attack.angel,
    );

    assert!(
        attacker_entry(runner.state(), attack.angel).is_none(),
        "CR 506.4: the removed creature stops being an attacking creature"
    );

    let commands: Vec<_> = membership_commands(runner.state(), journal_start)
        .into_iter()
        .filter(|command| command.object.object_id == attack.angel)
        .collect();
    assert_eq!(
        commands.len(),
        1,
        "the removal authority must journal exactly one resolved edit"
    );
    assert_eq!(
        commands[0].edit,
        ResolvedCombatMembershipEdit::Remove {
            expected_participation: expected.clone(),
        },
        "the receipt records the exact roles the removal pruned"
    );

    // Replay exactness against the captured predecessor.
    let mut replay = before_removal.clone();
    apply_resolved_combat_membership(&mut replay, &commands[0])
        .expect("the recorded removal must replay against its captured predecessor");
    assert!(
        attacker_entry(&replay, attack.angel).is_none(),
        "replay reproduces the prune"
    );
    assert_eq!(
        CombatParticipation::capture(&replay, attack.angel),
        CombatParticipation::default(),
        "replay leaves the object holding no combat role at all"
    );

    // Fail-closed: the same command against a state where the object no longer
    // participates must be rejected rather than pruning nothing silently.
    let error = apply_resolved_combat_membership(&mut replay, &commands[0])
        .expect_err("re-applying a removal to an unparticipating object must fail closed");
    assert!(
        matches!(
            error,
            ResolvedCombatMembershipReplayInvariantError::ParticipationMismatch { .. }
        ),
        "the failure is a typed participation mismatch, got {error:?}"
    );
}

/// Verbatim Oracle text (Scryfall, 2026-07).
const DAZZLING_BEAUTY: &str = "Cast this spell only during the declare blockers step.\nTarget unblocked attacking creature becomes blocked.";

/// Verbatim Oracle text (Scryfall, 2026-07).
const MIRROR_MATCH: &str = "Cast this spell only during the declare blockers step.\nFor each creature attacking you or a planeswalker you control, create a token that's a copy of that creature and that's blocking that creature. Exile those tokens at end of combat.";

/// Verbatim Oracle text (MTGJSON, 2026-10). The block-history rows below read
/// CR 509.4 through these printed cards.
const SIZZLING_BARRAGE: &str =
    "Sizzling Barrage deals 4 damage to target creature that blocked this turn.";
const DEMONIC_STENCH: &str = "Each creature that blocked this turn gets -1/-1 until end of turn. (Then exile this card. You may cast the creature later from exile.)";
const GIDEONS_TRIUMPH: &str = "Target opponent sacrifices a creature of their choice that attacked or blocked this turn. If you control a Gideon planeswalker, that player sacrifices two of those creatures of their choice instead.";
const CATHEDRAL_MEMBRANE: &str = "({W/P} can be paid with either {W} or 2 life.)\nDefender\nWhen this creature dies during combat, it deals 6 damage to each creature it blocked this combat.";
const ROLLING_STONES: &str = "Wall creatures can attack as though they didn't have defender.";
const ABU_JAFAR: &str = "When this creature dies, destroy all creatures blocking or blocked by it. They can't be regenerated.";
const MURDER: &str = "Destroy target creature.";
const SHOCK: &str = "Shock deals 2 damage to any target.";

/// Drive P0's attack into the declare-blockers step and hand priority to P1 so
/// the defending player can cast during that step (CR 509).
fn advance_to_declare_blockers_and_give_priority(runner: &mut GameRunner, caster: PlayerId) {
    for _ in 0..20 {
        if runner.state().phase == Phase::DeclareBlockers {
            let state = runner.state_mut();
            state.priority_player = caster;
            state.waiting_for = WaitingFor::Priority { player: caster };
            return;
        }
        let wf = runner.state().waiting_for.clone();
        let acted = match wf {
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
            WaitingFor::DeclareBlockers { .. } => runner.act(GameAction::DeclareBlockers {
                assignments: vec![],
            }),
            other => panic!("unexpected waiting state before declare-blockers: {other:?}"),
        };
        acted.expect("combat advance action");
    }
    panic!("did not reach the declare-blockers step");
}

/// `advance_to_declare_blockers_and_give_priority` for a board with real blocks:
/// `assignments` goes through `GameAction::DeclareBlockers` before priority is
/// handed to `caster`. A board with no possible blocker has no declaration to
/// make, so `assignments` is then empty and the prompt never appears.
fn declare_blocks_and_give_priority(
    runner: &mut GameRunner,
    assignments: &[(ObjectId, ObjectId)],
    caster: PlayerId,
) {
    for _ in 0..20 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: assignments.to_vec(),
                    })
                    .expect("the declared blocks are legal");
            }
            WaitingFor::Priority { .. } if runner.state().phase == Phase::DeclareBlockers => {
                advance_to_declare_blockers_and_give_priority(runner, caster);
                return;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass should advance toward declare blockers");
            }
            other => panic!("unexpected waiting state before declare-blockers: {other:?}"),
        }
    }
    panic!("did not reach the declare-blockers step");
}

/// P0 attacks P1 with `attackers`, P1 declares `assignments`, and P1's Mirror
/// Match resolves in the declare blockers step. Priority then goes to `caster`.
///
/// Returns each attacker's copy token, in `attackers` order. Mirror Match copies
/// every attacker, so an attacker with a declared blocker has a token as well.
/// Every token is a reach guard for its caller: it is blocking, its attacker is
/// blocked, and it was not among the declared blockers.
fn resolve_mirror_match(
    runner: &mut GameRunner,
    mirror_match: ObjectId,
    attackers: &[ObjectId],
    assignments: &[(ObjectId, ObjectId)],
    caster: PlayerId,
) -> Vec<ObjectId> {
    runner.advance_to_combat();
    let attacks: Vec<_> = attackers
        .iter()
        .map(|attacker| (*attacker, AttackTarget::Player(P1)))
        .collect();
    runner
        .declare_attackers(&attacks)
        .expect("declare attackers");
    declare_blocks_and_give_priority(runner, assignments, P1);
    runner.cast(mirror_match).resolve();
    runner.advance_until_stack_empty();
    advance_to_declare_blockers_and_give_priority(runner, caster);

    let state = runner.state();
    let combat = state.combat.as_ref().expect("combat is live");
    attackers
        .iter()
        .map(|attacker| {
            let token = *combat
                .blocker_assignments
                .get(attacker)
                .and_then(|blockers| blockers.last())
                .expect("Mirror Match must put a copy token in as a blocker");
            assert!(
                assignments.iter().all(|(blocker, _)| *blocker != token),
                "reach guard: the last blocker must be the token, not a declared blocker"
            );
            assert!(
                combat
                    .blocker_to_attacker
                    .get(&token)
                    .is_some_and(|blocked| blocked.contains(attacker)),
                "CR 509.4: the token is \"blocking\" its attacker"
            );
            assert!(
                attacker_entry(state, *attacker).is_some_and(|(_, _, blocked)| blocked),
                "CR 509.3c: the token's attacker is a blocked creature"
            );
            token
        })
        .collect()
}

/// A cast submitted with no announced targets, for tests whose subject is the
/// target prompt itself or the engine's refusal to open one.
fn cast_action(state: &GameState, spell: ObjectId) -> GameAction {
    GameAction::CastSpell {
        object_id: spell,
        card_id: state.objects[&spell].card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    }
}

/// CR 509.4: a creature put onto the battlefield blocking is "blocking" but never
/// "blocked", so no history ledger names it. Each ledger has its own assertion,
/// and each pair ledger is read by blocker object id, so an entry under any
/// attacker or incarnation fails.
fn assert_absent_from_block_history(state: &GameState, blocker: ObjectId, when: &str) {
    assert!(
        !state.creatures_blocked_this_turn.contains(&blocker),
        "CR 509.4 ({when}): the per-turn blocked set must not name it"
    );
    assert!(
        !state
            .creature_blocked_attackers_this_turn
            .iter()
            .any(|pair| pair.blocker.object_id == blocker),
        "CR 509.4 ({when}): the turn-scoped block history must not name it"
    );
    assert!(
        !state
            .combat
            .as_ref()
            .expect("combat is live")
            .creature_blocked_attackers_this_combat
            .iter()
            .any(|pair| pair.blocker.object_id == blocker),
        "CR 509.4 ({when}): the combat-scoped block history must not name it"
    );
}

/// CR 509.1g + CR 400.7: a declared block is in all three history ledgers, with
/// both sides pinned to their live incarnations.
fn assert_declared_block_history(
    state: &GameState,
    blocker: ObjectId,
    attacker: ObjectId,
    when: &str,
) {
    let pair = BlockHistoryPair {
        blocker: ObjectIncarnationRef::from_object(&state.objects[&blocker]),
        attacker: ObjectIncarnationRef::from_object(&state.objects[&attacker]),
    };
    assert!(
        state.creatures_blocked_this_turn.contains(&blocker),
        "CR 509.1g ({when}): the per-turn blocked set"
    );
    assert!(
        state.creature_blocked_attackers_this_turn.contains(&pair),
        "CR 509.1g ({when}): the turn-scoped block history"
    );
    assert!(
        state
            .combat
            .as_ref()
            .expect("combat is live")
            .creature_blocked_attackers_this_combat
            .contains(&pair),
        "CR 509.1g ({when}): the combat-scoped block history"
    );
}

fn on_battlefield(state: &GameState, object: ObjectId) -> bool {
    state
        .objects
        .get(&object)
        .is_some_and(|live| live.zone == Zone::Battlefield)
}

/// The stack drained and no prompt is pending, so the resolution completed.
fn assert_resolution_completed(runner: &GameRunner) {
    assert!(
        runner.state().stack.is_empty(),
        "the stack must have drained"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "no prompt may remain, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 509.1h via `mark_attacker_blocked`: an attacker made blocked purely by
/// effect, with no blocker assigned. Driven through a real Dazzling Beauty cast.
#[test]
fn become_blocked_journals_a_mark_blocked_edit_and_replays_it() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Charging Ox", 3, 3).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Dazzling Beauty", true, DAZZLING_BEAUTY)
        .id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare attackers");
    advance_to_declare_blockers_and_give_priority(&mut runner, P1);

    // Reach guard: the attacker must be an UNBLOCKED attacker before the cast,
    // otherwise the mark below is a no-op and is never journaled.
    let (_, _, blocked_before) =
        attacker_entry(runner.state(), attacker).expect("the Ox must be attacking");
    assert!(
        !blocked_before,
        "reach guard: the attacker must start unblocked"
    );

    let journal_start = runner.state().resolved_rules_journal.entries().len();
    let before = runner.state().clone();
    runner.cast(spell).target_objects(&[attacker]).resolve();

    // Reach guard: the effect actually took hold.
    let (_, _, blocked_after) =
        attacker_entry(runner.state(), attacker).expect("the Ox must still be attacking");
    assert!(
        blocked_after,
        "CR 509.1h: the resolved spell must make the attacker blocked"
    );

    let commands: Vec<_> = membership_commands(runner.state(), journal_start)
        .into_iter()
        .filter(|command| command.object.object_id == attacker)
        .collect();
    assert_eq!(
        commands.len(),
        1,
        "the mark-blocked authority must journal exactly one resolved edit"
    );
    assert_eq!(
        commands[0].edit,
        ResolvedCombatMembershipEdit::MarkBlocked,
        "CR 509.1h: the recorded edit is the effect-driven blocked mark"
    );

    // Replay exactness against the pre-cast state.
    let mut replay = before;
    apply_resolved_combat_membership(&mut replay, &commands[0])
        .expect("the recorded mark must replay against its captured predecessor");
    let (_, _, replayed_blocked) =
        attacker_entry(&replay, attacker).expect("replay keeps the Ox attacking");
    assert!(replayed_blocked, "replay installs the blocked bit");
    // CR 509.1h: marking blocked assigns NO blocker, so the maps stay empty.
    let participation = CombatParticipation::capture(&replay, attacker);
    assert!(
        participation.blocked_by.is_empty(),
        "CR 510.1c: an effect-blocked attacker has no creatures blocking it"
    );

    // Fail-closed: the bit is sticky, so re-applying must be rejected rather
    // than silently re-marking.
    let error = apply_resolved_combat_membership(&mut replay, &commands[0])
        .expect_err("re-marking an already-blocked attacker must fail closed");
    assert!(
        matches!(
            error,
            ResolvedCombatMembershipReplayInvariantError::BlockedPreconditionMismatch { .. }
        ),
        "the failure is a typed blocked-precondition mismatch, got {error:?}"
    );
}

/// CR 509.4 via `place_blocking`: a token put onto the battlefield already
/// blocking. Driven through a real Mirror Match cast.
///
/// L3: the authority writes the attacker's sticky `blocked` bit and the blocker
/// maps, and no history ledger. Each write is asserted after replay, individually.
#[test]
fn place_blocking_journals_a_block_edit_and_replays_every_write() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Charging Ox", 3, 3).id();
    let second_attacker = scenario.add_creature(P0, "Second Attacker", 2, 2).id();
    let declared = scenario.add_creature(P1, "Declared Blocker", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Mirror Match", true, MIRROR_MATCH)
        .id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (attacker, AttackTarget::Player(P1)),
            (second_attacker, AttackTarget::Player(P1)),
        ])
        .expect("declare attackers");
    declare_blocks_and_give_priority(&mut runner, &[(declared, second_attacker)], P1);

    let journal_start = runner.state().resolved_rules_journal.entries().len();
    runner.cast(spell).resolve();
    runner.advance_until_stack_empty();

    // Reach guard: a copy token exists and is genuinely blocking the attacker.
    let state = runner.state();
    let combat = state.combat.as_ref().expect("combat is live");
    let blockers = combat
        .blocker_assignments
        .get(&attacker)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        blockers.len(),
        1,
        "CR 509.1g: Mirror Match must put exactly one copy token in as a blocker"
    );
    let token = blockers[0];
    assert!(
        combat
            .blocker_to_attacker
            .get(&token)
            .is_some_and(|a| a.contains(&attacker)),
        "reach guard: the reverse lookup must name the attacker"
    );

    let commands: Vec<_> = membership_commands(state, journal_start)
        .into_iter()
        .filter(|command| command.object.object_id == token)
        .collect();
    assert_eq!(
        commands.len(),
        1,
        "the blocking authority must journal exactly one resolved edit for the token"
    );
    assert_eq!(
        commands[0].edit,
        ResolvedCombatMembershipEdit::Block {
            resulting_attacker: attacker,
            expected_attacker_blocked: false,
        },
        "CR 509.1h: the token is the FIRST blocker, so the sticky bit was clear"
    );

    // Same-attacker hostile case: the declared block and this attacker's own
    // copy token share one attacker, and only the declaration is history.
    let second_blockers = combat
        .blocker_assignments
        .get(&second_attacker)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        second_blockers.len(),
        2,
        "reach guard: the declared blocker and the second attacker's copy token"
    );
    assert_eq!(
        second_blockers[0], declared,
        "reach guard: the declaration precedes the token"
    );
    let second_token = second_blockers[1];

    // CR 509.4: asserted before the replay clone, so a live ledger write and a
    // replay ledger write each fail at their own assertion.
    assert_absent_from_block_history(state, token, "live");
    assert_absent_from_block_history(state, second_token, "live");
    assert_declared_block_history(state, declared, second_attacker, "live");

    // Replay exactness: undo only this family's edit, then reinstall it.
    let mut replay = state.clone();
    if let Some(combat) = replay.combat.as_mut() {
        combat.blocker_assignments.remove(&attacker);
        combat.blocker_to_attacker.remove(&token);
        for info in combat.attackers.iter_mut() {
            if info.object_id == attacker {
                info.blocked = false;
            }
        }
    }

    apply_resolved_combat_membership(&mut replay, &commands[0])
        .expect("the recorded block must replay against its predecessor");

    // Every write is asserted individually. `CombatState`'s hand-written
    // `PartialEq` deliberately omits the damage-step scratch fields, so a
    // whole-struct equality check here would be blind to exactly the
    // bookkeeping this family edits.
    let replayed = replay.combat.as_ref().expect("combat is live after replay");
    assert!(
        replayed
            .attackers
            .iter()
            .any(|a| a.object_id == attacker && a.blocked),
        "CR 509.1h: the attacker's sticky blocked bit"
    );
    assert_eq!(
        replayed.blocker_to_attacker.get(&token),
        Some(&vec![attacker]),
        "CR 509.1g: the blocker -> attacker reverse lookup"
    );
    assert_eq!(
        replayed.blocker_assignments.get(&attacker),
        Some(&vec![token]),
        "CR 509.1g: the attacker -> blocker forward assignment"
    );
    assert_absent_from_block_history(&replay, token, "replayed");
    assert_absent_from_block_history(&replay, second_token, "replayed");
    assert_declared_block_history(&replay, declared, second_attacker, "replayed");
}

/// CR 509.4: Sizzling Barrage's "target creature that blocked this turn" reads
/// declared blockers only. A token blocking the same attacker as a declared
/// blocker is no more legal than one blocking alone.
///
/// The prompt is the subject: with one legal target left, the cast driver would
/// settle on it and report success, so a `.target_object(token)` negative proves
/// nothing.
#[test]
fn sizzling_barrage_cannot_target_a_token_that_entered_blocking() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P0, "Charging Ox", 3, 3).id();
    let second = scenario.add_creature(P0, "Second Attacker", 2, 2).id();
    let third = scenario.add_creature(P0, "Third Attacker", 2, 2).id();
    let declared_one = scenario.add_creature(P1, "Declared One", 2, 2).id();
    let declared_two = scenario.add_creature(P1, "Declared Two", 2, 2).id();
    let bystander = scenario.add_creature(P1, "Bystander", 2, 2).id();
    let mirror_match = scenario
        .add_spell_to_hand_from_oracle(P1, "Mirror Match", true, MIRROR_MATCH)
        .id();
    let barrage = scenario
        .add_spell_to_hand_from_oracle(P1, "Sizzling Barrage", true, SIZZLING_BARRAGE)
        .id();
    let mut runner = scenario.build();

    let tokens = resolve_mirror_match(
        &mut runner,
        mirror_match,
        &[first, second, third],
        &[(declared_one, second), (declared_two, third)],
        P1,
    );

    let cast = cast_action(runner.state(), barrage);
    runner
        .act(cast)
        .expect("declared blockers are legal targets");
    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!(
            "two legal targets must open the prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    let legal = &target_slots[0].legal_targets;
    assert_eq!(legal.len(), 2, "only the declared blockers: {legal:?}");
    assert!(legal.contains(&TargetRef::Object(declared_one)));
    assert!(legal.contains(&TargetRef::Object(declared_two)));
    for token in &tokens {
        assert!(
            !legal.contains(&TargetRef::Object(*token)),
            "CR 509.4: a token that entered blocking never \"blocked\""
        );
    }
    assert!(
        !legal.contains(&TargetRef::Object(bystander)),
        "a creature that took no part in combat is not a target"
    );

    // CR 509.1g: the declared blocker did block, and 4 damage kills the 2/2.
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(declared_one)),
        })
        .expect("a declared blocker is a legal target");
    runner.advance_until_stack_empty();
    assert_resolution_completed(&runner);
    assert_eq!(runner.state().objects[&declared_one].zone, Zone::Graveyard);
}

/// CR 509.4: with only a token blocking, nothing "blocked this turn", so the
/// spell cannot be cast at all, even though the token is blocking.
#[test]
fn sizzling_barrage_has_no_target_when_only_a_token_entered_blocking() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Charging Ox", 3, 3).id();
    let mirror_match = scenario
        .add_spell_to_hand_from_oracle(P1, "Mirror Match", true, MIRROR_MATCH)
        .id();
    let barrage = scenario
        .add_spell_to_hand_from_oracle(P1, "Sizzling Barrage", true, SIZZLING_BARRAGE)
        .id();
    let mut runner = scenario.build();

    // Reach guards live in the helper: the token is blocking, the attacker blocked.
    resolve_mirror_match(&mut runner, mirror_match, &[attacker], &[], P1);

    let cast = cast_action(runner.state(), barrage);
    let result = runner.act(cast);
    assert!(
        matches!(
            &result,
            Err(EngineError::ActionNotAllowed(message)) if message == "No legal targets available"
        ),
        "CR 509.4: a token that entered blocking is not a legal target"
    );
}

/// CR 509.4: the same unary read as a mass effect. Demonic Stench shrinks the
/// declared blocker and skips both tokens, including the one sharing its attacker.
#[test]
fn demonic_stench_skips_a_token_that_entered_blocking() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Charging Ox", 3, 3).id();
    let second = scenario.add_creature(P0, "Second Attacker", 2, 2).id();
    let declared = scenario.add_creature(P1, "Declared Blocker", 2, 2).id();
    let mirror_match = scenario
        .add_spell_to_hand_from_oracle(P1, "Mirror Match", true, MIRROR_MATCH)
        .id();
    let stench = scenario
        .add_spell_to_hand_from_oracle(P1, "Demonic Stench", true, DEMONIC_STENCH)
        .id();
    let mut runner = scenario.build();

    let tokens = resolve_mirror_match(
        &mut runner,
        mirror_match,
        &[attacker, second],
        &[(declared, second)],
        P1,
    );
    let toughness =
        |runner: &GameRunner, object: ObjectId| runner.state().objects[&object].toughness;
    assert_eq!(toughness(&runner, tokens[0]), Some(3));
    assert_eq!(toughness(&runner, tokens[1]), Some(2));
    assert_eq!(toughness(&runner, declared), Some(2));

    runner.cast(stench).resolve();
    runner.advance_until_stack_empty();
    assert_resolution_completed(&runner);

    assert_eq!(
        toughness(&runner, declared),
        Some(1),
        "CR 509.1g: the declared blocker \"blocked this turn\""
    );
    assert_eq!(
        toughness(&runner, tokens[0]),
        Some(3),
        "CR 509.4: a token that entered blocking never \"blocked\""
    );
    assert_eq!(
        toughness(&runner, tokens[1]),
        Some(2),
        "CR 509.4: sharing an attacker with a declared blocker changes nothing"
    );
}

/// CR 509.4: the `AttackedOrBlockedThisTurn` arm through a choice set. Two
/// declared blockers keep the prompt open, so the offered cards can be read.
#[test]
fn gideons_triumph_cannot_sacrifice_a_token_that_entered_blocking() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P0, "Charging Ox", 3, 3).id();
    let second = scenario.add_creature(P0, "Second Attacker", 2, 2).id();
    let third = scenario.add_creature(P0, "Third Attacker", 2, 2).id();
    let declared_one = scenario.add_creature(P1, "Declared One", 2, 2).id();
    let declared_two = scenario.add_creature(P1, "Declared Two", 2, 2).id();
    let mirror_match = scenario
        .add_spell_to_hand_from_oracle(P1, "Mirror Match", true, MIRROR_MATCH)
        .id();
    let triumph = scenario
        .add_spell_to_hand_from_oracle(P0, "Gideon's Triumph", true, GIDEONS_TRIUMPH)
        .id();
    let mut runner = scenario.build();

    let tokens = resolve_mirror_match(
        &mut runner,
        mirror_match,
        &[first, second, third],
        &[(declared_one, second), (declared_two, third)],
        P0,
    );

    let outcome = runner.cast(triumph).target_player(P1).resolve();
    let WaitingFor::EffectZoneChoice { player, cards, .. } = outcome.final_waiting_for() else {
        panic!(
            "two legal creatures must open the choice, got {:?}",
            outcome.final_waiting_for()
        );
    };
    assert_eq!(*player, P1, "the target opponent chooses");
    assert_eq!(cards.len(), 2, "only the declared blockers: {cards:?}");
    assert!(cards.contains(&declared_one));
    assert!(cards.contains(&declared_two));
    for token in &tokens {
        assert!(
            !cards.contains(token),
            "CR 509.4: a token that entered blocking never \"blocked\""
        );
    }

    runner
        .act(GameAction::SelectCards {
            cards: vec![declared_one],
        })
        .expect("a declared blocker is a legal choice");
    runner.advance_until_stack_empty();
    assert_resolution_completed(&runner);
    assert_eq!(runner.state().objects[&declared_one].zone, Zone::Graveyard);
    assert!(on_battlefield(runner.state(), declared_two));
    for token in &tokens {
        assert!(on_battlefield(runner.state(), *token));
    }
}

/// CR 509.4: with one declared blocker there is nothing to choose between, because
/// neither token is eligible. That blocker is sacrificed with no prompt.
#[test]
fn gideons_triumph_takes_the_only_declared_blocker_without_a_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Charging Ox", 3, 3).id();
    let second = scenario.add_creature(P0, "Second Attacker", 2, 2).id();
    let declared = scenario.add_creature(P1, "Declared Blocker", 2, 2).id();
    let mirror_match = scenario
        .add_spell_to_hand_from_oracle(P1, "Mirror Match", true, MIRROR_MATCH)
        .id();
    let triumph = scenario
        .add_spell_to_hand_from_oracle(P0, "Gideon's Triumph", true, GIDEONS_TRIUMPH)
        .id();
    let mut runner = scenario.build();

    let tokens = resolve_mirror_match(
        &mut runner,
        mirror_match,
        &[attacker, second],
        &[(declared, second)],
        P0,
    );

    runner.cast(triumph).target_player(P1).resolve();
    runner.advance_until_stack_empty();
    assert_resolution_completed(&runner);
    assert_eq!(runner.state().objects[&declared].zone, Zone::Graveyard);
    for token in &tokens {
        assert!(
            on_battlefield(runner.state(), *token),
            "CR 509.4: a token that entered blocking never \"blocked\""
        );
    }
}

/// CR 509.4: the combat-pair read. A token copy of Cathedral Membrane blocked
/// nothing, so it deals no damage when it dies, while P1's own Membrane, declared
/// on another attacker, dies to the same spell and deals 6.
#[test]
fn cathedral_membrane_token_copy_never_blocked() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_enchantment_from_oracle(P0, "Rolling Stones", ROLLING_STONES)
        .id();
    let membrane = scenario
        .add_creature_from_oracle(P0, "Cathedral Membrane", 0, 3, CATHEDRAL_MEMBRANE)
        .with_subtypes(vec!["Phyrexian", "Wall"])
        .id();
    let filler = scenario.add_creature(P0, "Sturdy Attacker", 3, 7).id();
    let declared_membrane = scenario
        .add_creature_from_oracle(P1, "Cathedral Membrane", 0, 3, CATHEDRAL_MEMBRANE)
        .with_subtypes(vec!["Phyrexian", "Wall"])
        .id();
    let mirror_match = scenario
        .add_spell_to_hand_from_oracle(P1, "Mirror Match", true, MIRROR_MATCH)
        .id();
    let first_murder = scenario
        .add_spell_to_hand_from_oracle(P1, "Murder", true, MURDER)
        .id();
    let second_murder = scenario
        .add_spell_to_hand_from_oracle(P1, "Murder", true, MURDER)
        .id();
    let mut runner = scenario.build();

    let tokens = resolve_mirror_match(
        &mut runner,
        mirror_match,
        &[membrane, filler],
        &[(declared_membrane, filler)],
        P1,
    );

    let murder = runner.cast(first_murder).target_object(tokens[0]).resolve();
    // CR 603.2 + CR 603.3: the token's dies trigger went on the stack and its
    // `DamageAll` resolved, so the `BlockPairing { SubjectIsBlocker, ThisCombat }`
    // population was evaluated and matched nothing.
    assert!(
        murder.events().iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::DamageAll,
                source_id,
                ..
            } if *source_id == tokens[0]
        )),
        "reach guard: the token copy's dies trigger must resolve; {:?}",
        murder.events()
    );
    runner.advance_until_stack_empty();
    assert_resolution_completed(&runner);
    assert!(
        !on_battlefield(runner.state(), tokens[0]),
        "reach guard: the token copy died during combat"
    );
    assert_eq!(
        runner.state().objects[&membrane].damage_marked,
        0,
        "CR 509.4: the token copy never \"blocked\" the attacker"
    );
    assert!(on_battlefield(runner.state(), membrane));

    advance_to_declare_blockers_and_give_priority(&mut runner, P1);
    runner
        .cast(second_murder)
        .target_object(declared_membrane)
        .resolve();
    runner.advance_until_stack_empty();
    assert_resolution_completed(&runner);
    assert_eq!(
        runner.state().objects[&filler].damage_marked,
        6,
        "CR 509.1g: the declared Membrane blocked this attacker"
    );
}

/// CR 509.4: "blocking" stays true. Abu Ja'far's "blocking or blocked by it" reads
/// the live pairing, so it destroys a token that is absent from every history
/// ledger. The token's absence is what proves the pairing alone matched.
#[test]
fn abu_jafar_destroys_a_token_that_entered_blocking() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let abu = scenario
        .add_creature_from_oracle(P0, "Abu Ja'far", 0, 1, ABU_JAFAR)
        .id();
    let second = scenario.add_creature(P0, "Second Attacker", 2, 2).id();
    let declared = scenario.add_creature(P1, "Declared Blocker", 2, 2).id();
    let mirror_match = scenario
        .add_spell_to_hand_from_oracle(P1, "Mirror Match", true, MIRROR_MATCH)
        .id();
    let shock = scenario
        .add_spell_to_hand_from_oracle(P1, "Shock", true, SHOCK)
        .id();
    let mut runner = scenario.build();

    let tokens = resolve_mirror_match(
        &mut runner,
        mirror_match,
        &[abu, second],
        &[(declared, second)],
        P1,
    );
    assert_absent_from_block_history(runner.state(), tokens[0], "before Abu dies");

    runner.cast(shock).target_object(abu).resolve();
    runner.advance_until_stack_empty();
    assert_resolution_completed(&runner);

    assert_eq!(runner.state().objects[&abu].zone, Zone::Graveyard);
    assert!(
        !on_battlefield(runner.state(), tokens[0]),
        "CR 509.4: the token was \"blocking\" Abu Ja'far when it died"
    );
    assert!(
        on_battlefield(runner.state(), declared),
        "a blocker of another attacker is not Abu Ja'far's partner"
    );
    assert!(
        on_battlefield(runner.state(), tokens[1]),
        "the other attacker's token is not Abu Ja'far's partner"
    );
}
