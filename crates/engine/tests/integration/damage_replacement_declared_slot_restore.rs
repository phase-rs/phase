//! CR 614.9 + CR 115.1a + CR 601.2c: a damage-redirection replacement declares
//! its announced target slots through `damage_replacement_target_roles`
//! (`DeclaredSource`, `OriginalRecipient`, `RedirectRecipient`, in that
//! declaration order), not through a generic `target` field.
//!
//! `Effect::CreateDamageReplacement` has NO `target` field, so
//! `extract_target_filter_from_effect` answers `None` for it and the restore
//! guard's `declared_slot_announcement_filter` used to recover no filter for
//! any of those slots. The recorded-characteristics check therefore never ran
//! for a parked Harm's Way cast: a save could keep the redirect recipient's id,
//! zone and pin while rewriting its recorded characteristics to an
//! enchantment-only permanent, pass restore, and publish the contradictory
//! target into the cast history.
//!
//! These witnesses drive the production cast pipeline: Harm's Way is cast with
//! its "any target" redirect recipient chosen and parked at manual mana payment
//! (CR 601.2h), which is exactly the state a save at the payment boundary
//! captures.

use engine::ai_support::legal_actions_full;
use engine::game::game_object::ObjectIncarnationRefExt;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{DamageRedirectTarget, Effect, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{
    ActivationTargetFact, AnnouncedTargetFactConflict, CastPaymentMode, PersistedGameState,
    PersistedRestoreError, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim Oracle text (Scryfall): Harm's Way, an instant.
const HARMS_WAY: &str = "The next 2 damage that a source of your choice would deal to you and/or permanents you control this turn is dealt to any target instead.";

struct ParkedHarmsWay {
    runner: GameRunner,
    spell: ObjectId,
    recipient: ObjectId,
    plains: ObjectId,
}

/// CR 601.2c + CR 115.1a: cast Harm's Way with its "any target" redirect
/// recipient chosen (a creature P1 controls) and `CastPaymentMode::Manual`, so
/// the cast stops at `WaitingFor::ManaPayment` holding the announced-target
/// latch.
fn park_harms_way_at_manual_payment() -> ParkedHarmsWay {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let recipient = scenario.add_creature(P1, "Redirect Bear", 2, 2).id();
    // White source for Harm's Way's {W}.
    let plains = scenario.add_basic_land(P0, ManaColor::White);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Harm's Way", true, HARMS_WAY)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the manual Harm's Way cast begins");
    // CR 115.1a + CR 601.2c: answer the redirect-recipient slot with the chosen
    // creature. Three candidates are legal (both players and the Bear), so the
    // engine asks rather than auto-selecting.
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(recipient)),
                    })
                    .expect("CR 115.4: the Bear is a legal 'any target'");
            }
            WaitingFor::ManaPayment { player, .. } => {
                assert_eq!(player, P0, "the caster pays");
                break;
            }
            other => panic!("unexpected prompt while announcing Harm's Way: {other:?}"),
        }
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment { player: P0, .. }
        ),
        "reach-guard: the cast parks at manual payment, got {:?}",
        runner.state().waiting_for
    );
    ParkedHarmsWay {
        runner,
        spell,
        recipient,
        plains,
    }
}

/// CR 614.9 + CR 115.1: the parsed effect is the chosen-target redirection whose
/// redirect recipient is the unrestricted `Any` the role authority surfaces.
fn assert_redirect_recipient_shape(runner: &GameRunner, spell: ObjectId) {
    let ability = runner.state().objects[&spell]
        .abilities
        .iter()
        .find(|ability| {
            matches!(
                ability.effect.as_ref(),
                Effect::CreateDamageReplacement {
                    redirect_to: Some(DamageRedirectTarget::ChosenTarget),
                    ..
                }
            )
        })
        .expect("reach-guard: Harm's Way declares a chosen-target redirection");
    let Effect::CreateDamageReplacement {
        redirect_object_filter,
        recipient_object_filter,
        source_filter,
        ..
    } = ability.effect.as_ref()
    else {
        unreachable!("reach-guard: the effect was matched above");
    };
    assert_eq!(
        redirect_object_filter.as_ref(),
        Some(&TargetFilter::Any),
        "CR 115.4: Harm's Way's redirect recipient is 'any target'"
    );
    assert!(
        recipient_object_filter.is_none(),
        "'to you' names no original-recipient slot: {recipient_object_filter:?}"
    );
    assert!(
        matches!(source_filter, Some(TargetFilter::ChosenDamageSource { .. })),
        "reach-guard: 'a source of your choice' is chosen on resolution (CR 609.7a): \
         {source_filter:?}"
    );
    assert!(
        engine::types::ability::damage_replacement_target_roles(&ability.effect)
            .is_some_and(|roles| roles.len() == 1),
        "reach-guard: the redirect recipient is the ONLY declared role — the \
         resolution-chosen source contributes no slot"
    );
}

