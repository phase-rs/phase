use crate::game::replacement::CounterPlacementProhibitions;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::card_type::CoreType;
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// CR 701.68a + CR 701.68b: the creatures `player` can blight for `count`: those
/// they control that exist (CR 702.26b: a phased-out creature is treated as
/// though it doesn't exist, CR 702.26d: though it stays on the battlefield)
/// and that the counters can be put on (CR 614.17). The "can't" effects are
/// found once per scan and matched read-only through the replacement
/// pipeline's own shape predicate and applicability authority; no state is
/// cloned and the replacement index is neither rebuilt nor consulted. The real
/// placement still runs the pipeline, so CR 616.1 ordering among Doubling
/// Season, Vorinclex and Vizier of Remedies is chosen when the counters are put.
fn blightable_creatures(
    state: &GameState,
    player: PlayerId,
    count: u32,
) -> impl Iterator<Item = ObjectId> + '_ {
    let prohibitions = CounterPlacementProhibitions::active(state);
    state.battlefield.iter().copied().filter(move |id| {
        state.objects.get(id).is_some_and(|obj| {
            obj.controller == player
                && !obj.is_emblem
                && obj.is_phased_in_permanent()
                && obj.card_types.core_types.contains(&CoreType::Creature)
                && !prohibitions.forbid_object_counters(
                    state,
                    player,
                    *id,
                    CounterType::Minus1Minus1,
                    count,
                )
        })
    })
}

/// CR 701.68b + CR 608.2d: a player unable to put the counters on a creature
/// they control can't choose to blight, so the "you may" is an impossible option.
/// Judged before the offer for the acting player, and for an any-player 'may', for
/// each seat when it is offered and again when it accepts
/// (`effects::offered_seat_cannot_accept`).
pub(crate) fn optional_blight_is_infeasible(state: &GameState, ability: &ResolvedAbility) -> bool {
    let Effect::BlightEffect { count, player } = &ability.effect else {
        return false;
    };
    // CR 101.4 + CR 608.2c: an any-player "may" is the offered seat's choice, judged for each
    // seat when it is offered and again when it accepts (`effects::offered_seat_cannot_accept`);
    // an unbound any-player ability has no blighting player yet, and `ScopedPlayer` would fall
    // back to the controller.
    if ability.optional_for.is_some() && ability.context.accepting_player.is_none() {
        return false;
    }
    crate::game::effects::resolve_player_for_context_ref(state, ability, player)
        .is_none_or(|player| blightable_creatures(state, player, *count).next().is_none())
}

/// CR 701.68a: Blight N as an effect.
///
/// "To 'blight N' means to put N -1/-1 counters on a creature you control."
/// Blight is a keyword action that has the controller *choose* (not *target*)
/// a creature they control; hexproof and shroud do not prevent it (CR 701.68a).
/// The choice is made at resolution time via `WaitingFor::EffectZoneChoice`,
/// and the counters are placed by the `EffectKind::BlightEffect` handler arm in
/// `engine_resolution_choices.rs`.
///
/// CR 701.68b: If the controller is unable to put N -1/-1 counters on a creature
/// they control, blight is a no-op.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (count, blighting_player) = match &ability.effect {
        Effect::BlightEffect { count, player } => {
            // CR 701.68a: the blighting player puts the counters on a creature
            // *they* control. Default `TargetFilter::Controller` keeps "you
            // blight"; Champion of the Weird redirects to the targeted opponent
            // ("Target opponent blights 2").
            let blighting_player =
                crate::game::effects::resolve_player_for_context_ref(state, ability, player);
            (*count, blighting_player)
        }
        _ => return Ok(()),
    };
    let Some(blighting_player) = blighting_player else {
        // CR 608.2b: a missing player blights nothing, as when no creature is eligible.
        resolve_nothing_blighted(state, ability, events);
        return Ok(());
    };

    let source_id = ability.source_id;

    // CR 701.68a: Eligible creatures are those the blighting player controls on
    // the battlefield that can take the counters.
    let eligible: Vec<ObjectId> = blightable_creatures(state, blighting_player, count).collect();

    // CR 701.68b: A player unable to put N -1/-1 counters on a creature they
    // control can't blight, so the effect does nothing.
    if eligible.is_empty() {
        resolve_nothing_blighted(state, ability, events);
        return Ok(());
    }

    // CR 701.68a: The blighting player chooses exactly one creature they control.
    // `count` (in EffectZoneChoice) is the number of creatures chosen (1);
    // `count_param` carries N — the number of -1/-1 counters to place.
    state.waiting_for = WaitingFor::EffectZoneChoice {
        player: blighting_player,
        cards: eligible,
        count: 1,
        min_count: 1,
        up_to: false,
        source_id,
        effect_kind: EffectKind::BlightEffect,
        zone: Zone::Battlefield,
        destination: None,
        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
        enter_transformed: false,
        enters_under_player: None,
        enters_attacking: false,
        owner_library: false,
        track_exiled_by_source: false,
        face_down_in_exile: crate::types::ability::ExileConcealment::Public,
        // CR 708.2a: Blight places -1/-1 counters; no face-down entry.
        face_down_profile: None,
        enter_with_counters: vec![],
        conditional_enter_with_counters: vec![],
        count_param: count,
        library_position: None,
        mass_library_order: None,
        is_cost_payment: false,
        enters_modified_if: None,
        duration: None,
    };

    Ok(())
}

