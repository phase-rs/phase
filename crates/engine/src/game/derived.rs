use std::collections::HashSet;

use crate::game::combat::has_summoning_sickness;
use crate::game::coverage::unimplemented_mechanics;
use crate::game::devotion::count_devotion;
use crate::game::functioning_abilities::game_active_statics;
use crate::game::mana_abilities;
use crate::game::mana_sources::display_land_mana_pips;
use crate::game::static_abilities::{check_static_ability, StaticCheckContext};
use crate::types::ability::StaticCondition;
use crate::types::card_type::CoreType;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::statics::{ProhibitionScope, StaticMode};
use crate::types::zones::Zone;

/// Compute display-only derived fields (CR 302.6 summoning sickness, CR 700.5 devotion).
///
/// This must be called by any consumer (WASM, Tauri, server) before
/// serializing the state to the frontend. It sets:
/// - `GameObject::unimplemented_mechanics`
/// - `GameObject::has_summoning_sickness`
/// - `GameObject::devotion` (for Theros gods pattern)
/// - `GameObject::commander_tax` (CR 903.8 commander tax)
/// - `Player::can_look_at_top_of_library`
pub fn derive_display_state(state: &mut GameState) {
    let dirty = &state.public_state_dirty;

    let object_ids: Vec<_> = if dirty.all_objects_dirty {
        state.objects.keys().copied().collect()
    } else {
        dirty.dirty_objects.iter().copied().collect()
    };
    for id in object_ids {
        let (unimplemented, summoning_sickness) = {
            let Some(obj) = state.objects.get(&id) else {
                continue;
            };
            (
                unimplemented_mechanics(obj),
                // CR 302.6: Creature must have been under controller's control since turn began to attack or {T}.
                has_summoning_sickness(obj),
            )
        };

        let obj = state.objects.get_mut(&id).expect("object exists");
        obj.unimplemented_mechanics = unimplemented;
        obj.has_summoning_sickness = summoning_sickness;
        // Mana availability is only meaningful for battlefield permanents
        // (mana abilities are activated from the battlefield). For a dirty
        // object NOT on the battlefield — e.g. a permanent that just left —
        // reset its mana display fields here so a stale `has_mana_ability`
        // does not linger; the active battlefield values are (re)computed by
        // the board-wide mana sweep below. This is the cheap reset half; the
        // expensive auto-tap simulation runs only in that sweep.
        if obj.zone != Zone::Battlefield {
            obj.has_mana_ability = false;
            obj.mana_ability_index = None;
            obj.available_mana_pips.clear();
        }
    }

    // Mana availability (`has_mana_ability` / `mana_ability_index` /
    // `available_mana_pips`) is BOARD-GLOBAL derived state, not per-object:
    // `can_activate_mana_ability_now` runs an auto-tap payability simulation
    // over the whole controller's pool and every untapped source, so one
    // permanent's availability can flip when ANOTHER permanent taps, adds mana,
    // or gains/loses a depletion counter. It must therefore be re-derived as a
    // single battlefield-wide sweep gated on a mana-specific signal
    // (`mana_display_dirty`), never per-`dirty_objects` — otherwise a land
    // marked dirty by a non-mana event (Gemstone Mine depletion `CounterRemoved`,
    // damage to a creature-land) or a pool/tap change that flips a sibling
    // land's activatability leaves stale/blank mana display. The mana signal is
    // mana-specific (not `battlefield_display_dirty`) so spawning a creature
    // token never triggers this auto-tap sweep over every land.
    if dirty.mana_display_dirty || dirty.all_objects_dirty {
        let battlefield_ids: Vec<_> = state.battlefield.iter().copied().collect();
        crate::game::perf_counters::record_mana_display_sweep(battlefield_ids.len());
        // CR 604.1: hoist the activation-prohibition existence gates ONCE before
        // the per-source readiness checks. Without this, each of the (up to N)
        // mana sources re-scans the whole battlefield for City-of-Solitude-class
        // statics, making this sweep O(N^2) on go-wide mana boards.
        let activation_gates = mana_abilities::ManaActivationGates::compute(state);
        let mana_availability: Vec<(crate::types::identifiers::ObjectId, Option<usize>, _)> =
            battlefield_ids
                .into_iter()
                .filter_map(|id| {
                    let obj = state.objects.get(&id)?;
                    let mana_idx = obj
                        .abilities
                        .iter()
                        .enumerate()
                        .find(|(idx, ability)| {
                            mana_abilities::is_mana_ability(ability)
                                && mana_abilities::can_activate_mana_ability_now_gated(
                                    state,
                                    obj.controller,
                                    obj.id,
                                    *idx,
                                    ability,
                                    &activation_gates,
                                )
                        })
                        .map(|(idx, _)| idx);
                    let pips = if obj.card_types.core_types.contains(&CoreType::Land) {
                        display_land_mana_pips(state, id, obj.controller)
                    } else {
                        Vec::new()
                    };
                    Some((id, mana_idx, pips))
                })
                .collect();
        for (id, mana_idx, pips) in mana_availability {
            if let Some(obj) = state.objects.get_mut(&id) {
                obj.has_mana_ability = mana_idx.is_some();
                obj.mana_ability_index = mana_idx;
                obj.available_mana_pips = pips;
            }
        }
    }

    // Compute per-card devotion for cards with DevotionGE conditions
    // (Theros gods pattern — derive colors from the card's own base_color)
    if dirty.all_objects_dirty || dirty.battlefield_display_dirty {
        let devotion_cards: Vec<_> = state
            .objects
            .iter()
            .filter_map(|(&id, obj)| {
                // Classification scan: we only need to know whether this
                // object *declares* a devotion-conditioned static so the
                // dirty-tracker can pick it up. CR 604.1 / CR 702.26b
                // gating is applied later when the static actually
                // evaluates — here we must see every declared definition,
                // so `iter_unchecked` is the correct intent.
                let has_devotion_static =
                    obj.static_definitions
                        .iter_unchecked()
                        .any(|def| match &def.condition {
                            Some(StaticCondition::DevotionGE { .. }) => true,
                            Some(StaticCondition::Not { condition })
                                if matches!(
                                    condition.as_ref(),
                                    StaticCondition::DevotionGE { .. }
                                ) =>
                            {
                                true
                            }
                            _ => false,
                        });
                if has_devotion_static && !obj.base_color.is_empty() {
                    let devotion = count_devotion(state, obj.controller, &obj.base_color);
                    Some((id, devotion))
                } else {
                    None
                }
            })
            .collect();
        for (id, devotion) in devotion_cards {
            if let Some(obj) = state.objects.get_mut(&id) {
                obj.devotion = Some(devotion);
            }
        }
    }

    // CR 903.8: Compute commander tax for display.
    if dirty.all_objects_dirty || dirty.battlefield_display_dirty {
        let commander_taxes: Vec<_> = state
            .objects
            .iter()
            .filter_map(|(&id, obj)| {
                if obj.is_commander {
                    Some((id, super::commander::commander_tax(state, id)))
                } else {
                    None
                }
            })
            .collect();
        for (id, tax) in commander_taxes {
            if let Some(obj) = state.objects.get_mut(&id) {
                obj.commander_tax = Some(tax);
            }
        }
    }

    // (Dynamic land frame pips are computed together with `has_mana_ability` in
    // the single battlefield-wide mana-availability sweep above, gated on
    // `mana_display_dirty`, so the clear↔repopulate of `available_mana_pips`
    // is atomic and never split across two differently-gated blocks.)

    // CR 903.4: Per-player commander color identity. Derived from
    // `commander_color_identity` (which inspects deck pools and command-zone
    // objects) so the frontend can render `ManaPip::AnyInCommandersIdentity`
    // without recomputing identity client-side. Commander identity changes
    // only when commander objects move zones or deck pools update — gated by
    // the same flags that already cover those transitions.
    if dirty.all_players_dirty || dirty.all_objects_dirty || dirty.battlefield_display_dirty {
        let identities: Vec<Vec<crate::types::mana::ManaColor>> = state
            .players
            .iter()
            .map(|p| super::commander::commander_color_identity(state, p.id))
            .collect();
        for (i, identity) in identities.into_iter().enumerate() {
            state.players[i].commander_color_identity = identity;
        }
    }

    // Compute per-player derived fields
    if dirty.all_players_dirty || dirty.battlefield_display_dirty {
        let peek_flags: Vec<bool> = state
            .players
            .iter()
            .map(|p| {
                let ctx = StaticCheckContext {
                    player_id: Some(p.id),
                    ..Default::default()
                };
                check_static_ability(state, StaticMode::MayLookAtTopOfLibrary, &ctx)
            })
            .collect();
        for (i, flag) in peek_flags.into_iter().enumerate() {
            state.players[i].can_look_at_top_of_library = flag;
        }
    } else {
        let dirty_players: Vec<_> = dirty.dirty_players.iter().copied().collect();
        for player_id in dirty_players {
            let ctx = StaticCheckContext {
                player_id: Some(player_id),
                ..Default::default()
            };
            let flag = check_static_ability(state, StaticMode::MayLookAtTopOfLibrary, &ctx);
            if let Some(player) = state
                .players
                .iter_mut()
                .find(|player| player.id == player_id)
            {
                player.can_look_at_top_of_library = flag;
            }
        }
    }

    // Derive has_pending_cast so the frontend can read it directly
    // without maintaining a parallel list of casting-flow WaitingFor states.
    state.has_pending_cast = state.waiting_for.has_pending_cast();

    // Invariant: the two storage sites for "am I mid-cast" must agree. If
    // `waiting_for` says we're mid-cast, `GameState::pending_cast` must be
    // populated (either inline via the variant's `pending_cast_ref`, or on
    // the outer state for `ManaPayment`). Drift here is the bug class that
    // caused the `ChooseXValue` omission and the `Unsummon` cast/cancel loop
    // regression — this assert makes future drift surface immediately.
    debug_assert!(
        !state.has_pending_cast
            || state.pending_cast.is_some()
            || state.waiting_for.pending_cast_ref().is_some(),
        "has_pending_cast is true but no PendingCast is reachable — drift in {:?}",
        std::mem::discriminant(&state.waiting_for)
    );

    // CR 400.2: Continuous "play with the top card of your library revealed"
    // statics (Future Sight, Magus of the Future) must keep the library top in
    // `revealed_cards` across action boundaries — `apply_action` clears
    // momentary reveals at the start of each action, so re-sync here on every
    // derive pass before the state is exported to clients.
    sync_continuous_library_top_reveals(state);
    sync_continuous_hand_reveals(state);
}

