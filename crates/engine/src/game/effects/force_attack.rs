use super::resolve_player_for_context_ref;
use crate::game::targeting::resolved_object_ids_for_filter;
use crate::types::ability::{
    ContinuousModification, ControllerRef, Duration, Effect, EffectError, EffectKind, EffectScope,
    PlayerScope, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectIncarnationRef;
use crate::types::statics::{RequiredDefender, StaticMode};

/// CR 506.3 + CR 611.2: Classify the `required_defender` filter and snapshot it
/// into the durable [`RequiredDefender`] combat enforcement reads.
///
/// A filter naming an OBJECT lowers to `Permanent` (pinned by incarnation — CR
/// 400.7, so a defender that leaves and re-enters does not inherit a requirement
/// aimed at the old object); every other filter is a player reference and lowers
/// to `Fixed` via the shared context-ref resolver. `SelfRef` is the only object
/// arm a printed card reaches today (Gideon Jura's "attack Gideon Jura if
/// able"), but the classification is by REFERENT KIND rather than by that one
/// filter, so a future "attacks target planeswalker if able" needs no new
/// branch.
///
/// Returns `None` when an object referent names no live object, so the caller
/// grafts nothing rather than a requirement aimed at a vanished defender.
fn snapshot_required_defender(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
) -> Option<RequiredDefender> {
    if !filter_denotes_object(filter) {
        return Some(RequiredDefender::Fixed {
            player: resolve_player_for_context_ref(state, ability, filter),
        });
    }
    let defender_id = resolved_object_ids_for_filter(state, ability, filter)
        .into_iter()
        .next()?;
    let obj = state.objects.get(&defender_id)?;
    Some(RequiredDefender::Permanent {
        permanent: ObjectIncarnationRef::from_object(obj),
    })
}

/// CR 506.3: whether `filter` denotes an OBJECT defender rather than a player.
/// An explicit allow-list, never a catch-all: a filter whose referent kind is
/// unclear keeps the pre-existing player reading, which is the conservative
/// direction — every card using this effect before Gideon Jura named a player.
fn filter_denotes_object(filter: &TargetFilter) -> bool {
    matches!(
        filter,
        TargetFilter::SelfRef
            | TargetFilter::SpecificObject { .. }
            | TargetFilter::ParentTarget
            | TargetFilter::ParentTargetSlot { .. }
    )
}

/// CR 611.2c + CR 115.1: how a force-attack subject must be installed.
///
/// Three OUTCOMES, deliberately distinct rather than collapsed into an
/// `Option`. "Chosen target" and "broadcast population that could not be
/// lowered" both mean "no population filter to install", but they call for
/// opposite handling: the first is correctly grafted per object, while the
/// second must install NOTHING. Grafting an unlowerable population per object
/// would freeze it at resolution — exactly the CR 611.2c violation the Gideon
/// Jura ruling forbids — and would do so silently.
enum SubjectLowering {
    /// CR 115.1: a chosen-target subject ("target creature attacks you this
    /// combat if able"). Per-object `SpecificObject` grafting is correct;
    /// CR 611.2c's dynamic-population concern does not arise when the effect
    /// names specific objects.
    ChosenTarget,
    /// CR 611.2c: a broadcast population, lowered and ready to install INTACT so
    /// the layer pass re-derives its members every declare-attackers step.
    Population(TargetFilter),
    /// CR 611.2c: a broadcast population whose player reference could not be
    /// resolved (no player target to bind, or a filter shape this lowering does
    /// not understand). Unreachable for every printed card today; if it is ever
    /// reached, installing nothing is the honest failure — a frozen set would
    /// look like it worked while quietly disobeying the ruling.
    Unlowerable,
}

/// CR 611.2c: Classify a force-attack subject for installation.
///
/// Gideon Jura's official ruling is why the broadcast form cannot freeze its
/// set: the "+2" "doesn't lock in what it applies to … whatever creatures the
/// targeted opponent controls during the declare attackers step of their next
/// turn must attack Gideon Jura if able. This includes creatures that come under
/// that player's control after the ability has resolved."
///
/// Only `ControllerRef::TargetPlayer` / `TargetOpponent` need lowering:
/// `ControllerRef::You` / `Opponent` are resolved by `layers.rs` against the
/// continuous effect's own snapshotted `controller` (the Kardur path), and no
/// other controller ref reaches a broadcast force-attack subject today.
fn lower_dynamic_affected(
    ability: &ResolvedAbility,
    target: &TargetFilter,
    scope: EffectScope,
) -> SubjectLowering {
    // CR 115.1: the scope is the authority for which form this is — a `Single`
    // subject is a chosen target no matter what filter shape it happens to
    // carry, so it must never take the population path.
    if scope != EffectScope::All {
        return SubjectLowering::ChosenTarget;
    }
    // An `All` scope IS a population by construction, so every failure below is
    // `Unlowerable`, never `ChosenTarget`.
    let TargetFilter::Typed(typed) = target else {
        return SubjectLowering::Unlowerable;
    };
    let mut typed = typed.clone();
    if matches!(
        typed.controller,
        Some(ControllerRef::TargetPlayer | ControllerRef::TargetOpponent)
    ) {
        // CR 109.4 + CR 611.2: "that player" is fixed when the ability resolves.
        // `ability.targets` no longer exists when the layer pass re-derives the
        // affected set, so bind the id now.
        let Some(id) = ability.targets.iter().find_map(|t| match t {
            TargetRef::Player(pid) => Some(*pid),
            TargetRef::Object(_) => None,
        }) else {
            return SubjectLowering::Unlowerable;
        };
        typed.controller = Some(ControllerRef::SpecificPlayer { id });
    }
    SubjectLowering::Population(TargetFilter::Typed(typed))
}

/// CR 611.2 + CR 514.2: Lower a target-scoped duration to a resolution-time
/// snapshot, so the installed continuous effect's expiry still names a concrete
/// player after the resolving ability (and its `targets`) is gone.
///
/// `PlayerScope::Target` is the only scope needing this: `Controller` is already
/// carried by the continuous effect's own `controller` field, which
/// `layers.rs::prune_until_next_turn_effects` reads directly. A duration whose
/// target cannot be resolved is left untouched rather than guessed at — an
/// unarmable expiry is a visible bug, a silently wrong player is not.
fn lower_target_scoped_duration(ability: &ResolvedAbility, duration: Duration) -> Duration {
    let Duration::UntilEndOfNextTurnOf {
        player: PlayerScope::Target,
    } = duration
    else {
        return duration;
    };
    let Some(id) = ability.targets.iter().find_map(|t| match t {
        TargetRef::Player(pid) => Some(*pid),
        TargetRef::Object(_) => None,
    }) else {
        return duration;
    };
    Duration::UntilEndOfNextTurnOf {
        player: PlayerScope::SpecificPlayer { id },
    }
}

/// CR 508.1d: Force attack — the creatures matching `target` must attack the
/// required defender this turn/combat if able.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::ForceAttack {
        target,
        required_defender,
        duration,
        scope,
    } = &ability.effect
    else {
        return Ok(());
    };

    // CR 611.2a: "lasts as long as stated by the spell or ability creating it."
    // A stated duration written as a leading CLAUSE rather than inside the
    // predicate — Gideon Jura's "During target opponent's next turn, creatures
    // that player controls attack ~ if able" — is stamped by the parser onto
    // `ability.duration`, so it must win over the effect's own field. Same
    // precedence the `GenericEffect` arm of `effects/effect.rs::resolve` applies,
    // for the same reason.
    let duration = ability.duration.clone().unwrap_or_else(|| duration.clone());

    // CR 611.2 + CR 109.4: "during TARGET opponent's next turn" is scoped to the
    // player this ability targeted. `PlayerScope::Target` resolves by reading
    // `ability.targets`, which no longer exists once the continuous effect is
    // installed and the ability is gone — so snapshot it now, exactly as the
    // affected filter's controller ref is snapshotted below.
    let duration = lower_target_scoped_duration(ability, duration);

    let resolved = snapshot_required_defender(state, ability, required_defender);

    if let Some(defender) = resolved {
        // CR 611.2c: a broadcast subject keeps ONE continuous effect carrying the
        // live filter, so the affected creature set is re-derived every
        // declare-attackers step. `register_transient_effect` routes
        // `MustAttackAwayFromSource` grants down the same path for the same
        // reason (Kardur, Maximum Carnage); this resolver installs directly, so
        // it makes the same call here.
        match lower_dynamic_affected(ability, target, *scope) {
            SubjectLowering::Population(affected) => state.add_transient_continuous_effect(
                ability.source_id,
                ability.controller,
                duration.clone(),
                affected,
                vec![ContinuousModification::AddStaticMode {
                    mode: StaticMode::MustAttackDefender { defender },
                }],
                None,
            ),
            SubjectLowering::ChosenTarget => {
                for obj_id in resolved_object_ids_for_filter(state, ability, target) {
                    if !state.objects.contains_key(&obj_id) {
                        continue;
                    }

                    state.add_transient_continuous_effect(
                        ability.source_id,
                        ability.controller,
                        duration.clone(),
                        TargetFilter::SpecificObject { id: obj_id },
                        vec![ContinuousModification::AddStaticMode {
                            // CR 611.2: the required defender is snapshotted at resolution.
                            mode: StaticMode::MustAttackDefender {
                                defender: defender.clone(),
                            },
                        }],
                        None,
                    );
                }
                0
            }
            // CR 611.2c: install NOTHING rather than a frozen per-object graft.
            // See `SubjectLowering::Unlowerable`.
            SubjectLowering::Unlowerable => 0,
        };
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ForceAttack,
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{ControllerRef, Duration, TargetRef, TypedFilter};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_force_attack_ability(
        source: ObjectId,
        target: ObjectId,
        controller: PlayerId,
        duration: Duration,
    ) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::ForceAttack {
                target: TargetFilter::Any,
                required_defender: TargetFilter::Controller,
                duration,
                scope: EffectScope::Single,
            },
            vec![TargetRef::Object(target)],
            source,
            controller,
        )
    }

    #[test]
    fn force_attack_grants_must_attack_player_for_controller() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Siren".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let ability =
            make_force_attack_ability(source, target, PlayerId(0), Duration::UntilEndOfCombat);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let effect = state
            .transient_continuous_effects
            .iter()
            .find(|ce| ce.affected == TargetFilter::SpecificObject { id: target })
            .expect("force attack should create a transient effect for the target");

        assert_eq!(effect.duration, Duration::UntilEndOfCombat);
        assert!(effect.modifications.iter().any(|m| {
            matches!(
                m,
                ContinuousModification::AddStaticMode {
                    mode: StaticMode::MustAttackDefender {
                        defender: RequiredDefender::Fixed { player },
                    },
                } if *player == PlayerId(0)
            )
        }));

        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::ForceAttack,
                source_id,
            ..} if *source_id == source
        )));
    }

    #[test]
    fn force_attack_resolves_chosen_required_player() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Ruhan".to_string(),
            Zone::Battlefield,
        );
        let mut ability = ResolvedAbility::new(
            Effect::ForceAttack {
                target: TargetFilter::SelfRef,
                required_defender: TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::ChosenPlayer { index: 0 }),
                ),
                duration: Duration::UntilEndOfCombat,
                scope: EffectScope::Single,
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.chosen_players = vec![PlayerId(1)];

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let effect = state
            .transient_continuous_effects
            .iter()
            .find(|ce| ce.affected == TargetFilter::SpecificObject { id: source })
            .expect("force attack should create a transient effect for the source");

        assert!(effect.modifications.iter().any(|m| {
            matches!(
                m,
                ContinuousModification::AddStaticMode {
                    mode: StaticMode::MustAttackDefender {
                        defender: RequiredDefender::Fixed { player },
                    },
                } if *player == PlayerId(1)
            )
        }));
    }
}