/// The one object fact the parked cast's announcement latch carries.
fn parked_latch_fact(runner: &GameRunner) -> &ActivationTargetFact {
    let pending = runner
        .state()
        .pending_cast
        .as_deref()
        .expect("reach-guard: the manual payment park keeps its pending cast");
    let facts = pending
        .committed_target_facts
        .as_deref()
        .expect("reach-guard: the announcement latch is populated at the payment park");
    assert_eq!(
        facts.len(),
        1,
        "reach-guard: Harm's Way declares exactly one target slot"
    );
    &facts[0]
}

/// Save the parked cast through the authoritative capture and read it back.
fn save_and_restore(
    runner: &GameRunner,
) -> Result<engine::types::game_state::GameState, PersistedRestoreError> {
    let saved = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("the parked Harm's Way cast serializes");
    let restored: PersistedGameState =
        serde_json::from_str(&saved).expect("the parked Harm's Way cast deserializes");
    restored.into_game_state()
}

/// RESUMED PAYMENT — pay the parked cast from the restored state.
fn resume_payment(runner: &mut GameRunner, plains: ObjectId) {
    let (_, _, grouped) = legal_actions_full(runner.state());
    let selection = grouped
        .get(&plains)
        .into_iter()
        .flatten()
        .find_map(|action| match action {
            GameAction::TapLandForMana { selection } => Some(selection.clone()),
            _ => None,
        })
        .expect("the restored payment prompt offers the Plains' own {T}: Add {W}");
    runner
        .act(GameAction::TapLandForMana { selection })
        .expect("the restored cast takes the mana it announced it would");
    runner
        .act(GameAction::PassPriority)
        .expect("the restored manual payment finalizes");
}

/// WITNESS (control): an UNTOUCHED Harm's Way payment save restores, resumes its
/// payment, reaches the stack and resolves to its CR 609.7a source choice — the
/// resumed cast publishes the announced redirect recipient.
#[test]
fn untouched_harms_way_payment_save_restores_and_resolves() {
    let ParkedHarmsWay {
        runner,
        spell,
        recipient,
        plains,
    } = park_harms_way_at_manual_payment();
    assert_redirect_recipient_shape(&runner, spell);

    // Reach guard: the latch IS populated and states the announcement-time axes
    // the recorded-characteristics check reads.
    match parked_latch_fact(&runner) {
        ActivationTargetFact::Object { id, zone, lki, .. } => {
            assert_eq!(
                *id, recipient,
                "the latch names the chosen redirect recipient"
            );
            assert_eq!(
                *zone,
                engine::types::game_state::AnnouncedTargetZone::Known(Zone::Battlefield),
                "CR 601.2c: the recipient was announced on the battlefield"
            );
            assert!(
                lki.card_types.contains(&CoreType::Creature),
                "reach-guard: the recorded snapshot says creature: {:?}",
                lki.card_types
            );
        }
        other => panic!("reach-guard: the redirect recipient is an object, got {other:?}"),
    }

    let restored = save_and_restore(&runner)
        .expect("CR 601.2c: an untouched Harm's Way payment save is admitted");
    let mut runner = GameRunner::from_state(restored);
    resume_payment(&mut runner, plains);
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Stack,
        "reach-guard: the resumed cast reached the stack"
    );

    runner.advance_until_stack_empty();
    let WaitingFor::DamageSourceChoice {
        player, options, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "CR 609.7a: the resumed Harm's Way resolves to its source choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "the caster chooses the source");
    assert!(
        options.contains(&recipient),
        "reach-guard: every source is offered, so the shield can be completed: {options:?}"
    );
    runner
        .act(GameAction::ChooseDamageSource { source: recipient })
        .expect("the chosen source binds the redirection");
    assert!(
        !runner.state().pending_damage_replacements.is_empty(),
        "CR 614.9: the resumed cast installs its redirection shield"
    );

    let recorded_targets: Option<Vec<TargetRef>> = runner
        .state()
        .spells_cast_this_turn_by_player
        .values()
        .flat_map(|records| records.iter())
        .find(|record| record.spell_object_id == Some(spell))
        .and_then(|record| record.targets.as_deref())
        .map(|facts| {
            facts
                .iter()
                .map(|fact| match fact {
                    ActivationTargetFact::Object { id, .. } => TargetRef::Object(*id),
                    ActivationTargetFact::Player(player) => TargetRef::Player(*player),
                })
                .collect()
        });
    assert_eq!(
        recorded_targets,
        Some(vec![TargetRef::Object(recipient)]),
        "CR 601.2i: the resumed cast publishes the redirect recipient it announced"
    );
}

