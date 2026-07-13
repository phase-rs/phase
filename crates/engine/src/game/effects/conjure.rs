use crate::game::layers::compute_current_copiable_values;
use crate::game::printed_cards::{apply_card_face_to_object, apply_copiable_values};
use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::zones;
use crate::types::ability::{
    ConjureSource, CopiableValues, Effect, EffectError, EffectKind, LibraryPosition,
    ResolvedAbility, TargetFilter,
};
use crate::types::card::CardFace;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;
use rand::Rng;

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
    let (cards, destination, tapped, library_position, library_players) = match &ability.effect {
        Effect::Conjure {
            cards,
            destination,
            tapped,
            library_position,
            library_players,
        } => (
            cards,
            *destination,
            *tapped,
            library_position.clone(),
            library_players.clone(),
        ),
        _ => return Ok(()),
    };

    // Which players' libraries receive the conjured cards. `None` (every conjure
    // except the Alchemy "each player's library" fan-out) is the controller only,
    // preserving the historical single-recipient behavior. `Some(PlayerFilter)`
    // expands to each affected player, so a card conjured "into … each player's
    // library" lands one independent copy in every player's own library, owned by
    // that player (Sandcloud Harbinger). Non-eliminated players only (CR 104.3a).
    let recipients: Vec<PlayerId> = match &library_players {
        None => vec![ability.controller],
        Some(scope) => state
            .players
            .iter()
            .map(|p| p.id)
            .filter(|&pid| {
                crate::game::effects::matches_player_scope(
                    state,
                    pid,
                    scope,
                    ability.controller,
                    ability.source_id,
                )
            })
            .collect(),
    };

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

        // One independent copy per affected player, each in that player's own
        // library. For the single-recipient default (`recipients == [controller]`)
        // this is exactly the historical behavior.
        for &recipient in &recipients {
            for _ in 0..count {
                let obj_id = zones::create_object(
                    state,
                    CardId(0),
                    recipient,
                    card_name.clone(),
                    destination,
                );

                // `create_object` appends a library-bound card to the bottom. When
                // the conjure carries a positional constraint (the Alchemy "into the
                // top N cards … at random" arm), move it to the resolved slot within
                // this recipient's library instead.
                if destination == Zone::Library {
                    if let Some(position) = &library_position {
                        reposition_conjured_in_library(state, ability, recipient, obj_id, position);
                    }
                }

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
    }

    // A library reorder can change which card is on top of a library. If any
    // active continuous static is gated on the top card of a library (CR 611.3a:
    // a `TopOfLibraryMatches` ability such as Vampire Nocturnus, "as long as the
    // top card of your library is black …"), invalidate the cached layer
    // derivation so it re-evaluates against the new top — the same invalidation
    // seam every other top-of-library mutation (draw, mill, shuffle, put-on-top)
    // routes through. Only the positional-library conjure reorders; other
    // destinations append and never disturb an existing top card.
    if destination == Zone::Library && library_position.is_some() {
        crate::game::layers::mark_layers_full_if_top_of_library_static_live(state);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Conjure,
        source_id: ability.source_id,
        subject: None,
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

/// Reposition a just-conjured card within `owner`'s library. `create_object`
/// appended it to the bottom of that player's library; this moves it to the slot
/// named by `position`. `owner` is the recipient player (the controller for "your
/// library", or each affected player for the "each player's library" fan-out),
/// not necessarily the ability's controller. Only `RandomWithinTop` is produced
/// by the parser today (the Alchemy "into the top N cards … at random" conjure);
/// the other positions are honored for completeness so the field composes with
/// any future positional conjure.
fn reposition_conjured_in_library(
    state: &mut GameState,
    ability: &ResolvedAbility,
    owner: PlayerId,
    object_id: ObjectId,
    position: &LibraryPosition,
) {
    // Library length excluding the just-appended conjured card, so index math and
    // the random range treat the card as being *inserted* among the existing ones.
    let existing_len = state
        .players
        .iter()
        .find(|p| p.id == owner)
        .map_or(0, |p| p.library.len())
        .saturating_sub(1);
    let index = match position {
        LibraryPosition::Top => 0,
        LibraryPosition::Bottom => existing_len,
        // 1-based ("second from the top" => n=2, index 1); clamped to the bottom.
        LibraryPosition::NthFromTop { n } => (*n as usize).saturating_sub(1).min(existing_len),
        LibraryPosition::BeneathTop { depth } => {
            (resolve_quantity_with_targets(state, depth, ability).max(0) as usize).min(existing_len)
        }
        // Digital-only Alchemy (no CR entry): insert at a uniformly random 0-based
        // index in `[0, min(n, existing_len + 1))`, so the card lands somewhere
        // among the top N cards.
        LibraryPosition::RandomWithinTop { n } => {
            let n = resolve_quantity_with_targets(state, n, ability).max(1) as usize;
            let upper = n.min(existing_len + 1).max(1);
            state.rng.random_range(0..upper)
        }
    };
    let Some(player) = state.players.iter_mut().find(|p| p.id == owner) else {
        return;
    };
    // A just-conjured card is already in the library (create_object appended it);
    // this only reorders within the same zone, so it is not a replaceable zone event.
    if let Some(current) = player.library.iter().position(|id| *id == object_id) {
        // allow-raw-zone: in-library reorder of a just-conjured card, not a zone event.
        player.library.remove(current);
    }
    let clamped = index.min(player.library.len());
    // allow-raw-zone: in-library reorder of a just-conjured card, not a zone event.
    player.library.insert(clamped, object_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{ConjureCard, QuantityExpr, TargetRef};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    /// Issue #5614: "conjure a duplicate … into the top five cards of your library
    /// at random" must land the conjured card in a random slot AMONG the top five —
    /// not push it to the bottom (the collapsed `Zone::Library` behavior). Runs many
    /// conjures against a large library and asserts every one lands within the top
    /// five and that the slot actually varies (proving it is random, not a fixed
    /// top/bottom placement). Reverting `reposition_conjured_in_library` — which
    /// leaves the card at the bottom `create_object` places it — fails this test.
    #[test]
    fn conjure_random_within_top_lands_among_top_n_and_varies() {
        use crate::types::ability::LibraryPosition;
        use std::collections::HashSet;

        let mut state = GameState::new_two_player(42);

        // A library far larger than N, so "top five" is a genuine constraint.
        for i in 0..20 {
            crate::game::zones::create_object(
                &mut state,
                CardId(0),
                PlayerId(0),
                format!("Filler {i}"),
                Zone::Library,
            );
        }

        let mut indices = HashSet::new();
        for iter in 0..40 {
            let name = format!("Conjured {iter}");
            let ability = ResolvedAbility::new(
                Effect::Conjure {
                    cards: vec![ConjureCard {
                        source: ConjureSource::Named { name: name.clone() },
                        count: QuantityExpr::Fixed { value: 1 },
                    }],
                    destination: Zone::Library,
                    tapped: false,
                    library_position: Some(LibraryPosition::RandomWithinTop {
                        n: QuantityExpr::Fixed { value: 5 },
                    }),
                    library_players: None,
                },
                vec![],
                ObjectId(99),
                PlayerId(0),
            );
            let mut events = Vec::new();
            resolve(&mut state, &ability, &mut events).unwrap();

            let library = &state.players[0].library;
            let index = library
                .iter()
                .position(|id| state.objects[id].name == name)
                .expect("conjured card is in the library");
            assert!(
                index < 5,
                "conjured card must land among the top five, got index {index}"
            );
            indices.insert(index);
        }

        // The slot is drawn from the RNG, so across 40 conjures more than one
        // distinct position must appear — a fixed top (always 0) or bottom
        // placement would collapse this set to a single value.
        assert!(
            indices.len() > 1,
            "random-within-top slot should vary across draws, saw {indices:?}"
        );
    }

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
                library_position: None,
                library_players: None,
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
                library_position: None,
                library_players: None,
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

    /// Issue #5614 (re-review blocker): "conjure N cards … into the top ten cards
    /// of EACH player's library at random" (Sandcloud Harbinger) must fan out — one
    /// independent copy in EVERY player's own library, owned by that player — not
    /// pile all copies into the controller's library. Asserts each of the two
    /// players' libraries gains all three copies, owned by and placed among the top
    /// of that player's own library. Collapsing the scope to the controller (the
    /// pre-fix behavior) leaves player 1's library empty and fails this test.
    #[test]
    fn conjure_each_players_library_fans_out_to_every_player() {
        use crate::types::ability::{LibraryPosition, PlayerFilter};

        let mut state = GameState::new_two_player(7);

        // Give each player a sizable library so "top ten" is a genuine constraint.
        for pid in [PlayerId(0), PlayerId(1)] {
            for i in 0..15 {
                crate::game::zones::create_object(
                    &mut state,
                    CardId(0),
                    pid,
                    format!("Filler {i}"),
                    Zone::Library,
                );
            }
        }

        let ability = ResolvedAbility::new(
            Effect::Conjure {
                cards: vec![ConjureCard {
                    source: ConjureSource::Named {
                        name: "Sunscorched Desert".to_string(),
                    },
                    count: QuantityExpr::Fixed { value: 3 },
                }],
                destination: Zone::Library,
                tapped: false,
                library_position: Some(LibraryPosition::RandomWithinTop {
                    n: QuantityExpr::Fixed { value: 10 },
                }),
                library_players: Some(PlayerFilter::All),
            },
            vec![],
            ObjectId(99),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        for pid in [PlayerId(0), PlayerId(1)] {
            let player = state.players.iter().find(|p| p.id == pid).unwrap();
            let conjured: Vec<ObjectId> = player
                .library
                .iter()
                .copied()
                .filter(|id| state.objects[id].name == "Sunscorched Desert")
                .collect();
            assert_eq!(
                conjured.len(),
                3,
                "each player's library must receive all three conjured copies (player {pid:?})"
            );
            for id in &conjured {
                assert_eq!(
                    state.objects[id].owner, pid,
                    "conjured copy must be owned by the recipient player, not the controller"
                );
                let index = player.library.iter().position(|x| x == id).unwrap();
                assert!(
                    index < 10,
                    "conjured copy must land among the recipient's top ten, got index {index}"
                );
            }
        }
    }

    /// Issue #5614 (re-review blocker): repositioning a just-conjured card to the
    /// top of a library (RandomWithinTop with n=1 forces index 0) must route through
    /// the shared top-of-library invalidation seam
    /// (`mark_layers_full_if_top_of_library_static_live`) so a live
    /// `TopOfLibraryMatches` static (Vampire Nocturnus class) re-evaluates against
    /// the new top card. Without the seam a cached layer derivation retains the stale
    /// former top. PRODUCTION PATH: runs the real conjure resolver with NO manual
    /// `mark_full`, then asserts the conjured card is on top AND layers were dirtied.
    /// Removing the invalidation call leaves `layers_dirty` clean and fails here.
    #[test]
    fn conjure_to_top_of_library_reevaluates_top_of_library_static() {
        use crate::types::ability::{
            ContinuousModification, StaticCondition, StaticDefinition, TypeFilter, TypedFilter,
        };
        use crate::types::keywords::Keyword;
        use crate::types::statics::StaticMode;
        use std::sync::Arc;

        let mut state = GameState::new_two_player(1);

        // A battlefield source carrying a continuous static gated on the top card of
        // a library (the `TopOfLibraryMatches` class the seam protects).
        let top_static = StaticDefinition::new(StaticMode::Continuous)
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Flying,
            }])
            .condition(StaticCondition::TopOfLibraryMatches {
                filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
            });
        let source = crate::game::zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Nocturnus".to_string(),
            Zone::Battlefield,
        );
        {
            let o = state.objects.get_mut(&source).unwrap();
            o.static_definitions.push(top_static.clone());
            o.base_static_definitions = Arc::new(vec![top_static]);
        }

        // Existing library cards, so conjuring to index 0 is a genuine reorder to the
        // top over a non-empty library.
        for i in 0..5 {
            crate::game::zones::create_object(
                &mut state,
                CardId(0),
                PlayerId(0),
                format!("Filler {i}"),
                Zone::Library,
            );
        }

        // Clean the layer cache so ONLY the conjure's invalidation can re-dirty it.
        state.layers_dirty = crate::types::game_state::LayersDirty::Clean;

        // n=1 clamps the random range to `0..1`, so the conjured card is placed at
        // the top deterministically — the case that changes the library-top card.
        let ability = ResolvedAbility::new(
            Effect::Conjure {
                cards: vec![ConjureCard {
                    source: ConjureSource::Named {
                        name: "Conjured Top".to_string(),
                    },
                    count: QuantityExpr::Fixed { value: 1 },
                }],
                destination: Zone::Library,
                tapped: false,
                library_position: Some(LibraryPosition::RandomWithinTop {
                    n: QuantityExpr::Fixed { value: 1 },
                }),
                library_players: None,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.players[0]
                .library
                .front()
                .and_then(|id| state.objects.get(id))
                .map(|o| o.name.as_str()),
            Some("Conjured Top"),
            "n=1 must place the conjured card on top of the library"
        );
        assert!(
            state.layers_dirty.is_dirty(),
            "a live TopOfLibraryMatches static must re-evaluate after a conjure reorders the library top"
        );
    }
}
