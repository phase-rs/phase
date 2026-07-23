use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::replacement::{self, ReplacementResult};
use crate::game::zone_pipeline::{self, BatchMoveResult, ZoneMoveRequest};
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::proposed_event::ProposedEvent;
use crate::types::zones::Zone;

/// CR 701.17a: Mill N — put the top N cards of a player's library into their graveyard.
/// When `destination` is set to a zone other than Graveyard (e.g., Exile or Hand),
/// cards are moved there instead -- building block for top-of-library move patterns.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (num_cards, destination, target_player) = match &ability.effect {
        Effect::Mill {
            count,
            destination,
            target,
        } => (
            // CR 107.1b: Resolve with full ability context so `QuantityRef::Variable { "X" }`
            // reads the caster-chosen X from the resolving ability, and clamp a
            // negative result to zero before the `as usize` cast. Mill shares the
            // Draw/Mill/Discard dynamic-count parser, so a subtractive count
            // ("mill cards equal to A minus B" with B > A) resolves negative;
            // without the clamp `-1 as usize` wraps huge and the downstream
            // library-size `min` mills the entire library instead of nothing.
            // Mirrors the guard in `draw.rs` / `discard.rs`.
            resolve_quantity_with_targets(state, count, ability).max(0) as usize,
            *destination,
            // CR 701.17a + CR 115.1: Mirror Draw/Scry/Surveil — context-ref
            // target filters (Controller, PostReplacementSourceController,
            // ParentTargetController, etc.) must consult state slots, not
            // `ability.targets`, so a Mill sub-ability chained off a Player-
            // targeted parent does not inherit the parent's chosen player.
            super::resolve_player_for_context_ref(state, ability, target),
        ),
        _ => (1, Zone::Graveyard, ability.controller),
    };

    if destination == Zone::Graveyard {
        let proposed = ProposedEvent::Mill {
            player_id: target_player,
            count: num_cards as u32,
            destination,
            applied: Default::default(),
        };

        match replacement::replace_event(state, proposed, events) {
            ReplacementResult::Execute(event) => {
                // CR 616.1: a per-card pause leaves `state.waiting_for` set and
                // the tail parked; bail before emitting `EffectResolved` so the
                // surfaced prompt is not clobbered. The resume path
                // (`zone_pipeline::drain_pending_batch_deliveries`) finishes the
                // batch.
                if !apply_mill_after_replacement(state, event, events)? {
                    return Ok(());
                }
            }
            ReplacementResult::Prevented => {}
            ReplacementResult::NeedsChoice(player) => {
                state.waiting_for =
                    crate::game::replacement::replacement_choice_waiting_for(player, state);
                return Ok(());
            }
        }
    } else if !apply_mill_after_replacement(
        state,
        ProposedEvent::Mill {
            player_id: target_player,
            count: num_cards as u32,
            destination,
            applied: Default::default(),
        },
        events,
    )? {
        // CR 616.1: per-card pause (see above) — bail before `EffectResolved`.
        return Ok(());
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

/// CR 701.17a-b: Apply an accepted mill event after replacement effects have
/// had a chance to modify the count.
///
/// Returns `true` when every milled card was delivered, `false` when a per-card
/// `Moved` replacement surfaced a CR 616.1 ordering choice that parked the batch
/// (`state.waiting_for` is left set, the undelivered tail in
/// the active `BatchDelivery` frame). Callers that reset `state.waiting_for`
/// after applying an accepted event MUST early-return on `false` so they don't
/// clobber the parked prompt (mirrors the `apply_etb_counters` early-return
/// precedent in `handle_replacement_choice`).
pub fn apply_mill_after_replacement(
    state: &mut GameState,
    event: ProposedEvent,
    events: &mut Vec<GameEvent>,
) -> Result<bool, EffectError> {
    let ProposedEvent::Mill {
        player_id,
        count,
        destination,
        ..
    } = event
    else {
        return Ok(true);
    };

    let player = state
        .players
        .iter()
        .find(|p| p.id == player_id)
        .ok_or(EffectError::PlayerNotFound)?;

    // CR 701.17b: A player can't mill more cards than are in their library;
    // if instructed to, they mill as many as possible.
    let count = (count as usize).min(player.library.len());
    let cards_to_mill: Vec<_> = player.library.iter().take(count).copied().collect();
    state.last_effect_count = Some(cards_to_mill.len() as i32);

    // CR 701.17a + CR 614.6: Route each milled card through the zone-change
    // pipeline (the shared `zone_pipeline::move_objects_simultaneously` batch
    // entry) rather than a raw `zones::move_to_zone`. The raw move never
    // proposed a per-card ZoneChange, so `Moved` redirects ("if a card would be
    // put into a graveyard from anywhere, exile it instead" — Rest in Peace /
    // Leyline of the Void class) never fired for milled cards. The batch entry
    // proposes each inner ZoneChange and consults those replacements before
    // delivery, fixing the known bug.
    //
    // Attribution: the milled card itself anchors the `Effect` cause (mill to a
    // graveyard creates no exile-link, and a `Moved` replacement's `valid_card`
    // is evaluated against the moved card, so this matches the pre-pipeline raw
    // behavior while enabling the replacement consult).
    //
    // CR 616.1: a per-card ordering choice (two simultaneous graveyard→exile
    // redirects) parks `state.waiting_for` + the undelivered tail in
    // the active `BatchDelivery` frame; the replacement-choice resume path
    // (`zone_pipeline::drain_pending_batch_deliveries`) finishes the batch.
    let reqs: Vec<ZoneMoveRequest> = cards_to_mill
        .iter()
        .map(|&obj_id| ZoneMoveRequest::effect(obj_id, destination, obj_id))
        .collect();
    Ok(matches!(
        zone_pipeline::move_objects_simultaneously(state, reqs, events),
        BatchMoveResult::Done
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::effects::resolve_ability_chain;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, PlayerFilter, QuantityExpr, QuantityRef,
        ReplacementDefinition, ReplacementPlayerScope, TargetFilter, TargetRef,
    };
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::zones::Zone;

    fn make_mill_ability(num_cards: u32, targets: Vec<TargetRef>) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Mill {
                count: QuantityExpr::Fixed {
                    value: num_cards as i32,
                },
                target: TargetFilter::Any,
                destination: Zone::Graveyard,
            },
            targets,
            ObjectId(100),
            PlayerId(0),
        )
    }

    /// CR 614.6: a graveyard→exile `Moved` redirect (Rest in Peace / Leyline of
    /// the Void class). Two of these are simultaneously applicable to each milled
    /// card, so the CR 616.1 materiality classifier prompts for ordering per card.
    fn graveyard_exile_redirect(description: &str) -> ReplacementDefinition {
        use crate::types::zones::EtbTapState;
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .destination_zone(Zone::Graveyard)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    destination: Zone::Exile,
                    origin: None,
                    target: TargetFilter::SelfRef,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                },
            ))
            .description(description.to_string())
    }

    /// P1 regression (round-2 review): `apply_mill_after_replacement` MUST report
    /// a per-card pause to its caller (return `false`) rather than swallow it.
    ///
    /// The nested Mill-event resume path (`handle_replacement_choice`'s Mill arm)
    /// applies an accepted Mill event and then unconditionally resets
    /// `waiting_for` to Priority. If `apply_mill_after_replacement` swallowed a
    /// per-card CR 616.1 pause (the old `let _ =`), that reset would clobber the
    /// parked prompt and strand the first paused milled card. This test drives the
    /// shared seam directly: with two simultaneously-applicable graveyard→exile
    /// redirects, the first milled card surfaces a CR 616.1 ordering prompt, so
    /// the helper must return `false`, leave `state.waiting_for` set to that
    /// prompt, and park the undelivered tail.
    #[test]
    fn apply_mill_after_replacement_reports_per_card_pause_to_caller() {
        let mut state = GameState::new_two_player(42);

        for (description, source_card) in [
            ("Rest in Peace redirect", CardId(1000)),
            ("Leyline of the Void redirect", CardId(1001)),
        ] {
            let source = create_object(
                &mut state,
                source_card,
                PlayerId(0),
                "Redirect Source".to_string(),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&source)
                .unwrap()
                .replacement_definitions = vec![graveyard_exile_redirect(description)].into();
        }

        for i in 0..3 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(1),
                format!("Milled {i}"),
                Zone::Library,
            );
        }

        let mut events = Vec::new();
        let delivered = apply_mill_after_replacement(
            &mut state,
            ProposedEvent::Mill {
                player_id: PlayerId(1),
                count: 3,
                destination: Zone::Graveyard,
                applied: Default::default(),
            },
            &mut events,
        )
        .expect("mill applies");

        // The pause signal must reach the caller so it can early-return before
        // resetting `waiting_for`.
        assert!(
            !delivered,
            "a per-card CR 616.1 pause must be reported as a non-delivery (false)"
        );
        assert!(
            matches!(
                state.waiting_for,
                crate::types::game_state::WaitingFor::ReplacementChoice { .. }
            ),
            "the per-card ordering prompt must be parked in waiting_for"
        );
        assert!(
            state.active_batch_delivery().is_some(),
            "the undelivered tail must be stashed for the resume path"
        );
    }

    #[test]
    fn mill_3_moves_top_3_from_library_to_graveyard() {
        let mut state = GameState::new_two_player(42);
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(1),
                format!("Card {}", i),
                Zone::Library,
            );
        }
        let top_3: Vec<_> = state.players[1]
            .library
            .iter()
            .take(3)
            .copied()
            .collect::<Vec<_>>();

        let ability = make_mill_ability(3, vec![TargetRef::Player(PlayerId(1))]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[1].library.len(), 2);
        assert_eq!(state.players[1].graveyard.len(), 3);
        for id in &top_3 {
            assert!(state.players[1].graveyard.contains(id));
        }
    }

    #[test]
    fn mill_with_empty_library_does_nothing() {
        let mut state = GameState::new_two_player(42);
        assert!(state.players[1].library.is_empty());

        let ability = make_mill_ability(3, vec![TargetRef::Player(PlayerId(1))]);
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(state.players[1].graveyard.is_empty());
    }

    #[test]
    fn mill_with_fewer_cards_than_requested_mills_available() {
        let mut state = GameState::new_two_player(42);
        for i in 0..2 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(1),
                format!("Card {}", i),
                Zone::Library,
            );
        }

        let ability = make_mill_ability(5, vec![TargetRef::Player(PlayerId(1))]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.players[1].library.is_empty());
        assert_eq!(state.players[1].graveyard.len(), 2);
    }

    #[test]
    fn opponent_mill_replacement_doubles_resolved_mill_count() {
        let mut state = GameState::new_two_player(42);
        let replacement_source = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            "Mill Doubler".to_string(),
            Zone::Battlefield,
        );
        let mut replacement =
            ReplacementDefinition::new(ReplacementEvent::Mill).execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Mill {
                    count: QuantityExpr::Multiply {
                        factor: 2,
                        inner: Box::new(QuantityExpr::Ref {
                            qty: QuantityRef::EventContextAmount,
                        }),
                    },
                    target: TargetFilter::Controller,
                    destination: Zone::Graveyard,
                },
            ));
        replacement.valid_player = Some(ReplacementPlayerScope::Opponent);
        state
            .objects
            .get_mut(&replacement_source)
            .unwrap()
            .replacement_definitions = vec![replacement].into();
        for i in 0..8 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(1),
                format!("Card {}", i),
                Zone::Library,
            );
        }

        let ability = make_mill_ability(3, vec![TargetRef::Player(PlayerId(1))]);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[1].library.len(), 2);
        assert_eq!(state.players[1].graveyard.len(), 6);
    }

    #[test]
    fn opponent_mill_replacement_does_not_apply_to_controller_mill() {
        let mut state = GameState::new_two_player(42);
        let replacement_source = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            "Mill Doubler".to_string(),
            Zone::Battlefield,
        );
        let mut replacement =
            ReplacementDefinition::new(ReplacementEvent::Mill).execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Mill {
                    count: QuantityExpr::Multiply {
                        factor: 2,
                        inner: Box::new(QuantityExpr::Ref {
                            qty: QuantityRef::EventContextAmount,
                        }),
                    },
                    target: TargetFilter::Controller,
                    destination: Zone::Graveyard,
                },
            ));
        replacement.valid_player = Some(ReplacementPlayerScope::Opponent);
        state
            .objects
            .get_mut(&replacement_source)
            .unwrap()
            .replacement_definitions = vec![replacement].into();
        for i in 0..8 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }

        let ability = make_mill_ability(3, vec![TargetRef::Player(PlayerId(0))]);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].library.len(), 5);
        assert_eq!(state.players[0].graveyard.len(), 3);
    }

    /// Issue #310 (Maddening Cacophony / Fractured Sanity): "Each opponent
    /// mills N cards." parses as `Effect::Mill { target: Controller }` with
    /// `player_scope: Opponent` on the surrounding ability. The
    /// player_scope iteration loop must rebind `controller` to each opponent
    /// per CR 608.2 + CR 109.5 so the inner Mill effect mills the iterated
    /// opponent — not the printed controller.
    ///
    /// Three-player coverage: opponents must be expanded in APNAP order so the
    /// "each opponent" semantics is universal, not just "the next opponent."
    #[test]
    fn player_scope_opponent_mill_targets_each_opponent_three_player_apnap() {
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        for p in 0u8..3 {
            for i in 0u64..6 {
                create_object(
                    &mut state,
                    CardId(100 + (p as u64) * 10 + i),
                    PlayerId(p),
                    format!("P{p} Library {i}"),
                    Zone::Library,
                );
            }
        }

        let mut ability = ResolvedAbility::new(
            Effect::Mill {
                count: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::Opponent);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].graveyard.len(), 0, "caster not milled");
        assert_eq!(state.players[1].graveyard.len(), 3, "opponent 1 milled");
        assert_eq!(state.players[2].graveyard.len(), 3, "opponent 2 milled");
    }

    #[test]
    fn player_scope_opponent_mill_targets_each_opponent_not_controller() {
        let mut state = GameState::new_two_player(42);
        for i in 0..8 {
            create_object(
                &mut state,
                CardId(100 + i),
                PlayerId(0),
                format!("P0 Library {i}"),
                Zone::Library,
            );
            create_object(
                &mut state,
                CardId(200 + i),
                PlayerId(1),
                format!("P1 Library {i}"),
                Zone::Library,
            );
        }

        let mut ability = ResolvedAbility::new(
            Effect::Mill {
                count: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::Opponent);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Controller (PlayerId(0)) MUST NOT be milled — only opponents.
        assert_eq!(
            state.players[0].graveyard.len(),
            0,
            "controller must not be milled by Each opponent mills"
        );
        assert_eq!(
            state.players[1].graveyard.len(),
            3,
            "each opponent must be milled"
        );
    }

    /// Issue #310 (Maddening Cacophony kicker mode): "Each opponent mills
    /// half their library, rounded up." Parses as
    /// `Effect::Mill { count: ZoneCardCount{scope: ScopedPlayer, ...}/2 ceil,
    /// target: Controller }` with `player_scope: Opponent` after the parser
    /// rewrite at `parser/oracle_effect/mod.rs` promotes the
    /// `TargetZoneCardCount{Library}` form.
    ///
    /// CR 608.2 + CR 109.5: `CountScope::ScopedPlayer` MUST bind to the
    /// iterated player's library — not the caster's. A three-player game
    /// with libraries of differing sizes (caster: 4, opponent 1: 6,
    /// opponent 2: 10) exposes the bug clearly: opponent 1 must mill
    /// `ceil(6/2)=3`, opponent 2 must mill `ceil(10/2)=5`, and the caster
    /// must NOT be milled at all. Pre-fix the rewrite emitted
    /// `CountScope::Controller`, which counted the caster's 4-card library
    /// for both, milling each opponent `ceil(4/2)=2`.
    #[test]
    fn player_scope_opponent_mill_half_their_library_uses_iterated_library() {
        use crate::types::ability::{CountScope, RoundingMode, ZoneRef};
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        // Library sizes: caster (P0) = 4, opponent 1 (P1) = 6, opponent 2 (P2) = 10.
        // Differing sizes prove the count is computed per-iterated-player,
        // not from the caster's library.
        let library_sizes = [4u64, 6u64, 10u64];
        for (p, &size) in library_sizes.iter().enumerate() {
            for i in 0..size {
                create_object(
                    &mut state,
                    CardId(100 + (p as u64) * 100 + i),
                    PlayerId(p as u8),
                    format!("P{p} Library {i}"),
                    Zone::Library,
                );
            }
        }

        let mut ability = ResolvedAbility::new(
            Effect::Mill {
                count: QuantityExpr::DivideRounded {
                    inner: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::ZoneCardCount {
                            zone: ZoneRef::Library,
                            card_types: vec![],
                            scope: CountScope::ScopedPlayer,
                            filter: None,
                        },
                    }),
                    divisor: 2,
                    rounding: RoundingMode::Up,
                },
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::Opponent);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(
            state.players[0].graveyard.len(),
            0,
            "caster must NOT be milled — player_scope=Opponent only iterates opponents"
        );
        assert_eq!(
            state.players[1].graveyard.len(),
            3,
            "opponent 1 (library=6) must mill ceil(6/2)=3 — counted from their library, not caster's"
        );
        assert_eq!(
            state.players[2].graveyard.len(),
            5,
            "opponent 2 (library=10) must mill ceil(10/2)=5 — counted from their library, not caster's"
        );
    }

    /// Issue #310: `CountScope::Controller` (caster's "your library") MUST
    /// continue to mean the caster — even inside a `player_scope` iteration.
    /// "Each player sacrifices a land for each card in YOUR hand"
    /// (Thoughts of Ruin shape) is the canonical case: the count is the
    /// caster's hand size regardless of which iterated player is sacrificing.
    /// Pin this so any future change to the per-iteration semantics keeps
    /// `Controller` distinct from `ScopedPlayer`.
    #[test]
    fn player_scope_controller_count_scope_remains_caster_perspective() {
        use crate::types::ability::{CountScope, ZoneRef};
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        // Caster (P0) hand: 5 cards. Iterated players (P1, P2) hand: 1 card each.
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(100 + i),
                PlayerId(0),
                format!("Caster Hand {i}"),
                Zone::Hand,
            );
        }
        for p in 1u8..3 {
            create_object(
                &mut state,
                CardId(200 + u64::from(p)),
                PlayerId(p),
                format!("P{p} Hand"),
                Zone::Hand,
            );
        }
        // P1 / P2 each have a 10-card library so Mill is observable.
        for p in 1u8..3 {
            for i in 0..10 {
                create_object(
                    &mut state,
                    CardId(300 + u64::from(p) * 20 + i),
                    PlayerId(p),
                    format!("P{p} Library {i}"),
                    Zone::Library,
                );
            }
        }

        // Mill N where N = "cards in your hand" (CountScope::Controller).
        // player_scope=Opponent → each opponent mills 5 (caster's hand size),
        // not 1 (their own hand size).
        let mut ability = ResolvedAbility::new(
            Effect::Mill {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::ZoneCardCount {
                        zone: ZoneRef::Hand,
                        card_types: vec![],
                        scope: CountScope::Controller,
                        filter: None,
                    },
                },
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::Opponent);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].graveyard.len(), 0, "caster not milled");
        assert_eq!(
            state.players[1].graveyard.len(),
            5,
            "opponent 1 mills 5 — count uses CASTER's hand size (5), not their own (1)"
        );
        assert_eq!(
            state.players[2].graveyard.len(),
            5,
            "opponent 2 mills 5 — count uses CASTER's hand size (5), not their own (1)"
        );
    }

    /// Issue #477 — Renegade Reaper: "Mill four cards. If at least one Angel
    /// card is milled this way, you gain 4 life." The `GainLife` sub-ability
    /// carries `AbilityCondition::ZoneChangedThisWay { Angel }`; the life gain
    /// must fire ONLY when an Angel was among the milled cards.
    ///
    /// CR 608.2c + CR 400.7: the conditional gate references the cards moved
    /// by the preceding `Mill` this resolution (`last_zone_changed_ids`).
    ///
    /// This drives the real pipeline: `resolve_ability_chain` → `Mill` (emits
    /// `ZoneChanged`, populates `last_zone_changed_ids`) → sub-ability
    /// condition check (`evaluate_condition` for `ZoneChangedThisWay`) →
    /// `GainLife`. It is a runtime test, not a shape test.
    fn renegade_reaper_chain() -> ResolvedAbility {
        use crate::types::ability::{
            AbilityCondition, TargetFilter as TF, TypeFilter, TypedFilter,
        };
        ResolvedAbility::new(
            Effect::Mill {
                count: QuantityExpr::Fixed { value: 4 },
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            },
            vec![TargetRef::Player(PlayerId(0))],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability({
            let mut gain = ResolvedAbility::new(
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 4 },
                    player: TargetFilter::Controller,
                },
                vec![],
                ObjectId(100),
                PlayerId(0),
            );
            gain.condition = Some(AbilityCondition::ZoneChangedThisWay {
                filter: TF::Typed(TypedFilter::new(TypeFilter::Subtype("Angel".to_string()))),
            });
            gain
        })
    }

    #[test]
    fn renegade_reaper_gains_life_only_when_angel_milled() {
        // --- Case A: an Angel IS among the milled cards → life gained. ---
        let mut state = GameState::new_two_player(42);
        let life_before = state.players[0].life;
        // Top of library: 3 plain cards + 1 Angel within the milled top-4.
        for i in 0..3 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Plain {i}"),
                Zone::Library,
            );
        }
        let angel = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Test Angel".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&angel)
            .unwrap()
            .card_types
            .subtypes
            .push("Angel".to_string());

        let ability = renegade_reaper_chain();
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].graveyard.len(), 4, "all 4 cards milled");
        assert_eq!(
            state.players[0].life,
            life_before + 4,
            "life must increase by 4 — an Angel was milled this way"
        );

        // --- Case B: NO Angel among the milled cards → life unchanged. ---
        let mut state = GameState::new_two_player(42);
        let life_before = state.players[0].life;
        for i in 0..4 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Plain {i}"),
                Zone::Library,
            );
        }
        let ability = renegade_reaper_chain();
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].graveyard.len(), 4, "all 4 cards milled");
        assert_eq!(
            state.players[0].life, life_before,
            "life must be unchanged — no Angel was milled this way"
        );
    }

    /// CR 107.1b: a mill count that resolves negative must clamp to 0, not wrap
    /// through the `as usize` cast and mill the whole library. Mill shares the
    /// Draw/Mill/Discard dynamic-count parser, so "mill cards equal to A minus B"
    /// (with B > A) resolves negative. Revert-probe: without the `.max(0)` the
    /// downstream library-size `min` mills the target's entire library instead of
    /// nothing.
    #[test]
    fn mill_negative_count_clamps_to_zero() {
        use crate::types::ability::{AggregateFunction, PlayerScope};

        let mut state = GameState::new_two_player(7);
        // Controller (P0): 1 card in hand, 2 in library. Opponent (P1): 3 in hand.
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Hand".into(),
            Zone::Hand,
        );
        create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "LibA".into(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "LibB".into(),
            Zone::Library,
        );
        for i in 0..3u64 {
            create_object(
                &mut state,
                CardId(10 + i),
                PlayerId(1),
                "Theirs".into(),
                Zone::Hand,
            );
        }

        // count = HandSize{You} − HandSize{Opponent} = 1 − 3 = −2.
        let count = QuantityExpr::Sum {
            exprs: vec![
                QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::Controller,
                    },
                },
                QuantityExpr::Multiply {
                    factor: -1,
                    inner: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::HandSize {
                            player: PlayerScope::Opponent {
                                aggregate: AggregateFunction::Sum,
                            },
                        },
                    }),
                },
            ],
        };
        let ability = ResolvedAbility::new(
            Effect::Mill {
                count,
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.players[0].library.len(),
            2,
            "CR 107.1b: a negative mill count must mill 0, not the whole library"
        );
        assert!(
            state.players[0].graveyard.is_empty(),
            "no card may be milled"
        );
    }
}
