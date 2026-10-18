//! Encore (CR 702.141) — graveyard-activated, per-opponent token-copy generator.
//!
//! CR 702.141a: "Encore [cost]" is an activated ability that functions only
//! while the card with encore is in a graveyard. It means:
//!
//! > "[Cost], Exile this card from your graveyard: For each opponent, create a
//! > token that's a copy of this card that attacks that opponent this turn if
//! > able. The tokens gain haste. Sacrifice them at the beginning of the next
//! > end step. Activate only as a sorcery."
//!
//! The exile-from-graveyard is paid as part of the activation cost (composed in
//! `database/encore.rs`), so by the time this resolver runs the source card is
//! in exile. It keeps its `ObjectId` across the graveyard → exile cost move
//! (`game/zones.rs::move_to_zone` mutates the zone in place), so
//! `Effect::CopyTokenOf { target: SelfRef }` still resolves to the exiled card
//! and `compute_current_copiable_values` reads its printed copiable values from
//! the exile zone — exactly as Embalm / Eternalize rely on.
//!
//! This resolver mirrors `effects/myriad.rs` (CR 702.116a), Encore's closest
//! sibling: both create one copy token per opponent and bind each token to that
//! opponent. The differences are encoded here rather than in any shared effect:
//!
//! - **Per-opponent must-attack, not enters-attacking.** Encore is activated at
//!   sorcery speed (typically in a main phase, outside combat), so the tokens
//!   are *not* created already attacking. Instead each token gains a
//!   `MustAttackDefender { defender }` requirement (CR 508.1d) bound to the opponent
//!   it was created for, lasting until end of turn (CR 702.141a "this turn").
//!   Binding the requirement per opponent — instead of via the generic
//!   `Effect::ForceAttack`, whose `required_defender` context ref has no
//!   "the opponent" resolution and would fall back to the controller — is the
//!   reason Encore needs a dedicated resolver.
//! - **Haste is a later grant** (CR 611.2a), with no stated duration. It
//!   applies to every replacement-result member and is not copiable (CR 707.2).
//! - **Sacrifice at the next end step**, composed as a one-shot delayed trigger
//!   (CR 603.7d) over the exact token IDs created here, rather than Myriad's
//!   end-of-combat exile.

use crate::game::players;
use crate::types::ability::{
    ContinuousModification, DelayedTriggerCondition, Duration, Effect, EffectError, EffectKind,
    QuantityExpr, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{DelayedTrigger, GameState};
use crate::types::keywords::Keyword;
use crate::types::phase::Phase;
use crate::types::statics::{RequiredDefender, StaticMode};

/// CR 702.141a: Resolve a card's Encore ability — for each opponent of the
/// activating player, create a haste-bearing token copy of the exiled source
/// card that must attack that opponent this turn, then sacrifice every created
/// token at the beginning of the next end step.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    if !matches!(ability.effect, Effect::Encore) {
        return Err(EffectError::MissingParam("Encore".to_string()));
    }

    let mut opponents: std::collections::VecDeque<_> =
        players::opponents(state, ability.controller).into();
    let (caller, work) = if let Some(current) = opponents.pop_front() {
        (
            crate::types::game_state::TokenCreationCallerState::Encore {
                current,
                remaining: opponents,
            },
            copy_work(state, ability)?,
        )
    } else {
        (
            crate::types::game_state::TokenCreationCallerState::Plain,
            std::collections::VecDeque::new(),
        )
    };
    super::token_copy::begin_creation(state, ability, work, EffectKind::Encore, caller, events);
    Ok(())
}

pub(crate) fn copy_work(
    state: &mut GameState,
    ability: &ResolvedAbility,
) -> Result<std::collections::VecDeque<crate::types::game_state::TokenCreationWork>, EffectError> {
    let effect = Effect::CopyTokenOf {
        target: TargetFilter::SelfRef,
        owner: TargetFilter::Controller,
        source_filter: None,
        enters_attacking: false,
        tapped: false,
        count: QuantityExpr::Fixed { value: 1 },
        extra_keywords: vec![],
        additional_modifications: vec![],
    };
    super::token_copy::prepare_copy_creation(
        state,
        &super::token_copy::own_source_copy_ability(effect, ability),
    )
}

