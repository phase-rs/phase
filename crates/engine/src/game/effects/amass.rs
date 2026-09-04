use crate::game::effects::counters::{
    add_counter_with_replacement, stash_pending_counter_post_actions,
};
use crate::game::effects::token::apply_create_token_after_replacement;
use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{
    CostPaidObjectSnapshot, Effect, EffectError, EffectKind, ResolvedAbility,
};
use crate::types::card_type::CoreType;
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingCounterPostAction};
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
    let (subtype, count_expr, player_filter) = match &ability.effect {
        Effect::Amass {
            subtype,
            count,
            player,
        } => (subtype.clone(), count.clone(), player.clone()),
        _ => return Ok(()),
    };

    // CR 109.4 + CR 701.47a: the amass instruction is performed by `player`,
    // not necessarily the ability's own controller — Azog, Moria's Ruin's
    // "Its controller amasses Goblins X" binds this to the destroyed
    // creature's controller (`TargetFilter::ParentTargetController`), which
    // can differ from `ability.controller` (Azog's controller). Mirrors
    // `Manifest.target` / `Discover.player`'s resolution path.
    let controller = super::resolve_player_for_context_ref(state, ability, &player_filter);
    let n = resolve_quantity_with_targets(state, &count_expr, ability).max(0) as u32;

    // CR 701.47a: Find an existing Army creature on the controller's battlefield.
    let army_id = find_army(state, controller);

    let Some(target_id) = (if let Some(id) = army_id {
        Some(id)
    } else {
        create_army_token(state, controller, &subtype, n, ability, events)
    }) else {
        return Ok(());
    };
    continue_amass_on_army(state, controller, target_id, &subtype, n, ability, events);

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

pub(crate) fn continue_amass_after_token_creation(
    state: &mut GameState,
    controller: crate::types::player::PlayerId,
    subtype: &str,
    count: u32,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> bool {
    let Some(object_id) = find_army(state, controller) else {
        return true;
    };
    continue_amass_on_army(
        state, controller, object_id, subtype, count, ability, events,
    )
}

pub(crate) fn continue_amass_on_army(
    state: &mut GameState,
    controller: crate::types::player::PlayerId,
    object_id: ObjectId,
    subtype: &str,
    count: u32,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> bool {
    // CR 701.47a + CR 614.16: Counter replacement choices interrupt the amass
    // instruction before subtype addition and CR 701.47c binding complete.
    if count > 0
        && !add_counter_with_replacement(
            state,
            controller,
            object_id,
            CounterType::Plus1Plus1,
            count,
            events,
        )
    {
        stash_pending_counter_post_actions(
            state,
            EffectKind::Amass,
            ability.source_id,
            vec![PendingCounterPostAction::FinalizeAmass {
                object_id,
                subtype: subtype.to_string(),
                ability: Box::new(ability.clone()),
            }],
        );
        return false;
    }

    finalize_amass(state, object_id, subtype, ability, events);
    true
}

pub(crate) fn finalize_amass(
    state: &mut GameState,
    object_id: ObjectId,
    subtype: &str,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) {
    // CR 701.47a: If the chosen Army isn't a [subtype], it becomes a [subtype]
    // in addition to its other types.
    if let Some(obj) = state.objects.get_mut(&object_id) {
        if !obj
            .card_types
            .subtypes
            .iter()
            .any(|s| s.eq_ignore_ascii_case(subtype))
        {
            obj.card_types.subtypes.push(subtype.to_string());
            obj.base_card_types.subtypes.push(subtype.to_string());
        }
    }

    crate::game::layers::flush_layers(state);
    let Some(snapshot) = amassed_army_snapshot(state, object_id) else {
        return;
    };

    // CR 701.47c + CR 608.2c: Chained reflexive continuations may have been
    // stashed before a replacement choice finished. Stamp them with the final
    // post-replacement Army snapshot before they resume.
    if let Some(frame) = state.active_ability_continuation_frame_mut() {
        frame
            .pending
            .chain
            .set_amassed_army_object_recursive(snapshot.clone());
    }

    events.push(GameEvent::ArmyAmassed {
        object_id,
        source_id: ability.source_id,
        controller: ability.controller,
    });
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Amass,
        source_id: ability.source_id,
        subject: None,
    });
}

fn amassed_army_snapshot(state: &GameState, object_id: ObjectId) -> Option<CostPaidObjectSnapshot> {
    state
        .objects
        .get(&object_id)
        .map(|obj| CostPaidObjectSnapshot {
            object_id,
            lki: obj.snapshot_public_characteristics(),
        })
}

