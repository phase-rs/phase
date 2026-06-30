use crate::game::effects::counters::{
    add_counter_with_replacement, stash_pending_counter_completion_with_actions,
};
use crate::game::game_object::DisplaySource;
use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::zones;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::card_type::CardType;
use crate::types::card_type::CoreType;
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingCounterPostAction};
use crate::types::identifiers::CardId;
use crate::types::zones::Zone;

/// CR 701.53a: Incubate N — create an Incubator token that enters the
/// battlefield with N +1/+1 counters on it.
///
/// CR 111.10i: An Incubator token is a double-faced token. Its front face
/// is a colorless Incubator artifact with "{2}: Transform this token."
/// Its back face is a 0/0 colorless Phyrexian artifact creature named
/// "Phyrexian Token."
///
/// The transform activated ability is attached via `inject_predefined_token_abilities`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let count_expr = match &ability.effect {
        Effect::Incubate { count } => count.clone(),
        _ => return Ok(()),
    };

    let controller = ability.controller;
    let n = resolve_quantity_with_targets(state, &count_expr, ability).max(0) as u32;

    // CR 701.53a: Create an Incubator token on the battlefield.
    let obj_id = zones::create_object(
        state,
        CardId(0),
        controller,
        "Incubator".to_string(),
        Zone::Battlefield,
    );

    // CR 613.7d: the Incubator token enters the battlefield, so it receives a
    // timestamp. Drawn before the `get_mut` (`next_timestamp` takes `&mut self`).
    let entry_timestamp = state.next_timestamp();

    if let Some(obj) = state.objects.get_mut(&obj_id) {
        obj.is_token = true;
        obj.display_source = DisplaySource::Token;
        // CR 111.10i: Front face is a colorless Incubator artifact.
        obj.card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Artifact],
            subtypes: vec!["Incubator".to_string()],
        };
        obj.base_card_types = obj.card_types.clone();
        obj.color = vec![];
        obj.base_color = vec![];
        // CR 400.7 + CR 302.6: Single authority for ETB state.
        obj.reset_for_battlefield_entry(state.turn_number, entry_timestamp);
    }

    // Battlefield entry: incremental re-derive candidate for this Incubator
    // token (escalates to a full pass if it sources effects, carries
    // counters, etc.).
    crate::game::layers::mark_layers_entered(state, obj_id);
    crate::game::restrictions::record_battlefield_entry(state, obj_id);
    crate::game::restrictions::record_token_created(state, obj_id);

    // CR 603.6a: The Incubator token enters the battlefield as a zone change
    // from outside the game (`from: None`) — emit `ZoneChanged` so every ETB
    // trigger matcher (Altar of the Brood's "another permanent you control
    // enters", Soul Warden, Panharmonicon, etc.) fires for it through the
    // same code path used for normal token creation. Without this the
    // Incubator enters silently and no ETB ability ever triggers (issue
    // #4238). Mirrors `token.rs::apply_create_token_after_replacement_with_created_ids`
    // and `conjure.rs`'s identical fix for the same bug class.
    let zone_change_record = state
        .objects
        .get(&obj_id)
        .expect("incubator token was just created")
        .snapshot_for_zone_change(obj_id, None, Zone::Battlefield);
    state
        .zone_changes_this_turn
        .push(zone_change_record.clone());
    events.push(GameEvent::ZoneChanged {
        object_id: obj_id,
        from: None,
        to: Zone::Battlefield,
        record: Box::new(zone_change_record),
    });

    // CR 701.53a: The Incubator enters with N +1/+1 counters.
    if n > 0
        && !add_counter_with_replacement(
            state,
            ability.controller,
            obj_id,
            CounterType::Plus1Plus1,
            n,
            events,
        )
    {
        stash_pending_counter_completion_with_actions(
            state,
            EffectKind::Incubate,
            ability.source_id,
            vec![PendingCounterPostAction::InjectPredefinedTokenAbilities { object_id: obj_id }],
        );
        return Ok(());
    }

    super::token::inject_predefined_token_abilities(state, obj_id);

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Incubate,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{Effect, QuantityExpr};
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    fn make_incubate_ability(count: QuantityExpr) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Incubate { count },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn incubate_records_zone_change_for_etb_triggers() {
        let mut state = GameState::new_two_player(7);
        let mut events = Vec::new();
        let ability = make_incubate_ability(QuantityExpr::Fixed { value: 1 });

        resolve(&mut state, &ability, &mut events).unwrap();

        let zone_change = events
            .iter()
            .find_map(|event| match event {
                GameEvent::ZoneChanged {
                    object_id,
                    from,
                    to,
                    ..
                } => Some((*object_id, *from, *to)),
                _ => None,
            })
            .expect("incubate emits ZoneChanged so ETB triggers (e.g. Altar of the Brood) fire");

        assert_eq!(zone_change.1, None);
        assert_eq!(zone_change.2, Zone::Battlefield);
        assert_eq!(state.zone_changes_this_turn.len(), 1);
        assert_eq!(state.zone_changes_this_turn[0].object_id, zone_change.0);
        assert_eq!(state.zone_changes_this_turn[0].from_zone, None);
        assert_eq!(state.zone_changes_this_turn[0].to_zone, Zone::Battlefield);
    }

    /// Issue #4238: Altar of the Brood's "another permanent you control
    /// enters" trigger must fire when Incubate creates the Incubator token.
    /// Mirrors `token::tests::catalog_pest_dies_trigger_fires_through_zone_pipeline`'s
    /// pattern of resolving an effect, then calling `process_triggers`
    /// directly on the resulting events (the same chokepoint every
    /// cast/resolve path uses) to confirm the trigger is queued.
    #[test]
    fn incubate_token_fires_another_permanent_enters_trigger() {
        use crate::game::scenario::{GameScenario, P0};
        use crate::game::triggers::process_triggers;
        use crate::types::phase::Phase;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let altar_id = scenario
            .add_creature(P0, "Altar Stand-in", 0, 0)
            .from_oracle_text(
                "Whenever another permanent you control enters, each opponent mills a card.",
            )
            .id();

        let mut runner = scenario.build();
        let ability = ResolvedAbility::new(
            Effect::Incubate {
                count: QuantityExpr::Fixed { value: 1 },
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(runner.state_mut(), &ability, &mut events).unwrap();
        process_triggers(runner.state_mut(), &events);

        assert_eq!(
            runner.state().stack.len(),
            1,
            "Altar's ETB trigger should be queued on the stack"
        );
        let triggered = runner.state().stack[0]
            .ability()
            .expect("triggered ability");
        assert_eq!(runner.state().stack[0].source_id, altar_id);
        assert!(matches!(triggered.effect, Effect::Mill { .. }));
    }

    #[test]
    fn incubate_creates_artifact_token_with_counters() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();
        let ability = make_incubate_ability(QuantityExpr::Fixed { value: 3 });

        resolve(&mut state, &ability, &mut events).unwrap();

        // Should have created one artifact on the battlefield
        let incubators: Vec<_> = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|obj| obj.card_types.subtypes.iter().any(|s| s == "Incubator"))
            .collect();
        assert_eq!(incubators.len(), 1);
        let inc = incubators[0];
        assert!(inc.is_token);
        assert!(inc.card_types.core_types.contains(&CoreType::Artifact));
        assert!(!inc.card_types.core_types.contains(&CoreType::Creature));
        assert!(inc.color.is_empty()); // colorless
        assert_eq!(inc.name, "Incubator");
        // 3 +1/+1 counters
        assert_eq!(inc.counters.get(&CounterType::Plus1Plus1).copied(), Some(3));
        assert_eq!(inc.abilities.len(), 1);
        assert!(matches!(*inc.abilities[0].effect, Effect::Transform { .. }));
        assert!(inc.back_face.is_some());
    }

    #[test]
    fn incubate_zero_creates_token_without_counters() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();
        let ability = make_incubate_ability(QuantityExpr::Fixed { value: 0 });

        resolve(&mut state, &ability, &mut events).unwrap();

        let incubators: Vec<_> = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|obj| obj.card_types.subtypes.iter().any(|s| s == "Incubator"))
            .collect();
        assert_eq!(incubators.len(), 1);
        assert_eq!(
            incubators[0]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            None
        );
    }

    #[test]
    fn incubate_multiple_creates_separate_tokens() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        // Two separate incubate calls should create two tokens
        let ability = make_incubate_ability(QuantityExpr::Fixed { value: 2 });
        resolve(&mut state, &ability, &mut events).unwrap();
        resolve(&mut state, &ability, &mut events).unwrap();

        let incubators: Vec<_> = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|obj| obj.card_types.subtypes.iter().any(|s| s == "Incubator"))
            .collect();
        assert_eq!(incubators.len(), 2);
    }
}
