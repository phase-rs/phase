use crate::game::layers::compute_current_copiable_values;
use crate::game::printed_cards::{apply_card_face_to_object, apply_copiable_values};
use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::zones;
use crate::types::ability::{
    ConjureSource, CopiableValues, Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter,
};
use crate::types::card::CardFace;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::zones::Zone;

/// The fully-resolved identity of a conjured card for one `ConjureCard` entry.
enum ConjuredIdentity {
    /// A specific named card; `face` is its printed `CardFace` from the registry
    /// (`None` when the card is not in the registry). Boxed to keep the enum
    /// small (clippy::large_enum_variant).
    Named {
        name: String,
        face: Option<Box<CardFace>>,
    },
    /// CR 707.2: a duplicate of a referenced card — the referenced card's current
    /// copiable values, applied to the conjured card so it has real
    /// characteristics. Boxed to keep the enum small (clippy::large_enum_variant).
    Duplicate(Box<CopiableValues>),
}

/// Digital-only keyword action (no CR entry): Conjure creates a card from outside
/// the game and places it into a specified zone. Unlike tokens, conjured cards are
/// "real" cards with full card characteristics (mana value, types, abilities, etc.).
///
/// For a `Named` source the handler looks up the card from
/// `state.card_face_registry` (populated at game init by
/// `rehydrate_game_from_card_db`) and applies its printed face via
/// `apply_card_face_to_object`. For a `Duplicate` source (CR 707.2) it resolves
/// the referenced card and applies that card's current copiable values via
/// `apply_copiable_values`, so the conjured card has full characteristics
/// regardless of whether its name is in the (scoped) registry.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (cards, destination, tapped) = match &ability.effect {
        Effect::Conjure {
            cards,
            destination,
            tapped,
        } => (cards, *destination, *tapped),
        _ => return Ok(()),
    };

    let mut conjured_ids: Vec<ObjectId> = Vec::new();
    for conjure_card in cards {
        let count =
            resolve_quantity_with_targets(state, &conjure_card.count, ability).max(0) as u32;

        // Resolve the conjured card's full identity (CR 707.2):
        // - `Named`: look up the printed face from the registry by name.
        // - `Duplicate`: resolve the referenced card (it is in play with a full
        //   CardFace) and snapshot its *current copiable values* directly, so the
        //   conjured card carries real characteristics (types, mana cost, P/T,
        //   abilities) — not just a name. A name->registry round-trip would miss
        //   here because the registry only preloads statically-named conjures.
        // An unresolved reference conjures nothing.
        let identity = match &conjure_card.source {
            ConjureSource::Named { name } => ConjuredIdentity::Named {
                name: name.clone(),
                face: state
                    .card_face_registry
                    .get(&name.to_lowercase())
                    .cloned()
                    .map(Box::new),
            },
            ConjureSource::Duplicate { duplicate_of } => {
                match resolve_duplicate_reference(state, ability, duplicate_of)
                    .and_then(|id| compute_current_copiable_values(state, id))
                {
                    Some(values) => ConjuredIdentity::Duplicate(Box::new(values)),
                    None => continue,
                }
            }
        };
        let card_name = match &identity {
            ConjuredIdentity::Named { name, .. } => name.clone(),
            ConjuredIdentity::Duplicate(values) => values.name.clone(),
        };

        for _ in 0..count {
            let obj_id = zones::create_object(
                state,
                CardId(0),
                ability.controller,
                card_name.clone(),
                destination,
            );
            conjured_ids.push(obj_id);

            // CR 613.7d: an object receives a timestamp when it enters a zone.
            // Stage 2 stamps battlefield entries only, so only draw one when the
            // conjured card lands on the battlefield. Drawn before the `get_mut`
            // borrow (`next_timestamp` takes `&mut self`).
            let entry_timestamp =
                (destination == Zone::Battlefield).then(|| state.next_timestamp());

            if let Some(obj) = state.objects.get_mut(&obj_id) {
                // Conjured cards are real cards, not tokens.
                obj.is_token = false;

                // Apply full card characteristics: the printed face for a named
                // conjure, or the referenced card's copiable values (CR 707.2) for
                // a duplicate conjure.
                match &identity {
                    ConjuredIdentity::Named {
                        face: Some(face), ..
                    } => apply_card_face_to_object(obj, face),
                    ConjuredIdentity::Named { face: None, .. } => {}
                    ConjuredIdentity::Duplicate(values) => apply_copiable_values(obj, values),
                }

                if destination == Zone::Battlefield {
                    // CR 302.6: A creature entering the battlefield has summoning
                    // sickness unless its controller has controlled it continuously
                    // since their most recent turn began. A conjured permanent is a
                    // brand-new object, so it must run the same entry reset (summoning
                    // sickness, marked damage, per-turn activation flags) as any other
                    // battlefield entry — otherwise a conjured creature could attack or
                    // tap for {T} costs the turn it appears. Delegate to the single
                    // authority rather than setting flags ad hoc.
                    obj.reset_for_battlefield_entry(
                        state.turn_number,
                        entry_timestamp.expect("battlefield entry draws a timestamp"),
                    );

                    // Apply tapped state for "onto the battlefield tapped" patterns.
                    if tapped {
                        obj.tapped = true;
                    }
                }
            }

            // Record battlefield entry for restriction tracking.
            if destination == Zone::Battlefield {
                crate::game::restrictions::record_battlefield_entry(state, obj_id);
                // Battlefield entry: incremental re-derive candidate for this
                // conjured object (escalates to Full if it sources effects/etc.).
                crate::game::layers::mark_layers_entered(state, obj_id);

                // CR 603.6a: Conjuring places a card from outside the game
                // directly onto the battlefield — a zone change from `None`.
                // Emit `ZoneChanged { from: None, to: Battlefield }` (in addition to
                // `ObjectConjured`, which animation/logging consumers still read) so
                // every enters-the-battlefield triggered ability fires through the
                // same matcher path used for normal entries and token creation
                // (e.g. Verdant Dread's "another Verdant Dread enters" manifest-dread
                // trigger, Soul Warden, Panharmonicon). Without this the conjured
                // permanent enters silently and no ETB ability ever triggers.
                let zone_change_record = state
                    .objects
                    .get(&obj_id)
                    .expect("conjured object was just created")
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
            }

            events.push(GameEvent::ObjectConjured {
                object_id: obj_id,
                name: card_name.clone(),
            });
        }
    }

    // Publish the conjured objects as the engine-local most-recently-created set
    // so chained sub-abilities that reference them via `TargetFilter::LastCreated`
    // bind correctly (e.g. Three Tree Battalion / Blood Age Muster's "the
    // duplicate / its base power and toughness perpetually …"). A conjure that
    // produced nothing leaves any prior `LastCreated` untouched.
    if !conjured_ids.is_empty() {
        state.last_created_token_ids = conjured_ids;
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Conjure,
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 707.2: Resolve a duplicate-conjure reference to the name of the card being
/// copied. The reference is either the inherited parent target ("it" / "that
/// card") or an explicit target ("target … card exiled with ~"); either way it
/// resolves to a single object whose name identifies the card to conjure.
fn resolve_duplicate_reference(
    state: &GameState,
    ability: &ResolvedAbility,
    reference: &TargetFilter,
) -> Option<ObjectId> {
    let resolved = crate::game::targeting::resolved_targets(ability, reference, state);
    let object_ids = crate::game::effects::effect_object_targets(reference, &resolved);
    object_ids.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{ConjureCard, PerpetualModification, QuantityExpr, TargetRef};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    #[test]
    fn battlefield_conjure_records_zone_change_for_turn_history() {
        let mut state = GameState::new_two_player(7);
        let ability = ResolvedAbility::new(
            Effect::Conjure {
                cards: vec![ConjureCard {
                    source: ConjureSource::Named {
                        name: "Verdant Dread".to_string(),
                    },
                    count: QuantityExpr::Fixed { value: 1 },
                }],
                destination: Zone::Battlefield,
                tapped: false,
            },
            vec![],
            ObjectId(99),
            PlayerId(0),
        );
        let mut events = Vec::new();

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
            .expect("conjuring onto the battlefield emits ZoneChanged");

        assert_eq!(zone_change.1, None);
        assert_eq!(zone_change.2, Zone::Battlefield);
        assert_eq!(state.zone_changes_this_turn.len(), 1);
        assert_eq!(state.zone_changes_this_turn[0].object_id, zone_change.0);
        assert_eq!(state.zone_changes_this_turn[0].from_zone, None);
        assert_eq!(state.zone_changes_this_turn[0].to_zone, Zone::Battlefield);
    }

    /// Digital Alchemy: a conjure publishes the conjured object as the
    /// engine-local most-recently-created set, so a chained perpetual sub-ability that
    /// references it via `LastCreated` (Three Tree Battalion / Blood Age Muster's
    /// "the duplicate / its base power and toughness perpetually …") edits the
    /// conjured card — not the conjuring source.
    #[test]
    fn conjure_publishes_last_created_for_perpetual_subability() {
        let mut state = GameState::new_two_player(7);

        // The conjuring permanent. The perpetual edit must not land on it.
        let source = crate::game::zones::create_object(
            &mut state,
            CardId(7),
            PlayerId(0),
            "Three Tree Battalion".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.base_power = Some(3);
            obj.base_toughness = Some(3);
        }

        // Conjure a card onto the battlefield (the "duplicate").
        let conjure = ResolvedAbility::new(
            Effect::Conjure {
                cards: vec![ConjureCard {
                    source: ConjureSource::Named {
                        name: "Grizzly Bears".to_string(),
                    },
                    count: QuantityExpr::Fixed { value: 1 },
                }],
                destination: Zone::Battlefield,
                tapped: false,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &conjure, &mut events).unwrap();

        // The conjure published exactly the conjured object as LastCreated.
        assert_eq!(state.last_created_token_ids.len(), 1);
        let duplicate = state.last_created_token_ids[0];
        assert_ne!(duplicate, source);

        // The chained "the duplicate perpetually has base power and toughness 1/1"
        // sub-ability resolves against LastCreated.
        let perpetual = ResolvedAbility::new(
            Effect::ApplyPerpetual {
                target: TargetFilter::LastCreated,
                modification: PerpetualModification::SetBasePowerToughness {
                    power: 1,
                    toughness: 1,
                },
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        super::super::perpetual::resolve(&mut state, &perpetual, &mut events).unwrap();

        // The conjured object got 1/1; the conjuring source is untouched.
        let dup = state.objects.get(&duplicate).unwrap();
        assert_eq!(dup.base_power, Some(1));
        assert_eq!(dup.base_toughness, Some(1));
        let src = state.objects.get(&source).unwrap();
        assert_eq!(src.base_power, Some(3));
        assert_eq!(src.base_toughness, Some(3));
    }

    /// CR 707.2: "conjure a duplicate of <reference>" copies the referenced
    /// card by name into the destination — a new, distinct real card object.
    #[test]
    fn duplicate_conjure_copies_referenced_card_characteristics() {
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(7);
        // A referenced creature card (in exile) with real characteristics — these
        // must flow into the conjured duplicate (CR 707.2), not just the name.
        let referenced = crate::game::zones::create_object(
            &mut state,
            CardId(5),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Exile,
        );
        {
            let obj = state.objects.get_mut(&referenced).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }
        // The conjure ability inherits the referenced card as its target, so the
        // anaphoric `ParentTarget` reference resolves to it.
        let ability = ResolvedAbility::new(
            Effect::Conjure {
                cards: vec![ConjureCard {
                    source: ConjureSource::Duplicate {
                        duplicate_of: TargetFilter::ParentTarget,
                    },
                    count: QuantityExpr::Fixed { value: 1 },
                }],
                destination: Zone::Hand,
                tapped: false,
            },
            vec![TargetRef::Object(referenced)],
            ObjectId(99),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // A new card (distinct from the referenced one) is conjured into hand.
        let conjured_id = *state.players[0]
            .hand
            .iter()
            .find(|id| **id != referenced)
            .expect("duplicate-conjure should create a card in hand");
        let conjured = &state.objects[&conjured_id];

        // CR 707.2: the conjured card carries the referenced card's copiable
        // characteristics — name, types, and P/T — not merely its name.
        assert_eq!(conjured.name, "Grizzly Bears");
        assert!(
            conjured.card_types.core_types.contains(&CoreType::Creature),
            "conjured duplicate must copy the creature type, got {:?}",
            conjured.card_types.core_types
        );
        assert_eq!(
            conjured.power,
            Some(2),
            "conjured duplicate must copy the referenced card's power"
        );
        assert_eq!(conjured.toughness, Some(2));
        assert!(
            !conjured.is_token,
            "conjured cards are real cards, not tokens"
        );
    }
}