/// CR 400.2: Repopulate `revealed_cards` for every active `RevealTopOfLibrary`
/// static after action-boundary clears.
fn sync_continuous_library_top_reveals(state: &mut GameState) {
    let mut reveal_all_players = false;
    let mut reveal_controllers = HashSet::<PlayerId>::new();

    for (source, def) in game_active_statics(state) {
        let StaticMode::RevealTopOfLibrary { all_players } = def.mode else {
            continue;
        };
        if all_players {
            reveal_all_players = true;
        } else {
            reveal_controllers.insert(source.controller);
        }
    }

    if !reveal_all_players && reveal_controllers.is_empty() {
        return;
    }

    let tops: Vec<ObjectId> = if reveal_all_players {
        state
            .players
            .iter()
            .filter_map(|player| player.library.front().copied())
            .collect()
    } else {
        reveal_controllers
            .into_iter()
            .filter_map(|controller| {
                state
                    .players
                    .iter()
                    .find(|player| player.id == controller)
                    .and_then(|player| player.library.front().copied())
            })
            .collect()
    };

    for top in tops {
        state.revealed_cards.insert(top);
    }
}

/// CR 400.2 + CR 701.20a: Continuous "play with [a] hand revealed" statics
/// keep affected players' hands public after action-boundary reveal clears.
fn sync_continuous_hand_reveals(state: &mut GameState) {
    let mut reveal_all_players = false;
    let mut reveal_controllers = HashSet::<PlayerId>::new();
    let mut reveal_opponents_of = HashSet::<PlayerId>::new();

    for (source, def) in game_active_statics(state) {
        let StaticMode::RevealHand { who } = &def.mode else {
            continue;
        };
        match who {
            ProhibitionScope::AllPlayers => reveal_all_players = true,
            ProhibitionScope::Controller => {
                reveal_controllers.insert(source.controller);
            }
            ProhibitionScope::Opponents => {
                reveal_opponents_of.insert(source.controller);
            }
            ProhibitionScope::EnchantedCreatureController => {}
        }
    }

    if !reveal_all_players && reveal_controllers.is_empty() && reveal_opponents_of.is_empty() {
        return;
    }

    let hand_cards = state
        .players
        .iter()
        .filter(|player| {
            reveal_all_players
                || reveal_controllers.contains(&player.id)
                || reveal_opponents_of
                    .iter()
                    .any(|controller| player.id != *controller)
        })
        .flat_map(|player| player.hand.iter().copied());

    state.revealed_cards.extend(hand_cards);
}