fn resolve_nothing_blighted(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) {
    state.last_effect_count = Some(0);
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::BlightEffect,
        source_id: ability.source_id,
        subject: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::engine::apply;
    use crate::game::game_object::{PhaseOutCause, PhaseStatus};
    use crate::game::zones;
    use crate::types::ability::TargetFilter;
    use crate::types::actions::GameAction;
    use crate::types::identifiers::CardId;

    fn make_creature(state: &mut GameState, card_id: u64, controller: PlayerId) -> ObjectId {
        let id = zones::create_object(
            state,
            CardId(card_id),
            controller,
            format!("Creature {card_id}"),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_power = Some(3);
        obj.base_toughness = Some(3);
        obj.power = Some(3);
        obj.toughness = Some(3);
        id
    }

    fn blight_ability(source_id: ObjectId, controller: PlayerId, count: u32) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::BlightEffect {
                count,
                player: TargetFilter::Controller,
            },
            vec![],
            source_id,
            controller,
        )
    }

    /// Discriminator 1: Blight N places exactly N -1/-1 counters on the chosen
    /// creature. With the dispatch routed back to the no-op, this asserts 0
    /// counters land — proving the fix is reverted-fix-discriminating.
    #[test]
    fn blight_n_places_n_counters() {
        let mut state = GameState::new_two_player(7);
        let creature = make_creature(&mut state, 1, PlayerId(0));
        let ability = blight_ability(creature, PlayerId(0), 2);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Controller must choose the creature via EffectZoneChoice.
        match &state.waiting_for {
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                count_param,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(cards, &[creature]);
                assert_eq!(*count_param, 2);
            }
            other => panic!("expected EffectZoneChoice, got {other:?}"),
        }

        // Drive the choice through the real handler.
        apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectCards {
                cards: vec![creature],
            },
        )
        .unwrap();

        let obj = state.objects.get(&creature).unwrap();
        assert_eq!(
            obj.counters.get(&CounterType::Minus1Minus1).copied(),
            Some(2),
            "blight 2 must place 2 -1/-1 counters"
        );
    }

    /// Discriminator 2: CR 701.68b — controller controls no creatures, blight is
    /// a no-op (no WaitingFor, last_effect_count == Some(0), EffectResolved
    /// emitted, no panic).
    #[test]
    fn blight_no_creatures_is_noop() {
        let mut state = GameState::new_two_player(7);
        // A non-creature so the source id is valid.
        let source = zones::create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Enchantment".to_string(),
            Zone::Battlefield,
        );
        let ability = blight_ability(source, PlayerId(0), 2);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            !matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. }),
            "no eligible creatures must not prompt a choice"
        );
        assert_eq!(state.last_effect_count, Some(0));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::BlightEffect,
                ..
            }
        )));
    }

    /// CR 702.26b + CR 702.26d: a phased-out creature stays on the battlefield
    /// but is treated as though it doesn't exist, so it is neither offered for
    /// blight nor a reason blight is possible. Both `PhaseStatus::PhasedOut`
    /// causes are covered so the predicate can't key on one of them (CR 702.26g
    /// names the indirect cause; only attached permanents phase out that way).
    #[test]
    fn blight_skips_phased_out_creatures() {
        let board = |first: PhaseStatus, second: PhaseStatus| {
            let mut state = GameState::new_two_player(7);
            let first_id = make_creature(&mut state, 1, PlayerId(0));
            let second_id = make_creature(&mut state, 2, PlayerId(0));
            state.objects.get_mut(&first_id).unwrap().phase_status = first;
            state.objects.get_mut(&second_id).unwrap().phase_status = second;
            (state, first_id, second_id)
        };
        let directly = PhaseStatus::PhasedOut {
            cause: PhaseOutCause::Directly,
        };
        let indirectly = PhaseStatus::PhasedOut {
            cause: PhaseOutCause::Indirectly,
        };

        let (mut state, phased_in, phased_out) = board(PhaseStatus::PhasedIn, directly);
        assert!(
            state.battlefield.contains(&phased_out),
            "CR 702.26d: phasing out doesn't move the creature"
        );
        let ability = blight_ability(phased_in, PlayerId(0), 1);
        assert!(
            !optional_blight_is_infeasible(&state, &ability),
            "the phased-in creature can be blighted"
        );
        resolve(&mut state, &ability, &mut Vec::new()).unwrap();
        match &state.waiting_for {
            WaitingFor::EffectZoneChoice { cards, .. } => {
                assert_eq!(
                    cards,
                    &[phased_in],
                    "the phased-out creature is not offered"
                );
            }
            other => panic!("expected EffectZoneChoice, got {other:?}"),
        }

        let (mut state, first, _) = board(directly, indirectly);
        let ability = blight_ability(first, PlayerId(0), 1);
        assert!(
            optional_blight_is_infeasible(&state, &ability),
            "CR 701.68b: with every creature phased out there is no blight to choose"
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(
            !matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. }),
            "no creature exists to choose, so no choice opens"
        );
        assert_eq!(state.last_effect_count, Some(0));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::BlightEffect,
                ..
            }
        )));
    }

    /// Discriminator 3: Controller-scoped eligibility — the choice pool contains
    /// only the controller's creature, never the opponent's.
    #[test]
    fn blight_eligibility_is_controller_scoped() {
        let mut state = GameState::new_two_player(7);
        let mine = make_creature(&mut state, 1, PlayerId(0));
        let _theirs = make_creature(&mut state, 2, PlayerId(1));
        let ability = blight_ability(mine, PlayerId(0), 1);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::EffectZoneChoice { cards, .. } => {
                assert_eq!(cards, &[mine], "only the controller's creature is eligible");
            }
            other => panic!("expected EffectZoneChoice, got {other:?}"),
        }
    }

    /// Discriminator 4: Non-targeted discriminator — the controller's eligible
    /// creature has hexproof; blight still succeeds (CR 701.68a — blight is not
    /// a targeting choice, so hexproof is irrelevant).
    #[test]
    fn blight_succeeds_against_hexproof_creature() {
        use crate::types::keywords::Keyword;

        let mut state = GameState::new_two_player(7);
        let creature = make_creature(&mut state, 1, PlayerId(0));
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .keywords
            .push(Keyword::Hexproof);
        let ability = blight_ability(creature, PlayerId(0), 1);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();
        apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectCards {
                cards: vec![creature],
            },
        )
        .unwrap();

        let obj = state.objects.get(&creature).unwrap();
        assert_eq!(
            obj.counters.get(&CounterType::Minus1Minus1).copied(),
            Some(1),
            "blight ignores hexproof — it is a choice, not a target"
        );
        // CR 701.68a: a default-controller blight surfaces NO real target slot.
        // `target_filter()` returns `Some(Controller)` (mirroring the other
        // player-axis effects), but `extract_target_filter_from_effect` drops the
        // context ref via its final `is_context_ref()` guard, so the controller's
        // creature choice is never declared as a target (hexproof is irrelevant).
        assert!(
            crate::types::ability::extract_target_filter_from_effect(&Effect::BlightEffect {
                count: 1,
                player: TargetFilter::Controller,
            })
            .is_none(),
            "default 'you blight' must surface no target slot"
        );
        let _ = TargetFilter::Any; // keep import used across cfg
    }

    /// Discriminator 5: CR 614.1 replacement-aware placement — a counter-doubling
    /// replacement is active; blight 1 results in 2 -1/-1 counters, confirming
    /// the handler routes through `add_counter_with_replacement`.
    #[test]
    fn blight_is_replacement_aware() {
        use crate::types::ability::{QuantityModification, ReplacementDefinition};
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(7);
        let creature = make_creature(&mut state, 1, PlayerId(0));

        // CR 614.1a: counter-doubling replacement effect (Doubling Season-class
        // "those counters" wording — counter_match left None).
        let doubler = zones::create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Doubling Season".to_string(),
            Zone::Battlefield,
        );
        let repl = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(QuantityModification::DOUBLE);
        state
            .objects
            .get_mut(&doubler)
            .unwrap()
            .replacement_definitions = vec![repl].into();

        let ability = blight_ability(creature, PlayerId(0), 1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectCards {
                cards: vec![creature],
            },
        )
        .unwrap();

        let obj = state.objects.get(&creature).unwrap();
        assert_eq!(
            obj.counters.get(&CounterType::Minus1Minus1).copied(),
            Some(2),
            "counter-doubling replacement (CR 614.1a) must double blight's counters"
        );
    }

    /// Discriminator 6 (player-redirect): CR 701.68a — "Target opponent blights
    /// N" (Champion of the Weird) makes the *targeted player*, not the
    /// controller, blight. The prompt goes to the opponent and only the
    /// opponent's creatures are eligible. Reverting the `player` plumbing (back
    /// to `ability.controller`) flips both assertions: the prompt would address
    /// PlayerId(0) and scope eligibility to PlayerId(0)'s creature.
    #[test]
    fn blight_redirects_to_targeted_opponent() {
        use crate::types::ability::{ControllerRef, TargetRef, TypedFilter};

        let mut state = GameState::new_two_player(7);
        let controller = PlayerId(0);
        let opponent = PlayerId(1);
        // Controller has a creature too — it must NOT be eligible when the
        // opponent is the blighting player.
        let mine = make_creature(&mut state, 1, controller);
        let theirs = make_creature(&mut state, 2, opponent);

        // Mirror the exact parser output for "Target opponent blights 2": an
        // Opponent player filter carried in `player` (a non-context-ref target),
        // with the chosen opponent in `targets` (declared at announcement).
        let ability = ResolvedAbility::new(
            Effect::BlightEffect {
                count: 2,
                player: TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                ),
            },
            vec![TargetRef::Player(opponent)],
            mine, // source is the controller's permanent
            controller,
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                count_param,
                ..
            } => {
                assert_eq!(
                    *player, opponent,
                    "the targeted opponent (not the controller) must be prompted to blight"
                );
                assert_eq!(
                    cards,
                    &[theirs],
                    "only the opponent's creatures are eligible to be blighted"
                );
                assert_eq!(*count_param, 2);
            }
            other => panic!("expected EffectZoneChoice, got {other:?}"),
        }

        // Drive the opponent's choice through the real handler.
        apply(
            &mut state,
            opponent,
            GameAction::SelectCards {
                cards: vec![theirs],
            },
        )
        .unwrap();

        assert_eq!(
            state
                .objects
                .get(&theirs)
                .unwrap()
                .counters
                .get(&CounterType::Minus1Minus1)
                .copied(),
            Some(2),
            "the opponent's chosen creature receives the 2 -1/-1 counters"
        );
        assert!(
            !state
                .objects
                .get(&mine)
                .unwrap()
                .counters
                .contains_key(&CounterType::Minus1Minus1),
            "the controller's creature is untouched when the opponent blights"
        );
    }

    /// An any-player / any-opponent "may blight 1. If a player does, …" with
    /// `ScopedPlayer` naming the blighter, plus its "if a player does" rider
    /// giving the controller 1 life. Synthetic: no printed card has this
    /// combination. The nearest printed seams are the same fan-out and
    /// acceptance site — Carrion Wurm ("Whenever this creature attacks or
    /// blocks, any player may exile three cards from their graveyard. If a
    /// player does, this creature assigns no combat damage this turn.") and
    /// Desecration Demon ("At the beginning of each combat, any opponent may
    /// sacrifice a creature of their choice. If a player does, tap this
    /// creature and put a +1/+1 counter on it.").
    fn any_seat_blight(
        source: ObjectId,
        scope: crate::types::ability::OpponentMayScope,
    ) -> ResolvedAbility {
        use crate::types::ability::{AbilityCondition, EffectOutcomeSignal, QuantityExpr};

        let mut rider = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
            vec![],
            source,
            PlayerId(0),
        );
        rider.condition = Some(AbilityCondition::EffectOutcome {
            signal: EffectOutcomeSignal::OptionalEffectPerformed,
        });
        let mut ability = ResolvedAbility::new(
            Effect::BlightEffect {
                count: 1,
                player: TargetFilter::ScopedPlayer,
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.optional = true;
        ability.optional_for = Some(scope);
        ability.sub_ability = Some(Box::new(rider));
        ability
    }

    fn noncreature_source(state: &mut GameState) -> ObjectId {
        zones::create_object(
            state,
            CardId(50),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        )
    }

    fn accept(state: &mut GameState, seat: PlayerId) {
        apply(
            state,
            seat,
            GameAction::DecideOptionalEffect { accept: true },
        )
        .expect("the offered seat accepts");
    }

    fn assert_offered(state: &GameState, seat: PlayerId, rest: &[PlayerId], why: &str) {
        match &state.waiting_for {
            WaitingFor::OpponentMayChoice {
                player, remaining, ..
            } => {
                assert_eq!(*player, seat, "{why}");
                assert_eq!(remaining.as_slice(), rest, "{why}");
            }
            other => panic!("{why}: expected OpponentMayChoice, got {other:?}"),
        }
    }

    /// Completes the eligible seat's blight and its rider: the seat chooses its
    /// only creature, which takes one -1/-1 counter, and the controller gains 1.
    fn finish_blight_by(state: &mut GameState, seat: PlayerId, creature: ObjectId) {
        match &state.waiting_for {
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                effect_kind: EffectKind::BlightEffect,
                ..
            } => {
                assert_eq!(*player, seat, "CR 701.68a: the accepting seat blights");
                assert_eq!(cards, &[creature], "only that seat's creature is offered");
            }
            other => panic!("expected the blight choice, got {other:?}"),
        }
        apply(
            state,
            seat,
            GameAction::SelectCards {
                cards: vec![creature],
            },
        )
        .expect("the blighting seat chooses its creature");
        assert_eq!(
            state.objects[&creature]
                .counters
                .get(&CounterType::Minus1Minus1)
                .copied(),
            Some(1)
        );
        assert_eq!(
            state.players[0].life, 21,
            "the \"if a player does\" rider runs once"
        );
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.stack.is_empty());
    }

    /// CR 701.68b + CR 608.2d + CR 101.4: an any-player "may blight" is judged for
    /// each seat when it is offered. The controller can't blight (no creature), so
    /// it can't choose to and is never asked; the opponent who can blight is the
    /// first seat offered.
    #[test]
    fn an_any_player_blight_is_offered_only_to_a_seat_that_can_blight() {
        use crate::types::ability::OpponentMayScope;

        let mut state = GameState::new_two_player(7);
        let source = noncreature_source(&mut state);
        let theirs = make_creature(&mut state, 2, PlayerId(1));
        let ability = any_seat_blight(source, OpponentMayScope::AnyPlayer);

        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut Vec::new(), 0)
            .unwrap();
        assert_offered(
            &state,
            PlayerId(1),
            &[],
            "CR 701.68b: the controller can't choose to blight, so the first seat offered is the opponent",
        );
        assert_eq!(state.players[0].life, 20, "no player has blighted yet");

        accept(&mut state, PlayerId(1));
        finish_blight_by(&mut state, PlayerId(1), theirs);
    }

    /// CR 701.68b + CR 608.2d + CR 101.4: a later seat that can't blight is judged
    /// at its own turn and is never offered. The controller can blight and is
    /// offered first, with the opponent still queued; when the controller declines,
    /// the opponent (no creature) is skipped and no one has blighted.
    #[test]
    fn an_any_player_blight_is_not_offered_to_a_later_seat_that_cannot() {
        use crate::types::ability::OpponentMayScope;

        let mut state = GameState::new_two_player(7);
        let source = noncreature_source(&mut state);
        make_creature(&mut state, 2, PlayerId(0));
        let ability = any_seat_blight(source, OpponentMayScope::AnyPlayer);

        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut Vec::new(), 0)
            .unwrap();
        assert_offered(
            &state,
            PlayerId(0),
            &[PlayerId(1)],
            "the controller can blight and is offered; later seats stay queued unjudged",
        );

        apply(
            &mut state,
            PlayerId(0),
            GameAction::DecideOptionalEffect { accept: false },
        )
        .expect("the offered controller declines");
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "CR 701.68b: the opponent can't blight, so it is not offered; got {:?}",
            state.waiting_for
        );
        assert!(state.stack.is_empty());
        assert_eq!(
            state.players[0].life, 20,
            "no player blighted, so the rider stays closed"
        );
    }

    /// CR 701.68b + CR 608.2d: when no seat can blight, no seat is offered and the
    /// instruction resolves as declined by all, so the "if a player does" rider
    /// never runs. Given a creature, the same seat is offered the same ability.
    #[test]
    fn an_any_player_blight_no_seat_can_take_is_offered_to_no_one() {
        use crate::types::ability::OpponentMayScope;

        let mut state = GameState::new_two_player(7);
        let source = noncreature_source(&mut state);
        let ability = any_seat_blight(source, OpponentMayScope::AnyPlayer);

        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut Vec::new(), 0)
            .unwrap();
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "CR 701.68b: no seat can choose to blight, so no one is asked; got {:?}",
            state.waiting_for
        );
        assert!(state.stack.is_empty());
        assert_eq!(
            state.players[0].life, 20,
            "no player blighted, so the rider stays closed"
        );

        make_creature(&mut state, 3, PlayerId(1));
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut Vec::new(), 0)
            .unwrap();
        assert_offered(
            &state,
            PlayerId(1),
            &[],
            "a seat that can blight is offered the same ability",
        );
    }

    /// CR 701.68b + CR 101.4: an any-opponent "may blight" is offered to the
    /// opponent even though the controller, who is never offered it, has no
    /// creature.
    #[test]
    fn an_any_opponent_blight_is_judged_for_the_opponent() {
        use crate::types::ability::OpponentMayScope;

        let mut state = GameState::new_two_player(7);
        let source = noncreature_source(&mut state);
        let theirs = make_creature(&mut state, 2, PlayerId(1));
        let ability = any_seat_blight(source, OpponentMayScope::AnyOpponent);

        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut Vec::new(), 0)
            .unwrap();
        assert_offered(
            &state,
            PlayerId(1),
            &[],
            "the opponent is offered; the controller's board is not the blighter's",
        );

        accept(&mut state, PlayerId(1));
        finish_blight_by(&mut state, PlayerId(1), theirs);
    }
}
