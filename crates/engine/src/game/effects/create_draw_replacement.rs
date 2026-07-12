use crate::types::ability::{
    Effect, EffectError, EffectKind, ReplacementDefinition, ReplacementPlayerScope,
    ResolvedAbility, RestrictionExpiry,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::replacements::ReplacementEvent;

/// CR 614.1a + CR 614.6 + CR 514.2 + CR 121.1: Resolve
/// `Effect::CreateDrawReplacement` — install a one-shot, this-turn "the next
/// time you would draw a card this turn, [effect] instead" draw replacement
/// (Words of Worship: gain 5 life; Words of Wilding: create a 2/2 Bear token).
///
/// Mirrors `create_damage_replacement::resolve` for the `Draw` event class: it
/// builds a `ReplacementDefinition` for `ReplacementEvent::Draw` whose
/// substitute is carried in `runtime_execute` (a `ResolvedAbility`, so the
/// heterogeneous payload Effect resolves through the post-replacement
/// continuation drain in `draw_through_replacement`). The shield is one-shot
/// (`consume_on_apply`, CR 614.6) and dropped at end-of-turn cleanup
/// (`expiry: EndOfTurn`, CR 514.2).
///
/// Player scope: anchored at resolution time via `source_controller` +
/// `valid_player: You` so "you would draw" follows the activating player even
/// if the Words permanent leaves or changes controller before the draw (CR
/// 113.7a / CR 611.2a). NOTE: `valid_card` is deliberately NOT set: a
/// `ProposedEvent::Draw` has no `affected_object_id`, so a `valid_card:
/// SelfRef` gate would never match.
///
/// The shield lives in `pending_damage_replacements` under the sentinel
/// `ObjectId(0)` (same game-state pending slot used for resolution-time damage
/// shields) so it survives source zone changes.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::CreateDrawReplacement { replacement_effect } = &ability.effect else {
        return Err(EffectError::InvalidParam(
            "expected CreateDrawReplacement effect".to_string(),
        ));
    };

    // CR 614.6: the substitute that runs once in place of the replaced draw.
    // Capture it as a `ResolvedAbility` so the post-replacement continuation
    // drain (`apply_post_replacement_resolved_effect`) dispatches it directly
    // with the source/controller bound at install time (CR 121.1).
    let substitute = ResolvedAbility::new(
        (**replacement_effect).clone(),
        vec![],
        ability.source_id,
        ability.controller,
    );

    // CR 614.1a + CR 113.7a: anchor the installing controller at resolution
    // time so the shield outlives the source permanent's zone/controller.
    // CR 121.6b: a runtime shield substitutes ONE individual card draw ("the next
    // time you would draw a card this turn, ... instead"), not the instruction count.
    let mut shield = ReplacementDefinition::new(ReplacementEvent::Draw)
        .draw_scope(crate::types::ability::DrawReplacementScope::IndividualDraw);
    shield.runtime_execute = Some(Box::new(substitute));
    shield.consume_on_apply = true; // CR 614.6: one-shot ("the next time").
    shield.expiry = Some(RestrictionExpiry::EndOfTurn); // CR 514.2: "this turn".
    shield.source_controller = Some(ability.controller);
    shield.valid_player = Some(ReplacementPlayerScope::You);

    state.pending_damage_replacements.push(shield);

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::CreateDrawReplacement,
        source_id: ability.source_id,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::scenario::{GameScenario, P0, P1};
    use crate::types::ability::{QuantityExpr, TargetFilter};
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    /// A bare `Draw 1` for the given player, hosted on `source`.
    fn draw_one_for(player: PlayerId, source: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Player,
            },
            vec![crate::types::ability::TargetRef::Player(player)],
            source,
            player,
        )
    }

    fn gain_five_replacement(source: ObjectId, controller: PlayerId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::CreateDrawReplacement {
                replacement_effect: Box::new(Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 5 },
                    player: TargetFilter::Controller,
                }),
            },
            vec![],
            source,
            controller,
        )
    }

    /// Words of Worship, end-to-end: install the draw replacement, then run a
    /// real draw for the controller. DISCRIMINATING: the card is NOT drawn
    /// (hand unchanged) AND the controller gains 5 life AND the shield is
    /// consumed. Reverting the EDIT-3 pre-zero fix makes the card ALSO get
    /// drawn (hand +1) — this test fails. Reverting the variant makes the
    /// effect Unimplemented — the card is drawn and no life gained — also fails.
    #[test]
    fn worship_next_draw_gains_life_no_card_drawn() {
        let mut sc = GameScenario::new();
        let source = sc.add_creature(P0, "Words of Worship", 0, 0).id();
        let _top = sc.add_card_to_library_top(P0, "Mountain");
        let mut state = sc.state;
        let start_life = state.players[0].life;
        let start_hand = state.players[0].hand.len();

        let mut events = Vec::new();
        resolve(&mut state, &gain_five_replacement(source, P0), &mut events).unwrap();
        assert_eq!(
            state.pending_damage_replacements.len(),
            1,
            "the draw-replacement shield must be installed in pending game state"
        );
        assert!(
            state.objects[&source].replacement_definitions.is_empty(),
            "shield must not depend on the source permanent staying on the battlefield"
        );

        let mut events = Vec::new();
        crate::game::effects::draw::resolve(&mut state, &draw_one_for(P0, source), &mut events)
            .unwrap();

        assert_eq!(
            state.players[0].hand.len(),
            start_hand,
            "CR 614.6: the draw is replaced — no card is drawn"
        );
        assert_eq!(
            state.players[0].life,
            start_life + 5,
            "the substitute (gain 5 life) must run in place of the draw"
        );
        assert!(
            state.pending_damage_replacements[0].is_consumed,
            "CR 614.6: the one-shot shield is consumed after firing"
        );
    }

    /// CR 614.6: the shield is one-shot. A second draw the same turn is normal.
    #[test]
    fn one_shot_consumed_second_draw_is_normal() {
        let mut sc = GameScenario::new();
        let source = sc.add_creature(P0, "Words of Worship", 0, 0).id();
        let first = sc.add_card_to_library_top(P0, "Mountain");
        let second = sc.add_card_to_library_top(P0, "Forest");
        let mut state = sc.state;

        let mut events = Vec::new();
        resolve(&mut state, &gain_five_replacement(source, P0), &mut events).unwrap();

        // First draw: replaced (no card). `second` is on top now.
        let mut events = Vec::new();
        crate::game::effects::draw::resolve(&mut state, &draw_one_for(P0, source), &mut events)
            .unwrap();
        assert!(!state.players[0].hand.contains(&second));

        // Second draw: normal — top card lands in hand.
        let mut events = Vec::new();
        crate::game::effects::draw::resolve(&mut state, &draw_one_for(P0, source), &mut events)
            .unwrap();
        assert!(
            state.players[0].hand.contains(&second),
            "the second draw this turn is unaffected by the consumed one-shot shield"
        );
        let _ = first;
    }

    /// CR 514.2: the shield expires at end-of-turn cleanup if unused; a draw on
    /// the following turn is normal.
    #[test]
    fn expires_at_end_of_turn_if_unused() {
        let mut sc = GameScenario::new();
        let source = sc.add_creature(P0, "Words of Worship", 0, 0).id();
        let top = sc.add_card_to_library_top(P0, "Mountain");
        let mut state = sc.state;

        let mut events = Vec::new();
        resolve(&mut state, &gain_five_replacement(source, P0), &mut events).unwrap();
        assert_eq!(state.pending_damage_replacements.len(), 1);

        let mut cleanup_events = Vec::new();
        crate::game::turns::execute_cleanup(&mut state, &mut cleanup_events);
        assert!(
            state.pending_damage_replacements.is_empty(),
            "CR 514.2: the 'this turn' shield is dropped at end-of-turn cleanup"
        );

        let mut events = Vec::new();
        crate::game::effects::draw::resolve(&mut state, &draw_one_for(P0, source), &mut events)
            .unwrap();
        assert!(
            state.players[0].hand.contains(&top),
            "after the shield expires, the next-turn draw is normal"
        );
    }

    /// Words of Wilding: payload is a Token. DISCRIMINATING: a 2/2 Bear enters
    /// the battlefield and no card is drawn.
    #[test]
    fn wilding_creates_bear_instead() {
        use crate::parser::oracle_effect::parse_effect;
        let mut sc = GameScenario::new();
        let source = sc.add_creature(P0, "Words of Wilding", 0, 0).id();
        let _top = sc.add_card_to_library_top(P0, "Mountain");
        let mut state = sc.state;
        let start_hand = state.players[0].hand.len();
        let bf_before = state
            .objects
            .values()
            .filter(|o| o.zone == Zone::Battlefield && o.controller == P0)
            .count();

        let token_payload = parse_effect("create a 2/2 green Bear creature token");
        assert!(
            matches!(token_payload, Effect::Token { .. }),
            "Wilding payload must parse to a Token effect, got {token_payload:?}"
        );
        let install = ResolvedAbility::new(
            Effect::CreateDrawReplacement {
                replacement_effect: Box::new(token_payload),
            },
            vec![],
            source,
            P0,
        );

        let mut events = Vec::new();
        resolve(&mut state, &install, &mut events).unwrap();

        let mut events = Vec::new();
        crate::game::effects::draw::resolve(&mut state, &draw_one_for(P0, source), &mut events)
            .unwrap();

        assert_eq!(
            state.players[0].hand.len(),
            start_hand,
            "the draw is replaced by token creation — no card drawn"
        );
        let bears = state
            .objects
            .values()
            .filter(|o| {
                o.zone == Zone::Battlefield
                    && o.controller == P0
                    && o.card_types.core_types.contains(&CoreType::Creature)
                    && o.card_types
                        .subtypes
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case("Bear"))
            })
            .count();
        assert_eq!(bears, 1, "a 2/2 Bear token must be created instead");
        let bf_after = state
            .objects
            .values()
            .filter(|o| o.zone == Zone::Battlefield && o.controller == P0)
            .count();
        assert_eq!(
            bf_after,
            bf_before + 1,
            "exactly one new permanent (the Bear)"
        );
    }

    /// Source-player scope: the shield ("you would draw") does NOT replace an
    /// opponent's draw this turn.
    #[test]
    fn does_not_affect_opponent_draw() {
        let mut sc = GameScenario::new();
        let source = sc.add_creature(P0, "Words of Worship", 0, 0).id();
        let opp_top = sc.add_card_to_library_top(P1, "Island");
        let mut state = sc.state;
        let opp_start_life = state.players[1].life;

        let mut events = Vec::new();
        resolve(&mut state, &gain_five_replacement(source, P0), &mut events).unwrap();

        let mut events = Vec::new();
        crate::game::effects::draw::resolve(&mut state, &draw_one_for(P1, source), &mut events)
            .unwrap();

        assert!(
            state.players[1].hand.contains(&opp_top),
            "the opponent's draw is unaffected — 'you would draw' is source-player scoped"
        );
        assert_eq!(
            state.players[1].life, opp_start_life,
            "the opponent does not gain the controller's substitute life"
        );
    }

    /// CR 113.7a: the one-shot replacement survives the source leaving the
    /// battlefield before the draw fires.
    #[test]
    fn survives_source_leaving_before_draw() {
        let mut sc = GameScenario::new();
        let source = sc.add_creature(P0, "Words of Worship", 0, 0).id();
        let _top = sc.add_card_to_library_top(P0, "Mountain");
        let mut state = sc.state;
        let start_life = state.players[0].life;
        let start_hand = state.players[0].hand.len();

        let mut events = Vec::new();
        resolve(&mut state, &gain_five_replacement(source, P0), &mut events).unwrap();
        state.objects.get_mut(&source).unwrap().zone = Zone::Graveyard;

        let mut events = Vec::new();
        crate::game::effects::draw::resolve(&mut state, &draw_one_for(P0, source), &mut events)
            .unwrap();

        assert_eq!(state.players[0].hand.len(), start_hand);
        assert_eq!(state.players[0].life, start_life + 5);
    }

    /// CR 611.2a: the replacement stays scoped to the activating player even if
    /// the source permanent changes controller before the draw.
    #[test]
    fn stays_scoped_to_activating_player_after_control_change() {
        let mut sc = GameScenario::new();
        let source = sc.add_creature(P0, "Words of Worship", 0, 0).id();
        let p0_top = sc.add_card_to_library_top(P0, "Mountain");
        let p1_top = sc.add_card_to_library_top(P1, "Island");
        let mut state = sc.state;
        let p0_start_life = state.players[0].life;

        let mut events = Vec::new();
        resolve(&mut state, &gain_five_replacement(source, P0), &mut events).unwrap();
        state.objects.get_mut(&source).unwrap().controller = P1;

        // P0's draw is still replaced (P0 activated the ability).
        let mut events = Vec::new();
        crate::game::effects::draw::resolve(&mut state, &draw_one_for(P0, source), &mut events)
            .unwrap();
        assert!(!state.players[0].hand.contains(&p0_top));
        assert_eq!(state.players[0].life, p0_start_life + 5);

        // P1's draw is unaffected even though they now control the source.
        let mut events = Vec::new();
        crate::game::effects::draw::resolve(&mut state, &draw_one_for(P1, source), &mut events)
            .unwrap();
        assert!(state.players[1].hand.contains(&p1_top));
    }
}
