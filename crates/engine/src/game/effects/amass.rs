use crate::game::effects::counters::add_counter_with_replacement;
use crate::game::effects::token::apply_create_token_after_replacement;
use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::card_type::CoreType;
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaColor;
use crate::types::proposed_event::{EtbTapState, ProposedEvent, TokenCharacteristics, TokenSpec};
use std::collections::HashSet;

/// CR 701.47a: Amass [subtype] N.
///
/// If you don't control an Army creature, create a 0/0 black [subtype] Army
/// creature token. Choose an Army creature you control. Put N +1/+1 counters
/// on that creature. If it isn't a [subtype], it becomes a [subtype] in
/// addition to its other types.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (subtype, count_expr) = match &ability.effect {
        Effect::Amass { subtype, count } => (subtype.clone(), count.clone()),
        _ => return Ok(()),
    };

    let controller = ability.controller;
    let n = resolve_quantity_with_targets(state, &count_expr, ability).max(0) as u32;

    // CR 701.47a: Find an existing Army creature on the controller's battlefield.
    let army_id = find_army(state, controller);

    let target_id = if let Some(id) = army_id {
        id
    } else {
        let Some(id) = create_army_token(state, controller, &subtype, ability, events) else {
            return Ok(());
        };
        id
    };

    // CR 701.47a: Put N +1/+1 counters on the chosen Army.
    if n > 0 {
        add_counter_with_replacement(
            state,
            ability.controller,
            target_id,
            CounterType::Plus1Plus1,
            n,
            events,
        );
    }

    // CR 701.47a: If it isn't a [subtype], it becomes a [subtype] in addition to its other types.
    if let Some(obj) = state.objects.get_mut(&target_id) {
        if !obj
            .card_types
            .subtypes
            .iter()
            .any(|s| s.eq_ignore_ascii_case(&subtype))
        {
            obj.card_types.subtypes.push(subtype.clone());
            obj.base_card_types.subtypes.push(subtype);
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Amass,
        source_id: ability.source_id,
    });

    Ok(())
}

/// Find the first Army creature controlled by `controller` on the battlefield.
/// CR 701.47a: If multiple Armies exist, auto-select deterministically by ObjectId.
fn find_army(state: &GameState, controller: crate::types::player::PlayerId) -> Option<ObjectId> {
    state
        .battlefield
        .iter()
        .filter_map(|&id| state.objects.get(&id).map(|obj| (id, obj)))
        .filter(|(_, obj)| {
            obj.controller == controller
                && obj.card_types.core_types.contains(&CoreType::Creature)
                && obj.card_types.subtypes.iter().any(|s| s == "Army")
        })
        .map(|(id, _)| id)
        .min_by_key(|id| id.0) // deterministic: lowest ObjectId
}

