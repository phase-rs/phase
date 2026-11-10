use crate::types::ability::{
    Effect, EffectError, QuantityExpr, ResolvedAbility, TargetFilter, TokenAttackDestination,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 509.1g + CR 506.3e + CR 707.2: Resolve `Effect::CopyTokenBlockingAttacker`.
///
/// For each attacking creature matched by `source_filter`, create a token that's
/// a copy of it (CR 707.2) and put that token onto the battlefield blocking the
/// attacker it copies (CR 506.3e + CR 509.1g). Mirror Match is the canonical
/// card. The created token ids are republished to `state.last_created_token_ids`
/// so a composed delayed trigger can exile "those tokens" at end of combat via
/// `TargetFilter::LastCreated`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (source_filter, owner) = match &ability.effect {
        Effect::CopyTokenBlockingAttacker {
            source_filter,
            owner,
        } => (source_filter.clone(), owner.clone()),
        _ => {
            return Err(EffectError::MissingParam(
                "CopyTokenBlockingAttacker".to_string(),
            ))
        }
    };

    // CR 707.2: one copy-token instruction, whose copy sources are the attackers
    // the filter matches, delegated to the shared copy delivery so copiable
    // values, predefined abilities, and ETB events are handled identically to
    // every other copy-token effect. `Count = 1` per source gives exactly one
    // copy per attacker; the per-source entering-block binding below binds each
    // copy to its own attacker (CR 506.3e + CR 509.1g).
    //
    // CR 608.2c + 616.1: this is ONE batch owner for the whole instruction. The
    // shared driver's `PendingCopyTokenResolution` holds one batch per attacker
    // and delivers them in order, so an earlier copy that parks on a copied
    // as-enters replacement (Minion of the Wastes) holds every later copy; on
    // resume the drain continues in order and assigns the FULL created set to
    // `last_created_token_ids` — the "those tokens" a composed end-of-combat
    // exile snapshots. Driving each attacker as its own resolution (the prior
    // shape) started later members while an earlier one was parked and let each
    // resumed frame overwrite the referent with only its own subset.
    let copy_effect = Effect::CopyTokenOf {
        target: TargetFilter::Any,
        owner,
        source_filter: Some(source_filter),
        enters_attacking: false,
        attack_destination: TokenAttackDestination::ControllerChooses,
        tapped: false,
        count: QuantityExpr::Fixed { value: 1 },
        extra_keywords: vec![],
        additional_modifications: vec![],
    };
    let copy_ability =
        ResolvedAbility::new(copy_effect, vec![], ability.source_id, ability.controller);
    // CR 506.3e + CR 509.1g: each created copy is declared blocking the attacker
    // it copies, so the binding is the source's own id.
    crate::game::effects::token_copy::resolve_with_blocking_bindings(
        state,
        &copy_ability,
        events,
        Some,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::combat::{AttackTarget, AttackerInfo, CombatState};
    use crate::game::scenario::{GameRunner, GameScenario, P0, P1};
    use crate::game::zones::create_object;
    use crate::types::ability::{ControllerRef, FilterProp, TargetRef, TypedFilter};
    use crate::types::actions::GameAction;
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::game_state::{CastPaymentMode, WaitingFor};
    use crate::types::identifiers::CardId;
    use crate::types::identifiers::ObjectId;
    use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};
    use crate::types::phase::Phase;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    /// CR 506.3e + CR 509.1g: For each creature attacking the controller, a copy
    /// token is created and put onto the battlefield blocking that attacker. The
    /// attacker becomes blocked and the created tokens are published for the
    /// "those tokens" anaphor.
    #[test]
    fn copies_and_blocks_each_attacker() {
        let mut state = GameState::new_two_player(42);
        let defender = PlayerId(0); // Mirror Match's controller (defending player).
        let aggressor = PlayerId(1);

        // Two creatures controlled by the aggressor, attacking the defender.
        let make_attacker = |state: &mut GameState, name: &str| {
            let id = create_object(
                state,
                CardId(1),
                aggressor,
                name.to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec![],
            };
            obj.card_types = obj.base_card_types.clone();
            id
        };
        let atk_a = make_attacker(&mut state, "Grizzly Bears");
        let atk_b = make_attacker(&mut state, "Hill Giant");

        let mut combat = CombatState::default();
        combat.attackers.push(AttackerInfo::new(
            atk_a,
            AttackTarget::Player(defender),
            defender,
        ));
        combat.attackers.push(AttackerInfo::new(
            atk_b,
            AttackTarget::Player(defender),
            defender,
        ));
        state.combat = Some(combat);

        // Mirror Match: copy + block each creature attacking the controller.
        let source_filter =
            TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::Attacking {
                    defender: Some(ControllerRef::You),
                }]),
            );
        let ability = ResolvedAbility::new(
            Effect::CopyTokenBlockingAttacker {
                source_filter,
                owner: TargetFilter::Controller,
            },
            vec![],
            create_object(
                &mut state,
                CardId(2),
                defender,
                "Mirror Match".to_string(),
                Zone::Stack,
            ),
            defender,
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Two copy tokens were created, controlled by the defender.
        assert_eq!(state.last_created_token_ids.len(), 2);
        for token_id in &state.last_created_token_ids {
            let token = &state.objects[token_id];
            assert!(token.is_token, "created object is a token");
            assert_eq!(
                token.controller, defender,
                "copy is controlled by Mirror Match's controller"
            );
            assert!(!token.tapped, "blocking copy is not tapped");
        }

        // Both attackers are now blocked, each by exactly one created copy.
        let combat = state.combat.as_ref().unwrap();
        for atk in [atk_a, atk_b] {
            let info = combat
                .attackers
                .iter()
                .find(|a| a.object_id == atk)
                .unwrap();
            assert!(info.blocked, "attacker {atk:?} became blocked");
            assert_eq!(
                combat.blocker_assignments[&atk].len(),
                1,
                "one blocker per attacker"
            );
        }
        // The two blockers are the two created tokens.
        let blockers: std::collections::HashSet<_> =
            combat.blocker_to_attacker.keys().copied().collect();
        let created: std::collections::HashSet<_> =
            state.last_created_token_ids.iter().copied().collect();
        assert_eq!(blockers, created, "every created copy is a blocker");
    }

    /// CR 509.3d + CR 614.12a: Mirror Match copying a creature carries the
    /// copy's blocking assignment across the copied "as this creature enters,
    /// pay any amount of life" replacement (Minion of the Wastes). The copy is
    /// created but not published while the payment is unanswered; the declared
    /// block must be applied exactly once, at publication after the answer,
    /// emitting the attacker-side "becomes blocked" events.
    #[test]
    fn copied_entry_payment_keeps_the_entering_block() {
        // Full Oracle text, primary sources (Mirror Match, Minion of the Wastes).
        const MIRROR: &str = "Cast this spell only during the declare blockers step.\nFor each creature attacking you or a planeswalker you control, create a token that's a copy of that creature and that's blocking that creature. Exile those tokens at end of combat.";
        const MINION: &str = "Trample\nAs this creature enters, pay any amount of life.\nMinion of the Wastes's power and toughness are each equal to the life paid as it entered.";

        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Plains", "Plains", "Plains"]);
        scenario.with_library_top(P1, &["Island", "Island", "Island"]);
        // A legal creature that COULD block keeps the declare-blockers step (and
        // its priority window) in the turn even though this witness declares no
        // blocks — the engine otherwise rolls past an empty step.
        let _bystander = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
        // The attacker is a fixture that entered on an earlier turn: Minion's own
        // entry payment is not what this witness tests (the COPY's is). Seed its
        // remembered amount so it is a living 3/3 and may attack.
        let minion = scenario
            .add_creature_from_oracle(P0, "Minion of the Wastes", 0, 0, MINION)
            .id();
        let mirror = scenario
            .add_spell_to_hand_from_oracle(P1, "Mirror Match", true, MIRROR)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
                generic: 4,
            })
            .id();
        let islands: Vec<ObjectId> = (0..6)
            .map(|_| scenario.add_basic_land(P1, ManaColor::Blue))
            .collect();
        let mut runner = scenario.build();
        {
            let object = runner
                .state_mut()
                .objects
                .get_mut(&minion)
                .expect("the seeded attacker exists");
            object
                .chosen_attributes
                .push(crate::types::ability::ChosenAttribute::Number(3));
            runner.state_mut().layers_dirty.mark_full();
        }

        // Roll to the attacker declaration and attack the Mirror controller.
        for _ in 0..64 {
            match runner.state().waiting_for {
                WaitingFor::DeclareAttackers { .. } => break,
                WaitingFor::Priority { .. } => {
                    runner.act(GameAction::PassPriority).unwrap();
                }
                ref other => panic!("unexpected prompt before attackers: {other:?}"),
            }
        }
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![(minion, AttackTarget::Player(P1))],
                bands: vec![],
            })
            .expect("declare Minion as the attacker");

        // Empty declaration, then cast Mirror Match in the blockers step.
        for _ in 0..64 {
            match runner.state().waiting_for {
                WaitingFor::DeclareBlockers { .. } => break,
                WaitingFor::Priority { .. } => {
                    runner.act(GameAction::PassPriority).unwrap();
                }
                ref other => panic!("unexpected prompt before blockers: {other:?}"),
            }
        }
        runner
            .act(GameAction::DeclareBlockers {
                assignments: vec![],
            })
            .expect("declare no blockers");
        for _ in 0..8 {
            if matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { player, .. } if player == P1
            ) {
                break;
            }
            runner.act(GameAction::PassPriority).unwrap();
        }
        for island in islands {
            runner
                .act(GameAction::ActivateAbility {
                    source_id: island,
                    ability_index: 0,
                    payment_mode: CastPaymentMode::Auto,
                })
                .expect("tap an Island for Mirror Match");
        }
        let outcome = runner.cast(mirror).commit().resolve();

        // The copy's copied entry payment is the pause: the copy exists as the
        // pending entrant, but is not published (nor bound as a blocker) yet.
        let copy_id = match outcome.final_waiting_for() {
            WaitingFor::PayAmountChoice {
                player, source_id, ..
            } => {
                assert_eq!(*player, P1, "the copy's controller pays");
                *source_id
            }
            other => panic!("expected the copied entry payment, got {other:?}"),
        };
        {
            // The paused entrant is a liminal projection: decided, not yet
            // published into `objects`/`last_created_token_ids`.
            let entry = runner
                .state()
                .liminal_entries
                .get(&copy_id)
                .expect("the paused entrant exists as a liminal entry");
            assert_eq!(entry.name, "Minion of the Wastes");
            assert!(
                !runner.state().objects.contains_key(&copy_id),
                "an unanswered entrant is not published"
            );
            assert!(
                !runner.state().last_created_token_ids.contains(&copy_id),
                "an unanswered entrant is not published"
            );
            let combat = runner.state().combat.as_ref().unwrap();
            assert!(
                !combat.blocker_assignments.contains_key(&minion),
                "no blocker is bound before publication"
            );
        }

        // CR 614.12a: the answer completes the entry; the declared block lands
        // at publication, in this action's events.
        let answered = runner
            .act(GameAction::SubmitPayAmount { amount: 2 })
            .expect("answer the copied entry payment");
        runner.advance_until_stack_empty();

        {
            let copy = &runner.state().objects[&copy_id];
            assert_eq!(copy.zone, Zone::Battlefield, "the copy entered");
            assert!(copy.is_token, "the copy is a token");
            assert_eq!(
                copy.controller, P1,
                "the copy is Mirror Match's controller's"
            );
            assert_eq!(copy.name, "Minion of the Wastes");
            assert_eq!(
                (copy.power, copy.toughness),
                (Some(2), Some(2)),
                "the copy's own answered payment is its remembered P/T"
            );
        }
        assert_eq!(
            runner.state().last_created_token_ids,
            vec![copy_id],
            "the published copy keeps the \"those tokens\" anaphor"
        );
        let combat = runner.state().combat.as_ref().unwrap();
        assert_eq!(
            combat.blocker_assignments[&minion],
            vec![copy_id],
            "the published copy is the attacker's blocker, exactly once"
        );
        assert_eq!(combat.blocker_to_attacker[&copy_id], vec![minion]);
        assert!(
            combat
                .attackers
                .iter()
                .find(|a| a.object_id == minion)
                .unwrap()
                .blocked,
            "the attacker became blocked"
        );
        assert!(
            answered.events.iter().any(|event| matches!(
                event,
                GameEvent::AttackerBecameBlockedByEffect { attacker } if *attacker == minion
            )),
            "the bare attacker-side becomes-blocked event fires after the answer: {:?}",
            answered.events
        );
        assert!(
            answered.events.iter().any(|event| matches!(
                event,
                GameEvent::AttackerBecameBlockedByFilteredBlocker { attacker, blocker }
                    if *attacker == minion && *blocker == copy_id
            )),
            "the qualified attacker-side becomes-blocked event fires after the answer: {:?}",
            answered.events
        );
    }

    /// CR 616.1 + CR 509.3d: when a copy of a creature that "enters with three
    /// +1/+1 counters" meets two competing counter replacements, the entry parks
    /// on the CR 616.1 ordering choice. The declared entering block must survive
    /// that pause and apply at the counter-resume publication, with the
    /// attacker-side "becomes blocked" events.
    #[test]
    fn counter_pause_keeps_the_entering_block() {
        // Full Oracle text, primary sources.
        const MIRROR: &str = "Cast this spell only during the declare blockers step.\nFor each creature attacking you or a planeswalker you control, create a token that's a copy of that creature and that's blocking that creature. Exile those tokens at end of combat.";
        const WATCHDOG: &str = "Vigilance\nThis creature enters with three +1/+1 counters on it.";
        const HARDENED_SCALES: &str = "If one or more +1/+1 counters would be put on a creature you control, that many plus one +1/+1 counters are put on it instead.";
        const BRANCHING_EVOLUTION: &str = "If one or more +1/+1 counters would be put on a creature you control, twice that many +1/+1 counters are put on that creature instead.";

        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Plains", "Plains", "Plains"]);
        scenario.with_library_top(P1, &["Island", "Island", "Island"]);
        // The two simultaneously-applicable counter replacements that raise the
        // CR 616.1 ordering prompt for the copy P1 controls.
        scenario.add_enchantment_from_oracle(P1, "Hardened Scales", HARDENED_SCALES);
        scenario.add_enchantment_from_oracle(P1, "Branching Evolution", BRANCHING_EVOLUTION);
        let _bystander = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
        let watchdog = scenario
            .add_creature(P0, "Faithful Watchdog", 0, 0)
            .with_plus_counters(3)
            .from_oracle_text_with_keywords(&["Vigilance"], WATCHDOG)
            .id();
        let mirror = scenario
            .add_spell_to_hand_from_oracle(P1, "Mirror Match", true, MIRROR)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
                generic: 4,
            })
            .id();
        let islands: Vec<ObjectId> = (0..6)
            .map(|_| scenario.add_basic_land(P1, ManaColor::Blue))
            .collect();
        let mut runner = scenario.build();

        for _ in 0..64 {
            match runner.state().waiting_for {
                WaitingFor::DeclareAttackers { .. } => break,
                WaitingFor::Priority { .. } => {
                    runner.act(GameAction::PassPriority).unwrap();
                }
                ref other => panic!("unexpected prompt before attackers: {other:?}"),
            }
        }
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![(watchdog, AttackTarget::Player(P1))],
                bands: vec![],
            })
            .expect("declare the Watchdog as the attacker");

        for _ in 0..64 {
            match runner.state().waiting_for {
                WaitingFor::DeclareBlockers { .. } => break,
                WaitingFor::Priority { .. } => {
                    runner.act(GameAction::PassPriority).unwrap();
                }
                ref other => panic!("unexpected prompt before blockers: {other:?}"),
            }
        }
        runner
            .act(GameAction::DeclareBlockers {
                assignments: vec![],
            })
            .expect("declare no blockers");
        for _ in 0..8 {
            if matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { player, .. } if player == P1
            ) {
                break;
            }
            runner.act(GameAction::PassPriority).unwrap();
        }
        for island in islands {
            runner
                .act(GameAction::ActivateAbility {
                    source_id: island,
                    ability_index: 0,
                    payment_mode: CastPaymentMode::Auto,
                })
                .expect("tap an Island for Mirror Match");
        }
        let outcome = runner.cast(mirror).commit().resolve();

        // The counter replacements park the entry on their CR 616.1 ordering.
        match outcome.final_waiting_for() {
            WaitingFor::ReplacementChoice { candidates, .. } => assert_eq!(
                candidates.len(),
                2,
                "two competing +1/+1 replacements must raise the ordering choice"
            ),
            other => panic!("expected the counter-ordering pause, got {other:?}"),
        }
        let copy_id = runner
            .state()
            .objects
            .iter()
            .find_map(|(id, object)| {
                (object.is_token && object.controller == P1 && object.name == "Faithful Watchdog")
                    .then_some(*id)
            })
            .expect("the paused copy entrant exists");
        assert!(
            !runner.state().last_created_token_ids.contains(&copy_id),
            "the paused entrant is not published"
        );
        assert!(
            !runner
                .state()
                .combat
                .as_ref()
                .unwrap()
                .blocker_assignments
                .contains_key(&watchdog),
            "no blocker is bound before publication"
        );

        // CR 616.1: the answer resumes the finalize, which publishes the copy;
        // the declared block applies there, in this action's events.
        let answered = runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("answer the counter-ordering prompt");
        runner.advance_until_stack_empty();

        assert_eq!(runner.state().objects[&copy_id].zone, Zone::Battlefield);
        assert_eq!(
            runner.state().last_created_token_ids,
            vec![copy_id],
            "the published copy keeps the \"those tokens\" anaphor"
        );
        let combat = runner.state().combat.as_ref().unwrap();
        assert_eq!(
            combat.blocker_assignments[&watchdog],
            vec![copy_id],
            "the published copy is the attacker's blocker, exactly once"
        );
        assert_eq!(combat.blocker_to_attacker[&copy_id], vec![watchdog]);
        assert!(
            combat
                .attackers
                .iter()
                .find(|a| a.object_id == watchdog)
                .unwrap()
                .blocked,
            "the attacker became blocked"
        );
        assert!(
            answered.events.iter().any(|event| matches!(
                event,
                GameEvent::AttackerBecameBlockedByEffect { attacker } if *attacker == watchdog
            )),
            "the bare attacker-side becomes-blocked event fires after the counter answer: {:?}",
            answered.events
        );
        assert!(
            answered.events.iter().any(|event| matches!(
                event,
                GameEvent::AttackerBecameBlockedByFilteredBlocker { attacker, blocker }
                    if *attacker == watchdog && *blocker == copy_id
            )),
            "the qualified attacker-side becomes-blocked event fires after the counter answer: {:?}",
            answered.events
        );
    }

    const MIRROR_MATCH_TEXT: &str = "Cast this spell only during the declare blockers step.\nFor each creature attacking you or a planeswalker you control, create a token that's a copy of that creature and that's blocking that creature. Exile those tokens at end of combat.";
    const MINION_OF_THE_WASTES_TEXT: &str =
        "Trample\nAs this creature enters, pay any amount of life.\nMinion of the Wastes's power and toughness are each equal to the life paid as it entered.";

    /// Drive to P0's declare-attackers prompt, passing priority.
    fn pass_to_declare_attackers(runner: &mut GameRunner) {
        for _ in 0..64 {
            match runner.state().waiting_for {
                WaitingFor::DeclareAttackers { .. } => return,
                WaitingFor::OptionalEffectChoice { .. } => {
                    runner
                        .act(GameAction::DecideOptionalEffect { accept: false })
                        .unwrap();
                }
                WaitingFor::Priority { .. } => {
                    runner.act(GameAction::PassPriority).unwrap();
                }
                ref other => panic!("unexpected prompt before attackers: {other:?}"),
            }
        }
        panic!("declare-attackers prompt never arrived");
    }

    /// Pass priority through the empty block declaration and the post-declaration
    /// priority round until `player` holds priority in the declare-blockers step.
    fn pass_to_blockers_priority(runner: &mut GameRunner, player: PlayerId) {
        let mut blockers_step = false;
        for _ in 0..64 {
            match runner.state().waiting_for {
                WaitingFor::DeclareBlockers { .. } => {
                    blockers_step = true;
                    runner
                        .act(GameAction::DeclareBlockers {
                            assignments: vec![],
                        })
                        .expect("declare no blockers");
                }
                WaitingFor::OptionalEffectChoice { .. } => {
                    runner
                        .act(GameAction::DecideOptionalEffect { accept: false })
                        .unwrap();
                }
                WaitingFor::Priority { player: holder, .. }
                    if blockers_step && holder == player =>
                {
                    return;
                }
                WaitingFor::Priority { .. } => {
                    runner.act(GameAction::PassPriority).unwrap();
                }
                ref other => panic!("unexpected prompt before blockers: {other:?}"),
            }
        }
        panic!("blockers-step priority for {player:?} never arrived");
    }

    /// Tap `count` untapped Islands controlled by `player` so the following cast
    /// has mana available. Does not cast.
    fn tap_islands(runner: &mut GameRunner, player: PlayerId, count: usize) {
        let islands: Vec<ObjectId> = runner
            .state()
            .objects
            .iter()
            .filter(|(_, object)| {
                object.controller == player
                    && object.name == "Island"
                    && object.zone == Zone::Battlefield
                    && !object.tapped
            })
            .map(|(id, _)| *id)
            .take(count)
            .collect();
        assert_eq!(
            islands.len(),
            count,
            "enough untapped Islands are available"
        );
        for island in islands {
            runner
                .act(GameAction::ActivateAbility {
                    source_id: island,
                    ability_index: 0,
                    payment_mode: CastPaymentMode::Auto,
                })
                .expect("tap an Island");
        }
    }

    fn token_ids(runner: &GameRunner) -> Vec<ObjectId> {
        runner
            .state()
            .objects
            .iter()
            .filter_map(|(id, object)| object.is_token.then_some(*id))
            .collect()
    }

    /// CR 603.7c + 701.36a: drive the real combat phases to the end-of-combat
    /// step, assert the delayed "Exile those tokens at end of combat" trigger
    /// snapshotted the WHOLE batch (`state.last_created_token_ids`), then resolve
    /// it and assert every member — not just the last one — moved to exile.
    fn drive_to_end_of_combat_and_assert_exile(
        runner: &mut GameRunner,
        mirror: ObjectId,
        referent: &[ObjectId],
    ) {
        for _ in 0..128 {
            if runner.state().phase == Phase::EndCombat {
                break;
            }
            match runner.state().waiting_for {
                WaitingFor::Priority { .. } => {
                    runner.act(GameAction::PassPriority).unwrap();
                }
                ref other => panic!("unexpected prompt before end of combat: {other:?}"),
            }
        }
        assert_eq!(
            runner.state().phase,
            Phase::EndCombat,
            "combat reached the end-of-combat step"
        );
        assert_eq!(
            runner.state().last_created_token_ids,
            referent,
            "the referent still names the whole batch at the delayed-trigger window"
        );
        let expiry = runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.source_id == mirror)
            .expect("Mirror Match's printed end-of-combat exile trigger is on the stack");
        let captured: Vec<ObjectId> = expiry
            .ability()
            .expect("the delayed trigger carries its resolved ability")
            .targets
            .iter()
            .filter_map(|target| match target {
                TargetRef::Object(id) => Some(*id),
                TargetRef::Player(_) => None,
            })
            .collect();
        assert_eq!(
            captured, referent,
            "the delayed trigger's creation-time LastCreated snapshot is the whole batch"
        );

        let mut exiled: Vec<ObjectId> = Vec::new();
        for _ in 0..16 {
            if runner.state().stack.is_empty() {
                break;
            }
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            let action = runner.act(GameAction::PassPriority).unwrap();
            exiled.extend(action.events.iter().filter_map(|event| match event {
                GameEvent::ZoneChanged {
                    object_id,
                    from: Some(Zone::Battlefield),
                    to: Zone::Exile,
                    ..
                } => Some(*object_id),
                _ => None,
            }));
        }
        assert!(
            runner.state().stack.is_empty(),
            "the exile trigger resolved"
        );
        let mut exiled_sorted = exiled.clone();
        exiled_sorted.sort_by_key(|id| id.0);
        let mut referent_sorted = referent.to_vec();
        referent_sorted.sort_by_key(|id| id.0);
        assert_eq!(
            exiled_sorted, referent_sorted,
            "the trigger exiles the full batch, not just its last member"
        );
        for id in referent {
            assert!(
                !runner.state().battlefield.contains(id),
                "an exiled token copy is no longer on the battlefield"
            );
            // CR 111.7: a token that leaves the battlefield for exile ceases to
            // exist, so the engine drops the object after recording the move.
            assert!(
                runner
                    .state()
                    .objects
                    .get(id)
                    .is_none_or(|object| object.zone == Zone::Exile),
                "the copy left through the exile zone"
            );
        }
    }

    /// CR 608.2c + 616.1: Mirror Match's copies are ONE batch. With a Minion of
    /// the Wastes copy and an ordinary copy, the copy whose as-enters payment
    /// parks must hold every later member: no later copy may be created while
    /// the payment is unanswered, and once answered the drain continues in order
    /// and `last_created_token_ids` names the whole batch. Exercise both
    /// attacker orders (the parked member first and second).
    fn mirror_match_holds_later_copies_across_entry_payment(minion_first: bool) {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Plains", "Plains", "Plains"]);
        scenario.with_library_top(P1, &["Island", "Island", "Island"]);
        let _bystander = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
        // CR 707.2: copy sources enumerate in battlefield order, so the scenario
        // creation order is what decides whether the pausing (Minion) member is
        // the batch's first or second delivery.
        let minion;
        let bears;
        if minion_first {
            minion = scenario
                .add_creature_from_oracle(
                    P0,
                    "Minion of the Wastes",
                    0,
                    0,
                    MINION_OF_THE_WASTES_TEXT,
                )
                .id();
            bears = scenario.add_creature(P0, "Ornithopter", 0, 2).id();
        } else {
            bears = scenario.add_creature(P0, "Ornithopter", 0, 2).id();
            minion = scenario
                .add_creature_from_oracle(
                    P0,
                    "Minion of the Wastes",
                    0,
                    0,
                    MINION_OF_THE_WASTES_TEXT,
                )
                .id();
        }
        let mirror = scenario
            .add_spell_to_hand_from_oracle(P1, "Mirror Match", true, MIRROR_MATCH_TEXT)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
                generic: 4,
            })
            .id();
        for _ in 0..6 {
            scenario.add_basic_land(P1, ManaColor::Blue);
        }
        let mut runner = scenario.build();
        {
            let object = runner
                .state_mut()
                .objects
                .get_mut(&minion)
                .expect("the seeded attacker exists");
            object
                .chosen_attributes
                .push(crate::types::ability::ChosenAttribute::Number(3));
            runner.state_mut().layers_dirty.mark_full();
        }

        pass_to_declare_attackers(&mut runner);
        let attacks = if minion_first {
            vec![
                (minion, AttackTarget::Player(P1)),
                (bears, AttackTarget::Player(P1)),
            ]
        } else {
            vec![
                (bears, AttackTarget::Player(P1)),
                (minion, AttackTarget::Player(P1)),
            ]
        };
        runner
            .act(GameAction::DeclareAttackers {
                attacks,
                bands: vec![],
            })
            .expect("declare both attackers");
        pass_to_blockers_priority(&mut runner, P1);
        tap_islands(&mut runner, P1, 6);
        let outcome = runner.cast(mirror).commit().resolve();

        let paused = match outcome.final_waiting_for() {
            WaitingFor::PayAmountChoice {
                player, source_id, ..
            } => {
                assert_eq!(*player, P1, "the copy's controller pays");
                *source_id
            }
            other => panic!("expected the copied entry payment, got {other:?}"),
        };
        assert_eq!(
            runner.state().liminal_entries[&paused].name,
            "Minion of the Wastes"
        );
        if minion_first {
            assert!(
                token_ids(&runner).is_empty(),
                "no later copy may be created while the first copy's payment is parked"
            );
        } else {
            let created = token_ids(&runner);
            assert_eq!(
                created.len(),
                1,
                "only the earlier (non-pausing) copy exists while the Minion copy parks"
            );
            assert_eq!(runner.state().objects[&created[0]].name, "Ornithopter");
            assert_eq!(
                runner.state().combat.as_ref().unwrap().blocker_assignments[&bears],
                vec![created[0]],
                "the earlier copy is published and bound while the later copy parks"
            );
        }

        let answered = runner
            .act(GameAction::SubmitPayAmount { amount: 5 })
            .expect("answer the copied entry payment");
        runner.advance_until_stack_empty();

        let created = token_ids(&runner);
        assert_eq!(created.len(), 2, "both copies exist after the resume");
        let combat = runner.state().combat.as_ref().unwrap();
        assert_eq!(
            combat.blocker_assignments[&minion].len(),
            1,
            "the Minion attacker has exactly one blocker"
        );
        assert_eq!(
            combat.blocker_assignments[&bears].len(),
            1,
            "the ordinary attacker has exactly one blocker"
        );
        for (attacker, expected_name) in [(minion, "Minion of the Wastes"), (bears, "Ornithopter")]
        {
            let blocker = combat.blocker_assignments[&attacker][0];
            assert_eq!(runner.state().objects[&blocker].name, expected_name);
            assert_eq!(
                combat.blocker_to_attacker[&blocker],
                vec![attacker],
                "each copy blocks the attacker it copies"
            );
        }
        let ordered_attackers = if minion_first {
            [minion, bears]
        } else {
            [bears, minion]
        };
        let expected_referent: Vec<ObjectId> = ordered_attackers
            .iter()
            .map(|attacker| combat.blocker_assignments[attacker][0])
            .collect();
        assert_eq!(
            runner.state().last_created_token_ids,
            expected_referent,
            "the whole batch, in creation order, is the \"those tokens\" referent"
        );
        assert!(
            answered.events.iter().any(|event| matches!(
                event,
                GameEvent::AttackerBecameBlockedByEffect { attacker } if *attacker == minion
            )),
            "the parked member's block publishes on the answer: {:?}",
            answered.events
        );

        let referent = runner.state().last_created_token_ids.clone();
        drive_to_end_of_combat_and_assert_exile(&mut runner, mirror, &referent);
    }

    #[test]
    fn mirror_match_holds_later_copy_when_minion_copy_parks_first() {
        mirror_match_holds_later_copies_across_entry_payment(true);
    }

    #[test]
    fn mirror_match_holds_later_copy_when_minion_copy_parks_second() {
        mirror_match_holds_later_copies_across_entry_payment(false);
    }

    /// CR 608.2c: two Minion copies each park on their own as-enters payment.
    /// The second copy must not begin before the first is answered, and the
    /// final referent names both copies in attacker order.
    #[test]
    fn mirror_match_two_parked_minion_copies_keep_one_batch() {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Plains", "Plains", "Plains"]);
        scenario.with_library_top(P1, &["Island", "Island", "Island"]);
        let _bystander = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
        let first = scenario
            .add_creature_from_oracle(P0, "Minion of the Wastes", 0, 0, MINION_OF_THE_WASTES_TEXT)
            .id();
        let second = scenario
            .add_creature_from_oracle(P0, "Minion of the Wastes", 0, 0, MINION_OF_THE_WASTES_TEXT)
            .id();
        let mirror = scenario
            .add_spell_to_hand_from_oracle(P1, "Mirror Match", true, MIRROR_MATCH_TEXT)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
                generic: 4,
            })
            .id();
        for _ in 0..6 {
            scenario.add_basic_land(P1, ManaColor::Blue);
        }
        let mut runner = scenario.build();
        for attacker in [first, second] {
            let object = runner.state_mut().objects.get_mut(&attacker).unwrap();
            object
                .chosen_attributes
                .push(crate::types::ability::ChosenAttribute::Number(3));
        }
        runner.state_mut().layers_dirty.mark_full();

        pass_to_declare_attackers(&mut runner);
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![
                    (first, AttackTarget::Player(P1)),
                    (second, AttackTarget::Player(P1)),
                ],
                bands: vec![],
            })
            .expect("declare both Minions");
        pass_to_blockers_priority(&mut runner, P1);
        tap_islands(&mut runner, P1, 6);
        let outcome = runner.cast(mirror).commit().resolve();

        let first_copy = match outcome.final_waiting_for() {
            WaitingFor::PayAmountChoice { source_id, .. } => *source_id,
            other => panic!("expected the first copied entry payment, got {other:?}"),
        };
        assert_eq!(
            runner.state().liminal_entries[&first_copy].name,
            "Minion of the Wastes"
        );
        assert!(
            token_ids(&runner).is_empty(),
            "the second copy must not start while the first copy's payment parks"
        );

        let answered_first = runner
            .act(GameAction::SubmitPayAmount { amount: 5 })
            .expect("answer the first copied entry payment");
        let next_waiting = answered_first.waiting_for.clone();
        let second_copy = match next_waiting {
            WaitingFor::PayAmountChoice { source_id, .. } => source_id,
            other => {
                // The drain may need priority before surfacing the next prompt.
                let mut found = None;
                for _ in 0..16 {
                    match runner.state().waiting_for.clone() {
                        WaitingFor::Priority { .. } => {
                            runner.act(GameAction::PassPriority).unwrap();
                        }
                        WaitingFor::PayAmountChoice { source_id, .. } => {
                            found = Some(source_id);
                            break;
                        }
                        _ => break,
                    }
                }
                found.unwrap_or_else(|| {
                    panic!("expected the second copied entry payment, got {other:?}")
                })
            }
        };
        assert_ne!(first_copy, second_copy, "each Minion copy parks separately");
        let published = token_ids(&runner);
        assert_eq!(published.len(), 1, "only the first copy is published");
        let combat = runner.state().combat.as_ref().unwrap();
        assert_eq!(
            combat.blocker_assignments[&first],
            vec![published[0]],
            "the first copy blocks its own attacker after its answer"
        );
        assert!(
            !combat.blocker_assignments.contains_key(&second),
            "the second attacker is still unblocked while the second copy parks"
        );

        runner
            .act(GameAction::SubmitPayAmount { amount: 5 })
            .expect("answer the second copied entry payment");
        runner.advance_until_stack_empty();

        let created = token_ids(&runner);
        assert_eq!(created.len(), 2);
        let combat = runner.state().combat.as_ref().unwrap();
        let mut blockers_by_attacker = Vec::new();
        for attacker in [first, second] {
            assert_eq!(
                combat.blocker_assignments[&attacker].len(),
                1,
                "each Minion attacker has exactly one blocker"
            );
            let blocker = combat.blocker_assignments[&attacker][0];
            assert_eq!(combat.blocker_to_attacker[&blocker], vec![attacker]);
            blockers_by_attacker.push(blocker);
        }
        assert_ne!(blockers_by_attacker[0], blockers_by_attacker[1]);
        assert_eq!(
            runner.state().last_created_token_ids,
            blockers_by_attacker,
            "both parked copies are named, in attacker order"
        );
    }

    /// CR 509.4 + CR 614.6: Mystic Reflection's "they enter as copies of the
    /// chosen creature" modifies the entrants' characteristics inside Mirror
    /// Match's creation event; it must not drop the declared entering block.
    #[test]
    fn mystic_reflection_copy_keeps_the_entering_block() {
        const MYSTIC: &str = "Choose target nonlegendary creature. The next time one or more creatures or planeswalkers enter this turn, they enter as copies of the chosen creature.";

        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Plains", "Plains", "Plains"]);
        scenario.with_library_top(P1, &["Island", "Island", "Island"]);
        let dreadmaw = scenario.add_creature(P1, "Colossal Dreadmaw", 6, 6).id();
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let mystic = scenario
            .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC)
            .id();
        let mirror = scenario
            .add_spell_to_hand_from_oracle(P1, "Mirror Match", true, MIRROR_MATCH_TEXT)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
                generic: 4,
            })
            .id();
        for _ in 0..6 {
            scenario.add_basic_land(P1, ManaColor::Blue);
        }
        let mut runner = scenario.build();

        // The next entrants this turn enter as copies of the Dreadmaw.
        runner.cast(mystic).target_object(dreadmaw).resolve();
        pass_to_declare_attackers(&mut runner);
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![(bears, AttackTarget::Player(P1))],
                bands: vec![],
            })
            .expect("declare the attacker");
        pass_to_blockers_priority(&mut runner, P1);
        tap_islands(&mut runner, P1, 6);
        let _outcome = runner.cast(mirror).commit().resolve();
        runner.advance_until_stack_empty();

        let created = token_ids(&runner);
        assert_eq!(created.len(), 1, "Mirror Match created one copy");
        let copy = created[0];
        let object = &runner.state().objects[&copy];
        assert_eq!(object.name, "Colossal Dreadmaw", "Mystic replaced the copy");
        assert_eq!((object.power, object.toughness), (Some(6), Some(6)));
        assert_eq!(
            runner.state().last_created_token_ids,
            vec![copy],
            "the anaphor keeps the created copy"
        );
        let combat = runner.state().combat.as_ref().unwrap();
        assert_eq!(
            combat.blocker_assignments[&bears],
            vec![copy],
            "the Mystic copy still enters blocking the attacker"
        );
        assert_eq!(combat.blocker_to_attacker[&copy], vec![bears]);
        assert!(combat
            .attackers
            .iter()
            .any(|a| a.object_id == bears && a.blocked));
    }

    /// CR 614.1a + CR 509.4 + Chatterfang ruling: "those tokens plus that many
    /// 1/1 green Squirrel creature tokens" is one modified event; the creating
    /// effect's other instruction (Mirror Match's entering block) applies to the
    /// Squirrels too.
    #[test]
    fn chatterfang_appended_squirrels_inherit_the_entering_block() {
        const CHATTERFANG: &str = "Forestwalk (This creature can't be blocked as long as defending player controls a Forest.)\nIf one or more tokens would be created under your control, those tokens plus that many 1/1 green Squirrel creature tokens are created instead.\n{B}, Sacrifice X Squirrels: Target creature gets +X/-X until end of turn.";

        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Plains", "Plains", "Plains"]);
        scenario.with_library_top(P1, &["Island", "Island", "Island"]);
        scenario.add_creature_from_oracle(P1, "Chatterfang, Squirrel General", 3, 3, CHATTERFANG);
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let mirror = scenario
            .add_spell_to_hand_from_oracle(P1, "Mirror Match", true, MIRROR_MATCH_TEXT)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
                generic: 4,
            })
            .id();
        for _ in 0..6 {
            scenario.add_basic_land(P1, ManaColor::Blue);
        }
        let mut runner = scenario.build();

        pass_to_declare_attackers(&mut runner);
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![(bears, AttackTarget::Player(P1))],
                bands: vec![],
            })
            .expect("declare the attacker");
        pass_to_blockers_priority(&mut runner, P1);
        tap_islands(&mut runner, P1, 6);
        let outcome = runner.cast(mirror).commit().resolve();
        runner.advance_until_stack_empty();

        let created = token_ids(&runner);
        assert_eq!(created.len(), 2, "one copy plus one Squirrel");
        let mut copy = None;
        let mut squirrel = None;
        for id in &created {
            let object = &runner.state().objects[id];
            match object.name.as_str() {
                "Grizzly Bears" => copy = Some(*id),
                "Squirrel" => squirrel = Some(*id),
                other => panic!("unexpected created token {other}"),
            }
        }
        let copy = copy.expect("the copy exists");
        let squirrel = squirrel.expect("Chatterfang appended a Squirrel");
        assert!(runner.state().objects[&squirrel].is_token);
        assert_eq!(runner.state().objects[&squirrel].power, Some(1));

        let combat = runner.state().combat.as_ref().unwrap();
        let blockers = &combat.blocker_assignments[&bears];
        assert_eq!(
            blockers.len(),
            2,
            "the copy and the appended Squirrel both enter blocking the attacker"
        );
        assert!(blockers.contains(&copy));
        assert!(blockers.contains(&squirrel));
        for blocker in blockers {
            assert_eq!(combat.blocker_to_attacker[blocker], vec![bears]);
        }
        assert_eq!(
            runner.state().last_created_token_ids.len(),
            2,
            "the whole modified event is the \"those tokens\" referent"
        );
        for id in [copy, squirrel] {
            assert!(runner.state().last_created_token_ids.contains(&id));
        }
        assert!(
            outcome.events().iter().any(|event| matches!(
                event,
                GameEvent::AttackerBecameBlockedByFilteredBlocker { attacker, blocker }
                    if *attacker == bears && *blocker == squirrel
            )),
            "the Squirrel's inherited block emits the attacker-side event: {:?}",
            outcome.events()
        );
    }

    const COPTER_TEXT: &str = "Flying\nWhenever this Vehicle attacks or blocks, you may draw a card. If you do, discard a card.\nCrew 1 (Tap any number of creatures you control with total power 1 or more: This Vehicle becomes an artifact creature until end of turn.)";
    const MONUMENT_TEXT: &str = "Creatures you control get +1/+1 and have flying and indestructible.\nAt the beginning of your upkeep, sacrifice a creature. If you can't, sacrifice this artifact.";
    const MYSTIC_TEXT: &str = "Choose target nonlegendary creature. The next time one or more creatures or planeswalkers enter this turn, they enter as copies of the chosen creature.";
    const CHATTERFANG_TEXT: &str = "Forestwalk (This creature can't be blocked as long as defending player controls a Forest.)\nIf one or more tokens would be created under your control, those tokens plus that many 1/1 green Squirrel creature tokens are created instead.\n{B}, Sacrifice X Squirrels: Target creature gets +X/-X until end of turn.";

    /// CR 702.122a: crew `vehicle` by tapping `crewers` (announce, then pay).
    fn crew_vehicle(runner: &mut GameRunner, vehicle: ObjectId, crewers: &[ObjectId]) {
        runner
            .act(GameAction::CrewVehicle {
                vehicle_id: vehicle,
                creature_ids: vec![],
            })
            .expect("enter crew selection");
        runner
            .act(GameAction::CrewVehicle {
                vehicle_id: vehicle,
                creature_ids: crewers.to_vec(),
            })
            .expect("pay the crew cost");
        runner.advance_until_stack_empty();
    }

    fn add_copter(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
        scenario
            .add_creature(player, "Smuggler's Copter", 3, 3)
            .as_artifact()
            .with_subtypes(vec!["Vehicle"])
            .from_oracle_text(COPTER_TEXT)
            .id()
    }

    /// CR 707.2 + CR 614.6 + CR 509.4: a crewed Smuggler's Copter copy is a
    /// NONCREATURE artifact (the crew type change is not copiable), so its own
    /// declared block cannot bind; Chatterfang's appended Squirrel member is a
    /// creature and must still block the attacker, and the end-of-combat exile
    /// must name the whole produced set.
    #[test]
    fn crewed_copter_copy_keeps_the_appended_squirrels_block_and_exile() {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Plains", "Plains", "Plains"]);
        scenario.with_library_top(P1, &["Island", "Island", "Island"]);
        let copter = add_copter(&mut scenario, P0);
        let crewer = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        scenario.add_creature_from_oracle(
            P1,
            "Chatterfang, Squirrel General",
            3,
            3,
            CHATTERFANG_TEXT,
        );
        // 2/2 flying indestructible Squirrel survives the 3/3 copter's damage.
        scenario.add_artifact_from_oracle(P1, "Eldrazi Monument", MONUMENT_TEXT);
        let mirror = scenario
            .add_spell_to_hand_from_oracle(P1, "Mirror Match", true, MIRROR_MATCH_TEXT)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
                generic: 4,
            })
            .id();
        for _ in 0..6 {
            scenario.add_basic_land(P1, ManaColor::Blue);
        }
        let mut runner = scenario.build();

        crew_vehicle(&mut runner, copter, &[crewer]);
        pass_to_declare_attackers(&mut runner);
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![(copter, AttackTarget::Player(P1))],
                bands: vec![],
            })
            .expect("declare the crewed Copter");
        pass_to_blockers_priority(&mut runner, P1);
        tap_islands(&mut runner, P1, 6);
        runner.cast(mirror).commit().resolve();
        runner.advance_until_stack_empty();

        let created = token_ids(&runner);
        assert_eq!(created.len(), 2, "one copy plus one Squirrel");
        let mut copter_copy = None;
        let mut squirrel = None;
        for id in &created {
            let object = &runner.state().objects[id];
            match object.name.as_str() {
                "Smuggler's Copter" => copter_copy = Some(*id),
                "Squirrel" => squirrel = Some(*id),
                other => panic!("unexpected created token {other}"),
            }
        }
        let copter_copy = copter_copy.expect("the Copter copy exists");
        let squirrel = squirrel.expect("Chatterfang appended a Squirrel");
        // CR 707.2: the crew animation is not copied; only the Squirrel member
        // can block.
        assert!(
            !runner.state().objects[&copter_copy]
                .card_types
                .core_types
                .contains(&CoreType::Creature),
            "the copy of a crewed Vehicle is a noncreature artifact"
        );
        assert!(runner.state().objects[&squirrel]
            .card_types
            .core_types
            .contains(&CoreType::Creature));

        let combat = runner.state().combat.as_ref().unwrap();
        assert_eq!(
            combat.blocker_assignments[&copter],
            vec![squirrel],
            "the appended Squirrel member is the attacker's blocker"
        );
        assert_eq!(combat.blocker_to_attacker[&squirrel], vec![copter]);
        assert!(
            !combat.blocker_to_attacker.contains_key(&copter_copy),
            "the noncreature copy establishes no block"
        );
        assert_eq!(
            runner.state().last_created_token_ids,
            vec![copter_copy, squirrel],
            "the whole produced set is the \"those tokens\" referent"
        );
        drive_to_end_of_combat_and_assert_exile(&mut runner, mirror, &[copter_copy, squirrel]);
    }

    /// CR 614.6 + CR 509.4: Mystic Reflection rebuilds the appended member into
    /// a copy of the chosen creature. The member's blocking binding must survive
    /// the single-batch projection / reconstruction and bind the entrant.
    #[test]
    fn mystic_reflection_on_the_appended_member_keeps_its_block_and_exile() {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Plains", "Plains", "Plains"]);
        scenario.with_library_top(P1, &["Island", "Island", "Island"]);
        let copter = add_copter(&mut scenario, P0);
        let crewer = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let dreadmaw = scenario.add_creature(P1, "Colossal Dreadmaw", 6, 6).id();
        scenario.add_creature_from_oracle(
            P1,
            "Chatterfang, Squirrel General",
            3,
            3,
            CHATTERFANG_TEXT,
        );
        // A legal flying blocker for the flying attacker (and +1/+1 to the
        // rebuilt member), so the declare-blockers step is actually offered.
        scenario.add_artifact_from_oracle(P1, "Eldrazi Monument", MONUMENT_TEXT);
        let mystic = scenario
            .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_TEXT)
            .id();
        let mirror = scenario
            .add_spell_to_hand_from_oracle(P1, "Mirror Match", true, MIRROR_MATCH_TEXT)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
                generic: 4,
            })
            .id();
        for _ in 0..6 {
            scenario.add_basic_land(P1, ManaColor::Blue);
        }
        let mut runner = scenario.build();

        // The next creatures to enter become copies of the Dreadmaw.
        runner.cast(mystic).target_object(dreadmaw).resolve();
        crew_vehicle(&mut runner, copter, &[crewer]);
        pass_to_declare_attackers(&mut runner);
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![(copter, AttackTarget::Player(P1))],
                bands: vec![],
            })
            .expect("declare the crewed Copter");
        pass_to_blockers_priority(&mut runner, P1);
        tap_islands(&mut runner, P1, 6);
        runner.cast(mirror).commit().resolve();
        runner.advance_until_stack_empty();

        let created = token_ids(&runner);
        assert_eq!(
            created.len(),
            2,
            "one copy plus the rebuilt appended member"
        );
        let mut copter_copy = None;
        let mut member = None;
        for id in &created {
            let object = &runner.state().objects[id];
            match object.name.as_str() {
                "Smuggler's Copter" => copter_copy = Some(*id),
                "Colossal Dreadmaw" => member = Some(*id),
                other => panic!("unexpected created token {other}"),
            }
        }
        let copter_copy = copter_copy.expect("the Copter copy exists");
        let member = member.expect("Mystic rebuilt the appended Squirrel as a Dreadmaw copy");
        assert!(!runner.state().objects[&copter_copy]
            .card_types
            .core_types
            .contains(&CoreType::Creature));
        assert_eq!(runner.state().objects[&member].power, Some(7));

        let combat = runner.state().combat.as_ref().unwrap();
        assert_eq!(
            combat.blocker_assignments[&copter],
            vec![member],
            "the rebuilt appended member still blocks the attacker it was created against"
        );
        assert_eq!(combat.blocker_to_attacker[&member], vec![copter]);
        assert_eq!(
            runner.state().last_created_token_ids,
            vec![copter_copy, member]
        );
        drive_to_end_of_combat_and_assert_exile(&mut runner, mirror, &[copter_copy, member]);
    }
}