/// Commander damage received by `victim`, grouped by the commander's
/// controller (the attacking opponent). Each inner entry is
/// `(commander_object_id, damage)`. The frontend renders one badge per
/// entry, so this preserves the "separate commanders from the same
/// opponent" distinction (partners, backgrounds) while giving the HUD a
/// ready-to-render per-opponent summary without client-side filtering.
///
/// CR 903.10a tracks commander damage per commander; this helper adds the
/// display-oriented grouping-by-controller layer that clients need.
pub fn commander_damage_received(
    state: &GameState,
    victim: crate::types::player::PlayerId,
) -> std::collections::BTreeMap<
    crate::types::player::PlayerId,
    Vec<(crate::types::identifiers::ObjectId, u32)>,
> {
    let mut out: std::collections::BTreeMap<
        crate::types::player::PlayerId,
        Vec<(crate::types::identifiers::ObjectId, u32)>,
    > = std::collections::BTreeMap::new();
    for entry in &state.commander_damage {
        if entry.player != victim {
            continue;
        }
        // Look up the commander's controller (the attacking opponent).
        // A commander that has left the battlefield still exists in
        // state.objects — the Command zone sticks it back there — so the
        // lookup is stable across zone changes.
        let Some(commander_obj) = state.objects.get(&entry.commander) else {
            continue;
        };
        out.entry(commander_obj.controller)
            .or_default()
            .push((entry.commander, entry.damage));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::StaticDefinition;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::statics::ProhibitionScope;
    use crate::types::zones::Zone;

    #[test]
    fn derive_sets_summoning_sickness_for_new_creature() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 1;
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        obj.entered_battlefield_turn = Some(1);
        // CR 302.6: State-flip model — ETB-time sickness is a persistent flag,
        // set true on real ETB by `reset_for_battlefield_entry`. The test uses
        // `create_object` (scaffolding path) so we set it explicitly here.
        obj.summoning_sick = true;

        derive_display_state(&mut state);

        assert!(state.objects[&id].has_summoning_sickness);
    }

    #[test]
    fn derive_clears_summoning_sickness_for_old_creature() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 3;
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        state.objects.get_mut(&id).unwrap().entered_battlefield_turn = Some(1);
        // `summoning_sick` defaults to false — "old creature, not sick".

        derive_display_state(&mut state);

        assert!(!state.objects[&id].has_summoning_sickness);
    }

    #[test]
    fn derive_sets_unimplemented_flag() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test".to_string(),
            Zone::Battlefield,
        );

        derive_display_state(&mut state);

        // Should have set the flag (false for a card with no mechanics)
        let obj = &state.objects[&id];
        assert!(obj.unimplemented_mechanics.is_empty());
    }

    #[test]
    fn derive_reveals_opponents_hands_for_static_reveal_hand() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Telepathy".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::RevealHand {
                who: ProhibitionScope::Opponents,
            }));
        let controller_card = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Controller Card".to_string(),
            Zone::Hand,
        );
        let opponent_card = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Opponent Card".to_string(),
            Zone::Hand,
        );

        derive_display_state(&mut state);

        assert!(
            !state.revealed_cards.contains(&controller_card),
            "Telepathy-style static must not reveal its controller's hand"
        );
        assert!(
            state.revealed_cards.contains(&opponent_card),
            "Telepathy-style static must reveal opponents' hands"
        );
    }

    #[test]
    fn derive_reveals_all_hands_for_static_reveal_hand() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Revelation".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::RevealHand {
                who: ProhibitionScope::AllPlayers,
            }));
        let controller_card = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Controller Card".to_string(),
            Zone::Hand,
        );
        let opponent_card = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Opponent Card".to_string(),
            Zone::Hand,
        );

        derive_display_state(&mut state);

        assert!(state.revealed_cards.contains(&controller_card));
        assert!(state.revealed_cards.contains(&opponent_card));
    }

    #[test]
    fn derive_sets_can_look_at_top_default_false() {
        let mut state = GameState::new_two_player(42);

        derive_display_state(&mut state);

        assert!(!state.players[0].can_look_at_top_of_library);
        assert!(!state.players[1].can_look_at_top_of_library);
    }

    #[test]
    fn derive_sets_commander_tax_for_commander() {
        use crate::game::commander::record_commander_cast;

        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Commander".to_string(),
            Zone::Command,
        );
        state.objects.get_mut(&id).unwrap().is_commander = true;

        // No casts yet — tax should be 0
        derive_display_state(&mut state);
        assert_eq!(state.objects[&id].commander_tax, Some(0));

        // After 2 casts — tax should be 4
        record_commander_cast(&mut state, id);
        record_commander_cast(&mut state, id);
        derive_display_state(&mut state);
        assert_eq!(state.objects[&id].commander_tax, Some(4));
    }

    #[test]
    fn derive_does_not_set_commander_tax_for_non_commander() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        derive_display_state(&mut state);
        assert_eq!(state.objects[&id].commander_tax, None);
    }

    #[test]
    fn commander_damage_received_groups_by_controller() {
        use crate::types::game_state::CommanderDamageEntry;
        use crate::types::identifiers::ObjectId;

        let mut state = GameState::new_two_player(42);
        // Two commanders controlled by different opponents, both hitting P0.
        let cmdr_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Atraxa".to_string(),
            Zone::Battlefield,
        );
        let cmdr_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(2),
            "Breya".to_string(),
            Zone::Battlefield,
        );
        state.commander_damage = vec![
            CommanderDamageEntry {
                player: PlayerId(0),
                commander: cmdr_a,
                damage: 12,
            },
            CommanderDamageEntry {
                player: PlayerId(0),
                commander: cmdr_b,
                damage: 7,
            },
            // Unrelated entry: someone else's damage — must NOT appear in P0's map.
            CommanderDamageEntry {
                player: PlayerId(1),
                commander: ObjectId(9999),
                damage: 3,
            },
        ];

        let grouped = commander_damage_received(&state, PlayerId(0));
        assert_eq!(grouped.len(), 2, "expected two attacking opponents");
        assert_eq!(grouped[&PlayerId(1)], vec![(cmdr_a, 12)]);
        assert_eq!(grouped[&PlayerId(2)], vec![(cmdr_b, 7)]);
    }

    #[test]
    fn commander_damage_received_collects_partners_under_same_controller() {
        use crate::types::game_state::CommanderDamageEntry;

        let mut state = GameState::new_two_player(42);
        // Partner commanders: both controlled by P1, both hitting P0.
        let partner_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Ravos".to_string(),
            Zone::Battlefield,
        );
        let partner_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Tymna".to_string(),
            Zone::Battlefield,
        );
        state.commander_damage = vec![
            CommanderDamageEntry {
                player: PlayerId(0),
                commander: partner_a,
                damage: 6,
            },
            CommanderDamageEntry {
                player: PlayerId(0),
                commander: partner_b,
                damage: 4,
            },
        ];

        let grouped = commander_damage_received(&state, PlayerId(0));
        assert_eq!(grouped.len(), 1);
        let entries = &grouped[&PlayerId(1)];
        assert_eq!(
            entries.len(),
            2,
            "partners kept as distinct entries under same controller"
        );
    }
}