/// WITNESS (refusal): the SAME save with the recorded characteristics rewritten
/// to an enchantment-only permanent — id, zone and pin untouched — is refused.
/// The chosen redirect recipient uses `Any`, whose CR 115.4 domain is a player
/// or a creature/planeswalker/battle permanent, so the recorded snapshot cannot
/// be what this cast announced.
#[test]
fn restore_refuses_a_recorded_redirect_recipient_restated_as_an_enchantment() {
    let ParkedHarmsWay {
        runner,
        spell,
        recipient,
        ..
    } = park_harms_way_at_manual_payment();
    assert_redirect_recipient_shape(&runner, spell);

    let mut state = runner.state().clone();
    {
        let pending = state
            .pending_cast
            .as_mut()
            .expect("reach-guard: the manual payment park keeps its pending cast");
        let facts = pending
            .committed_target_facts
            .as_deref_mut()
            .expect("reach-guard: the announcement latch is populated");
        match &mut facts[0] {
            ActivationTargetFact::Object {
                id,
                lki,
                zone,
                token,
                ..
            } => {
                assert_eq!(
                    *id, recipient,
                    "reach-guard: the rewritten fact still names the announced recipient"
                );
                assert_eq!(
                    *zone,
                    engine::types::game_state::AnnouncedTargetZone::Known(Zone::Battlefield),
                    "reach-guard: the recorded zone is untouched"
                );
                assert_eq!(
                    *token,
                    engine::types::game_state::CapturedTokenBit::Known(false),
                    "reach-guard: the recorded token bit is untouched"
                );
                assert!(
                    lki.card_types.contains(&CoreType::Creature),
                    "reach-guard: the recorded snapshot says creature before the restatement"
                );
                lki.card_types
                    .retain(|card_type| *card_type != CoreType::Creature);
                lki.card_types.push(CoreType::Enchantment);
                assert_eq!(
                    lki.card_types,
                    vec![CoreType::Enchantment],
                    "reach-guard: the restated snapshot is an enchantment-only permanent"
                );
            }
            other => panic!("reach-guard: the redirect recipient is an object, got {other:?}"),
        }
    }
    // CR 400.7: the announcement pin is untouched and still current, so only the
    // recorded CHARACTERISTICS were rewritten — not the target's identity.
    {
        let pending = state
            .pending_cast
            .as_deref()
            .expect("reach-guard: the manual payment park keeps its pending cast");
        let pin = pending
            .ability
            .target_pins
            .first()
            .copied()
            .flatten()
            .expect("reach-guard: the announced recipient carries an incarnation pin");
        assert!(
            pin.is_current(&state),
            "reach-guard: the pin still names the live incarnation"
        );
    }

    let error = PersistedGameState::capture(state)
        .into_game_state()
        .expect_err(
            "CR 115.4: an enchantment cannot satisfy the 'any target' redirect recipient this \
             cast announced",
        );
    match error {
        PersistedRestoreError::ContradictoryAnnouncedTargetFacts {
            spell_object_id,
            reason,
        } => {
            assert_eq!(
                spell_object_id, spell,
                "the refusal names the cast it is about"
            );
            match reason {
                AnnouncedTargetFactConflict::SnapshotContradictsDeclaredFilter { target } => {
                    assert_eq!(
                        target,
                        TargetRef::Object(recipient),
                        "and the target whose recorded announcement contradicts its own slot"
                    );
                }
                other => panic!(
                    "expected the declared-filter refusal that reads the recorded snapshot, got \
                     {other:?}"
                ),
            }
        }
        other => panic!("expected a recorded-announcement refusal, got {other:?}"),
    }
}