pub(crate) fn complete_creation_segment(
    state: &mut GameState,
    ability: &ResolvedAbility,
    opponent: crate::types::player::PlayerId,
    members: &[crate::types::identifiers::ObjectIncarnationRef],
) {
    for member in members {
        if !state.objects.get(&member.object_id).is_some_and(|object| {
            crate::types::identifiers::ObjectIncarnationRef::from_object(object) == *member
                && object.zone == crate::types::zones::Zone::Battlefield
        }) {
            continue;
        }
        // CR 702.141a + CR 611.2a: the later haste grant has no duration.
        // CR 514.2: only the separate “this turn” attack requirement expires.
        for (duration, modification) in [
            (
                Duration::Permanent,
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Haste,
                },
            ),
            (
                Duration::UntilEndOfTurn,
                ContinuousModification::AddStaticMode {
                    mode: StaticMode::MustAttackDefender {
                        defender: RequiredDefender::Fixed { player: opponent },
                    },
                },
            ),
        ] {
            state.add_transient_continuous_effect_with_bindings(
                ability.source_id,
                ability.controller,
                duration,
                TargetFilter::SpecificObject {
                    id: member.object_id,
                },
                vec![modification],
                None,
                crate::types::game_state::TransientContinuousEffectBindings {
                    affected_recipient: Some(*member),
                    ..Default::default()
                },
            );
        }
    }
}

