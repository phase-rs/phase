use super::resolve_player_for_context_ref;
use crate::game::targeting::{resolve_live_parent_slot_from_root, resolved_object_ids_for_filter};
use crate::types::ability::{
    ContinuousModification, ControllerRef, Duration, Effect, EffectError, EffectKind, EffectScope,
    PlayerScope, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::{ObjectId, ObjectIncarnationRef};
use crate::types::player::PlayerId;
use crate::types::statics::{RequiredDefender, StaticMode};

/// CR 506.3: the RESOLVED defender a `required_defender` filter names.
///
/// CR 506.3's category is "a player, a planeswalker, or a battle", so this is the
/// discriminator the whole seam turns on.
enum DefenderReferent {
    /// A permanent — lowers to `RequiredDefender::Permanent`.
    Object(ObjectId),
    /// A player — lowers to `RequiredDefender::Fixed`.
    Player(PlayerId),
}

/// CR 506.3: Resolve a `required_defender` filter to its referent.
///
/// Two filters are unconditionally objects by construction (`SelfRef` is the
/// ability's own source; `SpecificObject` names one). The inherited-target forms
/// are NOT: `ParentTarget` / `ParentTargetSlot` name whatever the parent clause
/// targeted, which may be a player — so they must be resolved before they are
/// classified. Deciding by filter VARIANT instead routed a player-valued parent
/// target down the object path, where `resolved_object_ids_for_filter` finds
/// nothing and the whole requirement is silently dropped.
///
/// Everything else is a player reference, which is the conservative default:
/// every card using this effect before Gideon Jura named a player.
///
/// `None` when the referent names no live object (a vanished defender, or a
/// declared slot whose object left and returned) or a declared slot whose
/// target was illegal as the ability resolved, so the caller grafts nothing.
fn defender_referent(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
) -> Option<DefenderReferent> {
    let object_referent = || {
        resolved_object_ids_for_filter(state, ability, filter)
            .into_iter()
            .next()
            .map(DefenderReferent::Object)
    };
    let player_referent = || {
        Some(DefenderReferent::Player(resolve_player_for_context_ref(
            state, ability, filter,
        )))
    };
    match filter {
        TargetFilter::SelfRef | TargetFilter::SpecificObject { .. } => object_referent(),
        // CR 608.2c: the parent's chosen target. A player-valued parent target,
        // or no target to inherit at all, reads as a player, which
        // `resolve_player_for_context_ref` handles.
        TargetFilter::ParentTarget => match ability.targets.first() {
            Some(TargetRef::Object(_)) => object_referent(),
            Some(TargetRef::Player(_)) | None => player_referent(),
        },
        // CR 608.2c + CR 608.2b + CR 400.7: a declared slot of the WHOLE
        // resolving chain — never this node's local `targets`, which chain
        // propagation replaced with the most recent parent's. The chain-root
        // authority names the exact referent of either kind, so it lowers
        // directly; a slot whose target was illegal as the ability resolved, or
        // whose object left and returned, names none, and nothing is grafted.
        TargetFilter::ParentTargetSlot { index } => {
            resolve_live_parent_slot_from_root(state, ability, *index).map(|target| match target {
                TargetRef::Object(id) => DefenderReferent::Object(id),
                TargetRef::Player(player) => DefenderReferent::Player(player),
            })
        }
        _ => player_referent(),
    }
}

/// CR 506.3 + CR 611.2: Snapshot the `required_defender` filter into the durable
/// [`RequiredDefender`] combat enforcement reads.
///
/// An OBJECT referent lowers to `Permanent`, pinned by incarnation (CR 400.7, so
/// a defender that leaves and re-enters does not inherit a requirement aimed at
/// the old object); a PLAYER referent lowers to `Fixed`. `SelfRef` is the only
/// object form a printed card reaches today (Gideon Jura's "attack Gideon Jura
/// if able"), but the classification is genuinely by referent kind — see
/// [`defender_referent`] — so a future "attacks target planeswalker if able"
/// needs no new branch.
///
/// Returns `None` when an object referent names no live object, so the caller
/// grafts nothing rather than a requirement aimed at a vanished defender.
fn snapshot_required_defender(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
) -> Option<RequiredDefender> {
    match defender_referent(state, ability, filter)? {
        DefenderReferent::Player(player) => Some(RequiredDefender::Fixed { player }),
        DefenderReferent::Object(defender_id) => {
            let obj = state.objects.get(&defender_id)?;
            Some(RequiredDefender::Permanent {
                permanent: ObjectIncarnationRef::from_object(obj),
            })
        }
    }
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

    /// CR 506.3 + CR 608.2c: an inherited-target defender is classified by its
    /// RESOLVED referent, not by the filter variant.
    ///
    /// `ParentTarget` names whatever the parent clause targeted. When that is a
    /// PLAYER it must reach `RequiredDefender::Fixed`; classifying by variant sent
    /// it down the object path, where `resolved_object_ids_for_filter` finds
    /// nothing and the requirement is silently dropped entirely.
    ///
    /// The object half is the paired guard: it proves the object path still works
    /// and that the player half is not passing merely because everything became a
    /// player.
    #[test]
    fn parent_target_defender_is_classified_by_its_resolved_referent() {
        fn snapshot_for(target: TargetRef) -> Option<RequiredDefender> {
            let mut state = GameState::new_two_player(42);
            let source = create_object(
                &mut state,
                CardId(1),
                PlayerId(0),
                "Coercer".to_string(),
                Zone::Battlefield,
            );
            let ability = ResolvedAbility::new(
                Effect::ForceAttack {
                    target: TargetFilter::Any,
                    required_defender: TargetFilter::ParentTarget,
                    duration: Duration::UntilEndOfCombat,
                    scope: EffectScope::Single,
                },
                vec![target],
                source,
                PlayerId(0),
            );
            snapshot_required_defender(&state, &ability, &TargetFilter::ParentTarget)
        }

        // A PLAYER-valued parent target lowers to `Fixed`.
        assert_eq!(
            snapshot_for(TargetRef::Player(PlayerId(1))),
            Some(RequiredDefender::Fixed {
                player: PlayerId(1)
            }),
            "a player-valued parent target is a PLAYER defender"
        );

        // An OBJECT-valued parent target lowers to `Permanent`, pinned by
        // incarnation (CR 400.7).
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Coercer".to_string(),
            Zone::Battlefield,
        );
        let walker = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Some Planeswalker".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::ForceAttack {
                target: TargetFilter::Any,
                required_defender: TargetFilter::ParentTarget,
                duration: Duration::UntilEndOfCombat,
                scope: EffectScope::Single,
            },
            vec![TargetRef::Object(walker)],
            source,
            PlayerId(0),
        );
        let snapshot = snapshot_required_defender(&state, &ability, &TargetFilter::ParentTarget);
        let Some(RequiredDefender::Permanent { permanent }) = snapshot else {
            panic!("an object-valued parent target is a PERMANENT defender, got {snapshot:?}");
        };
        assert_eq!(permanent.object_id, walker);
    }

    /// CR 608.2c: `required_defender: ParentTargetSlot { index }` classifies the
    /// DECLARED chain-root slot, not the leaf's locally-propagated target. A
    /// nested chain whose root slot 0 is a PLAYER must lower to `Fixed` even when
    /// the leaf's local (propagated) target is an object.
    #[test]
    fn parent_target_slot_defender_classifies_from_chain_root() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Coercer".to_string(),
            Zone::Battlefield,
        );
        let walker = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Some Planeswalker".to_string(),
            Zone::Battlefield,
        );

        // Root chain declares slot 0 = player, slot 1 = the planeswalker.
        let root = ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Player(PlayerId(1))],
            source,
            PlayerId(0),
        )
        .sub_ability(ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(walker)],
            source,
            PlayerId(0),
        ));
        state.resolving_stack_entry = Some(StackEntry {
            id: source,
            source_id: source,
            controller: PlayerId(0),
            kind: StackEntryKind::ActivatedAbility {
                source_id: source,
                ability: Box::new(root),
            },
        });

        // The leaf's local (propagated) target is the planeswalker, but its
        // `ParentTargetSlot { index: 0 }` names root slot 0 = the player.
        let leaf = ResolvedAbility::new(
            Effect::ForceAttack {
                target: TargetFilter::Any,
                required_defender: TargetFilter::ParentTargetSlot { index: 0 },
                duration: Duration::UntilEndOfCombat,
                scope: EffectScope::Single,
            },
            vec![TargetRef::Object(walker)],
            source,
            PlayerId(0),
        );

        assert_eq!(
            snapshot_required_defender(&state, &leaf, &TargetFilter::ParentTargetSlot { index: 0 }),
            Some(RequiredDefender::Fixed {
                player: PlayerId(1)
            }),
            "root slot 0 is a player, so the requirement must be Fixed, not Permanent"
        );
    }

    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{ControllerRef, Duration, TargetRef, TypedFilter};
    use crate::types::game_state::{StackEntry, StackEntryKind};
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

    /// CR 608.2b + CR 506.3: a `ParentTargetSlot` required defender whose
    /// declared target was illegal as the ability resolved (the resolution
    /// carrier's `illegal_target_slots` stamp) names no defender, so no
    /// requirement is created. Classifying the slot from its declared player
    /// and then resolving that player separately would still aim the
    /// requirement at a player.
    #[test]
    fn parent_target_slot_defender_illegal_at_resolution_creates_no_requirement() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Coercer".to_string(),
            Zone::Battlefield,
        );
        let mut root = ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Player(PlayerId(1))],
            source,
            PlayerId(0),
        );
        root.illegal_target_slots = vec![0];
        state.resolving_stack_entry = Some(StackEntry {
            id: source,
            source_id: source,
            controller: PlayerId(0),
            kind: StackEntryKind::ActivatedAbility {
                source_id: source,
                ability: Box::new(root),
            },
        });
        let slot_zero = TargetFilter::ParentTargetSlot { index: 0 };
        let leaf = ResolvedAbility::new(
            Effect::ForceAttack {
                target: TargetFilter::SelfRef,
                required_defender: slot_zero.clone(),
                duration: Duration::UntilEndOfCombat,
                scope: EffectScope::Single,
            },
            vec![],
            source,
            PlayerId(0),
        );

        assert_eq!(
            snapshot_required_defender(&state, &leaf, &slot_zero),
            None,
            "an illegal declared player is not a defender the creature must attack"
        );
    }

    /// CR 608.2c + CR 506.3: a `ParentTargetSlot` required defender whose
    /// chain-root slot names a planeswalker resolves, through the full
    /// `resolve()`, to a `Permanent` requirement pinned to that walker. The
    /// chained node's own `targets` hold only the most recent parent target, so
    /// the slot must come from the chain root, and an object referent must
    /// lower to `Permanent`, never to a player or to nothing.
    #[test]
    fn parent_target_slot_object_defender_resolves_to_permanent() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Coercer".to_string(),
            Zone::Battlefield,
        );
        let walker = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Some Planeswalker".to_string(),
            Zone::Battlefield,
        );
        // Root chain declares slot 0 = opponent (player), slot 1 = walker.
        let root = ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Player(PlayerId(1))],
            source,
            PlayerId(0),
        )
        .sub_ability(ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(walker)],
            source,
            PlayerId(0),
        ));
        state.resolving_stack_entry = Some(StackEntry {
            id: source,
            source_id: source,
            controller: PlayerId(0),
            kind: StackEntryKind::ActivatedAbility {
                source_id: source,
                ability: Box::new(root),
            },
        });
        let leaf = ResolvedAbility::new(
            Effect::ForceAttack {
                target: TargetFilter::SelfRef,
                required_defender: TargetFilter::ParentTargetSlot { index: 1 },
                duration: Duration::UntilEndOfCombat,
                scope: EffectScope::Single,
            },
            vec![TargetRef::Player(PlayerId(1))],
            source,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &leaf, &mut events).unwrap();

        let defender = state
            .transient_continuous_effects
            .iter()
            .find(|ce| ce.affected == TargetFilter::SpecificObject { id: source })
            .and_then(|ce| {
                ce.modifications.iter().find_map(|m| match m {
                    ContinuousModification::AddStaticMode {
                        mode: StaticMode::MustAttackDefender { defender },
                    } => Some(defender.clone()),
                    _ => None,
                })
            });
        let Some(RequiredDefender::Permanent { permanent }) = defender else {
            panic!("root slot 1 is a planeswalker, a PERMANENT defender; got {defender:?}");
        };
        assert_eq!(permanent.object_id, walker);
    }
}