/// Create a 0/0 black [subtype] Army creature token on the battlefield.
fn create_army_token(
    state: &mut GameState,
    controller: crate::types::player::PlayerId,
    subtype: &str,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Option<ObjectId> {
    let name = format!("{subtype} Army");
    let spec = TokenSpec {
        characteristics: TokenCharacteristics {
            display_name: name.clone(),
            power: Some(0),
            toughness: Some(0),
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Army".to_string(), subtype.to_string()],
            supertypes: vec![],
            colors: vec![ManaColor::Black],
            keywords: vec![],
        },
        script_name: name,
        static_abilities: vec![],
        enter_with_counters: vec![],
        tapped: false,
        enters_attacking: false,
        attach_to: None,
        sacrifice_at: None,
        source_id: ability.source_id,
        controller: ability.controller,
    };

    let proposed = ProposedEvent::CreateToken {
        owner: controller,
        spec: Box::new(spec),
        enter_tapped: EtbTapState::Unspecified,
        count: 1,
        applied: HashSet::new(),
    };

    // CR 614.16 + CR 701.47a: Token-creation replacement effects apply to the
    // Army token an amass instruction creates.
    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            apply_create_token_after_replacement(state, event, events);
            find_army(state, controller)
        }
        ReplacementResult::Prevented => None,
        ReplacementResult::NeedsChoice(player) => {
            state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::scenario::{GameScenario, P0};
    use crate::types::ability::QuantityExpr;
    use crate::types::ability::{ControllerRef, ReplacementDefinition};
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;
    use crate::types::proposed_event::{TokenCharacteristics, TokenSpec};
    use crate::types::replacements::ReplacementEvent;

    fn make_amass_ability(subtype: &str, count: QuantityExpr) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Amass {
                subtype: subtype.to_string(),
                count,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn amass_creates_army_token_with_counters() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();
        let ability = make_amass_ability("Zombie", QuantityExpr::Fixed { value: 2 });

        resolve(&mut state, &ability, &mut events).unwrap();

        // Should have created one creature on the battlefield
        let armies: Vec<_> = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|obj| obj.card_types.subtypes.iter().any(|s| s == "Army"))
            .collect();
        assert_eq!(armies.len(), 1);
        let army = armies[0];
        assert!(army.is_token);
        assert_eq!(army.power, Some(0));
        assert_eq!(army.toughness, Some(0));
        assert!(army.card_types.subtypes.contains(&"Zombie".to_string()));
        assert!(army.card_types.subtypes.contains(&"Army".to_string()));
        assert_eq!(army.color, vec![ManaColor::Black]);
        // 2 +1/+1 counters
        assert_eq!(
            army.counters.get(&CounterType::Plus1Plus1).copied(),
            Some(2)
        );
    }

    #[test]
    fn amass_reuses_existing_army_and_adds_counters() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        // First amass creates the army
        let ability = make_amass_ability("Zombie", QuantityExpr::Fixed { value: 2 });
        resolve(&mut state, &ability, &mut events).unwrap();

        let army_count_before = state
            .battlefield
            .iter()
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .map(|o| o.card_types.subtypes.iter().any(|s| s == "Army"))
                    .unwrap_or(false)
            })
            .count();
        assert_eq!(army_count_before, 1);

        // Second amass should reuse the existing army
        events.clear();
        let ability2 = make_amass_ability("Zombie", QuantityExpr::Fixed { value: 3 });
        resolve(&mut state, &ability2, &mut events).unwrap();

        let armies: Vec<_> = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|obj| obj.card_types.subtypes.iter().any(|s| s == "Army"))
            .collect();
        assert_eq!(armies.len(), 1); // Still just one army
        assert_eq!(
            armies[0].counters.get(&CounterType::Plus1Plus1).copied(),
            Some(5)
        ); // 2 + 3
    }

    #[test]
    fn amass_adds_missing_subtype_to_existing_army() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        // Create with Zombie subtype
        let ability = make_amass_ability("Zombie", QuantityExpr::Fixed { value: 1 });
        resolve(&mut state, &ability, &mut events).unwrap();

        // Amass Orcs should add Orc subtype
        events.clear();
        let ability2 = make_amass_ability("Orc", QuantityExpr::Fixed { value: 1 });
        resolve(&mut state, &ability2, &mut events).unwrap();

        let army = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .find(|obj| obj.card_types.subtypes.iter().any(|s| s == "Army"))
            .unwrap();
        assert!(army.card_types.subtypes.contains(&"Zombie".to_string()));
        assert!(army.card_types.subtypes.contains(&"Orc".to_string()));
    }

    #[test]
    fn amass_creation_triggers_chatterfang_additional_squirrel() {
        let mut scenario = GameScenario::new();
        let chatterfang_replacement = ReplacementDefinition::new(ReplacementEvent::CreateToken)
            .token_owner_scope(ControllerRef::You)
            .additional_token_spec(TokenSpec {
                characteristics: TokenCharacteristics {
                    display_name: "Squirrel".to_string(),
                    power: Some(1),
                    toughness: Some(1),
                    core_types: vec![CoreType::Creature],
                    subtypes: vec!["Squirrel".to_string()],
                    supertypes: Vec::new(),
                    colors: vec![ManaColor::Green],
                    keywords: Vec::new(),
                },
                script_name: "Squirrel".to_string(),
                static_abilities: Vec::new(),
                enter_with_counters: Vec::new(),
                tapped: false,
                enters_attacking: false,
                attach_to: None,
                sacrifice_at: None,
                source_id: ObjectId(0),
                controller: P0,
            });
        scenario
            .add_creature(P0, "Chatterfang, Squirrel General", 3, 3)
            .with_subtypes(vec!["Squirrel", "Warrior"])
            .with_replacement_definition(chatterfang_replacement);
        let mut state = scenario.state;
        let mut events = Vec::new();
        let ability = make_amass_ability("Orc", QuantityExpr::Fixed { value: 1 });

        resolve(&mut state, &ability, &mut events).unwrap();

        let army_tokens: Vec<_> = state
            .objects
            .values()
            .filter(|obj| obj.is_token && obj.card_types.subtypes.iter().any(|s| s == "Army"))
            .collect();
        assert_eq!(army_tokens.len(), 1, "amass should create one Army token");
        let army = army_tokens[0];
        assert!(
            army.card_types.subtypes.iter().any(|s| s == "Orc"),
            "new Army should be an Orc"
        );
        assert_eq!(
            army.counters.get(&CounterType::Plus1Plus1).copied(),
            Some(1),
            "amass should still place its +1/+1 counter on the new Army"
        );

        let squirrel_tokens: Vec<_> = state
            .objects
            .values()
            .filter(|obj| obj.is_token && obj.card_types.subtypes.iter().any(|s| s == "Squirrel"))
            .collect();
        assert_eq!(
            squirrel_tokens.len(),
            1,
            "Chatterfang should append one Squirrel when amass creates the Army token"
        );
    }
}