pub(crate) fn complete_creation(
    state: &mut GameState,
    ability: &ResolvedAbility,
    result: &crate::types::ability::ForwardedResultContext,
    events: &mut Vec<GameEvent>,
) {
    if result.object_incarnations.is_empty() {
        return;
    }
    let mut sacrifice = ResolvedAbility::new(
        Effect::Sacrifice {
            target: TargetFilter::LastCreated,
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 0,
        },
        vec![],
        ability.source_id,
        ability.controller,
    );
    super::delayed_trigger::freeze_created_result(&mut sacrifice, result);
    crate::game::triggers::install_delayed_trigger(
        state,
        DelayedTrigger {
            condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
            ability: Box::new(sacrifice),
            controller: ability.controller,
            source_id: ability.source_id,
            one_shot: true,
            provenance: crate::types::identifiers::DelayedInstallIdentity::LegacyDelayed,
        },
        events,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encore_completion_synchronous() {
        let (mut state, events) = super::super::token_copy::tests::creation_wrapper_case(
            Effect::Encore,
            super::super::token_copy::tests::CreationPauseCase::Synchronous,
        );
        assert_eq!(state.delayed_triggers.len(), 1);
        assert_eq!(
            state.delayed_triggers[0].ability.target_incarnations.len(),
            state.last_created_token_ids.len()
        );
        crate::game::layers::flush_layers(&mut state);
        for id in &state.last_created_token_ids {
            assert!(state.objects[id]
                .keywords
                .contains(&crate::types::keywords::Keyword::Haste));
        }
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: EffectKind::Encore,
                        ..
                    }
                ))
                .count(),
            1
        );
    }
    #[test]
    fn encore_completion_queued_child() {
        let (mut state, events) = super::super::token_copy::tests::creation_wrapper_case(
            Effect::Encore,
            super::super::token_copy::tests::CreationPauseCase::Queued,
        );
        assert_eq!(state.delayed_triggers.len(), 1);
        assert_eq!(
            state.delayed_triggers[0].ability.target_incarnations.len(),
            state.last_created_token_ids.len()
        );
        crate::game::layers::flush_layers(&mut state);
        for id in &state.last_created_token_ids {
            assert!(state.objects[id]
                .keywords
                .contains(&crate::types::keywords::Keyword::Haste));
        }
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: EffectKind::Encore,
                        ..
                    }
                ))
                .count(),
            1
        );
    }
    #[test]
    fn encore_completion_counter_pause() {
        let (mut state, events) = super::super::token_copy::tests::creation_wrapper_case(
            Effect::Encore,
            super::super::token_copy::tests::CreationPauseCase::Counter,
        );
        assert_eq!(state.delayed_triggers.len(), 1);
        assert_eq!(
            state.delayed_triggers[0].ability.target_incarnations.len(),
            state.last_created_token_ids.len()
        );
        crate::game::layers::flush_layers(&mut state);
        for id in &state.last_created_token_ids {
            assert!(state.objects[id]
                .keywords
                .contains(&crate::types::keywords::Keyword::Haste));
        }
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: EffectKind::Encore,
                        ..
                    }
                ))
                .count(),
            1
        );
    }
    #[test]
    fn encore_completion_queued_child_current_reload() {
        let (mut state, events) = super::super::token_copy::tests::creation_wrapper_case(
            Effect::Encore,
            super::super::token_copy::tests::CreationPauseCase::ReloadQueued,
        );
        assert_eq!(state.delayed_triggers.len(), 1);
        assert_eq!(
            state.delayed_triggers[0].ability.target_incarnations.len(),
            state.last_created_token_ids.len()
        );
        crate::game::layers::flush_layers(&mut state);
        for id in &state.last_created_token_ids {
            assert!(state.objects[id]
                .keywords
                .contains(&crate::types::keywords::Keyword::Haste));
        }
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: EffectKind::Encore,
                        ..
                    }
                ))
                .count(),
            1
        );
    }
    #[test]
    fn encore_completion_counter_current_reload() {
        let (mut state, events) = super::super::token_copy::tests::creation_wrapper_case(
            Effect::Encore,
            super::super::token_copy::tests::CreationPauseCase::ReloadCounter,
        );
        assert_eq!(state.delayed_triggers.len(), 1);
        assert_eq!(
            state.delayed_triggers[0].ability.target_incarnations.len(),
            state.last_created_token_ids.len()
        );
        crate::game::layers::flush_layers(&mut state);
        for id in &state.last_created_token_ids {
            assert!(state.objects[id]
                .keywords
                .contains(&crate::types::keywords::Keyword::Haste));
        }
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: EffectKind::Encore,
                        ..
                    }
                ))
                .count(),
            1
        );
    }
    #[test]
    fn encore_haste_and_attack_grants_follow_birth_incarnation_not_reused_storage_id() {
        let (mut state, _events) = super::super::token_copy::tests::creation_wrapper_case(
            Effect::Encore,
            super::super::token_copy::tests::CreationPauseCase::Synchronous,
        );
        let births = state.delayed_triggers[0]
            .ability
            .target_incarnations
            .clone();
        assert_eq!(births.len(), 2);
        crate::game::layers::flush_layers(&mut state);
        assert!(births.iter().all(|member| state.objects[&member.object_id]
            .keywords
            .contains(&Keyword::Haste)));
        // Labelled epoch isolation; a real token blink/no-return is public in
        // Broken's Cloudshift sister, and actual Encore cleanup is public too.
        state
            .objects
            .get_mut(&births[0].object_id)
            .unwrap()
            .bump_incarnation();
        crate::game::layers::mark_layers_full(&mut state);
        crate::game::layers::flush_layers(&mut state);
        assert!(!state.objects[&births[0].object_id]
            .keywords
            .contains(&Keyword::Haste));
        assert!(state.objects[&births[1].object_id]
            .keywords
            .contains(&Keyword::Haste));
        assert!(!crate::game::combat::creature_must_attack(
            &state,
            births[0].object_id
        ));
        assert!(crate::game::combat::creature_must_attack(
            &state,
            births[1].object_id
        ));
    }
}