/// Create a 0/0 black [subtype] Army creature token on the battlefield.
fn create_army_token(
    state: &mut GameState,
    controller: crate::types::player::PlayerId,
    subtype: &str,
    count: u32,
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
        attach_to: crate::types::proposed_event::TokenHostRequest::NotRequested,
        sacrifice_at: None,
        source_id: ability.source_id,
        controller: ability.controller,
    };

    let proposed = ProposedEvent::CreateToken {
        owner: controller,
        spec: Box::new(spec),
        copy: None,
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
            stash_pending_counter_post_actions(
                state,
                EffectKind::Amass,
                ability.source_id,
                vec![PendingCounterPostAction::ContinueAmassAfterTokenCreation {
                    controller,
                    subtype: subtype.to_string(),
                    count,
                    ability: Box::new(ability.clone()),
                }],
            );
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
                player: crate::types::ability::TargetFilter::Controller,
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
        assert_eq!(army.power, Some(2));
        assert_eq!(army.toughness, Some(2));
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
                attach_to: crate::types::proposed_event::TokenHostRequest::NotRequested,
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

    // CR 701.47a + CR 701.47c + CR 301.5a (Goblin Plate Mail, HOB): the
    // provenance-binding regression pair. "When this Equipment enters, amass
    // Goblins 1, then attach this Equipment to the amassed Army" must attach
    // to the EXACT Army object amass just touched — the freshly created
    // token in the no-Army case, or the pre-existing Army in the other —
    // never re-derived by rescanning the battlefield for "an Army you
    // control" after the fact.
    const GOBLIN_PLATE_MAIL_ORACLE: &str = "When this Equipment enters, amass Goblins 1, then \
         attach this Equipment to the amassed Army. (To amass Goblins 1, put a +1/+1 counter on \
         an Army you control. It's also a Goblin. If you don't control an Army, create a 0/0 \
         black Goblin Army creature token first.)\nEquipped creature gets +1/+0 and has \
         menace.\nEquip {4}";

    /// CR 701.47a: no pre-existing Army — ETB creates a 0/0 black Goblin Army
    /// token, amass puts one +1/+1 counter on it, and the Equipment ends up
    /// attached to THAT token specifically. By the time resolution finishes,
    /// the Equipment's own "equipped creature gets +1/+0" (CR 613.4c layer
    /// 7c) is also live on its new host, so the token's EFFECTIVE power/
    /// toughness is 2/1 (0/0 base + the counter's +1/+1 + the equipment's
    /// +1/+0) — not the 1/1 the counter alone would produce.
    #[test]
    fn goblin_plate_mail_etb_creates_army_and_attaches_to_it() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(crate::types::phase::Phase::PreCombatMain);
        let equipment = scenario
            .add_artifact_to_hand_from_oracle(P0, "Goblin Plate Mail", GOBLIN_PLATE_MAIL_ORACLE)
            .with_subtypes(vec!["Equipment"])
            .with_mana_cost(crate::types::mana::ManaCost::generic(0))
            .id();
        let mut runner = scenario.build();

        let outcome = runner.cast(equipment).resolve();
        let state = outcome.state();

        let armies: Vec<_> = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|obj| obj.card_types.subtypes.iter().any(|s| s == "Army"))
            .collect();
        assert_eq!(
            armies.len(),
            1,
            "expected exactly one Army on the battlefield"
        );
        let army = armies[0];
        assert!(
            army.is_token,
            "no pre-existing Army — amass must create a token"
        );
        assert_eq!(army.base_power, Some(0), "amass creates a 0/0 Army token");
        assert_eq!(
            army.base_toughness,
            Some(0),
            "amass creates a 0/0 Army token"
        );
        assert_eq!(
            army.counters.get(&CounterType::Plus1Plus1).copied(),
            Some(1),
            "amass Goblins 1 puts one +1/+1 counter on the amassed Army"
        );
        // CR 613.4c (layer 7c): once attached, Goblin Plate Mail's own
        // "equipped creature gets +1/+0" is live on this token too, so the
        // EFFECTIVE power/toughness is base 0/0 + the counter's +1/+1 + the
        // equipment's +1/+0 = 2/1, not the 1/1 the counter alone would give.
        assert_eq!(army.power, Some(2));
        assert_eq!(army.toughness, Some(1));
        assert!(army.card_types.subtypes.iter().any(|s| s == "Goblin"));

        let equipment_obj = state.objects.get(&equipment).expect("equipment object");
        assert_eq!(
            equipment_obj.attached_to,
            Some(crate::game::game_object::AttachTarget::Object(army.id)),
            "Goblin Plate Mail must attach to the newly created Army token"
        );
    }

    /// CR 701.47c: a pre-existing Army — amass must reuse it (not create a
    /// second, phantom Army) and the Equipment must attach to THAT exact
    /// object, not to some Army found by re-scanning the battlefield.
    #[test]
    fn goblin_plate_mail_etb_reuses_existing_army_and_attaches_to_it() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(crate::types::phase::Phase::PreCombatMain);
        let existing_army = scenario
            .add_creature(P0, "Zombie Army", 0, 0)
            .with_subtypes(vec!["Army", "Zombie"])
            .id();
        scenario.with_counter(existing_army, CounterType::Plus1Plus1, 2);
        let equipment = scenario
            .add_artifact_to_hand_from_oracle(P0, "Goblin Plate Mail", GOBLIN_PLATE_MAIL_ORACLE)
            .with_subtypes(vec!["Equipment"])
            .with_mana_cost(crate::types::mana::ManaCost::generic(0))
            .id();
        let mut runner = scenario.build();

        let outcome = runner.cast(equipment).resolve();
        let state = outcome.state();

        let armies: Vec<_> = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|obj| obj.card_types.subtypes.iter().any(|s| s == "Army"))
            .collect();
        assert_eq!(
            armies.len(),
            1,
            "must not create a second Army when the controller already has one"
        );
        let army = armies[0];
        assert_eq!(
            army.id, existing_army,
            "must reuse the pre-existing Army object, not a new one"
        );
        assert_eq!(
            army.counters.get(&CounterType::Plus1Plus1).copied(),
            Some(3),
            "amass Goblins 1 adds to the existing Army's counters (2 -> 3)"
        );
        assert!(army.card_types.subtypes.iter().any(|s| s == "Zombie"));
        assert!(army.card_types.subtypes.iter().any(|s| s == "Goblin"));

        let equipment_obj = state.objects.get(&equipment).expect("equipment object");
        assert_eq!(
            equipment_obj.attached_to,
            Some(crate::game::game_object::AttachTarget::Object(existing_army)),
            "Goblin Plate Mail must attach to the EXISTING Army amass touched, not a phantom new one"
        );
    }
}
